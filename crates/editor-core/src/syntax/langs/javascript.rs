//! JavaScript / JSX language syntax plugin (`js`, `jsx`, `mjs`, `cjs`),
//! plus shared scanner parameterized for TypeScript / TSX (`typescript.rs`).
//!
//! Supports multi-line template literals `` `...${expr}...` `` with `${}` brace depth tracking,
//! `/* */` comments, regex literals `/pattern/flags`, JSX tags, decorators `@foo`,
//! escape sequences, numbers (`0x`/`0b`/`1_000n`), builtins, and functions.

use crate::syntax::cursor::{
    is_ident_start, is_operator_byte, is_pascal_case, is_punct_byte, is_screaming_const,
    push_span, ByteCursor,
};
use crate::syntax::{LanguageMeta, LanguageRegistry, LexState, SyntaxPlugin, TokenKind, TokenSpan};

pub struct JavaScriptPlugin;
pub static PLUGIN: JavaScriptPlugin = JavaScriptPlugin;

const META: LanguageMeta = LanguageMeta {
    id: "javascript",
    name: "JavaScript",
    aliases: &["js", "jsx", "mjs", "cjs", "node"],
    extensions: &["js", "jsx", "mjs", "cjs"],
    filenames: &[],
    line_comment: Some("//"),
    block_comment: Some(("/*", "*/")),
    default_word_wrap: false,
};

const MODE_BLOCK_COMMENT: u8 = 1;
const MODE_TEMPLATE: u8 = 2;

impl SyntaxPlugin for JavaScriptPlugin {
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
        lex_js_ts(line, state, false, out)
    }
}

