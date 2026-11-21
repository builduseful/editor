//! Custom-drawn chrome: Find/Replace bar, Go-To-Line prompt, Status bar,
//! and future-ready IME & Accessibility seams.

use editor_core::{Eol, Language};
use editor_platform::{FontStyle, GlyphRasterizer, PixelRect, ThemePreference};

use crate::atlas::GlyphAtlas;
use crate::painter::Painter;
use crate::text_input::TextInput;
use crate::theme::{px, Theme};

// ============================================================================
// Future-Ready Seams: IME & Accessibility (zero bloat, cleanly structured)
// ============================================================================

/// Preedit composition state for future OS IME integration (`NSTextInputClient` / TSF / Wayland).
#[derive(Debug, Default, Clone)]
pub struct ImeComposition {
    pub preedit: String,
    pub cursor_in_preedit: usize,
    pub caret_rect_px: PixelRect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum A11yRole {
    Window,
    Sidebar,
    Document,
    TextInput,
    Button,
    StatusBar,
}

/// Lightweight accessibility snapshot node for future VoiceOver / UIA / AT-SPI adapters.
#[derive(Debug, Clone)]
pub struct A11yNode {
    pub role: A11yRole,
    pub label: String,
    pub bounds: PixelRect,
    pub focused: bool,
}

// ============================================================================
// Find / Replace Bar
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindBarAction {
    None,
    FindNext,
    FindPrev,
    ToggleCase,
    ReplaceOne,
    ReplaceAll,
    Close,
    FocusFind,
    FocusReplace,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct FindBarLayout {
    pub bar: PixelRect,
    pub find_input: PixelRect,
    pub replace_input: PixelRect,
    pub btn_case: PixelRect,
    pub btn_prev: PixelRect,
    pub btn_next: PixelRect,
    pub btn_replace: PixelRect,
    pub btn_replace_all: PixelRect,
    pub btn_close: PixelRect,
}

pub struct FindBarState {
    pub visible: bool,
    pub show_replace: bool,
    pub find_input: TextInput,
    pub replace_input: TextInput,
    pub case_sensitive: bool,
    pub match_count: usize,
    pub active_match: Option<usize>,
    pub layout: FindBarLayout,
}

impl Default for FindBarState {
    fn default() -> Self {
        Self::new()
    }
}

impl FindBarState {
    pub fn new() -> Self {
        Self {
            visible: false,
            show_replace: false,
            find_input: TextInput::new("Find"),
            replace_input: TextInput::new("Replace"),
            case_sensitive: false,
            match_count: 0,
            active_match: None,
            layout: FindBarLayout::default(),
        }
    }

    pub fn open(&mut self, with_replace: bool, seed: Option<&str>) {
        self.visible = true;
        self.show_replace = with_replace;
        if let Some(s) = seed {
            if !s.is_empty() && !s.contains('\n') {
                self.find_input.set_text(s);
            }
        } else {
            self.find_input.select_all();
        }
        self.find_input.focused = true;
        self.replace_input.focused = false;
    }

    pub fn close(&mut self) {
        self.visible = false;
        self.find_input.focused = false;
        self.replace_input.focused = false;
    }

    pub fn is_focused(&self) -> bool {
        self.visible && (self.find_input.focused || self.replace_input.focused)
    }

    pub fn height_px(&self, scale: f32) -> i32 {
        if !self.visible {
            0
        } else if self.show_replace {
            px(70.0, scale)
        } else {
            px(38.0, scale)
        }
    }

    pub fn hit_test(&self, x_px: f32, y_px: f32) -> FindBarAction {
        if !self.visible {
            return FindBarAction::None;
        }
        let xi = x_px.round() as i32;
        let yi = y_px.round() as i32;
        let l = self.layout;
        if l.find_input.contains(xi, yi) {
            FindBarAction::FocusFind
        } else if self.show_replace && l.replace_input.contains(xi, yi) {
            FindBarAction::FocusReplace
        } else if l.btn_case.contains(xi, yi) {
            FindBarAction::ToggleCase
        } else if l.btn_prev.contains(xi, yi) {
            FindBarAction::FindPrev
        } else if l.btn_next.contains(xi, yi) {
            FindBarAction::FindNext
        } else if self.show_replace && l.btn_replace.contains(xi, yi) {
            FindBarAction::ReplaceOne
        } else if self.show_replace && l.btn_replace_all.contains(xi, yi) {
            FindBarAction::ReplaceAll
        } else if l.btn_close.contains(xi, yi) {
            FindBarAction::Close
        } else {
            FindBarAction::None
        }
    }

    pub fn draw(
        &mut self,
        painter: &mut Painter,
        rect: PixelRect,
        theme: &Theme,
        atlas: &mut GlyphAtlas,
        rasterizer: &mut dyn GlyphRasterizer,
        caret_visible: bool,
    ) {
        if !self.visible || rect.is_empty() {
            self.layout = FindBarLayout::default();
            return;
        }
        let scale = painter.scale();
        let hairline = (1.0 * scale).round().max(1.0) as i32;
        let pad = px(8.0, scale);
        let gap = px(6.0, scale);
        let row_h = px(24.0, scale);

        painter.fill_rect(rect, theme.page);
        painter.fill_rect(
            PixelRect::new(rect.x, rect.bottom() - hairline, rect.w, hairline),
            theme.border_subtle,
        );

        let y1 = rect.y + px(7.0, scale);
        let input_w = (rect.w - px(260.0, scale)).clamp(px(140.0, scale), px(340.0, scale));
        let find_rect = PixelRect::new(rect.x + pad, y1, input_w, row_h);
        self.find_input
            .draw(painter, find_rect, theme, atlas, rasterizer, caret_visible);

        let mut cx = find_rect.right() + gap;
        let btn_case = PixelRect::new(cx, y1, px(28.0, scale), row_h);
        draw_button(painter, btn_case, "Aa", self.case_sensitive, theme, atlas, rasterizer);
        cx = btn_case.right() + gap;

        let btn_prev = PixelRect::new(cx, y1, px(28.0, scale), row_h);
        draw_button(painter, btn_prev, "↑", false, theme, atlas, rasterizer);
        cx = btn_prev.right() + px(4.0, scale);

        let btn_next = PixelRect::new(cx, y1, px(28.0, scale), row_h);
        draw_button(painter, btn_next, "↓", false, theme, atlas, rasterizer);
        cx = btn_next.right() + px(10.0, scale);

        // Match counter label.
        let count_label = if self.find_input.text.is_empty() {
            String::new()
        } else if self.match_count == 0 {
            "No matches".to_string()
        } else if let Some(idx) = self.active_match {
            format!("{} of {}", idx + 1, self.match_count)
        } else {
            format!("{} matches", self.match_count)
        };
        if !count_label.is_empty() {
            let font_px = 11.5 * scale;
            let m = atlas.metrics(rasterizer, FontStyle::Ui, font_px);
            let baseline = y1 as f32 + (row_h as f32 + m.ascent - m.descent) * 0.5;
            let color = if self.match_count == 0 {
                theme.state_danger
            } else {
                theme.text_muted
            };
            painter.draw_text_ui(
                cx as f32,
                baseline,
                &count_label,
                FontStyle::Ui,
                font_px,
                color,
                atlas,
                rasterizer,
            );
        }

        let btn_close = PixelRect::new(rect.right() - pad - px(24.0, scale), y1, px(24.0, scale), row_h);
        draw_button(painter, btn_close, "×", false, theme, atlas, rasterizer);

        let mut replace_rect = PixelRect::ZERO;
        let mut btn_replace = PixelRect::ZERO;
        let mut btn_replace_all = PixelRect::ZERO;

        if self.show_replace {
            let y2 = y1 + row_h + px(6.0, scale);
            replace_rect = PixelRect::new(rect.x + pad, y2, input_w, row_h);
            self.replace_input
                .draw(painter, replace_rect, theme, atlas, rasterizer, caret_visible);
            let rx = replace_rect.right() + gap;
            btn_replace = PixelRect::new(rx, y2, px(64.0, scale), row_h);
            draw_button(painter, btn_replace, "Replace", false, theme, atlas, rasterizer);
            btn_replace_all = PixelRect::new(btn_replace.right() + gap, y2, px(76.0, scale), row_h);
            draw_button(
                painter,
                btn_replace_all,
                "Replace All",
                false,
                theme,
                atlas,
                rasterizer,
            );
        }

        self.layout = FindBarLayout {
            bar: rect,
            find_input: find_rect,
            replace_input: replace_rect,
            btn_case,
            btn_prev,
            btn_next,
            btn_replace,
            btn_replace_all,
            btn_close,
        };
    }
}

fn draw_button(
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
    let bg = if active {
        theme.selection
    } else {
        theme.surface
    };
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
    painter.draw_text_ui(tx, baseline, label, FontStyle::Ui, font_px, theme.text, atlas, rasterizer);
}

// ============================================================================
// Go-To-Line Inline Bar
// ============================================================================

pub struct GotoLineState {
    pub visible: bool,
    pub input: TextInput,
}

impl Default for GotoLineState {
    fn default() -> Self {
        Self::new()
    }
}

impl GotoLineState {
    pub fn new() -> Self {
        Self {
            visible: false,
            input: TextInput::new("Go to line number"),
        }
    }

    pub fn open(&mut self, current_line_1based: usize) {
        self.visible = true;
        self.input.set_text(current_line_1based.to_string());
        self.input.focused = true;
    }

    pub fn close(&mut self) {
        self.visible = false;
        self.input.focused = false;
    }

    pub fn parsed_line_1based(&self) -> Option<usize> {
        self.input.text.trim().parse::<usize>().ok().filter(|&n| n >= 1)
    }

    pub fn height_px(&self, scale: f32) -> i32 {
        if self.visible {
            px(38.0, scale)
        } else {
            0
        }
    }

    pub fn draw(
        &mut self,
        painter: &mut Painter,
        rect: PixelRect,
        theme: &Theme,
        atlas: &mut GlyphAtlas,
        rasterizer: &mut dyn GlyphRasterizer,
        caret_visible: bool,
    ) {
        if !self.visible || rect.is_empty() {
            return;
        }
        let scale = painter.scale();
        let hairline = (1.0 * scale).round().max(1.0) as i32;
        painter.fill_rect(rect, theme.page);
        painter.fill_rect(
            PixelRect::new(rect.x, rect.bottom() - hairline, rect.w, hairline),
            theme.border_subtle,
        );
        let input_rect = PixelRect::new(
            rect.x + px(8.0, scale),
            rect.y + px(7.0, scale),
            px(220.0, scale).min((rect.w - px(16.0, scale)).max(0)),
            px(24.0, scale),
        );
        self.input
            .draw(painter, input_rect, theme, atlas, rasterizer, caret_visible);
    }
}

// ============================================================================
// Status Bar (Build Useful quiet bottom chrome)
// ============================================================================

pub struct StatusBarData<'a> {
    pub file_label: &'a str,
    pub dirty: bool,
    pub line_1based: usize,
    pub col_1based: usize,
    pub selection_bytes: usize,
    pub language: Language,
    pub eol: Eol,
    pub word_wrap: bool,
    pub theme_pref: ThemePreference,
    pub notice: Option<(&'a str, bool)>, // (message, is_error)
}

pub fn status_bar_height_px(scale: f32) -> i32 {
    px(24.0, scale)
}

pub fn draw_status_bar(
    painter: &mut Painter,
    rect: PixelRect,
    data: &StatusBarData<'_>,
    theme: &Theme,
    atlas: &mut GlyphAtlas,
    rasterizer: &mut dyn GlyphRasterizer,
) {
    if rect.is_empty() {
        return;
    }
    let scale = painter.scale();
    let hairline = (1.0 * scale).round().max(1.0) as i32;
    painter.fill_rect(rect, theme.page);
    painter.fill_rect(
        PixelRect::new(rect.x, rect.y, rect.w, hairline),
        theme.border_subtle,
    );

    let font_px = 11.5 * scale;
    let metrics = atlas.metrics(rasterizer, FontStyle::Ui, font_px);
    let baseline = rect.y as f32 + (rect.h as f32 + metrics.ascent - metrics.descent) * 0.5;
    let pad = px(12.0, scale) as f32;

    // Left: filename + modified indicator + optional transient notice.
    let left_text = if data.dirty {
        format!("{} • Modified", data.file_label)
    } else {
        data.file_label.to_string()
    };
    let mut lx = painter.draw_text_ui(
        rect.x as f32 + pad,
        baseline,
        &left_text,
        FontStyle::Ui,
        font_px,
        theme.text,
        atlas,
        rasterizer,
    );

    if let Some((msg, is_err)) = data.notice {
        lx += px(14.0, scale) as f32;
        let color = if is_err {
            theme.state_danger
        } else {
            theme.state_success
        };
        painter.draw_text_ui(
            lx,
            baseline,
            msg,
            FontStyle::Ui,
            font_px,
            color,
            atlas,
            rasterizer,
        );
    }

    // Right: technical status values.
    let pos_text = if data.selection_bytes > 0 {
        format!(
            "Ln {}, Col {} ({} B)",
            data.line_1based, data.col_1based, data.selection_bytes
        )
    } else {
        format!("Ln {}, Col {}", data.line_1based, data.col_1based)
    };
    let wrap_text = if data.word_wrap { "Wrap" } else { "No Wrap" };
    let theme_text = match data.theme_pref {
        ThemePreference::System => "System",
        ThemePreference::Light => "Light",
        ThemePreference::Dark => "Dark",
    };
    let right_text = format!(
        "{}   {}   UTF-8   {}   {}   {}",
        pos_text,
        data.language.display_name(),
        data.eol.display_badge(),
        wrap_text,
        theme_text
    );
    let rw = Painter::measure_text_ui(&right_text, FontStyle::Ui, font_px, atlas, rasterizer);
    let rx = (rect.right() as f32 - pad - rw).max(lx + pad);
    painter.draw_text_ui(
        rx,
        baseline,
        &right_text,
        FontStyle::Ui,
        font_px,
        theme.text_muted,
        atlas,
        rasterizer,
    );
}
