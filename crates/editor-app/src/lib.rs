//! Platform-agnostic application state machine and event controller.
//!
//! Connects `editor-core`, `editor-render`, and the minimal `editor-platform` traits.
//! 100% testable headlessly by feeding `InputEvent`s and inspecting document state
//! and rendered pixels in `Framebuffer`.

pub mod keymap;
pub mod vim;
mod vim_exec;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use editor_core::{
    next_grapheme_boundary, next_word_boundary, prev_grapheme_boundary, prev_word_boundary,
    word_range_at, Document, IdentityCheck, Language, SearchMatch, SearchOptions,
};
use editor_platform::{
    AppCommand, Clipboard, ConfirmChoice, CursorShape, FileSystem, FontStyle, Framebuffer,
    GlyphRasterizer, InputEvent, Key, Modifiers, MouseButton, PixelRect, PlatformHost, SurfaceMut,
    SystemTheme, ThemePreference,
};
use editor_render::{
    chrome::{draw_status_bar, status_bar_height_px},
    A11yNode, A11yRole, CommandPalette, ConfirmModal, FindBarAction, FindBarState, GlyphAtlas,
    GotoLineState, ImeComposition, Painter, PaletteAction, ScrollbarHit, ShortcutsAction,
    ShortcutsOverlay, Sidebar, SidebarAction, SidebarHit, StatusBarData, Theme, Viewport,
};

pub use keymap::{KeyChord, KeyScope, Keymap, ShortcutEntry};
pub use vim::{VimMode, VimState};

#[derive(Debug, Clone, Copy, Default)]
pub struct AppLayout {
    pub sidebar: PixelRect,
    pub find_bar: PixelRect,
    pub goto_bar: PixelRect,
    /// Active pane's viewport rect (equals full right area when unsplit, or left/right half when split).
    pub viewport: PixelRect,
    /// Full combined viewport area across both panes when split.
    pub split_area: PixelRect,
    pub left_pane: PixelRect,
    pub right_pane: PixelRect,
    pub split_divider: PixelRect,
    pub status_bar: PixelRect,
}

/// Independent state for a second side-by-side split pane.
pub struct PaneState {
    pub doc: Document,
    pub caret: u64,
    pub anchor: u64,
    preferred_x_px: Option<f32>,
    pub viewport: Viewport,
    pub search_matches: Vec<SearchMatch>,
    pub active_match: Option<usize>,
}

impl PaneState {
    fn new_from_font(font_size_pt: f32, word_wrap: bool) -> Self {
        let mut vp = Viewport::new();
        vp.font_size_pt = font_size_pt;
        vp.word_wrap = word_wrap;
        Self {
            doc: Document::new_untitled(),
            caret: 0,
            anchor: 0,
            preferred_x_px: None,
            viewport: vp,
            search_matches: Vec::new(),
            active_match: None,
        }
    }

    fn selection(&self) -> Option<(u64, u64)> {
        if self.anchor == self.caret {
            None
        } else if self.anchor < self.caret {
            Some((self.anchor, self.caret))
        } else {
            Some((self.caret, self.anchor))
        }
    }
}

#[derive(Debug, Clone)]
enum PendingDiscardAction {
    NewFile,
    OpenPath(PathBuf),
    OpenAt {
        path: PathBuf,
        line_0based: Option<usize>,
        col_bytes: Option<usize>,
    },
    OpenFileDialog,
    Reload,
    CloseActivePane,
    Quit,
}

pub struct EditorApp {
    fs: Arc<dyn FileSystem>,
    pub doc: Document,
    pub caret: u64,
    pub anchor: u64,
    preferred_x_px: Option<f32>,

    pub sidebar: Sidebar,
    pub viewport: Viewport,
    /// Optional second side-by-side pane; `self.doc`/`self.viewport` ALWAYS hold the active pane!
    pub secondary_pane: Option<PaneState>,
    /// Active pane index (`0` = left, `1` = right when split).
    pub active_pane: usize,
    /// Split ratio for left pane (`0.2..=0.8`, default `0.5`).
    pub split_ratio: f32,
    pub find_bar: FindBarState,
    pub goto_line: GotoLineState,
    pub palette: CommandPalette,
    pub shortcuts: ShortcutsOverlay,
    pub confirm_modal: ConfirmModal,
    pub keymap: Keymap,
    pub vim: VimState,
    pub ime: ImeComposition,
    pending_discard: Option<PendingDiscardAction>,
    pub use_in_app_modal: bool,

    pub search_matches: Vec<SearchMatch>,
    pub active_match: Option<usize>,

    pub theme_pref: ThemePreference,
    pub system_theme: SystemTheme,
    /// Global fallback word wrap for files without a per-extension override.
    pub default_word_wrap: bool,
    /// Per-file-extension word wrap overrides (lowercase extension without dot, e.g. `"md" => true`).
    pub wrap_by_ext: BTreeMap<String, bool>,

    pub atlas: GlyphAtlas,
    pub framebuffer: Framebuffer,
    pub layout: AppLayout,
    /// Bounding pixel rect updated during the most recent `render()` call (for zero-copy IOSurface upload).
    pub last_rendered_dirty: PixelRect,

    window_focused: bool,
    caret_visible: bool,
    last_blink_ms: u64,
    now_ms: u64,
    notice: Option<(String, bool, u64)>,

    dragging_selection: bool,
    dragging_gutter_anchor_line: Option<usize>,
    dragging_find_input: bool,
    dragging_replace_input: bool,
    drag_click_count: u8,
    drag_word_anchor: (u64, u64),
    dragging_sidebar_edge: bool,
    dragging_split_edge: bool,
    dirty_frame: bool,
}

impl EditorApp {
    pub fn new(fs: Arc<dyn FileSystem>, width_px: u32, height_px: u32, scale: f32) -> Self {
        Self::with_framebuffer(
            fs,
            Framebuffer::new(width_px.max(1), height_px.max(1), scale),
        )
    }

    /// Construct with a 0-byte virtual framebuffer for zero-copy OS surfaces (`IOSurface` / `wl_shm`).
    pub fn new_zero_copy(
        fs: Arc<dyn FileSystem>,
        width_px: u32,
        height_px: u32,
        scale: f32,
    ) -> Self {
        Self::with_framebuffer(
            fs,
            Framebuffer::new_virtual(width_px.max(1), height_px.max(1), scale),
        )
    }

    fn with_framebuffer(fs: Arc<dyn FileSystem>, framebuffer: Framebuffer) -> Self {
        let mut app = Self {
            fs,
            doc: Document::new_untitled(),
            caret: 0,
            anchor: 0,
            preferred_x_px: None,
            sidebar: Sidebar::new(),
            viewport: Viewport::new(),
            secondary_pane: None,
            active_pane: 0,
            split_ratio: 0.5,
            find_bar: FindBarState::new(),
            goto_line: GotoLineState::new(),
            palette: CommandPalette::new(),
            shortcuts: ShortcutsOverlay::new(),
            confirm_modal: ConfirmModal::default(),
            keymap: Keymap::new(),
            vim: VimState::new(),
            ime: ImeComposition::default(),
            pending_discard: None,
            use_in_app_modal: false,
            search_matches: Vec::new(),
            active_match: None,
            theme_pref: ThemePreference::System,
            system_theme: SystemTheme::Light,
            default_word_wrap: false,
            wrap_by_ext: default_wrap_by_ext(),
            atlas: GlyphAtlas::new(),
            framebuffer,
            layout: AppLayout::default(),
            last_rendered_dirty: PixelRect::ZERO,
            window_focused: true,
            caret_visible: true,
            last_blink_ms: 0,
            now_ms: 0,
            notice: None,
            dragging_selection: false,
            dragging_gutter_anchor_line: None,
            dragging_find_input: false,
            dragging_replace_input: false,
            drag_click_count: 1,
            drag_word_anchor: (0, 0),
            dragging_sidebar_edge: false,
            dragging_split_edge: false,
            dirty_frame: true,
        };
        app.recompute_layout();
        app
    }

    /// Load persisted user preferences from the host adapter (NSUserDefaults on macOS).
    pub fn load_preferences(&mut self, host: &dyn PlatformHost) {
        if let Some(w) = host.load_pref("SidebarWidth").and_then(|s| s.parse::<f32>().ok()) {
            self.sidebar.set_width_pt(w);
        }
        if let Some(sz) = host.load_pref("FontSize").and_then(|s| s.parse::<f32>().ok()) {
            self.viewport.font_size_pt = sz.clamp(8.0, 36.0);
        }
        if let Some(wrap) = host.load_pref("WordWrap") {
            self.default_word_wrap = wrap == "1" || wrap == "true";
        }
        if let Some(ext_pref) = host.load_pref("WordWrapByExt") {
            self.wrap_by_ext = parse_wrap_by_ext(&ext_pref);
        }
        if let Some(theme) = host.load_pref("Theme") {
            self.theme_pref = match theme.as_str() {
                "light" => ThemePreference::Light,
                "dark" => ThemePreference::Dark,
                _ => ThemePreference::System,
            };
        }
        if let Some(vim) = host.load_pref("VimMode") {
            self.vim.set_enabled(vim == "1" || vim == "true");
        }
        self.sync_wrap_for_panes();
        self.recompute_layout();
        self.mark_dirty();
    }

    pub fn save_preferences(&self, host: &mut dyn PlatformHost) {
        host.save_pref("SidebarWidth", &format!("{:.0}", self.sidebar.width_pt()));
        host.save_pref("FontSize", &format!("{:.1}", self.viewport.font_size_pt));
        host.save_pref("WordWrap", if self.default_word_wrap { "1" } else { "0" });
        host.save_pref("WordWrapByExt", &format_wrap_by_ext(&self.wrap_by_ext));
        let t = match self.theme_pref {
            ThemePreference::System => "system",
            ThemePreference::Light => "light",
            ThemePreference::Dark => "dark",
        };
        host.save_pref("Theme", t);
        host.save_pref("VimMode", if self.vim.enabled { "1" } else { "0" });
    }

    /// Resolve effective word wrap for `path` (per-extension override -> global default).
    pub fn effective_word_wrap(&self, path: Option<&Path>) -> bool {
        if let Some(ext) = file_ext_key(path) {
            if let Some(&v) = self.wrap_by_ext.get(&ext) {
                return v;
            }
        }
        self.default_word_wrap
    }

    /// Set or clear a per-extension word wrap override (`Some(true/false)` or `None` to reset to default).
    pub fn set_word_wrap_for_ext(
        &mut self,
        ext: &str,
        enabled: Option<bool>,
        host: &mut dyn PlatformHost,
    ) {
        let key = ext.trim().trim_start_matches('.').to_ascii_lowercase();
        if key.is_empty() {
            return;
        }
        match enabled {
            Some(v) => {
                self.wrap_by_ext.insert(key, v);
            }
            None => {
                self.wrap_by_ext.remove(&key);
            }
        }
        self.sync_wrap_for_panes();
        self.save_preferences(host);
        self.mark_dirty();
        host.request_redraw();
    }

    fn sync_wrap_for_panes(&mut self) {
        let primary_wrap = self.effective_word_wrap(self.doc.path());
        if self.viewport.word_wrap != primary_wrap {
            self.viewport.word_wrap = primary_wrap;
            if primary_wrap {
                self.viewport.scroll_x_px = 0.0;
            }
        }
        let sec_path = self
            .secondary_pane
            .as_ref()
            .and_then(|s| s.doc.path().map(Path::to_path_buf));
        let sec_wrap = self.effective_word_wrap(sec_path.as_deref());
        if let Some(ref mut sec) = self.secondary_pane {
            if sec.viewport.word_wrap != sec_wrap {
                sec.viewport.word_wrap = sec_wrap;
                if sec_wrap {
                    sec.viewport.scroll_x_px = 0.0;
                }
            }
        }
    }

    pub fn active_theme(&self) -> Theme {
        match self.theme_pref {
            ThemePreference::System => Theme::for_system(self.system_theme),
            ThemePreference::Light => Theme::light(),
            ThemePreference::Dark => Theme::dark(),
        }
    }

    pub fn set_theme_preference(&mut self, pref: ThemePreference) {
        self.theme_pref = pref;
        self.mark_dirty();
    }

    pub fn set_sidebar_root(&mut self, dir: &Path) {
        self.sidebar.set_root(self.fs.as_ref(), dir);
        self.mark_dirty();
    }

    pub fn open_file(
        &mut self,
        path: &Path,
        host: &mut dyn PlatformHost,
    ) -> Result<(), String> {
        let doc = Document::open(self.fs.as_ref(), path).map_err(|e| e.to_string())?;
        let canonical = doc.path().map(Path::to_path_buf);
        self.doc = doc;
        self.caret = 0;
        self.anchor = 0;
        self.preferred_x_px = None;
        self.viewport.reset_for_document();
        self.viewport.word_wrap = self.effective_word_wrap(canonical.as_deref());
        self.search_matches.clear();
        self.active_match = None;

        // Sidebar root is set once (if unset) and never re-rooted on subsequent file opens!
        if let Some(ref p) = canonical {
            if self.sidebar.root().is_none() {
                if let Some(parent) = p.parent() {
                    self.sidebar.set_root(self.fs.as_ref(), parent);
                }
            }
            if self.sidebar.is_under_root(p) {
                self.sidebar.reveal(self.fs.as_ref(), p);
            }
        }

        self.sync_title(host);
        self.mark_dirty();
        host.request_redraw();
        Ok(())
    }

    pub fn open_path_or_dir(
        &mut self,
        path: &Path,
        host: &mut dyn PlatformHost,
    ) -> Result<(), String> {
        if let Ok(st) = self.fs.stat(path) {
            if st.is_dir {
                self.sidebar.set_visible(true);
                self.sidebar.set_root(self.fs.as_ref(), path);
                self.mark_dirty();
                host.request_redraw();
                return Ok(());
            }
        }
        self.open_file(path, host)
    }

    pub fn selection(&self) -> Option<(u64, u64)> {
        if self.anchor == self.caret {
            None
        } else if self.anchor < self.caret {
            Some((self.anchor, self.caret))
        } else {
            Some((self.caret, self.anchor))
        }
    }

    pub fn selected_text(&mut self) -> Option<String> {
        let (a, b) = self.selection()?;
        let bytes = self.doc.read_range(a, (b - a) as usize);
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }

    pub fn mark_dirty(&mut self) {
        self.dirty_frame = true;
        self.framebuffer.dirty.mark_full();
    }

    pub fn mark_dirty_rect(&mut self, rect: PixelRect) {
        if rect.is_empty() {
            return;
        }
        self.dirty_frame = true;
        self.framebuffer.dirty.mark_rect(rect);
    }

    pub fn is_dirty_frame(&self) -> bool {
        self.dirty_frame
    }

    pub fn set_notice(&mut self, msg: impl Into<String>, is_error: bool) {
        self.notice = Some((msg.into(), is_error, self.now_ms + 3_500));
        self.mark_dirty();
    }

    pub fn sync_title(&self, host: &mut dyn PlatformHost) {
        let name = self.doc.file_name();
        let title = if self.doc.is_dirty() {
            format!("{name} — Edited")
        } else {
            name.to_string()
        };
        host.set_title(&title);
    }

    pub fn is_split(&self) -> bool {
        self.secondary_pane.is_some()
    }

