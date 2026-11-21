//! Custom-drawn Unified Working-Set Strip (Harpoon `1..9` pins + Open/Preview tabs).
//!
//! Built strictly with Build Useful design tokens:
//! - Single 26pt bar across the editor column (`theme.page` ground, `1px` `theme.border_subtle` hairline).
//! - Left side: fixed numbered Harpoon pin slots (`1`..`9`) that never shift on unpinned churn.
//! - Subtle hairline divider `│` separating pinned slots from unpinned tabs.
//! - Right side: Kept open tabs + at most 1 transient single-click preview tab at the far right.
//! - Far right utilities: `+` new tab, fullscreen zoom toggle (when split), and quiet list trigger.
//! - Full hit-testing and horizontal drag insertion-line support.

use editor_platform::{FontStyle, GlyphRasterizer, PixelRect};

use crate::atlas::GlyphAtlas;
use crate::painter::Painter;
use crate::sidebar::{draw_sidebar_panel_icon, SidebarSide};
use crate::theme::{px, Theme};

pub const TAB_STRIP_HEIGHT_PT: f32 = 30.0;

#[inline]
pub fn tab_strip_height_px(scale: f32) -> i32 {
    px(TAB_STRIP_HEIGHT_PT, scale)
}

/// Logical display entry for one pill in the Working-Set Strip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabDisplayItem {
    pub id: u32,
    pub label: String,
    /// `Some(1..=9)` for Harpoon pinned slots; `None` for unpinned tabs.
    pub pin_slot: Option<u8>,
    pub dirty: bool,
    pub is_preview: bool,
    /// Currently displayed in the active pane.
    pub is_active: bool,
    /// Currently displayed in an inactive visible split pane.
    pub in_other_pane: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabStripHit {
    None,
    ToggleSidebar,
    Tab(u32),
    CloseTab(u32),
    NewTab,
    ToggleZoom,
    OpenMenu,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StripDropTarget {
    /// Insert / pin into the pinned section at 0-based index `idx` (clamped to `0..=pinned.len()`).
    PinnedAt(usize),
    /// Insert / unpin into the unpinned open section at 0-based index `idx`.
    OpenAt(usize),
}

#[derive(Debug, Clone, Copy)]
struct RenderedTab {
    id: u32,
    is_pinned: bool,
    section_index: usize,
    rect: PixelRect,
    close_rect: PixelRect,
}

pub struct TabStrip {
    pub scroll_x_px: f32,
    hovered_tab: Option<u32>,
    hovered_close: Option<u32>,
    hovered_sidebar_btn: bool,
    tabs: Vec<RenderedTab>,
    divider_x: Option<i32>,
    pinned_count: usize,
    open_count: usize,
    tabs_clip_rect: PixelRect,
    sidebar_btn_rect: PixelRect,
    new_tab_rect: PixelRect,
    zoom_btn_rect: PixelRect,
    menu_btn_rect: PixelRect,
    last_rect: PixelRect,
}

impl Default for TabStrip {
    fn default() -> Self {
        Self::new()
    }
}

impl TabStrip {
    pub fn new() -> Self {
        Self {
            scroll_x_px: 0.0,
            hovered_tab: None,
            hovered_close: None,
            hovered_sidebar_btn: false,
            tabs: Vec::new(),
            divider_x: None,
            pinned_count: 0,
            open_count: 0,
            tabs_clip_rect: PixelRect::ZERO,
            sidebar_btn_rect: PixelRect::ZERO,
            new_tab_rect: PixelRect::ZERO,
            zoom_btn_rect: PixelRect::ZERO,
            menu_btn_rect: PixelRect::ZERO,
            last_rect: PixelRect::ZERO,
        }
    }

    pub fn set_hovered(&mut self, x_px: f32, y_px: f32) -> (bool, TabStripHit) {
        let hit = self.hit_test(x_px, y_px);
        let new_tab = match hit {
            TabStripHit::Tab(id) | TabStripHit::CloseTab(id) => Some(id),
            _ => None,
        };
        let new_close = match hit {
            TabStripHit::CloseTab(id) => Some(id),
            _ => None,
        };
        let new_sb = hit == TabStripHit::ToggleSidebar;
        let changed = self.hovered_tab != new_tab
            || self.hovered_close != new_close
            || self.hovered_sidebar_btn != new_sb;
        self.hovered_tab = new_tab;
        self.hovered_close = new_close;
        self.hovered_sidebar_btn = new_sb;
        (changed, hit)
    }

    pub fn clear_hover(&mut self) -> bool {
        let changed = self.hovered_tab.is_some()
            || self.hovered_close.is_some()
            || self.hovered_sidebar_btn;
        self.hovered_tab = None;
        self.hovered_close = None;
        self.hovered_sidebar_btn = false;
        changed
    }

    pub fn hit_test(&self, x_px: f32, y_px: f32) -> TabStripHit {
        let xi = x_px.round() as i32;
        let yi = y_px.round() as i32;
        if !self.last_rect.contains(xi, yi) {
            return TabStripHit::None;
        }
        if !self.sidebar_btn_rect.is_empty() && self.sidebar_btn_rect.contains(xi, yi) {
            return TabStripHit::ToggleSidebar;
        }
        if self.zoom_btn_rect.contains(xi, yi) {
            return TabStripHit::ToggleZoom;
        }
        if self.menu_btn_rect.contains(xi, yi) {
            return TabStripHit::OpenMenu;
        }
        if self.new_tab_rect.contains(xi, yi) {
            return TabStripHit::NewTab;
        }
        if self.tabs_clip_rect.contains(xi, yi) {
            for t in &self.tabs {
                if t.rect.contains(xi, yi) {
                    if !t.close_rect.is_empty() && t.close_rect.contains(xi, yi) {
                        return TabStripHit::CloseTab(t.id);
                    }
                    return TabStripHit::Tab(t.id);
                }
            }
        }
        TabStripHit::None
    }

    pub fn scroll_by(&mut self, dx_px: f32) {
        let content_right = self.tabs.last().map(|t| t.rect.right()).unwrap_or(0);
        let content_left = self.tabs.first().map(|t| t.rect.x).unwrap_or(0);
        let content_w = (content_right - content_left).max(0) as f32;
        let max_scroll = (content_w - self.tabs_clip_rect.w as f32).max(0.0);
        self.scroll_x_px = (self.scroll_x_px + dx_px).clamp(0.0, max_scroll);
    }

    /// Resolve where dropping a tab or file at `(x_px, y_px)` inside the strip would insert it.
    pub fn drop_target_at(&self, x_px: f32, y_px: f32) -> Option<(StripDropTarget, i32)> {
        let xi = x_px.round() as i32;
        let yi = y_px.round() as i32;
        if !self.last_rect.contains(xi, yi) {
            return None;
        }
        // Only treat the strip's tab region (left of the utility buttons) as drop area.
        if xi > self.tabs_clip_rect.right() + 24 {
            return None;
        }
        // If there's a divider, left side = Pinned section, right side = Open section.
        if let Some(div_x) = self.divider_x {
            if xi <= div_x {
                return Some(self.drop_in_section(true, xi));
            } else {
                return Some(self.drop_in_section(false, xi));
            }
        }
        // Only one section currently populated:
        if self.pinned_count > 0 && self.open_count == 0 {
            // Dragging past the far right of pinned tabs creates the unpinned section; otherwise reorder pins.
            if let Some(last_pin) = self.tabs.iter().rev().find(|t| t.is_pinned) {
                if xi > last_pin.rect.right() + 18 {
                    return Some((StripDropTarget::OpenAt(0), last_pin.rect.right() + 8));
                }
            }
            return Some(self.drop_in_section(true, xi));
        }
        // Only unpinned tabs exist right now: dropping near the very left edge pins it as slot 1!
        if self.pinned_count == 0 {
            if let Some(first_open) = self.tabs.first() {
                if xi < first_open.rect.x + 14 {
                    return Some((StripDropTarget::PinnedAt(0), first_open.rect.x));
                }
            }
            return Some(self.drop_in_section(false, xi));
        }
        None
    }

    fn drop_in_section(&self, pinned: bool, xi: i32) -> (StripDropTarget, i32) {
        let mut count = 0usize;
        let mut last_right = self.tabs_clip_rect.x + 4;
        for t in self.tabs.iter().filter(|t| t.is_pinned == pinned) {
            let mid = t.rect.x + t.rect.w / 2;
            if xi < mid {
                let target = if pinned {
                    StripDropTarget::PinnedAt(t.section_index)
                } else {
                    StripDropTarget::OpenAt(t.section_index)
                };
                return (target, t.rect.x);
            }
            count = t.section_index + 1;
            last_right = t.rect.right();
        }
        let target = if pinned {
            StripDropTarget::PinnedAt(count)
        } else {
            StripDropTarget::OpenAt(count)
        };
        (target, last_right)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        painter: &mut Painter,
        rect: PixelRect,
        items: &[TabDisplayItem],
        is_split: bool,
        is_zoomed: bool,
        merge_active_bottom: bool,
        drop_indicator_x: Option<i32>,
        minimized_sidebar_side: Option<SidebarSide>,
        theme: &Theme,
        atlas: &mut GlyphAtlas,
        rasterizer: &mut dyn GlyphRasterizer,
    ) {
        self.last_rect = rect;
        self.tabs.clear();
        self.divider_x = None;
        self.pinned_count = 0;
        self.open_count = 0;
        self.sidebar_btn_rect = PixelRect::ZERO;
        self.new_tab_rect = PixelRect::ZERO;
        self.zoom_btn_rect = PixelRect::ZERO;
        self.menu_btn_rect = PixelRect::ZERO;

        if rect.is_empty() {
            self.tabs_clip_rect = PixelRect::ZERO;
            return;
        }

        let scale = painter.scale();
        let hairline = (1.0 * scale).round().max(1.0) as i32;

        // Chrome ground (`theme.page`) + bottom hairline separator (`theme.border_subtle`).
        painter.fill_rect(rect, theme.page);
        painter.fill_rect(
            PixelRect::new(rect.x, rect.bottom() - hairline, rect.w, hairline),
            theme.border_subtle,
        );

        let btn_h = rect.h - hairline;
        let mut left_edge = rect.x + px(4.0, scale);
        let mut right_edge = rect.right() - px(4.0, scale);

        // Quiet sidebar-expand button shown when the sidebar is minimized (on its docked edge!).
        if let Some(side) = minimized_sidebar_side {
            let sb_w = px(24.0, scale);
            let sb_h = (btn_h - px(6.0, scale)).max(12);
            let sb_y = rect.y + (btn_h - sb_h) / 2;
            let sb_x = match side {
                SidebarSide::Left => {
                    let x = left_edge;
                    left_edge += sb_w + px(4.0, scale);
                    x
                }
                SidebarSide::Right => {
                    right_edge -= sb_w;
                    let x = right_edge;
                    right_edge -= px(2.0, scale);
                    x
                }
            };
            self.sidebar_btn_rect = PixelRect::new(sb_x, sb_y, sb_w, sb_h);
            if self.hovered_sidebar_btn {
                painter.fill_rounded_rect(self.sidebar_btn_rect, 4.0 * scale, theme.current_line);
            }
            let sb_col = if self.hovered_sidebar_btn {
                theme.text
            } else {
                theme.text_muted
            };
            draw_sidebar_panel_icon(painter, self.sidebar_btn_rect, side, false, sb_col, scale);
        }

        // Right-side utility buttons (right-to-left): [Zoom (if split)] [Menu ⌘E] [+]
        // 1. Harpoon / open list button (quiet 3-bar icon).
        let menu_w = px(24.0, scale);
        right_edge -= menu_w;
        self.menu_btn_rect = PixelRect::new(right_edge, rect.y, menu_w, btn_h);
        draw_list_icon(painter, self.menu_btn_rect, theme.text_muted, scale);

        // 2. Fullscreen zoom toggle button (shown only when split panes exist).
        if is_split {
            let zoom_w = if is_zoomed {
                px(54.0, scale)
            } else {
                px(24.0, scale)
            };
            right_edge -= zoom_w + px(2.0, scale);
            self.zoom_btn_rect = PixelRect::new(right_edge, rect.y, zoom_w, btn_h);
            if is_zoomed {
                let pill = PixelRect::new(
                    self.zoom_btn_rect.x,
                    rect.y + px(4.0, scale),
                    self.zoom_btn_rect.w,
                    rect.h - px(9.0, scale),
                );
                painter.fill_rounded_rect(pill, 4.0 * scale, theme.selection);
                let fs = 10.5 * scale;
                let m = atlas.metrics(rasterizer, FontStyle::UiBold, fs);
                let lbl = "Zoomed";
                let lw = Painter::measure_text_ui(lbl, FontStyle::UiBold, fs, atlas, rasterizer);
                let base = pill.y as f32 + (pill.h as f32 + m.ascent - m.descent) * 0.5;
                painter.draw_text_ui(
                    pill.x as f32 + (pill.w as f32 - lw) * 0.5,
                    base,
                    lbl,
                    FontStyle::UiBold,
                    fs,
                    theme.text,
                    atlas,
                    rasterizer,
                );
            } else {
                draw_expand_icon(painter, self.zoom_btn_rect, theme.text_muted, scale);
            }
        }

        // 3. New tab `+` button.
        let plus_w = px(24.0, scale);
        right_edge -= plus_w + px(2.0, scale);
        self.new_tab_rect = PixelRect::new(right_edge, rect.y, plus_w, btn_h);
        let plus_fs = 14.0 * scale;
        let pm = atlas.metrics(rasterizer, FontStyle::Ui, plus_fs);
        let pw = Painter::measure_text_ui("+", FontStyle::Ui, plus_fs, atlas, rasterizer);
        let pbase = rect.y as f32 + (btn_h as f32 + pm.ascent - pm.descent) * 0.5;
        painter.draw_text_ui(
            self.new_tab_rect.x as f32 + (self.new_tab_rect.w as f32 - pw) * 0.5,
            pbase,
            "+",
            FontStyle::Ui,
            plus_fs,
            theme.text_muted,
            atlas,
            rasterizer,
        );

        // Clip region for scrollable tab pills.
        let tabs_right = (right_edge - px(4.0, scale)).max(left_edge);
        self.tabs_clip_rect = PixelRect::new(
            left_edge,
            rect.y,
            (tabs_right - left_edge).max(0),
            rect.h,
        );

        let font_px = 11.5 * scale;
        let badge_px = 10.0 * scale;
        let fm = atlas.metrics(rasterizer, FontStyle::Ui, font_px);
        let bm = atlas.metrics(rasterizer, FontStyle::UiBold, badge_px);
        let close_w = px(18.0, scale);
        let pad_l = px(8.0, scale);
        let pad_r = px(6.0, scale);
        let pill_gap = px(2.0, scale);
        let badge_w = px(15.0, scale);
        let badge_h = px(15.0, scale);
        let badge_gap = px(5.0, scale);
        let dot_space = px(10.0, scale);
        let divider_gap = px(10.0, scale);

        // Pre-measure every tab pill width so we can clamp scroll & keep active tab visible.
        let mut widths: Vec<i32> = Vec::with_capacity(items.len());
        let mut has_pinned = false;
        let mut has_unpinned = false;
        for it in items {
            if it.pin_slot.is_some() {
                has_pinned = true;
                self.pinned_count += 1;
            } else {
                has_unpinned = true;
                self.open_count += 1;
            }
            let style = if it.is_active && !it.is_preview {
                FontStyle::UiBold
            } else {
                FontStyle::Ui
            };
            let lw = Painter::measure_text_ui(&it.label, style, font_px, atlas, rasterizer);
            let mut w = pad_l + lw.ceil() as i32 + pad_r;
            if it.pin_slot.is_some() {
                w += badge_w + badge_gap;
            }
            if it.dirty || it.in_other_pane {
                w += dot_space;
            }
            // Reserve space for quiet `×` close button on tabs (keeps width stable on hover!).
            w += close_w;
            let min_w = px(60.0, scale);
            let max_w = px(210.0, scale);
            widths.push(w.clamp(min_w, max_w));
        }

        let mut total_w = 0i32;
        for (i, w) in widths.iter().enumerate() {
            if i > 0 {
                total_w += pill_gap;
                if has_pinned
                    && has_unpinned
                    && items[i - 1].pin_slot.is_some()
                    && items[i].pin_slot.is_none()
                {
                    total_w += divider_gap;
                }
            }
            total_w += *w;
        }
        let max_scroll = (total_w - self.tabs_clip_rect.w).max(0) as f32;
        self.scroll_x_px = self.scroll_x_px.clamp(0.0, max_scroll);

        // Ensure the active tab is inside the visible horizontal window.
        let mut cur_rel_x = 0i32;
        for (i, it) in items.iter().enumerate() {
            if i > 0 {
                cur_rel_x += pill_gap;
                if has_pinned
                    && has_unpinned
                    && items[i - 1].pin_slot.is_some()
                    && it.pin_slot.is_none()
                {
                    cur_rel_x += divider_gap;
                }
            }
            let w = widths[i];
            if it.is_active {
                let left = cur_rel_x as f32;
                let right = (cur_rel_x + w) as f32;
                if left < self.scroll_x_px {
                    self.scroll_x_px = left;
                } else if right > self.scroll_x_px + self.tabs_clip_rect.w as f32 {
                    self.scroll_x_px = (right - self.tabs_clip_rect.w as f32).clamp(0.0, max_scroll);
                }
                break;
            }
            cur_rel_x += w;
        }

        painter.push_clip(self.tabs_clip_rect);

        let mut x = self.tabs_clip_rect.x - self.scroll_x_px.round() as i32;
        let mut pin_idx = 0usize;
        let mut open_idx = 0usize;

        for (i, it) in items.iter().enumerate() {
            if i > 0 {
                x += pill_gap;
                if has_pinned
                    && has_unpinned
                    && items[i - 1].pin_slot.is_some()
                    && it.pin_slot.is_none()
                {
                    let dx = x + (divider_gap - hairline) / 2;
                    let dy = rect.y + px(6.0, scale);
                    let dh = (rect.h - px(13.0, scale)).max(hairline);
                    painter.fill_rect(PixelRect::new(dx, dy, hairline, dh), theme.border_subtle);
                    self.divider_x = Some(dx);
                    x += divider_gap;
                }
            }

            let w = widths[i];
            let is_hovered = self.hovered_tab == Some(it.id);
            let pill_top = rect.y + px(3.0, scale);
            let pill_h = if it.is_active && merge_active_bottom {
                rect.bottom() - pill_top
            } else {
                rect.h - px(5.0, scale)
            };
            let pill = PixelRect::new(x, pill_top, w, pill_h);

            // Pill background & subtle hairline framing.
            if it.is_active {
                let r = 5.0 * scale;
                painter.fill_rounded_rect(pill, r, theme.surface);
                // Top/side subtle hairline border for crisp separation from `theme.page`.
                painter.stroke_rounded_rect(pill, r, hairline as f32, theme.border_subtle);
                if merge_active_bottom {
                    // Cover bottom hairline so active tab merges cleanly into the document surface!
                    painter.fill_rect(
                        PixelRect::new(
                            pill.x + hairline,
                            rect.bottom() - px(4.0, scale),
                            (pill.w - 2 * hairline).max(0),
                            px(4.0, scale),
                        ),
                        theme.surface,
                    );
                }
            } else if is_hovered {
                painter.fill_rounded_rect(pill, 4.0 * scale, theme.current_line);
            }

            let mut tx = (pill.x + pad_l) as f32;
            let content_cy = rect.y as f32 + (rect.h - hairline) as f32 * 0.5;

            // 1. Optional Harpoon slot keycap badge (`1`..`9`).
            if let Some(slot) = it.pin_slot {
                let by = (content_cy - badge_h as f32 * 0.5).round() as i32;
                let brect = PixelRect::new(tx.round() as i32, by, badge_w, badge_h);
                let badge_bg = if it.is_active {
                    theme.selection
                } else {
                    theme.current_line
                };
                painter.fill_rounded_rect(brect, 3.0 * scale, badge_bg);
                painter.stroke_rounded_rect(brect, 3.0 * scale, hairline as f32, theme.border_subtle);
                let s = slot.to_string();
                let sw = Painter::measure_text_ui(&s, FontStyle::UiBold, badge_px, atlas, rasterizer);
                let bbase = brect.y as f32 + (brect.h as f32 + bm.ascent - bm.descent) * 0.5;
                let bcol = if it.is_active {
                    theme.text
                } else {
                    theme.text_muted
                };
                painter.draw_text_ui(
                    brect.x as f32 + (brect.w as f32 - sw) * 0.5,
                    bbase,
                    &s,
                    FontStyle::UiBold,
                    badge_px,
                    bcol,
                    atlas,
                    rasterizer,
                );
                tx = (brect.right() + badge_gap) as f32;
            }

            // 2. Right-side close `×` button hit rect.
            let close_rect = PixelRect::new(
                pill.right() - close_w - px(2.0, scale),
                rect.y,
                close_w,
                btn_h,
            );

            // 3. Filename label (clipped cleanly before dirty dot & close `×`).
            let right_reserve = close_w + px(4.0, scale) + if it.dirty || it.in_other_pane { dot_space } else { 0 };
            let text_max_right = (pill.right() - right_reserve).max(tx.ceil() as i32);
            let label_style = if it.is_active && !it.is_preview {
                FontStyle::UiBold
            } else {
                FontStyle::Ui
            };
            let label_color = if it.is_active {
                theme.text
            } else {
                theme.text_muted
            };
            let baseline = rect.y as f32 + ((rect.h - hairline) as f32 + fm.ascent - fm.descent) * 0.5;
            painter.push_clip(PixelRect::new(
                tx.floor() as i32,
                rect.y,
                (text_max_right - tx.floor() as i32).max(0),
                btn_h,
            ));
            let after_text = painter.draw_text_ui(
                tx,
                baseline,
                &it.label,
                label_style,
                font_px,
                label_color,
                atlas,
                rasterizer,
            );
            painter.pop_clip();

            // Subtle quiet underline on the transient preview slot so user feels it's preview-only.
            if it.is_preview {
                let ul_w = ((after_text - tx).round() as i32).min((text_max_right - tx.floor() as i32).max(0));
                if ul_w > 0 {
                    painter.fill_rect(
                        PixelRect::new(
                            tx.floor() as i32,
                            (baseline + px(2.0, scale) as f32).round() as i32,
                            ul_w,
                            hairline,
                        ),
                        theme.border_subtle,
                    );
                }
            }

            // 4. Dirty dot (`•`) or subtle visible-in-other-pane indicator.
            if it.dirty {
                let dot_x = (after_text.min(text_max_right as f32) + px(4.0, scale) as f32).round() as i32;
                let dot_sz = px(4.0, scale).max(2);
                let dot_y = (content_cy - dot_sz as f32 * 0.5).round() as i32;
                painter.fill_rounded_rect(
                    PixelRect::new(dot_x, dot_y, dot_sz, dot_sz),
                    dot_sz as f32 * 0.5,
                    theme.text,
                );
            } else if it.in_other_pane {
                let dot_x = (after_text.min(text_max_right as f32) + px(4.0, scale) as f32).round() as i32;
                let dot_sz = px(4.0, scale).max(2);
                let dot_y = (content_cy - dot_sz as f32 * 0.5).round() as i32;
                painter.stroke_rounded_rect(
                    PixelRect::new(dot_x, dot_y, dot_sz, dot_sz),
                    dot_sz as f32 * 0.5,
                    hairline as f32,
                    theme.text_muted,
                );
            }

            // 5. Quiet `×` close glyph (visible when active or hovered, keeping chrome calm).
            if it.is_active || is_hovered {
                let close_fs = 12.0 * scale;
                let cm = atlas.metrics(rasterizer, FontStyle::Ui, close_fs);
                let cw = Painter::measure_text_ui("×", FontStyle::Ui, close_fs, atlas, rasterizer);
                let cbase = rect.y as f32 + (btn_h as f32 + cm.ascent - cm.descent) * 0.5;
                let close_col = if self.hovered_close == Some(it.id) {
                    theme.text
                } else {
                    theme.text_muted
                };
                painter.draw_text_ui(
                    close_rect.x as f32 + (close_rect.w as f32 - cw) * 0.5,
                    cbase,
                    "×",
                    FontStyle::Ui,
                    close_fs,
                    close_col,
                    atlas,
                    rasterizer,
                );
            }

            let section_index = if it.pin_slot.is_some() {
                let idx = pin_idx;
                pin_idx += 1;
                idx
            } else {
                let idx = open_idx;
                open_idx += 1;
                idx
            };

            self.tabs.push(RenderedTab {
                id: it.id,
                is_pinned: it.pin_slot.is_some(),
                section_index,
                rect: PixelRect::new(x, rect.y, w, btn_h),
                close_rect,
            });

            x += w;
        }

        // Crisp vertical drop-insertion bar when dragging a tab or file over the strip.
        if let Some(ix) = drop_indicator_x {
            let bar_w = px(2.0, scale).max(2);
            let bar_y = rect.y + px(4.0, scale);
            let bar_h = (rect.h - px(9.0, scale)).max(bar_w);
            painter.fill_rounded_rect(
                PixelRect::new(ix - bar_w / 2, bar_y, bar_w, bar_h),
                1.0 * scale,
                theme.text,
            );
        }

        painter.pop_clip();
    }
}

/// Crisp geometric 3-line list icon (no emoji/symbol font dependency).
fn draw_list_icon(painter: &mut Painter, rect: PixelRect, color: crate::theme::Color, scale: f32) {
    let lw = px(10.0, scale);
    let lh = (1.5 * scale).round().max(1.0) as i32;
    let gap = px(3.0, scale);
    let total_h = lh * 3 + gap * 2;
    let x = rect.x + (rect.w - lw) / 2;
    let y0 = rect.y + (rect.h - total_h) / 2;
    for i in 0..3 {
        painter.fill_rect(PixelRect::new(x, y0 + i * (lh + gap), lw, lh), color);
    }
}

/// Crisp geometric fullscreen-expand icon (four corner brackets).
fn draw_expand_icon(painter: &mut Painter, rect: PixelRect, color: crate::theme::Color, scale: f32) {
    let sz = px(10.0, scale);
    let arm = px(3.5, scale).max(2);
    let t = (1.25 * scale).round().max(1.0) as i32;
    let x0 = rect.x + (rect.w - sz) / 2;
    let y0 = rect.y + (rect.h - sz) / 2;
    let x1 = x0 + sz;
    let y1 = y0 + sz;
    // Top-left
    painter.fill_rect(PixelRect::new(x0, y0, arm, t), color);
    painter.fill_rect(PixelRect::new(x0, y0, t, arm), color);
    // Top-right
    painter.fill_rect(PixelRect::new(x1 - arm, y0, arm, t), color);
    painter.fill_rect(PixelRect::new(x1 - t, y0, t, arm), color);
    // Bottom-left
    painter.fill_rect(PixelRect::new(x0, y1 - t, arm, t), color);
    painter.fill_rect(PixelRect::new(x0, y1 - arm, t, arm), color);
    // Bottom-right
    painter.fill_rect(PixelRect::new(x1 - arm, y1 - t, arm, t), color);
    painter.fill_rect(PixelRect::new(x1 - t, y1 - arm, t, arm), color);
}
