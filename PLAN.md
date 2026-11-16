# editor — Plan (v6.2)

> Name: **editor**. Status: planning. v6.2 — final audit pass applied.
> A native macOS text editor for **small, predictable memory, low CPU, and high
> throughput**, with a **CLI-first** surface so AI agents can drive it
> deterministically.
> One C++ core, one custom-drawn viewcore, AppKit chrome.

---

## 1. Decisions

| ID | Decision | Revisit |
| --- | --- | --- |
| D1 | **C++ all-in** (core + viewcore + platform + CLI); ObjC++/AppKit on macOS. | M0.5 kill gate: C++ bug density high despite sanitizers/fuzzing, Rust velocity/seam clearly better, or the untrusted surface grows. |
| D2 | **Custom-drawn view only.** No native-control hybrid. | Post-1.0, only if the custom view misses perf and a spike proves better. |
| D3 | **macOS only for v1.** Windows/Linux post-1.0. | Market or schedule. |
| D4 | **Bytes are the currency.** Line = byte range; `col` = UTF-8 byte offset. | Never. |
| D5 | **One-shot CLI + `batch`.** `serve` deferred. | Profiling shows repeated open cost dominates. |
| D6 | **CoW save**: same-length O(changed); length-changing O(tail) when tail ≤25%, else O(file). | A journal/extent-map proves simpler. |
| D7 | **Sparse line index**: 64 lines/chunk, order-statistic B-tree, in-chunk scan. | The §7 25M-line index budget fails. |
| D8 | **Stat-only `rev`.** No SHA-256 in v1. | Same-size saves within one mtime/ctime tick on APFS (esp. `--in-place`); then add a generation counter or hashing. |
| D9 | **No user-editable config file, no regex, no IME, no accessibility.** | If needed; post-1.0. |
| D10 | **UTF-8 only.** UTF-16 BOM → `E_UNSUPPORTED`; UTF-8 BOM accepted and excluded from logical lines. | UTF-16 becomes a blocker. |
| D11 | **No plugins/mods.** Scripts post-1.0, trust-gated. | Never for v1. |
| D12 | **MVP = macOS M0–M3.5**; if behind, cut per §11 (M3 polish, then M2 find/replace) and ship the reduced scope. | Schedule or market. |

