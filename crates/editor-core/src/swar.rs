//! Portable 64-bit SWAR (SIMD Within A Register) primitives in 100% safe Rust.
//!
//! Processes 32 bytes (4×`u64`) per branch on hot scan loops using portable bitmask
//! arithmetic, achieving multi-GB/s byte search, rarest-pair literal prefiltering,
//! and POPCNT newline counting across every CPU architecture with zero `unsafe` and
//! zero external crates.

const LO_BITS: u64 = 0x0101_0101_0101_0101;
const HI_BITS: u64 = 0x8080_8080_8080_8080;
const CASE_BIT: u64 = 0x2020_2020_2020_2020;

/// Approximate English/source-code byte frequency rank (0 = rarest, 255 = most common).
/// Used to pick the two rarest bytes in a search needle so SWAR prefiltering rejects
/// non-matching 32-byte blocks in a single branch even for common prefixes like `"pub fn "`.
#[rustfmt::skip]
const BYTE_FREQ: [u8; 256] = [
     0,  1,  2,  3,  4,  5,  6,  7,  8,140,245,  9, 10,160, 11, 12, // 0x00..0x0F (\t, \n, \r)
    13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, // 0x10..0x1F
   255, 70,130, 80, 72, 68, 76, 95,155,155,115, 85,150,135,165,125, // ' ' ! " # $ % & ' ( ) * + , - . /
   145,138,128,120,116,114,112,110,108,106,142,148,118,146,118, 74, // 0..9 : ; < = > ?
    64,175,152,162,164,195,156,154,166,185,132,144,168,160,180,182, // @ A..O
   158,126,178,176,190,163,147,157,134,151,124,104, 96,104, 66,192, // P..Z [ \ ] ^ _
    62,225,180,205,212,250,196,188,208,236,140,166,216,202,234,232, // ` a..o
   200,136,230,228,242,206,172,184,154,186,134,149, 78,149, 65, 29, // p..z { | } ~ DEL
    30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, // 0x80..0x8F
    30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, // 0x90..0x9F
    30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, // 0xA0..0xAF
    30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, // 0xB0..0xBF
    30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, // 0xC0..0xCF
    30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, // 0xD0..0xDF
    30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, // 0xE0..0xEF
    30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, // 0xF0..0xFF
];

#[inline(always)]
const fn repeat_byte(b: u8) -> u64 {
    (b as u64) * LO_BITS
}

#[inline(always)]
const fn has_zero_byte(v: u64) -> u64 {
    v.wrapping_sub(LO_BITS) & !v & HI_BITS
}

#[inline(always)]
const fn has_byte(word: u64, byte: u8) -> u64 {
    has_zero_byte(word ^ repeat_byte(byte))
}

#[inline(always)]
const fn case_mask_for(byte_lower: u8) -> u64 {
    if byte_lower.is_ascii_lowercase() {
        CASE_BIT
    } else {
        0
    }
}

/// Precomputed two-byte rarest-pair offsets inside a search needle (`len >= 2`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RarePair {
    pub off1: usize,
    pub off2: usize,
    pub b1: u8,
    pub b2: u8,
}

impl RarePair {
    /// Select the rarest byte and a distinct second-rarest byte in `needle`.
    /// For needles of length 0 or 1, returns a trivial zeroed pair (callers dispatch `len <= 1` directly).
    pub fn from_needle(needle: &[u8]) -> Self {
        if needle.len() < 2 {
            let b = needle.first().copied().unwrap_or(0);
            return Self {
                off1: 0,
                off2: 0,
                b1: b,
                b2: b,
            };
        }
        let mut off1 = 0usize;
        let mut rank1 = BYTE_FREQ[needle[0] as usize];
        for (i, &b) in needle.iter().enumerate().skip(1) {
            let r = BYTE_FREQ[b as usize];
            if r < rank1 {
                rank1 = r;
                off1 = i;
            }
        }
        let b1 = needle[off1];

        // Pick the rarest byte with a DIFFERENT value from `b1` (Maximizes pair filtering!).
        let mut off2 = None;
        let mut rank2 = u8::MAX;
        for (i, &b) in needle.iter().enumerate() {
            if i == off1 || b == b1 {
                continue;
            }
            let r = BYTE_FREQ[b as usize];
            if off2.is_none() || r < rank2 {
                rank2 = r;
                off2 = Some(i);
            }
        }
        // Fallback if all bytes in `needle` are identical (e.g. `"aaaa"`): pick farthest offset.
        let off2 = off2.unwrap_or_else(|| if off1 == 0 { needle.len() - 1 } else { 0 });
        Self {
            off1,
            off2,
            b1,
            b2: needle[off2],
        }
    }
}

