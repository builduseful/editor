//! Built-in baseline plugins: Plain Text, JSON, TOML/YAML/INI, and Swift.

use crate::syntax::cursor::{
    is_ident_start, is_punct_byte, lex_c_like, push_span, ByteCursor, CLikeSpec,
};
use crate::syntax::{LanguageMeta, LanguageRegistry, LexState, SyntaxPlugin, TokenKind, TokenSpan};

// ── Plain Text (index 0 in BUILTIN_PLUGINS) ─────────────────────────────────

pub struct PlainTextPlugin;
pub static PLAIN_TEXT: PlainTextPlugin = PlainTextPlugin;

const PLAIN_META: LanguageMeta = LanguageMeta {
    id: "plaintext",
    name: "Plain Text",
    aliases: &["text", "txt", "plain"],
    extensions: &["txt", "log"],
    filenames: &[],
    line_comment: None,
    block_comment: None,
    default_word_wrap: true,
};

impl SyntaxPlugin for PlainTextPlugin {
    fn meta(&self) -> &'static LanguageMeta {
        &PLAIN_META
    }

    fn lex_line(
        &self,
        _line: &str,
        _state: LexState,
        _registry: &LanguageRegistry,
        _out: &mut Vec<TokenSpan>,
    ) -> LexState {
        LexState::NORMAL
    }
}

// ── JSON / JSONC ────────────────────────────────────────────────────────────

pub struct JsonPlugin;
pub static JSON: JsonPlugin = JsonPlugin;

const JSON_META: LanguageMeta = LanguageMeta {
    id: "json",
    name: "JSON",
    aliases: &["jsonc"],
    extensions: &["json", "jsonc"],
    filenames: &[],
    line_comment: Some("//"),
    block_comment: Some(("/*", "*/")),
    default_word_wrap: false,
};

impl SyntaxPlugin for JsonPlugin {
    fn meta(&self) -> &'static LanguageMeta {
        &JSON_META
    }

    fn lex_line(
        &self,
        line: &str,
        _state: LexState,
        _registry: &LanguageRegistry,
        out: &mut Vec<TokenSpan>,
    ) -> LexState {
        let mut cur = ByteCursor::new(line);
        while !cur.is_eof() {
            let b = cur.bytes[cur.pos];
            if b == b' ' || b == b'\t' {
                cur.pos += 1;
            } else if b == b'/' && cur.peek_at(1) == Some(b'/') {
                push_span(out, cur.pos, cur.len(), TokenKind::Comment);
                return LexState::NORMAL;
            } else if b == b'"' {
                let s = cur.pos;
                cur.scan_quoted_string(b'"', true, out);
                let mut j = cur.pos;
                while j < cur.len() && cur.bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j < cur.len() && cur.bytes[j] == b':' {
                    // Re-tag the key string spans as `Property`.
                    for sp in out.iter_mut().rev() {
                        if (sp.start as usize) < s {
                            break;
                        }
                        if sp.kind == TokenKind::String {
                            sp.kind = TokenKind::Property;
                        }
                    }
                }
            } else if b.is_ascii_digit()
                || (b == b'-' && cur.peek_at(1).is_some_and(|c| c.is_ascii_digit()))
            {
                let s = cur.pos;
                if b == b'-' {
                    cur.advance(1);
                }
                cur.scan_number(false, out);
                if b == b'-' {
                    if let Some(last) = out.last_mut() {
                        last.start = s as u32;
                    }
                }
            } else if is_ident_start(b) {
                let (s, e, word) = cur.scan_ident();
                if matches!(word, "true" | "false" | "null") {
                    push_span(out, s, e, TokenKind::Constant);
                }
            } else if is_punct_byte(b) || b == b':' {
                push_span(out, cur.pos, cur.pos + 1, TokenKind::Punctuation);
                cur.advance(1);
            } else {
                cur.bump_char();
            }
        }
        LexState::NORMAL
    }
}

// ── TOML / YAML / INI ───────────────────────────────────────────────────────

pub struct TomlPlugin;
pub static TOML: TomlPlugin = TomlPlugin;

const TOML_META: LanguageMeta = LanguageMeta {
    id: "toml",
    name: "TOML / Config",
    aliases: &["yaml", "yml", "ini", "cfg", "conf"],
    extensions: &["toml", "yaml", "yml", "ini", "cfg", "conf"],
    filenames: &["cargo.lock"],
    line_comment: Some("#"),
    block_comment: None,
    default_word_wrap: false,
};

