//! Custom-drawn single-line text input widget (used for Find, Replace, and Go-To-Line).
//!
//! Zero native `NSTextField` dependency — full grapheme-aware caret, selection,
//! keyboard editing, mouse placement, and clipboard support rendered with Build Useful tokens.

use editor_core::{next_grapheme_boundary, next_word_boundary, prev_grapheme_boundary, prev_word_boundary};
use editor_platform::{FontStyle, GlyphRasterizer, Key, Modifiers, PixelRect};

use crate::atlas::GlyphAtlas;
use crate::painter::Painter;
use crate::theme::{px, Theme};

#[derive(Debug, Clone)]
pub struct TextInput {
    pub text: String,
    pub placeholder: String,
    pub caret: usize,
    pub anchor: usize,
    pub scroll_x: f32,
    pub focused: bool,
}

impl TextInput {
    pub fn new(placeholder: impl Into<String>) -> Self {
        Self {
            text: String::new(),
            placeholder: placeholder.into(),
            caret: 0,
            anchor: 0,
            scroll_x: 0.0,
            focused: false,
        }
    }

    pub fn set_text(&mut self, value: impl Into<String>) {
        self.text = value.into();
        self.caret = self.text.len();
        self.anchor = 0;
        self.scroll_x = 0.0;
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.caret = self.text.len();
    }

    pub fn selection_range(&self) -> (usize, usize) {
        if self.anchor <= self.caret {
            (self.anchor, self.caret)
        } else {
            (self.caret, self.anchor)
        }
    }

    pub fn has_selection(&self) -> bool {
        self.anchor != self.caret
    }

    pub fn selected_text(&self) -> &str {
        let (a, b) = self.selection_range();
        &self.text[a..b]
    }

    pub fn insert_str(&mut self, s: &str) {
        // Strip newlines for single-line inputs.
        let clean: String = s.chars().filter(|&c| c != '\n' && c != '\r').collect();
        let (a, b) = self.selection_range();
        if a != b {
            self.text.replace_range(a..b, &clean);
            self.caret = a + clean.len();
        } else {
            self.text.insert_str(self.caret, &clean);
            self.caret += clean.len();
        }
        self.anchor = self.caret;
    }

    pub fn delete_selection(&mut self) -> bool {
        let (a, b) = self.selection_range();
        if a == b {
            return false;
        }
        self.text.replace_range(a..b, "");
        self.caret = a;
        self.anchor = a;
        true
    }

    /// Hit-test a physical X coordinate inside `rect` to place or drag the caret.
    pub fn hit_test_x(
        &self,
        rect: PixelRect,
        x_px: f32,
        scale: f32,
        atlas: &mut GlyphAtlas,
        rasterizer: &mut dyn GlyphRasterizer,
    ) -> usize {
        let pad_x = px(8.0, scale) as f32;
        let font_px = 12.5 * scale;
        let rel_x = (x_px - (rect.x as f32 + pad_x) + self.scroll_x).max(0.0);
        let mut pen = 0.0f32;
        for (off, cluster) in editor_core::grapheme_clusters(&self.text) {
            let g = atlas.get_or_rasterize(rasterizer, FontStyle::Ui, font_px, cluster);
            if rel_x < pen + g.advance * 0.5 {
                return off;
            }
            pen += g.advance;
        }
        self.text.len()
    }

    pub fn mouse_down(
        &mut self,
        rect: PixelRect,
        x_px: f32,
        scale: f32,
        extend_selection: bool,
        atlas: &mut GlyphAtlas,
        rasterizer: &mut dyn GlyphRasterizer,
    ) {
        let pos = self.hit_test_x(rect, x_px, scale, atlas, rasterizer);
        self.caret = pos;
        if !extend_selection {
            self.anchor = pos;
        }
    }

