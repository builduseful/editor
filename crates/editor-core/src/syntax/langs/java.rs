//! Java language syntax plugin (`java`).

use crate::syntax::cursor::{lex_c_like, CLikeSpec};
use crate::syntax::{LanguageMeta, LanguageRegistry, LexState, SyntaxPlugin, TokenSpan};

pub struct JavaPlugin;
pub static PLUGIN: JavaPlugin = JavaPlugin;

const META: LanguageMeta = LanguageMeta {
    id: "java",
    name: "Java",
    aliases: &[],
    extensions: &["java"],
    filenames: &[],
    line_comment: Some("//"),
    block_comment: Some(("/*", "*/")),
    default_word_wrap: false,
};

const SPEC: CLikeSpec = CLikeSpec {
    nested_block_comments: false,
    preprocessor_hash: false,
    at_annotations: true,
    triple_double_strings: true,
    backtick_raw_strings: false,
    apostrophe_num_sep: false,
    is_keyword,
    is_type,
    is_constant,
};

impl SyntaxPlugin for JavaPlugin {
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
        "abstract" | "assert" | "break" | "case" | "catch" | "class" | "const" | "continue"
            | "default" | "do" | "else" | "enum" | "exports" | "extends" | "final" | "finally"
            | "for" | "goto" | "if" | "implements" | "import" | "instanceof" | "interface"
            | "module" | "native" | "new" | "non-sealed" | "open" | "opens" | "package"
            | "permits" | "private" | "protected" | "provides" | "public" | "record" | "requires"
            | "return" | "sealed" | "static" | "strictfp" | "super" | "switch" | "synchronized"
            | "this" | "throw" | "throws" | "to" | "transient" | "transitive" | "try" | "uses"
            | "var" | "volatile" | "when" | "while" | "with" | "yield"
    )
}

fn is_constant(w: &str) -> bool {
    matches!(w, "true" | "false" | "null")
}

fn is_type(w: &str) -> bool {
    matches!(
        w,
        "boolean" | "byte" | "char" | "double" | "float" | "int" | "long" | "short" | "void"
            | "String" | "Object" | "Integer" | "Long" | "Double" | "Boolean" | "List" | "Map"
            | "Set" | "Optional" | "Stream" | "System" | "Exception" | "RuntimeException"
    )
}
