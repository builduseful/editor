# editor — Rust v2 Architecture & Implementation Plan

> Zero-dependency, custom-drawn, ultra-fast native text editor in Rust following
> the **Build Useful** design system.
>
> This plan has been critically reviewed twice for architectural boundaries,
> performance bottlenecks, macOS event/menu nuances, pathological file inputs
> (100 MB single lines, multi-GB files), and cross-platform portability.

---

## 1. Core Architectural Decisions

| ID | Decision | Rationale |
| --- | --- | --- |
| R1 | **100% Pure Rust (`std` only), zero third-party crates.** | Full control, sub-second incremental compiles, ~500 KB binaries, zero supply-chain bloat, offline builds. |
| R2 | **Minimal Adapter Pattern (`editor-platform`).** | Only irreducible OS boundaries sit behind traits: `FileSystem`, 2-method `GlyphRasterizer`, `Clipboard`, `PlatformHost`, and raw BGRA8 framebuffer presentation. 95%+ of the editor compiles untouched on any OS. |
| R3 | **Custom CPU Software Rasterizer + Shelf-Packed Glyph Atlas (`editor-render`).** | Pixel-identical rendering across all OSes, zero native-control hybrids (`NSOutlineView`/`NSScrollView`/`NSTextField`), O(1) 96-entry direct-indexed ASCII table, sub-millisecond dirty-rect & scroll-blit frames. |
| R4 | **Monoid Order-Statistic B-Tree Piece Table (`editor-core`).** | Associative CRLF-aware `Summary { bytes, breaks, starts_with_lf, ends_with_cr }` keeps exact line counts across arbitrary edits in O(log N). Paged 512 KB base cache + 16 KB lazy SWAR indexing opens multi-GB files in < 1 ms. |
| R5 | **64-bit SWAR in 100% Safe Rust.** | 8-byte-at-a-time ASCII validation, newline scanning, and literal search at > 3 GB/s without unsafe platform intrinsics. |
| R6 | **Focused Unicode: UTF-8, English, Code, & Emoji Subset.** | Fast 1-byte ASCII path + clean UAX #29 Emoji subset (combining marks, VS15/VS16, skin tones, flag pairs, ZWJ chains, keycaps). |
| R7 | **Future-Ready IME & Accessibility Seams.** | `ImeComposition` (preedit + caret rect) and `A11yNode` snapshot live in the core render/app contracts without bloating v1. |
| R8 | **Build Useful Design System Tokens Only.** | Exact Light & Dark tokens in `editor-render::theme`; one ink, neutral grey ladder, 2px spacing rhythm, hairlines, color strictly for state (danger/success/info). |

---

## 2. Workspace Crate Graph

```text
crates/
├── editor-platform/   # Pure traits + POD types + StdFileSystem + MemoryFileSystem
│                      # + BuiltinBitmapRasterizer + Framebuffer + PNG encoder
├── editor-core/       # PieceTree, SWAR, Grapheme/Emoji, Undo, Search, Syntax, Document
├── editor-render/     # Build Useful Theme, GlyphAtlas, Painter, Viewport, Sidebar,
│                      # TextInput, Find/Replace, Go-To-Line, Modal Confirm, Status Bar
├── editor-app/        # Platform-agnostic App controller (shortcuts, mouse, focus,
│                      # partial dirty-rect redraws, preferences, a11y snapshot)
├── editor-cli/        # Deterministic JSON CLI (`editor` binary): read, write, edit,
│                      # info, validate, batch, version, schema
└── editor-macos/      # Minimal macOS adapter (~480 lines ObjC/C via build.rs):
                       # CoreText single-cluster rasterizer, CALayer BGRA8 blit,
                       # events, menus, drag-and-drop, NSUserDefaults, APFS clonefile,
                       # + Editor.app bundle generator with Build Useful icon
```

---

## 3. Hardening & Feature Completion Milestones (Reviewed Twice)

### Chunk 1 — Archive + Plan + Foundation Commit
- [x] Move original C++ implementation to `original-implementation/`.
- [x] Establish 6-crate Rust workspace with zero external crates.
- [x] Commit archived original implementation, `PLAN.md`, and initial 6-crate foundation.

### Chunk 2 — Core Engine Hardening (`editor-core` + `editor-platform`)
- [x] **Lockfile + APFS Tail CoW Save**:
  - Add advisory lockfile (`<path>.editor.lock`, `O_CREAT|O_EXCL`) to `FileSystem` so concurrent saves never race.
  - Add `clone_pwrite_tail` save path (length-changing edits with tail <= 25% of file use APFS `clone_file` + `pwrite` + `ftruncate` + atomic `rename`).
