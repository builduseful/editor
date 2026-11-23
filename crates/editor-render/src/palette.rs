//! Custom-drawn Quick Open (`Cmd+P`) and Project-Wide SWAR Search (`Cmd+Shift+F`) palette.
//!
//! Built with zero external libraries using Build Useful design-system tokens:
//! - Bounded directory walk skipping noisy build/VCS folders (`.git`, `target`, `node_modules`, `build`).
//! - Crisp subsequence fuzzy matcher for Quick Open.
//! - Fast SWAR literal search across project text files (skipping binaries and huge files).

use std::path::{Path, PathBuf};

use editor_core::swar::{find_byte, find_subslice};
use editor_platform::{EntryKind, FileSystem, FontStyle, GlyphRasterizer, Key, Modifiers, PixelRect};

use crate::atlas::GlyphAtlas;
use crate::painter::Painter;
use crate::text_input::TextInput;
use crate::theme::{px, Theme};

const MAX_PROJECT_FILES: usize = 5_000;
const MAX_WALK_DEPTH: usize = 10;
const MAX_SEARCH_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_SEARCH_RESULTS: usize = 200;
const MAX_VISIBLE_ITEMS: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteMode {
    Closed,
    QuickOpen,
    Commands,
    ProjectSearch,
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteAction {
    None,
    Close,
    Open {
        path: PathBuf,
        line_0based: Option<usize>,
        col_bytes: Option<usize>,
    },
    RunCommand(String),
    Changed,
}

pub struct CommandPalette {
    pub mode: PaletteMode,
    pub input: TextInput,
    pub case_sensitive: bool,
    root: Option<PathBuf>,
    all_files: Vec<(PathBuf, String)>, // (absolute path, relative display path)
    all_commands: Vec<PaletteCommandEntry>,
    pub items: Vec<PaletteItem>,
    pub selected: usize,
    pub status_summary: String,
    card_rect: PixelRect,
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
            root: None,
            all_files: Vec::new(),
            all_commands: Vec::new(),
            items: Vec::new(),
            selected: 0,
            status_summary: String::new(),
            card_rect: PixelRect::ZERO,
            item_rects: Vec::new(),
        }
    }

    pub fn is_open(&self) -> bool {
        self.mode != PaletteMode::Closed
    }

    pub fn close(&mut self) {
        self.mode = PaletteMode::Closed;
        self.input.focused = false;
        self.all_commands.clear();
        self.items.clear();
        self.item_rects.clear();
    }

    pub fn open_commands(&mut self, commands: Vec<PaletteCommandEntry>) {
        self.mode = PaletteMode::Commands;
        self.input = TextInput::new("Run a command");
        self.input.focused = true;
        self.selected = 0;
        self.all_commands = commands;
        self.refresh_command_results();
    }

    pub fn open_quick_open(&mut self, fs: &dyn FileSystem, root: Option<&Path>) {
        self.mode = PaletteMode::QuickOpen;
        self.input = TextInput::new("Open file by name");
        self.input.focused = true;
        self.selected = 0;
        self.root = root.map(Path::to_path_buf);
        self.all_commands.clear();
        self.all_files.clear();
        if let Some(r) = root {
            collect_project_files(fs, r, r, 0, &mut self.all_files);
        }
        self.refresh_results(fs);
    }

    pub fn open_project_search(
        &mut self,
        fs: &dyn FileSystem,
        root: Option<&Path>,
        seed: Option<&str>,
    ) {
        self.mode = PaletteMode::ProjectSearch;
        self.input = TextInput::new("Search text in folder");
        if let Some(s) = seed {
            if !s.is_empty() && !s.contains('\n') {
                self.input.set_text(s);
            }
        }
        self.input.focused = true;
        self.selected = 0;
        self.root = root.map(Path::to_path_buf);
        self.all_commands.clear();
        self.all_files.clear();
        if let Some(r) = root {
            collect_project_files(fs, r, r, 0, &mut self.all_files);
        }
        self.refresh_results(fs);
    }

    fn refresh_command_results(&mut self) {
        self.items.clear();
        self.selected = 0;
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
        let query = self.input.text.trim();

        match self.mode {
            PaletteMode::Closed | PaletteMode::Commands => {}
            PaletteMode::QuickOpen => {
                if query.is_empty() {
                    for (abs, rel) in self.all_files.iter().take(60) {
                        let name = abs
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or(rel.as_str())
                            .to_string();
                        self.items.push(PaletteItem {
                            path: abs.clone(),
                            command_id: None,
                            primary: name,
                            secondary: rel.clone(),
                            shortcut: String::new(),
                            line_0based: None,
                            col_bytes: None,
                        });
                    }
                    self.status_summary = format!("{} files", self.all_files.len());
                } else {
                    let mut scored: Vec<(i32, usize)> = Vec::new();
                    for (idx, (_, rel)) in self.all_files.iter().enumerate() {
                        if let Some(score) = fuzzy_score(query, rel) {
                            scored.push((score, idx));
                        }
                    }
                    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
                    let total_hits = scored.len();
                    for (_, idx) in scored.into_iter().take(60) {
                        let (abs, rel) = &self.all_files[idx];
                        let name = abs
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or(rel.as_str())
                            .to_string();
                        self.items.push(PaletteItem {
                            path: abs.clone(),
                            command_id: None,
                            primary: name,
                            secondary: rel.clone(),
                            shortcut: String::new(),
                            line_0based: None,
                            col_bytes: None,
                        });
                    }
                    self.status_summary = format!("{total_hits} matches");
                }
            }
            PaletteMode::ProjectSearch => {
                if query.len() < 2 {
                    self.status_summary = if query.is_empty() {
                        format!("{} files indexed", self.all_files.len())
                    } else {
                        "Type at least 2 characters".to_string()
                    };
                    return;
                }
                let needle = if self.case_sensitive {
                    query.as_bytes().to_vec()
                } else {
                    query.bytes().map(|b| b.to_ascii_lowercase()).collect()
                };
                let mut matched_files = 0usize;
                let mut buf = Vec::with_capacity(64 * 1024);
                let mut lower = Vec::new();

                for (abs, rel) in &self.all_files {
                    if self.items.len() >= MAX_SEARCH_RESULTS {
                        break;
                    }
                    let Ok(handle) = fs.open_read(abs) else { continue };
                    let len = handle.len();
                    if len == 0 || len > MAX_SEARCH_FILE_BYTES {
                        continue;
                    }
                    buf.clear();
                    buf.resize(len as usize, 0);
                    let mut got = 0usize;
                    while got < buf.len() {
                        match handle.read_at(got as u64, &mut buf[got..]) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => got += n,
                        }
                    }
                    buf.truncate(got);
                    // Skip binary files (NUL in first 8 KB).
                    if find_byte(&buf[..buf.len().min(8192)], 0).is_some() {
                        continue;
                    }

                    let search_slice: &[u8] = if self.case_sensitive {
                        &buf
                    } else {
                        lower.clear();
                        lower.extend(buf.iter().map(|b| b.to_ascii_lowercase()));
                        &lower
                    };

                    let mut file_hit = false;
                    let mut pos = 0usize;
                    let mut cur_line = 0usize;
                    let mut last_line_start = 0usize;
                    while pos + needle.len() <= search_slice.len()
                        && self.items.len() < MAX_SEARCH_RESULTS
                    {
                        let Some(rel_hit) = find_subslice(&search_slice[pos..], &needle) else {
                            break;
                        };
                        let abs_hit = pos + rel_hit;
                        // Advance line counter from `last_line_start` up to `abs_hit`.
                        for i in last_line_start..abs_hit {
                            if buf[i] == b'\n' {
                                cur_line += 1;
                                last_line_start = i + 1;
                            }
                        }
                        let line_end = find_byte(&buf[abs_hit..], b'\n')
                            .map(|d| abs_hit + d)
                            .unwrap_or(buf.len());
                        let mut preview_slice = &buf[last_line_start..line_end];
                        if preview_slice.ends_with(b"\r") {
                            preview_slice = &preview_slice[..preview_slice.len() - 1];
                        }
                        let preview = String::from_utf8_lossy(preview_slice)
                            .trim()
                            .chars()
                            .take(90)
                            .collect::<String>();

                        self.items.push(PaletteItem {
                            path: abs.clone(),
                            command_id: None,
                            primary: format!("{}:{}", rel, cur_line + 1),
                            secondary: preview,
                            shortcut: String::new(),
                            line_0based: Some(cur_line),
                            col_bytes: Some(abs_hit - last_line_start),
                        });
                        file_hit = true;
                        // Jump to next line so we don't spam duplicate rows on one line.
                        pos = (line_end + 1).max(abs_hit + needle.len());
                    }
                    if file_hit {
                        matched_files += 1;
                    }
                }
                self.status_summary = if self.items.len() >= MAX_SEARCH_RESULTS {
                    format!("{MAX_SEARCH_RESULTS}+ results in {matched_files} files")
                } else {
                    format!("{} results in {} files", self.items.len(), matched_files)
                };
            }
        }
    }

    pub fn handle_key(
        &mut self,
        fs: &dyn FileSystem,
        key: Key,
        mods: Modifiers,
        text: Option<&str>,
        clipboard: Option<&str>,
    ) -> (PaletteAction, Option<String>) {
        match key {
            Key::Escape => {
                self.close();
                return (PaletteAction::Close, None);
            }
            Key::Up => {
                if !self.items.is_empty() {
                    self.selected = self.selected.saturating_sub(1);
                }
                return (PaletteAction::Changed, None);
            }
            Key::Down => {
                if !self.items.is_empty() {
                    self.selected = (self.selected + 1).min(self.items.len() - 1);
                }
                return (PaletteAction::Changed, None);
            }
            Key::Enter => {
                if let Some(item) = self.items.get(self.selected).cloned() {
                    self.close();
                    if let Some(cmd_id) = item.command_id {
                        return (PaletteAction::RunCommand(cmd_id), None);
                    }
                    return (
                        PaletteAction::Open {
                            path: item.path,
                            line_0based: item.line_0based,
                            col_bytes: item.col_bytes,
                        },
                        None,
                    );
                }
                return (PaletteAction::None, None);
            }
            _ => {}
        }

        let (changed, copied) = self.input.handle_key(key, mods, text, clipboard);
        if changed {
            self.refresh_results(fs);
        }
        (PaletteAction::Changed, copied)
    }

    pub fn handle_click(&mut self, x_px: f32, y_px: f32) -> PaletteAction {
        if !self.is_open() {
            return PaletteAction::None;
        }
        let xi = x_px.round() as i32;
        let yi = y_px.round() as i32;
        if !self.card_rect.contains(xi, yi) {
            self.close();
            return PaletteAction::Close;
        }
        for &(idx, rect) in &self.item_rects {
            if rect.contains(xi, yi) {
                if let Some(item) = self.items.get(idx).cloned() {
                    self.close();
                    if let Some(cmd_id) = item.command_id {
                        return PaletteAction::RunCommand(cmd_id);
                    }
                    return PaletteAction::Open {
                        path: item.path,
                        line_0based: item.line_0based,
                        col_bytes: item.col_bytes,
                    };
                }
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
            self.item_rects.clear();
            return;
        }
        let scale = painter.scale();
        let scrim = if theme.is_dark {
            crate::theme::Color::rgba(0, 0, 0, 120)
        } else {
            crate::theme::Color::rgba(0x17, 0x1a, 0x1f, 64)
        };
        painter.fill_rect(bounds, scrim);

        let pad = px(10.0, scale);
        let input_h = px(28.0, scale);
        let row_h = if self.mode == PaletteMode::ProjectSearch {
            px(38.0, scale)
        } else {
            px(28.0, scale)
        };
        let visible_n = self.items.len().min(MAX_VISIBLE_ITEMS);
        let header_h = px(20.0, scale);
        let card_w = px(560.0, scale).min((bounds.w - px(32.0, scale)).max(px(260.0, scale)));
        let list_h = (visible_n as i32) * row_h + if visible_n == 0 { px(28.0, scale) } else { 0 };
        let card_h = pad + header_h + px(6.0, scale) + input_h + px(8.0, scale) + list_h + pad;
        let card = PixelRect::new(
            bounds.x + (bounds.w - card_w) / 2,
            bounds.y + px(48.0, scale),
            card_w,
            card_h.min(bounds.h - px(64.0, scale)),
        );
        self.card_rect = card;

        let radius = 8.0 * scale;
        painter.fill_rounded_rect(card, radius, theme.surface);
        painter.stroke_rounded_rect(card, radius, (1.0 * scale).max(1.0), theme.border_control);

        // Title + count summary row.
        let heading = match self.mode {
            PaletteMode::QuickOpen => "Quick Open",
            PaletteMode::Commands => "Command Palette",
            PaletteMode::ProjectSearch => "Search in Folder",
            PaletteMode::Closed => "",
        };
        let label_px = 11.5 * scale;
        let lm = atlas.metrics(rasterizer, FontStyle::UiBold, label_px);
        let hy = (card.y + pad) as f32 + lm.ascent;
        painter.draw_text_ui(
            (card.x + pad) as f32,
            hy,
            heading,
            FontStyle::UiBold,
            label_px,
            theme.text,
            atlas,
            rasterizer,
        );
        let sw = Painter::measure_text_ui(
            &self.status_summary,
            FontStyle::Ui,
            label_px,
            atlas,
            rasterizer,
        );
        painter.draw_text_ui(
            (card.right() - pad) as f32 - sw,
            hy,
            &self.status_summary,
            FontStyle::Ui,
            label_px,
            theme.text_muted,
            atlas,
            rasterizer,
        );

        // Input box.
        let input_rect = PixelRect::new(
            card.x + pad,
            card.y + pad + header_h + px(4.0, scale),
            card.w - pad * 2,
            input_h,
        );
        self.input
            .draw(painter, input_rect, theme, atlas, rasterizer, caret_visible);

        // Results list.
        let list_y = input_rect.bottom() + px(8.0, scale);
        self.item_rects.clear();
        let list_clip = PixelRect::new(
            card.x + pad,
            list_y,
            (card.w - pad * 2).max(0),
            (card.bottom() - pad - list_y).max(0),
        );
        painter.push_clip(list_clip);

        let font_px = 12.5 * scale;
        let sub_px = 11.0 * scale;
        let fm = atlas.metrics(rasterizer, FontStyle::Ui, font_px);
        let sm = atlas.metrics(rasterizer, FontStyle::Ui, sub_px);

        if self.items.is_empty() {
            let empty_msg = if self.mode != PaletteMode::Commands && self.root.is_none() {
                "Open a folder to search files"
            } else {
                "No matches"
            };
            painter.draw_text_ui(
                (list_clip.x + px(6.0, scale)) as f32,
                list_clip.y as f32 + fm.ascent + px(4.0, scale) as f32,
                empty_msg,
                FontStyle::Ui,
                font_px,
                theme.text_muted,
                atlas,
                rasterizer,
            );
        } else {
            // Keep selected item inside the visible window.
            let start_idx = if self.selected >= MAX_VISIBLE_ITEMS {
                self.selected + 1 - MAX_VISIBLE_ITEMS
            } else {
                0
            };
            let end_idx = (start_idx + MAX_VISIBLE_ITEMS).min(self.items.len());
            for (slot, idx) in (start_idx..end_idx).enumerate() {
                let item = &self.items[idx];
                let ry = list_clip.y + (slot as i32) * row_h;
                let r = PixelRect::new(list_clip.x, ry, list_clip.w, row_h);
                self.item_rects.push((idx, r));

                if idx == self.selected {
                    painter.fill_rounded_rect(r, 4.0 * scale, theme.selection);
                }

                let tx = (r.x + px(8.0, scale)) as f32;
                if self.mode == PaletteMode::ProjectSearch {
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
                        let sm_m = atlas.metrics(rasterizer, FontStyle::Mono, sub_px);
                        let kbase =
                            pill.y as f32 + (pill.h as f32 + sm_m.ascent - sm_m.descent) * 0.5;
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
                    painter.push_clip(PixelRect::new(
                        r.x,
                        r.y,
                        (right_limit - r.x).max(0),
                        r.h,
                    ));
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
    }
}

fn should_skip_dir(name: &str) -> bool {
    matches!(
        name,
        ".git" | "target" | "node_modules" | "build" | "build-asan" | ".hg" | ".svn" | "__pycache__"
    )
}

fn collect_project_files(
    fs: &dyn FileSystem,
    root: &Path,
    dir: &Path,
    depth: usize,
    out: &mut Vec<(PathBuf, String)>,
) {
    if depth > MAX_WALK_DEPTH || out.len() >= MAX_PROJECT_FILES {
        return;
    }
    let Ok(entries) = fs.read_dir(dir) else { return };
    for entry in entries {
        if out.len() >= MAX_PROJECT_FILES {
            break;
        }
        match entry.kind {
            EntryKind::Directory => {
                if !should_skip_dir(&entry.name) {
                    collect_project_files(fs, root, &entry.path, depth + 1, out);
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
                out.push((entry.path, rel));
            }
            _ => {}
        }
    }
}

/// Crisp subsequence fuzzy scorer (case-insensitive, rewards boundary/consecutive/basename hits).
pub fn fuzzy_score(pattern: &str, candidate: &str) -> Option<i32> {
    let pat: Vec<u8> = pattern.bytes().map(|b| b.to_ascii_lowercase()).collect();
    if pat.is_empty() {
        return Some(0);
    }
    let cand: Vec<u8> = candidate.bytes().map(|b| b.to_ascii_lowercase()).collect();
    let basename_start = candidate.rfind('/').map(|i| i + 1).unwrap_or(0);

    let mut pi = 0usize;
    let mut score = 0i32;
    let mut prev_matched = false;

    for (ci, &cb) in cand.iter().enumerate() {
        if cb == pat[pi] {
            score += 10;
            if prev_matched {
                score += 8; // consecutive bonus
            }
            if ci == 0 || ci == basename_start || matches!(cand[ci.saturating_sub(1)], b'/' | b'_' | b'-' | b'.') {
                score += 12; // word/path boundary bonus
            }
            if ci >= basename_start {
                score += 4; // basename preference
            }
            prev_matched = true;
            pi += 1;
            if pi == pat.len() {
                // Slightly prefer shorter paths when scores tie.
                score -= (cand.len() as i32) / 4;
                return Some(score);
            }
        } else {
            prev_matched = false;
        }
    }
    None
}
