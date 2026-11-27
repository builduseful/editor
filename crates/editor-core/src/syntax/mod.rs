//! Modular, zero-dependency, compiled-Rust plugin syntax highlighting engine.
//!
//! Architecture:
//! - **1 file = 1 language plugin**: every language in [`langs`] implements [`SyntaxPlugin`]
//!   with static [`LanguageMeta`] (extensions, filenames, aliases, comment delimiters, wrap
//!   defaults) and registers via a single entry in [`langs::BUILTIN_PLUGINS`].
//! - **4-byte `Copy` cross-line state** ([`LexState`]): packs `(mode, sub_lang, depth, detail)`
//!   with zero heap allocations per line, enabling multi-line comments, raw strings, template
//!   literals, HTML `<style>`/`<script>`, and Markdown fenced code block delegation.
//! - **True early-converging incremental cache** ([`SyntaxCache`]): keeps prior `end_states`
//!   across edits and stops re-lexing as soon as `new_state == old_state` (typically 1 line,
//!   < 5 µs). Bounded catch-up (4,000 lines) with 256-line local warm-up protects 60 MB / 1M+
//!   line files from ever stalling on random jumps.

pub mod cursor;
pub mod langs;

use std::collections::BTreeMap;
use std::path::Path;

/// Maximum bytes of a single line lexed in full (guards against pathological minified/10 MB lines).
pub const MAX_LEX_LINE_BYTES: usize = 16 * 1024;
/// Maximum lines scanned synchronously when catching up after an edit or jump.
const MAX_SYNC_CATCHUP_LINES: usize = 4_000;
/// Warm-up lookback window when jumping far past the cached prefix in a huge file.
const HUGE_JUMP_WARMUP_LINES: usize = 256;

/// Semantic token category (1 byte, `Copy`).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TokenKind {
    Plain = 0,
    Comment = 1,
    Keyword = 2,
    Type = 3,
    Function = 4,
    String = 5,
    Escape = 6,
    Number = 7,
    Constant = 8,
    Macro = 9,
    Tag = 10,
    Attribute = 11,
    Property = 12,
    Operator = 13,
    Punctuation = 14,
    Heading = 15,
    Link = 16,
    Emphasis = 17,
    Strong = 18,
}

impl TokenKind {
    pub const COUNT: usize = 19;

    pub const ALL: [TokenKind; Self::COUNT] = [
        TokenKind::Plain,
        TokenKind::Comment,
        TokenKind::Keyword,
        TokenKind::Type,
        TokenKind::Function,
        TokenKind::String,
        TokenKind::Escape,
        TokenKind::Number,
        TokenKind::Constant,
        TokenKind::Macro,
        TokenKind::Tag,
        TokenKind::Attribute,
        TokenKind::Property,
        TokenKind::Operator,
        TokenKind::Punctuation,
        TokenKind::Heading,
        TokenKind::Link,
        TokenKind::Emphasis,
        TokenKind::Strong,
    ];

    /// Canonical lowercase token identifier used in theme configs & CLI commands.
    #[inline]
    pub const fn as_str(self) -> &'static str {
        match self {
            TokenKind::Plain => "plain",
            TokenKind::Comment => "comment",
            TokenKind::Keyword => "keyword",
            TokenKind::Type => "type",
            TokenKind::Function => "function",
            TokenKind::String => "string",
            TokenKind::Escape => "escape",
            TokenKind::Number => "number",
            TokenKind::Constant => "constant",
            TokenKind::Macro => "macro",
            TokenKind::Tag => "tag",
            TokenKind::Attribute => "attribute",
            TokenKind::Property => "property",
            TokenKind::Operator => "operator",
            TokenKind::Punctuation => "punctuation",
            TokenKind::Heading => "heading",
            TokenKind::Link => "link",
            TokenKind::Emphasis => "emphasis",
            TokenKind::Strong => "strong",
        }
    }

    /// Parse a canonical token name or common shorthand alias (case-insensitive).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "plain" | "text" | "default" | "foreground" | "fg" => Some(TokenKind::Plain),
            "comment" | "comments" => Some(TokenKind::Comment),
            "keyword" | "keywords" | "kw" => Some(TokenKind::Keyword),
            "type" | "types" | "class" | "struct" => Some(TokenKind::Type),
            "function" | "functions" | "fn" | "func" | "method" => Some(TokenKind::Function),
            "string" | "strings" | "str" => Some(TokenKind::String),
            "escape" | "escapes" => Some(TokenKind::Escape),
            "number" | "numbers" | "num" => Some(TokenKind::Number),
            "constant" | "constants" | "const" | "bool" | "boolean" => Some(TokenKind::Constant),
            "macro" | "macros" | "preprocessor" | "preproc" => Some(TokenKind::Macro),
            "tag" | "tags" => Some(TokenKind::Tag),
            "attribute" | "attributes" | "attr" | "decorator" => Some(TokenKind::Attribute),
            "property" | "properties" | "prop" | "field" => Some(TokenKind::Property),
            "operator" | "operators" | "op" => Some(TokenKind::Operator),
            "punctuation" | "punct" | "delimiter" => Some(TokenKind::Punctuation),
            "heading" | "headings" | "title" => Some(TokenKind::Heading),
            "link" | "links" | "url" => Some(TokenKind::Link),
            "emphasis" | "em" | "italic" => Some(TokenKind::Emphasis),
            "strong" | "bold" => Some(TokenKind::Strong),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenSpan {
    pub start: u32,
    pub end: u32,
    pub kind: TokenKind,
}

