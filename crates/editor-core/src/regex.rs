//! Pure-Rust, zero-dependency, linear-time Thompson NFA / PikeVM regex engine.
//!
//! Design invariants:
//! - Guaranteed `O(N * M)` worst-case runtime (immune to catastrophic backtracking / ReDoS).
//! - UTF-8 `char`-stepped matching: every match and capture boundary is guaranteed valid UTF-8.
//! - Iterative epsilon-closure with generation-stamped visited table: zero recursion, zero
//!   per-byte heap allocations when reusing [`PikeVmScratch`].
//! - Leading literal-prefix extraction accelerates real-world patterns via SWAR byte scanning.
//! - CRLF-aware `$` and `.` (`.` excludes `\n` and `\r`, `$` matches before `\r\n`, `\n`, or EOF).

use std::fmt;

use crate::swar::{
    find_byte, find_byte_ascii_ci, find_subslice_ascii_ci_rare, find_subslice_rare, RarePair,
};

/// Maximum number of capture groups (`$0` whole match + `$1`..`$9`).
pub const MAX_CAPTURES: usize = 10;
const MAX_SLOTS: usize = MAX_CAPTURES * 2;
const UNSET: usize = usize::MAX;
const MAX_INSTS: usize = 2048;
const MAX_REPEAT: usize = 256;
const MAX_PARSE_DEPTH: usize = 64;

/// Compile-time regex syntax or complexity error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegexError {
    pub message: String,
    pub offset: usize,
}

impl RegexError {
    fn new(offset: usize, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            offset,
        }
    }
}

impl fmt::Display for RegexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for RegexError {}

/// Capture byte spans for a single match (`$0` whole match + `$1`..`$9`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Captures {
    slots: [Option<(usize, usize)>; MAX_CAPTURES],
}

impl Captures {
    /// Whole match byte range `(start, end)` relative to the searched `&str`.
    #[inline]
    pub fn full(&self) -> (usize, usize) {
        self.slots[0].unwrap_or((0, 0))
    }

    /// Capture group `index` (`0` = full match, `1..=9` = parenthesized groups).
    #[inline]
    pub fn get(&self, index: usize) -> Option<(usize, usize)> {
        self.slots.get(index).copied().flatten()
    }

    /// Expand a replacement template (`$0`..`$9`, `$$`, `\n`, `\t`, `\r`, `\\`) against `haystack`.
    pub fn expand(&self, haystack: &str, template: &str) -> String {
        let mut out = String::with_capacity(template.len());
        let bytes = template.as_bytes();
        let mut i = 0usize;
        while i < bytes.len() {
            match bytes[i] {
                b'$' if i + 1 < bytes.len() => {
                    let next = bytes[i + 1];
                    if next == b'$' {
                        out.push('$');
                        i += 2;
                    } else if next.is_ascii_digit() {
                        let idx = (next - b'0') as usize;
                        if let Some((s, e)) = self.get(idx) {
                            if let Some(sub) = haystack.get(s..e) {
                                out.push_str(sub);
                            }
                        }
                        i += 2;
                    } else {
                        out.push('$');
                        i += 1;
                    }
                }
                b'\\' if i + 1 < bytes.len() => {
                    match bytes[i + 1] {
                        b'n' => {
                            out.push('\n');
                            i += 2;
                        }
                        b'r' => {
                            out.push('\r');
                            i += 2;
                        }
                        b't' => {
                            out.push('\t');
                            i += 2;
                        }
                        b'\\' => {
                            out.push('\\');
                            i += 2;
                        }
                        _ => {
                            out.push('\\');
                            i += 1;
                        }
                    }
                }
                _ => {
                    let ch = template[i..].chars().next().unwrap();
                    out.push(ch);
                    i += ch.len_utf8();
                }
            }
        }
        out
    }
}

/// Word character test shared by `\b`, `\B`, `\w`, and whole-word literal search.
#[inline]
pub fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || (!c.is_ascii() && c.is_alphanumeric())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CharClass {
    negated: bool,
    ascii_bits: [u64; 2],
    ranges: Vec<(char, char)>,
    unicode_word: bool,
    unicode_space: bool,
}

impl CharClass {
    fn new(negated: bool) -> Self {
        Self {
            negated,
            ascii_bits: [0; 2],
            ranges: Vec::new(),
            unicode_word: false,
            unicode_space: false,
        }
    }

    #[inline]
    fn set_ascii(&mut self, b: u8) {
        if b < 64 {
            self.ascii_bits[0] |= 1u64 << b;
        } else if b < 128 {
            self.ascii_bits[1] |= 1u64 << (b - 64);
        }
    }

    fn add_char(&mut self, c: char, case_sensitive: bool) {
        if c.is_ascii() {
            let b = c as u8;
            self.set_ascii(b);
            if !case_sensitive && b.is_ascii_alphabetic() {
                self.set_ascii(b.to_ascii_lowercase());
                self.set_ascii(b.to_ascii_uppercase());
            }
        } else {
            self.ranges.push((c, c));
        }
    }

    fn add_range(&mut self, start: char, end: char, case_sensitive: bool) {
        let (a, b) = if start <= end { (start, end) } else { (end, start) };
        if b.is_ascii() {
            for byte in (a as u8)..=(b as u8) {
                self.set_ascii(byte);
                if !case_sensitive && byte.is_ascii_alphabetic() {
                    self.set_ascii(byte.to_ascii_lowercase());
                    self.set_ascii(byte.to_ascii_uppercase());
                }
            }
        } else if a.is_ascii() {
            for byte in (a as u8)..=127 {
                self.set_ascii(byte);
                if !case_sensitive && byte.is_ascii_alphabetic() {
                    self.set_ascii(byte.to_ascii_lowercase());
                    self.set_ascii(byte.to_ascii_uppercase());
                }
            }
            self.ranges.push(('\u{80}', b));
        } else {
            self.ranges.push((a, b));
        }
    }

    fn add_digits(&mut self) {
        for b in b'0'..=b'9' {
            self.set_ascii(b);
        }
    }

    fn add_word(&mut self) {
        for b in b'0'..=b'9' {
            self.set_ascii(b);
        }
        for b in b'A'..=b'Z' {
            self.set_ascii(b);
        }
        for b in b'a'..=b'z' {
            self.set_ascii(b);
        }
        self.set_ascii(b'_');
        self.unicode_word = true;
    }

    fn add_space(&mut self) {
        for &b in b" \t\n\r\x0B\x0C" {
            self.set_ascii(b);
        }
        self.unicode_space = true;
    }

    fn digit_class(negated: bool) -> Self {
        let mut c = Self::new(negated);
        c.add_digits();
        c
    }

    fn word_class(negated: bool) -> Self {
        let mut c = Self::new(negated);
        c.add_word();
        c
    }

