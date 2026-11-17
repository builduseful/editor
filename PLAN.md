# editor — Plan (v6.3)

> Name: **editor**. Status: planning. v6.3 — audit cuts + interface fixes applied.
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
| D13 | **Build Useful design system is a brand reference.** Follow its tokens/principles for brand consistency; do not vendor its web CSS/JS or bundle its code. Applies to chrome/app shell; the document view stays custom-drawn (D2). | The system's direction changes, or a native component library is adopted. |

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
  `info`, `validate`, `batch`, `version`, `schema`), emits machine-readable
  output, and shares domain ops with the GUI. GUI/conformance-only in v1:
  undo/redo/reload/save_as; `create` is used internally by `write`.
- Visual design follows the Build Useful design system (D13) for brand
  consistency.
- No vendored/bundled third-party code; OS system libraries and the C++
  standard library allowed.

**Non-goals (v1)**

- No Windows/Linux, no Intel Macs (Apple Silicon only in v1), no TUI, no
  user-editable config file, no regex, no SHA-256, no IME, no a11y.
- No plugins/mods, network sync, accounts, telemetry.
- No native-control hybrid.
- No multi-cursor, command palette, split panes, or GUI project tree.
- No Markdown rendered editing, git UI, script runner, or `serve` (post-1.0).
- No file-management command (`move` is post-1.0; use `mv`).
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
- **GUI session** — one `Editor` plus undo/change state; not a core type in v1.
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
#include <cstddef>
#include <cstdint>
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
  uint64_t dev = 0, ino = 0, size = 0, mtime_ns = 0, ctime_ns = 0;
};

struct Stat {
  uint64_t dev = 0, ino = 0, size = 0;
  uint32_t mode = 0, uid = 0, gid = 0;
  uint64_t mtime_ns = 0, ctime_ns = 0;
  bool regular = false;
};

struct Info {
  std::string_view path;           // borrowed; valid until next non-const call
  uint64_t bytes = 0;              // on-disk size, BOM included
  int64_t line_count = 0;
  Eol eol = Eol::Lf;
  Encoding encoding = Encoding::Utf8;
  bool bom = false, final_newline = true, dirty = false;
  Rev rev;
};

struct Change {
  int64_t first = 0, last = 0, new_count = 0;  // [first,last) line indices
  ChangeKind kind = ChangeKind::Insert;        // Save/Reload: invalidate all
};

struct Edit {
  int64_t line = 0, col = 0;       // 0-based; col = UTF-8 byte offset
  int64_t removed_len = 0;         // bytes; 0 for insert
  std::string_view text;           // borrowed for the call only
};

struct SaveInfo {
  const char* method = "none";     // none|clone_pwrite|clone_pwrite_tail|copy_range|full_rewrite
  uint64_t bytes_written = 0;
  uint64_t first_changed = 0;      // raw file offset, BOM included
  uint64_t last_changed = 0;
  bool in_place = false;
  bool durable = true;             // false -> Status::SavedButNotDurable
};

class Editor {
 public:
  static constexpr uint32_t kInterfaceVersion = 1;
  virtual ~Editor() = default;

  virtual Status open(std::string_view path_utf8) = 0;
  // Create a new empty file. Existing path -> E_CONFLICT. Never truncates.
  // The new file becomes the open document (empty, clean); the path must be
  // absolute and resolved. No expected-rev check (nothing existed).
  virtual Status create(std::string_view path_utf8) = 0;
  virtual Status info(Info* out) const = 0;
  virtual Rev rev() const = 0;

  // first inclusive, last exclusive. buf=nullptr -> size query
  // (`used`=needed, `out_n`=lines in [first,last) clamped).
  // On success `used`=bytes written, `out_n`=lines read. On BufferTooSmall:
  // no bytes written, `used`=full needed, `out_n`=0. Returns line bytes
  // INCLUDING the terminator. `first >= last`, `first >= line_count`, or
  // `last <= 0` -> empty/Ok. `last` clamps.
  virtual Status read_lines(int64_t first, int64_t last, char* buf, size_t cap,
                            size_t* used, int64_t* out_n) const = 0;
  // buf=nullptr -> size query (`used`=needed); `off >= size` -> empty/Ok;
  // `len` clamps to EOF. On BufferTooSmall: no bytes written, `used`=needed;
  // else `used`=bytes read. `read_range` is raw file bytes (BOM included).
  virtual Status read_range(int64_t off, size_t len, char* buf, size_t cap,
                            size_t* used) const = 0;

  // All edits in one call are relative to the state at the start of that call
  // (shared base). The caller must sort; unsorted or overlapping edits ->
  // E_RANGE. Edits at the same offset overlap, including zero-length inserts.
  // `removed_len` may cross lines. `line > line_count`, `col` outside the line
  // (except append at line_count/col 0), or a removed range past EOF -> E_RANGE.
  // Inserted newlines use the dominant EOL.
  virtual Status apply(std::span<const Edit> edits) = 0;

  virtual Status undo() = 0;       // empty stack -> Ok, no change
  virtual Status redo() = 0;       // empty stack -> Ok, no change
  // save: no-op if clean and expected_rev (if given) matches the current
  // on-disk Rev; method="none". An expected_rev mismatch, or a changed base
  // inode/path (§5.5), is E_CONFLICT — save never auto-reloads. Auto-reload of
  // a clean file after external mutation is check_identity()'s job (§5.6).
  // in_place writes the original inode; otherwise clone/temp.
  virtual Status save(const Rev* expected_rev = nullptr, bool in_place = false,
                      SaveInfo* out = nullptr) = 0;
  // save_as: no-op if clean and dst is the same file; expected_rev is checked
  // even when clean. With in_place, dst must be an existing regular file to
  // rewrite in place (a nonexistent dst is E_NOT_FOUND; use in_place=false to
  // create). Refuses a symlink or non-regular dst (lstat, no-follow) with
  // E_CONFLICT; refuses to overwrite an existing dst with E_CONFLICT unless
  // expected_rev matches dst's identity; if dst does not exist, expected_rev
  // must be null (otherwise E_CONFLICT). On success the open path becomes dst
  // (relative dst resolves against the current dir, then realpath).
  virtual Status save_as(std::string_view path_utf8,
                         const Rev* expected_rev = nullptr, bool in_place = false,
                         SaveInfo* out = nullptr) = 0;
  virtual Status reload() = 0;
  // Check the on-disk identity; auto-reload if clean, E_STALE if dirty.
  virtual Status check_identity() = 0;

