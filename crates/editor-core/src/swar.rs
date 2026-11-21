//! Portable 64-bit SWAR (SIMD Within A Register) primitives in 100% safe Rust.
//!
//! Processes 8 bytes per loop iteration using bitmask arithmetic, giving multi-GB/s
//! newline counting, byte search, and ASCII validation across every CPU architecture
//! without unsafe intrinsics or external crates.

const LO_BITS: u64 = 0x0101_0101_0101_0101;
const HI_BITS: u64 = 0x8080_8080_8080_8080;

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

/// Fast 8-byte-at-a-time search for a single byte (`memchr` in pure safe Rust).
#[inline]
pub fn find_byte(haystack: &[u8], needle: u8) -> Option<usize> {
    let mask_needle = repeat_byte(needle);
    let mut offset = 0usize;
    let chunks = haystack.chunks_exact(8);
    let rem = chunks.remainder();
    for chunk in chunks {
        let word = u64::from_le_bytes(chunk.try_into().unwrap());
        let hit = has_zero_byte(word ^ mask_needle);
        if hit != 0 {
            let idx = (hit.trailing_zeros() >> 3) as usize;
            return Some(offset + idx);
        }
        offset += 8;
    }
    for (i, &b) in rem.iter().enumerate() {
        if b == needle {
            return Some(offset + i);
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

/// Fast literal substring search using SWAR first-byte scan + last-byte prefilter.
#[inline]
pub fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
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
    let first = needle[0];
    let last = needle[nlen - 1];
    let last_offset = nlen - 1;
    let limit = haystack.len() - nlen;
    let mut pos = 0usize;
    while pos <= limit {
        let rel = find_byte(&haystack[pos..=limit], first)?;
        let candidate = pos + rel;
        if haystack[candidate + last_offset] == last
            && haystack[candidate + 1..candidate + last_offset] == needle[1..last_offset]
        {
            return Some(candidate);
        }
        pos = candidate + 1;
    }
    None
}