    fn space_class(negated: bool) -> Self {
        let mut c = Self::new(negated);
        c.add_space();
        c
    }

    #[inline]
    fn matches(&self, ch: char) -> bool {
        let hit = if ch.is_ascii() {
            let b = ch as u8;
            if b < 64 {
                (self.ascii_bits[0] & (1u64 << b)) != 0
            } else {
                (self.ascii_bits[1] & (1u64 << (b - 64))) != 0
            }
        } else {
            (self.unicode_word && ch.is_alphanumeric())
                || (self.unicode_space && ch.is_whitespace())
                || self.ranges.iter().any(|&(a, b)| ch >= a && ch <= b)
        };
        hit ^ self.negated
    }

    fn can_match_newline(&self) -> bool {
        self.matches('\n')
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AssertKind {
    StartLine,
    EndLine,
    WordBoundary,
    NotWordBoundary,
}

#[derive(Debug, Clone)]
enum Ast {
    Empty,
    Char(char),
    AnyNoNl,
    Class(CharClass),
    Assert(AssertKind),
    Concat(Vec<Ast>),
    Alt(Vec<Ast>),
    Repeat {
        inner: Box<Ast>,
        min: usize,
        max: Option<usize>,
        greedy: bool,
    },
    Group {
        index: Option<usize>,
        inner: Box<Ast>,
    },
}

struct Parser<'a> {
    src: &'a str,
    pos: usize,
    case_sensitive: bool,
    next_group: usize,
    depth: usize,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str, case_sensitive: bool) -> Self {
        Self {
            src,
            pos: 0,
            case_sensitive,
            next_group: 1,
            depth: 0,
        }
    }

    fn peek(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.pos += ch.len_utf8();
        Some(ch)
    }

    fn parse_all(mut self) -> Result<(Ast, usize), RegexError> {
        let ast = self.parse_alt()?;
        if let Some(')') = self.peek() {
            return Err(RegexError::new(self.pos, "Unmatched ')'"));
        }
        if self.pos < self.src.len() {
            return Err(RegexError::new(self.pos, "Unexpected trailing syntax"));
        }
        Ok((ast, self.next_group))
    }

    fn parse_alt(&mut self) -> Result<Ast, RegexError> {
        let mut branches = vec![self.parse_concat()?];
        while self.peek() == Some('|') {
            self.bump();
            branches.push(self.parse_concat()?);
        }
        if branches.len() == 1 {
            Ok(branches.pop().unwrap())
        } else {
            Ok(Ast::Alt(branches))
        }
    }

    fn parse_concat(&mut self) -> Result<Ast, RegexError> {
        let mut items = Vec::new();
        while let Some(ch) = self.peek() {
            if ch == '|' || ch == ')' {
                break;
            }
            items.push(self.parse_repeat()?);
        }
        match items.len() {
            0 => Ok(Ast::Empty),
            1 => Ok(items.pop().unwrap()),
            _ => Ok(Ast::Concat(items)),
        }
    }

    fn parse_repeat(&mut self) -> Result<Ast, RegexError> {
        let start_pos = self.pos;
        let mut atom = self.parse_atom()?;
        loop {
            let (min, max) = match self.peek() {
                Some('*') => {
                    self.bump();
                    (0, None)
                }
                Some('+') => {
                    self.bump();
                    (1, None)
                }
                Some('?') => {
                    self.bump();
                    (0, Some(1))
                }
                Some('{') => {
                    if let Some(bounds) = self.try_parse_counted_repeat()? {
                        bounds
                    } else {
                        break;
                    }
                }
                _ => break,
            };
            if matches!(atom, Ast::Empty | Ast::Assert(_)) {
                return Err(RegexError::new(start_pos, "Nothing to repeat"));
            }
            let greedy = if self.peek() == Some('?') {
                self.bump();
                false
            } else {
                true
            };
            atom = Ast::Repeat {
                inner: Box::new(atom),
                min,
                max,
                greedy,
            };
        }
        Ok(atom)
    }

    fn try_parse_counted_repeat(&mut self) -> Result<Option<(usize, Option<usize>)>, RegexError> {
        let saved = self.pos;
        self.bump(); // '{'
        let Some(min) = self.parse_uint() else {
            self.pos = saved;
            return Ok(None);
        };
        let max = match self.peek() {
            Some('}') => {
                self.bump();
                Some(min)
            }
            Some(',') => {
                self.bump();
                if self.peek() == Some('}') {
                    self.bump();
                    None
                } else if let Some(hi) = self.parse_uint() {
                    if self.peek() != Some('}') {
                        self.pos = saved;
                        return Ok(None);
                    }
                    self.bump();
                    if hi < min {
                        return Err(RegexError::new(saved, "Invalid repetition range"));
                    }
                    Some(hi)
                } else {
                    self.pos = saved;
                    return Ok(None);
                }
            }
            _ => {
                self.pos = saved;
                return Ok(None);
            }
        };
        if min > MAX_REPEAT || max.is_some_and(|m| m > MAX_REPEAT) {
            return Err(RegexError::new(saved, "Repetition count too large"));
        }
        Ok(Some((min, max)))
    }

    fn parse_uint(&mut self) -> Option<usize> {
        let start = self.pos;
        let mut val = 0usize;
        let mut saw = false;
        while let Some(c) = self.peek() {
            if let Some(d) = c.to_digit(10) {
                saw = true;
                val = val.saturating_mul(10).saturating_add(d as usize);
                self.bump();
            } else {
                break;
            }
        }
        if saw {
            Some(val)
        } else {
            self.pos = start;
            None
        }
    }

    fn parse_atom(&mut self) -> Result<Ast, RegexError> {
        let start = self.pos;
        let ch = self
            .bump()
            .ok_or_else(|| RegexError::new(start, "Unexpected end of pattern"))?;
        match ch {
            '*' | '+' | '?' => Err(RegexError::new(start, "Nothing to repeat")),
            '.' => Ok(Ast::AnyNoNl),
            '^' => Ok(Ast::Assert(AssertKind::StartLine)),
            '$' => Ok(Ast::Assert(AssertKind::EndLine)),
            '(' => {
                if self.depth >= MAX_PARSE_DEPTH {
                    return Err(RegexError::new(start, "Pattern nesting too deep"));
                }
                self.depth += 1;
                let index = if self.src[self.pos..].starts_with("?:") {
                    self.pos += 2;
                    None
                } else if self.next_group < MAX_CAPTURES {
                    let g = self.next_group;
                    self.next_group += 1;
                    Some(g)
                } else {
                    None
                };
                let inner = self.parse_alt()?;
                self.depth -= 1;
                if self.bump() != Some(')') {
                    return Err(RegexError::new(start, "Unclosed group '('"));
                }
                Ok(Ast::Group {
                    index,
                    inner: Box::new(inner),
                })
            }
            '[' => self.parse_class(start),
            '\\' => self.parse_escape(start),
            other => Ok(Ast::Char(other)),
        }
    }

