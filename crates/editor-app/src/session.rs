//! Bounded per-folder session persistence (open tabs & split panes with exact views) and
//! crash-safe Hot Exit draft backups (zero external dependencies, cross-platform).
//!
//! Design:
//! - One shared [`SavedViewPos`] (7 `Copy` integers) represents exact cursor, selection, and
//!   pixel scroll across open tabs, split panes, and Hot Exit drafts.
//! - [`FolderSession`] (stored compactly in host prefs under `FolderSessions`, capped at 12
//!   recent folders + `@standalone`) attaches `SavedViewPos` directly to open tabs (`SavedTab`)
//!   and visible panes (`SavedPane { col, row, tab }`). Rebuilt declaratively from live open tabs
//!   so closing a tab drops its view immediately with zero side-tables and zero disk pileup.
//! - [`DraftStore`] persists unsaved dirty buffers (`<= 16 MiB` each, `64 MiB` total) inside
//!   `state_dir/backups/` using immutable versioned files (`d<id>_r<rev>.buf`), SHA-256 + length
//!   verification, atomic `index.tsv` commits, `loaded_ok`-guarded orphan GC, and quarantine for
//!   corrupt `.buf` files. Leaves zero files on disk once all buffers are clean.

use std::path::{Path, PathBuf};

use editor_core::{Eol, UndoStack};
use editor_platform::{
    ensure_private_dir, set_private_file_permissions, sha256_bytes, sha256_hex, EntryKind, FileRev,
    FileSystem, FsError,
};

pub const MAX_RECENT_FOLDERS: usize = 12;
pub const DEFAULT_RATIO_BP: u16 = 5000;
/// Maximum size of a single dirty buffer eligible for automatic Hot Exit backup (16 MiB).
pub const MAX_BACKUP_BYTES: u64 = 16 * 1024 * 1024;
/// Maximum combined byte budget across all Hot Exit drafts (64 MiB).
pub const MAX_TOTAL_BACKUP_BYTES: u64 = 64 * 1024 * 1024;
/// Maximum undo/redo steps persisted per open tab.
pub const MAX_UNDO_PERSIST_STEPS: usize = 256;
/// Maximum serialized undo payload bytes per open tab (512 KiB).
pub const MAX_UNDO_PERSIST_BYTES: usize = 512 * 1024;
/// Maximum combined bytes across all persisted `.undo` files (8 MiB).
pub const MAX_TOTAL_UNDO_BYTES: u64 = 8 * 1024 * 1024;

const STANDALONE_FOLDER_TOKEN: &str = "@standalone";
const DRAFT_REF_PREFIX: &str = "@draft:";

/// Shared exact cursor, selection, and pixel-scroll snapshot (zero floats, `Copy + Eq`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SavedViewPos {
    pub line: usize,
    pub col: usize,
    pub anchor_line: usize,
    pub anchor_col: usize,
    pub scroll_top_line: usize,
    pub scroll_sub_y_tenths: u32,
    pub scroll_x_tenths: u32,
}

impl SavedViewPos {
    pub fn is_default(self) -> bool {
        self == Self::default()
    }

    pub fn from_pixels(
        line: usize,
        col: usize,
        anchor_line: usize,
        anchor_col: usize,
        scroll_top_line: usize,
        scroll_sub_y_px: f32,
        scroll_x_px: f32,
    ) -> Self {
        Self {
            line,
            col,
            anchor_line,
            anchor_col,
            scroll_top_line,
            scroll_sub_y_tenths: (scroll_sub_y_px.max(0.0) * 10.0).round() as u32,
            scroll_x_tenths: (scroll_x_px.max(0.0) * 10.0).round() as u32,
        }
    }

    pub fn scroll_sub_y_px(self) -> f32 {
        self.scroll_sub_y_tenths as f32 / 10.0
    }

    pub fn scroll_x_px(self) -> f32 {
        self.scroll_x_tenths as f32 / 10.0
    }

    pub fn encode_into(self, out: &mut String) {
        out.push_str(&format!(
            "{},{},{},{},{},{},{}",
            self.line,
            self.col,
            self.anchor_line,
            self.anchor_col,
            self.scroll_top_line,
            self.scroll_sub_y_tenths,
            self.scroll_x_tenths
        ));
    }

    pub fn decode(s: &str) -> Option<Self> {
        let mut it = s.split(',');
        let line = it.next()?.parse().ok()?;
        let col = it.next()?.parse().ok()?;
        let anchor_line = it.next()?.parse().ok()?;
        let anchor_col = it.next()?.parse().ok()?;
        let scroll_top_line = it.next()?.parse().ok()?;
        let scroll_sub_y_tenths = it.next()?.parse().ok()?;
        let scroll_x_tenths = it.next()?.parse().ok()?;
        if it.next().is_some() {
            return None;
        }
        Some(Self {
            line,
            col,
            anchor_line,
            anchor_col,
            scroll_top_line,
            scroll_sub_y_tenths,
            scroll_x_tenths,
        })
    }
}

/// Identifies a buffer in a saved session: either a real on-disk file or an Untitled draft ID.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SavedBufferRef {
    File(PathBuf),
    Draft(u64),
}

impl SavedBufferRef {
    pub fn as_file(&self) -> Option<&Path> {
        match self {
            SavedBufferRef::File(p) => Some(p.as_path()),
            SavedBufferRef::Draft(_) => None,
        }
    }

    fn encode_into(&self, out: &mut String) {
        match self {
            SavedBufferRef::Draft(id) => {
                out.push_str(DRAFT_REF_PREFIX);
                out.push_str(&id.to_string());
            }
            SavedBufferRef::File(p) => push_escaped_path(out, &p.to_string_lossy()),
        }
    }

