# Quill — Plan

> Working title **Quill** (placeholder — easy to rename).
> Status: planning. No code yet.

A terminal-native text editor built for **minimum memory, minimum CPU, maximum
speed**, with a **CLI-first** design so AI coding agents can drive it as reliably
as a human.

---

## 1. Goals

- Open and edit files from a few bytes to many gigabytes without loading the
  whole file into memory.
- Stay responsive with thousands of files in a project (fast walk, lazy indexing).
- Near-zero idle CPU and a small, predictable memory footprint.
- Native, single self-contained binary on macOS, Windows, and Linux.
- No third-party runtime libraries — only the standard library of the chosen
  language plus OS syscalls.
- A CLI that exposes **every** feature, emits machine-readable output, and is
  generated from the same command registry the TUI uses, so the two can never
  drift apart.

## 2. Non-goals

- No plugin/mod system (explicitly rejected for now — it adds complexity for
  little benefit at this stage).
- No network sync, accounts, telemetry, or cloud features.
- Not an IDE: no debugger, no language servers, no build system integration
  beyond "run a script".
- Not a WYSIWYG document editor (Markdown *rendering* is preview, not authoring).

## 3. Design principles

1. **One core, many front-ends.** All logic lives in a library (`core`). The TUI
   and the CLI are thin adapters over it. Every user-facing action is a
   first-class command with a stable name, JSON schema, and exit code.
2. **Lazy everything.** Never read, parse, index, or highlight data that is not
   needed right now.
3. **Edit in place conceptually, copy only what changes.** Use a piece table /
   rope so edits don't rewrite or reallocate the whole document.
4. **Event-driven, never poll.** Block on input/timers; do no work while idle.
5. **Safety without ceremony.** Memory-safe language, atomic file replacement,
   fuzzed invariants.
6. **Predictable cost.** Documented memory ceiling per open file; no unbounded
   caches.
7. **AI-first surface.** If a human can do it, an agent can do it via CLI with
   deterministic output.

## 4. Language / tech decision

**Recommendation: Rust, standard library only.**

Why:

- Memory safety without a GC → no pause times, predictable footprint.
- Excellent raw performance, comparable to C/C++.
- `std` covers files, mmap (via `std::fs` + platform calls), threads, paths,
  hashing, and process spawning — enough to avoid third-party crates.
- Cross-compiles cleanly to macOS / Windows / Linux; produces a single static-ish
  binary with no runtime dependency.
- Good tooling for fuzzing, property tests, and benchmarks.

Alternatives considered:

| Option | Pros | Cons |
| --- | --- | --- |
| C | Maximum control, tiny binaries | Manual memory safety, slow to build correctly, more platform code |
| C++ | Fast, mature | Heavy, easy to leak, larger surface |
| Zig | Great fit, tiny binaries | Immature ecosystem/tooling, smaller talent pool |
| Go | Easy cross-compile | GC pauses, larger binaries, less control over allocation |

**Decision needed:** confirm Rust. If the user prefers another language the plan
structure holds; only §4, §5, and §11 change materially.

Platform abstraction: a small `sys` module with `#[cfg(target_os = ...)]` blocks
for the handful of OS-specific pieces (terminal raw mode, window size, mmap,
file watching). Everything else is portable `std`.

## 5. Architecture

```
quill/
├── Cargo.toml
├── crates/
│   ├── core/         # buffer, piece table, line index, search, syntax, git,
│   │                 #   commands registry, config — no terminal/process UI
│   ├── tui/          # terminal front-end (raw mode, renderer, input)
│   └── cli/          # argv parser, JSON envelope, one-shot + serve mode
└── tests/            # integration, golden CLI, fuzz, benches
```

Dependency direction: `tui` and `cli` depend on `core`; `core` depends on nothing
but `std` (plus `sys`).

### 5.1 Document model

- **Piece table** over the original file plus an append-only edit log. An edit is
  metadata, not a data copy. Undo/redo is just moving a cursor in the log.
- **Line index**: lazy array of line-start offsets, built once per file, cached,
  invalidated on edit. Gives O(1) line↔offset conversion and O(log n) lookup.
