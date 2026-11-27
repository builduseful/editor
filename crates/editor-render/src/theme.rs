//! Build Useful design system semantic colour tokens & customizable O(1) syntax palettes.
//!
//! Architecture:
//! - **UI chrome** always stays strictly Build Useful monochrome (one ink, neutral grey ladder,
//!   hairline borders, colour strictly for danger / success / info state).
//! - **Code syntax** resolves into a flat, cache-line-compact `[TokenStyle; TokenKind::COUNT]`
//!   table (`~95 bytes`, `Copy`, zero heap) embedded directly inside [`Theme`].
//! - On the hot glyph rendering loop, [`Theme::syntax_font_style`] is literally a single branchless
//!   indexed load (`self.syntax.styles[kind as usize]`) — identical O(1) speed whether using a
//!   built-in preset or a fully customized agent-authored theme!

use std::collections::BTreeMap;

use editor_core::TokenKind;
use editor_platform::{FontStyle, SystemTheme};

/// Built-in curated syntax theme presets (each ships with paired Light & Dark palettes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SyntaxThemePreset {
    /// Quiet, low-saturation architectural tones harmonized with Build Useful Light & Dark.
    #[default]
    Editorial,
    /// Pure Build Useful monochrome ink ladder (weight + italic + neutral tonal steps, zero hue).
    Monochrome,
    /// Crisper classic IDE colour separation (balanced WCAG contrast in Light & Dark).
    Studio,
    /// Bookish warm sepia / olive / terracotta palette (paper-warm Light, espresso Dark).
    Warm,
}

/// Backwards-compatible alias.
pub type SyntaxStylePreference = SyntaxThemePreset;

impl SyntaxThemePreset {
    pub const ALL: [SyntaxThemePreset; 4] = [
        SyntaxThemePreset::Editorial,
        SyntaxThemePreset::Monochrome,
        SyntaxThemePreset::Studio,
        SyntaxThemePreset::Warm,
    ];

    #[inline]
    pub const fn as_str(self) -> &'static str {
        match self {
            SyntaxThemePreset::Editorial => "editorial",
            SyntaxThemePreset::Monochrome => "monochrome",
            SyntaxThemePreset::Studio => "studio",
            SyntaxThemePreset::Warm => "warm",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "editorial" | "color" | "colour" | "tinted" | "default" => {
                Some(SyntaxThemePreset::Editorial)
            }
            "monochrome" | "mono" | "ink" | "grey" | "gray" => Some(SyntaxThemePreset::Monochrome),
            "studio" | "vivid" | "classic" | "ide" => Some(SyntaxThemePreset::Studio),
            "warm" | "paper" | "sepia" | "book" => Some(SyntaxThemePreset::Warm),
            _ => None,
        }
    }
}

/// Parse a monospace font style label (`normal` | `bold` | `italic`).
pub fn parse_mono_style(s: &str) -> Option<FontStyle> {
    match s.trim().to_ascii_lowercase().as_str() {
        "normal" | "regular" | "plain" | "none" => Some(FontStyle::Mono),
        "bold" | "b" => Some(FontStyle::MonoBold),
        "italic" | "italics" | "i" => Some(FontStyle::MonoItalic),
        _ => None,
    }
}

/// Format a monospace `FontStyle` as a canonical lowercase string.
#[inline]
pub const fn mono_style_str(style: FontStyle) -> &'static str {
    match style {
        FontStyle::MonoBold => "bold",
        FontStyle::MonoItalic => "italic",
        _ => "normal",
    }
}

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

    /// Parse `#RGB`, `#RRGGBB`, `RRGGBB`, or `#RRGGBBAA` hex colours.
    pub fn parse_hex(raw: &str) -> Option<Self> {
        let s = raw.trim().strip_prefix('#').unwrap_or(raw.trim());
        if !s.is_ascii() {
            return None;
        }
        match s.len() {
            3 => {
                let r = u8::from_str_radix(&s[0..1], 16).ok()?;
                let g = u8::from_str_radix(&s[1..2], 16).ok()?;
                let b = u8::from_str_radix(&s[2..3], 16).ok()?;
                Some(Color::rgb(r * 17, g * 17, b * 17))
            }
            6 => {
                let r = u8::from_str_radix(&s[0..2], 16).ok()?;
                let g = u8::from_str_radix(&s[2..4], 16).ok()?;
                let b = u8::from_str_radix(&s[4..6], 16).ok()?;
                Some(Color::rgb(r, g, b))
            }
            8 => {
                let r = u8::from_str_radix(&s[0..2], 16).ok()?;
                let g = u8::from_str_radix(&s[2..4], 16).ok()?;
                let b = u8::from_str_radix(&s[4..6], 16).ok()?;
                let a = u8::from_str_radix(&s[6..8], 16).ok()?;
                Some(Color::rgba(r, g, b, a))
            }
            _ => None,
        }
    }

    /// Format as canonical lowercase `#rrggbb` (or `#rrggbbaa` when alpha != 255).
    pub fn to_hex(self) -> String {
        if self.a == 255 {
            format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
        } else {
            format!("#{:02x}{:02x}{:02x}{:02x}", self.r, self.g, self.b, self.a)
        }
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

/// Resolved visual style for one [`TokenKind`] (`5 bytes`, `Copy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenStyle {
    pub color: Color,
    pub font: FontStyle,
}

