# editor — Rust Architecture (v2)

A zero-dependency, ultra-fast, custom-drawn native text editor written in Rust,
following the **Build Useful** design system.

## Core Design Principles

1. **Zero third-party libraries.** Every layer — text buffer, line index, UTF-8 /
   emoji grapheme segmentation, syntax tokenizer, glyph atlas, 2D software
   rasterizer, custom UI controls, JSON serializer/parser, CLI — is written in
   pure Rust (`std` only). Zero external crates means sub-second incremental
   builds, full auditability, tiny binaries, and complete architectural control.
2. **Minimal, razor-thin Platform Adapter boundary (`editor-platform`).** Only
   irreducible OS services sit behind Rust traits:
   - `FileSystem`: file I/O (`pread`/`pwrite`), stat/identity, directory reads,
     atomic temp+rename, CoW clone (`clonefile` on APFS with portable copy
     fallback), advisory lockfile.
   - `GlyphRasterizer`: **2 methods** (`font_metrics`, `rasterize`) returning an
     `Alpha8` coverage bitmap (text) or `Rgba32` bitmap (color emoji) for a
     single grapheme cluster. The OS font engine is ONLY called on atlas misses.
   - `Clipboard`: `get_text` / `set_text`.
   - `PlatformHost`: window title, cursor shape, open/save/confirm dialogs,
     redraw/quit signals.
   - **Presentation**: the renderer produces a premultiplied BGRA8 framebuffer +
     dirty rectangles; the platform adapter blits it directly to the window.
     Porting to Windows, Linux, or WASM requires ~300–500 lines of adapter glue
     with 100% pixel-identical rendering across every OS.
3. **Unbelievable speed & tiny memory footprint.**
   - **Monoid B-Tree Piece Table (`editor-core::piece_tree`)**: immutable
     windowed/paged base file (opens multi-GB files lazily in < 1 ms without
     loading them into RAM), append-only segmented add buffer, order-statistic
     B-tree with a CRLF-aware monoid summary `(bytes, breaks, starts_with_lf,
     ends_with_cr)` for O(log N) exact line/offset queries and O(1) typing
     coalescing.
   - **SWAR (64-bit word) hot paths**: 8-byte-at-a-time ASCII validation and
     newline scanning in 100% safe portable Rust.
   - **L1-hot Shelf-Packed Glyph Atlas + Software Tile/Dirty-Rect Compositor
     (`editor-render`)**: flat 96-entry direct-index ASCII table (zero hashing),
     open-addressed cache for Unicode/Emoji/UI glyphs, framebuffer scroll-blit
     (`memmove` surviving viewport rows + rasterize only newly exposed lines),
     dirty-rect tracking so caret blink touches < 200 pixels.
4. **Focused Unicode support**: strict UTF-8, English, programming languages,
   and an ergonomic UAX #29 Emoji grapheme cluster subset (combining marks,
   variation selectors, skin tones, regional indicator flags, ZWJ sequences,
   keycaps).
5. **Future-ready seams**: `ImeComposition` (preedit text + caret rect) and
   `A11yNode` snapshots are structurally modeled so IME and screen-reader
   support can plug in cleanly without rewriting core or rendering layers.

## Crate Dependency Graph

```text
                ┌───────────────┐
                │editor-platform│  (pure traits + POD types + headless mocks)
                └───────┬───────┘
                        │
        ┌───────────────┼───────────────┐
        ▼               ▼               ▼
 ┌─────────────┐ ┌─────────────┐ ┌─────────────┐
 │ editor-core │ │editor-macos │ │  (future:   │
 └──────┬──────┘ │  (thin OS   │ │  win/linux) │
        │        │   adapter)  │ └─────────────┘
        ▼        └──────┬──────┘
 ┌─────────────┐        │
 │editor-render│        │
 └──────┬──────┘        │
        ▼               │
 ┌─────────────┐        │
 │ editor-app  │◄───────┘
 └─────────────┘
        ▲
 ┌──────┴──────┐
 │ editor-cli  │ (shares editor-core directly)
 └─────────────┘
```
