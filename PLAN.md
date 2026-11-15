# editor — Plan (v6)

> Name: **editor**. Status: planning. v6 — trimmed for clarity.
> A native macOS text editor for **minimum memory, minimum CPU, maximum speed**,
> with a **CLI-first** surface so AI agents can drive it as reliably as a human.
> One C++ core, one custom-drawn viewcore, AppKit chrome.

---

## 1. Decisions

| ID | Decision | Revisit |
| --- | --- | --- |
| D1 | **C++ all-in** (core + viewcore + platform + CLI); ObjC++/AppKit on macOS. | M0.5 kill gate: C++ bug density high despite sanitizers/fuzzing. |
| D2 | **Custom-drawn view only.** No native-control hybrid. | Post-1.0, only if custom view misses perf. |
| D3 | **macOS only for v1.** Windows/Linux post-1.0. | Market or schedule. |
| D4 | **Bytes are the currency.** Line = byte range; `col` = UTF-8 byte offset. | Never. |
| D5 | **One-shot CLI + `batch`.** `serve` deferred. | Profiling shows repeated open cost dominates. |
| D6 | **CoW save**: same-length O(changed), length-changing O(tail). | A journal/extent-map proves simpler. |
| D7 | **Sparse line index**: 64 lines/chunk, 8-byte offsets, in-chunk scan. | 25M-line memory gate fails. |
| D8 | **Stat-only `rev`.** No SHA-256 in v1. | Cross-platform collisions; then add hashing. |
| D9 | **No config file, no regex, no IME, no accessibility.** | If needed; post-1.0. |
| D10 | **UTF-8 only.** Detect UTF-16/BOM and fail clearly. | UTF-16 becomes a blocker. |
| D11 | **No plugins/mods.** Scripts post-1.0, trust-gated. | Never for v1. |
| D12 | **MVP = macOS M0–M3.5.** | Schedule or market. |

