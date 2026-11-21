//! Go language syntax plugin (`go`).
//!
//! Supports backtick raw multi-line strings, `/* */` comments, Go keywords,
//! primitive/builtin types (`uint64`, `error`, `any`), and functions.

use crate::syntax::cursor::{lex_c_like, CLikeSpec};
use crate::syntax::{LanguageMeta, LanguageRegistry, LexState, SyntaxPlugin, TokenSpan};

pub struct GoPlugin;
pub static PLUGIN: GoPlugin = GoPlugin;

const META: LanguageMeta = LanguageMeta {
    id: "go",
    name: "Go",
    aliases: &["golang"],
    extensions: &["go"],
    filenames: &[],
    line_comment: Some("//"),
    block_comment: Some(("/*", "*/")),
    default_word_wrap: false,
};

const SPEC: CLikeSpec = CLikeSpec {
    nested_block_comments: false,
    preprocessor_hash: false,
    at_annotations: false,
    triple_double_strings: false,
    backtick_raw_strings: true,
    apostrophe_num_sep: false,
    is_keyword,
    is_type,
    is_constant,
};

impl SyntaxPlugin for GoPlugin {
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
        lex_c_like(&SPEC, line, state, out)
    }
}

fn is_keyword(w: &str) -> bool {
    matches!(
        w,
        "break" | "case" | "chan" | "const" | "continue" | "default" | "defer" | "else"
            | "fallthrough" | "for" | "func" | "go" | "goto" | "if" | "import" | "interface"
            | "map" | "package" | "range" | "return" | "select" | "struct" | "switch" | "type"
            | "var"
    )
}

fn is_constant(w: &str) -> bool {
    matches!(w, "true" | "false" | "nil" | "iota")
}

fn is_type(w: &str) -> bool {
    matches!(
        w,
        "any" | "bool" | "byte" | "comparable" | "complex64" | "complex128" | "error" | "float32"
            | "float64" | "int" | "int8" | "int16" | "int32" | "int64" | "rune" | "string" | "uint"
            | "uint8" | "uint16" | "uint32" | "uint64" | "uintptr"
    )
}
