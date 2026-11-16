# editor — Plan (v6.1)

> Name: **editor**. Status: planning. v6.1 — trimmed, then audit fixes applied.
> A native macOS text editor for **minimum memory, minimum CPU, maximum speed**,
> with a **CLI-first** surface so AI agents can drive it as reliably as a human.
> One C++ core, one custom-drawn viewcore, AppKit chrome.

---

## 1. Decisions

| ID | Decision | Revisit |
| --- | --- | --- |
| D1 | **C++ all-in** (core + viewcore + platform + CLI); ObjC++/AppKit on macOS. | M0.5 kill gate: C++ bug density high despite sanitizers/fuzzing, or Rust velocity/seam clearly better. |
| D2 | **Custom-drawn view only.** No native-control hybrid. | Post-1.0, only if the custom view misses perf and a spike proves better. |
| D3 | **macOS only for v1.** Windows/Linux post-1.0. | Market or schedule. |
| D4 | **Bytes are the currency.** Line = byte range; `col` = UTF-8 byte offset. | Never. |
| D5 | **One-shot CLI + `batch`.** `serve` deferred. | Profiling shows repeated open cost dominates. |
| D6 | **CoW save**: same-length O(changed), length-changing O(tail). | A journal/extent-map proves simpler. |
| D7 | **Sparse line index**: 64 lines/chunk, 8 B chunk start + 4 B count, in-chunk scan. | 25M-line memory gate fails. |
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
- Stay responsive with thousands of files. (v1 is one-file-at-a-time; project
  tree/search are post-1.0.)
- Near-zero idle CPU; small, predictable, **measured** memory.
- Genuine native macOS app: real window, native chrome, fonts, dialogs.
- Portable core (C++) designed to be cross-platform; **macOS backend in v1**.
- A CLI that exposes every **file-level** feature, emits machine-readable output,
  and shares domain ops with the GUI. Interactive-only commands (undo/redo/reload)
  are GUI/conformance-only in v1.
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
  domain ops, `DocumentSession`. Workspace/search are post-1.0. No UI, no exit
  codes. OS access is injected via `FileSystem` (FileWatcher post-1.0); no
  `#ifdef` in core.
- **`viewcore` (C++)** — platform-independent view logic: wrapping, line cache,
  hit-testing, selection, scroll, caret, decorations. One implementation, one
  conformance suite.
- **`textbackend` (per OS)** — the only place that shapes/draws glyphs: `measure`,
  `shape` into runs, `draw` into a `Canvas`. v1: CoreText.
- **`platform/macos`** — window, menus, dialogs, keyboard/mouse, clipboard,
  `FileSystem`, `Canvas`. No editing logic. (File watching post-1.0.)
- **`workspace`** — **post-1.0**: directory tree cache, ignore rules, content mmap
  LRU, bounded parallel scan.
- **`DocumentSession`** — open documents + undo + change ranges. Used by the GUI;
  `serve` (post-1.0) is a transport over it.
- **`cli` (C++)** — argv → domain ops → persist → JSON envelope.

**Dependency rule:** `cli`, `platform/*`, `viewcore` → `core`; `core` → `std` +
injected OS interfaces. The frozen interface lives at
**`core/include/editor.hpp`**.

## 4. Core interface

Freeze the interface at **Pre-M0.0**; M0.5 is a conformance check, not a re-freeze.

