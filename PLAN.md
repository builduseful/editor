# editor — Plan

> Name: **editor** (chosen).
> Status: planning. No code yet.

A native desktop text editor built for **minimum memory, minimum CPU, maximum
speed**, with a **CLI-first** design so AI coding agents can drive it as reliably
as a human. The interactive interface is a **real native window** (not a terminal
UI), built on each OS's own frameworks with no third-party libraries.

---

## 1. Goals

- Open and edit files from a few bytes to many gigabytes without loading the
  whole file into memory.
- Stay responsive with thousands of files in a project (fast walk, lazy indexing).
- Near-zero idle CPU and a small, predictable memory footprint.
- A genuine **native app** on macOS, Windows, and Linux — real window, native
  fonts, menus, mouse, and accessibility.
- One shared **core** (Rust, std-only) for all platforms, with thin per-platform
  UI layers written against OS frameworks.
- A CLI that exposes **every** feature, emits machine-readable output, and is
  driven by the same command registry as the GUI, so the two can never drift.
- No third-party libraries. System OS frameworks are allowed (Cocoa/AppKit,
  Win32/DirectWrite, X11/Xft); they ship with the OS.

## 2. Non-goals

- No terminal UI (TUI). The interactive interface is a native window.
- No plugin/mod system (explicitly rejected for now — too much complexity).
- No network sync, accounts, telemetry, or cloud features.
- Not an IDE: no debugger, no language servers, no build-system integration
  beyond "run a script".
- Not a WYSIWYG document editor (Markdown *rendering* is preview, not authoring).

## 3. Design principles

1. **One core, native front-ends.** All logic lives in `core` (Rust). The GUI
   (per-OS) and the CLI are thin adapters. Every action is a first-class command
   with a stable name, JSON schema, and exit code.
2. **Lazy everything.** Never read, parse, index, lay out, or highlight data that
   is not needed right now.
3. **Copy only what changes.** Piece table: edits are metadata, not data copies.
4. **Event-driven, never poll.** Do no work while idle.
5. **Safety without ceremony.** Memory-safe core, atomic file replacement, fuzzed
   invariants.
6. **Predictable cost.** Documented memory ceiling per open file; bounded caches.
7. **AI-first surface.** If a human can do it, an agent can do it via CLI.
8. **Thin platform layers.** Platform code only creates a window, draws, and
   forwards events. It contains no editing logic.

## 4. Language / tech decision

**Core: Rust, standard library only. UI: each OS's native language.**

| Layer | Language | Frameworks |
| --- | --- | --- |
| `core` (buffer, edits, search, CLI) | Rust, `std` only | none |
| `platform/macos` | Objective-C++ | AppKit, CoreText |
| `platform/windows` | C++ | Win32, DirectWrite |
| `platform/linux` | C++ | X11, Xft (fontconfig/freetype) |

Why this split:

- **Rust core** gives memory safety, no GC, predictable performance, and easy
  cross-platform logic and tests. It builds as a static library with a small,
  stable C ABI.
- **Native UI layers** give a genuine OS-native look and feel, correct Unicode
  text shaping, native dialogs/menus/accessibility, and excellent performance.
- Writing AppKit/Win32/X11 directly from Rust is possible via FFI but is verbose
  and error-prone (selectors, blocks, COM, message loops). A thin native layer is
  **more reliable**, which is the priority.
- A vendored cross-platform UI library was rejected: it is a third-party library.

Rejected alternatives: C (manual memory safety), C++ for everything (heavier,
easy to leak), Zig (immature), Go (GC pauses), Rust-only FFI to native APIs
(error-prone), vendored UI toolkit (third-party).

## 5. Architecture

```
editor/
├── Cargo.toml                 # workspace
├── crates/
│   ├── core/                  # Rust: buffer, piece table, line index, search,
│   │                          #   command registry, config, C ABI (staticlib)
│   └── cli/                   # Rust: argv -> command registry, JSON envelope
├── platform/
│   ├── shared/                # editor.h C ABI + protocol notes
│   ├── macos/                 # ObjC++ / AppKit + CoreText, links libcore.a
│   ├── windows/               # C++ / Win32 + DirectWrite, links core.lib
│   └── linux/                 # C++ / X11 + Xft, links libcore.a
└── tests/                     # core tests, CLI golden, fuzz, benches
```