/// Fast 8-byte-at-a-time check that all bytes are pure ASCII (`< 0x80`).
#[inline]
pub fn is_all_ascii(bytes: &[u8]) -> bool {
    let mut chunks = bytes.chunks_exact(8);
    for chunk in &mut chunks {
        let word = u64::from_ne_bytes(chunk.try_into().unwrap());
        if (word & HI_BITS) != 0 {
            return false;
        }
    }
    chunks.remainder().iter().all(|&b| b < 0x80)
}

/// Fast 32-byte-unrolled SWAR search for a single byte (`memchr` in pure safe Rust).
#[inline]
pub fn find_byte(haystack: &[u8], needle: u8) -> Option<usize> {
    let mask = repeat_byte(needle);
    let len = haystack.len();
    let mut offset = 0usize;
    while offset + 32 <= len {
        let w0 = u64::from_le_bytes(haystack[offset..offset + 8].try_into().unwrap());
        let w1 = u64::from_le_bytes(haystack[offset + 8..offset + 16].try_into().unwrap());
        let w2 = u64::from_le_bytes(haystack[offset + 16..offset + 24].try_into().unwrap());
        let w3 = u64::from_le_bytes(haystack[offset + 24..offset + 32].try_into().unwrap());
        let h0 = has_zero_byte(w0 ^ mask);
        let h1 = has_zero_byte(w1 ^ mask);
        let h2 = has_zero_byte(w2 ^ mask);
        let h3 = has_zero_byte(w3 ^ mask);
        if (h0 | h1 | h2 | h3) != 0 {
            if h0 != 0 {
                return Some(offset + (h0.trailing_zeros() >> 3) as usize);
            }
            if h1 != 0 {
                return Some(offset + 8 + (h1.trailing_zeros() >> 3) as usize);
            }
            if h2 != 0 {
                return Some(offset + 16 + (h2.trailing_zeros() >> 3) as usize);
            }
            return Some(offset + 24 + (h3.trailing_zeros() >> 3) as usize);
        }
        offset += 32;
    }
    while offset + 8 <= len {
        let word = u64::from_le_bytes(haystack[offset..offset + 8].try_into().unwrap());
        let hit = has_zero_byte(word ^ mask);
        if hit != 0 {
            return Some(offset + (hit.trailing_zeros() >> 3) as usize);
        }
        offset += 8;
    }
    for (i, &b) in haystack[offset..].iter().enumerate() {
        if b == needle {
            return Some(offset + i);
        }
    }
    None
}

/// Fast reverse 8-byte SWAR search for a single byte (`memrchr` in pure safe Rust).
#[inline]
pub fn rfind_byte(haystack: &[u8], needle: u8) -> Option<usize> {
    let mask = repeat_byte(needle);
    let mut end = haystack.len();
    while end >= 8 {
        let start = end - 8;
        let word = u64::from_le_bytes(haystack[start..end].try_into().unwrap());
        let hit = has_zero_byte(word ^ mask);
        if hit != 0 {
            // Little-endian lane 7 sits at bit 63 (`0x8000_0000_0000_0000`), so `leading_zeros >> 3`
            // counts non-matching high lanes from lane 7 down toward lane 0.
            let lane = 7 - (hit.leading_zeros() >> 3) as usize;
            return Some(start + lane);
        }
        end = start;
    }
    while end > 0 {
        end -= 1;
        if haystack[end] == needle {
            return Some(end);
        }
    }
    None
}

