//! Pure-Rust, zero-dependency 2D software renderer, glyph atlas, and custom UI controls.
//!
//! Renders every pixel (document view, gutter, custom overlay scrollbars, lazy file
//! sidebar, find/replace bar, go-to-line bar, status bar) into a CPU `Framebuffer`
//! using the **Build Useful** design system tokens.

pub mod atlas;
pub mod chrome;
pub mod context_menu;
pub mod painter;
pub mod palette;
pub mod pins;
pub mod shortcuts;
pub mod sidebar;
pub mod tab_strip;
pub mod text_input;
pub mod theme;
pub mod viewport;

pub use atlas::{AtlasGlyph, GlyphAtlas};
pub use chrome::{
    A11yNode, A11yRole, ActionToast, ConfirmModal, FindBarAction, FindBarLayout, FindBarState,
    GotoLineState, ImeComposition, StatusBarData, ToastHit,
};
pub use context_menu::{ContextMenu, ContextMenuEntry};
pub use painter::Painter;
pub use palette::{
    fuzzy_score, CommandPalette, PaletteAction, PaletteCommandEntry, PaletteItem, PaletteMode,
    PaletteOpenTarget,
};
pub use pins::{PinEntry, PinsAction, PinsOverlay};
pub use shortcuts::{ShortcutDisplayItem, ShortcutsAction, ShortcutsOverlay};
pub use sidebar::{
    Sidebar, SidebarAction, SidebarBadge, SidebarContextAction, SidebarHit, SidebarPromptAction,
    SidebarPromptKind, SidebarSide, SIDEBAR_AUTO_MINIMIZE_WIDTH_PT,
};
pub use tab_strip::{
    tab_strip_height_px, StripDropTarget, TabDisplayItem, TabStrip, TabStripHit,
    TAB_STRIP_HEIGHT_PT,
};
pub use text_input::TextInput;
pub use theme::{
    mono_style_str, parse_mono_style, px, Color, SyntaxPalette, SyntaxStylePreference,
    SyntaxThemeConfig, SyntaxThemePreset, Theme, TokenOverride, TokenStyle,
};
pub use viewport::{DocHit, ScrollbarHit, Viewport, ViewportLayout};