Rationale for D1 is in the [appendix](#appendix-language-choice).

## 2. Goals and non-goals

**Goals**

- Open/edit files from a few bytes to many gigabytes without loading the whole
  file into memory.
- Architecture supports thousands of files; v1 is one-file-at-a-time.
  Responsiveness with thousands of files is a post-1.0 workspace goal.
- Near-zero idle CPU; small, predictable, **measured** memory.
- Genuine native macOS app: real window, native chrome, fonts, dialogs.
- Portable core (C++) designed to be cross-platform; **macOS backend in v1**.
- A CLI that exposes the v1 file-level operations (`read`, `write`, `edit`,
  `info`, `validate`, `move`, `batch`, `version`, `schema`), emits
  machine-readable output, and shares domain ops with the GUI. GUI/conformance-
  only in v1: undo/redo/reload/save_as; `create` is used internally by `write`.
- No vendored/bundled third-party code; OS system libraries and the C++ standard
  library allowed.

**Non-goals (v1)**

- No Windows/Linux, no Intel Macs (Apple Silicon only in v1), no TUI, no
  user-editable config file, no regex, no SHA-256, no IME, no a11y.
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
  domain ops. Workspace/search are post-1.0. No UI, no exit codes. OS access is
  injected via `FileSystem` (FileWatcher post-1.0); no `#ifdef` in core.
- **`viewcore` (C++)** — platform-independent view logic: wrapping, line cache,
  hit-testing, selection, scroll, caret, decorations. One implementation, one
  conformance suite.
- **`textbackend` (per OS)** — the only place that shapes/draws glyphs: `measure`,
  `shape` into runs, `draw` into a `Canvas`. v1: CoreText.
- **`Canvas`** — platform-neutral draw surface (`fill_rect`, `draw_run`, …);
  `platform/macos` implements it, `textbackend` draws into it.
- **`platform/macos`** — window, menus, dialogs, keyboard/mouse, clipboard,
  `FileSystem`, `Canvas`. No editing logic. (File watching post-1.0.)
- **`workspace`** — **post-1.0**: directory tree cache, ignore rules, content mmap
  LRU, bounded parallel scan.
- **`DocumentSession`** — **GUI-side** session state over a single `Editor`
  (one open document + undo + change ranges); not a separate core type in v1.
  `serve` (post-1.0) is a transport over it.
- **`cli` (C++)** — argv → domain ops → persist → JSON envelope.

**Dependency rule:** `cli`, `platform/*`, `viewcore` → `core`; `viewcore` also →
`textbackend` (pure virtual); `platform/*` implements `textbackend` and `Canvas`.
`core` → `std` + injected OS interfaces. The frozen interface lives at
**`core/include/editor.hpp`**.

## 4. Core interface

Freeze the interface at **Pre-M0.0**. M0.5 is **not** a re-freeze; it adds the
headless render oracle, the viewcore skeleton, the 5 GB read-only demo, and a
conformance check.

```cpp
// core/include/editor.hpp — v1
#pragma once
#include <atomic>
#include <cstddef>
#include <cstdint>
#include <memory>
#include <span>
#include <string>
#include <string_view>

// Core status covers document/IO. E_* = SCREAMING_SNAKE_CASE of the Status
// name. CLI-only codes (E_USAGE, E_SCHEMA, E_PARSE, E_OUTSIDE_ROOT,
// E_NO_MATCH, E_AMBIGUOUS) are listed in §6.3.
enum class Status : uint32_t {
  Ok = 0, Io, NotFound, Range, Conflict, Locked, Stale, Oom, InvalidUtf8,
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
  int64_t first = 0, last = 0, new_count = 0;  // [first,last) line indices; new lines
  ChangeKind kind = ChangeKind::Insert;        // Save/Reload: first = last = 0
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
  virtual Status create(std::string_view path_utf8, bool truncate) = 0;
  virtual Status info(Info* out) const = 0;
  virtual Rev rev() const = 0;

  // first inclusive, last exclusive. buf=nullptr -> size query
  // (`used`=needed, `out_n`=lines in [first,last) clamped).
  // On success `used`=bytes written, `out_n`=lines read. On BufferTooSmall:
  // no bytes written, `used`=full needed, `out_n`=0. Returns line bytes
  // INCLUDING the terminator. `first >= last` or `first > line_count` -> empty/Ok.
  // `last` clamps.
  virtual Status read_lines(int64_t first, int64_t last, char* buf, size_t cap,
                            size_t* used, int64_t* out_n) const = 0;
  // buf=nullptr -> size query (`used`=needed); `off >= size` -> empty/Ok;
  // `len` clamps to EOF. On BufferTooSmall: no bytes written, `used`=needed;
  // else `used`=bytes read.
  virtual Status read_range(int64_t off, size_t len, char* buf, size_t cap,
                            size_t* used) const = 0;

  // All edits in one call are relative to the state at the start of that call
  // (shared base; sorted, non-overlapping). Successive calls are relative to
  // the current state (base + add buffer). Unsorted/overlap -> E_RANGE.
  // `removed_len` may cross lines. Inserted newlines use the file's dominant EOL.
  // If `cancel` is set before commit (or at a 64 KB / 64-line checkpoint),
  // return Ok with no edits applied and no change event.
  virtual Status apply(std::span<const Edit> edits,
                       const CancelToken* cancel = nullptr) = 0;

  virtual Status undo() = 0;
  virtual Status redo() = 0;
  // save: no-op if clean (Ok, method="none"); expected_rev checked under lock.
  // in_place selects an in-place write; method receives
  // none/clone_pwrite/clone_pwrite_tail/copy_range/full_rewrite.
  virtual Status save(const Rev* expected_rev = nullptr, bool in_place = false,
                      std::string* method = nullptr) = 0;
  // save_as: refuses to overwrite an existing dst with E_CONFLICT unless
  // expected_rev matches dst's identity. Updates open path/identity.
  virtual Status save_as(std::string_view path_utf8,
                         const Rev* expected_rev = nullptr, bool in_place = false,
                         std::string* method = nullptr) = 0;
  virtual Status reload() = 0;

  using ChangeFn = void (*)(Editor*, const Change&, void*);
  virtual uint64_t on_change(ChangeFn, void* ud) = 0;   // id, never reused
  virtual void off_change(uint64_t id) = 0;
  virtual void drain() = 0;            // callbacks fire ONLY here
  virtual std::string_view errmsg() const = 0;  // thread-local, borrowed

  // Immutable, refcounted view. Mutations do not invalidate existing
  // snapshots; the last owner releases it. Safe for parallel readers.
  virtual std::shared_ptr<const Snapshot> snapshot() const = 0;
};

class Mapping {
 public:
  virtual ~Mapping() = default;
  virtual Status window(uint64_t off, size_t len) = 0;  // clamps to EOF
  // data()/size() are valid until window() or unmap().
  virtual const char* data() const = 0;
  virtual size_t size() const = 0;
};

class FileSystem {
 public:
  virtual ~FileSystem() = default;
  virtual Status open_readonly(std::string_view path, int* out_fd) = 0;
  virtual Status open_write(std::string_view path, bool create, bool truncate,
                            int* out_fd) = 0;
  virtual Status open_scan(std::string_view path, int* out_fd) = 0;  // F_NOCACHE
  virtual Status create_temp(std::string_view dir, std::string* out_path,
                             int* out_fd) = 0;  // O_CREAT|O_EXCL, 0600
  virtual void close_fd(int fd) = 0;
  virtual Status map_readonly(int fd, Mapping** out) = 0;
  virtual Status unmap(Mapping*) = 0;
  // pread: `used`=bytes read (clamped to EOF).
  virtual Status pread(int fd, uint64_t off, char* buf, size_t cap,
                       size_t* used) = 0;
  virtual Status pwrite(int fd, uint64_t off, const char* buf, size_t len,
                        size_t* used) = 0;
  virtual Status ftruncate(int fd, uint64_t size) = 0;
  virtual Status fchmod(int fd, uint32_t mode) = 0;
  virtual Status fchown(int fd, uint32_t uid, uint32_t gid) = 0;
  virtual Status fstat(int fd, Rev* out) = 0;
  virtual Status fsync(int fd, bool full) = 0;
  virtual Status fsync_dir(std::string_view dir) = 0;
  virtual Status clone_file(int src_fd, int dst_fd) = 0;  // clone src inode into dst
  virtual Status atomic_replace(std::string_view tmp, std::string_view dst) = 0;
  virtual Status rename(std::string_view src, std::string_view dst) = 0;
  virtual Status unlink(std::string_view path) = 0;
  virtual Status identity(std::string_view path, Rev* out) = 0;
  virtual Status lock(std::string_view path, void** lock) = 0;
  virtual void unlock(void* lock) = 0;  // unlock(nullptr) is a no-op
};
```

`clone_file` clones the **open src inode** into an already-open `dst_fd` (clone
to a new temp, then overwrite by rename; never unlink the original before a
successful clone). `create_temp` returns a 0600 path + fd in the same directory;
hold the fd from create through rename. `pwrite`/`ftruncate`/`fsync`/`fchmod`/
`fchown` are fd-based; `atomic_replace` commits a save, `rename` is for move,
`fsync_dir` syncs the directory after rename. `open_scan` sets `F_NOCACHE` for
bulk scans.

**Rules**

- Every fallible call returns `Status`; `close_fd`/`unlock` are void and
  no-op-safe. No `void` mutation/save.
- `(ptr, len)` / `string_view` / `span` everywhere; no NUL-terminated input.
- `int64_t` for document lines/byte offsets; `uint64_t` for file offsets/sizes.
  `0` is a valid line/length, never an error. Negative document values →
  `E_RANGE`.
- Core is 0-based; CLI is 1-based. `col` is a UTF-8 byte offset.
- `apply` validates that edits do not split a UTF-8 codepoint and that inserted
  text is valid UTF-8; violations → `E_INVALID_UTF8`/`E_RANGE`.
- An `apply` call pushes exactly one undo step; the cancel token is checked
  before commit and at least every 64 KB / 64 lines during `apply`.
- Core never calls the UI; it appends changes and the UI calls `drain()`.
- Callbacks must not call `on_change`/`off_change`/`apply`/`save`; they may only
  copy the `Change` (no re-entrancy).
- Exceptions must not escape; catch at every boundary → `Status::Internal`. A
  half-applied edit must leave the editor consistent.
- One `Editor` per thread. Snapshots are immutable and refcounted; parallel
  readers hold `shared_ptr`. Mutations do not invalidate existing snapshots.
- `rev()`/`Info::rev` return the last on-disk identity observed at `open` or
  after the last successful `save`/`reload`; they are not updated by unsaved
  edits.
- `errmsg` is thread-local and borrowed; no retention.
- Version tag `kInterfaceVersion`; defensive check at any core/GUI/CLI link
  boundary (core is statically linked).
- C++20, pinned toolchain; statically link core into GUI/CLI.
- Hardening: `_GLIBCXX_ASSERTIONS` / `_LIBCPP_HARDENING_MODE`, ASan/UBSan,
  `-Wall -Wextra -Wconversion` (MSVC `/sdl` post-1.0).

## 5. Document model

### 5.1 Bytes, lines, EOL, encoding

- A **line** is a byte range `[start, end)` excluding its terminator.
- `line_count` = terminators + (1 if the last line is non-empty). Empty = 0;
  `a\n` = 1; `a` = 1; `a\n\n` = 2.
- **BOM:** a UTF-8 BOM is accepted, excluded from every logical line range, and
  reported as `bom: true`; a UTF-16 BOM → `E_UNSUPPORTED`. `col` is relative to
  the post-BOM start of line 1. `read_lines` excludes the BOM; `read_range` is
  raw file bytes; `changed.from/to` are raw file offsets (an edit at line 0,
  col 0 reports `from = BOM_len`).
- `col` is a 0-based UTF-8 byte offset within the line (tabs = 1 byte).
- `line == line_count` with `col == 0` is valid (**append at EOF**); for an
  existing line, `0 <= col <= line_byte_length`.
- EOL is per-file (`lf`, `crlf`, `cr`, `mixed`); mixed endings are preserved.
  **Lone CR is a terminator.** Chunks are line-aligned, so a CRLF is never split;
  if a forced mid-line split leaves a CR at a chunk end, the scan reads one byte
  into the next chunk to distinguish CR from CRLF.
- **Inserted newlines** (`\n`, `\r\n`, `\r` in `Edit::text`) are normalized to
  the file's **dominant EOL** (most frequent terminator; tie → first seen; empty
  file **or no terminators** → LF). Existing EOLs are preserved.