  using ChangeFn = void (*)(Editor*, const Change&, void*);
  virtual uint64_t on_change(ChangeFn, void* ud) = 0;   // id, reused after wrap
  virtual void off_change(uint64_t id) = 0;             // unknown id: no-op
  virtual Status drain() = 0;          // callbacks fire ONLY here
  virtual std::string_view errmsg() const = 0;  // thread-local, borrowed
};

class Mapping {
 public:
  virtual ~Mapping() = default;
  virtual Status window(uint64_t off, size_t len) = 0;  // clamps to EOF
  // data()/size() are valid until window() or unmap(). off >= size or len == 0
  // -> empty window (data()==nullptr, size()==0). unmap before close_fd.
  virtual const char* data() const = 0;
  virtual size_t size() const = 0;
};

class FileSystem {
 public:
  virtual ~FileSystem() = default;
  virtual Status open_readonly(std::string_view path, bool nofollow,
                               int* out_fd) = 0;
  virtual Status open_write(std::string_view path, bool create, bool truncate,
                            bool nofollow, int* out_fd) = 0;
  virtual Status open_scan(std::string_view path, bool nofollow,
                           int* out_fd) = 0;  // sets F_NOCACHE before return
  virtual Status open_dir(std::string_view path, int* out_fd) = 0;
  virtual Status create_temp(std::string_view dir, std::string* out_path,
                             int* out_fd) = 0;  // O_CREAT|O_EXCL, 0600
  // Clone src_fd into a new path in dst_dir; the new path must not exist.
  // Returns the clone's fd. O(1) on APFS.
  virtual Status clone_to_new(std::string_view dst_dir, std::string* out_path,
                              int src_fd, int* out_fd) = 0;
  virtual void close_fd(int fd) = 0;
  virtual Status map_readonly(int fd, Mapping** out) = 0;
  virtual Status unmap(Mapping*) = 0;
  virtual Status pread(int fd, uint64_t off, char* buf, size_t cap,
                       size_t* used) = 0;
  virtual Status pwrite(int fd, uint64_t off, const char* buf, size_t len,
                        size_t* used) = 0;
  virtual Status ftruncate(int fd, uint64_t size) = 0;
  virtual Status fchmod(int fd, uint16_t mode) = 0;  // setuid/setgid not preserved
  virtual Status fchown(int fd, uint32_t uid, uint32_t gid) = 0;
  virtual Status fstat(int fd, Stat* out) = 0;
  virtual Status fsync(int fd, bool full) = 0;
  // Copy mode/ownership/ACLs/xattrs from src_fd to dst_fd (chown before chmod).
  virtual Status copy_metadata(int src_fd, int dst_fd) = 0;
  // tmp_path must be in dst_dir; rename its basename to dst_dirfd/dst_name,
  // replacing dst atomically.
  virtual Status rename_at(int dst_dirfd, std::string_view dst_name,
                           std::string_view tmp_path) = 0;
  virtual Status fsync_dir(int dirfd) = 0;
  virtual Status unlink(std::string_view path) = 0;
  virtual Status identity(std::string_view path, Rev* out) = 0;  // follows symlinks
  virtual Status resolve(std::string_view path, std::string* out_realpath) = 0;
  virtual Status lstat(std::string_view path, Stat* out) = 0;    // no-follow
  virtual Status lock(std::string_view path, void** lock) = 0;
  virtual void unlock(void* lock) = 0;  // unlock(nullptr) is a no-op
};
```

**Ownership.** A successful `open_*`, `open_dir`, `create_temp`, `clone_to_new`,
or `map_readonly` transfers the fd/mapping to the caller. On any later `Status`
failure the caller closes it. `Editor::open` closes its fd on every failing path;
`~Editor` closes the base fd, the add-buffer fd, and all spill fds. Use an RAII
`OwnedFd` in core.

**Rules**

- Every fallible call returns `Status`; `close_fd`/`unlock` are void and
  no-op-safe. No `void` mutation/save.
- `(ptr, len)` / `string_view` / `span` everywhere; no NUL-terminated input.
- `int64_t` for document lines/byte offsets; `uint64_t` for file offsets/sizes;
  `uint16_t` for mode. `0` is a valid line/length, never an error. Negative
  document values → `E_RANGE`. A successful `open`/`create`/`save_as` stores an
  absolute, symlink-resolved (`realpath`) open path; a relative `save_as` dst
  resolves against the current directory first.
- Core is 0-based; CLI is 1-based. `col` is a UTF-8 byte offset.
- `open`/`create`/`save` use `nofollow=true` and reject non-regular files
  (`Stat::regular=false` → `E_IO`).
- `apply` validates UTF-8 and overlap; violations → `E_INVALID_UTF8`/`E_RANGE`.
- An `apply` call pushes exactly one undo step.
- Core never calls the UI; it appends changes and the UI calls `drain()`.
- Callbacks must not call `on_change`/`off_change`/`apply`/`save`/`drain`; they
  may only copy the `Change`. Callbacks are `noexcept` by contract; a throwing
  callback is caught at the boundary and `drain()` returns `Status::Internal`.
- Exceptions must not escape; catch at every boundary → `Status::Internal`. A
  half-applied edit must leave the editor consistent.
- One `Editor` per thread; mutations are serialized by the owning thread.
- `rev()`/`Info::rev` return the last on-disk identity observed at `open` or
  after the last successful `save`/`reload`; not updated by unsaved edits.
- `Status::BufferTooSmall` is returned by `read_lines`/`read_range` when a
  non-null buffer is smaller than needed (no bytes written, `used`=needed);
  `buf=nullptr` is the size query. `FileSystem::pread`/`pwrite` clamp to EOF
  (short count in `used`, never `BufferTooSmall`).
- `errmsg` is thread-local and borrowed; no retention.
- Version tag `kInterfaceVersion`; core is statically linked into GUI/CLI.
- C++20, pinned toolchain; statically link core into GUI/CLI.
- Hardening: `_GLIBCXX_ASSERTIONS` / `_LIBCPP_HARDENING_MODE`, ASan/UBSan,
  `-Wall -Wextra -Wconversion` (MSVC `/sdl` post-1.0).

## 5. Document model

### 5.1 Bytes, lines, EOL, encoding

- A **line** is a byte range `[start, end)` excluding its terminator.
- `line_count` = terminators + (1 if the last line is non-empty). Empty = 0;
  `a\n` = 1; `a` = 1; `a\n\n` = 2.
- `final_newline` = true iff the file is non-empty and its last byte is a
  terminator: empty and `a` → false; `a\n` → true.
- **BOM:** a UTF-8 BOM is accepted, excluded from every logical line range, and
  reported as `bom: true`; a UTF-16 BOM → `E_UNSUPPORTED`. `col` is relative to
  the post-BOM start of line 1. `read_lines` excludes the BOM; `read_range` is
  raw file bytes; `changed.from/to` are raw file offsets (an edit at line 0,
  col 0 reports `from = BOM_len`). `Info::bytes` is the on-disk size including
  the BOM.
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

### 5.2 Piece table and add buffer

- Original file is an immutable base piece; edits append to an add buffer.
- **Coalesce** adjacent inserts at the add-buffer tail whose predecessor ends
  exactly at the append offset. Without it, typing creates unbounded pieces.
- Add buffer is **capped at 8 MB resident and spilled to a temp file**; compacted
  before save. A single insert larger than the cap spills and is not coalesced;
  the cap is resident, not per-edit. Non-tail inserts do not coalesce. An edit
  whose insert would push the resident add buffer over the cap spills the add
  buffer and restarts the insert in the fresh (disk-backed) buffer.
- "Edits are metadata" = piece descriptors + inserted bytes, not a document copy.
- The `Editor` owns the base fd, the add-buffer fd, and all spill fds; `~Editor`
  closes them.

### 5.3 Sparse chunk line index

- Balanced B-tree of chunks; each covers **up to 64 lines** and stores
  `line_count` (4 B) and `byte_len` (8 B, bytes of its lines including
  terminators, except a trailing CR whose LF is in the next chunk). Each tree
  node stores subtree sums of both (8 B each) for order-statistic lookup.
- A chunk's **absolute line/byte start** = sum of preceding chunks' counts/
  lengths, computed during the O(log n) descent. No absolute offsets are stored.
- An edit updates the touched chunk's count/len and the subtree sums **on the
  path** (O(log n)); later chunks are not rewritten.
- In-chunk scan uses the chunk's start line/byte (from the descent) to find the
  line. Chunks are line-aligned; a CRLF is never split.
- Chunk descriptors (12 B each) are resident for the whole file; the in-chunk
  scan reads the mapping/add buffer on demand. "Lazy" means the index is not
  built until the first line query/edit, not that descriptors are evicted.
- Node layout: 63-way B-tree (≈756 B/node + 16 B sums), ≈24 B/chunk total.
  5M lines ≈ 1.9 MB; 25M ≈ 9.4 MB. Chunk split/merge is specified and
  property-tested; each touched chunk is rewritten in O(64) bytes, and one
  `apply` costs O(changed_lines/64 + log n) chunk operations.

### 5.4 Undo/redo

- Undo = apply the **inverse edit** (delete inserted range, re-insert deleted text).
- Coalesce typing runs (500 ms or word boundary); **programmatic edits do not
  coalesce** (one `apply` call = one undo step).
- Cap: 10k steps or **8 MB resident**. Larger inverse text spills to a temp file;
  if that is impossible, undo is disabled for that edit, `errmsg()` reports it,
  and `undo()` returns `Status::Unsupported`.
- Redo is cleared on any new edit. Undo/redo on an empty stack return `Ok`
  with no change (no-op).
- **Undo is GUI/conformance-only in v1**; the CLI is one-shot and has no `undo`.
- At save: flatten the piece table, clear the add buffer, drop undo older than the
  save (piece offsets are invalid after flatten). "Undo to last save" is the only
  v1 behavior.
- Undo/redo emit the `ChangeKind` of the resulting edit (`Delete`/`Insert`/
  `Replace`); `Save`/`Reload` are reserved for save/reload and mean "invalidate
  everything."

### 5.5 Save path (honest cost model)

v1 targets macOS/APFS; Windows/Linux fallbacks are post-1.0. All paths hold the
expect-check + write (+ rename) under the OS lock, including the `expected_rev`
comparison and the post-rename identity check, but not the post-save
`open_readonly` reopen. The core resolves the open path once, splits it into dir +
name, and opens the directory before the rename.

- **No-op if clean.** `SaveInfo::method = "none"`. `expected_rev` is still checked
  (mismatch → `E_CONFLICT`).
- **Same-length edits**: `clone_to_new(dir, &tmp, src_fd, &tmp_fd)` (O(1) on APFS;
  O(file) elsewhere), `pwrite` changed spans into `tmp_fd`, `fsync`, `rename_at`,
  `fsync_dir`. `SaveInfo::method = "clone_pwrite"`.
- **Length-changing edits**: if the tail is ≤ 25% of the file, clone to a temp,
  `pwrite` the edited span + shifted tail into the **temp** at new offsets,
  `ftruncate(tmp_fd, new_size)`, `fsync`, `rename_at`, `fsync_dir`; otherwise full
  rewrite. `method = "clone_pwrite_tail"`.
- **Clone the original fd/inode**, not the path. The core holds the original fd
  from `open`; before cloning, require **both** `identity(path)` (path not
  replaced) and `fstat(src_fd)` (inode not truncated/rewritten in place) to
  match the open-time `Rev`. If either differs, fail `E_CONFLICT`; never mix old
  offsets with new bytes. `--force` skips the explicit `--expect` comparison,
  not this safety check.
- **Copy paths** (clone unavailable, non-APFS): `create_temp` (0600), stream the
  unchanged prefix/suffix ranges via `pread`/`pwrite` into the temp, write the
  edited span, `copy_metadata(src_fd, tmp_fd)`, `fsync`, `rename_at`,
  `fsync_dir`; O(file), `method = "copy_range"`. `full_rewrite` writes all bytes
  from the piece table. Never copy only a prefix and leave the suffix unwritten.
- **Clone carries mode/ownership/ACLs/xattrs**; do **not** call `fchmod` after a
  clone. `copy_metadata` is only for `copy_range`/`full_rewrite`; it does
  `fchown` before `fchmod`. Setuid/setgid are not preserved. New files use
  `fchmod(tmp_fd, 0666 & ~umask)` before rename.
- **In-place** (`--in-place`): `open_write(path, create=false, truncate=false,
  nofollow=true)`. **In-place requires an existing regular target**; a
  nonexistent `save_as` dst is `E_NOT_FOUND` in core / `E_USAGE` on the CLI
  (§6.1/§6.4). Creating a new file always uses `in_place=false` (temp + rename).
  Then `fstat` the new fd and require the same full `Rev`
  (`dev`, `ino`, `size`, `mtime_ns`, `ctime_ns`) as the applicable expected Rev
  — the open-time `Rev` for `save`, `expected_rev` (dst) for `save_as`
  (otherwise `E_CONFLICT`, no write). `pwrite` changed spans
  (or full rewrite), `ftruncate` for length changes, `fsync(original)`, update
  identity/rev. No temp/rename; `SaveInfo::method` is `copy_range` or
  `full_rewrite` with `in_place=true`. Expect-check under the lock. A failure may
  leave a partial file; report `E_IO`. Hardlinks and crash-safety are user opt-in.
- **Temp**: `create_temp` returns a 0600 path + fd in the same directory; hold the
  fd from create through rename. On any in-process failure, close and `unlink`
  the temp. Never leave the temp at 0600. A crash may leave a stale
  `*.editor-tmp-*`; it is ignored (or swept at startup). A read-only destination
  directory → `E_IO`; `--in-place` is the fallback.
- **Durability**: `fsync(tmp_fd, full)` → `rename_at` → `fsync_dir(dirfd)`; open
  the directory **before** the rename. If `fsync(full=true)` returns
  `Status::Unsupported`, retry with `full=false` and set `durable=false`. A
  post-rename failure → `SavedButNotDurable` warning, `ok: true`, `durable=false`;
  **never blind-retry**. The warning is reported only on the call that produced it.
- **Post-save transition**: close the pre-save base fd, reopen the path with
  `open_readonly(path, nofollow=true, &fd)` under the same lock, and verify the
  `fstat` matches the new `Rev`; on mismatch, `E_CONFLICT`. If the reopen itself
  fails, return `E_IO` and require `reload()`/`open()` before further edits (the
  on-disk save succeeded). Update identity/mtime/rev. Atomic w.r.t.
  other threads, not w.r.t. process crash; a crash **before** rename leaves the
  old file intact; after rename the new file is visible. Double-save test.
- **Gate**: same-length edit in 1 GB on APFS < 100 ms p99 with **crash-only
  `fsync`**; `F_FULLFSYNC` has a separate, looser budget. Length-changing edit
  measured separately.

### 5.6 External mutation and mmap

- Record `(dev, ino, size, mtime_ns, ctime_ns)` from `fstat(src_fd)` at open.
- Rename-based replacement is safe (old inode stays mapped). In-place truncate
  causes `SIGBUS` on macOS; handle with a signal handler + `sigsetjmp(env, 1)`/
  `siglongjmp` around risky accesses (**no locks or allocations in the handler**),
  or `pread` for at-risk files. The handler guards only the base read-only
  mapping and must not longjmp across core frames; never hold a `Mapping` across
  a `reload`. Prefer `pread` for at-risk files; mmap is the opt-in fast path.
  Windows `EXCEPTION_IN_PAGE_ERROR` is post-1.0.
- On mismatch: auto-reload if clean **and the identity differs**; if dirty,
  `E_STALE` with a reload hint. If the identity matches, do nothing.
  `reload()`/`check_identity()` on an already-current identity are no-ops (Ok).
  `reload()` discards undo, maps new content, maps the caret to the nearest valid
  position. `check_identity()` is the explicit trigger for the GUI.
- File watching (FSEvents) is post-1.0 and is **not** the source of truth; the
  identity check is.
- mmap policy: **sliding window** (256 MB–1 GB) for large files; `pread` for
  network FS; a buffered `read()` is fine under ~64 KB; **no `MAP_PRIVATE`
  writes** for large files (use the capped add buffer); base mapping is read-only;
  scans use `open_scan` (`F_NOCACHE`) + `pread`, or a bounded `mmap` window with
  `madvise(MADV_SEQUENTIAL)` (done inside `map_readonly`); never mmap-the-world.
  The 1.68 GB RSS row in §7 is a naive full-mmap baseline, not the v1 policy.

## 6. CLI

Binary: `editor`. The GUI is `Editor.app`, linking the same core.

### 6.1 Commands

```
editor read <path> [--from N --to N | --from-offset B --length L] [--max-bytes N]
editor write <path> (--stdin | @FILE) [(--expect REV | --force)] [--in-place]
editor edit <path> (--old TEXT|@FILE|- --new TEXT|@FILE|- [--all | --count N]
                   | --at LINE[:COL] --op insert|replace|delete
                     [--text TEXT|@FILE|-] [--length L])
                  (--expect REV | --force) [--dry-run] [--in-place]