```cpp
// core/include/editor.hpp — v1
#pragma once
#include <atomic>
#include <cstddef>
#include <cstdint>
#include <span>
#include <string_view>

// Core status covers document/IO. The CLI adds E_USAGE, E_SCHEMA, E_PARSE,
// E_LOCKED, E_OUTSIDE_ROOT, E_NO_MATCH, E_AMBIGUOUS.
enum class Status : uint32_t {
  Ok = 0, Io, NotFound, Range, Conflict, Stale, Oom, Encoding, InvalidUtf8,
  Binary, TooLarge, Unsupported, Internal, BufferTooSmall, SavedButNotDurable
};

enum class Eol : uint32_t { Lf, CrLf, Cr, Mixed };
enum class Encoding : uint32_t { Utf8, Utf16Le, Utf16Be, Other };  // v1: always Utf8
enum class ChangeKind : uint32_t { Insert, Delete, Replace, Reload, Save };

struct Rev {                       // stat-only in v1
  uint64_t dev = 0, ino = 0, mtime_ns = 0, ctime_ns = 0, size = 0;
};

struct Info {
  std::string_view path;           // borrowed; valid until next call
  int64_t bytes = 0, line_count = 0;
  Eol eol = Eol::Lf;
  Encoding encoding = Encoding::Utf8;
  bool bom = false, final_newline = true, dirty = false;
  Rev rev;
};

struct Change {
  int64_t first = 0, last = 0, new_count = 0;  // [first,last); new line count
  ChangeKind kind = ChangeKind::Insert;
};

struct Edit {
  int64_t line = 0, col = 0;       // 0-based; col = UTF-8 byte offset
  int64_t removed_len = 0;         // bytes; 0 for insert
  std::string_view text;           // inserted/replacement bytes; borrowed
};

struct CancelToken {
  mutable std::atomic<bool> cancelled{false};
  void cancel() const { cancelled.store(true, std::memory_order_relaxed); }
};

class Snapshot {
 public:
  virtual ~Snapshot() = default;
  virtual int64_t line_count() const = 0;
  virtual Rev rev() const = 0;
  virtual Status read_lines(int64_t first, int64_t last, char* buf, size_t cap,
                            size_t* used, int64_t* out_n) const = 0;
  virtual Status read_range(int64_t off, size_t len, char* buf, size_t cap,
                            size_t* used) const = 0;
};

class Editor {
 public:
  static constexpr uint32_t kInterfaceVersion = 1;
  virtual ~Editor() = default;

  virtual Status open(std::string_view path_utf8) = 0;
  virtual Status create(std::string_view path_utf8) = 0;  // new/truncated
  virtual Status info(Info* out) const = 0;
  virtual Rev rev() const = 0;

  // first inclusive, last exclusive. buf=nullptr -> size query.
  // Returns line bytes INCLUDING the terminator. On BufferTooSmall,
  // `used` = needed. `first >= last` -> empty/Ok. `last > line_count` clamps.
  // `out_n` = lines written.
  virtual Status read_lines(int64_t first, int64_t last, char* buf, size_t cap,
                            size_t* used, int64_t* out_n) const = 0;
  virtual Status read_range(int64_t off, size_t len, char* buf, size_t cap,
                            size_t* used) const = 0;

  // Edits are relative to the ORIGINAL buffer, sorted, non-overlapping,
  // all-or-nothing. Inserted newlines use the file's dominant EOL.
  virtual Status apply(std::span<const Edit> edits,
                       const CancelToken* cancel = nullptr) = 0;

  virtual Status undo() = 0;
  virtual Status redo() = 0;
  virtual Status save(const Rev* expected_rev = nullptr) = 0;
  virtual Status save_as(std::string_view path_utf8,
                         const Rev* expected_rev = nullptr) = 0;
  virtual Status reload() = 0;

  using ChangeFn = void (*)(Editor*, const Change&, void*);
  virtual uint64_t on_change(ChangeFn, void* ud) = 0;   // registration id
  virtual void off_change(uint64_t id) = 0;
  virtual void drain() = 0;            // callbacks fire ONLY here
  virtual std::string_view errmsg() const = 0;  // thread-local, borrowed

  // Borrowed immutable view; valid until release_snapshot or the next mutation.
  virtual const Snapshot* snapshot() const = 0;
  virtual void release_snapshot(const Snapshot*) = 0;
};

// Injected OS abstraction; keeps core free of platform #ifdefs.
struct Mapping;
class FileSystem {
 public:
  virtual ~FileSystem() = default;
  virtual Status map_readonly(std::string_view path, Mapping** out) = 0;
  virtual Status pread(std::string_view path, uint64_t off, char* buf,
                       size_t cap, size_t* used) = 0;
  virtual Status clone_file(std::string_view src, std::string_view dst) = 0;
  virtual Status atomic_replace(std::string_view tmp, std::string_view dst) = 0;
  virtual Status identity(std::string_view path, Rev* out) = 0;
  virtual Status lock(std::string_view path, void** lock) = 0;
  virtual void unlock(void* lock) = 0;
};
```