impl TokenStyle {
    #[inline]
    pub const fn new(color: Color, font: FontStyle) -> Self {
        Self { color, font }
    }
}

/// Pre-baked flat lookup table for all 19 semantic tokens (`~95 bytes`, `Copy`, O(1) indexed access).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyntaxPalette {
    pub styles: [TokenStyle; TokenKind::COUNT],
}

impl SyntaxPalette {
    #[inline]
    pub const fn get(self, kind: TokenKind) -> TokenStyle {
        self.styles[kind as usize]
    }

    #[inline]
    pub fn set(&mut self, kind: TokenKind, style: TokenStyle) {
        self.styles[kind as usize] = style;
    }

    pub const fn preset(preset: SyntaxThemePreset, is_dark: bool) -> Self {
        match (preset, is_dark) {
            (SyntaxThemePreset::Editorial, false) => Self::editorial_light(),
            (SyntaxThemePreset::Editorial, true) => Self::editorial_dark(),
            (SyntaxThemePreset::Monochrome, false) => Self::monochrome_light(),
            (SyntaxThemePreset::Monochrome, true) => Self::monochrome_dark(),
            (SyntaxThemePreset::Studio, false) => Self::studio_light(),
            (SyntaxThemePreset::Studio, true) => Self::studio_dark(),
            (SyntaxThemePreset::Warm, false) => Self::warm_light(),
            (SyntaxThemePreset::Warm, true) => Self::warm_dark(),
        }
    }

    /// Calm architectural editorial tones on `#ffffff` (Build Useful Light default).
    pub const fn editorial_light() -> Self {
        let text = Color::rgb(0x17, 0x1a, 0x1f);
        Self {
            styles: [
                TokenStyle::new(text, FontStyle::Mono),                                 // Plain
                TokenStyle::new(Color::rgb(0x6c, 0x74, 0x80), FontStyle::MonoItalic),   // Comment
                TokenStyle::new(Color::rgb(0x27, 0x46, 0x7a), FontStyle::MonoBold),     // Keyword
                TokenStyle::new(Color::rgb(0x5b, 0x3b, 0x78), FontStyle::Mono),         // Type
                TokenStyle::new(Color::rgb(0x1d, 0x52, 0x68), FontStyle::Mono),         // Function
                TokenStyle::new(Color::rgb(0x1e, 0x60, 0x42), FontStyle::Mono),         // String
                TokenStyle::new(Color::rgb(0x93, 0x49, 0x16), FontStyle::MonoBold),     // Escape
                TokenStyle::new(Color::rgb(0x8c, 0x4a, 0x14), FontStyle::Mono),         // Number
                TokenStyle::new(Color::rgb(0x80, 0x3d, 0x24), FontStyle::Mono),         // Constant
                TokenStyle::new(Color::rgb(0x6b, 0x42, 0x6b), FontStyle::MonoBold),     // Macro
                TokenStyle::new(Color::rgb(0x27, 0x46, 0x7a), FontStyle::MonoBold),     // Tag
                TokenStyle::new(Color::rgb(0x6e, 0x4c, 0x26), FontStyle::MonoItalic),   // Attribute
                TokenStyle::new(Color::rgb(0x2d, 0x4e, 0x6e), FontStyle::Mono),         // Property
                TokenStyle::new(Color::rgb(0x4b, 0x53, 0x60), FontStyle::Mono),         // Operator
                TokenStyle::new(Color::rgb(0x62, 0x6a, 0x76), FontStyle::Mono),         // Punctuation
                TokenStyle::new(text, FontStyle::MonoBold),                             // Heading
                TokenStyle::new(Color::rgb(0x1d, 0x4e, 0xd8), FontStyle::Mono),         // Link
                TokenStyle::new(text, FontStyle::MonoItalic),                           // Emphasis
                TokenStyle::new(text, FontStyle::MonoBold),                             // Strong
            ],
        }
    }

