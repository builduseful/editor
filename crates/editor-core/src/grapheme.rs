//! Focused UTF-8, programming-language word boundary, and Emoji grapheme cluster engine.
//!
//! Optimized for English, source code, and modern emojis:
//! - ASCII fast path skips table logic completely (2 branches per byte).
//! - Handles CRLF (`\r\n`) as a single cluster.
//! - Handles Emoji subset cleanly: variation selectors (U+FE0E/FE0F), combining
//!   diacritics, Fitzpatrick skin-tone modifiers (U+1F3FB..=1F3FF), Regional Indicator
//!   flag pairs (U+1F1E6..=1F1FF), ZWJ chains (U+200D), and keycap sequences (U+20E3).

use crate::swar::is_all_ascii;

/// Validate UTF-8 with an 8-byte SWAR ASCII fast path.
/// Returns `Ok(())` or `Err(first_invalid_byte_offset)`.
pub fn validate_utf8_offset(bytes: &[u8]) -> Result<(), usize> {
    if is_all_ascii(bytes) {
        return Ok(());
    }
    match std::str::from_utf8(bytes) {
        Ok(_) => Ok(()),
        Err(e) => Err(e.valid_up_to()),
    }
}

#[inline]
fn is_combining_or_extend(ch: char) -> bool {
    let cp = ch as u32;
    matches!(
        cp,
        0x0300..=0x036F      // Combining Diacritical Marks
            | 0x1AB0..=0x1AFF // Combining Diacritical Marks Extended
            | 0x1DC0..=0x1DFF // Combining Diacritical Marks Supplement
            | 0x20D0..=0x20FF // Combining Diacritical Marks for Symbols (incl. Keycap U+20E3)
            | 0xFE00..=0xFE0F // Variation Selectors (VS1..VS16, incl. text/emoji VS15/VS16)
            | 0xFE20..=0xFE2F // Combining Half Marks
            | 0x1F3FB..=0x1F3FF // Emoji Fitzpatrick Skin Tone Modifiers
            | 0xE0020..=0xE007F // Tag characters (subdivision flags like 🏴󠁧󠁢󠁳󠁣󠁴󠁿)
    )
}

#[inline]
fn is_regional_indicator(ch: char) -> bool {
    (0x1F1E6..=0x1F1FF).contains(&(ch as u32))
}

#[inline]
pub fn is_emoji_codepoint(ch: char) -> bool {
    let cp = ch as u32;
    matches!(
        cp,
        0x1F300..=0x1F9FF
            | 0x1FA00..=0x1FAFF
            | 0x2600..=0x27BF
            | 0x2300..=0x23FF
            | 0x1F1E6..=0x1F1FF
            | 0x1F000..=0x1F02F
            | 0x1F0A0..=0x1F0FF
    )
}

/// Visual monospace cell width of a single grapheme cluster (1 for ASCII/narrow, 2 for emoji/wide).
#[inline]
pub fn cluster_cell_width(cluster: &str) -> u32 {
    let bytes = cluster.as_bytes();
    if bytes.is_empty() {
        return 0;
    }
    if bytes.len() == 1 && bytes[0] < 0x80 {
        return 1;
    }
    let first = match cluster.chars().next() {
        Some(c) => c,
        None => return 1,
    };
    if is_emoji_codepoint(first)
        || cluster.contains('\u{FE0F}')
        || cluster.contains('\u{20E3}')
        || cluster.contains('\u{200D}')
        || is_wide_cjk(first)
    {
        2
    } else {
        1
    }
}

#[inline]
fn is_wide_cjk(ch: char) -> bool {
    let cp = ch as u32;
    matches!(
        cp,
        0x1100..=0x115F
            | 0x2E80..=0xA4CF
            | 0xAC00..=0xD7A3
            | 0xF900..=0xFAFF
            | 0xFE10..=0xFE19
            | 0xFE30..=0xFE6F
            | 0xFF00..=0xFF60
            | 0xFFE0..=0xFFE6
            | 0x20000..=0x3FFFD
    )
}