    /// Open a side-by-side right pane (optionally opening `path` inside it, else opening the current file or untitled).
    pub fn split_pane_with(&mut self, path: Option<&Path>, host: &mut dyn PlatformHost) {
        if self.secondary_pane.is_none() {
            let mut right = PaneState::new_from_font(self.viewport.font_size_pt, self.viewport.word_wrap);
            if let Some(p) = path {
                if let Ok(doc) = Document::open(self.fs.as_ref(), p) {
                    right.doc = doc;
                }
            } else if let Some(cur_path) = self.doc.path() {
                if let Ok(doc) = Document::open(self.fs.as_ref(), cur_path) {
                    right.doc = doc;
                }
            }
            self.secondary_pane = Some(right);
            self.active_pane = 0;
            // Focus the newly opened right pane and apply its per-extension word wrap!
            self.focus_pane(1, host);
            self.sync_wrap_for_panes();
        } else {
            self.focus_pane(1, host);
            if let Some(p) = path {
                let _ = self.open_path_or_dir(p, host);
            }
        }
        self.recompute_layout();
        self.mark_dirty();
        host.request_redraw();
    }

    /// Close the active split pane immediately, promoting the remaining pane to full width.
    pub fn close_split(&mut self, host: &mut dyn PlatformHost) {
        if let Some(other) = self.secondary_pane.take() {
            self.doc = other.doc;
            self.caret = other.caret;
            self.anchor = other.anchor;
            self.preferred_x_px = other.preferred_x_px;
            self.viewport = other.viewport;
            self.search_matches = other.search_matches;
            self.active_match = other.active_match;
            self.active_pane = 0;
            self.recompute_layout();
            self.sync_title(host);
            self.mark_dirty();
            host.request_redraw();
        }
    }

    /// Close the active split pane, prompting to save first if it has unsaved edits.
    pub fn request_close_active_pane(&mut self, host: &mut dyn PlatformHost) {
        if self.secondary_pane.is_none() {
            return;
        }
        self.request_discard_or_run(PendingDiscardAction::CloseActivePane, host);
    }

    /// Swap left and right split panes in O(1) while keeping focus on the active document.
    pub fn swap_panes(&mut self, host: &mut dyn PlatformHost) {
        if self.secondary_pane.is_none() {
            return;
        }
        self.active_pane = if self.active_pane == 0 { 1 } else { 0 };
        self.recompute_layout();
        self.mark_dirty();
        host.request_redraw();
    }

    /// Switch focus to pane `idx` (`0` = left, `1` = right) in O(1) via zero-copy swap!
    pub fn focus_pane(&mut self, idx: usize, host: &mut dyn PlatformHost) {
        let target = idx.min(1);
        if self.secondary_pane.is_none() || self.active_pane == target {
            return;
        }
        if let Some(other) = self.secondary_pane.as_mut() {
            std::mem::swap(&mut self.doc, &mut other.doc);
            std::mem::swap(&mut self.caret, &mut other.caret);
            std::mem::swap(&mut self.anchor, &mut other.anchor);
            std::mem::swap(&mut self.preferred_x_px, &mut other.preferred_x_px);
            std::mem::swap(&mut self.viewport, &mut other.viewport);
            std::mem::swap(&mut self.search_matches, &mut other.search_matches);
            std::mem::swap(&mut self.active_match, &mut other.active_match);
            self.active_pane = target;
            self.recompute_layout();
            self.sync_title(host);
            self.mark_dirty();
            host.request_redraw();
        }
    }

    pub fn recompute_layout(&mut self) {
        let w = self.framebuffer.width as i32;
        let h = self.framebuffer.height as i32;
        let scale = self.framebuffer.scale;

        let status_h = status_bar_height_px(scale);
        let main_h = (h - status_h).max(0);
        let sidebar_w = if self.sidebar.is_visible() {
            (self.sidebar.width_pt() * scale)
                .round()
                .min((w as f32 - 220.0 * scale).max(0.0)) as i32
        } else {
            0
        };
        let sidebar = PixelRect::new(0, 0, sidebar_w, main_h);
        let right_x = sidebar_w;
        let right_w = (w - sidebar_w).max(0);

        let find_h = self.find_bar.height_px(scale);
        let find_bar = PixelRect::new(right_x, 0, right_w, find_h);

        let goto_h = self.goto_line.height_px(scale);
        let goto_bar = PixelRect::new(right_x, find_bar.bottom(), right_w, goto_h);

        let vp_y = goto_bar.bottom();
        let vp_h = (main_h - vp_y).max(0);
        let split_area = PixelRect::new(right_x, vp_y, right_w, vp_h);

        let (left_pane, right_pane, split_divider, viewport) = if self.secondary_pane.is_some() {
            let hairline = (1.0 * scale).round().max(1.0) as i32;
            let header_h = (24.0 * scale).round() as i32;
            let left_w = ((right_w as f32) * self.split_ratio)
                .round()
                .clamp(120.0 * scale, (right_w as f32 - 120.0 * scale).max(120.0 * scale))
                as i32;
            let div = PixelRect::new(right_x + left_w, vp_y, hairline, vp_h);
            let lp = PixelRect::new(right_x, vp_y + header_h, left_w, (vp_h - header_h).max(0));
            let rp_x = div.right();
            let rp_w = (right_x + right_w - rp_x).max(0);
            let rp = PixelRect::new(rp_x, vp_y + header_h, rp_w, (vp_h - header_h).max(0));
            let active_vp = if self.active_pane == 0 { lp } else { rp };
            (lp, rp, div, active_vp)
        } else {
            (split_area, PixelRect::ZERO, PixelRect::ZERO, split_area)
        };

        let status_bar = PixelRect::new(0, main_h, w, status_h);

        self.layout = AppLayout {
            sidebar,
            find_bar,
            goto_bar,
            viewport,
            split_area,
            left_pane,
            right_pane,
            split_divider,
            status_bar,
        };
    }

    /// Render into the owned CPU `Framebuffer` (used by headless tests & PNG snapshots).
    pub fn render(&mut self, rasterizer: &mut dyn GlyphRasterizer) -> bool {
        if !self.dirty_frame {
            return false;
        }
        self.framebuffer.ensure_pixels();
        // Split borrow cleanly via local raw slice from our own Vec<u32> (valid for duration of paint).
        let w = self.framebuffer.width;
        let h = self.framebuffer.height;
        let scale = self.framebuffer.scale;
        let ptr = self.framebuffer.pixels.as_mut_ptr();
        let len = self.framebuffer.pixels.len();
        let surf = SurfaceMut {
            width: w,
            height: h,
            stride: w as usize,
            scale,
            pixels: unsafe { std::slice::from_raw_parts_mut(ptr, len) },
        };
        self.render_into(surf, rasterizer)
    }

    /// Zero-copy render directly into an OS-owned surface (e.g. a locked macOS `IOSurface`).
    /// Never allocates a CPU framebuffer copy!
    pub fn render_into(
        &mut self,
        surf: SurfaceMut<'_>,
        rasterizer: &mut dyn GlyphRasterizer,
    ) -> bool {
        self.render_into_with_damage(surf, rasterizer, PixelRect::ZERO, false)
            .is_some()
    }

    /// Zero-copy render with swapchain buffer-age damage tracking.
    ///
    /// - `extra_dirty`: damage accumulated on this swapchain buffer slot since it was last painted.
    /// - `force_full`: force a full-surface paint (newly allocated, resized, or purged surface).
    /// Returns `Some(painted_rect)` when pixels were written, or `None` if nothing needed painting.
    pub fn render_into_with_damage(
        &mut self,
        surf: SurfaceMut<'_>,
        rasterizer: &mut dyn GlyphRasterizer,
        extra_dirty: PixelRect,
        force_full: bool,
    ) -> Option<PixelRect> {
        if !self.dirty_frame && !force_full {
            return None;
        }
        self.framebuffer.width = surf.width;
        self.framebuffer.height = surf.height;
        self.framebuffer.scale = surf.scale;
        self.recompute_layout();

        let full_bounds = surf.bounds();
        let app_dirty = if self.framebuffer.dirty.is_full()
            || self.confirm_modal.visible
            || self.palette.is_open()
            || self.shortcuts.visible
        {
            full_bounds
        } else {
            self.framebuffer.dirty.bounding_rect(full_bounds)
        };
        let effective_clip = if force_full {
            full_bounds
        } else {
            app_dirty.union(extra_dirty).intersect(full_bounds)
        };
        if effective_clip.is_empty() {
            self.framebuffer.dirty.clear();
            self.dirty_frame = false;
            return None;
        }

        let theme = self.active_theme();
        let layout = self.layout;
        let (caret_line, caret_col) = self.doc.byte_to_line_col(self.caret);
        let sel = self.selection();
        let sel_bytes = sel.map(|(a, b)| (b - a) as usize).unwrap_or(0);
        let notice = self.notice.as_ref().map(|(m, e, _)| (m.as_str(), *e));
        let doc_focused = self.window_focused
            && !self.confirm_modal.visible
            && !self.palette.is_open()
            && !self.shortcuts.visible
            && !self.sidebar.is_focused()
            && !self.find_bar.is_focused()
            && !self.goto_line.visible;
        self.viewport.block_caret = self.vim.uses_block_caret();
        if let Some(ref mut sec) = self.secondary_pane {
            sec.viewport.block_caret = false;
        }
        let matching_brackets = if sel.is_none() && doc_focused {
            self.doc.find_matching_bracket(self.caret)
        } else {
            None
        };

        let mut painter = Painter::from_surface(surf);
        painter.push_clip(effective_clip);

        // 1. Sidebar
        if !layout.sidebar.intersect(effective_clip).is_empty() {
            self.sidebar
                .draw(&mut painter, layout.sidebar, &theme, &mut self.atlas, rasterizer);
        }

        // 2. Find / Replace bar
        if !layout.find_bar.intersect(effective_clip).is_empty() {
            self.find_bar.draw(
                &mut painter,
                layout.find_bar,
                &theme,
                &mut self.atlas,
                rasterizer,
                self.caret_visible,
            );
        }

        // 3. Go-To-Line bar
        if !layout.goto_bar.intersect(effective_clip).is_empty() {
            self.goto_line.draw(
                &mut painter,
                layout.goto_bar,
                &theme,
                &mut self.atlas,
                rasterizer,
                self.caret_visible,
            );
        }

        // 4. Document viewport(s) — single pane OR side-by-side split panes!
        if self.secondary_pane.is_some() {
            let scale = painter.scale();
            let hairline = (1.0 * scale).round().max(1.0) as i32;
            let header_h = (24.0 * scale).round() as i32;
            let left_hdr = PixelRect::new(layout.left_pane.x, layout.split_area.y, layout.left_pane.w, header_h);
            let right_hdr = PixelRect::new(layout.right_pane.x, layout.split_area.y, layout.right_pane.w, header_h);

            let (active_rect, inactive_rect) = if self.active_pane == 0 {
                (layout.left_pane, layout.right_pane)
            } else {
                (layout.right_pane, layout.left_pane)
            };
            let (active_hdr, inactive_hdr) = if self.active_pane == 0 {
                (left_hdr, right_hdr)
            } else {
                (right_hdr, left_hdr)
            };

            // Draw quiet pane headers (Build Useful tokens: page bg, hairline bottom, bold active name).
            if !active_hdr.intersect(effective_clip).is_empty() {
                draw_pane_header(
                    &mut painter,
                    active_hdr,
                    self.doc.file_name(),
                    self.doc.is_dirty(),
                    true,
                    &theme,
                    &mut self.atlas,
                    rasterizer,
                );
            }
            if let Some(ref mut sec) = self.secondary_pane {
                if !inactive_hdr.intersect(effective_clip).is_empty() {
                    draw_pane_header(
                        &mut painter,
                        inactive_hdr,
                        sec.doc.file_name(),
                        sec.doc.is_dirty(),
                        false,
                        &theme,
                        &mut self.atlas,
                        rasterizer,
                    );
                }
                if !inactive_rect.intersect(effective_clip).is_empty() {
                    let sec_sel = sec.selection();
                    sec.viewport.draw(
                        &mut painter,
                        inactive_rect,
                        &mut sec.doc,
                        sec.caret,
                        sec_sel,
                        &sec.search_matches,
                        sec.active_match,
                        None,
                        &ImeComposition::default(),
                        false,
                        false,
                        &theme,
                        &mut self.atlas,
                        rasterizer,
                    );
                }
            }
            if !active_rect.intersect(effective_clip).is_empty() {
                self.viewport.draw(
                    &mut painter,
                    active_rect,
                    &mut self.doc,
                    self.caret,
                    sel,
                    &self.search_matches,
                    self.active_match,
                    matching_brackets,
                    &self.ime,
                    self.caret_visible,
                    doc_focused,
                    &theme,
                    &mut self.atlas,
                    rasterizer,
                );
                self.ime.caret_rect_px = self.viewport.last_caret_rect;
            }
            // Crisp 1px vertical split hairline divider.
            let div_rect = PixelRect::new(
                layout.split_divider.x,
                layout.split_area.y,
                hairline,
                layout.split_area.h,
            );
            if !div_rect.intersect(effective_clip).is_empty() {
                painter.fill_rect(div_rect, theme.border_subtle);
            }
        } else if !layout.viewport.intersect(effective_clip).is_empty() {
            self.viewport.draw(
                &mut painter,
                layout.viewport,
                &mut self.doc,
                self.caret,
                sel,
                &self.search_matches,
                self.active_match,
                matching_brackets,
                &self.ime,
                self.caret_visible,
                doc_focused,
                &theme,
                &mut self.atlas,
                rasterizer,
            );
            self.ime.caret_rect_px = self.viewport.last_caret_rect;
        }

        // 5. Status bar
        if !layout.status_bar.intersect(effective_clip).is_empty() {
            let vim_badge_owned = self.vim.status_badge();
            let status_data = StatusBarData {
                file_label: self.doc.file_name(),
                dirty: self.doc.is_dirty(),
                line_1based: caret_line + 1,
                col_1based: caret_col + 1,
                selection_bytes: sel_bytes,
                language: self.doc.language(),
                eol: self.doc.eol(),
                word_wrap: self.viewport.word_wrap,
                theme_pref: self.theme_pref,
                notice,
                vim_badge: vim_badge_owned.as_deref(),
            };
            draw_status_bar(
                &mut painter,
                layout.status_bar,
                &status_data,
                &theme,
                &mut self.atlas,
                rasterizer,
            );
        }

        // 6. Optional Quick Open / Project Search palette overlay.
        if self.palette.is_open() {
            self.palette.draw(
                &mut painter,
                full_bounds,
                &theme,
                &mut self.atlas,
                rasterizer,
                self.caret_visible,
            );
        }

        // 7. Optional Keyboard Shortcuts overlay (`Cmd+?`).
        if self.shortcuts.visible {
            self.shortcuts.draw(
                &mut painter,
                full_bounds,
                &theme,
                &mut self.atlas,
                rasterizer,
                self.caret_visible,
            );
        }

        // 8. Optional in-app confirmation modal overlay.
        if self.confirm_modal.visible {
            self.confirm_modal
                .draw(&mut painter, full_bounds, &theme, &mut self.atlas, rasterizer);
        }

        painter.pop_clip();
        self.last_rendered_dirty = app_dirty;
        self.framebuffer.dirty.clear();
        self.dirty_frame = false;
        Some(app_dirty)
    }

    /// Snapshot of logical UI regions for future OS accessibility bridges.
    pub fn a11y_snapshot(&self) -> Vec<A11yNode> {
        let mut nodes = Vec::with_capacity(5);
        nodes.push(A11yNode {
            role: A11yRole::Window,
            label: self.doc.file_name().to_string(),
            bounds: self.framebuffer.bounds(),
            focused: self.window_focused,
        });
        if self.sidebar.is_visible() {
            nodes.push(A11yNode {
                role: A11yRole::Sidebar,
                label: "Files".to_string(),
                bounds: self.layout.sidebar,
                focused: self.sidebar.is_focused(),
            });
        }
        nodes.push(A11yNode {
            role: A11yRole::Document,
            label: self.doc.file_name().to_string(),
            bounds: self.layout.viewport,
            focused: self.window_focused
                && !self.sidebar.is_focused()
                && !self.find_bar.is_focused()
                && !self.goto_line.visible,
        });
        nodes.push(A11yNode {
            role: A11yRole::StatusBar,
            label: "Status".to_string(),
            bounds: self.layout.status_bar,
            focused: false,
        });
        nodes
    }