    /// Calm architectural editorial tones on `#1b1b1b` (Build Useful Dark default).
    pub const fn editorial_dark() -> Self {
        let text = Color::rgb(0xf1, 0xf1, 0xf1);
        Self {
            styles: [
                TokenStyle::new(text, FontStyle::Mono),                                 // Plain
                TokenStyle::new(Color::rgb(0x86, 0x8d, 0x97), FontStyle::MonoItalic),   // Comment
                TokenStyle::new(Color::rgb(0x90, 0xb6, 0xe4), FontStyle::MonoBold),     // Keyword
                TokenStyle::new(Color::rgb(0xc6, 0xa6, 0xe2), FontStyle::Mono),         // Type
                TokenStyle::new(Color::rgb(0x88, 0xc4, 0xd8), FontStyle::Mono),         // Function
                TokenStyle::new(Color::rgb(0x8c, 0xc6, 0xa2), FontStyle::Mono),         // String
                TokenStyle::new(Color::rgb(0xe2, 0xab, 0x76), FontStyle::MonoBold),     // Escape
                TokenStyle::new(Color::rgb(0xde, 0xab, 0x78), FontStyle::Mono),         // Number
                TokenStyle::new(Color::rgb(0xda, 0x9b, 0x84), FontStyle::Mono),         // Constant
                TokenStyle::new(Color::rgb(0xcc, 0xa2, 0xd0), FontStyle::MonoBold),     // Macro
                TokenStyle::new(Color::rgb(0x90, 0xb6, 0xe4), FontStyle::MonoBold),     // Tag
                TokenStyle::new(Color::rgb(0xd4, 0xb4, 0x8c), FontStyle::MonoItalic),   // Attribute
                TokenStyle::new(Color::rgb(0x9e, 0xc0, 0xdc), FontStyle::Mono),         // Property
                TokenStyle::new(Color::rgb(0xb6, 0xbd, 0xc7), FontStyle::Mono),         // Operator
                TokenStyle::new(Color::rgb(0x92, 0x98, 0xa2), FontStyle::Mono),         // Punctuation
                TokenStyle::new(text, FontStyle::MonoBold),                             // Heading
                TokenStyle::new(Color::rgb(0x60, 0xa5, 0xfa), FontStyle::Mono),         // Link
                TokenStyle::new(text, FontStyle::MonoItalic),                           // Emphasis
                TokenStyle::new(text, FontStyle::MonoBold),                             // Strong
            ],
        }
    }

    /// Pure Build Useful monochrome ink ladder (Light).
    pub const fn monochrome_light() -> Self {
        let text = Color::rgb(0x17, 0x1a, 0x1f);
        let mid = Color::rgb(0x45, 0x4c, 0x57);
        let muted = Color::rgb(0x62, 0x6a, 0x76);
        Self {
            styles: [
                TokenStyle::new(text, FontStyle::Mono),        // Plain
                TokenStyle::new(muted, FontStyle::MonoItalic), // Comment
                TokenStyle::new(text, FontStyle::MonoBold),    // Keyword
                TokenStyle::new(mid, FontStyle::Mono),         // Type
                TokenStyle::new(text, FontStyle::Mono),        // Function
                TokenStyle::new(mid, FontStyle::Mono),         // String
                TokenStyle::new(text, FontStyle::MonoBold),    // Escape
                TokenStyle::new(mid, FontStyle::Mono),         // Number
                TokenStyle::new(mid, FontStyle::MonoBold),     // Constant
                TokenStyle::new(text, FontStyle::MonoBold),    // Macro
                TokenStyle::new(text, FontStyle::MonoBold),    // Tag
                TokenStyle::new(muted, FontStyle::MonoItalic), // Attribute
                TokenStyle::new(text, FontStyle::Mono),        // Property
                TokenStyle::new(muted, FontStyle::Mono),       // Operator
                TokenStyle::new(muted, FontStyle::Mono),       // Punctuation
                TokenStyle::new(text, FontStyle::MonoBold),    // Heading
                TokenStyle::new(mid, FontStyle::Mono),         // Link
                TokenStyle::new(muted, FontStyle::MonoItalic), // Emphasis
                TokenStyle::new(text, FontStyle::MonoBold),    // Strong
            ],
        }
    }

    /// Pure Build Useful monochrome ink ladder (Dark).
    pub const fn monochrome_dark() -> Self {
        let text = Color::rgb(0xf1, 0xf1, 0xf1);
        let mid = Color::rgb(0xc8, 0xc8, 0xc8);
        let muted = Color::rgb(0x8e, 0x8e, 0x8e);
        Self {
            styles: [
                TokenStyle::new(text, FontStyle::Mono),        // Plain
                TokenStyle::new(muted, FontStyle::MonoItalic), // Comment
                TokenStyle::new(text, FontStyle::MonoBold),    // Keyword
                TokenStyle::new(mid, FontStyle::Mono),         // Type
                TokenStyle::new(text, FontStyle::Mono),        // Function
                TokenStyle::new(mid, FontStyle::Mono),         // String
                TokenStyle::new(text, FontStyle::MonoBold),    // Escape
                TokenStyle::new(mid, FontStyle::Mono),         // Number
                TokenStyle::new(mid, FontStyle::MonoBold),     // Constant
                TokenStyle::new(text, FontStyle::MonoBold),    // Macro
                TokenStyle::new(text, FontStyle::MonoBold),    // Tag
                TokenStyle::new(muted, FontStyle::MonoItalic), // Attribute
                TokenStyle::new(text, FontStyle::Mono),        // Property
                TokenStyle::new(muted, FontStyle::Mono),       // Operator
                TokenStyle::new(muted, FontStyle::Mono),       // Punctuation
                TokenStyle::new(text, FontStyle::MonoBold),    // Heading
                TokenStyle::new(mid, FontStyle::Mono),         // Link
                TokenStyle::new(muted, FontStyle::MonoItalic), // Emphasis
                TokenStyle::new(text, FontStyle::MonoBold),    // Strong
            ],
        }
    }