- **Windows/paging**: reads go through `mmap` (POSIX) or
  `CreateFileMapping`/`MapViewOfFile` (Windows); reads outside the mapped region
  fall back to positioned `pread`. Memory is reclaimed by the OS under pressure.
- **Writes**: mutate a copy-on-write segment, then write the whole logical
  document to a temp file in the same directory, `fsync`, and atomic-rename over
  the original. Never corrupt a file on crash. Preserve permissions/ownership.
- **Large-file mode**: if a file exceeds a threshold (configurable, e.g. 64 MB),
  the buffer operates in streaming mode — the piece table references the mmap and
  edits are logged but only touched regions are materialized.

### 5.2 Performance budget (targets)

- Cold start to first prompt: **< 5 ms** for a trivial session; **< 50 ms** to
  open a 1 GB file read-only.
- Idle CPU: **0%** (blocked on `read`/`poll`, no timers unless a feature needs one).
- Memory: baseline **< 10 MB RSS**; a 1 GB read-only file adds only touched pages
  (mmap is demand-paged), not 1 GB.
- Edit latency: **< 1 ms** for edits within the visible viewport.
- Search: linear scan with SIMD-friendly `memchr`-style inner loop; parallelized
  across cores for large trees.

### 5.3 Cross-platform notes

- Terminal: POSIX `termios` + `ioctl(TIOCGWINSZ)`; Windows Console API
  (`SetConsoleMode`, `GetConsoleScreenBufferInfo`). No crossterm-style crate.
- Signals, file watching (`inotify` / FSEvents / `ReadDirectoryChangesW`) are
  isolated in `sys` and only needed for later phases.
- Paths: `std::path` + careful handling of non-UTF8 paths on Unix.

## 6. CLI specification (AI-first)

Single binary `quill`. Two shapes:

- `quill <path>` → launch the TUI (human).
- `quill <command> ...` → one-shot operation (agent). Deterministic output.

### 6.1 Command surface

Every command supports `--json` for machine output. Human output is the default
when stdout is a TTY; `--json` is the default when stdout is piped.

| Command | Purpose |
| --- | --- |
| `quill read <path> [--from N] [--to N] [--json]` | Read whole file or line range. Never loads more than needed. |
| `quill write <path> [--stdin \| --content TEXT]` | Create/overwrite atomically. |
| `quill insert <path> --at LINE[:COL] --text TEXT` | Insert text. |
| `quill replace <path> --from LINE[:COL] --to LINE[:COL] --text TEXT` | Replace a range. |
| `quill delete <path> --from LINE[:COL] --to LINE[:COL]` | Delete a range. |
| `quill search <path\|dir> --pattern RE [--literal] [--json]` | Literal or regex search, file or tree. |
| `quill goto <path> --line N [--col N]` | Report context around a location (for agents). |
| `quill stats <path>` | Size, lines, encoding, line endings, hash. |
| `quill apply <path> --patch FILE` | Apply a unified diff / edit transaction. |
| `quill serve` | Long-lived JSON-lines RPC server (phase 2). |

### 6.2 Output contract

- Success: JSON object on stdout, exit `0`.
- Recoverable problem (e.g. range out of bounds): JSON `{"error": {...}}` on
  stderr, exit `2`.
- Usage/IO error: exit `1` / `64`.
- Every response includes `{"ok":bool, "path":..., "rev":<monotonic>, "lines":N,
  "bytes":N, "sha256":...}` so an agent can detect concurrent modification.
- Mutations return a **before/after hash** and refuse to write if the file changed
  since the agent read it (optimistic concurrency via `rev`/hash).

### 6.3 Always up to date

`core` exposes a **command registry** (name, arg schema, JSON schema, handler).
Both the TUI (keybindings → command) and the CLI (argv → command) resolve through
this registry. Adding a feature adds one handler and it appears in both surfaces
automatically. A test asserts every registered command has a CLI binding and a
golden output.

### 6.4 Example agent loop

```sh
quill read src/main.rs --from 120 --to 160 --json
quill replace src/main.rs --from 138 --to 139 --text "let x = compute();"
quill search src --pattern "compute" --literal --json
quill stats src/main.rs --json
```

## 7. TUI design

- Minimal, monochrome, two panes at most: a gutter (line numbers) and text.
- Status line shows file, line:col, dirty state, encoding, and a hint of the CLI
  equivalent of what you just did.
