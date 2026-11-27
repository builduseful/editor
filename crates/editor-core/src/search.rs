//! Unified streaming search engine (`CompiledQuery`) for both `PieceTree` documents and
//! in-memory / on-disk byte slices (used by Find/Replace and Project Search in Files).
//!
//! Performance architecture:
//! - Case-sensitive AND case-insensitive literal searches run **zero-copy** directly on raw
//!   bytes using two-byte SWAR pair prefiltering (`swar::find_subslice` / `find_subslice_ascii_ci`).
//! - Single-line regexes with a literal prefix use **ripgrep-style SWAR line prefiltering**:
//!   raw bytes are scanned at multi-GB/s for prefix hits, and UTF-8 validation + PikeVM run ONLY
//!   on the enclosing candidate line!
//! - All streaming scans over `PieceTree` stay bounded in `< 128 KB` scratch RAM even on 60+ MB files.

use crate::piece_tree::PieceTree;
use crate::regex::{is_word_char, PikeVmScratch, Regex, RegexError};
use crate::swar::{
    find_byte, find_subslice_ascii_ci_rare, find_subslice_rare, rfind_byte, RarePair,
};

const SEARCH_WINDOW_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchOptions {
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
    pub max_matches: usize,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            case_sensitive: true,
            whole_word: false,
            regex: false,
            max_matches: 10_000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchMatch {
    pub start: u64,
    pub end: u64,
}

/// Reusable scratch buffers for zero-allocation repeated slice/file searches.
#[derive(Default)]
pub struct SearchScratch {
    pike: PikeVmScratch,
}

impl SearchScratch {
    pub fn new() -> Self {
        Self::default()
    }
}

#[derive(Debug, Clone)]
struct LiteralMatcher {
    needle: Vec<u8>,
    rare: RarePair,
    case_sensitive: bool,
    whole_word: bool,
}

#[derive(Debug, Clone)]
enum QueryKind {
    Literal(LiteralMatcher),
    Regex(Regex),
}

/// A compiled search query shared by single-file Find/Replace and project-wide Search in Files.
#[derive(Debug, Clone)]
pub struct CompiledQuery {
    kind: QueryKind,
}

impl CompiledQuery {
    /// Compile `query` with `options`. Returns `Ok(None)` when `query` is empty.
    pub fn compile(query: &str, options: SearchOptions) -> Result<Option<Self>, RegexError> {
        if query.is_empty() {
            return Ok(None);
        }
        if options.regex {
            let re = Regex::compile(query, options.case_sensitive, options.whole_word)?;
            Ok(Some(Self {
                kind: QueryKind::Regex(re),
            }))
        } else {
            let needle = if options.case_sensitive {
                query.as_bytes().to_vec()
            } else {
                query.bytes().map(|b| b.to_ascii_lowercase()).collect()
            };
            let rare = RarePair::from_needle(&needle);
            Ok(Some(Self {
                kind: QueryKind::Literal(LiteralMatcher {
                    needle,
                    rare,
                    case_sensitive: options.case_sensitive,
                    whole_word: options.whole_word,
                }),
            }))
        }
    }

    /// True if this compiled query is a regex (so replacement templates expand `$0`..`$9`).
    #[inline]
    pub fn is_regex(&self) -> bool {
        matches!(self.kind, QueryKind::Regex(_))
    }

    /// Minimum byte overlap needed when streaming a file in fixed-size chunks.
    #[inline]
    pub fn stream_overlap_bytes(&self) -> usize {
        match &self.kind {
            QueryKind::Literal(lit) => lit.needle.len().saturating_sub(1),
            QueryKind::Regex(re) => {
                if re.can_match_newline() {
                    4096
                } else {
                    0
                }
            }
        }
    }

    /// True when this query can match across `\n` boundaries.
    #[inline]
    pub fn can_match_newline(&self) -> bool {
        match &self.kind {
            QueryKind::Literal(lit) => lit.needle.contains(&b'\n'),
            QueryKind::Regex(re) => re.can_match_newline(),
        }
    }

    /// Find non-overlapping matches inside a byte slice `bytes` (up to `max_matches`).
    /// Appends `(start, end)` byte ranges to `out`.
    pub fn find_in_bytes(
        &self,
        bytes: &[u8],
        max_matches: usize,
        scratch: &mut SearchScratch,
        out: &mut Vec<(usize, usize)>,
    ) {
        self.find_in_bytes_with_context(bytes, None, None, max_matches, scratch, out);
    }

    /// Context-aware variant for chunked streaming across large files (preserving `\b`, `^`, `$`).
    pub fn find_in_bytes_with_context(
        &self,
        bytes: &[u8],
        prev_before_slice: Option<char>,
        next_after_slice: Option<char>,
        max_matches: usize,
        scratch: &mut SearchScratch,
        out: &mut Vec<(usize, usize)>,
    ) {
        if bytes.is_empty() || max_matches == 0 {
            return;
        }
        match &self.kind {
            QueryKind::Literal(lit) => find_literal_in_bytes(
                bytes,
                lit,
                prev_before_slice,
                next_after_slice,
                max_matches,
                out,
            ),
            QueryKind::Regex(re) => find_regex_in_bytes(
                bytes,
                re,
                prev_before_slice,
                next_after_slice,
                max_matches,
                &mut scratch.pike,
                out,
            ),
        }
    }

    /// Expand a replacement template for a matched substring `matched_text`.
    /// Literal queries return `template` verbatim; regex queries expand `$0`..`$9`, `$$`, `\n`, `\t`.
    pub fn expand_replacement(
        &self,
        matched_text: &str,
        template: &str,
        scratch: &mut SearchScratch,
    ) -> String {
        self.expand_replacement_with_context(matched_text, None, None, template, scratch)
    }

    /// Expand a replacement template while preserving adjacent character context for `\b` / `\B` / `^` / `$`.
    pub fn expand_replacement_with_context(
        &self,
        matched_text: &str,
        prev_before_slice: Option<char>,
        next_after_slice: Option<char>,
        template: &str,
        scratch: &mut SearchScratch,
    ) -> String {
        match &self.kind {
            QueryKind::Literal(_) => template.to_string(),
            QueryKind::Regex(re) => {
                if let Some(caps) = re.find_captures_with_context(
                    matched_text,
                    0,
                    prev_before_slice,
                    next_after_slice,
                    &mut scratch.pike,
                ) {
                    caps.expand(matched_text, template)
                } else {
                    template.to_string()
                }
            }
        }
    }

    /// Stream all non-overlapping matches across `tree` without loading the whole file into RAM.
    pub fn find_all_in_tree(&self, tree: &mut PieceTree, max_matches: usize) -> Vec<SearchMatch> {
        if max_matches == 0 {
            return Vec::new();
        }
        match &self.kind {
            QueryKind::Literal(lit) => find_literal_in_tree(tree, lit, max_matches),
            QueryKind::Regex(re) => find_regex_in_tree(tree, re, max_matches),
        }
    }
}

/// Check UTF-8 whole-word boundary on both ends of `bytes[start..end]`.
#[inline]
fn check_whole_word(
    bytes: &[u8],
    start: usize,
    end: usize,
    prev_before_slice: Option<char>,
    next_after_slice: Option<char>,
) -> bool {
    let first_ch = next_char_in_bytes(&bytes[start..end]);
    let last_ch = prev_char_in_bytes(&bytes[start..end]);

    let before_ch = if start > 0 {
        prev_char_in_bytes(&bytes[..start])
    } else {
        prev_before_slice
    };
    let after_ch = if end < bytes.len() {
        next_char_in_bytes(&bytes[end..])
    } else {
        next_after_slice
    };

    let left_ok = match (before_ch, first_ch) {
        (Some(b), Some(f)) => !(is_word_char(b) && is_word_char(f)),
        _ => true,
    };
    let right_ok = match (last_ch, after_ch) {
        (Some(l), Some(a)) => !(is_word_char(l) && is_word_char(a)),
        _ => true,
    };
    left_ok && right_ok
}

#[inline]
pub fn prev_char_in_bytes(prefix: &[u8]) -> Option<char> {
    if prefix.is_empty() {
        return None;
    }
    let last = prefix[prefix.len() - 1];
    if last < 0x80 {
        return Some(last as char);
    }
    let mut start = prefix.len().saturating_sub(4);
    while start < prefix.len() && (prefix[start] & 0xC0) == 0x80 {
        start += 1;
    }
    std::str::from_utf8(&prefix[start..])
        .ok()
        .and_then(|s| s.chars().next_back())
}

#[inline]
pub fn next_char_in_bytes(suffix: &[u8]) -> Option<char> {
    if suffix.is_empty() {
        return None;
    }
    let first = suffix[0];
    if first < 0x80 {
        return Some(first as char);
    }
    let end = suffix.len().min(4);
    for len in (1..=end).rev() {
        if let Ok(s) = std::str::from_utf8(&suffix[..len]) {
            if let Some(c) = s.chars().next() {
                return Some(c);
            }
        }
    }
    None
}

/// Zero-copy literal match loop (case-sensitive OR ASCII case-insensitive via two-byte SWAR!).
fn find_literal_in_bytes(
    bytes: &[u8],
    lit: &LiteralMatcher,
    prev_before_slice: Option<char>,
    next_after_slice: Option<char>,
    max_matches: usize,
    out: &mut Vec<(usize, usize)>,
) {
    let nlen = lit.needle.len();
    if nlen == 0 || bytes.len() < nlen {
        return;
    }
    let mut pos = 0usize;
    while pos + nlen <= bytes.len() && out.len() < max_matches {
        let rel = if lit.case_sensitive {
            find_subslice_rare(&bytes[pos..], &lit.needle, lit.rare)
        } else {
            find_subslice_ascii_ci_rare(&bytes[pos..], &lit.needle, lit.rare)
        };
        let Some(rel) = rel else {
            break;
        };
        let start = pos + rel;
        let end = start + nlen;
        if !lit.whole_word || check_whole_word(bytes, start, end, prev_before_slice, next_after_slice)
        {
            out.push((start, end));
            pos = end;
        } else {
            pos = start + 1;
        }
    }
}

/// Fast regex matching over raw bytes:
/// - For single-line regexes WITH a literal prefix (the common 90%+ case like `fn\s+\w+`):
///   uses SWAR prefix scanning directly on raw `bytes` at multi-GB/s, validating UTF-8 and running
///   PikeVM ONLY on the single enclosing candidate line!
/// - For single-line regexes without a literal prefix: scans line-by-line cleanly (isolating any
///   stray invalid UTF-8 line and stopping immediately once `max_matches` is hit).
/// - For multiline regexes (`can_match_newline()`): runs directly over UTF-8 slice.
fn find_regex_in_bytes(
    bytes: &[u8],
    re: &Regex,
    prev_before_slice: Option<char>,
    next_after_slice: Option<char>,
    max_matches: usize,
    pike: &mut PikeVmScratch,
    out: &mut Vec<(usize, usize)>,
) {
    if !re.can_match_newline() {
        if re.has_line_prefilter() {
            // Ripgrep-style SWAR line prefilter using the longest mandatory literal or top-level
            // alternation literals: skips all non-candidate bytes at multi-GB/s!
            let mut scan = 0usize;
            while scan < bytes.len() && out.len() < max_matches {
                let Some(cand) = re.find_line_candidate(bytes, scan) else {
                    break;
                };
                // Locate enclosing line `[line_start..line_end]` via SWAR `rfind_byte`/`find_byte`.
                let line_start = rfind_byte(&bytes[..cand], b'\n').map(|p| p + 1).unwrap_or(0);
                let line_end = find_byte(&bytes[cand..], b'\n')
                    .map(|d| cand + d)
                    .unwrap_or(bytes.len());
                let prev_ch = if line_start == 0 {
                    prev_before_slice
                } else {
                    Some('\n')
                };
                let next_ch = if line_end < bytes.len() {
                    Some('\n')
                } else {
                    next_after_slice
                };
                if let Ok(line_str) = std::str::from_utf8(&bytes[line_start..line_end]) {
                    let mut lpos = 0usize;
                    while lpos <= line_str.len() && out.len() < max_matches {
                        let Some((s, e)) =
                            re.find_span_with_context(line_str, lpos, prev_ch, next_ch, pike)
                        else {
                            break;
                        };
                        out.push((line_start + s, line_start + e));
                        if e > s {
                            lpos = e;
                        } else if e >= line_str.len() {
                            break;
                        } else {
                            let ch = line_str[e..].chars().next().unwrap();
                            lpos = e + ch.len_utf8();
                        }
                    }
                }
                scan = (line_end + 1).min(bytes.len());
            }
            return;
        }

        // Single-line regex without literal prefix: scan line-by-line (early exit on `max_matches`).
        let mut line_start = 0usize;
        while line_start <= bytes.len() && out.len() < max_matches {
            let line_end = find_byte(&bytes[line_start..], b'\n')
                .map(|d| line_start + d)
                .unwrap_or(bytes.len());
            let prev_ch = if line_start == 0 {
                prev_before_slice
            } else {
                Some('\n')
            };
            let next_ch = if line_end < bytes.len() {
                Some('\n')
            } else {
                next_after_slice
            };
            if let Ok(line_str) = std::str::from_utf8(&bytes[line_start..line_end]) {
                let mut lpos = 0usize;
                while lpos <= line_str.len() && out.len() < max_matches {
                    let Some((s, e)) =
                        re.find_span_with_context(line_str, lpos, prev_ch, next_ch, pike)
                    else {
                        break;
                    };
                    out.push((line_start + s, line_start + e));
                    if e > s {
                        lpos = e;
                    } else if e >= line_str.len() {
                        break;
                    } else {
                        let ch = line_str[e..].chars().next().unwrap();
                        lpos = e + ch.len_utf8();
                    }
                }
            }
            if line_end >= bytes.len() {
                break;
            }
            line_start = line_end + 1;
        }
        return;
    }

    // Multiline regex (e.g. patterns containing `\s` or `\n`):
    // First reject non-matching files/chunks in O(N) via SWAR prefilter!
    if re.has_line_prefilter() && re.find_line_candidate(bytes, 0).is_none() {
        return;
    }
    let Ok(text) = std::str::from_utf8(bytes) else {
        return;
    };
    find_regex_in_str(text, re, prev_before_slice, next_after_slice, max_matches, pike, out);
}

fn find_regex_in_str(
    text: &str,
    re: &Regex,
    prev_before_slice: Option<char>,
    next_after_slice: Option<char>,
    max_matches: usize,
    pike: &mut PikeVmScratch,
    out: &mut Vec<(usize, usize)>,
) {
    let mut pos = 0usize;
    while pos <= text.len() && out.len() < max_matches {
        let Some((s, e)) =
            re.find_span_with_context(text, pos, prev_before_slice, next_after_slice, pike)
        else {
            break;
        };
        out.push((s, e));
        if e > s {
            pos = e;
        } else {
            // Zero-length match: step by one full UTF-8 char to guarantee forward progress!
            if e >= text.len() {
                break;
            }
            let ch = text[e..].chars().next().unwrap();
            pos = e + ch.len_utf8();
        }
    }
}

fn find_literal_in_tree(
    tree: &mut PieceTree,
    lit: &LiteralMatcher,
    max_matches: usize,
) -> Vec<SearchMatch> {
    let total_len = tree.len_bytes();
    let nlen = lit.needle.len() as u64;
    if nlen == 0 || nlen > total_len {
        return Vec::new();
    }

    let overlap = lit.needle.len().saturating_sub(1);
    let buf_cap = SEARCH_WINDOW_BYTES + overlap;
    let mut matches = Vec::new();
    let mut window = vec![0u8; buf_cap];
    let mut ctx_buf = [0u8; 4];
    let mut slice_hits = Vec::new();
    let mut offset = 0u64;

    while offset < total_len && matches.len() < max_matches {
        let read_len = ((total_len - offset) as usize).min(buf_cap);
        let got = tree.read_into_slice(offset, &mut window[..read_len]);
        if got < lit.needle.len() {
            break;
        }
        let raw_win_end = offset + got as u64;
        let is_eof = raw_win_end >= total_len;
        let usable = if lit.whole_word && !is_eof {
            let floored = utf8_floor(&window[..got]);
            if floored >= lit.needle.len() {
                floored
            } else {
                got
            }
        } else {
            got
        };
        let win = &window[..usable];

        let prev_ch = if lit.whole_word && offset > 0 {
            let back = offset.min(4) as usize;
            let n = tree.read_into_slice(offset - back as u64, &mut ctx_buf[..back]);
            prev_char_in_bytes(&ctx_buf[..n])
        } else {
            None
        };
        let win_end = offset + usable as u64;
        let next_ch = if lit.whole_word && win_end < total_len {
            let fwd = (total_len - win_end).min(4) as usize;
            let n = tree.read_into_slice(win_end, &mut ctx_buf[..fwd]);
            next_char_in_bytes(&ctx_buf[..n])
        } else {
            None
        };

        slice_hits.clear();
        find_literal_in_bytes(
            win,
            lit,
            prev_ch,
            next_ch,
            max_matches - matches.len(),
            &mut slice_hits,
        );

        let mut last_match_end = 0usize;
        for &(s, e) in &slice_hits {
            matches.push(SearchMatch {
                start: offset + s as u64,
                end: offset + e as u64,
            });
            last_match_end = e;
        }

        if win_end >= total_len {
            break;
        }
        let mut safe_step = usable.saturating_sub(overlap).max(last_match_end).max(1);
        if lit.whole_word && safe_step < usable {
            let snapped = utf8_floor(&win[..safe_step]);
            if snapped > 0 {
                safe_step = snapped;
            }
        }
        offset += safe_step as u64;
    }

    matches
}

/// Find the longest valid UTF-8 prefix length of `bytes` (trimming only an incomplete trailing char).
#[inline]
pub fn utf8_floor(bytes: &[u8]) -> usize {
    let len = bytes.len();
    if len == 0 {
        return 0;
    }
    // Walk back at most 3 continuation bytes to locate the start byte of the final UTF-8 scalar.
    let mut start = len - 1;
    let limit = len.saturating_sub(4);
    while start > limit && (bytes[start] & 0xC0) == 0x80 {
        start -= 1;
    }
    match std::str::from_utf8(&bytes[start..]) {
        Ok(_) => len,
        Err(_) => start,
    }
}

fn find_regex_in_tree(
    tree: &mut PieceTree,
    re: &Regex,
    max_matches: usize,
) -> Vec<SearchMatch> {
    let total_len = tree.len_bytes();
    let mut matches = Vec::new();
    let mut pike = PikeVmScratch::new();
    let mut window = vec![0u8; SEARCH_WINDOW_BYTES];
    let mut ctx_buf = [0u8; 4];
    let mut slice_hits = Vec::new();

    if total_len == 0 {
        find_regex_in_str("", re, None, None, max_matches, &mut pike, &mut slice_hits);
        for (s, e) in slice_hits {
            matches.push(SearchMatch {
                start: s as u64,
                end: e as u64,
            });
        }
        return matches;
    }

    let mut offset = 0u64;
    let multiline = re.can_match_newline();

    while offset < total_len && matches.len() < max_matches {
        let read_len = ((total_len - offset) as usize).min(SEARCH_WINDOW_BYTES);
        let got = tree.read_into_slice(offset, &mut window[..read_len]);
        if got == 0 {
            break;
        }
        let win = &window[..got];

        let is_eof = offset + got as u64 >= total_len;
        let mut usable = if is_eof { got } else { utf8_floor(win) };
        if !is_eof && !multiline {
            if let Some(last_nl) = rfind_byte(&win[..usable], b'\n') {
                usable = last_nl + 1;
            }
        }
        if usable == 0 {
            usable = utf8_floor(win).max(1);
        }

        let prev_ch = if offset > 0 {
            let back = offset.min(4) as usize;
            let n = tree.read_into_slice(offset - back as u64, &mut ctx_buf[..back]);
            prev_char_in_bytes(&ctx_buf[..n])
        } else {
            None
        };
        let chunk_end = offset + usable as u64;
        let next_ch = if chunk_end < total_len {
            let fwd = (total_len - chunk_end).min(4) as usize;
            let n = tree.read_into_slice(chunk_end, &mut ctx_buf[..fwd]);
            next_char_in_bytes(&ctx_buf[..n])
        } else {
            None
        };

        slice_hits.clear();
        find_regex_in_bytes(
            &win[..usable],
            re,
            prev_ch,
            next_ch,
            max_matches - matches.len(),
            &mut pike,
            &mut slice_hits,
        );

        let mut last_match_end = 0usize;
        for &(s, e) in &slice_hits {
            let abs_s = offset + s as u64;
            let abs_e = offset + e as u64;
            if !is_eof && s == usable && e == usable {
                continue;
            }
            if matches.last().is_some_and(|prev| prev.start == abs_s && prev.end == abs_e) {
                continue;
            }
            matches.push(SearchMatch {
                start: abs_s,
                end: abs_e,
            });
            last_match_end = e;
        }

        if chunk_end >= total_len {
            break;
        }
        if multiline {
            let target_step = usable.saturating_sub(4096).max(last_match_end).max(1);
            let mut step = target_step.min(usable);
            while step < usable && (win[step] & 0xC0) == 0x80 {
                step += 1;
            }
            offset += step as u64;
        } else {
            offset += usable as u64;
        }
    }

    matches
}

/// Find all non-overlapping matches of `query` in `tree` (returns empty on invalid regex).
pub fn find_all_matches(
    tree: &mut PieceTree,
    query: &str,
    options: SearchOptions,
) -> Vec<SearchMatch> {
    match find_all_matches_checked(tree, query, options) {
        Ok(m) => m,
        Err(_) => Vec::new(),
    }
}

/// Checked variant returning [`RegexError`] when `options.regex` is enabled and `query` is invalid.
pub fn find_all_matches_checked(
    tree: &mut PieceTree,
    query: &str,
    options: SearchOptions,
) -> Result<Vec<SearchMatch>, RegexError> {
    let Some(compiled) = CompiledQuery::compile(query, options)? else {
        return Ok(Vec::new());
    };
    Ok(compiled.find_all_in_tree(tree, options.max_matches))
}

/// Find the next occurrence of `query` at or after `from_byte`, wrapping around if `wrap` is true.
pub fn find_next_match(
    tree: &mut PieceTree,
    query: &str,
    from_byte: u64,
    options: SearchOptions,
    wrap: bool,
) -> Option<SearchMatch> {
    let matches = find_all_matches(tree, query, options);
    if matches.is_empty() {
        return None;
    }
    if let Some(m) = matches.iter().find(|m| m.start >= from_byte) {
        return Some(*m);
    }
    if wrap {
        matches.first().copied()
    } else {
        None
    }
}