/// Find the next line terminator (`\r\n`, `\n`, or `\r`) in `bytes`.
/// Returns `(start_offset, terminator_len)`.
#[inline]
pub fn find_next_line_break(bytes: &[u8]) -> Option<(usize, usize)> {
    let mut offset = 0usize;
    let chunks = bytes.chunks_exact(8);
    let rem = chunks.remainder();
    for chunk in chunks {
        let word = u64::from_le_bytes(chunk.try_into().unwrap());
        let hit = has_byte(word, b'\n') | has_byte(word, b'\r');
        if hit != 0 {
            let idx = offset + (hit.trailing_zeros() >> 3) as usize;
            if bytes[idx] == b'\r' && idx + 1 < bytes.len() && bytes[idx + 1] == b'\n' {
                return Some((idx, 2));
            }
            return Some((idx, 1));
        }
        offset += 8;
    }
    for (i, &b) in rem.iter().enumerate() {
        if b == b'\n' {
            return Some((offset + i, 1));
        }
        if b == b'\r' {
            let idx = offset + i;
            if idx + 1 < bytes.len() && bytes[idx + 1] == b'\n' {
                return Some((idx, 2));
            }
            return Some((idx, 1));
        }
    }
    None
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LineBreakCounts {
    pub lf: u64,
    pub crlf: u64,
    pub cr: u64,
}

impl LineBreakCounts {
    #[inline]
    pub fn total(self) -> u64 {
        self.lf + self.crlf + self.cr
    }
}

/// Count exact LF, CRLF, and lone-CR terminators in `bytes`, skipping 8-byte ASCII/text words
/// that contain neither `\n` nor `\r` in a single branch.
pub fn count_line_breaks(bytes: &[u8]) -> LineBreakCounts {
    let mut counts = LineBreakCounts::default();
    let mut i = 0usize;
    let len = bytes.len();

    while i + 8 <= len {
        let word = u64::from_le_bytes(bytes[i..i + 8].try_into().unwrap());
        let hit = has_byte(word, b'\n') | has_byte(word, b'\r');
        if hit == 0 {
            i += 8;
            continue;
        }
        let first = i + (hit.trailing_zeros() >> 3) as usize;
        if bytes[first] == b'\n' {
            counts.lf += 1;
            i = first + 1;
        } else if first + 1 < len && bytes[first + 1] == b'\n' {
            counts.crlf += 1;
            i = first + 2;
        } else {
            counts.cr += 1;
            i = first + 1;
        }
    }

    while i < len {
        match bytes[i] {
            b'\n' => {
                counts.lf += 1;
                i += 1;
            }
            b'\r' => {
                if i + 1 < len && bytes[i + 1] == b'\n' {
                    counts.crlf += 1;
                    i += 2;
                } else {
                    counts.cr += 1;
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }

    counts
}

/// Fast 32-byte-unrolled SWAR search for a single byte, matching ASCII case-insensitively.
/// `byte_lower` must already be ASCII-lowercased.
#[inline]
pub fn find_byte_ascii_ci(haystack: &[u8], byte_lower: u8) -> Option<usize> {
    if !byte_lower.is_ascii_lowercase() {
        return find_byte(haystack, byte_lower);
    }
    let mask = repeat_byte(byte_lower);
    let len = haystack.len();
    let mut offset = 0usize;
    while offset + 32 <= len {
        let w0 = u64::from_le_bytes(haystack[offset..offset + 8].try_into().unwrap());
        let w1 = u64::from_le_bytes(haystack[offset + 8..offset + 16].try_into().unwrap());
        let w2 = u64::from_le_bytes(haystack[offset + 16..offset + 24].try_into().unwrap());
        let w3 = u64::from_le_bytes(haystack[offset + 24..offset + 32].try_into().unwrap());
        let h0 = has_zero_byte((w0 | CASE_BIT) ^ mask);
        let h1 = has_zero_byte((w1 | CASE_BIT) ^ mask);
        let h2 = has_zero_byte((w2 | CASE_BIT) ^ mask);
        let h3 = has_zero_byte((w3 | CASE_BIT) ^ mask);
        if (h0 | h1 | h2 | h3) != 0 {
            if h0 != 0 {
                return Some(offset + (h0.trailing_zeros() >> 3) as usize);
            }
            if h1 != 0 {
                return Some(offset + 8 + (h1.trailing_zeros() >> 3) as usize);
            }
            if h2 != 0 {
                return Some(offset + 16 + (h2.trailing_zeros() >> 3) as usize);
            }
            return Some(offset + 24 + (h3.trailing_zeros() >> 3) as usize);
        }
        offset += 32;
    }
    while offset + 8 <= len {
        let word = u64::from_le_bytes(haystack[offset..offset + 8].try_into().unwrap());
        let hit = has_zero_byte((word | CASE_BIT) ^ mask);
        if hit != 0 {
            return Some(offset + (hit.trailing_zeros() >> 3) as usize);
        }
        offset += 8;
    }
    for (i, &b) in haystack[offset..].iter().enumerate() {
        if (b | 0x20) == byte_lower {
            return Some(offset + i);
        }
    }
    None
}

/// Fast 32-byte-unrolled count of occurrences of `needle` using SWAR + parallel POPCNT.
#[inline]
pub fn count_byte(haystack: &[u8], needle: u8) -> usize {
    let mask = repeat_byte(needle);
    let len = haystack.len();
    let mut offset = 0usize;
    let mut count = 0usize;
    while offset + 32 <= len {
        let w0 = u64::from_le_bytes(haystack[offset..offset + 8].try_into().unwrap());
        let w1 = u64::from_le_bytes(haystack[offset + 8..offset + 16].try_into().unwrap());
        let w2 = u64::from_le_bytes(haystack[offset + 16..offset + 24].try_into().unwrap());
        let w3 = u64::from_le_bytes(haystack[offset + 24..offset + 32].try_into().unwrap());
        let c0 = has_zero_byte(w0 ^ mask).count_ones() as usize;
        let c1 = has_zero_byte(w1 ^ mask).count_ones() as usize;
        let c2 = has_zero_byte(w2 ^ mask).count_ones() as usize;
        let c3 = has_zero_byte(w3 ^ mask).count_ones() as usize;
        count += c0 + c1 + c2 + c3;
        offset += 32;
    }
    while offset + 8 <= len {
        let word = u64::from_le_bytes(haystack[offset..offset + 8].try_into().unwrap());
        count += has_zero_byte(word ^ mask).count_ones() as usize;
        offset += 8;
    }
    for &b in &haystack[offset..] {
        if b == needle {
            count += 1;
        }
    }
    count
}

#[inline(always)]
fn verify_rare_word(
    haystack: &[u8],
    needle: &[u8],
    pos: usize,
    h1: u64,
    off2: usize,
    mask2: u64,
) -> Option<usize> {
    let w2 = u64::from_le_bytes(haystack[pos + off2..pos + off2 + 8].try_into().unwrap());
    let mut hit = h1 & has_zero_byte(w2 ^ mask2);
    let nlen = needle.len();
    while hit != 0 {
        let lane = (hit.trailing_zeros() >> 3) as usize;
        let cand = pos + lane;
        if nlen <= 2 || &haystack[cand..cand + nlen] == needle {
            return Some(cand);
        }
        hit &= hit - 1;
    }
    None
}

/// Fast 32-byte-unrolled literal substring search using a precomputed rarest-byte pair.
#[inline]
pub fn find_subslice_rare(haystack: &[u8], needle: &[u8], pair: RarePair) -> Option<usize> {
    let nlen = needle.len();
    if nlen == 0 {
        return Some(0);
    }
    if nlen > haystack.len() {
        return None;
    }
    if nlen == 1 {
        return find_byte(haystack, needle[0]);
    }
    let off1 = pair.off1;
    let off2 = pair.off2;
    let b1 = pair.b1;
    let b2 = pair.b2;
    let mask1 = repeat_byte(b1);
    let mask2 = repeat_byte(b2);
    let limit = haystack.len() - nlen;

    let mut pos = 0usize;
    while pos + 32 <= limit + 1 {
        let base1 = pos + off1;
        let w0 = u64::from_le_bytes(haystack[base1..base1 + 8].try_into().unwrap());
        let w1 = u64::from_le_bytes(haystack[base1 + 8..base1 + 16].try_into().unwrap());
        let w2 = u64::from_le_bytes(haystack[base1 + 16..base1 + 24].try_into().unwrap());
        let w3 = u64::from_le_bytes(haystack[base1 + 24..base1 + 32].try_into().unwrap());
        let h0 = has_zero_byte(w0 ^ mask1);
        let h1 = has_zero_byte(w1 ^ mask1);
        let h2 = has_zero_byte(w2 ^ mask1);
        let h3 = has_zero_byte(w3 ^ mask1);
        if (h0 | h1 | h2 | h3) != 0 {
            if h0 != 0 {
                if let Some(m) = verify_rare_word(haystack, needle, pos, h0, off2, mask2) {
                    return Some(m);
                }
            }
            if h1 != 0 {
                if let Some(m) = verify_rare_word(haystack, needle, pos + 8, h1, off2, mask2) {
                    return Some(m);
                }
            }
            if h2 != 0 {
                if let Some(m) = verify_rare_word(haystack, needle, pos + 16, h2, off2, mask2) {
                    return Some(m);
                }
            }
            if h3 != 0 {
                if let Some(m) = verify_rare_word(haystack, needle, pos + 24, h3, off2, mask2) {
                    return Some(m);
                }
            }
        }
        pos += 32;
    }
    while pos + 8 <= limit + 1 {
        let w0 = u64::from_le_bytes(haystack[pos + off1..pos + off1 + 8].try_into().unwrap());
        let h0 = has_zero_byte(w0 ^ mask1);
        if h0 != 0 {
            if let Some(m) = verify_rare_word(haystack, needle, pos, h0, off2, mask2) {
                return Some(m);
            }
        }
        pos += 8;
    }
    while pos <= limit {
        if haystack[pos + off1] == b1
            && haystack[pos + off2] == b2
            && &haystack[pos..pos + nlen] == needle
        {
            return Some(pos);
        }
        pos += 1;
    }
    None
}

/// Fast literal substring search using rarest-byte two-byte SWAR pair prefiltering.
#[inline]
pub fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    if needle.len() == 1 {
        return find_byte(haystack, needle[0]);
    }
    find_subslice_rare(haystack, needle, RarePair::from_needle(needle))
}

#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn verify_rare_word_ci(
    haystack: &[u8],
    needle_lower: &[u8],
    pos: usize,
    h1: u64,
    off2: usize,
    case2: u64,
    mask2: u64,
) -> Option<usize> {
    let w2 = u64::from_le_bytes(haystack[pos + off2..pos + off2 + 8].try_into().unwrap());
    let mut hit = h1 & has_zero_byte((w2 | case2) ^ mask2);
    let nlen = needle_lower.len();
    while hit != 0 {
        let lane = (hit.trailing_zeros() >> 3) as usize;
        let cand = pos + lane;
        if nlen <= 2 || haystack[cand..cand + nlen].eq_ignore_ascii_case(needle_lower) {
            return Some(cand);
        }
        hit &= hit - 1;
    }
    None
}

/// Zero-allocation 32-byte-unrolled ASCII case-insensitive substring search using a precomputed
/// rarest-byte pair (`needle_lower` and `pair` must already be ASCII-lowercased).
#[inline]
pub fn find_subslice_ascii_ci_rare(
    haystack: &[u8],
    needle_lower: &[u8],
    pair: RarePair,
) -> Option<usize> {
    let nlen = needle_lower.len();
    if nlen == 0 {
        return Some(0);
    }
    if nlen > haystack.len() {
        return None;
    }
    if nlen == 1 {
        return find_byte_ascii_ci(haystack, needle_lower[0]);
    }
    let off1 = pair.off1;
    let off2 = pair.off2;
    let b1 = pair.b1;
    let b2 = pair.b2;
    let case1 = case_mask_for(b1);
    let case2 = case_mask_for(b2);
    let mask1 = repeat_byte(b1);
    let mask2 = repeat_byte(b2);
    let limit = haystack.len() - nlen;

    let mut pos = 0usize;
    while pos + 32 <= limit + 1 {
        let base1 = pos + off1;
        let w0 = u64::from_le_bytes(haystack[base1..base1 + 8].try_into().unwrap());
        let w1 = u64::from_le_bytes(haystack[base1 + 8..base1 + 16].try_into().unwrap());
        let w2 = u64::from_le_bytes(haystack[base1 + 16..base1 + 24].try_into().unwrap());
        let w3 = u64::from_le_bytes(haystack[base1 + 24..base1 + 32].try_into().unwrap());
        let h0 = has_zero_byte((w0 | case1) ^ mask1);
        let h1 = has_zero_byte((w1 | case1) ^ mask1);
        let h2 = has_zero_byte((w2 | case1) ^ mask1);
        let h3 = has_zero_byte((w3 | case1) ^ mask1);
        if (h0 | h1 | h2 | h3) != 0 {
            if h0 != 0 {
                if let Some(m) =
                    verify_rare_word_ci(haystack, needle_lower, pos, h0, off2, case2, mask2)
                {
                    return Some(m);
                }
            }
            if h1 != 0 {
                if let Some(m) =
                    verify_rare_word_ci(haystack, needle_lower, pos + 8, h1, off2, case2, mask2)
                {
                    return Some(m);
                }
            }
            if h2 != 0 {
                if let Some(m) =
                    verify_rare_word_ci(haystack, needle_lower, pos + 16, h2, off2, case2, mask2)
                {
                    return Some(m);
                }
            }
            if h3 != 0 {
                if let Some(m) =
                    verify_rare_word_ci(haystack, needle_lower, pos + 24, h3, off2, case2, mask2)
                {
                    return Some(m);
                }
            }
        }
        pos += 32;
    }
    while pos + 8 <= limit + 1 {
        let w0 = u64::from_le_bytes(haystack[pos + off1..pos + off1 + 8].try_into().unwrap());
        let h0 = has_zero_byte((w0 | case1) ^ mask1);
        if h0 != 0 {
            if let Some(m) =
                verify_rare_word_ci(haystack, needle_lower, pos, h0, off2, case2, mask2)
            {
                return Some(m);
            }
        }
        pos += 8;
    }
    while pos <= limit {
        if haystack[pos + off1].to_ascii_lowercase() == b1
            && haystack[pos + off2].to_ascii_lowercase() == b2
            && haystack[pos..pos + nlen].eq_ignore_ascii_case(needle_lower)
        {
            return Some(pos);
        }
        pos += 1;
    }
    None
}

/// Zero-allocation ASCII case-insensitive substring search using rarest-byte SWAR pair prefiltering.
/// `needle_lower` must already be ASCII-lowercased.
#[inline]
pub fn find_subslice_ascii_ci(haystack: &[u8], needle_lower: &[u8]) -> Option<usize> {
    if needle_lower.is_empty() {
        return Some(0);
    }
    if needle_lower.len() == 1 {
        return find_byte_ascii_ci(haystack, needle_lower[0]);
    }
    find_subslice_ascii_ci_rare(haystack, needle_lower, RarePair::from_needle(needle_lower))
}