- Encoding: UTF-8 strict. `open` performs O(1) checks only (UTF-8/UTF-16 BOM,
  first-8 KB NUL); UTF-8 validity is lazy per touched line, so `open` returns Ok
  even if later bytes are invalid. `validate` scans the whole file and reports
  `invalid_utf8` with byte offsets. `apply` validates inserted text and the
  touched range. Edits that split a UTF-8 codepoint → `E_RANGE`; invalid bytes →
  `E_INVALID_UTF8`; inserted text must be valid UTF-8.
- Binary detection: a NUL byte in the first 8 KB → `E_BINARY`.
- Validation is lazy per touched line; a `validate` command can scan the whole file.

### 5.2 Piece table and add buffer

- Original file is an immutable base piece; edits append to an add buffer.
- **Coalesce** adjacent inserts at the add-buffer tail whose predecessor ends
  exactly at the append offset. Without it, typing creates unbounded pieces.
- Add buffer is **capped at 8 MB resident and spilled to a temp file**; compacted
  before save. A single insert larger than the cap spills and is not coalesced;
  the cap is resident, not per-edit. Non-tail inserts do not coalesce.
- "Edits are metadata" = piece descriptors + inserted bytes, not a document copy.

### 5.3 Sparse chunk line index

- Balanced B-tree of chunks; each covers **up to 64 lines** and stores
  `line_count` (4 B) and `byte_len` (8 B, bytes of its lines including
  terminators). Each tree node stores subtree sums of both (8 B each) for
  order-statistic lookup.
- A chunk's **absolute line/byte start** = sum of preceding chunks' counts/
  lengths, computed during the O(log n) descent. No absolute offsets are stored.
- An edit updates the touched chunk's count/len and the subtree sums **on the
  path** (O(log n)); later chunks are not rewritten.
- In-chunk scan uses the chunk's start line/byte (from the descent) to find the
  line.
- Chunk descriptors (12 B each) are resident for the whole file; the in-chunk
  line-start scan reads the mapping/add buffer on demand. "Lazy" means the index
  is not built until the first line query/edit, not that descriptors are evicted.
- Memory: 12 B/chunk base + B-tree nodes. 5M lines ≈ 0.9 MB (≈2 MB with nodes);
  25M ≈ 4.7 MB (≈9 MB); 125M ≈ 23 MB (≈47 MB). A binary tree is ≈2.9/14/72 MB
  (32-bit indices) or ≈3.8/19/95 MB (64-bit pointers).
- Chunk split/merge is specified and property-tested; worst case is O(64) (one
  chunk rewrite).

### 5.4 Undo/redo

- Undo = apply the **inverse edit** (delete inserted range, re-insert deleted text).
- Coalesce typing runs (500 ms or word boundary); **programmatic edits do not
  coalesce** (one `apply` call = one undo step).
- Cap: 10k steps or **8 MB resident**. Larger inverse text spills to a temp file;
  if that is impossible, undo is disabled for that edit and reported.
- Redo is cleared on any new edit.
- **Undo is GUI/conformance-only in v1**; the CLI is one-shot and has no `undo`
  command. (CLI undo would require a session/`serve`, post-1.0.)
- At save: flatten the piece table, drop undo older than the save, clear the add
  buffer. Undo entries older than the save are discarded because their piece
  offsets are invalid after flatten. "Undo to last save" is the default; full
  session undo is opt-in.
- Undo/redo emit the `ChangeKind` of the resulting edit (`Delete`/`Insert`/
  `Replace`); `Save`/`Reload` are reserved for save/reload.

### 5.5 Save path (honest cost model)

v1 targets macOS/APFS; Windows/Linux fallbacks are post-1.0.

- **No-op if clean.**
- **Same-length edits**: clone the original inode into a temp
  (`clone_file(src_fd, tmp_fd)`, O(1) on APFS; O(file) elsewhere), `pwrite` the
  changed spans into the temp, then `fsync` → `atomic_replace` → `fsync_dir`.
- **Length-changing edits**: if the tail is ≤ 25% of the file, clone the original
  to a temp, `pwrite` the edited span + shifted tail into the **temp** at their
  new offsets, `ftruncate(temp, new_size)`, `fsync`, `atomic_replace`; otherwise
  full rewrite. Report the method.
- **Clone the original fd/inode**, not the path. The core holds the original fd
  from `open`; `fstat` it and compare to the open-time `Rev`. If the identity
  changed, fail `E_CONFLICT`; never mix old offsets with new bytes.
- Fallback: clone unavailable (non-APFS) → `copy_range`: stream the unchanged
  prefix/suffix ranges via `pread`/`pwrite` into a temp, write the edited span,
  rename; O(file), report `copy_range`. `full_rewrite` writes all bytes from the
  piece table. Never copy only a prefix and leave the suffix unwritten.
