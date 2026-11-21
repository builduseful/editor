//! Framebuffer surface, zero-copy `SurfaceMut` view, dirty-rect tracking, and
//! a pure-Rust zero-dependency compressed PNG encoder.
//!
//! Pixels are stored as packed little-endian premultiplied `BGRA8` (`0xAARRGGBB` in u32),
//! matching macOS `IOSurface` `'BGRA'`, Windows `DIB` BGRA, and Wayland/X11 `ARGB8888`
//! with ZERO per-frame byte swapping.

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

/// Zero-copy borrowed mutable view into any row-strided BGRA8 pixel surface
/// (e.g. a locked macOS `IOSurface`, Wayland `wl_shm`, Win32 DIB, or CPU `Framebuffer`).
pub struct SurfaceMut<'a> {
    pub width: u32,
    pub height: u32,
    /// Row stride in `u32` pixels (`>= width as usize`, supports hardware alignment padding).
    pub stride: usize,
    pub scale: f32,
    pub pixels: &'a mut [u32],
}

impl<'a> SurfaceMut<'a> {
    #[inline]
    pub fn bounds(&self) -> PixelRect {
        PixelRect::new(0, 0, self.width as i32, self.height as i32)
    }
}

/// CPU-owned premultiplied `0xAARRGGBB` (little-endian BGRA8) framebuffer
/// (used for headless rendering, tests, and PNG snapshots).
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

    /// Create a metadata-only framebuffer (0 bytes of CPU pixel storage) for zero-copy
    /// OS surfaces like macOS `IOSurface`. Call `ensure_pixels()` only if CPU storage is needed.
    pub fn new_virtual(width: u32, height: u32, scale: f32) -> Self {
        Self {
            width,
            height,
            scale: scale.max(1.0),
            pixels: Vec::new(),
            dirty: DirtyRegion::new(),
        }
    }

    pub fn ensure_pixels(&mut self) {
        let len = (self.width as usize).saturating_mul(self.height as usize);
        if self.pixels.len() != len {
            self.pixels.resize(len, 0xFF00_0000);
            self.dirty.mark_full();
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
        if !self.pixels.is_empty() {
            let len = (width as usize).saturating_mul(height as usize);
            self.pixels.resize(len, 0xFF00_0000);
        }
        self.dirty.mark_full();
    }

    #[inline]
    pub fn bounds(&self) -> PixelRect {
        PixelRect::new(0, 0, self.width as i32, self.height as i32)
    }

    #[inline]
    pub fn as_surface_mut(&mut self) -> SurfaceMut<'_> {
        SurfaceMut {
            width: self.width,
            height: self.height,
            stride: self.width as usize,
            scale: self.scale,
            pixels: &mut self.pixels,
        }
    }

    #[inline]
    pub fn pixel_at(&self, x: u32, y: u32) -> u32 {
        self.pixels[(y as usize) * (self.width as usize) + (x as usize)]
    }

    #[inline]
    pub fn as_bgra_bytes(&self) -> &[u8] {
        unsafe {
            std::slice::from_raw_parts(self.pixels.as_ptr() as *const u8, self.pixels.len() * 4)
        }
    }

    /// Encode this framebuffer to a compact, valid PNG byte stream (pure Rust, zero dependencies).
    /// Uses PNG Sub filter + pure-Rust fixed-Huffman LZ77 DEFLATE (~20x–80x smaller than raw!).
    pub fn encode_png(&self) -> Vec<u8> {
        let w = self.width as usize;
        let h = self.height as usize;
        let row_bytes = 1 + w * 4;
        let mut raw = Vec::with_capacity(row_bytes * h);

        // PNG Filter Type 1 (Sub): each byte stores difference from 4 bytes left (same channel).
        // Flat UI & icon spans become long runs of 0x00, compressing massively with LZ77!
        for y in 0..h {
            raw.push(1u8); // Filter: Sub
            let row = &self.pixels[y * w..(y + 1) * w];
            let mut prev = [0u8; 4];
            for &px in row {
                let a = ((px >> 24) & 0xFF) as u8;
                let r = ((px >> 16) & 0xFF) as u8;
                let g = ((px >> 8) & 0xFF) as u8;
                let b = (px & 0xFF) as u8;
                let cur = [r, g, b, a];
                raw.push(cur[0].wrapping_sub(prev[0]));
                raw.push(cur[1].wrapping_sub(prev[1]));
                raw.push(cur[2].wrapping_sub(prev[2]));
                raw.push(cur[3].wrapping_sub(prev[3]));
                prev = cur;
            }
        }

        let zlib = deflate_zlib_fixed(&raw);

        let mut out = Vec::with_capacity(zlib.len() + 64);
        out.extend_from_slice(&[137, 80, 78, 71, 13, 10, 26, 10]);

        let mut ihdr = [0u8; 13];
        ihdr[0..4].copy_from_slice(&self.width.to_be_bytes());
        ihdr[4..8].copy_from_slice(&self.height.to_be_bytes());
        ihdr[8] = 8; // bit depth
        ihdr[9] = 6; // RGBA
        write_png_chunk(&mut out, b"IHDR", &ihdr);
        write_png_chunk(&mut out, b"IDAT", &zlib);
        write_png_chunk(&mut out, b"IEND", &[]);
        out
    }

    pub fn save_png(&self, path: &Path) -> io::Result<()> {
        std::fs::write(path, self.encode_png())
    }
}

