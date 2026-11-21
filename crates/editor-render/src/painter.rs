//! High-speed 2D CPU software painter over `Framebuffer`.
//!
//! Optimized for text editors:
//! - Opaque fills use `slice::fill` (auto-vectorized memset/stp)
//! - Branchless alpha compositing for antialiased glyphs & subtle highlights
//! - Analytic AA rounded rectangles and crisp vector chevrons/icons
//! - In-place vertical scroll-blit for sub-millisecond scrolling

use editor_core::grapheme_clusters;
use editor_platform::{FontStyle, Framebuffer, GlyphFormat, GlyphRasterizer, PixelRect, SurfaceMut};

use crate::atlas::{AtlasGlyph, GlyphAtlas};
use crate::theme::Color;

pub struct Painter<'a> {
    surf: SurfaceMut<'a>,
    clip_stack: Vec<PixelRect>,
}

impl<'a> Painter<'a> {
    pub fn new(fb: &'a mut Framebuffer) -> Self {
        Self::from_surface(fb.as_surface_mut())
    }

    pub fn from_surface(surf: SurfaceMut<'a>) -> Self {
        let full = surf.bounds();
        Self {
            surf,
            clip_stack: vec![full],
        }
    }

    #[inline]
    pub fn bounds(&self) -> PixelRect {
        self.surf.bounds()
    }

    #[inline]
    pub fn scale(&self) -> f32 {
        self.surf.scale
    }

    #[inline]
    pub fn current_clip(&self) -> PixelRect {
        *self.clip_stack.last().unwrap()
    }

    #[inline]
    pub fn push_clip(&mut self, rect: PixelRect) {
        let next = self.current_clip().intersect(rect);
        self.clip_stack.push(next);
    }

    #[inline]
    pub fn pop_clip(&mut self) {
        if self.clip_stack.len() > 1 {
            self.clip_stack.pop();
        }
    }

    /// Fill `rect` with `color` (clipped to current scissor).
    #[inline]
    pub fn fill_rect(&mut self, rect: PixelRect, color: Color) {
        if color.a == 0 {
            return;
        }
        let clipped = rect.intersect(self.current_clip());
        if clipped.is_empty() {
            return;
        }
        let stride = self.surf.stride;
        let x0 = clipped.x as usize;
        let x1 = clipped.right() as usize;
        if color.a == 255 {
            let packed = color.to_premul_u32();
            for y in clipped.y..clipped.bottom() {
                let row = (y as usize) * stride;
                self.surf.pixels[row + x0..row + x1].fill(packed);
            }
        } else {
            let sr = color.r as u32;
            let sg = color.g as u32;
            let sb = color.b as u32;
            let sa = color.a as u32;
            let inv_a = 255 - sa;
            for y in clipped.y..clipped.bottom() {
                let row = (y as usize) * stride;
                for px in &mut self.surf.pixels[row + x0..row + x1] {
                    *px = blend_over_rgb(*px, sr, sg, sb, sa, inv_a);
                }
            }
        }
    }

    /// Draw a crisp 1px (or `thickness` px) hairline border inside `rect`.
    pub fn stroke_rect(&mut self, rect: PixelRect, thickness: i32, color: Color) {
        if rect.is_empty() || thickness <= 0 || color.a == 0 {
            return;
        }
        let t = thickness.min(rect.w).min(rect.h);
        self.fill_rect(PixelRect::new(rect.x, rect.y, rect.w, t), color);
        self.fill_rect(
            PixelRect::new(rect.x, rect.bottom() - t, rect.w, t),
            color,
        );
        self.fill_rect(
            PixelRect::new(rect.x, rect.y + t, t, rect.h - 2 * t),
            color,
        );
        self.fill_rect(
            PixelRect::new(rect.right() - t, rect.y + t, t, rect.h - 2 * t),
            color,
        );
    }