/// Compact 4-byte `Copy` cross-line lexer state.
///
/// Layout (MSB → LSB):
/// - `mode` (bits 24..32): plugin-defined multi-line state (0 = Normal).
/// - `sub_lang` (bits 16..24): embedded language index for HTML `<script>`/`<style>` or Markdown fences.
/// - `depth` (bits 8..16): nesting depth (Rust `/* */`), `#` count (`r###"`), or `${}` brace depth.
/// - `detail` (bits 0..8): delimiter hash / fence char / quote kind / flags.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct LexState(pub u32);

/// Backwards-compatible alias.
pub type LineState = LexState;

impl LexState {
    pub const NORMAL: Self = Self(0);

    #[inline]
    pub const fn new(mode: u8, sub_lang: u8, depth: u8, detail: u8) -> Self {
        Self(
            ((mode as u32) << 24)
                | ((sub_lang as u32) << 16)
                | ((depth as u32) << 8)
                | (detail as u32),
        )
    }

    #[inline]
    pub const fn mode(self) -> u8 {
        (self.0 >> 24) as u8
    }

    #[inline]
    pub const fn sub_lang(self) -> u8 {
        ((self.0 >> 16) & 0xFF) as u8
    }

    #[inline]
    pub const fn depth(self) -> u8 {
        ((self.0 >> 8) & 0xFF) as u8
    }

    #[inline]
    pub const fn detail(self) -> u8 {
        (self.0 & 0xFF) as u8
    }

    #[inline]
    pub const fn is_normal(self) -> bool {
        self.0 == 0
    }
}

/// Static declarative metadata for a compiled Rust language plugin.
#[derive(Debug, Clone, Copy)]
pub struct LanguageMeta {
    /// Canonical lowercase identifier (e.g. `"rust"`, `"cpp"`, `"csharp"`, `"typescript"`).
    pub id: &'static str,
    /// Human-readable status-bar name (e.g. `"Rust"`, `"C++"`, `"C#"`, `"TypeScript"`).
    pub name: &'static str,
    /// Alternate names/tags recognized by CLI `syntax set` and Markdown `` ```tag `` fences.
    pub aliases: &'static [&'static str],
    /// Lowercase file extensions without leading dot.
    pub extensions: &'static [&'static str],
    /// Exact lowercase filenames matched without extension (e.g. `"makefile"`, `"dockerfile"`).
    pub filenames: &'static [&'static str],
    /// Line-comment prefix (automatically powers `⌘/` toggling).
    pub line_comment: Option<&'static str>,
    /// Block-comment delimiter pair `(open, close)`.
    pub block_comment: Option<(&'static str, &'static str)>,
    /// Whether prose-oriented word wrap should default to on.
    pub default_word_wrap: bool,
}

/// Trait implemented by every compiled Rust language plugin.
///
/// Adding a new language takes 1 file in `crates/editor-core/src/syntax/langs/`
/// plus 1 line in `langs::BUILTIN_PLUGINS`.
pub trait SyntaxPlugin: Send + Sync {
    fn meta(&self) -> &'static LanguageMeta;

    /// Lex a single line starting from `state`, appending sorted non-overlapping
    /// byte spans into `out`, and returning the outgoing [`LexState`] for the next line.
    fn lex_line(
        &self,
        line: &str,
        state: LexState,
        registry: &LanguageRegistry,
        out: &mut Vec<TokenSpan>,
    ) -> LexState;
}