    /// Crisper classic IDE colour separation (Light).
    pub const fn studio_light() -> Self {
        let text = Color::rgb(0x17, 0x1a, 0x1f);
        Self {
            styles: [
                TokenStyle::new(text, FontStyle::Mono),                                 // Plain
                TokenStyle::new(Color::rgb(0x6a, 0x73, 0x7d), FontStyle::MonoItalic),   // Comment
                TokenStyle::new(Color::rgb(0xb8, 0x24, 0x4c), FontStyle::MonoBold),     // Keyword
                TokenStyle::new(Color::rgb(0x6f, 0x42, 0xc1), FontStyle::Mono),         // Type
                TokenStyle::new(Color::rgb(0x00, 0x5c, 0xc5), FontStyle::Mono),         // Function
                TokenStyle::new(Color::rgb(0x11, 0x63, 0x29), FontStyle::Mono),         // String
                TokenStyle::new(Color::rgb(0xbc, 0x4c, 0x00), FontStyle::MonoBold),     // Escape
                TokenStyle::new(Color::rgb(0x05, 0x50, 0xae), FontStyle::Mono),         // Number
                TokenStyle::new(Color::rgb(0x95, 0x38, 0x00), FontStyle::Mono),         // Constant
                TokenStyle::new(Color::rgb(0x82, 0x50, 0xdf), FontStyle::MonoBold),     // Macro
                TokenStyle::new(Color::rgb(0x11, 0x63, 0x29), FontStyle::MonoBold),     // Tag
                TokenStyle::new(Color::rgb(0x95, 0x38, 0x00), FontStyle::MonoItalic),   // Attribute
                TokenStyle::new(Color::rgb(0x05, 0x50, 0xae), FontStyle::Mono),         // Property
                TokenStyle::new(Color::rgb(0x4b, 0x53, 0x60), FontStyle::Mono),         // Operator
                TokenStyle::new(Color::rgb(0x57, 0x60, 0x6a), FontStyle::Mono),         // Punctuation
                TokenStyle::new(Color::rgb(0x05, 0x50, 0xae), FontStyle::MonoBold),     // Heading
                TokenStyle::new(Color::rgb(0x09, 0x69, 0xda), FontStyle::Mono),         // Link
                TokenStyle::new(text, FontStyle::MonoItalic),                           // Emphasis
                TokenStyle::new(text, FontStyle::MonoBold),                             // Strong
            ],
        }
    }

    /// Crisper classic IDE colour separation (Dark).
    pub const fn studio_dark() -> Self {
        let text = Color::rgb(0xf1, 0xf1, 0xf1);
        Self {
            styles: [
                TokenStyle::new(text, FontStyle::Mono),                                 // Plain
                TokenStyle::new(Color::rgb(0x8b, 0x94, 0x9e), FontStyle::MonoItalic),   // Comment
                TokenStyle::new(Color::rgb(0xff, 0x7b, 0x72), FontStyle::MonoBold),     // Keyword
                TokenStyle::new(Color::rgb(0xd2, 0xa8, 0xff), FontStyle::Mono),         // Type
                TokenStyle::new(Color::rgb(0x79, 0xc0, 0xff), FontStyle::Mono),         // Function
                TokenStyle::new(Color::rgb(0x7e, 0xe7, 0x87), FontStyle::Mono),         // String
                TokenStyle::new(Color::rgb(0xff, 0xa6, 0x57), FontStyle::MonoBold),     // Escape
                TokenStyle::new(Color::rgb(0x79, 0xc0, 0xff), FontStyle::Mono),         // Number
                TokenStyle::new(Color::rgb(0xff, 0xa6, 0x57), FontStyle::Mono),         // Constant
                TokenStyle::new(Color::rgb(0xd2, 0xa8, 0xff), FontStyle::MonoBold),     // Macro
                TokenStyle::new(Color::rgb(0x7e, 0xe7, 0x87), FontStyle::MonoBold),     // Tag
                TokenStyle::new(Color::rgb(0xff, 0xa6, 0x57), FontStyle::MonoItalic),   // Attribute
                TokenStyle::new(Color::rgb(0xa5, 0xd6, 0xff), FontStyle::Mono),         // Property
                TokenStyle::new(Color::rgb(0xb6, 0xbd, 0xc7), FontStyle::Mono),         // Operator
                TokenStyle::new(Color::rgb(0x8b, 0x94, 0x9e), FontStyle::Mono),         // Punctuation
                TokenStyle::new(Color::rgb(0x79, 0xc0, 0xff), FontStyle::MonoBold),     // Heading
                TokenStyle::new(Color::rgb(0x58, 0xa6, 0xff), FontStyle::Mono),         // Link
                TokenStyle::new(text, FontStyle::MonoItalic),                           // Emphasis
                TokenStyle::new(text, FontStyle::MonoBold),                             // Strong
            ],
        }
    }

