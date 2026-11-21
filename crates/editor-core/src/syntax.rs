//! Zero-regex, 1-byte-state incremental lexical syntax highlighter.
//!
//! Stores only ONE byte (`LineState`) per logical line. Editing a line re-lexes
//! forward and terminates as soon as the outgoing `LineState` matches the cached
//! value (typically 1 line = < 5 microseconds).

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    PlainText,
    Rust,
    C,
    Go,
    Swift,
    Python,
    JavaScript,
    Json,
    Markdown,
    Shell,
    Toml,
    Html,
    Css,
    Sql,
}

impl Language {
    pub fn from_path(path: Option<&Path>) -> Self {
        let ext = path
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase());
        match ext.as_deref() {
            Some("rs") => Language::Rust,
            Some("c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hh" | "m" | "mm") => Language::C,
            Some("go") => Language::Go,
            Some("swift") => Language::Swift,
            Some("py" | "pyi") => Language::Python,
            Some("js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs") => Language::JavaScript,
            Some("json" | "jsonc") => Language::Json,
            Some("md" | "markdown") => Language::Markdown,
            Some("sh" | "bash" | "zsh") => Language::Shell,
            Some("toml" | "yaml" | "yml" | "ini") => Language::Toml,
            Some("html" | "htm" | "xml" | "svg" | "plist") => Language::Html,
            Some("css" | "scss") => Language::Css,
            Some("sql") => Language::Sql,
            _ => Language::PlainText,
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Language::PlainText => "Plain Text",
            Language::Rust => "Rust",
            Language::C => "C / C++",
            Language::Go => "Go",
            Language::Swift => "Swift",
            Language::Python => "Python",
            Language::JavaScript => "JS / TS",
            Language::Json => "JSON",
            Language::Markdown => "Markdown",
            Language::Shell => "Shell",
            Language::Toml => "TOML / Config",
            Language::Html => "HTML / XML",
            Language::Css => "CSS",
            Language::Sql => "SQL",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Plain,
    Keyword,
    Type,
    String,
    Number,
    Comment,
    Punctuation,
    Macro,
    Heading,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenSpan {
    pub start: u32,
    pub end: u32,
    pub kind: TokenKind,
}

/// Compact 1-byte cross-line lexer state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LineState(u8);

impl LineState {
    pub const NORMAL: Self = Self(0);
    const IN_BLOCK_COMMENT: u8 = 1; // + nesting depth in low nibble for Rust
    const IN_TRIPLE_DOUBLE_QUOTE: u8 = 16;
    const IN_TRIPLE_SINGLE_QUOTE: u8 = 17;
    const IN_CODE_FENCE: u8 = 18;
}

/// Incremental per-line state cache.
#[derive(Debug, Default)]
pub struct SyntaxCache {
    language: Language,
    /// `end_states[line]` is the lexer state AT THE END of `line`.
    end_states: Vec<LineState>,
    valid_prefix: usize,
}

impl Default for Language {
    fn default() -> Self {
        Language::PlainText
    }
}

impl SyntaxCache {
    pub fn new(language: Language) -> Self {
        Self {
            language,
            end_states: Vec::new(),
            valid_prefix: 0,
        }
    }

    pub fn language(&self) -> Language {
        self.language
    }

    pub fn set_language(&mut self, language: Language) {
        if self.language != language {
            self.language = language;
            self.end_states.clear();
            self.valid_prefix = 0;
        }
    }

    pub fn invalidate_from(&mut self, line: usize) {
        self.valid_prefix = self.valid_prefix.min(line);
        if self.end_states.len() > line {
            self.end_states.truncate(line);
        }
    }

    /// Lex `line_text` at `line_idx`, updating cached end state and emitting tokens into `out`.
    pub fn lex_line(
        &mut self,
        line_idx: usize,
        line_text: &str,
        out: &mut Vec<TokenSpan>,
    ) -> LineState {
        out.clear();
        if self.language == Language::PlainText || line_text.is_empty() {
            return LineState::NORMAL;
        }
        let start_state = if line_idx == 0 {
            LineState::NORMAL
        } else {
            self.end_states
                .get(line_idx - 1)
                .copied()
                .unwrap_or(LineState::NORMAL)
        };
        let end_state = lex_line_stateless(self.language, start_state, line_text, out);
        if line_idx == self.end_states.len() {
            self.end_states.push(end_state);
            self.valid_prefix = self.end_states.len();
        } else if line_idx < self.end_states.len() {
            self.end_states[line_idx] = end_state;
        }
        end_state
    }
}

pub fn lex_line_stateless(
    lang: Language,
    state: LineState,
    line: &str,
    out: &mut Vec<TokenSpan>,
) -> LineState {
    match lang {
        Language::PlainText => LineState::NORMAL,
        Language::Markdown => lex_markdown(state, line, out),
        Language::Json => lex_json(line, out),
        Language::Python => lex_python(state, line, out),
        Language::Shell | Language::Toml => lex_hash_script(lang, line, out),
        Language::Html => lex_html(line, out),
        Language::Sql => lex_sql(line, out),
        Language::Rust | Language::C | Language::Go | Language::Swift | Language::JavaScript | Language::Css => {
            lex_c_like(lang, state, line, out)
        }
    }
}

fn lex_html(line: &str, out: &mut Vec<TokenSpan>) -> LineState {
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        if bytes[i..].starts_with(b"<!--") {
            let start = i;
            i += 4;
            while i + 2 < len && !bytes[i..].starts_with(b"-->") {
                i += 1;
            }
            if i + 3 <= len && bytes[i..].starts_with(b"-->") {
                i += 3;
            } else {
                i = len;
            }
            push_span(out, start, i, TokenKind::Comment);
        } else if bytes[i] == b'<' {
            let start = i;
            i += 1;
            if i < len && (bytes[i] == b'/' || bytes[i] == b'?' || bytes[i] == b'!') {
                i += 1;
            }
            while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'-' || bytes[i] == b':') {
                i += 1;
            }
            push_span(out, start, i, TokenKind::Keyword);
        } else if bytes[i] == b'"' || bytes[i] == b'\'' {
            let q = bytes[i];
            let start = i;
            i += 1;
            while i < len && bytes[i] != q {
                i += 1;
            }
            if i < len {
                i += 1;
            }
            push_span(out, start, i, TokenKind::String);
        } else {
            i += 1;
        }
    }
    LineState::NORMAL
}