Rationale for D1 is in the [appendix](#appendix-language-choice).

## 2. Goals and non-goals

**Goals**

- Open/edit files from a few bytes to many gigabytes without loading the whole
  file into memory.
- Stay responsive with thousands of files.
- Near-zero idle CPU; small, predictable, **measured** memory.
- Genuine native macOS app: real window, native chrome, fonts, dialogs.
- One shared core (C++) designed for all platforms; macOS backend in v1.
- A CLI that exposes every feature, emits machine-readable output, and shares
  domain ops with the GUI.
- No vendored/bundled third-party code; OS system libraries allowed.

**Non-goals (v1)**

- No Windows/Linux, no TUI, no config file, no regex, no SHA-256, no IME, no a11y.
- No plugins/mods, network sync, accounts, telemetry.
- No native-control hybrid.
- No multi-cursor, command palette, split panes, or GUI project tree.
- No Markdown rendered editing, git UI, script runner, or `serve` (post-1.0).
- Not an IDE: no debugger, language servers, or build integration.
- No inverted search index (mmap + metadata cache + bounded content cache).

## 3. Architecture

```
   CLI (C++) ─────────►  core (C++)                 document · edits · undo
   platform/macos ────►  core                       sparse line index · save
                         │ core/include/editor.hpp
                         ▼
                        viewcore (C++)             wrap · cache · hit-test
                                                     selection · scroll · caret
                                                     decorations
                         │ textbackend + Canvas
                         ▼
                        CoreText (macOS)           [DirectWrite / Xft+HarfBuzz post-1.0]
```

**Layers**

- **`core` (C++)** — document model, piece table, sparse line index, undo, save,
  domain ops, `workspace`, `DocumentSession`. No UI, no exit codes. OS access is
  injected via `FileSystem`/`FileWatcher`; no `#ifdef` in core.
- **`viewcore` (C++)** — platform-independent view logic: wrapping, line cache,
  hit-testing, selection, scroll, caret, decorations. One implementation, one
  conformance suite.
- **`textbackend` (per OS)** — the only place that shapes/draws glyphs: `measure`,
  `shape` into runs, `draw` into a `Canvas`. v1: CoreText.
- **`platform/macos`** — window, menus, dialogs, keyboard/mouse, clipboard,
  `FileSystem`, `FileWatcher`, `Canvas`. No editing logic.
- **`workspace`** — directory tree cache, ignore rules, content mmap LRU,
  bounded parallel scan.
- **`DocumentSession`** — open documents + undo + change ranges. CLI and GUI both
  consume it; `serve` (post-1.0) is a transport over it.
- **`cli` (C++)** — argv → domain ops → persist → JSON envelope.

**Dependency rule:** `cli`, `platform/*`, `viewcore` → `core`; `core` → `std` +
injected OS interfaces. The frozen interface lives at
**`core/include/editor.hpp`**.

## 4. Core interface

Freeze a draft before M0; freeze formally after the C++ conformance test at M0.5.

```cpp
// core/include/editor.hpp — v1
#pragma once
#include <atomic>
#include <cstdint>
#include <span>
#include <string_view>

enum class Status : uint32_t {
  Ok = 0, Io, NotFound, Range, Conflict, Stale, Oom, Encoding, InvalidUtf8,
  Binary, OutsideRoot, Parse, TooLarge, Locked,
  Schema, Unsupported, Usage, Internal, TooSmall, SavedButNotDurable
};

enum class Eol : uint32_t { Lf, CrLf, Cr, Mixed };
enum class Encoding : uint32_t { Utf8, Utf16Le, Utf16Be, Other };
enum class EditKind : uint32_t { Insert, Delete, Replace };
enum class ChangeKind : uint32_t { Insert, Delete, Replace, Reload, Save };

struct Rev {                       // stat-only in v1
  uint64_t dev = 0, ino = 0, mtime_ns = 0, ctime_ns = 0, size = 0;
};

struct Info {
  std::string_view path;           // borrowed; valid until next call
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
  int64_t line = 0, col = 0;       // 0-based; col = byte offset
  int64_t removed_len = 0;         // 0 for insert
  std::string_view text;           // inserted/replacement bytes; borrowed
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

  // Edits relative to the ORIGINAL buffer, sorted, non-overlapping, all-or-nothing.
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

**Rules**

- Every fallible call returns `Status`; no `void` mutation/save.
- `(ptr, len)` / `string_view` / `span` everywhere; no NUL-terminated input.
- `int64_t` for lines/offsets; `0` is a valid line/length, never an error.
- Core is 0-based; CLI is 1-based. `col` is a UTF-8 byte offset.
- Core never calls the UI; it appends changes and the UI calls `drain()`.
- Exceptions must not escape; catch at every boundary → `Status::Internal`.
- One `Editor` per thread; parallel readers use `snapshot()`.
- `errmsg` is thread-local and borrowed; no retention.
- Version tag `kInterfaceVersion`; reject a mismatch at a shared-library boundary.
- Callbacks must not call back into the core (no re-entrancy).
- C++20, pinned toolchain; statically link core into GUI/CLI.
- Hardening: `_GLIBCXX_ASSERTIONS` / `_LIBCPP_HARDENING_MODE`, MSVC `/sdl`,
  ASan/UBSan, `-Wall -Wextra -Wconversion`.

## 5. Document model

### 5.1 Bytes, lines, EOL, encoding

- A **line** is a byte range `[start, end)` excluding its terminator.
- `line_count` = terminators + (1 if the last line is non-empty). Empty = 0;
  `a\n` = 1; `a` = 1; `a\n\n` = 2.
- `col` is a 0-based UTF-8 byte offset within the line (tabs = 1 byte).
- EOL is per-file (`lf`, `crlf`, `cr`, `mixed`); mixed endings are preserved.
  A CR at a chunk boundary + LF in the next chunk is **one** CRLF terminator.
- Encoding: UTF-8 strict. Invalid UTF-8 → `E_INVALID_UTF8` with the byte offset.
  Non-UTF-8 (UTF-16 BOM, Latin-1) → `E_UNSUPPORTED` with a clear diagnostic.
- BOM is metadata, not part of line 1. Validation is lazy per touched line; a
  `validate` command can scan the whole file.

### 5.2 Piece table and add buffer

- Original file is an immutable base piece; edits append to an add buffer.
- **Coalesce** adjacent inserts at the add-buffer tail whose predecessor ends
  exactly at the append offset. Without it, typing creates unbounded pieces.
- Add buffer is **capped at 8 MB and spilled to a temp file**; compacted before
  save. Non-tail inserts do not coalesce.
- "Edits are metadata" = piece descriptors + inserted bytes, not a document copy.

### 5.3 Sparse chunk line index

- Balanced tree of chunks; each covers **64 lines** and stores line-start offsets
  **relative to its own start** plus a line count.
- An edit touches O(log n) chunks; untouched chunks shift nothing.
- **Never invalidate the whole index on edit.**
- Memory: 8 B per 64 lines. 5M lines ≈ 0.6 MB; 25M ≈ 3 MB; 125M ≈ 16 MB.
- Chunk split/merge is specified and property-tested; worst case is O(chunk).

### 5.4 Undo/redo

- Undo = apply the **inverse edit** (delete inserted range, re-insert deleted text).
- Coalesce typing runs (500 ms or word boundary); **programmatic edits do not
  coalesce** (one edit = one undo step).
- Cap: 10k steps or 8 MB. Larger inverse text spills to a temp file; if that is
  impossible, undo is disabled for that edit and reported.
- Redo is cleared on any new edit.
- At save: flatten the piece table, drop undo older than the save, clear the add
  buffer. "Undo to last save" is the default; full session undo is opt-in.

### 5.5 Save path (honest cost model)

v1 targets macOS/APFS; Windows/Linux fallbacks are post-1.0.

- **No-op if clean.**
- **Same-length edits**: CoW-clone the original (O(1) on APFS; O(file) elsewhere),
  `pwrite` changed spans, then fsync → rename → fsync.
- **Length-changing edits**: the tail after the edit shifts. If the tail is ≤ 25%
  of the file, `pwrite` the tail; otherwise full rewrite. Report the method.
- **Clone the original fd/inode**, not the path. If the path identity changed
  since open, fail `E_CONFLICT`; never mix old offsets with new bytes.
- Fallback: `copy_file_range` for unchanged ranges; handle `EXDEV`/overlap with
  read/write. Never copy only a prefix and leave the suffix unwritten.
- Temp: same directory, `O_CREAT|O_EXCL`, 0600, cleanup on failure. Rename needs
  directory write permission; read-only dir → `E_IO` or `--in-place`.
- Metadata: `ReplaceFile` (Windows); `CLONE_ACL` + xattr/ACL copy (macOS); xattr/
  ACL copy (Linux). `chown` before `chmod`. Hardlink caveat → `--in-place`.
- Durability: `fsync(temp)` → `rename` → `fsync(dir)`; macOS `F_FULLFSYNC` for
  power-loss durability (or `fsync` for crash-only). Post-rename `fsync` failure
  → `SavedButNotDurable` warning, `ok: true`; **never blind-retry**.
- **Post-save transition (atomic)**: new base = saved bytes, clear add buffer,
  drop old undo, update identity/mtime/rev. Double-save conformance test.
- **Gate**: same-length edit in 1 GB on APFS < 100 ms p99; length-changing edit
  measured separately.

### 5.6 External mutation and mmap

- Record `(dev, ino, size, mtime_ns, ctime_ns)` at open.
- Rename-based replacement is safe (old inode stays mapped). In-place truncate
  causes `SIGBUS`/`EXCEPTION_IN_PAGE_ERROR`; handle with a signal handler +
  `sigsetjmp`/`siglongjmp` around risky accesses, or `pread` for at-risk files.
- On mismatch: auto-reload if clean; if dirty, `E_STALE` with a reload hint.
  `reload()` discards undo, maps new content, maps the caret to the nearest valid
  position.
- mmap policy: **sliding window** (256 MB–1 GB) for large files; `pread` for
  network FS; buffered `read()` under ~64 KB; **no `MAP_PRIVATE` writes** for
  large files (use the capped add buffer); base mapping is read-only; scans use
  chunked `posix_fadvise`/`madvise` + `pread`, never mmap-the-world.

## 6. CLI

Binary: `editor`. The GUI is `Editor.app`, linking the same core.

### 6.1 Commands

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

# post-1.0:
editor diff <path|dir> [--from REV] [--to REV] [--context N]
editor restore <path> [--to REV | --to HEAD]
editor apply [--patch FILE | --stdin] [--root DIR] [--check] [--best-effort]
editor open <path>
editor search <path|dir> --pattern TEXT [--glob G] [--context N]
                    [--max-results N] [--stream]   # literal + glob; no regex
```

**Global flags:** `--root DIR` (default cwd), `--allow-outside`,
`--allow-huge` (override `--max-file-bytes`, default 2 GB), `--verbose`.
`--json` is removed — output is always JSON.

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
      "rev_before": {"dev": 1, "ino": 2, "mtime_ns": 3, "ctime_ns": 4, "size": 4096},
      "rev": {"dev": 1, "ino": 2, "mtime_ns": 5, "ctime_ns": 6, "size": 4112},
      "bytes_before": 4096,
      "bytes": 4112,
      "line_count": 142,
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

- One shape for every command; single-file commands return a 1-element `results[]`.
- `rev` is stat-only in v1; always present. `--expect` accepts
  `stat:<dev>:<ino>:<mtime_ns>:<ctime_ns>:<size>`; local/advisory; `--force`.
- `changed.from`/`to` are byte offsets; `line_count` is logical lines.
- Deterministic: sort by (path bytes, offset); fixed field order; no
  `unordered_map` iteration; no timestamps by default; golden tests normalize.
- Version: `editor.result/1`; additive within a major; `--schema 1`; unknown
  major → exit 3, `E_SCHEMA`.
- Large outputs: `read` inline up to `--max-bytes` (default 256 KiB), else
  `--out` or `content_ref`; `search --stream` is NDJSON.

### 6.3 Errors and exit codes

`Status` maps 1:1 to `E_*`; errors carry `code`, `message`, `retryable`, `path`,
`hint`, and command-specific fields. `E_INVALID_UTF8` includes the byte offset;
`E_NO_MATCH`/`E_AMBIGUOUS` include a bounded list of `{line, col, byte_offset}`.

| Exit | Meaning | Examples |
| --- | --- | --- |
| 0 | ok | `Ok`, `SavedButNotDurable` (warning) |
| 2 | domain | `E_IO`, `E_NOT_FOUND`, `E_RANGE`, `E_CONFLICT`, `E_STALE`, `E_ENCODING`, `E_INVALID_UTF8`, `E_BINARY`, `E_OUTSIDE_ROOT`, `E_PARSE`, `E_TOO_LARGE`, `E_LOCKED`, `E_UNSUPPORTED`, `E_TOO_SMALL` |
| 3 | usage/schema | `E_USAGE`, `E_SCHEMA` |
| 4 | internal | `E_OOM`, `E_INTERNAL` |

### 6.4 Batch and concurrency

- `batch` is a JSON-lines program with **validation-stage atomicity**:
  1. Parse/validate every op against the original files; any failure → no writes.
  2. Stage each file write to a temp; commit (rename) each.
  3. On commit failure, roll back applied writes from backups; if rollback fails,
     return `ok: false`, `rolled_back: false`, and the applied ops.
  4. Buffer responses until commit. **Not crash-atomic**; `--check` validates only.
  5. Batch ops see the original files; two ops on the same file must not overlap.
- Mutations require `--expect <rev>` on existing files; `--force` overrides.
  Mismatch → `E_CONFLICT`, exit 2.
- The expect-check, write, and rename happen inside a short-lived lock
  (`<path>.editor.lock`, `O_CREAT|O_EXCL`, 0600, random token + pid + start time;
  stale detection by pid liveness + token).
- Symlinks: resolve, re-check inside `--root`, `O_NOFOLLOW`/`openat2` on the
  final component; never rename over a symlink.
- Refuse non-regular files; enforce `--max-file-bytes` unless `--allow-huge`.
- **GUI**: re-check the on-disk `rev` before save; surface a conflict instead of
  clobbering.

### 6.5 Security

- **Scripts** (post-1.0): arbitrary code execution by design; trust prompt /
  `--yes`; never from GUI, `apply`, or config; allowlist in `.editor/scripts/`.
- **No config file in v1.** If added post-1.0: data only, versioned, no `exec`.
- **Patches**: typed AST, hard caps (files, hunks, line length, bytes); reject
  absolute paths and `..`; resolve against `--root`; handle
  `\ No newline at end of file` and `/dev/null`; fuzz.
- **Secret leakage**: argv is visible in `ps`; prefer `--text @file`/`--stdin`;
  never echo file contents or matched lines in errors.
- **DoS defaults**: `--max-bytes`, `--max-results`, search timeout, tree-walk
  depth cap, symlink-cycle detection.
- **`serve` (post-1.0)**: Unix socket 0600, per-session token, timeouts.

### 6.6 Registry invariant

Registry entry = `{ name, arg_schema, core_fn, cli_binding, gui_binding? }`.

- A `core_fn` with no `cli_binding` is a test failure (except an explicit
  `cli_excluded` allowlist).
- A mutating `gui_binding` with no undo entry is a test failure.
- `editor info --schema` is generated from the registry.

## 7. Budgets and measurement

**Metric:** macOS `phys_footprint`; Windows private working set; Linux PSS
(post-1.0). Not directly comparable; compare the same workload.

**Measured (macOS, Apple Silicon, 8 GB)**

| Fact | Measurement |
| --- | --- |
| Bare C exe RSS / footprint | 6.4 MB / 1.6 MB |
| + `NSApplication` | 28.3 MB / 5.3 MB |
| + `NSWindow` | 39.9 MB / 10.2 MB |
| + window shown, one draw | 40.6 MB / 10.4 MB |
| mmap 2 GB untouched | +0 MB, 0.010 ms |
| mmap 2 GB full scan | **1.68 GB RSS** |
| `CTLineCreate` cold line | 9.2 µs |
| Cached CoreText line | ~3.1 KB each |
| 40 cached line draws × 60 | 0.26 ms/frame |
| Cold shape+draw | 0.43 ms/frame |
| Font resolve (one) | 1.6 ms |
| Font enumeration (175) | 80.8 ms |
| Time to first window | 60–390 ms |

**Budgets (caps, not additive)**

| Metric | Cap |
| --- | --- |
| Empty-window baseline | ≤ 12 MB |
| + line cache (1000–2000 entries, long lines skipped) | ≤ 6 MB |
| + undo/edit log (spilled) | ≤ 8 MB |
| + add buffer (spilled) | ≤ 8 MB |
| + sparse line index (5M lines) | ≤ 1 MB |
| + content mmap LRU (search) | ≤ 60 MB |
| **Loaded (1 GB file, cache + undo + index)** | **≤ 40 MB** |
| **With search LRU** | **≤ 96 MB** |

**Performance**

| Operation | Budget |
| --- | --- |
| Idle CPU | < 0.05% (caret blink only) |
| Scroll, cached | ≤ 3 ms/frame (18% of one core at 60 fps) |
| Scroll, cold | ≤ 8 ms/frame (48%) |
| Edit to model / frame | < 1 ms / < 3 ms |
| Open 1 GB read-only | < 100 ms to first paint |
| Same-length edit in 1 GB (APFS) | < 100 ms p99 |
| Length-changing edit in 1 GB | O(tail), measured separately |
| Scan 1 GB | 250–500 ms |
| Startup warm / cold | < 150 ms / < 400 ms |

**Method:** `CLOCK_MONOTONIC` around frames; `powermetrics`/`os_signpost` for CPU;
assert **p99**, not average. Measure `F_FULLFSYNC` (power-loss) vs `fsync`
(crash-only) separately. Bench 5M/25M-line, 5 GB, 10k-file, 100 MB single line.
Separate warm (index built) from cold (first random edit).

## 8. Rendering

- **Custom view only** on macOS in v1, driven by `viewcore` + CoreText. No
  native-control hybrid; native control is a post-1.0 experiment.
- The view owns selection, caret, scroll; the core owns bytes and edits.
- **No IME and no accessibility in v1** — keyboard + mouse only. Both are
  significant platform work and are not needed for a personal editor.
- **viewcore API** (sketch): `set_document`, `resize`, `set_wrap`, `set_font`,
  `set_decorations`, `selection`/`set_selection`, `scroll_to`, `draw`, `key`,
  `mouse`. **textbackend**: `measure`, `shape` into runs, `draw` into a platform
  `Canvas`.
- Line cache keyed by **(logical line, wrap width)**; one shaped run per *visual*
  line. **Bounded LRU, 1000–2000 entries and a total byte cap**; skip caching lines
  above ~1000 visual lines (a 100 MB single line must not blow the cache).
- Invalidate at/after the first changed line; invalidate all on width/font change.
- **Do not enumerate fonts at launch** (80.8 ms); resolve one (1.6 ms). Enumerate
  lazily when the font menu opens.
- Monospace default; OS fonts (no bundled font in v1).
- Windows: DirectWrite + D2D. Linux: X11/Xft + HarfBuzz, XWayland first.
  Both post-1.0.

## 9. Workspace, search, and future

- **Tree cache**: path, size, mtime, ino, mode; validate per-entry stat (not dir
  mtime alone); 10k files ≈ 1.5–3 MB; cache `~/.cache/editor/tree-<hash>`.
- **Content** mmap'd on demand; **no inverted index** in v1; bounded content-mmap
  LRU (50–60 MB) for repeated searches.
- **Bounded parallelism**: 4–8 workers, each with a small mmap window and a
  `Snapshot`; bounded work queue.
- **No `search` command in v1.** Literal + glob search and project/tree search are
  post-1.0; agents use `grep`/`rg` for regex. Fuzzy/vector search deferred.
- **Workspace** module is designed now, built post-1.0.
- **Future seams** (designed now, built later):
  - Syntax: lazy per-visible-line tokenization; data-driven keyword/pattern
    tables; emitted as `Decoration` ranges.
  - Markdown: preview only; small built-in renderer.
  - Git: via `git` CLI; a status sidecar supplies gutter decorations.
  - Scripts: `editor run <script>`, trust-gated, streams output + JSON result.
  - `serve`: transport over `DocumentSession`; Unix socket 0600.

## 10. Testing, CI, build

**Testing**

- Unit: core, viewcore, workspace, CLI parsers.
- Property: edit/insert/delete/undo invariants; piece-table round-trips;
  save/restore identity; UTF-8 boundaries; chunk split/merge; line-count
  conventions; coalescing; CRLF/CR/mixed EOL; empty file; no trailing newline;
  huge line.
- Fuzz: argv, JSON reader/writer, patch parser (post-1.0), UTF-8, line index.
- Golden CLI: exact JSON for every command, including errors.
- Conformance: `Editor` (open/read/apply/undo/save/reload/rev/snapshot/cancel/
  change), `TextView` (wrap/hit-test/selection/scroll/decorations),
  `textbackend` (measure/shape/draw), `FileSystem` (map/pread/clone/replace/
  identity/lock).
- Sanitizers: ASan/UBSan every PR (lite); TSan nightly (limited on macOS);
  hardened STL in debug.
- Fault injection: `fsync`/`rename`/`pwrite` failures; crash-consistency of
  temp→rename; batch rollback; lock/TOCTOU two-process stress; external truncate.
- Benchmarks: cold start, open 1 GB, same-length edit, length-changing edit,
  viewport edit, scan 1 GB, 5M/25M-line, 5 GB, 10k-file, 100 MB single line;
  footprint gates.

**CI:** macOS smoke every PR; perf nightly; release engineering as M3.5;
cross-platform CI post-1.0.

**Build:** one CMake graph (Xcode generator on macOS); artifacts `libcore.a`,
`libviewcore.a`, `editor` CLI, `Editor.app`; C++20; notarization (hardened
runtime, `notarytool`, staple); universal arm64+x86_64. No vendored third-party.
Post-1.0: same CMake for Windows (static `/MT`) and Linux (X11/Xft + HarfBuzz,
`.deb`/`.rpm`/AppImage/Flatpak).

## 11. Roadmap

MVP = **macOS M0–M3.5**. Windows/Linux/search/syntax are post-1.0.

| Milestone | Scope | Est. | Gate |
| --- | --- | --- | --- |
| **Pre-M0.0** | Freeze `editor.hpp` + C++ conformance test + data-model contract. | 1 wk | Interface compiles; conformance skeleton runs. |
| **M0a** | Piece table + sparse index + `read`/`write`/`stats` + same-length CoW save + JSON. | 3–4 wk | 1 GB same-length edit < 100 ms p99 (APFS). |
| **M0b** | `edit` + undo + length-changing save + fault injection. | 3–4 wk | Edit/undo property tests; double-save and save-crash tests. |
| **M0c** | `batch` + `move` + `tree` + security + error model + registry invariant. | 2–3 wk | Batch validation/rollback; fuzz; CLI golden. |
| **M0.5** | Headless render oracle + viewcore skeleton + **5 GB read-only demo** + C++ validation. | 2 wk | 5 GB at 60 fps, flat footprint; sanitizers clean. |
| **M1** | macOS read-only custom view: AppKit + CoreText + viewcore; keyboard/mouse. | 4–6 wk | Open/scroll 5 GB. |
| **M2** | macOS editing + undo + save + find/replace. | 4–6 wk | Usable daily editor. |
| **M3** | macOS polish: status bar, gutter, wrap, decorations. | 3–4 wk | Feature checklist. |
| **M3.5** | Release engineering: notarization, packaging, CI perf gates. | 2–3 wk | Signed `.app`; CI green. |
| **Post-1.0** | Windows port, Linux port, project search, syntax, Markdown, git, scripts, `serve`, native-control experiment. | — | — |

**Total:** ~35–50 weeks, about **9–12 months solo**.

**Cut list if behind:** search/syntax, Markdown/git/scripts/serve (already
post-1.0); do **not** cut save correctness, fault injection, find/replace,
status bar/gutter.

## 12. Risks and open decisions

| Risk | Mitigation |
| --- | --- |
| Length-changing save is O(tail) | Honest cost model; tail/full-rewrite threshold; benchmark both. |
| Hand-rolled JSON bugs | Fuzz + property tests; allowlist fallback. |
| Schedule (~9–12 months) | macOS-only MVP; cut list; post-1.0 fence. |
| Temptation to re-add hybrid/native control | D2 decision + post-1.0 trigger. |
| mmap SIGBUS on truncate | Signal handler/pread; identity check; reload. |
| External mutation race | ctime-aware rev; lock covers check+write; auto-reload clean. |
| No IME/a11y limits CJK/screen-reader use | Accepted for v1; post-1.0 if needed. |

**Open decisions**

1. **macOS minimum version**: Apple Silicon only, or both arm64 + Intel?
   (Default: both; universal binary.)
2. **Bundled font**: OS fonts (default) vs. +200–800 KB for identical metrics.

## Appendix: language choice

Three independent final reviews compared Rust, C++, and C with the developer
fluent in both Rust and C++. **All three ranked C++ 1st, Rust 2nd, C 3rd**
(medium confidence; C++ 34–36/40, Rust 26–30/40, C 17–26/40).

**C++ wins because:**

1. The plan is already polyglot (C++ viewcore + ObjC++ platform). A C++ core
   subtracts a language and a foreign-ABI seam; a Rust core adds one.
2. The hardest code — CoW save, mmap/madvise, file watching, FFI — is OS-edge in
   both; Rust's safety does not reach it.
3. The no-vendored rule strips Rust's ecosystem (serde, regex, rayon) while C++
   keeps `<format>`, `<span>`, `<charconv>`, `<filesystem>`, `std::variant`, and
   direct OS headers.
4. One language, one build graph, one debugger, one sanitizer pass for a solo
   developer shipping a native app.

Rust still wins UTF-8 validation in the CLI layer, untrusted parser safety,
race-free parallel search, and compiler-assisted refactoring — real, but localized
to the lower-risk logic layer. C++ safety is a process: RAII, `span`/`string_view`,
strong index types, hardened STL, ASan/UBSan/TSan, libFuzzer, property tests,
never exposing base pointers, remapping by swapping a new mapping.

**M0.5 kill gate:** if C++ bug density stays high despite sanitizers/fuzzing, or
the untrusted surface grows, switch to Rust.