    fn parse_escape(&mut self, start: usize) -> Result<Ast, RegexError> {
        let esc = self
            .bump()
            .ok_or_else(|| RegexError::new(start, "Trailing backslash"))?;
        match esc {
            'd' => Ok(Ast::Class(CharClass::digit_class(false))),
            'D' => Ok(Ast::Class(CharClass::digit_class(true))),
            'w' => Ok(Ast::Class(CharClass::word_class(false))),
            'W' => Ok(Ast::Class(CharClass::word_class(true))),
            's' => Ok(Ast::Class(CharClass::space_class(false))),
            'S' => Ok(Ast::Class(CharClass::space_class(true))),
            'b' => Ok(Ast::Assert(AssertKind::WordBoundary)),
            'B' => Ok(Ast::Assert(AssertKind::NotWordBoundary)),
            'n' => Ok(Ast::Char('\n')),
            'r' => Ok(Ast::Char('\r')),
            't' => Ok(Ast::Char('\t')),
            other => Ok(Ast::Char(other)),
        }
    }

    fn parse_class(&mut self, start: usize) -> Result<Ast, RegexError> {
        let negated = if self.peek() == Some('^') {
            self.bump();
            true
        } else {
            false
        };
        let mut cls = CharClass::new(negated);
        let mut first = true;
        let mut prev_literal: Option<char> = None;

        while let Some(ch) = self.bump() {
            if ch == ']' && !first {
                return Ok(Ast::Class(cls));
            }
            first = false;

            if ch == '-' && prev_literal.is_some() && self.peek() != Some(']') && self.peek().is_some()
            {
                let range_start = prev_literal.take().unwrap();
                let range_end = match self.bump().unwrap() {
                    '\\' => self.parse_class_escape_char(start)?,
                    c => c,
                };
                if range_end < range_start {
                    return Err(RegexError::new(start, "Invalid character class range"));
                }
                cls.add_range(range_start, range_end, self.case_sensitive);
                continue;
            }

            if ch == '\\' {
                let esc = self
                    .bump()
                    .ok_or_else(|| RegexError::new(start, "Unclosed character class '['"))?;
                match esc {
                    'd' => {
                        cls.add_digits();
                        prev_literal = None;
                    }
                    'D' => {
                        // Add all non-digits to this class.
                        for b in 0u8..128 {
                            if !b.is_ascii_digit() {
                                cls.set_ascii(b);
                            }
                        }
                        cls.ranges.push(('\u{80}', char::MAX));
                        prev_literal = None;
                    }
                    'w' => {
                        cls.add_word();
                        prev_literal = None;
                    }
                    'W' => {
                        for b in 0u8..128 {
                            if !(b.is_ascii_alphanumeric() || b == b'_') {
                                cls.set_ascii(b);
                            }
                        }
                        prev_literal = None;
                    }
                    's' => {
                        cls.add_space();
                        prev_literal = None;
                    }
                    'S' => {
                        for b in 0u8..128 {
                            if !matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C) {
                                cls.set_ascii(b);
                            }
                        }
                        prev_literal = None;
                    }
                    'n' => {
                        cls.add_char('\n', self.case_sensitive);
                        prev_literal = Some('\n');
                    }
                    'r' => {
                        cls.add_char('\r', self.case_sensitive);
                        prev_literal = Some('\r');
                    }
                    't' => {
                        cls.add_char('\t', self.case_sensitive);
                        prev_literal = Some('\t');
                    }
                    other => {
                        cls.add_char(other, self.case_sensitive);
                        prev_literal = Some(other);
                    }
                }
            } else {
                cls.add_char(ch, self.case_sensitive);
                prev_literal = Some(ch);
            }
        }
        Err(RegexError::new(start, "Unclosed character class '['"))
    }

    fn parse_class_escape_char(&mut self, start: usize) -> Result<char, RegexError> {
        let esc = self
            .bump()
            .ok_or_else(|| RegexError::new(start, "Unclosed character class '['"))?;
        Ok(match esc {
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            other => other,
        })
    }
}

#[derive(Debug, Clone)]
enum Inst {
    Char(char),
    CharCi(u8),
    AnyNoNl,
    Class(CharClass),
    Assert(AssertKind),
    Split(usize, usize),
    Jmp(usize),
    Save(u8),
    Match,
}

struct Compiler {
    insts: Vec<Inst>,
    case_sensitive: bool,
}

impl Compiler {
    fn new(case_sensitive: bool) -> Self {
        Self {
            insts: Vec::with_capacity(64),
            case_sensitive,
        }
    }

    fn emit(&mut self, inst: Inst) -> Result<usize, RegexError> {
        if self.insts.len() >= MAX_INSTS {
            return Err(RegexError::new(0, "Regex pattern too complex"));
        }
        let idx = self.insts.len();
        self.insts.push(inst);
        Ok(idx)
    }

    fn compile_ast(&mut self, ast: &Ast) -> Result<(), RegexError> {
        match ast {
            Ast::Empty => Ok(()),
            Ast::Char(c) => {
                if !self.case_sensitive && c.is_ascii_alphabetic() {
                    self.emit(Inst::CharCi((*c as u8).to_ascii_lowercase()))?;
                } else {
                    self.emit(Inst::Char(*c))?;
                }
                Ok(())
            }
            Ast::AnyNoNl => {
                self.emit(Inst::AnyNoNl)?;
                Ok(())
            }
            Ast::Class(cls) => {
                self.emit(Inst::Class(cls.clone()))?;
                Ok(())
            }
            Ast::Assert(kind) => {
                self.emit(Inst::Assert(*kind))?;
                Ok(())
            }
            Ast::Concat(items) => {
                for item in items {
                    self.compile_ast(item)?;
                }
                Ok(())
            }
            Ast::Group { index, inner } => {
                if let Some(g) = index {
                    self.emit(Inst::Save((*g * 2) as u8))?;
                    self.compile_ast(inner)?;
                    self.emit(Inst::Save((*g * 2 + 1) as u8))?;
                } else {
                    self.compile_ast(inner)?;
                }
                Ok(())
            }
            Ast::Alt(branches) => {
                if branches.is_empty() {
                    return Ok(());
                }
                let mut end_jmps = Vec::with_capacity(branches.len());
                for (idx, branch) in branches.iter().enumerate() {
                    if idx + 1 < branches.len() {
                        let split_pc = self.emit(Inst::Split(0, 0))?;
                        let branch_start = self.insts.len();
                        self.compile_ast(branch)?;
                        let jmp_pc = self.emit(Inst::Jmp(0))?;
                        end_jmps.push(jmp_pc);
                        let next_branch = self.insts.len();
                        self.insts[split_pc] = Inst::Split(branch_start, next_branch);
                    } else {
                        self.compile_ast(branch)?;
                    }
                }
                let after = self.insts.len();
                for j in end_jmps {
                    self.insts[j] = Inst::Jmp(after);
                }
                Ok(())
            }
            Ast::Repeat {
                inner,
                min,
                max,
                greedy,
            } => {
                // Emit `min` mandatory copies.
                for _ in 0..*min {
                    self.compile_ast(inner)?;
                }
                match *max {
                    None => {
                        // Remaining `0..inf` (Kleene star):
                        let split_pc = self.emit(Inst::Split(0, 0))?;
                        let body_start = self.insts.len();
                        self.compile_ast(inner)?;
                        self.emit(Inst::Jmp(split_pc))?;
                        let after = self.insts.len();
                        self.insts[split_pc] = if *greedy {
                            Inst::Split(body_start, after)
                        } else {
                            Inst::Split(after, body_start)
                        };
                    }
                    Some(hi) => {
                        // Emit `hi - min` optional copies.
                        for _ in *min..hi {
                            let split_pc = self.emit(Inst::Split(0, 0))?;
                            let body_start = self.insts.len();
                            self.compile_ast(inner)?;
                            let after = self.insts.len();
                            self.insts[split_pc] = if *greedy {
                                Inst::Split(body_start, after)
                            } else {
                                Inst::Split(after, body_start)
                            };
                        }
                    }
                }
                Ok(())
            }
        }
    }
}