`clone_file` must clone the **open original handle** (the original inode), not the
path, so external replacement/truncation cannot mix old offsets with new bytes.

**Rules**

- Every fallible call returns `Status`; no `void` mutation/save.
- `(ptr, len)` / `string_view` / `span` everywhere; no NUL-terminated input.
- `int64_t` for lines/offsets; `0` is a valid line/length, never an error.
- Core is 0-based; CLI is 1-based. `col` is a UTF-8 byte offset.
- `apply` validates that edits do not split a UTF-8 codepoint and that inserted
  text is valid UTF-8; violations → `E_INVALID_UTF8`/`E_RANGE`.
- Core never calls the UI; it appends changes and the UI calls `drain()`.
- Callbacks must not call back into the core (no re-entrancy).
- Exceptions must not escape; catch at every boundary → `Status::Internal`. A
  half-applied edit must leave the editor consistent.
- One `Editor` per thread; parallel readers use `snapshot()`.
- `errmsg` is thread-local and borrowed; no retention.
- Version tag `kInterfaceVersion`; reject a mismatch at a shared-library boundary.
- C++20, pinned toolchain; statically link core into GUI/CLI.
- Hardening: `_GLIBCXX_ASSERTIONS` / `_LIBCPP_HARDENING_MODE`, ASan/UBSan,
  `-Wall -Wextra -Wconversion` (MSVC `/sdl` post-1.0).

## 5. Document model

### 5.1 Bytes, lines, EOL, encoding

- A **line** is a byte range `[start, end)` excluding its terminator.
- `line_count` = terminators + (1 if the last line is non-empty). Empty = 0;
  `a\n` = 1; `a` = 1; `a\n\n` = 2.
- `col` is a 0-based UTF-8 byte offset within the line (tabs = 1 byte). `col = 0`
  is **after** the BOM; the BOM is not part of line 1.
- EOL is per-file (`lf`, `crlf`, `cr`, `mixed`); mixed endings are preserved.
  **Lone CR is a terminator.** A CR at a chunk boundary + LF in the next chunk is
  **one** CRLF terminator.
- **Inserted newlines use the file's dominant EOL**; existing EOLs are preserved.
- Encoding: UTF-8 strict. Invalid UTF-8 → `E_INVALID_UTF8` with the byte offset.
  Non-UTF-8 (UTF-16 BOM, Latin-1) → `E_UNSUPPORTED` with a clear diagnostic.
- Edits that would split a UTF-8 codepoint are rejected (`E_INVALID_UTF8`/
  `E_RANGE`); inserted text must be valid UTF-8.
- Validation is lazy per touched line; a `validate` command can scan the whole file.

### 5.2 Piece table and add buffer

- Original file is an immutable base piece; edits append to an add buffer.
- **Coalesce** adjacent inserts at the add-buffer tail whose predecessor ends
  exactly at the append offset. Without it, typing creates unbounded pieces.
- Add buffer is **capped at 8 MB and spilled to a temp file**; compacted before
  save. Non-tail inserts do not coalesce.
- "Edits are metadata" = piece descriptors + inserted bytes, not a document copy.

### 5.3 Sparse chunk line index

- Balanced tree of chunks; each covers **64 lines** and stores line-start offsets
  **relative to the chunk's first line**, plus a line count. The tree stores
  absolute chunk-start offsets.
- An edit updates chunk starts **on the path** (O(log n) chunks), not all chunks.
  Only that path is touched; the rest of the index is untouched.
- **Never invalidate the whole index on edit.** Built lazily; only touched chunks
  are resident.
- Memory: one 8 B chunk start + 4 B line count per 64 lines, plus tree nodes.
  5M lines ≈ 1 MB (≈2 MB with nodes); 25M ≈ 5 MB; 125M ≈ 23 MB.
- Chunk split/merge is specified and property-tested; worst case is O(lines in the
  affected chunk).

### 5.4 Undo/redo

- Undo = apply the **inverse edit** (delete inserted range, re-insert deleted text).
- Coalesce typing runs (500 ms or word boundary); **programmatic edits do not
  coalesce** (one edit = one undo step).
