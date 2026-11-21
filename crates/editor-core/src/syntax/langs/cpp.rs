//! C++ language syntax plugin (`cpp`, `cc`, `cxx`, `hpp`, `hh`, `h`, `mm`).
//!
//! Supports C++23 keywords, `#include <...>` / preprocessor directives, multi-line
//! `/* */` comments, C++ raw string literals `R"delim(...)delim"`, `'` digit separators,
//! escape sequences, primitive & common `std` types, and function highlighting.

use crate::syntax::cursor::{
    is_ident_start, is_operator_byte, is_pascal_case, is_punct_byte, is_screaming_const,
    push_span, ByteCursor,
};
use crate::syntax::{LanguageMeta, LanguageRegistry, LexState, SyntaxPlugin, TokenKind, TokenSpan};

pub struct CppPlugin;
pub static PLUGIN: CppPlugin = CppPlugin;

const META: LanguageMeta = LanguageMeta {
    id: "cpp",
    name: "C++",
    aliases: &["c++", "cc", "cxx", "hpp"],
    extensions: &["cpp", "cc", "cxx", "hpp", "hh", "hxx", "h", "ipp", "tpp", "mm"],
    filenames: &[],
    line_comment: Some("//"),
    block_comment: Some(("/*", "*/")),
    default_word_wrap: false,
};

const MODE_BLOCK_COMMENT: u8 = 1;
const MODE_RAW_STRING: u8 = 2;

