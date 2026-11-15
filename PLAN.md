# editor — Plan (v2)

> Name: **editor** (chosen).
> Status: planning. v2 — refined after a four-way critical review.
> Review areas: architecture & data model · CLI & agent ergonomics ·
> resource efficiency & rendering · cross-platform & UI.

A native desktop text editor for **minimum memory, minimum CPU, maximum speed**,
with a **CLI-first** surface so AI agents can drive it as reliably as a human.
Real native windows, one shared Rust core, thin per-OS chrome.

---

## 0. What changed from v1, and why

v1 had the right shape (Rust core, native window, custom text view, piece table
over an mmap'd base, atomic save). The review found several claims false and the
two most important contracts (the C ABI and the save path) underspecified. The
biggest changes:

1. **Memory is measured in private footprint, not RSS.** Measured on macOS:
   AppKit alone is ~28 MB RSS before a window; a shown window is ~40 MB RSS but
   **~10.2 MB footprint**. `<30 MB RSS` is unachievable for any AppKit app.
   `<12 MB footprint` is achievable. Every budget and CI gate now uses
   `phys_footprint` (macOS) / private working set (Windows) / PSS (Linux).
2. **The save path was O(file) — a full rewrite on every edit.** A one-line edit
   to a 1 GB file copied 1 GB. Now: no-op when clean, CoW-clone + `pwrite` of
   changed spans, `copy_file_range` fallback, fsync temp → rename → fsync dir.
   Benchmarked as an M0 gate.
3. **The C ABI is frozen before any UI exists.** Status codes on every fallible
   call, `int64_t`, `(ptr,len)`, one batched viewport read, drain-based events,
   `undo`/`redo`/`reload`, a version handshake, and panics that never cross FFI.
4. **Bytes are the currency.** A line is a byte range; `col` is a UTF-8 byte
   offset within the line. No UTF-16, no graphemes in the core contract.
5. **Line index is chunk-relative and never fully invalidated.** v1 made every
   keystroke O(file) on a large file. Pieces coalesce.
6. **mmap is a policy, not a primitive.** Sliding-window mapping, `pread` for
   network FS and small files, chunked `madvise` for scans. v1's "mmap is free"
   is false under scan: a measured 2 GB scan drove RSS to 1.68 GB.
7. **Undo is specified, not hand-waved.** Coalesced inverse edits, grouped into
   steps, capped, compacted at save. v1's "move a cursor" cannot un-apply an edit.
8. **Rendering is hybrid.** Native OS text control for small/medium files (free
   IME, accessibility, RTL), custom view for huge files. One shared C++ viewcore.
   The line-layout cache is bounded (~1000–2000 lines; measured 3.1 KB/line).
9. **CLI gets a real agent contract.** One-shot + `batch` transactions,
   content-anchored `edit`, opaque content-derived `rev`, machine error codes,
   always-JSON stdout, and security guardrails.
10. **Scope is cut to a completable MVP.** macOS first; Windows/Linux designed
    for and ported later; syntax/Markdown/git/scripts deferred. The v1 feature
    list was IDE-shaped and would have killed the "minimal" goal.
11. **"No third-party libraries" is restated precisely.** No vendored or bundled
    third-party code. OS system libraries are allowed (Cocoa/AppKit/CoreText,
    Win32/DirectWrite, X11/Xft/fontconfig/freetype/HarfBuzz). On Linux there is
    no "OS framework" in the macOS sense; freedesktop libraries are the system.

---

## 1. Goals

- Open and edit files from a few bytes to many gigabytes without loading the whole
  file into memory.
- Stay responsive with thousands of files in a project.
- Near-zero idle CPU; small, predictable, **measured** memory footprint.
- Genuine native app on macOS, Windows, Linux.
- One shared core (Rust, std-only) for all platforms; thin native chrome.
- A CLI that exposes every feature, emits machine-readable output, and is driven by
  the same domain ops as the GUI.
- No vendored third-party code.

## 2. Non-goals

- No TUI. The interactive surface is a native window.
- No plugins/mods. No network sync, accounts, telemetry.
- Not an IDE: no debugger, no language servers, no build-system integration beyond
  "run a script".
- Not a WYSIWYG editor (Markdown preview is preview, not authoring).
- No inverted search index in the MVP (mmap + metadata cache + bounded content
  cache is enough; revisit if profiling demands it).

## 3. Locked decisions

| Decision | Choice |
| --- | --- |
| Core language | Rust, std-only, OS FFI confined to `core::sys` |
| UI | Native OS windows + native chrome + custom/hybrid text view |
| Platform order | macOS first; Windows/Linux designed-for-now, ported later |
| Dependency rule | No vendored/bundled third-party; OS system libraries allowed |
| CLI model | Stateless one-shot + `batch` transactions; `serve` deferred |
| Addressing | Bytes; line = byte range; col = UTF-8 byte offset |
| Memory metric | Private footprint (not RSS) |
| View | Hybrid: native control under threshold, custom view above |
| ABI | Frozen v1 before UI; typed structs; drain-based events |
| Scope | Core + CLI + macOS first; search/syntax/markdown/git/scripts later |

---

## 4. Architecture

```
                       ┌──────────────────────────────┐
  CLI (Rust) ─────────► │                              │
                       │  core (Rust, std-only)       │
  platform/macos ─────► │  document · edits · undo     │
  platform/windows ───► │  line index · save · search  │
  platform/linux ─────► │  domain ops (pure)           │
                       └───────────────┬──────────────┘
                                       │ frozen C ABI
                       ┌───────────────▼──────────────┐
                       │  viewcore (C++ · shared)     │
                       │  wrap · line cache · hit-test│
                       │  selection · scroll · caret  │
                       │  IME state · a11y tree       │
                       └───────────────┬──────────────┘
                                       │ textbackend
             ┌─────────────────────────┼─────────────────────────┐
        CoreText (macOS)          DirectWrite (Windows)      Xft+HarfBuzz (Linux)
```

- **`core` (Rust)**: document model, piece table, chunk index, undo, save,
  search, pure domain ops. No UI, no I/O policy, no exit codes.
- **`viewcore` (C++)**: all platform-independent view logic — wrapping, line
  cache, hit-testing, selection, scroll math, caret blink, IME composition state,
  accessibility tree. One implementation, one test suite.
- **`textbackend` (per OS)**: the only place that shapes and draws glyphs. A
  narrow interface: measure, shape a line into runs, draw runs.
- **`platform/<os>`**: window, menus, dialogs, keyboard/mouse translation,
  clipboard, IME plumbing, a11y node emission, and the native-control path for
  small files. No editing logic.
- **`cli` (Rust)**: argv → domain ops → persist once → JSON envelope.

### 4.1 The three-layer command seam

v1's "one handler shared by GUI and CLI" was wrong: the CLI needs batch/exit
codes/atomicity; the GUI needs undo granularity, selection, IME, cancellation.
Split into three layers:

1. **Domain ops (`core`)**: pure functions over a `Buffer` — `insert`,
   `replace`, `search`, `parse_patch` — returning `Vec<EditOp>` + `Vec<Change>`.
   No I/O policy, no persistence, no exit codes.
2. **Transaction/undo layer**: inverse-op stack, dirty flag, save policy. Shared.
3. **Adapters**: CLI (argv → ops → persist once → envelope) and GUI (menu → ops →
   mark dirty → repaint from change ranges). Neither calls the other.

Registry entry = `{ name, arg_schema, core_fn, cli_binding, gui_binding? }`.
A command with a `gui_binding` but no undo entry is a test failure.

### 4.2 The C ABI (frozen draft)

This is the highest-leverage artifact in the project. It is cheap to write now and
expensive to retrofit at M2. Ship it with a **C conformance client compiled with
`cc` and no Rust knowledge**.

```c
// platform/shared/editor.h — v1
typedef struct Editor Editor;
typedef uint32_t EdStatus;              // 0 = ok; nonzero = EdCode
#define ED_OK 0
#define ED_E_IO 1
#define ED_E_NOT_FOUND 2
#define ED_E_RANGE 3
#define ED_E_CONFLICT 4                   // changed since read
#define ED_E_OOM 5
#define ED_E_ENCODING 6
#define ED_E_PANIC 99

typedef struct {
  int64_t first, last, new_count, caret_line, caret_col;
  uint32_t kind, flags;                   // INSERT / DELETE / REPLACE / RELOAD / SAVE
} EdChange;

typedef struct {
  int64_t bytes, lines /* -1 = unknown */, mtime_ns;
  uint32_t eol, enc, flags;
} EdInfo;

typedef struct { int64_t line, col, removed_len, inserted_len; uint32_t kind; } EdEdit;

uint32_t   editor_abi_version(void);      // reject mismatch at load
EdStatus   editor_open(const char* path_utf8, size_t path_len, Editor** out);
void       editor_close(Editor*);
EdStatus   editor_info(const Editor*, EdInfo*);
const char* editor_errmsg(const Editor*); // borrowed; valid until next call on this thread

// One batched viewport read per frame; caller-owned buffer.
EdStatus   editor_lines(const Editor*, int64_t first, int64_t last,
                        char* buf, size_t cap, size_t* used, int64_t* out_n);
EdStatus   editor_read_range(const Editor*, int64_t off, size_t len,
                             char* buf, size_t cap, size_t* used);

EdStatus   editor_apply(const Editor*, const EdEdit* edits, size_t n); // one transaction
EdStatus   editor_undo(Editor*);
EdStatus   editor_redo(Editor*);
EdStatus   editor_save(Editor*);
EdStatus   editor_save_as(Editor*, const char* path_utf8, size_t len);
EdStatus   editor_reload(Editor*);

void       editor_on_change(Editor*, void (*fn)(Editor*, const EdChange*, void*), void*);
void       editor_drain(Editor*);         // callbacks fire ONLY here, never re-entrantly
```

Non-negotiable rules written into the header as comments:

- **Every fallible call returns `EdStatus`.** No `void` mutation/save entry points.
  A silent save failure is a data-loss bug.
- **`(ptr, len)` everywhere.** No NUL-terminated input; embedded NUL is data.
- **`int64_t` for lines/offsets.** `int32_t` cannot express a >2 GB line or an
  error, and `0` must not be ambiguous between "empty line" and "error".
- **`col` is a UTF-8 byte offset within the line.** Pin it once, conformance-test
  it, never mix conventions.
- **Core never calls into the UI directly.** It bumps a revision and appends
  deltas; the UI calls `editor_drain` on its own thread. This removes re-entrancy.
- **`catch_unwind` at every exported entry**, or `panic = "abort"`. Panics must
  never cross FFI (UB).
- **One `Editor` per thread** unless an explicit lock is added.
- **Borrowed buffers** (`editor_errmsg`, `editor_lines`) are core-owned and valid
  until the next core call on that thread. No `strdup`, no retention.
- **Windows CRT**: ship `core.dll` (`cdylib`) on Windows to avoid two heaps;
  `staticlib` on macOS/Linux.

---

## 5. Document model contract

### 5.1 Bytes are the currency

- A **line** is a byte range `[start, end)` excluding its terminator.
- **`line`** is 1-based for humans; **byte offset** is 0-based.
- **`col`** is a UTF-8 byte offset within the line (tabs = 1 byte). A separate
  `display_col` may be emitted for humans.
- **EOL** is per-file (`lf`, `crlf`, `cr`); mixed endings are preserved; a
  missing final newline is recorded. Mutations must not alter untouched lines'
  endings.
- **Encoding**: UTF-8 strict. Invalid UTF-8 → `E_INVALID_UTF8` with the byte
  offset; no lossy replacement. Record `encoding`, `bom`, `line_endings`,
  `final_newline`. Non-UTF-8 filenames emit `path` lossily plus
  `path_bytes_b64`.
- **Graphemes** are a view/cursor concern, not a core storage concern. Cursor
  movement uses a grapheme pass; storage and addressing stay bytes.

### 5.2 Piece table

- Original file is an immutable base piece; edits append to an add buffer.
- **Coalescing is mandatory**: adjacent inserts in the same region merge into one
  piece. Without it, typing creates unbounded pieces and `editor_lines` degrades
  to O(k).
- The base is an mmap (or a sliding window over one). Edits are metadata.

### 5.3 Chunk-relative line index

- A balanced tree of chunks; each chunk stores **line-start offsets relative to
  its own start** plus a line count.
- An edit touches O(log n) chunks and shifts nothing in untouched chunks.
- **Never invalidate the whole index on edit.** v1 did, making every keystroke
  O(file) on a 10M-line file.
- Built lazily; cached; only touched chunks are resident.

### 5.4 Undo/redo (specified)

- Undo = apply the **inverse edit** (delete inserted range, re-insert deleted
  text). v1's "move a cursor" cannot do this.
- **Coalesce** typing runs into one step (e.g. 500 ms or word boundary).
- **Cap** the undo stack (e.g. 10k steps) and the edit log (e.g. 8 MB); evict
  oldest.
- **Redo** is cleared on any new edit.
- **Compaction** at save: flatten the piece table, drop undo history older than
  the save point (or keep a bounded tail).
- Undo across save: define as "undo to last save" by default; full session undo
  is opt-in.

### 5.5 Save path (incremental, not O(file) rewrite)

- **No-op if clean.**
- **CoW-clone** the original as the temp file where the FS supports it
  (`clonefile` on APFS, `FICLONE` on XFS/Btrfs, `FSCTL_DUPLICATE_EXTENTS_TO_FILE`
  on ReFS), then `pwrite` only changed spans.
- **Fallback**: `copy_file_range` for the unchanged prefix, `pwrite` changed
  spans.
- **Metadata**: copy mode from `fstat`; chown only if privileged; preserve
  xattrs/ACLs. Note atomic rename breaks hardlinks → offer `--in-place`.
- **Order**: `fsync(temp)` → `rename` → `fsync(dir)`; on macOS use `F_FULLFSYNC`
  for the file. Close all views before rename on Windows; retry sharing
  violations.
- **Windows**: `MoveFileEx` with `MOVEFILE_REPLACE_EXISTING` + retry.
- **Benchmark gate (M0)**: "edit 1 line in a 1 GB file, p99" must be
  < 100 ms with CoW, or < 1.5 s with full rewrite; document the number.

### 5.6 External mutation (mmap safety)

- Record `(dev, ino, size, mtime_ns)` at open.
- **Rename-based replacement** (git, most tools) is safe: the old inode stays
  mapped.
- **In-place truncate** causes `SIGBUS` (Linux/macOS) or
  `EXCEPTION_IN_PAGE_ERROR` (Windows) on next page touch. On mismatch: re-map and
  rewrite pieces (O(pieces)) or prompt to reload.
- Add `editor_reload`; detect changes via the recorded identity + file watching
  (FSEvents/inotify/`ReadDirectoryChangesW`), coalesced.

### 5.7 mmap policy (not "one mapping for everything")

- **Sliding-window mapping** (256 MB–1 GB) for large files: bounds virtual
  address space and avoids a 100 GB `MapViewOfFile`.
- **`pread`** for network filesystems (NFS/SMB coherence, SIGBUS risk) and as a
  fallback.
- **Buffered `read()` under ~64 KB**: small files are faster without mmap (syscall
  + VMA + page fault vs. one `read`).
- **No `MAP_PRIVATE` writes for large files**: dirty pages have no file backing,
  cannot be evicted, and count against footprint. Use an explicit append log in
  RAM, flushed to a temp file.
- **Scans**: chunked `madvise(MADV_SEQUENTIAL)` + `MADV_DONTNEED`, or `pread`
  into a sliding buffer. Never mmap-the-world for search/index: a measured 2 GB
  scan pulled 1.68 GB into RSS.

---

## 6. CLI & agent contract

Binary: `editor`. The GUI is a separate app (`Editor.app` / `editor-gui`) linking
the same core. Both resolve through the same domain ops.

### 6.1 Command surface

```
editor read    <path> [--from N | --from-offset B] [--to N | --length L]
                     [--around N --radius K] [--max-bytes N] [--out FILE] [--json]
editor write   <path> (--stdin | @FILE) [--expect REV] [--force] [--append]
editor edit    <path> (--old TEXT --new TEXT [--all | --count N]
                     | --at LINE[:COL] --text TEXT)
                     [--op insert|replace|delete] [--expect REV] [--dry-run]
editor diff    <path> | <dir> [--from REV] [--to REV] [--context N]
editor apply   [--patch FILE | --stdin] [--root DIR] [--check] [--best-effort]
editor search  <path|dir> --pattern RE [--literal] [--glob G] [--context N]
                     [--max-results N] [--stream]
editor stats   <path>                 # stat-only: no mmap, no hash
editor move    <src> <dst> [--expect REV]
editor restore <path> [--to REV | --to HEAD]
editor open    <path>                 # hand off to GUI; returns session id
editor batch   [--file F | --stdin]   # JSON-lines op program, one transaction
editor info    [--version] [--schema [COMMAND]]
```

**Cut from v1:** `goto` (folded into `read --around`), `insert`/`replace`/`delete`
as separate verbs (one `edit --op`), `write --content` (quoting/ARG_MAX/NUL),
TTY-sniffed output.

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
      "path": "src/main.rs",
      "rev_before": "sha256:9f2c1b7a...",
      "rev": "sha256:41d0e8c3...",
      "bytes_before": 4096,
      "bytes": 4112,
      "lines": 142,
      "sha256_before": "9f2c...",
      "sha256": "41d0...",
      "line_endings": "lf",
      "encoding": "utf-8",
      "bom": false,
      "final_newline": true,
      "changed": { "from": 4210, "to": 4288 }
    }
  ],
  "meta": { "editor_version": "0.1.0", "pid": 48211 }
}
```

Rules:

- **One shape for every command.** Single-file commands return a 1-element
  `results[]`; `apply`/`batch` return N. Agent code never special-cases
  cardinality.
- **`rev` is opaque and content/stat-derived**, never parsed:
  `"sha256:<16 hex>"`, or `"stat:<ino>:<mtime_ns>:<size>"` above a size
  threshold. "Monotonic" is unimplementable across processes and cannot detect
  edits by other tools.
- **`sha256` is nullable** with a reason: hashing 1 GB costs ~0.5–1 s and would
  blow the open budget. `stats` is stat-only by contract.
- **Always JSON on stdout.** Stderr is logs only (`--verbose`). No TTY sniffing —
  it makes the byte stream depend on whether a pty is attached.
- **Exit codes:** `0` ok · `2` domain error · `3` usage · `4` internal. `ok:false`
  always implies nonzero.
- **Errors carry machine codes**, not prose:
  `E_CONFLICT`, `E_NO_MATCH`, `E_AMBIGUOUS`, `E_INVALID_UTF8`, `E_BINARY`,
  `E_OUTSIDE_ROOT`, `E_PARSE`, `E_REGEX_UNSUPPORTED`, `E_TOO_LARGE`, `E_LOCKED`,
  `E_IO`, `E_OOM`. Include `retryable: bool` and a `hint`.
- **Large outputs:** `read` returns inline content only up to `--max-bytes`
  (default 256 KiB); beyond that require `--out FILE` or return a `content_ref`
  (path + offset + length + sha256) so the caller mmaps it. `search` streams
  NDJSON with a terminal summary record (`"stream": true`).
- **Determinism:** sort results by (path bytes, offset); fixed field order; no
  HashMap iteration; no timestamps by default; golden tests normalize
  `duration_us`/`pid`/abs paths.
- **Versioning:** `"editor.result/1"`. Additive-only within a major.
  `--schema 1` requests a version; unknown major → exit 4, `E_SCHEMA`.
  `editor info --schema` dumps command schemas so agents can adapt.

Error example:

```json
{
  "schema": "editor.result/1", "command": "edit", "ok": false, "duration_us": 900,
  "error": {
    "code": "E_CONFLICT",
    "message": "file changed since read",
    "retryable": true,
    "path": "src/main.rs",
    "expected_rev": "sha256:9f2c1b7a",
    "actual_rev": "sha256:41d0e8c3",
    "hint": "re-read, then retry with --expect sha256:41d0e8c3"
  }
}
```

### 6.3 One-shot + batch (no daemon for now)

- **One-shot** for reads, search, stats, and single surgical edits. Process spawn
  is ~1–2 ms for a static Rust binary — not the thing to optimize.
- **`editor batch`** is the answer to atomicity: one process, a JSON-lines
  program on stdin/file, streaming responses, **one implicit transaction** with
  rollback. It gets ~80% of a daemon's benefit at ~5% of the cost (no socket, no
  lifecycle, no auth, no zombie daemons, no version skew).
- **`serve` is deferred.** When built, it is a transport over the GUI's
  `DocumentSession` (buffers + undo + change ranges), not a new subsystem.

Batch program:

```json
{"op":"edit","path":"a.rs","old":"let x = 1;","new":"let x = 2;","expect":"sha256:9f2c"}
{"op":"edit","path":"b.rs","old":"foo()","new":"bar()","all":true}
{"op":"diff","path":"a.rs"}
```

### 6.4 Concurrency & correctness

- **`--expect <rev>` is mandatory on existing files**; `--force` overrides.
  Mismatch → `E_CONFLICT` with expected/actual, exit 2. Two agents editing
  simultaneously is safe by construction: one wins, one re-reads and retries.
- **Close the TOCTOU**: re-stat immediately before rename and compare
  `(ino, mtime_ns, ctime_ns, size)`.
- **Short-lived lock file** (`<path>.editor.lock`, `O_CREAT|O_EXCL`, 0600, pid +
  mtime inside) held only for the write critical section; stale-lock detection via
  pid liveness. Prevents two agents both doing an O(n) write.
- **Symlinks**: resolve then operate on the target; never rename over the link
  itself. Default follow; require `--allow-outside` to write outside `--root`.
  Use `O_NOFOLLOW` on lock/temp creation.
- **Refuse non-regular files** (FIFOs, devices, dirs); enforce `--max-file-bytes`
  unless `--allow-huge`.
- **GUI**: re-check on-disk `rev` before save and surface a conflict instead of
  clobbering. The "stateless" claim is about the CLI, not the GUI.

### 6.5 Security

- **Scripts are arbitrary code execution by design.** Require `--yes` or a
  per-project trust prompt; never invoke from GUI, `apply`, or config. Scripts
  live in `.editor/scripts/` and are allowlisted.
- **Config is data only** — no `exec`, no shell interpolation. A malicious repo
  shipping `.editorrc` must not execute.
- **Regex DoS**: step budget + timeout; forbid backreferences/lookaround; return
  `E_REGEX_UNSUPPORTED`; literal is the default fast path; publish the dialect.
- **Untrusted patches**: parse into a typed structure with hard caps (max files,
  hunks, line length, total bytes); reject absolute paths and `..`; resolve
  against `--root`; re-validate after resolution; handle
  `\ No newline at end of file` and `/dev/null`; fuzz it.
- **Secret leakage**: argv is visible in `ps`. Support `--text @file` and
  `--stdin`; document that `--text` leaks. Error messages must never echo file
  contents or matched lines.
- **DoS defaults**: `--max-bytes`, `--max-results`, search timeout, tree-walk depth
  cap, symlink-cycle detection via visited inodes.
- **`serve` (when built)**: Unix socket 0600, never TCP by default; per-session
  token; request timeouts; no shell-out over RPC.

---

## 7. Resource budgets (measured where possible)

Measured on macOS 27.2.1, Apple Silicon, 8 GB:

| Fact | Measurement |
| --- | --- |
| Bare C exe RSS / footprint | 6.4 MB / 1.6 MB |
| + `NSApplication` | 28.3 MB / 5.3 MB |
| + `NSWindow` | 39.9 MB / 10.2 MB |
| + window shown, one draw | 40.6 MB / 10.4 MB |
| `NSTextView` with 100k lines | 29.6 MB footprint |
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

Budgets:

| Metric | Budget |
| --- | --- |
| Footprint baseline (empty window) | **≤ 12 MB** |
| + line-layout cache (1000–2000 lines) | **≤ 6 MB** |
| + undo/edit log | **≤ 8 MB cap** |
| + content mmap LRU (search) | **≤ 64 MB** |
| Idle CPU | **< 0.05%** (caret blink only) |
| Scroll, cached | **< 3 ms/frame**, < 2% of one core |
| Scroll, cold lines | **< 8 ms/frame**, < 5% of one core |
| Edit to model / to frame | **< 1 ms / < 3 ms** |
| Open 1 GB read-only | **< 100 ms to first paint** |
| Scan 1 GB | **250–500 ms** |
| Startup warm / cold | **< 150 ms / < 400 ms** |

Measurement methodology: use `phys_footprint` (not RSS); `CLOCK_MONOTONIC` around
frames; `powermetrics`/`os_signpost` for CPU attribution; assert **p99 frame
budget** in CI, not average. Add a synthetic 5M-line file to the bench suite.

---

## 8. Rendering

### 8.1 Hybrid view (the key change)

- **Files below threshold (~2 MB and ~20k lines): use the OS native text control**
  (`NSTextView`, RichEdit, or the platform equivalent). It gives IME, marked
  text, accessibility, RTL, grapheme clusters, selection, drag-and-drop,
  spellcheck, and 20 years of edge cases **for free and correctly**.
- **Above threshold: custom view** driven by `viewcore` + `textbackend`, with a
  viewport-only line cache.
- Both implement one `TextView` interface; **commands go through the same core**.
  Only the drawing path swaps. This avoids rebuilding IME/a11y for the common case
  while keeping huge files viable.
- Threshold is configurable. Above it, the custom view must implement IME and
  basic a11y incrementally; declare the scope explicitly rather than claiming
  parity on day one.

### 8.2 Line-layout cache

- Key: **(logical line, wrap width)**. One `CTLine`/DirectWrite layout per *visual*
  line; wrapping can turn 1 logical line into N visual lines.
- **Bounded LRU, cap 1000–2000 entries.** At 3.1 KB/line, 10k entries = 31 MB,
  which silently blows the budget. 1000–2000 = 3–6 MB.
- Invalidate on edit of a line in range; invalidate all on width/font change.
- Cold re-shape is 0.43 ms/frame — invisible at 60 fps.

### 8.3 Startup

- **Do not enumerate fonts at launch.** `availableFontFamilies` = 80.8 ms and
  +4.8 MB; resolving one font (`userFixedPitchFontOfSize`) = 1.6 ms. Enumerate
  lazily only when the font menu opens.
- Don't use Swift (libswiftCore overhead); ObjC++ + Rust is leaner.
- Binary size is irrelevant (AppKit-linked exe = 51 KB); optimize memory and
  latency, not size.

### 8.4 Text shaping

- `textbackend` is the only place that shapes glyphs:
  **CoreText** (macOS), **DirectWrite** (Windows), **Xft + HarfBuzz**
  (Linux). Xft alone has no complex shaping; HarfBuzz closes the Linux parity gap
  and is a system library on every desktop Linux.
- Monospace default. If identical cross-platform metrics are required, bundle one
  monospace font (+200–800 KB) and say so.

---

## 9. Search & many files

Three tiers:

1. **In-memory tree** (dirs/files, path, size, mtime, inode): ~150 B/entry;
   10k files ≈ 1.5 MB. Build lazily per directory; cache to
   `~/.cache/editor/tree-<hash>` keyed by mtime.
2. **Content, mmap'd on demand**, not held. Search reads each file (I/O bound:
   100 MB ≈ 50–100 ms on NVMe), then `madvise`s it away.
3. **No inverted index in the MVP.** But keep a **bounded content-mmap LRU**
   (50–100 MB) so repeated agent searches don't re-hit disk: 100 × 100 ms = 10 s
   vs. ~1 s cached.

- **Chunked scans** with `madvise`/`pread`; never mmap-the-world.
- **Bounded parallelism**: each worker holds a small mmap window; cap workers
  (e.g. 4–8) rather than one thread per core each mapping freely.
- Regex: literal + glob + a documented subset (no backreferences/lookaround), or a
  small engine. Decide before M6; a hand-rolled full regex is 2–5k LOC and a
  correctness liability.

---

## 10. Testing & CI

- **C ABI conformance client** compiled with `cc`, no Rust knowledge — the real
  contract test.
- **Golden CLI tests**: exact JSON for every command, including errors.
- **Property tests**: edit/insert/delete/undo invariants; piece-table round-trips;
  save/restore identity.
- **Fuzzing**: argv parser, JSON reader/writer, line index, patch parser.
- **Benchmarks**: cold start, open 1 GB, edit viewport, scan 1 GB, 5M-line file,
  10k-file tree; memory-ceiling assertions on footprint.
- **CI**: macOS / Windows / Linux; fmt, clippy, test, bench; footprint gate;
  p99 frame budget; platform build smoke tests.

---

## 11. Build & distribution

- Rust core: `cargo build --release` → `libcore.a` / `core.lib` / `core.dll` +
  `editor` CLI.
- macOS: Xcode project (or Makefile) compiles the ObjC++ app, links the core,
  bundles `Editor.app`; notarization required (Developer account, hardened
  runtime, `notarytool`, staple).
- Windows: MSVC project links `core.lib`; match CRT (`/MT` vs `/MD`); SmartScreen
  reputation via EV cert or downloads.
- Linux: Makefile/CMake links `libcore.a`; `.deb`/`.rpm`/AppImage/Flatpak.
- No vendored third-party; only OS system libraries and the toolchain.

---

## 12. Roadmap (solid, demoable increments)

- **M0 — Core + CLI.** Piece table + coalescing, chunk index, mmap policy,
  incremental save, undo, domain ops, `read`/`write`/`edit`/`diff`/`apply`/
  `search`/`stats`/`move`/`restore`/`batch`/`info`, JSON envelope, security
  guardrails, tests, benchmarks. **Gate: edit 1 line in 1 GB < 100 ms (CoW).**
- **M0.5 — Freeze the ABI + headless render oracle.** C conformance client;
  viewport-read/layout harness with no UI; **5 GB read-only demo** scrolling at
  60 fps with flat footprint; `editor stats` JSON.
- **M1 — macOS read-only window.** AppKit + `viewcore` + CoreText backend:
  menu, open dialog, custom text view, scrolling. *First native app.*
- **M1.5 — Windows read-only spike.** DirectWrite + D2D implementing the same
  `TextView`; validates the ABI against the least forgiving stack. Port, don't
  redesign.
- **M2 — macOS editing.** Cursor, keyboard, insert/delete, undo, save, dirty
  indicator. *First usable native editor.*
- **M3 — macOS polish.** Selection, clipboard, find, go-to-line, status bar, line
  numbers, window title, native-control path for small files.
- **M4 — Windows full.** Menus, dialogs, IME, a11y basics, packaging.
- **M5 — Linux.** X11 + Xft + HarfBuzz, XIM, AT-SPI basics, packaging.
- **M6 — Search.** File + tree, GUI + CLI, ignore rules, content-mmap LRU.
- **M7 — Syntax highlighting.** Lazy, data-driven, per-language tables.
- **M8 — Markdown preview.**
- **M9 — Git** (via git CLI).
- **M10 — Script runner.**
- **M11 — `editor serve`** (only if batch proves insufficient).

Explicitly out of scope: plugins/mods, network, telemetry, TUI, inverted index
(until proven necessary).

---

## 13. Open decisions

1. **Hybrid threshold**: exact MB/line count where the view switches.
2. **Config format**: built-in key=value vs. a small embedded format (no serde).
3. **Regex engine**: literal + documented subset vs. a small vendored engine.
4. **Linux text stack**: confirm HarfBuzz as an allowed system library.
5. **Accessibility scope**: keyboard-complete + basic VoiceOver/Narrator, or full
   parity (recommend the former for the MVP).
6. **CLI zero-dependency purity**: hand-rolled JSON + SHA-256 (fuzzed) vs. a
   2-crate allowlist in `cli` only. Default: zero-dep.
7. **Bundled font**: accept +200–800 KB for identical cross-platform metrics, or
   rely on OS fonts and accept slight rendering differences.

---

## 14. What I would build first

1. **Freeze `platform/shared/editor.h`** (the draft above) and write the one-page
   data-model contract (§5). These are cheap now, expensive at M2.
2. **M0 core + CLI**, with the save-path benchmark as a hard gate.
3. **M0.5 headless render oracle + C client**, proving the ABI and the 5 GB
   read-only resource story before any native window exists.

Everything else is downstream of those three.