    /// Process a single platform-neutral `InputEvent`.
    pub fn handle_event(
        &mut self,
        event: InputEvent,
        rasterizer: &mut dyn GlyphRasterizer,
        clipboard: &mut dyn Clipboard,
        host: &mut dyn PlatformHost,
    ) {
        // Intercept keyboard & mouse when the custom in-app confirmation modal is open.
        if self.confirm_modal.visible {
            match event {
                InputEvent::KeyDown { key, .. } => {
                    let choice = match key {
                        Key::Enter | Key::Char('s') | Key::Char('S') => Some(ConfirmChoice::Save),
                        Key::Char('d') | Key::Char('D') => Some(ConfirmChoice::Discard),
                        Key::Escape => Some(ConfirmChoice::Cancel),
                        _ => None,
                    };
                    if let Some(c) = choice {
                        self.resolve_confirm_modal(c, host);
                    }
                    return;
                }
                InputEvent::MouseDown {
                    x_px,
                    y_px,
                    button: MouseButton::Left,
                    ..
                } => {
                    if let Some(c) = self.confirm_modal.hit_test(x_px, y_px) {
                        self.resolve_confirm_modal(c, host);
                    }
                    return;
                }
                InputEvent::Resize {
                    width_px,
                    height_px,
                    scale,
                } => {
                    self.framebuffer.resize(width_px.max(1), height_px.max(1), scale);
                    self.recompute_layout();
                    self.mark_dirty();
                    return;
                }
                _ => return,
            }
        }

        // Intercept keyboard, scroll & mouse when the Keyboard Shortcuts overlay (`Cmd+?`) is open.
        if self.shortcuts.visible {
            match event {
                InputEvent::KeyDown { key, mods, text } => {
                    self.caret_visible = true;
                    self.last_blink_ms = self.now_ms;
                    if self.keymap.lookup_command(key, mods) == Some(AppCommand::ShowShortcuts) {
                        self.shortcuts.close();
                        self.mark_dirty();
                        host.request_redraw();
                        return;
                    }
                    let act = self.shortcuts.handle_key(key, mods, text.as_deref());
                    self.handle_shortcuts_action(act, rasterizer, clipboard, host);
                    return;
                }
                InputEvent::Scroll { dy_px, .. } => {
                    if self.shortcuts.handle_scroll(dy_px) {
                        self.mark_dirty();
                        host.request_redraw();
                    }
                    return;
                }
                InputEvent::MouseDown {
                    x_px,
                    y_px,
                    button: MouseButton::Left,
                    ..
                } => {
                    let act = self.shortcuts.handle_mouse_down(x_px, y_px);
                    self.handle_shortcuts_action(act, rasterizer, clipboard, host);
                    return;
                }
                InputEvent::Command(AppCommand::ShowShortcuts) => {
                    self.shortcuts.close();
                    self.mark_dirty();
                    host.request_redraw();
                    return;
                }
                InputEvent::Command(cmd)
                    if matches!(
                        cmd,
                        AppCommand::Copy
                            | AppCommand::Cut
                            | AppCommand::Paste
                            | AppCommand::SelectAll
                    ) =>
                {
                    self.handle_command(cmd, rasterizer, clipboard, host);
                    return;
                }
                InputEvent::Resize {
                    width_px,
                    height_px,
                    scale,
                } => {
                    self.framebuffer.resize(width_px.max(1), height_px.max(1), scale);
                    self.recompute_layout();
                    self.mark_dirty();
                    return;
                }
                InputEvent::Tick { now_ms } => {
                    self.now_ms = now_ms;
                    if self.window_focused && now_ms.saturating_sub(self.last_blink_ms) >= 530 {
                        self.caret_visible = !self.caret_visible;
                        self.last_blink_ms = now_ms;
                        self.mark_dirty();
                        host.request_redraw();
                    }
                    return;
                }
                _ => return,
            }
        }

        // Intercept keyboard & mouse when Quick Open / Project Search palette is open.
        if self.palette.is_open() {
            match event {
                InputEvent::KeyDown { key, mods, text } => {
                    self.caret_visible = true;
                    self.last_blink_ms = self.now_ms;
                    let clip = clipboard.get_text();
                    let (act, copied) = self.palette.handle_key(
                        self.fs.as_ref(),
                        key,
                        mods,
                        text.as_deref(),
                        clip.as_deref(),
                    );
                    if let Some(c) = copied {
                        clipboard.set_text(&c);
                    }
                    self.handle_palette_action(act, rasterizer, host);
                    return;
                }
                InputEvent::MouseDown {
                    x_px,
                    y_px,
                    button: MouseButton::Left,
                    ..
                } => {
                    let act = self.palette.handle_click(x_px, y_px);
                    self.handle_palette_action(act, rasterizer, host);
                    return;
                }
                InputEvent::Command(cmd)
                    if matches!(
                        cmd,
                        AppCommand::Copy
                            | AppCommand::Cut
                            | AppCommand::Paste
                            | AppCommand::SelectAll
                            | AppCommand::QuickOpen
                            | AppCommand::ProjectSearch
                    ) =>
                {
                    self.handle_command(cmd, rasterizer, clipboard, host);
                    return;
                }
                InputEvent::Resize {
                    width_px,
                    height_px,
                    scale,
                } => {
                    self.framebuffer.resize(width_px.max(1), height_px.max(1), scale);
                    self.recompute_layout();
                    self.mark_dirty();
                    return;
                }
                InputEvent::Tick { now_ms } => {
                    self.now_ms = now_ms;
                    if self.window_focused && now_ms.saturating_sub(self.last_blink_ms) >= 530 {
                        self.caret_visible = !self.caret_visible;
                        self.last_blink_ms = now_ms;
                        self.mark_dirty();
                        host.request_redraw();
                    }
                    return;
                }
                _ => return,
            }
        }

        match event {
            InputEvent::Resize {
                width_px,
                height_px,
                scale,
            } => {
                self.framebuffer.resize(width_px.max(1), height_px.max(1), scale);
                self.recompute_layout();
                self.mark_dirty();
            }
            InputEvent::SystemThemeChanged(st) => {
                if self.system_theme != st {
                    self.system_theme = st;
                    self.mark_dirty();
                    host.request_redraw();
                }
            }
            InputEvent::FocusChanged(focused) => {
                self.window_focused = focused;
                if focused {
                    self.caret_visible = true;
                    self.last_blink_ms = self.now_ms;
                    self.sidebar.refresh(self.fs.as_ref());
                    match self.doc.check_identity(self.fs.as_ref()) {
                        IdentityCheck::ReloadedClean => {
                            self.caret = self.caret.min(self.doc.len_bytes());
                            self.anchor = self.caret;
                            self.set_notice("Reloaded external changes", false);
                        }
                        IdentityCheck::StaleDirty => {
                            self.set_notice("File changed on disk", true);
                        }
                        _ => {}
                    }
                }
                self.mark_dirty();
                host.request_redraw();
            }
            InputEvent::OpenPath(path) => {
                self.request_discard_or_run(PendingDiscardAction::OpenPath(path), host);
            }
            InputEvent::Tick { now_ms } => {
                self.now_ms = now_ms;
                if let Some((_, _, exp)) = &self.notice {
                    if now_ms >= *exp {
                        self.notice = None;
                        self.mark_dirty_rect(self.layout.status_bar);
                        host.request_redraw();
                    }
                }
                if self.window_focused && now_ms.saturating_sub(self.last_blink_ms) >= 530 {
                    self.caret_visible = !self.caret_visible;
                    self.last_blink_ms = now_ms;
                    if self.find_bar.is_focused() {
                        self.mark_dirty_rect(self.layout.find_bar);
                    } else if self.goto_line.visible {
                        self.mark_dirty_rect(self.layout.goto_bar);
                    } else if !self.sidebar.is_focused() && !self.viewport.last_caret_rect.is_empty()
                    {
                        // Sub-0.05 ms partial dirty-rect redraw for caret blink!
                        let r = self.viewport.last_caret_rect;
                        self.mark_dirty_rect(PixelRect::new(r.x - 4, r.y - 2, r.w + 8, r.h + 4));
                    } else {
                        self.mark_dirty();
                    }
                    host.request_redraw();
                }
            }
            InputEvent::Command(cmd) => {
                self.handle_command(cmd, rasterizer, clipboard, host);
            }
            InputEvent::KeyDown { key, mods, text } => {
                self.caret_visible = true;
                self.last_blink_ms = self.now_ms;
                self.handle_key_down(key, mods, text.as_deref(), rasterizer, clipboard, host);
            }
            InputEvent::MouseDown {
                x_px,
                y_px,
                button,
                click_count,
                mods,
            } => {
                self.caret_visible = true;
                self.last_blink_ms = self.now_ms;
                if button == MouseButton::Left {
                    self.handle_mouse_down(x_px, y_px, click_count, mods, rasterizer, host);
                }
            }
            InputEvent::MouseMove { x_px, y_px, .. } => {
                self.handle_mouse_move(x_px, y_px, rasterizer, host);
            }
            InputEvent::MouseUp { button, .. } => {
                if button == MouseButton::Left {
                    if self.dragging_sidebar_edge {
                        self.save_preferences(host);
                    }
                    self.dragging_selection = false;
                    self.dragging_gutter_anchor_line = None;
                    self.dragging_find_input = false;
                    self.dragging_replace_input = false;
                    self.dragging_sidebar_edge = false;
                    self.dragging_split_edge = false;
                    self.viewport.end_scrollbar_drag();
                }
            }
            InputEvent::Scroll {
                dx_px,
                dy_px,
                x_px,
                y_px,
                ..
            } => {
                self.handle_scroll(dx_px, dy_px, x_px, y_px, rasterizer, host);
            }
        }
    }

    fn request_discard_or_run(&mut self, action: PendingDiscardAction, host: &mut dyn PlatformHost) {
        if !self.doc.is_dirty() {
            self.execute_pending_action(action, host);
            return;
        }
        if self.use_in_app_modal {
            self.pending_discard = Some(action);
            self.confirm_modal.open(self.doc.file_name());
            self.mark_dirty();
            host.request_redraw();
        } else {
            let choice = host.confirm_discard(self.doc.file_name());
            match choice {
                ConfirmChoice::Save => {
                    if self.save_current(host) {
                        self.execute_pending_action(action, host);
                    }
                }
                ConfirmChoice::Discard => self.execute_pending_action(action, host),
                ConfirmChoice::Cancel => {}
            }
        }
    }

    fn resolve_confirm_modal(&mut self, choice: ConfirmChoice, host: &mut dyn PlatformHost) {
        self.confirm_modal.close();
        let action = self.pending_discard.take();
        match choice {
            ConfirmChoice::Save => {
                if self.save_current(host) {
                    if let Some(a) = action {
                        self.execute_pending_action(a, host);
                    }
                }
            }
            ConfirmChoice::Discard => {
                if let Some(a) = action {
                    self.execute_pending_action(a, host);
                }
            }
            ConfirmChoice::Cancel => {}
        }
        self.mark_dirty();
        host.request_redraw();
    }

    fn execute_pending_action(&mut self, action: PendingDiscardAction, host: &mut dyn PlatformHost) {
        match action {
            PendingDiscardAction::NewFile => {
                self.doc = Document::new_untitled();
                self.caret = 0;
                self.anchor = 0;
                self.viewport.reset_for_document();
                self.search_matches.clear();
                self.active_match = None;
                self.sync_title(host);
                self.mark_dirty();
                host.request_redraw();
            }
            PendingDiscardAction::OpenPath(path) => {
                if let Err(e) = self.open_path_or_dir(&path, host) {
                    self.set_notice(e, true);
                } else {
                    self.sidebar.set_focused(false);
                }
            }
            PendingDiscardAction::OpenAt {
                path,
                line_0based,
                col_bytes,
            } => {
                let already_open = self.doc.path() == Some(path.as_path());
                let res = if already_open {
                    Ok(())
                } else {
                    self.open_file(&path, host)
                };
                match res {
                    Ok(()) => {
                        self.sidebar.set_focused(false);
                        if let Some(line) = line_0based {
                            self.doc.tree_mut().ensure_indexed_for_line(line + 16);
                            let clamped = line.min(self.doc.editable_line_count().saturating_sub(1));
                            if let Some((start, content_end, _)) = self.doc.line_byte_range(clamped)
                            {
                                let off = start
                                    + (col_bytes.unwrap_or(0) as u64).min(content_end - start);
                                self.caret = off;
                                self.anchor = off;
                                self.viewport.scroll_top_line = clamped.saturating_sub(6);
                                self.viewport.scroll_sub_y_px = 0.0;
                            }
                        }
                        self.mark_dirty();
                        host.request_redraw();
                    }
                    Err(e) => self.set_notice(e, true),
                }
            }
            PendingDiscardAction::OpenFileDialog => {
                if let Some(path) = host.open_file_dialog() {
                    if let Err(e) = self.open_file(&path, host) {
                        self.set_notice(e, true);
                    }
                }
            }
            PendingDiscardAction::Reload => {
                if let Err(e) = self.doc.reload(self.fs.as_ref()) {
                    self.set_notice(format!("Reload failed: {e}"), true);
                } else {
                    self.caret = self.caret.min(self.doc.len_bytes());
                    self.anchor = self.caret;
                    self.refresh_search();
                    self.sync_title(host);
                    self.set_notice("Reloaded", false);
                }
                self.mark_dirty();
                host.request_redraw();
            }
            PendingDiscardAction::CloseActivePane => {
                self.close_split(host);
            }
            PendingDiscardAction::Quit => {
                host.request_quit();
            }
        }
    }

    fn doc_metrics(&mut self, rasterizer: &mut dyn GlyphRasterizer) -> editor_platform::FontMetrics {
        let font_px = self.viewport.font_px(self.framebuffer.scale);
        self.atlas.metrics(rasterizer, FontStyle::Mono, font_px)
    }

    fn save_current(&mut self, host: &mut dyn PlatformHost) -> bool {
        if self.doc.path().is_none() {
            return self.save_as_dialog(host);
        }
        match self.doc.save(self.fs.as_ref(), None, false) {
            Ok(report) => {
                self.sync_title(host);
                self.set_notice(format!("Saved ({})", report.method.as_str()), false);
                host.request_redraw();
                true
            }
            Err(e) => {
                self.set_notice(format!("Save failed: {e}"), true);
                host.request_redraw();
                false
            }
        }
    }

    fn save_as_dialog(&mut self, host: &mut dyn PlatformHost) -> bool {
        let Some(dst) = host.save_file_dialog(Some(self.doc.file_name())) else {
            return false;
        };
        match self.doc.save_as(self.fs.as_ref(), &dst, None, false) {
            Ok(_) => {
                self.sync_title(host);
                self.sidebar.refresh(self.fs.as_ref());
                if let Some(p) = self.doc.path().map(Path::to_path_buf) {
                    if self.sidebar.is_under_root(&p) {
                        self.sidebar.reveal(self.fs.as_ref(), &p);
                    }
                }
                self.set_notice("Saved", false);
                host.request_redraw();
                true
            }
            Err(e) => {
                self.set_notice(format!("Save As failed: {e}"), true);
                host.request_redraw();
                false
            }
        }
    }