- Cap: 10k steps or 8 MB. Larger inverse text spills to a temp file; if that is
  impossible, undo is disabled for that edit and reported.
- Redo is cleared on any new edit.
- **Undo is GUI/conformance-only in v1**; the CLI is one-shot and has no `undo`
  command. (CLI undo would require a session/`serve`, post-1.0.)
- At save: flatten the piece table, drop undo older than the save, clear the add
  buffer. "Undo to last save" is the default; full session undo is opt-in.

### 5.5 Save path (honest cost model)

v1 targets macOS/APFS; Windows/Linux fallbacks are post-1.0.

- **No-op if clean.**
- **Same-length edits**: CoW-clone the original (O(1) on APFS; O(file) elsewhere),
  `pwrite` changed spans, then fsync → rename → fsync.
- **Length-changing edits**: write the edited span, then the shifted tail, then
  `ftruncate` to the new size (tail = 0 means just truncate). If the tail is ≤ 25%
  of the file, do the tail write; otherwise full rewrite. Report the method.
- **Clone the original fd/inode**, not the path. If the path identity changed
  since open, fail `E_CONFLICT`; never mix old offsets with new bytes.
- Fallback: `fcopyfile` (macOS) for unchanged ranges; handle errors with
  read/write. Never copy only a prefix and leave the suffix unwritten.
- Temp: same directory, `O_CREAT|O_EXCL`, 0600, cleanup on failure. Rename needs
  directory write permission; read-only dir → `E_IO` or `--in-place`.
- Metadata: macOS `CLONE_ACL` + xattr copy; `chown` before `chmod`; hardlink
  caveat → `--in-place`. Windows/Linux metadata post-1.0.
- Durability: `fsync(temp)` → `rename` → `fsync(dir)`; macOS `F_FULLFSYNC` for
  power-loss durability (or `fsync` for crash-only). Post-rename `fsync` failure
  → `SavedButNotDurable` warning, `ok: true`; **never blind-retry**.
- **Post-save transition (atomic)**: new base = saved bytes, clear add buffer,
  drop old undo, update identity/mtime/rev. Double-save conformance test.
- **Gate**: same-length edit in 1 GB on APFS < 100 ms p99 with **crash-only
  `fsync`**; `F_FULLFSYNC` has a separate, looser budget. Length-changing edit
  measured separately.
- The JSON `save` field is one of `clone_pwrite`, `clone_pwrite_tail`,
  `copy_range`, `full_rewrite`.

### 5.6 External mutation and mmap

- Record `(dev, ino, size, mtime_ns, ctime_ns)` at open.
- Rename-based replacement is safe (old inode stays mapped). In-place truncate
  causes `SIGBUS` on macOS; handle with a signal handler + `sigsetjmp`/`siglongjmp`
  around risky accesses (**no locks or allocations in the handler**), or `pread` for
  at-risk files. Windows `EXCEPTION_IN_PAGE_ERROR` is post-1.0.
- On mismatch: auto-reload if clean; if dirty, `E_STALE` with a reload hint.
  `reload()` discards undo, maps new content, maps the caret to the nearest valid
  position.
- File watching (FSEvents) is post-1.0 and is **not** the source of truth; the
  identity check is.
- mmap policy: **sliding window** (256 MB–1 GB) for large files; `pread` for
  network FS; buffered `read()` under ~64 KB; **no `MAP_PRIVATE` writes** for
  large files (use the capped add buffer); base mapping is read-only; scans use
  chunked `fcntl(F_NOCACHE)`/`madvise` + `pread`, never mmap-the-world.

## 6. CLI

Binary: `editor`. The GUI is `Editor.app`, linking the same core.

### 6.1 Commands

```
editor read <path> [--from N | --from-offset B] [--to N | --length L]
                   [--around N --radius K] [--max-bytes N] [--stream]
editor write <path> (--stdin | @FILE) [--expect REV] [--force] [--append] [--in-place]
editor edit <path> (--old TEXT|@FILE --new TEXT|@FILE [--all | --count N]
                   | --at LINE[:COL] --text TEXT|@FILE
                     [--op insert|replace|delete] [--length L | --end LINE[:COL]])
                   [--expect REV] [--force] [--dry-run] [--in-place]
editor info <path>
editor validate <path> [--max-errors N]
editor move <src> <dst> [--expect REV] [--force]
editor batch [--file F | --stdin] [--check] [--force]
editor version
editor schema [COMMAND]

# post-1.0:
editor diff | restore | apply | open | tree | search
# search is literal + glob only (no regex)
```

