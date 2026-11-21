//! Streaming literal search across a `PieceTree` without loading the entire file into RAM.

use crate::piece_tree::PieceTree;
use crate::swar::find_subslice;

const SEARCH_WINDOW_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchOptions {
    pub case_sensitive: bool,
    pub max_matches: usize,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            case_sensitive: true,
            max_matches: 10_000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchMatch {
    pub start: u64,
    pub end: u64,
}

/// Find all non-overlapping literal occurrences of `query` in `tree` (up to `options.max_matches`).
pub fn find_all_matches(
    tree: &mut PieceTree,
    query: &str,
    options: SearchOptions,
) -> Vec<SearchMatch> {
    let needle = query.as_bytes();
    if needle.is_empty() {
        return Vec::new();
    }
    let total_len = tree.len_bytes();
    let nlen = needle.len() as u64;
    if nlen > total_len {
        return Vec::new();
    }

    let needle_lower: Vec<u8> = if options.case_sensitive {
        Vec::new()
    } else {
        needle.iter().map(|b| b.to_ascii_lowercase()).collect()
    };

    let mut matches = Vec::new();
    let mut window = Vec::with_capacity(SEARCH_WINDOW_BYTES + needle.len());
    let mut lower_buf = Vec::new();
    let mut offset = 0u64;

    while offset < total_len && matches.len() < options.max_matches {
        window.clear();
        let read_len = ((total_len - offset) as usize)
            .min(SEARCH_WINDOW_BYTES + needle.len().saturating_sub(1));
        tree.read_range(offset, read_len, &mut window);
        if window.len() < needle.len() {
            break;
        }

        let search_buf: &[u8] = if options.case_sensitive {
            &window
        } else {
            lower_buf.clear();
            lower_buf.extend(window.iter().map(|b| b.to_ascii_lowercase()));
            &lower_buf
        };
        let target: &[u8] = if options.case_sensitive {
            needle
        } else {
            &needle_lower
        };

        let mut pos = 0usize;
        let mut last_match_end = 0usize;
        while pos + target.len() <= search_buf.len() && matches.len() < options.max_matches {
            if let Some(rel) = find_subslice(&search_buf[pos..], target) {
                let abs_start = offset + (pos + rel) as u64;
                let abs_end = abs_start + nlen;
                matches.push(SearchMatch {
                    start: abs_start,
                    end: abs_end,
                });
                pos = pos + rel + target.len();
                last_match_end = pos;
            } else {
                break;
            }
        }

        if offset + window.len() as u64 >= total_len {
            break;
        }
        // Advance while keeping `needle.len() - 1` bytes of overlap for cross-boundary matches,
        // without re-matching already matched bytes!
        let overlap = needle.len().saturating_sub(1);
        let safe_step = window.len().saturating_sub(overlap).max(last_match_end).max(1);
        offset += safe_step as u64;
    }

    matches
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