impl SyntaxPlugin for CppPlugin {
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
                let rem = cur.scan_block_comment(1, false, out);
                if rem > 0 {
                    return LexState::new(MODE_BLOCK_COMMENT, 0, 1, 0);
                }
            }
            MODE_RAW_STRING => {
                let dhash = state.detail();
                if scan_cpp_raw_body(&mut cur, dhash, out) {
                    return LexState::new(MODE_RAW_STRING, 0, 0, dhash);
                }
            }
            _ => {}
        }

        // Preprocessor directives (`#include <vector>`, `#define FOO`, `#pragma once`).
        if cur.pos == 0 {
            let mut p = 0;
            while p < cur.len() && (cur.bytes[p] == b' ' || cur.bytes[p] == b'\t') {
                p += 1;
            }
            if p < cur.len() && cur.bytes[p] == b'#' {
                cur.pos = p;
                let s = cur.pos;
                cur.advance(1);
                cur.skip_ws();
                let (_, _, dir) = cur.scan_ident();
                push_span(out, s, cur.pos, TokenKind::Macro);
                if dir == "include" {
                    cur.skip_ws();
                    if cur.peek() == Some(b'<') {
                        let hs = cur.pos;
                        while !cur.is_eof() && cur.peek() != Some(b'>') {
                            cur.bump_char();
                        }
                        if cur.peek() == Some(b'>') {
                            cur.advance(1);
                        }
                        push_span(out, hs, cur.pos, TokenKind::String);
                    }
                }
            }
        }

        while !cur.is_eof() {
            let b = cur.bytes[cur.pos];
            if b == b' ' || b == b'\t' {
                cur.pos += 1;
            } else if b == b'/' && cur.peek_at(1) == Some(b'/') {
                push_span(out, cur.pos, cur.len(), TokenKind::Comment);
                return LexState::NORMAL;
            } else if b == b'/' && cur.peek_at(1) == Some(b'*') {
                let rem = cur.scan_block_comment(0, false, out);
                if rem > 0 {
                    return LexState::new(MODE_BLOCK_COMMENT, 0, 1, 0);
                }
            } else if let Some((open_len, dhash)) = detect_cpp_raw_string(&cur) {
                let s = cur.pos;
                cur.advance(open_len);
                push_span(out, s, cur.pos, TokenKind::String);
                if scan_cpp_raw_body(&mut cur, dhash, out) {
                    return LexState::new(MODE_RAW_STRING, 0, 0, dhash);
                }
            } else if b == b'"' || b == b'\'' {
                cur.scan_quoted_string(b, true, out);
            } else if b.is_ascii_digit() {
                cur.scan_number(true, out);
            } else if is_ident_start(b) {
                let (s, e, word) = cur.scan_ident();
                if is_keyword(word) {
                    push_span(out, s, e, TokenKind::Keyword);
                } else if is_constant(word) {
                    push_span(out, s, e, TokenKind::Constant);
                } else if is_type(word) {
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

fn delim_tag(delim: &[u8]) -> u8 {
    let mut h = 0x5Au8;
    for &b in delim {
        h = h.wrapping_mul(31).wrapping_add(b);
    }
    h
}

fn detect_cpp_raw_string(cur: &ByteCursor<'_>) -> Option<(usize, u8)> {
    let rest = &cur.bytes[cur.pos..];
    let prefix = if rest.starts_with(b"u8R\"") {
        4
    } else if rest.starts_with(b"uR\"") || rest.starts_with(b"UR\"") || rest.starts_with(b"LR\"") {
        3
    } else if rest.starts_with(b"R\"") {
        2
    } else {
        return None;
    };
    let mut i = prefix;
    while i < rest.len() && i - prefix <= 16 {
        let b = rest[i];
        if b == b'(' {
            let tag = delim_tag(&rest[prefix..i]);
            return Some((i + 1, tag));
        }
        if b == b' ' || b == b'\t' || b == b'\\' || b == b')' {
            return None;
        }
        i += 1;
    }
    None
}

fn scan_cpp_raw_body(cur: &mut ByteCursor<'_>, dhash: u8, out: &mut Vec<TokenSpan>) -> bool {
    let start = cur.pos;
    let len = cur.len();
    while cur.pos < len {
        if cur.bytes[cur.pos] == b')' {
            let mut j = cur.pos + 1;
            while j < len && j - (cur.pos + 1) <= 16 && cur.bytes[j] != b'"' {
                j += 1;
            }
            if j < len && cur.bytes[j] == b'"' && delim_tag(&cur.bytes[cur.pos + 1..j]) == dhash {
                cur.pos = j + 1;
                push_span(out, start, cur.pos, TokenKind::String);
                return false;
            }
        }
        cur.bump_char();
    }
    push_span(out, start, cur.pos, TokenKind::String);
    true
}

fn is_keyword(w: &str) -> bool {
    matches!(
        w,
        "alignas" | "alignof" | "and" | "and_eq" | "asm" | "auto" | "bitand" | "bitor" | "break"
            | "case" | "catch" | "class" | "co_await" | "co_return" | "co_yield" | "compl"
            | "concept" | "const" | "consteval" | "constexpr" | "constinit" | "const_cast"
            | "continue" | "decltype" | "default" | "delete" | "do" | "dynamic_cast" | "else"
            | "enum" | "explicit" | "export" | "extern" | "final" | "for" | "friend" | "goto"
            | "if" | "import" | "inline" | "module" | "mutable" | "namespace" | "new" | "noexcept"
            | "not" | "not_eq" | "operator" | "or" | "or_eq" | "override" | "private" | "protected"
            | "public" | "register" | "reinterpret_cast" | "requires" | "return" | "sizeof"
            | "static" | "static_assert" | "static_cast" | "struct" | "switch" | "template"
            | "this" | "thread_local" | "throw" | "try" | "typedef" | "typeid" | "typename"
            | "union" | "using" | "virtual" | "volatile" | "while" | "xor" | "xor_eq"
    )
}

fn is_constant(w: &str) -> bool {
    matches!(w, "true" | "false" | "nullptr" | "NULL")
}

fn is_type(w: &str) -> bool {
    matches!(
        w,
        "void" | "bool" | "char" | "char8_t" | "char16_t" | "char32_t" | "wchar_t" | "short"
            | "int" | "long" | "signed" | "unsigned" | "float" | "double" | "size_t" | "ssize_t"
            | "ptrdiff_t" | "intptr_t" | "uintptr_t" | "int8_t" | "int16_t" | "int32_t" | "int64_t"
            | "uint8_t" | "uint16_t" | "uint32_t" | "uint64_t" | "string" | "string_view"
            | "vector" | "array" | "map" | "unordered_map" | "set" | "unordered_set" | "optional"
            | "variant" | "unique_ptr" | "shared_ptr" | "weak_ptr" | "span" | "pair" | "tuple"
            | "std"
    )
}
