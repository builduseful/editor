//! Python language syntax plugin (`py`, `pyi`, `pyw`).
//!
//! Supports triple-quoted multi-line strings (`"""` / `'''`), string prefixes (`f`/`r`/`b`/`u`),
//! `@decorators`, `#` comments, escape sequences, Python 3.12 keywords, builtins, and functions.

use crate::syntax::cursor::{
    is_ident_start, is_operator_byte, is_pascal_case, is_punct_byte, is_screaming_const,
    push_span, ByteCursor,
};
use crate::syntax::{LanguageMeta, LanguageRegistry, LexState, SyntaxPlugin, TokenKind, TokenSpan};

pub struct PythonPlugin;
pub static PLUGIN: PythonPlugin = PythonPlugin;

const META: LanguageMeta = LanguageMeta {
    id: "python",
    name: "Python",
    aliases: &["py", "python3"],
    extensions: &["py", "pyi", "pyw"],
    filenames: &["sconstruct", "sconscript"],
    line_comment: Some("#"),
    block_comment: None,
    default_word_wrap: false,
};

const MODE_TRIPLE_DOUBLE: u8 = 1;
const MODE_TRIPLE_SINGLE: u8 = 2;

impl SyntaxPlugin for PythonPlugin {
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
            MODE_TRIPLE_DOUBLE => {
                if cur.scan_triple_string(b'"', true, true, out) {
                    return LexState::new(MODE_TRIPLE_DOUBLE, 0, 0, 0);
                }
            }
            MODE_TRIPLE_SINGLE => {
                if cur.scan_triple_string(b'\'', true, true, out) {
                    return LexState::new(MODE_TRIPLE_SINGLE, 0, 0, 0);
                }
            }
            _ => {}
        }

        while !cur.is_eof() {
            let b = cur.bytes[cur.pos];
            if b == b' ' || b == b'\t' {
                cur.pos += 1;
            } else if b == b'#' {
                push_span(out, cur.pos, cur.len(), TokenKind::Comment);
                return LexState::NORMAL;
            } else if b == b'@' && cur.peek_at(1).is_some_and(is_ident_start) {
                let s = cur.pos;
                cur.advance(1);
                while !cur.is_eof()
                    && (cur.bytes[cur.pos].is_ascii_alphanumeric()
                        || cur.bytes[cur.pos] == b'_'
                        || cur.bytes[cur.pos] == b'.')
                {
                    cur.pos += 1;
                }
                push_span(out, s, cur.pos, TokenKind::Attribute);
            } else if let Some((plen, q, triple, is_raw)) = detect_py_string(&cur) {
                let s = cur.pos;
                cur.advance(plen);
                if cur.pos > s {
                    push_span(out, s, cur.pos, TokenKind::String);
                }
                if triple {
                    if cur.scan_triple_string(q, false, !is_raw, out) {
                        return LexState::new(
                            if q == b'"' {
                                MODE_TRIPLE_DOUBLE
                            } else {
                                MODE_TRIPLE_SINGLE
                            },
                            0,
                            0,
                            0,
                        );
                    }
                } else {
                    cur.scan_quoted_string(q, !is_raw, out);
                }
            } else if b.is_ascii_digit() {
                cur.scan_number(false, out);
            } else if is_ident_start(b) {
                let (s, e, word) = cur.scan_ident();
                if is_keyword(word) {
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

fn detect_py_string(cur: &ByteCursor<'_>) -> Option<(usize, u8, bool, bool)> {
    let bytes = &cur.bytes[cur.pos..];
    let mut i = 0;
    let mut is_raw = false;
    while i < 2 && i < bytes.len() {
        match bytes[i] {
            b'r' | b'R' => {
                is_raw = true;
                i += 1;
            }
            b'f' | b'F' | b'b' | b'B' | b'u' | b'U' => {
                i += 1;
            }
            _ => break,
        }
    }
    if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
        let q = bytes[i];
        let triple = i + 2 < bytes.len() && bytes[i + 1] == q && bytes[i + 2] == q;
        Some((i, q, triple, is_raw))
    } else {
        None
    }
}

fn is_keyword(w: &str) -> bool {
    matches!(
        w,
        "and" | "as" | "assert" | "async" | "await" | "break" | "case" | "class" | "continue"
            | "def" | "del" | "elif" | "else" | "except" | "finally" | "for" | "from" | "global"
            | "if" | "import" | "in" | "is" | "lambda" | "match" | "nonlocal" | "not" | "or"
            | "pass" | "raise" | "return" | "try" | "type" | "while" | "with" | "yield"
    )
}

fn is_constant(w: &str) -> bool {
    matches!(w, "True" | "False" | "None" | "Ellipsis" | "NotImplemented")
}

fn is_builtin_type(w: &str) -> bool {
    matches!(
        w,
        "int" | "float" | "str" | "bool" | "bytes" | "bytearray" | "list" | "dict" | "set"
            | "frozenset" | "tuple" | "object" | "range" | "slice" | "complex" | "Any"
            | "Optional" | "Union" | "Callable" | "Sequence" | "Mapping" | "self" | "cls"
    )
}