- Temp: `create_temp` returns a 0600 path + fd in the same directory; hold the
  fd from create through rename. After cloning and before rename, `fstat` the
  original and apply `fchown(tmp_fd, st_uid, st_gid)` **before**
  `fchmod(tmp_fd, st.st_mode & 07777)`, `fclonefileat(...,
  CLONE_ACL)` for ACLs, and copy xattrs. New files use `0666 & ~umask`. Never
  leave the temp at 0600. Rename needs directory write permission; read-only dir
  → `E_IO` or `--in-place`. A crash may leave a stale `*.editor-tmp-*`; it is
  ignored (or swept at startup).
- Metadata: `clonefileat`/`fclonefileat` with `CLONE_ACL` for ACLs; xattrs copied
  explicitly (or `copyfile` with `COPYFILE_XATTR`); `chown` before `chmod`;
  hardlink caveat → `--in-place`. Windows/Linux metadata post-1.0.
- Durability: `fsync(temp)` → `atomic_replace` → `fsync_dir`; macOS `F_FULLFSYNC`
  for power-loss durability (or `fsync` for crash-only). Post-rename `fsync`
  failure → `SavedButNotDurable` warning, `ok: true`; **never blind-retry**.
- **Post-save transition**: new base = saved bytes, clear add buffer, drop old
  undo, update identity/mtime/rev. Atomic w.r.t. other threads (under a mutex),
  not w.r.t. process crash; a crash **before** rename leaves the old file intact;
  after rename the new file is visible and the post-rename fsync/reload path
  handles durability. Double-save conformance test.
- **Gate**: same-length edit in 1 GB on APFS < 100 ms p99 with **crash-only
  `fsync`**; `F_FULLFSYNC` has a separate, looser budget. Length-changing edit
  measured separately.
- The JSON `save` field is one of `none` (clean no-op), `clone_pwrite`,
  `clone_pwrite_tail`, `copy_range`, `full_rewrite`; `--in-place` adds
  `"in_place": true`.

### 5.6 External mutation and mmap

- Record `(dev, ino, size, mtime_ns, ctime_ns)` at open.
- Rename-based replacement is safe (old inode stays mapped). In-place truncate
  causes `SIGBUS` on macOS; handle with a signal handler + `sigsetjmp(env, 1)`/
  `siglongjmp` around risky accesses (**no locks or allocations in the handler**),
  or `pread` for at-risk files. The handler guards only the base read-only
  mapping and must not longjmp across core frames; never hold a `Mapping` across
  a `reload`. Prefer `pread` for at-risk files; mmap is the opt-in fast path.
  Windows `EXCEPTION_IN_PAGE_ERROR` is post-1.0.
- On mismatch: auto-reload if clean; if dirty, `E_STALE` with a reload hint.
  `reload()` discards undo, maps new content, maps the caret to the nearest valid
  position.
- File watching (FSEvents) is post-1.0 and is **not** the source of truth; the
  identity check is.
- mmap policy: **sliding window** (256 MB–1 GB) for large files; `pread` for
  network FS; buffered `read()` under ~64 KB; **no `MAP_PRIVATE` writes** for
  large files (use the capped add buffer); base mapping is read-only; scans use
  `open_scan` (`F_NOCACHE`) + `pread`, or a bounded `mmap` window with
  `madvise(MADV_SEQUENTIAL)`; never mmap-the-world. The 1.68 GB RSS row in §7 is
  a naive full-mmap baseline, not the v1 policy.
- Snapshots are immutable and refcounted; mutations do not invalidate them, so
  parallel readers are safe while the owning thread edits.

## 6. CLI

Binary: `editor`. The GUI is `Editor.app`, linking the same core.

### 6.1 Commands

```
editor read <path> [--from N --to N | --from-offset B --length L]
                   [--around N --radius K] [--max-bytes N] [--stream]
editor write <path> (--stdin | @FILE) [(--expect REV | --force)] [--append] [--in-place]
editor edit <path> (--old TEXT|@FILE|- --new TEXT|@FILE|- [--all | --count N]
                   | --at LINE[:COL] --op insert|replace|delete
                     [--text TEXT|@FILE|-] [--length L | --end LINE[:COL]])
                   [--expect REV] [--force] [--dry-run] [--in-place]
# --op insert|replace require --text; --op replace|delete require --length or --end
editor info <path>
editor validate <path> [--max-errors N]
editor move <src> <dst> [--expect REV] [--force]
editor batch [--file F | --stdin] [--check] [--force]
editor version
editor schema [COMMAND]

# post-1.0 (sketches; schemas not frozen; non-exhaustive):
editor diff | restore | patch-apply | gui | tree | search | serve | run
# `gui` launches the GUI (not the core open); search is literal + glob only
```

**Global flags:** `--root DIR` (default cwd), `--allow-outside`,
`--max-file-bytes N` (input size cap, default 2 GB), `--allow-huge` (ignore
`--max-file-bytes`), `--max-results N` (cap on `batch` `results[]`/`errors[]`,
default 1000), `--verbose` (extra fields: timings). Global flags may appear
**before or after** the subcommand (`editor --root R read f` ≡
`editor read f --root R`). Always JSON on stdout (except `--stream` NDJSON); no
`--json`. Help/usage text goes to **stderr** and exits 0. `editor --version` is
an alias of `version`; `editor --help`/`editor help` and `editor <cmd> --help`
print usage to stderr.

**Units and defaults**

- `read`: `--from`/`--to` 1-based lines (defaults 1 and EOF); `--from-offset`/
  `--length` 0-based bytes (defaults 0 and EOF). The three axes are **mutually
  exclusive**; combining any two → `E_USAGE`. `--around N --radius K` selects
  lines `[max(1, N-K), min(line_count, N+K)]` (inclusive); `from`/`to` report
  those clamped **1-based line numbers**; `N` outside `1..line_count` →
  `E_NO_MATCH`. The byte axis reports 0-based `from`/`to`; the line axis reports
  1-based. A line-axis `--from` past EOF yields empty `content`,
  `truncated:false`, `ok:true`; `--radius 0` yields the single line `N`.
  `--max-bytes` is the **output cap** (default 256 KiB); if content
  exceeds it, return the first N with `truncated: true`, or use
  `--stream`/redirect. `--stream` ignores `--max-bytes` and emits one
  `{"schema":"editor.chunk/1","path":…,"from":…,"to":…,"content":…}` per
  line/chunk. Result: `content` or `content_ref` `{path, offset, length}`;
  `from`/`to` use the request axis; `truncated`, `rev`. Line-axis `content`
  **excludes** the UTF-8 BOM; byte-axis offsets are raw (offset 0 = BOM).
