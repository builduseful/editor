//! Built-in compiled Rust language plugins.
//!
//! To add a new language plugin (human or agent):
//! 1. Create `crates/editor-core/src/syntax/langs/<lang>.rs` implementing [`SyntaxPlugin`].
//! 2. Add `pub mod <lang>;` and `&<lang>::PLUGIN` to [`BUILTIN_PLUGINS`] below.
//! 3. Run `cargo test -p editor-core` — the generic plugin contract tests automatically verify
//!    bounds, UTF-8 safety, determinism, and unique metadata across every registered plugin!

pub mod c;
pub mod cpp;
pub mod csharp;
pub mod css;
pub mod extra;
pub mod go;
pub mod html;
pub mod java;
pub mod javascript;
pub mod kotlin;
pub mod markdown;
pub mod php;
pub mod powershell;
pub mod python;
pub mod rust;
pub mod shell;
pub mod sql;
pub mod typescript;

use super::SyntaxPlugin;

/// Single source of truth for all compiled language plugins.
/// Index 0 MUST remain [`extra::PLAIN_TEXT`].
pub static BUILTIN_PLUGINS: &[&dyn SyntaxPlugin] = &[
    &extra::PLAIN_TEXT,
    &rust::PLUGIN,
    &cpp::PLUGIN,
    &c::PLUGIN,
    &csharp::PLUGIN,
    &javascript::PLUGIN,
    &typescript::PLUGIN,
    &html::PLUGIN,
    &css::PLUGIN,
    &markdown::PLUGIN,
    &python::PLUGIN,
    &java::PLUGIN,
    &kotlin::PLUGIN,
    &go::PLUGIN,
    &php::PLUGIN,
    &shell::PLUGIN,
    &powershell::PLUGIN,
    &sql::PLUGIN,
    &extra::JSON,
    &extra::TOML,
    &extra::SWIFT,
];
