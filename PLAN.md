# editor — Plan (v4)

> Name: **editor** (chosen).
> Status: planning. v4 — implementation baseline after five independent audits.
> Audits covered: consistency & clarity · technical accuracy · architecture &
> scope · requirements & product · delivery & testability.

A native desktop text editor for **minimum memory, minimum CPU, maximum speed**,
with a **CLI-first** surface so AI agents can drive it as reliably as a human.
One shared C++ core, one custom-drawn viewcore, thin per-OS chrome.

---

## 0. What changed in v4

Five independent audits found blockers. v4 fixes them:

- **Custom view only** (D2): the v3 hybrid native/custom view was two sources of
  truth (`NSTextView`/RichEdit own selection, undo, IME, a11y) and had no Linux
  path. v4 uses one custom viewcore on all platforms.
- **`Editor` interface repaired**: `Edit` now carries inserted text; `read_lines`
  has defined size/cap/terminator semantics; `save(expected_rev)`, `rev()`,
  `snapshot()`, `cancel`, and change registration are specified.
- **Save path corrected**: the v3 "copy the unchanged prefix" fallback could
  truncate the file. v4 copies all unchanged ranges or clones the full file,
  states the **O(tail)** cost for length-changing edits, and defines the post-save
  transition.
- **M0 split and reordered**: Pre-M0.0 interface freeze → M0a read/write/stats →
  M0b edit/undo/save → M0c batch/security → M0.5 render oracle. M0 is no longer a
  product-sized bundle.
- **CLI contract tightened**: one error model, structured `rev` with `ctime_ns`,
  global `--root`, mandatory `--expect`, `@file`/stdin for edits, batch
  validation/rollback semantics, no `--json` (always JSON).
- **Budgets corrected**: per-OS metrics, aggregate caps (not a misleading 12 MB
  headline), CPU percentages fixed (3 ms/frame = 18% of a core, not 2%), sparse
  line-index memory budgeted.
- **Scope fenced**: MVP = macOS M0–M3.5; Windows/Linux/search/syntax post-MVP;
  Markdown/git/scripts/serve post-1.0.
- **Missing layers added**: workspace, `DocumentSession`, viewcore `TextView`,
  textbackend/`Canvas`, platform FS/watcher, decorations, feature checklist, risk
  register, decision log, TOC/glossary.

---

## Contents