**Global flags:** `--root DIR` (default cwd), `--allow-outside`,
`--allow-huge` (override `--max-file-bytes`, default 2 GB), `--verbose`.
Always JSON on stdout; no `--json`.

**Units and defaults**

- `read`: `--from`/`--to` 1-based lines; `--from-offset`/`--length` 0-based bytes;
  one axis at a time. `--around` requires `--radius`. Default `--max-bytes` 256 KiB;
  beyond that `--stream` (NDJSON) or redirect. Result: `content` (string) or
  `content_ref` `{path, offset, length}`; `from`, `to`, `truncated`.
- `edit`: `--at` is 1-based line and 1-based byte column; `--old`/`--new` literal
  text or `@FILE`/`-` (stdin); `--count`/`--all` exclusive (default one match);
  delete/replace need `--length` or `--end`; `--force` overrides `--expect`.
  Result: `changed`, `rev`.
- `write`: truncates by default; `--append` appends; `--in-place` for hardlinks.
  Result: `bytes`, `rev`.
- `info`: `path`, `bytes`, `line_count`, `eol`, `encoding`, `rev`, `dirty`.
- `validate`: `errors[]` of `{line, col, byte_offset, code}`; `--max-errors` 100.
- `move`: `--force` overwrites; cross-device = copy + delete. Result: `from`, `to`,
  `rev`.
- `batch`: `--check` validates only; `--force` at op level; see §6.4. Result:
  per-op `results[]` + `rolled_back`.
- `tree` (post-1.0): `--depth` 3.

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
      "eol": "lf",
      "encoding": "utf-8",
      "bom": false,
      "final_newline": true,
      "changed": {"from": 2100, "to": 2116},
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
- `path` is as provided, normalized; prefer absolute or `--root`-relative paths.
- Read-only file commands include `rev`; `rev_before`/`changed` appear only on
  mutations.
- Deterministic: sort by (path bytes, offset); fixed field order; no
  `unordered_map` iteration; no wall-clock fields (file mtime/ctime are in `rev`);
  golden tests normalize.
- Version: `editor.result/1`; additive within a major. `editor schema [COMMAND]`
  dumps command schemas; `editor version` prints the CLI version.
- Large outputs: `read` inline up to `--max-bytes` (default 256 KiB), else
  `--stream` NDJSON or `content_ref` `{path, offset, length}`; `search --stream`
  is NDJSON.

### 6.3 Errors and exit codes

Core `Status` covers document/IO; the CLI adds `E_USAGE`, `E_SCHEMA`, `E_PARSE`,
`E_LOCKED`, `E_OUTSIDE_ROOT`, `E_NO_MATCH`, `E_AMBIGUOUS`. Errors carry `code`,
`message`, `retryable`, `path`, `hint`, and command-specific fields.
`E_INVALID_UTF8` includes the byte offset; `E_NO_MATCH`/`E_AMBIGUOUS` include a
bounded list of `{line, col, byte_offset}`.

| Exit | Meaning | Examples |
| --- | --- | --- |
| 0 | ok | `Ok`, `SavedButNotDurable` (warning) |
| 2 | domain | `E_IO`, `E_NOT_FOUND`, `E_RANGE`, `E_CONFLICT`, `E_STALE`, `E_ENCODING`, `E_INVALID_UTF8`, `E_BINARY`, `E_OUTSIDE_ROOT`, `E_PARSE`, `E_TOO_LARGE`, `E_LOCKED`, `E_UNSUPPORTED`, `E_BUFFER_TOO_SMALL`, `E_NO_MATCH`, `E_AMBIGUOUS` |
| 3 | usage/schema | `E_USAGE`, `E_SCHEMA` |
| 4 | internal | `E_OOM`, `E_INTERNAL` |