/// Extract a guaranteed literal byte prefix (exact UTF-8 when case-sensitive, or lowercased ASCII
/// when case-insensitive) so both the PikeVM and line-prefiltered search can skip at GB/s speed.
fn extract_literal_prefix(ast: &Ast, case_sensitive: bool, out: &mut Vec<u8>) -> bool {
    const MAX_PREFIX: usize = 32;
    if out.len() >= MAX_PREFIX {
        return false;
    }
    match ast {
        Ast::Char(c) => {
            if !case_sensitive {
                if !c.is_ascii() {
                    return false;
                }
                if out.len() + 1 <= MAX_PREFIX {
                    out.push((*c as u8).to_ascii_lowercase());
                    return true;
                }
                return false;
            }
            let mut buf = [0u8; 4];
            let bytes = c.encode_utf8(&mut buf).as_bytes();
            if out.len() + bytes.len() <= MAX_PREFIX {
                out.extend_from_slice(bytes);
                true
            } else {
                false
            }
        }
        Ast::Group { inner, .. } => extract_literal_prefix(inner, case_sensitive, out),
        Ast::Concat(items) => {
            for item in items {
                if !extract_literal_prefix(item, case_sensitive, out) {
                    return false;
                }
            }
            true
        }
        Ast::Repeat { inner, min, .. } if *min >= 1 => {
            for _ in 0..*min {
                if !extract_literal_prefix(inner, case_sensitive, out) {
                    return false;
                }
            }
            false
        }
        Ast::Assert(_) | Ast::Empty => true,
        _ => false,
    }
}

/// Extract the longest mandatory contiguous literal substring anywhere in a top-level `Concat`
/// (e.g. `"unique_needle_"` from `pub\s+fn\s+unique_needle_\d+`) for ripgrep-style line prefiltering.
fn extract_longest_required_literal(ast: &Ast, case_sensitive: bool) -> Vec<u8> {
    const MAX_LIT: usize = 32;
    fn collect_runs(ast: &Ast, case_sensitive: bool, cur: &mut Vec<u8>, best: &mut Vec<u8>) {
        let flush = |cur: &mut Vec<u8>, best: &mut Vec<u8>| {
            if cur.len() > best.len() {
                best.clear();
                best.extend_from_slice(cur);
            }
            cur.clear();
        };
        match ast {
            Ast::Char(c) => {
                if !case_sensitive {
                    if c.is_ascii() && cur.len() < MAX_LIT {
                        cur.push((*c as u8).to_ascii_lowercase());
                    } else {
                        flush(cur, best);
                    }
                } else {
                    let mut buf = [0u8; 4];
                    let bytes = c.encode_utf8(&mut buf).as_bytes();
                    if cur.len() + bytes.len() <= MAX_LIT {
                        cur.extend_from_slice(bytes);
                    } else {
                        flush(cur, best);
                    }
                }
            }
            Ast::Group { inner, .. } => collect_runs(inner, case_sensitive, cur, best),
            Ast::Concat(items) => {
                for item in items {
                    collect_runs(item, case_sensitive, cur, best);
                }
            }
            Ast::Repeat { inner, min, .. } if *min >= 1 => {
                // `min >= 1` guarantees `inner` occurs at least once, then breaks contiguity.
                collect_runs(inner, case_sensitive, cur, best);
                flush(cur, best);
            }
            Ast::Assert(_) | Ast::Empty => {}
            _ => flush(cur, best),
        }
    }
    let mut cur = Vec::new();
    let mut best = Vec::new();
    collect_runs(ast, case_sensitive, &mut cur, &mut best);
    if cur.len() > best.len() {
        best = cur;
    }
    best
}

/// Extract per-branch mandatory literals for a top-level alternation (`foo|bar|baz`, 2..=8 branches)
/// so alternations also get multi-GB/s SWAR prefiltering!
fn extract_alt_literals(ast: &Ast, case_sensitive: bool) -> Vec<(Vec<u8>, RarePair)> {
    const MAX_ALT_BRANCHES: usize = 8;
    fn unwrap_alt(ast: &Ast) -> Option<&[Ast]> {
        match ast {
            Ast::Alt(branches) => Some(branches),
            Ast::Group { inner, .. } => unwrap_alt(inner),
            Ast::Concat(items) => {
                // Handle whole-word wrapper `\b(?:foo|bar)\b` (single non-assert child).
                let mut single = None;
                for item in items {
                    if matches!(item, Ast::Assert(_) | Ast::Empty) {
                        continue;
                    }
                    if single.is_some() {
                        return None;
                    }
                    single = Some(item);
                }
                single.and_then(unwrap_alt)
            }
            _ => None,
        }
    }
    let Some(branches) = unwrap_alt(ast) else {
        return Vec::new();
    };
    if branches.len() < 2 || branches.len() > MAX_ALT_BRANCHES {
        return Vec::new();
    }
    let mut out: Vec<(Vec<u8>, RarePair)> = Vec::with_capacity(branches.len());
    for branch in branches {
        let lit = extract_longest_required_literal(branch, case_sensitive);
        if lit.is_empty() {
            return Vec::new();
        }
        if !out.iter().any(|(existing, _)| existing == &lit) {
            let rare = RarePair::from_needle(&lit);
            out.push((lit, rare));
        }
    }
    out
}