Dependency direction: `cli` and each `platform/*` depend on `core`; `core`
depends on nothing but `std` (plus OS syscalls for mmap).

### 5.1 The C ABI (core ↔ native UI)

A deliberately small, stable boundary. The native UI never sees Rust types.

```c
// platform/shared/editor.h
typedef struct Editor Editor;

Editor* editor_open(const char* path, char* err, size_t errcap);
void    editor_close(Editor*);

int32_t editor_line_count(const Editor*);
// Copy line `index` (without newline) into buf. Returns bytes needed
// (so callers can size a buffer). Returns 0 for empty lines.
int32_t editor_line(const Editor*, int32_t index, char* buf, size_t cap);

void    editor_insert(Editor*, int32_t line, int32_t col, const char* text);
void    editor_delete(Editor*, int32_t fl, int32_t fc, int32_t tl, int32_t tc);
void    editor_save(Editor*);
int32_t editor_is_dirty(const Editor*);

typedef void (*editor_change_fn)(void* ud, int32_t first_line, int32_t last_line);
void    editor_set_change_cb(Editor*, editor_change_fn, void* ud);
```

The callback tells the UI which lines to redraw. All line access goes through
`editor_line`, so the UI only ever touches the viewport.

### 5.2 Document model

- **Piece table** over the original file plus an append-only edit log. An edit is
  metadata, not a data copy. Undo/redo is moving a cursor in the log.
- **Line index**: sparse, block-based index of line-start offsets, built lazily
  and cached, invalidated on edit. Blocks (e.g. 1 Ki lines) keep memory bounded on
  huge files; small files behave like a flat O(1) index.
- **Memory-mapped reads**: `mmap` (POSIX) or `CreateFileMapping`/`MapViewOfFile`
  (Windows). The OS demand-pages only touched regions and reclaims under pressure.
  The same code path handles a 1 KB file and a 100 GB file — no full read, no size
  threshold.
- **Writes**: copy-on-write segment, then write the logical document to a temp file
  in the same directory, `fsync`, and atomic-rename over the original. Never
  corrupt a file on crash. Preserve permissions/ownership.
- **Large files**: mmap *is* the streaming approach. No separate mode. Fallback to
  positioned `pread` if mmap is unavailable.

### 5.3 Performance budget (targets)

- Cold start to window: **< 100 ms**; open a 1 GB file read-only: **< 100 ms**.
- Idle CPU: **0%** (event loop blocks; no timers unless a feature needs one).
- Memory: baseline **< 30 MB RSS** including the UI; a 1 GB read-only file adds
  only touched pages (mmap), not 1 GB.
- Edit latency: **< 1 ms** for edits within the viewport; redraw only visible
  lines.
- Search: SIMD-friendly literal scan; parallel tree walk for large trees.

### 5.4 Platform layer notes

- **macOS**: AppKit app (Xcode project), custom `NSView` for text (not
  `NSTextView`, which loads the whole string), CoreText for line layout, native
  open/save panels and menu bar.
- **Windows**: Win32 window + common controls, DirectWrite for layout, native
  dialogs.
- **Linux**: X11 window + Xft for text; Wayland later (XWayland first if needed).
- Each platform implements the same small set: create window, draw visible lines,
  deliver keyboard/mouse/IME, draw cursor/selection, clipboard, dialogs.
- Text is laid out **per visible line only**, with a bounded layout cache. This is
  the key to handling huge files in a GUI.

## 6. CLI specification (AI-first)

Binary: `editor`. The GUI is a separate app (`Editor.app` / `editor-gui`) that
links the same core. Both resolve through the same command registry.

```
editor <command> ...   # one-shot operation (agent or script)
```