```json
{"schema":"editor.result/1","command":"edit","ok":false,"duration_us":900,
 "error":{"code":"E_CONFLICT","message":"file changed since read","retryable":true,
          "path":"src/main.cpp","expected_rev":"stat:1:2:3:4:4096",
          "actual_rev":"stat:1:2:5:6:4112","hint":"re-read, then retry"},
 "results":[]}
```

### 6.4 Batch and concurrency

`batch` reads a JSON-lines program; each op is one object:

```json
{"op":"edit","path":"a.cpp","old":"let x = 1;","new":"let x = 2;","expect":"stat:1:2:3:4:4096"}
{"op":"write","path":"b.cpp","content":"...","force":true}
{"op":"move","src":"a.cpp","dst":"c.cpp"}
```

- **Validation-stage atomicity**: parse/validate every op against the original
  files; any failure → no writes. Stage each file write to a temp; commit (rename)
  each. On commit failure, roll back applied writes from backups; if rollback fails,
  return `ok: false`, `rolled_back: false`, and the applied ops. Buffer responses
  until commit. `--check` validates only. **Not crash-atomic.**
- Ops see the original files; two ops on the same file must not overlap (`E_RANGE`).
- `batch` adds `rolled_back` (bool) and `applied[]` to the envelope; `--check`
  returns `valid` + `errors[]` and writes nothing.
- Mutations require `--expect` on existing files; `--force` (command or op)
  overrides. Mismatch → `E_CONFLICT`, exit 2. **GUI**: re-check the on-disk `rev`
  before save; `core::save` performs the expect-check + write + rename under an OS
  lock, so the GUI does not need a separate lock.
- Lock: `<path>.editor.lock`, `O_CREAT|O_EXCL`, 0600, random token + pid + start
  time; stale by pid liveness + token; held only for the write critical section.
- Symlinks: resolve, re-check inside `--root`, `O_NOFOLLOW` + `realpath` on the
  final component; never rename over a symlink.
- Refuse non-regular files; `--max-file-bytes` unless `--allow-huge`.
- **`E_CONFLICT` vs `E_STALE`**: `E_CONFLICT` = `--expect` mismatch on mutation;
  `E_STALE` = file changed externally while dirty and auto-reload is not possible.

### 6.5 Security

- **Scripts** (post-1.0): arbitrary code execution by design; trust prompt /
  `--yes`; never from GUI, `apply`, or config; allowlist in `.editor/scripts/`.
- **No config file in v1.** If added post-1.0: data only, versioned, no `exec`.
- **Patches** (post-1.0): typed AST, hard caps (files, hunks, line length, bytes);
  reject absolute paths and `..`; resolve against `--root`; handle
  `\ No newline at end of file` and `/dev/null`; fuzz.
- **Secret leakage**: argv is visible in `ps`; prefer `--text @FILE`/`-` (stdin) for
  `edit`, and `@FILE`/`--stdin` for `write`/`batch`; never echo file contents or
  matched lines in errors.
- **DoS defaults**: `--max-bytes`, `--max-results`, search timeout, tree-walk
  depth cap, symlink-cycle detection.
- **`serve` (post-1.0)**: Unix socket 0600, per-session token, timeouts.

### 6.6 Registry invariant

Registry entry = `{ name, arg_schema, core_fn, cli_binding, gui_binding? }`.

- A `core_fn` with no `cli_binding` is a test failure (except an explicit
  `cli_excluded` allowlist).
- A mutating `gui_binding` with no undo entry is a test failure.
- A golden test asserts **every GUI menu item maps to a domain op id**.
- `editor schema` is generated from the registry.

## 7. Budgets and measurement

**Metric:** macOS `phys_footprint`; Windows private working set; Linux PSS
(post-1.0). Not directly comparable; compare the same workload.

**Measured (macOS, Apple Silicon, 8 GB)**

| Fact | Measurement |
| --- | --- |
| Empty window (shown, one draw) | 40.6 MB RSS / 10.4 MB footprint |
| mmap 2 GB untouched | +0 MB, 0.010 ms |
| mmap 2 GB full scan | **1.68 GB RSS** |
| `CTLineCreate` cold line | 9.2 µs |
| Cached CoreText line | ~3.1 KB each |
| 40 cached line draws × 60 | 0.26 ms/frame |
| Cold shape+draw | 0.43 ms/frame |
| One font resolve | 1.6 ms |
| First `CTFontCreateWithName` | 16.7 ms |
| Font enumeration (175) | 80.8 ms |
| Time to first window | 60–390 ms |