/// 128-bit ASCII start-byte bitmap for fast-skipping unprefixed regexes (e.g. `\d+`, `[A-Z]\w+`).
#[derive(Debug, Clone, Copy)]
struct StartFilter {
    ascii_bits: [u64; 2],
    allow_non_ascii: bool,
}

impl StartFilter {
    #[inline(always)]
    fn allows_byte(&self, b: u8) -> bool {
        if b < 64 {
            (self.ascii_bits[0] & (1u64 << b)) != 0
        } else if b < 128 {
            (self.ascii_bits[1] & (1u64 << (b - 64))) != 0
        } else {
            self.allow_non_ascii
        }
    }

    #[inline]
    fn union(self, other: Self) -> Self {
        Self {
            ascii_bits: [
                self.ascii_bits[0] | other.ascii_bits[0],
                self.ascii_bits[1] | other.ascii_bits[1],
            ],
            allow_non_ascii: self.allow_non_ascii || other.allow_non_ascii,
        }
    }

    #[inline]
    fn is_selective(self) -> bool {
        self.ascii_bits[0] != u64::MAX || self.ascii_bits[1] != u64::MAX
    }
}

fn extract_start_filter(ast: &Ast, case_sensitive: bool) -> Option<StartFilter> {
    match ast {
        Ast::Empty | Ast::Assert(_) | Ast::AnyNoNl => None,
        Ast::Char(c) => {
            if c.is_ascii() {
                let mut bits = [0u64; 2];
                let set = |bits: &mut [u64; 2], b: u8| {
                    if b < 64 {
                        bits[0] |= 1u64 << b;
                    } else {
                        bits[1] |= 1u64 << (b - 64);
                    }
                };
                let b = *c as u8;
                set(&mut bits, b);
                if !case_sensitive && b.is_ascii_alphabetic() {
                    set(&mut bits, b.to_ascii_lowercase());
                    set(&mut bits, b.to_ascii_uppercase());
                }
                Some(StartFilter {
                    ascii_bits: bits,
                    allow_non_ascii: false,
                })
            } else {
                Some(StartFilter {
                    ascii_bits: [0, 0],
                    allow_non_ascii: true,
                })
            }
        }
        Ast::Class(cls) => {
            let sf = if cls.negated {
                StartFilter {
                    ascii_bits: [!cls.ascii_bits[0], !cls.ascii_bits[1]],
                    allow_non_ascii: true,
                }
            } else {
                StartFilter {
                    ascii_bits: cls.ascii_bits,
                    allow_non_ascii: cls.unicode_word
                        || cls.unicode_space
                        || !cls.ranges.is_empty(),
                }
            };
            sf.is_selective().then_some(sf)
        }
        Ast::Group { inner, .. } => extract_start_filter(inner, case_sensitive),
        Ast::Repeat { inner, min, .. } if *min >= 1 => extract_start_filter(inner, case_sensitive),
        Ast::Concat(items) => {
            for item in items {
                if matches!(item, Ast::Empty | Ast::Assert(_)) {
                    continue;
                }
                return extract_start_filter(item, case_sensitive);
            }
            None
        }
        Ast::Alt(branches) => {
            let mut acc: Option<StartFilter> = None;
            for b in branches {
                let sf = extract_start_filter(b, case_sensitive)?;
                acc = Some(match acc {
                    Some(prev) => prev.union(sf),
                    None => sf,
                });
            }
            acc.filter(|sf| sf.is_selective())
        }
        _ => None,
    }
}

/// Reusable PikeVM execution scratch space (zero allocations per line or file).
/// Capture-free span searches (`find_span_with_context`) only touch the compact 16-byte `span_*`
/// tables; full capture arrays (`cap_*`) stay unallocated until `expand_replacement` needs them.
#[derive(Default)]
pub struct PikeVmScratch {
    span_curr: Vec<(u32, usize)>,
    span_next: Vec<(u32, usize)>,
    span_stack: Vec<(u32, usize)>,
    cap_curr: Vec<(u32, [usize; MAX_SLOTS])>,
    cap_next: Vec<(u32, [usize; MAX_SLOTS])>,
    cap_stack: Vec<(u32, [usize; MAX_SLOTS])>,
    visited: Vec<u32>,
    gen: u32,
}

impl PikeVmScratch {
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    fn ensure_len(&mut self, n: usize) {
        if self.visited.len() < n {
            self.visited.resize(n, 0);
        }
    }

    #[inline]
    fn next_gen(&mut self) -> u32 {
        self.gen = self.gen.wrapping_add(1);
        if self.gen == 0 {
            self.visited.fill(0);
            self.gen = 1;
        }
        self.gen
    }
}

/// Compiled linear-time regular expression.
#[derive(Debug, Clone)]
pub struct Regex {
    insts: Vec<Inst>,
    literal_prefix: Vec<u8>,
    prefix_rare: RarePair,
    /// Longest mandatory literal anywhere in the pattern (for SWAR line prefiltering).
    line_literal: Vec<u8>,
    line_rare: RarePair,
    /// Top-level alternation mandatory literals (`foo|bar|baz`) when `line_literal` is empty.
    alt_literals: Vec<(Vec<u8>, RarePair)>,
    /// 128-bit ASCII start-byte bitmap for skipping non-matching bytes when unprefixed.
    start_filter: Option<StartFilter>,
    case_sensitive: bool,
    can_match_newline: bool,
    has_assert: bool,
}

impl Regex {
    /// Compile `pattern`. When `whole_word` is true, wraps the pattern in `\b(?:...)\b`.
    pub fn compile(
        pattern: &str,
        case_sensitive: bool,
        whole_word: bool,
    ) -> Result<Self, RegexError> {
        let (parsed, _groups) = Parser::new(pattern, case_sensitive).parse_all()?;
        let root = if whole_word {
            Ast::Concat(vec![
                Ast::Assert(AssertKind::WordBoundary),
                Ast::Group {
                    index: None,
                    inner: Box::new(parsed),
                },
                Ast::Assert(AssertKind::WordBoundary),
            ])
        } else {
            parsed
        };

        let mut literal_prefix = Vec::new();
        extract_literal_prefix(&root, case_sensitive, &mut literal_prefix);
        let mut line_literal = extract_longest_required_literal(&root, case_sensitive);
        if literal_prefix.len() >= line_literal.len() {
            line_literal = literal_prefix.clone();
        }
        let alt_literals = if line_literal.is_empty() {
            extract_alt_literals(&root, case_sensitive)
        } else {
            Vec::new()
        };
        let prefix_rare = RarePair::from_needle(&literal_prefix);
        let line_rare = RarePair::from_needle(&line_literal);
        let start_filter = if literal_prefix.is_empty() {
            extract_start_filter(&root, case_sensitive)
        } else {
            None
        };

        let mut compiler = Compiler::new(case_sensitive);
        compiler.emit(Inst::Save(0))?;
        compiler.compile_ast(&root)?;
        compiler.emit(Inst::Save(1))?;
        compiler.emit(Inst::Match)?;

        let can_match_newline = compiler.insts.iter().any(|inst| match inst {
            Inst::Char('\n') => true,
            Inst::Class(cls) => cls.can_match_newline(),
            _ => false,
        });
        let has_assert = compiler
            .insts
            .iter()
            .any(|inst| matches!(inst, Inst::Assert(_)));

        Ok(Self {
            insts: compiler.insts,
            literal_prefix,
            prefix_rare,
            line_literal,
            line_rare,
            alt_literals,
            start_filter,
            case_sensitive,
            can_match_newline,
            has_assert,
        })
    }

