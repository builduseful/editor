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
| R1 | **100% Pure Rust (`std` only), zero third-party crates.** | Full control, sub-second incremental compiles, tiny binaries, zero supply-chain bloat, offline builds. |
| R2 | **Minimal Adapter Pattern (`editor-platform`).** | Only irreducible OS boundaries sit behind traits: `FileSystem`, 2-method `GlyphRasterizer`, `Clipboard`, `PlatformHost`, `MainLoopWaker`, and raw BGRA8 framebuffer presentation. 95%+ of the editor compiles untouched on any OS. |
| R3 | **Custom CPU Software Rasterizer + Shelf-Packed Glyph Atlas (`editor-render`).** | Pixel-identical rendering across all OSes, zero native-control hybrids (`NSOutlineView`/`NSScrollView`/`NSTextField`), O(1) 96-entry direct-indexed ASCII table, sub-millisecond dirty-rect & scroll-blit frames. |
| R4 | **Monoid Order-Statistic B-Tree Piece Table (`editor-core`).** | Associative CRLF-aware `Summary { bytes, breaks, starts_with_lf, ends_with_cr }` keeps exact line counts across arbitrary edits in O(log N). Paged 512 KB base cache + 16 KB lazy SWAR indexing opens multi-GB files in < 1 ms. |
| R5 | **32-Byte SWAR & Linear-Time Regex in 100% Safe Rust.** | 4×`u64` unrolled SWAR scanning, rarest-pair byte filtering, and a ReDoS-proof Thompson NFA / PikeVM regex engine (`O(N × M)`) with literal/alternation prefilters and `$0`–`$9` capture replacement. |
| R6 | **Focused Unicode: UTF-8, English, Code, & Emoji Subset.** | Fast 1-byte ASCII path + clean UAX #29 Emoji subset (combining marks, VS15/VS16, skin tones, flag pairs, ZWJ chains, keycaps). |
| R7 | **Future-Ready IME & Accessibility Seams.** | `ImeComposition` (preedit + caret rect) and `A11yNode` snapshot live in the core render/app contracts without bloating v1. |
| R8 | **Build Useful Design System Tokens Only.** | Exact Light & Dark tokens in `editor-render::theme`; one ink, neutral grey ladder, 2px spacing rhythm, hairlines, colour strictly for state (danger/success/info). |
| R9 | **Single-Source Versioning & Self-Contained Distribution.** | `[workspace.package] version` in `Cargo.toml` feeds every binary and `Info.plist`. On macOS, `Editor.app` bundles both `editor-gui` and `editor` inside `Contents/MacOS/` so GUI and CLI stay atomically version-locked with a single symlink in `PATH`. |

---

## 2. Workspace Crate Graph

```text
crates/
├── editor-platform/   # Pure traits + POD types + StdFileSystem + MemoryFileSystem
│                      # + BuiltinBitmapRasterizer + Framebuffer + PNG encoder
│                      # + Per-OS IgnoreRules & .gitignore + SemVer version + IPC
├── editor-core/       # PieceTree, 32-byte SWAR, linear-time Regex, Search,
│                      # Grapheme/Emoji, Undo, Syntax plugins, Document
├── editor-render/     # Build Useful Theme, GlyphAtlas, Painter, Viewport, Sidebar,
│                      # TabStrip, TextInput, Find/Replace, Two-Pane Palette & SearchSlab,
│                      # Pins, Shortcuts Overlay, Modal Confirm, Status Bar
├── editor-app/        # Platform-agnostic App controller (Keymap, Buffers, 2×2 Panes,
│                      # Multi-Cursor, Vim mode, Session store, AutoPairs, FS Undo,
│                      # partial dirty-rect redraws, preferences, a11y snapshot)
├── editor-cli/        # Deterministic JSON CLI (`editor` binary): read, write, edit,
│                      # info, validate, batch, live IPC commands, version, schema
└── editor-macos/      # Minimal macOS adapter (ObjC/C via build.rs):
                       # CoreText single-cluster rasterizer, IOSurface zero-copy blit,
                       # FSEvents watcher, events, menus, drag-and-drop, NSUserDefaults,
                       # APFS clonefile, + Editor.app packager with pure-Rust icon
scripts/
└── install-macos.sh   # Minimal atomic macOS installer & uninstaller (zero scattered files)
```

---

## 3. Completed Milestones

### Chunk 1 — Archive + Plan + Foundation
- [x] Archive original C++ v1 implementation (subsequently removed; preserved in git history).
- [x] Establish 6-crate Rust workspace with zero external crates.
- [x] Commit archived original implementation, `PLAN.md`, and initial 6-crate foundation.

