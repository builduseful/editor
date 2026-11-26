# editor — Rust Architecture (v2)

A zero-dependency, ultra-fast, custom-drawn native text editor written in Rust,
following the **Build Useful** design system.

## Core Design Principles

1. **Zero third-party libraries.** Every layer — text buffer, line index, UTF-8 /
   emoji grapheme segmentation, 32-byte SWAR scanner, linear-time regex engine,
   syntax tokenizer, glyph atlas, 2D software rasterizer, custom UI controls,
   OS/`.`gitignore` walker, JSON serializer/parser, IPC, and CLI — is written in
   pure Rust (`std` only). Zero external crates means sub-second incremental
   builds, full auditability, tiny binaries, and complete architectural control.
2. **Minimal, razor-thin Platform Adapter boundary (`editor-platform`).** Only
   irreducible OS services sit behind Rust traits:
   - `FileSystem`: file I/O (`pread`/`pwrite`), `read_prefix_into`, stat/identity,
     directory reads, atomic temp+rename, CoW clone (`clonefile` on APFS with
     portable copy fallback), advisory lockfile.
   - `GlyphRasterizer`: **2 methods** (`font_metrics`, `rasterize`) returning an
     `Alpha8` coverage bitmap (text) or `Rgba32` bitmap (color emoji) for a
     single grapheme cluster. The OS font engine is only called on atlas misses.
   - `Clipboard`: `get_text` / `set_text`.
   - `PlatformHost`: window title, cursor shape, open/save/confirm dialogs,
     persisted preferences, single-stream directory watcher (`FSEvents` on macOS,
     with optional secondary `.git` metadata directory for monorepo subfolders
     and worktrees), redraw/quit signals, native HTTPS fetcher, and optional
     `MainLoopWaker` for zero-polling background wakeups.
   - `IgnoreRules`, `GitIgnoreLayer` & `matches_path_filter`: declarative per-OS
     workspace ignore tables (`macos`, `linux`, `windows`, `unix_generic`),
     hierarchical `.gitignore` matching (including ancestor `.gitignore` layers
     for monorepo subfolders and boundary resets at nested `.git` repos), and
     interactive glob/exclusion path filtering for Search in Files.
   - `git`: pure-Rust repo discovery (`discover_repo`, supporting worktrees and
     submodules) and `.git/HEAD` branch parsing (`< 50 µs`, zero subprocesses),
     bounded multi-repo `GitStatusCache`, and lock-free `git --no-optional-locks`
     status/HEAD helpers (pathspec-scoped, single-flight, adaptive cooldown).
   - `version` & `update`: single-source-of-truth SemVer parsing, pure-Rust
     SHA-256 verification, CDN `release.json` manifest validation, and atomic
     bundle swap for GitHub Releases auto-updates.
   - **Presentation**: the renderer paints directly into a premultiplied BGRA8
     surface (`IOSurface` zero-copy on macOS) with dirty rectangles. Porting to
     Windows or Linux requires only a thin OS adapter with pixel-identical output.
3. **Speed, memory, CPU, and battery first.**
   - **Monoid B-Tree Piece Table (`editor-core::piece_tree`)**: immutable
     windowed/paged base file (opens multi-GB files lazily in < 1 ms without
     loading them into RAM), append-only segmented add buffer, order-statistic
     B-tree with a CRLF-aware monoid summary `(bytes, breaks, starts_with_lf,
     ends_with_cr)` for O(log N) exact line/offset queries and O(1) typing
     coalescing. Large streaming reads (`>= 64 KB`) bypass the page cache.
   - **32-Byte SWAR & Linear-Time Regex (`editor-core::{swar, regex, search}`)**:
     4×`u64` unrolled SWAR byte search, rarest-pair byte filtering using an
     empirical 256-byte frequency table, branchless ASCII case-insensitive search,
     and a ReDoS-proof Thompson NFA / PikeVM regex engine (`O(N × M)`) with
     literal/alternation prefilters and `$0`–`$9` capture replacement.
   - **L1-Hot Shelf-Packed Glyph Atlas + Dirty-Rect Compositor (`editor-render`)**:
     flat 96-entry direct-index ASCII table (zero hashing), open-addressed cache
     for Unicode/Emoji/UI glyphs, scroll-blit (`memmove` surviving rows), and
     dirty-rect tracking so caret blink touches < 200 pixels (`~5–10 µs`).
   - **Instant Workspace Search & Live Myers Diff (`editor-render::palette`, `editor-core::diff`)**:
     two-pane Quick Open (`⌘P`) and Search in Files (`⇧⌘F`) with live syntax-
     highlighted preview, opt-in `Paths` glob filter and confirmed `Replace in Files`,
     hybrid 8 ms sync head-start → cancellable background streaming for huge
     directories (e.g. `$HOME`), and a unified lazy-filling 20 MB modal-lifetime
     `SearchSlab` that frees 100% of its RAM on close. Live gutter diff ticks run
     an in-memory Myers O(ND) diff over CRLF-normalized FNV-1a 64-bit line hashes
     (`< 0.1 ms` on edits, zero subprocesses while typing).
   - **Zero idle CPU**: event-driven main loop with coalesced OS events and no
     background polling timers or speculative filesystem scans.
4. **Focused Unicode support**: strict UTF-8, English, programming languages,
   and an ergonomic UAX #29 Emoji grapheme cluster subset (combining marks,
   variation selectors, skin tones, regional indicator flags, ZWJ sequences,
   keycaps).
5. **Self-contained distribution**: on macOS, `bundle-app` renders the Build
   Useful `.icns` icon in pure Rust and packages both `editor-gui` and `editor`
   into one `Editor.app` bundle (`Contents/MacOS/`). `scripts/install-macos.sh`
   installs `Editor.app` atomically and links a single `editor` symlink into
   `PATH` so GUI and CLI stay atomically version-locked with zero scattered files.
6. **Future-ready seams**: `ImeComposition` (preedit text + caret rect) and
   `A11yNode` snapshots are structurally modeled so native IME and accessibility
   plug in cleanly without rewriting core or rendering layers.

## Crate Dependency Graph

```text
                ┌───────────────┐
                │editor-platform│  (traits, POD types, ignore, version, IPC, mocks)
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
 │ editor-cli  │ (shares editor-core + editor-platform directly)
 └─────────────┘
```

## Module Overview

- `crates/editor-platform`: `fs`, `git`, `host`, `ignore`, `input`, `ipc`, `raster`, `surface`, `update`, `version`.
- `crates/editor-core`: `piece_tree`, `document`, `diff`, `swar`, `regex`, `search`, `grapheme`, `undo`, `syntax`.
- `crates/editor-render`: `theme`, `atlas`, `painter`, `viewport`, `sidebar`, `tab_strip`, `chrome`, `text_input`, `palette`, `pins`, `shortcuts`.
- `crates/editor-app`: `lib` (`EditorApp` controller), `keymap`, `buffers`, `panes`, `session`, `autopairs`, `fs_undo`, `vim`, `vim_exec`.
- `crates/editor-cli`: `main`, `json`, `json_parse`.
- `crates/editor-macos`: `main` (`editor-gui`), `bin/bundle_app` (`Editor.app` + pure-Rust `.icns` packager), `objc/mac_adapter.m`.
- `scripts/install-macos.sh`: zero-clutter macOS installer and uninstaller.
