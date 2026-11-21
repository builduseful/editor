//! Bash / Shell language syntax plugin (`sh`, `bash`, `zsh`, `ksh`, `env`).
//!
//! Supports `#` comments & shebang `#!/...`, `"..."` and `'...'` strings, `$VAR` / `${VAR}`,
//! CLI flags `--flag`, pipes/redirections, and POSIX/Bash keywords & builtins.

use crate::syntax::cursor::{is_ident_start, push_span, ByteCursor};
use crate::syntax::{LanguageMeta, LanguageRegistry, LexState, SyntaxPlugin, TokenKind, TokenSpan};

pub struct ShellPlugin;
pub static PLUGIN: ShellPlugin = ShellPlugin;

const META: LanguageMeta = LanguageMeta {
    id: "shell",
    name: "Shell",
    aliases: &["bash", "sh", "zsh", "shellscript"],
    extensions: &["sh", "bash", "zsh", "ksh", "bashrc", "zshrc"],
    filenames: &[".bashrc", ".zshrc", ".profile", ".bash_profile", "makefile"],
    line_comment: Some("#"),
    block_comment: None,
    default_word_wrap: false,
};

impl SyntaxPlugin for ShellPlugin {
    fn meta(&self) -> &'static LanguageMeta {
        &META
    }

    fn lex_line(
        &self,
        line: &str,
        _state: LexState,
        _registry: &LanguageRegistry,
        out: &mut Vec<TokenSpan>,
    ) -> LexState {
        let mut cur = ByteCursor::new(line);

        if cur.starts_with(b"#!") {
            push_span(out, 0, cur.len(), TokenKind::Macro);
            return LexState::NORMAL;
        }

        while !cur.is_eof() {
            let b = cur.bytes[cur.pos];
            if b == b' ' || b == b'\t' {
                cur.pos += 1;
            } else if b == b'#' {
                push_span(out, cur.pos, cur.len(), TokenKind::Comment);
                return LexState::NORMAL;
            } else if b == b'"' {
                cur.scan_quoted_string(b'"', true, out);
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
                    push_span(out, s, cur.pos, TokenKind::Property);
                } else if cur.peek() == Some(b'(') {
                    cur.advance(1);
                    push_span(out, s, cur.pos, TokenKind::Punctuation);
                } else if cur.peek().is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'?' | b'@' | b'*' | b'#' | b'$' | b'!')) {
                    if cur.peek().is_some_and(is_ident_start) {
                        cur.scan_ident();
                    } else {
                        cur.advance(1);
                    }
                    push_span(out, s, cur.pos, TokenKind::Property);
                } else {
                    push_span(out, s, cur.pos, TokenKind::Operator);
                }
            } else if b == b'-'
                && (cur.pos == 0 || cur.bytes[cur.pos - 1] == b' ' || cur.bytes[cur.pos - 1] == b'\t')
                && cur.peek_at(1).is_some_and(|c| c.is_ascii_alphanumeric() || c == b'-')
            {
                let s = cur.pos;
                while !cur.is_eof()
                    && (cur.bytes[cur.pos].is_ascii_alphanumeric()
                        || cur.bytes[cur.pos] == b'-'
                        || cur.bytes[cur.pos] == b'_')
                {
                    cur.pos += 1;
                }
                push_span(out, s, cur.pos, TokenKind::Attribute);
            } else if b.is_ascii_digit() {
                cur.scan_number(false, out);
            } else if is_ident_start(b) {
                let s = cur.pos;
                while !cur.is_eof()
                    && (cur.bytes[cur.pos].is_ascii_alphanumeric()
                        || cur.bytes[cur.pos] == b'_'
                        || cur.bytes[cur.pos] == b'-')
                {
                    cur.pos += 1;
                }
                let e = cur.pos;
                let word = &cur.src[s..e];
                if cur.peek() == Some(b'=') {
                    push_span(out, s, e, TokenKind::Property);
                } else if is_keyword(word) {
                    push_span(out, s, e, TokenKind::Keyword);
                } else if is_builtin(word) {
                    push_span(out, s, e, TokenKind::Function);
                } else if matches!(word, "true" | "false") {
                    push_span(out, s, e, TokenKind::Constant);
                }
            } else if matches!(b, b'|' | b'&' | b'>' | b'<' | b'=' | b'!') {
                push_span(out, cur.pos, cur.pos + 1, TokenKind::Operator);
                cur.advance(1);
            } else if matches!(b, b';' | b'(' | b')' | b'{' | b'}' | b'[' | b']') {
                push_span(out, cur.pos, cur.pos + 1, TokenKind::Punctuation);
                cur.advance(1);
            } else {
                cur.bump_char();
            }
        }

        LexState::NORMAL
    }
}

fn is_keyword(w: &str) -> bool {
    matches!(
        w,
        "if" | "then" | "else" | "elif" | "fi" | "for" | "in" | "do" | "done" | "while" | "until"
            | "case" | "esac" | "select" | "function" | "return" | "local" | "export" | "readonly"
            | "declare" | "typeset" | "unset" | "break" | "continue" | "exit" | "set" | "shift"
            | "source" | "trap"
    )
}

fn is_builtin(w: &str) -> bool {
    matches!(
        w,
        "echo" | "printf" | "cd" | "pwd" | "read" | "test" | "eval" | "exec" | "command" | "type"
            | "which" | "alias" | "unalias" | "mkdir" | "rm" | "cp" | "mv" | "ls" | "cat" | "grep"
            | "sed" | "awk" | "find" | "xargs" | "curl" | "git" | "cargo"
    )
}