    fn handle_palette_action(
        &mut self,
        act: PaletteAction,
        rasterizer: &mut dyn GlyphRasterizer,
        host: &mut dyn PlatformHost,
    ) {
        match act {
            PaletteAction::None => {}
            PaletteAction::Close | PaletteAction::Changed => {
                self.mark_dirty();
                host.request_redraw();
            }
            PaletteAction::Open {
                path,
                line_0based,
                col_bytes,
            } => {
                let same_file = self.doc.path() == Some(path.as_path());
                if same_file {
                    self.execute_pending_action(
                        PendingDiscardAction::OpenAt {
                            path,
                            line_0based,
                            col_bytes,
                        },
                        host,
                    );
                    let m = self.doc_metrics(rasterizer);
                    self.viewport
                        .ensure_caret_visible(&mut self.doc, self.caret, m, self.framebuffer.scale);
                } else {
                    self.request_discard_or_run(
                        PendingDiscardAction::OpenAt {
                            path,
                            line_0based,
                            col_bytes,
                        },
                        host,
                    );
                }
            }
        }
    }

    fn handle_shortcuts_action(
        &mut self,
        act: ShortcutsAction,
        rasterizer: &mut dyn GlyphRasterizer,
        clipboard: &mut dyn Clipboard,
        host: &mut dyn PlatformHost,
    ) {
        match act {
            ShortcutsAction::None => {}
            ShortcutsAction::Changed | ShortcutsAction::Close => {
                self.mark_dirty();
                host.request_redraw();
            }
            ShortcutsAction::RunAction(id) => {
                let cmd = editor_platform::ipc::IpcCommand::Action(id.to_string());
                let _ = self.execute_ipc(&cmd, rasterizer, clipboard, host);
                self.mark_dirty();
                host.request_redraw();
            }
        }
    }

    fn active_text_input_mut(&mut self) -> Option<&mut editor_render::TextInput> {
        if self.shortcuts.visible {
            Some(&mut self.shortcuts.input)
        } else if self.palette.is_open() {
            Some(&mut self.palette.input)
        } else if self.goto_line.visible {
            Some(&mut self.goto_line.input)
        } else if self.find_bar.visible && self.find_bar.find_input.focused {
            Some(&mut self.find_bar.find_input)
        } else if self.find_bar.visible && self.find_bar.replace_input.focused {
            Some(&mut self.find_bar.replace_input)
        } else {
            None
        }
    }

    fn handle_command(
        &mut self,
        cmd: AppCommand,
        rasterizer: &mut dyn GlyphRasterizer,
        clipboard: &mut dyn Clipboard,
        host: &mut dyn PlatformHost,
    ) {
        match cmd {
            AppCommand::NewFile => {
                self.request_discard_or_run(PendingDiscardAction::NewFile, host);
            }
            AppCommand::OpenFile => {
                self.request_discard_or_run(PendingDiscardAction::OpenFileDialog, host);
            }
            AppCommand::OpenFolder => {
                if let Some(dir) = host.open_folder_dialog() {
                    self.sidebar.set_visible(true);
                    self.sidebar.set_root(self.fs.as_ref(), &dir);
                    self.recompute_layout();
                    self.mark_dirty();
                    host.request_redraw();
                }
            }
            AppCommand::Save => {
                self.save_current(host);
            }
            AppCommand::SaveAs => {
                self.save_as_dialog(host);
            }
            AppCommand::Reload => {
                self.request_discard_or_run(PendingDiscardAction::Reload, host);
            }
            AppCommand::Undo => {
                if let Some(pos) = self.doc.undo() {
                    self.caret = pos.min(self.doc.len_bytes());
                    self.anchor = self.caret;
                    self.after_document_mutated(rasterizer, host);
                }
            }
            AppCommand::Redo => {
                if let Some(pos) = self.doc.redo() {
                    self.caret = pos.min(self.doc.len_bytes());
                    self.anchor = self.caret;
                    self.after_document_mutated(rasterizer, host);
                }
            }
            AppCommand::Copy => {
                // Route menu Copy to focused TextInput when Find / Go-To-Line is active!
                if let Some(inp) = self.active_text_input_mut() {
                    if inp.has_selection() {
                        let s = inp.selected_text().to_string();
                        clipboard.set_text(&s);
                    }
                    return;
                }
                if let Some(text) = self.selected_text() {
                    clipboard.set_text(&text);
                }
            }
            AppCommand::Cut => {
                let in_find = self.find_bar.visible && self.find_bar.find_input.focused;
                let in_palette = self.palette.is_open();
                if let Some(inp) = self.active_text_input_mut() {
                    if inp.has_selection() {
                        let s = inp.selected_text().to_string();
                        clipboard.set_text(&s);
                        inp.delete_selection();
                        if in_palette {
                            self.palette.refresh_results(self.fs.as_ref());
                        } else if in_find {
                            self.refresh_search();
                        }
                        self.mark_dirty();
                        host.request_redraw();
                    }
                    return;
                }
                if let Some(text) = self.selected_text() {
                    clipboard.set_text(&text);
                    let (a, b) = self.selection().unwrap();
                    if let Ok(pos) = self.doc.edit(a, b - a, "", self.now_ms, false) {
                        self.caret = pos;
                        self.anchor = pos;
                        self.after_document_mutated(rasterizer, host);
                    }
                }
            }
            AppCommand::Paste => {
                let in_find = self.find_bar.visible && self.find_bar.find_input.focused;
                let in_palette = self.palette.is_open();
                if let Some(clip) = clipboard.get_text() {
                    if let Some(inp) = self.active_text_input_mut() {
                        inp.insert_str(&clip);
                        if in_palette {
                            self.palette.refresh_results(self.fs.as_ref());
                        } else if in_find {
                            self.refresh_search();
                        }
                        self.mark_dirty();
                        host.request_redraw();
                        return;
                    }
                    let (start, removed) = self
                        .selection()
                        .map(|(a, b)| (a, b - a))
                        .unwrap_or((self.caret, 0));
                    if let Ok(pos) = self.doc.edit(start, removed, &clip, self.now_ms, false) {
                        self.caret = pos;
                        self.anchor = pos;
                        self.after_document_mutated(rasterizer, host);
                    }
                }
            }
            AppCommand::SelectAll => {
                if let Some(inp) = self.active_text_input_mut() {
                    inp.select_all();
                    self.mark_dirty();
                    host.request_redraw();
                    return;
                }
                self.doc.tree_mut().ensure_fully_indexed();
                self.anchor = 0;
                self.caret = self.doc.len_bytes();
                self.mark_dirty();
                host.request_redraw();
            }
            AppCommand::Find => {
                self.goto_line.close();
                self.sidebar.set_focused(false);
                let seed = self.selected_text();
                self.find_bar.open(false, seed.as_deref());
                self.recompute_layout();
                self.refresh_search();
                self.mark_dirty();
                host.request_redraw();
            }
            AppCommand::Replace => {
                self.goto_line.close();
                self.sidebar.set_focused(false);
                let seed = self.selected_text();
                self.find_bar.open(true, seed.as_deref());
                self.recompute_layout();
                self.refresh_search();
                self.mark_dirty();
                host.request_redraw();
            }
            AppCommand::FindNext => {
                self.jump_search_match(true, rasterizer, host);
            }
            AppCommand::FindPrev => {
                self.jump_search_match(false, rasterizer, host);
            }
            AppCommand::GoToLine => {
                self.find_bar.close();
                self.palette.close();
                self.sidebar.set_focused(false);
                let (line, _) = self.doc.byte_to_line_col(self.caret);
                self.goto_line.open(line + 1);
                self.recompute_layout();
                self.mark_dirty();
                host.request_redraw();
            }
            AppCommand::QuickOpen => {
                self.goto_line.close();
                self.sidebar.set_focused(false);
                let root = self
                    .sidebar
                    .root()
                    .map(Path::to_path_buf)
                    .or_else(|| self.doc.path().and_then(|p| p.parent()).map(Path::to_path_buf));
                self.palette.open_quick_open(self.fs.as_ref(), root.as_deref());
                self.mark_dirty();
                host.request_redraw();
            }
            AppCommand::ProjectSearch => {
                self.goto_line.close();
                self.sidebar.set_focused(false);
                let seed = self.selected_text();
                let root = self
                    .sidebar
                    .root()
                    .map(Path::to_path_buf)
                    .or_else(|| self.doc.path().and_then(|p| p.parent()).map(Path::to_path_buf));
                self.palette
                    .open_project_search(self.fs.as_ref(), root.as_deref(), seed.as_deref());
                self.mark_dirty();
                host.request_redraw();
            }
            AppCommand::DuplicateLine => {
                self.duplicate_current_line(rasterizer, host);
            }
            AppCommand::DeleteLine => {
                self.delete_current_line(rasterizer, host);
            }
            AppCommand::MoveLineUp => {
                self.move_current_line(true, rasterizer, host);
            }
            AppCommand::MoveLineDown => {
                self.move_current_line(false, rasterizer, host);
            }
            AppCommand::ToggleComment => {
                self.toggle_line_comment(rasterizer, host);
            }
            AppCommand::ToggleWrap => {
                self.viewport.toggle_wrap();
                let enabled = self.viewport.word_wrap;
                if let Some(ext) = file_ext_key(self.doc.path()) {
                    self.wrap_by_ext.insert(ext.clone(), enabled);
                    self.set_notice(
                        format!(
                            "Word wrap {} for .{ext}",
                            if enabled { "on" } else { "off" }
                        ),
                        false,
                    );
                } else {
                    self.default_word_wrap = enabled;
                    self.set_notice(
                        if enabled {
                            "Word wrap on (default)"
                        } else {
                            "Word wrap off (default)"
                        },
                        false,
                    );
                }
                self.sync_wrap_for_panes();
                self.save_preferences(host);
                self.mark_dirty();
                host.request_redraw();
            }
            AppCommand::ToggleSidebar => {
                self.sidebar.toggle_visible();
                self.recompute_layout();
                self.mark_dirty();
                host.request_redraw();
            }
            AppCommand::SplitPane => {
                self.split_pane_with(None, host);
            }
            AppCommand::CloseSplit => {
                self.request_close_active_pane(host);
            }
            AppCommand::SwapPanes => {
                self.swap_panes(host);
            }
            AppCommand::FocusLeftPane => {
                self.focus_pane(0, host);
            }
            AppCommand::FocusRightPane => {
                self.focus_pane(1, host);
            }
            AppCommand::ZoomIn => {
                self.viewport.zoom_in();
                self.save_preferences(host);
                self.mark_dirty();
                host.request_redraw();
            }
            AppCommand::ZoomOut => {
                self.viewport.zoom_out();
                self.save_preferences(host);
                self.mark_dirty();
                host.request_redraw();
            }
            AppCommand::ZoomReset => {
                self.viewport.zoom_reset();
                self.save_preferences(host);
                self.mark_dirty();
                host.request_redraw();
            }
            AppCommand::SetTheme(pref) => {
                self.theme_pref = pref;
                self.save_preferences(host);
                self.mark_dirty();
                host.request_redraw();
            }
            AppCommand::ShowShortcuts => {
                if self.shortcuts.visible {
                    self.shortcuts.close();
                } else {
                    self.palette.close();
                    self.goto_line.close();
                    self.shortcuts.open(self.keymap.to_display_items());
                }
                self.mark_dirty();
                host.request_redraw();
            }
            AppCommand::ToggleVimMode => {
                let enabled = self.vim.toggle();
                self.save_preferences(host);
                self.set_notice(
                    if enabled {
                        "Vim mode enabled (Normal)"
                    } else {
                        "Vim mode disabled"
                    },
                    false,
                );
                self.mark_dirty();
                host.request_redraw();
            }
            AppCommand::Quit => {
                self.request_discard_or_run(PendingDiscardAction::Quit, host);
            }
        }
    }

    fn handle_key_down(
        &mut self,
        key: Key,
        mods: Modifiers,
        text: Option<&str>,
        rasterizer: &mut dyn GlyphRasterizer,
        clipboard: &mut dyn Clipboard,
        host: &mut dyn PlatformHost,
    ) {
        // 1. Context-scoped shortcut dispatch via single-source-of-truth `self.keymap`.
        if self.find_bar.visible {
            if let Some(cmd) = self.keymap.resolve(KeyScope::FindBar, key, mods) {
                self.handle_command(cmd, rasterizer, clipboard, host);
                return;
            }
        }
        if let Some(cmd) = self.keymap.resolve(KeyScope::Global, key, mods) {
            self.handle_command(cmd, rasterizer, clipboard, host);
            return;
        }

        // Route to Go-To-Line bar if open.
        if self.goto_line.visible {
            if key == Key::Escape {
                self.goto_line.close();
                self.recompute_layout();
                self.mark_dirty();
                host.request_redraw();
                return;
            }
            if key == Key::Enter {
                if let Some(line1) = self.goto_line.parsed_line_1based() {
                    let target = line1.saturating_sub(1);
                    self.doc.tree_mut().ensure_indexed_for_line(target);
                    let clamped = target.min(self.doc.editable_line_count().saturating_sub(1));
                    if let Some((start, _, _)) = self.doc.line_byte_range(clamped) {
                        self.caret = start;
                        self.anchor = start;
                        let m = self.doc_metrics(rasterizer);
                        self.viewport
                            .ensure_caret_visible(&mut self.doc, self.caret, m, self.framebuffer.scale);
                    }
                }
                self.goto_line.close();
                self.recompute_layout();
                self.mark_dirty();
                host.request_redraw();
                return;
            }
            let clip = clipboard.get_text();
            let (_, copied) = self
                .goto_line
                .input
                .handle_key(key, mods, text, clip.as_deref());
            if let Some(c) = copied {
                clipboard.set_text(&c);
            }
            self.mark_dirty();
            host.request_redraw();
            return;
        }

        // Route to Find / Replace bar if focused.
        if self.find_bar.is_focused() {
            if key == Key::Escape {
                self.find_bar.close();
                self.search_matches.clear();
                self.active_match = None;
                self.recompute_layout();
                self.mark_dirty();
                host.request_redraw();
                return;
            }
            if key == Key::Tab && self.find_bar.show_replace {
                let f = self.find_bar.find_input.focused;
                self.find_bar.find_input.focused = !f;
                self.find_bar.replace_input.focused = f;
                self.mark_dirty();
                host.request_redraw();
                return;
            }
            if key == Key::Enter {
                if self.find_bar.find_input.focused {
                    self.jump_search_match(!mods.shift, rasterizer, host);
                } else if self.find_bar.replace_input.focused {
                    self.replace_active_match(rasterizer, host);
                }
                return;
            }
            let clip = clipboard.get_text();
            if self.find_bar.find_input.focused {
                let (changed, copied) = self
                    .find_bar
                    .find_input
                    .handle_key(key, mods, text, clip.as_deref());
                if let Some(c) = copied {
                    clipboard.set_text(&c);
                }
                if changed {
                    self.refresh_search();
                    if let Some(idx) = self.active_match {
                        self.select_match(idx, rasterizer);
                    }
                }
            } else {
                let (_, copied) = self
                    .find_bar
                    .replace_input
                    .handle_key(key, mods, text, clip.as_deref());
                if let Some(c) = copied {
                    clipboard.set_text(&c);
                }
            }
            self.mark_dirty();
            host.request_redraw();
            return;
        }

        // Route to Sidebar if focused.
        if self.sidebar.is_focused() {
            if key == Key::Escape {
                self.sidebar.set_focused(false);
                self.mark_dirty();
                host.request_redraw();
                return;
            }
            match self
                .sidebar
                .handle_key(self.fs.as_ref(), key, self.layout.sidebar.h)
            {
                SidebarAction::OpenFile(p) => {
                    self.request_discard_or_run(PendingDiscardAction::OpenPath(p), host);
                }
                SidebarAction::Changed => {
                    self.mark_dirty();
                    host.request_redraw();
                }
                SidebarAction::None => {}
            }
            return;
        }

        // Document-scoped shortcuts (Undo/Redo, clipboard, comment, duplicate/delete/move line)
        // dispatched via the same unified `self.keymap`!
        if let Some(cmd) = self.keymap.resolve(KeyScope::Document, key, mods) {
            self.handle_command(cmd, rasterizer, clipboard, host);
            return;
        }

        // Neovim modal editing when Vim mode is enabled and document is focused.
        if self.vim.enabled {
            let vim_act = self.vim.handle_key(key, mods, text);
            if vim_act != crate::vim::VimAction::PassThrough {
                self.execute_vim_action(vim_act, rasterizer, clipboard, host);
                return;
            }
        }

        // Document navigation & text editing!
        self.handle_document_key(key, mods, text, rasterizer, host);
    }

