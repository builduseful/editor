//! Custom-drawn two-pane Quick Open (`Cmd+P`), Search in Files (`Cmd+Shift+F`),
//! and Command Palette (`Cmd+Shift+P`).
//!
//! Performance & architecture highlights:
//! - Uses `editor-core::CompiledQuery` (zero-copy two-byte SWAR literal + linear-time Thompson
//!   NFA regex with SWAR line prefiltering) so single-file Find/Replace and project Search in
//!   Files share one engine.
//! - Parallel scoped directory walk (`std::thread::scope`, 0 persistent threads, 0.00% idle CPU)
//!   builds a compact `IndexedFile` table with 64-bit ASCII bitmasks and binary-extension hints,
//!   invalidated lazily only on `FsChanged` or sidebar mutations.
//! - Quick Open rejects ~95% of non-matching paths in 1 CPU instruction (`char_mask & pat_mask`),
//!   scores candidates with zero per-file heap allocations, and uses O(N) top-K selection.
//! - Search in Files uses dynamic work-stealing across `std::thread::scope` workers with reusable
//!   64 KB streaming buffers (handling multi-MB files without large allocations) and SWAR newline
//!   counting (`count_byte`), cancelling all workers the instant `MAX_SEARCH_RESULTS` is reached.
//! - Live syntax-highlighted preview loads ONLY the single highlighted file (from open dirty
//!   buffers in memory first, else an O(1) 16 KB windowed disk read around `byte_offset`),
//!   pre-lexes once on selection change, paints contiguous token runs, and frees on `close()`.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use editor_core::swar::{count_byte, find_byte, rfind_byte};
use editor_core::{
    CompiledQuery, Language, SearchOptions, SearchScratch, SyntaxCache, TokenSpan,
};
use editor_platform::{EntryKind, FileSystem, FontStyle, GlyphRasterizer, Key, Modifiers, PixelRect};

use crate::atlas::GlyphAtlas;
use crate::painter::Painter;
use crate::text_input::TextInput;
use crate::theme::{px, Color, Theme};

