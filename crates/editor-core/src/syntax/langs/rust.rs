//! Rust language syntax plugin (`rs`).
//!
//! Supports nested `/* /* */ */` block comments, multi-line raw strings `r###"..."###`,
//! byte/c-strings, lifetimes `'a`, attributes `#[...]` / `#![...]`, macros `foo!`,
//! escape sequences, primitive/std types, and function-call highlighting.

use crate::syntax::cursor::{
    is_ident_start, is_operator_byte, is_pascal_case, is_punct_byte, is_screaming_const,
    push_span, ByteCursor,
};
use crate::syntax::{LanguageMeta, LanguageRegistry, LexState, SyntaxPlugin, TokenKind, TokenSpan};

pub struct RustPlugin;
pub static PLUGIN: RustPlugin = RustPlugin;

const META: LanguageMeta = LanguageMeta {
    id: "rust",
    name: "Rust",
    aliases: &["rs"],
    extensions: &["rs"],
    filenames: &[],
    line_comment: Some("//"),
    block_comment: Some(("/*", "*/")),
    default_word_wrap: false,
};

const MODE_BLOCK_COMMENT: u8 = 1;
const MODE_RAW_STRING: u8 = 2;

impl SyntaxPlugin for RustPlugin {
    fn meta(&self) -> &'static LanguageMeta {
        &META
    }

    fn lex_line(
        &self,
        line: &str,
        state: LexState,
        _registry: &LanguageRegistry,
        out: &mut Vec<TokenSpan>,
    ) -> LexState {
        let mut cur = ByteCursor::new(line);

        match state.mode() {
            MODE_BLOCK_COMMENT => {
                let rem = cur.scan_block_comment(state.depth().max(1), true, out);
                if rem > 0 {
                    return LexState::new(MODE_BLOCK_COMMENT, 0, rem, 0);
                }
            }
            MODE_RAW_STRING => {
                let hashes = state.depth();
                if scan_raw_string_body(&mut cur, hashes, out) {
                    return LexState::new(MODE_RAW_STRING, 0, hashes, 0);
                }
            }
            _ => {}
        }

        while !cur.is_eof() {
            let b = cur.bytes[cur.pos];
            if b == b' ' || b == b'\t' {
                cur.pos += 1;
            } else if b == b'/' && cur.peek_at(1) == Some(b'/') {
                push_span(out, cur.pos, cur.len(), TokenKind::Comment);
                return LexState::NORMAL;
            } else if b == b'/' && cur.peek_at(1) == Some(b'*') {
                let rem = cur.scan_block_comment(0, true, out);
                if rem > 0 {
                    return LexState::new(MODE_BLOCK_COMMENT, 0, rem, 0);
                }
            } else if b == b'#'
                && (cur.peek_at(1) == Some(b'[')
                    || (cur.peek_at(1) == Some(b'!') && cur.peek_at(2) == Some(b'[')))
            {
                let start = cur.pos;
                let mut depth = 0i32;
                while !cur.is_eof() {
                    match cur.bytes[cur.pos] {
                        b'[' => depth += 1,
                        b']' => {
                            depth -= 1;
                            cur.pos += 1;
                            if depth <= 0 {
                                break;
                            }
                            continue;
                        }
                        b'"' => {
                            push_span(out, start, cur.pos, TokenKind::Macro);
                            cur.scan_quoted_string(b'"', true, out);
                            // Remaining attribute tail highlighted cleanly by next iterations.
                            break;
                        }
                        _ => {}
                    }
                    cur.bump_char();
                }
                if cur.pos > start && (out.last().is_none_or(|t| (t.end as usize) <= start)) {
                    push_span(out, start, cur.pos, TokenKind::Macro);
                }
            } else if let Some((prefix_len, hashes)) = detect_raw_string(&cur) {
                let start = cur.pos;
                cur.advance(prefix_len);
                push_span(out, start, cur.pos, TokenKind::String);
                if scan_raw_string_body(&mut cur, hashes, out) {
                    return LexState::new(MODE_RAW_STRING, 0, hashes, 0);
                }
            } else if (b == b'b' || b == b'c') && cur.peek_at(1) == Some(b'"') {
                let s = cur.pos;
                cur.advance(1);
                push_span(out, s, cur.pos, TokenKind::String);
                cur.scan_quoted_string(b'"', true, out);
            } else if b == b'"' {
                cur.scan_quoted_string(b'"', true, out);
            } else if b == b'\'' {
                scan_char_or_lifetime(&mut cur, out);
            } else if b.is_ascii_digit() {
                cur.scan_number(false, out);
            } else if is_ident_start(b) {
                let (s, e, word) = cur.scan_ident();
                if cur.peek() == Some(b'!') && cur.peek_at(1) != Some(b'=') {
                    cur.advance(1);
                    push_span(out, s, cur.pos, TokenKind::Macro);
                } else if is_keyword(word) {
                    push_span(out, s, e, TokenKind::Keyword);
                } else if is_constant(word) {
                    push_span(out, s, e, TokenKind::Constant);
                } else if is_builtin_type(word) {
                    push_span(out, s, e, TokenKind::Type);
                } else if cur.followed_by_paren() {
                    push_span(out, s, e, TokenKind::Function);
                } else if is_pascal_case(word) {
                    push_span(out, s, e, TokenKind::Type);
                } else if is_screaming_const(word) {
                    push_span(out, s, e, TokenKind::Constant);
                }
            } else if is_operator_byte(b) {
                let s = cur.pos;
                cur.advance(1);
                push_span(out, s, cur.pos, TokenKind::Operator);
            } else if is_punct_byte(b) {
                let s = cur.pos;
                cur.advance(1);
                push_span(out, s, cur.pos, TokenKind::Punctuation);
            } else {
                cur.bump_char();
            }
        }

        LexState::NORMAL
    }
}

