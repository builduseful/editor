//! Custom-drawn Keyboard Shortcuts modal overlay (`Cmd+?` / `Shift+Cmd+/`).
//!
//! Built with zero external libraries using the Build Useful design tokens:
//! - Auto-focused live search across actions, categories, and keycap strings
//! - Progressive disclosure: featured "Main Shortcuts" shown immediately at top
//! - Quiet expandable "Show all shortcuts (N more)" section (`Tab` or click)
//! - Full categorized reference (including Neovim motions) with smooth scroll
//! - Pressing `Enter` on any runnable shortcut executes it directly

use editor_platform::{FontStyle, GlyphRasterizer, Key, Modifiers, PixelRect};

use crate::atlas::GlyphAtlas;
use crate::painter::Painter;
use crate::palette::fuzzy_score;
use crate::text_input::TextInput;
use crate::theme::{px, Color, Theme};

#[derive(Debug, Clone)]
pub struct ShortcutDisplayItem {
    /// Stable action ID (e.g. `"view.split_pane"`), runnable directly via `Enter` if `runnable`.
    pub id: &'static str,
    pub label: &'static str,
    pub category: &'static str,
    /// Human-readable keycap badges separated by spaces or `/` (e.g. `"⌘P"`, `"⌘⇧F"`, `"dd"`).
    pub keys: String,
    pub featured: bool,
    pub runnable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShortcutsAction {
    None,
    Changed,
    Close,
    RunAction(&'static str),
}

#[derive(Debug, Clone)]
enum DisplayRow {
    SectionHeader(String),
    Item(usize),
    ToggleAll { remaining: usize, expanded: bool },
}

pub struct ShortcutsOverlay {
    pub visible: bool,
    pub input: TextInput,
    pub expanded: bool,
    items: Vec<ShortcutDisplayItem>,
    rows: Vec<DisplayRow>,
    selected_row: usize,
    scroll_y_px: f32,
    card_rect: PixelRect,
    list_rect: PixelRect,
    row_rects: Vec<(usize, PixelRect)>,
}

impl Default for ShortcutsOverlay {
    fn default() -> Self {
        Self::new()
    }
}

impl ShortcutsOverlay {
    pub fn new() -> Self {
        Self {
            visible: false,
            input: TextInput::new("Search shortcuts, actions, or Vim keys…"),
            expanded: false,
            items: Vec::new(),
            rows: Vec::new(),
            selected_row: 0,
            scroll_y_px: 0.0,
            card_rect: PixelRect::ZERO,
            list_rect: PixelRect::ZERO,
            row_rects: Vec::new(),
        }
    }

    pub fn open(&mut self, items: Vec<ShortcutDisplayItem>) {
        self.visible = true;
        self.items = items;
        self.expanded = false;
        self.input.set_text("");
        self.input.focused = true;
        self.scroll_y_px = 0.0;
        self.rebuild_rows();
        self.select_first_interactive();
    }

    pub fn close(&mut self) {
        self.visible = false;
        self.input.focused = false;
    }

    pub fn toggle_expanded(&mut self) {
        self.expanded = !self.expanded;
        self.rebuild_rows();
        if self.expanded {
            // Focus the first newly revealed shortcut so expanding immediately scrolls into view!
            if let Some(toggle_pos) = self
                .rows
                .iter()
                .position(|r| matches!(r, DisplayRow::ToggleAll { .. }))
            {
                self.selected_row = self.rows[toggle_pos + 1..]
                    .iter()
                    .position(Self::is_row_interactive)
                    .map(|off| toggle_pos + 1 + off)
                    .unwrap_or(toggle_pos);
            }
        } else {
            self.select_first_interactive();
            self.scroll_y_px = 0.0;
        }
    }

    fn is_row_interactive(row: &DisplayRow) -> bool {
        !matches!(row, DisplayRow::SectionHeader(_))
    }

    fn select_first_interactive(&mut self) {
        self.selected_row = self
            .rows
            .iter()
            .position(Self::is_row_interactive)
            .unwrap_or(0);
    }

    fn move_selection(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let mut idx = self.selected_row.min(self.rows.len() - 1) as isize;
        let len = self.rows.len() as isize;
        for _ in 0..len {
            idx = (idx + delta).clamp(0, len - 1);
            if Self::is_row_interactive(&self.rows[idx as usize]) {
                self.selected_row = idx as usize;
                return;
            }
            if (delta < 0 && idx == 0) || (delta > 0 && idx == len - 1) {
                break;
            }
        }
    }

    fn rebuild_rows(&mut self) {
        self.rows.clear();
        let query = self.input.text.trim();
        if !query.is_empty() {
            // Live search across ALL shortcuts (main + extra + Vim motions) ranked by match quality.
            let q_low = query.to_ascii_lowercase();
            let mut scored: Vec<(i32, usize)> = Vec::new();
            for (idx, item) in self.items.iter().enumerate() {
                let hay = format!("{} {} {}", item.label, item.category, item.keys);
                let hay_low = hay.to_ascii_lowercase();
                let mut score = if hay_low.contains(&q_low) {
                    200
                } else {
                    match fuzzy_score(&q_low, &hay) {
                        Some(s) => s,
                        None => continue,
                    }
                };
                if item.label.to_ascii_lowercase().starts_with(&q_low) {
                    score += 80;
                }
                if item.keys.to_ascii_lowercase().contains(&q_low) {
                    score += 60;
                }
                if item.featured {
                    score += 15;
                }
                scored.push((score, idx));
            }
            scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            if !scored.is_empty() {
                self.rows
                    .push(DisplayRow::SectionHeader(format!("Matches ({})", scored.len())));
                for (_, idx) in scored {
                    self.rows.push(DisplayRow::Item(idx));
                }
            }
            self.select_first_interactive();
            self.scroll_y_px = 0.0;
            return;
        }

        // Empty query: progressive disclosure -> "Main Shortcuts" first, then expandable "All Shortcuts".
        let mut featured_indices = Vec::new();
        let mut extra_indices = Vec::new();
        for (idx, item) in self.items.iter().enumerate() {
            if item.featured {
                featured_indices.push(idx);
            } else {
                extra_indices.push(idx);
            }
        }

        self.rows
            .push(DisplayRow::SectionHeader("Main Shortcuts".to_string()));
        for idx in featured_indices {
            self.rows.push(DisplayRow::Item(idx));
        }

        if !extra_indices.is_empty() {
            self.rows.push(DisplayRow::ToggleAll {
                remaining: extra_indices.len(),
                expanded: self.expanded,
            });
            if self.expanded {
                let mut current_cat = "";
                for idx in extra_indices {
                    let cat = self.items[idx].category;
                    if cat != current_cat {
                        current_cat = cat;
                        self.rows
                            .push(DisplayRow::SectionHeader(cat.to_string()));
                    }
                    self.rows.push(DisplayRow::Item(idx));
                }
            }
        }
    }

    pub fn handle_key(
        &mut self,
        key: Key,
        mods: Modifiers,
        text: Option<&str>,
    ) -> ShortcutsAction {
        if !self.visible {
            return ShortcutsAction::None;
        }
        match key {
            Key::Escape => {
                self.close();
                return ShortcutsAction::Close;
            }
            Key::Up => {
                self.move_selection(-1);
                return ShortcutsAction::Changed;
            }
            Key::Down => {
                self.move_selection(1);
                return ShortcutsAction::Changed;
            }
            Key::Tab => {
                if self.input.text.trim().is_empty() {
                    self.toggle_expanded();
                    return ShortcutsAction::Changed;
                }
                return ShortcutsAction::None;
            }
            Key::Enter => {
                return self.activate_row(self.selected_row);
            }
            _ => {}
        }

        let (changed, _) = self.input.handle_key(key, mods, text, None);
        if changed {
            self.rebuild_rows();
            return ShortcutsAction::Changed;
        }
        ShortcutsAction::None
    }

    fn activate_row(&mut self, row_idx: usize) -> ShortcutsAction {
        let Some(row) = self.rows.get(row_idx).cloned() else {
            return ShortcutsAction::None;
        };
        match row {
            DisplayRow::SectionHeader(_) => ShortcutsAction::None,
            DisplayRow::ToggleAll { .. } => {
                self.toggle_expanded();
                ShortcutsAction::Changed
            }
            DisplayRow::Item(item_idx) => {
                let item = &self.items[item_idx];
                if item.runnable {
                    let id = item.id;
                    self.close();
                    ShortcutsAction::RunAction(id)
                } else {
                    ShortcutsAction::None
                }
            }
        }
    }

    pub fn handle_scroll(&mut self, dy_px: f32) -> bool {
        if !self.visible {
            return false;
        }
        self.scroll_y_px = (self.scroll_y_px + dy_px).max(0.0);
        true
    }

    pub fn handle_mouse_down(&mut self, x_px: f32, y_px: f32) -> ShortcutsAction {
        if !self.visible {
            return ShortcutsAction::None;
        }
        let xi = x_px.round() as i32;
        let yi = y_px.round() as i32;
        if !self.card_rect.contains(xi, yi) {
            self.close();
            return ShortcutsAction::Close;
        }
        for &(row_idx, rect) in &self.row_rects {
            if rect.contains(xi, yi) {
                self.selected_row = row_idx;
                return self.activate_row(row_idx);
            }
        }
        ShortcutsAction::Changed
    }

    pub fn draw(
        &mut self,
        painter: &mut Painter,
        bounds: PixelRect,
        theme: &Theme,
        atlas: &mut GlyphAtlas,
        rasterizer: &mut dyn GlyphRasterizer,
        caret_visible: bool,
    ) {
        if !self.visible {
            return;
        }
        let scale = painter.scale();
        let hairline = (1.0 * scale).round().max(1.0) as i32;

        // Subtle dim scrim over the editor.
        let scrim = Color::rgba(0, 0, 0, if theme.is_dark { 140 } else { 65 });
        painter.fill_rect(bounds, scrim);

        let card_w = (bounds.w - px(64.0, scale))
            .min(px(620.0, scale))
            .max(px(340.0, scale));
        let card_h = (bounds.h - px(72.0, scale))
            .min(px(540.0, scale))
            .max(px(260.0, scale));
        let card_x = bounds.x + (bounds.w - card_w) / 2;
        let card_y = bounds.y + ((bounds.h - card_h) / 2).max(px(28.0, scale));
        let card = PixelRect::new(card_x, card_y, card_w, card_h);
        self.card_rect = card;

        let radius = 8.0 * scale;
        painter.fill_rounded_rect(card, radius, theme.surface);
        painter.stroke_rounded_rect(card, radius, hairline as f32, theme.border_control);

        // Header title + count badge.
        let pad = px(16.0, scale);
        let header_h = px(40.0, scale);
        let title_font = 13.5 * scale;
        let small_font = 11.5 * scale;
        let tm = atlas.metrics(rasterizer, FontStyle::UiBold, title_font);
        let title_base = card.y as f32 + (header_h as f32 + tm.ascent - tm.descent) * 0.5;
        painter.draw_text_ui(
            (card.x + pad) as f32,
            title_base,
            "Keyboard Shortcuts",
            FontStyle::UiBold,
            title_font,
            theme.text,
            atlas,
            rasterizer,
        );
        let subtitle = format!("{} shortcuts", self.items.len());
        let sub_w = Painter::measure_text_ui(&subtitle, FontStyle::Ui, small_font, atlas, rasterizer);
        let sm = atlas.metrics(rasterizer, FontStyle::Ui, small_font);
        let sub_base = card.y as f32 + (header_h as f32 + sm.ascent - sm.descent) * 0.5;
        painter.draw_text_ui(
            (card.right() - pad) as f32 - sub_w,
            sub_base,
            &subtitle,
            FontStyle::Ui,
            small_font,
            theme.text_muted,
            atlas,
            rasterizer,
        );

        // Search input box.
        let input_h = px(32.0, scale);
        let input_rect = PixelRect::new(card.x + pad, card.y + header_h, card.w - pad * 2, input_h);
        self.input
            .draw(painter, input_rect, theme, atlas, rasterizer, caret_visible);

        let div_y = input_rect.bottom() + px(12.0, scale);
        painter.fill_rect(
            PixelRect::new(card.x, div_y, card.w, hairline),
            theme.border_subtle,
        );

        // Footer hint strip.
        let footer_h = px(30.0, scale);
        let footer_y = card.bottom() - footer_h;
        painter.fill_rect(
            PixelRect::new(card.x, footer_y, card.w, hairline),
            theme.border_subtle,
        );
        let toggle_hint = if self.expanded {
            "Tab Main only"
        } else {
            "Tab Show all"
        };
        let hint = format!("↑↓ Navigate   ·   ↵ Run   ·   {toggle_hint}   ·   Esc Close");
        let fm = atlas.metrics(rasterizer, FontStyle::Ui, 11.0 * scale);
        let footer_base = footer_y as f32 + (footer_h as f32 + fm.ascent - fm.descent) * 0.5;
        painter.draw_text_ui(
            (card.x + pad) as f32,
            footer_base,
            &hint,
            FontStyle::Ui,
            11.0 * scale,
            theme.text_muted,
            atlas,
            rasterizer,
        );

        // Scrollable rows viewport.
        let list_rect = PixelRect::new(
            card.x + px(8.0, scale),
            div_y + px(6.0, scale),
            card.w - px(16.0, scale),
            (footer_y - div_y - px(12.0, scale)).max(0),
        );
        self.list_rect = list_rect;
        self.row_rects.clear();

        let item_h = px(30.0, scale);
        let hdr_h = px(26.0, scale);
        let toggle_h = px(32.0, scale);

        // Compute total content height & ensure selected row is scrolled into view.
        let mut row_offsets: Vec<(i32, i32)> = Vec::with_capacity(self.rows.len());
        let mut y_acc = 0i32;
        for row in &self.rows {
            let h = match row {
                DisplayRow::SectionHeader(_) => hdr_h,
                DisplayRow::Item(_) => item_h,
                DisplayRow::ToggleAll { .. } => toggle_h,
            };
            row_offsets.push((y_acc, h));
            y_acc += h;
        }
        let total_h = y_acc;
        let max_scroll = (total_h - list_rect.h).max(0) as f32;
        if let Some(&(sy, sh)) = row_offsets.get(self.selected_row) {
            let sy_f = sy as f32;
            let sb_f = (sy + sh) as f32;
            if sy_f < self.scroll_y_px {
                self.scroll_y_px = sy_f;
            } else if sb_f > self.scroll_y_px + list_rect.h as f32 {
                self.scroll_y_px = sb_f - list_rect.h as f32;
            }
        }
        self.scroll_y_px = self.scroll_y_px.clamp(0.0, max_scroll);

        painter.push_clip(list_rect);

        if self.rows.is_empty() {
            let empty_msg = "No matching shortcuts";
            let ew = Painter::measure_text_ui(empty_msg, FontStyle::Ui, 12.5 * scale, atlas, rasterizer);
            let em = atlas.metrics(rasterizer, FontStyle::Ui, 12.5 * scale);
            painter.draw_text_ui(
                list_rect.x as f32 + (list_rect.w as f32 - ew) * 0.5,
                list_rect.y as f32 + px(36.0, scale) as f32 + em.ascent,
                empty_msg,
                FontStyle::Ui,
                12.5 * scale,
                theme.text_muted,
                atlas,
                rasterizer,
            );
        }

        let scroll_i = self.scroll_y_px.round() as i32;
        let label_font = 12.5 * scale;
        let key_font = 11.5 * scale;
        let cat_font = 11.0 * scale;
        let lm = atlas.metrics(rasterizer, FontStyle::Ui, label_font);
        let km = atlas.metrics(rasterizer, FontStyle::Mono, key_font);
        let cm = atlas.metrics(rasterizer, FontStyle::UiBold, cat_font);
        let inner_pad = px(10.0, scale);
        let right_gutter = if max_scroll > 0.0 { px(12.0, scale) } else { 0 };

        for (row_idx, (row, &(ry_rel, rh))) in self.rows.iter().zip(row_offsets.iter()).enumerate() {
            let ry = list_rect.y + ry_rel - scroll_i;
            if ry + rh <= list_rect.y || ry >= list_rect.bottom() {
                continue;
            }
            let row_rect = PixelRect::new(list_rect.x, ry, list_rect.w - right_gutter, rh);
            let selected = row_idx == self.selected_row;

            match row {
                DisplayRow::SectionHeader(title) => {
                    let base = ry as f32 + (rh as f32 + cm.ascent - cm.descent) * 0.5 + 2.0 * scale;
                    painter.draw_text_ui(
                        (row_rect.x + inner_pad) as f32,
                        base,
                        &title.to_uppercase(),
                        FontStyle::UiBold,
                        cat_font,
                        theme.text_muted,
                        atlas,
                        rasterizer,
                    );
                }
                DisplayRow::ToggleAll {
                    remaining,
                    expanded,
                } => {
                    self.row_rects.push((row_idx, row_rect));
                    let bg = if selected {
                        theme.selection
                    } else {
                        theme.page
                    };
                    let pill = PixelRect::new(
                        row_rect.x + px(4.0, scale),
                        row_rect.y + px(3.0, scale),
                        row_rect.w - px(8.0, scale),
                        row_rect.h - px(6.0, scale),
                    );
                    painter.fill_rounded_rect(pill, 5.0 * scale, bg);
                    painter.stroke_rounded_rect(pill, 5.0 * scale, hairline as f32, theme.border_subtle);
                    let text = if *expanded {
                        format!("▾  Hide extra shortcuts ({remaining})")
                    } else {
                        format!("▸  Show all shortcuts ({remaining} more)")
                    };
                    let base = pill.y as f32 + (pill.h as f32 + lm.ascent - lm.descent) * 0.5;
                    painter.draw_text_ui(
                        (pill.x + inner_pad) as f32,
                        base,
                        &text,
                        FontStyle::UiBold,
                        12.0 * scale,
                        theme.text,
                        atlas,
                        rasterizer,
                    );
                    let badge = "Tab";
                    let bw = Painter::measure_text_ui(badge, FontStyle::Mono, key_font, atlas, rasterizer);
                    painter.draw_text_ui(
                        (pill.right() - inner_pad) as f32 - bw,
                        pill.y as f32 + (pill.h as f32 + km.ascent - km.descent) * 0.5,
                        badge,
                        FontStyle::Mono,
                        key_font,
                        theme.text_muted,
                        atlas,
                        rasterizer,
                    );
                }
                DisplayRow::Item(item_idx) => {
                    self.row_rects.push((row_idx, row_rect));
                    let item = &self.items[*item_idx];
                    if selected {
                        painter.fill_rounded_rect(row_rect, 5.0 * scale, theme.current_line);
                    }
                    let base = ry as f32 + (rh as f32 + lm.ascent - lm.descent) * 0.5;
                    painter.draw_text_ui(
                        (row_rect.x + inner_pad) as f32,
                        base,
                        item.label,
                        FontStyle::Ui,
                        label_font,
                        theme.text,
                        atlas,
                        rasterizer,
                    );

                    // Tactile keycap pill on the right.
                    let key_w = Painter::measure_text_ui(
                        &item.keys,
                        FontStyle::Mono,
                        key_font,
                        atlas,
                        rasterizer,
                    )
                    .ceil() as i32;
                    let cap_pad_x = px(7.0, scale);
                    let cap_w = key_w + cap_pad_x * 2;
                    let cap_h = px(20.0, scale);
                    let cap_rect = PixelRect::new(
                        row_rect.right() - inner_pad - cap_w,
                        ry + (rh - cap_h) / 2,
                        cap_w,
                        cap_h,
                    );
                    painter.fill_rounded_rect(cap_rect, 4.0 * scale, theme.page);
                    painter.stroke_rounded_rect(cap_rect, 4.0 * scale, hairline as f32, theme.border_subtle);
                    let kbase = cap_rect.y as f32 + (cap_rect.h as f32 + km.ascent - km.descent) * 0.5;
                    painter.draw_text_ui(
                        (cap_rect.x + cap_pad_x) as f32,
                        kbase,
                        &item.keys,
                        FontStyle::Mono,
                        key_font,
                        theme.text,
                        atlas,
                        rasterizer,
                    );
                }
            }
        }

        // Quiet scrollbar thumb when list overflows.
        if max_scroll > 0.0 && list_rect.h > 0 {
            let thumb_w = px(4.0, scale);
            let thumb_h = ((list_rect.h as f32 * list_rect.h as f32) / total_h as f32)
                .max(24.0 * scale)
                .min(list_rect.h as f32)
                .round() as i32;
            let travel = (list_rect.h - thumb_h).max(0);
            let frac = (self.scroll_y_px / max_scroll).clamp(0.0, 1.0);
            let ty = list_rect.y + (frac * travel as f32).round() as i32;
            painter.fill_rounded_rect(
                PixelRect::new(list_rect.right() - thumb_w - px(2.0, scale), ty, thumb_w, thumb_h),
                thumb_w as f32 * 0.5,
                theme.border_control.with_alpha(120),
            );
        }

        painter.pop_clip();
    }
}
