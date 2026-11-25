# editor

An ultra-lightweight, zero-dependency, custom-drawn native text editor written in pure **Rust** (`std` only), following the **Build Useful** design system.

- **Zero external crates.** Every layer — piece-tree text buffer, 32-byte SWAR scanner, linear-time regex engine, syntax highlighter, glyph atlas, 2D software rasterizer, UI controls, `.gitignore` matcher, IPC, and JSON CLI — is built on Rust `std` alone.
- **Zero idle CPU, minimal memory.** Event-driven redraws with true dirty-rect tracking (`~5–10 µs` caret blink frames), lazy paged base files for multi-GB documents, lazy directory sidebar, and bounded modal-lifetime caches.
- **100% custom-drawn UI.** Light and Dark themes using a neutral ink/grey palette, 2px spacing ladder, hairline borders, and colour reserved strictly for state (`danger`, `success`, `info`).
- **One self-contained bundle.** On macOS, `Editor.app` holds both `editor-gui` and the deterministic `editor` CLI inside `Contents/MacOS/`, with a single symlink in `PATH`.

---

## Install (macOS)

Requires macOS 13+. Install the latest prebuilt universal (`arm64` + `x86_64`) GitHub Release with one command (verifies SHA-256 and code signature):

```sh
curl -fsSL https://raw.githubusercontent.com/builduseful/editor/main/scripts/install-macos.sh | sh
```

Or build from source (requires Xcode Command Line Tools and Rust `cargo`):

```sh
# System-wide install (/Applications/Editor.app + /usr/local/bin/editor)
./scripts/install-macos.sh

# User-only install (~/Applications/Editor.app + ~/.local/bin/editor, no sudo)
./scripts/install-macos.sh --user
```

Update anytime from the app menu (**editor → Check for Updates…**), Command Palette (`Check for updates`), or terminal:

```sh
editor update --check   # check for a newer GitHub Release
editor update           # download, verify SHA-256 + codesign, and swap Editor.app atomically
```

Uninstall cleanly (removes `Editor.app` and the `editor` symlink without touching unrelated files):

```sh
./scripts/install-macos.sh --uninstall          # or add --user
./scripts/install-macos.sh --uninstall --purge  # also clears macOS preferences
```

---

## Build, Test & Benchmark

```sh
# Run all workspace unit, property, render, app, and CLI tests
cargo test --workspace

# Build release GUI, CLI, and bundle packager
cargo build --release --workspace --bins

# Package and ad-hoc sign target/release/Editor.app (bundles both editor-gui and editor)
./target/release/bundle-app

# Run 60 MB / 1M line + 10 MB single-line + Retina frame + phys_footprint benchmark
cargo run --release -p editor-app --example perf_bench

# Render deterministic CoreText Retina PNG snapshots (Light and Dark)
./target/release/editor-gui --snapshot out-light.png --theme light --size 1120x740 --sidebar-root . Cargo.toml
./target/release/editor-gui --snapshot out-dark.png  --theme dark  --size 1120x740 --sidebar-root . Cargo.toml
```

---

## Keyboard Shortcuts

Press `⌘K` (or `⇧⌘/`) inside the app to search and run any shortcut.

### File & Navigation

| Action | Shortcut |
| --- | --- |
| Quick Open (two-pane with live preview) | `⌘P` |
| Search in Files (SWAR + regex + live preview) | `⇧⌘F` |
| Command Palette | `⇧⌘P` |
| Pinned & Open Files | `⌘E` |
| Pin / Unpin Current File (slots `1–9`) | `⌘M` |
| Jump to Pinned File `1–9` | `⌘1` – `⌘9` |
| Switch to Previous File | `⌃⇥` |
| Previous / Next Tab | `⇧⌘[` / `⇧⌘]` |
| Find / Find & Replace | `⌘F` / `⌥⌘F` |
| Find Next / Previous (while Find is open) | `⌘G` / `⇧⌘G` |
| Go to Line | `⌘G` |
| New Tab / New File | `⌘T` / `⌘N` |
| Open File / Open Folder | `⌘O` / `⇧⌘O` |
| Save / Save As | `⌘S` / `⇧⌘S` |
| Close Tab or Active Split Pane | `⌘W` |

### Editing & Multi-Cursor

| Action | Shortcut |
| --- | --- |
| Select Next Occurrence (multi-cursor) | `⌘D` |
| Add Caret at Click | `⌥` + Click |
| Toggle Line Comment | `⌘/` |
| Duplicate Line / Delete Line | `⇧⌘D` / `⇧⌘K` |
| Move Line Up / Down | `⌥↑` / `⌥↓` |
| Undo / Redo | `⌘Z` / `⇧⌘Z` |

### Panes & View

| Action | Shortcut |
| --- | --- |
| Toggle Sidebar / Focus Sidebar | `⌘B` / `⇧⌘E` |
| Move Sidebar Left / Right | `⌥⌘B` |
| Split Editor Right / Down (up to 2×2 grid) | `⌘\` / `⌥⌘\` |
| Focus Split Pane | `⌥⌘←` / `⌥⌘→` / `⌥⌘↑` / `⌥⌘↓` |
| Swap Split Panes / Close Active Pane | `⌃⌘\` / `⇧⌘\` |
| Toggle Fullscreen Pane Zoom | `⇧⌘↵` |
| Toggle Word Wrap | `⌥Z` |
| Toggle Vim Mode | `⇧⌘V` |
| Zoom In / Out / Reset | `⌘+` / `⌘-` / `⌘0` |

Vim mode includes standard Normal/Insert/Visual motions, text objects (`iw`, `i"`, `i(`, `i{`), split navigation (`⌃W`), and leader chords (`<Space>ff` Quick Open, `<Space>fg` / `<Space>/` Search in Files, `<Space>fb` / `<Space>e` Pinned & Open Files, `<Space>fe` Focus Sidebar, `<Space>a` Pin File, `<Space>1–9` Jump Pin).

---

## CLI (`editor`)

`editor` provides both everyday terminal launching and a deterministic JSON-first interface for scripts and automation:

```sh
# Open a folder or file in the GUI
editor .
editor src/main.rs

# Deterministic file inspection and edits (symlink-safe confinement with --root)
editor info Cargo.toml
editor read Cargo.toml --from 1 --to 20
editor edit file.txt --old "foo" --new "bar" --expect <REV>
editor batch --file ops.jsonl --check

# Live GUI inspection and control over per-user 0600 IPC socket
editor status
editor goto 42:1
editor split src/lib.rs
editor version
editor schema
```

---

## Versioning

Version numbers have one source of truth: `[workspace.package] version` in [Cargo.toml](Cargo.toml).

- `editor --version` / `editor version` returns `{ "version": "X.Y.Z", "build": N }`.
- `editor-gui --version` prints `editor-gui X.Y.Z (build N)`.
- `bundle-app` derives `CFBundleShortVersionString` and a strictly increasing `CFBundleVersion` (`MAJOR * 10000 + MINOR * 100 + PATCH`, or `EDITOR_BUILD_NUMBER`) directly from the workspace version into `Editor.app/Contents/Info.plist`.

---

## Documentation

- [ARCHITECTURE.md](ARCHITECTURE.md) — crate boundaries, data structures, rendering, and search engine design.
- [PLAN.md](PLAN.md) — completed milestones and architecture decisions.
- [SYNTAX_PLUGINS.md](SYNTAX_PLUGINS.md) — built-in zero-dependency syntax highlighting plugins and theme customisation.
- [LICENSE](LICENSE) — licensing terms.