/// Detect `r"..."`, `r#"..."#`, `br#"..."#`, `cr#"..."#`. Returns `(opening_len, hash_count)`.
fn detect_raw_string(cur: &ByteCursor<'_>) -> Option<(usize, u8)> {
    let bytes = &cur.bytes[cur.pos..];
    let mut i = if bytes.starts_with(b"br") || bytes.starts_with(b"cr") {
        2
    } else if bytes.starts_with(b"r") {
        1
    } else {
        return None;
    };
    let mut hashes = 0u8;
    while i < bytes.len() && bytes[i] == b'#' && hashes < 250 {
        i += 1;
        hashes += 1;
    }
    if i < bytes.len() && bytes[i] == b'"' {
        Some((i + 1, hashes))
    } else {
        None
    }
}

/// Scan inside a Rust raw string until `"` followed by `hashes` `#` chars. Returns `true` if still open.
fn scan_raw_string_body(cur: &mut ByteCursor<'_>, hashes: u8, out: &mut Vec<TokenSpan>) -> bool {
    let start = cur.pos;
    let len = cur.len();
    let h = hashes as usize;
    while cur.pos < len {
        if cur.bytes[cur.pos] == b'"' && cur.pos + 1 + h <= len {
            if cur.bytes[cur.pos + 1..cur.pos + 1 + h]
                .iter()
                .all(|&c| c == b'#')
            {
                cur.pos += 1 + h;
                push_span(out, start, cur.pos, TokenKind::String);
                return false;
            }
        }
        cur.bump_char();
    }
    push_span(out, start, cur.pos, TokenKind::String);
    true
}

fn scan_char_or_lifetime(cur: &mut ByteCursor<'_>, out: &mut Vec<TokenSpan>) {
    let start = cur.pos;
    let len = cur.len();
    if start + 1 >= len {
        cur.advance(1);
        push_span(out, start, cur.pos, TokenKind::Punctuation);
        return;
    }
    // Escaped char literal: `'\n'`, `'\''`, `'\x7f'`, `'\u{1F600}'`.
    if cur.bytes[start + 1] == b'\\' {
        cur.scan_quoted_string(b'\'', true, out);
        return;
    }
    // Single-char literal: `'a'`, `'🚀'`.
    let mut probe = start + 1;
    if let Some(ch) = cur.src[probe..].chars().next() {
        probe += ch.len_utf8();
        if probe < len && cur.bytes[probe] == b'\'' {
            cur.pos = probe + 1;
            push_span(out, start, cur.pos, TokenKind::String);
            return;
        }
    }
    // Otherwise it's a Rust lifetime or loop label (`'a`, `'static`, `'_`).
    cur.advance(1);
    if cur.peek().is_some_and(|b| is_ident_start(b)) {
        cur.scan_ident();
        push_span(out, start, cur.pos, TokenKind::Attribute);
    } else {
        push_span(out, start, cur.pos, TokenKind::Punctuation);
    }
}

fn is_keyword(w: &str) -> bool {
    matches!(
        w,
        "as" | "async" | "await" | "break" | "const" | "continue" | "crate" | "dyn" | "else"
            | "enum" | "extern" | "fn" | "for" | "if" | "impl" | "in" | "let" | "loop" | "match"
            | "mod" | "move" | "mut" | "pub" | "ref" | "return" | "self" | "static" | "struct"
            | "super" | "trait" | "type" | "unsafe" | "use" | "where" | "while" | "yield"
    )
}

fn is_constant(w: &str) -> bool {
    matches!(w, "true" | "false" | "None" | "Some" | "Ok" | "Err")
}

fn is_builtin_type(w: &str) -> bool {
    matches!(
        w,
        "u8" | "u16" | "u32" | "u64" | "u128" | "usize" | "i8" | "i16" | "i32" | "i64" | "i128"
            | "isize" | "f32" | "f64" | "bool" | "char" | "str" | "Self" | "Option" | "Result"
            | "Vec" | "String" | "Box" | "Arc" | "Rc" | "Pin" | "Cow" | "HashMap" | "BTreeMap"
            | "HashSet" | "BTreeSet" | "Path" | "PathBuf"
    )
}
