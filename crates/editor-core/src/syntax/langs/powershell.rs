//! PowerShell language syntax plugin (`ps1`, `psm1`, `psd1`).
//!
//! Supports `#` line comments, `<# ... #>` multi-line block comments, `$variables`,
//! `[Type]` brackets, `-Parameters` / `-eq` / `-match` operators, `Verb-Noun` cmdlets,
//! backtick escapes, and PowerShell keywords.

use crate::syntax::cursor::{is_ident_start, push_span, ByteCursor};
use crate::syntax::{LanguageMeta, LanguageRegistry, LexState, SyntaxPlugin, TokenKind, TokenSpan};

pub struct PowerShellPlugin;
pub static PLUGIN: PowerShellPlugin = PowerShellPlugin;

const META: LanguageMeta = LanguageMeta {
    id: "powershell",
    name: "PowerShell",
    aliases: &["ps1", "pwsh", "ps"],
    extensions: &["ps1", "psm1", "psd1"],
    filenames: &[],
    line_comment: Some("#"),
    block_comment: Some(("<#", "#>")),
    default_word_wrap: false,
};

const MODE_BLOCK_COMMENT: u8 = 1;

impl SyntaxPlugin for PowerShellPlugin {
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
            if scan_ps_block_comment(&mut cur, true, out) {
                return LexState::new(MODE_BLOCK_COMMENT, 0, 0, 0);
            }
        }

        while !cur.is_eof() {
            let b = cur.bytes[cur.pos];
            if b == b' ' || b == b'\t' {
                cur.pos += 1;
            } else if cur.starts_with(b"<#") {
                if scan_ps_block_comment(&mut cur, false, out) {
                    return LexState::new(MODE_BLOCK_COMMENT, 0, 0, 0);
                }
            } else if b == b'#' {
                push_span(out, cur.pos, cur.len(), TokenKind::Comment);
                return LexState::NORMAL;
            } else if b == b'"' {
                scan_ps_double_string(&mut cur, out);
            } else if b == b'\'' {
                cur.scan_quoted_string(b'\'', false, out);
            } else if b == b'$' {
                let s = cur.pos;
                cur.advance(1);
                if cur.peek() == Some(b'{') {
                    while !cur.is_eof() && cur.peek() != Some(b'}') {
                        cur.bump_char();
                    }
                    if cur.peek() == Some(b'}') {
                        cur.advance(1);
                    }
                } else {
                    while !cur.is_eof()
                        && (cur.bytes[cur.pos].is_ascii_alphanumeric()
                            || cur.bytes[cur.pos] == b'_'
                            || cur.bytes[cur.pos] == b':')
                    {
                        cur.pos += 1;
                    }
                }
                let word = &cur.src[s..cur.pos];
                let kind = if word.eq_ignore_ascii_case("$true")
                    || word.eq_ignore_ascii_case("$false")
                    || word.eq_ignore_ascii_case("$null")
                {
                    TokenKind::Constant
                } else {
                    TokenKind::Property
                };
                push_span(out, s, cur.pos, kind);
            } else if b == b'-' && cur.peek_at(1).is_some_and(|c| c.is_ascii_alphabetic()) {
                let s = cur.pos;
                cur.advance(1);
                while !cur.is_eof()
                    && (cur.bytes[cur.pos].is_ascii_alphanumeric()
                        || cur.bytes[cur.pos] == b'_')
                {
                    cur.pos += 1;
                }
                let word = &cur.src[s + 1..cur.pos];
                let kind = if is_ps_operator(word) {
                    TokenKind::Operator
                } else {
                    TokenKind::Attribute
                };
                push_span(out, s, cur.pos, kind);
            } else if b == b'[' && cur.peek_at(1).is_some_and(|c| c.is_ascii_alphabetic()) {
                let s = cur.pos;
                cur.advance(1);
                while !cur.is_eof()
                    && (cur.bytes[cur.pos].is_ascii_alphanumeric()
                        || matches!(cur.bytes[cur.pos], b'.' | b'_' | b'[' | b']'))
                {
                    if cur.bytes[cur.pos] == b']' && cur.peek_at(1) != Some(b'[') {
                        cur.advance(1);
                        break;
                    }
                    cur.pos += 1;
                }
                push_span(out, s, cur.pos, TokenKind::Type);
            } else if b.is_ascii_digit() {
                cur.scan_number(false, out);
            } else if is_ident_start(b) {
                let s = cur.pos;
                let mut has_dash = false;
                while !cur.is_eof()
                    && (cur.bytes[cur.pos].is_ascii_alphanumeric()
                        || cur.bytes[cur.pos] == b'_'
                        || cur.bytes[cur.pos] == b'-')
                {
                    if cur.bytes[cur.pos] == b'-' {
                        has_dash = true;
                    }
                    cur.pos += 1;
                }
                let e = cur.pos;
                let word = &cur.src[s..e];
                if is_ps_keyword(word) {
                    push_span(out, s, e, TokenKind::Keyword);
                } else if has_dash || cur.followed_by_paren() {
                    push_span(out, s, e, TokenKind::Function);
                }
            } else if matches!(b, b'|' | b'=' | b'+' | b'*' | b'/' | b'%' | b'!' | b'>' | b'<') {
                push_span(out, cur.pos, cur.pos + 1, TokenKind::Operator);
                cur.advance(1);
            } else if matches!(b, b'(' | b')' | b'{' | b'}' | b'[' | b']' | b';' | b',' | b'.') {
                push_span(out, cur.pos, cur.pos + 1, TokenKind::Punctuation);
                cur.advance(1);
            } else {
                cur.bump_char();
            }
        }

        LexState::NORMAL
    }
}