- `edit`: two mutually exclusive forms. (a) `--old TEXT|@FILE|- --new
  TEXT|@FILE|-` (search/replace, with `--all`/`--count N`); (b) `--at LINE[:COL]
  --op insert|replace|delete`. `--op` is only valid with `--at`; combining the
  forms → `E_USAGE`. With neither `--all` nor `--count`, exactly one match is
  required (multiple → `E_AMBIGUOUS`, zero → `E_NO_MATCH`); `--count N` requires
  **at least N** matches (fewer → `E_NO_MATCH`); `--count 0` → `E_USAGE`.
  `--op insert|replace` require `--text`; `--op delete` ignores `--text`;
  `--op replace|delete` require `--length` (bytes) or `--end LINE:COL` (0-based
  col, **end-exclusive**); `--length` and `--end` are mutually exclusive.
  `--force` overrides `--expect`; `--dry-run` reports the planned edit
  (`changed`, `bytes`, `line_count`, and the `save` method that would be used)
  plus `rev_before`, and **omits `rev`** (a stat-only rev cannot be predicted
  without writing). `changed.from/to` = first and last changed byte across all
  edits. Result: `changed`, `rev`, `bytes`, `bytes_written`, `save`,
  `in_place` (when set).
- `write`: a mutation. If `<path>` exists, `--expect` (or `--force`) is
  **required**; if it does not exist, neither is required and `--expect` is
  rejected with `E_USAGE`. Creates the file if missing; otherwise truncates by
  default; `--append` appends; `--in-place` for hardlinks. `--append` also
  requires `--expect`/`--force` on an existing file. Mismatch → `E_CONFLICT`
  (exit 2), file unchanged. Result: `bytes` (resulting file size),
  `bytes_written` (this call), `rev`, `save`.
- `info`: `path`, `bytes`, `line_count`, `eol`, `encoding`, `bom`,
  `final_newline`, `rev`, `dirty`.
- `validate`: `errors[]` of `{line, col, byte_offset, code}`; `--max-errors` 100.
  Codes: `invalid_utf8`, `unsupported`. `ok` is `true` iff `errors[]` is empty;
  non-empty → `ok:false`, exit **2**, `error` = the first error, and `errors[]`
  still lists all (up to `--max-errors`, with `"truncated": true`). Include `rev`.
- `move`: `--expect` refers to `src`; `--force` overwrites; if `dst` exists and
  neither `--expect` (on src) nor `--force` is given → `E_CONFLICT` (exit 2).
  Cross-device = copy + delete. Result: `from`, `to`, `rev` (dst).
- `batch`: default stdin; `--file F` reads from F; `--check` validates only;
  `--force` at op level; see §6.4. Result: per-op `results[]` + `rolled_back`;
  `--check` returns `results: []`, `valid`, and `errors[]`, and writes nothing.
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
      "path": "/path/to/editor/src/main.cpp",
      "rev_before": {"dev": "1", "ino": "2", "mtime_ns": "3", "ctime_ns": "4", "size": "4096"},
      "rev": {"dev": "1", "ino": "2", "mtime_ns": "5", "ctime_ns": "6", "size": "4112"},
      "bytes_before": 4096,
      "bytes": 4112,
      "bytes_written": 16,
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

- The **top-level** envelope always has `schema`, `command`, `ok`, `duration_us`,
  `cwd`, `warnings`, `results`, `error`, `meta`. Each element of `results[]` for
  a **file command** (`read`/`write`/`edit`/`info`/`validate`/`move`, and each op
  of `batch`) always has `path`, `rev`, `bytes`, `line_count`, `eol`,
  `encoding`, `bom`, `final_newline`; per-command lines list only extras.
- `version` and `schema` return a 1-element `results[]` containing only their
  documented fields (`version`, or `commands[]`) and **omit** the file fields
  (never emit `null`). `batch --check` returns `results: []` plus top-level
  `valid` and `errors[]`.
- `rev` is stat-only in v1 and always present for file commands. Each `rev`/
  `rev_before` field is a **decimal string** (the nanosecond fields exceed 2^53):
  `{"dev":"1","ino":"2","mtime_ns":"1700000000123456789","ctime_ns":"…","size":"4096"}`.
  `--expect` accepts `stat:<dev>:<ino>:<mtime_ns>:<ctime_ns>:<size>` and the
  object form with string or number values. Local/advisory; `--force` overrides.
- `path` is always the **absolute, symlink-resolved (`realpath`)** path of the
  target, identical regardless of how it was spelled on argv.
- `changed.from`/`to` are **raw file byte offsets** (BOM included); `line_count`
  is logical lines.
- Read-only file commands include `rev`; `rev_before`/`changed` appear only on
  mutations.
- All mutations (`write`, `edit`, `move`, and each `batch` op) always include
  `rev_before`, `rev`, `bytes`, `bytes_written`, `save`, `in_place` (when
  `--in-place`), and `changed` (when applicable). `--verbose` adds only `timings`.
- Deterministic: sort by path, then first byte offset; fixed field order; no
  `unordered_map` iteration; no wall-clock timestamps (file mtime/ctime are in
  `rev`); golden tests normalize `duration_us`/`meta.pid`/abs paths.
- Version: `editor.result/1`; additive within a major; unknown major → exit 3,
  `E_SCHEMA`. `editor schema [COMMAND]` dumps command schemas
  (`results[0].commands[]`); `editor version` returns `results[0].version`.
- Stream chunks use `editor.chunk/1` = `{schema, path, from, to, content,
  truncated?, rev?}`; `from`/`to` use the request axis; unknown major → exit 3,
  `E_SCHEMA`.
- EOL casing: enum `Eol` -> lowercase JSON (`lf`, `crlf`, `cr`, `mixed`).
  `Encoding` -> `"utf-8"`, `"utf-16le"`, `"utf-16be"`, `"other"`.
- `warnings[]` elements are `{code, message, path?}`; `SavedButNotDurable` →
  `{"code":"E_SAVED_NOT_DURABLE","message":"post-rename fsync failed","path":…}`
  with `ok:true`, exit 0.
- Large outputs: see `read` in §6.1 (output cap, `--stream`, `content_ref`).

### 6.3 Errors and exit codes

