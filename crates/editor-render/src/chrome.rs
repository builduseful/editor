//! Custom-drawn chrome: Find/Replace bar, Go-To-Line prompt, Status bar,
//! and future-ready IME & Accessibility seams.

use editor_core::{Eol, Language};
use editor_platform::{ConfirmChoice, FontStyle, GlyphRasterizer, PixelRect, ThemePreference};

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
    ToggleWholeWord,
    ToggleRegex,
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
    pub btn_word: PixelRect,
    pub btn_regex: PixelRect,
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
    pub whole_word: bool,
    pub regex: bool,
    pub regex_error: Option<String>,
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
            whole_word: false,
            regex: false,
            regex_error: None,
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
        } else if l.btn_word.contains(xi, yi) {
            FindBarAction::ToggleWholeWord
        } else if l.btn_regex.contains(xi, yi) {
            FindBarAction::ToggleRegex
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
        let input_w = (rect.w - px(340.0, scale)).clamp(px(120.0, scale), px(320.0, scale));
        let find_rect = PixelRect::new(rect.x + pad, y1, input_w, row_h);
        self.find_input
            .draw(painter, find_rect, theme, atlas, rasterizer, caret_visible);

        let mut cx = find_rect.right() + gap;
        let btn_case = PixelRect::new(cx, y1, px(28.0, scale), row_h);
        draw_button(painter, btn_case, "Aa", self.case_sensitive, theme, atlas, rasterizer);
        cx = btn_case.right() + px(4.0, scale);

        let btn_word = PixelRect::new(cx, y1, px(26.0, scale), row_h);
        draw_button(painter, btn_word, "W", self.whole_word, theme, atlas, rasterizer);
        cx = btn_word.right() + px(4.0, scale);

        let btn_regex = PixelRect::new(cx, y1, px(28.0, scale), row_h);
        draw_button(painter, btn_regex, ".*", self.regex, theme, atlas, rasterizer);
        cx = btn_regex.right() + gap;

        let btn_prev = PixelRect::new(cx, y1, px(28.0, scale), row_h);
        draw_button(painter, btn_prev, "↑", false, theme, atlas, rasterizer);
        cx = btn_prev.right() + px(4.0, scale);

        let btn_next = PixelRect::new(cx, y1, px(28.0, scale), row_h);
        draw_button(painter, btn_next, "↓", false, theme, atlas, rasterizer);
        cx = btn_next.right() + px(10.0, scale);

        // Match counter or quiet inline regex error label.
        let (count_label, is_err) = if let Some(ref err) = self.regex_error {
            (err.clone(), true)
        } else if self.find_input.text.is_empty() {
            (String::new(), false)
        } else if self.match_count == 0 {
            ("No matches".to_string(), true)
        } else if let Some(idx) = self.active_match {
            (format!("{} of {}", idx + 1, self.match_count), false)
        } else {
            (format!("{} matches", self.match_count), false)
        };
        if !count_label.is_empty() {
            let font_px = 11.5 * scale;
            let m = atlas.metrics(rasterizer, FontStyle::Ui, font_px);
            let baseline = y1 as f32 + (row_h as f32 + m.ascent - m.descent) * 0.5;
            let color = if is_err {
                theme.state_danger
            } else {
                theme.text_muted
            };
            let max_label_x = rect.right() - pad - px(30.0, scale);
            if cx < max_label_x {
                painter.push_clip(PixelRect::new(cx, y1, max_label_x - cx, row_h));
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
                painter.pop_clip();
            }
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
            btn_word,
            btn_regex,
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
    pub extra_cursor_count: usize,
    pub language: Language,
    pub eol: Eol,
    pub word_wrap: bool,
    pub theme_pref: ThemePreference,
    pub notice: Option<(&'a str, bool)>, // (message, is_error)
    pub vim_badge: Option<&'a str>,
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

    // Left: optional Vim mode pill + filename + modified indicator + optional transient notice.
    let mut lx = rect.x as f32 + pad;
    if let Some(vim) = data.vim_badge {
        let badge_font = 10.5 * scale;
        let bm = atlas.metrics(rasterizer, FontStyle::MonoBold, badge_font);
        let bw = Painter::measure_text_ui(vim, FontStyle::MonoBold, badge_font, atlas, rasterizer)
            .ceil() as i32;
        let pill_pad_x = px(6.0, scale);
        let pill_w = bw + pill_pad_x * 2;
        let pill_h = px(17.0, scale);
        let pill_rect = PixelRect::new(
            lx.round() as i32,
            rect.y + (rect.h - pill_h) / 2,
            pill_w,
            pill_h,
        );
        painter.fill_rounded_rect(pill_rect, 3.5 * scale, theme.surface);
        painter.stroke_rounded_rect(pill_rect, 3.5 * scale, hairline as f32, theme.border_control);
        let bbase = pill_rect.y as f32 + (pill_rect.h as f32 + bm.ascent - bm.descent) * 0.5;
        painter.draw_text_ui(
            (pill_rect.x + pill_pad_x) as f32,
            bbase,
            vim,
            FontStyle::MonoBold,
            badge_font,
            theme.text,
            atlas,
            rasterizer,
        );
        lx = pill_rect.right() as f32 + px(10.0, scale) as f32;
    }

    let left_text = if data.dirty {
        format!("{} • Modified", data.file_label)
    } else {
        data.file_label.to_string()
    };
    lx = painter.draw_text_ui(
        lx,
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
    let base_pos = if data.selection_bytes > 0 {
        format!(
            "Ln {}, Col {} ({} B)",
            data.line_1based, data.col_1based, data.selection_bytes
        )
    } else {
        format!("Ln {}, Col {}", data.line_1based, data.col_1based)
    };
    let pos_text = if data.extra_cursor_count > 0 {
        format!("{} cursors   {base_pos}", data.extra_cursor_count + 1)
    } else {
        base_pos
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

// ============================================================================
// Custom-Drawn In-App Confirmation Modal (100% OS-Independent)
// ============================================================================

#[derive(Debug, Default)]
pub struct ConfirmModal {
    pub visible: bool,
    pub file_name: String,
    pub is_delete: bool,
    pub delete_is_dir: bool,
    pub is_move_confirm: bool,
    pub move_body: String,
    pub btn_save: PixelRect,
    pub btn_discard: PixelRect,
    pub btn_cancel: PixelRect,
}

impl ConfirmModal {
    pub fn open(&mut self, file_name: &str) {
        self.visible = true;
        self.is_delete = false;
        self.delete_is_dir = false;
        self.is_move_confirm = false;
        self.move_body.clear();
        self.file_name = file_name.to_string();
    }

    pub fn open_delete(&mut self, name: &str, is_dir: bool) {
        self.visible = true;
        self.is_delete = true;
        self.delete_is_dir = is_dir;
        self.is_move_confirm = false;
        self.move_body.clear();
        self.file_name = name.to_string();
    }

    pub fn open_confirm_move(&mut self, name: &str, dest_name: &str, summary: &str) {
        self.visible = true;
        self.is_delete = false;
        self.delete_is_dir = false;
        self.is_move_confirm = true;
        self.file_name = name.to_string();
        self.move_body = format!("Move \"{name}\" ({summary}) into \"{dest_name}\"?");
    }

    pub fn close(&mut self) {
        self.visible = false;
        self.is_delete = false;
        self.is_move_confirm = false;
    }

    pub fn hit_test(&self, x_px: f32, y_px: f32) -> Option<ConfirmChoice> {
        if !self.visible {
            return None;
        }
        let xi = x_px.round() as i32;
        let yi = y_px.round() as i32;
        if !self.btn_save.is_empty() && self.btn_save.contains(xi, yi) {
            Some(ConfirmChoice::Save)
        } else if self.btn_discard.contains(xi, yi) {
            Some(ConfirmChoice::Discard)
        } else if self.btn_cancel.contains(xi, yi) {
            Some(ConfirmChoice::Cancel)
        } else {
            None
        }
    }

    pub fn draw(
        &mut self,
        painter: &mut Painter,
        bounds: PixelRect,
        theme: &Theme,
        atlas: &mut GlyphAtlas,
        rasterizer: &mut dyn GlyphRasterizer,
    ) {
        if !self.visible || bounds.is_empty() {
            return;
        }
        let scale = painter.scale();
        // Subtle scrim over window.
        let scrim = if theme.is_dark {
            crate::theme::Color::rgba(0, 0, 0, 140)
        } else {
            crate::theme::Color::rgba(0x17, 0x1a, 0x1f, 80)
        };
        painter.fill_rect(bounds, scrim);

        let card_w = px(400.0, scale).min(bounds.w - px(32.0, scale));
        let card_h = px(136.0, scale);
        let card = PixelRect::new(
            bounds.x + (bounds.w - card_w) / 2,
            bounds.y + (bounds.h - card_h) / 3,
            card_w,
            card_h,
        );
        let radius = 8.0 * scale;
        painter.fill_rounded_rect(card, radius, theme.surface);
        painter.stroke_rounded_rect(card, radius, (1.0 * scale).max(1.0), theme.border_control);

        let pad = px(18.0, scale);
        let title_px = 14.0 * scale;
        let body_px = 12.0 * scale;
        let tm = atlas.metrics(rasterizer, FontStyle::UiBold, title_px);
        let bm = atlas.metrics(rasterizer, FontStyle::Ui, body_px);

        let (heading, body): (String, &str) = if self.is_delete {
            (
                format!("Delete {}?", self.file_name),
                if self.delete_is_dir {
                    "This folder and all its contents will be permanently removed."
                } else {
                    "This file will be permanently removed."
                },
            )
        } else if self.is_move_confirm {
            ("Move Large Folder?".to_string(), self.move_body.as_str())
        } else {
            (
                format!("Save Changes to {}?", self.file_name),
                "Your unsaved changes will be lost if you do not save them.",
            )
        };
        painter.push_clip(PixelRect::new(card.x + pad, card.y, (card.w - pad * 2).max(0), card.h));
        painter.draw_text_ui(
            (card.x + pad) as f32,
            (card.y + pad) as f32 + tm.ascent,
            &heading,
            FontStyle::UiBold,
            title_px,
            theme.text,
            atlas,
            rasterizer,
        );
        painter.draw_text_ui(
            (card.x + pad) as f32,
            (card.y + pad + px(28.0, scale)) as f32 + bm.ascent,
            body,
            FontStyle::Ui,
            body_px,
            theme.text_muted,
            atlas,
            rasterizer,
        );
        painter.pop_clip();

        let btn_h = px(28.0, scale);
        let btn_y = card.bottom() - pad - btn_h;
        let gap = px(8.0, scale);
        let cancel_w = px(72.0, scale);

        if self.is_delete {
            let del_w = px(82.0, scale);
            self.btn_save = PixelRect::ZERO;
            self.btn_discard = PixelRect::new(card.right() - pad - del_w, btn_y, del_w, btn_h);
            self.btn_cancel = PixelRect::new(card.x + pad, btn_y, cancel_w, btn_h);
            draw_button(painter, self.btn_cancel, "Cancel", false, theme, atlas, rasterizer);
            // Danger action button using Build Useful `state_danger` token.
            let r = 4.0 * scale;
            painter.fill_rounded_rect(self.btn_discard, r, theme.surface);
            painter.stroke_rounded_rect(
                self.btn_discard,
                r,
                (1.0 * scale).max(1.0),
                theme.state_danger,
            );
            let font_px = 11.5 * scale;
            let m = atlas.metrics(rasterizer, FontStyle::UiBold, font_px);
            let tw = Painter::measure_text_ui("Delete", FontStyle::UiBold, font_px, atlas, rasterizer);
            let tx = self.btn_discard.x as f32 + (self.btn_discard.w as f32 - tw) * 0.5;
            let baseline = self.btn_discard.y as f32 + (self.btn_discard.h as f32 + m.ascent - m.descent) * 0.5;
            painter.draw_text_ui(
                tx,
                baseline,
                "Delete",
                FontStyle::UiBold,
                font_px,
                theme.state_danger,
                atlas,
                rasterizer,
            );
        } else if self.is_move_confirm {
            let move_w = px(82.0, scale);
            self.btn_save = PixelRect::ZERO;
            self.btn_discard = PixelRect::new(card.right() - pad - move_w, btn_y, move_w, btn_h);
            self.btn_cancel = PixelRect::new(card.x + pad, btn_y, cancel_w, btn_h);
            draw_button(painter, self.btn_cancel, "Cancel", false, theme, atlas, rasterizer);
            draw_button(painter, self.btn_discard, "Move", true, theme, atlas, rasterizer);
        } else {
            let save_w = px(72.0, scale);
            let discard_w = px(92.0, scale);
            self.btn_save = PixelRect::new(card.right() - pad - save_w, btn_y, save_w, btn_h);
            self.btn_discard = PixelRect::new(self.btn_save.x - gap - discard_w, btn_y, discard_w, btn_h);
            self.btn_cancel = PixelRect::new(card.x + pad, btn_y, cancel_w, btn_h);

            draw_button(painter, self.btn_cancel, "Cancel", false, theme, atlas, rasterizer);
            draw_button(painter, self.btn_discard, "Don't Save", false, theme, atlas, rasterizer);
            draw_button(painter, self.btn_save, "Save", true, theme, atlas, rasterizer);
        }
    }
}

// ============================================================================
// Quiet Gmail-Style Floating Action Toast with Inline `Undo` Button
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastHit {
    None,
    Body,
    Undo,
    Close,
}

#[derive(Debug, Default)]
pub struct ActionToast {
    pub visible: bool,
    pub message: String,
    pub can_undo: bool,
    pub is_error: bool,
    pub expires_at_ms: u64,
    hovered_undo: bool,
    hovered_close: bool,
    pub card_rect: PixelRect,
    undo_rect: PixelRect,
    close_rect: PixelRect,
}

impl ActionToast {
    pub fn show(&mut self, message: impl Into<String>, can_undo: bool, is_error: bool, now_ms: u64) {
        self.visible = true;
        self.message = message.into();
        self.can_undo = can_undo;
        self.is_error = is_error;
        self.expires_at_ms = now_ms.saturating_add(6_000);
        self.hovered_undo = false;
        self.hovered_close = false;
    }

    pub fn dismiss(&mut self) -> bool {
        let was = self.visible;
        self.visible = false;
        self.hovered_undo = false;
        self.hovered_close = false;
        self.card_rect = PixelRect::ZERO;
        self.undo_rect = PixelRect::ZERO;
        self.close_rect = PixelRect::ZERO;
        was
    }

    pub fn expire_if_needed(&mut self, now_ms: u64) -> bool {
        if self.visible && now_ms >= self.expires_at_ms {
            self.dismiss();
            return true;
        }
        false
    }

    pub fn hit_test(&self, x_px: f32, y_px: f32) -> ToastHit {
        if !self.visible || self.card_rect.is_empty() {
            return ToastHit::None;
        }
        let xi = x_px.round() as i32;
        let yi = y_px.round() as i32;
        if !self.card_rect.contains(xi, yi) {
            return ToastHit::None;
        }
        if self.can_undo && self.undo_rect.contains(xi, yi) {
            ToastHit::Undo
        } else if self.close_rect.contains(xi, yi) {
            ToastHit::Close
        } else {
            ToastHit::Body
        }
    }

    pub fn set_hovered(&mut self, x_px: f32, y_px: f32) -> (bool, ToastHit) {
        let hit = self.hit_test(x_px, y_px);
        let u = hit == ToastHit::Undo;
        let c = hit == ToastHit::Close;
        let changed = self.hovered_undo != u || self.hovered_close != c;
        self.hovered_undo = u;
        self.hovered_close = c;
        (changed, hit)
    }

    pub fn clear_hover(&mut self) -> bool {
        let changed = self.hovered_undo || self.hovered_close;
        self.hovered_undo = false;
        self.hovered_close = false;
        changed
    }

    pub fn draw(
        &mut self,
        painter: &mut Painter,
        viewport_bounds: PixelRect,
        theme: &Theme,
        atlas: &mut GlyphAtlas,
        rasterizer: &mut dyn GlyphRasterizer,
    ) {
        if !self.visible || viewport_bounds.is_empty() {
            self.card_rect = PixelRect::ZERO;
            self.undo_rect = PixelRect::ZERO;
            self.close_rect = PixelRect::ZERO;
            return;
        }
        let scale = painter.scale();
        let font_px = 12.0 * scale;
        let m = atlas.metrics(rasterizer, FontStyle::Ui, font_px);
        let msg_w = Painter::measure_text_ui(&self.message, FontStyle::Ui, font_px, atlas, rasterizer);
        let pad_x = px(12.0, scale);
        let gap = px(10.0, scale);
        let undo_w = if self.can_undo { px(46.0, scale) } else { 0 };
        let close_w = px(20.0, scale);
        let card_h = px(32.0, scale);
        let max_w = (viewport_bounds.w - px(28.0, scale)).max(px(160.0, scale));
        let desired_w = pad_x
            + msg_w.ceil() as i32
            + if self.can_undo { gap + undo_w } else { 0 }
            + px(6.0, scale)
            + close_w
            + px(8.0, scale);
        let card_w = desired_w.min(max_w);
        let card_x = viewport_bounds.x + px(14.0, scale);
        let card_y = viewport_bounds.bottom() - card_h - px(12.0, scale);
        let card = PixelRect::new(card_x, card_y, card_w, card_h);
        self.card_rect = card;

        let radius = 6.0 * scale;
        let border = if self.is_error {
            theme.state_danger
        } else {
            theme.border_control
        };
        painter.fill_rounded_rect(card, radius, theme.surface);
        painter.stroke_rounded_rect(card, radius, (1.0 * scale).max(1.0), border);

        let close_h = px(20.0, scale);
        self.close_rect = PixelRect::new(
            card.right() - px(6.0, scale) - close_w,
            card.y + (card.h - close_h) / 2,
            close_w,
            close_h,
        );
        let mut right_edge = self.close_rect.x - px(4.0, scale);

        if self.can_undo {
            let uh = px(22.0, scale);
            self.undo_rect = PixelRect::new(
                right_edge - undo_w,
                card.y + (card.h - uh) / 2,
                undo_w,
                uh,
            );
            let bg = if self.hovered_undo {
                theme.selection
            } else {
                theme.current_line
            };
            painter.fill_rounded_rect(self.undo_rect, 4.0 * scale, bg);
            painter.stroke_rounded_rect(
                self.undo_rect,
                4.0 * scale,
                (1.0 * scale).max(1.0),
                theme.border_subtle,
            );
            let uw = Painter::measure_text_ui("Undo", FontStyle::UiBold, font_px, atlas, rasterizer);
            let ubase = self.undo_rect.y as f32 + (self.undo_rect.h as f32 + m.ascent - m.descent) * 0.5;
            painter.draw_text_ui(
                self.undo_rect.x as f32 + (self.undo_rect.w as f32 - uw) * 0.5,
                ubase,
                "Undo",
                FontStyle::UiBold,
                font_px,
                theme.text,
                atlas,
                rasterizer,
            );
            right_edge = self.undo_rect.x - gap;
        } else {
            self.undo_rect = PixelRect::ZERO;
        }

        if self.hovered_close {
            painter.fill_rounded_rect(self.close_rect, 4.0 * scale, theme.current_line);
        }
        let xw = Painter::measure_text_ui("×", FontStyle::Ui, font_px, atlas, rasterizer);
        let xbase = self.close_rect.y as f32 + (self.close_rect.h as f32 + m.ascent - m.descent) * 0.5;
        painter.draw_text_ui(
            self.close_rect.x as f32 + (self.close_rect.w as f32 - xw) * 0.5,
            xbase,
            "×",
            FontStyle::Ui,
            font_px,
            if self.hovered_close {
                theme.text
            } else {
                theme.text_muted
            },
            atlas,
            rasterizer,
        );

        let text_clip = PixelRect::new(
            card.x + pad_x,
            card.y,
            (right_edge - (card.x + pad_x)).max(0),
            card.h,
        );
        painter.push_clip(text_clip);
        let baseline = card.y as f32 + (card.h as f32 + m.ascent - m.descent) * 0.5;
        let msg_col = if self.is_error {
            theme.state_danger
        } else {
            theme.text
        };
        painter.draw_text_ui(
            text_clip.x as f32,
            baseline,
            &self.message,
            FontStyle::Ui,
            font_px,
            msg_col,
            atlas,
            rasterizer,
        );
        painter.pop_clip();
    }
}