### Chunk 2 — Core Engine Hardening (`editor-core` + `editor-platform`)
- [x] **Lockfile + APFS Tail CoW Save**: advisory lockfile (`<path>.editor.lock`, `O_CREAT|O_EXCL`) and `clone_pwrite_tail` CoW save path.
- [x] **External Mutation Detection**: `Document::check_identity(fs)` returning `Unchanged | ReloadedClean | StaleDirty`, wired into `FocusChanged(true)` and `FSEvents`.
- [x] **Pathological Single-Line Protection**: `Document::read_line_window(line, byte_start, max_bytes)` + `line_byte_len(line)`.
- [x] **Multi-Line Bracket Matching + Tree Hygiene**: `find_matching_bracket` (up to 32 KB scan) + leaf-merging on sparse B-tree deletes.
- [x] **Expanded Stress & Fuzz Tests**: randomized PieceTree oracle, heavy delete compaction, lockfile tests, external mutation tests, 2 MB single-line window tests.

### Chunk 3 — Renderer & Custom UI (`editor-render` + `editor-app`)
- [x] **True Dirty-Rect Partial Redraws**: caret blink marks only the caret line rect dirty (`~5–10 µs` frames).
- [x] **Huge Single-Line Viewport Fast Path**: O(1) ASCII column calculation + windowed line reading.
- [x] **Custom-Drawn In-App Confirm Modal**: pure-Rust modal overlay ("Save Changes to …?") with keyboard shortcuts.
- [x] **Editor Ergonomics**: matching bracket highlight, indent guides, gutter line selection, multi-line Tab/Shift+Tab, Duplicate/Move/Delete Line, Toggle Line Comment, multi-cursor (`⌘D`, `⌥`+Click, vertical/horizontal arrows), 2×2 split panes with drag-and-drop, pinned working set (`⌘1`–`⌘9`), Vim mode + leader chords, and data-driven `Keymap` + `⌘K` Shortcuts overlay.

### Chunk 4 — Deterministic CLI (`editor-cli`)
- [x] Hand-rolled strict zero-dependency JSON parser (`json_parse.rs`) with unit tests.
- [x] Global `--root DIR` symlink-safe confinement (`E_OUTSIDE_ROOT`), `read` line & byte axes, `edit` match & positional forms with `--dry-run`, transactional `batch` JSON-lines with `--check` and rollback.
- [x] Per-user `0600` Unix-socket IPC (`status`, `open`, `split`, `pin`, `goto`, `select`, `insert`, `find`, `save`, `syntax`, `action`, `close`/`quit`) and enclosing-bundle launch resolution.

### Chunk 5 — macOS Adapter, Preferences, Drag-and-Drop & `Editor.app` Bundle (`editor-macos`)
- [x] `IOSurface` zero-copy dirty-rect presentation, `FSEvents` directory watcher, `keyDown:` modifier-safe + UTF-8/emoji dispatch (blocking AppKit PUA function keys from text entry), `NSUserDefaults` preferences, drag-and-drop, Finder Open With (`CFBundleDocumentTypes`), and standard macOS App Menu (`About editor`, Hide, Quit).
- [x] Pure-Rust Build Useful `.icns` generator + `bundle-app` packager embedding both `editor-gui` and `editor` inside `Editor.app/Contents/MacOS/`.

### Chunk 6 — Search Engine, Huge-Folder Streaming, Versioning & Distribution
- [x] **32-Byte SWAR & Linear-Time Regex**: rarest-pair SWAR scanning, branchless ASCII CI search, ReDoS-proof Thompson NFA / PikeVM regex engine, `$0`–`$9` capture replacement, and Find/Replace `W` & `.*` toggles.
- [x] **Two-Pane Quick Open (`⌘P`) & Search in Files (`⇧⌘F`)**: live syntax-highlighted preview, automatic resume, bounded 20 MB modal-lifetime `SearchSlab` (100% freed on close).
- [x] **Per-OS Ignore Rules, `.gitignore`, & Hybrid Streaming**: declarative per-OS ignore tables + hierarchical `.gitignore` in `editor-platform::ignore`; 8 ms sync head-start → cancellable background streaming via `MainLoopWaker` so huge directories like `$HOME` stay instant and 0% idle CPU.
- [x] **Sidebar Polish**: lazy tree, drag-resizable width, left/right dock, keyboard navigation, and right-click `Copy Path` / `Copy Relative Path`.
- [x] **Single-Source Versioning, Minimal macOS Installer & GitHub Releases Auto-Updater**: `editor-platform::{version, update}` (SemVer, pure-Rust SHA-256, CDN `release.json` manifest, host allowlist, App Translocation/dev guards, atomic bundle swap), `NSURLSession` native HTTPS check (once per 24h on launch + manual **Check for Updates…** / `editor update`), `scripts/install-macos.sh` (source build, `--from-release`, and standalone `curl | sh`), `.github/workflows/{ci,release}.yml`, `README.md`, and updated `ARCHITECTURE.md` / `PLAN.md`.

---

## 4. Next Planned Milestones

- [ ] **Subtle, Zero-Idle-CPU Git Awareness**: status bar branch badge (direct `.git/HEAD` read on open/focus/`FsChanged`), quiet gutter diff ticks, and optional `.gitignore` dimming in sidebar.
- [ ] **Cross-Platform Adapters & Native Seams**: Linux/Windows adapters behind `editor-platform`, plus native `NSTextInputClient` (IME) and `NSAccessibility` wired to the existing `ImeComposition` / `A11yNode` seams.
