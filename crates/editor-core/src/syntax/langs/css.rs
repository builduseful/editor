//! CSS / SCSS language syntax plugin (`css`, `scss`, `less`).
//!
//! Tracks brace nesting depth across lines so selectors (`.class`, `#id`, `tag`, `:hover`)
//! and declarations (`property: value 12px #hex var(--token) !important;`) highlight cleanly!

use crate::syntax::cursor::{push_span, ByteCursor};
use crate::syntax::{LanguageMeta, LanguageRegistry, LexState, SyntaxPlugin, TokenKind, TokenSpan};

pub struct CssPlugin;
pub static PLUGIN: CssPlugin = CssPlugin;

const META: LanguageMeta = LanguageMeta {
    id: "css",
    name: "CSS",
    aliases: &["scss", "less", "style"],
    extensions: &["css", "scss", "less"],
    filenames: &[],
    line_comment: Some("//"),
    block_comment: Some(("/*", "*/")),
    default_word_wrap: false,
};

const MODE_BLOCK_COMMENT: u8 = 1;

impl SyntaxPlugin for CssPlugin {
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
        let mut brace_depth = state.depth();

        if state.mode() == MODE_BLOCK_COMMENT {
            let rem = cur.scan_block_comment(1, false, out);
            if rem > 0 {
                return LexState::new(MODE_BLOCK_COMMENT, 0, brace_depth, 0);
            }
        }

        let mut after_colon = false;

        while !cur.is_eof() {
            let b = cur.bytes[cur.pos];
            if b == b' ' || b == b'\t' {
                cur.pos += 1;
                continue;
            }
            if b == b'/' && cur.peek_at(1) == Some(b'*') {
                let rem = cur.scan_block_comment(0, false, out);
                if rem > 0 {
                    return LexState::new(MODE_BLOCK_COMMENT, 0, brace_depth, 0);
                }
                continue;
            }
            if b == b'/' && cur.peek_at(1) == Some(b'/') {
                // SCSS single-line comment.
                push_span(out, cur.pos, cur.len(), TokenKind::Comment);
                return LexState::new(0, 0, brace_depth, 0);
            }
            if b == b'"' || b == b'\'' {
                cur.scan_quoted_string(b, true, out);
                continue;
            }
            if b == b'{' {
                brace_depth = brace_depth.saturating_add(1);
                after_colon = false;
                push_span(out, cur.pos, cur.pos + 1, TokenKind::Punctuation);
                cur.advance(1);
                continue;
            }
            if b == b'}' {
                brace_depth = brace_depth.saturating_sub(1);
                after_colon = false;
                push_span(out, cur.pos, cur.pos + 1, TokenKind::Punctuation);
                cur.advance(1);
                continue;
            }
            if b == b';' {
                after_colon = false;
                push_span(out, cur.pos, cur.pos + 1, TokenKind::Punctuation);
                cur.advance(1);
                continue;
            }
            if b == b'@' {
                let s = cur.pos;
                cur.advance(1);
                scan_css_ident(&mut cur);
                push_span(out, s, cur.pos, TokenKind::Macro);
                continue;
            }
            if b == b'!' {
                let s = cur.pos;
                cur.advance(1);
                scan_css_ident(&mut cur);
                push_span(out, s, cur.pos, TokenKind::Keyword);
                continue;
            }
            // Hex color (`#ff00aa`) or `#id` selector.
            if b == b'#' {
                let s = cur.pos;
                cur.advance(1);
                scan_css_ident(&mut cur);
                let kind = if after_colon {
                    TokenKind::Constant
                } else {
                    TokenKind::Tag
                };
                push_span(out, s, cur.pos, kind);
                continue;
            }
            // Numeric literal with optional unit (`12px`, `1.5rem`, `100%`, `-0.5em`).
            if b.is_ascii_digit()
                || (b == b'.' && cur.peek_at(1).is_some_and(|c| c.is_ascii_digit()))
                || (b == b'-'
                    && after_colon
                    && cur.peek_at(1).is_some_and(|c| c.is_ascii_digit() || c == b'.'))
            {
                let s = cur.pos;
                if b == b'-' {
                    cur.advance(1);
                }
                while !cur.is_eof()
                    && (cur.bytes[cur.pos].is_ascii_digit() || cur.bytes[cur.pos] == b'.')
                {
                    cur.pos += 1;
                }
                // CSS unit (`px`, `rem`, `vh`, `%`, `ms`, `deg`, etc.)
                if cur.peek() == Some(b'%') {
                    cur.advance(1);
                } else {
                    while !cur.is_eof() && cur.bytes[cur.pos].is_ascii_alphabetic() {
                        cur.pos += 1;
                    }
                }
                push_span(out, s, cur.pos, TokenKind::Number);
                continue;
            }
            // `.class` selector outside values.
            if b == b'.' && !after_colon {
                let s = cur.pos;
                cur.advance(1);
                scan_css_ident(&mut cur);
                push_span(out, s, cur.pos, TokenKind::Type);
                continue;
            }
            // `:pseudo` / `::pseudo` selector vs property `:` delimiter.
            if b == b':' {
                if brace_depth > 0 && !after_colon && cur.peek_at(1) != Some(b':') && line_has_decl_colon(cur.bytes, cur.pos) {
                    after_colon = true;
                    push_span(out, cur.pos, cur.pos + 1, TokenKind::Punctuation);
                    cur.advance(1);
                } else {
                    let s = cur.pos;
                    cur.advance(1);
                    if cur.peek() == Some(b':') {
                        cur.advance(1);
                    }
                    scan_css_ident(&mut cur);
                    push_span(out, s, cur.pos, TokenKind::Attribute);
                }
                continue;
            }
            // CSS custom property `--my-var` or SCSS `$var`.
            if (b == b'-' && cur.peek_at(1) == Some(b'-')) || b == b'$' {
                let s = cur.pos;
                cur.advance(1);
                scan_css_ident(&mut cur);
                push_span(out, s, cur.pos, TokenKind::Property);
                continue;
            }
            if b.is_ascii_alphabetic() || b == b'_' || b == b'-' {
                let s = cur.pos;
                scan_css_ident(&mut cur);
                let e = cur.pos;
                let word = &cur.src[s..e];
                if cur.peek() == Some(b'(') {
                    push_span(out, s, e, TokenKind::Function);
                } else if brace_depth > 0 && !after_colon && next_non_ws_is(cur.bytes, e, b':') {
                    push_span(out, s, e, TokenKind::Property);
                } else if brace_depth == 0 {
                    push_span(out, s, e, TokenKind::Tag);
                } else if is_css_value_keyword(word) {
                    push_span(out, s, e, TokenKind::Keyword);
                }
                continue;
            }
            if matches!(b, b'(' | b')' | b'[' | b']' | b',') {
                push_span(out, cur.pos, cur.pos + 1, TokenKind::Punctuation);
                cur.advance(1);
            } else if matches!(b, b'>' | b'+' | b'~' | b'*' | b'=') {
                push_span(out, cur.pos, cur.pos + 1, TokenKind::Operator);
                cur.advance(1);
            } else {
                cur.bump_char();
            }
        }