    /// Warm bookish sepia / olive / rust palette (Light).
    pub const fn warm_light() -> Self {
        let text = Color::rgb(0x1f, 0x1b, 0x16);
        Self {
            styles: [
                TokenStyle::new(text, FontStyle::Mono),                                 // Plain
                TokenStyle::new(Color::rgb(0x7c, 0x72, 0x64), FontStyle::MonoItalic),   // Comment
                TokenStyle::new(Color::rgb(0x8a, 0x3b, 0x1b), FontStyle::MonoBold),     // Keyword
                TokenStyle::new(Color::rgb(0x5e, 0x45, 0x26), FontStyle::Mono),         // Type
                TokenStyle::new(Color::rgb(0x2f, 0x54, 0x5c), FontStyle::Mono),         // Function
                TokenStyle::new(Color::rgb(0x46, 0x62, 0x2c), FontStyle::Mono),         // String
                TokenStyle::new(Color::rgb(0x9e, 0x4f, 0x16), FontStyle::MonoBold),     // Escape
                TokenStyle::new(Color::rgb(0x8c, 0x54, 0x1c), FontStyle::Mono),         // Number
                TokenStyle::new(Color::rgb(0x7d, 0x38, 0x2c), FontStyle::Mono),         // Constant
                TokenStyle::new(Color::rgb(0x6c, 0x42, 0x52), FontStyle::MonoBold),     // Macro
                TokenStyle::new(Color::rgb(0x8a, 0x3b, 0x1b), FontStyle::MonoBold),     // Tag
                TokenStyle::new(Color::rgb(0x74, 0x56, 0x26), FontStyle::MonoItalic),   // Attribute
                TokenStyle::new(Color::rgb(0x3a, 0x57, 0x52), FontStyle::Mono),         // Property
                TokenStyle::new(Color::rgb(0x5a, 0x51, 0x46), FontStyle::Mono),         // Operator
                TokenStyle::new(Color::rgb(0x70, 0x67, 0x5b), FontStyle::Mono),         // Punctuation
                TokenStyle::new(text, FontStyle::MonoBold),                             // Heading
                TokenStyle::new(Color::rgb(0x2f, 0x54, 0x5c), FontStyle::Mono),         // Link
                TokenStyle::new(text, FontStyle::MonoItalic),                           // Emphasis
                TokenStyle::new(text, FontStyle::MonoBold),                             // Strong
            ],
        }
    }

    /// Warm bookish sepia / olive / amber palette (Dark).
    pub const fn warm_dark() -> Self {
        let text = Color::rgb(0xee, 0xe8, 0xdd);
        Self {
            styles: [
                TokenStyle::new(text, FontStyle::Mono),                                 // Plain
                TokenStyle::new(Color::rgb(0x92, 0x87, 0x78), FontStyle::MonoItalic),   // Comment
                TokenStyle::new(Color::rgb(0xdf, 0x94, 0x6e), FontStyle::MonoBold),     // Keyword
                TokenStyle::new(Color::rgb(0xd8, 0xb8, 0x82), FontStyle::Mono),         // Type
                TokenStyle::new(Color::rgb(0x8e, 0xbe, 0xb6), FontStyle::Mono),         // Function
                TokenStyle::new(Color::rgb(0xa8, 0xc2, 0x86), FontStyle::Mono),         // String
                TokenStyle::new(Color::rgb(0xe8, 0xad, 0x6c), FontStyle::MonoBold),     // Escape
                TokenStyle::new(Color::rgb(0xde, 0xa8, 0x72), FontStyle::Mono),         // Number
                TokenStyle::new(Color::rgb(0xd6, 0x8e, 0x7e), FontStyle::Mono),         // Constant
                TokenStyle::new(Color::rgb(0xc8, 0x9a, 0xb0), FontStyle::MonoBold),     // Macro
                TokenStyle::new(Color::rgb(0xdf, 0x94, 0x6e), FontStyle::MonoBold),     // Tag
                TokenStyle::new(Color::rgb(0xd4, 0xb6, 0x84), FontStyle::MonoItalic),   // Attribute
                TokenStyle::new(Color::rgb(0x9a, 0xc0, 0xb2), FontStyle::Mono),         // Property
                TokenStyle::new(Color::rgb(0xb8, 0xae, 0xa0), FontStyle::Mono),         // Operator
                TokenStyle::new(Color::rgb(0x96, 0x8c, 0x7e), FontStyle::Mono),         // Punctuation
                TokenStyle::new(text, FontStyle::MonoBold),                             // Heading
                TokenStyle::new(Color::rgb(0x8e, 0xbe, 0xb6), FontStyle::Mono),         // Link
                TokenStyle::new(text, FontStyle::MonoItalic),                           // Emphasis
                TokenStyle::new(text, FontStyle::MonoBold),                             // Strong
            ],
        }
    }