/// Lightweight 2-byte handle to a registered language plugin (`0` = Plain Text).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Language(pub u16);

impl Language {
    pub const PLAIN_TEXT: Self = Self(0);

    #[inline]
    pub fn from_path(path: Option<&Path>) -> Self {
        LanguageRegistry::global().detect_path(path, None)
    }

    #[inline]
    pub fn from_path_with_overrides(
        path: Option<&Path>,
        overrides: &BTreeMap<String, String>,
    ) -> Self {
        LanguageRegistry::global().detect_path(path, Some(overrides))
    }

    #[inline]
    pub fn from_id(id_or_alias: &str) -> Option<Self> {
        LanguageRegistry::global().find(id_or_alias)
    }

    #[inline]
    pub fn meta(self) -> &'static LanguageMeta {
        LanguageRegistry::global().plugin(self).meta()
    }

    #[inline]
    pub fn id(self) -> &'static str {
        self.meta().id
    }

    #[inline]
    pub fn display_name(self) -> &'static str {
        self.meta().name
    }

    #[inline]
    pub fn line_comment(self) -> Option<&'static str> {
        self.meta().line_comment
    }

    #[inline]
    pub fn block_comment(self) -> Option<(&'static str, &'static str)> {
        self.meta().block_comment
    }

    #[inline]
    pub fn default_word_wrap(self) -> bool {
        self.meta().default_word_wrap
    }

    /// Return all registered language handles (starting with Plain Text at index 0).
    pub fn all() -> Vec<Self> {
        let n = LanguageRegistry::global().len();
        (0..n).map(|i| Language(i as u16)).collect()
    }
}

/// Static registry of compiled Rust language plugins.
pub struct LanguageRegistry {
    plugins: &'static [&'static dyn SyntaxPlugin],
}

static GLOBAL_REGISTRY: LanguageRegistry = LanguageRegistry {
    plugins: langs::BUILTIN_PLUGINS,
};

impl LanguageRegistry {
    #[inline]
    pub fn global() -> &'static Self {
        &GLOBAL_REGISTRY
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.plugins.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    #[inline]
    pub fn plugins(&self) -> &'static [&'static dyn SyntaxPlugin] {
        self.plugins
    }

    #[inline]
    pub fn all(&self) -> impl Iterator<Item = Language> {
        (0..self.plugins.len()).map(|i| Language(i as u16))
    }

    #[inline]
    pub fn plugin(&self, lang: Language) -> &'static dyn SyntaxPlugin {
        self.plugins
            .get(lang.0 as usize)
            .copied()
            .unwrap_or(self.plugins[0])
    }

    /// Look up a language by canonical `id`, `alias`, or `extension` (case-insensitive).
    pub fn find(&self, query: &str) -> Option<Language> {
        let q = query.trim().trim_start_matches('.').to_ascii_lowercase();
        if q.is_empty() {
            return None;
        }
        // 1. Exact ID match first.
        for (i, p) in self.plugins.iter().enumerate() {
            if p.meta().id == q {
                return Some(Language(i as u16));
            }
        }
        // 2. Alias match.
        for (i, p) in self.plugins.iter().enumerate() {
            if p.meta().aliases.iter().any(|&a| a == q) {
                return Some(Language(i as u16));
            }
        }
        // 3. Extension match.
        for (i, p) in self.plugins.iter().enumerate() {
            if p.meta().extensions.iter().any(|&e| e == q) {
                return Some(Language(i as u16));
            }
        }
        None
    }

    /// Resolve a file path using optional user/CLI extension overrides first, then plugin metadata.
    pub fn detect_path(
        &self,
        path: Option<&Path>,
        ext_overrides: Option<&BTreeMap<String, String>>,
    ) -> Language {
        let Some(p) = path else {
            return Language::PLAIN_TEXT;
        };
        if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
            let ext_low = ext.to_ascii_lowercase();
            if let Some(ov) = ext_overrides {
                if let Some(lang_id) = ov.get(&ext_low) {
                    if let Some(lang) = self.find(lang_id) {
                        return lang;
                    }
                }
            }
            for (i, plugin) in self.plugins.iter().enumerate() {
                if plugin.meta().extensions.iter().any(|&e| e == ext_low) {
                    return Language(i as u16);
                }
            }
        }
        if let Some(fname) = p.file_name().and_then(|n| n.to_str()) {
            let fname_low = fname.to_ascii_lowercase();
            for (i, plugin) in self.plugins.iter().enumerate() {
                if plugin.meta().filenames.iter().any(|&f| f == fname_low) {
                    return Language(i as u16);
                }
            }
        }
        Language::PLAIN_TEXT
    }
}