0. [What changed in v4](#0-what-changed-in-v4)
1. [Decision log](#1-decision-log)
2. [Goals and non-goals](#2-goals-and-non-goals)
3. [Glossary](#3-glossary)
4. [Architecture](#4-architecture)
5. [Document model contract](#5-document-model-contract)
6. [CLI and agent contract](#6-cli-and-agent-contract)
7. [Resource budgets and measurement](#7-resource-budgets-and-measurement)
8. [Rendering](#8-rendering)
9. [Workspace and search](#9-workspace-and-search)
10. [Future features and seams](#10-future-features-and-seams)
11. [Testing and CI](#11-testing-and-ci)
12. [Build and distribution](#12-build-and-distribution)
13. [Roadmap, effort, and cut list](#13-roadmap-effort-and-cut-list)
14. [Feature checklist](#14-feature-checklist)
15. [Open decisions](#15-open-decisions)
16. [Risk register](#16-risk-register)
17. [What I would build first](#17-what-i-would-build-first)
Appendix: [Language decision](#appendix-language-decision)

---

## 1. Decision log

Every load-bearing decision has an ID, a status, and a revisit trigger. The
detailed rationale for the language choice is in the [appendix](#appendix-language-decision).

| ID | Decision | Status | Revisit trigger |
| --- | --- | --- | --- |
| D1 | Core language: **C++ all-in** (core + viewcore + platform + CLI); ObjC++ on macOS. | Accepted | M0.5 kill gate: C++ bug density high despite sanitizers/fuzzing, or Rust velocity/seam clearly better. |
| D2 | UI: **custom-drawn view only** for v1. No native-control hybrid. | Accepted | Post-MVP, only if custom view misses IME/a11y/perf and a native-control spike proves better. |
| D3 | **Bytes are the currency.** Line = byte range; `col` = byte offset. | Accepted | Never. |
| D4 | CLI: **one-shot + `batch`**; `serve` deferred. | Accepted | Profiling shows repeated open/index cost dominates. |
| D5 | Save: CoW clone + `pwrite` changed spans; **honest O(tail) for length-changing edits**. | Accepted | A journal/extent-map save proves simpler or necessary. |
| D6 | Line index: **sparse, every 64 lines**, 8-byte offsets, in-chunk scan. | Accepted | Memory/perf gate at 25M lines fails. |
| D7 | CLI JSON + SHA-256: **hand-rolled, zero-dep, fuzzed**. | Accepted | Bug rate high; then allow a vetted header-only JSON/SHA-256 in `cli` only. |
| D8 | Regex: **RE2-like documented subset**, no backreferences/lookaround, step budget. | Accepted for M6 | If agents need full PCRE. |
| D9 | Encoding: **UTF-8 only for v1**; detect UTF-16/BOM and fail clearly; transcoding post-MVP. | Accepted | UTF-16 becomes a common blocker. |
| D10 | Linux: **X11/XWayland first**; native Wayland/text-input post-MVP. | Accepted | Wayland becomes the only viable path. |
| D11 | Config: small **key=value**, data-only, versioned. | Proposed | See §15. |
| D12 | Accessibility: **keyboard-complete + basic screen reader**; full parity post-MVP. | Accepted | Post-MVP. |
| D13 | No GUI project tree in v1; **CLI `tree`** + OS file dialog. | Accepted | Project workflow demands a tree. |
| D14 | **MVP = macOS M0–M3.5.** Windows/Linux follow. | Accepted | Schedule or market changes. |
| D15 | No plugins/mods. Scripts are **post-1.0, trust-gated**, not a mod API. | Accepted | Never for v1. |
| D16 | CLI: **always JSON on stdout**; no TTY sniffing; no `--json`. | Accepted | Never. |
| D17 | **Global `--root`** on all path-taking commands; `--allow-outside` escape. | Accepted | Never. |
| D18 | Error model: `Status` in core, `E_*` codes in CLI; exit `0/2/3/4`. | Accepted | Never. |
| D19 | `rev`: **stat-based by default**; SHA-256 under a size threshold or with `--hash`; includes `ctime_ns`. | Accepted | Cross-platform collision issues. |
| D20 | `batch`: **validation-stage atomic**; stage + commit; best-effort rollback; **not crash-atomic**. | Accepted | Crash-atomicity becomes a requirement. |
| D21 | Custom-view IME/a11y built in viewcore from M1; Linux XIM/AT-SPI. | Accepted | M3. |
| D22 | No bundled font for v1; OS fonts. | Proposed | See §15. |
| D23 | Windows CRT: static `/MT`, one CMake graph. | Proposed | Link/CRT issues. |
| D24 | `--expect` mandatory on existing files for mutations; `--force` bypasses. | Accepted | Never. |

---

## 2. Goals and non-goals

### Goals

- Open and edit files from a few bytes to many gigabytes without loading the whole
  file into memory.
- Stay responsive with thousands of files in a project.
- Near-zero idle CPU; small, predictable, **measured** memory footprint.
- Genuine native app on macOS, Windows, and Linux: real window, native chrome,
  native fonts, native dialogs, keyboard/mouse.
- One shared core (C++) for all platforms; one shared viewcore; thin per-OS chrome.
- A CLI that exposes every feature, emits machine-readable output, and is driven by
  the same domain ops as the GUI.
- No vendored/bundled third-party code. OS system libraries and their headers are
  allowed.

### Non-goals (v1)

- No terminal UI.
- No plugins/mods. No network sync, accounts, telemetry.
- No native-control hybrid (custom view only for v1).
- No multi-cursor, command palette, split panes, or GUI project tree.
- No Markdown rendered editing (preview only, post-1.0).
- No git UI, script runner, or `serve` (post-1.0).
- Not an IDE: no debugger, no language servers, no build-system integration beyond
  "run a script" (post-1.0).
- No inverted search index in v1 (mmap + metadata cache + bounded content cache;
  revisit if profiling demands it).

---

## 3. Glossary

- **Byte offset** — 0-based offset into the file. All core addressing is bytes.
- **Line** — a byte range `[start, end)` excluding its terminator. `line_count`
  = terminators + (1 if the last line is non-empty). Empty file = 0 lines.
- **Col** — 0-based UTF-8 byte offset within a line (tabs = 1 byte). CLI exposes
  1-based `LINE:COL`; the core interface is 0-based.
- **Piece table** — immutable base pieces + an add buffer; edits are descriptors.
- **Chunk** — a fixed-size run of lines in the line index (default 64 lines).
- **Rev** — an opaque content/stat identity used for optimistic concurrency.
- **Root** — the directory a command may write within; `--allow-outside` escapes.
- **Batch** — a multi-op JSON-lines program with validation-stage atomicity.
- **DocumentSession** — open documents + undo + change ranges; shared by CLI/GUI.
- **Snapshot** — an immutable read-only view of a document for parallel readers.
- **Viewcore** — platform-independent view logic: wrap, cache, hit-test, selection,
  scroll, caret, IME state, a11y tree, decorations.
- **Textbackend** — per-OS glyph shaping/drawing: CoreText, DirectWrite, Xft+HarfBuzz.
- **Decoration** — a styled byte range over the viewport (search, syntax, git).
- **Footprint** — private memory: `phys_footprint` (macOS), private working set
  (Windows), PSS (Linux). Not directly comparable across OSes.

---

## 4. Architecture

```
                         ┌──────────────────────────────┐
   CLI (C++) ──────────► │                              │
                         │  core (C++)                  │
   platform/macos ─────► │  document · edits · undo     │
   platform/windows ───► │  sparse line index · save    │
   platform/linux ─────► │  workspace · search · domain │
                         └───────────────┬──────────────┘
                                         │ core/include/editor.hpp
                         ┌───────────────▼──────────────┐
                         │  viewcore (C++)              │
                         │  wrap · cache · hit-test     │
                         │  selection · scroll · caret  │
                         │  IME state · a11y · decorate │
                         └───────────────┬──────────────┘
                                         │ textbackend + Canvas
             ┌─────────────────────────┼─────────────────────────┐
        CoreText (macOS)          DirectWrite (Windows)     Xft+HarfBuzz (Linux)
```

### 4.1 Layers and dependency rules

- **`core` (C++)** — document model, piece table, sparse line index, undo, save,
  domain ops, `workspace`, `DocumentSession`. No UI, no exit codes. OS access is
  through injected `FileSystem`/`FileWatcher` interfaces; no `#ifdef` in core.
- **`viewcore` (C++)** — all platform-independent view logic: wrapping, line
  cache, hit-testing, selection, scroll math, caret, IME composition state,
  accessibility tree, decorations. One implementation, one conformance suite.
- **`textbackend` (per OS)** — the only place that shapes and draws glyphs.
  `measure`, `shape` into runs, `draw` into a `Canvas`.
- **`platform/<os>`** — window, menus, dialogs, keyboard/mouse translation,
  clipboard, IME plumbing, a11y node emission, `FileSystem`, `FileWatcher`,
  `Canvas`. No editing logic.
- **`workspace`** — directory tree cache, ignore rules, content mmap LRU,
  bounded parallel scan.
- **`DocumentSession`** — open documents, undo stacks, change ranges. Both CLI and
  GUI consume it; `serve` (post-1.0) is a transport over it, not a GUI feature.
- **`cli` (C++)** — argv → domain ops → persist → JSON envelope.

Dependency direction: `cli`, `platform/*`, and `viewcore` depend on `core`;
`core` depends only on `std` + injected OS interfaces. The frozen interface lives
at **`core/include/editor.hpp`**, not under `platform/`.

### 4.2 Core interface (`core/include/editor.hpp`)

This is the highest-leverage artifact. It is cheap to write now and expensive to
retrofit at M2. Freeze a **draft** before M0; freeze formally after the C++
conformance test passes at M0.5.

```cpp
// core/include/editor.hpp — v1 (draft)
#pragma once
#include <atomic>
#include <cstdint>
#include <span>
#include <string_view>

enum class Status : uint32_t {
  Ok = 0, Io, NotFound, Range, Conflict, Stale, Oom, Encoding, InvalidUtf8,
  Binary, OutsideRoot, Parse, RegexUnsupported, TooLarge, Locked,
  Schema, Unsupported, Usage, Internal, TooSmall, SavedButNotDurable
};

enum class Eol : uint32_t { Lf, CrLf, Cr, Mixed };
enum class Encoding : uint32_t { Utf8, Utf16Le, Utf16Be, Other };
enum class EditKind : uint32_t { Insert, Delete, Replace };
enum class ChangeKind : uint32_t { Insert, Delete, Replace, Reload, Save };

struct Rev {
  enum class Kind : uint32_t { Stat, Sha256 } kind = Kind::Stat;
  uint64_t dev = 0, ino = 0, mtime_ns = 0, ctime_ns = 0, size = 0;
  uint8_t sha256[32] = {};
};

struct Info {
  std::string_view path;                 // borrowed; valid until next call
  uint64_t dev = 0, ino = 0;
  int64_t bytes = 0, line_count = 0, mtime_ns = 0, ctime_ns = 0;
  Eol eol = Eol::Lf;
  Encoding encoding = Encoding::Utf8;
  bool bom = false, final_newline = true, dirty = false;
  Rev rev;
  uint32_t flags = 0;
};

struct Change {
  int64_t first = 0, last = 0, new_count = 0;  // [first,last); new line count
  ChangeKind kind = ChangeKind::Insert;
  uint32_t flags = 0;
};

struct Edit {
  int64_t line = 0, col = 0;             // 0-based; col = byte offset
  int64_t removed_len = 0;               // 0 for insert
  std::string_view text;                 // inserted/replacement bytes; borrowed
  EditKind kind = EditKind::Insert;
};

struct CancelToken { std::atomic<bool> cancelled{false}; };

class Snapshot {
 public:
  virtual ~Snapshot() = default;
  virtual int64_t line_count() const = 0;
  virtual Status read_lines(int64_t first, int64_t last, char* buf, size_t cap,
                            size_t* used, int64_t* out_n) const = 0;
};

class Editor {
 public:
  static constexpr uint32_t kInterfaceVersion = 1;
  virtual ~Editor() = default;

  virtual Status open(std::string_view path_utf8) = 0;
  virtual Status info(Info* out) const = 0;
  virtual Rev rev() const = 0;

  // first inclusive, last exclusive. buf=nullptr -> size query.
  // Returns line bytes INCLUDING the terminator. On TooSmall, `used` = needed.
  virtual Status read_lines(int64_t first, int64_t last, char* buf, size_t cap,
                            size_t* used, int64_t* out_n) const = 0;
  virtual Status read_range(int64_t off, size_t len, char* buf, size_t cap,
                            size_t* used) const = 0;

  // Edits are relative to the ORIGINAL buffer, sorted, non-overlapping.
  // All-or-nothing. Inserted newline uses the file's dominant EOL.
  virtual Status apply(std::span<const Edit> edits,
                       const CancelToken* cancel = nullptr) = 0;

  virtual Status undo() = 0;
  virtual Status redo() = 0;
  virtual Status save(std::string_view expected_rev = {}) = 0;
  virtual Status save_as(std::string_view path_utf8,
                         std::string_view expected_rev = {}) = 0;
  virtual Status reload() = 0;

  using ChangeFn = void (*)(Editor*, const Change&, void*);
  virtual uint64_t on_change(ChangeFn, void* ud) = 0;   // registration id
  virtual void off_change(uint64_t id) = 0;
  virtual void drain() = 0;            // callbacks fire ONLY here
  virtual std::string_view errmsg() const = 0;  // thread-local, borrowed

  virtual const Snapshot* snapshot() const = 0; // immutable, for workers
  virtual void release_snapshot(const Snapshot*) = 0;
};
```

Non-negotiable rules (written into the header as comments):

- **Every fallible call returns `Status`.** No `void` mutation/save entry points.
- **`(ptr, len)` / `string_view` / `span` everywhere.** No NUL-terminated input;
  embedded NUL is data.
- **`int64_t` for lines/offsets.** `int32_t` cannot express a >2 GB line or an
  error; `0` is a valid line/length, never an error.
- **Line/col are 0-based in the core; CLI is 1-based.** Pin it, conformance-test it.
- **Core never calls the UI directly.** It appends changes; the UI calls `drain()`
  on its own thread. Callbacks must not call back into the core.
- **Exceptions must not escape the interface.** Catch at every boundary and return
  `Status::Internal`; a half-applied edit must leave the editor consistent.
- **One `Editor` per thread** unless an explicit lock is added. Parallel readers
  use `snapshot()`.
- **`errmsg` is thread-local and borrowed**, valid until the next core call on that
  thread. No retention.
- **C++20, pinned toolchain.** Statically link core into GUI/CLI (one build graph)
  or provide an explicit `extern "C"` ABI with struct sizes + version.
- **Hardening in debug/CI:** `_GLIBCXX_ASSERTIONS` / `_LIBCPP_HARDENING_MODE`,
  MSVC `/sdl` + `_ITERATOR_DEBUG_LEVEL=2`, ASan/UBSan, `-Wall -Wextra -Wconversion`.

### 4.3 Viewcore interface (`viewcore/view.hpp`)

Custom view only. The view owns selection, caret, scroll, IME composition, and
a11y; the core owns bytes and edits.

```cpp
struct Rect { double x, y, w, h; };
struct Range { int64_t begin, end; };      // byte offsets
struct Decoration { Range range; uint32_t style; };

class Canvas {                              // platform draw target
 public:
  virtual ~Canvas() = default;
  virtual void draw_run(const ShapedRun&, double x, double y) = 0;
  virtual void fill_rect(const Rect&, uint32_t color) = 0;
};

class TextView {
 public:
  virtual ~TextView() = default;
  virtual void set_document(Editor*) = 0;
  virtual void set_snapshot(const Snapshot*) = 0; // parallel read path
  virtual void resize(int width, int height, double scale) = 0;
  virtual void set_wrap(bool) = 0;
  virtual void set_font(const Font&) = 0;
  virtual void set_decorations(std::span<const Decoration>) = 0;
  virtual Range selection() const = 0;
  virtual void set_selection(Range) = 0;
  virtual int64_t first_visible_line() const = 0;
  virtual void scroll_to(int64_t line) = 0;
  virtual void draw(Canvas&, const Rect& viewport) = 0;
  virtual bool key(const KeyEvent&) = 0;
  virtual bool mouse(const MouseEvent&) = 0;
  virtual bool ime(const ImeEvent&) = 0;
};
```

Conformance tests run against the custom view on every platform; the native-control
path is a post-MVP experiment, not a second v1 implementation. The `TextView` and
`textbackend` sketches omit supporting types (`Font`, `ShapedRun`, `KeyEvent`,
`MouseEvent`, `ImeEvent`); they are defined in `viewcore/types.hpp`.

### 4.4 Textbackend and Canvas

```cpp
class TextBackend {
 public:
  virtual ~TextBackend() = default;
  virtual double measure(std::string_view text, const Font&) = 0;
  virtual void shape(std::string_view text, const Font&, ShapedRun* out) = 0;
  virtual void draw(Canvas&, const ShapedRun&, double x, double y) = 0;
};
```

CoreText (macOS), DirectWrite (Windows), Xft+HarfBuzz (Linux). `Canvas` is owned
by the platform; `textbackend` borrows it.

### 4.5 Platform FS and watcher

Core never calls `mmap`/`clonefile`/inotify directly. It uses injected interfaces:

```cpp
class FileSystem {
 public:
  virtual ~FileSystem() = default;
  virtual Status map_readonly(std::string_view path, Mapping* out) = 0;
  virtual Status map_window(std::string_view path, uint64_t off, size_t len,
                            Mapping* out) = 0;
  virtual Status pread(std::string_view path, uint64_t off, char* buf,
                       size_t cap, size_t* used) = 0;
  virtual Status clone_file(std::string_view src, std::string_view dst) = 0;
  virtual Status copy_range(std::string_view src, std::string_view dst,
                            uint64_t off, size_t len) = 0;
  virtual Status atomic_replace(std::string_view tmp, std::string_view dst) = 0;
  virtual Status fsync_dir(std::string_view path) = 0;
  virtual Status identity(std::string_view path, uint64_t* dev, uint64_t* ino,
                          int64_t* size, int64_t* mtime_ns, int64_t* ctime_ns) = 0;
  virtual Status lock(std::string_view path, void** lock) = 0;
  virtual void unlock(void* lock) = 0;
};

class FileWatcher {
 public:
  virtual ~FileWatcher() = default;
  virtual Status watch(std::string_view path, uint64_t* id) = 0;
  virtual bool poll(uint64_t* id, uint32_t* mask) = 0;
};
```

Per-OS implementations live in `platform/macos`, `platform/windows`, `platform/linux`.
This keeps core OS-free and testable with an in-memory fake. `clone_file` must
clone the **open original handle** (the original inode), not the path, so external
replacement/truncation cannot mix old offsets with new bytes. `Mapping`/`FileHandle`
are opaque platform types.

### 4.6 Workspace and session

- **`workspace`** owns the directory tree cache, ignore rules, content mmap LRU,
  and bounded parallel scan. It is a core module, not a UI feature.
- **`DocumentSession`** owns open documents, undo stacks, and change ranges. The
  CLI and GUI both consume it. `serve` (post-1.0) is a transport over it.

---

## 5. Document model contract

### 5.1 Bytes, lines, EOL, encoding

- A **line** is a byte range `[start, end)` excluding its terminator.
- **`line_count`** = number of terminators + (1 if the last line is non-empty).
  Empty file = 0. `a\n` = 1; `a` = 1; `a\n\n` = 2.
- **`col`** is a 0-based UTF-8 byte offset within the line (tabs = 1 byte).
- **EOL** is per-file (`lf`, `crlf`, `cr`, or `mixed`). Mixed endings are
  preserved. A CR at a chunk boundary plus LF in the next chunk is **one** CRLF
  terminator (one-byte cross-chunk lookahead). Lone CR is a terminator.
- **Encoding**: UTF-8 strict for v1. Invalid UTF-8 → `E_INVALID_UTF8` with the byte
  offset. Non-UTF-8 (UTF-16 BOM, Latin-1) → `E_UNSUPPORTED` with a clear diagnostic;
  transcoding is post-MVP. Record `encoding`, `bom`, `final_newline`.
- **BOM** is metadata; it is not part of line 1's content. `col` is measured after
  the BOM.
- **Validation is lazy**: only touched lines are validated. A `validate` command
  can scan the whole file on request. `Info.encoding` is always `utf-8` in v1.

### 5.2 Piece table and add buffer

- The original file is an immutable base piece; edits append to an add buffer.
- **Coalescing is mandatory**: adjacent inserts at the add-buffer tail with a
  predecessor ending exactly at the append offset merge into one piece. Without it,
  typing creates unbounded pieces and `read_lines` degrades to O(k).
- The add buffer is **capped (8 MB) and spilled to a temp file** when exceeded;
  it is compacted before save. Non-tail inserts do not coalesce.
- "Edits are metadata" means **piece descriptors + inserted bytes** in the add
  buffer, not a full-document copy.

### 5.3 Sparse chunk line index

- A balanced tree of chunks; each chunk covers **64 lines** and stores
  line-start offsets **relative to its own start** plus a line count.
- An edit touches O(log n) chunks and shifts nothing in untouched chunks.
- **Never invalidate the whole index on edit.**
- Memory: 8 bytes per 64 lines. 5M lines ≈ 0.6 MB; 25M lines ≈ 3 MB;
  125M lines ≈ 16 MB. Budgeted in §7.
- Worst case: an edit that splits a chunk costs O(lines in that chunk), bounded by
  the chunk size; split/merge rules are specified and property-tested.
- Built lazily; only touched chunks are resident.

### 5.4 Undo/redo

- Undo = apply the **inverse edit** (delete inserted range, re-insert deleted text).
- **Coalesce** typing runs into one step (500 ms or word boundary). **Programmatic
  edits (`apply`, `batch`) do not coalesce** — one edit = one undo step.
- **Cap** the undo stack at 10k steps or 8 MB, whichever comes first. Inverse text
  larger than the cap is **spilled to a temp file**; if spilling is impossible,
  undo is disabled for that edit and reported.
- **Redo** is cleared on any new edit.
- **Compaction at save**: flatten the piece table, drop undo older than the save
  point (or keep a bounded tail), clear the add buffer.
- Undo across save: "undo to last save" by default; full session undo is opt-in.

### 5.5 Save path (incremental, with an honest cost model)

**The O(changed) claim only holds for same-length edits.** A length-changing edit
shifts the tail. The plan states both cases.

- **No-op if clean.**
- **Same-length edits**: CoW-clone the original (O(1) metadata on APFS; O(file) on
  non-CoW filesystems), `pwrite` only changed spans, then fsync → rename → fsync.
- **Length-changing edits**: the tail after the edit must be rewritten. If the
  shifted tail is ≤ 25% of the file, `pwrite` the tail; otherwise fall back to a
  full rewrite. Document the chosen path in the save result.
- **Clone the original fd/inode**, not the path. If the path's identity changed
  since open, fail with `E_CONFLICT`; never mix old offsets with new bytes.
- **Fallback**: `copy_file_range` for unchanged ranges; handle `EXDEV` and
  same-file overlap (`EINVAL`) with a read/write fallback. Never copy only a prefix
  and leave the suffix unwritten.
- **Temp file**: same directory, `O_CREAT|O_EXCL`, 0600, cleanup on failure.
  Rename requires write permission on the directory; if the directory is read-only,
  return `E_IO` with a clear message or offer `--in-place` (documented caveat).
- **Metadata**: `ReplaceFile` on Windows; `CLONE_ACL` + explicit xattr/ACL copy on
  macOS; explicit xattr/ACL copy on Linux. `chown` before `chmod` (chown can clear
  setuid/setgid). Preserve hardlink caveat → `--in-place`.
- **Durability**: `fsync(temp)` → `rename` → `fsync(dir)`; macOS `F_FULLFSYNC` for
  power-loss durability (with a cheaper `fsync` crash-only mode); Windows
  `FlushFileBuffers`/volume flush (no directory fsync). If `fsync(dir)` fails after
  rename, return `SavedButNotDurable` (a warning, `ok: true`); **never blind-retry
  after rename**.
- **Post-save transition (atomic)**: new base = saved bytes (remap the temp or
  rebuild the piece table), clear the add buffer, drop undo older than the save,
  update identity/mtime/rev. A double-save conformance test must pass.
- **Gate**: same-length edit in 1 GB on APFS < 100 ms; length-changing edit has a
  separate measured budget (O(tail)). Non-CoW filesystems document the O(file) cost.

### 5.6 External mutation and SIGBUS

- Record `(dev, ino, size, mtime_ns, ctime_ns)` at open.
- **Rename-based replacement** (git, most tools) is safe: the old inode stays mapped.
- **In-place truncate** causes `SIGBUS` (Linux/macOS) or
  `EXCEPTION_IN_PAGE_ERROR` (Windows). Handle with a `SIGBUS` handler +
  `sigsetjmp`/`siglongjmp` around risky accesses (no locks/allocations in the
  handler), or use `pread` for at-risk files; Windows uses a vectored exception
  handler / `__try`.
- On identity mismatch: **auto-reload if clean**; if dirty, return `E_STALE` with a
  reload hint (non-interactive; the CLI/agent decides). `reload()` discards undo,
  maps the new content, and maps the caret to the nearest valid position.
- File watching (FSEvents/inotify/`ReadDirectoryChangesW`) is coalesced and used to
  detect changes; it is not the source of truth.

### 5.7 mmap policy (not "one mapping for everything")

- **Sliding-window mapping** (256 MB–1 GB) for large files: bounds VA and avoids a
  100 GB `MapViewOfFile`.
- **`pread`** for network filesystems (NFS/SMB coherence) and as a fallback.
- **Buffered `read()` under ~64 KB**: small files are faster without mmap.
- **No `MAP_PRIVATE` writes for large files**: dirty pages have no file backing,
  cannot be evicted, and count against footprint. Use the capped/spilled add buffer.
- **Base mapping is `MAP_PRIVATE` read-only** (immutable); external writes are
  detected by identity, not by observing the mapping.
- **Scans**: chunked `posix_fadvise(POSIX_FADV_SEQUENTIAL/DONTNEED)` (or
  `madvise` where appropriate) + `pread` into a sliding buffer. Never
  mmap-the-world: a measured 2 GB scan pulled 1.68 GB into RSS.

---

## 6. CLI and agent contract

Binary: `editor`. The GUI is a separate app (`Editor.app` / `editor-gui`) linking
the same core. Both resolve through the same domain ops.

### 6.1 Command surface

```
editor read <path> [--from N | --from-offset B] [--to N | --length L]
                   [--around N --radius K] [--max-bytes N] [--out FILE] [--stream]
editor write <path> (--stdin | @FILE) [--expect REV] [--force] [--append] [--in-place]
editor edit <path> (--old TEXT|@FILE --new TEXT|@FILE [--all | --count N]
                   | --at LINE[:COL] --text TEXT|@FILE
                     [--op insert|replace|delete] [--length L | --end LINE[:COL]])
                   [--expect REV] [--dry-run] [--in-place]
editor stats <path>
editor validate <path> [--max-errors N]
editor move <src> <dst> [--expect REV] [--force]
editor tree <dir> [--depth N] [--glob G]
editor batch [--file F | --stdin]
editor info [--version] [--schema [COMMAND]]

# post-M0 (need a history store or patch parser):
editor diff <path|dir> [--from REV] [--to REV] [--context N]
editor restore <path> [--to REV | --to HEAD]
editor apply [--patch FILE | --stdin] [--root DIR] [--check] [--best-effort]
editor open <path>

# M6 (project search):
editor search <path|dir> --pattern RE [--literal] [--glob G] [--context N]
                    [--max-results N] [--stream]
```

**Global flags** on every path-taking command: `--root DIR` (default: current
directory), `--allow-outside` (escape), `--hash` (force SHA-256 `rev`),
`--allow-huge` (override `--max-file-bytes`), `--verbose` (stderr logs).
`--json` is removed — output is always JSON.

**Units and defaults:**

- `read`: `--from`/`--to` are 1-based lines; `--from-offset`/`--length` are bytes.
  Mutually exclusive per axis. `--around` requires `--radius`. Default
  `--max-bytes` 256 KiB; beyond that use `--out` or `--stream`. `--stream` emits
  NDJSON with a terminal summary record.
- `edit`: `--at LINE[:COL]` (1-based) with `--op insert|replace|delete`; delete/
  replace need `--length` or `--end`. `--old`/`--new` accept `TEXT`, `@FILE`, or
  `-` (stdin). `--count`/`--all` are mutually exclusive; default is one match.
- `search`: literal by default; `--glob`; `--context` default 0; `--max-results`
  default 1000; `--stream` emits NDJSON.
- `tree`: read-only listing for agents; `--depth` default 3; `--glob`.
- `write`/`edit`: `--in-place` for hardlink/read-only-directory cases (documented
  crash-safety caveat). `--max-file-bytes` defaults to 2 GB; override with
  `--allow-huge`.
- `validate`: scans for invalid UTF-8/BOM/encoding issues; `--max-errors` default 100.

**Cut from v3:** `goto` (folded into `read --around`), `insert`/`replace`/`delete`
as separate verbs (one `edit --op`), `write --content` (quoting/ARG_MAX/NUL),
`--json` (always JSON).

### 6.2 JSON envelope (always stdout)

```json
{
  "schema": "editor.result/1",
  "command": "edit",
  "ok": true,
  "duration_us": 2140,
  "cwd": "/path/to/editor",
  "warnings": [],
  "results": [
    {
      "path": "src/main.cpp",
      "rev_before": {"kind": "stat", "value": {"dev": 1, "ino": 2, "mtime_ns": 3, "ctime_ns": 4, "size": 4096}},
      "rev": {"kind": "stat", "value": {"dev": 1, "ino": 2, "mtime_ns": 5, "ctime_ns": 6, "size": 4112}},
      "bytes_before": 4096,
      "bytes": 4112,
      "line_count": 142,
      "sha256_before": null,
      "sha256": null,
      "line_endings": "lf",
      "encoding": "utf-8",
      "bom": false,
      "final_newline": true,
      "changed": {"from": 4210, "to": 4288},
      "save": "clone_pwrite_tail"
    }
  ],
  "error": null,
  "meta": {"editor_version": "0.1.0", "pid": 48211}
}
```

Rules:

- **One shape for every command.** Single-file commands return a 1-element
  `results[]`; `apply`/`batch` return N. Agent code never special-cases cardinality.
- **Always JSON on stdout.** Stderr is logs only (`--verbose`). No TTY sniffing.
- **Exit codes:** `0` ok · `2` domain error · `3` usage/schema · `4` internal.
  `ok:false` always implies nonzero.
- **Determinism:** sort results by (path bytes, offset); fixed field order; no
  `unordered_map` iteration; no timestamps by default; golden tests normalize
  `duration_us`/`pid`/abs paths. Cross-OS golden matrix for path separators/EOL.
- **Versioning:** `"editor.result/1"`. Additive-only within a major.
  `--schema 1` requests a version; unknown major → exit 3, `E_SCHEMA`.
  `editor info --schema` dumps command schemas.
- **Large outputs:** `read` inline up to `--max-bytes`; beyond that `--out` or
  `content_ref` (path + offset + length + sha256). `search --stream` is NDJSON.
- **Units:** `changed.from`/`changed.to` are byte offsets; `line_count` is logical
  lines; `bytes` is file size.
- **`sha256` is nullable** with a reason: hashing 1 GB costs ~0.5–1 s. It is
  computed when the file is under the hash threshold (default 1 MB) or `--hash` is
  passed. `rev` is always present.

### 6.3 Rev model

- `rev` is a structured value: `{"kind":"stat"|"sha256","value":...}`.
- **Default is `stat`**: `dev`, `ino`, `mtime_ns`, `ctime_ns`, `size`. Cheap,
  works on huge files, includes `ctime_ns` to reduce collisions.
- **SHA-256** is used under the hash threshold or with `--hash`; it is the portable
  choice for `--expect` across filesystems.
- `--expect` accepts both compact forms: `sha256:<64 hex>` or
  `stat:<dev>:<ino>:<mtime_ns>:<ctime_ns>:<size>`. `stat` is advisory across
  platforms; `sha256` is portable.
- The GUI uses `save(expected_rev)` with the same model.

### 6.4 Error model and exit codes

One taxonomy. `Status` (core) maps 1:1 to CLI `E_*` codes.

| Core `Status` | CLI code | Exit |
| --- | --- | --- |
| `Ok` | — | 0 |
| `Io` | `E_IO` | 2 |
| `NotFound` | `E_NOT_FOUND` | 2 |
| `Range` | `E_RANGE` | 2 |
| `Conflict` | `E_CONFLICT` | 2 |
| `Stale` | `E_STALE` | 2 |
| `Oom` | `E_OOM` | 4 |
| `Encoding` | `E_ENCODING` | 2 |
| `InvalidUtf8` | `E_INVALID_UTF8` | 2 |
| `Binary` | `E_BINARY` | 2 |
| `OutsideRoot` | `E_OUTSIDE_ROOT` | 2 |
| `Parse` | `E_PARSE` | 2 |
| `RegexUnsupported` | `E_REGEX_UNSUPPORTED` | 2 |
| `TooLarge` | `E_TOO_LARGE` | 2 |
| `Locked` | `E_LOCKED` | 2 |
| `Schema` | `E_SCHEMA` | 3 |
| `Unsupported` | `E_UNSUPPORTED` | 2 |
| `Usage` | `E_USAGE` | 3 |
| `Internal` | `E_INTERNAL` | 4 |
| `TooSmall` | `E_TOO_SMALL` | 2 |
| `SavedButNotDurable` | warning, `ok:true` | 0 |

Errors carry `code`, `message`, `retryable`, `path`, `hint`, and command-specific
fields. `E_INVALID_UTF8` includes the byte offset. `E_NO_MATCH`/`E_AMBIGUOUS`
include the match count and a bounded list of candidate `{line, col, byte_offset}`
— never file contents.

```json
{
  "schema": "editor.result/1", "command": "edit", "ok": false, "duration_us": 900,
  "error": {
    "code": "E_CONFLICT",
    "message": "file changed since read",
    "retryable": true,
    "path": "src/main.cpp",
    "expected_rev": "stat:1:2:3:4:4096",
    "actual_rev": "stat:1:2:5:6:4112",
    "hint": "re-read, then retry with --expect stat:1:2:5:6:4112"
  },
  "results": []
}
```

### 6.5 One-shot + batch

- **One-shot** for reads, search, stats, and single edits. Process spawn is
  ~1–2 ms for a static C++ binary — not the thing to optimize.
- **`batch`** is a JSON-lines program with **validation-stage atomicity**:
  1. Parse and validate every op (paths, revs, ranges) against the original files.
     If any op fails validation, **no writes occur**.
  2. Stage each file write to a temp file; then commit (rename) each.
  3. On a commit failure, roll back already-applied writes from backups. If rollback
     fails, return `ok: false`, `rolled_back: false`, and the list of applied ops.
  4. Responses are buffered until commit; streamed responses are not emitted before
     commit.
- **Not crash-atomic.** A crash during commit can leave partial writes. Document
  this loudly; `--check` validates without writing.
- **Read-your-writes**: batch ops see the original files, not earlier batch edits.
  Two ops on the same file must be non-overlapping or the later one fails with
  `E_RANGE`/`E_CONFLICT`.
- **`serve` is deferred.** When built (post-1.0), it is a transport over
  `DocumentSession` (buffers + undo + change ranges), not a new subsystem.

Batch program:

```json
{"op":"edit","path":"a.cpp","old":"let x = 1;","new":"let x = 2;","expect":"sha256:9f2c"}
{"op":"edit","path":"b.cpp","old":"foo()","new":"bar()","all":true}
{"op":"diff","path":"a.cpp"}
```

### 6.6 Concurrency and security

- **`--expect <rev>` is mandatory on existing files** for mutations; `--force`
  overrides (and is reported). Mismatch → `E_CONFLICT`, exit 2.
- **Close the TOCTOU**: the expect-check, write, and rename happen inside the lock.
- **Lock file**: `<path>.editor.lock`, `O_CREAT|O_EXCL`, 0600, containing a random
  token + pid + process start time. Stale detection uses pid liveness + token
  (PID reuse safe). Advisory/cooperative only.
- **Symlinks**: resolve, re-check inside `--root`, `O_NOFOLLOW`/`openat2
  (RESOLVE_NO_SYMLINKS)` on the final component; Windows reparse-point check.
  Never rename over a symlink.
- **Refuse non-regular files** (FIFOs, devices, dirs); enforce `--max-file-bytes`
  unless `--allow-huge`.
- **Scripts** (post-1.0): arbitrary code execution by design. Trust prompt /
  `--yes`; never invoked from GUI, `apply`, or config; allowlist in
  `.editor/scripts/`; not a mod API.
- **Config** (`.editorrc`): data only, versioned, no `exec`/shell interpolation;
  fuzzed. A malicious repo must not execute.
- **Regex DoS**: step budget + timeout; forbid backreferences/lookaround; return
  `E_REGEX_UNSUPPORTED`; literal is the default fast path; publish the dialect.
- **Untrusted patches**: typed AST, hard caps (max files, hunks, line length, total
  bytes); reject absolute paths and `..`; resolve against `--root`; re-validate;
  handle `\ No newline at end of file` and `/dev/null`; fuzz.
- **Secret leakage**: argv is visible in `ps`; `--text @file`/`--stdin` preferred;
  document the leak; error messages never echo file contents or matched lines.
- **DoS defaults**: `--max-bytes`, `--max-results`, search timeout, tree-walk depth
  cap, symlink-cycle detection via visited inodes.
- **`serve` (post-1.0)**: Unix socket 0600, never TCP by default; per-session token;
  request timeouts; no shell-out over RPC.

### 6.7 "Always up to date" registry invariant

The command registry entry is `{ name, arg_schema, core_fn, cli_binding,
gui_binding? }`.

- **A registry entry with `core_fn` and no `cli_binding` is a test failure**, except
  an explicit `cli_excluded` allowlist (with a reason).
- **A `gui_binding` with no undo entry is a test failure** for mutating commands.
- `editor info --schema` is generated from the registry, not hand-written.
- A golden test asserts every GUI menu item maps to a domain op id.

---

## 7. Resource budgets and measurement

### 7.1 Metrics per OS (not directly comparable)

| OS | Metric | Tool |
| --- | --- | --- |
| macOS | `phys_footprint` | `footprint` / `ps` |
| Windows | private working set | `GetProcessMemoryInfo` |
| Linux | PSS (proportional set size) | `/proc/<pid>/smaps_rollup` |

State the metric with every measurement. Do not compare raw numbers across OSes;
compare the same workload.

### 7.2 Budgets (caps, not additive)

| Metric | Cap |
| --- | --- |
| Empty-window baseline | **≤ 12 MB** |
| + line-layout cache (1000–2000 entries, long lines skipped) | **≤ 6 MB** |
| + undo/edit log (spilled large inverse text) | **≤ 8 MB** |
| + add buffer (spilled) | **≤ 8 MB** |
| + sparse line index (5M lines) | **≤ 1 MB** |
| + content mmap LRU (search) | **≤ 60 MB** |
| **Worst-case loaded (1 GB file, cache + undo + index)** | **≤ 40 MB** |
| **Worst-case with search LRU** | **≤ 96 MB** |

The 12 MB figure is the **empty-window baseline**, not a product-wide claim.

### 7.3 Measured facts (macOS, Apple Silicon, 8 GB)

| Fact | Measurement |
| --- | --- |
| Bare C exe RSS / footprint | 6.4 MB / 1.6 MB |
| + `NSApplication` | 28.3 MB / 5.3 MB |
| + `NSWindow` | 39.9 MB / 10.2 MB |
| + window shown, one draw | 40.6 MB / 10.4 MB |
| mmap 2 GB untouched | +0 MB, 0.010 ms |
| mmap 2 GB full scan | **1.68 GB RSS** |
| `CTLineCreate` cold unique line | 9.2 µs |
| Cached CoreText line object | ~3.1 KB heap each |
| 60 frames × 40 cached line draws | 0.26 ms/frame |
| Cold shape+draw | 0.43 ms/frame |
| `CTFontCreateWithName(Menlo)` first call | 16.7 ms |
| `NSFontManager availableFontFamilies` | 80.8 ms (175 families) |
| AppKit-linked binary | 51 KB |
| Time to first window | 60–390 ms |

### 7.4 Performance budgets

| Operation | Budget |
| --- | --- |
| Idle CPU | **< 0.05%** (caret blink only) |
| Scroll, cached | **≤ 3 ms/frame** = 18% of one core at 60 fps |
| Scroll, cold lines | **≤ 8 ms/frame** = 48% of one core at 60 fps |
| Edit to model / to frame | **< 1 ms / < 3 ms** |
| Open 1 GB read-only | **< 100 ms to first paint** (lazy validation) |
| Same-length edit in 1 GB (APFS) | **< 100 ms p99** |
| Length-changing edit in 1 GB | **O(tail)**, measured separately |
| Scan 1 GB | **250–500 ms** |
| Startup warm / cold | **< 150 ms / < 400 ms** |

### 7.5 Measurement methodology

- Use the per-OS metric above; `CLOCK_MONOTONIC` around frames; `powermetrics`/
  `os_signpost` for CPU attribution.
- Assert **p99 frame budget**, not average.
- Bench suite: 5M-line file, 25M-line file, 5 GB file, 10k-file tree, 100 MB single
  line, 1 MB single line.
- Separate **warm** (index built) from **cold** (first random edit) budgets.
- `F_FULLFSYNC` (power-loss durability) vs `fsync` (crash-only) measured separately.

---

## 8. Rendering

### 8.1 Custom view only (v1)

- One custom view on all platforms, driven by `viewcore` + `textbackend`.
- The view owns selection, caret, scroll, IME composition, and a11y; the core owns
  bytes and edits.
- No native-control hybrid in v1. A native control is a post-MVP experiment, only
  if the custom view misses IME/a11y/perf and a spike proves it is better.
- This avoids two sources of truth, the UTF-16 seam, and the Linux gap.

### 8.2 Line-layout cache

- Key: **(logical line, wrap width)**. One shaped run per *visual* line.
- **Bounded LRU, cap 1000–2000 entries and a total byte cap.** Skip caching for
  lines above N visual lines (e.g. 1000) — a single 100 MB line must not blow the
  cache.
- Invalidate entries at/after the first changed line; invalidate all on width/font
  change. Cold re-shape is 0.43 ms/frame — invisible at 60 fps.
- Long-line benchmark: 100 MB single line, 1 MB single line.

### 8.3 IME and accessibility

- IME and a11y are implemented in `viewcore` against each platform's APIs:
  `NSTextInputClient` (macOS), TSF/IMM32 (Windows), XIM (Linux/X11).
- Budget them in M1 (macOS), M4 (Windows), M5 (Linux), not "post-MVP."
- **Known gap**: full VoiceOver/Narrator/AT-SPI parity is post-MVP; v1 targets
  keyboard-complete + basic screen-reader support. State this in the release notes.

### 8.4 Text shaping and fonts

- `textbackend` is the only place that shapes glyphs: CoreText (macOS),
  DirectWrite (Windows), Xft+HarfBuzz (Linux). Xft alone has no complex shaping;
  HarfBuzz closes the Linux parity gap (confirm availability on target distros).
- **Do not enumerate fonts at launch.** `availableFontFamilies` = 80.8 ms and
  +4.8 MB; resolve one font (`userFixedPitchFontOfSize`) = 1.6 ms. Enumerate lazily
  when the font menu opens.
- Monospace default. No bundled font in v1 (OS fonts); if identical metrics are
  required later, bundle one monospace font (+200–800 KB).

### 8.5 Wayland

- v1 targets **X11/XWayland**. Native Wayland + text-input is post-MVP.
- Document XWayland IME/HiDPI limits; do not claim native Wayland support.

---

## 9. Workspace and search

- **In-memory tree**: dirs/files, path, size, mtime, ino, mode. Per-entry stat
  validation (not directory mtime alone). 10k files ≈ 1.5–3 MB.
- **Content, mmap'd on demand**, not held. Search reads each file (I/O bound:
  100 MB ≈ 50–100 ms on NVMe), then releases it.
- **No inverted index in v1.** Keep a **bounded content-mmap LRU** (50–60 MB) so
  repeated agent searches don't re-hit disk.
- **Chunked scans** with `posix_fadvise`/`pread`; never mmap-the-world.
- **Bounded parallelism**: 4–8 workers, each with a small mmap window and a
  `Snapshot` of the document. Bounded work queue.
- **Cache location**: `~/.cache/editor/tree-<hash>`; keyed by tree root + config
  version; validate per-entry stat on load; stale entries are dropped.
- **M0 search is single-file only.** Project/tree search lands in M6 with the
  `workspace` module.
- **`editor tree`** is the read-only agent listing; no GUI project tree in v1.

---

## 10. Future features and seams

The v1 architecture includes the seams so these do not force a redesign.

### 10.1 Search (M6)

- Literal + glob + RE2-like documented subset; no backreferences/lookaround; step
  budget + timeout; literal fast path; fuzz.
- Project walk: ignore rules, parallel workers, content LRU, `Snapshot`.
- Fuzzy/vector search deferred; index format designed to allow it later.

### 10.2 Syntax highlighting (M7)

- Lazy, per-visible-line tokenization; nothing off-screen is highlighted.
- Data-driven rule tables per language (keywords/regex), compiled once, cached.
- Hand-written tables, no tree-sitter; start with a handful of languages.
- Emitted as `Decoration` ranges + styles over the viewport.

### 10.3 Markdown preview (post-1.0)

- Preview only at MVP; rendered editing is post-MVP.
- A small built-in renderer; blocks map to the `textbackend` run interface.
- Never blocks editing.

### 10.4 Git (post-1.0)

- Via the `git` CLI subprocess; native status/diff/commit commands.
- A git-status sidecar supplies gutter decorations; no reimplementation.

### 10.5 Scripts (post-1.0)

- `editor run <script>` with a trust prompt/allowlist; streams stdout/stderr and a
  JSON result record.
- Not a mod API; no in-process plugin ABI.

### 10.6 `serve` (post-1.0)

- A transport over `DocumentSession`; Unix socket 0600; per-session token; request
  timeouts; no shell-out over RPC.

---

## 11. Testing and CI

- **Unit tests**: core, viewcore, workspace, CLI parsers.
- **Property tests**: edit/insert/delete/undo invariants; piece-table round-trips;
  save/restore identity; UTF-8 boundary; chunk split/merge; line-count conventions;
  coalescing (typing run → one step); CRLF/CR/mixed EOL; empty file; no trailing
  newline; huge line.
- **Fuzzing**: argv, JSON reader/writer, patch parser, config parser, UTF-8
  boundaries, line index.
- **Golden CLI tests**: exact JSON for every command, including errors; cross-OS
  normalization for path separators/EOL.
- **Conformance tests**: `Editor` interface (open/read/apply/undo/save/reload/
  rev/snapshot/cancel/change), `TextView` (wrap/hit-test/selection/scroll/IME/
  decorations), `textbackend` (measure/shape/draw), `FileSystem` (map/pread/clone/
  replace/identity/lock).
- **Sanitizers**: ASan/UBSan on every PR (lite); TSan nightly (full on Linux;
  limited on macOS/ObjC++; MSVC ASan separately). Hardened STL in debug.
- **Fault injection**: `fsync`/`rename`/`pwrite` failures; crash-consistency of
  temp→rename; batch rollback; lock/TOCTOU two-process stress; external truncate
  during read.
- **Benchmarks**: cold start, open 1 GB, same-length edit, length-changing edit,
  edit viewport, scan 1 GB, 5M/25M-line file, 5 GB file, 10k-file tree, 100 MB
  single line; footprint gates per OS.
- **CI**: functional smoke on all 3 OSes every PR; perf nightly/dedicated runners;
  release engineering as its own milestone (M3.5).

---

## 12. Build and distribution

- **One CMake build graph** generates the Xcode project (macOS) and MSVC project
  (Windows); Linux uses the same CMake. No separate hand-maintained Xcode/MSVC
  projects.
- Artifacts: `libcore.a`/`core.lib`, `libviewcore.a`/`viewcore.lib`,
  `platform-<os>` libs, `editor` CLI, `Editor.app`/`editor-gui.exe`/`editor-gui`.
- C++20, pinned toolchain. Windows CRT: static `/MT` (one runtime in the build).
- macOS: notarization (Developer account, hardened runtime, `notarytool`, staple);
  universal arm64+x86_64.
- Windows: SmartScreen reputation (EV cert or downloads); `ReplaceFile`,
  `FILE_FLAG_OPEN_REPARSE_POINT`.
- Linux: `.deb`/`.rpm`/AppImage/Flatpak; freedesktop libs (X11/Xft, fontconfig,
  freetype, HarfBuzz) treated as system libraries; confirm availability.
- No vendored third-party; only OS system libraries and the toolchain.

---

## 13. Roadmap, effort, and cut list

Estimates are for one expert developer, including tests/CI, not calendar promises.
MVP = **macOS M0–M3.5**. Windows/Linux/search/syntax are post-MVP.

| Milestone | Scope | Est. | Gate |
| --- | --- | --- | --- |
| **Pre-M0.0** | Freeze `editor.hpp` draft + C++ conformance test + data-model contract. | 1 wk | Interface compiles and conformance skeleton runs. |
| **M0a** | Piece table + sparse chunk index + `read`/`write`/`stats` CLI + save (same-length CoW) + JSON envelope. | 3–4 wk | Read/write round-trip; 1 GB same-length edit < 100 ms p99 on APFS. |
| **M0b** | `edit` + undo/redo + length-changing save (tail/full rewrite) + fault-injection tests. | 3–4 wk | Edit/undo property tests; double-save test; save-crash test. |
| **M0c** | `batch` + `move` + `tree` + security guardrails + error model + registry invariant. | 2–3 wk | Batch validation/rollback; fuzz parsers; CLI golden. |
| **M0.5** | Headless render oracle + viewcore skeleton + **5 GB read-only demo** + C++ bug-density/perf validation. | 2 wk | 5 GB at 60 fps, flat footprint; sanitizers/fuzz clean. |
| **M1** | macOS read-only custom view: AppKit + CoreText + viewcore + IME/a11y skeleton. | 4–6 wk | Open/scroll 5 GB; keyboard-complete. |
| **M2** | macOS editing + undo + save + find/replace + settings/keybindings. | 4–6 wk | Usable daily editor. |
| **M3** | macOS polish: status bar, gutter, wrap, decorations, IME/a11y. | 3–4 wk | Feature checklist (macOS). |
| **M3.5** | Release engineering: notarization, packaging, CI perf gates. | 2–3 wk | Signed `.app`; CI green. |
| **M4** | Windows custom view: DirectWrite, IME, a11y. | 5–7 wk | Windows parity (macOS feature set). |
| **M5** | Linux custom view: X11/Xft/HarfBuzz, XIM, AT-SPI, Wayland assessment. | 6–8 wk | Linux parity (documented limits). |
| **M6** | Project search (workspace, ignore rules, LRU, parallel). | 3–4 wk | 10k-file search < 1 s warm. |
| **M7** | Syntax highlighting (data-driven, per-language). | 3–4 wk | Visible lines highlighted at 60 fps. |
| **Post-1.0** | Markdown preview, git, scripts, `serve`, native-control experiment. | — | — |

**Rough total:** ~35–50 weeks for the macOS MVP (M0–M3.5), ~11–15 weeks for
Windows/Linux (M4–M5), ~6–8 weeks for search/syntax (M6–M7). About **12–18 months
solo** to a three-platform v1; the macOS MVP is ~9–12 months.

**Cut list if behind schedule (in order):**

1. Windows/Linux → post-1.0 (macOS-only v1).
2. Wayland native → post-1.0 (XWayland only).
3. Full a11y parity → keyboard-complete only.
4. Search/syntax → post-1.0.
5. Markdown/git/scripts/serve → post-1.0 (already).
6. Do **not** cut: save-path correctness, fault injection, find/replace, settings,
   status bar/gutter, IME on macOS.

---

## 14. Feature checklist

### In v1 (macOS M0–M3.5)

- Open/save (atomic, incremental), edit, undo/redo.
- Find/replace (M2), settings + keybindings (M2), status bar + gutter + wrap (M3).
- CLI: `read`, `write`, `edit`, `stats`, `move`, `tree`, `batch`, `info`.
- IME + keyboard-complete a11y on macOS.

### Explicit non-goals for v1

- Multi-cursor, command palette, split panes, GUI project tree.
- Native-control hybrid.
- Plugins/mods, network, Markdown rendered editing, git UI, script runner, `serve`.
- Full a11y parity, native Wayland, UTF-16.

### Post-1.0

- Markdown preview, git, scripts, `serve`.
- Native-control experiment, UTF-16 transcoding, full a11y, native Wayland.
- Search (M6) and syntax (M7) are v1 but post-MVP.

---

## 15. Open decisions

1. **Config format**: key=value (default) vs. a small embedded format. Data-only,
   versioned, fuzzed.
2. **Regex dialect**: RE2-like subset (default) — publish the grammar and a
   conformance corpus.
3. **CLI JSON/SHA-256**: zero-dep hand-rolled (default) vs. a vetted header-only
   allowlist in `cli` only.
4. **Bundled font**: OS fonts (default) vs. +200–800 KB for identical metrics.
5. **Linux freedesktop libs**: confirm HarfBuzz/fontconfig/freetype availability on
   target distros.
6. **Native-control experiment**: only if custom view misses IME/a11y/perf.
7. **Windows CRT**: static `/MT` (default) vs. `/MD`.
8. **`stat` rev across platforms**: document `st_ino`/`ChangeTime` semantics; prefer
   `sha256` for portable `--expect`.

---

## 16. Risk register

| Risk | Likelihood | Impact | Mitigation |
| --- | --- | --- | --- |
| Length-changing save is O(tail), not O(changed) | High | High | Honest cost model; tail/full-rewrite threshold; benchmark both. |
| Custom-view IME/a11y effort | High | High | Budget in M1/M2/M3; native-control post-MVP fallback. |
| Linux platform long tail (XIM/AT-SPI/Wayland) | High | Medium | XWayland first; document limits; M5 estimate 6–8 wk. |
| Hand-rolled JSON/SHA-256/regex bugs | Medium | High | Fuzz + property tests; allowlist fallback. |
| Schedule (~12–18 months solo) | High | High | MVP = macOS; cut list; post-1.0 fence. |
| Temptation to re-add hybrid/native control | Medium | Medium | D2 decision + post-MVP trigger. |
| mmap SIGBUS on truncate | Medium | High | Signal handler/pread; identity check; reload. |
| External mutation race | Medium | High | ctime-aware rev; lock covers check+write; auto-reload clean. |

---

## 17. What I would build first

1. **Pre-M0.0**: freeze `core/include/editor.hpp` (with text payload, `read_lines`
   semantics, `save(expected_rev)`, `snapshot`, `cancel`) + the C++ conformance
   test + the one-page data-model contract (§5).
2. **M0a**: piece table + sparse chunk index + `read`/`write`/`stats` + same-length
   CoW save; 1 GB same-length edit gate.
3. **M0b**: `edit` + undo + length-changing save + fault-injection tests; double-save
   and save-crash conformance.

Everything else is downstream of those three.

---

## Appendix: language decision

Three independent final reviews (correctness, delivery, systems) compared Rust,
C++, and C against this plan, with the developer fluent in both Rust and C++.
**All three returned the same ranking: C++ 1st, Rust 2nd, C 3rd**, medium
confidence (~55–60%). Scorecards: C++ 34–36/40, Rust 26–30/40, C 17–26/40.

**Decision: C++ all-in.** Rust is the documented fallback.

Why C++ wins:

1. **The plan is already polyglot.** C++ viewcore + ObjC++ platform are mandatory.
   A C++ core subtracts a language and a foreign-ABI seam; a Rust core adds one.
2. **The hardest code is where Rust's safety does not reach.** CoW save,
   mmap/madvise, file watching, and the FFI boundary are OS-edge in both.
3. **The no-vendored rule is asymmetric.** It strips Rust's ecosystem (serde,
   regex, rayon) while C++ still gets `<format>`, `<span>`, `<charconv>`,
   `<filesystem>`, `std::variant`/`optional`, and direct OS headers.
4. **One language, one build graph, one debugger, one sanitizer pass.** Fewer
   moving parts for a solo developer shipping three OSes.

What Rust still wins (acknowledged): UTF-8 validation in the CLI/JSON layer,
untrusted parser safety, race-free parallel search, borrow-checked chunk index,
compiler-assisted refactoring. These are real but localized to the lower-risk
logic layer.

C++ safety is a process, not a proof: RAII, `std::span`/`string_view`, strong
index types, generation counters, hardened STL, ASan/UBSan/TSan, libFuzzer,
property tests, C++26 profiles/contracts, never exposing base pointers, and
remapping by swapping a new mapping.

**M0.5 kill gate:** if C++ pure-logic bug density stays high despite
sanitizers/fuzzing, or the untrusted surface grows (network `serve`, plugins),
switch to Rust.

**Flip conditions to Rust:** the crate ban lifts to a vetted allowlist
(`serde_json`, `regex`, `memchr`, `rayon`); the product becomes primarily an
untrusted-input processor; the team grows beyond one developer; M0.5 shows C++
bug density high or Rust velocity/seam clearly better; the project drops the C++
viewcore/ObjC++ platform.

C is out for the core at this scope.