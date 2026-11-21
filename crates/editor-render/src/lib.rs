//! Pure-Rust, zero-dependency 2D software renderer, glyph atlas, and custom UI controls.
//!
//! Renders every pixel (document view, gutter, custom overlay scrollbars, lazy file
//! sidebar, find/replace bar, go-to-line bar, status bar) into a CPU `Framebuffer`
//! using the **Build Useful** design system tokens.

pub mod atlas;
pub mod chrome;
pub mod painter;
pub mod palette;
pub mod shortcuts;
pub mod sidebar;
pub mod text_input;
pub mod theme;
pub mod viewport;

pub use atlas::{AtlasGlyph, GlyphAtlas};
pub use chrome::{
    A11yNode, A11yRole, ConfirmModal, FindBarAction, FindBarLayout, FindBarState, GotoLineState,
    ImeComposition, StatusBarData,
};
pub use painter::Painter;
pub use palette::{fuzzy_score, CommandPalette, PaletteAction, PaletteItem, PaletteMode};
pub use shortcuts::{ShortcutDisplayItem, ShortcutsAction, ShortcutsOverlay};
pub use sidebar::{Sidebar, SidebarAction, SidebarHit};
pub use text_input::TextInput;
pub use theme::{Color, Theme};
pub use viewport::{DocHit, ScrollbarHit, Viewport, ViewportLayout};