/// Incremental per-line cross-line state cache with early convergence and windowed bounded memory.
///
/// For normal files (`<= 4,000` lines scrolled from start), `cache_offset == 0` and `end_states[i]`
/// holds the outgoing state of line `i`. When randomly jumping far into a 1M-line file, `cache_offset`
/// slides to `target - 256` so memory stays ~1–2 KB instead of allocating megabytes of state.
#[derive(Debug, Default)]
pub struct SyntaxCache {
    language: Language,
    /// Line index corresponding to `end_states[0]`.
    cache_offset: usize,
    /// `end_states[line - cache_offset]` holds the outgoing `LexState` at the end of `line`.
    end_states: Vec<LexState>,
    /// Lowest line index whose outgoing state may be stale after an edit.
    dirty_from: usize,
    /// Highest line index touched by the most recent edit (cannot early-exit before this).
    dirty_through: usize,
}

impl SyntaxCache {
    pub fn new(language: Language) -> Self {
        Self {
            language,
            cache_offset: 0,
            end_states: Vec::new(),
            dirty_from: 0,
            dirty_through: 0,
        }
    }

    #[inline]
    pub fn language(&self) -> Language {
        self.language
    }

    #[inline]
    pub fn cached_len(&self) -> usize {
        self.end_states.len()
    }

    #[inline]
    fn get_end_state(&self, line: usize) -> Option<LexState> {
        let rel = line.checked_sub(self.cache_offset)?;
        self.end_states.get(rel).copied()
    }

    #[inline]
    fn set_end_state(&mut self, line: usize, state: LexState) {
        if line < self.cache_offset {
            self.cache_offset = line;
            self.end_states.clear();
        }
        let rel = line - self.cache_offset;
        if rel == self.end_states.len() {
            self.end_states.push(state);
        } else if rel < self.end_states.len() {
            self.end_states[rel] = state;
        } else if rel <= MAX_SYNC_CATCHUP_LINES + HUGE_JUMP_WARMUP_LINES {
            self.end_states.resize(rel + 1, LexState::NORMAL);
            self.end_states[rel] = state;
        } else {
            self.cache_offset = line;
            self.end_states.clear();
            self.end_states.push(state);
        }
    }

    pub fn set_language(&mut self, language: Language) {
        if self.language != language {
            self.language = language;
            self.cache_offset = 0;
            self.end_states.clear();
            self.dirty_from = 0;
            self.dirty_through = 0;
        }
    }

    /// Invalidate cached end states from `line` onward (truncating potentially shifted states
    /// after line-count-changing operations such as undo, redo, multi-cursor edits, or replace-all).
    pub fn invalidate_from(&mut self, line: usize) {
        if line < self.cache_offset {
            self.cache_offset = 0;
            self.end_states.clear();
            self.dirty_from = 0;
            self.dirty_through = line;
        } else {
            let rel = line - self.cache_offset;
            if rel < self.end_states.len() {
                self.end_states.truncate(rel);
            }
            self.dirty_from = self.dirty_from.min(line);
            self.dirty_through = self.dirty_through.max(line);
        }
    }

    /// Adjust cached end-states when an edit at `line` changes the line count by `delta_lines`.
    pub fn notify_edit(&mut self, start_line: usize, delta_lines: isize) {
        self.dirty_from = self.dirty_from.min(start_line);
        if start_line < self.cache_offset {
            // Edit happened before our sliding window -> reset window cleanly on next render.
            self.cache_offset = 0;
            self.end_states.clear();
            self.dirty_from = 0;
            self.dirty_through = start_line;
            return;
        }
        let rel = start_line - self.cache_offset;
        if delta_lines > 0 && rel < self.end_states.len() {
            let add = delta_lines as usize;
            if add > 4096 {
                // Large paste: truncate tail so shifted states cannot cause premature convergence.
                self.end_states.truncate(rel);
                self.dirty_through = self.dirty_through.max(start_line);
            } else {
                let fill = self
                    .end_states
                    .get(rel)
                    .copied()
                    .unwrap_or(LexState::NORMAL);
                self.end_states
                    .splice(rel..rel, std::iter::repeat_n(fill, add));
                self.dirty_through = self.dirty_through.max(start_line + add);
            }
        } else if delta_lines < 0 && rel < self.end_states.len() {
            let rem = (-delta_lines) as usize;
            let end = (rel + rem).min(self.end_states.len());
            self.end_states.drain(rel..end);
            self.dirty_through = self.dirty_through.max(start_line);
        } else {
            self.dirty_through = self.dirty_through.max(start_line);
        }
    }