impl SyntaxPlugin for TomlPlugin {
    fn meta(&self) -> &'static LanguageMeta {
        &TOML_META
    }

    fn lex_line(
        &self,
        line: &str,
        _state: LexState,
        _registry: &LanguageRegistry,
        out: &mut Vec<TokenSpan>,
    ) -> LexState {
        let mut cur = ByteCursor::new(line);
        if line.trim_start().starts_with('[') {
            let s = line.len() - line.trim_start().len();
            push_span(out, s, line.len(), TokenKind::Heading);
            return LexState::NORMAL;
        }
        while !cur.is_eof() {
            let b = cur.bytes[cur.pos];
            if b == b' ' || b == b'\t' {
                cur.pos += 1;
            } else if b == b'#' {
                push_span(out, cur.pos, cur.len(), TokenKind::Comment);
                return LexState::NORMAL;
            } else if b == b'"' || b == b'\'' {
                cur.scan_quoted_string(b, b == b'"', out);
            } else if b.is_ascii_digit() {
                cur.scan_number(false, out);
            } else if is_ident_start(b) {
                let s = cur.pos;
                while !cur.is_eof()
                    && (cur.bytes[cur.pos].is_ascii_alphanumeric()
                        || matches!(cur.bytes[cur.pos], b'_' | b'-'))
                {
                    cur.pos += 1;
                }
                let e = cur.pos;
                let word = &cur.src[s..e];
                let mut j = e;
                while j < cur.len() && (cur.bytes[j] == b' ' || cur.bytes[j] == b'\t') {
                    j += 1;
                }
                if j < cur.len() && (cur.bytes[j] == b'=' || cur.bytes[j] == b':') {
                    push_span(out, s, e, TokenKind::Property);
                } else if matches!(word, "true" | "false" | "null") {
                    push_span(out, s, e, TokenKind::Constant);
                }
            } else if b == b'=' || b == b':' {
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

// ── Swift ───────────────────────────────────────────────────────────────────

pub struct SwiftPlugin;
pub static SWIFT: SwiftPlugin = SwiftPlugin;

const SWIFT_META: LanguageMeta = LanguageMeta {
    id: "swift",
    name: "Swift",
    aliases: &[],
    extensions: &["swift"],
    filenames: &[],
    line_comment: Some("//"),
    block_comment: Some(("/*", "*/")),
    default_word_wrap: false,
};

const SWIFT_SPEC: CLikeSpec = CLikeSpec {
    nested_block_comments: true,
    preprocessor_hash: true,
    at_annotations: true,
    triple_double_strings: true,
    backtick_raw_strings: false,
    apostrophe_num_sep: false,
    is_keyword: is_swift_kw,
    is_type: is_swift_type,
    is_constant: is_swift_const,
};

impl SyntaxPlugin for SwiftPlugin {
    fn meta(&self) -> &'static LanguageMeta {
        &SWIFT_META
    }

    fn lex_line(
        &self,
        line: &str,
        state: LexState,
        _registry: &LanguageRegistry,
        out: &mut Vec<TokenSpan>,
    ) -> LexState {
        lex_c_like(&SWIFT_SPEC, line, state, out)
    }
}

fn is_swift_kw(w: &str) -> bool {
    matches!(
        w,
        "actor" | "as" | "async" | "await" | "break" | "case" | "catch" | "class" | "continue"
            | "default" | "defer" | "do" | "else" | "enum" | "extension" | "for" | "func" | "guard"
            | "if" | "import" | "in" | "init" | "inout" | "internal" | "is" | "let" | "open"
            | "override" | "private" | "protocol" | "public" | "repeat" | "return" | "self"
            | "static" | "struct" | "super" | "switch" | "throw" | "throws" | "try" | "typealias"
            | "var" | "where" | "while"
    )
}

fn is_swift_const(w: &str) -> bool {
    matches!(w, "true" | "false" | "nil")
}

fn is_swift_type(w: &str) -> bool {
    matches!(
        w,
        "Any" | "Bool" | "Character" | "Double" | "Float" | "Int" | "Int8" | "Int16" | "Int32"
            | "Int64" | "UInt" | "UInt8" | "UInt16" | "UInt32" | "UInt64" | "String" | "Array"
            | "Dictionary" | "Set" | "Optional" | "Result" | "Self" | "Void"
    )
}
