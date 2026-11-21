//! Platform-agnostic application state machine and event controller.
//!
//! Connects `editor-core`, `editor-render`, and the minimal `editor-platform` traits.
//! 100% testable headlessly by feeding `InputEvent`s and inspecting document state
//! and rendered pixels in `Framebuffer`.

pub mod buffers;
pub mod keymap;
pub mod panes;
pub mod vim;
mod vim_exec;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use editor_core::{
    next_grapheme_boundary, next_word_boundary, prev_grapheme_boundary, prev_word_boundary,
    word_range_at, Document, IdentityCheck, Language, LanguageRegistry, SearchMatch, SearchOptions,
    TokenKind,
};
use editor_platform::{
    AppCommand, Clipboard, ConfirmChoice, CursorShape, DragPhase, FileSystem, FontStyle,
    Framebuffer, GlyphRasterizer, InputEvent, Key, Modifiers, MouseButton, PixelRect, PlatformHost,
    SurfaceMut, SystemTheme, ThemePreference,
};
use editor_render::{
    chrome::{draw_status_bar, status_bar_height_px},
    parse_mono_style, tab_strip_height_px, A11yNode, A11yRole, Color, CommandPalette, ConfirmModal,
    FindBarAction, FindBarState, GlyphAtlas, GotoLineState, HarpoonAction, HarpoonOverlay,
    ImeComposition, Painter, PaletteAction, ScrollbarHit, ShortcutsAction, ShortcutsOverlay,
    Sidebar, SidebarAction, SidebarBadge, SidebarHit, StatusBarData, StripDropTarget,
    SyntaxPalette, SyntaxStylePreference, SyntaxThemeConfig, SyntaxThemePreset, TabStrip,
    TabStripHit, Theme, TokenOverride, Viewport,
};

pub use buffers::{BufferId, OpenMode, ParkedBuffer, WorkingSet, MAX_PINS};
pub use keymap::{KeyChord, KeyScope, Keymap, ShortcutEntry};
pub use panes::{
    Arrangement, Direction, Divider, DividerAxis, DropPlan, DropZone, GridLayout, PaneId,
    PaneRect, PaneSlot, MAX_PANES,
};
pub use vim::{VimMode, VimState};

#[derive(Debug, Clone, Copy, Default)]
pub struct AppLayout {
    pub sidebar: PixelRect,
    /// Unified top Working-Set Strip (Harpoon `1..9` pins + Open/Preview tabs).
    pub tab_strip: PixelRect,
    pub find_bar: PixelRect,
    pub goto_bar: PixelRect,
    /// Active pane's document viewport rect (the whole pane area when only one pane is open).
    pub viewport: PixelRect,
    /// Resolved pane grid (up to 2 × 2): per-pane header/body rects plus divider hairlines.
    pub grid: GridLayout,
    pub status_bar: PixelRect,
}

impl AppLayout {
    /// Body (document viewport) rect of the pane at reading-order index `idx`.
    pub fn pane_body(&self, arr: &Arrangement, idx: usize) -> PixelRect {
        arr.id_at_index(idx)
            .and_then(|id| self.grid.get(id))
            .map(|r| r.body)
            .unwrap_or_default()
    }
}

/// What is being dragged towards the tab strip or pane grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DragPayload {
    /// A file from the sidebar or from Finder.
    File(PathBuf),
    /// A tab pill from the top Working-Set Strip.
    Tab(BufferId),
    /// An existing pane, dragged by its header.
    Pane(PaneId),
}

/// Live drag-and-drop gesture state. All visuals are derived from this; damage is limited
/// to the old ∪ new preview and chip rects, so a drag repaints only a few small rects per frame.
#[derive(Debug, Clone)]
struct PaneDrag {
    payload: DragPayload,
    origin: (f32, f32),
    pos: (f32, f32),
    /// Passed the 4pt movement threshold (a plain click never becomes a drag).
    started: bool,
    /// Came from outside the app (Finder): releasing off-grid opens in the active pane.
    external: bool,
    /// Click count at mouse-down (e.g. double-click graduates sidebar preview to a Kept tab).
    click_count: u8,
    plan: Option<DropPlan>,
    /// Drop insertion target when hovering over the top Working-Set Strip.
    strip_drop: Option<(StripDropTarget, i32)>,
    /// Floating label chip rect last drawn (for exact damage tracking).
    chip: PixelRect,
}

/// Independent state for an inactive pane. The *active* pane always lives directly in
/// `EditorApp::{doc, caret, anchor, viewport, ...}` and is swapped in O(1) on focus change.
pub struct PaneState {
    pub id: PaneId,
    pub buf_id: BufferId,
    pub doc: Document,
    pub caret: u64,
    pub anchor: u64,
    preferred_x_px: Option<f32>,
    pub viewport: Viewport,
    pub search_matches: Vec<SearchMatch>,
    pub active_match: Option<usize>,
}

