//! TypeScript / TSX language syntax plugin (`ts`, `tsx`, `mts`, `cts`).
//!
//! Reuses the shared JS/TS scanner in [`super::javascript`] with TypeScript keywords,
//! primitive & utility types, decorators, and TSX tag support enabled.

use crate::syntax::{LanguageMeta, LanguageRegistry, LexState, SyntaxPlugin, TokenSpan};

pub struct TypeScriptPlugin;
pub static PLUGIN: TypeScriptPlugin = TypeScriptPlugin;

const META: LanguageMeta = LanguageMeta {
    id: "typescript",
    name: "TypeScript",
    aliases: &["ts", "tsx", "mts", "cts"],
    extensions: &["ts", "tsx", "mts", "cts"],
    filenames: &[],
    line_comment: Some("//"),
    block_comment: Some(("/*", "*/")),
    default_word_wrap: false,
};

impl SyntaxPlugin for TypeScriptPlugin {
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
        super::javascript::lex_js_ts(line, state, true, out)
    }
}