pub(crate) fn lex_js_ts(
    line: &str,
    state: LexState,
    is_ts: bool,
    out: &mut Vec<TokenSpan>,
) -> LexState {
    let mut cur = ByteCursor::new(line);
    // `template_brace_depth > 0` means we are inside `${ ... }` inside a template literal!
    let mut template_brace_depth: u8 = 0;

    match state.mode() {
        MODE_BLOCK_COMMENT => {
            let rem = cur.scan_block_comment(1, false, out);
            if rem > 0 {
                return LexState::new(MODE_BLOCK_COMMENT, 0, 1, 0);
            }
        }
        MODE_TEMPLATE => {
            if state.depth() == 0 {
                match scan_template_segment(&mut cur, out) {
                    TemplateStop::Eol => return LexState::new(MODE_TEMPLATE, 0, 0, 0),
                    TemplateStop::Closed => {}
                    TemplateStop::InterpStart => {
                        template_brace_depth = 1;
                    }
                }
            } else {
                template_brace_depth = state.depth();
            }
        }
        _ => {}
    }

    let mut allow_regex = true;

    while !cur.is_eof() {
        let b = cur.bytes[cur.pos];
        if b == b' ' || b == b'\t' {
            cur.pos += 1;
            continue;
        }
        if b == b'/' && cur.peek_at(1) == Some(b'/') {
            push_span(out, cur.pos, cur.len(), TokenKind::Comment);
            break;
        }
        if b == b'/' && cur.peek_at(1) == Some(b'*') {
            let rem = cur.scan_block_comment(0, false, out);
            if rem > 0 {
                return LexState::new(MODE_BLOCK_COMMENT, 0, 1, 0);
            }
            continue;
        }
        if b == b'`' {
            let s = cur.pos;
            cur.advance(1);
            push_span(out, s, cur.pos, TokenKind::String);
            match scan_template_segment(&mut cur, out) {
                TemplateStop::Eol => return LexState::new(MODE_TEMPLATE, 0, 0, 0),
                TemplateStop::Closed => {
                    allow_regex = false;
                }
                TemplateStop::InterpStart => {
                    template_brace_depth = template_brace_depth.saturating_add(1);
                    allow_regex = true;
                }
            }
            continue;
        }
        if b == b'{' && template_brace_depth > 0 {
            template_brace_depth = template_brace_depth.saturating_add(1);
            push_span(out, cur.pos, cur.pos + 1, TokenKind::Punctuation);
            cur.advance(1);
            allow_regex = true;
            continue;
        }
        if b == b'}' && template_brace_depth > 0 {
            template_brace_depth -= 1;
            push_span(out, cur.pos, cur.pos + 1, TokenKind::Punctuation);
            cur.advance(1);
            if template_brace_depth == 0 {
                match scan_template_segment(&mut cur, out) {
                    TemplateStop::Eol => return LexState::new(MODE_TEMPLATE, 0, 0, 0),
                    TemplateStop::Closed => {
                        allow_regex = false;
                    }
                    TemplateStop::InterpStart => {
                        template_brace_depth = 1;
                        allow_regex = true;
                    }
                }
            } else {
                allow_regex = false;
            }
            continue;
        }
        if b == b'"' || b == b'\'' {
            cur.scan_quoted_string(b, true, out);
            allow_regex = false;
            continue;
        }
        // Regex literal heuristic (`/pattern/gi`) when `/` follows an operator/delimiter/keyword.
        if b == b'/' && allow_regex && looks_like_regex(&cur) {
            scan_regex_literal(&mut cur, out);
            allow_regex = false;
            continue;
        }
        // JSX/TSX opening or closing tag (`<Component`, `</div>`) when `<` follows delimiter/return.
        if b == b'<'
            && allow_regex
            && (cur.peek_at(1).is_some_and(|c| c.is_ascii_alphabetic() || c == b'/'))
        {
            let start = cur.pos;
            cur.advance(1);
            if cur.peek() == Some(b'/') {
                cur.advance(1);
            }
            let ts = cur.pos;
            while !cur.is_eof()
                && (cur.bytes[cur.pos].is_ascii_alphanumeric()
                    || cur.bytes[cur.pos] == b'-'
                    || cur.bytes[cur.pos] == b'.'
                    || cur.bytes[cur.pos] == b'_')
            {
                cur.pos += 1;
            }
            if cur.pos > ts {
                push_span(out, start, cur.pos, TokenKind::Tag);
                allow_regex = false;
                continue;
            } else {
                cur.pos = start;
            }
        }
        if b == b'@' && cur.peek_at(1).is_some_and(is_ident_start) {
            let s = cur.pos;
            cur.advance(1);
            cur.scan_ident();
            push_span(out, s, cur.pos, TokenKind::Macro);
            allow_regex = false;
            continue;
        }
        if b.is_ascii_digit() {
            cur.scan_number(false, out);
            allow_regex = false;
            continue;
        }
        if is_ident_start(b) || b == b'$' {
            let s = cur.pos;
            while !cur.is_eof()
                && (cur.bytes[cur.pos].is_ascii_alphanumeric()
                    || cur.bytes[cur.pos] == b'_'
                    || cur.bytes[cur.pos] == b'$')
            {
                cur.pos += 1;
            }
            let e = cur.pos;
            let word = &cur.src[s..e];
            if is_js_keyword(word) || (is_ts && is_ts_keyword(word)) {
                push_span(out, s, e, TokenKind::Keyword);
                allow_regex = matches!(
                    word,
                    "return" | "throw" | "case" | "typeof" | "in" | "of" | "await" | "yield" | "new"
                );
            } else if is_js_constant(word) {
                push_span(out, s, e, TokenKind::Constant);
                allow_regex = false;
            } else if is_js_builtin_type(word) || (is_ts && is_ts_type(word)) {
                push_span(out, s, e, TokenKind::Type);
                allow_regex = false;
            } else if cur.followed_by_paren() {
                push_span(out, s, e, TokenKind::Function);
                allow_regex = false;
            } else if is_pascal_case(word) {
                push_span(out, s, e, TokenKind::Type);
                allow_regex = false;
            } else if is_screaming_const(word) {
                push_span(out, s, e, TokenKind::Constant);
                allow_regex = false;
            } else {
                allow_regex = false;
            }
            continue;
        }
        if is_operator_byte(b) {
            let s = cur.pos;
            cur.advance(1);
            push_span(out, s, cur.pos, TokenKind::Operator);
            allow_regex = true;
        } else if is_punct_byte(b) {
            let s = cur.pos;
            cur.advance(1);
            push_span(out, s, cur.pos, TokenKind::Punctuation);
            allow_regex = matches!(b, b'(' | b'[' | b'{' | b',' | b';');
        } else {
            cur.bump_char();
        }
    }

    if template_brace_depth > 0 {
        LexState::new(MODE_TEMPLATE, 0, template_brace_depth, 0)
    } else {
        LexState::NORMAL
    }
}

enum TemplateStop {
    Closed,
    InterpStart,
    Eol,
}