    fn decode(raw: &str) -> Option<Self> {
        if raw.is_empty() {
            return None;
        }
        if let Some(rest) = raw.strip_prefix(DRAFT_REF_PREFIX) {
            return Some(SavedBufferRef::Draft(rest.parse::<u64>().ok()?));
        }
        let s = unescape(raw);
        (!s.is_empty()).then(|| SavedBufferRef::File(PathBuf::from(s)))
    }

    fn remap(&mut self, from: &Path, to: &Path, is_dir: bool) -> bool {
        if let SavedBufferRef::File(ref mut p) = self {
            if let Some(next) = remap_one_path(p, from, to, is_dir) {
                *p = next;
                return true;
            }
        }
        false
    }
}

/// A saved open tab with its exact cursor, selection, and scroll position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedTab {
    pub target: SavedBufferRef,
    pub view: SavedViewPos,
}

impl SavedTab {
    fn encode_into(&self, out: &mut String) {
        self.target.encode_into(out);
        if !self.view.is_default() {
            out.push(',');
            self.view.encode_into(out);
        }
    }

    fn decode(raw: &str) -> Option<Self> {
        if raw.is_empty() {
            return None;
        }
        if let Some((raw_target, raw_view)) = raw.split_once(',') {
            let target = SavedBufferRef::decode(raw_target)?;
            let view = SavedViewPos::decode(raw_view)?;
            Some(Self { target, view })
        } else {
            Some(Self {
                target: SavedBufferRef::decode(raw)?,
                view: SavedViewPos::default(),
            })
        }
    }
}

/// A saved visible pane (`col`, `row`, and its `SavedTab` target + view).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedPane {
    pub col: u8,
    pub row: u8,
    pub tab: SavedTab,
}

impl SavedPane {
    fn encode_into(&self, out: &mut String) {
        out.push_str(&format!("{},{},", self.col.min(1), self.row.min(1)));
        self.tab.encode_into(out);
    }