impl PaneState {
    fn new_from_font(id: PaneId, buf_id: BufferId, font_size_pt: f32, word_wrap: bool) -> Self {
        let mut vp = Viewport::new();
        vp.font_size_pt = font_size_pt;
        vp.word_wrap = word_wrap;
        Self {
            id,
            buf_id,
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
    Reload,
    CloseBuffer(BufferId),
    Quit,
}

pub struct EditorApp {
    fs: Arc<dyn FileSystem>,
    pub doc: Document,
    pub caret: u64,
    pub anchor: u64,
    preferred_x_px: Option<f32>,

    /// Stable buffer id currently displayed in the active pane (`self.doc`).
    pub active_buf_id: BufferId,
    /// Shared in-memory buffer pool + Harpoon `1..9` pins + Kept/Preview working set.
    pub working_set: WorkingSet,
    /// Custom-drawn top Working-Set Strip.
    pub tab_strip: TabStrip,
    /// Custom-drawn Harpoon & Open Files quick overlay (`⌘E` / `<Space>e`).
    pub harpoon: HarpoonOverlay,

    pub sidebar: Sidebar,
    pub viewport: Viewport,
    /// Inactive panes (0–3). `self.doc`/`self.viewport` ALWAYS hold the active pane.
    pub other_panes: Vec<PaneState>,
    /// Stable id of the active pane (its slot lives in `arrangement`).
    pub active_id: PaneId,
    next_pane_id: PaneId,
    /// Pane grid model: who lives where (≤ 2 columns × ≤ 2 rows) plus split ratios.
    pub arrangement: Arrangement,
    /// Temporarily zoom the active split pane to 100% fullscreen (`⇧⌘↵` / `⌃W z`).
    pub zoomed: bool,
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
    /// Customizable syntax theme configuration (preset + per-polarity token overrides).
    pub syntax_theme: SyntaxThemeConfig,
    /// Pre-baked 95-byte Light & Dark flat lookup palettes (updated only on theme edits, O(1) per frame!).
    resolved_light_syntax: SyntaxPalette,
    resolved_dark_syntax: SyntaxPalette,
    /// User/agent-configured extension -> language id overrides (e.g. `"ino" => "cpp"`).
    pub syntax_ext_map: BTreeMap<String, String>,
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
    dragging_divider: Option<Divider>,
    drag: Option<PaneDrag>,
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
            active_buf_id: 1,
            working_set: WorkingSet::new(1),
            tab_strip: TabStrip::new(),
            harpoon: HarpoonOverlay::new(),
            sidebar: Sidebar::new(),
            viewport: Viewport::new(),
            other_panes: Vec::new(),
            active_id: 1,
            next_pane_id: 2,
            arrangement: Arrangement::new(1),
            zoomed: false,
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
            syntax_theme: SyntaxThemeConfig::default(),
            resolved_light_syntax: SyntaxPalette::editorial_light(),
            resolved_dark_syntax: SyntaxPalette::editorial_dark(),
            syntax_ext_map: BTreeMap::new(),
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
            dragging_divider: None,
            drag: None,
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
        if let Some(vis) = host.load_pref("SidebarVisible") {
            self.sidebar.set_visible(vis == "1" || vis == "true");
        }
        if self.sidebar.root().is_none() {
            if let Some(raw_dir) = host.load_pref("LastFolder") {
                let dir = PathBuf::from(raw_dir.trim());
                if !dir.as_os_str().is_empty()
                    && self.fs.stat(&dir).map(|s| s.is_dir).unwrap_or(false)
                {
                    self.sidebar.set_root(self.fs.as_ref(), &dir);
                }
            }
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
        if let Some(preset) = host
            .load_pref("SyntaxTheme")
            .or_else(|| host.load_pref("SyntaxStyle"))
            .and_then(|s| SyntaxThemePreset::parse(&s))
        {
            self.syntax_theme.preset = preset;
        }
        if let Some(ov) = host.load_pref("SyntaxLightOverrides") {
            self.syntax_theme.light_overrides = SyntaxThemeConfig::decode_overrides(&ov);
        }
        if let Some(ov) = host.load_pref("SyntaxDarkOverrides") {
            self.syntax_theme.dark_overrides = SyntaxThemeConfig::decode_overrides(&ov);
        }
        self.rebuild_syntax_palettes();
        if let Some(ext_map) = host.load_pref("SyntaxExtMap") {
            self.syntax_ext_map = parse_str_map(&ext_map);
        }
        if let Some(vim) = host.load_pref("VimMode") {
            self.vim.set_enabled(vim == "1" || vim == "true");
        }
        self.sync_wrap_for_panes();
        self.sync_languages_for_panes();
        self.recompute_layout();
        self.mark_dirty();
    }

    pub fn save_preferences(&self, host: &mut dyn PlatformHost) {
        host.save_pref("SidebarWidth", &format!("{:.0}", self.sidebar.width_pt()));
        host.save_pref("SidebarVisible", if self.sidebar.is_visible() { "1" } else { "0" });
        self.persist_last_folder(host);
        host.save_pref("FontSize", &format!("{:.1}", self.viewport.font_size_pt));
        host.save_pref("WordWrap", if self.default_word_wrap { "1" } else { "0" });
        host.save_pref("WordWrapByExt", &format_wrap_by_ext(&self.wrap_by_ext));
        let t = match self.theme_pref {
            ThemePreference::System => "system",
            ThemePreference::Light => "light",
            ThemePreference::Dark => "dark",
        };
        host.save_pref("Theme", t);
        host.save_pref("SyntaxTheme", self.syntax_theme.preset.as_str());
        host.save_pref(
            "SyntaxLightOverrides",
            &SyntaxThemeConfig::encode_overrides(&self.syntax_theme.light_overrides),
        );
        host.save_pref(
            "SyntaxDarkOverrides",
            &SyntaxThemeConfig::encode_overrides(&self.syntax_theme.dark_overrides),
        );
        host.save_pref("SyntaxExtMap", &format_str_map(&self.syntax_ext_map));
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
        for i in 0..self.other_panes.len() {
            let wrap = self.effective_word_wrap(self.other_panes[i].doc.path());
            let sec = &mut self.other_panes[i];
            if sec.viewport.word_wrap != wrap {
                sec.viewport.word_wrap = wrap;
                if wrap {
                    sec.viewport.scroll_x_px = 0.0;
                }
            }
        }
    }

    #[inline]
    pub fn active_is_dark(&self) -> bool {
        match self.theme_pref {
            ThemePreference::System => matches!(self.system_theme, SystemTheme::Dark),
            ThemePreference::Light => false,
            ThemePreference::Dark => true,
        }
    }

    #[inline]
    pub fn active_theme(&self) -> Theme {
        if self.active_is_dark() {
            Theme::dark().with_syntax_palette(self.resolved_dark_syntax)
        } else {
            Theme::light().with_syntax_palette(self.resolved_light_syntax)
        }
    }

    pub fn rebuild_syntax_palettes(&mut self) {
        self.resolved_light_syntax = self.syntax_theme.resolve(false);
        self.resolved_dark_syntax = self.syntax_theme.resolve(true);
    }

    pub fn set_syntax_preset_headless(&mut self, preset: SyntaxThemePreset) {
        self.syntax_theme.preset = preset;
        self.rebuild_syntax_palettes();
        self.mark_dirty();
    }

    pub fn set_theme_preference(&mut self, pref: ThemePreference) {
        self.theme_pref = pref;
        self.mark_dirty();
    }

    fn apply_language_override(&self, doc: &mut Document) {
        if self.syntax_ext_map.is_empty() {
            return;
        }
        if let Some(p) = doc.path().map(Path::to_path_buf) {
            let lang = Language::from_path_with_overrides(Some(p.as_path()), &self.syntax_ext_map);
            doc.set_language(lang);
        }
    }

    fn sync_languages_for_panes(&mut self) {
        if let Some(p) = self.doc.path().map(Path::to_path_buf) {
            let lang = Language::from_path_with_overrides(Some(p.as_path()), &self.syntax_ext_map);
            self.doc.set_language(lang);
        }
        for pane in &mut self.other_panes {
            if let Some(p) = pane.doc.path().map(Path::to_path_buf) {
                let lang = Language::from_path_with_overrides(Some(p.as_path()), &self.syntax_ext_map);
                pane.doc.set_language(lang);
            }
        }
    }

    pub fn set_syntax_style(
        &mut self,
        style: SyntaxStylePreference,
        host: &mut dyn PlatformHost,
    ) {
        self.syntax_theme.preset = style;
        self.rebuild_syntax_palettes();
        self.save_preferences(host);
        self.set_notice(
            match style {
                SyntaxThemePreset::Editorial => "Syntax theme: Editorial",
                SyntaxThemePreset::Monochrome => "Syntax theme: Monochrome",
                SyntaxThemePreset::Studio => "Syntax theme: Studio",
                SyntaxThemePreset::Warm => "Syntax theme: Warm",
            },
            false,
        );
        self.mark_dirty();
        host.request_redraw();
    }

    pub fn set_syntax_token_override(
        &mut self,
        mode: &str,
        token_name: &str,
        color_spec: &str,
        style_spec: &str,
        host: &mut dyn PlatformHost,
    ) -> Result<(), String> {
        let kind = TokenKind::parse(token_name).ok_or_else(|| {
            format!("unknown syntax token `{token_name}` (run `editor syntax theme` to see all 19 tokens)")
        })?;
        let apply_targets: [bool; 2] = match mode.trim().to_ascii_lowercase().as_str() {
            "" | "active" => [!self.active_is_dark(), self.active_is_dark()],
            "light" => [true, false],
            "dark" => [false, true],
            "both" | "all" => [true, true],
            other => {
                return Err(format!(
                    "invalid mode `{other}` (use active|light|dark|both)"
                ));
            }
        };

        let reset = color_spec.eq_ignore_ascii_case("reset")
            || color_spec.eq_ignore_ascii_case("default");
        let parsed_color = if reset
            || color_spec.is_empty()
            || color_spec == "-"
            || color_spec.eq_ignore_ascii_case("keep")
        {
            None
        } else {
            Some(Color::parse_hex(color_spec).ok_or_else(|| {
                format!("invalid hex colour `{color_spec}` (expected #RRGGBB)")
            })?)
        };
        let parsed_font = if style_spec.is_empty()
            || style_spec == "-"
            || style_spec.eq_ignore_ascii_case("keep")
        {
            None
        } else {
            Some(parse_mono_style(style_spec).ok_or_else(|| {
                format!("invalid font style `{style_spec}` (use normal|bold|italic)")
            })?)
        };

        if !reset && parsed_color.is_none() && parsed_font.is_none() {
            return Err("provide a #RRGGBB colour, font style, or `reset`".to_string());
        }

        for (is_dark, enabled) in [(false, apply_targets[0]), (true, apply_targets[1])] {
            if !enabled {
                continue;
            }
            let map = if is_dark {
                &mut self.syntax_theme.dark_overrides
            } else {
                &mut self.syntax_theme.light_overrides
            };
            if reset {
                map.remove(&kind);
            } else {
                let entry = map.entry(kind).or_insert(TokenOverride::default());
                if let Some(c) = parsed_color {
                    entry.color = Some(c);
                }
                if let Some(f) = parsed_font {
                    entry.font = Some(f);
                }
            }
        }
        self.rebuild_syntax_palettes();
        self.save_preferences(host);
        self.set_notice(format!("Updated syntax token `{}`", kind.as_str()), false);
        self.mark_dirty();
        host.request_redraw();
        Ok(())
    }

    pub fn reset_syntax_overrides(&mut self, mode: &str, host: &mut dyn PlatformHost) -> Result<(), String> {
        match mode.trim().to_ascii_lowercase().as_str() {
            "" | "all" | "both" => {
                self.syntax_theme.light_overrides.clear();
                self.syntax_theme.dark_overrides.clear();
            }
            "light" => self.syntax_theme.light_overrides.clear(),
            "dark" => self.syntax_theme.dark_overrides.clear(),
            "active" => {
                if self.active_is_dark() {
                    self.syntax_theme.dark_overrides.clear();
                } else {
                    self.syntax_theme.light_overrides.clear();
                }
            }
            other => {
                return Err(format!(
                    "invalid reset mode `{other}` (use all|light|dark|active)"
                ));
            }
        }
        self.rebuild_syntax_palettes();
        self.save_preferences(host);
        self.set_notice("Reset custom syntax colours", false);
        self.mark_dirty();
        host.request_redraw();
        Ok(())
    }

    pub fn apply_syntax_theme_json(
        &mut self,
        json: &str,
        host: &mut dyn PlatformHost,
    ) -> Result<(), String> {
        let mut next = self.syntax_theme.clone();
        next.apply_json(json)?;
        self.syntax_theme = next;
        self.rebuild_syntax_palettes();
        self.save_preferences(host);
        self.set_notice(
            format!("Applied syntax theme ({})", self.syntax_theme.preset.as_str()),
            false,
        );
        self.mark_dirty();
        host.request_redraw();
        Ok(())
    }

    pub fn syntax_theme_json(&self) -> String {
        self.syntax_theme.to_json(self.active_is_dark())
    }

    pub fn set_active_language(
        &mut self,
        id_or_alias: &str,
        host: &mut dyn PlatformHost,
    ) -> Result<Language, String> {
        let lang = Language::from_id(id_or_alias).ok_or_else(|| {
            format!("unknown language `{id_or_alias}` (run `editor syntax list` to see available plugins)")
        })?;
        self.doc.set_language(lang);
        self.set_notice(format!("Language: {}", lang.display_name()), false);
        self.mark_dirty();
        host.request_redraw();
        Ok(lang)
    }

    pub fn set_syntax_ext_map(
        &mut self,
        ext: &str,
        lang_spec: Option<&str>,
        host: &mut dyn PlatformHost,
    ) -> Result<(), String> {
        let key = ext.trim().trim_start_matches('.').to_ascii_lowercase();
        if key.is_empty() {
            return Err("extension cannot be empty".to_string());
        }
        match lang_spec {
            Some(spec) => {
                let lang = Language::from_id(spec).ok_or_else(|| {
                    format!("unknown language `{spec}` (run `editor syntax list` to see available plugins)")
                })?;
                self.syntax_ext_map.insert(key.clone(), lang.id().to_string());
                self.set_notice(format!("Mapped .{key} to {}", lang.display_name()), false);
            }
            None => {
                self.syntax_ext_map.remove(&key);
                self.set_notice(format!("Reset .{key} language mapping"), false);
            }
        }
        self.sync_languages_for_panes();
        self.save_preferences(host);
        self.mark_dirty();
        host.request_redraw();
        Ok(())
    }

    pub fn languages_json() -> String {
        let reg = LanguageRegistry::global();
        let mut out = String::from("{\"languages\":[");
        for (i, lang) in reg.all().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let m = lang.meta();
            out.push_str("{\"id\":");
            out.push_str(&json_quote(m.id));
            out.push_str(",\"name\":");
            out.push_str(&json_quote(m.name));
            out.push_str(",\"extensions\":[");
            for (j, ext) in m.extensions.iter().enumerate() {
                if j > 0 {
                    out.push(',');
                }
                out.push_str(&json_quote(ext));
            }
            out.push_str("],\"aliases\":[");
            for (j, alias) in m.aliases.iter().enumerate() {
                if j > 0 {
                    out.push(',');
                }
                out.push_str(&json_quote(alias));
            }
            out.push_str("],\"line_comment\":");
            match m.line_comment {
                Some(lc) => out.push_str(&json_quote(lc)),
                None => out.push_str("null"),
            }
            out.push_str(",\"default_word_wrap\":");
            out.push_str(if m.default_word_wrap { "true" } else { "false" });
            out.push('}');
        }
        out.push_str("]}");
        out
    }

    fn persist_last_folder(&self, host: &mut dyn PlatformHost) {
        if let Some(root) = self.sidebar.root() {
            host.save_pref("LastFolder", &root.to_string_lossy());
        }
    }

    pub fn set_sidebar_root(&mut self, dir: &Path) {
        self.sidebar.set_root(self.fs.as_ref(), dir);
        self.mark_dirty();
    }

    // ========================================================================
    // Buffer Pool, Working-Set Strip (Harpoon 1..9 + Open/Preview) & Zoom
    // ========================================================================

    fn is_initial_empty_untitled(&self) -> bool {
        self.working_set.len() == 1
            && !self.is_split()
            && self.doc.path().is_none()
            && !self.doc.is_dirty()
            && self.doc.len_bytes() == 0
            && !self.working_set.is_pinned(self.active_buf_id)
    }

    fn other_pane_buf_ids(&self) -> Vec<BufferId> {
        self.other_panes.iter().map(|p| p.buf_id).collect()
    }

    fn is_buf_in_other_pane(&self, id: BufferId) -> bool {
        self.other_panes.iter().any(|p| p.buf_id == id)
    }

    fn sync_active_buffer_meta(&mut self) {
        let path = self.doc.path().map(Path::to_path_buf);
        let name = self.doc.file_name().to_string();
        let dirty = self.doc.is_dirty();
        self.working_set
            .update_meta(self.active_buf_id, path, name, dirty);
        if dirty && self.working_set.is_preview(self.active_buf_id) {
            self.working_set.graduate_preview(self.active_buf_id);
        }
    }

    fn is_buffer_dirty(&self, id: BufferId) -> bool {
        if id == self.active_buf_id {
            return self.doc.is_dirty();
        }
        if let Some(p) = self.other_panes.iter().find(|p| p.buf_id == id) {
            return p.doc.is_dirty();
        }
        self.working_set
            .get_parked(id)
            .map(|p| p.doc.is_dirty())
            .or_else(|| self.working_set.get_meta(id).map(|m| m.dirty))
            .unwrap_or(false)
    }

    fn any_buffer_dirty(&self) -> bool {
        if self.doc.is_dirty() || self.other_panes.iter().any(|p| p.doc.is_dirty()) {
            return true;
        }
        self.working_set.any_dirty()
    }

    fn first_dirty_buffer(&self) -> Option<BufferId> {
        if self.doc.is_dirty() {
            return Some(self.active_buf_id);
        }
        if let Some(p) = self.other_panes.iter().find(|p| p.doc.is_dirty()) {
            return Some(p.buf_id);
        }
        self.working_set.first_dirty()
    }

    fn park_active_into_pool(&mut self, drop_current: bool) {
        let old_id = self.active_buf_id;
        let old_doc = std::mem::replace(&mut self.doc, Document::new_untitled());
        if drop_current {
            if !self.is_buf_in_other_pane(old_id) {
                self.working_set.remove_buffer(old_id);
            }
            return;
        }
        if !self.working_set.contains(old_id) {
            return;
        }
        // Only park if no other pane currently holds a live copy (or ours is dirty).
        if !self.is_buf_in_other_pane(old_id) || old_doc.is_dirty() {
            self.working_set.park(
                old_id,
                ParkedBuffer {
                    doc: old_doc,
                    caret: self.caret,
                    anchor: self.anchor,
                    preferred_x_px: self.preferred_x_px,
                    scroll_top_line: self.viewport.scroll_top_line,
                    scroll_sub_y_px: self.viewport.scroll_sub_y_px,
                    scroll_x_px: self.viewport.scroll_x_px,
                },
            );
        }
    }

    fn install_buffer_in_active_pane(&mut self, target_id: BufferId) -> bool {
        if let Some(parked) = self.working_set.take_parked(target_id) {
            self.active_buf_id = target_id;
            self.doc = parked.doc;
            self.caret = parked.caret.min(self.doc.len_bytes());
            self.anchor = parked.anchor.min(self.doc.len_bytes());
            self.preferred_x_px = parked.preferred_x_px;
            self.viewport.reset_for_document();
            self.viewport.scroll_top_line = parked.scroll_top_line;
            self.viewport.scroll_sub_y_px = parked.scroll_sub_y_px;
            self.viewport.scroll_x_px = parked.scroll_x_px;
            self.viewport.word_wrap = self.effective_word_wrap(self.doc.path());
            self.search_matches.clear();
            self.active_match = None;
            return true;
        }
        // Fallback: open from path recorded in metadata (or create untitled).
        let path_opt = self
            .working_set
            .get_meta(target_id)
            .and_then(|m| m.path.clone());
        let mut doc = match path_opt {
            Some(ref p) => match Document::open(self.fs.as_ref(), p) {
                Ok(d) => d,
                Err(_) => return false,
            },
            None => Document::new_untitled(),
        };
        self.apply_language_override(&mut doc);
        self.active_buf_id = target_id;
        self.doc = doc;
        self.caret = 0;
        self.anchor = 0;
        self.preferred_x_px = None;
        self.viewport.reset_for_document();
        self.viewport.word_wrap = self.effective_word_wrap(self.doc.path());
        self.search_matches.clear();
        self.active_match = None;
        true
    }

    /// Activate `target_id`: if already displayed in another pane, teleports focus there
    /// (preserving grid layout and zoomed fullscreen seamlessly); otherwise swaps it into
    /// the active pane while parking the outgoing buffer safely in memory!
    pub fn activate_buffer(&mut self, target_id: BufferId, host: &mut dyn PlatformHost) -> bool {
        if !self.working_set.contains(target_id) {
            return false;
        }
        if target_id == self.active_buf_id {
            self.working_set.touch_mru(target_id);
            self.sync_title(host);
            return true;
        }
        // Teleport to an existing split pane if it already displays `target_id`.
        if let Some(pid) = self
            .other_panes
            .iter()
            .find(|p| p.buf_id == target_id)
            .map(|p| p.id)
        {
            self.focus_pane_id(pid, host);
            self.working_set.touch_mru(target_id);
            if let Some(p) = self.doc.path().map(Path::to_path_buf) {
                if self.sidebar.is_under_root(&p) {
                    self.sidebar.reveal(self.fs.as_ref(), &p);
                }
            }
            self.mark_dirty();
            host.request_redraw();
            return true;
        }
        self.sync_active_buffer_meta();
        self.park_active_into_pool(false);
        if !self.install_buffer_in_active_pane(target_id) {
            return false;
        }
        self.working_set.touch_mru(target_id);
        if self.find_bar.visible {
            self.refresh_search();
        }
        if let Some(p) = self.doc.path().map(Path::to_path_buf) {
            if self.sidebar.is_under_root(&p) {
                self.sidebar.reveal(self.fs.as_ref(), &p);
            }
        }
        self.sync_title(host);
        self.mark_dirty();
        host.request_redraw();
        true
    }

    /// Open a file into the Working-Set Strip (`OpenMode::Preview` for single-click sidebar
    /// browse; `OpenMode::Keep` for double-click, Quick Open, CLI open, or explicit opens).
    pub fn open_file_with_mode(
        &mut self,
        path: &Path,
        mode: OpenMode,
        host: &mut dyn PlatformHost,
    ) -> Result<BufferId, String> {
        let canonical = self
            .fs
            .canonicalize(path)
            .unwrap_or_else(|_| path.to_path_buf());

        // Sidebar root is set once (if unset) and never re-rooted on subsequent file opens!
        if self.sidebar.root().is_none() {
            if let Some(parent) = canonical.parent() {
                self.sidebar.set_root(self.fs.as_ref(), parent);
                self.persist_last_folder(host);
            }
        }

        // Reuse existing buffer if already open in the working set!
        if let Some(existing_id) = self.working_set.find_by_path(&canonical) {
            if mode == OpenMode::Keep {
                self.working_set.graduate_preview(existing_id);
            }
            self.activate_buffer(existing_id, host);
            return Ok(existing_id);
        }

        let mut doc = Document::open(self.fs.as_ref(), &canonical).map_err(|e| e.to_string())?;
        self.apply_language_override(&mut doc);
        let doc_path = doc.path().map(Path::to_path_buf).or(Some(canonical.clone()));
        let fname = doc.file_name().to_string();

        let target_id = if self.is_initial_empty_untitled() {
            // Replace the untouched startup Untitled placeholder cleanly.
            let id = self.active_buf_id;
            self.doc = doc;
            self.caret = 0;
            self.anchor = 0;
            self.preferred_x_px = None;
            self.viewport.reset_for_document();
            self.viewport.word_wrap = self.effective_word_wrap(doc_path.as_deref());
            self.search_matches.clear();
            self.active_match = None;
            self.working_set.update_meta(id, doc_path.clone(), fname, false);
            if mode == OpenMode::Preview {
                self.working_set.open.retain(|&b| b != id);
                self.working_set.preview = Some(id);
            }
            id
        } else {
            self.sync_active_buffer_meta();
            let new_id = self.working_set.alloc_buffer(doc_path.clone(), fname, false);
            let evicted = self.working_set.register_buffer(new_id, mode);
            let drop_current = evicted == Some(self.active_buf_id);
            if let Some(old_prev) = evicted {
                if old_prev != self.active_buf_id && !self.is_buf_in_other_pane(old_prev) {
                    self.working_set.remove_buffer(old_prev);
                }
            }
            self.park_active_into_pool(drop_current);
            self.active_buf_id = new_id;
            self.doc = doc;
            self.caret = 0;
            self.anchor = 0;
            self.preferred_x_px = None;
            self.viewport.reset_for_document();
            self.viewport.word_wrap = self.effective_word_wrap(doc_path.as_deref());
            self.search_matches.clear();
            self.active_match = None;
            new_id
        };

        self.working_set.touch_mru(target_id);
        if self.find_bar.visible {
            self.refresh_search();
        }
        if let Some(ref p) = doc_path {
            if self.sidebar.is_under_root(p) {
                self.sidebar.reveal(self.fs.as_ref(), p);
            }
        }
        self.sync_title(host);
        self.mark_dirty();
        host.request_redraw();
        Ok(target_id)
    }

    pub fn open_file(
        &mut self,
        path: &Path,
        host: &mut dyn PlatformHost,
    ) -> Result<(), String> {
        self.open_file_with_mode(path, OpenMode::Keep, host)
            .map(|_| ())
    }

    /// Create a fresh untitled tab in the Working-Set Strip and switch to it without losing edits.
    pub fn new_tab(&mut self, host: &mut dyn PlatformHost) -> BufferId {
        self.sync_active_buffer_meta();
        self.park_active_into_pool(false);
        let id = self
            .working_set
            .alloc_buffer(None, "Untitled".to_string(), false);
        self.working_set.register_buffer(id, OpenMode::Keep);
        self.active_buf_id = id;
        self.doc = Document::new_untitled();
        self.caret = 0;
        self.anchor = 0;
        self.preferred_x_px = None;
        self.viewport.reset_for_document();
        self.viewport.word_wrap = self.effective_word_wrap(None);
        self.search_matches.clear();
        self.active_match = None;
        self.working_set.touch_mru(id);
        self.sync_title(host);
        self.mark_dirty();
        host.request_redraw();
        id
    }

    /// Toggle Harpoon pin (`1..9`) on `id`.
    pub fn toggle_pin_buffer(&mut self, id: BufferId, host: &mut dyn PlatformHost) {
        if !self.working_set.contains(id) {
            return;
        }
        match self.working_set.toggle_pin(id) {
            Ok(Some(slot)) => {
                self.set_notice(format!("Pinned to slot {slot}"), false);
            }
            Ok(None) => {
                self.set_notice("Unpinned", false);
            }
            Err(()) => {
                self.set_notice(format!("Maximum of {MAX_PINS} pinned files"), false);
            }
        }
        self.mark_dirty();
        host.request_redraw();
    }

    /// Jump directly to Harpoon pin slot `1..=9` (`⌘1`..`⌘9` / `<Space>1`..`9`).
    pub fn jump_to_pin(&mut self, slot_1based: u8, host: &mut dyn PlatformHost) -> bool {
        let idx = slot_1based.saturating_sub(1) as usize;
        if let Some(&id) = self.working_set.pinned.get(idx) {
            self.activate_buffer(id, host)
        } else {
            self.set_notice(format!("No file pinned in slot {slot_1based}"), false);
            host.request_redraw();
            false
        }
    }

    /// Toggle between the two most recently used buffers (`⌃Tab` / `⌃^` / `<Space><Space>`).
    pub fn toggle_last_buffer(&mut self, host: &mut dyn PlatformHost) -> bool {
        if let Some(prev) = self.working_set.previous_mru(self.active_buf_id) {
            self.activate_buffer(prev, host)
        } else {
            false
        }
    }

    /// Cycle next (`+1`) or previous (`-1`) along the Working-Set Strip (`⇧⌘[` / `⇧⌘]`, `gt` / `gT`).
    pub fn step_tab(&mut self, delta: isize, host: &mut dyn PlatformHost) -> bool {
        if let Some(next) = self.working_set.step_strip(self.active_buf_id, delta) {
            self.activate_buffer(next, host)
        } else {
            false
        }
    }

    /// Toggle 100% fullscreen zoom on the active pane when split (`⇧⌘↵` / `⌃W z`).
    pub fn toggle_zoom_pane(&mut self, host: &mut dyn PlatformHost) {
        if !self.is_split() {
            self.zoomed = false;
            return;
        }
        self.zoomed = !self.zoomed;
        self.recompute_layout();
        self.mark_dirty();
        host.request_redraw();
    }

    /// Close all split panes except the active one (`⌃W o`), parking their buffers safely.
    pub fn only_active_pane(&mut self, host: &mut dyn PlatformHost) {
        if self.other_panes.is_empty() {
            return;
        }
        let others = std::mem::take(&mut self.other_panes);
        for p in others {
            if p.buf_id != self.active_buf_id && self.working_set.contains(p.buf_id) {
                self.working_set.park(
                    p.buf_id,
                    ParkedBuffer {
                        doc: p.doc,
                        caret: p.caret,
                        anchor: p.anchor,
                        preferred_x_px: p.preferred_x_px,
                        scroll_top_line: p.viewport.scroll_top_line,
                        scroll_sub_y_px: p.viewport.scroll_sub_y_px,
                        scroll_x_px: p.viewport.scroll_x_px,
                    },
                );
            }
        }
        self.arrangement = Arrangement::new(self.active_id);
        self.zoomed = false;
        self.recompute_layout();
        self.mark_dirty();
        host.request_redraw();
    }

    /// Close buffer `id` from the Working-Set Strip, prompting first if it has unsaved changes.
    pub fn request_close_buffer(&mut self, id: BufferId, host: &mut dyn PlatformHost) {
        if !self.working_set.contains(id) {
            return;
        }
        if self.is_buffer_dirty(id) {
            // Focus the dirty buffer so the user sees what they are deciding to save/discard.
            self.activate_buffer(id, host);
            self.request_discard_or_run(PendingDiscardAction::CloseBuffer(id), host);
        } else {
            self.force_close_buffer(id, host);
        }
    }

    fn force_close_buffer(&mut self, id: BufferId, host: &mut dyn PlatformHost) {
        if !self.working_set.contains(id) {
            return;
        }
        let replacement = self
            .working_set
            .previous_mru(id)
            .or_else(|| self.working_set.step_strip(id, 1))
            .filter(|&r| r != id);

        self.working_set.remove_buffer(id);

        // Close any inactive split panes displaying `id`.
        let panes_to_drop: Vec<PaneId> = self
            .other_panes
            .iter()
            .filter(|p| p.buf_id == id)
            .map(|p| p.id)
            .collect();
        for pid in panes_to_drop {
            self.arrangement.remove(pid);
            self.other_panes.retain(|p| p.id != pid);
        }
        if self.other_panes.is_empty() {
            self.zoomed = false;
        }

        if id == self.active_buf_id {
            if let Some(next_id) = replacement {
                if let Some(pid) = self
                    .other_panes
                    .iter()
                    .find(|p| p.buf_id == next_id)
                    .map(|p| p.id)
                {
                    // Close the active pane if split, or promote `pid`.
                    if self.is_split() {
                        self.arrangement.remove(self.active_id);
                        let idx = self
                            .other_panes
                            .iter()
                            .position(|p| p.id == pid)
                            .unwrap_or(0);
                        let next = self.other_panes.swap_remove(idx);
                        self.active_id = next.id;
                        self.active_buf_id = next.buf_id;
                        self.doc = next.doc;
                        self.caret = next.caret;
                        self.anchor = next.anchor;
                        self.preferred_x_px = next.preferred_x_px;
                        self.viewport = next.viewport;
                        self.search_matches = next.search_matches;
                        self.active_match = next.active_match;
                        if self.other_panes.is_empty() {
                            self.zoomed = false;
                        }
                    } else {
                        self.install_buffer_in_active_pane(next_id);
                    }
                } else {
                    self.install_buffer_in_active_pane(next_id);
                }
                self.working_set.touch_mru(self.active_buf_id);
            } else {
                // Closing the last buffer resets cleanly to a fresh untitled buffer.
                let fresh = self
                    .working_set
                    .alloc_buffer(None, "Untitled".to_string(), false);
                self.working_set.register_buffer(fresh, OpenMode::Keep);
                self.active_buf_id = fresh;
                self.doc = Document::new_untitled();
                self.caret = 0;
                self.anchor = 0;
                self.preferred_x_px = None;
                self.viewport.reset_for_document();
                self.viewport.word_wrap = self.effective_word_wrap(None);
                self.search_matches.clear();
                self.active_match = None;
                self.working_set.touch_mru(fresh);
            }
        }
        self.recompute_layout();
        self.sync_title(host);
        self.mark_dirty();
        host.request_redraw();
    }

    fn refresh_harpoon_overlay(&mut self, keep_selected_id: Option<u32>) {
        self.sync_active_buffer_meta();
        let others = self.other_pane_buf_ids();
        let entries =
            self.working_set
                .to_harpoon_entries(self.active_buf_id, &others, self.sidebar.root());
        if self.harpoon.visible {
            self.harpoon.refresh(entries, keep_selected_id);
        } else {
            self.harpoon.open(entries);
        }
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
                self.save_preferences(host);
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
        !self.other_panes.is_empty()
    }

    /// Number of visible panes (1–4).
    pub fn pane_count(&self) -> usize {
        self.arrangement.len()
    }

    /// Reading-order index (TL, TR, BL, BR) of the active pane.
    pub fn active_pane_index(&self) -> usize {
        self.arrangement.index_of(self.active_id).unwrap_or(0)
    }

    fn alloc_pane_id(&mut self) -> PaneId {
        let id = self.next_pane_id;
        self.next_pane_id = self.next_pane_id.wrapping_add(1).max(1);
        id
    }

    /// Build a fresh inactive pane, opening `path` (or the active file when `None`).
    fn make_pane(&mut self, path: Option<&Path>) -> Result<PaneState, String> {
        let id = self.alloc_pane_id();
        if let Some(raw_p) = path {
            let canonical = self
                .fs
                .canonicalize(raw_p)
                .unwrap_or_else(|_| raw_p.to_path_buf());
            if let Some(existing_id) = self.working_set.find_by_path(&canonical) {
                self.working_set.graduate_preview(existing_id);
                return self.make_pane_for_buffer(id, existing_id);
            }
            let mut doc = Document::open(self.fs.as_ref(), &canonical).map_err(|e| e.to_string())?;
            self.apply_language_override(&mut doc);
            let doc_path = doc.path().map(Path::to_path_buf).or(Some(canonical));
            let fname = doc.file_name().to_string();
            let buf_id = self.working_set.alloc_buffer(doc_path, fname, false);
            self.working_set.register_buffer(buf_id, OpenMode::Keep);
            let mut pane = PaneState::new_from_font(
                id,
                buf_id,
                self.viewport.font_size_pt,
                self.effective_word_wrap(doc.path()),
            );
            pane.doc = doc;
            return Ok(pane);
        }
        // Splitting without an explicit path opens a view of the active buffer.
        self.make_pane_for_buffer(id, self.active_buf_id)
    }

    fn make_pane_for_buffer(&mut self, pane_id: PaneId, buf_id: BufferId) -> Result<PaneState, String> {
        if let Some(parked) = self.working_set.take_parked(buf_id) {
            let wrap = self.effective_word_wrap(parked.doc.path());
            let mut pane = PaneState::new_from_font(pane_id, buf_id, self.viewport.font_size_pt, wrap);
            pane.doc = parked.doc;
            pane.caret = parked.caret;
            pane.anchor = parked.anchor;
            pane.preferred_x_px = parked.preferred_x_px;
            pane.viewport.scroll_top_line = parked.scroll_top_line;
            pane.viewport.scroll_sub_y_px = parked.scroll_sub_y_px;
            pane.viewport.scroll_x_px = parked.scroll_x_px;
            return Ok(pane);
        }
        let path_opt = if buf_id == self.active_buf_id {
            self.doc.path().map(Path::to_path_buf)
        } else {
            self.working_set
                .get_meta(buf_id)
                .and_then(|m| m.path.clone())
        };
        let mut pane = PaneState::new_from_font(
            pane_id,
            buf_id,
            self.viewport.font_size_pt,
            self.effective_word_wrap(path_opt.as_deref()),
        );
        if let Some(p) = path_opt {
            pane.doc = Document::open(self.fs.as_ref(), &p).map_err(|e| e.to_string())?;
            self.apply_language_override(&mut pane.doc);
        }
        pane.viewport.word_wrap = self.effective_word_wrap(pane.doc.path());
        Ok(pane)
    }

    /// Create a new pane at `zone` of pane `target` and focus it.
    /// Returns `false` (with a quiet notice) when the grid is full or the zone is impossible.
    pub fn split_pane_at(
        &mut self,
        target: PaneId,
        zone: DropZone,
        path: Option<&Path>,
        host: &mut dyn PlatformHost,
    ) -> bool {
        if !self.arrangement.can_insert(target, zone) {
            if self.arrangement.is_full() {
                self.set_notice(format!("Maximum of {MAX_PANES} panes"), false);
            }
            return false;
        }
        let pane = match self.make_pane(path) {
            Ok(p) => p,
            Err(e) => {
                self.set_notice(e, true);
                return false;
            }
        };
        let id = pane.id;
        let buf_id = pane.buf_id;
        self.zoomed = false;
        self.arrangement.insert(target, zone, id);
        self.other_panes.push(pane);
        self.focus_pane_id(id, host);
        self.working_set.touch_mru(buf_id);
        self.recompute_layout();
        self.mark_dirty();
        host.request_redraw();
        true
    }

    /// Create a new split pane displaying an existing buffer `buf_id` (used by tab drag-to-split
    /// and `⌘Enter` in the Harpoon overlay).
    pub fn split_buffer_at(
        &mut self,
        target: PaneId,
        zone: DropZone,
        buf_id: BufferId,
        host: &mut dyn PlatformHost,
    ) -> bool {
        if !self.arrangement.can_insert(target, zone) {
            if self.arrangement.is_full() {
                self.set_notice(format!("Maximum of {MAX_PANES} panes"), false);
            }
            return false;
        }
        let pane_id = self.alloc_pane_id();
        let pane = match self.make_pane_for_buffer(pane_id, buf_id) {
            Ok(p) => p,
            Err(e) => {
                self.set_notice(e, true);
                return false;
            }
        };
        self.working_set.graduate_preview(buf_id);
        self.zoomed = false;
        self.arrangement.insert(target, zone, pane_id);
        self.other_panes.push(pane);
        self.focus_pane_id(pane_id, host);
        self.working_set.touch_mru(buf_id);
        self.recompute_layout();
        self.mark_dirty();
        host.request_redraw();
        true
    }

    /// Split the active pane to the right (falling back to below once two columns exist).
    pub fn split_pane_with(&mut self, path: Option<&Path>, host: &mut dyn PlatformHost) {
        let a = self.active_id;
        let zone = if self.arrangement.can_insert(a, DropZone::Right) {
            DropZone::Right
        } else {
            DropZone::Bottom
        };
        self.split_pane_at(a, zone, path, host);
    }

    /// Split the active pane downwards (falling back to the right when its column is full).
    pub fn split_pane_down(&mut self, path: Option<&Path>, host: &mut dyn PlatformHost) {
        let a = self.active_id;
        let zone = if self.arrangement.can_insert(a, DropZone::Bottom) {
            DropZone::Bottom
        } else {
            DropZone::Right
        };
        self.split_pane_at(a, zone, path, host);
    }

    /// Close the active split pane immediately, parking its buffer safely in the Working-Set Strip
    /// and focusing the pane that takes over its space.
    pub fn close_split(&mut self, host: &mut dyn PlatformHost) {
        if self.other_panes.is_empty() {
            return;
        }
        self.sync_active_buffer_meta();
        let old_frame = self.layout.grid.get(self.active_id).map(|r| r.frame);
        self.park_active_into_pool(false);
        self.arrangement.remove(self.active_id);
        if self.other_panes.len() <= 1 {
            self.zoomed = false;
        }
        self.recompute_layout();
        // Prefer the pane that now covers the centre of the closed pane's old area.
        let heir = old_frame
            .and_then(|f| self.layout.grid.pane_at(f.x + f.w / 2, f.y + f.h / 2).map(|r| r.id))
            .filter(|id| self.other_panes.iter().any(|p| p.id == *id))
            .unwrap_or(self.other_panes[0].id);
        let idx = self.other_panes.iter().position(|p| p.id == heir).unwrap_or(0);
        let next = self.other_panes.swap_remove(idx);
        self.active_id = next.id;
        self.active_buf_id = next.buf_id;
        self.doc = next.doc;
        self.caret = next.caret;
        self.anchor = next.anchor;
        self.preferred_x_px = next.preferred_x_px;
        self.viewport = next.viewport;
        self.search_matches = next.search_matches;
        self.active_match = next.active_match;
        self.working_set.touch_mru(self.active_buf_id);
        self.recompute_layout();
        self.sync_title(host);
        self.mark_dirty();
        host.request_redraw();
    }

    /// Close the active split pane (its buffer stays safely parked in the Working-Set Strip).
    pub fn request_close_active_pane(&mut self, host: &mut dyn PlatformHost) {
        if self.other_panes.is_empty() {
            return;
        }
        self.close_split(host);
    }

    /// Swap the active pane with its neighbour (right/left first, then below/above).
    pub fn swap_panes(&mut self, host: &mut dyn PlatformHost) {
        let a = self.active_id;
        let grid = self.layout.grid;
        let other = [Direction::Right, Direction::Left, Direction::Down, Direction::Up]
            .iter()
            .find_map(|d| self.arrangement.neighbor(&grid, a, *d));
        if let Some(b) = other {
            self.arrangement.swap(a, b);
            self.recompute_layout();
            self.mark_dirty();
            host.request_redraw();
        }
    }

    /// Move the active pane next to (or swap with, for `Center`) the pane at index `target_idx`.
    pub fn move_pane(&mut self, src: PaneId, target: PaneId, zone: DropZone, host: &mut dyn PlatformHost) -> bool {
        let ok = self.arrangement.move_pane(src, target, zone);
        if ok {
            self.recompute_layout();
            self.mark_dirty();
            host.request_redraw();
        }
        ok
    }

    /// Focus the pane at reading-order index `idx` (TL=0, TR=1, BL=2, BR=3).
    pub fn focus_pane(&mut self, idx: usize, host: &mut dyn PlatformHost) {
        if let Some(id) = self.arrangement.id_at_index(idx) {
            self.focus_pane_id(id, host);
        }
    }

    /// Focus the spatial neighbour of the active pane in `dir`.
    pub fn focus_direction(&mut self, dir: Direction, host: &mut dyn PlatformHost) {
        let grid = self.layout.grid;
        if let Some(id) = self.arrangement.neighbor(&grid, self.active_id, dir) {
            self.focus_pane_id(id, host);
        }
    }

    /// Cycle focus through panes in reading order.
    pub fn focus_next_pane(&mut self, host: &mut dyn PlatformHost) {
        let n = self.arrangement.len();
        if n > 1 {
            let next = (self.active_pane_index() + 1) % n;
            self.focus_pane(next, host);
        }
    }

    /// Switch focus to pane `id` in O(1) by swapping its state with the active fields.
    pub fn focus_pane_id(&mut self, id: PaneId, host: &mut dyn PlatformHost) {
        if id == self.active_id {
            return;
        }
        let Some(i) = self.other_panes.iter().position(|p| p.id == id) else {
            return;
        };
        let prev_active = self.layout.grid.get(self.active_id).map(|r| r.frame);
        let other = &mut self.other_panes[i];
        std::mem::swap(&mut self.doc, &mut other.doc);
        std::mem::swap(&mut self.caret, &mut other.caret);
        std::mem::swap(&mut self.anchor, &mut other.anchor);
        std::mem::swap(&mut self.preferred_x_px, &mut other.preferred_x_px);
        std::mem::swap(&mut self.viewport, &mut other.viewport);
        std::mem::swap(&mut self.search_matches, &mut other.search_matches);
        std::mem::swap(&mut self.active_match, &mut other.active_match);
        std::mem::swap(&mut self.active_id, &mut other.id);
        std::mem::swap(&mut self.active_buf_id, &mut other.buf_id);
        self.working_set.touch_mru(self.active_buf_id);
        self.recompute_layout();
        self.sync_title(host);
        // Only the two affected panes (headers + carets), the top tab strip, and the status bar change.
        if let Some(f) = prev_active {
            self.mark_dirty_rect(f);
        }
        if let Some(r) = self.layout.grid.get(self.active_id) {
            self.mark_dirty_rect(r.frame);
        }
        self.mark_dirty_rect(self.layout.tab_strip);
        self.mark_dirty_rect(self.layout.status_bar);
        host.request_redraw();
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

        let strip_h = tab_strip_height_px(scale);
        let tab_strip = PixelRect::new(right_x, 0, right_w, strip_h);

        let find_h = self.find_bar.height_px(scale);
        let find_bar = PixelRect::new(right_x, tab_strip.bottom(), right_w, find_h);

        let goto_h = self.goto_line.height_px(scale);
        let goto_bar = PixelRect::new(right_x, find_bar.bottom(), right_w, goto_h);

        let vp_y = goto_bar.bottom();
        let vp_h = (main_h - vp_y).max(0);
        let pane_area = PixelRect::new(right_x, vp_y, right_w, vp_h);
        let grid = if self.zoomed && self.is_split() {
            self.arrangement.layout_zoomed(pane_area, self.active_id)
        } else {
            self.arrangement.layout(pane_area, scale)
        };
        let viewport = grid.get(self.active_id).map(|r| r.body).unwrap_or(pane_area);

        let status_bar = PixelRect::new(0, main_h, w, status_h);

        self.layout = AppLayout {
            sidebar,
            tab_strip,
            find_bar,
            goto_bar,
            viewport,
            grid,
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
            || self.harpoon.visible
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

        self.sync_active_buffer_meta();
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
            && !self.harpoon.visible
            && !self.sidebar.is_focused()
            && !self.find_bar.is_focused()
            && !self.goto_line.visible;
        self.viewport.block_caret = self.vim.uses_block_caret();
        for sec in &mut self.other_panes {
            sec.viewport.block_caret = false;
        }
        let matching_brackets = if sel.is_none() && doc_focused {
            self.doc.find_matching_bracket(self.caret)
        } else {
            None
        };

        let mut painter = Painter::from_surface(surf);
        painter.push_clip(effective_clip);

        // 1. Sidebar (with quiet Harpoon `1..9` badges and `•` dirty dots!)
        if !layout.sidebar.intersect(effective_clip).is_empty() {
            let ws = &self.working_set;
            self.sidebar.draw_with_badges(
                &mut painter,
                layout.sidebar,
                &theme,
                &mut self.atlas,
                rasterizer,
                |path| SidebarBadge {
                    pin_slot: ws.pin_slot_for_path(path),
                    dirty: ws.is_dirty_path(path),
                },
            );
        }

        // 1b. Unified Top Working-Set Strip (Harpoon `1..9` pins + Open/Preview tabs)
        if !layout.tab_strip.intersect(effective_clip).is_empty() {
            let other_ids = self.other_pane_buf_ids();
            let tab_items = self.working_set.to_tab_items(self.active_buf_id, &other_ids);
            let is_split = self.is_split();
            let merge_active = (!is_split || self.zoomed)
                && !self.find_bar.visible
                && !self.goto_line.visible;
            let drop_ix = self
                .drag
                .as_ref()
                .filter(|d| d.started)
                .and_then(|d| d.strip_drop.map(|(_, ix)| ix));
            self.tab_strip.draw(
                &mut painter,
                layout.tab_strip,
                &tab_items,
                is_split,
                self.zoomed,
                merge_active,
                drop_ix,
                &theme,
                &mut self.atlas,
                rasterizer,
            );
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

        // 4. Document viewport(s) — 1 to 4 panes in a ≤ 2 × 2 grid (or 1 zoomed fullscreen pane).
        let show_pane_headers = !self.other_panes.is_empty() && !self.zoomed;
        for pr in layout.grid.panes() {
            let pr = *pr;
            if pr.frame.intersect(effective_clip).is_empty() {
                continue;
            }
            if pr.id == self.active_id {
                if show_pane_headers && !pr.header.intersect(effective_clip).is_empty() {
                    draw_pane_header(
                        &mut painter,
                        pr.header,
                        self.doc.file_name(),
                        self.doc.is_dirty(),
                        true,
                        &theme,
                        &mut self.atlas,
                        rasterizer,
                    );
                }
                if !pr.body.intersect(effective_clip).is_empty() {
                    self.viewport.draw(
                        &mut painter,
                        pr.body,
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
            } else if let Some(sec) = self.other_panes.iter_mut().find(|p| p.id == pr.id) {
                if show_pane_headers && !pr.header.intersect(effective_clip).is_empty() {
                    draw_pane_header(
                        &mut painter,
                        pr.header,
                        sec.doc.file_name(),
                        sec.doc.is_dirty(),
                        false,
                        &theme,
                        &mut self.atlas,
                        rasterizer,
                    );
                }
                if !pr.body.intersect(effective_clip).is_empty() {
                    let sec_sel = sec.selection();
                    sec.viewport.draw(
                        &mut painter,
                        pr.body,
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
        }
        // Crisp 1px hairline dividers between columns / rows.
        for d in layout.grid.dividers() {
            if !d.rect.intersect(effective_clip).is_empty() {
                painter.fill_rect(d.rect, theme.border_subtle);
            }
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

        // 5b. Drag-and-drop preview: exact post-drop rect + floating label chip.
        if let Some(drag) = self.drag.as_ref().filter(|d| d.started) {
            let (plan, chip) = (drag.plan, drag.chip);
            let (name, action) = self.drag_labels();
            draw_drag_overlay(
                &mut painter,
                plan.map(|p| p.preview),
                chip,
                &name,
                action,
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

        // 7b. Optional Harpoon & Open Files Quick Overlay (`Cmd+E`).
        if self.harpoon.visible {
            self.harpoon
                .draw(&mut painter, full_bounds, &theme, &mut self.atlas, rasterizer);
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

        // Intercept keyboard & mouse when Harpoon & Open Files overlay (`Cmd+E`) is open.
        if self.harpoon.visible {
            match event {
                InputEvent::KeyDown { key, mods, .. } => {
                    if self.keymap.lookup_command(key, mods) == Some(AppCommand::ShowHarpoon) {
                        self.harpoon.close();
                        self.mark_dirty();
                        host.request_redraw();
                        return;
                    }
                    let act = self.harpoon.handle_key(key, mods);
                    self.handle_harpoon_action(act, host);
                    return;
                }
                InputEvent::MouseDown {
                    x_px,
                    y_px,
                    button: MouseButton::Left,
                    mods,
                    ..
                } => {
                    let act = self.harpoon.handle_click(x_px, y_px, mods);
                    self.handle_harpoon_action(act, host);
                    return;
                }
                InputEvent::Command(AppCommand::ShowHarpoon) => {
                    self.harpoon.close();
                    self.mark_dirty();
                    host.request_redraw();
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
                if !focused {
                    let _ = self.sidebar.clear_hover();
                    let _ = self.tab_strip.clear_hover();
                    self.viewport.v_bar_hovered = false;
                    self.viewport.h_bar_hovered = false;
                } else {
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
                if let Err(e) = self.open_path_or_dir(&path, host) {
                    self.set_notice(e, true);
                } else {
                    self.sidebar.set_focused(false);
                }
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
                if key == Key::Escape && self.drag.is_some() {
                    self.cancel_drag(host);
                    return;
                }
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
            InputEvent::MouseUp {
                x_px, y_px, button, ..
            } => {
                if button == MouseButton::Left {
                    if self.dragging_sidebar_edge {
                        self.save_preferences(host);
                    }
                    if self.drag.is_some() {
                        self.finish_drag(x_px, y_px, rasterizer, host);
                    }
                    self.dragging_selection = false;
                    self.dragging_gutter_anchor_line = None;
                    self.dragging_find_input = false;
                    self.dragging_replace_input = false;
                    self.dragging_sidebar_edge = false;
                    self.dragging_divider = None;
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
            InputEvent::ExternalDrag {
                phase,
                x_px,
                y_px,
                path,
            } => match phase {
                DragPhase::Enter => {
                    let is_file = path
                        .as_deref()
                        .is_some_and(|p| self.fs.stat(p).map(|st| !st.is_dir).unwrap_or(false));
                    if let (true, Some(p)) = (is_file, path) {
                        self.begin_drag(DragPayload::File(p), x_px, y_px, true, 1);
                        self.update_drag(x_px, y_px, rasterizer, host);
                    }
                }
                DragPhase::Move => {
                    if self.drag.is_some() {
                        self.update_drag(x_px, y_px, rasterizer, host);
                    }
                }
                DragPhase::Exit => self.cancel_drag(host),
                DragPhase::Drop => {
                    if self.drag.is_some() {
                        self.finish_drag(x_px, y_px, rasterizer, host);
                    } else if let Some(p) = path {
                        self.handle_event(InputEvent::OpenPath(p), rasterizer, clipboard, host);
                    }
                }
            },
        }
    }

    // ========================================================================
    // Pane drag & drop (sidebar files, Finder files, pane headers)
    // ========================================================================

    fn begin_drag(
        &mut self,
        payload: DragPayload,
        x: f32,
        y: f32,
        started: bool,
        click_count: u8,
    ) {
        self.drag = Some(PaneDrag {
            payload,
            origin: (x, y),
            pos: (x, y),
            started,
            external: started,
            click_count,
            plan: None,
            strip_drop: None,
            chip: PixelRect::ZERO,
        });
    }

    /// Whether a drag gesture is currently showing its drop preview.
    pub fn is_dragging(&self) -> bool {
        self.drag.as_ref().is_some_and(|d| d.started)
    }

    /// Current drop plan of an active drag (what releasing now would do).
    pub fn drag_plan(&self) -> Option<DropPlan> {
        self.drag.as_ref().and_then(|d| d.plan)
    }

    fn pane_file_name(&self, id: PaneId) -> &str {
        if id == self.active_id {
            return self.doc.file_name();
        }
        self.other_panes
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.doc.file_name())
            .unwrap_or("")
    }

    fn drag_labels(&self) -> (String, &'static str) {
        let Some(d) = self.drag.as_ref() else {
            return (String::new(), "");
        };
        let (name, is_pane) = match &d.payload {
            DragPayload::File(p) => (
                p.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| p.to_string_lossy().into_owned()),
                false,
            ),
            DragPayload::Tab(buf_id) => (
                self.working_set
                    .get_meta(*buf_id)
                    .map(|m| m.file_name.clone())
                    .unwrap_or_else(|| "Untitled".to_string()),
                false,
            ),
            DragPayload::Pane(id) => (self.pane_file_name(*id).to_string(), true),
        };
        if let Some((target, _)) = d.strip_drop {
            let action = match target {
                StripDropTarget::PinnedAt(_) => "Pin to strip",
                StripDropTarget::OpenAt(_) => "Move in strip",
            };
            return (name, action);
        }
        let action = match (d.plan.map(|p| p.zone), is_pane) {
            (None, _) => "Drop on a pane",
            (Some(DropZone::Center), false) => "Open here",
            (Some(DropZone::Left), false) => "Open to the left",
            (Some(DropZone::Right), false) => "Open to the right",
            (Some(DropZone::Top), false) => "Open above",
            (Some(DropZone::Bottom), false) => "Open below",
            (Some(DropZone::Center), true) => "Swap panes",
            (Some(DropZone::Left), true) => "Move to the left",
            (Some(DropZone::Right), true) => "Move to the right",
            (Some(DropZone::Top), true) => "Move above",
            (Some(DropZone::Bottom), true) => "Move below",
        };
        (name, action)
    }

    /// Floating label chip geometry (identical maths to `draw_drag_overlay`).
    fn drag_chip_rect(&mut self, x: f32, y: f32, rasterizer: &mut dyn GlyphRasterizer) -> PixelRect {
        let scale = self.framebuffer.scale;
        let (name, action) = self.drag_labels();
        let fs = DRAG_CHIP_FONT_PT * scale;
        let wn = Painter::measure_text_ui(&name, FontStyle::UiBold, fs, &mut self.atlas, rasterizer);
        let wa = Painter::measure_text_ui(action, FontStyle::Ui, fs, &mut self.atlas, rasterizer);
        let w = (wn + wa + (DRAG_CHIP_GAP_PT + 2.0 * DRAG_CHIP_PAD_PT) * scale).ceil() as i32;
        let h = (DRAG_CHIP_H_PT * scale).round() as i32;
        let b = self.framebuffer.bounds();
        let mut cx = (x + 14.0 * scale) as i32;
        let mut cy = (y + 16.0 * scale) as i32;
        if cx + w > b.right() {
            cx = (x - 10.0 * scale) as i32 - w;
        }
        if cy + h > b.bottom() {
            cy = (y - 10.0 * scale) as i32 - h;
        }
        PixelRect::new(cx.max(0), cy.max(0), w, h)
    }

    /// Track the pointer during a drag. Only the old ∪ new chip (and the preview, when the
    /// target changes) are damaged, so each frame repaints a few small rects.
    fn update_drag(&mut self, x: f32, y: f32, rasterizer: &mut dyn GlyphRasterizer, host: &mut dyn PlatformHost) {
        let scale = self.framebuffer.scale;
        let Some(d) = self.drag.as_mut() else { return };
        d.pos = (x, y);
        if !d.started {
            let (dx, dy) = (x - d.origin.0, y - d.origin.1);
            let thr = DRAG_THRESHOLD_PT * scale;
            if dx * dx + dy * dy < thr * thr {
                return;
            }
            d.started = true;
        }
        let can_drop_strip = matches!(d.payload, DragPayload::Tab(_) | DragPayload::File(_));
        let moving = match d.payload {
            DragPayload::Pane(id) => Some(id),
            DragPayload::File(_) | DragPayload::Tab(_) => None,
        };
        let old_plan = d.plan;
        let old_strip = d.strip_drop;
        let old_chip = d.chip;

        let strip_drop = if can_drop_strip {
            self.tab_strip.drop_target_at(x, y)
        } else {
            None
        };
        let plan = if strip_drop.is_some() {
            None
        } else {
            panes::plan_drop_at(
                &self.arrangement,
                &self.layout.grid,
                x.round() as i32,
                y.round() as i32,
                moving,
                scale,
            )
        };
        if let Some(d) = self.drag.as_mut() {
            d.plan = plan;
            d.strip_drop = strip_drop;
        }
        let chip = self.drag_chip_rect(x, y, rasterizer);
        if let Some(d) = self.drag.as_mut() {
            d.chip = chip;
        }
        if old_plan != plan {
            if let Some(p) = old_plan {
                self.mark_dirty_rect(p.preview);
            }
            if let Some(p) = plan {
                self.mark_dirty_rect(p.preview);
            }
        }
        if old_strip != strip_drop {
            self.mark_dirty_rect(self.layout.tab_strip);
        }
        self.mark_dirty_rect(old_chip);
        self.mark_dirty_rect(chip);
        host.set_cursor(CursorShape::Grabbing);
        host.request_redraw();
    }

    fn cancel_drag(&mut self, host: &mut dyn PlatformHost) {
        if let Some(d) = self.drag.take() {
            self.mark_dirty_rect(d.chip);
            if let Some(p) = d.plan {
                self.mark_dirty_rect(p.preview);
            }
            if d.strip_drop.is_some() {
                self.mark_dirty_rect(self.layout.tab_strip);
            }
            host.set_cursor(CursorShape::Arrow);
            host.request_redraw();
        }
    }

    fn finish_drag(&mut self, x: f32, y: f32, rasterizer: &mut dyn GlyphRasterizer, host: &mut dyn PlatformHost) {
        // Recompute at the release point so the outcome always equals the last preview.
        if self.is_dragging() {
            self.update_drag(x, y, rasterizer, host);
        }
        let Some(d) = self.drag.take() else { return };
        self.mark_dirty_rect(d.chip);
        if let Some(p) = d.plan {
            self.mark_dirty_rect(p.preview);
        }
        if d.strip_drop.is_some() {
            self.mark_dirty_rect(self.layout.tab_strip);
        }
        host.set_cursor(CursorShape::Arrow);
        if !d.started {
            // Plain click (below 4pt threshold):
            match d.payload {
                DragPayload::File(p) => {
                    let mode = if d.click_count >= 2 {
                        OpenMode::Keep
                    } else {
                        OpenMode::Preview
                    };
                    if let Err(e) = self.open_file_with_mode(&p, mode, host) {
                        self.set_notice(e, true);
                    } else {
                        self.sidebar.set_focused(false);
                    }
                }
                DragPayload::Tab(buf_id) => {
                    if d.click_count >= 2 {
                        self.working_set.graduate_preview(buf_id);
                    }
                    self.activate_buffer(buf_id, host);
                }
                DragPayload::Pane(_) => {}
            }
            host.request_redraw();
            return;
        }
        if let Some((strip_target, _)) = d.strip_drop {
            let buf_opt = match d.payload {
                DragPayload::Tab(id) => Some(id),
                DragPayload::File(p) => self.open_file_with_mode(&p, OpenMode::Keep, host).ok(),
                DragPayload::Pane(_) => None,
            };
            if let Some(buf_id) = buf_opt {
                match strip_target {
                    StripDropTarget::PinnedAt(idx) => {
                        if !self.working_set.move_to_pinned(buf_id, idx) {
                            self.set_notice(format!("Maximum of {MAX_PINS} pinned files"), false);
                        }
                    }
                    StripDropTarget::OpenAt(idx) => {
                        self.working_set.move_to_open(buf_id, idx);
                    }
                }
                self.mark_dirty();
            }
            host.request_redraw();
            return;
        }
        match d.plan {
            Some(plan) => {
                self.apply_drop(d.payload, plan, host);
            }
            None if d.external => {
                if let DragPayload::File(p) = d.payload {
                    if let Err(e) = self.open_path_or_dir(&p, host) {
                        self.set_notice(e, true);
                    }
                }
            }
            None => {}
        }
        host.request_redraw();
    }

    /// Execute a drop plan (shared by mouse drags, Finder drops and the CLI `drop`/`move-pane`).
    pub fn apply_drop(&mut self, payload: DragPayload, plan: DropPlan, host: &mut dyn PlatformHost) -> bool {
        match (payload, plan.zone) {
            (DragPayload::File(p), DropZone::Center) => {
                self.focus_pane_id(plan.target, host);
                if let Err(e) = self.open_file_with_mode(&p, OpenMode::Keep, host) {
                    self.set_notice(e, true);
                    return false;
                }
                true
            }
            (DragPayload::File(p), zone) => self.split_pane_at(plan.target, zone, Some(&p), host),
            (DragPayload::Tab(buf_id), DropZone::Center) => {
                self.focus_pane_id(plan.target, host);
                self.working_set.graduate_preview(buf_id);
                self.activate_buffer(buf_id, host)
            }
            (DragPayload::Tab(buf_id), zone) => self.split_buffer_at(plan.target, zone, buf_id, host),
            (DragPayload::Pane(src), zone) => self.move_pane(src, plan.target, zone, host),
        }
    }

    /// Plan a drop onto the pane at reading-order index `idx` with an explicit zone.
    pub fn plan_drop_on(&self, idx: usize, zone: DropZone, moving: Option<PaneId>) -> Option<DropPlan> {
        let target = self.arrangement.id_at_index(idx)?;
        panes::plan_drop(
            &self.arrangement,
            &self.layout.grid,
            target,
            zone,
            moving,
            self.framebuffer.scale,
        )
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
            PendingDiscardAction::Reload => {
                if let Err(e) = self.doc.reload(self.fs.as_ref()) {
                    self.set_notice(format!("Reload failed: {e}"), true);
                } else {
                    self.caret = self.caret.min(self.doc.len_bytes());
                    self.anchor = self.caret;
                    self.sync_active_buffer_meta();
                    self.refresh_search();
                    self.sync_title(host);
                    self.set_notice("Reloaded", false);
                }
                self.mark_dirty();
                host.request_redraw();
            }
            PendingDiscardAction::CloseBuffer(id) => {
                self.force_close_buffer(id, host);
            }
            PendingDiscardAction::Quit => {
                // If discarding the active dirty buffer during quit, close it and check any other dirty buffers!
                if self.doc.is_dirty() {
                    let cur = self.active_buf_id;
                    self.force_close_buffer(cur, host);
                }
                if let Some(next_dirty) = self.first_dirty_buffer() {
                    self.activate_buffer(next_dirty, host);
                    self.request_discard_or_run(PendingDiscardAction::Quit, host);
                } else {
                    host.request_quit();
                }
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
                // Saving NEVER removes a Kept tab from `open` or `pinned`!
                self.sync_active_buffer_meta();
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
                self.sync_active_buffer_meta();
                self.working_set.graduate_preview(self.active_buf_id);
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

    fn open_at_location(
        &mut self,
        path: &Path,
        line_0based: Option<usize>,
        col_bytes: Option<usize>,
        rasterizer: &mut dyn GlyphRasterizer,
        host: &mut dyn PlatformHost,
    ) {
        let res = self.open_file_with_mode(path, OpenMode::Keep, host);
        match res {
            Ok(_) => {
                self.sidebar.set_focused(false);
                if let Some(line) = line_0based {
                    self.doc.tree_mut().ensure_indexed_for_line(line + 16);
                    let clamped = line.min(self.doc.editable_line_count().saturating_sub(1));
                    if let Some((start, content_end, _)) = self.doc.line_byte_range(clamped) {
                        let off = start + (col_bytes.unwrap_or(0) as u64).min(content_end - start);
                        self.caret = off;
                        self.anchor = off;
                        self.viewport.scroll_top_line = clamped.saturating_sub(6);
                        self.viewport.scroll_sub_y_px = 0.0;
                    }
                    let m = self.doc_metrics(rasterizer);
                    self.viewport
                        .ensure_caret_visible(&mut self.doc, self.caret, m, self.framebuffer.scale);
                }
                self.mark_dirty();
                host.request_redraw();
            }
            Err(e) => self.set_notice(e, true),
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
                self.open_at_location(&path, line_0based, col_bytes, rasterizer, host);
            }
        }
    }

    fn handle_harpoon_action(&mut self, act: HarpoonAction, host: &mut dyn PlatformHost) {
        match act {
            HarpoonAction::None => {}
            HarpoonAction::Changed | HarpoonAction::Close => {
                self.mark_dirty();
                host.request_redraw();
            }
            HarpoonAction::JumpToBuffer { id, split_right } => {
                if split_right {
                    let a = self.active_id;
                    let zone = if self.arrangement.can_insert(a, DropZone::Right) {
                        DropZone::Right
                    } else {
                        DropZone::Bottom
                    };
                    self.split_buffer_at(a, zone, id, host);
                } else {
                    self.activate_buffer(id, host);
                }
            }
            HarpoonAction::JumpPin(slot) => {
                self.jump_to_pin(slot, host);
            }
            HarpoonAction::MovePin { slot_0based, delta } => {
                let id_before = self.working_set.pinned.get(slot_0based).copied();
                if self.working_set.nudge_pin(slot_0based, delta) {
                    self.refresh_harpoon_overlay(id_before);
                    self.mark_dirty();
                    host.request_redraw();
                }
            }
            HarpoonAction::TogglePin(id) => {
                self.toggle_pin_buffer(id, host);
                self.refresh_harpoon_overlay(Some(id));
                self.mark_dirty();
                host.request_redraw();
            }
            HarpoonAction::RemoveOrClose(id) => {
                if self.working_set.is_pinned(id) {
                    // In Harpoon modal, `d` on a pinned slot unpins it cleanly without losing the file.
                    let _ = self.working_set.toggle_pin(id);
                    self.refresh_harpoon_overlay(None);
                    self.mark_dirty();
                    host.request_redraw();
                } else {
                    if self.is_buffer_dirty(id) {
                        self.harpoon.close();
                        self.request_close_buffer(id, host);
                    } else {
                        self.force_close_buffer(id, host);
                        self.refresh_harpoon_overlay(None);
                        self.mark_dirty();
                        host.request_redraw();
                    }
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
            AppCommand::NewFile | AppCommand::NewTab => {
                self.new_tab(host);
            }
            AppCommand::CloseTab => {
                if self.is_split() && !self.zoomed {
                    self.request_close_active_pane(host);
                } else {
                    let cur = self.active_buf_id;
                    self.request_close_buffer(cur, host);
                }
            }
            AppCommand::OpenFile => {
                if let Some(path) = host.open_file_dialog() {
                    if let Err(e) = self.open_file(&path, host) {
                        self.set_notice(e, true);
                    }
                }
            }
            AppCommand::OpenFolder => {
                if let Some(dir) = host.open_folder_dialog() {
                    self.sidebar.set_visible(true);
                    self.sidebar.set_root(self.fs.as_ref(), &dir);
                    self.save_preferences(host);
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
                self.save_preferences(host);
                self.recompute_layout();
                self.mark_dirty();
                host.request_redraw();
            }
            AppCommand::SplitPane => {
                self.split_pane_with(None, host);
            }
            AppCommand::SplitPaneDown => {
                self.split_pane_down(None, host);
            }
            AppCommand::CloseSplit => {
                self.request_close_active_pane(host);
            }
            AppCommand::OnlyActivePane => {
                self.only_active_pane(host);
            }
            AppCommand::ToggleZoomPane => {
                self.toggle_zoom_pane(host);
            }
            AppCommand::TogglePin => {
                let id = self.active_buf_id;
                self.toggle_pin_buffer(id, host);
            }
            AppCommand::JumpPin(slot) => {
                self.jump_to_pin(slot, host);
            }
            AppCommand::ShowHarpoon => {
                if self.harpoon.visible {
                    self.harpoon.close();
                } else {
                    self.palette.close();
                    self.shortcuts.close();
                    self.goto_line.close();
                    self.refresh_harpoon_overlay(None);
                }
                self.mark_dirty();
                host.request_redraw();
            }
            AppCommand::NextTab => {
                self.step_tab(1, host);
            }
            AppCommand::PrevTab => {
                self.step_tab(-1, host);
            }
            AppCommand::ToggleLastBuffer => {
                self.toggle_last_buffer(host);
            }
            AppCommand::SwapPanes => {
                self.swap_panes(host);
            }
            AppCommand::FocusLeftPane => self.focus_direction(Direction::Left, host),
            AppCommand::FocusRightPane => self.focus_direction(Direction::Right, host),
            AppCommand::FocusUpPane => self.focus_direction(Direction::Up, host),
            AppCommand::FocusDownPane => self.focus_direction(Direction::Down, host),
            AppCommand::FocusNextPane => self.focus_next_pane(host),
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
            AppCommand::ToggleSyntaxStyle => {
                let next = match self.syntax_theme.preset {
                    SyntaxThemePreset::Monochrome => SyntaxThemePreset::Editorial,
                    _ => SyntaxThemePreset::Monochrome,
                };
                self.set_syntax_style(next, host);
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
                if let Some(dirty_id) = self.first_dirty_buffer() {
                    self.activate_buffer(dirty_id, host);
                    self.request_discard_or_run(PendingDiscardAction::Quit, host);
                } else {
                    host.request_quit();
                }
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
            let root_before = self.sidebar.root().map(Path::to_path_buf);
            match self
                .sidebar
                .handle_key(self.fs.as_ref(), key, self.layout.sidebar.h)
            {
                SidebarAction::OpenFile(p) => {
                    if let Err(e) = self.open_file_with_mode(&p, OpenMode::Keep, host) {
                        self.set_notice(e, true);
                    } else {
                        self.sidebar.set_focused(false);
                    }
                }
                SidebarAction::Changed => {
                    if self.sidebar.root() != root_before.as_deref() {
                        self.persist_last_folder(host);
                    }
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
        let prefix_trim = self.doc.language().line_comment().unwrap_or("//");
        let prefix_owned = format!("{prefix_trim} ");
        let prefix = prefix_owned.as_str();
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
        self.sync_active_buffer_meta();
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
                let root_before = self.sidebar.root().map(Path::to_path_buf);
                match self
                    .sidebar
                    .handle_click(self.fs.as_ref(), self.layout.sidebar, x_px, y_px, scale)
                {
                    SidebarAction::OpenFile(p) => {
                        // Start a drag candidate: if released within 4pt it opens in Preview
                        // (single-click) or Kept (double-click); dragging beyond 4pt shows the drop preview!
                        self.begin_drag(DragPayload::File(p), x_px, y_px, false, click_count);
                        self.mark_dirty_rect(self.layout.sidebar);
                        host.request_redraw();
                    }
                    SidebarAction::Changed => {
                        if self.sidebar.root() != root_before.as_deref() {
                            self.persist_last_folder(host);
                        }
                        self.mark_dirty();
                        host.request_redraw();
                    }
                    SidebarAction::None => {}
                }
                return;
            }
        }

        // 1b. Unified Top Working-Set Strip (tabs, close `×`, `+`, zoom toggle, Harpoon menu).
        if self.layout.tab_strip.contains(xi, yi) {
            self.sidebar.set_focused(false);
            self.find_bar.find_input.focused = false;
            self.find_bar.replace_input.focused = false;
            match self.tab_strip.hit_test(x_px, y_px) {
                TabStripHit::Tab(buf_id) => {
                    self.begin_drag(DragPayload::Tab(buf_id), x_px, y_px, false, click_count);
                }
                TabStripHit::CloseTab(buf_id) => {
                    self.request_close_buffer(buf_id, host);
                }
                TabStripHit::NewTab => {
                    self.new_tab(host);
                }
                TabStripHit::ToggleZoom => {
                    self.toggle_zoom_pane(host);
                }
                TabStripHit::OpenMenu => {
                    self.refresh_harpoon_overlay(None);
                    self.mark_dirty();
                    host.request_redraw();
                }
                TabStripHit::None => {
                    if click_count >= 2 {
                        self.new_tab(host);
                    }
                }
            }
            return;
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

        // 3. Pane grid divider drag, pane header close (`×`) / double-click zoom / drag, or focus switch!
        if !self.other_panes.is_empty() && !self.zoomed {
            let grab = (5.0 * scale).round() as i32;
            if let Some(div) = self.layout.grid.divider_at(xi, yi, grab) {
                self.dragging_divider = Some(div);
                let cur = match div.axis {
                    DividerAxis::Columns => CursorShape::ResizeHorizontal,
                    DividerAxis::Rows(_) => CursorShape::ResizeVertical,
                };
                host.set_cursor(cur);
                return;
            }
            let close_w = (24.0 * scale).round() as i32;
            if let Some(pr) = self.layout.grid.pane_at(xi, yi).copied() {
                if pr.header.contains(xi, yi) {
                    let close_rect = PixelRect::new(
                        pr.header.right() - close_w,
                        pr.header.y,
                        close_w,
                        pr.header.h,
                    );
                    if close_rect.contains(xi, yi) {
                        self.focus_pane_id(pr.id, host);
                        self.request_close_active_pane(host);
                        return;
                    }
                    if click_count >= 2 {
                        self.focus_pane_id(pr.id, host);
                        self.toggle_zoom_pane(host);
                        return;
                    }
                    self.focus_pane_id(pr.id, host);
                    self.begin_drag(DragPayload::Pane(pr.id), x_px, y_px, false, click_count);
                    return;
                }
                if pr.id != self.active_id {
                    self.focus_pane_id(pr.id, host);
                }
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
        if self.drag.is_some() {
            self.update_drag(x_px, y_px, rasterizer, host);
            return;
        }
        if self.dragging_sidebar_edge {
            self.sidebar.set_width_pt(x_px / scale);
            self.recompute_layout();
            self.mark_dirty();
            host.request_redraw();
            return;
        }
        if let Some(div) = self.dragging_divider {
            self.arrangement.drag_divider(&div, x_px, y_px);
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

        // Unconditional single-pass hover reconciliation across ALL hoverable surfaces
        // (sidebar, tab strip, scrollbars). Never early-return before reconciling every surface,
        // so moving between regions or out of the window can NEVER leave stale hover pixels on
        // a swapchain back-buffer!
        let xi = x_px.round() as i32;
        let yi = y_px.round() as i32;
        let overlay_open = self.confirm_modal.visible
            || self.palette.is_open()
            || self.shortcuts.visible
            || self.harpoon.visible;
        let mut needs_redraw = false;

        // 1. Sidebar hover (always reconciled so exiting to ResizeEdge, editor, or outside invalidates!)
        let sb_hit = if !overlay_open && self.sidebar.is_visible() {
            let (changed, hit) = self
                .sidebar
                .set_hovered(self.layout.sidebar, x_px, y_px, scale);
            if changed {
                self.mark_dirty_rect(self.layout.sidebar);
                needs_redraw = true;
            }
            hit
        } else {
            if self.sidebar.clear_hover() {
                self.mark_dirty_rect(self.layout.sidebar);
                needs_redraw = true;
            }
            SidebarHit::None
        };

        // 2. Working-Set Tab Strip hover
        let in_tab_strip =
            !overlay_open && sb_hit == SidebarHit::None && self.layout.tab_strip.contains(xi, yi);
        let tab_hit = if in_tab_strip {
            let (changed, hit) = self.tab_strip.set_hovered(x_px, y_px);
            if changed {
                self.mark_dirty_rect(self.layout.tab_strip);
                needs_redraw = true;
            }
            hit
        } else {
            if self.tab_strip.clear_hover() {
                self.mark_dirty_rect(self.layout.tab_strip);
                needs_redraw = true;
            }
            TabStripHit::None
        };

        // 3. Active viewport scrollbar hover
        let sb_scroll = if !overlay_open && sb_hit == SidebarHit::None && !in_tab_strip {
            self.viewport.hit_test_scrollbar(x_px, y_px)
        } else {
            ScrollbarHit::None
        };
        let prev_vh = self.viewport.v_bar_hovered;
        let prev_hh = self.viewport.h_bar_hovered;
        self.viewport.v_bar_hovered =
            matches!(sb_scroll, ScrollbarHit::VerticalThumb | ScrollbarHit::VerticalTrack);
        self.viewport.h_bar_hovered = matches!(
            sb_scroll,
            ScrollbarHit::HorizontalThumb | ScrollbarHit::HorizontalTrack
        );
        if prev_vh != self.viewport.v_bar_hovered || prev_hh != self.viewport.h_bar_hovered {
            self.mark_dirty_rect(self.layout.viewport);
            needs_redraw = true;
        }

        // 4. Resolve cursor shape cleanly from active hit region.
        let cursor = if overlay_open {
            CursorShape::Arrow
        } else if sb_hit == SidebarHit::ResizeEdge {
            CursorShape::ResizeHorizontal
        } else if sb_hit != SidebarHit::None {
            CursorShape::Pointer
        } else if in_tab_strip {
            if tab_hit != TabStripHit::None {
                CursorShape::Pointer
            } else {
                CursorShape::Arrow
            }
        } else if let Some(div) = (!self.other_panes.is_empty() && !self.zoomed)
            .then(|| self.layout.grid.divider_at(xi, yi, (5.0 * scale).round() as i32))
            .flatten()
        {
            match div.axis {
                DividerAxis::Columns => CursorShape::ResizeHorizontal,
                DividerAxis::Rows(_) => CursorShape::ResizeVertical,
            }
        } else if !self.other_panes.is_empty()
            && !self.zoomed
            && self
                .layout
                .grid
                .pane_at(xi, yi)
                .is_some_and(|pr| pr.header.contains(xi, yi))
        {
            CursorShape::Pointer
        } else if sb_scroll != ScrollbarHit::None {
            CursorShape::Arrow
        } else if self.viewport.layout.text.contains(xi, yi)
            || self
                .other_panes
                .iter()
                .any(|p| p.viewport.layout.text.contains(xi, yi))
        {
            CursorShape::IBeam
        } else {
            CursorShape::Arrow
        };
        host.set_cursor(cursor);

        if needs_redraw {
            host.request_redraw();
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
        if self.layout.tab_strip.contains(xi, yi) {
            let delta = if dx_px.abs() > 0.1 { dx_px } else { dy_px };
            self.tab_strip.scroll_by(delta);
            self.mark_dirty_rect(self.layout.tab_strip);
            host.request_redraw();
            return;
        }
        if !self.other_panes.is_empty() && !self.zoomed {
            if let Some(pr) = self.layout.grid.pane_at(xi, yi).copied() {
                if pr.id != self.active_id {
                    if let Some(sec) = self.other_panes.iter_mut().find(|p| p.id == pr.id) {
                        sec.viewport
                            .scroll_by(dx_px, dy_px, &mut sec.doc, metrics, self.framebuffer.scale);
                        self.mark_dirty_rect(pr.body);
                        host.request_redraw();
                        return;
                    }
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

        let (ordered_ids, ordered_n) = self.arrangement.ordered();
        let mut panes_json = String::from("[");
        for (idx, &pid) in ordered_ids[..ordered_n].iter().enumerate() {
            if idx > 0 {
                panes_json.push(',');
            }
            let slot = self.arrangement.slot_of(pid).unwrap_or_default();
            let (p_json, fname, dirty) = if pid == self.active_id {
                (path_json.clone(), self.doc.file_name(), self.doc.is_dirty())
            } else if let Some(sec) = self.other_panes.iter().find(|p| p.id == pid) {
                let sp = match sec.doc.path() {
                    Some(p) => json_quote(&p.to_string_lossy()),
                    None => "null".to_string(),
                };
                (sp, sec.doc.file_name(), sec.doc.is_dirty())
            } else {
                ("null".to_string(), "Untitled", false)
            };
            panes_json.push_str(&format!(
                "{{\"index\":{idx},\"col\":{},\"row\":{},\"path\":{},\"file_name\":{},\"dirty\":{}}}",
                slot.col,
                slot.row,
                p_json,
                json_quote(fname),
                dirty
            ));
        }
        panes_json.push(']');

        let mut pins_json = String::from("[");
        for (i, &bid) in self.working_set.pinned.iter().enumerate() {
            if i > 0 {
                pins_json.push(',');
            }
            if let Some(m) = self.working_set.get_meta(bid) {
                let p_json = match &m.path {
                    Some(p) => json_quote(&p.to_string_lossy()),
                    None => "null".to_string(),
                };
                pins_json.push_str(&format!(
                    "{{\"slot\":{},\"id\":{bid},\"path\":{},\"file_name\":{},\"dirty\":{}}}",
                    i + 1,
                    p_json,
                    json_quote(&m.file_name),
                    self.is_buffer_dirty(bid)
                ));
            }
        }
        pins_json.push(']');

        let mut buffers_json = String::from("[");
        for (i, bid) in self.working_set.ordered_ids().into_iter().enumerate() {
            if i > 0 {
                buffers_json.push(',');
            }
            if let Some(m) = self.working_set.get_meta(bid) {
                let p_json = match &m.path {
                    Some(p) => json_quote(&p.to_string_lossy()),
                    None => "null".to_string(),
                };
                let pin_json = match self.working_set.pin_slot(bid) {
                    Some(s) => s.to_string(),
                    None => "null".to_string(),
                };
                buffers_json.push_str(&format!(
                    "{{\"id\":{bid},\"path\":{},\"file_name\":{},\"dirty\":{},\"pin_slot\":{},\"preview\":{},\"active\":{}}}",
                    p_json,
                    json_quote(&m.file_name),
                    self.is_buffer_dirty(bid),
                    pin_json,
                    self.working_set.is_preview(bid),
                    bid == self.active_buf_id
                ));
            }
        }
        buffers_json.push(']');

        format!(
            concat!(
                "{{\"running\":true,\"path\":{},\"file_name\":{},\"dirty\":{},",
                "\"split\":{},\"zoomed\":{},\"active_pane\":{},\"panes\":{},",
                "\"pins\":{},\"buffers\":{},\"harpoon_open\":{},",
                "\"bytes\":{},\"line_count\":{},\"eol\":{},\"language\":{},\"language_id\":{},",
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
                "\"view\":{{\"word_wrap\":{},\"default_word_wrap\":{},\"word_wrap_by_ext\":{},\"font_size_pt\":{:.1},\"theme_preference\":{},\"active_theme\":{},\"syntax_style\":{},\"syntax_ext_map\":{}}}}}"
            ),
            path_json,
            json_quote(self.doc.file_name()),
            self.doc.is_dirty(),
            self.is_split(),
            self.zoomed,
            self.active_pane_index(),
            panes_json,
            pins_json,
            buffers_json,
            self.harpoon.visible,
            self.doc.len_bytes(),
            line_count,
            json_quote(self.doc.eol().as_str()),
            json_quote(self.doc.language().display_name()),
            json_quote(self.doc.language().id()),
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
            json_quote(self.syntax_theme.preset.as_str()),
            str_map_json(&self.syntax_ext_map),
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
                self.save_preferences(host);
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
                        self.sync_active_buffer_meta();
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
                    "new-tab" => self.handle_command(AppCommand::NewTab, rasterizer, clipboard, host),
                    "close-tab" => self.handle_command(AppCommand::CloseTab, rasterizer, clipboard, host),
                    "pin" | "pin-file" | "toggle-pin" => {
                        self.handle_command(AppCommand::TogglePin, rasterizer, clipboard, host)
                    }
                    "unpin" => {
                        let id = self.active_buf_id;
                        if self.working_set.is_pinned(id) {
                            self.toggle_pin_buffer(id, host);
                        }
                    }
                    "harpoon" => {
                        activate = true;
                        self.handle_command(AppCommand::ShowHarpoon, rasterizer, clipboard, host);
                    }
                    "next-tab" => self.handle_command(AppCommand::NextTab, rasterizer, clipboard, host),
                    "prev-tab" => self.handle_command(AppCommand::PrevTab, rasterizer, clipboard, host),
                    "last-buffer" => {
                        self.handle_command(AppCommand::ToggleLastBuffer, rasterizer, clipboard, host)
                    }
                    "zoom" | "zoom-pane" | "toggle-zoom" => {
                        self.handle_command(AppCommand::ToggleZoomPane, rasterizer, clipboard, host)
                    }
                    "only-pane" => {
                        self.handle_command(AppCommand::OnlyActivePane, rasterizer, clipboard, host)
                    }
                    "jump-pin-1" => self.handle_command(AppCommand::JumpPin(1), rasterizer, clipboard, host),
                    "jump-pin-2" => self.handle_command(AppCommand::JumpPin(2), rasterizer, clipboard, host),
                    "jump-pin-3" => self.handle_command(AppCommand::JumpPin(3), rasterizer, clipboard, host),
                    "jump-pin-4" => self.handle_command(AppCommand::JumpPin(4), rasterizer, clipboard, host),
                    "jump-pin-5" => self.handle_command(AppCommand::JumpPin(5), rasterizer, clipboard, host),
                    "jump-pin-6" => self.handle_command(AppCommand::JumpPin(6), rasterizer, clipboard, host),
                    "jump-pin-7" => self.handle_command(AppCommand::JumpPin(7), rasterizer, clipboard, host),
                    "jump-pin-8" => self.handle_command(AppCommand::JumpPin(8), rasterizer, clipboard, host),
                    "jump-pin-9" => self.handle_command(AppCommand::JumpPin(9), rasterizer, clipboard, host),
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
                        self.save_preferences(host);
                        self.recompute_layout();
                        self.mark_dirty();
                        host.request_redraw();
                    }
                    "sidebar-hide" => {
                        self.sidebar.set_visible(false);
                        self.save_preferences(host);
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
                    "split" | "split-right" => {
                        self.handle_command(AppCommand::SplitPane, rasterizer, clipboard, host)
                    }
                    "split-down" => {
                        self.handle_command(AppCommand::SplitPaneDown, rasterizer, clipboard, host)
                    }
                    "unsplit" | "close-pane" => {
                        self.handle_command(AppCommand::CloseSplit, rasterizer, clipboard, host)
                    }
                    "swap-panes" => self.handle_command(AppCommand::SwapPanes, rasterizer, clipboard, host),
                    "focus-left" => self.handle_command(AppCommand::FocusLeftPane, rasterizer, clipboard, host),
                    "focus-right" => self.handle_command(AppCommand::FocusRightPane, rasterizer, clipboard, host),
                    "focus-up" => self.handle_command(AppCommand::FocusUpPane, rasterizer, clipboard, host),
                    "focus-down" => self.handle_command(AppCommand::FocusDownPane, rasterizer, clipboard, host),
                    "focus-next" => self.handle_command(AppCommand::FocusNextPane, rasterizer, clipboard, host),
                    "find-next" => self.handle_command(AppCommand::FindNext, rasterizer, clipboard, host),
                    "find-prev" => self.handle_command(AppCommand::FindPrev, rasterizer, clipboard, host),
                    "move-line-up" => self.handle_command(AppCommand::MoveLineUp, rasterizer, clipboard, host),
                    "move-line-down" => self.handle_command(AppCommand::MoveLineDown, rasterizer, clipboard, host),
                    "open-dialog" => self.handle_command(AppCommand::OpenFile, rasterizer, clipboard, host),
                    "open-folder" => self.handle_command(AppCommand::OpenFolder, rasterizer, clipboard, host),
                    "save-as" => self.handle_command(AppCommand::SaveAs, rasterizer, clipboard, host),
                    "quit" => self.handle_command(AppCommand::Quit, rasterizer, clipboard, host),
                    "focus" => activate = true,
                    other if other.starts_with("pin:") => {
                        let p = PathBuf::from(&other["pin:".len()..]);
                        match self.open_file_with_mode(&p, OpenMode::Keep, host) {
                            Ok(id) => {
                                if !self.working_set.is_pinned(id) {
                                    self.toggle_pin_buffer(id, host);
                                }
                            }
                            Err(e) => return (IpcReply::Error(e), false, false),
                        }
                    }
                    other if other.starts_with("unpin:") => {
                        let arg = &other["unpin:".len()..];
                        if let Ok(slot) = arg.parse::<usize>() {
                            if slot >= 1 && slot <= self.working_set.pinned.len() {
                                let id = self.working_set.pinned[slot - 1];
                                self.toggle_pin_buffer(id, host);
                            } else {
                                return (
                                    IpcReply::Error(format!("no pinned file in slot {slot}")),
                                    false,
                                    false,
                                );
                            }
                        } else {
                            let canonical = self
                                .fs
                                .canonicalize(Path::new(arg))
                                .unwrap_or_else(|_| PathBuf::from(arg));
                            if let Some(id) = self.working_set.find_by_path(&canonical) {
                                if self.working_set.is_pinned(id) {
                                    self.toggle_pin_buffer(id, host);
                                }
                            } else {
                                return (
                                    IpcReply::Error(format!("file not pinned: {arg}")),
                                    false,
                                    false,
                                );
                            }
                        }
                    }
                    other if other.starts_with("jump-pin:") => {
                        let arg = &other["jump-pin:".len()..];
                        match arg.parse::<u8>() {
                            Ok(slot @ 1..=9) => {
                                self.jump_to_pin(slot, host);
                            }
                            _ => {
                                return (
                                    IpcReply::Error(format!("pin slot must be 1..9, got `{arg}`")),
                                    false,
                                    false,
                                );
                            }
                        }
                    }
                    other if other.starts_with("split-down:") => {
                        let p = PathBuf::from(&other["split-down:".len()..]);
                        self.split_pane_down(Some(&p), host);
                        activate = true;
                    }
                    other if other.starts_with("drop:") => {
                        // drop:<target_idx>:<zone>:<path>
                        let rest = &other["drop:".len()..];
                        let mut parts = rest.splitn(3, ':');
                        let (Some(t_str), Some(z_str), Some(p_str)) =
                            (parts.next(), parts.next(), parts.next())
                        else {
                            return (
                                IpcReply::Error("usage: drop:<pane_idx>:<zone>:<path>".to_string()),
                                false,
                                false,
                            );
                        };
                        let Ok(target_idx) = t_str.parse::<usize>() else {
                            return (
                                IpcReply::Error(format!("invalid pane index `{t_str}`")),
                                false,
                                false,
                            );
                        };
                        let Some(zone) = DropZone::parse(z_str) else {
                            return (
                                IpcReply::Error(format!(
                                    "invalid drop zone `{z_str}` (use center|left|right|top|bottom)"
                                )),
                                false,
                                false,
                            );
                        };
                        let Some(plan) = self.plan_drop_on(target_idx, zone, None) else {
                            return (
                                IpcReply::Error(format!("pane index {target_idx} out of range")),
                                false,
                                false,
                            );
                        };
                        self.apply_drop(DragPayload::File(PathBuf::from(p_str)), plan, host);
                        activate = true;
                    }
                    other if other.starts_with("move-pane:") => {
                        // move-pane:<src_idx>:<target_idx>:<zone>
                        let rest = &other["move-pane:".len()..];
                        let mut parts = rest.splitn(3, ':');
                        let (Some(s_str), Some(t_str), Some(z_str)) =
                            (parts.next(), parts.next(), parts.next())
                        else {
                            return (
                                IpcReply::Error(
                                    "usage: move-pane:<src_idx>:<target_idx>:<zone>".to_string(),
                                ),
                                false,
                                false,
                            );
                        };
                        let (Ok(src_idx), Ok(target_idx)) =
                            (s_str.parse::<usize>(), t_str.parse::<usize>())
                        else {
                            return (
                                IpcReply::Error("invalid pane index".to_string()),
                                false,
                                false,
                            );
                        };
                        let Some(zone) = DropZone::parse(z_str) else {
                            return (
                                IpcReply::Error(format!(
                                    "invalid drop zone `{z_str}` (use center|left|right|top|bottom)"
                                )),
                                false,
                                false,
                            );
                        };
                        let Some(src_id) = self.arrangement.id_at_index(src_idx) else {
                            return (
                                IpcReply::Error(format!("source pane {src_idx} out of range")),
                                false,
                                false,
                            );
                        };
                        let Some(plan) = self.plan_drop_on(target_idx, zone, Some(src_id)) else {
                            return (
                                IpcReply::Error("illegal pane move".to_string()),
                                false,
                                false,
                            );
                        };
                        self.apply_drop(DragPayload::Pane(src_id), plan, host);
                    }
                    "toggle-syntax-style" => {
                        self.handle_command(
                            AppCommand::ToggleSyntaxStyle,
                            rasterizer,
                            clipboard,
                            host,
                        );
                    }
                    "syntax-list" => {
                        return (IpcReply::Json(Self::languages_json()), false, false);
                    }
                    "syntax-theme" => {
                        return (IpcReply::Json(self.syntax_theme_json()), false, false);
                    }
                    other if other.starts_with("syntax-set:") => {
                        let lang_id = &other["syntax-set:".len()..];
                        if let Err(e) = self.set_active_language(lang_id, host) {
                            return (IpcReply::Error(e), false, false);
                        }
                    }
                    other if other.starts_with("syntax-style:") || other.starts_with("syntax-theme-preset:") => {
                        let val = other
                            .strip_prefix("syntax-style:")
                            .or_else(|| other.strip_prefix("syntax-theme-preset:"))
                            .unwrap_or("");
                        match SyntaxThemePreset::parse(val) {
                            Some(style) => self.set_syntax_style(style, host),
                            None => {
                                return (
                                    IpcReply::Error(format!(
                                        "invalid syntax preset `{val}` (use editorial|monochrome|studio|warm)"
                                    )),
                                    false,
                                    false,
                                );
                            }
                        }
                        return (IpcReply::Json(self.syntax_theme_json()), false, false);
                    }
                    other if other.starts_with("syntax-theme-json:") => {
                        let json = &other["syntax-theme-json:".len()..];
                        if let Err(e) = self.apply_syntax_theme_json(json, host) {
                            return (IpcReply::Error(e), false, false);
                        }
                        return (IpcReply::Json(self.syntax_theme_json()), false, false);
                    }
                    other if other.starts_with("syntax-color:") => {
                        // syntax-color:<mode>:<token>:<color_or_reset>:<style_or_keep>
                        let rest = &other["syntax-color:".len()..];
                        let parts: Vec<&str> = rest.splitn(4, ':').collect();
                        if parts.len() < 3 {
                            return (
                                IpcReply::Error(
                                    "usage: syntax-color:<mode>:<token>:<#RRGGBB|reset>[:<normal|bold|italic|keep>]"
                                        .to_string(),
                                ),
                                false,
                                false,
                            );
                        }
                        let style_spec = parts.get(3).copied().unwrap_or("keep");
                        if let Err(e) =
                            self.set_syntax_token_override(parts[0], parts[1], parts[2], style_spec, host)
                        {
                            return (IpcReply::Error(e), false, false);
                        }
                        return (IpcReply::Json(self.syntax_theme_json()), false, false);
                    }
                    other if other.starts_with("syntax-reset:") => {
                        let mode = &other["syntax-reset:".len()..];
                        if let Err(e) = self.reset_syntax_overrides(mode, host) {
                            return (IpcReply::Error(e), false, false);
                        }
                        return (IpcReply::Json(self.syntax_theme_json()), false, false);
                    }
                    other if other.starts_with("syntax-map:") => {
                        let spec = &other["syntax-map:".len()..];
                        if let Some((ext, val)) = spec.split_once('=') {
                            let val_trim = val.trim();
                            let lang_opt = match val_trim.to_ascii_lowercase().as_str() {
                                "default" | "reset" | "none" | "" => None,
                                _ => Some(val_trim),
                            };
                            if let Err(e) = self.set_syntax_ext_map(ext, lang_opt, host) {
                                return (IpcReply::Error(e), false, false);
                            }
                        } else {
                            return (
                                IpcReply::Error(
                                    "usage: syntax-map:<ext>=<language_id|default>".to_string(),
                                ),
                                false,
                                false,
                            );
                        }
                    }
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
                if *force || !self.any_buffer_dirty() {
                    (IpcReply::Quitting, false, true)
                } else {
                    self.handle_command(AppCommand::Quit, rasterizer, clipboard, host);
                    (IpcReply::Prompting, true, false)
                }
            }
        }
    }
}

const DRAG_THRESHOLD_PT: f32 = 4.0;
const DRAG_CHIP_FONT_PT: f32 = 11.5;
const DRAG_CHIP_H_PT: f32 = 26.0;
const DRAG_CHIP_PAD_PT: f32 = 10.0;
const DRAG_CHIP_GAP_PT: f32 = 8.0;

#[allow(clippy::too_many_arguments)]
fn draw_drag_overlay(
    painter: &mut Painter,
    preview: Option<PixelRect>,
    chip: PixelRect,
    name: &str,
    action: &str,
    theme: &Theme,
    atlas: &mut GlyphAtlas,
    rasterizer: &mut dyn GlyphRasterizer,
) {
    let scale = painter.scale();
    let hairline = (1.0 * scale).round().max(1.0) as i32;
    let clip = painter.current_clip();

    if let Some(p) = preview {
        let inset = (6.0 * scale).round() as i32;
        let box_rect = PixelRect::new(
            p.x + inset,
            p.y + inset,
            (p.w - 2 * inset).max(0),
            (p.h - 2 * inset).max(0),
        );
        if !box_rect.is_empty() && !box_rect.intersect(clip).is_empty() {
            let radius = 6.0 * scale;
            let fill = theme.selection.with_alpha(if theme.is_dark { 145 } else { 155 });
            painter.fill_rounded_rect(box_rect, radius, fill);
            painter.stroke_rounded_rect(
                box_rect,
                radius,
                hairline as f32,
                theme.border_control,
            );
        }
    }

    if !chip.is_empty() && !chip.intersect(clip).is_empty() {
        let radius = 6.0 * scale;
        painter.fill_rounded_rect(chip, radius, theme.page.with_alpha(242));
        painter.stroke_rounded_rect(chip, radius, hairline as f32, theme.border_control);

        let fs = DRAG_CHIP_FONT_PT * scale;
        let m_bold = atlas.metrics(rasterizer, FontStyle::UiBold, fs);
        let baseline = chip.y as f32 + (chip.h as f32 + m_bold.ascent - m_bold.descent) * 0.5;
        let x0 = chip.x as f32 + DRAG_CHIP_PAD_PT * scale;
        let pen_after_name = painter.draw_text_ui(
            x0,
            baseline,
            name,
            FontStyle::UiBold,
            fs,
            theme.text,
            atlas,
            rasterizer,
        );
        painter.draw_text_ui(
            pen_after_name + DRAG_CHIP_GAP_PT * scale,
            baseline,
            action,
            FontStyle::Ui,
            fs,
            theme.text_muted,
            atlas,
            rasterizer,
        );
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

fn parse_str_map(raw: &str) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    for pair in raw.split(',') {
        let pair = pair.trim();
        if let Some((k, v)) = pair.split_once('=') {
            let ext = k.trim().trim_start_matches('.').to_ascii_lowercase();
            let val = v.trim().to_ascii_lowercase();
            if !ext.is_empty() && !val.is_empty() {
                m.insert(ext, val);
            }
        }
    }
    m
}

fn format_str_map(map: &BTreeMap<String, String>) -> String {
    let mut out = String::new();
    for (idx, (k, v)) in map.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        out.push_str(k);
        out.push('=');
        out.push_str(v);
    }
    out
}

fn str_map_json(map: &BTreeMap<String, String>) -> String {
    let mut out = String::from("{");
    for (idx, (k, v)) in map.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        out.push_str(&json_quote(k));
        out.push(':');
        out.push_str(&json_quote(v));
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