fn lex_sql(line: &str, out: &mut Vec<TokenSpan>) -> LineState {
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        if bytes[i] == b'-' && i + 1 < len && bytes[i + 1] == b'-' {
            push_span(out, i, len, TokenKind::Comment);
            return LineState::NORMAL;
        } else if bytes[i] == b'\'' {
            let start = i;
            i += 1;
            while i < len {
                if bytes[i] == b'\'' {
                    i += 1;
                    if i < len && bytes[i] == b'\'' {
                        i += 1;
                    } else {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            push_span(out, start, i, TokenKind::String);
        } else if bytes[i].is_ascii_digit() {
            let start = i;
            while i < len && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            push_span(out, start, i, TokenKind::Number);
        } else if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
            let start = i;
            while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let up = line[start..i].to_ascii_uppercase();
            if matches!(
                up.as_str(),
                "SELECT" | "FROM" | "WHERE" | "INSERT" | "INTO" | "VALUES" | "UPDATE" | "SET"
                    | "DELETE" | "CREATE" | "TABLE" | "INDEX" | "DROP" | "ALTER" | "JOIN"
                    | "LEFT" | "RIGHT" | "INNER" | "OUTER" | "ON" | "AS" | "AND" | "OR" | "NOT"
                    | "NULL" | "ORDER" | "BY" | "GROUP" | "HAVING" | "LIMIT" | "OFFSET"
                    | "PRIMARY" | "KEY" | "BEGIN" | "COMMIT" | "ROLLBACK"
            ) {
                push_span(out, start, i, TokenKind::Keyword);
            }
        } else {
            i += 1;
        }
    }
    LineState::NORMAL
}

fn push_span(out: &mut Vec<TokenSpan>, start: usize, end: usize, kind: TokenKind) {
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

fn lex_markdown(state: LineState, line: &str, out: &mut Vec<TokenSpan>) -> LineState {
    let trimmed = line.trim_start();
    if trimmed.starts_with("```") {
        push_span(out, 0, line.len(), TokenKind::Comment);
        return if state.0 == LineState::IN_CODE_FENCE {
            LineState::NORMAL
        } else {
            LineState(LineState::IN_CODE_FENCE)
        };
    }
    if state.0 == LineState::IN_CODE_FENCE {
        push_span(out, 0, line.len(), TokenKind::String);
        return state;
    }
    if trimmed.starts_with('#') {
        push_span(out, 0, line.len(), TokenKind::Heading);
        return LineState::NORMAL;
    }
    if trimmed.starts_with('>') {
        push_span(out, 0, line.len(), TokenKind::Comment);
        return LineState::NORMAL;
    }
    // Highlight inline `code` spans.
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            let start = i;
            i += 1;
            while i < bytes.len() && bytes[i] != b'`' {
                i += 1;
            }
            if i < bytes.len() {
                i += 1;
            }
            push_span(out, start, i, TokenKind::String);
        } else {
            i += 1;
        }
    }
    LineState::NORMAL
}