    /// Analytic anti-aliased rounded rectangle fill.
    pub fn fill_rounded_rect(&mut self, rect: PixelRect, radius: f32, color: Color) {
        if color.a == 0 || rect.is_empty() {
            return;
        }
        let r = radius
            .max(0.0)
            .min(rect.w as f32 * 0.5)
            .min(rect.h as f32 * 0.5);
        if r <= 0.5 {
            self.fill_rect(rect, color);
            return;
        }
        let clipped = rect.intersect(self.current_clip());
        if clipped.is_empty() {
            return;
        }

        let r_ceil = r.ceil() as i32;
        // Fast path for the interior middle band that has zero corner curvature!
        if rect.h > r_ceil * 2 {
            self.fill_rect(
                PixelRect::new(rect.x, rect.y + r_ceil, rect.w, rect.h - r_ceil * 2),
                color,
            );
        }

        let stride = self.surf.stride;
        let sr = color.r as u32;
        let sg = color.g as u32;
        let sb = color.b as u32;
        let base_a = color.a as f32;

        let cx_left = rect.x as f32 + r;
        let cx_right = rect.right() as f32 - r;
        let cy_top = rect.y as f32 + r;
        let cy_bot = rect.bottom() as f32 - r;

        let bands = [
            (rect.y, (rect.y + r_ceil).min(rect.bottom())),
            ((rect.bottom() - r_ceil).max(rect.y + r_ceil), rect.bottom()),
        ];

        for (by0, by1) in bands {
            let y0 = by0.max(clipped.y);
            let y1 = by1.min(clipped.bottom());
            for y in y0..y1 {
                let py = y as f32 + 0.5;
                let dy = if py < cy_top {
                    cy_top - py
                } else if py > cy_bot {
                    py - cy_bot
                } else {
                    0.0
                };
                let row_off = (y as usize) * stride;
                for x in clipped.x..clipped.right() {
                    let px_f = x as f32 + 0.5;
                    let dx = if px_f < cx_left {
                        cx_left - px_f
                    } else if px_f > cx_right {
                        px_f - cx_right
                    } else {
                        0.0
                    };
                    if dx == 0.0 && dy == 0.0 {
                        let dst = &mut self.surf.pixels[row_off + x as usize];
                        if color.a == 255 {
                            *dst = color.to_premul_u32();
                        } else {
                            let sa = color.a as u32;
                            *dst = blend_over_rgb(*dst, sr, sg, sb, sa, 255 - sa);
                        }
                        continue;
                    }
                    let dist = (dx * dx + dy * dy).sqrt() - r;
                    let cov = (0.5 - dist).clamp(0.0, 1.0);
                    if cov > 0.0 {
                        let sa = (cov * base_a).round() as u32;
                        if sa > 0 {
                            let dst = &mut self.surf.pixels[row_off + x as usize];
                            *dst = blend_over_rgb(*dst, sr, sg, sb, sa, 255 - sa);
                        }
                    }
                }
            }
        }
    }