- [x] **External Mutation Detection**: `Document::check_identity(fs)` returning `Unchanged | ReloadedClean | StaleDirty`, wired into `FocusChanged(true)`.
- [x] **Pathological Single-Line Protection**: `Document::read_line_window(line, byte_start, max_bytes)` + `line_byte_len(line)` so a 100 MB single-line file never allocates 100 MB in the viewport!
- [x] **Multi-Line Bracket Matching + Tree Hygiene**: `find_matching_bracket` (up to 32 KB scan) + leaf-merging on sparse B-tree deletes.
- [x] **Expanded Stress & Fuzz Tests**: randomized PieceTree oracle, heavy delete compaction, lockfile tests, external mutation tests, 2 MB single-line window tests.

### Chunk 3 — Renderer & Custom UI Excellence (`editor-render` + `editor-app`)
- [x] **True Dirty-Rect Partial Redraws**: Caret blink marks only the caret line rect dirty (skipping sidebar, status bar, and untouched lines -> sub-0.05 ms blink frames!).
- [x] **Huge Single-Line Viewport Fast Path**: O(1) ASCII column calculation + windowed line reading so a 10 MB+ single line renders in < 1 ms with flat RAM.
- [x] **Custom-Drawn In-App Confirm Modal**: Pure-Rust modal overlay ("Save Changes to …?" with Save / Don't Save / Cancel buttons & keyboard shortcuts) so unsaved-change prompts work identically on every OS.
- [x] **Pro Editor Ergonomics**:
  - Matching bracket 1px subtle highlight box in viewport.
  - Subtle vertical indent guides (`border_subtle` hairlines).
  - Gutter click/drag selects full logical lines.
  - Multi-line Tab / Shift+Tab indent & outdent.
  - Line actions: Duplicate Line (`Cmd+Shift+D`), Move Line Up/Down (`Option+Up/Down`), Delete Line (`Cmd+Shift+K`), Toggle Line Comment (`Cmd+/`).
  - Mouse click & drag selection inside `TextInput` (Find / Replace / Go-To-Line).
  - Menu `Copy`/`Cut`/`Paste`/`SelectAll` correctly routed to focused `TextInput` when Find/Go-To-Line is active!

### Chunk 4 — Complete Deterministic CLI (`editor-cli`)
- [x] Hand-rolled strict zero-dependency JSON parser (`json_parse.rs`) with unit tests.
- [x] Complete CLI features:
  - Global `--root DIR` symlink-safe confinement (`E_OUTSIDE_ROOT`).
  - `read` byte axis (`--from-offset B --length L`) in addition to line axis (`--from`/`--to`).
  - `edit` positional form (`--at LINE[:COL] --op insert|replace|delete --text ... --length ...`), `--count N`, `--dry-run`, and `@FILE` / `-` payloads.
  - `batch` JSON-lines transactional multi-file command (`--file F` / stdin, `--check`, atomic rollback on commit failure).
- [x] Comprehensive CLI integration test suite covering every command, error code, and rollback path.

### Chunk 5 — macOS Adapter Polish, Preferences, Drag-and-Drop & `Editor.app` Bundle (`editor-macos`)
- [x] Fix `keyDown:` to pass both `charactersIgnoringModifiers` (for reliable `Option+Z`, `Cmd+/`, `Option+Up/Down`, etc.) AND `characters` (for UTF-8/emoji text entry).
- [x] Persisted preferences via `PlatformHost` (`load_pref` / `save_pref` backed by `NSUserDefaults`: `SidebarWidth`, `Theme`, `FontSize`, `WordWrap`).
- [x] Window drag-and-drop for files and folders + `application:openFiles:` (Finder "Open With").
- [x] Pure-Rust **Build Useful App Icon generator** + `Editor.app` packager binary (`crates/editor-macos/src/bin/bundle_app.rs`) rendering the 512pt dark tile (`#111111`, 112pt radius) with crisp caret motif (`#f1f1f1`) at 16/32/128/256/512 @1x/@2x into `.iconset` -> `iconutil` -> `target/release/Editor.app`!

### Chunk 6 — Final Full Verification, Performance Benchmarks & Visual QA
- [x] Run full `cargo test --workspace` (zero warnings, all unit/property/integration tests green).
- [x] Run release benchmarks (60 MB / 1M lines + 10 MB single-line pathological file + caret-blink partial frame + Retina frame times + binary sizes).
- [x] Render & visually inspect fresh Light & Dark macOS CoreText Retina PNG snapshots.
- [x] Build & verify `target/release/Editor.app`.