- Vi-like modal editing to start (familiar, fast, small); configurable later.
- Rendering is **viewport-only**: compute the visible line range from the line
  index, format only those lines, emit a minimal diff of changed cells.
- No mouse dependency; keyboard-first. Terminal resize recomputes viewport.
- The TUI is a *client* of the same buffers/commands — it never reimplements
  editing logic.

## 8. Search (future, designed now)

- Literal and regex over a file or tree.
- Tree walk is parallel (work-stealing across cores), respects ignore rules,
  uses cached metadata for thousands of small files.
- Regex via a small built-in engine (backtracking-free where possible) — or a
  literal fast path with SIMD-style scanning.
- Fuzzy and vector search are explicitly deferred; the index format is designed
  to allow an added index later without breaking the CLI.

## 9. Syntax highlighting (future, efficient by design)

- **Lazy, per-visible-line** tokenization; nothing is highlighted off-screen.
- A tiny, data-driven rule format (regex/keyword tables) per language, compiled
  once and cached. No tree-sitter-scale runtime.
- Goal is speed and memory, not perfect fidelity. Extensible via the plugin
  question (currently **no plugins**) — languages ship built-in as data tables.

## 10. Markdown / Git / Scripts (future)

- **Markdown**: side-by-side or toggle preview; render from the same buffer using
  a small built-in renderer. Never blocks editing.
- **Git**: native status/diff/commit via direct `git` CLI subprocess (fast,
  correct, zero reimplementation). `quill git status|diff|commit ...`.
- **Scripts**: a lightweight command runner (`quill run <script>`) that executes
  project scripts / `dotnet run`, streams output. This is the sanctioned escape
  hatch instead of building build systems into the editor.

## 11. Testing & quality

- **Property tests**: edit/insert/delete/undo invariants; piece-table round-trips.
- **Fuzzing**: argv parser, line index, patch application.
- **Golden CLI tests**: exact JSON for every command, including error cases.
- **Benchmarks**: cold start, open 1 GB, edit viewport, search repo; tracked over
  time with a memory ceiling assertion.
- **CI**: GitHub Actions on macOS / Windows / Linux; fmt, clippy, test, bench.
- **Cross-platform smoke tests** run the CLI suite on all three OSes.

## 12. Build & distribution

- `cargo build --release` → single binary per target triple.
- No dynamic runtime deps beyond libc / OS.
- Distribution: GitHub Releases with per-OS binaries; optional `install.sh`.

## 13. Repo layout & first commits

```
.
├── PLAN.md            # this file
├── .gitignore
├── README.md          # short pitch + usage (added with first code)
├── Cargo.toml         # workspace (phase 0)
└── crates/            # core, tui, cli (phase 0)
```

## 14. Roadmap

- **Phase 0 — Skeleton**: workspace, `core`/`tui`/`cli` crates, command registry,
  `--json` envelope, CI, benchmarks harness. *No editing yet.*
- **Phase 1 — Buffer + CLI**: piece table, line index, `read`/`write`/`insert`/
  `replace`/`delete`/`stats`, atomic save, optimistic concurrency, tests.
- **Phase 2 — TUI MVP**: viewport renderer, modal editing, save/quit, status line,
  cross-platform terminal layer.
- **Phase 3 — Search**: file + tree search, CLI + TUI integration, ignore rules.
- **Phase 4 — Syntax highlighting**: lazy, data-driven, per-language tables.
- **Phase 5 — Markdown preview**.
- **Phase 6 — Git** (via git CLI).
- **Phase 7 — Script runner**.
- **Phase 8 — `quill serve`** long-lived JSON-lines RPC for agent sessions.

Explicitly out of scope: plugins/mods, network, telemetry.

## 15. Open questions

1. **Language**: confirm Rust (std-only) vs. C/Zig preference.
2. **Name**: keep "Quill" or pick another?
3. **Editing model**: modal (vi-like) vs. modeless default for the TUI.
4. **File size ceiling**: what "very large" means in practice (100 MB? 10 GB?),
   which sets the streaming threshold and test targets.
5. **`serve` mode**: needed in MVP, or fine to start with stateless one-shot CLI?
6. **Config format**: built-in key=value, or a small embedded format (no serde).