Core `Status` covers document/IO; the CLI adds `E_USAGE`, `E_SCHEMA`, `E_PARSE`,
`E_OUTSIDE_ROOT`, `E_NO_MATCH`, `E_AMBIGUOUS`. `E_LOCKED` is `Status::Locked`.
`E_*` = SCREAMING_SNAKE_CASE of the Status name. Errors carry `code`, `message`,
`retryable`, `path`, `hint`, and command-specific fields.
`E_INVALID_UTF8` includes the byte offset; `E_NO_MATCH`/`E_AMBIGUOUS` include at
most 20 entries of `{line, col, byte_offset}` (1-based, CLI).
`E_CONFLICT` = `--expect` mismatch; `E_LOCKED` = lock contention
(`Status::Locked`); `E_STALE` = external mutation while dirty and auto-reload is
not possible.

| Exit | Meaning | Examples |
| --- | --- | --- |
| 0 | ok | `Ok`, `SavedButNotDurable` (warning) |
| 2 | domain | `E_IO`, `E_NOT_FOUND`, `E_RANGE`, `E_CONFLICT`, `E_STALE`, `E_INVALID_UTF8`, `E_BINARY`, `E_OUTSIDE_ROOT`, `E_PARSE`, `E_TOO_LARGE`, `E_LOCKED`, `E_UNSUPPORTED`, `E_BUFFER_TOO_SMALL`, `E_NO_MATCH`, `E_AMBIGUOUS` |
| 3 | usage/schema | `E_USAGE`, `E_SCHEMA` |
| 4 | internal | `E_OOM`, `E_INTERNAL` |

`validate` with a non-empty `errors[]` → `ok:false`, exit **2**, `error` = the
first error. `batch --check` with `valid:false` → exit **2**.

```json
{"schema":"editor.result/1","command":"edit","ok":false,"duration_us":900,
 "cwd":"/path/to/editor","warnings":[],
 "results":[],
 "error":{"code":"E_CONFLICT","message":"file changed since read","retryable":true,
          "path":"/path/to/editor/src/main.cpp",
          "expected_rev":"stat:1:2:3:4:4096",
          "actual_rev":"stat:1:2:5:6:4112","hint":"re-read, then retry"},
 "meta":{"editor_version":"0.1.0","pid":48211}}
```

### 6.4 Batch and concurrency

`batch` reads a JSON-lines program; each op is one object:

```json
{"op":"edit","path":"a.cpp","old":"let x = 1;","new":"let x = 2;","expect":"stat:1:2:3:4:4096"}
{"op":"write","path":"b.cpp","content":"...","force":true}
{"op":"move","src":"a.cpp","dst":"c.cpp","expect":"stat:1:2:3:4:4096"}
```

- Ops: `write`, `edit`, `move`. `write` uses `content`/`content_file`/
  `content_stdin` plus `append`/`in_place`/`expect`/`force`. `edit` uses
  `old`/`new`/`text`/`at`/`op`/`length`/`end`/`count`/`all`/`dry_run`/`expect`/
  `force`. `count` and `all` are mutually exclusive; `count: N` requires **at
  least N** matches (fewer → `E_NO_MATCH`); `count: 0` → `E_USAGE`;
  `op:"replace"` without `text` → `E_USAGE`.
- **Validation-stage atomicity**: parse/validate every op against the original
  files; any failure → no writes. Before each commit, rename the original to a
  same-dir backup. Stage each file write to a temp; commit (rename) each. On
  failure, restore from the backup; delete backups on success. If rollback fails,
  return `ok: false`, `rolled_back: false`, and `results[]` for the applied ops.
  Buffer responses until commit. `--check` validates only. **Not crash-atomic.**
- Ops see the original files; two ops on the same file must not overlap
  (`E_RANGE`); disjoint ops on the same file are merged into one write.
- `batch` adds `rolled_back` (bool) and per-op `results[]` to the envelope;
  `--check` returns `results: []`, `valid` + `errors[]` and writes nothing.
- Mutations require `--expect` on existing files; `--force` (command or op)
  overrides. `write` to a non-existent path needs neither. Mismatch →
  `E_CONFLICT`, exit 2. **GUI**: re-check the on-disk `rev` before save;
  `Editor::save` performs the expect-check + write + rename under an OS lock, so
  the GUI does not need a separate lock.
- Lock: `<path>.editor.lock`, `O_CREAT|O_EXCL`, 0600, random token + pid + start
  time; stale by pid liveness + token; held for the expect-check + write + rename
  critical section.
- Symlinks: resolve, re-check inside `--root`, `O_NOFOLLOW` + `realpath` on the
  final component; never rename over a symlink.
- Refuse non-regular files; `--max-file-bytes` unless `--allow-huge`.
- See §6.3 for `E_CONFLICT`/`E_LOCKED`/`E_STALE` semantics.

### 6.5 Security

- **Scripts** (post-1.0): arbitrary code execution by design; trust prompt /
  `--yes`; never from GUI, `apply`, or config; allowlist in `.editor/scripts/`.
- **No user-editable config file in v1.** GUI preferences via `NSUserDefaults` are
  allowed. If a config file is added post-1.0: data only, versioned, no `exec`.
- **Patches** (post-1.0): typed AST, hard caps (files, hunks, line length, bytes);
  reject absolute paths and `..`; resolve against `--root`; handle
  `\ No newline at end of file` and `/dev/null`; fuzz.
- **Secret leakage**: argv is visible in `ps`; prefer `--old @FILE --new @FILE`
  (or `--text @FILE`/`-` for an `--at` insert), and `@FILE`/`--stdin` for
  `write`/`batch`; never echo file contents or matched lines in errors.
- **DoS defaults (v1)**: `--max-bytes` (read output), `--max-file-bytes`/
  `--allow-huge` (input), `--max-results` (batch), `--max-errors` (validate),
  lock TTL, symlink-cycle detection. Post-1.0: search timeout, tree-walk depth.
- **`serve` (post-1.0)**: Unix socket 0600, per-session token, timeouts.

### 6.6 Registry invariant

Registry entry = `{ name, arg_schema, core_fns: [ids], cli_binding, gui_binding?,
cli_excluded? }`.

- A `core_fn` with no `cli_binding` and no `gui_binding` is a test failure
  (except an explicit `cli_excluded` allowlist).
- A `cli_binding` must name **≥1** `core_fn`; composed commands list all
  (`write` = `create`+`apply`+`save`; `edit` = `apply`+`save`; `batch` = the ops
  it dispatches).
- `move`, `validate`, `version`, `schema` are CLI-only entries with
  `core_fns: []` and are listed so `schema` can generate them.
