//! Generic custom-drawn right-click context menu (`Build Useful` design system).
//!
//! Shared across all editor surfaces (file sidebar rows & empty space, Working-Set
//! tab strip, split-pane headers) so styling, dynamic width sizing, viewport clamping,
//! pointer hover, and keyboard navigation (`↑` / `↓` / `Enter` / `Esc`) live in one place.

use editor_platform::{FontStyle, GlyphRasterizer, PixelRect};

use crate::atlas::GlyphAtlas;
use crate::painter::Painter;
use crate::theme::{px, Theme};

/// A single entry in a `ContextMenu`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextMenuEntry {
    Item {
        id: u32,
        label: String,
        danger: bool,
    },
    Separator,
}

impl ContextMenuEntry {
    pub fn item(id: u32, label: impl Into<String>) -> Self {
        Self::Item {
            id,
            label: label.into(),
            danger: false,
        }
    }

    pub fn danger_item(id: u32, label: impl Into<String>) -> Self {
        Self::Item {
            id,
            label: label.into(),
            danger: true,
        }
    }
}

/// Custom-drawn floating context menu with dynamic label-based width measurement.
#[derive(Debug, Clone, Default)]
pub struct ContextMenu {
    open: bool,
    anchor_x: i32,
    anchor_y: i32,
    entries: Vec<ContextMenuEntry>,
    /// Index among clickable `ContextMenuEntry::Item` entries (ignoring separators).
    selected_item: Option<usize>,
    card_rect: PixelRect,
    item_rects: Vec<(u32, PixelRect)>,
}

impl ContextMenu {
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn open(&mut self, x_px: f32, y_px: f32, entries: Vec<ContextMenuEntry>) {
        let has_item = entries
            .iter()
            .any(|e| matches!(e, ContextMenuEntry::Item { .. }));
        if !has_item {
            self.close();
            return;
        }
        self.open = true;
        self.anchor_x = x_px.round() as i32;
        self.anchor_y = y_px.round() as i32;
        self.entries = entries;
        self.selected_item = None;
        self.card_rect = PixelRect::ZERO;
        self.item_rects.clear();
    }

    pub fn close(&mut self) {
        self.open = false;
        self.entries.clear();
        self.selected_item = None;
        self.card_rect = PixelRect::ZERO;
        self.item_rects.clear();
    }

    pub fn entries(&self) -> &[ContextMenuEntry] {
        &self.entries
    }

    pub fn card_rect(&self) -> PixelRect {
        self.card_rect
    }

    /// Return the rendered hit rectangle of the item with the given `id`, if laid out.
    pub fn item_rect(&self, id: u32) -> Option<PixelRect> {
        self.item_rects
            .iter()
            .find(|(item_id, _)| *item_id == id)
            .map(|(_, r)| *r)
    }

    /// Update pointer hover state. Returns `(selection_changed, over_clickable_item)`.
    pub fn hover(&mut self, x_px: f32, y_px: f32) -> (bool, bool) {
        if !self.open {
            return (false, false);
        }
        let xi = x_px.round() as i32;
        let yi = y_px.round() as i32;
        let next = self.item_rects.iter().position(|(_, r)| r.contains(xi, yi));
        let changed = self.selected_item != next;
        self.selected_item = next;
        (changed, next.is_some())
    }

    /// Activate the item under `(x_px, y_px)` and close the menu.
    pub fn click(&mut self, x_px: f32, y_px: f32) -> Option<u32> {
        if !self.open {
            return None;
        }
        let xi = x_px.round() as i32;
        let yi = y_px.round() as i32;
        let hit = self
            .item_rects
            .iter()
            .find(|(_, r)| r.contains(xi, yi))
            .map(|(id, _)| *id);
        self.close();
        hit
    }

    /// Move keyboard highlight up (`delta < 0`) or down (`delta > 0`), skipping separators.
    pub fn move_selection(&mut self, delta: isize) -> bool {
        if !self.open || delta == 0 {
            return false;
        }
        let count = self
            .entries
            .iter()
            .filter(|e| matches!(e, ContextMenuEntry::Item { .. }))
            .count();
        if count == 0 {
            return false;
        }
        let next = match self.selected_item {
            None => {
                if delta > 0 {
                    0
                } else {
                    count - 1
                }
            }
            Some(cur) => {
                let c = count as isize;
                ((cur as isize + delta).rem_euclid(c)) as usize
            }
        };
        let changed = self.selected_item != Some(next);
        self.selected_item = Some(next);
        changed
    }