    pub fn apply_overrides(&mut self, overrides: &BTreeMap<TokenKind, TokenOverride>) {
        for (&kind, ov) in overrides {
            let slot = &mut self.styles[kind as usize];
            if let Some(c) = ov.color {
                slot.color = c;
            }
            if let Some(f) = ov.font {
                slot.font = f;
            }
        }
    }
}

/// Optional per-token override authored by a user or agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TokenOverride {
    pub color: Option<Color>,
    pub font: Option<FontStyle>,
}

impl TokenOverride {
    #[inline]
    pub fn is_empty(self) -> bool {
        self.color.is_none() && self.font.is_none()
    }

    /// Parse shorthand spec like `"#90b6e4"`, `"#90b6e4 bold"`, `"#90b6e4:italic"`, or `"italic"`.
    pub fn parse_spec(spec: &str) -> Result<Self, String> {
        let mut ov = TokenOverride::default();
        for raw_part in spec.split(|c: char| c.is_whitespace() || c == ':') {
            let part = raw_part.trim();
            if part.is_empty() || part == "-" || part.eq_ignore_ascii_case("keep") {
                continue;
            }
            if let Some(c) = Color::parse_hex(part) {
                ov.color = Some(c);
            } else if let Some(f) = parse_mono_style(part) {
                ov.font = Some(f);
            } else {
                return Err(format!(
                    "invalid token style `{part}` (expected #RRGGBB hex or normal|bold|italic)"
                ));
            }
        }
        if ov.is_empty() {
            return Err("empty token override spec".to_string());
        }
        Ok(ov)
    }

    /// Compact deterministic persistence format (`"#rrggbb:bold"`, `"#rrggbb"`, or `":italic"`).
    pub fn encode(self) -> String {
        match (self.color, self.font) {
            (Some(c), Some(f)) => format!("{}:{}", c.to_hex(), mono_style_str(f)),
            (Some(c), None) => c.to_hex(),
            (None, Some(f)) => format!(":{}", mono_style_str(f)),
            (None, None) => String::new(),
        }
    }
}

/// Complete customizable syntax theme configuration (persisted & agent-editable).
///
/// Resolved once on mutation into paired 95-byte [`SyntaxPalette`]s (Light & Dark) so
/// rendering incurs literally zero per-frame allocation, branching, or map lookup!
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SyntaxThemeConfig {
    pub preset: SyntaxThemePreset,
    pub light_overrides: BTreeMap<TokenKind, TokenOverride>,
    pub dark_overrides: BTreeMap<TokenKind, TokenOverride>,
}

impl SyntaxThemeConfig {
    pub fn resolve(&self, is_dark: bool) -> SyntaxPalette {
        let mut pal = SyntaxPalette::preset(self.preset, is_dark);
        let overrides = if is_dark {
            &self.dark_overrides
        } else {
            &self.light_overrides
        };
        pal.apply_overrides(overrides);
        pal
    }

    pub fn encode_overrides(map: &BTreeMap<TokenKind, TokenOverride>) -> String {
        let mut out = String::new();
        for (i, (&kind, &ov)) in map.iter().enumerate() {
            if ov.is_empty() {
                continue;
            }
            if i > 0 && !out.is_empty() {
                out.push(',');
            }
            out.push_str(kind.as_str());
            out.push('=');
            out.push_str(&ov.encode());
        }
        out
    }

    pub fn decode_overrides(raw: &str) -> BTreeMap<TokenKind, TokenOverride> {
        let mut map = BTreeMap::new();
        for pair in raw.split(',') {
            let pair = pair.trim();
            if let Some((k, v)) = pair.split_once('=') {
                if let (Some(kind), Ok(ov)) = (TokenKind::parse(k), TokenOverride::parse_spec(v)) {
                    map.insert(kind, ov);
                }
            }
        }
        map
    }

    /// Serialize full resolved Light & Dark palettes, overrides, and available presets as JSON.
    pub fn to_json(&self, active_is_dark: bool) -> String {
        let light = self.resolve(false);
        let dark = self.resolve(true);

        fn write_palette(out: &mut String, pal: SyntaxPalette) {
            out.push('{');
            for (i, kind) in TokenKind::ALL.iter().copied().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                let st = pal.get(kind);
                out.push('"');
                out.push_str(kind.as_str());
                out.push_str("\":{\"color\":\"");
                out.push_str(&st.color.to_hex());
                out.push_str("\",\"style\":\"");
                out.push_str(mono_style_str(st.font));
                out.push_str("\"}");
            }
            out.push('}');
        }