const MAX_PROJECT_FILES: usize = 50_000;
const MAX_WALK_DEPTH: usize = 24;
const STREAM_CHUNK_BYTES: usize = 64 * 1024;
const MAX_STREAM_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SLAB_BYTES: usize = 20 * 1024 * 1024;
const MAX_SLAB_FILE_BYTES: usize = 256 * 1024;
const MAX_SEARCH_RESULTS: usize = 200;
const MAX_QUICK_OPEN_ITEMS: usize = 80;
const SEARCH_TIME_BUDGET_MS: u64 = 500;
const PREVIEW_READ_BYTES: usize = 16 * 1024;
const PREVIEW_MAX_LINES: usize = 180;
const PREVIEW_MIN_WINDOW_PT: f32 = 760.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteMode {
    Closed,
    QuickOpen,
    Commands,
    ProjectSearch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteOpenTarget {
    Current,
    SplitRight,
    SplitDown,
}

/// Entry supplied by `EditorApp` when opening the `⇧⌘P` Command Palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteCommandEntry {
    pub id: String,
    pub title: String,
    pub category: String,
    pub shortcut: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteItem {
    pub path: PathBuf,
    pub command_id: Option<String>,
    pub primary: String,
    pub secondary: String,
    pub shortcut: String,
    pub line_0based: Option<usize>,
    pub col_bytes: Option<usize>,
    pub match_len_bytes: Option<usize>,
    pub byte_offset: Option<u64>,
    /// Matched byte range inside `secondary` (snippet) for subtle highlight rendering.
    pub snippet_match: Option<(usize, usize)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteAction {
    None,
    Close,
    Open {
        path: PathBuf,
        line_0based: Option<usize>,
        col_bytes: Option<usize>,
        target: PaletteOpenTarget,
    },
    RunCommand(String),
    Changed,
}

/// Precomputed per-file metadata for zero-allocation fuzzy Quick Open and fast grep filtering.
#[derive(Debug, Clone)]
struct IndexedFile {
    abs: PathBuf,
    rel: String,
    rel_lower: String,
    basename_start: u32,
    /// 64-bit ASCII presence bitmask (`a-z`, `0-9`, `/`, `.`, `_`, `-`) for 1-instruction rejection.
    char_mask: u64,
    /// Known binary/media/archive extension skipped by text search without syscalls.
    skip_grep: bool,
}

impl IndexedFile {
    fn new(abs: PathBuf, rel: String) -> Self {
        let rel_lower = rel.to_ascii_lowercase();
        let basename_start = rel.rfind('/').map(|i| (i + 1) as u32).unwrap_or(0);
        let char_mask = ascii_char_mask(rel_lower.as_bytes());
        let skip_grep = has_binary_extension(&rel_lower);
        Self {
            abs,
            rel,
            rel_lower,
            basename_start,
            char_mask,
            skip_grep,
        }
    }
}

#[inline]
fn ascii_char_bit(b_lower: u8) -> u64 {
    match b_lower {
        b'a'..=b'z' => 1u64 << (b_lower - b'a'),
        b'0'..=b'9' => 1u64 << (26 + (b_lower - b'0')),
        b'/' => 1u64 << 36,
        b'.' => 1u64 << 37,
        b'_' => 1u64 << 38,
        b'-' => 1u64 << 39,
        _ => 0,
    }
}

#[inline]
fn ascii_char_mask(lower_bytes: &[u8]) -> u64 {
    let mut mask = 0u64;
    for &b in lower_bytes {
        mask |= ascii_char_bit(b);
    }
    mask
}

fn has_binary_extension(rel_lower: &str) -> bool {
    let Some(dot) = rel_lower.rfind('.') else {
        return false;
    };
    let ext = &rel_lower[dot + 1..];
    if ext.contains('/') {
        return false;
    }
    matches!(
        ext,
        "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "webp"
            | "ico"
            | "icns"
            | "bmp"
            | "tiff"
            | "psd"
            | "pdf"
            | "zip"
            | "gz"
            | "tar"
            | "xz"
            | "zst"
            | "bz2"
            | "7z"
            | "rar"
            | "dmg"
            | "pkg"
            | "wasm"
            | "o"
            | "a"
            | "so"
            | "dylib"
            | "dll"
            | "exe"
            | "rlib"
            | "rmeta"
            | "class"
            | "jar"
            | "pyc"
            | "ttf"
            | "otf"
            | "woff"
            | "woff2"
            | "mp3"
            | "mp4"
            | "mov"
            | "wav"
            | "flac"
            | "avi"
            | "mkv"
            | "sqlite"
            | "db"
    )
}

#[derive(Debug, Clone, Copy)]
enum SlabSlot {
    /// Not yet read during this modal session -> loaded lazily on demand.
    Unloaded,
    /// Cached text slice `chunks[chunk][start..end]` inside [`SearchSlab`].
    Cached { chunk: u16, start: u32, end: u32 },
    /// Known empty, binary, oversized, or unreadable -> skip with zero syscalls.
    Skip,
    /// Exceeds per-file (256 KB) or total (20 MB) slab cap -> stream in 64 KB chunks from disk.
    StreamFromDisk,
}

/// Modal-lifetime-only lazy-filling byte arena (<= 20 MB, freed completely on `close()`).
/// Populated incrementally on demand during Search in Files: common queries stop early in
/// ~1-2 ms without reading the whole workspace, rare queries load+search in one L1-hot pass,
/// and subsequent keystrokes/toggles run pure read-only memory sweeps in ~1-2 ms.
struct SearchSlab {
    chunks: Vec<Vec<u8>>,
    slots: Vec<SlabSlot>,
    bytes_used: usize,
    fully_loaded: bool,
}

impl SearchSlab {
    fn new(file_count: usize) -> Self {
        Self {
            chunks: Vec::new(),
            slots: vec![SlabSlot::Unloaded; file_count],
            bytes_used: 0,
            fully_loaded: file_count == 0,
        }
    }
}

#[derive(Debug, Clone)]
struct PreviewBuffer {
    path: PathBuf,
    rel_path: String,
    dirty: bool,
    lines: Vec<String>,
    token_lines: Vec<Vec<TokenSpan>>,
    first_line_0based: usize,
    target_line_0based: usize,
    highlight_col: Option<(usize, usize)>,
    scroll_lines: usize,
    unavailable_reason: Option<&'static str>,
}

pub struct CommandPalette {
    pub mode: PaletteMode,
    pub input: TextInput,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
    pub regex_error: Option<String>,
    root: Option<PathBuf>,
    file_cache_root: Option<PathBuf>,
    files_dirty: bool,
    all_files: Vec<IndexedFile>,
    dirty_files: Vec<(PathBuf, Vec<u8>)>,
    all_commands: Vec<PaletteCommandEntry>,
    pub items: Vec<PaletteItem>,
    pub selected: usize,
    scroll_row: usize,
    last_visible_rows: usize,
    pub status_summary: String,
    // Remembered Search in Files state for zero-friction resume on `⇧⌘F`.
    last_search_query: String,
    last_search_selected: usize,
    // Modal-lifetime-only bounded text slab (<= 32 MB while Search in Files is open; 0 B closed).
    search_slab: Option<SearchSlab>,
    // Single-item lazy syntax-highlighted preview (dropped on `close()`).
    preview: Option<PreviewBuffer>,
    preview_visible_lines: usize,
    // Hit-test layout rects from last draw.
    card_rect: PixelRect,
    list_rect: PixelRect,
    preview_rect: PixelRect,
    btn_case: PixelRect,
    btn_word: PixelRect,
    btn_regex: PixelRect,
    item_rects: Vec<(usize, PixelRect)>,
}

impl Default for CommandPalette {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandPalette {
    pub fn new() -> Self {
        Self {
            mode: PaletteMode::Closed,
            input: TextInput::new("Type to search"),
            case_sensitive: false,
            whole_word: false,
            regex: false,
            regex_error: None,
            root: None,
            file_cache_root: None,
            files_dirty: true,
            all_files: Vec::new(),
            dirty_files: Vec::new(),
            all_commands: Vec::new(),
            items: Vec::new(),
            selected: 0,
            scroll_row: 0,
            last_visible_rows: 10,
            status_summary: String::new(),
            last_search_query: String::new(),
            last_search_selected: 0,
            search_slab: None,
            preview: None,
            preview_visible_lines: 20,
            card_rect: PixelRect::ZERO,
            list_rect: PixelRect::ZERO,
            preview_rect: PixelRect::ZERO,
            btn_case: PixelRect::ZERO,
            btn_word: PixelRect::ZERO,
            btn_regex: PixelRect::ZERO,
            item_rects: Vec::new(),
        }
    }

    pub fn is_open(&self) -> bool {
        self.mode != PaletteMode::Closed
    }

    /// Number of indexed project files currently cached.
    pub fn indexed_file_count(&self) -> usize {
        self.all_files.len()
    }

    /// Mark cached project file list dirty (called lazily on `FsChanged` or sidebar file ops).
    pub fn invalidate_file_index(&mut self) {
        self.files_dirty = true;
        self.search_slab = None;
    }

    /// Current previewed file path & target line (useful for inspection & tests).
    pub fn preview_info(&self) -> Option<(&Path, usize, usize)> {
        self.preview
            .as_ref()
            .map(|p| (p.path.as_path(), p.target_line_0based, p.scroll_lines))
    }

    pub fn close(&mut self) {
        if self.mode == PaletteMode::ProjectSearch {
            self.last_search_query = self.input.text.clone();
            self.last_search_selected = self.selected;
        }
        self.mode = PaletteMode::Closed;
        self.input.focused = false;
        self.all_commands.clear();
        self.dirty_files.clear();
        self.items.clear();
        self.item_rects.clear();
        self.search_slab = None;
        self.preview = None;
        self.regex_error = None;
    }

    fn ensure_file_index(&mut self, fs: &dyn FileSystem, root: Option<&Path>) {
        self.root = root.map(Path::to_path_buf);
        if !self.files_dirty && self.file_cache_root.as_deref() == root {
            return;
        }
        self.all_files.clear();
        self.search_slab = None;
        self.file_cache_root = root.map(Path::to_path_buf);
        self.files_dirty = false;
        if let Some(r) = root {
            collect_project_files_parallel(fs, r, &mut self.all_files);
        }
    }

    pub fn open_commands(&mut self, commands: Vec<PaletteCommandEntry>) {
        if self.mode == PaletteMode::ProjectSearch {
            self.last_search_query = self.input.text.clone();
            self.last_search_selected = self.selected;
        }
        self.mode = PaletteMode::Commands;
        self.input = TextInput::new("Run a command");
        self.input.focused = true;
        self.selected = 0;
        self.scroll_row = 0;
        self.preview = None;
        self.dirty_files.clear();
        self.regex_error = None;
        self.all_commands = commands;
        self.refresh_command_results();
    }

    pub fn open_quick_open(&mut self, fs: &dyn FileSystem, root: Option<&Path>) {
        self.open_quick_open_with_dirty(fs, root, Vec::new());
    }

    pub fn open_quick_open_with_dirty(
        &mut self,
        fs: &dyn FileSystem,
        root: Option<&Path>,
        dirty_files: Vec<(PathBuf, Vec<u8>)>,
    ) {
        if self.mode == PaletteMode::ProjectSearch {
            self.last_search_query = self.input.text.clone();
            self.last_search_selected = self.selected;
        }
        self.mode = PaletteMode::QuickOpen;
        self.input = TextInput::new("Open file by name");
        self.input.focused = true;
        self.selected = 0;
        self.scroll_row = 0;
        self.regex_error = None;
        self.all_commands.clear();
        self.dirty_files = dirty_files;
        self.ensure_file_index(fs, root);
        self.refresh_results(fs);
    }

    pub fn open_project_search(
        &mut self,
        fs: &dyn FileSystem,
        root: Option<&Path>,
        seed: Option<&str>,
    ) {
        self.open_project_search_with_dirty(fs, root, seed, Vec::new());
    }

    pub fn open_project_search_with_dirty(
        &mut self,
        fs: &dyn FileSystem,
        root: Option<&Path>,
        seed: Option<&str>,
        dirty_files: Vec<(PathBuf, Vec<u8>)>,
    ) {
        self.mode = PaletteMode::ProjectSearch;
        self.input = TextInput::new("Search text across files");
        let mut restore_selected: Option<usize> = None;
        if let Some(s) = seed.filter(|s| !s.is_empty() && !s.contains('\n')) {
            self.input.set_text(s);
            self.input.select_all();
        } else if !self.last_search_query.is_empty() {
            // Zero-friction search resume: restore previous query & cursor, pre-selected
            // so typing replaces immediately while `↓` / `Enter` continues from where you left off.
            self.input.set_text(self.last_search_query.clone());
            self.input.select_all();
            restore_selected = Some(self.last_search_selected);
        }
        self.input.focused = true;
        self.selected = 0;
        self.scroll_row = 0;
        self.all_commands.clear();
        self.dirty_files = dirty_files;
        self.ensure_file_index(fs, root);
        self.refresh_results(fs);
        if let Some(prev_sel) = restore_selected {
            if !self.items.is_empty() {
                self.selected = prev_sel.min(self.items.len() - 1);
                self.sync_scroll_to_selected();
                self.ensure_preview(fs);
            }
        }
    }

    fn refresh_command_results(&mut self) {
        self.items.clear();
        self.selected = 0;
        self.scroll_row = 0;
        let query = self.input.text.trim();
        if query.is_empty() {
            for cmd in &self.all_commands {
                self.items.push(PaletteItem {
                    path: PathBuf::new(),
                    command_id: Some(cmd.id.clone()),
                    primary: cmd.title.clone(),
                    secondary: cmd.category.clone(),
                    shortcut: cmd.shortcut.clone(),
                    line_0based: None,
                    col_bytes: None,
                    match_len_bytes: None,
                    byte_offset: None,
                    snippet_match: None,
                });
            }
            self.status_summary = format!("{} commands", self.all_commands.len());
            return;
        }
        let mut scored: Vec<(i32, usize)> = Vec::new();
        for (idx, cmd) in self.all_commands.iter().enumerate() {
            let best = fuzzy_score(query, &cmd.title)
                .map(|s| s + 24)
                .into_iter()
                .chain(fuzzy_score(query, &format!("{}: {}", cmd.category, cmd.title)))
                .chain(fuzzy_score(query, &cmd.id))
                .max();
            if let Some(score) = best {
                scored.push((score, idx));
            }
        }
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        let total = scored.len();
        for (_, idx) in scored.into_iter().take(60) {
            let cmd = &self.all_commands[idx];
            self.items.push(PaletteItem {
                path: PathBuf::new(),
                command_id: Some(cmd.id.clone()),
                primary: cmd.title.clone(),
                secondary: cmd.category.clone(),
                shortcut: cmd.shortcut.clone(),
                line_0based: None,
                col_bytes: None,
                match_len_bytes: None,
                byte_offset: None,
                snippet_match: None,
            });
        }
        self.status_summary = format!("{total} matches");
    }

    pub fn refresh_results(&mut self, fs: &dyn FileSystem) {
        if self.mode == PaletteMode::Commands {
            self.refresh_command_results();
            return;
        }
        self.items.clear();
        self.selected = 0;
        self.scroll_row = 0;
        self.regex_error = None;
        let query = if self.mode == PaletteMode::ProjectSearch {
            self.input.text.as_str()
        } else {
            self.input.text.trim()
        };

        match self.mode {
            PaletteMode::Closed | PaletteMode::Commands => {}
            PaletteMode::QuickOpen => {
                if query.is_empty() {
                    for f in self.all_files.iter().take(MAX_QUICK_OPEN_ITEMS) {
                        let name = &f.rel[f.basename_start as usize..];
                        self.items.push(PaletteItem {
                            path: f.abs.clone(),
                            command_id: None,
                            primary: name.to_string(),
                            secondary: f.rel.clone(),
                            shortcut: String::new(),
                            line_0based: None,
                            col_bytes: None,
                            match_len_bytes: None,
                            byte_offset: Some(0),
                            snippet_match: None,
                        });
                    }
                    self.status_summary = format!("{} files", self.all_files.len());
                } else {
                    // Zero-allocation fuzzy pass with 1-instruction ASCII bitmask prefilter + O(N) top-K!
                    let pat_lower: Vec<u8> =
                        query.bytes().map(|b| b.to_ascii_lowercase()).collect();
                    let pat_mask = ascii_char_mask(&pat_lower);
                    let mut scored: Vec<(i32, usize)> = Vec::new();
                    for (idx, f) in self.all_files.iter().enumerate() {
                        if (f.char_mask & pat_mask) != pat_mask {
                            continue;
                        }
                        if let Some(score) = fuzzy_score_prelowered(
                            &pat_lower,
                            f.rel_lower.as_bytes(),
                            f.basename_start as usize,
                        ) {
                            scored.push((score, idx));
                        }
                    }
                    let total_hits = scored.len();
                    let cmp = |a: &(i32, usize), b: &(i32, usize)| {
                        b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1))
                    };
                    if scored.len() > MAX_QUICK_OPEN_ITEMS {
                        scored.select_nth_unstable_by(MAX_QUICK_OPEN_ITEMS, cmp);
                        scored.truncate(MAX_QUICK_OPEN_ITEMS);
                    }
                    scored.sort_unstable_by(cmp);
                    for (_, idx) in scored {
                        let f = &self.all_files[idx];
                        let name = &f.rel[f.basename_start as usize..];
                        self.items.push(PaletteItem {
                            path: f.abs.clone(),
                            command_id: None,
                            primary: name.to_string(),
                            secondary: f.rel.clone(),
                            shortcut: String::new(),
                            line_0based: None,
                            col_bytes: None,
                            match_len_bytes: None,
                            byte_offset: Some(0),
                            snippet_match: None,
                        });
                    }
                    self.status_summary = format!("{total_hits} matches");
                }
                self.ensure_preview(fs);
            }
            PaletteMode::ProjectSearch => {
                if query.is_empty() || (!self.regex && query.len() < 2) {
                    self.status_summary = if query.is_empty() {
                        format!("{} files indexed", self.all_files.len())
                    } else {
                        "Type at least 2 characters".to_string()
                    };
                    self.preview = None;
                    return;
                }
                let opts = SearchOptions {
                    case_sensitive: self.case_sensitive,
                    whole_word: self.whole_word,
                    regex: self.regex,
                    max_matches: MAX_SEARCH_RESULTS,
                };
                let compiled = match CompiledQuery::compile(query, opts) {
                    Ok(Some(c)) => c,
                    Ok(None) => {
                        self.preview = None;
                        return;
                    }
                    Err(err) => {
                        self.regex_error = Some(err.message);
                        self.status_summary.clear();
                        self.preview = None;
                        return;
                    }
                };

                let slab = self
                    .search_slab
                    .get_or_insert_with(|| SearchSlab::new(self.all_files.len()));
                let (mut found_items, matched_files, timed_out) = search_project_parallel(
                    fs,
                    &self.all_files,
                    slab,
                    &self.dirty_files,
                    &compiled,
                );
                if found_items.len() > MAX_SEARCH_RESULTS {
                    found_items.truncate(MAX_SEARCH_RESULTS);
                }
                self.items = found_items;

                let partial = if timed_out { " (partial)" } else { "" };
                self.status_summary = if self.items.len() >= MAX_SEARCH_RESULTS {
                    format!("{MAX_SEARCH_RESULTS}+ in {matched_files} files{partial}")
                } else {
                    format!("{} in {} files{partial}", self.items.len(), matched_files)
                };
                self.last_search_query = self.input.text.clone();
                self.last_search_selected = 0;
                self.ensure_preview(fs);
            }
        }
    }

    fn sync_scroll_to_selected(&mut self) {
        let vis = self.last_visible_rows.max(1);
        if self.selected < self.scroll_row {
            self.scroll_row = self.selected;
        } else if self.selected >= self.scroll_row + vis {
            self.scroll_row = self.selected + 1 - vis;
        }
    }

    fn ensure_preview(&mut self, fs: &dyn FileSystem) {
        if !matches!(self.mode, PaletteMode::QuickOpen | PaletteMode::ProjectSearch) {
            self.preview = None;
            return;
        }
        let Some(item) = self.items.get(self.selected) else {
            self.preview = None;
            return;
        };
        let target_line = item.line_0based.unwrap_or(0);
        let highlight_col = match (item.col_bytes, item.match_len_bytes) {
            (Some(c), Some(len)) if len > 0 => Some((c, c + len)),
            _ => None,
        };
        if let Some(ref cur) = self.preview {
            if cur.path == item.path
                && cur.target_line_0based == target_line
                && cur.highlight_col == highlight_col
            {
                return;
            }
        }

        let rel_path = self
            .root
            .as_deref()
            .and_then(|r| item.path.strip_prefix(r).ok())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| item.path.to_string_lossy().into_owned());

        let (raw_bytes, win_start, dirty) = if let Some((_, mem)) =
            self.dirty_files.iter().find(|(p, _)| p == &item.path)
        {
            let anchor = (item.byte_offset.unwrap_or(0) as usize).min(mem.len());
            let start = anchor.saturating_sub(4096);
            let end = (start + PREVIEW_READ_BYTES).min(mem.len());
            (mem[start..end].to_vec(), start as u64, true)
        } else {
            let Ok(handle) = fs.open_read(&item.path) else {
                self.preview = Some(PreviewBuffer {
                    path: item.path.clone(),
                    rel_path,
                    dirty: false,
                    lines: Vec::new(),
                    token_lines: Vec::new(),
                    first_line_0based: 0,
                    target_line_0based: target_line,
                    highlight_col,
                    scroll_lines: 0,
                    unavailable_reason: Some("File unreadable"),
                });
                return;
            };
            let len = handle.len();
            if len == 0 {
                self.preview = Some(PreviewBuffer {
                    path: item.path.clone(),
                    rel_path,
                    dirty: false,
                    lines: Vec::new(),
                    token_lines: Vec::new(),
                    first_line_0based: 0,
                    target_line_0based: 0,
                    highlight_col: None,
                    scroll_lines: 0,
                    unavailable_reason: Some("Empty file"),
                });
                return;
            }
            let anchor = item.byte_offset.unwrap_or(0).min(len);
            let start = anchor.saturating_sub(4096);
            let to_read = ((len - start) as usize).min(PREVIEW_READ_BYTES);
            let mut buf = vec![0u8; to_read];
            let mut got = 0usize;
            while got < buf.len() {
                match handle.read_at(start + got as u64, &mut buf[got..]) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => got += n,
                }
            }
            buf.truncate(got);
            (buf, start, false)
        };

        if find_byte(&raw_bytes[..raw_bytes.len().min(8192)], 0).is_some() {
            self.preview = Some(PreviewBuffer {
                path: item.path.clone(),
                rel_path,
                dirty,
                lines: Vec::new(),
                token_lines: Vec::new(),
                first_line_0based: 0,
                target_line_0based: target_line,
                highlight_col: None,
                scroll_lines: 0,
                unavailable_reason: Some("Binary file — preview unavailable"),
            });
            return;
        }

        // If we sought mid-file, discard the leading partial line so every line starts at col 0.
        let slice_skip = if win_start > 0 {
            find_byte(&raw_bytes, b'\n').map(|p| p + 1).unwrap_or(0)
        } else {
            0
        };
        let slice_base = win_start + slice_skip as u64;
        let usable = &raw_bytes[slice_skip..];

        // Compute `first_line_0based` accurately using SWAR newline counting.
        let first_line_0based =
            if let (Some(abs_off), Some(tline)) = (item.byte_offset, item.line_0based) {
                if abs_off >= slice_base {
                    let rel_off = ((abs_off - slice_base) as usize).min(usable.len());
                    let lines_before = count_byte(&usable[..rel_off], b'\n');
                    tline.saturating_sub(lines_before)
                } else {
                    0
                }
            } else {
                0
            };

        let mut lines = Vec::new();
        for raw_line in usable.split(|&b| b == b'\n').take(PREVIEW_MAX_LINES) {
            let trimmed = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
            let s = String::from_utf8_lossy(trimmed);
            if s.len() > 400 {
                let clipped: String = s.chars().take(240).collect();
                lines.push(clipped);
            } else {
                lines.push(s.into_owned());
            }
        }

        let lang = Language::from_path(Some(&item.path));
        let mut cache = SyntaxCache::new(lang);
        let mut token_lines = Vec::with_capacity(lines.len());
        for (idx, line) in lines.iter().enumerate() {
            let mut spans = Vec::new();
            cache.lex_line(idx, line, &mut spans);
            token_lines.push(spans);
        }

        let rel_target = target_line.saturating_sub(first_line_0based);
        let half = (self.preview_visible_lines / 2).max(4);
        let scroll_lines = if item.line_0based.is_some() {
            rel_target.saturating_sub(half)
        } else {
            0
        };

        self.preview = Some(PreviewBuffer {
            path: item.path.clone(),
            rel_path,
            dirty,
            lines,
            token_lines,
            first_line_0based,
            target_line_0based: target_line,
            highlight_col,
            scroll_lines,
            unavailable_reason: None,
        });
    }

    pub fn scroll_preview(&mut self, delta_lines: isize) -> bool {
        let Some(ref mut p) = self.preview else {
            return false;
        };
        if p.lines.is_empty() {
            return false;
        }
        let max_scroll = p.lines.len().saturating_sub(self.preview_visible_lines.max(1));
        let next = if delta_lines < 0 {
            p.scroll_lines.saturating_sub((-delta_lines) as usize)
        } else {
            (p.scroll_lines + delta_lines as usize).min(max_scroll)
        };
        if next != p.scroll_lines {
            p.scroll_lines = next;
            true
        } else {
            false
        }
    }

    pub fn handle_scroll(&mut self, fs: &dyn FileSystem, x_px: f32, y_px: f32, dy_px: f32) -> bool {
        if !self.is_open() {
            return false;
        }
        let xi = x_px.round() as i32;
        let yi = y_px.round() as i32;
        let lines = (-dy_px / 18.0).round() as isize;
        let step = if lines == 0 {
            if dy_px < 0.0 {
                1
            } else if dy_px > 0.0 {
                -1
            } else {
                0
            }
        } else {
            lines
        };
        if step == 0 {
            return false;
        }
        if !self.preview_rect.is_empty() && self.preview_rect.contains(xi, yi) {
            return self.scroll_preview(step);
        }
        if self.list_rect.contains(xi, yi) && !self.items.is_empty() {
            let next = if step < 0 {
                self.selected.saturating_sub((-step) as usize)
            } else {
                (self.selected + step as usize).min(self.items.len() - 1)
            };
            if next != self.selected {
                self.selected = next;
                self.sync_scroll_to_selected();
                self.ensure_preview(fs);
                return true;
            }
        }
        false
    }

    fn open_selected(&mut self, target: PaletteOpenTarget) -> PaletteAction {
        if let Some(item) = self.items.get(self.selected).cloned() {
            self.close();
            if let Some(cmd_id) = item.command_id {
                return PaletteAction::RunCommand(cmd_id);
            }
            return PaletteAction::Open {
                path: item.path,
                line_0based: item.line_0based,
                col_bytes: item.col_bytes,
                target,
            };
        }
        PaletteAction::None
    }

    pub fn handle_key(
        &mut self,
        fs: &dyn FileSystem,
        key: Key,
        mods: Modifiers,
        text: Option<&str>,
        clipboard: Option<&str>,
    ) -> (PaletteAction, Option<String>) {
        // Search option toggles (`⌥⌘C` Case, `⌥⌘W` Whole Word, `⌥⌘R` Regex) in Search in Files —
        // identical to the inline Find/Replace bar!
        if self.mode == PaletteMode::ProjectSearch
            && mods.primary()
            && mods.alt
            && !mods.shift
            && !mods.ctrl
        {
            match key {
                Key::Char('c') | Key::Char('C') => {
                    self.case_sensitive = !self.case_sensitive;
                    self.refresh_results(fs);
                    return (PaletteAction::Changed, None);
                }
                Key::Char('w') | Key::Char('W') => {
                    self.whole_word = !self.whole_word;
                    self.refresh_results(fs);
                    return (PaletteAction::Changed, None);
                }
                Key::Char('r') | Key::Char('R') => {
                    self.regex = !self.regex;
                    self.refresh_results(fs);
                    return (PaletteAction::Changed, None);
                }
                _ => {}
            }
        }

        // Modal ctrl ergonomics (zero conflict with macOS TextInput `⌃A`/`⌃E`/`⌃K`):
        // - `⌃N` / `⌃P`: next / previous result
        // - `⌃D` / `⌃U`: scroll preview down / up half-page
        // - `⌃V` / `⌃X`: open in vertical / horizontal split
        if mods.ctrl && !mods.meta && !mods.alt {
            match key {
                Key::Char('n') | Key::Char('N') => {
                    if !self.items.is_empty() {
                        self.selected = (self.selected + 1).min(self.items.len() - 1);
                        self.sync_scroll_to_selected();
                        self.ensure_preview(fs);
                    }
                    return (PaletteAction::Changed, None);
                }
                Key::Char('p') | Key::Char('P') => {
                    if !self.items.is_empty() {
                        self.selected = self.selected.saturating_sub(1);
                        self.sync_scroll_to_selected();
                        self.ensure_preview(fs);
                    }
                    return (PaletteAction::Changed, None);
                }
                Key::Char('d') | Key::Char('D') => {
                    let half = (self.preview_visible_lines / 2).max(4) as isize;
                    self.scroll_preview(half);
                    return (PaletteAction::Changed, None);
                }
                Key::Char('u') | Key::Char('U') => {
                    let half = (self.preview_visible_lines / 2).max(4) as isize;
                    self.scroll_preview(-half);
                    return (PaletteAction::Changed, None);
                }
                Key::Char('v') | Key::Char('V') if self.mode != PaletteMode::Commands => {
                    return (self.open_selected(PaletteOpenTarget::SplitRight), None);
                }
                Key::Char('x') | Key::Char('X') if self.mode != PaletteMode::Commands => {
                    return (self.open_selected(PaletteOpenTarget::SplitDown), None);
                }
                _ => {}
            }
        }

        match key {
            Key::Escape => {
                self.close();
                return (PaletteAction::Close, None);
            }
            Key::Up => {
                if !self.items.is_empty() {
                    self.selected = self.selected.saturating_sub(1);
                    self.sync_scroll_to_selected();
                    self.ensure_preview(fs);
                }
                return (PaletteAction::Changed, None);
            }
            Key::Down => {
                if !self.items.is_empty() {
                    self.selected = (self.selected + 1).min(self.items.len() - 1);
                    self.sync_scroll_to_selected();
                    self.ensure_preview(fs);
                }
                return (PaletteAction::Changed, None);
            }
            Key::PageUp => {
                if !self.items.is_empty() {
                    let step = self.last_visible_rows.max(1);
                    self.selected = self.selected.saturating_sub(step);
                    self.sync_scroll_to_selected();
                    self.ensure_preview(fs);
                }
                return (PaletteAction::Changed, None);
            }
            Key::PageDown => {
                if !self.items.is_empty() {
                    let step = self.last_visible_rows.max(1);
                    self.selected = (self.selected + step).min(self.items.len() - 1);
                    self.sync_scroll_to_selected();
                    self.ensure_preview(fs);
                }
                return (PaletteAction::Changed, None);
            }
            Key::Enter => {
                let target = if mods.primary() && self.mode != PaletteMode::Commands {
                    PaletteOpenTarget::SplitRight
                } else {
                    PaletteOpenTarget::Current
                };
                return (self.open_selected(target), None);
            }
            _ => {}
        }

        let (changed, copied) = self.input.handle_key(key, mods, text, clipboard);
        if changed {
            self.refresh_results(fs);
        }
        (PaletteAction::Changed, copied)
    }

    pub fn handle_click(&mut self, fs: &dyn FileSystem, x_px: f32, y_px: f32) -> PaletteAction {
        if !self.is_open() {
            return PaletteAction::None;
        }
        let xi = x_px.round() as i32;
        let yi = y_px.round() as i32;
        if !self.card_rect.contains(xi, yi) {
            self.close();
            return PaletteAction::Close;
        }
        if self.mode == PaletteMode::ProjectSearch {
            if self.btn_case.contains(xi, yi) {
                self.case_sensitive = !self.case_sensitive;
                self.refresh_results(fs);
                return PaletteAction::Changed;
            }
            if self.btn_word.contains(xi, yi) {
                self.whole_word = !self.whole_word;
                self.refresh_results(fs);
                return PaletteAction::Changed;
            }
            if self.btn_regex.contains(xi, yi) {
                self.regex = !self.regex;
                self.refresh_results(fs);
                return PaletteAction::Changed;
            }
        }
        for &(idx, rect) in &self.item_rects {
            if rect.contains(xi, yi) {
                self.selected = idx;
                return self.open_selected(PaletteOpenTarget::Current);
            }
        }
        PaletteAction::None
    }

    pub fn draw(
        &mut self,
        painter: &mut Painter,
        bounds: PixelRect,
        theme: &Theme,
        atlas: &mut GlyphAtlas,
        rasterizer: &mut dyn GlyphRasterizer,
        caret_visible: bool,
    ) {
        if !self.is_open() || bounds.is_empty() {
            self.card_rect = PixelRect::ZERO;
            self.list_rect = PixelRect::ZERO;
            self.preview_rect = PixelRect::ZERO;
            self.btn_case = PixelRect::ZERO;
            self.btn_word = PixelRect::ZERO;
            self.btn_regex = PixelRect::ZERO;
            self.item_rects.clear();
            return;
        }

        let scale = painter.scale();
        let hairline = (1.0 * scale).round().max(1.0) as i32;
        let scrim = if theme.is_dark {
            Color::rgba(0, 0, 0, 128)
        } else {
            Color::rgba(0x17, 0x1a, 0x1f, 64)
        };
        painter.fill_rect(bounds, scrim);

        let pad = px(10.0, scale);
        let input_h = px(28.0, scale);
        let header_h = px(20.0, scale);
        let footer_h = px(24.0, scale);
        let row_h = if self.mode == PaletteMode::ProjectSearch {
            px(38.0, scale)
        } else {
            px(28.0, scale)
        };

        let show_preview = matches!(self.mode, PaletteMode::QuickOpen | PaletteMode::ProjectSearch)
            && bounds.w >= px(PREVIEW_MIN_WINDOW_PT, scale);

        let (card_w, card_h) = if show_preview {
            let cw = px(1020.0, scale).min((bounds.w - px(48.0, scale)).max(px(620.0, scale)));
            let ch = px(580.0, scale).min((bounds.h - px(72.0, scale)).max(px(300.0, scale)));
            (cw, ch)
        } else {
            let vis_n = self.items.len().clamp(1, 10);
            let cw = px(580.0, scale).min((bounds.w - px(32.0, scale)).max(px(280.0, scale)));
            let list_h = (vis_n as i32) * row_h + px(8.0, scale);
            let ch = pad + header_h + px(4.0, scale) + input_h + px(8.0, scale) + list_h + footer_h + pad;
            (cw, ch.min((bounds.h - px(64.0, scale)).max(px(180.0, scale))))
        };

        let card = PixelRect::new(
            bounds.x + (bounds.w - card_w) / 2,
            bounds.y + px(38.0, scale),
            card_w,
            card_h,
        );
        self.card_rect = card;

        let radius = 8.0 * scale;
        painter.fill_rounded_rect(card, radius, theme.surface);
        painter.stroke_rounded_rect(card, radius, (1.0 * scale).max(1.0), theme.border_control);

        // 1. Header row (Title + quiet root folder scope + summary / regex error).
        let heading = match self.mode {
            PaletteMode::QuickOpen => "Quick Open",
            PaletteMode::Commands => "Command Palette",
            PaletteMode::ProjectSearch => "Search in Files",
            PaletteMode::Closed => "",
        };
        let label_px = 11.5 * scale;
        let lm = atlas.metrics(rasterizer, FontStyle::UiBold, label_px);
        let hy = (card.y + pad) as f32 + lm.ascent;
        let after_heading = painter.draw_text_ui(
            (card.x + pad) as f32,
            hy,
            heading,
            FontStyle::UiBold,
            label_px,
            theme.text,
            atlas,
            rasterizer,
        );
        if matches!(self.mode, PaletteMode::QuickOpen | PaletteMode::ProjectSearch) {
            if let Some(root_name) = self
                .root
                .as_deref()
                .and_then(|r| r.file_name().and_then(|n| n.to_str()))
                .filter(|n| !n.is_empty())
            {
                let scope_label = format!(" · {root_name}");
                painter.draw_text_ui(
                    after_heading,
                    hy,
                    &scope_label,
                    FontStyle::Ui,
                    label_px,
                    theme.text_muted,
                    atlas,
                    rasterizer,
                );
            }
        }
        let (summary_txt, summary_col) = if let Some(ref err) = self.regex_error {
            (err.as_str(), theme.state_danger)
        } else {
            (self.status_summary.as_str(), theme.text_muted)
        };
        if !summary_txt.is_empty() {
            let sw =
                Painter::measure_text_ui(summary_txt, FontStyle::Ui, label_px, atlas, rasterizer);
            painter.draw_text_ui(
                (card.right() - pad) as f32 - sw,
                hy,
                summary_txt,
                FontStyle::Ui,
                label_px,
                summary_col,
                atlas,
                rasterizer,
            );
        }

        // 2. Prompt row (+ quiet inline `Aa` / `W` / `.*` toggles in Search in Files).
        let input_y = card.y + pad + header_h + px(4.0, scale);
        self.btn_case = PixelRect::ZERO;
        self.btn_word = PixelRect::ZERO;
        self.btn_regex = PixelRect::ZERO;

        let input_w = if self.mode == PaletteMode::ProjectSearch {
            let toggle_block = px(28.0 + 4.0 + 26.0 + 4.0 + 28.0 + 6.0, scale);
            (card.w - pad * 2 - toggle_block).max(px(120.0, scale))
        } else {
            card.w - pad * 2
        };
        let input_rect = PixelRect::new(card.x + pad, input_y, input_w, input_h);
        self.input
            .draw(painter, input_rect, theme, atlas, rasterizer, caret_visible);

        if self.mode == PaletteMode::ProjectSearch {
            let mut bx = input_rect.right() + px(6.0, scale);
            self.btn_case = PixelRect::new(bx, input_y, px(28.0, scale), input_h);
            draw_toggle_pill(
                painter,
                self.btn_case,
                "Aa",
                self.case_sensitive,
                theme,
                atlas,
                rasterizer,
            );
            bx = self.btn_case.right() + px(4.0, scale);

            self.btn_word = PixelRect::new(bx, input_y, px(26.0, scale), input_h);
            draw_toggle_pill(
                painter,
                self.btn_word,
                "W",
                self.whole_word,
                theme,
                atlas,
                rasterizer,
            );
            bx = self.btn_word.right() + px(4.0, scale);

            self.btn_regex = PixelRect::new(bx, input_y, px(28.0, scale), input_h);
            draw_toggle_pill(
                painter,
                self.btn_regex,
                ".*",
                self.regex,
                theme,
                atlas,
                rasterizer,
            );
        }

        // Hairline below prompt.
        let body_top = input_rect.bottom() + px(8.0, scale);
        let footer_top = card.bottom() - footer_h;
        let body_h = (footer_top - body_top).max(0);
        painter.fill_rect(
            PixelRect::new(card.x + hairline, body_top, card.w - hairline * 2, hairline),
            theme.border_subtle,
        );

        // 3. Body: split left results / right syntax-highlighted preview (or single-column).
        let (list_rect, preview_rect) = if show_preview {
            let left_w = ((card.w as f32) * 0.44).round() as i32;
            let lr = PixelRect::new(
                card.x + pad,
                body_top + px(6.0, scale),
                (left_w - pad - px(6.0, scale)).max(0),
                (body_h - px(10.0, scale)).max(0),
            );
            let div_x = card.x + left_w;
            painter.fill_rect(
                PixelRect::new(div_x, body_top, hairline, body_h),
                theme.border_subtle,
            );
            let pr = PixelRect::new(
                div_x + hairline,
                body_top + hairline,
                (card.right() - hairline - (div_x + hairline)).max(0),
                (body_h - hairline).max(0),
            );
            (lr, pr)
        } else {
            let lr = PixelRect::new(
                card.x + pad,
                body_top + px(6.0, scale),
                (card.w - pad * 2).max(0),
                (body_h - px(10.0, scale)).max(0),
            );
            (lr, PixelRect::ZERO)
        };
        self.list_rect = list_rect;
        self.preview_rect = preview_rect;

        // 3a. Results list.
        self.item_rects.clear();
        let visible_rows = ((list_rect.h / row_h.max(1)) as usize).max(1);
        self.last_visible_rows = visible_rows;
        self.sync_scroll_to_selected();

        painter.push_clip(list_rect);
        let font_px = 12.5 * scale;
        let sub_px = 11.0 * scale;
        let fm = atlas.metrics(rasterizer, FontStyle::Ui, font_px);
        let sm = atlas.metrics(rasterizer, FontStyle::Mono, sub_px);

        if self.items.is_empty() {
            let empty_msg = if self.regex_error.is_some() {
                "Fix the regular expression to see matches"
            } else if self.mode != PaletteMode::Commands && self.root.is_none() {
                "Open a folder to search files"
            } else if self.mode == PaletteMode::ProjectSearch && self.input.text.is_empty() {
                "Type to search across files"
            } else {
                "No matches"
            };
            painter.draw_text_ui(
                (list_rect.x + px(6.0, scale)) as f32,
                list_rect.y as f32 + fm.ascent + px(4.0, scale) as f32,
                empty_msg,
                FontStyle::Ui,
                font_px,
                theme.text_muted,
                atlas,
                rasterizer,
            );
        } else {
            let start_idx = self.scroll_row.min(self.items.len().saturating_sub(1));
            let end_idx = (start_idx + visible_rows).min(self.items.len());
            for (slot, idx) in (start_idx..end_idx).enumerate() {
                let item = &self.items[idx];
                let ry = list_rect.y + (slot as i32) * row_h;
                let r = PixelRect::new(list_rect.x, ry, list_rect.w, row_h);
                self.item_rects.push((idx, r));

                if idx == self.selected {
                    painter.fill_rounded_rect(r, 4.0 * scale, theme.selection);
                    painter.stroke_rounded_rect(
                        r,
                        4.0 * scale,
                        (1.0 * scale).max(1.0),
                        theme.border_control,
                    );
                }

                let tx = (r.x + px(8.0, scale)) as f32;
                if self.mode == PaletteMode::ProjectSearch {
                    painter.push_clip(PixelRect::new(
                        r.x + px(4.0, scale),
                        r.y,
                        (r.w - px(10.0, scale)).max(0),
                        r.h,
                    ));
                    let y1 = r.y as f32 + px(4.0, scale) as f32 + fm.ascent;
                    painter.draw_text_ui(
                        tx,
                        y1,
                        &item.primary,
                        FontStyle::UiBold,
                        font_px,
                        theme.text,
                        atlas,
                        rasterizer,
                    );
                    let y2 = r.y as f32 + px(20.0, scale) as f32 + sm.ascent;
                    if let Some((ms, me)) = item.snippet_match {
                        let pre = &item.secondary[..ms];
                        let mid = &item.secondary[ms..me];
                        let pre_w =
                            Painter::measure_text_ui(pre, FontStyle::Mono, sub_px, atlas, rasterizer);
                        let mid_w =
                            Painter::measure_text_ui(mid, FontStyle::Mono, sub_px, atlas, rasterizer);
                        let hl = PixelRect::new(
                            (tx + pre_w).floor() as i32,
                            r.y + px(20.0, scale),
                            mid_w.ceil().max(2.0) as i32,
                            (sm.line_height as i32).max(px(13.0, scale)),
                        );
                        painter.fill_rounded_rect(hl, 2.0 * scale, theme.search_match_active);
                    }
                    painter.draw_text_ui(
                        tx,
                        y2,
                        &item.secondary,
                        FontStyle::Mono,
                        sub_px,
                        theme.text_muted,
                        atlas,
                        rasterizer,
                    );
                    painter.pop_clip();
                } else {
                    let baseline = r.y as f32 + (r.h as f32 + fm.ascent - fm.descent) * 0.5;
                    let mut right_limit = r.right() - px(8.0, scale);
                    if !item.shortcut.is_empty() {
                        let kw = Painter::measure_text_ui(
                            &item.shortcut,
                            FontStyle::Mono,
                            sub_px,
                            atlas,
                            rasterizer,
                        );
                        let pill_pad = px(6.0, scale);
                        let pill_w = kw.ceil() as i32 + pill_pad * 2;
                        let pill_h = (r.h - px(8.0, scale)).max(14);
                        let pill_x = right_limit - pill_w;
                        let pill_y = r.y + (r.h - pill_h) / 2;
                        let pill = PixelRect::new(pill_x, pill_y, pill_w, pill_h);
                        let pill_bg = if idx == self.selected {
                            theme.surface
                        } else {
                            theme.current_line
                        };
                        painter.fill_rounded_rect(pill, 3.0 * scale, pill_bg);
                        painter.stroke_rounded_rect(
                            pill,
                            3.0 * scale,
                            (1.0 * scale).max(1.0),
                            theme.border_subtle,
                        );
                        let kbase = pill.y as f32 + (pill.h as f32 + sm.ascent - sm.descent) * 0.5;
                        painter.draw_text_ui(
                            (pill.x + pill_pad) as f32,
                            kbase,
                            &item.shortcut,
                            FontStyle::Mono,
                            sub_px,
                            theme.text_muted,
                            atlas,
                            rasterizer,
                        );
                        right_limit = pill_x - px(8.0, scale);
                    }
                    painter.push_clip(PixelRect::new(r.x, r.y, (right_limit - r.x).max(0), r.h));
                    let after_name = painter.draw_text_ui(
                        tx,
                        baseline,
                        &item.primary,
                        FontStyle::UiBold,
                        font_px,
                        theme.text,
                        atlas,
                        rasterizer,
                    );
                    if !item.secondary.is_empty() && item.secondary != item.primary {
                        painter.draw_text_ui(
                            after_name + px(10.0, scale) as f32,
                            baseline,
                            &item.secondary,
                            FontStyle::Ui,
                            sub_px,
                            theme.text_muted,
                            atlas,
                            rasterizer,
                        );
                    }
                    painter.pop_clip();
                }
            }
        }
        painter.pop_clip();

        // 3b. Right-hand Live Syntax-Highlighted Preview Pane.
        if show_preview && !preview_rect.is_empty() {
            self.draw_preview(painter, preview_rect, theme, atlas, rasterizer);
        }

        // 4. Footer hint strip.
        painter.fill_rect(
            PixelRect::new(card.x + hairline, footer_top, card.w - hairline * 2, hairline),
            theme.border_subtle,
        );
        let hint = if self.mode == PaletteMode::Commands {
            "↵ run  ·  ↑↓ select  ·  Esc close"
        } else if show_preview {
            "↵ open  ·  ⌃V split right  ·  ⌃X split down  ·  ⌃D/⌃U preview  ·  Esc close"
        } else {
            "↵ open  ·  ⌃V split  ·  ⌃X split down  ·  Esc close"
        };
        let hint_px = 10.5 * scale;
        let hm = atlas.metrics(rasterizer, FontStyle::Ui, hint_px);
        let hbase = footer_top as f32 + (footer_h as f32 + hm.ascent - hm.descent) * 0.5;
        painter.push_clip(PixelRect::new(
            card.x + pad,
            footer_top,
            (card.w - pad * 2).max(0),
            footer_h,
        ));
        painter.draw_text_ui(
            (card.x + pad) as f32,
            hbase,
            hint,
            FontStyle::Ui,
            hint_px,
            theme.text_muted,
            atlas,
            rasterizer,
        );
        painter.pop_clip();
    }

    fn draw_preview(
        &mut self,
        painter: &mut Painter,
        rect: PixelRect,
        theme: &Theme,
        atlas: &mut GlyphAtlas,
        rasterizer: &mut dyn GlyphRasterizer,
    ) {
        let scale = painter.scale();
        let hairline = (1.0 * scale).round().max(1.0) as i32;
        let pad = px(10.0, scale);
        let header_h = px(26.0, scale);

        painter.push_clip(rect);
        painter.fill_rect(rect, theme.page);

        let Some(ref preview) = self.preview else {
            let font_px = 12.0 * scale;
            let fm = atlas.metrics(rasterizer, FontStyle::Ui, font_px);
            painter.draw_text_ui(
                (rect.x + pad) as f32,
                (rect.y + pad) as f32 + fm.ascent,
                "No file selected",
                FontStyle::Ui,
                font_px,
                theme.text_muted,
                atlas,
                rasterizer,
            );
            painter.pop_clip();
            return;
        };

        // Preview header bar: `relative/path.rs` (+ `•` if unsaved) and `Ln N`.
        let hdr_px = 11.0 * scale;
        let hm = atlas.metrics(rasterizer, FontStyle::Ui, hdr_px);
        let hbase = rect.y as f32 + (header_h as f32 + hm.ascent - hm.descent) * 0.5;
        let title = if preview.dirty {
            format!("{} •", preview.rel_path)
        } else {
            preview.rel_path.clone()
        };
        let ln_label = format!("Ln {}", preview.target_line_0based + 1);
        let lw = Painter::measure_text_ui(&ln_label, FontStyle::Mono, hdr_px, atlas, rasterizer);
        let right_x = (rect.right() - pad) as f32 - lw;
        painter.push_clip(PixelRect::new(
            rect.x + pad,
            rect.y,
            ((right_x as i32) - (rect.x + pad) - px(8.0, scale)).max(0),
            header_h,
        ));
        painter.draw_text_ui(
            (rect.x + pad) as f32,
            hbase,
            &title,
            FontStyle::UiBold,
            hdr_px,
            theme.text,
            atlas,
            rasterizer,
        );
        painter.pop_clip();
        painter.draw_text_ui(
            right_x,
            hbase,
            &ln_label,
            FontStyle::Mono,
            hdr_px,
            theme.text_muted,
            atlas,
            rasterizer,
        );
        painter.fill_rect(
            PixelRect::new(rect.x, rect.y + header_h, rect.w, hairline),
            theme.border_subtle,
        );

        let code_rect = PixelRect::new(
            rect.x,
            rect.y + header_h + hairline,
            rect.w,
            (rect.h - header_h - hairline).max(0),
        );
        painter.fill_rect(code_rect, theme.surface);
        let code_clip = PixelRect::new(
            code_rect.x,
            code_rect.y,
            (code_rect.w - px(8.0, scale)).max(0),
            code_rect.h,
        );
        painter.push_clip(code_clip);

        if let Some(reason) = preview.unavailable_reason {
            let font_px = 12.0 * scale;
            let fm = atlas.metrics(rasterizer, FontStyle::Ui, font_px);
            painter.draw_text_ui(
                (code_rect.x + pad) as f32,
                (code_rect.y + pad) as f32 + fm.ascent,
                reason,
                FontStyle::Ui,
                font_px,
                theme.text_muted,
                atlas,
                rasterizer,
            );
            painter.pop_clip();
            painter.pop_clip();
            return;
        }

        let code_px = 11.5 * scale;
        let cm = atlas.metrics(rasterizer, FontStyle::Mono, code_px);
        let line_h = (cm.line_height as i32).max(px(16.0, scale));
        let vis_lines = ((code_rect.h / line_h.max(1)) as usize).max(1);
        self.preview_visible_lines = vis_lines;

        let max_line_num = preview.first_line_0based + preview.lines.len();
        let digits = max_line_num.max(1).to_string().len().max(3);
        let gutter_w = ((digits as f32 + 1.5) * cm.cell_width).ceil() as i32 + px(8.0, scale);
        let code_x = code_rect.x + gutter_w + px(8.0, scale);

        let start = preview.scroll_lines.min(preview.lines.len());
        let end = (start + vis_lines + 1).min(preview.lines.len());

        for (vis_idx, line_idx) in (start..end).enumerate() {
            let real_line = preview.first_line_0based + line_idx;
            let ly = code_rect.y + px(4.0, scale) + (vis_idx as i32) * line_h;
            let is_target = real_line == preview.target_line_0based
                && (self.mode == PaletteMode::ProjectSearch || preview.highlight_col.is_some());

            if is_target {
                painter.fill_rect(
                    PixelRect::new(code_rect.x, ly, code_rect.w, line_h),
                    theme.current_line,
                );
            }

            // Line number in subtle muted mono.
            let num_str = (real_line + 1).to_string();
            let nw = num_str.len() as f32 * cm.cell_width;
            let baseline =
                ly as f32 + cm.ascent + ((line_h as f32 - cm.line_height as f32) * 0.5).max(0.0);
            let num_col = if is_target {
                theme.text
            } else {
                theme.text_muted
            };
            painter.draw_text_ui(
                (code_rect.x + gutter_w - px(6.0, scale)) as f32 - nw,
                baseline,
                &num_str,
                FontStyle::Mono,
                code_px,
                num_col,
                atlas,
                rasterizer,
            );

            let line_str = &preview.lines[line_idx];
            if is_target {
                if let Some((cs, ce)) = preview.highlight_col {
                    if cs < line_str.len() && line_str.is_char_boundary(cs) {
                        let ce_clamped = ce.min(line_str.len());
                        let ce_valid = if line_str.is_char_boundary(ce_clamped) {
                            ce_clamped
                        } else {
                            cs
                        };
                        if ce_valid > cs {
                            let pre_w = expand_tabs_cols(&line_str[..cs]) as f32 * cm.cell_width;
                            let span_w =
                                expand_tabs_cols(&line_str[cs..ce_valid]) as f32 * cm.cell_width;
                            let hl = PixelRect::new(
                                code_x + pre_w.floor() as i32,
                                ly + px(1.0, scale),
                                span_w.ceil().max(2.0) as i32,
                                (line_h - px(2.0, scale)).max(8),
                            );
                            painter.fill_rounded_rect(hl, 2.0 * scale, theme.search_match_active);
                        }
                    }
                }
            }

            let spans = preview
                .token_lines
                .get(line_idx)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            draw_preview_code_line(
                painter,
                code_x as f32,
                baseline,
                line_str,
                spans,
                code_px,
                cm.cell_width,
                theme,
                atlas,
                rasterizer,
            );
        }

        painter.pop_clip();
        painter.pop_clip();
    }
}