- `save_as`/`reload`/`undo`/`redo` are GUI/conformance-only in v1; `create` has
  no dedicated CLI command but is used internally by `write`.
- GUI-only ops (find/replace) are registered with `core_fn = null` and an explicit
  `gui_only` allowlist; the golden menu test covers them.
- A golden test asserts **every GUI menu item maps to a domain op id** (or a
  `gui_only` entry).
- `editor schema` is generated from the registry.

## 7. Budgets and measurement

**Metric:** macOS `phys_footprint`; Windows private working set; Linux PSS
(post-1.0). Not directly comparable; compare the same workload.

**Measured (macOS, Apple Silicon, 8 GB)**

| Fact | Measurement |
| --- | --- |
| Empty window (shown, one draw) | 40.6 MB RSS / 10.4 MB footprint |
| mmap 2 GB untouched | +0 MB, 0.010 ms |
| mmap 2 GB full scan (naive baseline) | **1.68 GB RSS** |
| `CTLineCreate` cold line | 9.2 µs |
| Cached CoreText line (per visual line) | ~3.1 KB each |
| 40 cached line draws × 60 | 0.26 ms/frame |
| Cold shape+draw | 0.43 ms/frame |
| One font resolve | 1.6 ms |
| First `CTFontCreateWithName` | 16.7 ms |
| Font enumeration (175) | 80.8 ms |
| Time to first window | 60–390 ms |

The 1.68 GB row is a naive full-mmap baseline; v1 uses the §5.6 bounded
windows/`pread` policy.

**Budgets (per-component caps; the loaded-resident row is a separate combined cap)**

| Metric | Cap |
| --- | --- |
| Empty-window baseline | ≤ 12 MB footprint (RSS is higher; measured 40.6 MB) |
| + line cache (1000–1900 entries **and** ≤ 6 MB; evict on either) | ≤ 6 MB |
| + undo/edit log (resident portion) | ≤ 8 MB (overflow spills to disk) |
| + add buffer (resident portion) | ≤ 8 MB (overflow spills to disk) |
| + sparse line index (5M lines, incl. tree nodes) | ≤ 2 MB |
| + sparse line index (25M lines, incl. tree nodes) | ≤ 14 MB |
| **Loaded, resident (1 GB file with ≤5M lines, i.e. ≥200 B/line; cache + undo + index + add buffer)** | **≤ 40 MB** |

Spilled undo/add-buffer data is disk, not footprint; the 40 MB cap is resident
only and assumes ≤5M lines. At 25M lines the index alone is ~9 MB (≈14 MB with
nodes); at 125M it is ~47 MB (≈72 MB), so the 40 MB cap does not apply. Add a
line-count budget or a hard line cap for the GUI.
Post-1.0, the content-mmap LRU (50–60 MB) is additional.

**Performance**

| Operation | Budget |
| --- | --- |
| Idle CPU | < 0.05% (caret blink only) |
| Scroll, cached | ≤ 3 ms/frame (18% of a 16.7 ms frame budget) |
| Scroll, cold | ≤ 8 ms/frame (48%) |
| Edit to model / frame | < 1 ms / < 3 ms |
| Open 1 GB read-only | < 100 ms to first paint |
| Open 5 GB read-only | < 250 ms to first paint; scroll ≥60 fps |
| Same-length edit in 1 GB (APFS) | < 100 ms p99 |
| Length-changing edit in 1 GB | O(tail) or O(file) on full rewrite, measured separately |
| Scan 1 GB (cold, `F_NOCACHE` + `pread`) | 250–500 ms |
| Startup warm / cold | < 150 ms / < 400 ms |

**Method:** `CLOCK_MONOTONIC` around frames; `powermetrics`/`os_signpost` for CPU;
assert **p99**, not average. Measure `F_FULLFSYNC` (power-loss) vs `fsync`
(crash-only) separately. Bench 5M/25M-line, 5 GB, 10k-file (post-1.0), 100 MB
single line. Separate warm (index built) from cold (first random edit).

## 8. Rendering

- **Custom view only** on macOS in v1, driven by `viewcore` + `textbackend`
  (CoreText). No native-control hybrid; native control is a post-1.0 experiment.
- The view owns selection, caret, scroll; the core owns bytes and edits.
- **No IME and no accessibility in v1** — keyboard + mouse only. Both are
  significant platform work and are not needed for a personal editor.
- `viewcore` API (sketch): `set_document`, `set_snapshot`, `resize`,
  `set_wrap`, `set_font`, `set_decorations`, `selection`/`set_selection`,
  `first_visible_line`, `scroll_to`, `draw`, `key`, `mouse`. `textbackend`:
  `measure`, `shape` into runs, `draw` into a platform `Canvas`. `Canvas`:
  `fill_rect`, `draw_run`, … (platform-neutral; `platform/macos` implements it).
- Line cache keyed by **(logical line, wrap width)**; one shaped run per *visual*
  line. **Bounded LRU, 1000–1900 entries and ≤ 6 MB**; skip caching lines above
  ~1000 visual lines (a 100 MB single line must not blow the cache).
- Invalidate at/after the first changed line; on a line-count-changing edit, clear
  from the first changed line onward (rekeyed). Invalidate all on width/font change.
- **Do not enumerate fonts at launch** (80.8 ms); resolve one (1.6 ms). First
  `CTFontCreateWithName` can cost 16.7 ms. Enumerate lazily when the font menu opens.
- Monospace default; OS fonts (no bundled font in v1).

## 9. Workspace, search, and future

**All post-1.0.** v1 has no workspace, tree command, or **project** search.
In-file find/replace is a v1 **GUI-only** feature over `Snapshot` (literal, no
regex); it has no core search API and no CLI command.

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
- Golden CLI: exact JSON for every command after the §6.2 normalizations,
  including errors.
- Conformance: `Editor` (open/create/info/read_lines/read_range/apply/undo/redo/
  save/save_as/reload/rev/snapshot/on_change/off_change/drain/errmsg/
  CancelToken/change), `viewcore` (§8 API: wrap/hit-test/selection/scroll/
  decorations), `textbackend` (measure/shape/draw), `Canvas` (fill_rect/draw_run),
  `Mapping` (window/data/size), `FileSystem` (open_readonly/open_write/open_scan/
  create_temp/close_fd/map_readonly/unmap/pread/pwrite/ftruncate/fchmod/fchown/
  fstat/fsync/fsync_dir/clone_file/atomic_replace/rename/unlink/identity/lock/
  unlock).
