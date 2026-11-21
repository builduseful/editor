//! L1-cache-hot Shelf-Packed Glyph Atlas (Alpha8 text + Bgra32 color emoji).
//!
//! Hot-path innovation:
//! - Printable ASCII (`0x20..=0x7E`) in `Mono` and `MonoBold` at the active document
//!   size are stored in flat `[Option<AtlasGlyph>; 96]` direct-index arrays!
//!   Zero hashing, zero allocations, O(1) lookup by `byte - 32`!
//! - Proportional UI glyphs, non-ASCII UTF-8, and Emoji clusters use a fast hash map
//!   backed by the same compact shelf-packed pixel atlases.

use std::collections::HashMap;

use editor_platform::{FontMetrics, FontStyle, GlyphFormat, GlyphRasterizer};

#[derive(Debug, Clone, Copy)]
pub struct AtlasGlyph {
    pub atlas_x: u32,
    pub atlas_y: u32,
    pub width: u32,
    pub height: u32,
    pub bearing_x: i32,
    pub bearing_y: i32,
    pub advance: f32,
    pub format: GlyphFormat,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct GlyphKey {
    style: FontStyle,
    size_q4: u32,
    cluster: String,
}

struct ShelfPacker {
    width: u32,
    height: u32,
    cursor_x: u32,
    cursor_y: u32,
    row_height: u32,
    bytes_per_pixel: usize,
    pixels: Vec<u8>,
}

impl ShelfPacker {
    fn new(width: u32, height: u32, bytes_per_pixel: usize) -> Self {
        Self {
            width,
            height,
            cursor_x: 1,
            cursor_y: 1,
            row_height: 0,
            bytes_per_pixel,
            pixels: vec![0u8; (width as usize) * (height as usize) * bytes_per_pixel],
        }
    }

    fn clear(&mut self) {
        self.cursor_x = 1;
        self.cursor_y = 1;
        self.row_height = 0;
        self.pixels.fill(0);
    }

    fn allocate(&mut self, w: u32, h: u32, src: &[u8]) -> Option<(u32, u32)> {
        if w == 0 || h == 0 {
            return Some((0, 0));
        }
        let pad = 1u32;
        if self.cursor_x + w + pad > self.width {
            self.cursor_x = 1;
            self.cursor_y += self.row_height + pad;
            self.row_height = 0;
        }
        if self.cursor_y + h + pad > self.height {
            if self.height >= 4096 {
                return None;
            }
            let new_h = (self.height * 2).min(4096);
            self.pixels
                .resize((self.width as usize) * (new_h as usize) * self.bytes_per_pixel, 0);
            self.height = new_h;
            if self.cursor_y + h + pad > self.height {
                return None;
            }
        }
        let ax = self.cursor_x;
        let ay = self.cursor_y;
        self.cursor_x += w + pad;
        self.row_height = self.row_height.max(h);

        let bpp = self.bytes_per_pixel;
        let dst_stride = (self.width as usize) * bpp;
        let src_stride = (w as usize) * bpp;
        if src.len() >= src_stride * (h as usize) {
            for row in 0..(h as usize) {
                let d0 = ((ay as usize) + row) * dst_stride + (ax as usize) * bpp;
                let s0 = row * src_stride;
                self.pixels[d0..d0 + src_stride].copy_from_slice(&src[s0..s0 + src_stride]);
            }
        }
        Some((ax, ay))
    }
}

pub struct GlyphAtlas {
    alpha: ShelfPacker,
    color: ShelfPacker,
    cache: HashMap<GlyphKey, AtlasGlyph>,
    metrics_cache: HashMap<(FontStyle, u32), FontMetrics>,
    /// Hot direct-indexed ASCII tables for the active document monospace font size!
    fast_mono_size_q4: u32,
    fast_mono: [Option<AtlasGlyph>; 96],
    fast_mono_bold: [Option<AtlasGlyph>; 96],
}

impl Default for GlyphAtlas {
    fn default() -> Self {
        Self::new()
    }
}

impl GlyphAtlas {
    pub fn new() -> Self {
        Self {
            alpha: ShelfPacker::new(512, 512, 1),
            color: ShelfPacker::new(512, 256, 4),
            cache: HashMap::with_capacity(512),
            metrics_cache: HashMap::with_capacity(16),
            fast_mono_size_q4: 0,
            fast_mono: [None; 96],
            fast_mono_bold: [None; 96],
        }
    }