        fn write_overrides(out: &mut String, map: &BTreeMap<TokenKind, TokenOverride>) {
            out.push('{');
            for (i, (&kind, &ov)) in map.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push('"');
                out.push_str(kind.as_str());
                out.push_str("\":\"");
                out.push_str(&ov.encode());
                out.push('"');
            }
            out.push('}');
        }

        let mut out = String::with_capacity(1600);
        out.push_str("{\"preset\":\"");
        out.push_str(self.preset.as_str());
        out.push_str("\",\"active_mode\":\"");
        out.push_str(if active_is_dark { "dark" } else { "light" });
        out.push_str("\",\"available_presets\":[");
        for (i, p) in SyntaxThemePreset::ALL.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push('"');
            out.push_str(p.as_str());
            out.push('"');
        }
        out.push_str("],\"tokens\":[");
        for (i, k) in TokenKind::ALL.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push('"');
            out.push_str(k.as_str());
            out.push('"');
        }
        out.push_str("],\"light\":");
        write_palette(&mut out, light);
        out.push_str(",\"dark\":");
        write_palette(&mut out, dark);
        out.push_str(",\"light_overrides\":");
        write_overrides(&mut out, &self.light_overrides);
        out.push_str(",\"dark_overrides\":");
        write_overrides(&mut out, &self.dark_overrides);
        out.push('}');
        out
    }

    /// Atomically apply a declarative JSON theme payload:
    /// `{"preset":"editorial","replace":false,"light":{"keyword":"#27467a bold","comment":{"color":"#6c7480","style":"italic"}},"dark":{...}}`
    pub fn apply_json(&mut self, json: &str) -> Result<(), String> {
        let root = parse_simple_json_object(json)?;
        if let Some(JsonNode::Bool(true)) = root.get("replace") {
            self.light_overrides.clear();
            self.dark_overrides.clear();
        }
        if let Some(node) = root.get("preset") {
            let s = node
                .as_str()
                .ok_or_else(|| "`preset` must be a string".to_string())?;
            let preset = SyntaxThemePreset::parse(s).ok_or_else(|| {
                format!("unknown syntax preset `{s}` (use editorial|monochrome|studio|warm)")
            })?;
            self.preset = preset;
        }
        for (section, is_dark) in [("light", false), ("dark", true)] {
            if let Some(node) = root.get(section) {
                let obj = node
                    .as_object()
                    .ok_or_else(|| format!("`{section}` must be a JSON object"))?;
                let target = if is_dark {
                    &mut self.dark_overrides
                } else {
                    &mut self.light_overrides
                };
                for (tok_name, val) in obj {
                    let kind = TokenKind::parse(tok_name).ok_or_else(|| {
                        format!("unknown syntax token `{tok_name}` in `{section}`")
                    })?;
                    match val {
                        JsonNode::Null => {
                            target.remove(&kind);
                        }
                        JsonNode::Str(spec) => {
                            if spec.eq_ignore_ascii_case("reset")
                                || spec.eq_ignore_ascii_case("default")
                            {
                                target.remove(&kind);
                            } else {
                                let ov = TokenOverride::parse_spec(spec)?;
                                let entry = target.entry(kind).or_default();
                                if ov.color.is_some() {
                                    entry.color = ov.color;
                                }
                                if ov.font.is_some() {
                                    entry.font = ov.font;
                                }
                            }
                        }
                        JsonNode::Object(sub) => {
                            let mut entry = target.get(&kind).copied().unwrap_or_default();
                            if let Some(cnode) = sub.get("color") {
                                if let Some(cs) = cnode.as_str() {
                                    entry.color = Some(Color::parse_hex(cs).ok_or_else(|| {
                                        format!("invalid hex color `{cs}` for `{tok_name}`")
                                    })?);
                                } else if matches!(cnode, JsonNode::Null) {
                                    entry.color = None;
                                } else {
                                    return Err(format!("`{tok_name}.color` must be hex string"));
                                }
                            }
                            if let Some(snode) = sub.get("style") {
                                if let Some(ss) = snode.as_str() {
                                    entry.font = Some(parse_mono_style(ss).ok_or_else(|| {
                                        format!("invalid style `{ss}` for `{tok_name}` (use normal|bold|italic)")
                                    })?);
                                } else if matches!(snode, JsonNode::Null) {
                                    entry.font = None;
                                } else {
                                    return Err(format!("`{tok_name}.style` must be string"));
                                }
                            }
                            if entry.is_empty() {
                                target.remove(&kind);
                            } else {
                                target.insert(kind, entry);
                            }
                        }
                        _ => {
                            return Err(format!(
                                "invalid value for token `{tok_name}` (expected hex string, object, or null)"
                            ));
                        }
                    }
                }
            }
        }
        Ok(())
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
    /// `color.text-muted` — line numbers, secondary chrome labels
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
    /// Pre-baked O(1) flat syntax palette (`~95 bytes`, zero branches on glyph draw loop).
    pub syntax: SyntaxPalette,
}

impl Theme {
    pub const fn for_system(mode: SystemTheme) -> Self {
        match mode {
            SystemTheme::Light => Self::light(),
            SystemTheme::Dark => Self::dark(),
        }
    }

    #[inline]
    pub const fn with_syntax_palette(mut self, syntax: SyntaxPalette) -> Self {
        self.syntax = syntax;
        self
    }