    /// True if any instruction in the compiled regex can consume `'\n'`.
    #[inline]
    pub fn can_match_newline(&self) -> bool {
        self.can_match_newline
    }

    /// True if this regex has a SWAR line/chunk prefilter (`line_literal` or `alt_literals`).
    #[inline]
    pub fn has_line_prefilter(&self) -> bool {
        !self.line_literal.is_empty() || !self.alt_literals.is_empty()
    }

    /// Longest mandatory literal substring (exact when `case_sensitive`, lowercased ASCII otherwise).
    #[inline]
    pub fn line_literal(&self) -> (&[u8], bool) {
        (&self.line_literal, self.case_sensitive)
    }

    /// Fast SWAR search for the next candidate offset of `line_literal` or `alt_literals` in `bytes[pos..]`.
    #[inline]
    pub fn find_line_candidate(&self, bytes: &[u8], pos: usize) -> Option<usize> {
        if !self.line_literal.is_empty() {
            return find_lit_swar(bytes, pos, &self.line_literal, self.line_rare, self.case_sensitive);
        }
        if self.alt_literals.is_empty() || pos >= bytes.len() {
            return None;
        }
        let mut earliest: Option<usize> = None;
        for (lit, rare) in &self.alt_literals {
            // Shrink search window to `earliest` so an early hit in branch 0 avoids scanning the rest!
            let limit = earliest
                .map_or(bytes.len(), |e| e.saturating_add(lit.len().saturating_sub(1)).min(bytes.len()));
            if let Some(hit) = find_lit_swar(&bytes[..limit], pos, lit, *rare, self.case_sensitive) {
                if earliest.is_none_or(|e| hit < e) {
                    earliest = Some(hit);
                    if hit == pos {
                        break;
                    }
                }
            }
        }
        earliest
    }

    /// Fast SWAR search for the next candidate offset of `literal_prefix` in `bytes[pos..]`.
    #[inline]
    pub fn find_prefix_candidate(&self, bytes: &[u8], pos: usize) -> Option<usize> {
        find_lit_swar(
            bytes,
            pos,
            &self.literal_prefix,
            self.prefix_rare,
            self.case_sensitive,
        )
    }

    /// Fast capture-free PikeVM returning only `(start, end)` (uses 16-byte threads instead of
    /// 168-byte capture threads, skips `Save` ops, and uses `start_filter` when unprefixed).
    /// Produces the exact same leftmost-first match span as [`Self::find_captures_with_context`].
    pub fn find_span_with_context(
        &self,
        haystack: &str,
        from_byte: usize,
        prev_before_slice: Option<char>,
        next_after_slice: Option<char>,
        scratch: &mut PikeVmScratch,
    ) -> Option<(usize, usize)> {
        if from_byte > haystack.len() || !haystack.is_char_boundary(from_byte) {
            return None;
        }
        let bytes = haystack.as_bytes();
        let has_assert = self.has_assert;
        scratch.ensure_len(self.insts.len());
        scratch.span_curr.clear();
        scratch.span_next.clear();

        let mut pos = from_byte;
        let mut prev_ch = if !has_assert {
            None
        } else if pos > 0 {
            haystack[..pos].chars().next_back()
        } else {
            prev_before_slice
        };
        let mut curr_gen = scratch.next_gen();
        let mut best: Option<(usize, usize)> = None;

        loop {
            // Fast-skip when no candidate match is currently active.
            if best.is_none() && scratch.span_curr.is_empty() && pos < bytes.len() {
                if !self.literal_prefix.is_empty() {
                    if let Some(cand) = self.find_prefix_candidate(bytes, pos) {
                        if cand > pos {
                            pos = cand;
                            if has_assert {
                                prev_ch = haystack[..pos].chars().next_back();
                            }
                            curr_gen = scratch.next_gen();
                        }
                    } else {
                        return None;
                    }
                } else if let Some(sf) = self.start_filter {
                    let old_pos = pos;
                    while pos < bytes.len() && !sf.allows_byte(bytes[pos]) {
                        pos += 1;
                    }
                    if pos >= bytes.len() {
                        return None;
                    }
                    if pos > old_pos {
                        if has_assert {
                            prev_ch = haystack[..pos].chars().next_back();
                        }
                        curr_gen = scratch.next_gen();
                    }
                }
            }

            let cur_ch = if pos < bytes.len() {
                let b = bytes[pos];
                if b < 0x80 {
                    Some(b as char)
                } else {
                    haystack[pos..].chars().next()
                }
            } else {
                None
            };
            let next_pos = match cur_ch {
                Some(c) => pos + c.len_utf8(),
                None => pos,
            };

            let (lookahead, after_lookahead) = if has_assert {
                let la = cur_ch.or(next_after_slice);
                let after = if cur_ch.is_some() {
                    bytes.get(next_pos).copied()
                } else {
                    None
                };
                (la, after)
            } else {
                (None, None)
            };

            // Spawn fresh candidate starting at `pos` (lower priority than existing threads).
            if best.is_none() {
                add_span_thread(
                    &self.insts,
                    &mut scratch.span_curr,
                    &mut scratch.span_stack,
                    &mut scratch.visited,
                    curr_gen,
                    1, // Skip leading `Inst::Save(0)` at pc 0; `start = pos`.
                    pos,
                    prev_ch,
                    lookahead,
                    after_lookahead,
                );
            }

            let next_gen = scratch.next_gen();
            scratch.span_next.clear();

            let (next_ch, after_next_byte) = if has_assert && cur_ch.is_some() {
                let nch = if next_pos < bytes.len() {
                    let b = bytes[next_pos];
                    if b < 0x80 {
                        Some(b as char)
                    } else {
                        haystack[next_pos..].chars().next()
                    }
                } else {
                    next_after_slice
                };
                let anb = if next_pos < bytes.len() {
                    let step = if bytes[next_pos] < 0x80 {
                        1
                    } else {
                        haystack[next_pos..].chars().next().map_or(0, char::len_utf8)
                    };
                    bytes.get(next_pos + step).copied()
                } else {
                    None
                };
                (nch, anb)
            } else {
                (None, None)
            };

            let curr_len = scratch.span_curr.len();
            for i in 0..curr_len {
                let (pc, start) = scratch.span_curr[i];
                match &self.insts[pc as usize] {
                    Inst::Match => {
                        best = Some((start, pos));
                        break;
                    }
                    Inst::Char(c) => {
                        if cur_ch == Some(*c) {
                            add_span_thread(
                                &self.insts,
                                &mut scratch.span_next,
                                &mut scratch.span_stack,
                                &mut scratch.visited,
                                next_gen,
                                pc + 1,
                                start,
                                cur_ch,
                                next_ch,
                                after_next_byte,
                            );
                        }
                    }
                    Inst::CharCi(lo) => {
                        if let Some(ch) = cur_ch {
                            if ch.is_ascii() && (ch as u8).to_ascii_lowercase() == *lo {
                                add_span_thread(
                                    &self.insts,
                                    &mut scratch.span_next,
                                    &mut scratch.span_stack,
                                    &mut scratch.visited,
                                    next_gen,
                                    pc + 1,
                                    start,
                                    cur_ch,
                                    next_ch,
                                    after_next_byte,
                                );
                            }
                        }
                    }
                    Inst::AnyNoNl => {
                        if let Some(ch) = cur_ch {
                            if ch != '\n' && ch != '\r' {
                                add_span_thread(
                                    &self.insts,
                                    &mut scratch.span_next,
                                    &mut scratch.span_stack,
                                    &mut scratch.visited,
                                    next_gen,
                                    pc + 1,
                                    start,
                                    cur_ch,
                                    next_ch,
                                    after_next_byte,
                                );
                            }
                        }
                    }
                    Inst::Class(cls) => {
                        if let Some(ch) = cur_ch {
                            if cls.matches(ch) {
                                add_span_thread(
                                    &self.insts,
                                    &mut scratch.span_next,
                                    &mut scratch.span_stack,
                                    &mut scratch.visited,
                                    next_gen,
                                    pc + 1,
                                    start,
                                    cur_ch,
                                    next_ch,
                                    after_next_byte,
                                );
                            }
                        }
                    }
                    _ => {}
                }
            }

            std::mem::swap(&mut scratch.span_curr, &mut scratch.span_next);
            curr_gen = next_gen;

            if best.is_some() && scratch.span_curr.is_empty() {
                break;
            }
            if cur_ch.is_none() {
                break;
            }
            if has_assert {
                prev_ch = cur_ch;
            }
            pos = next_pos;
        }

        best
    }

