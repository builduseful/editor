//! SQL language syntax plugin (`sql`).
//!
//! Zero-allocation ASCII case-insensitive SQL keywords & data types, `--` line comments,
//! `/* */` multi-line block comments, single-quoted strings with `''` escapes, and functions.

use crate::syntax::cursor::{is_ident_start, is_operator_byte, is_punct_byte, push_span, ByteCursor};
use crate::syntax::{LanguageMeta, LanguageRegistry, LexState, SyntaxPlugin, TokenKind, TokenSpan};

pub struct SqlPlugin;
pub static PLUGIN: SqlPlugin = SqlPlugin;

const META: LanguageMeta = LanguageMeta {
    id: "sql",
    name: "SQL",
    aliases: &["postgres", "postgresql", "sqlite", "mysql"],
    extensions: &["sql", "ddl", "dml"],
    filenames: &[],
    line_comment: Some("--"),
    block_comment: Some(("/*", "*/")),
    default_word_wrap: false,
};

const MODE_BLOCK_COMMENT: u8 = 1;

impl SyntaxPlugin for SqlPlugin {
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

        if state.mode() == MODE_BLOCK_COMMENT {
            let rem = cur.scan_block_comment(1, false, out);
            if rem > 0 {
                return LexState::new(MODE_BLOCK_COMMENT, 0, 1, 0);
            }
        }

        while !cur.is_eof() {
            let b = cur.bytes[cur.pos];
            if b == b' ' || b == b'\t' {
                cur.pos += 1;
            } else if b == b'-' && cur.peek_at(1) == Some(b'-') {
                push_span(out, cur.pos, cur.len(), TokenKind::Comment);
                return LexState::NORMAL;
            } else if b == b'/' && cur.peek_at(1) == Some(b'*') {
                let rem = cur.scan_block_comment(0, false, out);
                if rem > 0 {
                    return LexState::new(MODE_BLOCK_COMMENT, 0, 1, 0);
                }
            } else if b == b'\'' {
                let mut seg = cur.pos;
                cur.advance(1);
                while !cur.is_eof() {
                    if cur.bytes[cur.pos] == b'\'' {
                        if cur.peek_at(1) == Some(b'\'') {
                            push_span(out, seg, cur.pos, TokenKind::String);
                            let es = cur.pos;
                            cur.advance(2);
                            push_span(out, es, cur.pos, TokenKind::Escape);
                            seg = cur.pos;
                        } else {
                            cur.advance(1);
                            push_span(out, seg, cur.pos, TokenKind::String);
                            seg = cur.pos;
                            break;
                        }
                    } else {
                        cur.bump_char();
                    }
                }
                if cur.pos > seg {
                    push_span(out, seg, cur.pos, TokenKind::String);
                }
            } else if b == b'"' || b == b'`' {
                cur.scan_quoted_string(b, false, out);
            } else if b.is_ascii_digit() {
                cur.scan_number(false, out);
            } else if is_ident_start(b) {
                let (s, e, word) = cur.scan_ident();
                if is_sql_keyword(word) {
                    push_span(out, s, e, TokenKind::Keyword);
                } else if is_sql_constant(word) {
                    push_span(out, s, e, TokenKind::Constant);
                } else if is_sql_type(word) {
                    push_span(out, s, e, TokenKind::Type);
                } else if cur.followed_by_paren() {
                    push_span(out, s, e, TokenKind::Function);
                }
            } else if is_operator_byte(b) {
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

fn is_sql_keyword(w: &str) -> bool {
    const KWS: &[&str] = &[
        "ADD", "ALL", "ALTER", "AND", "AS", "ASC", "BETWEEN", "BY", "CASE", "CHECK", "COLUMN",
        "COMMIT", "CONSTRAINT", "CREATE", "CROSS", "DATABASE", "DEFAULT", "DELETE", "DESC",
        "DISTINCT", "DROP", "ELSE", "END", "EXCEPT", "EXISTS", "EXPLAIN", "FOREIGN", "FROM",
        "FULL", "GROUP", "HAVING", "IF", "ILIKE", "IN", "INDEX", "INNER", "INSERT", "INTERSECT",
        "INTO", "IS", "JOIN", "KEY", "LEFT", "LIKE", "LIMIT", "NOT", "OFFSET", "ON", "OR", "ORDER",
        "OUTER", "OVER", "PARTITION", "PRIMARY", "RECURSIVE", "REFERENCES", "RETURNING", "RIGHT",
        "ROLLBACK", "SELECT", "SET", "TABLE", "THEN", "TRANSACTION", "TRIGGER", "UNION", "UNIQUE",
        "UPDATE", "USING", "VALUES", "VIEW", "WHEN", "WHERE", "WINDOW", "WITH", "BEGIN",
    ];
    KWS.iter().any(|&k| k.eq_ignore_ascii_case(w))
}

fn is_sql_constant(w: &str) -> bool {
    const CONS: &[&str] = &["TRUE", "FALSE", "NULL", "CURRENT_TIMESTAMP", "CURRENT_DATE"];
    CONS.iter().any(|&c| c.eq_ignore_ascii_case(w))
}

fn is_sql_type(w: &str) -> bool {
    const TYPES: &[&str] = &[
        "BIGINT", "BINARY", "BLOB", "BOOL", "BOOLEAN", "BYTEA", "CHAR", "DATE", "DATETIME",
        "DECIMAL", "DOUBLE", "FLOAT", "INT", "INTEGER", "JSON", "JSONB", "NUMERIC", "REAL",
        "SERIAL", "SMALLINT", "TEXT", "TIME", "TIMESTAMP", "TIMESTAMPTZ", "UUID", "VARCHAR",
    ];
    TYPES.iter().any(|&t| t.eq_ignore_ascii_case(w))
}
