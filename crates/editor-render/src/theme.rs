//! Build Useful design system semantic colour tokens (Light & Dark).
//!
//! Single source of truth for all colours in the editor. No hardcoded hex values
//! exist anywhere else in UI or rendering code:
//! - One ink, a short ladder of neutral greys, hairline borders, quiet chrome.
//! - Colour marks state only (danger / success / info). No accent hue.

use editor_core::TokenKind;
use editor_platform::SystemTheme;

/// Non-premultiplied RGBA8 color, with fast conversion to little-endian premultiplied `0xAARRGGBB`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    #[inline]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    #[inline]
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    #[inline]
    pub const fn with_alpha(self, a: u8) -> Self {
        Self { a, ..self }
    }

    /// Convert to packed `0xAARRGGBB` (premultiplied).
    #[inline]
    pub const fn to_premul_u32(self) -> u32 {
        if self.a == 255 {
            return (0xFFu32 << 24)
                | ((self.r as u32) << 16)
                | ((self.g as u32) << 8)
                | (self.b as u32);
        }
        let a = self.a as u32;
        let r = (self.r as u32 * a + 127) / 255;
        let g = (self.g as u32 * a + 127) / 255;
        let b = (self.b as u32 * a + 127) / 255;
        (a << 24) | (r << 16) | (g << 8) | b
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub is_dark: bool,
    /// `color.surface` — document ground
    pub surface: Color,
    /// `color.text` — primary ink
    pub text: Color,
    /// `color.secondary-hover` — neutral selection fill
    pub selection: Color,
    /// `color.surface-subtle` — current line & subtle hover fill
    pub current_line: Color,
    /// `color.page` — chrome ground (gutter, sidebar, status bar)
    pub page: Color,
    /// `color.text-muted` — line numbers, comments, secondary chrome labels
    pub text_muted: Color,
    /// `color.border-subtle` — 1px hairline separators
    pub border_subtle: Color,
    /// `color.border-control` — scrollbar thumbs, active control borders
    pub border_control: Color,
    /// Quiet match highlight (neutral tinted fill)
    pub search_match: Color,
    /// Active/current search match highlight
    pub search_match_active: Color,
    /// State-only semantic colours (danger / success / info)
    pub state_danger: Color,
    pub state_success: Color,
    pub state_info: Color,
    /// Subtle ink-ladder syntax tones (quiet, high-contrast, no garish neon)
    pub syn_string: Color,
    pub syn_number: Color,
    pub syn_type: Color,
    pub syn_macro: Color,
}

impl Theme {
    pub const fn for_system(mode: SystemTheme) -> Self {
        match mode {
            SystemTheme::Light => Self::light(),
            SystemTheme::Dark => Self::dark(),
        }
    }

    pub const fn light() -> Self {
        Self {
            is_dark: false,
            surface: Color::rgb(0xff, 0xff, 0xff),        // color.surface
            text: Color::rgb(0x17, 0x1a, 0x1f),           // color.text
            selection: Color::rgb(0xdf, 0xe4, 0xea),      // color.secondary-hover
            current_line: Color::rgb(0xf4, 0xf6, 0xf8),   // color.surface-subtle
            page: Color::rgb(0xf6, 0xf7, 0xf9),           // color.page
            text_muted: Color::rgb(0x62, 0x6a, 0x76),     // color.text-muted
            border_subtle: Color::rgb(0xe6, 0xe9, 0xee),  // color.border-subtle
            border_control: Color::rgb(0x7e, 0x87, 0x94), // color.border-control
            search_match: Color::rgba(0x7e, 0x87, 0x94, 48),
            search_match_active: Color::rgba(0x17, 0x1a, 0x1f, 64),
            state_danger: Color::rgb(0xb4, 0x23, 0x18),
            state_success: Color::rgb(0x06, 0x76, 0x47),
            state_info: Color::rgb(0x1d, 0x4e, 0xd8),
            // Quiet tonal ladder for code legibility while staying monochrome-faithful:
            syn_string: Color::rgb(0x3b, 0x48, 0x59),
            syn_number: Color::rgb(0x34, 0x3d, 0x4b),
            syn_type: Color::rgb(0x2c, 0x33, 0x3f),
            syn_macro: Color::rgb(0x47, 0x51, 0x60),
        }
    }

    pub const fn dark() -> Self {
        Self {
            is_dark: true,
            surface: Color::rgb(0x1b, 0x1b, 0x1b),        // color.surface
            text: Color::rgb(0xf1, 0xf1, 0xf1),           // color.text
            selection: Color::rgb(0x30, 0x30, 0x30),      // color.secondary-hover
            current_line: Color::rgb(0x20, 0x20, 0x20),   // color.surface-subtle
            page: Color::rgb(0x11, 0x11, 0x11),           // color.page
            text_muted: Color::rgb(0xb8, 0xb8, 0xb8),     // color.text-muted
            border_subtle: Color::rgb(0x2a, 0x2a, 0x2a),  // color.border-subtle
            border_control: Color::rgb(0x6d, 0x6d, 0x6d), // color.border-control
            search_match: Color::rgba(0xb8, 0xb8, 0xb8, 42),
            search_match_active: Color::rgba(0xf1, 0xf1, 0xf1, 68),
            state_danger: Color::rgb(0xf9, 0x70, 0x66),
            state_success: Color::rgb(0x47, 0xcd, 0x89),
            state_info: Color::rgb(0x60, 0xa5, 0xfa),
            syn_string: Color::rgb(0xd4, 0xd8, 0xdf),
            syn_number: Color::rgb(0xdc, 0xdf, 0xe4),
            syn_type: Color::rgb(0xe2, 0xe5, 0xe9),
            syn_macro: Color::rgb(0xc8, 0xcc, 0xd4),
        }
    }

    /// Resolve syntax token colour and boldness cleanly from the theme's ink ladder.
    #[inline]
    pub const fn syntax_style(self, kind: TokenKind) -> (Color, bool) {
        match kind {
            TokenKind::Plain | TokenKind::Punctuation => (self.text, false),
            TokenKind::Keyword | TokenKind::Heading => (self.text, true),
            TokenKind::Type => (self.syn_type, false),
            TokenKind::String => (self.syn_string, false),
            TokenKind::Number => (self.syn_number, false),
            TokenKind::Comment => (self.text_muted, false),
            TokenKind::Macro => (self.syn_macro, true),
        }
    }
}

/// 2px spacing ladder helper scaled by backing scale factor.
#[inline]
pub fn px(pt: f32, scale: f32) -> i32 {
    (pt * scale).round() as i32
}
