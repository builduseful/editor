# Compiled Rust Syntax Plugins & Customizable Themes

`editor` implements syntax highlighting as **zero-overhead compiled Rust plugins** in `crates/editor-core/src/syntax/` paired with a **flat, cache-line-compact O(1) customizable syntax palette** in `crates/editor-render/src/theme.rs`.

## Architecture Highlights

- **1 file = 1 compiled Rust plugin**: every language in `crates/editor-core/src/syntax/langs/` implements `SyntaxPlugin` with static `LanguageMeta` and registers via 1 line in `langs::BUILTIN_PLUGINS`. Zero JS/WASM runtime, zero FFI, zero third-party crates.
- **21 built-in languages**: Plain Text, C++, C#, Rust, Markdown, HTML, CSS, JavaScript, TypeScript, Python, Java, Bash/Shell, PowerShell, Go, SQL, PHP, C, Kotlin, JSON, TOML/YAML/INI, Swift.
- **Native language embedding**: plugins receive `&LanguageRegistry`, so HTML delegates `<style>` / `<script>` blocks to CSS / JavaScript and Markdown delegates ```` ```lang ```` fenced code blocks to any registered language plugin.
- **Windowed 4-byte `LexState(u32)` cache**: early convergence on edits (stops as soon as `new_state == old_state`), blank-line state preservation, 16 KB single-line guard, and a sliding window (`cache_offset`) that keeps 1,000,000-line random-jump memory under **2 KB**.
- **O(1) branchless customizable syntax palettes (`SyntaxPalette`)**: all 19 semantic tokens resolve once on theme edits into a flat 95-byte `[TokenStyle; 19]` array embedded directly in `Theme`. On the hot glyph rendering loop, `theme.syntax_font_style(kind)` is literally a single indexed load (`self.syntax.styles[kind as usize]`) — identical ~1 ns speed for both built-in presets and fully customized agent themes.

## Adding a New Language Plugin (1 File + 1 Line)

1. Create `crates/editor-core/src/syntax/langs/<lang>.rs` implementing `SyntaxPlugin`.
2. Export the module and add `&<lang>::PLUGIN` to `BUILTIN_PLUGINS` in `crates/editor-core/src/syntax/langs/mod.rs`.
3. Run `cargo test --workspace` — the generic plugin contract test in `crates/editor-core/tests/core_tests.rs` automatically validates every registered plugin for determinism, UTF-8 boundaries, non-overlapping sorted spans, and huge-line safety.

## Configuring Syntax Highlighting & Themes via CLI (Human & Agent)

```sh
# 1. Inspect compiled language plugins (works offline)
editor syntax list
editor languages

# 2. Override active buffer language or map custom file extensions (persisted)
editor syntax set rust
editor syntax map ino cpp
editor syntax map ino default

# 3. Inspect current syntax theme, all 19 canonical tokens, and resolved Light & Dark palettes
editor syntax theme

# 4. Switch built-in curated syntax presets (paired Light + Dark, persisted)
editor syntax theme editorial     # default: calm Build Useful architectural tones
editor syntax theme monochrome    # pure greyscale ink + bold/italic ladder
editor syntax theme studio        # crisper classic IDE separation
editor syntax theme warm          # warm sepia / olive / amber palette

# 5. Customize individual token colours & font styles (normal | bold | italic)
#    Defaults to the active theme polarity so Light & Dark stay independently legible!
editor syntax color comment '#55aa77' italic --mode dark
editor syntax color keyword '#6f42c1' bold --mode light
editor syntax color string '#2a7a5c' --mode both
editor syntax color comment reset --mode dark

# 6. Atomically apply a complete custom JSON theme (inline, @FILE, or --stdin)
editor syntax theme --json '{"preset":"editorial","replace":true,"light":{"keyword":"#27467a bold","comment":"#6c7480 italic"},"dark":{"keyword":"#c792ea bold","function":"#82aaff","string":"#c3e88d","comment":"#676e95 italic"}}'
editor syntax theme @my-theme.json

# 7. Clear custom token overrides back to the active preset
editor syntax reset               # all
editor syntax reset --mode dark   # dark only
```

### Canonical Semantic Token Names (19)

`plain`, `comment`, `keyword`, `type`, `function`, `string`, `escape`, `number`, `constant`, `macro`, `tag`, `attribute`, `property`, `operator`, `punctuation`, `heading`, `link`, `emphasis`, `strong`