        LexState::new(0, 0, brace_depth, 0)
    }
}

fn scan_css_ident(cur: &mut ByteCursor<'_>) {
    while !cur.is_eof()
        && (cur.bytes[cur.pos].is_ascii_alphanumeric()
            || cur.bytes[cur.pos] == b'-'
            || cur.bytes[cur.pos] == b'_')
    {
        cur.pos += 1;
    }
}

fn next_non_ws_is(bytes: &[u8], mut i: usize, target: u8) -> bool {
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    i < bytes.len() && bytes[i] == target
}

fn line_has_decl_colon(bytes: &[u8], colon_pos: usize) -> bool {
    // If `{` appears later on the same line without `;` first, this is likely a pseudo-selector (`a:hover {`).
    for &c in &bytes[colon_pos + 1..] {
        if c == b';' {
            return true;
        }
        if c == b'{' {
            return false;
        }
    }
    true
}

fn is_css_value_keyword(w: &str) -> bool {
    matches!(
        w,
        "auto" | "none" | "inherit" | "initial" | "unset" | "revert" | "flex" | "grid" | "block"
            | "inline" | "inline-block" | "inline-flex" | "relative" | "absolute" | "fixed"
            | "sticky" | "static" | "hidden" | "visible" | "scroll" | "solid" | "dashed"
            | "dotted" | "transparent" | "currentColor" | "bold" | "normal" | "italic"
            | "center" | "left" | "right" | "top" | "bottom" | "nowrap" | "wrap" | "pointer"
    )
}
