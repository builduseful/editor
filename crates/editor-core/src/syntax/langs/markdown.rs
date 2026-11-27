//! Markdown language syntax plugin (`md`, `markdown`, `mdx`).
//!
//! Supports ATX headings (`#`–`######`), blockquotes, lists, horizontal rules,
//! `**bold**`, `*italic*`, `` `inline code` ``, `[links](url)`, and **fenced code blocks
//! (`` ```rust ``, `` ```cpp ``, `` ```ts ``, etc.) that delegate directly to any registered
//! language plugin in [`LanguageRegistry`]**!

use crate::syntax::cursor::{push_span, ByteCursor};
use crate::syntax::{
    Language, LanguageMeta, LanguageRegistry, LexState, SyntaxPlugin, TokenKind, TokenSpan,
};

pub struct MarkdownPlugin;
pub static PLUGIN: MarkdownPlugin = MarkdownPlugin;

const META: LanguageMeta = LanguageMeta {
    id: "markdown",
    name: "Markdown",
    aliases: &["md", "mdx"],
    extensions: &["md", "markdown", "mdx"],
    filenames: &["readme"],
    line_comment: None,
    block_comment: Some(("<!--", "-->")),
    default_word_wrap: true,
};

const MODE_FENCE: u8 = 1;

impl SyntaxPlugin for MarkdownPlugin {
    fn meta(&self) -> &'static LanguageMeta {
        &META
    }

    fn lex_line(
        &self,
        line: &str,
        state: LexState,
        registry: &LanguageRegistry,
        out: &mut Vec<TokenSpan>,
    ) -> LexState {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();

        // 1. Inside a fenced code block: check for closing fence, else delegate to embedded plugin!
        if state.mode() == MODE_FENCE {
            let fence_char = if state.detail() == b'~' { '~' } else { '`' };
            if trimmed.starts_with("```") && fence_char == '`'
                || trimmed.starts_with("~~~") && fence_char == '~'
            {
                push_span(out, indent, line.len(), TokenKind::Comment);
                return LexState::NORMAL;
            }
            let sub_id = state.sub_lang();
            if sub_id > 0 {
                let sub_lang = Language(sub_id as u16);
                // Pack embedded language's `(mode 0..15, depth 0..15)` into our 8-bit `depth` byte
                // so multi-line template literals, raw strings (0..15 hashes), and nested comments round-trip cleanly.
                let packed = state.depth();
                let sub_mode = packed & 0x0F;
                let sub_depth = (packed >> 4) & 0x0F;
                let sub_in = if sub_mode == 0 && sub_depth == 0 {
                    LexState::NORMAL
                } else {
                    LexState::new(sub_mode, 0, sub_depth, 0)
                };
                let sub_out = registry.plugin(sub_lang).lex_line(line, sub_in, registry, out);
                let out_packed = (sub_out.mode() & 0x0F) | ((sub_out.depth().min(15)) << 4);
                return LexState::new(MODE_FENCE, sub_id, out_packed, state.detail());
            } else {
                push_span(out, 0, line.len(), TokenKind::String);
                return state;
            }
        }

        // 2. Opening fenced code block (` ```lang ` or ` ~~~lang `).
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            let fc = trimmed.as_bytes()[0];
            let fence_end = indent
                + trimmed
                    .bytes()
                    .take_while(|&b| b == fc)
                    .count();
            push_span(out, indent, fence_end, TokenKind::Comment);
            let info = line[fence_end..].trim();
            let tag = info.split_whitespace().next().unwrap_or("");
            if !tag.is_empty() {
                let tag_off = line[fence_end..].find(tag).unwrap_or(0) + fence_end;
                push_span(out, tag_off, tag_off + tag.len(), TokenKind::Type);
            }
            let sub_id = registry
                .find(tag)
                .filter(|l| *l != Language::PLAIN_TEXT && l.id() != "markdown")
                .map(|l| l.0 as u8)
                .unwrap_or(0);
            return LexState::new(MODE_FENCE, sub_id, 0, fc);
        }

        // 3. ATX Heading (`# Title`).
        if trimmed.starts_with('#') {
            let hashes = trimmed.bytes().take_while(|&b| b == b'#').count();
            if hashes <= 6 && (hashes == trimmed.len() || trimmed.as_bytes()[hashes] == b' ') {
                push_span(out, indent, line.len(), TokenKind::Heading);
                return LexState::NORMAL;
            }
        }

        // 4. Horizontal rule (`---`, `***`, `___`).
        if trimmed.len() >= 3
            && (trimmed.bytes().all(|b| b == b'-' || b == b' ') && trimmed.bytes().filter(|&b| b == b'-').count() >= 3
                || trimmed.bytes().all(|b| b == b'*' || b == b' ') && trimmed.bytes().filter(|&b| b == b'*').count() >= 3
                || trimmed.bytes().all(|b| b == b'_' || b == b' ') && trimmed.bytes().filter(|&b| b == b'_').count() >= 3)
        {
            push_span(out, indent, line.len(), TokenKind::Punctuation);
            return LexState::NORMAL;
        }

        // 5. Blockquote (`> quote`).
        if trimmed.starts_with('>') {
            push_span(out, indent, line.len(), TokenKind::Comment);
            return LexState::NORMAL;
        }

        // 6. List bullet or numbered list prefix + inline formatting!
        let mut cur = ByteCursor::new(line);
        cur.pos = indent;
        if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ") {
            push_span(out, cur.pos, cur.pos + 1, TokenKind::Operator);
            cur.advance(2);
            // Checkbox `[ ]` / `[x]`
            if cur.starts_with(b"[ ] ") || cur.starts_with(b"[x] ") || cur.starts_with(b"[X] ") {
                push_span(out, cur.pos, cur.pos + 3, TokenKind::Constant);
                cur.advance(4);
            }
        } else if cur.peek().is_some_and(|b| b.is_ascii_digit()) {
            let s = cur.pos;
            while !cur.is_eof() && cur.bytes[cur.pos].is_ascii_digit() {
                cur.pos += 1;
            }
            if cur.peek() == Some(b'.') && cur.peek_at(1) == Some(b' ') {
                cur.advance(1);
                push_span(out, s, cur.pos, TokenKind::Number);
                cur.advance(1);
            } else {
                cur.pos = s;
            }
        }

        // Inline spans: `code`, **strong**, *emphasis*, [label](url).
        while !cur.is_eof() {
            let b = cur.bytes[cur.pos];
            if b == b'`' {
                let s = cur.pos;
                cur.advance(1);
                while !cur.is_eof() && cur.bytes[cur.pos] != b'`' {
                    cur.bump_char();
                }
                if cur.peek() == Some(b'`') {
                    cur.advance(1);
                }
                push_span(out, s, cur.pos, TokenKind::String);
            } else if cur.starts_with(b"**") || cur.starts_with(b"__") {
                let delim = [b, b];
                let s = cur.pos;
                if let Some(rel_end) = cur.src[s + 2..].find(std::str::from_utf8(&delim).unwrap()) {
                    cur.pos = s + 2 + rel_end + 2;
                    push_span(out, s, cur.pos, TokenKind::Strong);
                } else {
                    cur.advance(2);
                }
            } else if b == b'*' && cur.peek_at(1).is_some_and(|c| c != b' ' && c != b'*') {
                let s = cur.pos;
                if let Some(rel_end) = cur.src[s + 1..].find('*') {
                    if rel_end > 0 {
                        cur.pos = s + 1 + rel_end + 1;
                        push_span(out, s, cur.pos, TokenKind::Emphasis);
                        continue;
                    }
                }
                cur.advance(1);
            } else if b == b'[' {
                let s = cur.pos;
                if let Some(close_br) = cur.src[s + 1..].find(']') {
                    let after_br = s + 1 + close_br + 1;
                    if after_br < cur.len() && cur.bytes[after_br] == b'(' {
                        if let Some(close_par) = cur.src[after_br + 1..].find(')') {
                            push_span(out, s, after_br, TokenKind::Link);
                            let url_end = after_br + 1 + close_par + 1;
                            push_span(out, after_br, url_end, TokenKind::Comment);
                            cur.pos = url_end;
                            continue;
                        }
                    }
                }
                cur.advance(1);
            } else {
                cur.bump_char();
            }
        }

        LexState::NORMAL
    }
}
