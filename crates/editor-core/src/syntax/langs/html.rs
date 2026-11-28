//! HTML / XML language syntax plugin (`html`, `htm`, `xml`, `svg`, `plist`).
//!
//! Supports multi-line `<!-- ... -->` comments, `<!DOCTYPE>`, `<tag>` / `</tag>`,
//! attributes `attr="val"`, `&entities;`, and **embedded `<style>` (CSS) & `<script>` (JS)**
//! delegation via [`LanguageRegistry`]!

use crate::syntax::cursor::{push_span, ByteCursor};
use crate::syntax::{
    Language, LanguageMeta, LanguageRegistry, LexState, SyntaxPlugin, TokenKind, TokenSpan,
};

pub struct HtmlPlugin;
pub static PLUGIN: HtmlPlugin = HtmlPlugin;

const META: LanguageMeta = LanguageMeta {
    id: "html",
    name: "HTML",
    aliases: &["htm", "xml", "svg", "xhtml"],
    extensions: &["html", "htm", "xml", "svg", "plist", "xhtml"],
    filenames: &[],
    line_comment: None,
    block_comment: Some(("<!--", "-->")),
    default_word_wrap: false,
};

const MODE_COMMENT: u8 = 1;
const MODE_IN_TAG: u8 = 2;
const MODE_EMBED_SCRIPT: u8 = 3;
const MODE_EMBED_STYLE: u8 = 4;

impl SyntaxPlugin for HtmlPlugin {
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
        // 1. Embedded `<script>` or `<style>` block delegation!
        if state.mode() == MODE_EMBED_SCRIPT || state.mode() == MODE_EMBED_STYLE {
            let close_needle = if state.mode() == MODE_EMBED_SCRIPT {
                "</script"
            } else {
                "</style"
            };
            let sub_lang = Language(state.sub_lang() as u16);
            let sub_state = LexState::new(state.depth(), 0, state.detail(), 0);
            if let Some(close_pos) = find_ascii_ci(line, close_needle) {
                if close_pos > 0 {
                    let prefix = &line[..close_pos];
                    registry
                        .plugin(sub_lang)
                        .lex_line(prefix, sub_state, registry, out);
                }
                let mut cur = ByteCursor::new(line);
                cur.pos = close_pos;
                return lex_html_from(&mut cur, false, None, registry, out);
            } else {
                let next_sub = registry
                    .plugin(sub_lang)
                    .lex_line(line, sub_state, registry, out);
                return LexState::new(state.mode(), state.sub_lang(), next_sub.mode(), next_sub.depth());
            }
        }

        let mut cur = ByteCursor::new(line);
        let mut in_tag = false;
        let mut pending_embed: Option<u8> = None;

        match state.mode() {
            MODE_COMMENT => {
                if scan_html_comment(&mut cur, true, out) {
                    return LexState::new(MODE_COMMENT, 0, 0, 0);
                }
            }
            MODE_IN_TAG => {
                in_tag = true;
                let d = state.detail();
                if d == MODE_EMBED_SCRIPT || d == MODE_EMBED_STYLE {
                    pending_embed = Some(d);
                }
            }
            _ => {}
        }

        lex_html_from(&mut cur, in_tag, pending_embed, registry, out)
    }
}

