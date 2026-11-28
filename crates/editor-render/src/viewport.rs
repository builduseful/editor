//! Document viewport: pixel-smooth scroll, soft wrap, stable gutter, selection,
//! syntax-highlighted + emoji rendering, and custom-drawn overlay scrollbars.

use std::collections::HashMap;

use editor_core::{
    cluster_cell_width, grapheme_clusters, lookup_line_diff,
    swar::{find_byte, is_all_ascii},
    Document, LineDiffKind, LineDiffMark, SearchMatch, TokenKind, TokenSpan,
};
use editor_platform::{FontMetrics, FontStyle, GlyphRasterizer, PixelRect};

use crate::atlas::GlyphAtlas;
use crate::chrome::ImeComposition;
use crate::painter::Painter;
use crate::theme::{px, Theme};

pub const H_SCROLLBAR_STRIP_PT: f32 = 9.0;
pub const V_SCROLLBAR_WIDTH_PT: f32 = 9.0;
const MAX_WRAP_CACHE_ENTRIES: usize = 1024;
const MAX_CACHED_VISUAL_LINES: usize = 512;
const MAX_EXACT_WRAP_METRICS_LINES: usize = 4096;
const HUGE_LINE_WINDOW_THRESHOLD: u64 = 16 * 1024;
const HUGE_LINE_WINDOW_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy, Default)]
pub struct ViewportLayout {
    pub full: PixelRect,
    pub gutter: PixelRect,
    pub text: PixelRect,
    pub v_bar: PixelRect,
    pub h_bar: PixelRect,
    pub v_thumb: PixelRect,
    pub h_thumb: PixelRect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollbarHit {
    None,
    VerticalThumb,
    VerticalTrack,
    HorizontalThumb,
    HorizontalTrack,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocHit {
    pub byte_offset: u64,
    pub line: usize,
    pub col_bytes: usize,
}

#[derive(Debug, Clone)]
struct WrappedSegment {
    /// Byte range `[byte_start, byte_end)` inside the logical line string.
    byte_start: usize,
    byte_end: usize,
    width_px: f32,
}

pub struct Viewport {
    pub font_size_pt: f32,
    pub tab_width: usize,
    pub word_wrap: bool,
    /// Top logical line currently at or above the top of the viewport.
    pub scroll_top_line: usize,
    /// Pixel offset into `scroll_top_line` (for pixel-smooth scrolling & wrapped lines).
    pub scroll_sub_y_px: f32,
    /// Horizontal scroll in physical pixels (when `word_wrap == false`).
    pub scroll_x_px: f32,
    /// Maximum line width seen so far (for horizontal scrollbar range).
    pub max_line_width_px: f32,
    pub v_bar_hovered: bool,
    pub h_bar_hovered: bool,
    pub v_bar_dragging: bool,
    pub h_bar_dragging: bool,
    drag_anchor_px: f32,
    drag_start_scroll: f32,
    /// Cached line wrapping keyed by logical line index, invalidated on doc gen / width / font change.
    wrap_cache: HashMap<usize, Vec<WrappedSegment>>,
    wrap_cache_doc_gen: u64,
    wrap_cache_width: i32,
    wrap_cache_font_q: u32,
    /// Prefix sums of visual wrapped line counts (`len == total_logical + 1` for files <= 4096 lines).
    /// Enables exact O(1) vertical scrollbar thumb sizing and O(log N) thumb dragging in wrap mode.
    wrapped_prefix: Vec<usize>,
    wrapped_total_visual_lines: usize,
    wrapped_metrics_doc_gen: u64,
    wrapped_metrics_width: i32,
    wrapped_metrics_font_q: u32,
    wrapped_metrics_line_count: usize,
    pub layout: ViewportLayout,
    pub last_caret_rect: PixelRect,
    /// Render a cell-wide block cursor (used in Vim Normal / Visual modes) instead of a thin bar.
    pub block_caret: bool,
    /// Optional secondary `(anchor, caret)` byte-offset pairs for multi-cursor rendering.
    pub extra_cursors: Vec<(u64, u64)>,
    /// Sorted compact live Git gutter diff marks (`O(log H)` lookup per visible line).
    pub git_diff: Vec<LineDiffMark>,
}

impl Default for Viewport {
    fn default() -> Self {
        Self::new()
    }
}

impl Viewport {
    pub fn new() -> Self {
        Self {
            font_size_pt: 13.5,
            tab_width: 4,
            word_wrap: false,
            scroll_top_line: 0,
            scroll_sub_y_px: 0.0,
            scroll_x_px: 0.0,
            max_line_width_px: 0.0,
            v_bar_hovered: false,
            h_bar_hovered: false,
            v_bar_dragging: false,
            h_bar_dragging: false,
            drag_anchor_px: 0.0,
            drag_start_scroll: 0.0,
            wrap_cache: HashMap::new(),
            wrap_cache_doc_gen: 0,
            wrap_cache_width: 0,
            wrap_cache_font_q: 0,
            wrapped_prefix: Vec::new(),
            wrapped_total_visual_lines: 0,
            wrapped_metrics_doc_gen: 0,
            wrapped_metrics_width: 0,
            wrapped_metrics_font_q: 0,
            wrapped_metrics_line_count: 0,
            layout: ViewportLayout::default(),
            last_caret_rect: PixelRect::ZERO,
            block_caret: false,
            extra_cursors: Vec::new(),
            git_diff: Vec::new(),
        }
    }

    fn clear_wrap_caches(&mut self) {
        self.wrap_cache.clear();
        self.wrapped_prefix.clear();
        self.wrapped_total_visual_lines = 0;
        self.wrapped_metrics_width = 0;
        self.wrapped_metrics_line_count = 0;
    }

    pub fn reset_for_document(&mut self) {
        self.scroll_top_line = 0;
        self.scroll_sub_y_px = 0.0;
        self.scroll_x_px = 0.0;
        self.max_line_width_px = 0.0;
        self.clear_wrap_caches();
    }

    pub fn font_px(&self, scale: f32) -> f32 {
        (self.font_size_pt * scale).clamp(8.0, 96.0)
    }

    pub fn zoom_in(&mut self) {
        let old = self.font_size_pt.max(1.0);
        self.font_size_pt = (self.font_size_pt + 1.5).min(36.0);
        let ratio = self.font_size_pt / old;
        self.max_line_width_px *= ratio;
        self.scroll_x_px *= ratio;
        self.clear_wrap_caches();
    }

    pub fn zoom_out(&mut self) {
        let old = self.font_size_pt.max(1.0);
        self.font_size_pt = (self.font_size_pt - 1.5).max(8.0);
        let ratio = self.font_size_pt / old;
        self.max_line_width_px *= ratio;
        self.scroll_x_px *= ratio;
        self.clear_wrap_caches();
    }

    pub fn zoom_reset(&mut self) {
        let old = self.font_size_pt.max(1.0);
        self.font_size_pt = 13.5;
        let ratio = self.font_size_pt / old;
        self.max_line_width_px *= ratio;
        self.scroll_x_px *= ratio;
        self.clear_wrap_caches();
    }

    pub fn toggle_wrap(&mut self) {
        self.word_wrap = !self.word_wrap;
        if self.word_wrap {
            self.scroll_x_px = 0.0;
        }
        self.clear_wrap_caches();
    }

    fn sync_wrap_cache(&mut self, doc_gen: u64, text_w: i32, font_px: f32) {
        let fq = (font_px * 4.0).round() as u32;
        if self.wrap_cache_doc_gen != doc_gen
            || self.wrap_cache_width != text_w
            || self.wrap_cache_font_q != fq
        {
            self.wrap_cache.clear();
            self.wrap_cache_doc_gen = doc_gen;
            self.wrap_cache_width = text_w;
            self.wrap_cache_font_q = fq;
        }
        if self.wrap_cache.len() > MAX_WRAP_CACHE_ENTRIES {
            self.wrap_cache.clear();
        }
    }

    /// Zero-allocation exact count of visual rows `text` wraps into (matches `layout_line` 1:1).
    fn count_wrapped_visual_lines(&self, text: &str, cell_w: f32, max_w: f32) -> usize {
        if !self.word_wrap || max_w <= cell_w * 2.0 {
            return 1;
        }
        let max_cells = (max_w / cell_w).floor().max(4.0) as usize;
        // Fast path: tab-free ASCII line shorter than viewport width never wraps.
        if text.len() <= max_cells
            && is_all_ascii(text.as_bytes())
            && find_byte(text.as_bytes(), b'\t').is_none()
        {
            return 1;
        }
        let mut segs = 0usize;
        let mut seg_start = 0usize;
        let mut col_cells = 0usize;
        let mut last_break_byte: Option<usize> = None;

        for (off, cluster) in grapheme_clusters(text) {
            let mut cw = if cluster == "\t" {
                self.tab_width - (col_cells % self.tab_width)
            } else {
                cluster_cell_width(cluster) as usize
            };
            if col_cells > 0 && col_cells + cw > max_cells {
                if let Some(brk) = last_break_byte {
                    if brk > seg_start {
                        segs += 1;
                        seg_start = brk;
                        col_cells = self
                            .measure_prefix_px(&text[seg_start..off], off - seg_start, 1.0)
                            .round() as usize;
                        last_break_byte = None;
                    } else {
                        segs += 1;
                        seg_start = off;
                        col_cells = 0;
                        last_break_byte = None;
                    }
                } else {
                    segs += 1;
                    seg_start = off;
                    col_cells = 0;
                }
                if cluster == "\t" {
                    cw = self.tab_width - (col_cells % self.tab_width);
                }
                if col_cells > 0 && col_cells + cw > max_cells {
                    segs += 1;
                    seg_start = off;
                    col_cells = 0;
                    if cluster == "\t" {
                        cw = self.tab_width;
                    }
                }
            }
            col_cells += cw;
            if cluster == " " || cluster == "\t" {
                last_break_byte = Some(off + cluster.len());
            }
        }
        segs + 1
    }

    /// Keep cached visual wrapped line counts in sync with `(doc_gen, text_w, font_px, lines)`.
    /// O(1) when unchanged; exact prefix sums for <= 4096 lines; bounded 128-line sample for huge files.
    fn sync_wrapped_metrics(
        &mut self,
        doc: &mut Document,
        text_w: i32,
        metrics: FontMetrics,
        scale: f32,
    ) {
        if !self.word_wrap || text_w <= 0 {
            if !self.wrapped_prefix.is_empty() || self.wrapped_total_visual_lines != 0 {
                self.wrapped_prefix.clear();
                self.wrapped_total_visual_lines = 0;
                self.wrapped_metrics_width = 0;
                self.wrapped_metrics_line_count = 0;
            }
            return;
        }
        let font_px = self.font_px(scale);
        let fq = (font_px * 4.0).round() as u32;
        let doc_gen = doc.generation();
        let total_logical = doc.editable_line_count().max(1);
        if self.wrapped_metrics_doc_gen == doc_gen
            && self.wrapped_metrics_width == text_w
            && self.wrapped_metrics_font_q == fq
            && self.wrapped_metrics_line_count == total_logical
            && self.wrapped_total_visual_lines > 0
        {
            return;
        }
        self.wrapped_metrics_doc_gen = doc_gen;
        self.wrapped_metrics_width = text_w;
        self.wrapped_metrics_font_q = fq;
        self.wrapped_metrics_line_count = total_logical;

        let text_pad_x = px(12.0, scale) as f32;
        let max_wrap_w = (text_w as f32 - text_pad_x * 2.0).max(metrics.cell_width * 4.0);

        if doc.is_fully_indexed() && total_logical <= MAX_EXACT_WRAP_METRICS_LINES {
            self.wrapped_prefix.clear();
            self.wrapped_prefix.reserve(total_logical + 1);
            let mut acc = 0usize;
            self.wrapped_prefix.push(0);
            for line in 0..total_logical {
                let content = doc.line_content(line).unwrap_or_default();
                acc += self.count_wrapped_visual_lines(&content, metrics.cell_width, max_wrap_w);
                self.wrapped_prefix.push(acc);
            }
            self.wrapped_total_visual_lines = acc.max(1);
        } else {
            self.wrapped_prefix.clear();
            let sample_start = self
                .scroll_top_line
                .min(total_logical.saturating_sub(1));
            let sample_end = (sample_start + 128).min(total_logical);
            let mut sampled_vis = 0usize;
            let mut sampled_log = 0usize;
            for line in sample_start..sample_end {
                let content = doc.line_content(line).unwrap_or_default();
                sampled_vis +=
                    self.count_wrapped_visual_lines(&content, metrics.cell_width, max_wrap_w);
                sampled_log += 1;
            }
            let avg = (sampled_vis.max(1) as f64 / sampled_log.max(1) as f64).max(1.0);
            let est_logical = doc.estimated_editable_line_count().max(total_logical);
            self.wrapped_metrics_line_count = est_logical;
            self.wrapped_total_visual_lines =
                ((est_logical as f64) * avg).ceil().max(est_logical as f64) as usize;
        }
    }

    fn current_visual_scroll_y(&self, line_h: f32) -> f32 {
        if self.word_wrap && !self.wrapped_prefix.is_empty() {
            let max_idx = self.wrapped_prefix.len().saturating_sub(1);
            let vis_before = self.wrapped_prefix[self.scroll_top_line.min(max_idx)];
            vis_before as f32 * line_h + self.scroll_sub_y_px
        } else if self.word_wrap
            && self.wrapped_total_visual_lines > 0
            && self.wrapped_metrics_line_count > 0
        {
            let ratio =
                self.wrapped_total_visual_lines as f32 / self.wrapped_metrics_line_count as f32;
            self.scroll_top_line as f32 * line_h * ratio + self.scroll_sub_y_px
        } else {
            self.scroll_top_line as f32 * line_h + self.scroll_sub_y_px
        }
    }

    /// Compute stable gutter width from the estimated total line count (never jitters mid-scroll).
    fn compute_gutter_width(&self, doc: &Document, cell_w: f32, scale: f32) -> i32 {
        let est_lines = doc.estimated_editable_line_count().max(1);
        let digits = ((est_lines as f64).log10().floor() as usize + 1).max(3);
        let pad_left = px(12.0, scale);
        let pad_right = px(12.0, scale);
        pad_left + (digits as f32 * cell_w).ceil() as i32 + pad_right
    }

    pub fn compute_layout(
        &mut self,
        rect: PixelRect,
        doc: &Document,
        metrics: FontMetrics,
        scale: f32,
    ) -> ViewportLayout {
        let gutter_w = self.compute_gutter_width(doc, metrics.cell_width, scale).min(rect.w);
        let h_strip = if self.word_wrap {
            0
        } else {
            px(H_SCROLLBAR_STRIP_PT, scale)
        };
        let v_bar_w = px(V_SCROLLBAR_WIDTH_PT, scale);

        let text_h = (rect.h - h_strip).max(0);
        let text_w = (rect.w - gutter_w).max(0);

        let gutter = PixelRect::new(rect.x, rect.y, gutter_w, rect.h);
        let text = PixelRect::new(rect.x + gutter_w, rect.y, text_w, text_h);
        let v_bar = PixelRect::new(rect.right() - v_bar_w, rect.y, v_bar_w, text_h);
        let h_bar = PixelRect::new(rect.x + gutter_w, rect.bottom() - h_strip, text_w, h_strip);

        // Compute vertical thumb (visual wrapped height when `word_wrap` is on).
        let est_logical = doc.estimated_editable_line_count().max(1);
        let total_visual_lines = if self.word_wrap && self.wrapped_total_visual_lines > 0 {
            self.wrapped_total_visual_lines.max(est_logical)
        } else {
            est_logical
        };
        let line_h = metrics.line_height.max(1) as f32;
        let total_h = total_visual_lines as f32 * line_h;
        let v_thumb = if total_h > text_h as f32 && text_h > 0 {
            let cur_y = self.current_visual_scroll_y(line_h);
            let max_y = (total_h - text_h as f32).max(1.0);
            let frac = (cur_y / max_y).clamp(0.0, 1.0);
            let thumb_h = ((text_h as f32 * text_h as f32) / total_h)
                .max(24.0 * scale)
                .min(text_h as f32)
                .round() as i32;
            let travel = (text_h - thumb_h).max(0);
            let thumb_y = v_bar.y + (frac * travel as f32).round() as i32;
            PixelRect::new(
                v_bar.x + px(2.0, scale),
                thumb_y,
                (v_bar.w - px(4.0, scale)).max(2),
                thumb_h,
            )
        } else {
            PixelRect::ZERO
        };

        // Clamp horizontal scroll when measured content fits (e.g. window widen or zoom out).
        if !self.word_wrap && text_w > 0 && self.max_line_width_px > 0.0 {
            let max_x = if self.max_line_width_px > text_w as f32 {
                (self.max_line_width_px + 32.0 * scale - text_w as f32).max(0.0)
            } else {
                0.0
            };
            self.scroll_x_px = self.scroll_x_px.clamp(0.0, max_x);
        }

        // Compute horizontal thumb.
        let h_thumb = if !self.word_wrap && self.max_line_width_px > text_w as f32 && text_w > 0 {
            let content_w = self.max_line_width_px + 32.0 * scale;
            let max_x = (content_w - text_w as f32).max(1.0);
            let frac = (self.scroll_x_px / max_x).clamp(0.0, 1.0);
            let thumb_w = ((text_w as f32 * text_w as f32) / content_w)
                .max(28.0 * scale)
                .min(text_w as f32)
                .round() as i32;
            let travel = (text_w - thumb_w).max(0);
            let thumb_x = h_bar.x + (frac * travel as f32).round() as i32;
            PixelRect::new(
                thumb_x,
                h_bar.y + px(2.0, scale),
                thumb_w,
                (h_bar.h - px(4.0, scale)).max(2),
            )
        } else {
            PixelRect::ZERO
        };

        self.layout = ViewportLayout {
            full: rect,
            gutter,
            text,
            v_bar,
            h_bar,
            v_thumb,
            h_thumb,
        };
        self.layout
    }

    pub fn hit_test_scrollbar(&self, x_px: f32, y_px: f32) -> ScrollbarHit {
        let xi = x_px.round() as i32;
        let yi = y_px.round() as i32;
        if !self.layout.v_thumb.is_empty() && self.layout.v_thumb.contains(xi, yi) {
            return ScrollbarHit::VerticalThumb;
        }
        if !self.layout.v_thumb.is_empty() && self.layout.v_bar.contains(xi, yi) {
            return ScrollbarHit::VerticalTrack;
        }
        if !self.layout.h_thumb.is_empty() && self.layout.h_thumb.contains(xi, yi) {
            return ScrollbarHit::HorizontalThumb;
        }
        if !self.layout.h_thumb.is_empty() && self.layout.h_bar.contains(xi, yi) {
            return ScrollbarHit::HorizontalTrack;
        }
        ScrollbarHit::None
    }

    pub fn begin_scrollbar_drag(&mut self, hit: ScrollbarHit, x_px: f32, y_px: f32, metrics: FontMetrics) {
        let line_h = metrics.line_height.max(1) as f32;
        match hit {
            ScrollbarHit::VerticalThumb => {
                self.v_bar_dragging = true;
                self.drag_anchor_px = y_px;
                self.drag_start_scroll = self.current_visual_scroll_y(line_h);
            }
            ScrollbarHit::HorizontalThumb => {
                self.h_bar_dragging = true;
                self.drag_anchor_px = x_px;
                self.drag_start_scroll = self.scroll_x_px;
            }
            _ => {}
        }
    }

    pub fn drag_scrollbar(
        &mut self,
        x_px: f32,
        y_px: f32,
        doc: &mut Document,
        metrics: FontMetrics,
        scale: f32,
    ) -> bool {
        let line_h = metrics.line_height.max(1) as f32;
        if self.v_bar_dragging {
            if self.word_wrap {
                self.sync_wrapped_metrics(doc, self.layout.text.w, metrics, scale);
            }
            let est_logical = doc.estimated_editable_line_count().max(1);
            let total_visual = if self.word_wrap && self.wrapped_total_visual_lines > 0 {
                self.wrapped_total_visual_lines.max(est_logical)
            } else {
                est_logical
            };
            let total_h = total_visual as f32 * line_h;
            let text_h = self.layout.text.h.max(1) as f32;
            let travel = (self.layout.v_bar.h - self.layout.v_thumb.h).max(1) as f32;
            let max_y = (total_h - text_h).max(0.0);
            let dy = y_px - self.drag_anchor_px;
            let target = (self.drag_start_scroll + (dy / travel) * max_y).clamp(0.0, max_y);
            self.set_vertical_scroll_px(target, doc, metrics, scale);
            return true;
        }
        if self.h_bar_dragging && !self.word_wrap {
            let content_w = self.max_line_width_px + 32.0 * scale;
            let text_w = self.layout.text.w.max(1) as f32;
            let travel = (self.layout.h_bar.w - self.layout.h_thumb.w).max(1) as f32;
            let max_x = (content_w - text_w).max(0.0);
            let dx = x_px - self.drag_anchor_px;
            self.scroll_x_px = (self.drag_start_scroll + (dx / travel) * max_x).clamp(0.0, max_x);
            return true;
        }
        false
    }

    pub fn end_scrollbar_drag(&mut self) {
        self.v_bar_dragging = false;
        self.h_bar_dragging = false;
    }

    pub fn scroll_by(
        &mut self,
        dx_px: f32,
        dy_px: f32,
        doc: &mut Document,
        metrics: FontMetrics,
        scale: f32,
    ) {
        let line_h = metrics.line_height.max(1) as f32;
        if dy_px.abs() > 0.01 {
            if self.word_wrap {
                self.scroll_wrapped_by(dy_px, doc, metrics, scale);
            } else {
                let cur = self.scroll_top_line as f32 * line_h + self.scroll_sub_y_px;
                let target = (cur + dy_px).max(0.0);
                self.set_vertical_scroll_px(target, doc, metrics, scale);
            }
        }
        if !self.word_wrap && dx_px.abs() > 0.01 {
            let max_x = (self.max_line_width_px + 32.0 * scale - self.layout.text.w as f32).max(0.0);
            self.scroll_x_px = (self.scroll_x_px + dx_px).clamp(0.0, max_x);
        }
    }

    /// Pixel-smooth vertical stepping in word-wrap mode (walks actual wrapped visual line heights).
    fn scroll_wrapped_by(
        &mut self,
        dy_px: f32,
        doc: &mut Document,
        metrics: FontMetrics,
        scale: f32,
    ) {
        let line_h = metrics.line_height.max(1) as f32;
        let text_pad_x = px(12.0, scale) as f32;
        let max_wrap_w =
            (self.layout.text.w as f32 - text_pad_x * 2.0).max(metrics.cell_width * 4.0);
        let visible_lines = ((self.layout.text.h.max(1) as f32) / line_h).ceil().max(1.0) as usize;
        let extra_step = ((dy_px.abs() / line_h).ceil() as usize).min(4096);
        if dy_px > 0.0 {
            doc.tree_mut()
                .ensure_indexed_for_line(self.scroll_top_line + visible_lines + extra_step + 16);
        }
        let total_lines = doc.editable_line_count().max(1);
        if self.scroll_top_line >= total_lines {
            self.scroll_top_line = total_lines - 1;
            self.scroll_sub_y_px = 0.0;
        }

        self.scroll_sub_y_px += dy_px;
        if dy_px > 0.0 {
            while self.scroll_top_line + 1 < total_lines {
                let content = doc.line_content(self.scroll_top_line).unwrap_or_default();
                let vis = self.count_wrapped_visual_lines(&content, metrics.cell_width, max_wrap_w);
                let block_h = vis as f32 * line_h;
                if self.scroll_sub_y_px >= block_h {
                    self.scroll_sub_y_px -= block_h;
                    self.scroll_top_line += 1;
                } else {
                    break;
                }
            }
        } else {
            while self.scroll_sub_y_px < 0.0 && self.scroll_top_line > 0 {
                self.scroll_top_line -= 1;
                let content = doc.line_content(self.scroll_top_line).unwrap_or_default();
                let vis = self.count_wrapped_visual_lines(&content, metrics.cell_width, max_wrap_w);
                self.scroll_sub_y_px += vis as f32 * line_h;
            }
            if self.scroll_sub_y_px < 0.0 {
                self.scroll_sub_y_px = 0.0;
            }
        }
        self.sync_wrapped_metrics(doc, self.layout.text.w, metrics, scale);
        self.clamp_vertical_scroll(doc, metrics, scale);
    }

    /// Compute the exact maximum top position `(max_line, max_sub_y_px)` so the last line
    /// lands flush with the bottom of the text viewport (zero excess whitespace at the bottom).
    fn compute_max_top(
        &mut self,
        doc: &mut Document,
        metrics: FontMetrics,
        scale: f32,
    ) -> (usize, f32) {
        let line_h = metrics.line_height.max(1) as f32;
        let text_h = self.layout.text.h.max(0) as f32;
        if text_h <= 0.0 {
            return (0, 0.0);
        }
        let total_lines = doc.editable_line_count().max(1);
        if !self.word_wrap {
            let total_h = total_lines as f32 * line_h;
            let max_y = (total_h - text_h).max(0.0);
            let max_line = (max_y / line_h).floor() as usize;
            let max_sub = (max_y - max_line as f32 * line_h).max(0.0);
            return (max_line.min(total_lines - 1), max_sub);
        }

        // Wrapped mode: walk backward at most one viewport of visual blocks from the last line.
        let text_pad_x = px(12.0, scale) as f32;
        let max_wrap_w =
            (self.layout.text.w as f32 - text_pad_x * 2.0).max(metrics.cell_width * 4.0);
        let mut acc_h = 0.0f32;
        let mut line = total_lines;
        while line > 0 {
            line -= 1;
            let content = doc.line_content(line).unwrap_or_default();
            let vis = self.count_wrapped_visual_lines(&content, metrics.cell_width, max_wrap_w);
            let block_h = vis as f32 * line_h;
            if acc_h + block_h >= text_h {
                let overflow = (acc_h + block_h - text_h).max(0.0);
                return (line, overflow);
            }
            acc_h += block_h;
        }
        (0, 0.0)
    }

    pub fn clamp_vertical_scroll(
        &mut self,
        doc: &mut Document,
        metrics: FontMetrics,
        scale: f32,
    ) {
        if !doc.is_fully_indexed() {
            // Only clamp to bottom once we've reached the end of the file.
            return;
        }
        let (max_line, max_sub) = self.compute_max_top(doc, metrics, scale);
        if self.scroll_top_line > max_line
            || (self.scroll_top_line == max_line && self.scroll_sub_y_px > max_sub)
        {
            self.scroll_top_line = max_line;
            self.scroll_sub_y_px = max_sub;
        }
    }

    fn set_vertical_scroll_px(
        &mut self,
        target_px: f32,
        doc: &mut Document,
        metrics: FontMetrics,
        scale: f32,
    ) {
        let line_h = metrics.line_height.max(1) as f32;
        let text_h = self.layout.text.h.max(0) as f32;

        if self.word_wrap {
            let font_px = self.font_px(scale);
            self.sync_wrap_cache(doc.generation(), self.layout.text.w, font_px);
            self.sync_wrapped_metrics(doc, self.layout.text.w, metrics, scale);
            if !self.wrapped_prefix.is_empty() {
                let total_vis = *self.wrapped_prefix.last().unwrap_or(&1).max(&1);
                let max_y = (total_vis as f32 * line_h - text_h).max(0.0);
                let clamped = target_px.clamp(0.0, max_y);
                let target_vis_line = (clamped / line_h).floor().max(0.0) as usize;
                // Find the logical line containing `target_vis_line` via O(log N) binary search.
                let idx = self
                    .wrapped_prefix
                    .partition_point(|&v| v <= target_vis_line)
                    .saturating_sub(1);
                let logical_max = doc.editable_line_count().saturating_sub(1);
                self.scroll_top_line = idx.min(logical_max);
                let base_px = self.wrapped_prefix[self.scroll_top_line] as f32 * line_h;
                self.scroll_sub_y_px = (clamped - base_px).max(0.0);
                self.clamp_vertical_scroll(doc, metrics, scale);
                return;
            }
            let est_logical = doc.estimated_editable_line_count().max(1);
            let total_vis = self.wrapped_total_visual_lines.max(est_logical);
            let max_y = (total_vis as f32 * line_h - text_h).max(0.0);
            let clamped = target_px.clamp(0.0, max_y);
            let frac = if max_y > 0.0 { clamped / max_y } else { 0.0 };
            let target_line = ((frac * est_logical.saturating_sub(1) as f32).floor().max(0.0)) as usize;
            let visible_lines = (text_h / line_h).ceil().max(1.0) as usize;
            doc.tree_mut()
                .ensure_indexed_for_line(target_line + visible_lines + 16);
            self.scroll_top_line = target_line;
            self.scroll_sub_y_px = 0.0;
            self.clamp_vertical_scroll(doc, metrics, scale);
            return;
        }

        let est_total_h = doc.estimated_editable_line_count().max(1) as f32 * line_h;
        let est_max_y = (est_total_h - text_h).max(0.0);
        let clamped_est = target_px.clamp(0.0, est_max_y);

        let target_line = (clamped_est / line_h).floor().max(0.0) as usize;
        let visible_lines = (text_h / line_h).ceil().max(1.0) as usize;
        doc.tree_mut()
            .ensure_indexed_for_line(target_line + visible_lines + 16);

        self.scroll_top_line = target_line;
        self.scroll_sub_y_px = (clamped_est - target_line as f32 * line_h).max(0.0);
        self.clamp_vertical_scroll(doc, metrics, scale);
    }

    /// Ensure the caret at `(line, col_bytes)` is comfortably visible inside the viewport.
    pub fn ensure_caret_visible(
        &mut self,
        doc: &mut Document,
        caret_byte: u64,
        metrics: FontMetrics,
        scale: f32,
    ) {
        let (line, col_bytes) = doc.byte_to_line_col(caret_byte);
        let line_h = metrics.line_height.max(1) as f32;
        let text_h = self.layout.text.h as f32;
        if text_h <= 0.0 {
            return;
        }

        if self.word_wrap {
            let font_px = self.font_px(scale);
            self.sync_wrap_cache(doc.generation(), self.layout.text.w, font_px);
            self.sync_wrapped_metrics(doc, self.layout.text.w, metrics, scale);
            let text_pad_x = px(12.0, scale) as f32;
            let max_wrap_w =
                (self.layout.text.w as f32 - text_pad_x * 2.0).max(metrics.cell_width * 4.0);
            let line_content = doc.line_content(line).unwrap_or_default();
            let segs = self.layout_line(line, &line_content, metrics.cell_width, max_wrap_w);
            let caret_seg_idx = segs
                .iter()
                .enumerate()
                .find(|&(idx, seg)| {
                    col_bytes >= seg.byte_start
                        && (col_bytes < seg.byte_end || idx + 1 == segs.len())
                })
                .map(|(idx, _)| idx)
                .unwrap_or(0);
            let caret_in_line_top = caret_seg_idx as f32 * line_h;
            let caret_in_line_bottom = caret_in_line_top + line_h;

            // Above current scroll top -> snap top cleanly to the caret's visual row.
            if line < self.scroll_top_line
                || (line == self.scroll_top_line && caret_in_line_top < self.scroll_sub_y_px)
            {
                self.scroll_top_line = line;
                self.scroll_sub_y_px = caret_in_line_top;
                self.clamp_vertical_scroll(doc, metrics, scale);
                return;
            }

            // Compute caret's visual bottom relative to the top of the viewport.
            let needs_scroll_down = if !self.wrapped_prefix.is_empty()
                && line < self.wrapped_prefix.len()
                && self.scroll_top_line < self.wrapped_prefix.len()
            {
                let vis_before_caret = self.wrapped_prefix[line];
                let vis_before_top = self.wrapped_prefix[self.scroll_top_line];
                let rel_bottom = (vis_before_caret.saturating_sub(vis_before_top) as f32) * line_h
                    - self.scroll_sub_y_px
                    + caret_in_line_bottom;
                rel_bottom > text_h
            } else if line - self.scroll_top_line > 512 {
                true
            } else {
                let mut acc_y = -self.scroll_sub_y_px;
                let mut overflow = false;
                for l in self.scroll_top_line..line {
                    let c = doc.line_content(l).unwrap_or_default();
                    let vis = self.count_wrapped_visual_lines(&c, metrics.cell_width, max_wrap_w);
                    acc_y += vis as f32 * line_h;
                    if acc_y >= text_h {
                        overflow = true;
                        break;
                    }
                }
                overflow || (acc_y + caret_in_line_bottom > text_h)
            };

            if needs_scroll_down {
                // Walk backward at most one viewport height from the caret's visual bottom so the
                // caret's visual row lands cleanly at the bottom edge of the viewport (O(visible_lines)).
                let mut rem_h = text_h - caret_in_line_bottom;
                if rem_h <= 0.0 {
                    self.scroll_top_line = line;
                    self.scroll_sub_y_px = (caret_in_line_bottom - text_h).max(0.0);
                } else {
                    let mut top_line = line;
                    let mut top_sub = 0.0f32;
                    while top_line > 0 {
                        top_line -= 1;
                        let c = doc.line_content(top_line).unwrap_or_default();
                        let vis =
                            self.count_wrapped_visual_lines(&c, metrics.cell_width, max_wrap_w);
                        let bh = vis as f32 * line_h;
                        if bh >= rem_h {
                            top_sub = (bh - rem_h).max(0.0);
                            break;
                        }
                        rem_h -= bh;
                    }
                    self.scroll_top_line = top_line;
                    self.scroll_sub_y_px = top_sub;
                }
                self.clamp_vertical_scroll(doc, metrics, scale);
            }
            return;
        }

        let visible_lines = (text_h / line_h).floor().max(1.0) as usize;
        if line < self.scroll_top_line
            || (line == self.scroll_top_line && self.scroll_sub_y_px > 0.0)
        {
            self.scroll_top_line = line;
            self.scroll_sub_y_px = 0.0;
        } else if line >= self.scroll_top_line + visible_lines {
            self.scroll_top_line = line + 1 - visible_lines;
            self.scroll_sub_y_px = 0.0;
        }

        if let Some(text) = doc.line_content(line) {
            let cx = self.measure_prefix_px(&text, col_bytes, metrics.cell_width);
            let text_w = self.layout.text.w as f32;
            let pad = 24.0 * scale;
            if cx < self.scroll_x_px + pad {
                self.scroll_x_px = (cx - pad).max(0.0);
            } else if cx > self.scroll_x_px + text_w - pad {
                self.scroll_x_px = (cx - text_w + pad).max(0.0);
            }
        }
    }

    /// Measure horizontal pixel X of `byte_col` within `line` (O(1) fast path for tab-free ASCII,
    /// handles tabs and wide emojis accurately).
    pub fn measure_prefix_px(&self, line: &str, byte_col: usize, cell_w: f32) -> f32 {
        let target = byte_col.min(line.len());
        let prefix_bytes = &line.as_bytes()[..target];
        if is_all_ascii(prefix_bytes) && find_byte(prefix_bytes, b'\t').is_none() {
            return target as f32 * cell_w;
        }
        let mut col_cells = 0usize;
        for (off, cluster) in grapheme_clusters(line) {
            if off >= target {
                break;
            }
            if cluster == "\t" {
                let step = self.tab_width - (col_cells % self.tab_width);
                col_cells += step;
            } else {
                col_cells += cluster_cell_width(cluster) as usize;
            }
        }
        col_cells as f32 * cell_w
    }

    /// Hit-test a Y coordinate inside the gutter to select a whole logical line.
    pub fn hit_test_gutter(
        &mut self,
        y_px: f32,
        doc: &mut Document,
        metrics: FontMetrics,
        scale: f32,
    ) -> usize {
        let hit = self.hit_test_text(
            self.layout.text.x as f32 + px(12.0, scale) as f32,
            y_px,
            doc,
            metrics,
            scale,
        );
        hit.line
    }

    /// Wrap a logical line into visual segments (fast single-segment path when wrap is off or short).
    fn layout_line(&mut self, line_idx: usize, text: &str, cell_w: f32, max_w: f32) -> Vec<WrappedSegment> {
        if !self.word_wrap || max_w <= cell_w * 2.0 {
            let w = self.measure_prefix_px(text, text.len(), cell_w);
            return vec![WrappedSegment {
                byte_start: 0,
                byte_end: text.len(),
                width_px: w,
            }];
        }
        if let Some(cached) = self.wrap_cache.get(&line_idx) {
            return cached.clone();
        }
        let mut segs = Vec::new();
        let mut seg_start = 0usize;
        let mut col_cells = 0usize;
        let mut last_break_byte: Option<usize> = None;
        let mut last_break_width = 0.0f32;
        let max_cells = (max_w / cell_w).floor().max(4.0) as usize;

        for (off, cluster) in grapheme_clusters(text) {
            let mut cw = if cluster == "\t" {
                self.tab_width - (col_cells % self.tab_width)
            } else {
                cluster_cell_width(cluster) as usize
            };
            if col_cells > 0 && col_cells + cw > max_cells {
                if let Some(brk) = last_break_byte {
                    if brk > seg_start {
                        segs.push(WrappedSegment {
                            byte_start: seg_start,
                            byte_end: brk,
                            width_px: last_break_width,
                        });
                        seg_start = brk;
                        // Recompute cells from `seg_start` to `off`.
                        col_cells = self
                            .measure_prefix_px(&text[seg_start..off], off - seg_start, 1.0)
                            .round() as usize;
                        last_break_byte = None;
                    } else {
                        segs.push(WrappedSegment {
                            byte_start: seg_start,
                            byte_end: off,
                            width_px: col_cells as f32 * cell_w,
                        });
                        seg_start = off;
                        col_cells = 0;
                        last_break_byte = None;
                    }
                } else {
                    segs.push(WrappedSegment {
                        byte_start: seg_start,
                        byte_end: off,
                        width_px: col_cells as f32 * cell_w,
                    });
                    seg_start = off;
                    col_cells = 0;
                }
                if cluster == "\t" {
                    cw = self.tab_width - (col_cells % self.tab_width);
                }
                if col_cells > 0 && col_cells + cw > max_cells {
                    segs.push(WrappedSegment {
                        byte_start: seg_start,
                        byte_end: off,
                        width_px: col_cells as f32 * cell_w,
                    });
                    seg_start = off;
                    col_cells = 0;
                    if cluster == "\t" {
                        cw = self.tab_width;
                    }
                }
            }
            col_cells += cw;
            if cluster == " " || cluster == "\t" {
                last_break_byte = Some(off + cluster.len());
                last_break_width = col_cells as f32 * cell_w;
            }
        }
        segs.push(WrappedSegment {
            byte_start: seg_start,
            byte_end: text.len(),
            width_px: col_cells as f32 * cell_w,
        });

        if segs.len() <= MAX_CACHED_VISUAL_LINES {
            self.wrap_cache.insert(line_idx, segs.clone());
        }
        segs
    }

    /// Hit-test a physical pixel coordinate `(x_px, y_px)` inside the document viewport.
    pub fn hit_test_text(
        &mut self,
        x_px: f32,
        y_px: f32,
        doc: &mut Document,
        metrics: FontMetrics,
        scale: f32,
    ) -> DocHit {
        let line_h = metrics.line_height.max(1) as f32;
        let font_px = self.font_px(scale);
        self.sync_wrap_cache(doc.generation(), self.layout.text.w, font_px);
        let text_pad_x = px(12.0, scale) as f32;
        let text_origin_x = self.layout.text.x as f32 + text_pad_x - self.scroll_x_px;
        let max_wrap_w = (self.layout.text.w as f32 - text_pad_x * 2.0).max(metrics.cell_width * 4.0);

        let target_y = y_px - self.layout.text.y as f32 + self.scroll_sub_y_px;
        let mut cur_y = 0.0f32;
        let total_lines = doc.editable_line_count().max(1);
        let mut line = self.scroll_top_line.min(total_lines - 1);

        if target_y < 0.0 {
            let lines_up = ((-target_y) / line_h).ceil() as usize;
            line = line.saturating_sub(lines_up);
            let (start, _, _) = doc.line_byte_range(line).unwrap_or((0, 0, 0));
            return DocHit {
                byte_offset: start,
                line,
                col_bytes: 0,
            };
        }

        while line < total_lines {
            let content = doc.line_content(line).unwrap_or_default();
            let segs = self.layout_line(line, &content, metrics.cell_width, max_wrap_w);
            for (seg_idx, seg) in segs.iter().enumerate() {
                if target_y < cur_y + line_h
                    || (line + 1 == total_lines && seg_idx + 1 == segs.len())
                {
                    let seg_str = &content[seg.byte_start..seg.byte_end];
                    let rel_x = (x_px - text_origin_x).max(0.0);
                    let col_in_seg = self.x_to_col_bytes(seg_str, rel_x, metrics.cell_width);
                    let col_bytes = seg.byte_start + col_in_seg;
                    let (start, _, _) = doc.line_byte_range(line).unwrap_or((0, 0, 0));
                    return DocHit {
                        byte_offset: start + col_bytes as u64,
                        line,
                        col_bytes,
                    };
                }
                cur_y += line_h;
            }
            line += 1;
        }

        let last_line = total_lines.saturating_sub(1);
        let (start, content_end, _) = doc.line_byte_range(last_line).unwrap_or((0, 0, 0));
        DocHit {
            byte_offset: content_end,
            line: last_line,
            col_bytes: (content_end - start) as usize,
        }
    }

    fn x_to_col_bytes(&self, segment: &str, target_x: f32, cell_w: f32) -> usize {
        let mut col_cells = 0usize;
        for (off, cluster) in grapheme_clusters(segment) {
            let cw = if cluster == "\t" {
                self.tab_width - (col_cells % self.tab_width)
            } else {
                cluster_cell_width(cluster) as usize
            };
            let start_x = col_cells as f32 * cell_w;
            let mid_x = start_x + (cw as f32 * cell_w) * 0.5;
            if target_x < mid_x {
                return off;
            }
            col_cells += cw;
        }
        segment.len()
    }

    /// Paint the entire document viewport (gutter, lines, selection, search highlights,
    /// matching brackets, quiet indent guides, caret, IME preedit underline, and overlay scrollbars).
    /// Respects `painter.current_clip()` so caret-blink dirty rects skip untouched lines!
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        painter: &mut Painter,
        rect: PixelRect,
        doc: &mut Document,
        caret: u64,
        selection: Option<(u64, u64)>,
        search_matches: &[SearchMatch],
        active_match_idx: Option<usize>,
        matching_brackets: Option<(u64, u64)>,
        ime: &ImeComposition,
        caret_visible: bool,
        focused: bool,
        theme: &Theme,
        atlas: &mut GlyphAtlas,
        rasterizer: &mut dyn GlyphRasterizer,
    ) {
        if rect.is_empty() {
            return;
        }
        let scale = painter.scale();
        let font_px = self.font_px(scale);
        let metrics = atlas.metrics(rasterizer, FontStyle::Mono, font_px);
        let line_h = metrics.line_height.max(1) as f32;
        // Guarantee lazy PieceTree indexing covers at least the visible viewport + lookahead,
        // even for >16 KB files that start with long single lines.
        let est_vis_lines = ((rect.h.max(1) as f32) / line_h).ceil().max(1.0) as usize;
        doc.tree_mut()
            .ensure_indexed_for_line(self.scroll_top_line + est_vis_lines + 8);
        let pre_layout = self.compute_layout(rect, doc, metrics, scale);
        self.sync_wrap_cache(doc.generation(), pre_layout.text.w, font_px);
        self.sync_wrapped_metrics(doc, pre_layout.text.w, metrics, scale);
        self.clamp_vertical_scroll(doc, metrics, scale);
        let layout = self.compute_layout(rect, doc, metrics, scale);

        let clip = painter.current_clip();
        // Reset per-frame caret bounding rect only on full text-area redraws or when unfocused,
        // so partial dirty-rect repaints (toast expiry, scrollbar hover, single-caret blink) never
        // discard off-clip multi-cursor rects or clear the IME caret anchor.
        if !focused || layout.text.intersect(clip) == layout.text {
            self.last_caret_rect = PixelRect::ZERO;
        }
        let hairline = (1.0 * scale).round().max(1.0) as i32;
        let text_pad_x = px(12.0, scale) as f32;
        let cell_w = metrics.cell_width;
        let max_wrap_w = (layout.text.w as f32 - text_pad_x * 2.0).max(cell_w * 4.0);

        // 1. Backgrounds: document surface + gutter page ground + bottom h-scrollbar strip.
        painter.fill_rect(layout.full, theme.surface);
        if !layout.gutter.intersect(clip).is_empty() {
            painter.fill_rect(layout.gutter, theme.page);
            painter.fill_rect(
                PixelRect::new(
                    layout.gutter.right() - hairline,
                    layout.gutter.y,
                    hairline,
                    layout.gutter.h,
                ),
                theme.border_subtle,
            );
        }

        let (caret_line, caret_col) = doc.byte_to_line_col(caret);
        let total_lines = doc.editable_line_count().max(1);
        if self.scroll_top_line >= total_lines {
            self.scroll_top_line = total_lines.saturating_sub(1);
            self.scroll_sub_y_px = 0.0;
        }

        let mut tokens: Vec<TokenSpan> = Vec::with_capacity(64);
        let mut syntax_prepared = false;
        let mut cur_y = layout.text.y as f32 - self.scroll_sub_y_px;
        let view_bottom = (layout.text.bottom().min(clip.bottom())) as f32;
        let clip_top = clip.y as f32;
        let mut line_idx = self.scroll_top_line;
        let text_origin_x = layout.text.x as f32 + text_pad_x - self.scroll_x_px;
        let indent_guide_color = theme.border_subtle.with_alpha(150);

        // 2. Render visible logical lines.
        while line_idx < total_lines && cur_y < view_bottom {
            let Some((line_start, line_content_end, line_full_end)) = doc.line_byte_range(line_idx)
            else {
                break;
            };
            let full_line_bytes = line_content_end - line_start;

            // Fast skip for lines completely above the dirty clip rect when wrap is off!
            if !self.word_wrap && cur_y + line_h <= clip_top {
                cur_y += line_h;
                line_idx += 1;
                continue;
            }

            // Pathological huge-single-line protection: read ONLY the visible horizontal window!
            let (win_rel_start, line_text, is_windowed) =
                if !self.word_wrap && full_line_bytes > HUGE_LINE_WINDOW_THRESHOLD {
                    let approx_col = (self.scroll_x_px / cell_w).floor().max(0.0) as u64;
                    let req = approx_col.saturating_sub(32);
                    let (actual_rel, _, slice) = doc
                        .read_line_window(line_idx, req, HUGE_LINE_WINDOW_BYTES)
                        .unwrap_or((0, full_line_bytes, String::new()));
                    let est_w = full_line_bytes as f32 * cell_w + text_pad_x * 2.0;
                    if est_w > self.max_line_width_px {
                        self.max_line_width_px = est_w;
                    }
                    (actual_rel as usize, slice, true)
                } else {
                    let full = doc.line_content(line_idx).unwrap_or_default();
                    (0usize, full, false)
                };

            if !is_windowed {
                if !syntax_prepared {
                    doc.prepare_syntax_to(line_idx);
                    syntax_prepared = true;
                }
                doc.lex_line(line_idx, &line_text, &mut tokens);
            } else {
                tokens.clear();
            }

            let segs = if is_windowed {
                vec![WrappedSegment {
                    byte_start: 0,
                    byte_end: line_text.len(),
                    width_px: full_line_bytes as f32 * cell_w,
                }]
            } else {
                self.layout_line(line_idx, &line_text, cell_w, max_wrap_w)
            };

            if !self.word_wrap && !is_windowed {
                if let Some(first_seg) = segs.first() {
                    if first_seg.width_px + text_pad_x * 2.0 > self.max_line_width_px {
                        self.max_line_width_px = first_seg.width_px + text_pad_x * 2.0;
                    }
                }
            }

            let is_current_line = line_idx == caret_line;
            let block_h = segs.len() as f32 * line_h;
            if cur_y + block_h <= clip_top {
                cur_y += block_h;
                line_idx += 1;
                continue;
            }

            // Current-line highlight.
            if is_current_line {
                painter.push_clip(layout.text);
                painter.fill_rect(
                    PixelRect::new(
                        layout.text.x,
                        cur_y.round() as i32,
                        layout.text.w,
                        block_h.round() as i32,
                    ),
                    theme.current_line,
                );
                painter.pop_clip();
            }

            // Gutter line number (skip if gutter isn't inside dirty clip).
            let gutter_text_clip = PixelRect::new(
                layout.gutter.x,
                layout.gutter.y,
                (layout.gutter.w - hairline).max(0),
                layout.text.h,
            );
            if !gutter_text_clip.intersect(clip).is_empty() {
                painter.push_clip(gutter_text_clip);
                let num_str = (line_idx + 1).to_string();
                let num_w = num_str.len() as f32 * cell_w;
                let num_x = (layout.gutter.right() - hairline - px(12.0, scale)) as f32 - num_w;
                let num_baseline = cur_y + metrics.ascent + metrics.leading * 0.5;
                let num_color = if is_current_line {
                    theme.text
                } else {
                    theme.text_muted
                };
                let mut gx = num_x;
                for b in num_str.bytes() {
                    let g = atlas.get_ascii_mono(rasterizer, false, font_px, b);
                    painter.draw_glyph(gx, num_baseline, g, num_color, atlas);
                    gx += cell_w;
                }
                if !self.git_diff.is_empty() {
                    if let Some(diff_kind) = lookup_line_diff(&self.git_diff, line_idx) {
                        let bar_w = px(2.0, scale).max(2);
                        let bar_x = layout.gutter.right() - hairline - px(3.0, scale) - bar_w;
                        let ly = cur_y.round() as i32;
                        let bh = block_h.round().max(1.0) as i32;
                        match diff_kind {
                            LineDiffKind::Added => {
                                painter.fill_rect(
                                    PixelRect::new(bar_x, ly + 1, bar_w, (bh - 2).max(2)),
                                    theme.state_success,
                                );
                            }
                            LineDiffKind::Modified => {
                                painter.fill_rect(
                                    PixelRect::new(bar_x, ly + 1, bar_w, (bh - 2).max(2)),
                                    theme.state_info,
                                );
                            }
                            LineDiffKind::DeletedAbove => {
                                let tw = px(5.0, scale).max(4);
                                let th = px(2.0, scale).max(2);
                                painter.fill_rect(
                                    PixelRect::new(
                                        layout.gutter.right() - hairline - px(2.0, scale) - tw,
                                        ly,
                                        tw,
                                        th,
                                    ),
                                    theme.state_danger,
                                );
                            }
                            LineDiffKind::DeletedBelow => {
                                let tw = px(5.0, scale).max(4);
                                let th = px(2.0, scale).max(2);
                                painter.fill_rect(
                                    PixelRect::new(
                                        layout.gutter.right() - hairline - px(2.0, scale) - tw,
                                        (ly + bh - th).max(ly),
                                        tw,
                                        th,
                                    ),
                                    theme.state_danger,
                                );
                            }
                        }
                    }
                }
                painter.pop_clip();
            }

            // Render each visual segment of this logical line inside `layout.text`.
            painter.push_clip(layout.text);
            let win_x_shift = win_rel_start as f32 * cell_w;

            // Quiet vertical indent guides for leading spaces on non-windowed lines.
            if !is_windowed && !line_text.is_empty() {
                let mut lead_cols = 0usize;
                for &b in line_text.as_bytes() {
                    if b == b' ' {
                        lead_cols += 1;
                    } else if b == b'\t' {
                        lead_cols += self.tab_width - (lead_cols % self.tab_width);
                    } else {
                        break;
                    }
                }
                let levels = lead_cols / self.tab_width;
                for lv in 1..=levels {
                    let gx = (text_origin_x + (lv * self.tab_width) as f32 * cell_w).round() as i32;
                    painter.fill_rect(
                        PixelRect::new(gx, cur_y.round() as i32, hairline, line_h.round() as i32),
                        indent_guide_color,
                    );
                }
            }

            for (seg_idx, seg) in segs.iter().enumerate() {
                let seg_y = cur_y + seg_idx as f32 * line_h;
                if seg_y + line_h < layout.text.y as f32 || seg_y >= view_bottom {
                    continue;
                }
                let seg_str = &line_text[seg.byte_start..seg.byte_end];
                let seg_abs_start = line_start + (win_rel_start + seg.byte_start) as u64;
                let seg_abs_end = line_start + (win_rel_start + seg.byte_end) as u64;
                let is_line_end = seg_abs_end == line_content_end;
                let seg_origin_x = text_origin_x + win_x_shift;

                // Selection highlights on this visual segment (primary + any extra cursors).
                let primary_sel_iter = selection.into_iter();
                let extra_sel_iter = self.extra_cursors.iter().filter_map(|&(ea, ec)| {
                    if ea == ec {
                        None
                    } else if ea < ec {
                        Some((ea, ec))
                    } else {
                        Some((ec, ea))
                    }
                });
                for (sel_a, sel_b) in primary_sel_iter.chain(extra_sel_iter) {
                    if sel_b > seg_abs_start && sel_a < line_full_end {
                        let ov_start = sel_a.max(seg_abs_start);
                        let ov_end = sel_b.min(seg_abs_end);
                        let x0 = seg_origin_x
                            + self.measure_prefix_px(
                                seg_str,
                                (ov_start - seg_abs_start) as usize,
                                cell_w,
                            );
                        let mut x1 = seg_origin_x
                            + self.measure_prefix_px(
                                seg_str,
                                (ov_end.saturating_sub(seg_abs_start)) as usize,
                                cell_w,
                            );
                        if is_line_end && sel_b > line_content_end {
                            x1 += cell_w * 0.65;
                        }
                        if x1 > x0 {
                            painter.fill_rect(
                                PixelRect::new(
                                    x0.round() as i32,
                                    seg_y.round() as i32,
                                    (x1 - x0).round().max(1.0) as i32,
                                    line_h.round() as i32,
                                ),
                                theme.selection,
                            );
                        }
                    }
                }

                // Matching bracket quiet hairline boxes.
                if let Some((b0, b1)) = matching_brackets {
                    for brk in [b0, b1] {
                        if brk >= seg_abs_start && brk < seg_abs_end {
                            let bx = seg_origin_x
                                + self.measure_prefix_px(
                                    seg_str,
                                    (brk - seg_abs_start) as usize,
                                    cell_w,
                                );
                            let box_rect = PixelRect::new(
                                bx.round() as i32,
                                seg_y.round() as i32 + 1,
                                cell_w.round().max(4.0) as i32,
                                (line_h.round() as i32 - 2).max(2),
                            );
                            painter.stroke_rounded_rect(
                                box_rect,
                                2.0 * scale,
                                (1.0 * scale).max(1.0),
                                theme.border_control.with_alpha(170),
                            );
                        }
                    }
                }

                // Search match highlights overlapping this segment.
                if !search_matches.is_empty() {
                    let first_idx = search_matches.partition_point(|m| m.end <= seg_abs_start);
                    for (mi, m) in search_matches.iter().enumerate().skip(first_idx) {
                        if m.start >= seg_abs_end {
                            break;
                        }
                        let ov_s = m.start.max(seg_abs_start);
                        let ov_e = m.end.min(seg_abs_end);
                        let mx0 = seg_origin_x
                            + self.measure_prefix_px(seg_str, (ov_s - seg_abs_start) as usize, cell_w);
                        let mx1 = seg_origin_x
                            + self.measure_prefix_px(seg_str, (ov_e - seg_abs_start) as usize, cell_w);
                        let is_active = active_match_idx == Some(mi);
                        let m_color = if is_active {
                            theme.search_match_active
                        } else {
                            theme.search_match
                        };
                        painter.fill_rounded_rect(
                            PixelRect::new(
                                mx0.round() as i32,
                                seg_y.round() as i32 + 1,
                                (mx1 - mx0).round().max(2.0) as i32,
                                (line_h.round() as i32 - 2).max(1),
                            ),
                            2.0 * scale,
                            m_color,
                        );
                    }
                }

                // Draw syntax-highlighted text & color emojis!
                let baseline = seg_y + metrics.ascent + metrics.leading * 0.5;
                let mut col_cells = 0usize;
                let mut tok_cursor = 0usize;
                let clip_left = (layout.text.x.max(clip.x)) as f32 - cell_w * 3.0;
                let clip_right = (layout.text.right().min(clip.right())) as f32 + cell_w;

                for (rel_off, cluster) in grapheme_clusters(seg_str) {
                    let abs_col_in_line = seg.byte_start + rel_off;
                    let pen_x = seg_origin_x + col_cells as f32 * cell_w;
                    if cluster == "\t" {
                        col_cells += self.tab_width - (col_cells % self.tab_width);
                        continue;
                    }
                    let cw = cluster_cell_width(cluster) as usize;
                    if pen_x > clip_right {
                        break;
                    }
                    if pen_x >= clip_left {
                        while tok_cursor < tokens.len()
                            && (tokens[tok_cursor].end as usize) <= abs_col_in_line
                        {
                            tok_cursor += 1;
                        }
                        let tok_kind = tokens
                            .get(tok_cursor)
                            .filter(|t| {
                                (t.start as usize) <= abs_col_in_line
                                    && abs_col_in_line < (t.end as usize)
                            })
                            .map(|t| t.kind)
                            .unwrap_or(TokenKind::Plain);
                        let (fg, font_style) = theme.syntax_font_style(tok_kind);

                        let bytes = cluster.as_bytes();
                        if bytes.len() == 1 && bytes[0] >= 0x20 && bytes[0] <= 0x7E {
                            let glyph =
                                atlas.get_ascii_mono_styled(rasterizer, font_style, font_px, bytes[0]);
                            painter.draw_glyph(pen_x, baseline, glyph, fg, atlas);
                        } else {
                            let glyph = atlas.get_or_rasterize(rasterizer, font_style, font_px, cluster);
                            painter.draw_glyph(pen_x, baseline, glyph, fg, atlas);
                        }
                    }
                    col_cells += cw;
                }

                // Track & draw blinking caret if it falls on this visual segment.
                if is_current_line && focused {
                    let caret_abs = line_start + caret_col as u64;
                    let in_seg = caret_abs >= seg_abs_start
                        && (caret_abs < seg_abs_end
                            || (is_line_end && caret_abs <= line_content_end));
                    if in_seg {
                        let local_col = ((caret_abs - seg_abs_start) as usize).min(seg_str.len());
                        let cx = seg_origin_x + self.measure_prefix_px(seg_str, local_col, cell_w);
                        let caret_w = if self.block_caret {
                            cell_w.round().max(4.0) as i32
                        } else {
                            (2.0 * scale).round().max(1.0) as i32
                        };
                        let crect = PixelRect::new(
                            cx.round() as i32,
                            seg_y.round() as i32 + 1,
                            caret_w,
                            (line_h.round() as i32 - 2).max(1),
                        );
                        self.last_caret_rect = if self.last_caret_rect.is_empty() {
                            crect
                        } else {
                            self.last_caret_rect.union(crect)
                        };
                        if caret_visible {
                            if self.block_caret {
                                painter.fill_rect(crect, theme.text.with_alpha(95));
                                painter.fill_rect(
                                    PixelRect::new(crect.x, crect.y, crect.w, (1.0 * scale).round().max(1.0) as i32),
                                    theme.text,
                                );
                                painter.fill_rect(
                                    PixelRect::new(crect.x, crect.bottom() - (1.0 * scale).round().max(1.0) as i32, crect.w, (1.0 * scale).round().max(1.0) as i32),
                                    theme.text,
                                );
                                painter.fill_rect(
                                    PixelRect::new(crect.x, crect.y, (1.0 * scale).round().max(1.0) as i32, crect.h),
                                    theme.text,
                                );
                                painter.fill_rect(
                                    PixelRect::new(crect.right() - (1.0 * scale).round().max(1.0) as i32, crect.y, (1.0 * scale).round().max(1.0) as i32, crect.h),
                                    theme.text,
                                );
                            } else {
                                painter.fill_rect(crect, theme.text);
                            }
                        }
                        // Future IME composition preview underline if preedit text is active.
                        if !ime.preedit.is_empty() {
                            let pre_w = self.measure_prefix_px(&ime.preedit, ime.preedit.len(), cell_w);
                            painter.fill_rect(
                                PixelRect::new(
                                    cx.round() as i32,
                                    (seg_y + line_h - 2.0 * scale).round() as i32,
                                    pre_w.round().max(4.0) as i32,
                                    (1.5 * scale).round().max(1.0) as i32,
                                ),
                                theme.border_control,
                            );
                        }
                    }
                }
                // Draw any secondary multi-cursor carets falling inside this visual segment.
                if focused && !self.extra_cursors.is_empty() {
                    let caret_w = (2.0 * scale).round().max(1.0) as i32;
                    for &(_, ec) in &self.extra_cursors {
                        let in_seg = ec >= seg_abs_start
                            && (ec < seg_abs_end || (is_line_end && ec <= line_content_end));
                        if in_seg {
                            let local_col = ((ec - seg_abs_start) as usize).min(seg_str.len());
                            let cx =
                                seg_origin_x + self.measure_prefix_px(seg_str, local_col, cell_w);
                            let crect = PixelRect::new(
                                cx.round() as i32,
                                seg_y.round() as i32 + 1,
                                caret_w,
                                (line_h.round() as i32 - 2).max(1),
                            );
                            self.last_caret_rect = if self.last_caret_rect.is_empty() {
                                crect
                            } else {
                                self.last_caret_rect.union(crect)
                            };
                            if caret_visible {
                                painter.fill_rect(crect, theme.text);
                            }
                        }
                    }
                }
            }

            painter.pop_clip(); // layout.text
            cur_y += block_h;
            line_idx += 1;
        }

        // 3. Clamp horizontal scroll against measured line width and recompute scrollbar thumbs.
        if !self.word_wrap && layout.text.w > 0 {
            let max_x = if self.max_line_width_px > layout.text.w as f32 {
                (self.max_line_width_px + 32.0 * scale - layout.text.w as f32).max(0.0)
            } else {
                0.0
            };
            self.scroll_x_px = self.scroll_x_px.clamp(0.0, max_x);
        }
        let layout = self.compute_layout(rect, doc, metrics, scale);
        if !layout.v_thumb.is_empty() && !layout.v_bar.intersect(clip).is_empty() {
            let alpha = if self.v_bar_dragging || self.v_bar_hovered {
                190
            } else {
                105
            };
            painter.fill_rounded_rect(
                layout.v_thumb,
                layout.v_thumb.w as f32 * 0.5,
                theme.border_control.with_alpha(alpha),
            );
        }
        if !layout.h_thumb.is_empty() && !layout.h_bar.intersect(clip).is_empty() {
            let alpha = if self.h_bar_dragging || self.h_bar_hovered {
                190
            } else {
                105
            };
            painter.fill_rounded_rect(
                layout.h_thumb,
                layout.h_thumb.h as f32 * 0.5,
                theme.border_control.with_alpha(alpha),
            );
        }
    }
}