### 6.1 Command surface

Every command supports `--json`. Human output is default when stdout is a TTY;
`--json` is default when piped.

| Command | Purpose |
| --- | --- |
| `editor read <path> [--from N] [--to N] [--json]` | Read whole file or line range. |
| `editor write <path> [--stdin \| --content TEXT]` | Create/overwrite atomically. |
| `editor insert <path> --at LINE[:COL] --text TEXT` | Insert text. |
| `editor replace <path> --from LINE[:COL] --to LINE[:COL] --text TEXT` | Replace a range. |
| `editor delete <path> --from LINE[:COL] --to LINE[:COL]` | Delete a range. |
| `editor search <path\|dir> --pattern RE [--literal] [--json]` | Literal/regex search. |
| `editor goto <path> --line N [--col N]` | Report context around a location. |
| `editor stats <path>` | Size, lines, encoding, line endings, hash. |
| `editor apply <path> --patch FILE` | Apply a unified diff / edit transaction. |
| `editor serve` | Long-lived JSON-lines RPC server (deferred — §6.5). |

### 6.2 Output contract

- Success: JSON on stdout, exit `0`.
- Recoverable problem: JSON `{"error": {...}}` on stderr, exit `2`.
- Usage/IO error: exit `1` / `64`.
- Every response includes `{"ok":bool, "path":..., "rev":<monotonic>, "lines":N,
  "bytes":N, "sha256":...}`.
- Mutations return before/after hashes and refuse to write if the file changed
  since it was read (optimistic concurrency).

### 6.3 Always up to date

`core` exposes a **command registry** (name, arg schema, JSON schema, handler).
Both the GUI (menu/action → command) and the CLI (argv → command) resolve through
it. Adding a feature adds one handler and it appears in both. A test asserts every
registered command has a CLI binding and a golden output.

### 6.4 Example agent loop

```sh
editor read src/main.rs --from 120 --to 160 --json
editor replace src/main.rs --from 138 --to 139 --text "let x = compute();"
editor search src --pattern "compute" --literal --json
editor stats src/main.rs --json
```

### 6.5 One-shot vs. long-lived server

**Decision: stateless one-shot commands in the MVP; `editor serve` deferred.**

- **One-shot**: each command is a short-lived process that opens the file, applies
  the change, atomically saves, and exits. No background state to go stale; mmap
  makes open cheap, and the block index only builds touched lines.
- **Server**: keeps buffers/indexes/undo alive between calls, but adds a protocol,
  lifecycle, stale-state handling, and process management.

One-shot is enough because the hash/`rev` check already detects concurrent
modification. `serve` is additive — same handlers in-process — so it can be added
later if profiling shows repeated open cost dominating.

## 7. Native GUI design

- **Real native window** per OS: menu bar, native dialogs, real fonts, mouse,
  accessibility.