    /// Find the first match (with captures `$0`..`$9`) in `haystack` starting at or after `from_byte`.
    /// `prev_before_slice` and `next_after_slice` supply boundary characters for windowed scans.
    pub fn find_captures_with_context(
        &self,
        haystack: &str,
        from_byte: usize,
        prev_before_slice: Option<char>,
        next_after_slice: Option<char>,
        scratch: &mut PikeVmScratch,
    ) -> Option<Captures> {
        if from_byte > haystack.len() || !haystack.is_char_boundary(from_byte) {
            return None;
        }
        let bytes = haystack.as_bytes();
        scratch.ensure_len(self.insts.len());
        scratch.cap_curr.clear();
        scratch.cap_next.clear();

        let mut pos = from_byte;
        let mut prev_ch = if pos > 0 {
            haystack[..pos].chars().next_back()
        } else {
            prev_before_slice
        };
        let mut curr_gen = scratch.next_gen();
        let mut best: Option<[usize; MAX_SLOTS]> = None;

        loop {
            if best.is_none() && scratch.cap_curr.is_empty() && pos < bytes.len() {
                if !self.literal_prefix.is_empty() {
                    if let Some(cand) = self.find_prefix_candidate(bytes, pos) {
                        if cand > pos {
                            pos = cand;
                            prev_ch = haystack[..pos].chars().next_back();
                            curr_gen = scratch.next_gen();
                        }
                    } else {
                        return None;
                    }
                } else if let Some(sf) = self.start_filter {
                    let old_pos = pos;
                    while pos < bytes.len() && !sf.allows_byte(bytes[pos]) {
                        pos += 1;
                    }
                    if pos >= bytes.len() {
                        return None;
                    }
                    if pos > old_pos {
                        prev_ch = haystack[..pos].chars().next_back();
                        curr_gen = scratch.next_gen();
                    }
                }
            }

            let cur_ch = if pos < bytes.len() {
                let b = bytes[pos];
                if b < 0x80 {
                    Some(b as char)
                } else {
                    haystack[pos..].chars().next()
                }
            } else {
                None
            };
            let next_pos = match cur_ch {
                Some(c) => pos + c.len_utf8(),
                None => pos,
            };
            let lookahead = cur_ch.or(next_after_slice);
            let after_lookahead = if cur_ch.is_some() {
                bytes.get(next_pos).copied()
            } else {
                None
            };

            if best.is_none() {
                add_cap_thread(
                    &self.insts,
                    &mut scratch.cap_curr,
                    &mut scratch.cap_stack,
                    &mut scratch.visited,
                    curr_gen,
                    0,
                    [UNSET; MAX_SLOTS],
                    pos,
                    prev_ch,
                    lookahead,
                    after_lookahead,
                );
            }

            let next_gen = scratch.next_gen();
            scratch.cap_next.clear();

            let next_ch = if cur_ch.is_some() {
                if next_pos < bytes.len() {
                    let b = bytes[next_pos];
                    if b < 0x80 {
                        Some(b as char)
                    } else {
                        haystack[next_pos..].chars().next()
                    }
                } else {
                    next_after_slice
                }
            } else {
                None
            };
            let after_next_byte = if cur_ch.is_some() && next_pos < bytes.len() {
                let step = if bytes[next_pos] < 0x80 {
                    1
                } else {
                    haystack[next_pos..].chars().next().map_or(0, char::len_utf8)
                };
                bytes.get(next_pos + step).copied()
            } else {
                None
            };

            let curr_len = scratch.cap_curr.len();
            for i in 0..curr_len {
                let (pc, caps) = scratch.cap_curr[i];
                match &self.insts[pc as usize] {
                    Inst::Match => {
                        best = Some(caps);
                        break;
                    }
                    Inst::Char(c) => {
                        if cur_ch == Some(*c) {
                            add_cap_thread(
                                &self.insts,
                                &mut scratch.cap_next,
                                &mut scratch.cap_stack,
                                &mut scratch.visited,
                                next_gen,
                                pc + 1,
                                caps,
                                next_pos,
                                cur_ch,
                                next_ch,
                                after_next_byte,
                            );
                        }
                    }
                    Inst::CharCi(lo) => {
                        if let Some(ch) = cur_ch {
                            if ch.is_ascii() && (ch as u8).to_ascii_lowercase() == *lo {
                                add_cap_thread(
                                    &self.insts,
                                    &mut scratch.cap_next,
                                    &mut scratch.cap_stack,
                                    &mut scratch.visited,
                                    next_gen,
                                    pc + 1,
                                    caps,
                                    next_pos,
                                    cur_ch,
                                    next_ch,
                                    after_next_byte,
                                );
                            }
                        }
                    }
                    Inst::AnyNoNl => {
                        if let Some(ch) = cur_ch {
                            if ch != '\n' && ch != '\r' {
                                add_cap_thread(
                                    &self.insts,
                                    &mut scratch.cap_next,
                                    &mut scratch.cap_stack,
                                    &mut scratch.visited,
                                    next_gen,
                                    pc + 1,
                                    caps,
                                    next_pos,
                                    cur_ch,
                                    next_ch,
                                    after_next_byte,
                                );
                            }
                        }
                    }
                    Inst::Class(cls) => {
                        if let Some(ch) = cur_ch {
                            if cls.matches(ch) {
                                add_cap_thread(
                                    &self.insts,
                                    &mut scratch.cap_next,
                                    &mut scratch.cap_stack,
                                    &mut scratch.visited,
                                    next_gen,
                                    pc + 1,
                                    caps,
                                    next_pos,
                                    cur_ch,
                                    next_ch,
                                    after_next_byte,
                                );
                            }
                        }
                    }
                    _ => {}
                }
            }

            std::mem::swap(&mut scratch.cap_curr, &mut scratch.cap_next);
            curr_gen = next_gen;

            if best.is_some() && scratch.cap_curr.is_empty() {
                break;
            }
            if cur_ch.is_none() {
                break;
            }
            prev_ch = cur_ch;
            pos = next_pos;
        }

        best.map(|raw| {
            let mut slots = [None; MAX_CAPTURES];
            for g in 0..MAX_CAPTURES {
                let s = raw[g * 2];
                let e = raw[g * 2 + 1];
                if s != UNSET && e != UNSET {
                    slots[g] = Some((s, e));
                }
            }
            Captures { slots }
        })
    }