fn lex_html_from(
    cur: &mut ByteCursor<'_>,
    mut in_tag: bool,
    mut pending_embed: Option<u8>,
    registry: &LanguageRegistry,
    out: &mut Vec<TokenSpan>,
) -> LexState {

    while !cur.is_eof() {
        if in_tag {
            let b = cur.bytes[cur.pos];
            if b == b' ' || b == b'\t' {
                cur.pos += 1;
            } else if b == b'>' || (b == b'/' && cur.peek_at(1) == Some(b'>')) {
                let self_closing = b == b'/';
                let s = cur.pos;
                cur.advance(if self_closing { 2 } else { 1 });
                push_span(out, s, cur.pos, TokenKind::Tag);
                in_tag = false;
                if !self_closing {
                    if let Some(embed_mode) = pending_embed.take() {
                        let lang_id = if embed_mode == MODE_EMBED_SCRIPT {
                            "javascript"
                        } else {
                            "css"
                        };
                        let close_needle = if embed_mode == MODE_EMBED_SCRIPT {
                            "</script"
                        } else {
                            "</style"
                        };
                        if let Some(sub_lang) = registry.find(lang_id) {
                            let rest = cur.rest();
                            if let Some(rel_close) = find_ascii_ci(rest, close_needle) {
                                if rel_close > 0 {
                                    let mut sub_spans = Vec::new();
                                    registry.plugin(sub_lang).lex_line(
                                        &rest[..rel_close],
                                        LexState::NORMAL,
                                        registry,
                                        &mut sub_spans,
                                    );
                                    for sp in sub_spans {
                                        push_span(
                                            out,
                                            cur.pos + sp.start as usize,
                                            cur.pos + sp.end as usize,
                                            sp.kind,
                                        );
                                    }
                                }
                                cur.pos += rel_close;
                            } else {
                                if !rest.is_empty() {
                                    let mut sub_spans = Vec::new();
                                    let next_sub = registry.plugin(sub_lang).lex_line(
                                        rest,
                                        LexState::NORMAL,
                                        registry,
                                        &mut sub_spans,
                                    );
                                    for sp in sub_spans {
                                        push_span(
                                            out,
                                            cur.pos + sp.start as usize,
                                            cur.pos + sp.end as usize,
                                            sp.kind,
                                        );
                                    }
                                    return LexState::new(
                                        embed_mode,
                                        sub_lang.0 as u8,
                                        next_sub.mode(),
                                        next_sub.depth(),
                                    );
                                }
                                return LexState::new(embed_mode, sub_lang.0 as u8, 0, 0);
                            }
                        }
                    }
                } else {
                    pending_embed = None;
                }
            } else if b == b'"' || b == b'\'' {
                cur.scan_quoted_string(b, false, out);
            } else if b == b'=' {
                push_span(out, cur.pos, cur.pos + 1, TokenKind::Operator);
                cur.advance(1);
            } else if b.is_ascii_alphabetic() || b == b'_' || b == b':' || b == b'@' || b == b'-' {
                let s = cur.pos;
                while !cur.is_eof()
                    && (cur.bytes[cur.pos].is_ascii_alphanumeric()
                        || matches!(cur.bytes[cur.pos], b'-' | b'_' | b':' | b'.' | b'@'))
                {
                    cur.pos += 1;
                }
                push_span(out, s, cur.pos, TokenKind::Attribute);
            } else {
                cur.bump_char();
            }
            continue;
        }

        let b = cur.bytes[cur.pos];
        if cur.starts_with(b"<!--") {
            if scan_html_comment(cur, false, out) {
                return LexState::new(MODE_COMMENT, 0, 0, 0);
            }
        } else if cur.starts_with(b"<!") {
            let s = cur.pos;
            while !cur.is_eof() && cur.peek() != Some(b'>') {
                cur.bump_char();
            }
            if cur.peek() == Some(b'>') {
                cur.advance(1);
            }
            push_span(out, s, cur.pos, TokenKind::Macro);
        } else if b == b'<' {
            let s = cur.pos;
            cur.advance(1);
            let is_close = if cur.peek() == Some(b'/') || cur.peek() == Some(b'?') {
                let c = cur.peek() == Some(b'/');
                cur.advance(1);
                c
            } else {
                false
            };
            let name_start = cur.pos;
            while !cur.is_eof()
                && (cur.bytes[cur.pos].is_ascii_alphanumeric()
                    || matches!(cur.bytes[cur.pos], b'-' | b':' | b'_'))
            {
                cur.pos += 1;
            }
            let tag_name = &cur.src[name_start..cur.pos];
            if !tag_name.is_empty() {
                push_span(out, s, cur.pos, TokenKind::Tag);
                in_tag = true;
                pending_embed = if !is_close && tag_name.eq_ignore_ascii_case("script") {
                    Some(MODE_EMBED_SCRIPT)
                } else if !is_close && tag_name.eq_ignore_ascii_case("style") {
                    Some(MODE_EMBED_STYLE)
                } else {
                    None
                };
            } else {
                push_span(out, s, cur.pos, TokenKind::Punctuation);
            }
        } else if b == b'&' {
            let s = cur.pos;
            cur.advance(1);
            let mut valid = false;
            while !cur.is_eof() && cur.pos - s <= 12 {
                let c = cur.bytes[cur.pos];
                if c == b';' {
                    cur.advance(1);
                    valid = true;
                    break;
                }
                if !(c.is_ascii_alphanumeric() || c == b'#') {
                    break;
                }
                cur.pos += 1;
            }
            if valid {
                push_span(out, s, cur.pos, TokenKind::Constant);
            }
        } else {
            cur.bump_char();
        }
    }

    if in_tag {
        LexState::new(MODE_IN_TAG, 0, 0, pending_embed.unwrap_or(0))
    } else {
        LexState::NORMAL
    }
}

fn scan_html_comment(cur: &mut ByteCursor<'_>, continuing: bool, out: &mut Vec<TokenSpan>) -> bool {
    let start = cur.pos;
    if !continuing {
        cur.advance(4);
    }
    while cur.pos + 2 < cur.len() {
        if cur.starts_with(b"-->") {
            cur.advance(3);
            push_span(out, start, cur.pos, TokenKind::Comment);
            return false;
        }
        cur.bump_char();
    }
    cur.pos = cur.len();
    push_span(out, start, cur.pos, TokenKind::Comment);
    true
}

fn find_ascii_ci(haystack: &str, needle_lower: &str) -> Option<usize> {
    let hb = haystack.as_bytes();
    let nb = needle_lower.as_bytes();
    if nb.is_empty() || nb.len() > hb.len() {
        return None;
    }
    for i in 0..=(hb.len() - nb.len()) {
        if hb[i..i + nb.len()]
            .iter()
            .zip(nb.iter())
            .all(|(&a, &b)| a.to_ascii_lowercase() == b)
        {
            return Some(i);
        }
    }
    None
}