fn lex_json(line: &str, out: &mut Vec<TokenSpan>) -> LineState {
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        let b = bytes[i];
        if b == b'"' {
            let start = i;
            i += 1;
            while i < len {
                if bytes[i] == b'\\' && i + 1 < len {
                    i += 2;
                } else if bytes[i] == b'"' {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            // Check if followed by `:` -> property key (Keyword/Type styling).
            let mut j = i;
            while j < len && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            let kind = if j < len && bytes[j] == b':' {
                TokenKind::Keyword
            } else {
                TokenKind::String
            };
            push_span(out, start, i, kind);
        } else if b.is_ascii_digit() || (b == b'-' && i + 1 < len && bytes[i + 1].is_ascii_digit()) {
            let start = i;
            i += 1;
            while i < len && matches!(bytes[i], b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-') {
                i += 1;
            }
            push_span(out, start, i, TokenKind::Number);
        } else if b.is_ascii_alphabetic() {
            let start = i;
            while i < len && bytes[i].is_ascii_alphabetic() {
                i += 1;
            }
            let word = &line[start..i];
            if matches!(word, "true" | "false" | "null") {
                push_span(out, start, i, TokenKind::Keyword);
            }
        } else {
            i += 1;
        }
    }
    LineState::NORMAL
}

fn lex_python(mut state: LineState, line: &str, out: &mut Vec<TokenSpan>) -> LineState {
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    if state.0 == LineState::IN_TRIPLE_DOUBLE_QUOTE || state.0 == LineState::IN_TRIPLE_SINGLE_QUOTE {
        let q = if state.0 == LineState::IN_TRIPLE_DOUBLE_QUOTE { b'"' } else { b'\'' };
        let start = 0;
        while i + 2 < len {
            if bytes[i] == q && bytes[i + 1] == q && bytes[i + 2] == q {
                i += 3;
                push_span(out, start, i, TokenKind::String);
                state = LineState::NORMAL;
                break;
            }
            i += 1;
        }
        if state != LineState::NORMAL {
            push_span(out, 0, len, TokenKind::String);
            return state;
        }
    }

    while i < len {
        let b = bytes[i];
        if b == b'#' {
            push_span(out, i, len, TokenKind::Comment);
            return LineState::NORMAL;
        }
        if (b == b'"' || b == b'\'') && i + 2 < len && bytes[i + 1] == b && bytes[i + 2] == b {
            let start = i;
            i += 3;
            let mut closed = false;
            while i + 2 < len {
                if bytes[i] == b && bytes[i + 1] == b && bytes[i + 2] == b {
                    i += 3;
                    closed = true;
                    break;
                }
                i += 1;
            }
            if closed {
                push_span(out, start, i, TokenKind::String);
            } else {
                push_span(out, start, len, TokenKind::String);
                return if b == b'"' {
                    LineState(LineState::IN_TRIPLE_DOUBLE_QUOTE)
                } else {
                    LineState(LineState::IN_TRIPLE_SINGLE_QUOTE)
                };
            }
        } else if b == b'"' || b == b'\'' {
            let start = i;
            i += 1;
            while i < len {
                if bytes[i] == b'\\' && i + 1 < len {
                    i += 2;
                } else if bytes[i] == b {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            push_span(out, start, i, TokenKind::String);
        } else if b.is_ascii_digit() {
            let start = i;
            while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'.') {
                i += 1;
            }
            push_span(out, start, i, TokenKind::Number);
        } else if b.is_ascii_alphabetic() || b == b'_' {
            let start = i;
            while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let word = &line[start..i];
            if is_python_keyword(word) {
                push_span(out, start, i, TokenKind::Keyword);
            }
        } else {
            i += 1;
        }
    }
    LineState::NORMAL
}

fn lex_hash_script(lang: Language, line: &str, out: &mut Vec<TokenSpan>) -> LineState {
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    if lang == Language::Toml && line.trim_start().starts_with('[') {
        push_span(out, 0, len, TokenKind::Heading);
        return LineState::NORMAL;
    }
    while i < len {
        let b = bytes[i];
        if b == b'#' {
            push_span(out, i, len, TokenKind::Comment);
            return LineState::NORMAL;
        }
        if b == b'"' || b == b'\'' {
            let start = i;
            i += 1;
            while i < len {
                if b == b'"' && bytes[i] == b'\\' && i + 1 < len {
                    i += 2;
                } else if bytes[i] == b {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            push_span(out, start, i, TokenKind::String);
        } else if b.is_ascii_digit() {
            let start = i;
            while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'.' || bytes[i] == b'_') {
                i += 1;
            }
            push_span(out, start, i, TokenKind::Number);
        } else if b.is_ascii_alphabetic() || b == b'_' {
            let start = i;
            while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'-') {
                i += 1;
            }
            let word = &line[start..i];
            if lang == Language::Shell
                && matches!(
                    word,
                    "if" | "then" | "else" | "elif" | "fi" | "for" | "in" | "do" | "done"
                        | "while" | "case" | "esac" | "function" | "return" | "local" | "export"
                )
            {
                push_span(out, start, i, TokenKind::Keyword);
            } else if lang == Language::Toml && matches!(word, "true" | "false") {
                push_span(out, start, i, TokenKind::Keyword);
            }
        } else {
            i += 1;
        }
    }
    LineState::NORMAL
}

fn lex_c_like(lang: Language, state: LineState, line: &str, out: &mut Vec<TokenSpan>) -> LineState {
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut comment_depth = if state.0 >= LineState::IN_BLOCK_COMMENT && state.0 < 16 {
        state.0
    } else {
        0
    };
    let nested_comments = lang == Language::Rust;

    if comment_depth > 0 {
        let start = 0;
        while i + 1 < len && comment_depth > 0 {
            if nested_comments && bytes[i] == b'/' && bytes[i + 1] == b'*' {
                comment_depth = comment_depth.saturating_add(1).min(15);
                i += 2;
            } else if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                comment_depth -= 1;
                i += 2;
            } else {
                i += 1;
            }
        }
        if comment_depth > 0 {
            push_span(out, start, len, TokenKind::Comment);
            return LineState(comment_depth);
        } else {
            push_span(out, start, i, TokenKind::Comment);
        }
    }

    // C/C++ preprocessor directive at start of line.
    if lang == Language::C && line.trim_start().starts_with('#') {
        push_span(out, 0, len, TokenKind::Macro);
        return LineState::NORMAL;
    }

    while i < len {
        let b = bytes[i];
        if b == b'/' && i + 1 < len && bytes[i + 1] == b'/' {
            push_span(out, i, len, TokenKind::Comment);
            return LineState::NORMAL;
        }
        if b == b'/' && i + 1 < len && bytes[i + 1] == b'*' {
            let start = i;
            i += 2;
            comment_depth = 1;
            while i + 1 < len && comment_depth > 0 {
                if nested_comments && bytes[i] == b'/' && bytes[i + 1] == b'*' {
                    comment_depth = comment_depth.saturating_add(1).min(15);
                    i += 2;
                } else if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                    comment_depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            if comment_depth > 0 {
                push_span(out, start, len, TokenKind::Comment);
                return LineState(comment_depth);
            } else {
                push_span(out, start, i, TokenKind::Comment);
            }
        } else if b == b'"' || (lang == Language::JavaScript && (b == b'\'' || b == b'`')) {
            let start = i;
            i += 1;
            while i < len {
                if bytes[i] == b'\\' && i + 1 < len {
                    i += 2;
                } else if bytes[i] == b {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            push_span(out, start, i, TokenKind::String);
        } else if b == b'\'' && lang != Language::Rust {
            let start = i;
            i += 1;
            while i < len {
                if bytes[i] == b'\\' && i + 1 < len {
                    i += 2;
                } else if bytes[i] == b'\'' {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            push_span(out, start, i, TokenKind::String);
        } else if b.is_ascii_digit() {
            let start = i;
            while i < len
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'.')
            {
                i += 1;
            }
            push_span(out, start, i, TokenKind::Number);
        } else if b.is_ascii_alphabetic() || b == b'_' {
            let start = i;
            while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let word = &line[start..i];
            if lang == Language::Rust && i < len && bytes[i] == b'!' {
                i += 1;
                push_span(out, start, i, TokenKind::Macro);
            } else if is_c_like_keyword(lang, word) {
                push_span(out, start, i, TokenKind::Keyword);
            } else if lang == Language::Rust && is_rust_builtin_type(word) {
                push_span(out, start, i, TokenKind::Type);
            }
        } else {
            i += 1;
        }
    }

    LineState::NORMAL
}

fn is_rust_builtin_type(w: &str) -> bool {
    matches!(
        w,
        "u8" | "u16" | "u32" | "u64" | "u128" | "usize"
            | "i8" | "i16" | "i32" | "i64" | "i128" | "isize"
            | "f32" | "f64" | "bool" | "char" | "str"
            | "Self" | "Option" | "Result" | "Vec" | "String" | "Box" | "Arc" | "Rc"
    )
}

fn is_c_like_keyword(lang: Language, w: &str) -> bool {
    match lang {
        Language::Rust => matches!(
            w,
            "as" | "async" | "await" | "break" | "const" | "continue" | "crate" | "dyn"
                | "else" | "enum" | "extern" | "false" | "fn" | "for" | "if" | "impl" | "in"
                | "let" | "loop" | "match" | "mod" | "move" | "mut" | "pub" | "ref" | "return"
                | "self" | "static" | "struct" | "super" | "trait" | "true" | "type" | "unsafe"
                | "use" | "where" | "while"
        ),
        Language::C => matches!(
            w,
            "auto" | "bool" | "break" | "case" | "char" | "class" | "const" | "constexpr"
                | "continue" | "default" | "delete" | "do" | "double" | "else" | "enum"
                | "explicit" | "extern" | "false" | "float" | "for" | "if" | "inline" | "int"
                | "long" | "namespace" | "new" | "noexcept" | "nullptr" | "override" | "private"
                | "protected" | "public" | "return" | "short" | "signed" | "sizeof" | "static"
                | "struct" | "switch" | "template" | "this" | "throw" | "true" | "try"
                | "typedef" | "typename" | "union" | "unsigned" | "using" | "virtual" | "void"
                | "volatile" | "while"
        ),
        Language::JavaScript => matches!(
            w,
            "async" | "await" | "break" | "case" | "catch" | "class" | "const" | "continue"
                | "default" | "delete" | "do" | "else" | "enum" | "export" | "extends" | "false"
                | "finally" | "for" | "from" | "function" | "if" | "implements" | "import" | "in"
                | "instanceof" | "interface" | "let" | "new" | "null" | "of" | "private"
                | "protected" | "public" | "readonly" | "return" | "static" | "super" | "switch"
                | "this" | "throw" | "true" | "try" | "type" | "typeof" | "undefined" | "var"
                | "void" | "while" | "yield"
        ),
        Language::Go => matches!(
            w,
            "break" | "case" | "chan" | "const" | "continue" | "default" | "defer" | "else"
                | "fallthrough" | "for" | "func" | "go" | "goto" | "if" | "import" | "interface"
                | "map" | "nil" | "package" | "range" | "return" | "select" | "struct" | "switch"
                | "true" | "false" | "type" | "var"
        ),
        Language::Swift => matches!(
            w,
            "actor" | "as" | "async" | "await" | "break" | "case" | "catch" | "class" | "continue"
                | "default" | "defer" | "do" | "else" | "enum" | "extension" | "false" | "for"
                | "func" | "guard" | "if" | "import" | "in" | "init" | "inout" | "internal"
                | "is" | "let" | "nil" | "open" | "override" | "private" | "protocol" | "public"
                | "repeat" | "return" | "self" | "Self" | "static" | "struct" | "super"
                | "switch" | "throw" | "throws" | "true" | "try" | "typealias" | "var" | "where"
                | "while"
        ),
        Language::Css => matches!(
            w,
            "color" | "background" | "display" | "flex" | "grid" | "margin" | "padding"
                | "border" | "font" | "width" | "height" | "position" | "top" | "left"
                | "right" | "bottom" | "none" | "auto" | "inherit" | "important"
        ),
        _ => false,
    }
}

fn is_python_keyword(w: &str) -> bool {
    matches!(
        w,
        "False" | "None" | "True" | "and" | "as" | "assert" | "async" | "await" | "break"
            | "class" | "continue" | "def" | "del" | "elif" | "else" | "except" | "finally"
            | "for" | "from" | "global" | "if" | "import" | "in" | "is" | "lambda" | "nonlocal"
            | "not" | "or" | "pass" | "raise" | "return" | "try" | "while" | "with" | "yield"
    )
}