    #[inline]
    pub const fn with_syntax_style(mut self, preset: SyntaxThemePreset) -> Self {
        self.syntax = SyntaxPalette::preset(preset, self.is_dark);
        self
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
            syntax: SyntaxPalette::editorial_light(),
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
            syntax: SyntaxPalette::editorial_dark(),
        }
    }

    /// Branchless O(1) syntax token style lookup (~1 ns, L1 cache hit).
    #[inline(always)]
    pub const fn syntax_font_style(self, kind: TokenKind) -> (Color, FontStyle) {
        let st = self.syntax.styles[kind as usize];
        (st.color, st.font)
    }

    /// Backwards-compatible `(Color, bold)` helper.
    #[inline(always)]
    pub const fn syntax_style(self, kind: TokenKind) -> (Color, bool) {
        let st = self.syntax.styles[kind as usize];
        (st.color, matches!(st.font, FontStyle::MonoBold))
    }
}

/// 2px spacing ladder helper scaled by backing scale factor.
#[inline]
pub fn px(pt: f32, scale: f32) -> i32 {
    (pt * scale).round() as i32
}

// ============================================================================
// Minimal zero-dependency JSON parser strictly for theme config objects
// ============================================================================

#[derive(Debug)]
enum JsonNode {
    Null,
    Bool(bool),
    Str(String),
    Object(BTreeMap<String, JsonNode>),
}

impl JsonNode {
    fn as_str(&self) -> Option<&str> {
        match self {
            JsonNode::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }

    fn as_object(&self) -> Option<&BTreeMap<String, JsonNode>> {
        match self {
            JsonNode::Object(m) => Some(m),
            _ => None,
        }
    }
}

struct JsonScanner<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> JsonScanner<'a> {
    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len() && matches!(self.bytes[self.pos], b' ' | b'\t' | b'\r' | b'\n') {
            self.pos += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip_ws();
        self.bytes.get(self.pos).copied()
    }

    fn parse_str(&mut self) -> Result<String, String> {
        self.skip_ws();
        if self.bytes.get(self.pos) != Some(&b'"') {
            return Err("expected JSON string".to_string());
        }
        self.pos += 1;
        let mut out = String::new();
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            self.pos += 1;
            if b == b'"' {
                return Ok(out);
            }
            if b == b'\\' {
                let esc = *self
                    .bytes
                    .get(self.pos)
                    .ok_or_else(|| "unterminated string escape".to_string())?;
                self.pos += 1;
                match esc {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'/' => out.push('/'),
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    _ => return Err(format!("unsupported JSON escape \\{}", esc as char)),
                }
            } else {
                // Safe UTF-8 decode from original slice.
                let start = self.pos - 1;
                let s = std::str::from_utf8(&self.bytes[start..])
                    .map_err(|_| "invalid UTF-8 in JSON".to_string())?;
                let ch = s.chars().next().ok_or_else(|| "unexpected EOF".to_string())?;
                out.push(ch);
                self.pos = start + ch.len_utf8();
            }
        }
        Err("unterminated JSON string".to_string())
    }

    fn parse_object(&mut self) -> Result<BTreeMap<String, JsonNode>, String> {
        self.skip_ws();
        if self.bytes.get(self.pos) != Some(&b'{') {
            return Err("expected `{` for JSON object".to_string());
        }
        self.pos += 1;
        let mut map = BTreeMap::new();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(map);
        }
        loop {
            let key = self.parse_str()?;
            if self.peek() != Some(b':') {
                return Err(format!("expected `:` after key `{key}`"));
            }
            self.pos += 1;
            let val = self.parse_value()?;
            map.insert(key, val);
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(map);
                }
                _ => return Err("expected `,` or `}` in JSON object".to_string()),
            }
        }
    }

    fn parse_value(&mut self) -> Result<JsonNode, String> {
        match self.peek() {
            Some(b'{') => Ok(JsonNode::Object(self.parse_object()?)),
            Some(b'"') => Ok(JsonNode::Str(self.parse_str()?)),
            Some(b't') if self.bytes[self.pos..].starts_with(b"true") => {
                self.pos += 4;
                Ok(JsonNode::Bool(true))
            }
            Some(b'f') if self.bytes[self.pos..].starts_with(b"false") => {
                self.pos += 5;
                Ok(JsonNode::Bool(false))
            }
            Some(b'n') if self.bytes[self.pos..].starts_with(b"null") => {
                self.pos += 4;
                Ok(JsonNode::Null)
            }
            _ => Err("unexpected JSON token in syntax theme".to_string()),
        }
    }
}

fn parse_simple_json_object(input: &str) -> Result<BTreeMap<String, JsonNode>, String> {
    let mut sc = JsonScanner {
        bytes: input.as_bytes(),
        pos: 0,
    };
    let obj = sc.parse_object()?;
    sc.skip_ws();
    if sc.pos != sc.bytes.len() {
        return Err("trailing characters after JSON object".to_string());
    }
    Ok(obj)
}
