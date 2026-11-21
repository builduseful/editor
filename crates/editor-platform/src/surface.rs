//! Framebuffer surface, dirty-rect tracking, and a pure-Rust zero-dependency PNG encoder.
//!
//! Pixels are stored as packed little-endian premultiplied `BGRA8` (`0xAARRGGBB` in u32),
//! which matches macOS `kCGBitmapByteOrder32Little | kCGImageAlphaPremultipliedFirst`,
//! Windows `DIB` BGRA, and Wayland/X11 `ARGB8888` with ZERO per-frame byte swapping!

use std::io;
use std::path::Path;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PixelRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl PixelRect {
    pub const ZERO: Self = Self {
        x: 0,
        y: 0,
        w: 0,
        h: 0,
    };

    #[inline]
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    #[inline]
    pub const fn right(self) -> i32 {
        self.x + self.w
    }

    #[inline]
    pub const fn bottom(self) -> i32 {
        self.y + self.h
    }

    #[inline]
    pub const fn is_empty(self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    #[inline]
    pub fn contains(self, px: i32, py: i32) -> bool {
        px >= self.x && px < self.right() && py >= self.y && py < self.bottom()
    }

    #[inline]
    pub fn intersect(self, other: Self) -> Self {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = self.right().min(other.right());
        let y1 = self.bottom().min(other.bottom());
        if x1 <= x0 || y1 <= y0 {
            Self::ZERO
        } else {
            Self::new(x0, y0, x1 - x0, y1 - y0)
        }
    }

    #[inline]
    pub fn union(self, other: Self) -> Self {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        let x0 = self.x.min(other.x);
        let y0 = self.y.min(other.y);
        let x1 = self.right().max(other.right());
        let y1 = self.bottom().max(other.bottom());
        Self::new(x0, y0, x1 - x0, y1 - y0)
    }
}

/// Compact dirty-rectangle tracker for minimal partial redraws.
#[derive(Debug, Default, Clone)]
pub struct DirtyRegion {
    full: bool,
    rects: Vec<PixelRect>,
}

impl DirtyRegion {
    pub fn new() -> Self {
        Self {
            full: true,
            rects: Vec::new(),
        }
    }

    pub fn mark_full(&mut self) {
        self.full = true;
        self.rects.clear();
    }

    pub fn mark_rect(&mut self, rect: PixelRect) {
        if self.full || rect.is_empty() {
            return;
        }
        if self.rects.len() >= 16 {
            let mut merged = rect;
            for r in self.rects.drain(..) {
                merged = merged.union(r);
            }
            self.rects.push(merged);
        } else {
            self.rects.push(rect);
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.full || !self.rects.is_empty()
    }

    pub fn is_full(&self) -> bool {
        self.full
    }

    pub fn rects(&self) -> &[PixelRect] {
        &self.rects
    }

    pub fn bounding_rect(&self, bounds: PixelRect) -> PixelRect {
        if self.full {
            return bounds;
        }
        let mut out = PixelRect::ZERO;
        for r in &self.rects {
            out = out.union(r.intersect(bounds));
        }
        out
    }

    pub fn clear(&mut self) {
        self.full = false;
        self.rects.clear();
    }
}

/// CPU-owned premultiplied `0xAARRGGBB` (little-endian BGRA8) framebuffer.
#[derive(Debug, Clone)]
pub struct Framebuffer {
    pub width: u32,
    pub height: u32,
    pub scale: f32,
    pub pixels: Vec<u32>,
    pub dirty: DirtyRegion,
}

impl Framebuffer {
    pub fn new(width: u32, height: u32, scale: f32) -> Self {
        let len = (width as usize).saturating_mul(height as usize);
        Self {
            width,
            height,
            scale: scale.max(1.0),
            pixels: vec![0xFF00_0000; len],
            dirty: DirtyRegion::new(),
        }
    }

    pub fn resize(&mut self, width: u32, height: u32, scale: f32) {
        let scale = scale.max(1.0);
        if self.width == width && self.height == height && (self.scale - scale).abs() < 1e-4 {
            return;
        }
        self.width = width;
        self.height = height;
        self.scale = scale;
        let len = (width as usize).saturating_mul(height as usize);
        self.pixels.resize(len, 0xFF00_0000);
        self.dirty.mark_full();
    }

    #[inline]
    pub fn bounds(&self) -> PixelRect {
        PixelRect::new(0, 0, self.width as i32, self.height as i32)
    }

    #[inline]
    pub fn pixel_at(&self, x: u32, y: u32) -> u32 {
        self.pixels[(y as usize) * (self.width as usize) + (x as usize)]
    }

    #[inline]
    pub fn as_bgra_bytes(&self) -> &[u8] {
        // Safe: u32 slice reinterpreted as contiguous u8 bytes of length pixels.len() * 4.
        unsafe {
            std::slice::from_raw_parts(self.pixels.as_ptr() as *const u8, self.pixels.len() * 4)
        }
    }

    /// Encode this framebuffer to a valid PNG byte stream (pure Rust, zero dependencies).
    /// Uses stored (uncompressed) zlib blocks for instant, deterministic snapshot output.
    pub fn encode_png(&self) -> Vec<u8> {
        let w = self.width as usize;
        let h = self.height as usize;
        // Raw scanlines: 1 filter byte (0 = None) + w * 4 RGBA bytes per row.
        let row_bytes = 1 + w * 4;
        let mut raw = Vec::with_capacity(row_bytes * h);
        for y in 0..h {
            raw.push(0u8); // Filter type: None
            let row = &self.pixels[y * w..(y + 1) * w];
            for &px in row {
                let a = ((px >> 24) & 0xFF) as u8;
                let r = ((px >> 16) & 0xFF) as u8;
                let g = ((px >> 8) & 0xFF) as u8;
                let b = (px & 0xFF) as u8;
                raw.extend_from_slice(&[r, g, b, a]);
            }
        }

        // Wrap `raw` in a valid zlib stream using uncompressed DEFLATE blocks (max 65535 bytes each).
        let mut zlib = Vec::with_capacity(raw.len() + (raw.len() / 65535 + 1) * 5 + 6);
        zlib.push(0x78); // CMF: deflate, 32K window
        zlib.push(0x01); // FLG: no dict, check bits valid (0x7801 % 31 == 0)
        let mut pos = 0;
        if raw.is_empty() {
            zlib.extend_from_slice(&[0x01, 0x00, 0x00, 0xFF, 0xFF]);
        } else {
            while pos < raw.len() {
                let chunk_len = (raw.len() - pos).min(65535);
                let is_last = pos + chunk_len >= raw.len();
                zlib.push(if is_last { 0x01 } else { 0x00 });
                let len16 = chunk_len as u16;
                let nlen16 = !len16;
                zlib.extend_from_slice(&len16.to_le_bytes());
                zlib.extend_from_slice(&nlen16.to_le_bytes());
                zlib.extend_from_slice(&raw[pos..pos + chunk_len]);
                pos += chunk_len;
            }
        }
        let adler = adler32(&raw);
        zlib.extend_from_slice(&adler.to_be_bytes());

        let mut out = Vec::with_capacity(zlib.len() + 64);
        out.extend_from_slice(&[137, 80, 78, 71, 13, 10, 26, 10]); // PNG signature

        let mut ihdr = [0u8; 13];
        ihdr[0..4].copy_from_slice(&self.width.to_be_bytes());
        ihdr[4..8].copy_from_slice(&self.height.to_be_bytes());
        ihdr[8] = 8; // bit depth
        ihdr[9] = 6; // color type: RGBA
        ihdr[10] = 0; // compression
        ihdr[11] = 0; // filter
        ihdr[12] = 0; // interlace
        write_png_chunk(&mut out, b"IHDR", &ihdr);
        write_png_chunk(&mut out, b"IDAT", &zlib);
        write_png_chunk(&mut out, b"IEND", &[]);
        out
    }

    pub fn save_png(&self, path: &Path) -> io::Result<()> {
        std::fs::write(path, self.encode_png())
    }
}

fn write_png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(kind, data);
    out.extend_from_slice(&crc.to_be_bytes());
}

fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65521;
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for chunk in data.chunks(5552) {
        for &byte in chunk {
            a += byte as u32;
            b += a;
        }
        a %= MOD;
        b %= MOD;
    }
    (b << 16) | a
}

fn crc32(prefix: &[u8], data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in prefix.iter().chain(data.iter()) {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}
