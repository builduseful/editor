//! PHP language syntax plugin (`php`, `phtml`).
//!
//! Supports `<?php`/`?>` tags, `//`, `#`, and `/* */` comments, `#[Attributes]`,
//! `$variables`, strings with escapes, PHP 8 keywords (`fn`, `match`, `readonly`, `enum`),
//! primitive types, and functions.

use crate::syntax::cursor::{
    is_ident_start, is_operator_byte, is_pascal_case, is_punct_byte, is_screaming_const,
    push_span, ByteCursor,
};
use crate::syntax::{LanguageMeta, LanguageRegistry, LexState, SyntaxPlugin, TokenKind, TokenSpan};

pub struct PhpPlugin;
pub static PLUGIN: PhpPlugin = PhpPlugin;

const META: LanguageMeta = LanguageMeta {
    id: "php",
    name: "PHP",
    aliases: &["php8", "phtml"],
    extensions: &["php", "phtml", "php8"],
    filenames: &[],
    line_comment: Some("//"),
    block_comment: Some(("/*", "*/")),
    default_word_wrap: false,
};

const MODE_BLOCK_COMMENT: u8 = 1;

impl SyntaxPlugin for PhpPlugin {
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

        if state.mode() == MODE_BLOCK_COMMENT {
            let rem = cur.scan_block_comment(1, false, out);
            if rem > 0 {
                return LexState::new(MODE_BLOCK_COMMENT, 0, 1, 0);
            }
        }

        while !cur.is_eof() {
            let b = cur.bytes[cur.pos];
            if b == b' ' || b == b'\t' {
                cur.pos += 1;
            } else if cur.starts_with(b"<?php") || cur.starts_with(b"<?=") {
                let n = if cur.starts_with(b"<?php") { 5 } else { 3 };
                push_span(out, cur.pos, cur.pos + n, TokenKind::Tag);
                cur.advance(n);
            } else if cur.starts_with(b"?>") {
                push_span(out, cur.pos, cur.pos + 2, TokenKind::Tag);
                cur.advance(2);
            } else if b == b'/' && cur.peek_at(1) == Some(b'/') {
                push_span(out, cur.pos, cur.len(), TokenKind::Comment);
                return LexState::NORMAL;
            } else if b == b'/' && cur.peek_at(1) == Some(b'*') {
                let rem = cur.scan_block_comment(0, false, out);
                if rem > 0 {
                    return LexState::new(MODE_BLOCK_COMMENT, 0, 1, 0);
                }
            } else if b == b'#' && cur.peek_at(1) == Some(b'[') {
                let s = cur.pos;
                while !cur.is_eof() && cur.peek() != Some(b']') {
                    cur.bump_char();
                }
                if cur.peek() == Some(b']') {
                    cur.advance(1);
                }
                push_span(out, s, cur.pos, TokenKind::Macro);
            } else if b == b'#' {
                push_span(out, cur.pos, cur.len(), TokenKind::Comment);
                return LexState::NORMAL;
            } else if b == b'"' || b == b'\'' {
                cur.scan_quoted_string(b, true, out);
            } else if b == b'$' && cur.peek_at(1).is_some_and(is_ident_start) {
                let s = cur.pos;
                cur.advance(1);
                cur.scan_ident();
                push_span(out, s, cur.pos, TokenKind::Property);
            } else if b.is_ascii_digit() {
                cur.scan_number(false, out);
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
                push_span(out, cur.pos, cur.pos + 1, TokenKind::Operator);
                cur.advance(1);
            } else if is_punct_byte(b) {
                push_span(out, cur.pos, cur.pos + 1, TokenKind::Punctuation);
                cur.advance(1);
            } else {
                cur.bump_char();
            }
        }

        LexState::NORMAL
    }
}

fn is_keyword(w: &str) -> bool {
    matches!(
        w,
        "abstract" | "and" | "as" | "break" | "callable" | "case" | "catch" | "class" | "clone"
            | "const" | "continue" | "declare" | "default" | "do" | "echo" | "else" | "elseif"
            | "empty" | "enddeclare" | "endfor" | "endforeach" | "endif" | "endswitch" | "endwhile"
            | "enum" | "extends" | "final" | "finally" | "fn" | "for" | "foreach" | "function"
            | "global" | "goto" | "if" | "implements" | "include" | "include_once" | "instanceof"
            | "insteadof" | "interface" | "isset" | "list" | "match" | "namespace" | "new" | "or"
            | "print" | "private" | "protected" | "public" | "readonly" | "require"
            | "require_once" | "return" | "static" | "switch" | "throw" | "trait" | "try" | "unset"
            | "use" | "var" | "while" | "xor" | "yield"
    )
}

fn is_constant(w: &str) -> bool {
    matches!(
        w,
        "true" | "false" | "null" | "TRUE" | "FALSE" | "NULL" | "__FILE__" | "__LINE__"
            | "__DIR__" | "__CLASS__" | "__FUNCTION__"
    )
}

fn is_type(w: &str) -> bool {
    matches!(
        w,
        "int" | "float" | "string" | "bool" | "array" | "object" | "mixed" | "void" | "never"
            | "iterable" | "self" | "parent"
    )
}
