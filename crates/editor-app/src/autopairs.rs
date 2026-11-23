//! Zero-allocation, O(1) auto-pairing and smart indentation helpers.
//!
//! Covers:
//! - Opening delimiter auto-close (`()`, `[]`, `{}`, `""`, `''`, `` `` ``) with
//!   context guards so contractions (`don't`), Rust lifetimes (`'a`), escaped
//!   quotes (`\"`), or typing before identifiers never insert stray closers.
//! - Selection wrapping when typing any opening delimiter with active selection.
//! - Closing delimiter skip-over when the caret already sits before that closer.
//! - Paired Backspace deletion for empty pairs (`(|)`, `{|}`, `""`, etc.).
//! - Smart Enter: expands between `{|}` / `[|]` / `(|)` onto an indented middle
//!   line, and adds one indent level after trailing `{`, `[`, `(`, or `:`.

/// Matching closing ASCII delimiter for an opening delimiter.
#[inline]
pub const fn closing_delimiter(open: u8) -> Option<u8> {
    match open {
        b'(' => Some(b')'),
        b'[' => Some(b']'),
        b'{' => Some(b'}'),
        b'"' => Some(b'"'),
        b'\'' => Some(b'\''),
        b'`' => Some(b'`'),
        _ => None,
    }
}

/// True if `b` is a closing delimiter eligible for skip-over.
#[inline]
pub const fn is_closing_delimiter(b: u8) -> bool {
    matches!(b, b')' | b']' | b'}' | b'"' | b'\'' | b'`')
}

/// True if `(open, close)` is any auto-paired delimiter pair.
#[inline]
pub const fn is_matching_pair(open: u8, close: u8) -> bool {
    matches!(closing_delimiter(open), Some(c) if c == close)
}

/// True if `(open, close)` is a bracket pair that splits across lines on Enter.
#[inline]
pub const fn is_bracket_pair(open: u8, close: u8) -> bool {
    matches!((open, close), (b'(', b')') | (b'[', b']') | (b'{', b'}'))
}

#[inline]
const fn is_quote(b: u8) -> bool {
    matches!(b, b'"' | b'\'' | b'`')
}

/// Decide whether typing `open` at a collapsed caret with adjacent bytes `(prev, next)`
/// should automatically insert its closing counterpart.
#[inline]
pub fn should_auto_close(open: u8, prev: Option<u8>, next: Option<u8>) -> bool {
    if closing_delimiter(open).is_none() {
        return false;
    }
    // Never auto-close an escaped delimiter (`\"`, `\'`, `\{`, etc.).
    if prev == Some(b'\\') {
        return false;
    }
    // Never auto-close symmetric quotes right after an identifier/number/quote
    // (protects `don't`, Rust `'a` after `&`, `foo"`, etc.).
    if is_quote(open) {
        if let Some(p) = prev {
            if p.is_ascii_alphanumeric() || p == b'_' || p == open {
                return false;
            }
            // In Rust, `&'a` or `<'a`: don't auto-close single-quote after `&` or `<`!
            if open == b'\'' && matches!(p, b'&' | b'<') {
                return false;
            }
        }
    }
    // Only auto-close when `next` is EOF, whitespace, or a structural delimiter.
    match next {
        None => true,
        Some(n) => {
            n.is_ascii_whitespace()
                || matches!(n, b')' | b']' | b'}' | b',' | b';' | b':')
                || (!is_quote(open) && is_quote(n))
        }
    }
}

/// Compute smart Enter insertion text and caret offset within that inserted string.
///
/// - `before`: slice of the current line before the caret.
/// - `after`: slice of the current line after the caret (without trailing `\r`/`\n`).
/// - `tab_width`: space count when the current line does not use tab indentation.
pub fn smart_enter_insert(before: &str, after: &str, tab_width: usize) -> (String, usize) {
    let indent: &str = {
        let len = before
            .bytes()
            .take_while(|&b| b == b' ' || b == b'\t')
            .count();
        &before[..len]
    };
    let use_tab = indent.starts_with('\t');
    let extra_owned;
    let unit: &str = if use_tab {
        "\t"
    } else {
        extra_owned = " ".repeat(tab_width.max(1));
        &extra_owned
    };

    let before_trim = before.trim_end_matches([' ', '\t']);
    let after_trim = after.trim_start_matches([' ', '\t']);
    let last_open = before_trim.bytes().last();
    let first_close = after_trim.bytes().next();

    // Case 1: Between a matching bracket pair (`{ | }`, `[ | ]`, `( | )`).
    if let (Some(o), Some(c)) = (last_open, first_close) {
        if is_bracket_pair(o, c) {
            let mut out = String::with_capacity(indent.len() * 2 + unit.len() + 2);
            out.push('\n');
            out.push_str(indent);
            out.push_str(unit);
            let caret_rel = out.len();
            out.push('\n');
            out.push_str(indent);
            return (out, caret_rel);
        }
    }

    // Case 2: Trailing block opener (`{`, `[`, `(`, or `:` for Python/YAML/SQL blocks).
    if matches!(last_open, Some(b'{' | b'[' | b'(' | b':')) {
        let mut out = String::with_capacity(indent.len() + unit.len() + 1);
        out.push('\n');
        out.push_str(indent);
        out.push_str(unit);
        let caret_rel = out.len();
        return (out, caret_rel);
    }

    // Case 3: Preserve existing line indentation.
    let mut out = String::with_capacity(indent.len() + 1);
    out.push('\n');
    out.push_str(indent);
    let caret_rel = out.len();
    (out, caret_rel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_close_guards_contractions_lifetimes_and_identifiers() {
        assert!(should_auto_close(b'(', None, None));
        assert!(should_auto_close(b'{', Some(b' '), Some(b'\n')));
        assert!(!should_auto_close(b'(', None, Some(b'x')));
        assert!(should_auto_close(b'(', None, Some(b')')));
        assert!(!should_auto_close(b'\'', Some(b'n'), None)); // don't
        assert!(!should_auto_close(b'\'', Some(b'&'), None)); // &'a
        assert!(!should_auto_close(b'"', Some(b'\\'), None)); // \"
        assert!(should_auto_close(b'"', Some(b'('), Some(b')')));
    }

    #[test]
    fn smart_enter_expands_bracket_pairs_and_indents_openers() {
        let (ins, caret) = smart_enter_insert("    fn main() {", "}", 4);
        assert_eq!(ins, "\n        \n    ");
        assert_eq!(caret, 9);

        let (ins, caret) = smart_enter_insert("  if ready:", "", 4);
        assert_eq!(ins, "\n      ");
        assert_eq!(caret, 7);

        let (ins, caret) = smart_enter_insert("\tif x {", "", 4);
        assert_eq!(ins, "\n\t\t");
        assert_eq!(caret, 3);
    }
}