    pub fn mouse_drag(
        &mut self,
        rect: PixelRect,
        x_px: f32,
        scale: f32,
        atlas: &mut GlyphAtlas,
        rasterizer: &mut dyn GlyphRasterizer,
    ) {
        self.caret = self.hit_test_x(rect, x_px, scale, atlas, rasterizer);
    }

    /// Handle a keystroke. Returns `true` if the text content changed.
    pub fn handle_key(
        &mut self,
        key: Key,
        mods: Modifiers,
        text: Option<&str>,
        clipboard: Option<&str>,
    ) -> (bool, Option<String>) {
        let mut changed = false;
        let mut copied: Option<String> = None;

        if mods.primary() {
            match key {
                Key::Char('a') | Key::Char('A') => {
                    self.select_all();
                    return (false, None);
                }
                Key::Char('c') | Key::Char('C') => {
                    if self.has_selection() {
                        copied = Some(self.selected_text().to_string());
                    }
                    return (false, copied);
                }
                Key::Char('x') | Key::Char('X') => {
                    if self.has_selection() {
                        copied = Some(self.selected_text().to_string());
                        let (a, b) = self.selection_range();
                        self.text.replace_range(a..b, "");
                        self.caret = a;
                        self.anchor = a;
                        changed = true;
                    }
                    return (changed, copied);
                }
                Key::Char('v') | Key::Char('V') => {
                    if let Some(clip) = clipboard {
                        self.insert_str(clip);
                        changed = true;
                    }
                    return (changed, None);
                }
                Key::Left => {
                    self.caret = 0;
                    if !mods.shift {
                        self.anchor = self.caret;
                    }
                    return (false, None);
                }
                Key::Right => {
                    self.caret = self.text.len();
                    if !mods.shift {
                        self.anchor = self.caret;
                    }
                    return (false, None);
                }
                _ => {}
            }
        }

        match key {
            Key::Left => {
                if self.has_selection() && !mods.shift {
                    let (a, _) = self.selection_range();
                    self.caret = a;
                    self.anchor = a;
                } else {
                    self.caret = if mods.alt {
                        prev_word_boundary(&self.text, self.caret)
                    } else {
                        prev_grapheme_boundary(&self.text, self.caret)
                    };
                    if !mods.shift {
                        self.anchor = self.caret;
                    }
                }
            }
            Key::Right => {
                if self.has_selection() && !mods.shift {
                    let (_, b) = self.selection_range();
                    self.caret = b;
                    self.anchor = b;
                } else {
                    self.caret = if mods.alt {
                        next_word_boundary(&self.text, self.caret)
                    } else {
                        next_grapheme_boundary(&self.text, self.caret)
                    };
                    if !mods.shift {
                        self.anchor = self.caret;
                    }
                }
            }
            Key::Home => {
                self.caret = 0;
                if !mods.shift {
                    self.anchor = 0;
                }
            }
            Key::End => {
                self.caret = self.text.len();
                if !mods.shift {
                    self.anchor = self.caret;
                }
            }
            Key::Backspace => {
                if self.has_selection() {
                    let (a, b) = self.selection_range();
                    self.text.replace_range(a..b, "");
                    self.caret = a;
                    self.anchor = a;
                    changed = true;
                } else if self.caret > 0 {
                    let prev = if mods.alt {
                        prev_word_boundary(&self.text, self.caret)
                    } else {
                        prev_grapheme_boundary(&self.text, self.caret)
                    };
                    self.text.replace_range(prev..self.caret, "");
                    self.caret = prev;
                    self.anchor = prev;
                    changed = true;
                }
            }
            Key::Delete => {
                if self.has_selection() {
                    let (a, b) = self.selection_range();
                    self.text.replace_range(a..b, "");
                    self.caret = a;
                    self.anchor = a;
                    changed = true;
                } else if self.caret < self.text.len() {
                    let next = if mods.alt {
                        next_word_boundary(&self.text, self.caret)
                    } else {
                        next_grapheme_boundary(&self.text, self.caret)
                    };
                    self.text.replace_range(self.caret..next, "");
                    changed = true;
                }
            }
            Key::Char(_) => {
                if !mods.primary() && !mods.ctrl {
                    if let Some(s) = text {
                        if !s.is_empty()
                            && !s
                                .chars()
                                .all(|c| c.is_control() || matches!(c, '\u{F700}'..='\u{F8FF}'))
                        {
                            self.insert_str(s);
                            changed = true;
                        }
                    }
                }
            }
            _ => {}
        }
        (changed, copied)
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
        let scale = painter.scale();
        let radius = 4.0 * scale;
        let pad_x = px(8.0, scale);
        let font_px = 12.5 * scale;
        let metrics = atlas.metrics(rasterizer, FontStyle::Ui, font_px);

        painter.fill_rounded_rect(rect, radius, theme.surface);
        let border_color = if self.focused {
            theme.border_control
        } else {
            theme.border_subtle
        };
        painter.stroke_rounded_rect(rect, radius, (1.0 * scale).max(1.0), border_color);

        let inner = PixelRect::new(
            rect.x + pad_x,
            rect.y + 1,
            (rect.w - pad_x * 2).max(0),
            (rect.h - 2).max(0),
        );
        painter.push_clip(inner);

        let caret_x_rel = Painter::measure_text_ui(
            &self.text[..self.caret],
            FontStyle::Ui,
            font_px,
            atlas,
            rasterizer,
        );
        let total_w = Painter::measure_text_ui(&self.text, FontStyle::Ui, font_px, atlas, rasterizer);
        let inner_w = inner.w as f32;
        let max_scroll = (total_w - inner_w + 8.0 * scale).max(0.0);
        self.scroll_x = self.scroll_x.clamp(0.0, max_scroll);
        if caret_x_rel - self.scroll_x > inner_w - 4.0 * scale {
            self.scroll_x = (caret_x_rel - inner_w + 8.0 * scale).clamp(0.0, max_scroll);
        } else if caret_x_rel < self.scroll_x {
            self.scroll_x = (caret_x_rel - 4.0 * scale).max(0.0);
        }

        let baseline = rect.y as f32
            + (rect.h as f32 + metrics.ascent - metrics.descent) * 0.5;
        let text_x = inner.x as f32 - self.scroll_x;

        if self.text.is_empty() {
            painter.draw_text_ui(
                inner.x as f32,
                baseline,
                &self.placeholder,
                FontStyle::Ui,
                font_px,
                theme.text_muted,
                atlas,
                rasterizer,
            );
        } else {
            if self.has_selection() && self.focused {
                let (a, b) = self.selection_range();
                let sx0 = Painter::measure_text_ui(
                    &self.text[..a],
                    FontStyle::Ui,
                    font_px,
                    atlas,
                    rasterizer,
                );
                let sx1 = Painter::measure_text_ui(
                    &self.text[..b],
                    FontStyle::Ui,
                    font_px,
                    atlas,
                    rasterizer,
                );
                let sel_rect = PixelRect::new(
                    (text_x + sx0).round() as i32,
                    rect.y + px(3.0, scale),
                    (sx1 - sx0).round().max(1.0) as i32,
                    rect.h - px(6.0, scale),
                );
                painter.fill_rect(sel_rect, theme.selection);
            }
            painter.draw_text_ui(
                text_x,
                baseline,
                &self.text,
                FontStyle::Ui,
                font_px,
                theme.text,
                atlas,
                rasterizer,
            );
        }

        if self.focused && caret_visible {
            let cx = (text_x + caret_x_rel).round() as i32;
            let caret_w = (1.5 * scale).round().max(1.0) as i32;
            painter.fill_rect(
                PixelRect::new(cx, rect.y + px(4.0, scale), caret_w, rect.h - px(8.0, scale)),
                theme.text,
            );
        }

        painter.pop_clip();
    }
}