    fn duplicate_current_line(
        &mut self,
        rasterizer: &mut dyn GlyphRasterizer,
        host: &mut dyn PlatformHost,
    ) {
        let (line, col) = self.doc.byte_to_line_col(self.caret);
        let Some((_, content_end, _)) = self.doc.line_byte_range(line) else {
            return;
        };
        let content = self.doc.line_content(line).unwrap_or_default();
        let insert = format!("\n{content}");
        if self.doc.edit(content_end, 0, &insert, self.now_ms, false).is_ok() {
            if let Some((next_start, next_end, _)) = self.doc.line_byte_range(line + 1) {
                let clamped_col = (col as u64).min(next_end - next_start);
                self.caret = next_start + clamped_col;
                self.anchor = self.caret;
            }
            self.after_document_mutated(rasterizer, host);
        }
    }

    fn delete_current_line(
        &mut self,
        rasterizer: &mut dyn GlyphRasterizer,
        host: &mut dyn PlatformHost,
    ) {
        let (line, _) = self.doc.byte_to_line_col(self.caret);
        let Some((start, _, full_end)) = self.doc.line_byte_range(line) else {
            return;
        };
        let (del_start, del_len) = if full_end > start {
            (start, full_end - start)
        } else if line > 0 {
            let (_, prev_content_end, prev_full_end) =
                self.doc.line_byte_range(line - 1).unwrap_or((0, 0, 0));
            (prev_content_end, prev_full_end - prev_content_end)
        } else {
            return;
        };
        if let Ok(pos) = self.doc.edit(del_start, del_len, "", self.now_ms, false) {
            self.caret = pos;
            self.anchor = pos;
            self.after_document_mutated(rasterizer, host);
        }
    }

    fn move_current_line(
        &mut self,
        up: bool,
        rasterizer: &mut dyn GlyphRasterizer,
        host: &mut dyn PlatformHost,
    ) {
        let (line, col) = self.doc.byte_to_line_col(self.caret);
        let total = self.doc.editable_line_count();
        let target_line = if up {
            if line == 0 {
                return;
            }
            line - 1
        } else {
            if line + 1 >= total {
                return;
            }
            line + 1
        };
        let (a_line, b_line) = if up { (target_line, line) } else { (line, target_line) };
        let Some((a_start, _, _)) = self.doc.line_byte_range(a_line) else { return };
        let Some((_, b_content_end, _)) = self.doc.line_byte_range(b_line) else { return };
        let a_text = self.doc.line_content(a_line).unwrap_or_default();
        let b_text = self.doc.line_content(b_line).unwrap_or_default();
        let swapped = format!("{b_text}\n{a_text}");
        if self
            .doc
            .edit(a_start, b_content_end - a_start, &swapped, self.now_ms, false)
            .is_ok()
        {
            if let Some((ns, ne, _)) = self.doc.line_byte_range(target_line) {
                self.caret = ns + (col as u64).min(ne - ns);
                self.anchor = self.caret;
            }
            self.after_document_mutated(rasterizer, host);
        }
    }

    fn toggle_line_comment(
        &mut self,
        rasterizer: &mut dyn GlyphRasterizer,
        host: &mut dyn PlatformHost,
    ) {
        let prefix = match self.doc.language() {
            Language::Python | Language::Shell | Language::Toml => "# ",
            _ => "// ",
        };
        let prefix_trim = prefix.trim();
        let (start_line, end_line) = self.selected_line_span();
        let Some((span_start, _, _)) = self.doc.line_byte_range(start_line) else { return };
        let Some((_, span_end, _)) = self.doc.line_byte_range(end_line) else { return };

        let mut lines = Vec::new();
        for l in start_line..=end_line {
            lines.push(self.doc.line_content(l).unwrap_or_default());
        }
        let all_commented = lines
            .iter()
            .filter(|l| !l.trim().is_empty())
            .all(|l| l.trim_start().starts_with(prefix_trim));

        let mut rebuilt = String::new();
        for (i, line) in lines.iter().enumerate() {
            if i > 0 {
                rebuilt.push('\n');
            }
            if all_commented {
                let indent_len = line.len() - line.trim_start().len();
                let rest = &line[indent_len..];
                if let Some(stripped) = rest.strip_prefix(prefix) {
                    rebuilt.push_str(&line[..indent_len]);
                    rebuilt.push_str(stripped);
                } else if let Some(stripped) = rest.strip_prefix(prefix_trim) {
                    rebuilt.push_str(&line[..indent_len]);
                    rebuilt.push_str(stripped);
                } else {
                    rebuilt.push_str(line);
                }
            } else {
                let indent_len = line.len() - line.trim_start().len();
                rebuilt.push_str(&line[..indent_len]);
                rebuilt.push_str(prefix);
                rebuilt.push_str(&line[indent_len..]);
            }
        }
        if let Ok(end_pos) = self
            .doc
            .edit(span_start, span_end - span_start, &rebuilt, self.now_ms, false)
        {
            if start_line == end_line {
                self.caret = end_pos;
                self.anchor = end_pos;
            } else {
                self.anchor = span_start;
                self.caret = end_pos;
            }
            self.after_document_mutated(rasterizer, host);
        }
    }

    fn selected_line_span(&mut self) -> (usize, usize) {
        if let Some((a, b)) = self.selection() {
            let (la, _) = self.doc.byte_to_line_col(a);
            let (mut lb, cb) = self.doc.byte_to_line_col(b);
            if lb > la && cb == 0 {
                lb -= 1;
            }
            (la, lb)
        } else {
            let (l, _) = self.doc.byte_to_line_col(self.caret);
            (l, l)
        }
    }

    fn indent_or_outdent_lines(
        &mut self,
        outdent: bool,
        rasterizer: &mut dyn GlyphRasterizer,
        host: &mut dyn PlatformHost,
    ) {
        let (start_line, end_line) = self.selected_line_span();
        let Some((span_start, _, _)) = self.doc.line_byte_range(start_line) else { return };
        let Some((_, span_end, _)) = self.doc.line_byte_range(end_line) else { return };
        let tab_spaces = " ".repeat(self.viewport.tab_width);

        let mut rebuilt = String::new();
        for l in start_line..=end_line {
            if l > start_line {
                rebuilt.push('\n');
            }
            let content = self.doc.line_content(l).unwrap_or_default();
            if outdent {
                if let Some(rest) = content.strip_prefix('\t') {
                    rebuilt.push_str(rest);
                } else {
                    let remove = content
                        .bytes()
                        .take(self.viewport.tab_width)
                        .take_while(|&b| b == b' ')
                        .count();
                    rebuilt.push_str(&content[remove..]);
                }
            } else {
                rebuilt.push_str(&tab_spaces);
                rebuilt.push_str(&content);
            }
        }
        if let Ok(end_pos) = self
            .doc
            .edit(span_start, span_end - span_start, &rebuilt, self.now_ms, false)
        {
            self.anchor = span_start;
            self.caret = end_pos;
            self.after_document_mutated(rasterizer, host);
        }
    }

    fn handle_document_key(
        &mut self,
        key: Key,
        mods: Modifiers,
        text: Option<&str>,
        rasterizer: &mut dyn GlyphRasterizer,
        host: &mut dyn PlatformHost,
    ) {
        let metrics = self.doc_metrics(rasterizer);
        let scale = self.framebuffer.scale;

        match key {
            Key::Escape => {
                if self.find_bar.visible {
                    self.find_bar.close();
                    self.search_matches.clear();
                    self.active_match = None;
                    self.recompute_layout();
                } else {
                    self.anchor = self.caret;
                }
                self.mark_dirty();
                host.request_redraw();
            }
            Key::Left => {
                self.doc.break_undo_coalescing();
                self.preferred_x_px = None;
                if !mods.shift && self.selection().is_some() {
                    let (a, _) = self.selection().unwrap();
                    self.caret = a;
                    self.anchor = a;
                } else if mods.primary() {
                    let (line, _) = self.doc.byte_to_line_col(self.caret);
                    let (start, _, _) = self.doc.line_byte_range(line).unwrap_or((0, 0, 0));
                    self.caret = start;
                    if !mods.shift {
                        self.anchor = self.caret;
                    }
                } else {
                    self.caret = if mods.alt {
                        self.move_word_left(self.caret)
                    } else {
                        self.move_grapheme_left(self.caret)
                    };
                    if !mods.shift {
                        self.anchor = self.caret;
                    }
                }
                self.viewport
                    .ensure_caret_visible(&mut self.doc, self.caret, metrics, scale);
                self.mark_dirty();
                host.request_redraw();
            }
            Key::Right => {
                self.doc.break_undo_coalescing();
                self.preferred_x_px = None;
                if !mods.shift && self.selection().is_some() {
                    let (_, b) = self.selection().unwrap();
                    self.caret = b;
                    self.anchor = b;
                } else if mods.primary() {
                    let (line, _) = self.doc.byte_to_line_col(self.caret);
                    let (_, end, _) = self.doc.line_byte_range(line).unwrap_or((0, 0, 0));
                    self.caret = end;
                    if !mods.shift {
                        self.anchor = self.caret;
                    }
                } else {
                    self.caret = if mods.alt {
                        self.move_word_right(self.caret)
                    } else {
                        self.move_grapheme_right(self.caret)
                    };
                    if !mods.shift {
                        self.anchor = self.caret;
                    }
                }
                self.viewport
                    .ensure_caret_visible(&mut self.doc, self.caret, metrics, scale);
                self.mark_dirty();
                host.request_redraw();
            }
            Key::Up => {
                self.doc.break_undo_coalescing();
                if mods.primary() {
                    self.caret = 0;
                    self.preferred_x_px = None;
                } else {
                    self.move_vertical(-1, metrics);
                }
                if !mods.shift {
                    self.anchor = self.caret;
                }
                self.viewport
                    .ensure_caret_visible(&mut self.doc, self.caret, metrics, scale);
                self.mark_dirty();
                host.request_redraw();
            }
            Key::Down => {
                self.doc.break_undo_coalescing();
                if mods.primary() {
                    self.doc.tree_mut().ensure_fully_indexed();
                    self.caret = self.doc.len_bytes();
                    self.preferred_x_px = None;
                } else {
                    self.move_vertical(1, metrics);
                }
                if !mods.shift {
                    self.anchor = self.caret;
                }
                self.viewport
                    .ensure_caret_visible(&mut self.doc, self.caret, metrics, scale);
                self.mark_dirty();
                host.request_redraw();
            }
            Key::Home => {
                self.doc.break_undo_coalescing();
                self.preferred_x_px = None;
                if mods.primary() {
                    self.caret = 0;
                } else {
                    let (line, _) = self.doc.byte_to_line_col(self.caret);
                    let (start, _, _) = self.doc.line_byte_range(line).unwrap_or((0, 0, 0));
                    self.caret = start;
                }
                if !mods.shift {
                    self.anchor = self.caret;
                }
                self.viewport
                    .ensure_caret_visible(&mut self.doc, self.caret, metrics, scale);
                self.mark_dirty();
                host.request_redraw();
            }
            Key::End => {
                self.doc.break_undo_coalescing();
                self.preferred_x_px = None;
                if mods.primary() {
                    self.doc.tree_mut().ensure_fully_indexed();
                    self.caret = self.doc.len_bytes();
                } else {
                    let (line, _) = self.doc.byte_to_line_col(self.caret);
                    let (_, end, _) = self.doc.line_byte_range(line).unwrap_or((0, 0, 0));
                    self.caret = end;
                }
                if !mods.shift {
                    self.anchor = self.caret;
                }
                self.viewport
                    .ensure_caret_visible(&mut self.doc, self.caret, metrics, scale);
                self.mark_dirty();
                host.request_redraw();
            }
            Key::PageUp => {
                self.doc.break_undo_coalescing();
                let page = (self.layout.viewport.h / metrics.line_height.max(1) as i32).max(1) as isize;
                self.move_vertical(-page, metrics);
                if !mods.shift {
                    self.anchor = self.caret;
                }
                self.viewport
                    .ensure_caret_visible(&mut self.doc, self.caret, metrics, scale);
                self.mark_dirty();
                host.request_redraw();
            }
            Key::PageDown => {
                self.doc.break_undo_coalescing();
                let page = (self.layout.viewport.h / metrics.line_height.max(1) as i32).max(1) as isize;
                self.move_vertical(page, metrics);
                if !mods.shift {
                    self.anchor = self.caret;
                }
                self.viewport
                    .ensure_caret_visible(&mut self.doc, self.caret, metrics, scale);
                self.mark_dirty();
                host.request_redraw();
            }
            Key::Backspace => {
                self.preferred_x_px = None;
                let (del_start, del_len, coalescable) = if let Some((a, b)) = self.selection() {
                    (a, b - a, false)
                } else if self.caret > 0 {
                    let prev = if mods.primary() {
                        let (line, _) = self.doc.byte_to_line_col(self.caret);
                        self.doc.line_byte_range(line).map(|(s, _, _)| s).unwrap_or(0)
                    } else if mods.alt {
                        self.move_word_left(self.caret)
                    } else {
                        self.move_grapheme_left(self.caret)
                    };
                    (prev, self.caret - prev, !mods.alt && !mods.primary())
                } else {
                    return;
                };
                if let Ok(pos) = self.doc.edit(del_start, del_len, "", self.now_ms, coalescable) {
                    self.caret = pos;
                    self.anchor = pos;
                    self.after_document_mutated(rasterizer, host);
                }
            }
            Key::Delete => {
                self.preferred_x_px = None;
                let (del_start, del_len) = if let Some((a, b)) = self.selection() {
                    (a, b - a)
                } else if self.caret < self.doc.len_bytes() {
                    let next = if mods.alt {
                        self.move_word_right(self.caret)
                    } else {
                        self.move_grapheme_right(self.caret)
                    };
                    (self.caret, next - self.caret)
                } else {
                    return;
                };
                if let Ok(pos) = self.doc.edit(del_start, del_len, "", self.now_ms, false) {
                    self.caret = pos;
                    self.anchor = pos;
                    self.after_document_mutated(rasterizer, host);
                }
            }
            Key::Enter => {
                self.preferred_x_px = None;
                let (line, col) = self.doc.byte_to_line_col(self.caret);
                let indent: String = self
                    .doc
                    .line_content(line)
                    .map(|l| {
                        l[..col.min(l.len())]
                            .chars()
                            .take_while(|&c| c == ' ' || c == '\t')
                            .collect()
                    })
                    .unwrap_or_default();
                let insert_str = format!("\n{indent}");
                let (start, removed) = self
                    .selection()
                    .map(|(a, b)| (a, b - a))
                    .unwrap_or((self.caret, 0));
                if let Ok(pos) = self.doc.edit(start, removed, &insert_str, self.now_ms, false) {
                    self.caret = pos;
                    self.anchor = pos;
                    self.after_document_mutated(rasterizer, host);
                }
            }
            Key::Tab => {
                self.preferred_x_px = None;
                let (la, lb) = self.selected_line_span();
                if mods.shift || la != lb {
                    self.indent_or_outdent_lines(mods.shift, rasterizer, host);
                    return;
                }
                let spaces = " ".repeat(self.viewport.tab_width);
                let (start, removed) = self
                    .selection()
                    .map(|(a, b)| (a, b - a))
                    .unwrap_or((self.caret, 0));
                if let Ok(pos) = self.doc.edit(start, removed, &spaces, self.now_ms, false) {
                    self.caret = pos;
                    self.anchor = pos;
                    self.after_document_mutated(rasterizer, host);
                }
            }
            _ => {
                if !mods.primary() && !mods.ctrl {
                    if let Some(s) = text {
                        if !s.is_empty() && !s.chars().all(|c| c.is_control()) {
                            self.preferred_x_px = None;
                            let (start, removed) = self
                                .selection()
                                .map(|(a, b)| (a, b - a))
                                .unwrap_or((self.caret, 0));
                            let coalescable = removed == 0 && s.len() <= 8;
                            if let Ok(pos) =
                                self.doc.edit(start, removed, s, self.now_ms, coalescable)
                            {
                                self.caret = pos;
                                self.anchor = pos;
                                self.after_document_mutated(rasterizer, host);
                            }
                        }
                    }
                }
            }
        }
    }