    /// Analytic anti-aliased rounded rectangle border stroke.
    pub fn stroke_rounded_rect(
        &mut self,
        rect: PixelRect,
        radius: f32,
        thickness: f32,
        color: Color,
    ) {
        if color.a == 0 || rect.is_empty() || thickness <= 0.0 {
            return;
        }
        let clipped = rect.intersect(self.current_clip());
        if clipped.is_empty() {
            return;
        }
        let r = radius
            .max(0.0)
            .min(rect.w as f32 * 0.5)
            .min(rect.h as f32 * 0.5);
        let half_w = rect.w as f32 * 0.5;
        let half_h = rect.h as f32 * 0.5;
        let cx = rect.x as f32 + half_w;
        let cy = rect.y as f32 + half_h;
        let bx = half_w - r;
        let by = half_h - r;

        let stride = self.surf.stride;
        let sr = color.r as u32;
        let sg = color.g as u32;
        let sb = color.b as u32;
        let base_a = color.a as f32;

        for y in clipped.y..clipped.bottom() {
            let py = (y as f32 + 0.5 - cy).abs() - by;
            let row = (y as usize) * stride;
            for x in clipped.x..clipped.right() {
                let px_f = (x as f32 + 0.5 - cx).abs() - bx;
                // Skip interior fast.
                if px_f < -(thickness + 1.5) && py < -(thickness + 1.5) {
                    continue;
                }
                let qx = px_f.max(0.0);
                let qy = py.max(0.0);
                let sdf = (qx * qx + qy * qy).sqrt() + px_f.max(py).min(0.0) - r;
                // Outer edge at sdf <= 0, inner edge at sdf >= -thickness.
                let outer = (0.5 - sdf).clamp(0.0, 1.0);
                let inner = (0.5 + sdf + thickness).clamp(0.0, 1.0);
                let cov = outer.min(inner);
                if cov > 0.0 {
                    let sa = (cov * base_a).round() as u32;
                    if sa > 0 {
                        let dst = &mut self.surf.pixels[row + x as usize];
                        *dst = blend_over_rgb(*dst, sr, sg, sb, sa, 255 - sa);
                    }
                }
            }
        }
    }