fn scan_ps_block_comment(cur: &mut ByteCursor<'_>, continuing: bool, out: &mut Vec<TokenSpan>) -> bool {
    let start = cur.pos;
    if !continuing {
        cur.advance(2);
    }
    while cur.pos + 1 < cur.len() {
        if cur.starts_with(b"#>") {
            cur.advance(2);
            push_span(out, start, cur.pos, TokenKind::Comment);
            return false;
        }
        cur.bump_char();
    }
    cur.pos = cur.len();
    push_span(out, start, cur.pos, TokenKind::Comment);
    true
}

fn scan_ps_double_string(cur: &mut ByteCursor<'_>, out: &mut Vec<TokenSpan>) {
    let mut seg_start = cur.pos;
    cur.advance(1);
    while !cur.is_eof() {
        let b = cur.bytes[cur.pos];
        if b == b'`' && cur.pos + 1 < cur.len() {
            push_span(out, seg_start, cur.pos, TokenKind::String);
            let es = cur.pos;
            cur.advance(2);
            push_span(out, es, cur.pos, TokenKind::Escape);
            seg_start = cur.pos;
        } else if b == b'"' {
            cur.advance(1);
            push_span(out, seg_start, cur.pos, TokenKind::String);
            return;
        } else {
            cur.bump_char();
        }
    }
    push_span(out, seg_start, cur.pos, TokenKind::String);
}

fn is_ps_keyword(w: &str) -> bool {
    const KWS: &[&str] = &[
        "begin", "break", "catch", "class", "continue", "data", "do", "dynamicparam", "else",
        "elseif", "end", "enum", "exit", "filter", "finally", "for", "foreach", "from", "function",
        "hidden", "if", "in", "param", "process", "return", "static", "switch", "throw", "trap",
        "try", "until", "using", "var", "while", "workflow",
    ];
    KWS.iter().any(|&k| k.eq_ignore_ascii_case(w))
}

fn is_ps_operator(w: &str) -> bool {
    const OPS: &[&str] = &[
        "eq", "ne", "gt", "ge", "lt", "le", "like", "notlike", "match", "notmatch", "contains",
        "notcontains", "in", "notin", "replace", "split", "join", "is", "isnot", "as", "and", "or",
        "xor", "not", "band", "bor", "bxor", "f",
    ];
    OPS.iter().any(|&o| o.eq_ignore_ascii_case(w))
}