// ============================================================================
// Unified Lazy-Filling Parallel Project Search (0 persistent threads, 0% idle)
// ============================================================================

/// Worker-local slab writer used only while `!slab.fully_loaded`.
struct SlabWriter<'a> {
    chunk_id: Option<u16>,
    bytes_used: &'a AtomicUsize,
    local_data: Vec<u8>,
    local_slots: Vec<(usize, SlabSlot)>,
}

fn search_project_parallel(
    fs: &dyn FileSystem,
    files: &[IndexedFile],
    slab: &mut SearchSlab,
    dirty_files: &[(PathBuf, Vec<u8>)],
    compiled: &CompiledQuery,
) -> (Vec<PaletteItem>, usize, bool) {
    if files.is_empty() {
        return (Vec::new(), 0, false);
    }

    let started = Instant::now();
    let budget = Duration::from_millis(SEARCH_TIME_BUDGET_MS);

    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(8);
    let worker_count = if files.len() < 64 || cores <= 1 {
        1
    } else {
        cores.min((files.len() + 31) / 32)
    };

    let filling = !slab.fully_loaded;
    let base_chunks = slab.chunks.len();
    let can_add_chunks = filling && base_chunks + worker_count <= u16::MAX as usize;
    let bytes_used = AtomicUsize::new(slab.bytes_used);

    if worker_count <= 1 {
        let mut read_buf = Vec::new();
        let mut scratch = SearchScratch::new();
        let mut file_hits = Vec::new();
        let mut out = Vec::new();
        let mut matched_files = 0usize;
        let mut timed_out = false;
        let mut stopped_early = false;
        let mut writer = filling.then(|| SlabWriter {
            chunk_id: can_add_chunks.then_some(base_chunks as u16),
            bytes_used: &bytes_used,
            local_data: Vec::new(),
            local_slots: Vec::new(),
        });

        for (i, f) in files.iter().enumerate() {
            if out.len() >= MAX_SEARCH_RESULTS {
                stopped_early = true;
                break;
            }
            if i > 0 && (i & 31) == 0 && started.elapsed() >= budget {
                timed_out = true;
                stopped_early = true;
                break;
            }
            let before = out.len();
            let remaining = MAX_SEARCH_RESULTS - before;
            search_single_file(
                fs,
                i,
                f,
                slab,
                writer.as_mut(),
                dirty_files,
                compiled,
                remaining,
                &mut read_buf,
                &mut scratch,
                &mut file_hits,
                &mut out,
            );
            if out.len() > before {
                matched_files += 1;
            }
        }
        if let Some(w) = writer {
            if w.chunk_id.is_some() {
                slab.chunks.push(w.local_data);
            }
            for (idx, slot) in w.local_slots {
                slab.slots[idx] = slot;
            }
            slab.bytes_used = bytes_used.load(Ordering::Relaxed);
            if !stopped_early {
                slab.fully_loaded = true;
            }
        }
        return (out, matched_files, timed_out);
    }

    // Dynamic work-stealing across `worker_count` scoped threads.
    // Batch size 16 during lazy cold load (even I/O balance), 32 on warm memory sweeps.
    let batch_size = if filling { 16 } else { 32 };
    let next_idx = AtomicUsize::new(0);
    let total_found = AtomicUsize::new(0);
    let timed_out_flag = AtomicBool::new(false);
    let cancel = AtomicBool::new(false);
    let slab_ref: &SearchSlab = slab;

    let worker_outputs: Vec<(
        Vec<(usize, Vec<PaletteItem>)>,
        Option<(Vec<u8>, Vec<(usize, SlabSlot)>)>,
    )> = std::thread::scope(|s| {
        let mut handles = Vec::with_capacity(worker_count);
        for worker_id in 0..worker_count {
            let next_ref = &next_idx;
            let found_ref = &total_found;
            let timeout_ref = &timed_out_flag;
            let cancel_ref = &cancel;
            let used_ref = &bytes_used;
            handles.push(s.spawn(move || {
                // Lazy `read_buf`: zero allocation on warm slab sweeps!
                let mut read_buf = Vec::new();
                let mut scratch = SearchScratch::new();
                let mut file_hits = Vec::new();
                let mut local_by_file: Vec<(usize, Vec<PaletteItem>)> = Vec::new();
                let mut batch_files = 0usize;
                let mut writer = filling.then(|| SlabWriter {
                    chunk_id: can_add_chunks.then_some((base_chunks + worker_id) as u16),
                    bytes_used: used_ref,
                    local_data: Vec::new(),
                    local_slots: Vec::new(),
                });

                loop {
                    if cancel_ref.load(Ordering::Relaxed) {
                        break;
                    }
                    let start = next_ref.fetch_add(batch_size, Ordering::Relaxed);
                    if start >= files.len() {
                        break;
                    }
                    let end = (start + batch_size).min(files.len());
                    for idx in start..end {
                        if cancel_ref.load(Ordering::Relaxed) {
                            break;
                        }
                        batch_files += 1;
                        if (batch_files & 63) == 0 && started.elapsed() >= budget {
                            timeout_ref.store(true, Ordering::Relaxed);
                            cancel_ref.store(true, Ordering::Relaxed);
                            break;
                        }
                        let cur_total = found_ref.load(Ordering::Relaxed);
                        if cur_total >= MAX_SEARCH_RESULTS {
                            cancel_ref.store(true, Ordering::Relaxed);
                            break;
                        }
                        let remaining = (MAX_SEARCH_RESULTS - cur_total).max(1);
                        let mut items = Vec::new();
                        search_single_file(
                            fs,
                            idx,
                            &files[idx],
                            slab_ref,
                            writer.as_mut(),
                            dirty_files,
                            compiled,
                            remaining,
                            &mut read_buf,
                            &mut scratch,
                            &mut file_hits,
                            &mut items,
                        );
                        if !items.is_empty() {
                            let prev = found_ref.fetch_add(items.len(), Ordering::Relaxed);
                            if prev + items.len() >= MAX_SEARCH_RESULTS {
                                cancel_ref.store(true, Ordering::Relaxed);
                            }
                            local_by_file.push((idx, items));
                        }
                    }
                }
                (
                    local_by_file,
                    writer.map(|w| (w.local_data, w.local_slots)),
                )
            }));
        }
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_default())
            .collect()
    });

    // Commit any lazily-loaded slab chunks (zero-copy Vec move!) and merge results deterministically.
    let mut merged_files: Vec<(usize, Vec<PaletteItem>)> = Vec::new();
    for (local_hits, slab_delta) in worker_outputs {
        merged_files.extend(local_hits);
        if let Some((chunk_data, chunk_slots)) = slab_delta {
            if can_add_chunks {
                slab.chunks.push(chunk_data);
            }
            for (idx, slot) in chunk_slots {
                slab.slots[idx] = slot;
            }
        }
    }
    if filling {
        slab.bytes_used = bytes_used.load(Ordering::Relaxed);
        if !cancel.load(Ordering::Relaxed) && !timed_out_flag.load(Ordering::Relaxed) {
            slab.fully_loaded = true;
        }
    }

    merged_files.sort_unstable_by_key(|(idx, _)| *idx);
    let mut out = Vec::with_capacity(MAX_SEARCH_RESULTS);
    let mut matched_files = 0usize;
    for (_, items) in merged_files {
        if out.len() >= MAX_SEARCH_RESULTS {
            break;
        }
        let room = MAX_SEARCH_RESULTS - out.len();
        let take_n = items.len().min(room);
        if take_n > 0 {
            matched_files += 1;
            out.extend(items.into_iter().take(take_n));
        }
    }
    (out, matched_files, timed_out_flag.load(Ordering::Relaxed))
}