/// Advance from `byte_offset` to the end of the next grapheme cluster in `s`.
#[inline]
pub fn next_grapheme_boundary(s: &str, byte_offset: usize) -> usize {
    let bytes = s.as_bytes();
    let len = bytes.len();
    if byte_offset >= len {
        return len;
    }

    // Ultra-fast ASCII path: single byte unless `\r\n` or followed by combining mark / keycap.
    let b0 = bytes[byte_offset];
    if b0 < 0x80 {
        if b0 == b'\r' && byte_offset + 1 < len && bytes[byte_offset + 1] == b'\n' {
            return byte_offset + 2;
        }
        if byte_offset + 1 == len || bytes[byte_offset + 1] < 0x80 {
            return byte_offset + 1;
        }
    }

    // Decode first codepoint starting at (or snapping to) a char boundary.
    let start = snap_down_char_boundary(s, byte_offset);
    let mut iter = s[start..].char_indices();
    let (_, first) = match iter.next() {
        Some(pair) => pair,
        None => return len,
    };
    let mut end = start + first.len_utf8();

    // Handle Regional Indicator flag pairs (2 regional indicators = 1 flag).
    if is_regional_indicator(first) {
        if let Some((rel, next_ch)) = iter.next() {
            if is_regional_indicator(next_ch) {
                end = start + rel + next_ch.len_utf8();
            }
        }
        return end;
    }

    // Consume combining marks, variation selectors, skin tones, and ZWJ emoji chains.
    let mut after_zwj = false;
    while end < len {
        let next_ch = match s[end..].chars().next() {
            Some(c) => c,
            None => break,
        };
        if after_zwj {
            // Attach the joined codepoint after U+200D.
            end += next_ch.len_utf8();
            after_zwj = false;
            continue;
        }
        if next_ch == '\u{200D}' {
            // Zero-width joiner: peek if there's another non-control char to join.
            let next_end = end + next_ch.len_utf8();
            if let Some(joined) = s[next_end..].chars().next() {
                if !joined.is_control() {
                    end = next_end;
                    after_zwj = true;
                    continue;
                }
            }
            break;
        }
        if is_combining_or_extend(next_ch) {
            end += next_ch.len_utf8();
            continue;
        }
        break;
    }

    end
}

/// Move backward from `byte_offset` to the start of the preceding grapheme cluster in `s`.
#[inline]
pub fn prev_grapheme_boundary(s: &str, byte_offset: usize) -> usize {
    let bytes = s.as_bytes();
    let target = byte_offset.min(bytes.len());
    if target == 0 {
        return 0;
    }

    // Fast ASCII path when the previous byte is plain ASCII and NOT preceded by ZWJ (U+200D).
    let prev_b = bytes[target - 1];
    let after_zwj = target >= 4 && bytes[target - 4..target - 1] == [0xE2, 0x80, 0x8D];
    if prev_b < 0x80 && !after_zwj {
        if prev_b == b'\n' && target >= 2 && bytes[target - 2] == b'\r' {
            return target - 2;
        }
        return target - 1;
    }

    // For non-ASCII clusters, scan forward from a small local anchor (up to 64 bytes back,
    // stopping at ASCII boundary) so ZWJ / Regional Indicator parity is 100% consistent
    // with `next_grapheme_boundary`.
    let mut anchor = target.saturating_sub(64);
    while anchor > 0 && !s.is_char_boundary(anchor) {
        anchor -= 1;
    }
    // Walk back further if we landed in the middle of a regional indicator run, combining run, or ZWJ chain.
    while anchor > 0 {
        let prev_start = snap_down_char_boundary(s, anchor - 1);
        let ch = s[prev_start..].chars().next().unwrap();
        anchor = prev_start;
        if ch.is_ascii() && ch != '\r' {
            break;
        }
    }

    let mut cur = anchor;
    let mut last_start = anchor;
    while cur < target {
        last_start = cur;
        let next = next_grapheme_boundary(s, cur);
        if next <= cur {
            break;
        }
        cur = next;
    }
    last_start
}