    fn decode(raw: &str) -> Option<Self> {
        let (c_str, rest) = raw.split_once(',')?;
        let (r_str, tab_str) = rest.split_once(',')?;
        let col = c_str.parse::<u8>().ok()?.min(1);
        let row = r_str.parse::<u8>().ok()?.min(1);
        let tab = SavedTab::decode(tab_str)?;
        Some(Self { col, row, tab })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderSession {
    /// Canonical workspace folder path, or empty `PathBuf` for a standalone (no-folder) session.
    pub folder: PathBuf,
    pub pins: Vec<SavedTab>,
    pub open: Vec<SavedTab>,
    /// Visible panes (0 = empty, 1 = single-pane, 2..=4 = split grid).
    pub panes: Vec<SavedPane>,
    /// Index into `panes` of the focused pane.
    pub active_pane_idx: usize,
    /// Column split ratio in basis points (`1500..=8500`, default `5000` = `0.50`).
    pub col_ratio_bp: u16,
    /// Per-column row split ratios in basis points (`1500..=8500`, default `[5000, 5000]`).
    pub row_ratio_bp: [u16; 2],
    /// Whether fullscreen pane zoom was active.
    pub zoomed: bool,
}

impl FolderSession {
    pub fn col_ratio(&self) -> f32 {
        (self.col_ratio_bp.clamp(1500, 8500) as f32) / 10_000.0
    }

    pub fn row_ratios(&self) -> [f32; 2] {
        [
            (self.row_ratio_bp[0].clamp(1500, 8500) as f32) / 10_000.0,
            (self.row_ratio_bp[1].clamp(1500, 8500) as f32) / 10_000.0,
        ]
    }
}

pub fn ratio_to_bp(r: f32) -> u16 {
    (r.clamp(0.15, 0.85) * 10_000.0).round() as u16
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionStore {
    /// MRU-ordered recent folder sessions (`folders[0]` is most recent).
    pub folders: Vec<FolderSession>,
}

impl SessionStore {
    pub fn upsert(&mut self, session: FolderSession) {
        self.folders.retain(|f| f.folder != session.folder);
        self.folders.insert(0, session);
        if self.folders.len() > MAX_RECENT_FOLDERS {
            self.folders.truncate(MAX_RECENT_FOLDERS);
        }
    }

    pub fn remove(&mut self, folder: &Path) -> bool {
        let before = self.folders.len();
        self.folders.retain(|f| f.folder != folder);
        self.folders.len() != before
    }

    pub fn get(&self, folder: &Path) -> Option<&FolderSession> {
        self.folders.iter().find(|f| f.folder == folder)
    }

    pub fn remap_paths(&mut self, from: &Path, to: &Path, is_dir: bool) -> bool {
        let mut changed = false;
        for fs in &mut self.folders {
            if !fs.folder.as_os_str().is_empty() {
                if let Some(next) = remap_one_path(&fs.folder, from, to, is_dir) {
                    fs.folder = next;
                    changed = true;
                }
            }
            for t in &mut fs.pins {
                changed |= t.target.remap(from, to, is_dir);
            }
            for t in &mut fs.open {
                changed |= t.target.remap(from, to, is_dir);
            }
            for p in &mut fs.panes {
                changed |= p.tab.target.remap(from, to, is_dir);
            }
        }
        changed
    }

    /// Encode all folder sessions into a single deterministic 7-column TSV string:
    /// `<folder>\t<pins>\t<open>\t<panes>\t<active_pane_idx>\t<ratios>\t<zoomed>`
    pub fn encode(&self) -> String {
        let mut out = String::new();
        for (idx, fs) in self.folders.iter().take(MAX_RECENT_FOLDERS).enumerate() {
            if idx > 0 {
                out.push('\n');
            }
            if fs.folder.as_os_str().is_empty() {
                out.push_str(STANDALONE_FOLDER_TOKEN);
            } else {
                push_escaped_path(&mut out, &fs.folder.to_string_lossy());
            }
            out.push('\t');
            for (i, t) in fs.pins.iter().enumerate() {
                if i > 0 {
                    out.push('|');
                }
                t.encode_into(&mut out);
            }
            out.push('\t');
            for (i, t) in fs.open.iter().enumerate() {
                if i > 0 {
                    out.push('|');
                }
                t.encode_into(&mut out);
            }
            out.push('\t');
            for (i, sp) in fs.panes.iter().enumerate() {
                if i > 0 {
                    out.push('|');
                }
                sp.encode_into(&mut out);
            }
            out.push('\t');
            out.push_str(&fs.active_pane_idx.to_string());
            out.push('\t');
            out.push_str(&format!(
                "{},{},{}",
                fs.col_ratio_bp.clamp(1500, 8500),
                fs.row_ratio_bp[0].clamp(1500, 8500),
                fs.row_ratio_bp[1].clamp(1500, 8500),
            ));
            out.push('\t');
            out.push(if fs.zoomed { '1' } else { '0' });
        }
        out
    }

    pub fn decode(raw: &str) -> Self {
        let mut folders = Vec::new();
        for line in raw.lines() {
            if line.is_empty() {
                continue;
            }
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() != 7 {
                continue;
            }
            let folder = if parts[0] == STANDALONE_FOLDER_TOKEN {
                PathBuf::new()
            } else {
                let p = PathBuf::from(unescape(parts[0]));
                if p.as_os_str().is_empty() {
                    continue;
                }
                p
            };
            let pins: Vec<SavedTab> = if parts[1].is_empty() {
                Vec::new()
            } else {
                parts[1].split('|').filter_map(SavedTab::decode).collect()
            };
            let open: Vec<SavedTab> = if parts[2].is_empty() {
                Vec::new()
            } else {
                parts[2].split('|').filter_map(SavedTab::decode).collect()
            };
            let panes: Vec<SavedPane> = if parts[3].is_empty() {
                Vec::new()
            } else {
                parts[3].split('|').filter_map(SavedPane::decode).collect()
            };
            let active_pane_idx = parts[4].parse::<usize>().unwrap_or(0);
            let mut r_it = parts[5].split(',');
            let col_ratio_bp = r_it
                .next()
                .and_then(|v| v.parse::<u16>().ok())
                .unwrap_or(DEFAULT_RATIO_BP)
                .clamp(1500, 8500);
            let r0 = r_it
                .next()
                .and_then(|v| v.parse::<u16>().ok())
                .unwrap_or(DEFAULT_RATIO_BP)
                .clamp(1500, 8500);
            let r1 = r_it
                .next()
                .and_then(|v| v.parse::<u16>().ok())
                .unwrap_or(DEFAULT_RATIO_BP)
                .clamp(1500, 8500);
            let zoomed = parts[6] == "1" || parts[6] == "true";

            folders.push(FolderSession {
                folder,
                pins,
                open,
                panes,
                active_pane_idx,
                col_ratio_bp,
                row_ratio_bp: [r0, r1],
                zoomed,
            });
            if folders.len() >= MAX_RECENT_FOLDERS {
                break;
            }
        }
        Self { folders }
    }
}

// ============================================================================
// Crash-Safe Hot Exit Draft Store (`backups/index.tsv` + `d<id>_r<rev>.buf`)
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftRecord {
    pub id: u64,
    pub rev: u64,
    /// Workspace folder scope for Untitled drafts (`PathBuf::new()` = standalone).
    pub scope_folder: PathBuf,
    /// Canonical path for pre-existing files; `None` for newly created Untitled buffers.
    pub original_path: Option<PathBuf>,
    /// File revision when the pre-existing file was opened/last saved.
    pub base_rev: FileRev,
    pub bom: bool,
    pub eol: Eol,
    pub lang_override: Option<String>,
    pub view: SavedViewPos,
    pub byte_len: usize,
    pub sha256: String,
}

impl DraftRecord {
    pub fn file_name(&self) -> String {
        format!("d{}_r{}.buf", self.id, self.rev)
    }
}

fn parse_eol(s: &str) -> Option<Eol> {
    match s {
        "lf" => Some(Eol::Lf),
        "crlf" => Some(Eol::CrLf),
        "cr" => Some(Eol::Cr),
        "mixed" => Some(Eol::Mixed),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftStore {
    /// `true` iff `index.tsv` was loaded cleanly (or absent). If `false` due to an I/O read
    /// error or partial index corruption, we NEVER overwrite `index.tsv` or run orphan GC so
    /// existing `.buf` backups stay completely safe on disk.
    pub loaded_ok: bool,
    pub next_id: u64,
    pub records: Vec<DraftRecord>,
    /// Filenames of `.buf` files that failed SHA-256/length verification on restore; orphan GC
    /// never deletes these so damaged drafts remain intact for manual recovery without disabling
    /// Hot Exit for healthy buffers.
    pub quarantined_files: Vec<String>,
}

impl Default for DraftStore {
    fn default() -> Self {
        Self {
            loaded_ok: true,
            next_id: 1,
            records: Vec::new(),
            quarantined_files: Vec::new(),
        }
    }
}

impl DraftStore {
    pub fn backups_dir(state_dir: &Path) -> PathBuf {
        state_dir.join("backups")
    }

    pub fn index_path(state_dir: &Path) -> PathBuf {
        Self::backups_dir(state_dir).join("index.tsv")
    }

    pub fn alloc_id(&mut self) -> u64 {
        let id = self.next_id.max(1);
        self.next_id = id + 1;
        id
    }

    pub fn quarantine_file(&mut self, name: String) {
        if !self.quarantined_files.contains(&name) {
            self.quarantined_files.push(name);
        }
    }

    pub fn find_by_id(&self, id: u64) -> Option<&DraftRecord> {
        self.records.iter().find(|r| r.id == id)
    }

    pub fn find_by_path(&self, path: &Path) -> Option<&DraftRecord> {
        self.records
            .iter()
            .find(|r| r.original_path.as_deref() == Some(path))
    }

    pub fn untitled_for_scope(&self, scope: &Path) -> Vec<&DraftRecord> {
        self.records
            .iter()
            .filter(|r| r.original_path.is_none() && r.scope_folder == scope)
            .collect()
    }

    pub fn file_drafts_for_scope(&self, scope: &Path) -> Vec<&DraftRecord> {
        self.records
            .iter()
            .filter(|r| r.original_path.is_some() && r.scope_folder == scope)
            .collect()
    }

    pub fn remap_paths(&mut self, from: &Path, to: &Path, is_dir: bool) -> bool {
        let mut changed = false;
        for rec in &mut self.records {
            if !rec.scope_folder.as_os_str().is_empty() {
                if let Some(next) = remap_one_path(&rec.scope_folder, from, to, is_dir) {
                    rec.scope_folder = next;
                    changed = true;
                }
            }
            if let Some(ref mut p) = rec.original_path {
                if let Some(next) = remap_one_path(p, from, to, is_dir) {
                    *p = next;
                    changed = true;
                }
            }
        }
        changed
    }

    pub fn encode(&self) -> String {
        let mut out = format!("v1\t{}\n", self.next_id);
        for rec in &self.records {
            out.push_str(&rec.id.to_string());
            out.push('\t');
            out.push_str(&rec.rev.to_string());
            out.push('\t');
            if rec.scope_folder.as_os_str().is_empty() {
                out.push_str(STANDALONE_FOLDER_TOKEN);
            } else {
                push_escaped_path(&mut out, &rec.scope_folder.to_string_lossy());
            }
            out.push('\t');
            if let Some(ref p) = rec.original_path {
                push_escaped_path(&mut out, &p.to_string_lossy());
            }
            out.push('\t');
            out.push_str(&format!(
                "{},{},{},{},{}",
                rec.base_rev.dev,
                rec.base_rev.ino,
                rec.base_rev.size,
                rec.base_rev.mtime_ns,
                rec.base_rev.ctime_ns
            ));
            out.push('\t');
            out.push(if rec.bom { '1' } else { '0' });
            out.push('\t');
            out.push_str(rec.eol.as_str());
            out.push('\t');
            if let Some(ref lang) = rec.lang_override {
                push_escaped(&mut out, lang);
            }
            out.push('\t');
            rec.view.encode_into(&mut out);
            out.push('\t');
            out.push_str(&rec.byte_len.to_string());
            out.push('\t');
            out.push_str(&rec.sha256);
            out.push('\n');
        }
        out
    }

    pub fn decode(raw: &str) -> Self {
        if raw.trim().is_empty() {
            return Self::default();
        }
        let mut loaded_ok = true;
        let mut next_id = 1u64;
        let mut records = Vec::new();
        let mut lines = raw.lines();

        match lines.next() {
            Some(header) => {
                if let Some(rest) = header.strip_prefix("v1\t") {
                    if let Ok(n) = rest.parse::<u64>() {
                        next_id = n.max(1);
                    } else {
                        loaded_ok = false;
                    }
                } else {
                    loaded_ok = false;
                }
            }
            None => return Self::default(),
        }

        for line in lines {
            if line.is_empty() {
                continue;
            }
            if let Some(rec) = decode_draft_row(line) {
                next_id = next_id.max(rec.id + 1);
                records.push(rec);
            } else {
                // Keep recovering any valid rows, but refuse destructive GC on a damaged index.
                loaded_ok = false;
            }
        }
        Self {
            loaded_ok,
            next_id,
            records,
            quarantined_files: Vec::new(),
        }
    }

    pub fn load(fs: &dyn FileSystem, state_dir: &Path) -> Self {
        let idx = Self::index_path(state_dir);
        let mut buf = Vec::new();
        match fs.read_prefix_into(&idx, 2 * 1024 * 1024, &mut buf) {
            Ok(Some(_)) => match std::str::from_utf8(&buf) {
                Ok(s) => Self::decode(s),
                Err(_) => Self {
                    loaded_ok: false,
                    next_id: 1,
                    records: Vec::new(),
                    quarantined_files: Vec::new(),
                },
            },
            Ok(None) | Err(FsError::NotFound(_)) => Self::default(),
            Err(_) => Self {
                loaded_ok: false,
                next_id: 1,
                records: Vec::new(),
                quarantined_files: Vec::new(),
            },
        }
    }

    /// Write an immutable snapshot file `d<id>_r<rev>.buf` atomically and return `(byte_len, sha256)`.
    pub fn write_draft_bytes(
        fs: &dyn FileSystem,
        state_dir: &Path,
        id: u64,
        rev: u64,
        bytes: &[u8],
    ) -> Result<(usize, String), String> {
        ensure_private_dir(fs, state_dir).map_err(|e| e.to_string())?;
        let dir = Self::backups_dir(state_dir);
        ensure_private_dir(fs, &dir).map_err(|e| e.to_string())?;
        let path = dir.join(format!("d{id}_r{rev}.buf"));
        fs.write_atomic(&path, &mut |w| w.write_all(bytes))
            .map_err(|e| e.to_string())?;
        set_private_file_permissions(&path);
        Ok((bytes.len(), sha256_hex(bytes)))
    }

    /// Read and cryptographically verify a draft's `.buf` file before restoring.
    pub fn read_verified_text(
        fs: &dyn FileSystem,
        state_dir: &Path,
        rec: &DraftRecord,
    ) -> Result<String, String> {
        if rec.byte_len as u64 > MAX_BACKUP_BYTES {
            return Err("draft exceeds size limit".to_string());
        }
        let path = Self::backups_dir(state_dir).join(rec.file_name());
        let mut buf = Vec::with_capacity(rec.byte_len);
        match fs.read_prefix_into(&path, MAX_BACKUP_BYTES as usize + 1, &mut buf) {
            Ok(Some(total)) => {
                if total as usize != rec.byte_len || buf.len() != rec.byte_len {
                    return Err(format!(
                        "draft {} length mismatch (expected {}, found {})",
                        rec.file_name(),
                        rec.byte_len,
                        total
                    ));
                }
                if sha256_hex(&buf) != rec.sha256 {
                    return Err(format!("draft {} failed SHA-256 verification", rec.file_name()));
                }
                String::from_utf8(buf)
                    .map_err(|_| format!("draft {} contains invalid UTF-8", rec.file_name()))
            }
            Ok(None) | Err(FsError::NotFound(_)) => {
                Err(format!("draft {} missing", rec.file_name()))
            }
            Err(e) => Err(e.to_string()),
        }
    }

    /// Atomically commit `index.tsv`, then delete ONLY unreferenced, non-quarantined
    /// `d<digits>_r<digits>.buf` files. Removes `index.tsv` and empty `backups/` / `state_dir`
    /// when zero drafts remain.
    pub fn commit_and_gc(&self, fs: &dyn FileSystem, state_dir: &Path) -> Result<(), String> {
        if !self.loaded_ok {
            return Err("draft store was not cleanly loaded; refusing to modify backups".into());
        }
        let dir = Self::backups_dir(state_dir);
        let idx = Self::index_path(state_dir);

        if self.records.is_empty() {
            if fs.stat(&idx).is_ok() {
                let _ = fs.remove_file(&idx);
            }
            if let Ok(entries) = fs.read_dir(&dir) {
                for e in entries {
                    if e.kind == EntryKind::File
                        && is_draft_buf_filename(&e.name)
                        && !self.quarantined_files.contains(&e.name)
                    {
                        let _ = fs.remove_file(&e.path);
                    }
                }
                // Best-effort remove empty `backups/` and `state_dir` (OS refuses if non-empty).
                let _ = std::fs::remove_dir(&dir);
                let _ = std::fs::remove_dir(state_dir);
            }
            return Ok(());
        }

        ensure_private_dir(fs, state_dir).map_err(|e| e.to_string())?;
        ensure_private_dir(fs, &dir).map_err(|e| e.to_string())?;
        let encoded = self.encode();
        fs.write_atomic(&idx, &mut |w| w.write_all(encoded.as_bytes()))
            .map_err(|e| e.to_string())?;
        set_private_file_permissions(&idx);

        if let Ok(entries) = fs.read_dir(&dir) {
            for e in entries {
                if e.kind == EntryKind::File
                    && is_draft_buf_filename(&e.name)
                    && !self.quarantined_files.contains(&e.name)
                    && !self.records.iter().any(|r| r.file_name() == e.name)
                {
                    let _ = fs.remove_file(&e.path);
                }
            }
        }
        Ok(())
    }
}

fn decode_draft_row(line: &str) -> Option<DraftRecord> {
    let parts: Vec<&str> = line.split('\t').collect();
    if parts.len() != 11 {
        return None;
    }
    let id = parts[0].parse::<u64>().ok()?;
    let rev = parts[1].parse::<u64>().ok()?;
    let scope_folder = if parts[2] == STANDALONE_FOLDER_TOKEN || parts[2].is_empty() {
        PathBuf::new()
    } else {
        PathBuf::from(unescape(parts[2]))
    };
    let original_path = if parts[3].is_empty() {
        None
    } else {
        Some(PathBuf::from(unescape(parts[3])))
    };
    let mut rv = parts[4].split(',');
    let base_rev = FileRev {
        dev: rv.next()?.parse().ok()?,
        ino: rv.next()?.parse().ok()?,
        size: rv.next()?.parse().ok()?,
        mtime_ns: rv.next()?.parse().ok()?,
        ctime_ns: rv.next()?.parse().ok()?,
    };
    if rv.next().is_some() {
        return None;
    }
    let bom = match parts[5] {
        "1" => true,
        "0" => false,
        _ => return None,
    };
    let eol = parse_eol(parts[6])?;
    let lang_override = (!parts[7].is_empty()).then(|| unescape(parts[7]));
    let view = SavedViewPos::decode(parts[8])?;
    let byte_len = parts[9].parse::<usize>().ok()?;
    let sha256 = parts[10].to_string();
    if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(DraftRecord {
        id,
        rev,
        scope_folder,
        original_path,
        base_rev,
        bom,
        eol,
        lang_override,
        view,
        byte_len,
        sha256,
    })
}

fn is_draft_buf_filename(name: &str) -> bool {
    let Some(stem) = name.strip_prefix('d').and_then(|s| s.strip_suffix(".buf")) else {
        return false;
    };
    let Some((id_part, rev_part)) = stem.split_once("_r") else {
        return false;
    };
    !id_part.is_empty()
        && !rev_part.is_empty()
        && id_part.bytes().all(|b| b.is_ascii_digit())
        && rev_part.bytes().all(|b| b.is_ascii_digit())
}

// ============================================================================
// Self-Contained Open-Tab Undo/Redo Persistence (`state_dir/undo/*.undo`)
// ============================================================================

const UNDO_FILE_MAGIC: &[u8; 4] = b"EDUN";
const UNDO_FILE_VERSION: u8 = 1;
const MAX_UNDO_FILE_BYTES: usize = MAX_UNDO_PERSIST_BYTES + 4096;

/// Verifiable identity binding an on-disk `.undo` snapshot to either a clean file on disk
/// (`FileRev` + byte length + content fingerprint) or an immutable Hot Exit draft (`id`, `rev`, `len`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UndoBinding {
    OnDisk {
        rev: FileRev,
        len: u64,
        fingerprint: [u8; 32],
    },
    Draft {
        id: u64,
        rev: u64,
        len: u64,
    },
}

pub struct UndoStore;

impl UndoStore {
    pub fn undo_dir(state_dir: &Path) -> PathBuf {
        state_dir.join("undo")
    }

    /// Fixed-length 24-hex ASCII filename (`<24-hex>.undo`) — immune to Windows `MAX_PATH`
    /// and special characters across all filesystems.
    pub fn key_for_path(path: &Path) -> String {
        let hex = sha256_hex(path.to_string_lossy().as_bytes());
        format!("{}.undo", &hex[..24])
    }

    pub fn key_for_draft(id: u64) -> String {
        format!("draft_{id}.undo")
    }

    /// Atomically persist a bounded, SHA-256-verified `.undo` file. Returns `Ok(true)` if written,
    /// `Ok(false)` if the stack is empty or exceeds the per-file budget.
    pub fn write_undo(
        fs: &dyn FileSystem,
        state_dir: &Path,
        key: &str,
        binding: UndoBinding,
        undo: &UndoStack,
    ) -> Result<bool, String> {
        let Some(payload) = undo.encode_bounded(MAX_UNDO_PERSIST_STEPS, MAX_UNDO_PERSIST_BYTES)
        else {
            let _ = fs.remove_file(&Self::undo_dir(state_dir).join(key));
            return Ok(false);
        };
        let mut frame = Vec::with_capacity(128 + payload.len() + 32);
        frame.extend_from_slice(UNDO_FILE_MAGIC);
        frame.push(UNDO_FILE_VERSION);
        match binding {
            UndoBinding::OnDisk {
                rev,
                len,
                fingerprint,
            } => {
                frame.push(0);
                frame.extend_from_slice(&rev.dev.to_le_bytes());
                frame.extend_from_slice(&rev.ino.to_le_bytes());
                frame.extend_from_slice(&rev.size.to_le_bytes());
                frame.extend_from_slice(&rev.mtime_ns.to_le_bytes());
                frame.extend_from_slice(&rev.ctime_ns.to_le_bytes());
                frame.extend_from_slice(&len.to_le_bytes());
                frame.extend_from_slice(&fingerprint);
            }
            UndoBinding::Draft { id, rev, len } => {
                frame.push(1);
                frame.extend_from_slice(&id.to_le_bytes());
                frame.extend_from_slice(&rev.to_le_bytes());
                frame.extend_from_slice(&len.to_le_bytes());
            }
        }
        frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        frame.extend_from_slice(&payload);
        let digest = sha256_bytes(&frame);
        frame.extend_from_slice(&digest);

        ensure_private_dir(fs, state_dir).map_err(|e| e.to_string())?;
        let dir = Self::undo_dir(state_dir);
        ensure_private_dir(fs, &dir).map_err(|e| e.to_string())?;
        let path = dir.join(key);
        fs.write_atomic(&path, &mut |w| w.write_all(&frame))
            .map_err(|e| e.to_string())?;
        set_private_file_permissions(&path);
        Ok(true)
    }

    /// Load and verify a `.undo` file: checks magic, version, trailing SHA-256, and exact
    /// [`UndoBinding`] match before decoding. Returns `None` on missing, stale, or corrupt files.
    pub fn read_verified_undo(
        fs: &dyn FileSystem,
        state_dir: &Path,
        key: &str,
        expected: UndoBinding,
    ) -> Option<UndoStack> {
        let path = Self::undo_dir(state_dir).join(key);
        let mut buf = Vec::new();
        let total = fs
            .read_prefix_into(&path, MAX_UNDO_FILE_BYTES + 1, &mut buf)
            .ok()??;
        if total as usize != buf.len() || buf.len() < 4 + 1 + 1 + 4 + 32 || buf.len() > MAX_UNDO_FILE_BYTES {
            return None;
        }
        let body_len = buf.len() - 32;
        if sha256_bytes(&buf[..body_len]) != buf[body_len..] {
            return None;
        }
        let body = &buf[..body_len];
        if &body[0..4] != UNDO_FILE_MAGIC || body[4] != UNDO_FILE_VERSION {
            return None;
        }
        let mut off = 5usize;
        let tag = *body.get(off)?;
        off += 1;
        let read_u64 = |b: &[u8], o: &mut usize| -> Option<u64> {
            let slice = b.get(*o..*o + 8)?;
            *o += 8;
            Some(u64::from_le_bytes(slice.try_into().ok()?))
        };
        let actual = match tag {
            0 => {
                let rev = FileRev {
                    dev: read_u64(body, &mut off)?,
                    ino: read_u64(body, &mut off)?,
                    size: read_u64(body, &mut off)?,
                    mtime_ns: read_u64(body, &mut off)?,
                    ctime_ns: read_u64(body, &mut off)?,
                };
                let len = read_u64(body, &mut off)?;
                let fp_slice = body.get(off..off + 32)?;
                off += 32;
                let fingerprint: [u8; 32] = fp_slice.try_into().ok()?;
                UndoBinding::OnDisk {
                    rev,
                    len,
                    fingerprint,
                }
            }
            1 => {
                let id = read_u64(body, &mut off)?;
                let rev = read_u64(body, &mut off)?;
                let len = read_u64(body, &mut off)?;
                UndoBinding::Draft { id, rev, len }
            }
            _ => return None,
        };
        if actual != expected {
            return None;
        }
        let payload_len_slice = body.get(off..off + 4)?;
        off += 4;
        let payload_len = u32::from_le_bytes(payload_len_slice.try_into().ok()?) as usize;
        let payload = body.get(off..off + payload_len)?;
        if off + payload_len != body.len() {
            return None;
        }
        UndoStack::decode(payload)
    }

    /// Declarative GC for `state_dir/undo/`:
    /// 1. Removes every `*.undo` file not in `keep_keys`.
    /// 2. Enforces [`MAX_TOTAL_UNDO_BYTES`] (8 MiB), preferring to keep `active_keys` and newest mtimes.
    /// 3. Removes empty `undo/` and best-effort empty `state_dir`.
    pub fn gc_undo(
        fs: &dyn FileSystem,
        state_dir: &Path,
        keep_keys: &[String],
        active_keys: &[String],
    ) {
        let dir = Self::undo_dir(state_dir);
        let Ok(entries) = fs.read_dir(&dir) else {
            return;
        };
        let mut kept: Vec<(PathBuf, String, u64, u64, bool)> = Vec::new();
        let mut total_bytes = 0u64;
        for e in entries {
            if e.kind != EntryKind::File || !is_undo_filename(&e.name) {
                continue;
            }
            if !keep_keys.iter().any(|k| k == &e.name) {
                let _ = fs.remove_file(&e.path);
                continue;
            }
            if let Ok(st) = fs.stat(&e.path) {
                let is_active = active_keys.iter().any(|k| k == &e.name);
                total_bytes = total_bytes.saturating_add(st.rev.size);
                kept.push((e.path, e.name, st.rev.size, st.rev.mtime_ns, is_active));
            }
        }
        if total_bytes > MAX_TOTAL_UNDO_BYTES {
            // Evict inactive folders first, then oldest mtime first.
            kept.sort_by_key(|(_, _, _, mtime, is_active)| (*is_active, *mtime));
            for (path, _, sz, _, _) in kept {
                if total_bytes <= MAX_TOTAL_UNDO_BYTES {
                    break;
                }
                if fs.remove_file(&path).is_ok() {
                    total_bytes = total_bytes.saturating_sub(sz);
                }
            }
        }
        if fs.read_dir(&dir).is_ok_and(|v| v.is_empty()) {
            let _ = std::fs::remove_dir(&dir);
            let _ = std::fs::remove_dir(state_dir);
        }
    }
}

fn is_undo_filename(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".undo") else {
        return false;
    };
    if let Some(id_part) = stem.strip_prefix("draft_") {
        return !id_part.is_empty() && id_part.bytes().all(|b| b.is_ascii_digit());
    }
    stem.len() == 24 && stem.bytes().all(|b| b.is_ascii_hexdigit())
}

fn remap_one_path(p: &Path, from: &Path, to: &Path, is_dir: bool) -> Option<PathBuf> {
    if !is_dir {
        (p == from).then(|| to.to_path_buf())
    } else if p.starts_with(from) {
        let rel = p.strip_prefix(from).ok()?;
        Some(if rel.as_os_str().is_empty() {
            to.to_path_buf()
        } else {
            to.join(rel)
        })
    } else {
        None
    }
}

/// Percent-escape delimiters (`%`, `\t`, `\n`, `\r`, `|`, `,`) plus a leading `@` so
/// reserved tokens (`@standalone`, `@draft:<id>`) can never collide with real OS paths.
fn push_escaped_path(out: &mut String, s: &str) {
    if let Some(rest) = s.strip_prefix('@') {
        out.push_str("%40");
        push_escaped(out, rest);
    } else {
        push_escaped(out, s);
    }
}

fn push_escaped(out: &mut String, s: &str) {
    for ch in s.chars() {
        match ch {
            '%' => out.push_str("%25"),
            '\t' => out.push_str("%09"),
            '\n' => out.push_str("%0A"),
            '\r' => out.push_str("%0D"),
            '|' => out.push_str("%7C"),
            ',' => out.push_str("%2C"),
            other => out.push(other),
        }
    }
}

fn unescape(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_nibble(bytes[i + 1]), hex_nibble(bytes[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor_platform::MemoryFileSystem;

    #[test]
    fn session_store_round_trips_special_chars_at_prefix_and_bounds_recent_folders() {
        let mut store = SessionStore::default();
        for i in 0..16 {
            let folder = if i == 15 {
                PathBuf::from("@standalone_real_dir")
            } else {
                PathBuf::from(format!("/ws/proj {i} | 100%, 🚀"))
            };
            let tab_a = SavedTab {
                target: SavedBufferRef::File(PathBuf::from(format!("/ws/proj {i}/@draft:1,a|b.rs"))),
                view: SavedViewPos::from_pixels(12, 4, 10, 2, 8, 3.5, 14.0),
            };
            let tab_b = SavedTab {
                target: SavedBufferRef::Draft(i as u64 + 1),
                view: SavedViewPos::default(),
            };
            store.upsert(FolderSession {
                folder,
                pins: vec![tab_a.clone()],
                open: vec![tab_b.clone()],
                panes: vec![
                    SavedPane {
                        col: 0,
                        row: 0,
                        tab: tab_a,
                    },
                    SavedPane {
                        col: 1,
                        row: 0,
                        tab: tab_b,
                    },
                ],
                active_pane_idx: 1,
                col_ratio_bp: 6200,
                row_ratio_bp: [4500, 5500],
                zoomed: true,
            });
        }
        assert_eq!(store.folders.len(), MAX_RECENT_FOLDERS);
        let encoded = store.encode();
        let decoded = SessionStore::decode(&encoded);
        assert_eq!(decoded, store);
        assert_eq!(decoded.folders[0].folder, PathBuf::from("@standalone_real_dir"));
        assert!(matches!(
            decoded.folders[0].pins[0].target,
            SavedBufferRef::File(_)
        ));
        assert!(matches!(
            decoded.folders[0].open[0].target,
            SavedBufferRef::Draft(_)
        ));

        // Standalone session encode/decode + explicit remove.
        let mut standalone = SessionStore::default();
        standalone.upsert(FolderSession {
            folder: PathBuf::new(),
            pins: Vec::new(),
            open: vec![SavedTab {
                target: SavedBufferRef::Draft(7),
                view: SavedViewPos::default(),
            }],
            panes: vec![SavedPane {
                col: 0,
                row: 0,
                tab: SavedTab {
                    target: SavedBufferRef::Draft(7),
                    view: SavedViewPos::default(),
                },
            }],
            active_pane_idx: 0,
            col_ratio_bp: DEFAULT_RATIO_BP,
            row_ratio_bp: [DEFAULT_RATIO_BP, DEFAULT_RATIO_BP],
            zoomed: false,
        });
        assert_eq!(SessionStore::decode(&standalone.encode()), standalone);
        assert!(standalone.remove(Path::new("")));
        assert!(standalone.folders.is_empty());
    }

    #[test]
    fn draft_store_verifies_sha256_quarantines_corrupt_and_blocks_gc_on_damaged_index() {
        let fs = MemoryFileSystem::new();
        let state = Path::new("/state");
        let mut store = DraftStore::load(&fs, state);
        assert!(store.loaded_ok);

        let id = store.alloc_id();
        let (len1, sha1) = DraftStore::write_draft_bytes(&fs, state, id, 1, b"v1 draft").unwrap();
        let rec1 = DraftRecord {
            id,
            rev: 1,
            scope_folder: PathBuf::from("/proj"),
            original_path: Some(PathBuf::from("/proj/a.rs")),
            base_rev: FileRev {
                dev: 1,
                ino: 2,
                size: 10,
                mtime_ns: 3,
                ctime_ns: 4,
            },
            bom: true,
            eol: Eol::Cr,
            lang_override: Some("rust".into()),
            view: SavedViewPos::from_pixels(2, 3, 2, 1, 0, 0.0, 0.0),
            byte_len: len1,
            sha256: sha1,
        };
        store.records.push(rec1.clone());
        store.commit_and_gc(&fs, state).unwrap();
        assert_eq!(DraftStore::read_verified_text(&fs, state, &rec1).unwrap(), "v1 draft");

        // Supersede with rev 2 -> commit_and_gc removes d1_r1.buf automatically.
        let (len2, sha2) = DraftStore::write_draft_bytes(&fs, state, id, 2, b"v2 draft!").unwrap();
        store.records[0].rev = 2;
        store.records[0].byte_len = len2;
        store.records[0].sha256 = sha2;
        store.commit_and_gc(&fs, state).unwrap();

        let reloaded = DraftStore::load(&fs, state);
        assert!(reloaded.loaded_ok);
        assert_eq!(reloaded.records[0].eol, Eol::Cr);
        assert_eq!(
            DraftStore::read_verified_text(&fs, state, &reloaded.records[0]).unwrap(),
            "v2 draft!"
        );
        let dir_entries = fs.read_dir(&DraftStore::backups_dir(state)).unwrap();
        assert!(dir_entries.iter().any(|e| e.name == "d1_r2.buf"));
        assert!(!dir_entries.iter().any(|e| e.name == "d1_r1.buf"));

        // Corrupt d1_r2.buf -> verification fails, and quarantining preserves it across GC.
        fs.add_file("/state/backups/d1_r2.buf", b"corrupted".to_vec());
        assert!(DraftStore::read_verified_text(&fs, state, &reloaded.records[0]).is_err());
        let mut qstore = reloaded.clone();
        qstore.quarantine_file("d1_r2.buf".into());
        qstore.records.clear();
        qstore.commit_and_gc(&fs, state).unwrap();
        assert!(fs.stat(Path::new("/state/backups/d1_r2.buf")).is_ok());

        // Damaged index.tsv sets loaded_ok = false and blocks commit_and_gc.
        let damaged = DraftStore::decode("v1\t5\ngarbage_corrupt_line\n");
        assert!(!damaged.loaded_ok);
        assert!(damaged.commit_and_gc(&fs, state).is_err());
    }

    #[test]
    fn undo_store_round_trips_verifies_binding_sha256_and_gc() {
        let fs = MemoryFileSystem::new();
        let state = Path::new("/state");
        let mut stack = UndoStack::new();
        stack.record(
            editor_core::EditOp {
                offset: 0,
                deleted: Vec::new(),
                inserted: b"hello".to_vec(),
                cursor_before: 0,
                cursor_after: 5,
            },
            100,
            false,
        );
        stack.mark_saved();

        let key = UndoStore::key_for_path(Path::new("/proj/a.rs"));
        let binding = UndoBinding::OnDisk {
            rev: FileRev {
                dev: 1,
                ino: 2,
                size: 5,
                mtime_ns: 99,
                ctime_ns: 99,
            },
            len: 5,
            fingerprint: sha256_bytes(b"hello"),
        };
        assert!(UndoStore::write_undo(&fs, state, &key, binding, &stack).unwrap());

        // Matching binding restores undo cleanly.
        let restored = UndoStore::read_verified_undo(&fs, state, &key, binding).unwrap();
        assert!(restored.can_undo() && !restored.is_dirty());

        // Different fingerprint (external edit within same mtime) rejects cleanly!
        let stale_binding = match binding {
            UndoBinding::OnDisk { rev, len, .. } => UndoBinding::OnDisk {
                rev,
                len,
                fingerprint: sha256_bytes(b"world"),
            },
            _ => unreachable!(),
        };
        assert!(UndoStore::read_verified_undo(&fs, state, &key, stale_binding).is_none());

        // Declarative GC removes keys not in keep_keys.
        UndoStore::gc_undo(&fs, state, &[], &[]);
        assert!(fs.stat(&UndoStore::undo_dir(state).join(&key)).is_err());
    }
}