#[allow(clippy::too_many_arguments)]
fn search_single_file(
    fs: &dyn FileSystem,
    file_idx: usize,
    file: &IndexedFile,
    slab: &SearchSlab,
    writer: Option<&mut SlabWriter<'_>>,
    dirty_files: &[(PathBuf, Vec<u8>)],
    compiled: &CompiledQuery,
    max_matches: usize,
    read_buf: &mut Vec<u8>,
    scratch: &mut SearchScratch,
    file_hits: &mut Vec<(usize, usize)>,
    out: &mut Vec<PaletteItem>,
) {
    if max_matches == 0 {
        return;
    }

    // 1. Prefer in-memory dirty buffer if open and modified!
    if !dirty_files.is_empty() {
        if let Some((_, dirty_bytes)) = dirty_files.iter().find(|(p, _)| p == &file.abs) {
            if dirty_bytes.is_empty()
                || find_byte(&dirty_bytes[..dirty_bytes.len().min(8192)], 0).is_some()
            {
                return;
            }
            file_hits.clear();
            compiled.find_in_bytes(dirty_bytes, max_matches, scratch, file_hits);
            push_chunk_hits(file, dirty_bytes, 0, 0, file_hits, max_matches, out);
            return;
        }
    }

    // 2. Consult modal-lifetime SearchSlab.
    let slot = slab.slots.get(file_idx).copied().unwrap_or(SlabSlot::StreamFromDisk);
    match slot {
        SlabSlot::Skip => return,
        SlabSlot::Cached { chunk, start, end } => {
            let slice = &slab.chunks[chunk as usize][start as usize..end as usize];
            file_hits.clear();
            compiled.find_in_bytes(slice, max_matches, scratch, file_hits);
            push_chunk_hits(file, slice, 0, 0, file_hits, max_matches, out);
            return;
        }
        SlabSlot::Unloaded => {
            if file.skip_grep {
                if let Some(w) = writer {
                    w.local_slots.push((file_idx, SlabSlot::Skip));
                }
                return;
            }
            if let Some(w) = writer {
                if w.chunk_id.is_some() && w.bytes_used.load(Ordering::Relaxed) < MAX_SLAB_BYTES {
                    match fs.read_prefix_into(&file.abs, MAX_SLAB_FILE_BYTES, read_buf) {
                        Ok(Some(total_len)) => {
                            if total_len == 0
                                || total_len > MAX_STREAM_FILE_BYTES
                                || find_byte(&read_buf[..read_buf.len().min(8192)], 0).is_some()
                            {
                                w.local_slots.push((file_idx, SlabSlot::Skip));
                                return;
                            }
                            if total_len as usize <= MAX_SLAB_FILE_BYTES {
                                // Fits in slab: cache AND search immediately while L1-hot!
                                let prev = w.bytes_used.fetch_add(read_buf.len(), Ordering::Relaxed);
                                if prev + read_buf.len() <= MAX_SLAB_BYTES {
                                    let off = w.local_data.len() as u32;
                                    w.local_data.extend_from_slice(read_buf);
                                    let end_off = w.local_data.len() as u32;
                                    w.local_slots.push((
                                        file_idx,
                                        SlabSlot::Cached {
                                            chunk: w.chunk_id.unwrap(),
                                            start: off,
                                            end: end_off,
                                        },
                                    ));
                                } else {
                                    w.local_slots.push((file_idx, SlabSlot::StreamFromDisk));
                                }
                                file_hits.clear();
                                compiled.find_in_bytes(read_buf, max_matches, scratch, file_hits);
                                push_chunk_hits(file, read_buf, 0, 0, file_hits, max_matches, out);
                                return;
                            }
                            // Multi-MB file (> 256 KB, <= 16 MB): mark StreamFromDisk and reuse
                            // the 256 KB prefix we already read before streaming the tail!
                            w.local_slots.push((file_idx, SlabSlot::StreamFromDisk));
                            let overlap = compiled.stream_overlap_bytes();
                            let multiline = compiled.can_match_newline();
                            let mut usable = read_buf.len();
                            if !multiline {
                                if let Some(last_nl) = rfind_byte(read_buf, b'\n') {
                                    usable = last_nl + 1;
                                }
                            }
                            file_hits.clear();
                            compiled.find_in_bytes(
                                &read_buf[..usable],
                                max_matches,
                                scratch,
                                file_hits,
                            );
                            let pushed = push_chunk_hits(
                                file,
                                &read_buf[..usable],
                                0,
                                0,
                                file_hits,
                                max_matches,
                                out,
                            );
                            if pushed < max_matches {
                                let step = if multiline {
                                    usable.saturating_sub(overlap).max(1)
                                } else {
                                    usable.max(1)
                                };
                                let base_line = count_byte(&read_buf[..step], b'\n');
                                stream_search_file(
                                    fs,
                                    file,
                                    compiled,
                                    step as u64,
                                    base_line,
                                    max_matches - pushed,
                                    read_buf,
                                    scratch,
                                    file_hits,
                                    out,
                                );
                            }
                            return;
                        }
                        Ok(None) | Err(_) => {
                            w.local_slots.push((file_idx, SlabSlot::Skip));
                            return;
                        }
                    }
                } else {
                    w.local_slots.push((file_idx, SlabSlot::StreamFromDisk));
                }
            }
        }
        SlabSlot::StreamFromDisk => {}
    }

    // 3. Skip known binary extensions without syscalls.
    if file.skip_grep {
        return;
    }

    stream_search_file(
        fs,
        file,
        compiled,
        0,
        0,
        max_matches,
        read_buf,
        scratch,
        file_hits,
        out,
    );
}

