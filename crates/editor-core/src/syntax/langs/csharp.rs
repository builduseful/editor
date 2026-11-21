//! C# language syntax plugin (`cs`, `csx`).
//!
//! Supports verbatim strings `@"..."`, interpolated strings `$"..."`, multi-line raw string
//! literals `"""..."""`, `[Attributes]`, `#region`/`#nullable` directives, `/* */` comments,
//! async/LINQ/record keywords, and .NET built-in types.

use crate::syntax::cursor::{
    is_ident_start, is_operator_byte, is_pascal_case, is_punct_byte, is_screaming_const,
    push_span, ByteCursor,
};
use crate::syntax::{LanguageMeta, LanguageRegistry, LexState, SyntaxPlugin, TokenKind, TokenSpan};

pub struct CSharpPlugin;
pub static PLUGIN: CSharpPlugin = CSharpPlugin;

const META: LanguageMeta = LanguageMeta {
    id: "csharp",
    name: "C#",
    aliases: &["cs", "c#", "dotnet"],
    extensions: &["cs", "csx"],
    filenames: &[],
    line_comment: Some("//"),
    block_comment: Some(("/*", "*/")),
    default_word_wrap: false,
};

const MODE_BLOCK_COMMENT: u8 = 1;
const MODE_VERBATIM_STRING: u8 = 2;
const MODE_RAW_STRING: u8 = 3;

impl SyntaxPlugin for CSharpPlugin {
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
            MODE_VERBATIM_STRING => {
                if scan_verbatim_body(&mut cur, out) {
                    return LexState::new(MODE_VERBATIM_STRING, 0, 0, 0);
                }
            }
            MODE_RAW_STRING => {
                if cur.scan_triple_string(b'"', true, false, out) {
                    return LexState::new(MODE_RAW_STRING, 0, 0, 0);
                }
            }
            _ => {}
        }

        // Preprocessor directives (`#region`, `#endregion`, `#nullable`, `#if`, `#pragma`).
        if cur.pos == 0 {
            let mut p = 0;
            while p < cur.len() && (cur.bytes[p] == b' ' || cur.bytes[p] == b'\t') {
                p += 1;
            }
            if p < cur.len() && cur.bytes[p] == b'#' {
                push_span(out, p, cur.len(), TokenKind::Macro);
                return LexState::NORMAL;
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
            } else if cur.starts_with(b"\"\"\"") || cur.starts_with(b"$\"\"\"") || cur.starts_with(b"$$\"\"\"") {
                let s = cur.pos;
                while cur.peek() == Some(b'$') {
                    cur.advance(1);
                }
                if cur.pos > s {
                    push_span(out, s, cur.pos, TokenKind::String);
                }
                if cur.scan_triple_string(b'"', false, false, out) {
                    return LexState::new(MODE_RAW_STRING, 0, 0, 0);
                }
            } else if cur.starts_with(b"@\"") || cur.starts_with(b"$@\"") || cur.starts_with(b"@$\"") {
                let s = cur.pos;
                while matches!(cur.peek(), Some(b'@' | b'$')) {
                    cur.advance(1);
                }
                cur.advance(1); // opening `"`
                push_span(out, s, cur.pos, TokenKind::String);
                if scan_verbatim_body(&mut cur, out) {
                    return LexState::new(MODE_VERBATIM_STRING, 0, 0, 0);
                }
            } else if b == b'$' && cur.peek_at(1) == Some(b'"') {
                let s = cur.pos;
                cur.advance(1);
                push_span(out, s, cur.pos, TokenKind::String);
                cur.scan_quoted_string(b'"', true, out);
            } else if b == b'"' || b == b'\'' {
                cur.scan_quoted_string(b, true, out);
            } else if b == b'[' && cur.peek_at(1).is_some_and(is_ident_start) && is_line_start_attr(&cur) {
                let s = cur.pos;
                cur.advance(1);
                cur.scan_ident();
                push_span(out, s, cur.pos, TokenKind::Attribute);
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

fn is_line_start_attr(cur: &ByteCursor<'_>) -> bool {
    cur.bytes[..cur.pos]
        .iter()
        .all(|&b| b == b' ' || b == b'\t')
}

/// Scan C# verbatim string body (`""` is escaped quote). Returns `true` if still open at EOL.
fn scan_verbatim_body(cur: &mut ByteCursor<'_>, out: &mut Vec<TokenSpan>) -> bool {
    let mut seg_start = cur.pos;
    while !cur.is_eof() {
        if cur.bytes[cur.pos] == b'"' {
            if cur.peek_at(1) == Some(b'"') {
                push_span(out, seg_start, cur.pos, TokenKind::String);
                let es = cur.pos;
                cur.advance(2);
                push_span(out, es, cur.pos, TokenKind::Escape);
                seg_start = cur.pos;
            } else {
                cur.advance(1);
                push_span(out, seg_start, cur.pos, TokenKind::String);
                return false;
            }
        } else {
            cur.bump_char();
        }
    }
    push_span(out, seg_start, cur.pos, TokenKind::String);
    true
}

fn is_keyword(w: &str) -> bool {
    matches!(
        w,
        "abstract" | "as" | "async" | "await" | "base" | "break" | "case" | "catch" | "checked"
            | "class" | "const" | "continue" | "default" | "delegate" | "do" | "else" | "enum"
            | "event" | "explicit" | "extern" | "file" | "finally" | "fixed" | "for" | "foreach"
            | "from" | "get" | "global" | "goto" | "group" | "if" | "implicit" | "in" | "init"
            | "interface" | "internal" | "into" | "is" | "join" | "let" | "lock" | "namespace"
            | "new" | "not" | "operator" | "or" | "orderby" | "out" | "override" | "params"
            | "partial" | "private" | "protected" | "public" | "readonly" | "record" | "ref"
            | "required" | "return" | "scoped" | "sealed" | "select" | "set" | "sizeof"
            | "stackalloc" | "static" | "struct" | "switch" | "this" | "throw" | "try" | "typeof"
            | "unchecked" | "unsafe" | "using" | "var" | "virtual" | "volatile" | "when" | "where"
            | "while" | "with" | "yield"
    )
}

fn is_constant(w: &str) -> bool {
    matches!(w, "true" | "false" | "null")
}

fn is_type(w: &str) -> bool {
    matches!(
        w,
        "bool" | "byte" | "sbyte" | "char" | "decimal" | "double" | "float" | "int" | "uint"
            | "nint" | "nuint" | "long" | "ulong" | "short" | "ushort" | "object" | "string"
            | "void" | "dynamic" | "Task" | "ValueTask" | "List" | "Dictionary" | "HashSet"
            | "IEnumerable" | "IList" | "Span" | "ReadOnlySpan" | "Guid" | "DateTime"
            | "Exception" | "Console"
    )
}
