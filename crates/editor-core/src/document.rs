//! High-level `Document` combining `PieceTree`, `UndoStack`, EOL/BOM rules,
//! incremental syntax cache, and atomic / APFS CoW persistence.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use editor_platform::{FileRev, FileSystem, FsError};

use crate::grapheme::validate_utf8_offset;
use crate::piece_tree::PieceTree;
use crate::search::{find_all_matches, SearchMatch, SearchOptions};
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
        let lang = Language::from_path(Some(path.as_path()));
        self.path = Some(path);
        self.set_language(lang);
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
    pub fn generation(&self) -> u64 {
        self.generation
    }

    #[inline]
    pub fn language(&self) -> Language {
        self.syntax.language()
    }

    pub fn set_language(&mut self, language: Language) {
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

        let (start_line, _) = self.tree.byte_to_line_col(offset);
        let deleted = if removed_len > 0 {
            self.tree.delete(offset, removed_len)
        } else {
            Vec::new()
        };
        if !inserted.is_empty() {
            self.tree.insert(offset, &inserted);
        }
        let after = offset + inserted.len() as u64;

        let added_nl = inserted.iter().filter(|&&b| b == b'\n').count() as isize;
        let removed_nl = deleted.iter().filter(|&&b| b == b'\n').count() as isize;
        let delta_lines = added_nl - removed_nl;

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

    /// Replace all occurrences of `query` with `replacement` in one atomic undo step.
    /// Returns the number of replacements made.
    pub fn replace_all(
        &mut self,
        query: &str,
        replacement: &str,
        options: SearchOptions,
        timestamp_ms: u64,
    ) -> usize {
        let matches = find_all_matches(&mut self.tree, query, options);
        if matches.is_empty() {
            return 0;
        }
        let count = matches.len();
        let inserted_bytes = self.normalize_insert_eol(replacement);
        let mut ops = Vec::with_capacity(count);
        let mut earliest_line = usize::MAX;

        // Apply from back to front so earlier byte offsets remain valid!
        for m in matches.into_iter().rev() {
            let (line, _) = self.tree.byte_to_line_col(m.start);
            earliest_line = earliest_line.min(line);
            let deleted = self.tree.delete(m.start, m.end - m.start);
            if !inserted_bytes.is_empty() {
                self.tree.insert(m.start, &inserted_bytes);
            }
            ops.push(EditOp {
                offset: m.start,
                deleted,
                inserted: inserted_bytes.clone(),
                cursor_before: m.end,
                cursor_after: m.start + inserted_bytes.len() as u64,
            });
        }
        self.undo.record_batch(ops, timestamp_ms);
        if earliest_line != usize::MAX {
            self.syntax.invalidate_from(earliest_line);
        }
        self.generation += 1;
        count
    }

    pub fn find_all(&mut self, query: &str, options: SearchOptions) -> Vec<SearchMatch> {
        find_all_matches(&mut self.tree, query, options)
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
    pub fn undo(&mut self) -> Option<u64> {
        let ops = self.undo.pop_undo()?;
        let mut cursor = 0u64;
        let mut earliest_line = usize::MAX;
        // Invert ops in reverse order of application.
        for op in ops.iter().rev() {
            let (line, _) = self.tree.byte_to_line_col(op.offset);
            earliest_line = earliest_line.min(line);
            if !op.inserted.is_empty() {
                self.tree.delete(op.offset, op.inserted.len() as u64);
            }
            if !op.deleted.is_empty() {
                self.tree.insert(op.offset, &op.deleted);
            }
            cursor = op.cursor_before;
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
    pub fn redo(&mut self) -> Option<u64> {
        let ops = self.undo.pop_redo()?;
        let mut cursor = 0u64;
        let mut earliest_line = usize::MAX;
        for op in ops.iter() {
            let (line, _) = self.tree.byte_to_line_col(op.offset);
            earliest_line = earliest_line.min(line);
            if !op.deleted.is_empty() {
                self.tree.delete(op.offset, op.deleted.len() as u64);
            }
            if !op.inserted.is_empty() {
                self.tree.insert(op.offset, &op.inserted);
            }
            cursor = op.cursor_after;
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
        self.save_to_path(fs, &path, expected_rev, in_place, true)
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
        self.save_to_path(fs, &canonical, expected_rev, in_place, same_target)
    }

    fn save_to_path(
        &mut self,
        fs: &dyn FileSystem,
        target: &Path,
        expected_rev: Option<FileRev>,
        in_place: bool,
        same_target: bool,
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
        } else if same_target && self.rev != FileRev::default() {
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

        // Fast path 1a: Same-length APFS CoW clone + pwrite (O(changed) even on 5 GB files!).
        if same_target && !in_place {
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
            if same_target {
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
        self.path = Some(canonical.clone());
        self.rev = new_rev;
        self.syntax.set_language(Language::from_path(Some(&canonical)));
        self.undo.mark_saved();
        self.generation += 1;
        Ok(())
    }

    /// Reload from disk (discards unsaved edits and undo stack).
    pub fn reload(&mut self, fs: &dyn FileSystem) -> Result<(), DocumentError> {
        let path = self.path.clone().ok_or(DocumentError::NoPath)?;
        let reloaded = Document::open(fs, &path)?;
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