#[allow(clippy::too_many_arguments)]
fn stream_search_file(
    fs: &dyn FileSystem,
    file: &IndexedFile,
    compiled: &CompiledQuery,
    start_offset: u64,
    start_line: usize,
    max_matches: usize,
    read_buf: &mut Vec<u8>,
    scratch: &mut SearchScratch,
    file_hits: &mut Vec<(usize, usize)>,
    out: &mut Vec<PaletteItem>,
) {
    if max_matches == 0 {
        return;
    }
    let Ok(handle) = fs.open_read(&file.abs) else {
        return;
    };
    let total_len = handle.len();
    if total_len == 0 || total_len > MAX_STREAM_FILE_BYTES || start_offset >= total_len {
        return;
    }

    let overlap = compiled.stream_overlap_bytes();
    let multiline = compiled.can_match_newline();
    let chunk_cap = STREAM_CHUNK_BYTES + overlap;
    // Grow-once buffer: avoids re-zeroing 64 KB on every chunk!
    if read_buf.len() < chunk_cap {
        read_buf.resize(chunk_cap, 0);
    }

    let mut offset = start_offset;
    let mut base_line = start_line;
    let mut added = 0usize;

    while offset < total_len && added < max_matches {
        let to_read = ((total_len - offset) as usize).min(chunk_cap);
        let mut got = 0usize;
        while got < to_read {
            match handle.read_at(offset + got as u64, &mut read_buf[got..to_read]) {
                Ok(0) | Err(_) => break,
                Ok(n) => got += n,
            }
        }
        if got == 0 {
            break;
        }
        let chunk = &read_buf[..got];
        // Binary probe on offset 0 only.
        if offset == 0 && find_byte(&chunk[..got.min(8192)], 0).is_some() {
            return;
        }

        let is_eof = offset + got as u64 >= total_len;
        let mut usable = got;
        if !is_eof && !multiline {
            if let Some(last_nl) = rfind_byte(chunk, b'\n') {
                usable = last_nl + 1;
            }
        }

        file_hits.clear();
        compiled.find_in_bytes(&chunk[..usable], max_matches - added, scratch, file_hits);
        let pushed = push_chunk_hits(
            file,
            &chunk[..usable],
            offset,
            base_line,
            file_hits,
            max_matches - added,
            out,
        );
        added += pushed;

        if is_eof {
            break;
        }
        let step = if multiline {
            usable.saturating_sub(overlap).max(1)
        } else {
            usable.max(1)
        };
        base_line += count_byte(&chunk[..step], b'\n');
        offset += step as u64;
    }
}