    /// Draw a crisp anti-aliased line segment (used for vector chevrons and icons).
    pub fn draw_line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, thickness: f32, color: Color) {
        if color.a == 0 {
            return;
        }
        let pad = thickness.ceil() as i32 + 2;
        let min_x = (x0.min(x1).floor() as i32) - pad;
        let max_x = (x0.max(x1).ceil() as i32) + pad;
        let min_y = (y0.min(y1).floor() as i32) - pad;
        let max_y = (y0.max(y1).ceil() as i32) + pad;
        let clipped =
            PixelRect::new(min_x, min_y, max_x - min_x, max_y - min_y).intersect(self.current_clip());
        if clipped.is_empty() {
            return;
        }
        let vx = x1 - x0;
        let vy = y1 - y0;
        let len_sq = (vx * vx + vy * vy).max(1e-6);
        let half_t = thickness * 0.5;
        let stride = self.surf.stride;
        let sr = color.r as u32;
        let sg = color.g as u32;
        let sb = color.b as u32;
        let base_a = color.a as f32;

        for y in clipped.y..clipped.bottom() {
            let py = y as f32 + 0.5;
            let row = (y as usize) * stride;
            for x in clipped.x..clipped.right() {
                let px_f = x as f32 + 0.5;
                let t = (((px_f - x0) * vx + (py - y0) * vy) / len_sq).clamp(0.0, 1.0);
                let dx = px_f - (x0 + t * vx);
                let dy = py - (y0 + t * vy);
                let dist = (dx * dx + dy * dy).sqrt() - half_t;
                let cov = (0.5 - dist).clamp(0.0, 1.0);
                if cov > 0.0 {
                    let sa = (cov * base_a).round() as u32;
                    if sa > 0 {
                        let dst = &mut self.surf.pixels[row + x as usize];
                        *dst = blend_over_rgb(*dst, sr, sg, sb, sa, 255 - sa);
                    }
                }
            }
        }
    }

    /// Draw a quiet vector disclosure chevron (pointing right or down) centered at `(cx, cy)`.
    pub fn draw_chevron(&mut self, cx: f32, cy: f32, size: f32, down: bool, color: Color) {
        let half = size * 0.5;
        let stroke = (size * 0.22).max(1.2);
        if down {
            self.draw_line(cx - half, cy - half * 0.45, cx, cy + half * 0.55, stroke, color);
            self.draw_line(cx, cy + half * 0.55, cx + half, cy - half * 0.45, stroke, color);
        } else {
            self.draw_line(cx - half * 0.45, cy - half, cx + half * 0.55, cy, stroke, color);
            self.draw_line(cx + half * 0.55, cy, cx - half * 0.45, cy + half, stroke, color);
        }
    }

    /// Blit a single cached atlas glyph at `(pen_x, baseline_y)`.
    #[inline]
    pub fn draw_glyph(
        &mut self,
        pen_x: f32,
        baseline_y: f32,
        glyph: AtlasGlyph,
        fg: Color,
        atlas: &GlyphAtlas,
    ) {
        if glyph.width == 0 || glyph.height == 0 || fg.a == 0 {
            return;
        }
        let gx = pen_x.round() as i32 + glyph.bearing_x;
        let gy = baseline_y.round() as i32 - glyph.bearing_y;
        let dst_rect = PixelRect::new(gx, gy, glyph.width as i32, glyph.height as i32);
        let clipped = dst_rect.intersect(self.current_clip());
        if clipped.is_empty() {
            return;
        }

        let sx0 = (clipped.x - gx) as usize;
        let sy0 = (clipped.y - gy) as usize;
        let draw_w = clipped.w as usize;
        let draw_h = clipped.h as usize;
        let fb_stride = self.surf.stride;

        match glyph.format {
            GlyphFormat::Alpha8 => {
                let (alpha_buf, alpha_stride_u32) = atlas.alpha_pixels();
                let alpha_stride = alpha_stride_u32 as usize;
                let sr = fg.r as u32;
                let sg = fg.g as u32;
                let sb = fg.b as u32;
                let fg_a = fg.a as u32;

                for row in 0..draw_h {
                    let src_row =
                        ((glyph.atlas_y as usize) + sy0 + row) * alpha_stride + (glyph.atlas_x as usize) + sx0;
                    let dst_row = ((clipped.y as usize) + row) * fb_stride + (clipped.x as usize);
                    let src_slice = &alpha_buf[src_row..src_row + draw_w];
                    let dst_slice = &mut self.surf.pixels[dst_row..dst_row + draw_w];

                    if fg_a == 255 {
                        for (dst, &cov) in dst_slice.iter_mut().zip(src_slice.iter()) {
                            if cov == 0 {
                                continue;
                            }
                            if cov == 255 {
                                *dst = (0xFFu32 << 24) | (sr << 16) | (sg << 8) | sb;
                            } else {
                                let sa = cov as u32;
                                *dst = blend_over_rgb(*dst, sr, sg, sb, sa, 255 - sa);
                            }
                        }
                    } else {
                        for (dst, &cov) in dst_slice.iter_mut().zip(src_slice.iter()) {
                            if cov == 0 {
                                continue;
                            }
                            let sa = (cov as u32 * fg_a + 127) / 255;
                            if sa > 0 {
                                *dst = blend_over_rgb(*dst, sr, sg, sb, sa, 255 - sa);
                            }
                        }
                    }
                }
            }
            GlyphFormat::Bgra32 => {
                let (color_buf, color_stride_u32) = atlas.color_pixels();
                let color_stride = (color_stride_u32 as usize) * 4;
                for row in 0..draw_h {
                    let src_row = ((glyph.atlas_y as usize) + sy0 + row) * color_stride
                        + ((glyph.atlas_x as usize) + sx0) * 4;
                    let dst_row = ((clipped.y as usize) + row) * fb_stride + (clipped.x as usize);
                    for col in 0..draw_w {
                        let si = src_row + col * 4;
                        let sb = color_buf[si] as u32;
                        let sg = color_buf[si + 1] as u32;
                        let sr = color_buf[si + 2] as u32;
                        let sa = color_buf[si + 3] as u32;
                        if sa == 0 {
                            continue;
                        }
                        let dst = &mut self.surf.pixels[dst_row + col];
                        if sa == 255 {
                            *dst = (0xFFu32 << 24) | (sr << 16) | (sg << 8) | sb;
                        } else {
                            // Source is already premultiplied BGRA8.
                            let inv = 255 - sa;
                            let dr = (*dst >> 16) & 0xFF;
                            let dg = (*dst >> 8) & 0xFF;
                            let db = *dst & 0xFF;
                            let or = (sr + (dr * inv + 127) / 255).min(255);
                            let og = (sg + (dg * inv + 127) / 255).min(255);
                            let ob = (sb + (db * inv + 127) / 255).min(255);
                            *dst = (0xFFu32 << 24) | (or << 16) | (og << 8) | ob;
                        }
                    }
                }
            }
        }
    }

    /// Draw UI text (proportional system UI font) and return the final pen X coordinate.
    pub fn draw_text_ui(
        &mut self,
        x: f32,
        baseline_y: f32,
        text: &str,
        style: FontStyle,
        size_px: f32,
        color: Color,
        atlas: &mut GlyphAtlas,
        rasterizer: &mut dyn GlyphRasterizer,
    ) -> f32 {
        let mut pen = x;
        let clip_right = self.current_clip().right() as f32;
        for (_, cluster) in grapheme_clusters(text) {
            if pen > clip_right {
                break;
            }
            let glyph = atlas.get_or_rasterize(rasterizer, style, size_px, cluster);
            self.draw_glyph(pen, baseline_y, glyph, color, atlas);
            pen += glyph.advance;
        }
        pen
    }

    /// Measure UI text width in physical pixels.
    pub fn measure_text_ui(
        text: &str,
        style: FontStyle,
        size_px: f32,
        atlas: &mut GlyphAtlas,
        rasterizer: &mut dyn GlyphRasterizer,
    ) -> f32 {
        let mut w = 0.0f32;
        for (_, cluster) in grapheme_clusters(text) {
            let glyph = atlas.get_or_rasterize(rasterizer, style, size_px, cluster);
            w += glyph.advance;
        }
        w
    }

    /// Shift a sub-rectangle of the framebuffer vertically by `dy` pixels (positive = move down).
    /// Enables sub-millisecond scrolling by reusing 95% of already-rasterized pixels!
    pub fn scroll_rect_vertical(&mut self, rect: PixelRect, dy: i32) {
        let r = rect.intersect(self.surf.bounds());
        if r.is_empty() || dy == 0 || dy.abs() >= r.h {
            return;
        }
        let stride = self.surf.stride;
        let x0 = r.x as usize;
        let x1 = r.right() as usize;
        if dy > 0 {
            // Move rows downward: iterate bottom-to-top.
            for dst_y in ((r.y + dy)..r.bottom()).rev() {
                let src_y = dst_y - dy;
                let src_start = (src_y as usize) * stride + x0;
                let dst_start = (dst_y as usize) * stride + x0;
                self.surf
                    .pixels
                    .copy_within(src_start..src_start + (x1 - x0), dst_start);
            }
        } else {
            // Move rows upward: iterate top-to-bottom.
            let shift = -dy;
            for dst_y in r.y..(r.bottom() - shift) {
                let src_y = dst_y + shift;
                let src_start = (src_y as usize) * stride + x0;
                let dst_start = (dst_y as usize) * stride + x0;
                self.surf
                    .pixels
                    .copy_within(src_start..src_start + (x1 - x0), dst_start);
            }
        }
    }
}

/// Branchless alpha-blend of non-premultiplied source `(sr, sg, sb, sa)` over opaque `dst`.
#[inline(always)]
fn blend_over_rgb(dst: u32, sr: u32, sg: u32, sb: u32, sa: u32, inv_a: u32) -> u32 {
    let dr = (dst >> 16) & 0xFF;
    let dg = (dst >> 8) & 0xFF;
    let db = dst & 0xFF;
    let r = (sr * sa + dr * inv_a + 127) / 255;
    let g = (sg * sa + dg * inv_a + 127) / 255;
    let b = (sb * sa + db * inv_a + 127) / 255;
    (0xFFu32 << 24) | (r << 16) | (g << 8) | b
}
