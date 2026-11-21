//! Ultra-fast, zero-dependency text engine for `editor`.
//!
//! Architecture highlights:
//! - Monoid order-statistic B-tree piece table (`piece_tree`) with lazy multi-GB base backing
//! - 64-bit SWAR (SIMD-within-a-register) byte scanning (`swar`) in 100% safe Rust
//! - UTF-8, programming-language, and Emoji grapheme cluster engine (`grapheme`)
//! - Coalescing undo/redo history (`undo`)
//! - Streaming literal search (`search`)
//! - 1-byte-state incremental lexical syntax highlighter (`syntax`)
//! - Atomic + APFS-CoW document persistence (`document`)

pub mod document;
pub mod grapheme;
pub mod piece_tree;
pub mod search;
pub mod swar;
pub mod syntax;
pub mod undo;

pub use document::{Document, DocumentError, DocumentInfo, Eol, SaveMethod, SaveReport};
pub use grapheme::{
    cluster_cell_width, grapheme_clusters, next_grapheme_boundary, next_word_boundary,
    prev_grapheme_boundary, prev_word_boundary, validate_utf8_offset, word_range_at,
};
pub use piece_tree::{PieceTree, Summary};
pub use search::{find_all_matches, find_next_match, SearchMatch, SearchOptions};
pub use syntax::{Language, LineState, SyntaxCache, TokenKind, TokenSpan};
pub use undo::{EditOp, UndoStack};