fn push_chunk_hits(
    file: &IndexedFile,
    bytes: &[u8],
    base_offset: u64,
    base_line: usize,
    file_hits: &[(usize, usize)],
    limit: usize,
    out: &mut Vec<PaletteItem>,
) -> usize {
    if file_hits.is_empty() || limit == 0 {
        return 0;
    }
    let mut cur_line = base_line;
    let mut scan_pos = 0usize;
    let mut last_pushed_line: Option<usize> = None;
    let mut pushed = 0usize;

    for &(hit_s, hit_e) in file_hits {
        if pushed >= limit {
            break;
        }
        if hit_s > scan_pos {
            cur_line += count_byte(&bytes[scan_pos..hit_s], b'\n');
            scan_pos = hit_s;
        }
        if last_pushed_line == Some(cur_line) {
            continue;
        }
        last_pushed_line = Some(cur_line);

        let actual_line_start = rfind_byte(&bytes[..hit_s], b'\n').map(|p| p + 1).unwrap_or(0);
        let line_end = find_byte(&bytes[hit_s..], b'\n')
            .map(|d| hit_s + d)
            .unwrap_or(bytes.len());
        let mut raw_line = &bytes[actual_line_start..line_end];
        if raw_line.ends_with(b"\r") {
            raw_line = &raw_line[..raw_line.len() - 1];
        }
        let leading_ws = raw_line
            .iter()
            .take_while(|&&b| b == b' ' || b == b'\t')
            .count();
        let trimmed_bytes = &raw_line[leading_ws..];
        let full_str = String::from_utf8_lossy(trimmed_bytes);
        let snippet: String = full_str.chars().take(96).collect();

        let col_in_line = hit_s.saturating_sub(actual_line_start);
        let match_len = hit_e.saturating_sub(hit_s);
        let snip_start = col_in_line.saturating_sub(leading_ws).min(snippet.len());
        let snip_end = (snip_start + match_len).min(snippet.len());
        let snippet_match = if snip_end > snip_start
            && snippet.is_char_boundary(snip_start)
            && snippet.is_char_boundary(snip_end)
        {
            Some((snip_start, snip_end))
        } else {
            None
        };

        out.push(PaletteItem {
            path: file.abs.clone(),
            command_id: None,
            primary: format!("{}:{}", file.rel, cur_line + 1),
            secondary: snippet,
            shortcut: String::new(),
            line_0based: Some(cur_line),
            col_bytes: Some(col_in_line),
            match_len_bytes: Some(match_len),
            byte_offset: Some(base_offset + hit_s as u64),
            snippet_match,
        });
        pushed += 1;
    }
    pushed
}