    /// Convenience helper for whole-string capture matching.
    #[inline]
    pub fn find_captures(
        &self,
        haystack: &str,
        from_byte: usize,
        scratch: &mut PikeVmScratch,
    ) -> Option<Captures> {
        self.find_captures_with_context(haystack, from_byte, None, None, scratch)
    }
}

#[inline]
fn find_lit_swar(
    bytes: &[u8],
    pos: usize,
    lit: &[u8],
    rare: RarePair,
    case_sensitive: bool,
) -> Option<usize> {
    if lit.is_empty() || pos >= bytes.len() {
        return None;
    }
    let rel = if case_sensitive {
        if lit.len() == 1 {
            find_byte(&bytes[pos..], lit[0])?
        } else {
            find_subslice_rare(&bytes[pos..], lit, rare)?
        }
    } else if lit.len() == 1 {
        find_byte_ascii_ci(&bytes[pos..], lit[0])?
    } else {
        find_subslice_ascii_ci_rare(&bytes[pos..], lit, rare)?
    };
    Some(pos + rel)
}

#[inline(always)]
fn eval_assert(
    kind: AssertKind,
    prev_ch: Option<char>,
    next_ch: Option<char>,
    byte_after_next: Option<u8>,
) -> bool {
    match kind {
        AssertKind::StartLine => prev_ch.is_none() || prev_ch == Some('\n'),
        AssertKind::EndLine => {
            next_ch.is_none()
                || next_ch == Some('\n')
                || (next_ch == Some('\r') && byte_after_next == Some(b'\n'))
        }
        AssertKind::WordBoundary => {
            let before = prev_ch.is_some_and(is_word_char);
            let after = next_ch.is_some_and(is_word_char);
            before != after
        }
        AssertKind::NotWordBoundary => {
            let before = prev_ch.is_some_and(is_word_char);
            let after = next_ch.is_some_and(is_word_char);
            before == after
        }
    }
}

#[allow(clippy::too_many_arguments)]
#[inline]
fn add_span_thread(
    insts: &[Inst],
    list: &mut Vec<(u32, usize)>,
    stack: &mut Vec<(u32, usize)>,
    visited: &mut [u32],
    gen: u32,
    start_pc: u32,
    start_pos: usize,
    prev_ch: Option<char>,
    next_ch: Option<char>,
    byte_after_next: Option<u8>,
) {
    stack.clear();
    stack.push((start_pc, start_pos));
    while let Some((pc, start)) = stack.pop() {
        let idx = pc as usize;
        if visited[idx] == gen {
            continue;
        }
        visited[idx] = gen;
        match &insts[idx] {
            Inst::Jmp(target) => {
                stack.push((*target as u32, start));
            }
            Inst::Split(a, b) => {
                stack.push((*b as u32, start));
                stack.push((*a as u32, start));
            }
            Inst::Save(_) => {
                stack.push((pc + 1, start));
            }
            Inst::Assert(kind) => {
                if eval_assert(*kind, prev_ch, next_ch, byte_after_next) {
                    stack.push((pc + 1, start));
                }
            }
            _ => {
                list.push((pc, start));
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
#[inline]
fn add_cap_thread(
    insts: &[Inst],
    list: &mut Vec<(u32, [usize; MAX_SLOTS])>,
    stack: &mut Vec<(u32, [usize; MAX_SLOTS])>,
    visited: &mut [u32],
    gen: u32,
    start_pc: u32,
    start_caps: [usize; MAX_SLOTS],
    byte_pos: usize,
    prev_ch: Option<char>,
    next_ch: Option<char>,
    byte_after_next: Option<u8>,
) {
    stack.clear();
    stack.push((start_pc, start_caps));
    while let Some((pc, mut caps)) = stack.pop() {
        let idx = pc as usize;
        if visited[idx] == gen {
            continue;
        }
        visited[idx] = gen;
        match &insts[idx] {
            Inst::Jmp(target) => {
                stack.push((*target as u32, caps));
            }
            Inst::Split(a, b) => {
                stack.push((*b as u32, caps));
                stack.push((*a as u32, caps));
            }
            Inst::Save(slot) => {
                caps[*slot as usize] = byte_pos;
                stack.push((pc + 1, caps));
            }
            Inst::Assert(kind) => {
                if eval_assert(*kind, prev_ch, next_ch, byte_after_next) {
                    stack.push((pc + 1, caps));
                }
            }
            _ => {
                list.push((pc, caps));
            }
        }
    }
}
