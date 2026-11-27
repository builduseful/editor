//! Zero-allocation byte scanner primitives shared across compiled Rust language plugins.
//!
//! Every helper operates strictly on UTF-8 byte indices, emits sorted non-overlapping
//! [`TokenSpan`]s, and performs zero heap allocations per line.

use super::{LexState, TokenKind, TokenSpan};

/// Append a token span, coalescing with the previous span if adjacent with the same kind.
#[inline]
pub fn push_span(out: &mut Vec<TokenSpan>, start: usize, end: usize, kind: TokenKind) {
    if start >= end {
        return;
    }
    if let Some(last) = out.last_mut() {
        if last.kind == kind && last.end as usize == start {
            last.end = end as u32;
            return;
        }
    }
    out.push(TokenSpan {
        start: start as u32,
        end: end as u32,
        kind,
    });
}

/// Fast zero-allocation cursor over a single UTF-8 line.
pub struct ByteCursor<'a> {
    pub src: &'a str,
    pub bytes: &'a [u8],
    pub pos: usize,
}

impl<'a> ByteCursor<'a> {
    #[inline]
    pub fn new(src: &'a str) -> Self {
        Self {
            src,
            bytes: src.as_bytes(),
            pos: 0,
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    #[inline]
    pub fn is_eof(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    #[inline]
    pub fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    #[inline]
    pub fn peek_at(&self, offset: usize) -> Option<u8> {
        self.bytes.get(self.pos + offset).copied()
    }

    #[inline]
    pub fn starts_with(&self, prefix: &[u8]) -> bool {
        self.bytes[self.pos..].starts_with(prefix)
    }

    #[inline]
    pub fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }

    #[inline]
    pub fn advance(&mut self, n: usize) {
        self.pos = (self.pos + n).min(self.bytes.len());
    }

    /// Advance past a single UTF-8 codepoint (never splits a multi-byte character).
    #[inline]
    pub fn bump_char(&mut self) {
        if self.pos >= self.bytes.len() {
            return;
        }
        let b = self.bytes[self.pos];
        let step = if b < 0x80 {
            1
        } else if b & 0xE0 == 0xC0 {
            2
        } else if b & 0xF0 == 0xE0 {
            3
        } else {
            4
        };
        self.pos = (self.pos + step).min(self.bytes.len());
    }

    #[inline]
    pub fn skip_ws(&mut self) {
        while let Some(b) = self.peek() {
            if b == b' ' || b == b'\t' {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    /// Scan a C-style `/* ... */` block comment (optionally nested as in Rust/Kotlin).
    /// Assumes cursor is either continuing from `depth > 0` at start of line, or positioned
    /// right at `/*` with `depth == 0`. Returns remaining nesting depth (`0` if closed).
    pub fn scan_block_comment(
        &mut self,
        mut depth: u8,
        nested: bool,
        out: &mut Vec<TokenSpan>,
    ) -> u8 {
        let start = self.pos;
        if depth == 0 {
            if !self.starts_with(b"/*") {
                return 0;
            }
            self.pos += 2;
            depth = 1;
        }
        let len = self.bytes.len();
        while self.pos + 1 < len && depth > 0 {
            if nested && self.bytes[self.pos] == b'/' && self.bytes[self.pos + 1] == b'*' {
                depth = depth.saturating_add(1).min(255);
                self.pos += 2;
            } else if self.bytes[self.pos] == b'*' && self.bytes[self.pos + 1] == b'/' {
                depth -= 1;
                self.pos += 2;
            } else {
                self.pos += 1;
            }
        }
        if depth > 0 {
            self.pos = len;
        }
        push_span(out, start, self.pos, TokenKind::Comment);
        depth
    }

    /// Scan a single-line quoted string (`"` or `'`), emitting escape sequences as `TokenKind::Escape`.
    pub fn scan_quoted_string(
        &mut self,
        quote: u8,
        allow_escape: bool,
        out: &mut Vec<TokenSpan>,
    ) {
        let len = self.bytes.len();
        let mut seg_start = self.pos;
        self.pos += 1; // opening quote
        while self.pos < len {
            let b = self.bytes[self.pos];
            if allow_escape && b == b'\\' && self.pos + 1 < len {
                push_span(out, seg_start, self.pos, TokenKind::String);
                let esc_start = self.pos;
                self.pos += 1;
                let esc_b = self.bytes[self.pos];
                self.bump_char();
                // Handle `\xNN`, `\uNNNN`, `\u{...}` neatly.
                match esc_b {
                    b'x' => {
                        let mut k = 0;
                        while k < 2
                            && self.pos < len
                            && self.bytes[self.pos].is_ascii_hexdigit()
                        {
                            self.pos += 1;
                            k += 1;
                        }
                    }
                    b'u' if self.pos < len && self.bytes[self.pos] == b'{' => {
                        self.pos += 1;
                        while self.pos < len
                            && self.bytes[self.pos] != b'}'
                            && self.bytes[self.pos] != quote
                        {
                            self.pos += 1;
                        }
                        if self.pos < len && self.bytes[self.pos] == b'}' {
                            self.pos += 1;
                        }
                    }
                    b'u' => {
                        let mut k = 0;
                        while k < 4
                            && self.pos < len
                            && self.bytes[self.pos].is_ascii_hexdigit()
                        {
                            self.pos += 1;
                            k += 1;
                        }
                    }
                    _ => {}
                }
                push_span(out, esc_start, self.pos, TokenKind::Escape);
                seg_start = self.pos;
            } else if b == quote {
                self.pos += 1;
                push_span(out, seg_start, self.pos, TokenKind::String);
                return;
            } else {
                self.bump_char();
            }
        }
        push_span(out, seg_start, self.pos, TokenKind::String);
    }

    /// Scan a triple-quoted multi-line string (`"""` or `'''`).
    /// Returns `true` if still open at end of line.
    pub fn scan_triple_string(
        &mut self,
        quote: u8,
        continuing: bool,
        allow_escape: bool,
        out: &mut Vec<TokenSpan>,
    ) -> bool {
        let len = self.bytes.len();
        let mut seg_start = self.pos;
        if !continuing {
            self.pos += 3;
        }
        while self.pos < len {
            let b = self.bytes[self.pos];
            if allow_escape && b == b'\\' && self.pos + 1 < len {
                push_span(out, seg_start, self.pos, TokenKind::String);
                let esc_start = self.pos;
                self.pos += 1;
                self.bump_char();
                push_span(out, esc_start, self.pos, TokenKind::Escape);
                seg_start = self.pos;
            } else if b == quote
                && self.pos + 2 < len
                && self.bytes[self.pos + 1] == quote
                && self.bytes[self.pos + 2] == quote
            {
                self.pos += 3;
                push_span(out, seg_start, self.pos, TokenKind::String);
                return false;
            } else {
                self.bump_char();
            }
        }
        push_span(out, seg_start, self.pos, TokenKind::String);
        true
    }

    /// Scan a numeric literal (decimal, `0x`, `0b`, `0o`, float, exponent, `_`/`'` separators, type suffix).
    pub fn scan_number(&mut self, allow_apos_sep: bool, out: &mut Vec<TokenSpan>) {
        let start = self.pos;
        let len = self.bytes.len();
        if self.bytes[self.pos] == b'0' && self.pos + 1 < len {
            match self.bytes[self.pos + 1] {
                b'x' | b'X' => {
                    self.pos += 2;
                    while self.pos < len
                        && (self.bytes[self.pos].is_ascii_hexdigit()
                            || self.bytes[self.pos] == b'_'
                            || (allow_apos_sep && self.bytes[self.pos] == b'\''))
                    {
                        self.pos += 1;
                    }
                    self.consume_num_suffix();
                    push_span(out, start, self.pos, TokenKind::Number);
                    return;
                }
                b'b' | b'B' | b'o' | b'O' => {
                    self.pos += 2;
                    while self.pos < len
                        && (self.bytes[self.pos].is_ascii_digit()
                            || self.bytes[self.pos] == b'_'
                            || (allow_apos_sep && self.bytes[self.pos] == b'\''))
                    {
                        self.pos += 1;
                    }
                    self.consume_num_suffix();
                    push_span(out, start, self.pos, TokenKind::Number);
                    return;
                }
                _ => {}
            }
        }
        let mut saw_dot = false;
        let mut saw_exp = false;
        while self.pos < len {
            let b = self.bytes[self.pos];
            if b.is_ascii_digit() || b == b'_' || (allow_apos_sep && b == b'\'') {
                self.pos += 1;
            } else if b == b'.'
                && !saw_dot
                && !saw_exp
                && self.pos + 1 < len
                && self.bytes[self.pos + 1].is_ascii_digit()
            {
                saw_dot = true;
                self.pos += 1;
            } else if (b == b'e' || b == b'E') && !saw_exp {
                saw_exp = true;
                self.pos += 1;
                if self.pos < len && (self.bytes[self.pos] == b'+' || self.bytes[self.pos] == b'-')
                {
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
        self.consume_num_suffix();
        push_span(out, start, self.pos, TokenKind::Number);
    }

    fn consume_num_suffix(&mut self) {
        let len = self.bytes.len();
        while self.pos < len && (self.bytes[self.pos].is_ascii_alphanumeric() || self.bytes[self.pos] == b'_') {
            self.pos += 1;
        }
    }

    /// Scan an ASCII identifier `[A-Za-z_][A-Za-z0-9_]*` and return `(start, end, slice)`.
    #[inline]
    pub fn scan_ident(&mut self) -> (usize, usize, &'a str) {
        let start = self.pos;
        let len = self.bytes.len();
        while self.pos < len
            && (self.bytes[self.pos].is_ascii_alphanumeric() || self.bytes[self.pos] == b'_')
        {
            self.pos += 1;
        }
        (start, self.pos, &self.src[start..self.pos])
    }

    /// True if the next non-space character is `(` (function call / declaration heuristic).
    #[inline]
    pub fn followed_by_paren(&self) -> bool {
        let mut i = self.pos;
        while i < self.bytes.len() && (self.bytes[i] == b' ' || self.bytes[i] == b'\t') {
            i += 1;
        }
        i < self.bytes.len() && self.bytes[i] == b'('
    }
}

#[inline]
pub fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

/// Heuristic: `PascalCase` identifiers (start uppercase + at least one lowercase) are usually Types.
#[inline]
pub fn is_pascal_case(word: &str) -> bool {
    let bytes = word.as_bytes();
    bytes.len() >= 2
        && bytes[0].is_ascii_uppercase()
        && bytes[1..].iter().any(|b| b.is_ascii_lowercase())
}

/// Heuristic: `SCREAMING_SNAKE` (len >= 2, all uppercase/digits/underscore, contains uppercase).
#[inline]
pub fn is_screaming_const(word: &str) -> bool {
    let bytes = word.as_bytes();
    bytes.len() >= 2
        && bytes[0].is_ascii_uppercase()
        && bytes
            .iter()
            .all(|&b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        && bytes.iter().any(|&b| b == b'_' || bytes.len() >= 3)
}

#[inline]
pub fn is_operator_byte(b: u8) -> bool {
    matches!(
        b,
        b'+' | b'-' | b'*' | b'/' | b'%' | b'=' | b'!' | b'<' | b'>' | b'&' | b'|' | b'^' | b'~' | b'?' | b':'
    )
}

#[inline]
pub fn is_punct_byte(b: u8) -> bool {
    matches!(b, b'(' | b')' | b'{' | b'}' | b'[' | b']' | b';' | b',' | b'.')
}

/// Parameterized fast C-family scanner used by clean C-family plugins (C, Java, Go, Swift, etc.).
pub struct CLikeSpec {
    pub nested_block_comments: bool,
    pub preprocessor_hash: bool,
    pub at_annotations: bool,
    pub triple_double_strings: bool,
    pub backtick_raw_strings: bool,
    pub apostrophe_num_sep: bool,
    pub is_keyword: fn(&str) -> bool,
    pub is_type: fn(&str) -> bool,
    pub is_constant: fn(&str) -> bool,
}

const MODE_BLOCK_COMMENT: u8 = 1;
const MODE_TRIPLE_STRING: u8 = 2;
const MODE_BACKTICK_STRING: u8 = 3;

pub fn lex_c_like(
    spec: &CLikeSpec,
    line: &str,
    state: LexState,
    out: &mut Vec<TokenSpan>,
) -> LexState {
    let mut cur = ByteCursor::new(line);

    // 1. Resume multi-line states.
    match state.mode() {
        MODE_BLOCK_COMMENT => {
            let rem = cur.scan_block_comment(state.depth().max(1), spec.nested_block_comments, out);
            if rem > 0 {
                return LexState::new(MODE_BLOCK_COMMENT, 0, rem, 0);
            }
        }
        MODE_TRIPLE_STRING => {
            if cur.scan_triple_string(b'"', true, true, out) {
                return LexState::new(MODE_TRIPLE_STRING, 0, 0, 0);
            }
        }
        MODE_BACKTICK_STRING => {
            let start = cur.pos;
            while !cur.is_eof() {
                if cur.peek() == Some(b'`') {
                    cur.advance(1);
                    push_span(out, start, cur.pos, TokenKind::String);
                    break;
                }
                cur.bump_char();
            }
            if cur.is_eof() && !line.ends_with('`') {
                push_span(out, start, cur.pos, TokenKind::String);
                return LexState::new(MODE_BACKTICK_STRING, 0, 0, 0);
            }
        }
        _ => {}
    }

    // 2. Preprocessor directive at start of line (`#include`, `#define`, ...).
    if spec.preprocessor_hash && cur.pos == 0 {
        let mut p = 0;
        while p < cur.len() && (cur.bytes[p] == b' ' || cur.bytes[p] == b'\t') {
            p += 1;
        }
        if p < cur.len() && cur.bytes[p] == b'#' {
            cur.pos = p;
            let start = cur.pos;
            cur.advance(1);
            cur.skip_ws();
            let (_, _, dir) = cur.scan_ident();
            push_span(out, start, cur.pos, TokenKind::Macro);
            if dir == "include" {
                cur.skip_ws();
                if cur.peek() == Some(b'<') {
                    let s = cur.pos;
                    while !cur.is_eof() && cur.peek() != Some(b'>') {
                        cur.bump_char();
                    }
                    if cur.peek() == Some(b'>') {
                        cur.advance(1);
                    }
                    push_span(out, s, cur.pos, TokenKind::String);
                }
            }
        }
    }

    // 3. Main scan loop.
    while !cur.is_eof() {
        let b = cur.bytes[cur.pos];
        if b == b' ' || b == b'\t' {
            cur.pos += 1;
        } else if b == b'/' && cur.peek_at(1) == Some(b'/') {
            push_span(out, cur.pos, cur.len(), TokenKind::Comment);
            return LexState::NORMAL;
        } else if b == b'/' && cur.peek_at(1) == Some(b'*') {
            let rem = cur.scan_block_comment(0, spec.nested_block_comments, out);
            if rem > 0 {
                return LexState::new(MODE_BLOCK_COMMENT, 0, rem, 0);
            }
        } else if spec.triple_double_strings && cur.starts_with(b"\"\"\"") {
            if cur.scan_triple_string(b'"', false, true, out) {
                return LexState::new(MODE_TRIPLE_STRING, 0, 0, 0);
            }
        } else if b == b'"' || b == b'\'' {
            cur.scan_quoted_string(b, true, out);
        } else if spec.backtick_raw_strings && b == b'`' {
            let start = cur.pos;
            cur.advance(1);
            let mut closed = false;
            while !cur.is_eof() {
                if cur.peek() == Some(b'`') {
                    cur.advance(1);
                    closed = true;
                    break;
                }
                cur.bump_char();
            }
            push_span(out, start, cur.pos, TokenKind::String);
            if !closed {
                return LexState::new(MODE_BACKTICK_STRING, 0, 0, 0);
            }
        } else if spec.at_annotations && b == b'@' {
            let start = cur.pos;
            cur.advance(1);
            cur.scan_ident();
            push_span(out, start, cur.pos, TokenKind::Macro);
        } else if b.is_ascii_digit() {
            cur.scan_number(spec.apostrophe_num_sep, out);
        } else if is_ident_start(b) {
            let (s, e, word) = cur.scan_ident();
            if (spec.is_keyword)(word) {
                push_span(out, s, e, TokenKind::Keyword);
            } else if (spec.is_constant)(word) {
                push_span(out, s, e, TokenKind::Constant);
            } else if (spec.is_type)(word) {
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