#[inline]
pub fn snap_down_char_boundary(s: &str, mut offset: usize) -> usize {
    offset = offset.min(s.len());
    while offset > 0 && !s.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// Iterator over `(byte_offset, cluster_str)` in `s`.
pub struct GraphemeIter<'a> {
    s: &'a str,
    pos: usize,
}

impl<'a> Iterator for GraphemeIter<'a> {
    type Item = (usize, &'a str);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.s.len() {
            return None;
        }
        let start = self.pos;
        let end = next_grapheme_boundary(self.s, start);
        self.pos = end;
        Some((start, &self.s[start..end]))
    }
}

#[inline]
pub fn grapheme_clusters(s: &str) -> GraphemeIter<'_> {
    GraphemeIter { s, pos: 0 }
}

// ============================================================================
// Word boundary navigation (for Option+Left/Right and double-click selection)
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharClass {
    Whitespace,
    Word,
    Punctuation,
}

#[inline]
fn classify_char(ch: char) -> CharClass {
    if ch.is_whitespace() {
        CharClass::Whitespace
    } else if ch.is_alphanumeric() || ch == '_' {
        CharClass::Word
    } else {
        CharClass::Punctuation
    }
}

/// Move forward by one word (macOS Option+Right convention: skip non-word, then consume word run).
pub fn next_word_boundary(line: &str, byte_offset: usize) -> usize {
    let mut pos = snap_down_char_boundary(line, byte_offset);
    let len = line.len();
    if pos >= len {
        return len;
    }
    // Skip leading whitespace.
    while pos < len {
        let ch = line[pos..].chars().next().unwrap();
        if classify_char(ch) != CharClass::Whitespace {
            break;
        }
        pos = next_grapheme_boundary(line, pos);
    }
    if pos >= len {
        return len;
    }
    // Consume one contiguous class (either Word or Punctuation).
    let target_class = classify_char(line[pos..].chars().next().unwrap());
    while pos < len {
        let ch = line[pos..].chars().next().unwrap();
        if classify_char(ch) != target_class {
            break;
        }
        pos = next_grapheme_boundary(line, pos);
    }
    pos
}

/// Move backward by one word (macOS Option+Left convention).
pub fn prev_word_boundary(line: &str, byte_offset: usize) -> usize {
    let mut pos = snap_down_char_boundary(line, byte_offset);
    if pos == 0 {
        return 0;
    }
    // Skip trailing whitespace.
    while pos > 0 {
        let prev = prev_grapheme_boundary(line, pos);
        let ch = line[prev..].chars().next().unwrap();
        if classify_char(ch) != CharClass::Whitespace {
            break;
        }
        pos = prev;
    }
    if pos == 0 {
        return 0;
    }
    let prev = prev_grapheme_boundary(line, pos);
    let target_class = classify_char(line[prev..].chars().next().unwrap());
    pos = prev;
    while pos > 0 {
        let p = prev_grapheme_boundary(line, pos);
        let ch = line[p..].chars().next().unwrap();
        if classify_char(ch) != target_class {
            break;
        }
        pos = p;
    }
    pos
}

/// Return `(start, end)` byte range of the word/token under `byte_offset` (for double-click).
pub fn word_range_at(line: &str, byte_offset: usize) -> (usize, usize) {
    if line.is_empty() {
        return (0, 0);
    }
    let mut pos = snap_down_char_boundary(line, byte_offset);
    if pos >= line.len() {
        pos = prev_grapheme_boundary(line, line.len());
    }
    let class = classify_char(line[pos..].chars().next().unwrap());
    let mut start = pos;
    while start > 0 {
        let prev = prev_grapheme_boundary(line, start);
        if classify_char(line[prev..].chars().next().unwrap()) != class {
            break;
        }
        start = prev;
    }
    let mut end = next_grapheme_boundary(line, pos);
    while end < line.len() {
        if classify_char(line[end..].chars().next().unwrap()) != class {
            break;
        }
        end = next_grapheme_boundary(line, end);
    }
    (start, end)
}