- **Custom text view** (not the OS's heavyweight text control): the view asks
  `core` for the visible line range, lays out only those lines with CoreText /
  DirectWrite / Xft, and draws them. A bounded cache holds nearby lines.
- **Viewport-only rendering**: on scroll, only newly visible lines are laid out;
  on edit, only the changed range redraws.
- **Editing model**: modeless (standard GUI behavior) to start. A vi-like mode can
  be added later as an option.
- **Status bar**: file, line:col, dirty state, encoding.
- **Monospace by default**; variable-width fonts are a later option.
- The GUI is a **client of core** — it never reimplements editing. It sends
  commands and redraws from change callbacks.
- **Increment order**: read-only view first, then cursor + editing, then polish.

## 8. Search (future, designed now)

- Literal and regex over a file or tree.
- Tree walk parallel, respects ignore rules, cached metadata for thousands of
  small files.
- Literal fast path with a SIMD-friendly inner loop; regex via a small built-in
  engine (no backtracking where avoidable).
- Fuzzy/vector search deferred; index format designed to allow it later.

## 9. Syntax highlighting (future, efficient by design)

- **Lazy, per-visible-line** tokenization; nothing off-screen is highlighted.
- Small, data-driven rule format (keywords/regex tables) per language, compiled
  once and cached.
- Speed and memory over perfect fidelity. Languages ship as built-in data tables
  (no plugins).

## 10. Markdown / Git / Scripts (future)

- **Markdown**: side-by-side or toggle preview; rendered from the same buffer by a
  small built-in renderer. Never blocks editing.
- **Git**: via the `git` CLI subprocess (fast, correct, no reimplementation).
- **Scripts**: a lightweight runner (`editor run <script>`) for project scripts /
  `dotnet run`, streaming output. The sanctioned escape hatch instead of building
  build systems into the editor.

## 11. Testing & quality

- **Property tests**: edit/insert/delete/undo invariants; piece-table round-trips.
- **Fuzzing**: argv parser, line index, patch application.
- **C ABI contract tests**: the native layers and Rust core agree on the boundary.
- **Golden CLI tests**: exact JSON for every command, including errors.
- **Benchmarks**: cold start, open 1 GB, edit viewport, search repo; memory-ceiling
  assertions.
- **CI**: macOS / Windows / Linux; fmt, clippy, test, bench, plus platform build
  smoke tests.

## 12. Build & distribution

- Rust core: `cargo build --release` → `libcore.a` / `core.lib` + `editor` CLI.
- macOS: Xcode project (or Makefile) compiles the ObjC++ app, links the core,
  bundles `Editor.app`. Native menus/lifecycle via AppKit.
- Windows: MSVC project links `core.lib`, builds `editor-gui.exe`.
- Linux: Makefile/CMake links `libcore.a`, builds `editor-gui`.
- No third-party libraries; only OS frameworks and the toolchain.
- Distribution: GitHub Releases with per-OS artifacts.

## 13. Repo layout

```
.
├── PLAN.md
├── README.md          # added with first code
├── Cargo.toml
├── crates/{core,cli}/
├── platform/{shared,macos,windows,linux}/
└── tests/
```

## 14. Roadmap (solid, demoable increments)

- **M0 — Core + CLI (no UI).** Piece table, block line index, mmap reads, atomic
  save, command registry, `read`/`write`/`insert`/`replace`/`delete`/`stats`/
  `search`/`apply`, JSON contract, tests, benchmarks, CI.
- **M1 — macOS read-only window.** C ABI, core static lib, ObjC++ AppKit app:
  menu bar, open dialog, custom text view, CoreText layout, scrolling. Opens and
  displays a file. *First native app.*
- **M2 — macOS editing.** Cursor, keyboard input, insert/delete, arrows, save,
  dirty indicator. *First usable native editor.*
- **M3 — macOS polish.** Selection, clipboard, find, go-to-line, status bar, line
  numbers, window title. *Daily-driver quality.*
- **M4 — Windows port.** Win32 + DirectWrite implementing the same UI contract.
- **M5 — Linux port.** X11 + Xft.
- **M6 — Search.** File + tree search, GUI + CLI, ignore rules.
- **M7 — Syntax highlighting.** Lazy, data-driven, per-language tables.
- **M8 — Markdown preview.**
- **M9 — Git** (via git CLI).
- **M10 — Script runner.**
- **M11 — `editor serve`** (optional, only if needed).

Explicitly out of scope: plugins/mods, network, telemetry, TUI.

## 15. Resolved decisions

1. **Language**: Rust `std`-only core + native OS UI layers.
2. **Name**: `editor`.
3. **Interface**: native GUI via OS frameworks; macOS first, then Windows/Linux.
4. **File size**: no threshold — mmap + piece table + block line index.
5. **`serve` mode**: deferred; stateless one-shot CLI in the MVP.
6. **Editing model**: modeless (GUI standard); vi-like mode optional later.

Still open:

7. **Config format**: built-in key=value, or a small embedded format (no serde).
8. **Regex engine**: hand-written vs. a small vendored engine (affects "no
   third-party").
9. **Linux text stack**: Xft/fontconfig now; Wayland native later?