    pub fn clear(&mut self) {
        self.alpha.clear();
        self.color.clear();
        self.cache.clear();
        self.metrics_cache.clear();
        self.fast_mono_size_q4 = 0;
        self.fast_mono = [None; 96];
        self.fast_mono_bold = [None; 96];
    }

    #[inline]
    fn quantize_size(size_px: f32) -> u32 {
        (size_px.max(4.0) * 4.0).round() as u32
    }

    pub fn metrics(
        &mut self,
        rasterizer: &mut dyn GlyphRasterizer,
        style: FontStyle,
        size_px: f32,
    ) -> FontMetrics {
        let q = Self::quantize_size(size_px);
        if let Some(&m) = self.metrics_cache.get(&(style, q)) {
            return m;
        }
        let m = rasterizer.metrics(style, q as f32 / 4.0);
        self.metrics_cache.insert((style, q), m);
        m
    }

    /// Ultra-fast direct-indexed ASCII glyph fetch for monospace document rendering.
    #[inline]
    pub fn get_ascii_mono(
        &mut self,
        rasterizer: &mut dyn GlyphRasterizer,
        bold: bool,
        size_px: f32,
        ascii_byte: u8,
    ) -> AtlasGlyph {
        let q = Self::quantize_size(size_px);
        if self.fast_mono_size_q4 != q {
            self.fast_mono_size_q4 = q;
            self.fast_mono = [None; 96];
            self.fast_mono_bold = [None; 96];
        }
        let idx = (ascii_byte.saturating_sub(0x20) as usize).min(95);
        let table = if bold {
            &self.fast_mono_bold
        } else {
            &self.fast_mono
        };
        if let Some(g) = table[idx] {
            return g;
        }
        let buf = [ascii_byte];
        let s = std::str::from_utf8(&buf).unwrap_or(" ");
        let style = if bold {
            FontStyle::MonoBold
        } else {
            FontStyle::Mono
        };
        let g = self.get_or_rasterize(rasterizer, style, size_px, s);
        if bold {
            self.fast_mono_bold[idx] = Some(g);
        } else {
            self.fast_mono[idx] = Some(g);
        }
        g
    }

    /// Fetch or rasterize any grapheme cluster (ASCII, accented UTF-8, or Emoji).
    pub fn get_or_rasterize(
        &mut self,
        rasterizer: &mut dyn GlyphRasterizer,
        style: FontStyle,
        size_px: f32,
        cluster: &str,
    ) -> AtlasGlyph {
        let q = Self::quantize_size(size_px);
        let key = GlyphKey {
            style,
            size_q4: q,
            cluster: cluster.to_string(),
        };
        if let Some(&g) = self.cache.get(&key) {
            return g;
        }

        let r = rasterizer.rasterize(style, q as f32 / 4.0, cluster);
        let alloc = match r.format {
            GlyphFormat::Alpha8 => self.alpha.allocate(r.width, r.height, &r.pixels),
            GlyphFormat::Bgra32 => self.color.allocate(r.width, r.height, &r.pixels),
        };
        let (ax, ay) = match alloc {
            Some(pos) => pos,
            None => {
                // Atlas full: reset cleanly and re-pack this glyph.
                self.clear();
                match r.format {
                    GlyphFormat::Alpha8 => self
                        .alpha
                        .allocate(r.width, r.height, &r.pixels)
                        .unwrap_or((0, 0)),
                    GlyphFormat::Bgra32 => self
                        .color
                        .allocate(r.width, r.height, &r.pixels)
                        .unwrap_or((0, 0)),
                }
            }
        };

        let entry = AtlasGlyph {
            atlas_x: ax,
            atlas_y: ay,
            width: r.width,
            height: r.height,
            bearing_x: r.bearing_x,
            bearing_y: r.bearing_y,
            advance: r.advance,
            format: r.format,
        };
        self.cache.insert(key, entry);
        entry
    }

    #[inline]
    pub fn alpha_pixels(&self) -> (&[u8], u32) {
        (&self.alpha.pixels, self.alpha.width)
    }

    #[inline]
    pub fn color_pixels(&self) -> (&[u8], u32) {
        (&self.color.pixels, self.color.width)
    }

    pub fn cached_glyph_count(&self) -> usize {
        self.cache.len()
    }
}