    fn after_document_mutated(
        &mut self,
        rasterizer: &mut dyn GlyphRasterizer,
        host: &mut dyn PlatformHost,
    ) {
        let metrics = self.doc_metrics(rasterizer);
        self.viewport
            .ensure_caret_visible(&mut self.doc, self.caret, metrics, self.framebuffer.scale);
        if self.find_bar.visible {
            self.refresh_search();
        }
        self.sync_title(host);
        self.mark_dirty();
        host.request_redraw();
    }

    fn move_grapheme_left(&mut self, offset: u64) -> u64 {
        if offset == 0 {
            return 0;
        }
        let (line, col) = self.doc.byte_to_line_col(offset);
        if col > 0 {
            let content = self.doc.line_content(line).unwrap_or_default();
            let prev = prev_grapheme_boundary(&content, col);
            let (start, _, _) = self.doc.line_byte_range(line).unwrap_or((0, 0, 0));
            start + prev as u64
        } else if line > 0 {
            let (_, prev_content_end, _) = self.doc.line_byte_range(line - 1).unwrap_or((0, 0, 0));
            prev_content_end
        } else {
            0
        }
    }

    fn move_grapheme_right(&mut self, offset: u64) -> u64 {
        let total = self.doc.len_bytes();
        if offset >= total {
            return total;
        }
        let (line, col) = self.doc.byte_to_line_col(offset);
        let (start, content_end, full_end) = self.doc.line_byte_range(line).unwrap_or((0, 0, 0));
        let content_len = (content_end - start) as usize;
        if col < content_len {
            let content = self.doc.line_content(line).unwrap_or_default();
            let next = next_grapheme_boundary(&content, col);
            start + next as u64
        } else {
            full_end
        }
    }

    fn move_word_left(&mut self, offset: u64) -> u64 {
        if offset == 0 {
            return 0;
        }
        let (line, col) = self.doc.byte_to_line_col(offset);
        if col > 0 {
            let content = self.doc.line_content(line).unwrap_or_default();
            let prev = prev_word_boundary(&content, col);
            let (start, _, _) = self.doc.line_byte_range(line).unwrap_or((0, 0, 0));
            start + prev as u64
        } else if line > 0 {
            let (_, prev_content_end, _) = self.doc.line_byte_range(line - 1).unwrap_or((0, 0, 0));
            prev_content_end
        } else {
            0
        }
    }

    fn move_word_right(&mut self, offset: u64) -> u64 {
        let total = self.doc.len_bytes();
        if offset >= total {
            return total;
        }
        let (line, col) = self.doc.byte_to_line_col(offset);
        let (start, content_end, full_end) = self.doc.line_byte_range(line).unwrap_or((0, 0, 0));
        let content_len = (content_end - start) as usize;
        if col < content_len {
            let content = self.doc.line_content(line).unwrap_or_default();
            let next = next_word_boundary(&content, col);
            start + next as u64
        } else {
            full_end
        }
    }

    fn move_vertical(&mut self, delta_lines: isize, metrics: editor_platform::FontMetrics) {
        let (line, col) = self.doc.byte_to_line_col(self.caret);
        let target_x = match self.preferred_x_px {
            Some(x) => x,
            None => {
                let text = self.doc.line_content(line).unwrap_or_default();
                let x = self.viewport.measure_prefix_px(&text, col, metrics.cell_width);
                self.preferred_x_px = Some(x);
                x
            }
        };

        let target_line = if delta_lines < 0 {
            line.saturating_sub((-delta_lines) as usize)
        } else {
            let cand = line.saturating_add(delta_lines as usize);
            self.doc.tree_mut().ensure_indexed_for_line(cand);
            cand.min(self.doc.editable_line_count().saturating_sub(1))
        };

        let content = self.doc.line_content(target_line).unwrap_or_default();
        let (start, _, _) = self.doc.line_byte_range(target_line).unwrap_or((0, 0, 0));
        let mut best_col = content.len();
        let mut cur_cells = 0usize;
        for (off, cluster) in editor_core::grapheme_clusters(&content) {
            let cw = if cluster == "\t" {
                self.viewport.tab_width - (cur_cells % self.viewport.tab_width)
            } else {
                editor_core::cluster_cell_width(cluster) as usize
            };
            let x0 = cur_cells as f32 * metrics.cell_width;
            let mid = x0 + (cw as f32 * metrics.cell_width) * 0.5;
            if target_x < mid {
                best_col = off;
                break;
            }
            cur_cells += cw;
        }
        self.caret = start + best_col as u64;
    }

    fn handle_mouse_down(
        &mut self,
        x_px: f32,
        y_px: f32,
        click_count: u8,
        mods: Modifiers,
        rasterizer: &mut dyn GlyphRasterizer,
        host: &mut dyn PlatformHost,
    ) {
        let scale = self.framebuffer.scale;
        let xi = x_px.round() as i32;
        let yi = y_px.round() as i32;

        // 1. Sidebar resize edge or click.
        if self.sidebar.is_visible() {
            let sb_hit = self.sidebar.hit_test(self.layout.sidebar, x_px, y_px, scale);
            if sb_hit == SidebarHit::ResizeEdge {
                self.dragging_sidebar_edge = true;
                host.set_cursor(CursorShape::ResizeHorizontal);
                return;
            }
            if sb_hit != SidebarHit::None {
                self.sidebar.set_focused(true);
                self.find_bar.find_input.focused = false;
                self.find_bar.replace_input.focused = false;
                match self
                    .sidebar
                    .handle_click(self.fs.as_ref(), self.layout.sidebar, x_px, y_px, scale)
                {
                    SidebarAction::OpenFile(p) => {
                        self.request_discard_or_run(PendingDiscardAction::OpenPath(p), host);
                    }
                    SidebarAction::Changed => {
                        self.mark_dirty();
                        host.request_redraw();
                    }
                    SidebarAction::None => {}
                }
                return;
            }
        }

        // 2. Find / Replace bar click (including mouse caret placement inside inputs!).
        if self.find_bar.visible && self.layout.find_bar.contains(xi, yi) {
            self.sidebar.set_focused(false);
            match self.find_bar.hit_test(x_px, y_px) {
                FindBarAction::FocusFind => {
                    self.find_bar.find_input.focused = true;
                    self.find_bar.replace_input.focused = false;
                    let rect = self.find_bar.layout.find_input;
                    self.find_bar.find_input.mouse_down(
                        rect,
                        x_px,
                        scale,
                        mods.shift,
                        &mut self.atlas,
                        rasterizer,
                    );
                    self.dragging_find_input = true;
                }
                FindBarAction::FocusReplace => {
                    self.find_bar.find_input.focused = false;
                    self.find_bar.replace_input.focused = true;
                    let rect = self.find_bar.layout.replace_input;
                    self.find_bar.replace_input.mouse_down(
                        rect,
                        x_px,
                        scale,
                        mods.shift,
                        &mut self.atlas,
                        rasterizer,
                    );
                    self.dragging_replace_input = true;
                }
                FindBarAction::ToggleCase => {
                    self.find_bar.case_sensitive = !self.find_bar.case_sensitive;
                    self.refresh_search();
                }
                FindBarAction::FindNext => {
                    self.jump_search_match(true, rasterizer, host);
                    return;
                }
                FindBarAction::FindPrev => {
                    self.jump_search_match(false, rasterizer, host);
                    return;
                }
                FindBarAction::ReplaceOne => {
                    self.replace_active_match(rasterizer, host);
                    return;
                }
                FindBarAction::ReplaceAll => {
                    self.replace_all_matches(rasterizer, host);
                    return;
                }
                FindBarAction::Close => {
                    self.find_bar.close();
                    self.search_matches.clear();
                    self.active_match = None;
                    self.recompute_layout();
                }
                FindBarAction::None => {}
            }
            self.mark_dirty();
            host.request_redraw();
            return;
        }

        // 3. Split pane divider drag, pane header close (`×`) / double-click swap, or focus switch!
        if self.secondary_pane.is_some() {
            let grab = (5.0 * scale).round() as i32;
            if (xi - self.layout.split_divider.x).abs() <= grab
                && yi >= self.layout.split_area.y
                && yi < self.layout.split_area.bottom()
            {
                self.dragging_split_edge = true;
                host.set_cursor(CursorShape::ResizeHorizontal);
                return;
            }
            let header_h = (24.0 * scale).round() as i32;
            let close_w = (24.0 * scale).round() as i32;
            let left_hdr = PixelRect::new(
                self.layout.left_pane.x,
                self.layout.split_area.y,
                self.layout.left_pane.w,
                header_h,
            );
            let right_hdr = PixelRect::new(
                self.layout.right_pane.x,
                self.layout.split_area.y,
                self.layout.right_pane.w,
                header_h,
            );
            let left_close = PixelRect::new(left_hdr.right() - close_w, left_hdr.y, close_w, header_h);
            let right_close =
                PixelRect::new(right_hdr.right() - close_w, right_hdr.y, close_w, header_h);
            if left_close.contains(xi, yi) {
                self.focus_pane(0, host);
                self.request_close_active_pane(host);
                return;
            }
            if right_close.contains(xi, yi) {
                self.focus_pane(1, host);
                self.request_close_active_pane(host);
                return;
            }
            // Double-clicking either pane header swaps left ↔ right panes!
            if click_count >= 2 && (left_hdr.contains(xi, yi) || right_hdr.contains(xi, yi)) {
                self.swap_panes(host);
                return;
            }
            let clicked_left = xi >= self.layout.left_pane.x
                && xi < self.layout.split_divider.x
                && yi >= self.layout.split_area.y
                && yi < self.layout.split_area.bottom();
            let clicked_right = xi >= self.layout.split_divider.right()
                && xi < self.layout.split_area.right()
                && yi >= self.layout.split_area.y
                && yi < self.layout.split_area.bottom();
            if clicked_left && self.active_pane != 0 {
                self.focus_pane(0, host);
            } else if clicked_right && self.active_pane != 1 {
                self.focus_pane(1, host);
            }
        }

        // 4. Document viewport (scrollbars, gutter whole-line select, then text hit-test).
        if self.layout.viewport.contains(xi, yi) {
            self.sidebar.set_focused(false);
            self.find_bar.find_input.focused = false;
            self.find_bar.replace_input.focused = false;
            self.goto_line.close();
            self.doc.break_undo_coalescing();

            let metrics = self.doc_metrics(rasterizer);
            let sb_hit = self.viewport.hit_test_scrollbar(x_px, y_px);
            match sb_hit {
                ScrollbarHit::VerticalThumb | ScrollbarHit::HorizontalThumb => {
                    self.viewport.begin_scrollbar_drag(sb_hit, x_px, y_px, metrics);
                    self.mark_dirty();
                    host.request_redraw();
                    return;
                }
                ScrollbarHit::VerticalTrack => {
                    let page = self.viewport.layout.text.h as f32 * 0.85;
                    let dir = if yi < self.viewport.layout.v_thumb.y {
                        -page
                    } else {
                        page
                    };
                    self.viewport
                        .scroll_by(0.0, dir, &mut self.doc, metrics, scale);
                    self.mark_dirty();
                    host.request_redraw();
                    return;
                }
                ScrollbarHit::HorizontalTrack => {
                    let page = self.viewport.layout.text.w as f32 * 0.85;
                    let dir = if xi < self.viewport.layout.h_thumb.x {
                        -page
                    } else {
                        page
                    };
                    self.viewport
                        .scroll_by(dir, 0.0, &mut self.doc, metrics, scale);
                    self.mark_dirty();
                    host.request_redraw();
                    return;
                }
                ScrollbarHit::None => {}
            }

            // Gutter click selects the whole logical line!
            if self.viewport.layout.gutter.contains(xi, yi) {
                let line = self.viewport.hit_test_gutter(y_px, &mut self.doc, metrics, scale);
                let (ls, _, l_full) = self.doc.line_byte_range(line).unwrap_or((0, 0, 0));
                self.anchor = ls;
                self.caret = l_full;
                self.dragging_gutter_anchor_line = Some(line);
                self.mark_dirty();
                host.request_redraw();
                return;
            }

            let hit = self
                .viewport
                .hit_test_text(x_px, y_px, &mut self.doc, metrics, scale);
            self.preferred_x_px = None;
            self.dragging_selection = true;
            self.drag_click_count = click_count.clamp(1, 3);

            match self.drag_click_count {
                1 => {
                    self.caret = hit.byte_offset;
                    if !mods.shift {
                        self.anchor = hit.byte_offset;
                    }
                }
                2 => {
                    let content = self.doc.line_content(hit.line).unwrap_or_default();
                    let (ws, we) = word_range_at(&content, hit.col_bytes);
                    let (ls, _, _) = self.doc.line_byte_range(hit.line).unwrap_or((0, 0, 0));
                    self.anchor = ls + ws as u64;
                    self.caret = ls + we as u64;
                    self.drag_word_anchor = (self.anchor, self.caret);
                }
                _ => {
                    let (ls, _, l_full) = self.doc.line_byte_range(hit.line).unwrap_or((0, 0, 0));
                    self.anchor = ls;
                    self.caret = l_full;
                    self.drag_word_anchor = (ls, l_full);
                }
            }
            self.mark_dirty();
            host.request_redraw();
        }
    }