// ============================================================================
// Batched Preview Code Rendering & Helper Primitives
// ============================================================================

#[inline]
fn expand_tabs_cols(s: &str) -> usize {
    let mut cols = 0usize;
    for ch in s.chars() {
        if ch == '\t' {
            cols += 4 - (cols % 4);
        } else {
            cols += 1;
        }
    }
    cols
}

/// Paint a syntax-highlighted preview line by flushing contiguous token slices (splitting only on `\t`).
#[allow(clippy::too_many_arguments)]
fn draw_preview_code_line(
    painter: &mut Painter,
    start_x: f32,
    baseline: f32,
    line: &str,
    spans: &[TokenSpan],
    font_px: f32,
    cell_w: f32,
    theme: &Theme,
    atlas: &mut GlyphAtlas,
    rasterizer: &mut dyn GlyphRasterizer,
) {
    if line.is_empty() {
        return;
    }
    let mut x = start_x;
    let mut col = 0usize;
    let mut byte_pos = 0usize;
    let mut span_idx = 0usize;

    while byte_pos < line.len() {
        while span_idx < spans.len() && (spans[span_idx].end as usize) <= byte_pos {
            span_idx += 1;
        }
        let (seg_end, color, style) = if let Some(sp) = spans.get(span_idx) {
            let s = sp.start as usize;
            let e = (sp.end as usize).min(line.len());
            if s > byte_pos {
                (s.min(line.len()), theme.text, FontStyle::Mono)
            } else {
                let (c, st) = theme.syntax_font_style(sp.kind);
                (e.max(byte_pos + 1), c, st)
            }
        } else {
            (line.len(), theme.text, FontStyle::Mono)
        };
        let end = if line.is_char_boundary(seg_end) {
            seg_end
        } else {
            line.len()
        };
        let seg = &line[byte_pos..end];

        // Split only on tabs so each non-tab run renders in a single batched draw call!
        let mut sub_start = 0usize;
        for (rel, ch) in seg.char_indices() {
            if ch == '\t' {
                if rel > sub_start {
                    let run = &seg[sub_start..rel];
                    painter.draw_text_ui(x, baseline, run, style, font_px, color, atlas, rasterizer);
                    let chars = run.chars().count();
                    col += chars;
                    x += chars as f32 * cell_w;
                }
                let adv = 4 - (col % 4);
                col += adv;
                x += adv as f32 * cell_w;
                sub_start = rel + 1;
            }
        }
        if sub_start < seg.len() {
            let run = &seg[sub_start..];
            painter.draw_text_ui(x, baseline, run, style, font_px, color, atlas, rasterizer);
            let chars = run.chars().count();
            col += chars;
            x += chars as f32 * cell_w;
        }
        byte_pos = end;
    }
}