**Budgets (caps, not additive)**

| Metric | Cap |
| --- | --- |
| Empty-window baseline | ≤ 12 MB |
| + line cache (1000–2000 entries **and** ≤ 6 MB; evict on either) | ≤ 6 MB |
| + undo/edit log (spilled) | ≤ 8 MB |
| + add buffer (spilled) | ≤ 8 MB |
| + sparse line index (5M lines, incl. tree nodes) | ≤ 2 MB |
| **Loaded (1 GB file, cache + undo + index + add buffer)** | **≤ 40 MB** |

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
- **viewcore API** (sketch): `set_document`, `set_snapshot`, `resize`,
  `set_wrap`, `set_font`, `set_decorations`, `selection`/`set_selection`,
  `first_visible_line`, `scroll_to`, `draw`, `key`, `mouse`. **textbackend**:
  `measure`, `shape` into runs, `draw` into a platform `Canvas`.
- Line cache keyed by **(logical line, wrap width)**; one shaped run per *visual*
  line. **Bounded LRU, 1000–2000 entries and ≤ 6 MB**; skip caching lines above
  ~1000 visual lines (a 100 MB single line must not blow the cache).
- Invalidate at/after the first changed line; invalidate all on width/font change.
- **Do not enumerate fonts at launch** (80.8 ms); resolve one (1.6 ms). First
  `CTFontCreateWithName` can cost 16.7 ms. Enumerate lazily when the font menu opens.
- Monospace default; OS fonts (no bundled font in v1).

## 9. Workspace, search, and future

**All post-1.0.** v1 has no workspace, tree command, or search.

- **Tree cache**: path, size, mtime, ino, mode; validate per-entry stat; 10k files
  ≈ 1.5–3 MB; cache `~/.cache/editor/tree-<hash>` keyed by root + app version.
- **Content** mmap'd on demand; no inverted index; bounded content-mmap LRU
  (50–60 MB) for repeated searches.
- **Bounded parallelism**: 4–8 workers, each with a small mmap window + `Snapshot`;
  bounded queue.
- **Search**: literal + glob only; no regex (agents use `grep`/`rg`). Fuzzy/vector
  deferred.
- **Future seams** (designed now, built later): `Decoration` ranges for
  syntax/search/git; `DocumentSession` for `serve`; `Snapshot` for parallel
  readers. Syntax = lazy per-visible-line tables; Markdown = preview; git = `git`
  CLI; scripts = `editor run`.

## 10. Testing, CI, build

**Pre-M0.0 decisions**

- **Tests:** hand-rolled `CHECK` macros + a small PRNG + libFuzzer. No vendored
  test framework.
- **JSON:** hand-rolled writer + strict reader, fuzzed. Foundation
  `NSJSONSerialization` is an allowed macOS fallback if bugs appear.

**Testing**

- Unit: core, viewcore, CLI parsers.
- Property: edit/insert/delete/undo invariants; piece-table round-trips;
  save/restore identity; UTF-8 boundaries; chunk split/merge; line-count
  conventions; coalescing; CRLF/CR/mixed EOL; empty file; no trailing newline;
  huge line.
- Fuzz: argv, JSON reader/writer, patch parser (post-1.0), UTF-8, line index.
- Golden CLI: exact JSON for every command, including errors.
- Conformance: `Editor` (open/create/read/apply/undo/save/save_as/reload/rev/
  snapshot/cancel/change), `viewcore` (`TextView`: wrap/hit-test/selection/scroll/
  decorations), `textbackend` (measure/shape/draw), `FileSystem` (map/pread/clone/
  replace/identity/lock).
- Sanitizers: ASan/UBSan every PR (lite); TSan nightly (limited on macOS);
  hardened STL in debug.
- Fault injection: `fsync`/`rename`/`pwrite` failures; crash-consistency of
  temp→rename; batch rollback; lock/TOCTOU two-process stress; external truncate.