    /// Ensure cached cross-line state is accurate up to `target_line` (for `start_state` of `target_line`),
    /// pulling line text on demand via `get_line`. Bounded to keep 1M-line random jumps instant & ~1.5 KB!
    pub fn catch_up_to<F>(&mut self, target_line: usize, mut get_line: F)
    where
        F: FnMut(usize) -> Option<String>,
    {
        if self.language == Language::PLAIN_TEXT || target_line == 0 {
            return;
        }
        let cached_end = self.cache_offset + self.end_states.len();

        // Already valid up to `target_line` within our cached window?
        if target_line >= self.cache_offset
            && self.dirty_from >= target_line
            && cached_end >= target_line
        {
            return;
        }

        // Did the user jump backwards before our sliding window or far ahead past the cap?
        let mut start = if target_line < self.cache_offset {
            let new_off = target_line.saturating_sub(HUGE_JUMP_WARMUP_LINES);
            self.cache_offset = new_off;
            self.end_states.clear();
            self.dirty_from = new_off;
            self.dirty_through = 0;
            new_off
        } else {
            self.dirty_from.max(self.cache_offset).min(cached_end)
        };

        if target_line.saturating_sub(start) > MAX_SYNC_CATCHUP_LINES {
            let new_off = target_line.saturating_sub(HUGE_JUMP_WARMUP_LINES);
            self.cache_offset = new_off;
            self.end_states.clear();
            self.dirty_from = new_off;
            self.dirty_through = 0;
            start = new_off;
        }

        let registry = LanguageRegistry::global();
        let plugin = registry.plugin(self.language);
        let mut scratch = Vec::with_capacity(64);

        let mut state = if start == 0 || start <= self.cache_offset {
            LexState::NORMAL
        } else {
            self.get_end_state(start - 1).unwrap_or(LexState::NORMAL)
        };

        for line_idx in start..target_line {
            let Some(text) = get_line(line_idx) else {
                break;
            };
            let next_state = if text.is_empty() || text.len() > MAX_LEX_LINE_BYTES {
                state
            } else {
                scratch.clear();
                plugin.lex_line(&text, state, registry, &mut scratch)
            };
            let rel = line_idx - self.cache_offset;
            if rel < self.end_states.len() {
                let prev_cached = self.end_states[rel];
                self.end_states[rel] = next_state;
                if next_state == prev_cached && line_idx >= self.dirty_through {
                    // Converged! All subsequent cached states remain valid.
                    self.dirty_from = self.cache_offset + self.end_states.len();
                    self.dirty_through = 0;
                    return;
                }
            } else {
                self.end_states.push(next_state);
            }
            state = next_state;
        }
        self.dirty_from = self.dirty_from.max(target_line);
        if self.dirty_from > self.dirty_through {
            self.dirty_through = 0;
        }
    }

    /// Lex `line_text` at `line_idx`, updating cached end state and emitting tokens into `out`.
    pub fn lex_line(
        &mut self,
        line_idx: usize,
        line_text: &str,
        out: &mut Vec<TokenSpan>,
    ) -> LexState {
        out.clear();
        if self.language == Language::PLAIN_TEXT {
            return LexState::NORMAL;
        }
        let start_state = if line_idx == 0 {
            LexState::NORMAL
        } else {
            self.get_end_state(line_idx - 1).unwrap_or(LexState::NORMAL)
        };
        if line_text.is_empty() || line_text.len() > MAX_LEX_LINE_BYTES {
            self.set_end_state(line_idx, start_state);
            if line_idx == self.dirty_from {
                self.dirty_from = line_idx + 1;
            }
            return start_state;
        }
        let registry = LanguageRegistry::global();
        let end_state = registry
            .plugin(self.language)
            .lex_line(line_text, start_state, registry, out);

        self.set_end_state(line_idx, end_state);
        if line_idx == self.dirty_from {
            self.dirty_from = line_idx + 1;
        }
        end_state
    }
}

/// Stateless one-shot helper for tests and embedded sub-lexing.
pub fn lex_line_stateless(
    lang: Language,
    state: LexState,
    line: &str,
    out: &mut Vec<TokenSpan>,
) -> LexState {
    let registry = LanguageRegistry::global();
    registry.plugin(lang).lex_line(line, state, registry, out)
}