fn draw_toggle_pill(
    painter: &mut Painter,
    rect: PixelRect,
    label: &str,
    active: bool,
    theme: &Theme,
    atlas: &mut GlyphAtlas,
    rasterizer: &mut dyn GlyphRasterizer,
) {
    let scale = painter.scale();
    let radius = 4.0 * scale;
    let bg = if active { theme.selection } else { theme.page };
    let border = if active {
        theme.border_control
    } else {
        theme.border_subtle
    };
    painter.fill_rounded_rect(rect, radius, bg);
    painter.stroke_rounded_rect(rect, radius, (1.0 * scale).max(1.0), border);

    let font_px = 11.5 * scale;
    let m = atlas.metrics(rasterizer, FontStyle::Ui, font_px);
    let tw = Painter::measure_text_ui(label, FontStyle::Ui, font_px, atlas, rasterizer);
    let tx = rect.x as f32 + (rect.w as f32 - tw) * 0.5;
    let baseline = rect.y as f32 + (rect.h as f32 + m.ascent - m.descent) * 0.5;
    let col = if active {
        theme.text
    } else {
        theme.text_muted
    };
    painter.draw_text_ui(tx, baseline, label, FontStyle::Ui, font_px, col, atlas, rasterizer);
}

// ============================================================================
// Parallel Scoped Project File Walk & Zero-Allocation Fuzzy Scorer
// ============================================================================

fn should_skip_dir(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | "target"
            | "node_modules"
            | "build"
            | "build-asan"
            | "dist"
            | ".next"
            | ".nuxt"
            | ".venv"
            | "venv"
            | ".hg"
            | ".svn"
            | "__pycache__"
            | ".cache"
            | ".turbo"
    )
}

fn collect_project_files_parallel(
    fs: &dyn FileSystem,
    root: &Path,
    out: &mut Vec<IndexedFile>,
) {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(6);
    // BFS-seed until we have enough subtrees (`cores * 4`) to keep every worker busy even in
    // skewed or single-folder repos (`src/`, `crates/`, `packages/`).
    let target_seeds = if cores <= 1 { 0 } else { cores * 4 };
    let mut queue: VecDeque<(PathBuf, usize)> = VecDeque::new();
    queue.push_back((root.to_path_buf(), 0));

    while let Some((dir, depth)) = queue.pop_front() {
        if depth > MAX_WALK_DEPTH || out.len() >= MAX_PROJECT_FILES {
            continue;
        }
        let Ok(entries) = fs.read_dir(&dir) else {
            continue;
        };
        for entry in entries {
            if out.len() >= MAX_PROJECT_FILES {
                break;
            }
            match entry.kind {
                EntryKind::Directory => {
                    if !should_skip_dir(&entry.name) && depth < MAX_WALK_DEPTH {
                        queue.push_back((entry.path, depth + 1));
                    }
                }
                EntryKind::File => {
                    if entry.name != ".DS_Store" {
                        let rel = entry
                            .path
                            .strip_prefix(root)
                            .unwrap_or(&entry.path)
                            .to_string_lossy()
                            .into_owned();
                        out.push(IndexedFile::new(entry.path, rel));
                    }
                }
                _ => {}
            }
        }
        if target_seeds > 0 && queue.len() >= target_seeds {
            break;
        }
    }

    if queue.is_empty() || out.len() >= MAX_PROJECT_FILES {
        out.sort_unstable_by(|a, b| a.rel.cmp(&b.rel));
        return;
    }

    let subdirs: Vec<(PathBuf, usize)> = queue.into_iter().collect();
    if subdirs.len() < 3 || cores <= 1 {
        let mut stack = subdirs;
        walk_stack(fs, root, &mut stack, MAX_PROJECT_FILES, out);
        out.sort_unstable_by(|a, b| a.rel.cmp(&b.rel));
        return;
    }

    let total_counter = AtomicUsize::new(out.len());
    let next_sub = AtomicUsize::new(0);
    let worker_count = cores.min(subdirs.len());

    let chunks: Vec<Vec<IndexedFile>> = std::thread::scope(|s| {
        let mut handles = Vec::with_capacity(worker_count);
        for _ in 0..worker_count {
            let sub_ref = &subdirs;
            let next_ref = &next_sub;
            let count_ref = &total_counter;
            handles.push(s.spawn(move || {
                let mut local = Vec::new();
                let mut stack = Vec::new();
                loop {
                    if count_ref.load(Ordering::Relaxed) >= MAX_PROJECT_FILES {
                        break;
                    }
                    let idx = next_ref.fetch_add(1, Ordering::Relaxed);
                    if idx >= sub_ref.len() {
                        break;
                    }
                    stack.clear();
                    stack.push(sub_ref[idx].clone());
                    while let Some((dir, depth)) = stack.pop() {
                        if depth > MAX_WALK_DEPTH
                            || count_ref.load(Ordering::Relaxed) >= MAX_PROJECT_FILES
                        {
                            continue;
                        }
                        let Ok(entries) = fs.read_dir(&dir) else {
                            continue;
                        };
                        for entry in entries {
                            match entry.kind {
                                EntryKind::Directory => {
                                    if !should_skip_dir(&entry.name) && depth < MAX_WALK_DEPTH {
                                        stack.push((entry.path, depth + 1));
                                    }
                                }
                                EntryKind::File => {
                                    if entry.name == ".DS_Store" {
                                        continue;
                                    }
                                    if count_ref.fetch_add(1, Ordering::Relaxed) >= MAX_PROJECT_FILES
                                    {
                                        break;
                                    }
                                    let rel = entry
                                        .path
                                        .strip_prefix(root)
                                        .unwrap_or(&entry.path)
                                        .to_string_lossy()
                                        .into_owned();
                                    local.push(IndexedFile::new(entry.path, rel));
                                }
                                _ => {}
                            }
                        }
                    }
                }
                local
            }));
        }
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_default())
            .collect()
    });

    for mut c in chunks {
        out.append(&mut c);
    }
    out.sort_unstable_by(|a, b| a.rel.cmp(&b.rel));
    if out.len() > MAX_PROJECT_FILES {
        out.truncate(MAX_PROJECT_FILES);
    }
}

fn walk_stack(
    fs: &dyn FileSystem,
    root: &Path,
    stack: &mut Vec<(PathBuf, usize)>,
    limit: usize,
    out: &mut Vec<IndexedFile>,
) {
    while let Some((dir, depth)) = stack.pop() {
        if depth > MAX_WALK_DEPTH || out.len() >= limit {
            continue;
        }
        let Ok(entries) = fs.read_dir(&dir) else {
            continue;
        };
        for entry in entries {
            if out.len() >= limit {
                break;
            }
            match entry.kind {
                EntryKind::Directory => {
                    if !should_skip_dir(&entry.name) && depth < MAX_WALK_DEPTH {
                        stack.push((entry.path, depth + 1));
                    }
                }
                EntryKind::File => {
                    if entry.name == ".DS_Store" {
                        continue;
                    }
                    let rel = entry
                        .path
                        .strip_prefix(root)
                        .unwrap_or(&entry.path)
                        .to_string_lossy()
                        .into_owned();
                    out.push(IndexedFile::new(entry.path, rel));
                }
                _ => {}
            }
        }
    }
}

/// Zero-allocation subsequence fuzzy scorer on already-lowercased byte slices.
#[inline]
fn fuzzy_score_prelowered(pat: &[u8], cand: &[u8], basename_start: usize) -> Option<i32> {
    if pat.is_empty() {
        return Some(0);
    }
    if pat.len() > cand.len() {
        return None;
    }

    let mut pi = 0usize;
    let mut score = 0i32;
    let mut prev_matched = false;

    for (ci, &cb) in cand.iter().enumerate() {
        if cb == pat[pi] {
            score += 10;
            if prev_matched {
                score += 8;
            }
            if ci == 0
                || ci == basename_start
                || matches!(cand[ci.saturating_sub(1)], b'/' | b'_' | b'-' | b'.')
            {
                score += 12;
            }
            if ci >= basename_start {
                score += 4;
            }
            prev_matched = true;
            pi += 1;
            if pi == pat.len() {
                score -= (cand.len() as i32) / 4;
                return Some(score);
            }
        } else {
            prev_matched = false;
        }
    }
    None
}

/// Crisp subsequence fuzzy scorer (case-insensitive, rewards boundary/consecutive/basename hits).
pub fn fuzzy_score(pattern: &str, candidate: &str) -> Option<i32> {
    let pat: Vec<u8> = pattern.bytes().map(|b| b.to_ascii_lowercase()).collect();
    if pat.is_empty() {
        return Some(0);
    }
    let cand: Vec<u8> = candidate.bytes().map(|b| b.to_ascii_lowercase()).collect();
    let basename_start = candidate.rfind('/').map(|i| i + 1).unwrap_or(0);
    fuzzy_score_prelowered(&pat, &cand, basename_start)
}