- Sanitizers: ASan/UBSan every PR (lite); TSan nightly on core/viewcore only
  (ObjC++ excluded); hardened STL in debug.
- Fault injection: `fsync`/`rename`/`pwrite` failures; crash-consistency of
  temp→rename; batch rollback; lock/TOCTOU two-process stress; external truncate.
- Benchmarks: cold start, open 1 GB, same-length edit, length-changing edit,
  viewport edit, scan 1 GB, 5M/25M-line, 5 GB, 10k-file (post-1.0), 100 MB
  single line; footprint gates.

**CI:** macOS smoke every PR; perf nightly; release engineering as M3.5.

**Build:** one CMake graph (Xcode generator on macOS); artifacts `libcore.a`,
`libviewcore.a`, `editor` CLI, `Editor.app`; C++20 with the pinned toolchain and
`CXX_STANDARD 20`, `CXX_STANDARD_REQUIRED ON`, `CXX_EXTENSIONS OFF` (see
appendix); notarization (hardened runtime, `notarytool`, staple); **Apple Silicon
only** in v1. No vendored third-party.

## 11. Roadmap

MVP = **macOS M0–M3.5**. Windows/Linux/search/syntax are post-1.0.

**v1 checklist:** open/save, edit, undo/redo, save_as/reload/create (GUI or
conformance-only), find/replace (GUI-only, literal), font size + tab width + wrap
settings, status bar, gutter, decorations, and the CLI (`read`, `write`, `edit`,
`info`, `validate`, `move`, `batch`, `version`, `schema`).

| Milestone | Scope | Est. | Gate |
| --- | --- | --- | --- |
| **Pre-M0.0** | Freeze `editor.hpp` + C++ conformance test + §5.1–5.3 data-model rules + test/JSON decisions. | 1 wk | Interface compiles; conformance skeleton runs. |
| **M0a** | Piece table + sparse index + core `apply` + `read`/`write`/`info` + root/symlink checks + same-length CoW save + JSON + basic Status→`E_*` mapping. | 3–4 wk | Open + read + write + reopen + read round-trip; golden JSON; 1 GB same-length `apply`+`save` < 100 ms p99 (APFS, crash-only `fsync`). |
| **M0b** | `edit` + undo + length-changing save + fault injection. | 3–4 wk | Edit/undo property tests; double-save and save-crash tests. |
| **M0c** | `batch` + `move` + `validate` + security + full error model (`E_NO_MATCH`/`E_AMBIGUOUS`) + batch symlink handling/TOCTOU + registry invariant. | 2–3 wk | Batch validation/rollback; fuzz; CLI golden. |
| **M0.5** | Headless render oracle + viewcore skeleton + **5 GB read-only demo** + C++ validation + conformance check. | 2 wk | 5 GB at 60 fps, flat footprint; sanitizers clean; conformance pass; D1 kill-gate decision recorded. |
| **M1** | macOS read-only custom view: AppKit + CoreText + viewcore; keyboard/mouse. | 4–6 wk | Open 1 GB to first paint < 100 ms; scroll 5 GB at ≥60 fps. |
| **M2** | macOS editing + undo + save + **GUI-only find/replace** (literal, over `Snapshot`) + settings (font size, tab width, wrap via `NSUserDefaults`; keybindings hardcoded). | 4–6 wk | v1 checklist: edit, undo, find/replace, settings. |
| **M3** | macOS polish: status bar, gutter, decorations. | 3–4 wk | v1 checklist complete. |
| **M3.5** | Release engineering: notarization, packaging, CI perf gates. | 2–3 wk | Signed `.app`; CI green. |
| **Post-1.0** | Windows port, Linux port, workspace/tree, project search, syntax, Markdown, git, scripts, `serve`, diff/restore/patch-apply/gui/tree/search, native-control experiment. | — | — |

**Total:** the table sums to **24–33 weeks of work**. With ~30% overhead that is
**31–43 weeks full-time (~7–10 months)**; solo at ~75% time, plan on **9–12
months**; half-time is **14–20 months**.

**Cut list if behind (v1 cuts first):**

1. M3 polish (decorations, status bar, gutter) → ship M2-level editor.
2. M2 find/replace → ship without it.
3. Do **not** cut: save correctness, fault injection, basic editing/undo.

## 12. Risks and open decisions

| Risk | Mitigation |
| --- | --- |
| Length-changing save is O(tail) or O(file) | Honest cost model; tail/full-rewrite threshold; benchmark both. |
| Hand-rolled JSON bugs | Fuzz + property tests; keep the parser small and strict. |
| Schedule (~9–12 months solo at ~75%) | macOS-only MVP; cut list; post-1.0 fence. |
| Temptation to re-add hybrid/native control | D2 decision + post-1.0 trigger. |
| mmap SIGBUS on truncate | Signal handler/pread; identity check; reload. |
| External mutation race | ctime-aware rev; lock covers expect-check + write + rename; auto-reload clean. |
| Stat-only `rev` collision (same-size save in one mtime/ctime tick) | Advisory `rev`; `--in-place` opt-in; lock; generation/hash post-1.0. |
| No IME/a11y limits CJK/screen-reader use | Accepted for v1; post-1.0 if needed. |

**Open decisions**

1. **Bundled font (post-1.0)**: OS fonts (default for v1) vs. +200–800 KB for
   identical metrics.

Resolved decisions are in §1, §2, §4, §5, §8, and §10.

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

**M0.5 kill gate:** if C++ bug density stays high despite sanitizers/fuzzing, Rust
velocity/seam is clearly better, or the untrusted surface grows, switch to Rust.

**C++ standard: C++20, confirmed by three blind reviews (Nov 2026).** All three
independently ranked C++20 1st (~85%), C++23 2nd (~60%), C++26 3rd/reject (~90%).
Everything the project needs — `std::span`, `std::string_view`, `std::format`,
`std::atomic`, `std::charconv`, `std::source_location`, `std::bit_cast` — is C++20.
C++23's `std::expected` would fight the `Status` + out-param + `errmsg` design and
force an interface re-freeze; C++26 is not ISO-published, has no stable MSVC mode,
and Apple's Cocoa headers fail under `-std=c++26` on Xcode 26.2. Pin the toolchain
(Apple Clang 21 / Xcode 26.x, MSVC 14.50–14.52 / VS 2026 LTSC, GCC 16.1 or
Clang 22) and set `CXX_STANDARD 20`, `CXX_STANDARD_REQUIRED ON`,
`CXX_EXTENSIONS OFF`. C++23 is a later option only if we deliberately adopt
`std::expected`; C++26 is a post-1.0 experiment.