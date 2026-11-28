//! High-level `Document` combining `PieceTree`, `UndoStack`, EOL/BOM rules,
//! incremental syntax cache, and atomic / APFS CoW persistence.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use editor_platform::{FileRev, FileSystem, FsError};

use crate::grapheme::validate_utf8_offset;
use crate::piece_tree::PieceTree;
use crate::regex::RegexError;
use crate::search::{
    find_all_matches, find_all_matches_checked, CompiledQuery, SearchMatch, SearchOptions,
    SearchScratch,
};
use crate::syntax::{Language, SyntaxCache, TokenSpan};
use crate::undo::{EditOp, UndoStack};

const BINARY_PROBE_BYTES: usize = 8192;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eol {
    Lf,
    CrLf,
    Cr,
    Mixed,
}

impl Eol {
    pub fn as_str(self) -> &'static str {
        match self {
            Eol::Lf => "lf",
            Eol::CrLf => "crlf",
            Eol::Cr => "cr",
            Eol::Mixed => "mixed",
        }
    }

    pub fn display_badge(self) -> &'static str {
        match self {
            Eol::Lf => "LF",
            Eol::CrLf => "CRLF",
            Eol::Cr => "CR",
            Eol::Mixed => "Mixed",
        }
    }

    pub fn dominant_bytes(self) -> &'static [u8] {
        match self {
            Eol::CrLf => b"\r\n",
            Eol::Cr => b"\r",
            Eol::Lf | Eol::Mixed => b"\n",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentError {
    Fs(FsError),
    Binary { offset: usize },
    UnsupportedEncoding(&'static str),
    InvalidUtf8 { offset: usize },
    Range(String),
    Conflict { expected: FileRev, actual: FileRev },
    NoPath,
}

impl std::fmt::Display for DocumentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DocumentError::Fs(e) => write!(f, "{e}"),
            DocumentError::Binary { offset } => write!(f, "binary NUL byte at offset {offset}"),
            DocumentError::UnsupportedEncoding(e) => write!(f, "unsupported encoding: {e}"),
            DocumentError::InvalidUtf8 { offset } => write!(f, "invalid UTF-8 at offset {offset}"),
            DocumentError::Range(m) => write!(f, "out of range: {m}"),
            DocumentError::Conflict { .. } => write!(f, "file changed on disk since open"),
            DocumentError::NoPath => write!(f, "document has no file path"),
        }
    }
}

impl std::error::Error for DocumentError {}

