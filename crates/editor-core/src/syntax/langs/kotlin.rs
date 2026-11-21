//! Kotlin language syntax plugin (`kt`, `kts`).
//!
//! Supports nested `/* /* */ */` block comments, `"""..."""` multi-line raw strings,
//! `@Annotations`, Kotlin keywords (`fun`/`val`/`var`/`suspend`/`data`/`sealed`), and types.

use crate::syntax::cursor::{lex_c_like, CLikeSpec};
use crate::syntax::{LanguageMeta, LanguageRegistry, LexState, SyntaxPlugin, TokenSpan};

pub struct KotlinPlugin;
pub static PLUGIN: KotlinPlugin = KotlinPlugin;

const META: LanguageMeta = LanguageMeta {
    id: "kotlin",
    name: "Kotlin",
    aliases: &["kt", "kts"],
    extensions: &["kt", "kts"],
    filenames: &[],
    line_comment: Some("//"),
    block_comment: Some(("/*", "*/")),
    default_word_wrap: false,
};

const SPEC: CLikeSpec = CLikeSpec {
    nested_block_comments: true,
    preprocessor_hash: false,
    at_annotations: true,
    triple_double_strings: true,
    backtick_raw_strings: false,
    apostrophe_num_sep: false,
    is_keyword,
    is_type,
    is_constant,
};

impl SyntaxPlugin for KotlinPlugin {
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
        "abstract" | "actual" | "annotation" | "as" | "break" | "by" | "catch" | "class"
            | "companion" | "const" | "constructor" | "continue" | "crossinline" | "data"
            | "delegate" | "do" | "dynamic" | "else" | "enum" | "expect" | "external" | "field"
            | "file" | "final" | "finally" | "for" | "fun" | "get" | "if" | "import" | "in"
            | "infix" | "init" | "inline" | "inner" | "interface" | "internal" | "is" | "it"
            | "lateinit" | "noinline" | "object" | "open" | "operator" | "out" | "override"
            | "package" | "param" | "private" | "property" | "protected" | "public" | "reified"
            | "return" | "sealed" | "set" | "super" | "suspend" | "tailrec" | "this" | "throw"
            | "try" | "typealias" | "typeof" | "val" | "value" | "var" | "vararg" | "when"
            | "where" | "while"
    )
}

fn is_constant(w: &str) -> bool {
    matches!(w, "true" | "false" | "null")
}

fn is_type(w: &str) -> bool {
    matches!(
        w,
        "Any" | "Unit" | "Nothing" | "Boolean" | "Byte" | "Short" | "Int" | "Long" | "Float"
            | "Double" | "Char" | "String" | "UByte" | "UShort" | "UInt" | "ULong" | "Array"
            | "List" | "MutableList" | "Map" | "MutableMap" | "Set" | "MutableSet" | "Pair"
            | "Triple" | "Result" | "Sequence" | "Flow"
    )
}