    fn handle_mouse_move(
        &mut self,
        x_px: f32,
        y_px: f32,
        rasterizer: &mut dyn GlyphRasterizer,
        host: &mut dyn PlatformHost,
    ) {
        let scale = self.framebuffer.scale;
        if self.dragging_sidebar_edge {
            self.sidebar.set_width_pt(x_px / scale);
            self.recompute_layout();
            self.mark_dirty();
            host.request_redraw();
            return;
        }
        if self.dragging_split_edge && self.layout.split_area.w > 0 {
            let rel = (x_px - self.layout.split_area.x as f32) / (self.layout.split_area.w as f32);
            self.split_ratio = rel.clamp(0.2, 0.8);
            self.recompute_layout();
            self.mark_dirty();
            host.request_redraw();
            return;
        }
        if self.dragging_find_input {
            let rect = self.find_bar.layout.find_input;
            self.find_bar
                .find_input
                .mouse_drag(rect, x_px, scale, &mut self.atlas, rasterizer);
            self.mark_dirty_rect(self.layout.find_bar);
            host.request_redraw();
            return;
        }
        if self.dragging_replace_input {
            let rect = self.find_bar.layout.replace_input;
            self.find_bar
                .replace_input
                .mouse_drag(rect, x_px, scale, &mut self.atlas, rasterizer);
            self.mark_dirty_rect(self.layout.find_bar);
            host.request_redraw();
            return;
        }
        if let Some(anchor_line) = self.dragging_gutter_anchor_line {
            let metrics = self.doc_metrics(rasterizer);
            let cur_line = self
                .viewport
                .hit_test_gutter(y_px, &mut self.doc, metrics, scale);
            let (low, high) = if cur_line < anchor_line {
                (cur_line, anchor_line)
            } else {
                (anchor_line, cur_line)
            };
            let (ls, _, _) = self.doc.line_byte_range(low).unwrap_or((0, 0, 0));
            let (_, _, he) = self.doc.line_byte_range(high).unwrap_or((0, 0, 0));
            if cur_line < anchor_line {
                self.anchor = he;
                self.caret = ls;
            } else {
                self.anchor = ls;
                self.caret = he;
            }
            self.mark_dirty();
            host.request_redraw();
            return;
        }
        if self.viewport.v_bar_dragging || self.viewport.h_bar_dragging {
            let metrics = self.doc_metrics(rasterizer);
            if self
                .viewport
                .drag_scrollbar(x_px, y_px, &mut self.doc, metrics, scale)
            {
                self.mark_dirty();
                host.request_redraw();
            }
            return;
        }
        if self.dragging_selection {
            let metrics = self.doc_metrics(rasterizer);
            let hit = self
                .viewport
                .hit_test_text(x_px, y_px, &mut self.doc, metrics, scale);
            match self.drag_click_count {
                1 => self.caret = hit.byte_offset,
                2 => {
                    let content = self.doc.line_content(hit.line).unwrap_or_default();
                    let (ws, we) = word_range_at(&content, hit.col_bytes);
                    let (ls, _, _) = self.doc.line_byte_range(hit.line).unwrap_or((0, 0, 0));
                    let (a0, b0) = self.drag_word_anchor;
                    let (cur_s, cur_e) = (ls + ws as u64, ls + we as u64);
                    if cur_s < a0 {
                        self.anchor = b0;
                        self.caret = cur_s;
                    } else {
                        self.anchor = a0;
                        self.caret = cur_e.max(b0);
                    }
                }
                _ => {
                    let (ls, _, l_full) = self.doc.line_byte_range(hit.line).unwrap_or((0, 0, 0));
                    let (a0, b0) = self.drag_word_anchor;
                    if ls < a0 {
                        self.anchor = b0;
                        self.caret = ls;
                    } else {
                        self.anchor = a0;
                        self.caret = l_full.max(b0);
                    }
                }
            }
            self.mark_dirty();
            host.request_redraw();
            return;
        }

        // Hover cursor & scrollbar hover highlight.
        let xi = x_px.round() as i32;
        let yi = y_px.round() as i32;
        if self.sidebar.is_visible() {
            let hit = self
                .sidebar
                .set_hovered(self.layout.sidebar, x_px, y_px, scale);
            if hit == SidebarHit::ResizeEdge {
                host.set_cursor(CursorShape::ResizeHorizontal);
                return;
            }
            if hit != SidebarHit::None {
                host.set_cursor(CursorShape::Pointer);
                self.mark_dirty_rect(self.layout.sidebar);
                host.request_redraw();
                return;
            }
        }
        if self.secondary_pane.is_some() {
            let grab = (5.0 * scale).round() as i32;
            if (xi - self.layout.split_divider.x).abs() <= grab
                && yi >= self.layout.split_area.y
                && yi < self.layout.split_area.bottom()
            {
                host.set_cursor(CursorShape::ResizeHorizontal);
                return;
            }
        }
        let sb = self.viewport.hit_test_scrollbar(x_px, y_px);
        let prev_vh = self.viewport.v_bar_hovered;
        let prev_hh = self.viewport.h_bar_hovered;
        self.viewport.v_bar_hovered =
            matches!(sb, ScrollbarHit::VerticalThumb | ScrollbarHit::VerticalTrack);
        self.viewport.h_bar_hovered = matches!(
            sb,
            ScrollbarHit::HorizontalThumb | ScrollbarHit::HorizontalTrack
        );
        if prev_vh != self.viewport.v_bar_hovered || prev_hh != self.viewport.h_bar_hovered {
            self.mark_dirty();
            host.request_redraw();
        }
        if sb != ScrollbarHit::None {
            host.set_cursor(CursorShape::Arrow);
        } else if self.viewport.layout.text.contains(xi, yi)
            || self
                .secondary_pane
                .as_ref()
                .is_some_and(|p| p.viewport.layout.text.contains(xi, yi))
        {
            host.set_cursor(CursorShape::IBeam);
        } else {
            host.set_cursor(CursorShape::Arrow);
        }
    }

    fn handle_scroll(
        &mut self,
        dx_px: f32,
        dy_px: f32,
        x_px: f32,
        y_px: f32,
        rasterizer: &mut dyn GlyphRasterizer,
        host: &mut dyn PlatformHost,
    ) {
        let xi = x_px.round() as i32;
        let yi = y_px.round() as i32;
        if self.sidebar.is_visible() && self.layout.sidebar.contains(xi, yi) {
            self.sidebar.scroll_by(dy_px, self.layout.sidebar.h);
            self.mark_dirty_rect(self.layout.sidebar);
            host.request_redraw();
            return;
        }
        let metrics = self.doc_metrics(rasterizer);
        if self.secondary_pane.is_some() {
            let inactive_rect = if self.active_pane == 0 {
                self.layout.right_pane
            } else {
                self.layout.left_pane
            };
            let over_inactive_col = xi >= inactive_rect.x
                && xi < inactive_rect.right()
                && yi >= self.layout.split_area.y
                && yi < self.layout.split_area.bottom();
            if over_inactive_col {
                if let Some(ref mut sec) = self.secondary_pane {
                    sec.viewport
                        .scroll_by(dx_px, dy_px, &mut sec.doc, metrics, self.framebuffer.scale);
                    self.mark_dirty_rect(inactive_rect);
                    host.request_redraw();
                    return;
                }
            }
        }
        self.viewport
            .scroll_by(dx_px, dy_px, &mut self.doc, metrics, self.framebuffer.scale);
        self.mark_dirty_rect(self.layout.viewport);
        host.request_redraw();
    }

    // ========================================================================
    // Find / Replace Helpers
    // ========================================================================

    pub fn refresh_search(&mut self) {
        let query = self.find_bar.find_input.text.clone();
        if query.is_empty() {
            self.search_matches.clear();
            self.active_match = None;
            self.find_bar.match_count = 0;
            self.find_bar.active_match = None;
            return;
        }
        let opts = SearchOptions {
            case_sensitive: self.find_bar.case_sensitive,
            max_matches: 10_000,
        };
        self.search_matches = self.doc.find_all(&query, opts);
        self.find_bar.match_count = self.search_matches.len();
        self.active_match = if self.search_matches.is_empty() {
            None
        } else {
            self.search_matches
                .iter()
                .position(|m| m.start >= self.caret)
                .or(Some(0))
        };
        self.find_bar.active_match = self.active_match;
    }

    fn select_match(&mut self, idx: usize, rasterizer: &mut dyn GlyphRasterizer) {
        if let Some(m) = self.search_matches.get(idx).copied() {
            self.active_match = Some(idx);
            self.find_bar.active_match = Some(idx);
            self.anchor = m.start;
            self.caret = m.end;
            let metrics = self.doc_metrics(rasterizer);
            self.viewport
                .ensure_caret_visible(&mut self.doc, self.caret, metrics, self.framebuffer.scale);
        }
    }

    fn jump_search_match(
        &mut self,
        forward: bool,
        rasterizer: &mut dyn GlyphRasterizer,
        host: &mut dyn PlatformHost,
    ) {
        if self.search_matches.is_empty() {
            self.refresh_search();
        }
        let len = self.search_matches.len();
        if len == 0 {
            return;
        }
        let next_idx = match self.active_match {
            Some(i) if forward => (i + 1) % len,
            Some(i) => (i + len - 1) % len,
            None => 0,
        };
        self.select_match(next_idx, rasterizer);
        self.mark_dirty();
        host.request_redraw();
    }

    fn replace_active_match(
        &mut self,
        rasterizer: &mut dyn GlyphRasterizer,
        host: &mut dyn PlatformHost,
    ) {
        let Some(idx) = self.active_match else {
            return;
        };
        let Some(m) = self.search_matches.get(idx).copied() else {
            return;
        };
        let replacement = self.find_bar.replace_input.text.clone();
        if let Ok(pos) = self
            .doc
            .edit(m.start, m.end - m.start, &replacement, self.now_ms, false)
        {
            self.caret = pos;
            self.anchor = pos;
            self.after_document_mutated(rasterizer, host);
            if let Some(next_idx) = self.active_match {
                self.select_match(next_idx, rasterizer);
            }
        }
    }

    fn replace_all_matches(
        &mut self,
        rasterizer: &mut dyn GlyphRasterizer,
        host: &mut dyn PlatformHost,
    ) {
        let query = self.find_bar.find_input.text.clone();
        if query.is_empty() {
            return;
        }
        let replacement = self.find_bar.replace_input.text.clone();
        let opts = SearchOptions {
            case_sensitive: self.find_bar.case_sensitive,
            max_matches: 100_000,
        };
        let count = self.doc.replace_all(&query, &replacement, opts, self.now_ms);
        self.caret = self.caret.min(self.doc.len_bytes());
        self.anchor = self.caret;
        self.after_document_mutated(rasterizer, host);
        self.set_notice(format!("Replaced {count} occurrence(s)"), false);
    }

    // ========================================================================
    // Full Live GUI Inspection & Control over IPC (Portable across all OSes!)
    // ========================================================================

    /// Emit a complete, deterministic JSON snapshot of the live editor state so
    /// the CLI and AI agents can inspect everything open and visible in the GUI.
    pub fn status_json(&mut self) -> String {
        let (line0, col) = self.doc.byte_to_line_col(self.caret);
        let sel = self.selection();
        let sel_text = self.selected_text();
        let line_count = self.doc.exact_line_count();
        let path_json = match self.doc.path() {
            Some(p) => json_quote(&p.to_string_lossy()),
            None => "null".to_string(),
        };
        let sb_root_json = match self.sidebar.root() {
            Some(p) => json_quote(&p.to_string_lossy()),
            None => "null".to_string(),
        };
        let sb_sel_json = match self.sidebar.selected() {
            Some(p) => json_quote(&p.to_string_lossy()),
            None => "null".to_string(),
        };
        let sel_json = match (sel, sel_text) {
            (Some((a, b)), Some(txt)) => {
                let preview: String = txt.chars().take(4096).collect();
                format!(
                    "{{\"start\":{a},\"end\":{b},\"bytes\":{},\"text\":{}}}",
                    b - a,
                    json_quote(&preview)
                )
            }
            _ => "null".to_string(),
        };
        let active_match_json = match self.active_match {
            Some(i) => i.to_string(),
            None => "null".to_string(),
        };
        let palette_mode = match self.palette.mode {
            editor_render::PaletteMode::Closed => "closed",
            editor_render::PaletteMode::QuickOpen => "quick_open",
            editor_render::PaletteMode::ProjectSearch => "project_search",
        };
        let theme_pref = match self.theme_pref {
            ThemePreference::System => "system",
            ThemePreference::Light => "light",
            ThemePreference::Dark => "dark",
        };
        let active_theme = if self.active_theme().is_dark {
            "dark"
        } else {
            "light"
        };
        let rev = self.doc.rev();

        let panes_json = if let Some(ref sec) = self.secondary_pane {
            let active_obj = format!(
                "{{\"path\":{},\"file_name\":{},\"dirty\":{}}}",
                path_json,
                json_quote(self.doc.file_name()),
                self.doc.is_dirty()
            );
            let sec_path = match sec.doc.path() {
                Some(p) => json_quote(&p.to_string_lossy()),
                None => "null".to_string(),
            };
            let sec_obj = format!(
                "{{\"path\":{},\"file_name\":{},\"dirty\":{}}}",
                sec_path,
                json_quote(sec.doc.file_name()),
                sec.doc.is_dirty()
            );
            if self.active_pane == 0 {
                format!("[{active_obj},{sec_obj}]")
            } else {
                format!("[{sec_obj},{active_obj}]")
            }
        } else {
            format!(
                "[{{\"path\":{},\"file_name\":{},\"dirty\":{}}}]",
                path_json,
                json_quote(self.doc.file_name()),
                self.doc.is_dirty()
            )
        };

        format!(
            concat!(
                "{{\"running\":true,\"path\":{},\"file_name\":{},\"dirty\":{},",
                "\"split\":{},\"active_pane\":{},\"panes\":{},",
                "\"bytes\":{},\"line_count\":{},\"eol\":{},\"language\":{},",
                "\"rev\":{{\"dev\":\"{}\",\"ino\":\"{}\",\"size\":\"{}\",\"mtime_ns\":\"{}\",\"ctime_ns\":\"{}\"}},",
                "\"can_undo\":{},\"can_redo\":{},",
                "\"cursor\":{{\"line\":{},\"col\":{},\"byte_offset\":{}}},",
                "\"selection\":{},",
                "\"scroll\":{{\"top_line\":{},\"scroll_x_px\":{:.1}}},",
                "\"sidebar\":{{\"visible\":{},\"root\":{},\"width_pt\":{:.0},\"selected\":{}}},",
                "\"find\":{{\"visible\":{},\"show_replace\":{},\"query\":{},\"replace\":{},\"case_sensitive\":{},\"matches\":{},\"active_match\":{}}},",
                "\"palette\":{{\"mode\":{},\"query\":{},\"items\":{}}},",
                "\"shortcuts_open\":{},",
                "\"vim\":{{\"enabled\":{},\"mode\":{}}},",
                "\"confirm_modal_visible\":{},",
                "\"view\":{{\"word_wrap\":{},\"default_word_wrap\":{},\"word_wrap_by_ext\":{},\"font_size_pt\":{:.1},\"theme_preference\":{},\"active_theme\":{}}}}}"
            ),
            path_json,
            json_quote(self.doc.file_name()),
            self.doc.is_dirty(),
            self.secondary_pane.is_some(),
            self.active_pane,
            panes_json,
            self.doc.len_bytes(),
            line_count,
            json_quote(self.doc.eol().as_str()),
            json_quote(self.doc.language().display_name()),
            rev.dev,
            rev.ino,
            rev.size,
            rev.mtime_ns,
            rev.ctime_ns,
            self.doc.can_undo(),
            self.doc.can_redo(),
            line0 + 1,
            col,
            self.caret,
            sel_json,
            self.viewport.scroll_top_line + 1,
            self.viewport.scroll_x_px,
            self.sidebar.is_visible(),
            sb_root_json,
            self.sidebar.width_pt(),
            sb_sel_json,
            self.find_bar.visible,
            self.find_bar.show_replace,
            json_quote(&self.find_bar.find_input.text),
            json_quote(&self.find_bar.replace_input.text),
            self.find_bar.case_sensitive,
            self.search_matches.len(),
            active_match_json,
            json_quote(palette_mode),
            json_quote(&self.palette.input.text),
            self.palette.items.len(),
            self.shortcuts.visible,
            self.vim.enabled,
            json_quote(self.vim.mode.label()),
            self.confirm_modal.visible,
            self.viewport.word_wrap,
            self.default_word_wrap,
            wrap_by_ext_json(&self.wrap_by_ext),
            self.viewport.font_size_pt,
            json_quote(theme_pref),
            json_quote(active_theme),
        )
    }

    fn resolve_line_col_offset(&mut self, line_1based: usize, col_bytes: usize) -> u64 {
        let target = line_1based.saturating_sub(1);
        self.doc.tree_mut().ensure_indexed_for_line(target + 16);
        let max_line = self.doc.editable_line_count().saturating_sub(1);
        let clamped = target.min(max_line);
        let (start, content_end, _) = self.doc.line_byte_range(clamped).unwrap_or((0, 0, 0));
        start + (col_bytes as u64).min(content_end - start)
    }