    /// Return the `id` of the currently highlighted item (if any).
    pub fn selected_id(&self) -> Option<u32> {
        let idx = self.selected_item?;
        self.entries
            .iter()
            .filter_map(|e| match e {
                ContextMenuEntry::Item { id, .. } => Some(*id),
                ContextMenuEntry::Separator => None,
            })
            .nth(idx)
    }

    pub fn draw(
        &mut self,
        painter: &mut Painter,
        bounds: PixelRect,
        theme: &Theme,
        atlas: &mut GlyphAtlas,
        rasterizer: &mut dyn GlyphRasterizer,
    ) {
        if !self.open || self.entries.is_empty() {
            return;
        }
        let scale = painter.scale();
        let pad = px(4.0, scale);
        let h_inset = px(8.0, scale);
        let row_h = px(24.0, scale);
        let sep_h = px(7.0, scale);
        let hairline = (1.0 * scale).round().max(1.0) as i32;
        let font_px = 12.0 * scale;

        let mut max_label_w = 0.0f32;
        let mut content_h = 0i32;
        for entry in &self.entries {
            match entry {
                ContextMenuEntry::Item { label, .. } => {
                    let w = Painter::measure_text_ui(
                        label,
                        FontStyle::Ui,
                        font_px,
                        atlas,
                        rasterizer,
                    );
                    if w > max_label_w {
                        max_label_w = w;
                    }
                    content_h += row_h;
                }
                ContextMenuEntry::Separator => {
                    content_h += sep_h;
                }
            }
        }

        let min_w = px(168.0, scale);
        let max_w = (bounds.w - px(12.0, scale)).max(min_w);
        let measured_w = max_label_w.ceil() as i32 + (pad + h_inset) * 2 + px(8.0, scale);
        let card_w = measured_w.clamp(min_w, max_w);
        let card_h = pad * 2 + content_h;

        let x = self
            .anchor_x
            .min(bounds.right() - card_w - px(6.0, scale))
            .max(bounds.x + px(4.0, scale));
        let y = self
            .anchor_y
            .min(bounds.bottom() - card_h - px(6.0, scale))
            .max(bounds.y + px(4.0, scale));
        let card = PixelRect::new(x, y, card_w, card_h);
        self.card_rect = card;
        self.item_rects.clear();

        painter.fill_rounded_rect(card, 6.0 * scale, theme.surface);
        painter.stroke_rounded_rect(card, 6.0 * scale, (1.0 * scale).max(1.0), theme.border_control);

        let m = atlas.metrics(rasterizer, FontStyle::Ui, font_px);
        let mut cy = card.y + pad;
        let mut item_idx = 0usize;
        for entry in &self.entries {
            match entry {
                ContextMenuEntry::Separator => {
                    let sy = cy + (sep_h - hairline) / 2;
                    painter.fill_rect(
                        PixelRect::new(
                            card.x + pad + px(4.0, scale),
                            sy,
                            card.w - (pad + px(4.0, scale)) * 2,
                            hairline,
                        ),
                        theme.border_subtle,
                    );
                    cy += sep_h;
                }
                ContextMenuEntry::Item { id, label, danger } => {
                    let r = PixelRect::new(card.x + pad, cy, card.w - pad * 2, row_h);
                    self.item_rects.push((*id, r));
                    if self.selected_item == Some(item_idx) {
                        painter.fill_rounded_rect(r, 4.0 * scale, theme.selection);
                    }
                    let baseline = r.y as f32 + (r.h as f32 + m.ascent - m.descent) * 0.5;
                    let col = if *danger {
                        theme.state_danger
                    } else {
                        theme.text
                    };
                    painter.push_clip(PixelRect::new(
                        r.x + h_inset,
                        r.y,
                        (r.w - h_inset * 2).max(0),
                        r.h,
                    ));
                    painter.draw_text_ui(
                        (r.x + h_inset) as f32,
                        baseline,
                        label,
                        FontStyle::Ui,
                        font_px,
                        col,
                        atlas,
                        rasterizer,
                    );
                    painter.pop_clip();
                    cy += row_h;
                    item_idx += 1;
                }
            }
        }
    }
}