# --op insert|replace require --text; --op replace|delete require --length
editor info <path>
editor validate <path>
editor batch [--file F | --stdin] [--check]
editor version
editor schema [COMMAND]

# post-1.0 (sketches; schemas not frozen; non-exhaustive):
editor diff | restore | patch-apply | gui | tree | search | move | serve | run
# `gui` launches the GUI (not the core open); search is literal + glob only
```

**Global flags:** `--root DIR` (default cwd), `--max-file-bytes N` (input size
cap, default 2 GB), `--allow-huge` (ignore `--max-file-bytes`). Global flags may
appear **before or after** the subcommand (`editor --root R read f` ≡
`editor read f --root R`). Always JSON on stdout; no `--json`. Help/usage text
goes to **stderr** and exits 0. `editor --version` is an alias of `version`;
`editor --help`/`editor help` and `editor <cmd> --help` print usage to stderr.
`--` ends option parsing.

**Units and defaults**

- `read`: `--from`/`--to` 1-based lines (defaults 1 and EOF); `--from-offset`/
  `--length` 0-based bytes (defaults 0 and EOF). The two axes are **mutually
  exclusive**; combining them → `E_USAGE`. The byte axis reports 0-based
  `from`/`to`; the line axis reports 1-based. A line-axis `--from` past EOF, or
  `--from > --to`, yields empty `content`, `truncated:false`, `ok:true`.
  `--max-bytes` is the **output cap** (default 256 KiB); if content exceeds it,
  return the first N **at a UTF-8 codepoint boundary** with `truncated:true`.
  `--max-bytes 0` = unlimited. `--max-bytes` is the read output cap;
  `--max-file-bytes` is the input cap. Result: `content`; `from`/`to` use the
  request axis; `truncated`, `rev`. Line-axis `content` includes the terminator;
  byte-axis offsets are raw (offset 0 = BOM).
- `edit`: two mutually exclusive forms. (a) `--old TEXT|@FILE|- --new
  TEXT|@FILE|-` (literal search/replace, with `--all`/`--count N`); (b) `--at
  LINE[:COL] --op insert|replace|delete` (lines 1-based, cols 0-based UTF-8 byte
  offsets). `--old -` and `--new -` cannot both be `-` → `E_USAGE`. `--op` is
  only valid with `--at`; combining forms → `E_USAGE`.
  With neither `--all` nor `--count`, exactly one match is required (multiple →
  `E_AMBIGUOUS`, zero → `E_NO_MATCH`); `--count N` replaces exactly the first N
  matches and requires at least N (fewer → `E_NO_MATCH`); `--count 0` →
  `E_USAGE`. `--op insert|replace` require `--text`; `--op delete` ignores
  `--text`; `--op replace|delete` require `--length` (bytes, end-exclusive);
  `--length` with `--op insert` → `E_USAGE`. `--all`/`--count` with `--at` →
  `E_USAGE`. On an existing file, `--expect` (or `--force`) is required;
  missing → `E_USAGE` (exit 3). `--dry-run` reports the planned
  edit (`changed`, `bytes`, `line_count`, predicted `save` method) plus
  `rev_before`, and **omits `rev`**. `changed = {from, to}` where `from` is the
  first affected byte and `to = max(old_end, new_end)` over the edited region
  (same rule as `write`; a pure insert at X of L bytes → `{X, X+L}`, a pure
  delete of R bytes at X → `{X, X+R}`). Result: `changed`, `rev`, `bytes_before`,
  `bytes`, `bytes_written`, `save`, `in_place` (when set).
- `write`: a mutation. If `<path>` exists, `--expect` (or `--force`) is
  **required**; if it does not exist, neither is required and `--expect` →
  `E_USAGE`. Creates the file if missing; otherwise replaces its contents
  atomically (temp + rename). `--in-place` for hardlinks and requires an
  existing target (`E_USAGE` otherwise). Missing `--expect`/`--force` on an
  existing target → `E_USAGE` (exit 3). Result:
  `bytes` (resulting size), `bytes_written` (this call; 0 for an identical
  no-op), `rev`, `save` (`full_rewrite` for a new file, `none` for a no-op),
  `changed` (replacement: `{0, max(bytes_before, bytes)}`; new file:
  `{0, bytes}`; omitted for a no-op), `bytes_before` (existing target only).
- `info`: `path`, `bytes`, `line_count`, `eol`, `encoding`, `bom`,
  `final_newline`, `rev`, `dirty`.
- `validate`: `errors[]` of `{line, col, byte_offset, code}`; capped at 100 with
  a top-level `truncated` flag. Codes: `invalid_utf8`, `unsupported`. `ok` is
  `true` iff `errors[]` is empty; non-empty → `ok:false`, exit **2**, `error` =
  the first error. Include `rev`.
- `batch`: default stdin; `--file F` reads from F; `--check` validates only; see
  §6.4. Result: per-op `results[]` + `rolled_back`; `--check` returns
  `results: []`, `valid`, `errors[]`, and writes nothing.
- `version`: `results[0].version = "0.1.0"`.
- `schema`: no argument → `results[0].commands[]` for all commands; with
  `COMMAND` → that command only, or `E_NOT_FOUND`.

### 6.2 JSON envelope (always stdout)

```json
{
  "schema": "editor.result/1",
  "command": "edit",
  "ok": true,
  "duration_us": 2140,
  "cwd": "/work/editor",
  "warnings": [],
  "results": [
    {
      "path": "/work/editor/src/main.cpp",
      "rev_before": {"dev": "1", "ino": "2", "size": "4096", "mtime_ns": "3", "ctime_ns": "4"},
      "rev": {"dev": "1", "ino": "2", "size": "4112", "mtime_ns": "5", "ctime_ns": "6"},
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
  `cwd`, `warnings`, `results`, `error`, `meta`. Commands may add conditional
  top-level fields: `truncated` (validate/batch), `valid` + `errors[]`
  (batch `--check`), `rolled_back` (batch). Each element of `results[]` for
  a **file command** (`read`/`write`/`edit`/`info`/`validate`, and each such op
  of `batch`) always has `path`, `bytes`, `line_count`, `eol`, `encoding`,
  `bom`, `final_newline`, and `rev` (except `edit --dry-run`, §6.1);
  per-command lines list only extras.
- `version` and `schema` return a 1-element `results[]` containing only their
  documented fields (`version`, or `commands[]`) and **omit** the file fields
  (never emit `null`). `batch --check` returns `results: []` plus top-level
  `valid` and `errors[]`.
- `rev` is stat-only in v1 and present on every file-command result **except
  `edit --dry-run`**, which reports `rev_before` instead and omits `rev` (§6.1).
  Each `rev`/`rev_before` field is a **decimal string** (nanosecond fields exceed 2^53):
  `{"dev":"1","ino":"2","size":"4096","mtime_ns":"…","ctime_ns":"…"}`.
  `--expect` accepts `stat:<dev>:<ino>:<size>:<mtime_ns>:<ctime_ns>` and the
  object form with string values. Local/advisory; `--force` overrides.
- `path` is always the **absolute, symlink-resolved (`realpath`)** path of the
  target, identical regardless of how it was spelled on argv. Relative paths
  resolve against `--root` (default cwd).
- `changed.from`/`to` are **raw file byte offsets** (BOM included); `line_count`
  is logical lines.
- `rev_before` is present for mutations on an existing target and omitted for
  new files. `rev` is present except for `edit --dry-run` (§6.1). `bytes_before`
  is present for existing targets.
- All `write`/`edit` mutations (and each `batch` write/edit op) always include
  `rev_before` (existing target), `rev`, `bytes_before` (existing target),
  `bytes`, `bytes_written`, `save`, `in_place` (when set), and `changed` (when
  applicable).
- Deterministic: sort by path, then first byte offset; fixed field order; no
  `unordered_map` iteration; no wall-clock timestamps (file mtime/ctime are in
  `rev`); golden tests normalize `duration_us`/`meta.pid`/abs paths.
- Version: `editor.result/1`; additive within a major; unknown major → exit 3,
  `E_SCHEMA`. `editor schema [COMMAND]` dumps command schemas
  (`results[0].commands[]`); `editor version` returns `results[0].version`.
- EOL casing: enum `Eol` -> lowercase JSON (`lf`, `crlf`, `cr`, `mixed`).
  `Encoding` -> `"utf-8"`, `"utf-16le"`, `"utf-16be"`, `"other"`.
- `warnings[]` elements are `{code, message, path?}`; `SavedButNotDurable` →
  `{"code":"E_SAVED_BUT_NOT_DURABLE","message":"post-rename fsync failed","path":…}`
  with `ok:true`, exit 0.

### 6.3 Errors and exit codes

Core `Status` covers document/IO; the CLI adds `E_USAGE`, `E_SCHEMA`, `E_PARSE`,
`E_OUTSIDE_ROOT`, `E_NO_MATCH`, `E_AMBIGUOUS`. `E_LOCKED` is `Status::Locked`.
`E_*` = SCREAMING_SNAKE_CASE of the Status name. The top-level `error` object
carries `code`, `message`, `retryable`, `path`, `hint`, and command-specific
fields. `E_INVALID_UTF8` includes the byte offset; `E_NO_MATCH`/`E_AMBIGUOUS`
include at most 20 entries of `{line, col, byte_offset}` (1-based, CLI).
`validate`'s `errors[]` are lightweight findings with lowercase `code`
(`invalid_utf8`, `unsupported`, §6.1); when the first finding is promoted to the
top-level `error` (validate, or `batch --check` with failures), `code` is the E_
form (`E_INVALID_UTF8`/`E_UNSUPPORTED`) with the full field set.

| Condition | Code |
| --- | --- |
| `--expect` mismatch | `E_CONFLICT` |
| Base inode changed at save | `E_CONFLICT` |
| Lock contention (`Status::Locked`) | `E_LOCKED` |
| External mutation detected while dirty | `E_STALE` |
| External mutation detected while clean (via `check_identity()`) | silent auto-reload |

| Exit | Meaning | Examples |
| --- | --- | --- |
| 0 | ok | `Ok`, `SavedButNotDurable` (warning) |
| 2 | domain | `E_IO`, `E_NOT_FOUND`, `E_RANGE`, `E_CONFLICT`, `E_STALE`, `E_INVALID_UTF8`, `E_BINARY`, `E_OUTSIDE_ROOT`, `E_PARSE`, `E_TOO_LARGE`, `E_LOCKED`, `E_UNSUPPORTED`, `E_NO_MATCH`, `E_AMBIGUOUS` |
| 3 | usage/schema | `E_USAGE`, `E_SCHEMA` |
| 4 | internal | `E_OOM`, `E_INTERNAL` |

`validate` with a non-empty `errors[]` → `ok:false`, exit **2**, `error` = the
first error. `batch --check` with `valid:false` → exit **2**.

```json
{"schema":"editor.result/1","command":"edit","ok":false,"duration_us":900,
 "cwd":"/work/editor","warnings":[],
 "results":[],
 "error":{"code":"E_CONFLICT","message":"file changed since read","retryable":true,
          "path":"/work/editor/src/main.cpp",
          "expected_rev":"stat:1:2:4096:3:4",
          "actual_rev":"stat:1:2:4112:5:6","hint":"re-read, then retry"},
 "meta":{"editor_version":"0.1.0","pid":48211}}
```

### 6.4 Batch and concurrency

`batch` reads a JSON-lines program; each op is one object:

```json
{"op":"edit","path":"a.cpp","old":"let x = 1;","new":"let x = 2;","expect":"stat:1:2:4096:3:4"}
{"op":"write","path":"b.cpp","content":"...","force":true}
```

- Ops: `write`, `edit`. `write` uses `content`/`content_file` plus
  `in_place`/`expect`/`force` (`in_place` requires an existing target, else
  `E_USAGE`). `edit` uses `old`/`new`/`text`/`at`/`op`/
  `length`/`count`/`all`/`dry_run`/`expect`/`force`. `count` and `all` are
  mutually exclusive; `count: N` replaces the first N (at least N required);
  `count: 0` → `E_USAGE`; `op:"replace"` without `text` → `E_USAGE`.
- **Validation-stage atomicity**: parse/validate every op against the original
  files; any failure → no writes. Stage each file write to a same-dir temp;
  commit with `rename_at`. On failure, restore from same-dir backups; delete
  backups on success. If rollback fails, return `ok:false`,
  `rolled_back:false`, and `results[]` for applied ops. Buffer responses until
  commit. `--check` validates only. **Not crash-atomic.**
- Ops see the original files. A `write` op covers the whole file and overlaps
  every other op on that path. An `edit` op covers the matched `old` range (or
  the `--at`/`--length` range; `--op insert` is a zero-length range at the
  insertion point); overlap is byte-range intersection. Two inserts at the same
  offset overlap (`E_RANGE`), matching `apply`. Same-file ops must not overlap;
  disjoint edits are merged into one `apply`, and the merged op reports one
  `changed` range covering all merged edits.
- `batch` adds `rolled_back` (bool) and per-op `results[]` to the envelope;
  results are capped at 1000 with `"truncated": true`. `--check` returns
  `results: []`, `valid` + `errors[]` and writes nothing; on `valid:false`,
  `error` is the first error.
- Mutations require `--expect` on existing files; per-op `force` overrides.
  `write` to a non-existent path needs neither. Mismatch → `E_CONFLICT`, exit 2.
- Lock: `<path>.editor.lock`, `O_CREAT|O_EXCL`, 0600, random token + pid + start
  time; stale by pid liveness + token; held for the expect-check + write + rename
  critical section. Each file's lock is acquired and released around that file's
  own commit (never held across files), so concurrent batches cannot deadlock.
- Symlinks: `resolve` + re-check inside `--root`; open with `nofollow=true` and
  `lstat` the final component; never rename over a symlink.
- Refuse non-regular files; `--max-file-bytes` unless `--allow-huge`.
- If the batch program is stdin, `content_file` is required for `write` ops
  (no `content_stdin`).

### 6.5 Security

- **Scripts/patches** (post-1.0): trust-gated, data-only, hard caps,
  path-confined; never `exec` from GUI/config. Not specified in v1.
- **No user-editable config file in v1.** GUI preferences via `NSUserDefaults`
  are allowed. If a config file is added post-1.0: data only, versioned,
  no `exec`.
- **Secret leakage**: argv is visible in `ps`; prefer `--old @FILE --new @FILE`
  (or `--text @FILE`/`-` for an `--at` insert), and `@FILE` for `write`/`batch`;
  never echo file contents or matched lines in errors.
- **DoS defaults (v1)**: `--max-bytes` (read output), `--max-file-bytes`/
  `--allow-huge` (input), lock TTL, symlink-cycle detection. Post-1.0: search
  timeout, tree-walk depth.
- **`serve` (post-1.0)**: Unix socket 0600, per-session token, timeouts.

### 6.6 Binding invariant

Every `core_fn` has a `cli_binding` or `gui_binding` (or an explicit
`cli_excluded`); `editor schema` is generated from that table. `write` =
`(open|create)+apply+save`; `edit` = `apply+save`; `batch` = the ops it
dispatches. `version`/`schema` are CLI-only entries. `save_as`/`reload`/`undo`/
`redo` are GUI/conformance-only in v1; `create` has no dedicated CLI command but
is used internally by `write`. The golden menu test is a code-level check.

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
| + line cache (1024 entries **and** ≤ 6 MB; evict on either) | ≤ 6 MB |
| + undo/edit log (resident portion) | ≤ 8 MB (overflow spills to disk) |
| + add buffer (resident portion) | ≤ 8 MB (overflow spills to disk) |
| + sparse line index (5M lines, incl. nodes) | ≤ 2 MB |
| + sparse line index (25M lines, incl. nodes) | ≤ 10 MB |
| **Loaded, resident (1 GB file with ≤5M lines, i.e. ≥200 B/line; cache + undo + index + add buffer)** | **≤ 40 MB** |

Spilled undo/add-buffer data is disk, not footprint; the 40 MB cap is resident
only and assumes ≤5M lines. At 25M lines the index is ~9.4 MB, so the loaded
total (≈43 MB) exceeds the cap; add a line-count budget or a hard line cap for
the GUI.
Post-1.0, the content-mmap LRU (50–60 MB) is additional.

**Performance**

| Operation | Budget |
| --- | --- |
| Idle CPU | No periodic redraw; caret blink only |
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

**Design and brand.** `editor` follows the shared **Build Useful design system**
for brand consistency — one ink, a short ladder of greys, the system UI typeface,
a 2px spacing rhythm, quiet controls, and colour only for state — not exact
component reuse. The system is a **brand reference**, not a vendored library: its
web CSS/JS is not copied into this native app, and no third-party code is bundled
(D13). Chrome uses the system UI font; the document and technical values use the
mono face. Light and dark share the same semantic tokens. Before UI work, ask the
user for access to the current design-system source; never commit local paths,
personal data, or proprietary files.

- **Custom view only** on macOS in v1, driven by `viewcore` + `textbackend`
  (CoreText). No native-control hybrid; native control is a post-1.0 experiment.
- The view owns selection, caret, scroll; the core owns bytes and edits.
- **No IME and no accessibility in v1** — keyboard + mouse only. Both are
  significant platform work and are not needed for a personal editor.
- `viewcore` API (sketch): `set_document`, `resize`, `set_wrap`, `set_font`,
  `set_decorations`, `selection`/`set_selection`, `first_visible_line`,
  `scroll_to`, `draw`, `key`, `mouse`. `textbackend`: `measure`, `shape` into
  runs, `draw` into a platform `Canvas`. `Canvas`: `fill_rect`, `draw_run`, …
- Line cache keyed by **(logical line, wrap width)**; one shaped run per *visual*
  line. **Bounded LRU, 1024 entries and ≤ 6 MB**; skip caching lines above ~1000
  visual lines (a 100 MB single line must not blow the cache).
- Invalidate at/after the first changed line; on a line-count-changing edit, clear
  from the first changed line onward (rekeyed). Invalidate all on width/font change.
- Do not enumerate fonts at launch; resolve one font and enumerate lazily when
  the font menu opens.
- Monospace default; OS fonts (no bundled font in v1).

## 9. Workspace and future

**All post-1.0.** v1 has no workspace, tree command, or **project** search.
In-file find/replace is a v1 feature: the CLI exposes the literal matcher through
`editor edit --old/--new`; the GUI has the same menu operation. There is no
separate `find` command and no core search API.

- **Post-1.0:** workspace/tree cache, project search (literal + glob only, no
  regex), syntax, Markdown preview, git, scripts, `serve`, `move`, native-control
  experiment. Detailed sizing and seams are intentionally unspecified here.

## 10. Testing, CI, build

**Pre-M0.0 decisions**

- **Tests:** hand-rolled `CHECK` macros + a small PRNG + libFuzzer. No vendored
  test framework.
- **JSON:** hand-rolled writer + strict reader, fuzzed. Foundation
  `NSJSONSerialization` is an allowed macOS fallback if bugs appear.

**Testing**

- Unit: core, viewcore, CLI parsers.
- Property: edit/insert/delete/undo invariants; piece-table round-trips;
  save/restore identity, mode/owner/ACL/xattr round-trip; UTF-8 boundaries;
  chunk split/merge; line-count conventions; coalescing; CRLF/CR/mixed EOL;
  empty file; no trailing newline; huge line.
- Fuzz: argv, JSON reader/writer, patch parser (post-1.0), UTF-8, line index.
- Golden CLI: exact JSON for every command after the §6.2 normalizations,
  including errors.
- Conformance: one suite per §4 interface (`Editor`, `Mapping`, `FileSystem`)
  generated from the headers, plus `viewcore`, `textbackend`, `Canvas`.
- Sanitizers: ASan/UBSan every PR (lite); TSan nightly on core/viewcore only
  (ObjC++ excluded); hardened STL in debug.
- Fault injection: `fsync`/`rename_at`/`pwrite` failures; crash-consistency of
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
conformance-only), find/replace (CLI `edit --old/--new` + GUI), font size + tab
width + wrap settings, status bar, gutter, decorations, and the CLI (`read`,
`write`, `edit`, `info`, `validate`, `batch`, `version`, `schema`).

| Milestone | Scope | Est. | Gate |
| --- | --- | --- | --- |
| **Pre-M0.0** | Freeze `editor.hpp` + C++ conformance test + §5.1–5.3 data-model rules + test/JSON decisions. | 1 wk | Interface compiles; conformance skeleton runs. |
| **M0a** | Piece table + sparse index + core `apply` + `read`/`write`/`info` + root/symlink checks + same-length CoW save + JSON + basic Status→`E_*` mapping. | 3–4 wk | Open + read + write + reopen + read round-trip; golden JSON; 1 GB same-length `apply`+`save` < 100 ms p99 (APFS, crash-only `fsync`). |
| **M0b** | `edit` + undo + length-changing save + fault injection. | 3–4 wk | Edit/undo property tests; double-save and save-crash tests. |
| **M0c** | `batch` + `validate` + security + full error model (`E_NO_MATCH`/`E_AMBIGUOUS`) + batch symlink handling/TOCTOU + binding invariant. | 2–3 wk | Batch validation/rollback; fuzz; CLI golden. |
| **M0.5** | Headless render oracle + viewcore skeleton + **5 GB read-only demo** + C++ validation + conformance check. | 2 wk | 5 GB at 60 fps, flat footprint; sanitizers clean; conformance pass; D1 kill-gate decision recorded. |
| **M1** | macOS read-only custom view: AppKit + CoreText + viewcore; keyboard/mouse. | 4–6 wk | Open 1 GB to first paint < 100 ms; open 5 GB < 250 ms; scroll 5 GB at ≥60 fps. |
| **M2** | macOS editing + undo + save + **find/replace** (literal) + settings (font size, tab width, wrap via `NSUserDefaults`; keybindings hardcoded). | 4–6 wk | v1 checklist: edit, undo, find/replace, settings. |
| **M3** | macOS polish: status bar, gutter, decorations. | 3–4 wk | v1 checklist complete. |
| **M3.5** | Release engineering: notarization, packaging, CI perf gates. | 2–3 wk | Signed `.app`; CI green. |
| **Post-1.0** | Windows port, Linux port, workspace/tree, project search, syntax, Markdown, git, scripts, `serve`, `move`, diff/restore/patch-apply/gui/tree/search, native-control experiment. | — | — |

**Total:** the table sums to **24–33 weeks of work**. With ~30% overhead that is
**31–43 weeks full-time (~7–10 months)**; solo at ~75% time, plan on **10–13
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
| Schedule (~10–13 months solo at ~75%) | macOS-only MVP; cut list; post-1.0 fence. |
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

Three blind reviews ranked C++20 1st (34–36/40), Rust 2nd (26–30/40), C 3rd
(17–26/40). C++ keeps one language/build graph, avoids a foreign-ABI seam, and
matches the OS-edge nature of the hard parts. The M0.5 kill gate (D1) is the only
live decision. C++20 is pinned in §10.