fn scan_template_segment(cur: &mut ByteCursor<'_>, out: &mut Vec<TokenSpan>) -> TemplateStop {
    let mut seg_start = cur.pos;
    let len = cur.len();
    while cur.pos < len {
        let b = cur.bytes[cur.pos];
        if b == b'\\' && cur.pos + 1 < len {
            push_span(out, seg_start, cur.pos, TokenKind::String);
            let es = cur.pos;
            cur.advance(1);
            cur.bump_char();
            push_span(out, es, cur.pos, TokenKind::Escape);
            seg_start = cur.pos;
        } else if b == b'$' && cur.peek_at(1) == Some(b'{') {
            push_span(out, seg_start, cur.pos, TokenKind::String);
            let ps = cur.pos;
            cur.advance(2);
            push_span(out, ps, cur.pos, TokenKind::Punctuation);
            return TemplateStop::InterpStart;
        } else if b == b'`' {
            cur.advance(1);
            push_span(out, seg_start, cur.pos, TokenKind::String);
            return TemplateStop::Closed;
        } else {
            cur.bump_char();
        }
    }
    push_span(out, seg_start, cur.pos, TokenKind::String);
    TemplateStop::Eol
}

fn looks_like_regex(cur: &ByteCursor<'_>) -> bool {
    let bytes = &cur.bytes[cur.pos + 1..];
    if bytes.is_empty() || bytes[0] == b'/' || bytes[0] == b'*' {
        return false;
    }
    let mut i = 0;
    let mut in_class = false;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\\' && i + 1 < bytes.len() {
            i += 2;
        } else if b == b'[' {
            in_class = true;
            i += 1;
        } else if b == b']' {
            in_class = false;
            i += 1;
        } else if b == b'/' && !in_class {
            return true;
        } else {
            i += 1;
        }
    }
    false
}

fn scan_regex_literal(cur: &mut ByteCursor<'_>, out: &mut Vec<TokenSpan>) {
    let start = cur.pos;
    cur.advance(1);
    let mut in_class = false;
    while !cur.is_eof() {
        let b = cur.bytes[cur.pos];
        if b == b'\\' && cur.pos + 1 < cur.len() {
            cur.advance(1);
            cur.bump_char();
        } else if b == b'[' {
            in_class = true;
            cur.advance(1);
        } else if b == b']' {
            in_class = false;
            cur.advance(1);
        } else if b == b'/' && !in_class {
            cur.advance(1);
            while !cur.is_eof() && cur.bytes[cur.pos].is_ascii_alphabetic() {
                cur.advance(1);
            }
            break;
        } else {
            cur.bump_char();
        }
    }
    push_span(out, start, cur.pos, TokenKind::String);
}

fn is_js_keyword(w: &str) -> bool {
    matches!(
        w,
        "async" | "await" | "break" | "case" | "catch" | "class" | "const" | "continue"
            | "debugger" | "default" | "delete" | "do" | "else" | "export" | "extends" | "finally"
            | "for" | "from" | "function" | "get" | "if" | "import" | "in" | "instanceof" | "let"
            | "new" | "of" | "return" | "set" | "static" | "super" | "switch" | "this" | "throw"
            | "try" | "typeof" | "var" | "void" | "while" | "with" | "yield"
    )
}

fn is_ts_keyword(w: &str) -> bool {
    matches!(
        w,
        "abstract" | "as" | "asserts" | "declare" | "enum" | "implements" | "infer" | "interface"
            | "is" | "keyof" | "module" | "namespace" | "override" | "private" | "protected"
            | "public" | "readonly" | "satisfies" | "type"
    )
}

fn is_js_constant(w: &str) -> bool {
    matches!(w, "true" | "false" | "null" | "undefined" | "NaN" | "Infinity")
}

fn is_js_builtin_type(w: &str) -> bool {
    matches!(
        w,
        "Array" | "Object" | "String" | "Number" | "Boolean" | "Symbol" | "BigInt" | "Map" | "Set"
            | "WeakMap" | "WeakSet" | "Promise" | "Date" | "RegExp" | "Error" | "Uint8Array"
            | "Int32Array" | "Float64Array" | "ArrayBuffer" | "JSON" | "Math" | "console"
    )
}

fn is_ts_type(w: &str) -> bool {
    matches!(
        w,
        "any" | "unknown" | "never" | "string" | "number" | "boolean" | "bigint" | "symbol"
            | "object" | "Partial" | "Required" | "Readonly" | "Record" | "Pick" | "Omit"
            | "Exclude" | "Extract" | "NonNullable" | "ReturnType" | "Awaited"
    )
}