impl From<FsError> for DocumentError {
    fn from(value: FsError) -> Self {
        DocumentError::Fs(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveMethod {
    None,
    ClonePwrite,
    ClonePwriteTail,
    AtomicRewrite,
    InPlace,
}

impl SaveMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            SaveMethod::None => "none",
            SaveMethod::ClonePwrite => "clone_pwrite",
            SaveMethod::ClonePwriteTail => "clone_pwrite_tail",
            SaveMethod::AtomicRewrite => "full_rewrite",
            SaveMethod::InPlace => "in_place",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityCheck {
    Unchanged,
    ReloadedClean,
    StaleDirty,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveReport {
    pub method: SaveMethod,
    pub bytes_written: u64,
    pub rev: FileRev,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentInfo {
    pub path: Option<PathBuf>,
    pub bytes: u64,
    pub line_count: usize,
    pub eol: Eol,
    pub bom: bool,
    pub final_newline: bool,
    pub dirty: bool,
    pub rev: FileRev,
}

pub struct Document {
    path: Option<PathBuf>,
    tree: PieceTree,
    undo: UndoStack,
    syntax: SyntaxCache,
    rev: FileRev,
    bom: bool,
    dominant_eol: Eol,
    generation: u64,
}

impl Document {
    pub fn new_untitled() -> Self {
        Self {
            path: None,
            tree: PieceTree::new_empty(),
            undo: UndoStack::new(),
            syntax: SyntaxCache::new(Language::PLAIN_TEXT),
            rev: FileRev::default(),
            bom: false,
            dominant_eol: Eol::Lf,
            generation: 1,
        }
    }

    pub fn from_str(text: &str, path: Option<PathBuf>) -> Self {
        let lang = Language::from_path(path.as_deref());
        let tree = PieceTree::from_str(text);
        let eol = infer_eol(tree.base_eol_counts());
        Self {
            path,
            tree,
            undo: UndoStack::new(),
            syntax: SyntaxCache::new(lang),
            rev: FileRev::default(),
            bom: false,
            dominant_eol: eol,
            generation: 1,
        }
    }

    /// Construct a document restored from an unsaved Hot Exit draft: starts marked `is_dirty() == true`
    /// while preserving its original file path, optimistic `base_rev`, BOM, and dominant EOL.
    pub fn from_unsaved_draft(
        text: &str,
        path: Option<PathBuf>,
        base_rev: FileRev,
        bom: bool,
        eol: Eol,
    ) -> Self {
        let lang = Language::from_path(path.as_deref());
        let tree = PieceTree::from_str(text);
        Self {
            path,
            tree,
            undo: UndoStack::new_dirty(),
            syntax: SyntaxCache::new(lang),
            rev: base_rev,
            bom,
            dominant_eol: eol,
            generation: 1,
        }
    }

    /// Open a file lazily: O(1) BOM & first-8KB binary check + initial 64 KB index only.
    pub fn open(fs: &dyn FileSystem, path: &Path) -> Result<Self, DocumentError> {
        let canonical = fs.canonicalize(path)?;
        let handle = fs.open_read(&canonical)?;
        let stat = handle.stat()?;

        let mut probe = vec![0u8; BINARY_PROBE_BYTES.min(handle.len() as usize)];
        let probe_len = if probe.is_empty() {
            0
        } else {
            handle.read_at(0, &mut probe)?
        };
        let probe = &probe[..probe_len];

        // Reject UTF-16 BOMs explicitly.
        if probe.len() >= 2 && ((probe[0] == 0xFF && probe[1] == 0xFE) || (probe[0] == 0xFE && probe[1] == 0xFF)) {
            return Err(DocumentError::UnsupportedEncoding("utf-16"));
        }
        // Detect UTF-8 BOM.
        let (bom, start_off) = if probe.len() >= 3 && probe[0] == 0xEF && probe[1] == 0xBB && probe[2] == 0xBF {
            (true, 3u64)
        } else {
            (false, 0u64)
        };
        // Binary check: NUL byte in first 8 KB.
        if let Some(nul_pos) = crate::swar::find_byte(probe, 0) {
            return Err(DocumentError::Binary { offset: nul_pos });
        }

        let tree = PieceTree::from_file_handle(handle, start_off);
        let eol = infer_eol(tree.base_eol_counts());
        let lang = Language::from_path(Some(&canonical));

        Ok(Self {
            path: Some(canonical),
            tree,
            undo: UndoStack::new(),
            syntax: SyntaxCache::new(lang),
            rev: stat.rev,
            bom,
            dominant_eol: eol,
            generation: 1,
        })
    }

    #[inline]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn set_path(&mut self, path: PathBuf) {
        let old_default = Language::from_path(self.path.as_deref());
        let new_default = Language::from_path(Some(path.as_path()));
        self.path = Some(path);
        if old_default != new_default {
            self.set_language(new_default);
        }
    }

    #[inline]
    pub fn file_name(&self) -> &str {
        self.path
            .as_deref()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("Untitled")
    }

    #[inline]
    pub fn rev(&self) -> FileRev {
        self.rev
    }

    /// Update the cached file revision after an editor-initiated metadata change (such as
    /// sidebar rename, which bumps POSIX `ctime` without changing file contents).
    #[inline]
    pub fn set_rev(&mut self, rev: FileRev) {
        self.rev = rev;
    }

    #[inline]
    pub fn has_bom(&self) -> bool {
        self.bom
    }

    #[inline]
    pub fn eol(&self) -> Eol {
        self.dominant_eol
    }

    #[inline]
    pub fn is_dirty(&self) -> bool {
        self.undo.is_dirty()
    }

    #[inline]
    pub fn undo_stack(&self) -> &UndoStack {
        &self.undo
    }

    #[inline]
    pub fn undo_stack_mut(&mut self) -> &mut UndoStack {
        &mut self.undo
    }

    pub fn set_undo_stack(&mut self, undo: UndoStack) {
        self.undo = undo;
    }

    /// Compute a fast content fingerprint for verifying persisted undo bindings:
    /// - Files `<= 1 MiB`: full SHA-256 of document bytes.
    /// - Files `> 1 MiB`: O(1) SHA-256 of `(len || first 64 KiB || last 64 KiB)` so huge
    ///   files remain strictly lazy without full scans.
    pub fn content_fingerprint(&mut self) -> [u8; 32] {
        const FULL_HASH_MAX: u64 = 1024 * 1024;
        const EDGE_BYTES: usize = 64 * 1024;
        let len = self.tree.len_bytes();
        if len <= FULL_HASH_MAX {
            let mut buf = Vec::with_capacity(len as usize);
            if len > 0 {
                self.tree.read_range(0, len as usize, &mut buf);
            }
            editor_platform::sha256_bytes(&buf)
        } else {
            let mut buf = Vec::with_capacity(8 + EDGE_BYTES * 2);
            buf.extend_from_slice(&len.to_le_bytes());
            self.tree.read_range(0, EDGE_BYTES, &mut buf);
            self.tree
                .read_range(len - EDGE_BYTES as u64, EDGE_BYTES, &mut buf);
            editor_platform::sha256_bytes(&buf)
        }
    }

    /// Free cached base-file pages when parking this document in an inactive tab.
    pub fn evict_page_cache(&self) {
        self.tree.evict_page_cache();
    }

    #[inline]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    #[inline]
    pub fn language(&self) -> Language {
        self.syntax.language()
    }

    pub fn set_language(&mut self, language: Language) {
        if self.syntax.language() == language {
            return;
        }
        self.syntax.set_language(language);
        self.generation += 1;
    }

    pub fn prepare_syntax_to(&mut self, target_line: usize) {
        let tree = &mut self.tree;
        self.syntax
            .catch_up_to(target_line, |l| tree.line_content(l));
    }

    #[inline]
    pub fn len_bytes(&self) -> u64 {
        self.tree.len_bytes()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.tree.is_empty()
    }

    #[inline]
    pub fn is_fully_indexed(&self) -> bool {
        self.tree.is_fully_indexed()
    }

    #[inline]
    pub fn editable_line_count(&self) -> usize {
        self.tree.editable_line_count()
    }

    #[inline]
    pub fn estimated_editable_line_count(&self) -> usize {
        self.tree.estimated_editable_line_count()
    }

    pub fn exact_line_count(&mut self) -> usize {
        self.tree.exact_line_count()
    }

    pub fn tree(&self) -> &PieceTree {
        &self.tree
    }

    pub fn tree_mut(&mut self) -> &mut PieceTree {
        &mut self.tree
    }

    pub fn info(&mut self) -> DocumentInfo {
        let line_count = self.tree.exact_line_count();
        let final_newline = self.tree.final_newline();
        let bom_len = if self.bom { 3 } else { 0 };
        DocumentInfo {
            path: self.path.clone(),
            bytes: self.tree.len_bytes() + bom_len,
            line_count,
            eol: infer_eol(self.tree.base_eol_counts()),
            bom: self.bom,
            final_newline,
            dirty: self.is_dirty(),
            rev: self.rev,
        }
    }

    pub fn line_content(&mut self, line: usize) -> Option<String> {
        self.tree.line_content(line)
    }

    pub fn read_line_window(
        &mut self,
        line: usize,
        rel_byte_start: u64,
        max_bytes: usize,
    ) -> Option<(u64, u64, String)> {
        self.tree.read_line_window(line, rel_byte_start, max_bytes)
    }

    pub fn line_byte_len(&mut self, line: usize) -> Option<u64> {
        self.tree.line_byte_len(line)
    }

    /// Multi-line matching bracket lookup around `caret_byte` (checks byte at caret or caret-1,
    /// scans up to 250 lines outward so huge files never stall).
    pub fn find_matching_bracket(&mut self, caret_byte: u64) -> Option<(u64, u64)> {
        let total = self.tree.len_bytes();
        if total == 0 {
            return None;
        }
        let candidates = [
            (caret_byte < total).then_some(caret_byte),
            (caret_byte > 0).then(|| caret_byte - 1),
        ];
        for cand in candidates.into_iter().flatten() {
            let b = self.read_range(cand, 1);
            let Some(&ch) = b.first() else { continue };
            let (open, close, forward) = match ch {
                b'(' => (b'(', b')', true),
                b'[' => (b'[', b']', true),
                b'{' => (b'{', b'}', true),
                b')' => (b')', b'(', false),
                b']' => (b']', b'[', false),
                b'}' => (b'}', b'{', false),
                _ => continue,
            };
            const MAX_SCAN: u64 = 32 * 1024;
            let mut depth = 0i32;
            if forward {
                let len = (total - cand).min(MAX_SCAN) as usize;
                let bytes = self.read_range(cand, len);
                for (i, &c) in bytes.iter().enumerate() {
                    if c == open {
                        depth += 1;
                    } else if c == close {
                        depth -= 1;
                        if depth == 0 {
                            return Some((cand, cand + i as u64));
                        }
                    }
                }
            } else {
                let start = cand.saturating_sub(MAX_SCAN);
                let len = (cand - start + 1) as usize;
                let bytes = self.read_range(start, len);
                for i in (0..bytes.len()).rev() {
                    let c = bytes[i];
                    if c == open {
                        depth += 1;
                    } else if c == close {
                        depth -= 1;
                        if depth == 0 {
                            return Some((start + i as u64, cand));
                        }
                    }
                }
            }
        }
        None
    }

    /// Check whether the file on disk was modified externally:
    /// - unchanged -> `Unchanged`
    /// - changed + clean -> silently reloads and returns `ReloadedClean`
    /// - changed + dirty -> keeps unsaved edits and returns `StaleDirty`
    pub fn check_identity(&mut self, fs: &dyn FileSystem) -> IdentityCheck {
        let Some(path) = self.path.clone() else {
            return IdentityCheck::Unchanged;
        };
        let stat = match fs.stat(&path) {
            Ok(s) => s,
            Err(_) => return IdentityCheck::Missing,
        };
        if stat.rev == self.rev {
            return IdentityCheck::Unchanged;
        }
        if self.is_dirty() {
            IdentityCheck::StaleDirty
        } else if self.reload(fs).is_ok() {
            IdentityCheck::ReloadedClean
        } else {
            IdentityCheck::StaleDirty
        }
    }

    pub fn line_byte_range(&mut self, line: usize) -> Option<(u64, u64, u64)> {
        self.tree.line_byte_range(line)
    }

    pub fn byte_to_line_col(&mut self, byte_offset: u64) -> (usize, usize) {
        self.tree.byte_to_line_col(byte_offset)
    }

    pub fn read_range(&mut self, offset: u64, len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len.min(1024 * 1024));
        self.tree.read_range(offset, len, &mut out);
        out
    }

    pub fn to_string_lossy(&mut self) -> String {
        self.tree.to_string_lossy()
    }

    pub fn lex_line(&mut self, line_idx: usize, line_text: &str, out: &mut Vec<TokenSpan>) {
        self.syntax.lex_line(line_idx, line_text, out);
    }

    /// Normalize line endings in inserted text to the document's dominant EOL.
    fn normalize_insert_eol(&self, text: &str) -> Vec<u8> {
        let eol_bytes = self.dominant_eol.dominant_bytes();
        if eol_bytes == b"\n" && !text.contains('\r') {
            return text.as_bytes().to_vec();
        }
        let mut out = Vec::with_capacity(text.len());
        let bytes = text.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'\r' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                    i += 2;
                } else {
                    i += 1;
                }
                out.extend_from_slice(eol_bytes);
            } else if bytes[i] == b'\n' {
                i += 1;
                out.extend_from_slice(eol_bytes);
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        out
    }

    /// Apply a single edit `[offset, offset + removed_len)` -> `inserted_text`.
    /// Returns the new byte offset right after the inserted text.
    pub fn edit(
        &mut self,
        offset: u64,
        removed_len: u64,
        inserted_text: &str,
        timestamp_ms: u64,
        coalescable: bool,
    ) -> Result<u64, DocumentError> {
        let doc_len = self.tree.len_bytes();
        if offset > doc_len || offset.saturating_add(removed_len) > doc_len {
            return Err(DocumentError::Range(format!(
                "edit range {}..{} exceeds document length {}",
                offset,
                offset.saturating_add(removed_len),
                doc_len
            )));
        }
        let inserted = self.normalize_insert_eol(inserted_text);
        if removed_len == 0 && inserted.is_empty() {
            return Ok(offset);
        }

        // Ensure lazy base indexing covers the full edited range BEFORE sampling `lines_before`
        // so newly indexed base lines never inflate `delta_lines` and desync `SyntaxCache`.
        let (start_line, _) = self.tree.byte_to_line_col(offset);
        if removed_len > 0 {
            let _ = self.tree.byte_to_line_col(offset + removed_len);
        }
        let lines_before = self.tree.editable_line_count();
        let deleted = if removed_len > 0 {
            self.tree.delete(offset, removed_len)
        } else {
            Vec::new()
        };
        if !inserted.is_empty() {
            self.tree.insert(offset, &inserted);
        }
        let after = offset + inserted.len() as u64;
        let delta_lines = self.tree.editable_line_count() as isize - lines_before as isize;

        self.undo.record(
            EditOp {
                offset,
                deleted,
                inserted,
                cursor_before: offset + removed_len,
                cursor_after: after,
            },
            timestamp_ms,
            coalescable,
        );
        self.syntax.notify_edit(start_line, delta_lines);
        self.generation += 1;
        Ok(after)
    }

    /// Apply multiple non-overlapping edits `(offset, removed_len, text)` in a single atomic
    /// undo/redo step (used by multi-cursor editing).
    ///
    /// `edits` must be sorted in ascending `offset` order with non-overlapping ranges.
    /// Returns the post-edit end byte offset for each edit in the same ascending order.
    pub fn edit_multi(
        &mut self,
        edits: &[(u64, u64, String)],
        timestamp_ms: u64,
    ) -> Result<Vec<u64>, DocumentError> {
        if edits.is_empty() {
            return Ok(Vec::new());
        }
        let doc_len = self.len_bytes();
        let mut prev_end = 0u64;
        let mut normalized: Vec<(u64, u64, Vec<u8>)> = Vec::with_capacity(edits.len());
        for (offset, removed_len, text) in edits {
            if *offset < prev_end || offset.saturating_add(*removed_len) > doc_len {
                return Err(DocumentError::Range(
                    "multi-edit range out of bounds or overlapping".into(),
                ));
            }
            for &boundary in &[*offset, offset + removed_len] {
                if boundary > 0 && boundary < doc_len {
                    let mut b = Vec::with_capacity(1);
                    self.tree.read_range(boundary, 1, &mut b);
                    if let Some(&byte) = b.first() {
                        if (byte & 0xC0) == 0x80 {
                            return Err(DocumentError::Range(
                                "multi-edit offset is not a UTF-8 boundary".into(),
                            ));
                        }
                    }
                }
            }
            prev_end = offset + removed_len;
            normalized.push((*offset, *removed_len, self.normalize_insert_eol(text)));
        }

        // Compute final post-edit end offsets in ascending order before mutating.
        let mut result_ends = Vec::with_capacity(normalized.len());
        let mut delta: i64 = 0;
        for (offset, removed_len, inserted) in &normalized {
            let new_start = (*offset as i64 + delta).max(0) as u64;
            let new_end = new_start + inserted.len() as u64;
            result_ends.push(new_end);
            delta += inserted.len() as i64 - *removed_len as i64;
        }

        // Apply back-to-front so earlier offsets stay exact.
        let mut ops = Vec::with_capacity(normalized.len());
        let mut earliest_line = usize::MAX;
        for (offset, removed_len, inserted) in normalized.into_iter().rev() {
            let (line, _) = self.tree.byte_to_line_col(offset);
            earliest_line = earliest_line.min(line);
            let deleted = if removed_len > 0 {
                self.tree.delete(offset, removed_len)
            } else {
                Vec::new()
            };
            if !inserted.is_empty() {
                self.tree.insert(offset, &inserted);
            }
            let after = offset + inserted.len() as u64;
            ops.push(EditOp {
                offset,
                deleted,
                inserted,
                cursor_before: offset + removed_len,
                cursor_after: after,
            });
        }

        self.undo.record_batch(ops, timestamp_ms);
        if earliest_line != usize::MAX {
            self.syntax.invalidate_from(earliest_line);
        }
        self.generation += 1;
        Ok(result_ends)
    }

    /// Replace all occurrences of `query` with `replacement` in one atomic undo step.
    /// When `options.regex` is enabled, expands `$0`..`$9`, `$$`, `\n`, `\t` captures per match.
    /// Returns the number of replacements made.
    pub fn replace_all(
        &mut self,
        query: &str,
        replacement: &str,
        options: SearchOptions,
        timestamp_ms: u64,
    ) -> usize {
        let Ok(Some(compiled)) = CompiledQuery::compile(query, options) else {
            return 0;
        };
        let matches = compiled.find_all_in_tree(&mut self.tree, options.max_matches);
        if matches.is_empty() {
            return 0;
        }
        let count = matches.len();
        let is_regex = compiled.is_regex();
        let literal_bytes = if is_regex {
            Vec::new()
        } else {
            self.normalize_insert_eol(replacement)
        };
        let mut scratch = SearchScratch::new();
        let mut ops = Vec::with_capacity(count);
        let mut earliest_line = usize::MAX;

        // Apply from back to front so earlier byte offsets remain valid!
        // Preserve the original leading bytes of the previously replaced match so adjacent
        // regex matches (e.g. `(foo)\B|bar`) see the true pre-replacement `next_ch`.
        let mut ctx_buf = [0u8; 4];
        let mut prev_match_head: Option<(u64, [u8; 4], usize)> = None;
        for m in matches.into_iter().rev() {
            let (line, _) = self.tree.byte_to_line_col(m.start);
            earliest_line = earliest_line.min(line);
            let prev_ch = if is_regex && m.start > 0 {
                let back = m.start.min(4) as usize;
                let n = self.tree.read_into_slice(m.start - back as u64, &mut ctx_buf[..back]);
                crate::search::prev_char_in_bytes(&ctx_buf[..n])
            } else {
                None
            };
            let next_ch = if is_regex {
                let (head_slice, head_len) = match prev_match_head {
                    Some((pm_start, ref pm_buf, pm_len)) if pm_start == m.end && pm_len > 0 => {
                        (pm_buf, pm_len)
                    }
                    _ if m.end < self.tree.len_bytes() => {
                        let fwd = (self.tree.len_bytes() - m.end).min(4) as usize;
                        let n = self.tree.read_into_slice(m.end, &mut ctx_buf[..fwd]);
                        (&ctx_buf, n)
                    }
                    _ => (&ctx_buf, 0),
                };
                if head_len >= 2 && head_slice[0] == b'\r' && head_slice[1] == b'\n' {
                    // Normalize CRLF lookahead to `\n` so `$` anchors match in single-char context.
                    Some('\n')
                } else {
                    crate::search::next_char_in_bytes(&head_slice[..head_len])
                }
            } else {
                None
            };
            let deleted = self.tree.delete(m.start, m.end - m.start);
            if is_regex && !deleted.is_empty() {
                let mut h = [0u8; 4];
                let n = deleted.len().min(4);
                h[..n].copy_from_slice(&deleted[..n]);
                prev_match_head = Some((m.start, h, n));
            } else {
                prev_match_head = None;
            }
            let inserted_bytes = if is_regex {
                let matched_str = String::from_utf8_lossy(&deleted);
                let expanded = compiled.expand_replacement_with_context(
                    &matched_str,
                    prev_ch,
                    next_ch,
                    replacement,
                    &mut scratch,
                );
                self.normalize_insert_eol(&expanded)
            } else {
                literal_bytes.clone()
            };
            if !inserted_bytes.is_empty() {
                self.tree.insert(m.start, &inserted_bytes);
            }
            ops.push(EditOp {
                offset: m.start,
                deleted,
                inserted: inserted_bytes,
                cursor_before: m.end,
                cursor_after: m.start + (ops.last().map_or(0, |_| 0)),
            });
            if let Some(op) = ops.last_mut() {
                op.cursor_after = m.start + op.inserted.len() as u64;
            }
        }
        self.undo.record_batch(ops, timestamp_ms);
        if earliest_line != usize::MAX {
            self.syntax.invalidate_from(earliest_line);
        }
        self.generation += 1;
        count
    }

    /// Expand `replacement` for a single match `m` (literal verbatim, or `$0`..`$9` in regex mode).
    pub fn expand_replacement_at(
        &mut self,
        query: &str,
        options: SearchOptions,
        m: SearchMatch,
        replacement: &str,
    ) -> String {
        if !options.regex {
            return replacement.to_string();
        }
        let Ok(Some(compiled)) = CompiledQuery::compile(query, options) else {
            return replacement.to_string();
        };
        let mut buf = Vec::with_capacity((m.end - m.start) as usize);
        self.tree.read_range(m.start, (m.end - m.start) as usize, &mut buf);
        let matched = String::from_utf8_lossy(&buf);
        let mut ctx_buf = [0u8; 4];
        let prev_ch = if m.start > 0 {
            let back = m.start.min(4) as usize;
            let n = self.tree.read_into_slice(m.start - back as u64, &mut ctx_buf[..back]);
            crate::search::prev_char_in_bytes(&ctx_buf[..n])
        } else {
            None
        };
        let next_ch = if m.end < self.tree.len_bytes() {
            let fwd = (self.tree.len_bytes() - m.end).min(4) as usize;
            let n = self.tree.read_into_slice(m.end, &mut ctx_buf[..fwd]);
            if n >= 2 && ctx_buf[0] == b'\r' && ctx_buf[1] == b'\n' {
                Some('\n')
            } else {
                crate::search::next_char_in_bytes(&ctx_buf[..n])
            }
        } else {
            None
        };
        let mut scratch = SearchScratch::new();
        compiled.expand_replacement_with_context(&matched, prev_ch, next_ch, replacement, &mut scratch)
    }

    pub fn find_all(&mut self, query: &str, options: SearchOptions) -> Vec<SearchMatch> {
        find_all_matches(&mut self.tree, query, options)
    }

    pub fn find_all_checked(
        &mut self,
        query: &str,
        options: SearchOptions,
    ) -> Result<Vec<SearchMatch>, RegexError> {
        find_all_matches_checked(&mut self.tree, query, options)
    }

    pub fn break_undo_coalescing(&mut self) {
        self.undo.break_coalescing();
    }

    pub fn can_undo(&self) -> bool {
        self.undo.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.undo.can_redo()
    }

    /// Undo the last logical edit step, returning the restored cursor byte offset.
    /// Verifies each op's bounds and matching bytes before mutating, rolling back atomically
    /// if a stale persisted undo op ever disagrees with document content.
    pub fn undo(&mut self) -> Option<u64> {
        let ops = self.undo.pop_undo()?;
        let mut cursor = 0u64;
        let mut earliest_line = usize::MAX;
        let mut verify = Vec::new();
        let mut applied = 0usize;
        // Invert ops in reverse order of application.
        for op in ops.iter().rev() {
            let ins_len = op.inserted.len() as u64;
            let valid = op.offset.checked_add(ins_len).is_some_and(|end| end <= self.tree.len_bytes())
                && if op.inserted.is_empty() {
                    true
                } else {
                    verify.clear();
                    self.tree.read_range(op.offset, op.inserted.len(), &mut verify);
                    verify.as_slice() == op.inserted.as_slice()
                };
            if !valid {
                // Roll back already-inverted ops in forward order so the document stays untouched.
                let start = ops.len() - applied;
                for prev in &ops[start..] {
                    if !prev.deleted.is_empty() {
                        self.tree.delete(prev.offset, prev.deleted.len() as u64);
                    }
                    if !prev.inserted.is_empty() {
                        self.tree.insert(prev.offset, &prev.inserted);
                    }
                }
                self.undo.clear_preserving_dirty();
                return None;
            }
            let (line, _) = self.tree.byte_to_line_col(op.offset);
            earliest_line = earliest_line.min(line);
            if !op.inserted.is_empty() {
                self.tree.delete(op.offset, ins_len);
            }
            if !op.deleted.is_empty() {
                self.tree.insert(op.offset, &op.deleted);
            }
            cursor = op.cursor_before;
            applied += 1;
        }
        if earliest_line != usize::MAX {
            self.syntax.invalidate_from(earliest_line);
        }
        if !self.undo.is_dirty() {
            self.tree.set_clean();
        }
        self.generation += 1;
        Some(cursor)
    }

    /// Redo the last undone step, returning the restored cursor byte offset.
    /// Verifies bounds and matching bytes atomically, rolling back cleanly on any mismatch.
    pub fn redo(&mut self) -> Option<u64> {
        let ops = self.undo.pop_redo()?;
        let mut cursor = 0u64;
        let mut earliest_line = usize::MAX;
        let mut verify = Vec::new();
        let mut applied = 0usize;
        for op in ops.iter() {
            let del_len = op.deleted.len() as u64;
            let valid = op.offset.checked_add(del_len).is_some_and(|end| end <= self.tree.len_bytes())
                && if op.deleted.is_empty() {
                    true
                } else {
                    verify.clear();
                    self.tree.read_range(op.offset, op.deleted.len(), &mut verify);
                    verify.as_slice() == op.deleted.as_slice()
                };
            if !valid {
                for prev in ops[..applied].iter().rev() {
                    if !prev.inserted.is_empty() {
                        self.tree.delete(prev.offset, prev.inserted.len() as u64);
                    }
                    if !prev.deleted.is_empty() {
                        self.tree.insert(prev.offset, &prev.deleted);
                    }
                }
                self.undo.clear_preserving_dirty();
                return None;
            }
            let (line, _) = self.tree.byte_to_line_col(op.offset);
            earliest_line = earliest_line.min(line);
            if !op.deleted.is_empty() {
                self.tree.delete(op.offset, del_len);
            }
            if !op.inserted.is_empty() {
                self.tree.insert(op.offset, &op.inserted);
            }
            cursor = op.cursor_after;
            applied += 1;
        }
        if earliest_line != usize::MAX {
            self.syntax.invalidate_from(earliest_line);
        }
        if !self.undo.is_dirty() {
            self.tree.set_clean();
        }
        self.generation += 1;
        Some(cursor)
    }

    /// Validate full document UTF-8 cleanly in streaming 64 KB chunks.
    pub fn validate_utf8(&mut self) -> Result<(), usize> {
        let total = self.tree.len_bytes();
        let mut offset = 0u64;
        let mut carry: Vec<u8> = Vec::new();
        let mut chunk = Vec::with_capacity(64 * 1024 + 4);
        while offset < total {
            chunk.clear();
            chunk.extend_from_slice(&carry);
            carry.clear();
            let take = ((total - offset) as usize).min(64 * 1024);
            self.tree.read_range(offset, take, &mut chunk);
            let chunk_start = offset - (chunk.len() - take) as u64;
            offset += take as u64;

            match std::str::from_utf8(&chunk) {
                Ok(_) => {}
                Err(e) => {
                    let valid = e.valid_up_to();
                    if e.error_len().is_none() && offset < total {
                        // Incomplete multi-byte sequence at chunk boundary; carry forward.
                        carry.extend_from_slice(&chunk[valid..]);
                    } else {
                        return Err((chunk_start as usize) + valid);
                    }
                }
            }
        }
        if !carry.is_empty() {
            if let Err(off) = validate_utf8_offset(&carry) {
                return Err((total as usize) - carry.len() + off);
            }
        }
        Ok(())
    }

    /// Save to the current path, using APFS CoW `clone_file` + `pwrite` on same-length edits
    /// when available, otherwise atomic temp+rename!
    pub fn save(
        &mut self,
        fs: &dyn FileSystem,
        expected_rev: Option<FileRev>,
        in_place: bool,
    ) -> Result<SaveReport, DocumentError> {
        let path = self.path.clone().ok_or(DocumentError::NoPath)?;
        self.save_to_path(fs, &path, expected_rev, in_place, true, false)
    }

    /// Force-save to the current path, intentionally bypassing the optimistic `self.rev` check
    /// after the user explicitly confirmed overwriting external disk changes.
    pub fn save_force(
        &mut self,
        fs: &dyn FileSystem,
        in_place: bool,
    ) -> Result<SaveReport, DocumentError> {
        let path = self.path.clone().ok_or(DocumentError::NoPath)?;
        self.save_to_path(fs, &path, None, in_place, true, true)
    }

    /// Save to a new path and update the document's open path and language.
    pub fn save_as(
        &mut self,
        fs: &dyn FileSystem,
        dst: &Path,
        expected_rev: Option<FileRev>,
        in_place: bool,
    ) -> Result<SaveReport, DocumentError> {
        let canonical = fs.canonicalize(dst)?;
        let same_target = self.path.as_deref() == Some(canonical.as_path());
        self.save_to_path(fs, &canonical, expected_rev, in_place, same_target, false)
    }

    fn save_to_path(
        &mut self,
        fs: &dyn FileSystem,
        target: &Path,
        expected_rev: Option<FileRev>,
        in_place: bool,
        same_target: bool,
        force_overwrite: bool,
    ) -> Result<SaveReport, DocumentError> {
        // Hold exclusive advisory lockfile for the entire check + write + commit critical section.
        let _lock = fs.lock_file(target)?;

        // Check optimistic concurrency if requested or if saving back to our existing file.
        if let Some(exp) = expected_rev {
            let actual = fs.stat(target)?.rev;
            if actual != exp {
                return Err(DocumentError::Conflict {
                    expected: exp,
                    actual,
                });
            }
        } else if !force_overwrite && same_target && self.rev != FileRev::default() {
            if let Ok(actual) = fs.stat(target) {
                if actual.rev != self.rev {
                    return Err(DocumentError::Conflict {
                        expected: self.rev,
                        actual: actual.rev,
                    });
                }
            }
        }

        if same_target && !self.is_dirty() && self.rev != FileRev::default() {
            return Ok(SaveReport {
                method: SaveMethod::None,
                bytes_written: 0,
                rev: self.rev,
            });
        }

        let bom = self.bom;
        let bom_len = if bom { 3 } else { 0 };

        // Only allow CoW / delta fast paths when `self.tree` is backed directly by the target file
        // on disk AND we are not force-overwriting external disk modifications.
        let can_use_delta = same_target && !force_overwrite && self.tree.is_file_backed();

        // Fast path 1a: Same-length APFS CoW clone + pwrite (O(changed) even on 5 GB files!).
        if can_use_delta && !in_place {
            if let Some(spans) = self.tree.same_length_changed_spans(bom_len) {
                let parent = target.parent().unwrap_or_else(|| Path::new("."));
                let name = target
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("file");
                let tmp = parent.join(format!(
                    ".{name}.editor-cow-{}-{}",
                    std::process::id(),
                    self.generation
                ));
                if fs.clone_file(target, &tmp).is_ok() {
                    let slices: Vec<(u64, &[u8])> =
                        spans.iter().map(|(off, b)| (*off, b.as_slice())).collect();
                    let written: u64 = spans.iter().map(|(_, b)| b.len() as u64).sum();
                    let write_res = fs.write_in_place(&tmp, &slices, None);
                    if write_res.is_ok() && fs.rename(&tmp, target).is_ok() {
                        let new_stat = fs.stat(target)?;
                        self.rebind_after_save(fs, target, new_stat.rev)?;
                        return Ok(SaveReport {
                            method: SaveMethod::ClonePwrite,
                            bytes_written: written,
                            rev: new_stat.rev,
                        });
                    } else {
                        let _ = fs.remove_file(&tmp);
                    }
                }
            } else if let Some((tail_doc_off, new_doc_len)) = self.tree.tail_rewrite_start() {
                // Fast path 1b: Length-changing edit in last <= 25% of file -> APFS clone + tail pwrite + ftruncate!
                let parent = target.parent().unwrap_or_else(|| Path::new("."));
                let name = target
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("file");
                let tmp = parent.join(format!(
                    ".{name}.editor-tail-{}-{}",
                    std::process::id(),
                    self.generation
                ));
                if fs.clone_file(target, &tmp).is_ok() {
                    let tail_len = (new_doc_len - tail_doc_off) as usize;
                    let tail_bytes = self.read_range(tail_doc_off, tail_len);
                    let raw_off = bom_len + tail_doc_off;
                    let new_raw_len = bom_len + new_doc_len;
                    let write_res =
                        fs.write_in_place(&tmp, &[(raw_off, &tail_bytes)], Some(new_raw_len));
                    if write_res.is_ok() && fs.rename(&tmp, target).is_ok() {
                        let new_stat = fs.stat(target)?;
                        self.rebind_after_save(fs, target, new_stat.rev)?;
                        return Ok(SaveReport {
                            method: SaveMethod::ClonePwriteTail,
                            bytes_written: tail_bytes.len() as u64,
                            rev: new_stat.rev,
                        });
                    } else {
                        let _ = fs.remove_file(&tmp);
                    }
                }
            }
        }

        // Fast path 2: Explicit `--in-place` same-length write.
        if in_place {
            if can_use_delta {
                if let Some(spans) = self.tree.same_length_changed_spans(bom_len) {
                    let slices: Vec<(u64, &[u8])> =
                        spans.iter().map(|(off, b)| (*off, b.as_slice())).collect();
                    let written: u64 = spans.iter().map(|(_, b)| b.len() as u64).sum();
                    let new_stat = fs.write_in_place(target, &slices, None)?;
                    self.rebind_after_save(fs, target, new_stat.rev)?;
                    return Ok(SaveReport {
                        method: SaveMethod::InPlace,
                        bytes_written: written,
                        rev: new_stat.rev,
                    });
                }
            }
            let mut full = Vec::with_capacity((self.tree.len_bytes() + bom_len) as usize);
            let written = self
                .tree
                .write_to(bom, &mut full)
                .map_err(|e| DocumentError::Fs(FsError::Io(e.to_string())))?;
            let new_stat = fs.write_in_place(target, &[(0, &full)], Some(full.len() as u64))?;
            self.rebind_after_save(fs, target, new_stat.rev)?;
            return Ok(SaveReport {
                method: SaveMethod::InPlace,
                bytes_written: written,
                rev: new_stat.rev,
            });
        }

        // Standard safe path: Atomic temp file + fsync + rename!
        let mut bytes_written = 0u64;
        let tree_ref = &mut self.tree;
        let new_stat = fs.write_atomic(target, &mut |w| {
            bytes_written = tree_ref.write_to(bom, w)?;
            Ok(())
        })?;

        self.rebind_after_save(fs, target, new_stat.rev)?;
        Ok(SaveReport {
            method: SaveMethod::AtomicRewrite,
            bytes_written,
            rev: new_stat.rev,
        })
    }

    fn rebind_after_save(
        &mut self,
        fs: &dyn FileSystem,
        target: &Path,
        new_rev: FileRev,
    ) -> Result<(), DocumentError> {
        let canonical = fs.canonicalize(target).unwrap_or_else(|_| target.to_path_buf());
        let handle = fs.open_read(&canonical)?;
        let start_off = if self.bom { 3 } else { 0 };
        self.tree = PieceTree::from_file_handle(handle, start_off);
        self.tree.set_clean();
        let same_path = self.path.as_deref() == Some(canonical.as_path());
        self.path = Some(canonical.clone());
        self.rev = new_rev;
        if !same_path {
            self.syntax.set_language(Language::from_path(Some(&canonical)));
        }
        self.undo.mark_saved();
        self.generation += 1;
        Ok(())
    }

    /// Reload from disk (discards unsaved edits and undo stack).
    pub fn reload(&mut self, fs: &dyn FileSystem) -> Result<(), DocumentError> {
        let path = self.path.clone().ok_or(DocumentError::NoPath)?;
        let prev_lang = self.language();
        let mut reloaded = Document::open(fs, &path)?;
        reloaded.set_language(prev_lang);
        let gen = self.generation + 1;
        *self = reloaded;
        self.generation = gen;
        Ok(())
    }
}

fn infer_eol(counts: crate::swar::LineBreakCounts) -> Eol {
    let kinds = (counts.lf > 0) as u8 + (counts.crlf > 0) as u8 + (counts.cr > 0) as u8;
    if kinds == 0 {
        return Eol::Lf;
    }
    if kinds > 1 {
        return Eol::Mixed;
    }
    if counts.crlf > 0 {
        Eol::CrLf
    } else if counts.cr > 0 {
        Eol::Cr
    } else {
        Eol::Lf
    }
}

/// Convenience helper for wrapping a `Arc<dyn FileSystem>` or checking bracket matches.
pub fn find_matching_bracket(line: &str, byte_offset: usize) -> Option<usize> {
    let bytes = line.as_bytes();
    if byte_offset >= bytes.len() {
        return None;
    }
    let (open, close, forward) = match bytes[byte_offset] {
        b'(' => (b'(', b')', true),
        b'[' => (b'[', b']', true),
        b'{' => (b'{', b'}', true),
        b')' => (b')', b'(', false),
        b']' => (b']', b'[', false),
        b'}' => (b'}', b'{', false),
        _ => return None,
    };
    let mut depth = 0i32;
    if forward {
        for (i, &b) in bytes.iter().enumerate().skip(byte_offset) {
            if b == open {
                depth += 1;
            } else if b == close {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
        }
    } else {
        for i in (0..=byte_offset).rev() {
            if bytes[i] == open {
                depth += 1;
            } else if bytes[i] == close {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
        }
    }
    None
}

// Keep `Arc` imported cleanly for helper ergonomics if needed.
#[allow(dead_code)]
fn _assert_send_sync(_: Arc<dyn FileSystem>) {}
