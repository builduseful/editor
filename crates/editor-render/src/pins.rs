//! Custom-drawn Pinned & Open Files overlay (`Cmd+E` / `<Space>e` / `<Space>fb`).
//!
//! Built strictly with Build Useful design-system tokens:
//! - Lists numbered pinned files (`1`..`9`) first, then open unpinned buffers below.
//! - `1`..`9` jumps immediately to that pin slot; `j`/`k`/`↑`/`↓`/`⌃N`/`⌃P` navigates;
//!   `J`/`K` or `⌥↑`/`⌥↓` reorders pins; `p`/`m` toggles pin; `d`/`⌫` unpins or closes;
//!   `Enter` activates; `⌃V`/`⌘Enter` splits right; `⌃X` splits down; `Esc`/`q` closes.

use editor_platform::{FontStyle, GlyphRasterizer, Key, Modifiers, PixelRect};

use crate::atlas::GlyphAtlas;
use crate::painter::Painter;
use crate::palette::PaletteOpenTarget;
use crate::theme::{px, Theme};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinEntry {
    pub id: u32,
    pub name: String,
    pub rel_path: String,
    /// `Some(1..=9)` when pinned, `None` when in Open / Preview tabs.
    pub pin_slot: Option<u8>,
    pub dirty: bool,
    pub is_preview: bool,
    pub is_active: bool,
    pub in_other_pane: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinsAction {
    None,
    Changed,
    Close,
    JumpToBuffer { id: u32, target: PaletteOpenTarget },
    JumpPin(u8),
    MovePin { slot_0based: usize, delta: isize },
    TogglePin(u32),
    RemoveOrClose(u32),
}

#[derive(Debug, Clone)]
enum Row {
    Header(&'static str),
    Item(usize),
}

pub struct PinsOverlay {
    pub visible: bool,
    entries: Vec<PinEntry>,
    rows: Vec<Row>,
    selected_row: usize,
    scroll_y_px: f32,
    scroll_to_selected: bool,
    card_rect: PixelRect,
    item_rects: Vec<(usize, PixelRect)>,
}

impl Default for PinsOverlay {
    fn default() -> Self {
        Self::new()
    }
}

impl PinsOverlay {
    pub fn new() -> Self {
        Self {
            visible: false,
            entries: Vec::new(),
            rows: Vec::new(),
            selected_row: 0,
            scroll_y_px: 0.0,
            scroll_to_selected: false,
            card_rect: PixelRect::ZERO,
            item_rects: Vec::new(),
        }
    }

    pub fn open(&mut self, entries: Vec<PinEntry>) {
        self.visible = true;
        self.scroll_y_px = 0.0;
        self.refresh(entries, None);
    }

    pub fn close(&mut self) {
        self.visible = false;
        self.entries.clear();
        self.rows.clear();
        self.item_rects.clear();
        self.scroll_y_px = 0.0;
        self.scroll_to_selected = false;
    }

    pub fn handle_scroll(&mut self, dy_px: f32) -> bool {
        if !self.visible {
            return false;
        }
        self.scroll_y_px = (self.scroll_y_px + dy_px).max(0.0);
        self.scroll_to_selected = false;
        true
    }

    pub fn scroll_y_px(&self) -> f32 {
        self.scroll_y_px
    }

    pub fn refresh(&mut self, entries: Vec<PinEntry>, keep_selected_id: Option<u32>) {
        self.entries = entries;
        self.rows.clear();

        let pinned_indices: Vec<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(i, e)| e.pin_slot.map(|_| i))
            .collect();
        let open_indices: Vec<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(i, e)| e.pin_slot.is_none().then_some(i))
            .collect();

        if !pinned_indices.is_empty() {
            self.rows.push(Row::Header("Pinned (1–9)"));
            for idx in pinned_indices {
                self.rows.push(Row::Item(idx));
            }
        }
        if !open_indices.is_empty() {
            self.rows.push(Row::Header("Open Files"));
            for idx in open_indices {
                self.rows.push(Row::Item(idx));
            }
        }

        if let Some(target_id) = keep_selected_id {
            if let Some(r_idx) = self.rows.iter().position(|r| match r {
                Row::Item(i) => self.entries[*i].id == target_id,
                Row::Header(_) => false,
            }) {
                self.selected_row = r_idx;
                self.scroll_to_selected = true;
                return;
            }
        }
        // Prefer selecting the active buffer if present, else first interactive row.
        if let Some(r_idx) = self.rows.iter().position(|r| match r {
            Row::Item(i) => self.entries[*i].is_active,
            Row::Header(_) => false,
        }) {
            self.selected_row = r_idx;
        } else {
            self.selected_row = self
                .rows
                .iter()
                .position(|r| matches!(r, Row::Item(_)))
                .unwrap_or(0);
        }
        self.scroll_to_selected = true;
    }

    fn selected_entry(&self) -> Option<&PinEntry> {
        match self.rows.get(self.selected_row) {
            Some(Row::Item(idx)) => self.entries.get(*idx),
            _ => None,
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let len = self.rows.len() as isize;
        let mut idx = self.selected_row.min(self.rows.len() - 1) as isize;
        for _ in 0..len {
            idx = (idx + delta).clamp(0, len - 1);
            if matches!(self.rows[idx as usize], Row::Item(_)) {
                self.selected_row = idx as usize;
                self.scroll_to_selected = true;
                return;
            }
            if (delta < 0 && idx == 0) || (delta > 0 && idx == len - 1) {
                break;
            }
        }
    }

    pub fn handle_key(&mut self, key: Key, mods: Modifiers) -> PinsAction {
        if !self.visible {
            return PinsAction::None;
        }
        // Consistent modal ctrl ergonomics matching Quick Open & Search in Files (`⌃N`/`⌃P`/`⌃V`/`⌃X`).
        if mods.ctrl && !mods.meta && !mods.alt {
            match key {
                Key::Char('n') | Key::Char('N') => {
                    self.move_selection(1);
                    return PinsAction::Changed;
                }
                Key::Char('p') | Key::Char('P') => {
                    self.move_selection(-1);
                    return PinsAction::Changed;
                }
                Key::Char('v') | Key::Char('V') => {
                    if let Some(e) = self.selected_entry() {
                        let id = e.id;
                        self.close();
                        return PinsAction::JumpToBuffer {
                            id,
                            target: PaletteOpenTarget::SplitRight,
                        };
                    }
                    return PinsAction::None;
                }
                Key::Char('x') | Key::Char('X') => {
                    if let Some(e) = self.selected_entry() {
                        let id = e.id;
                        self.close();
                        return PinsAction::JumpToBuffer {
                            id,
                            target: PaletteOpenTarget::SplitDown,
                        };
                    }
                    return PinsAction::None;
                }
                _ => {}
            }
        }

        match key {
            Key::Escape => {
                self.close();
                return PinsAction::Close;
            }
            Key::Up => {
                if mods.alt {
                    if let Some(e) = self.selected_entry() {
                        if let Some(slot) = e.pin_slot {
                            return PinsAction::MovePin {
                                slot_0based: (slot - 1) as usize,
                                delta: -1,
                            };
                        }
                    }
                    return PinsAction::None;
                }
                self.move_selection(-1);
                return PinsAction::Changed;
            }
            Key::Down => {
                if mods.alt {
                    if let Some(e) = self.selected_entry() {
                        if let Some(slot) = e.pin_slot {
                            return PinsAction::MovePin {
                                slot_0based: (slot - 1) as usize,
                                delta: 1,
                            };
                        }
                    }
                    return PinsAction::None;
                }
                self.move_selection(1);
                return PinsAction::Changed;
            }
            Key::Enter => {
                if let Some(e) = self.selected_entry() {
                    let id = e.id;
                    self.close();
                    return PinsAction::JumpToBuffer {
                        id,
                        target: if mods.primary() {
                            PaletteOpenTarget::SplitRight
                        } else {
                            PaletteOpenTarget::Current
                        },
                    };
                }
                return PinsAction::None;
            }
            Key::Backspace | Key::Delete => {
                if let Some(e) = self.selected_entry() {
                    return PinsAction::RemoveOrClose(e.id);
                }
                return PinsAction::None;
            }
            Key::Char(c) if !mods.primary() && !mods.ctrl && !mods.alt => match c {
                '1'..='9' => {
                    let slot = (c as u8) - b'0';
                    self.close();
                    return PinsAction::JumpPin(slot);
                }
                'j' if !mods.shift => {
                    self.move_selection(1);
                    return PinsAction::Changed;
                }
                'k' if !mods.shift => {
                    self.move_selection(-1);
                    return PinsAction::Changed;
                }
                'J' | 'j' if mods.shift => {
                    if let Some(e) = self.selected_entry() {
                        if let Some(slot) = e.pin_slot {
                            return PinsAction::MovePin {
                                slot_0based: (slot - 1) as usize,
                                delta: 1,
                            };
                        }
                    }
                    return PinsAction::None;
                }
                'K' | 'k' if mods.shift => {
                    if let Some(e) = self.selected_entry() {
                        if let Some(slot) = e.pin_slot {
                            return PinsAction::MovePin {
                                slot_0based: (slot - 1) as usize,
                                delta: -1,
                            };
                        }
                    }
                    return PinsAction::None;
                }
                'p' | 'P' | 'm' | 'M' => {
                    if let Some(e) = self.selected_entry() {
                        return PinsAction::TogglePin(e.id);
                    }
                    return PinsAction::None;
                }
                'd' | 'D' | 'x' | 'X' => {
                    if let Some(e) = self.selected_entry() {
                        return PinsAction::RemoveOrClose(e.id);
                    }
                    return PinsAction::None;
                }
                'q' => {
                    self.close();
                    return PinsAction::Close;
                }
                _ => {}
            },
            _ => {}
        }
        PinsAction::None
    }

    pub fn handle_click(&mut self, x_px: f32, y_px: f32, mods: Modifiers) -> PinsAction {
        if !self.visible {
            return PinsAction::None;
        }
        let xi = x_px.round() as i32;
        let yi = y_px.round() as i32;
        if !self.card_rect.contains(xi, yi) {
            self.close();
            return PinsAction::Close;
        }
        for &(entry_idx, rect) in &self.item_rects {
            if rect.contains(xi, yi) {
                if let Some(e) = self.entries.get(entry_idx) {
                    let id = e.id;
                    self.close();
                    return PinsAction::JumpToBuffer {
                        id,
                        target: if mods.primary() {
                            PaletteOpenTarget::SplitRight
                        } else {
                            PaletteOpenTarget::Current
                        },
                    };
                }
            }
        }
        PinsAction::None
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
            self.card_rect = PixelRect::ZERO;
            self.item_rects.clear();
            return;
        }
        let scale = painter.scale();
        let hairline = (1.0 * scale).round().max(1.0) as i32;

        // Quiet scrim from theme tokens.
        painter.fill_rect(bounds, theme.scrim);

        let pad = px(12.0, scale);
        let title_h = px(22.0, scale);
        let section_h = px(22.0, scale);
        let row_h = px(30.0, scale);
        let footer_h = px(22.0, scale);

        let mut list_h = 0i32;
        let mut row_offsets: Vec<(i32, i32)> = Vec::with_capacity(self.rows.len());
        if self.rows.is_empty() {
            list_h = px(34.0, scale);
        } else {
            for (i, r) in self.rows.iter().enumerate() {
                match r {
                    Row::Header(_) => {
                        if i > 0 {
                            list_h += px(4.0, scale);
                        }
                        row_offsets.push((list_h, section_h));
                        list_h += section_h;
                    }
                    Row::Item(_) => {
                        row_offsets.push((list_h, row_h));
                        list_h += row_h;
                    }
                }
            }
        }

        let card_w = px(540.0, scale).min((bounds.w - px(32.0, scale)).max(px(280.0, scale)));
        let max_card_h = (bounds.h - px(72.0, scale)).max(px(140.0, scale));
        let card_h = (pad + title_h + px(8.0, scale) + list_h + px(8.0, scale) + footer_h + pad)
            .min(max_card_h);
        let card = PixelRect::new(
            bounds.x + (bounds.w - card_w) / 2,
            bounds.y + px(54.0, scale),
            card_w,
            card_h,
        );
        self.card_rect = card;

        let radius = 8.0 * scale;
        painter.fill_rounded_rect(card, radius, theme.surface);
        painter.stroke_rounded_rect(card, radius, hairline as f32, theme.border_control);
        painter.push_clip(card);

        // Title bar
        let title_px = 12.5 * scale;
        let hint_px = 10.5 * scale;
        let tm = atlas.metrics(rasterizer, FontStyle::UiBold, title_px);
        let ty = (card.y + pad) as f32 + tm.ascent;
        painter.draw_text_ui(
            (card.x + pad) as f32,
            ty,
            "Pinned & Open Files",
            FontStyle::UiBold,
            title_px,
            theme.text,
            atlas,
            rasterizer,
        );
        let pin_count = self.entries.iter().filter(|e| e.pin_slot.is_some()).count();
        let summary = format!("{pin_count}/9 pinned · {} open", self.entries.len());
        let sw = Painter::measure_text_ui(&summary, FontStyle::Ui, hint_px, atlas, rasterizer);
        painter.draw_text_ui(
            (card.right() - pad) as f32 - sw,
            ty,
            &summary,
            FontStyle::Ui,
            hint_px,
            theme.text_muted,
            atlas,
            rasterizer,
        );

        let sep_y = card.y + pad + title_h + px(4.0, scale);
        painter.fill_rect(
            PixelRect::new(card.x + pad, sep_y, card.w - pad * 2, hairline),
            theme.border_subtle,
        );

        let list_y = sep_y + px(4.0, scale);
        let footer_y = card.bottom() - pad - footer_h;
        let list_clip = PixelRect::new(
            card.x + pad,
            list_y,
            (card.w - pad * 2).max(0),
            (footer_y - px(6.0, scale) - list_y).max(0),
        );
        let max_scroll = (list_h - list_clip.h).max(0) as f32;
        if self.scroll_to_selected {
            self.scroll_to_selected = false;
            if let Some(&(ry, rh)) = row_offsets.get(self.selected_row) {
                let top = ry as f32;
                let bot = (ry + rh) as f32;
                if top < self.scroll_y_px {
                    self.scroll_y_px = top;
                } else if bot > self.scroll_y_px + list_clip.h as f32 {
                    self.scroll_y_px = bot - list_clip.h as f32;
                }
            }
        }
        self.scroll_y_px = self.scroll_y_px.clamp(0.0, max_scroll);
        let scroll_i = self.scroll_y_px.round() as i32;
        let right_gutter = if max_scroll > 0.0 { px(8.0, scale) } else { 0 };
        painter.push_clip(list_clip);

        self.item_rects.clear();
        let name_px = 12.5 * scale;
        let sub_px = 11.0 * scale;
        let badge_px = 10.5 * scale;
        let nm = atlas.metrics(rasterizer, FontStyle::UiBold, name_px);
        let bm = atlas.metrics(rasterizer, FontStyle::UiBold, badge_px);
        let hm = atlas.metrics(rasterizer, FontStyle::UiBold, hint_px);

        if self.rows.is_empty() {
            painter.draw_text_ui(
                (list_clip.x + px(6.0, scale)) as f32,
                list_clip.y as f32 + nm.ascent + px(6.0, scale) as f32,
                "Press ⌘M to pin the current file",
                FontStyle::Ui,
                name_px,
                theme.text_muted,
                atlas,
                rasterizer,
            );
        } else {
            for (r_idx, (row, &(ry_rel, rh))) in
                self.rows.iter().zip(row_offsets.iter()).enumerate()
            {
                let cy = list_clip.y + ry_rel - scroll_i;
                if cy + rh <= list_clip.y || cy >= list_clip.bottom() {
                    continue;
                }
                match *row {
                    Row::Header(title) => {
                        let base = cy as f32 + (section_h as f32 + hm.ascent - hm.descent) * 0.5;
                        painter.draw_text_ui(
                            (list_clip.x + px(6.0, scale)) as f32,
                            base,
                            title,
                            FontStyle::UiBold,
                            hint_px,
                            theme.text_muted,
                            atlas,
                            rasterizer,
                        );
                    }
                    Row::Item(entry_idx) => {
                        let e = &self.entries[entry_idx];
                        let r = PixelRect::new(
                            list_clip.x,
                            cy,
                            (list_clip.w - right_gutter).max(0),
                            row_h,
                        );
                        let clipped_r = r.intersect(list_clip);
                        if !clipped_r.is_empty() {
                            self.item_rects.push((entry_idx, clipped_r));
                        }

                        if r_idx == self.selected_row {
                            painter.fill_rounded_rect(r, 5.0 * scale, theme.selection);
                            painter.stroke_rounded_rect(
                                r,
                                5.0 * scale,
                                hairline as f32,
                                theme.border_control,
                            );
                        }

                        let mut x = (r.x + px(8.0, scale)) as f32;
                        let row_cy = r.y as f32 + r.h as f32 * 0.5;
                        let badge_sz = px(18.0, scale);
                        let brect = PixelRect::new(
                            x.round() as i32,
                            (row_cy - badge_sz as f32 * 0.5).round() as i32,
                            badge_sz,
                            badge_sz,
                        );
                        if let Some(slot) = e.pin_slot {
                            painter.fill_rounded_rect(brect, 4.0 * scale, theme.page);
                            painter.stroke_rounded_rect(
                                brect,
                                4.0 * scale,
                                hairline as f32,
                                theme.border_control,
                            );
                            let s = slot.to_string();
                            let sw = Painter::measure_text_ui(
                                &s,
                                FontStyle::UiBold,
                                badge_px,
                                atlas,
                                rasterizer,
                            );
                            let bbase =
                                brect.y as f32 + (brect.h as f32 + bm.ascent - bm.descent) * 0.5;
                            painter.draw_text_ui(
                                brect.x as f32 + (brect.w as f32 - sw) * 0.5,
                                bbase,
                                &s,
                                FontStyle::UiBold,
                                badge_px,
                                theme.text,
                                atlas,
                                rasterizer,
                            );
                        } else {
                            let dot = px(5.0, scale).max(2);
                            painter.fill_rounded_rect(
                                PixelRect::new(
                                    brect.x + (brect.w - dot) / 2,
                                    brect.y + (brect.h - dot) / 2,
                                    dot,
                                    dot,
                                ),
                                dot as f32 * 0.5,
                                theme.border_control,
                            );
                        }
                        x = (brect.right() + px(10.0, scale)) as f32;

                        // Right-aligned status tag (`Active` / `Split` / `Preview`).
                        let tag = if e.is_active {
                            Some("Active")
                        } else if e.in_other_pane {
                            Some("Split")
                        } else if e.is_preview {
                            Some("Preview")
                        } else {
                            None
                        };
                        let mut right_limit = r.right() - px(8.0, scale);
                        let base = r.y as f32 + (r.h as f32 + nm.ascent - nm.descent) * 0.5;
                        if let Some(t) = tag {
                            let tw =
                                Painter::measure_text_ui(t, FontStyle::Ui, hint_px, atlas, rasterizer);
                            let tx = (right_limit as f32 - tw).round();
                            painter.draw_text_ui(
                                tx,
                                base,
                                t,
                                FontStyle::Ui,
                                hint_px,
                                theme.text_muted,
                                atlas,
                                rasterizer,
                            );
                            right_limit = (tx as i32 - px(10.0, scale)).max(r.x);
                        }

                        painter.push_clip(PixelRect::new(
                            x.floor() as i32,
                            r.y,
                            (right_limit - x.floor() as i32).max(0),
                            r.h,
                        ));
                        let title = if e.dirty {
                            format!("{} •", e.name)
                        } else {
                            e.name.clone()
                        };
                        let after_name = painter.draw_text_ui(
                            x,
                            base,
                            &title,
                            FontStyle::UiBold,
                            name_px,
                            theme.text,
                            atlas,
                            rasterizer,
                        );
                        if !e.rel_path.is_empty() && e.rel_path != e.name {
                            painter.draw_text_ui(
                                after_name + px(10.0, scale) as f32,
                                base,
                                &e.rel_path,
                                FontStyle::Ui,
                                sub_px,
                                theme.text_muted,
                                atlas,
                                rasterizer,
                            );
                        }
                        painter.pop_clip();
                    }
                }
            }
        }
        if max_scroll > 0.0 && list_clip.h > 0 {
            let thumb_w = px(4.0, scale);
            let thumb_h = ((list_clip.h as f32 * list_clip.h as f32) / list_h as f32)
                .max(24.0 * scale)
                .min(list_clip.h as f32)
                .round() as i32;
            let travel = (list_clip.h - thumb_h).max(0);
            let frac = (self.scroll_y_px / max_scroll).clamp(0.0, 1.0);
            let ty = list_clip.y + (frac * travel as f32).round() as i32;
            painter.fill_rounded_rect(
                PixelRect::new(list_clip.right() - thumb_w, ty, thumb_w, thumb_h),
                thumb_w as f32 * 0.5,
                theme.border_control.with_alpha(120),
            );
        }
        painter.pop_clip();

        // Footer hint bar
        painter.fill_rect(
            PixelRect::new(card.x + pad, footer_y - px(4.0, scale), card.w - pad * 2, hairline),
            theme.border_subtle,
        );
        let fbase = footer_y as f32 + (footer_h as f32 + hm.ascent - hm.descent) * 0.5;
        painter.push_clip(PixelRect::new(
            card.x + pad,
            footer_y,
            (card.w - pad * 2).max(0),
            footer_h,
        ));
        painter.draw_text_ui(
            (card.x + pad) as f32,
            fbase,
            "1–9 jump  ·  ↵ open  ·  ⌃V/⌃X split  ·  p pin  ·  ⌥↑↓ reorder  ·  d remove",
            FontStyle::Ui,
            hint_px,
            theme.text_muted,
            atlas,
            rasterizer,
        );
        painter.pop_clip();
        painter.pop_clip();
    }
}