    /// Execute any CLI/agent IPC command directly against this live `EditorApp`.
    /// Returns `(reply, should_activate_window, should_quit_immediately)`.
    pub fn execute_ipc(
        &mut self,
        cmd: &editor_platform::ipc::IpcCommand,
        rasterizer: &mut dyn GlyphRasterizer,
        clipboard: &mut dyn Clipboard,
        host: &mut dyn PlatformHost,
    ) -> (editor_platform::ipc::IpcReply, bool, bool) {
        use editor_platform::ipc::{IpcCommand, IpcReply};
        match cmd {
            IpcCommand::Status => (IpcReply::Json(self.status_json()), false, false),
            IpcCommand::Open(path) => {
                self.handle_event(
                    InputEvent::OpenPath(path.clone()),
                    rasterizer,
                    clipboard,
                    host,
                );
                (IpcReply::Json(self.status_json()), true, false)
            }
            IpcCommand::OpenFolder(dir) => {
                self.sidebar.set_visible(true);
                self.sidebar.set_root(self.fs.as_ref(), dir);
                self.recompute_layout();
                self.mark_dirty();
                host.request_redraw();
                (IpcReply::Json(self.status_json()), true, false)
            }
            IpcCommand::Goto {
                line_1based,
                col_bytes,
            } => {
                let off = self.resolve_line_col_offset(*line_1based, *col_bytes);
                self.caret = off;
                self.anchor = off;
                self.preferred_x_px = None;
                let m = self.doc_metrics(rasterizer);
                self.viewport
                    .ensure_caret_visible(&mut self.doc, self.caret, m, self.framebuffer.scale);
                self.mark_dirty();
                host.request_redraw();
                (IpcReply::Json(self.status_json()), false, false)
            }
            IpcCommand::Select {
                start_line,
                start_col,
                end_line,
                end_col,
            } => {
                let a = self.resolve_line_col_offset(*start_line, *start_col);
                let b = self.resolve_line_col_offset(*end_line, *end_col);
                self.anchor = a;
                self.caret = b;
                let m = self.doc_metrics(rasterizer);
                self.viewport
                    .ensure_caret_visible(&mut self.doc, self.caret, m, self.framebuffer.scale);
                self.mark_dirty();
                host.request_redraw();
                (IpcReply::Json(self.status_json()), false, false)
            }
            IpcCommand::Insert { text } => {
                let (start, removed) = self
                    .selection()
                    .map(|(a, b)| (a, b - a))
                    .unwrap_or((self.caret, 0));
                match self.doc.edit(start, removed, text, self.now_ms, false) {
                    Ok(pos) => {
                        self.caret = pos;
                        self.anchor = pos;
                        self.after_document_mutated(rasterizer, host);
                        (IpcReply::Json(self.status_json()), false, false)
                    }
                    Err(e) => (IpcReply::Error(e.to_string()), false, false),
                }
            }
            IpcCommand::Find {
                query,
                replace,
                case_sensitive,
                replace_all,
                close,
            } => {
                if *close {
                    self.find_bar.close();
                    self.search_matches.clear();
                    self.active_match = None;
                    self.recompute_layout();
                    self.mark_dirty();
                    host.request_redraw();
                    return (IpcReply::Json(self.status_json()), false, false);
                }
                self.goto_line.close();
                self.palette.close();
                self.find_bar.open(replace.is_some(), Some(query));
                self.find_bar.find_input.set_text(query.clone());
                self.find_bar.case_sensitive = *case_sensitive;
                if let Some(rep) = replace {
                    self.find_bar.replace_input.set_text(rep.clone());
                }
                self.recompute_layout();
                self.refresh_search();
                if *replace_all && replace.is_some() {
                    self.replace_all_matches(rasterizer, host);
                } else if let Some(idx) = self.active_match {
                    self.select_match(idx, rasterizer);
                }
                self.mark_dirty();
                host.request_redraw();
                (IpcReply::Json(self.status_json()), false, false)
            }
            IpcCommand::Save { as_path } => {
                let res = match as_path {
                    Some(dst) => self.doc.save_as(self.fs.as_ref(), dst, None, false),
                    None => self.doc.save(self.fs.as_ref(), None, false),
                };
                match res {
                    Ok(rep) => {
                        self.sync_title(host);
                        self.sidebar.refresh(self.fs.as_ref());
                        self.set_notice(format!("Saved ({})", rep.method.as_str()), false);
                        host.request_redraw();
                        (IpcReply::Json(self.status_json()), false, false)
                    }
                    Err(e) => (IpcReply::Error(e.to_string()), false, false),
                }
            }
            IpcCommand::Split { path } => {
                self.split_pane_with(path.as_deref(), host);
                (IpcReply::Json(self.status_json()), true, false)
            }
            IpcCommand::Unsplit => {
                self.close_split(host);
                (IpcReply::Json(self.status_json()), false, false)
            }
            IpcCommand::FocusPane(idx) => {
                self.focus_pane(*idx, host);
                (IpcReply::Json(self.status_json()), false, false)
            }
            IpcCommand::Action(name) => {
                let mut activate = false;
                match name.as_str() {
                    "new" => self.handle_command(AppCommand::NewFile, rasterizer, clipboard, host),
                    "reload" => {
                        self.handle_command(AppCommand::Reload, rasterizer, clipboard, host)
                    }
                    "save" => self.handle_command(AppCommand::Save, rasterizer, clipboard, host),
                    "undo" => self.handle_command(AppCommand::Undo, rasterizer, clipboard, host),
                    "redo" => self.handle_command(AppCommand::Redo, rasterizer, clipboard, host),
                    "cut" => self.handle_command(AppCommand::Cut, rasterizer, clipboard, host),
                    "copy" => self.handle_command(AppCommand::Copy, rasterizer, clipboard, host),
                    "paste" => self.handle_command(AppCommand::Paste, rasterizer, clipboard, host),
                    "select-all" => {
                        self.handle_command(AppCommand::SelectAll, rasterizer, clipboard, host)
                    }
                    "find" => {
                        activate = true;
                        self.handle_command(AppCommand::Find, rasterizer, clipboard, host);
                    }
                    "replace" => {
                        activate = true;
                        self.handle_command(AppCommand::Replace, rasterizer, clipboard, host);
                    }
                    "goto-line" => {
                        activate = true;
                        self.handle_command(AppCommand::GoToLine, rasterizer, clipboard, host);
                    }
                    "quick-open" => {
                        activate = true;
                        self.handle_command(AppCommand::QuickOpen, rasterizer, clipboard, host);
                    }
                    "project-search" => {
                        activate = true;
                        self.handle_command(AppCommand::ProjectSearch, rasterizer, clipboard, host);
                    }
                    "toggle-wrap" => {
                        self.handle_command(AppCommand::ToggleWrap, rasterizer, clipboard, host)
                    }
                    "toggle-sidebar" => {
                        self.handle_command(AppCommand::ToggleSidebar, rasterizer, clipboard, host)
                    }
                    "sidebar-show" => {
                        self.sidebar.set_visible(true);
                        self.recompute_layout();
                        self.mark_dirty();
                        host.request_redraw();
                    }
                    "sidebar-hide" => {
                        self.sidebar.set_visible(false);
                        self.recompute_layout();
                        self.mark_dirty();
                        host.request_redraw();
                    }
                    "zoom-in" => {
                        self.handle_command(AppCommand::ZoomIn, rasterizer, clipboard, host)
                    }
                    "zoom-out" => {
                        self.handle_command(AppCommand::ZoomOut, rasterizer, clipboard, host)
                    }
                    "zoom-reset" => {
                        self.handle_command(AppCommand::ZoomReset, rasterizer, clipboard, host)
                    }
                    "theme-system" => self.handle_command(
                        AppCommand::SetTheme(ThemePreference::System),
                        rasterizer,
                        clipboard,
                        host,
                    ),
                    "theme-light" => self.handle_command(
                        AppCommand::SetTheme(ThemePreference::Light),
                        rasterizer,
                        clipboard,
                        host,
                    ),
                    "theme-dark" => self.handle_command(
                        AppCommand::SetTheme(ThemePreference::Dark),
                        rasterizer,
                        clipboard,
                        host,
                    ),
                    "duplicate-line" => self.duplicate_current_line(rasterizer, host),
                    "delete-line" => self.delete_current_line(rasterizer, host),
                    "toggle-comment" => self.toggle_line_comment(rasterizer, host),
                    "shortcuts" => {
                        activate = true;
                        self.handle_command(AppCommand::ShowShortcuts, rasterizer, clipboard, host);
                    }
                    "toggle-vim" => {
                        self.handle_command(AppCommand::ToggleVimMode, rasterizer, clipboard, host);
                    }
                    "vim-on" => {
                        self.vim.set_enabled(true);
                        self.save_preferences(host);
                        self.mark_dirty();
                        host.request_redraw();
                    }
                    "vim-off" => {
                        self.vim.set_enabled(false);
                        self.save_preferences(host);
                        self.mark_dirty();
                        host.request_redraw();
                    }
                    "split" => self.handle_command(AppCommand::SplitPane, rasterizer, clipboard, host),
                    "unsplit" | "close-pane" => {
                        self.handle_command(AppCommand::CloseSplit, rasterizer, clipboard, host)
                    }
                    "swap-panes" => self.handle_command(AppCommand::SwapPanes, rasterizer, clipboard, host),
                    "focus-left" => self.handle_command(AppCommand::FocusLeftPane, rasterizer, clipboard, host),
                    "focus-right" => self.handle_command(AppCommand::FocusRightPane, rasterizer, clipboard, host),
                    "find-next" => self.handle_command(AppCommand::FindNext, rasterizer, clipboard, host),
                    "find-prev" => self.handle_command(AppCommand::FindPrev, rasterizer, clipboard, host),
                    "move-line-up" => self.handle_command(AppCommand::MoveLineUp, rasterizer, clipboard, host),
                    "move-line-down" => self.handle_command(AppCommand::MoveLineDown, rasterizer, clipboard, host),
                    "open-dialog" => self.handle_command(AppCommand::OpenFile, rasterizer, clipboard, host),
                    "open-folder" => self.handle_command(AppCommand::OpenFolder, rasterizer, clipboard, host),
                    "save-as" => self.handle_command(AppCommand::SaveAs, rasterizer, clipboard, host),
                    "quit" => self.handle_command(AppCommand::Quit, rasterizer, clipboard, host),
                    "focus" => activate = true,
                    other if other.starts_with("wrap-ext:") => {
                        let spec = &other["wrap-ext:".len()..];
                        if let Some((ext, val)) = spec.split_once('=') {
                            let mode = match val.trim().to_ascii_lowercase().as_str() {
                                "1" | "on" | "true" => Some(Some(true)),
                                "0" | "off" | "false" => Some(Some(false)),
                                "default" | "reset" | "none" => Some(None),
                                _ => None,
                            };
                            if let Some(m) = mode {
                                self.set_word_wrap_for_ext(ext, m, host);
                            } else {
                                return (
                                    IpcReply::Error(format!(
                                        "invalid wrap value `{val}` (use on|off|default)"
                                    )),
                                    false,
                                    false,
                                );
                            }
                        } else {
                            return (
                                IpcReply::Error(
                                    "usage: wrap-ext:<ext>=<on|off|default>".to_string(),
                                ),
                                false,
                                false,
                            );
                        }
                    }
                    other => {
                        return (
                            IpcReply::Error(format!("unknown action: {other}")),
                            false,
                            false,
                        );
                    }
                }
                (IpcReply::Json(self.status_json()), activate, false)
            }
            IpcCommand::Quit { force } => {
                if *force || !self.doc.is_dirty() {
                    (IpcReply::Quitting, false, true)
                } else {
                    self.handle_command(AppCommand::Quit, rasterizer, clipboard, host);
                    (IpcReply::Prompting, true, false)
                }
            }
        }
    }
}

fn draw_pane_header(
    painter: &mut Painter,
    rect: PixelRect,
    file_name: &str,
    dirty: bool,
    active: bool,
    theme: &Theme,
    atlas: &mut GlyphAtlas,
    rasterizer: &mut dyn GlyphRasterizer,
) {
    if rect.is_empty() {
        return;
    }
    let scale = painter.scale();
    let hairline = (1.0 * scale).round().max(1.0) as i32;
    let bg = if active { theme.surface } else { theme.page };
    painter.fill_rect(rect, bg);
    painter.fill_rect(
        PixelRect::new(rect.x, rect.bottom() - hairline, rect.w, hairline),
        theme.border_subtle,
    );
    let font_px = 11.5 * scale;
    let style = if active { FontStyle::UiBold } else { FontStyle::Ui };
    let color = if active { theme.text } else { theme.text_muted };
    let metrics = atlas.metrics(rasterizer, style, font_px);
    let baseline = rect.y as f32 + (rect.h as f32 + metrics.ascent - metrics.descent) * 0.5;
    let label = if dirty {
        format!("{file_name} •")
    } else {
        file_name.to_string()
    };
    let close_w = (24.0 * scale).round() as i32;
    let text_clip = PixelRect::new(rect.x, rect.y, (rect.w - close_w).max(0), rect.h);
    painter.push_clip(text_clip);
    painter.draw_text_ui(
        rect.x as f32 + (12.0 * scale),
        baseline,
        &label,
        style,
        font_px,
        color,
        atlas,
        rasterizer,
    );
    painter.pop_clip();

    // Quiet `×` close button on the right edge of each pane header.
    let close_glyph = "×";
    let cw = Painter::measure_text_ui(close_glyph, FontStyle::Ui, 13.0 * scale, atlas, rasterizer);
    let cm = atlas.metrics(rasterizer, FontStyle::Ui, 13.0 * scale);
    let cbase = rect.y as f32 + (rect.h as f32 + cm.ascent - cm.descent) * 0.5;
    painter.draw_text_ui(
        rect.right() as f32 - (close_w as f32 + cw) * 0.5,
        cbase,
        close_glyph,
        FontStyle::Ui,
        13.0 * scale,
        theme.text_muted,
        atlas,
        rasterizer,
    );
}

fn file_ext_key(path: Option<&Path>) -> Option<String> {
    let ext = path?.extension()?.to_str()?;
    let lower = ext.trim().to_ascii_lowercase();
    (!lower.is_empty()).then_some(lower)
}

fn default_wrap_by_ext() -> BTreeMap<String, bool> {
    let mut m = BTreeMap::new();
    m.insert("md".to_string(), true);
    m.insert("markdown".to_string(), true);
    m.insert("txt".to_string(), true);
    m
}

fn parse_wrap_by_ext(raw: &str) -> BTreeMap<String, bool> {
    let mut m = BTreeMap::new();
    for pair in raw.split(',') {
        let pair = pair.trim();
        if let Some((k, v)) = pair.split_once('=') {
            let ext = k.trim().trim_start_matches('.').to_ascii_lowercase();
            if !ext.is_empty() {
                m.insert(ext, v.trim() == "1" || v.trim().eq_ignore_ascii_case("true"));
            }
        }
    }
    m
}

fn format_wrap_by_ext(map: &BTreeMap<String, bool>) -> String {
    let mut out = String::new();
    for (idx, (k, &v)) in map.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        out.push_str(k);
        out.push('=');
        out.push(if v { '1' } else { '0' });
    }
    out
}

fn wrap_by_ext_json(map: &BTreeMap<String, bool>) -> String {
    let mut out = String::from("{");
    for (idx, (k, &v)) in map.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        out.push_str(&json_quote(k));
        out.push(':');
        out.push_str(if v { "true" } else { "false" });
    }
    out.push('}');
    out
}

fn json_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                use std::fmt::Write;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