- Benchmarks: cold start, open 1 GB, same-length edit, length-changing edit,
  viewport edit, scan 1 GB, 5M/25M-line, 5 GB, 10k-file, 100 MB single line;
  footprint gates.

**CI:** macOS smoke every PR; perf nightly; release engineering as M3.5.

**Build:** one CMake graph (Xcode generator on macOS); artifacts `libcore.a`,
`libviewcore.a`, `editor` CLI, `Editor.app`; C++20; notarization (hardened
runtime, `notarytool`, staple); **Apple Silicon only** in v1. No vendored
third-party.

## 11. Roadmap

MVP = **macOS M0–M3.5**. Windows/Linux/search/syntax are post-1.0.

**v1 checklist:** open/save, edit, undo/redo, find/replace, font size + tab width
+ wrap settings, status bar, gutter, wrap, and the CLI (`read`, `write`, `edit`,
`info`, `validate`, `move`, `batch`, `version`, `schema`).

| Milestone | Scope | Est. | Gate |
| --- | --- | --- | --- |
| **Pre-M0.0** | Freeze `editor.hpp` + C++ conformance test + data-model contract + test/JSON decisions. | 1 wk | Interface compiles; conformance skeleton runs. |
| **M0a** | Piece table + sparse index + `read`/`write`/`info` + same-length CoW save + JSON. | 3–4 wk | Read/write round-trip + golden JSON; 1 GB same-length edit < 100 ms p99 (APFS, crash-only `fsync`). |
| **M0b** | `edit` + undo + length-changing save + fault injection. | 3–4 wk | Edit/undo property tests; double-save and save-crash tests. |
| **M0c** | `batch` + `move` + `validate` + security + error model + registry invariant. | 2–3 wk | Batch validation/rollback; fuzz; CLI golden. |
| **M0.5** | Headless render oracle + viewcore skeleton + **5 GB read-only demo** + C++ validation. | 2 wk | 5 GB at 60 fps, flat footprint; sanitizers clean. |
| **M1** | macOS read-only custom view: AppKit + CoreText + viewcore; keyboard/mouse. | 4–6 wk | Open/scroll 5 GB. |
| **M2** | macOS editing + undo + save + find/replace + settings (font size, tab width, wrap via `NSUserDefaults`; keybindings hardcoded). | 4–6 wk | v1 checklist: edit, undo, find/replace, settings. |
| **M3** | macOS polish: status bar, gutter, wrap, decorations. | 3–4 wk | v1 checklist complete. |
| **M3.5** | Release engineering: notarization, packaging, CI perf gates. | 2–3 wk | Signed `.app`; CI green. |
| **Post-1.0** | Windows port, Linux port, workspace/tree, project search, syntax, Markdown, git, scripts, `serve`, native-control experiment. | — | — |

**Total:** the table sums to **24–33 weeks of work**. With ~30% overhead that is
**31–43 weeks full-time (~7–10 months)**; solo/part-time, plan on **9–12 months**.

**Cut list if behind (v1 cuts first):**

1. M3 polish (decorations, status bar) → ship M2-level editor.
2. M2 find/replace → ship without it.
3. Search/syntax, Markdown/git/scripts/serve (already post-1.0).
4. Do **not** cut: save correctness, fault injection, basic editing/undo, status
   bar/gutter.

## 12. Risks and open decisions

| Risk | Mitigation |
| --- | --- |
| Length-changing save is O(tail) | Honest cost model; tail/full-rewrite threshold; benchmark both. |
| Hand-rolled JSON bugs | Fuzz + property tests; keep the parser small and strict. |
| Schedule (~9–12 months) | macOS-only MVP; cut list; post-1.0 fence. |
| Temptation to re-add hybrid/native control | D2 decision + post-1.0 trigger. |
| mmap SIGBUS on truncate | Signal handler/pread; identity check; reload. |
| External mutation race | ctime-aware rev; lock covers check+write; auto-reload clean. |
| No IME/a11y limits CJK/screen-reader use | Accepted for v1; post-1.0 if needed. |

**Open decisions**

1. **Bundled font**: OS fonts (default) vs. +200–800 KB for identical metrics.

Resolved: Apple Silicon only for v1; no config file; no regex; no SHA-256; no
IME/a11y; no Windows/Linux in v1.

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