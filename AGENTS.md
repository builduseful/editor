# AGENTS.md

## Project

`editor` — an ultra-lightweight, zero-dependency, custom-drawn native text editor
implemented in **Rust** (`std` only, zero third-party crates), with a minimal
platform adapter boundary (`editor-platform`) and a deterministic CLI (`editor`).
`PLAN.md` and `ARCHITECTURE.md` are the sources of truth for scope, decisions,
and architecture. The archived C++ v1 implementation lives in
`original-implementation/`.

## Design and brand

This app follows the **Build Useful design system** for brand consistency: one ink,
a short ladder of greys, the operating system's UI typeface, a 2px spacing rhythm,
quiet controls, and colour only for state (danger / success / info). It is a brand
reference, not a component library.

- **Do not vendor the design system's web CSS/JS into this native app.** Implement
  the look natively in our pure-Rust custom renderer (`editor-render`).
- Use its tokens as the reference: ink and grey palette, light/dark themes, spacing
  scale, type ramp, subtle borders, minimal chrome. No accent hue.
- The resolved colour tokens live in one place:
  `crates/editor-render/src/theme.rs` (`Theme::light()` / `Theme::dark()`).
  Keep them in sync with the design system's colour tokens; never hard-code a
  colour elsewhere. The app offers System / Light / Dark and applies the matching
  theme.
- Use the system UI font (`FontStyle::Ui` / `UiBold`) for chrome; the system
  fixed-pitch mono face (`FontStyle::Mono` / `MonoBold`) for the document and
  technical values.
- Never commit local paths, personal data, or proprietary design-system files.
- `LICENSE` follows the design system's app-licence template.

### UI & interaction rules

- **Tokens for every value.** Colours come from `editor-render::theme`; spacing
  follows the 2px ladder; borders are hairlines.
- **No accent hue.** Colour marks state only (danger / success / info). Selection,
  current line, gutter and scrollbars are neutral greys — not blue.
- **Both themes, always.** Anything drawn must look right in light and dark;
  verify both (`editor-gui --snapshot OUT.png --theme light|dark`).
- **Quiet, minimal chrome.** No gradients, spinners, illustrations or decorative
  motion.
- **Copy style:** actions in sentence case starting with a verb; headings in Title
  Case; no exclamation marks.
- **100% custom-drawn controls** — zero native-control hybrids (`NSOutlineView`,
  `NSScrollView`, `NSTextField`, `NSAlert`). Every widget (viewport, gutter,
  overlay scrollbars, lazy sidebar, find/replace bar, go-to-line bar, quick open,
  project search, status bar, confirmation modal) is rasterized by `editor-render`.
- **The horizontal scrollbar keeps its own space** (`H_SCROLLBAR_STRIP_PT = 9.0`),
  clipping text cleanly so rows never collide with the thumb. Vertical scrolling
  clamps flush to the bottom of the viewport with no excess whitespace.
- **The file sidebar is lazy by construction.** Directories are read only when
  expanded or navigated into; selection is held by `PathBuf`; opening a file never
  re-roots the sidebar; left chevron expands inline, right chevron enters folder,
  breadcrumbs navigate back; right edge is drag-resizable (160–480 pt, persisted
  under `SidebarWidth`).

## Engineering

- **Rust 2021, zero external crates (`std` only).** No third-party crates in
  `Cargo.toml`.
- **Performance, memory, CPU, and battery first.** Every feature must stay
  extremely lightweight and resource-efficient across all platforms:
  - **Zero idle CPU and battery-friendly by design.** Never add busy loops,
    background polling timers, or speculative filesystem scans. Prefer OS
    event-driven primitives, coalesce bursty events, and stay completely idle
    when unfocused or hidden.
  - **Minimal memory footprint.** Prefer lazy loading/paging, compact flat
    tables, bounded caches, and zero unnecessary allocations on hot paths
    (typing, scrolling, lexing, layout, painting).
  - **Sub-millisecond hot paths and incremental rendering.** Redraw only
    damaged regions, keep edit/search/render latency flat even on very large or
    pathological files, and verify with benchmarks.
  - **Minimal disk and install footprint.** Keep binaries small and avoid
    scattering caches, metadata, or state files across the user's machine.
- **Minimal platform adapter (`crates/editor-platform`).** Only irreducible OS
  boundaries sit behind traits (`FileSystem`, 2-method `GlyphRasterizer`,
  `Clipboard`, `PlatformHost`, and BGRA8 framebuffer + dirty-rect presentation).
  Platform glue lives exclusively in `crates/editor-macos/` (currently ~500 lines
  of ObjC/C compiled by system `clang` via `build.rs`, using `IOSurface` for
  zero-copy dirty-rect compositing and CoreText strictly on glyph-atlas misses).
- **Focused Unicode**: strict UTF-8, English, programming languages, and an Emoji
  grapheme cluster subset (combining marks, VS15/VS16, skin tones, flag pairs,
  ZWJ chains, keycaps). `ImeComposition` and `A11yNode` seams keep future IME and
  accessibility integration clean.
- **Build, test, bundle, and benchmark commands**:
  - `cargo test --workspace` — run all unit, property/oracle, render, app, and CLI
    tests (must compile with zero warnings).
  - `cargo build --release --workspace --bins` — build `target/release/editor`
    (CLI) and `target/release/editor-gui` (GUI).
  - `./target/release/bundle-app` — render the Build Useful `.icns` icon in pure
    Rust and package + ad-hoc sign `target/release/Editor.app`.
  - `cargo run --release -p editor-app --example perf_bench` — run the 60 MB / 1M
    line + 10 MB single-line + Retina frame + macOS `phys_footprint` benchmark.
  - `./target/release/editor-gui --snapshot OUT.png --theme light|dark --size WxH --sidebar-root DIR [FILE]`
    — headless CoreText Retina PNG snapshot for visual verification.

## Repo hygiene

- **Never add or commit personal information.** No personal paths (`/Users/...`,
  home directories, usernames), local machine details, emails, tokens, or
  secrets in any tracked file, test, comment, or commit message.
- Keep tracked files minimal and commit in clean, logical chunks.
