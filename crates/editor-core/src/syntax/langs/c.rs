//! C language syntax plugin (`c`, `m`).

use crate::syntax::cursor::{lex_c_like, CLikeSpec};
use crate::syntax::{LanguageMeta, LanguageRegistry, LexState, SyntaxPlugin, TokenSpan};

pub struct CPlugin;
pub static PLUGIN: CPlugin = CPlugin;

const META: LanguageMeta = LanguageMeta {
    id: "c",
    name: "C",
    aliases: &["objc"],
    extensions: &["c", "m"],
    filenames: &[],
    line_comment: Some("//"),
    block_comment: Some(("/*", "*/")),
    default_word_wrap: false,
};

const SPEC: CLikeSpec = CLikeSpec {
    nested_block_comments: false,
    preprocessor_hash: true,
    at_annotations: false,
    triple_double_strings: false,
    backtick_raw_strings: false,
    apostrophe_num_sep: true,
    is_keyword,
    is_type,
    is_constant,
};

impl SyntaxPlugin for CPlugin {
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
        "auto" | "break" | "case" | "const" | "constexpr" | "continue" | "default" | "do" | "else"
            | "enum" | "extern" | "for" | "goto" | "if" | "inline" | "register" | "restrict"
            | "return" | "sizeof" | "static" | "static_assert" | "struct" | "switch"
            | "thread_local" | "typedef" | "typeof" | "union" | "volatile" | "while"
            | "_Alignas" | "_Alignof" | "_Atomic" | "_Generic" | "_Noreturn" | "_Static_assert"
            | "_Thread_local"
    )
}

fn is_constant(w: &str) -> bool {
    matches!(w, "true" | "false" | "NULL" | "nullptr" | "EOF")
}

fn is_type(w: &str) -> bool {
    matches!(
        w,
        "void" | "bool" | "_Bool" | "char" | "short" | "int" | "long" | "signed" | "unsigned"
            | "float" | "double" | "size_t" | "ssize_t" | "ptrdiff_t" | "intptr_t" | "uintptr_t"
            | "int8_t" | "int16_t" | "int32_t" | "int64_t" | "uint8_t" | "uint16_t" | "uint32_t"
            | "uint64_t" | "FILE"
    )
}