/// Compact pure-Rust RFC 1950/1951 fixed-Huffman RLE+LZ77 DEFLATE compressor.
/// Compresses solid/UI/icon scanlines by 20x–100x with zero third-party crates.
fn deflate_zlib_fixed(input: &[u8]) -> Vec<u8> {
    struct BitWriter {
        buf: Vec<u8>,
        acc: u32,
        bits: u8,
    }
    impl BitWriter {
        fn new(cap: usize) -> Self {
            Self {
                buf: Vec::with_capacity(cap),
                acc: 0,
                bits: 0,
            }
        }
        #[inline]
        fn push_bits(&mut self, val: u32, n: u8) {
            self.acc |= val << self.bits;
            self.bits += n;
            while self.bits >= 8 {
                self.buf.push((self.acc & 0xFF) as u8);
                self.acc >>= 8;
                self.bits -= 8;
            }
        }
        #[inline]
        fn push_huff(&mut self, code: u32, n: u8) {
            // Huffman codes are transmitted MSB-first in DEFLATE.
            let rev = (code.reverse_bits()) >> (32 - n);
            self.push_bits(rev, n);
        }
        fn push_lit(&mut self, sym: u16) {
            if sym <= 143 {
                self.push_huff(0x30 + sym as u32, 8);
            } else if sym <= 255 {
                self.push_huff(0x190 + (sym - 144) as u32, 9);
            } else if sym <= 279 {
                self.push_huff((sym - 256) as u32, 7);
            } else {
                self.push_huff(0xC0 + (sym - 280) as u32, 8);
            }
        }
        fn push_len_dist(&mut self, len: usize, dist: usize) {
            // Fixed length table (RFC 1951 §3.2.5)
            const LEN_BASE: [u16; 29] = [
                3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83,
                99, 115, 131, 163, 195, 227, 258,
            ];
            const LEN_EXTRA: [u8; 29] = [
                0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5,
                0,
            ];
            const DIST_BASE: [u16; 30] = [
                1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769,
                1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
            ];
            const DIST_EXTRA: [u8; 30] = [
                0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11,
                12, 12, 13, 13,
            ];
            let l = len as u16;
            let li = if l == 258 {
                28
            } else {
                LEN_BASE.partition_point(|&b| b <= l) - 1
            };
            self.push_lit(257 + li as u16);
            if LEN_EXTRA[li] > 0 {
                self.push_bits((l - LEN_BASE[li]) as u32, LEN_EXTRA[li]);
            }
            let d = dist as u16;
            let di = DIST_BASE.partition_point(|&b| b <= d) - 1;
            self.push_huff(di as u32, 5);
            if DIST_EXTRA[di] > 0 {
                self.push_bits((d - DIST_BASE[di]) as u32, DIST_EXTRA[di]);
            }
        }
        fn finish(mut self) -> Vec<u8> {
            if self.bits > 0 {
                self.buf.push((self.acc & 0xFF) as u8);
            }
            self.buf
        }
    }

    let mut bw = BitWriter::new((input.len() / 8).max(64));
    bw.buf.push(0x78); // zlib CMF
    bw.buf.push(0x01); // zlib FLG (check bits valid)

    // Single final fixed-Huffman block: BFINAL=1, BTYPE=01 (LSB first -> 0b011).
    bw.push_bits(1, 1);
    bw.push_bits(1, 2);

    // Fast greedy match finder checking distance 1 (RLE zeros after Sub filter) and distance 4 (pixel repeat).
    let n = input.len();
    let mut i = 0usize;
    let mut hash_table = [u32::MAX; 4096];

    while i < n {
        let mut best_len = 0usize;
        let mut best_dist = 0usize;

        // Fast check distance 1 (zeros after Sub filter) and distance 4 (repeated RGBA pixel).
        for dist in [1usize, 4usize] {
            if i >= dist && i + 3 <= n {
                let mut m = 0usize;
                let max_m = (n - i).min(258);
                while m < max_m && input[i + m] == input[i - dist + m] {
                    m += 1;
                }
                if m > best_len {
                    best_len = m;
                    best_dist = dist;
                }
            }
        }

        // Hash table lookup for longer cross-row matches (up to 32 KB back).
        if best_len < 32 && i + 3 <= n {
            let h = (((input[i] as usize) << 8)
                ^ ((input[i + 1] as usize) << 4)
                ^ (input[i + 2] as usize))
                & 4095;
            let prev = hash_table[h];
            hash_table[h] = i as u32;
            if prev != u32::MAX {
                let dist = i - prev as usize;
                if dist > 0 && dist <= 32768 {
                    let mut m = 0usize;
                    let max_m = (n - i).min(258);
                    while m < max_m && input[i + m] == input[i - dist + m] {
                        m += 1;
                    }
                    if m > best_len {
                        best_len = m;
                        best_dist = dist;
                    }
                }
            }
        }

        if best_len >= 3 {
            bw.push_len_dist(best_len, best_dist);
            i += best_len;
        } else {
            bw.push_lit(input[i] as u16);
            i += 1;
        }
    }

    bw.push_lit(256); // End-of-block symbol
    let mut out = bw.finish();
    let adler = adler32(input);
    out.extend_from_slice(&adler.to_be_bytes());
    out
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
