# editor — Plan (v6.11)

> Name: **editor**. Status: planning. v6.11 — eighth refinement pass:
> `batch --check` exit class, `edit` binding, copy-path and `open`/`create`
> wording tightened. Scope unchanged.
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
| D6 | **CoW save**: same-length O(changed) writes (O(1) clone on APFS; O(file) where cloning is unavailable); length-changing O(tail) when tail ≤25%, else O(file). | A journal/extent-map proves simpler. |
| D7 | **Sparse line index**: 64 lines/chunk, order-statistic B-tree, in-chunk scan. | The §7 25M-line index budget fails. |
| D8 | **Stat-only `rev`.** No SHA-256 in v1. | Same-size saves within one mtime/ctime tick on APFS (esp. `--in-place`); then add a generation counter or hashing. |
| D9 | **No user-editable config file, no regex, no IME, no accessibility.** | If needed; post-1.0. |
| D10 | **UTF-8 only.** UTF-16 BOM → `E_UNSUPPORTED`; UTF-8 BOM accepted and excluded from logical lines. | UTF-16 becomes a blocker. |
| D11 | **No plugins/mods.** Scripts post-1.0, trust-gated. | Never for v1. |
| D12 | **MVP = macOS M0–M3.5**; if behind, cut per §11 (M3 polish, then M2 find/replace) and ship the reduced scope. | Schedule or market. |
| D13 | **Build Useful design system is a brand reference.** Follow its tokens/principles for brand consistency; do not vendor its web CSS/JS or bundle its code. Applies to chrome/app shell; the document view stays custom-drawn (D2). | The system's direction changes, or a native component library is adopted. |

Rationale for D1 is in the [appendix](#appendix-language-choice).

**D1 kill gate — M0.5, decided 2026-11-17: keep C++.** The M0.5 viewcore
skeleton, headless render oracle, and 5 GB read-only demo landed without the
warning signs the gate lists. Sanitizers (ASan/UBSan) and the seeded fuzzers are
clean; the code is small and the seams (`FileSystem`, `textbackend`, `Canvas`)
are holding; the untrusted surface is still just argv and the JSON reader. The
5 GB gate passes: scroll p99 0.25 ms (>60 fps) with a flat ~78 MB footprint, and
the line cache stays bounded (1024 entries / ≤6 MB). Reproduce with
`make demo` / `demo/view_demo.cpp`.

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
  uint16_t mode = 0; uint32_t uid = 0, gid = 0;
  uint64_t mtime_ns = 0, ctime_ns = 0;
  bool regular = false;
};

struct Info {
  std::string_view path;           // borrowed; valid until next non-const call
  uint64_t bytes = 0;              // on-disk size, BOM included
  int64_t line_count = 0;          // document state (includes unsaved edits)
  Eol eol = Eol::Lf;               // document state
  Encoding encoding = Encoding::Utf8;  // document state
  bool bom = false, final_newline = true, dirty = false;  // document state
  Rev rev;
};

struct Change {
  int64_t first = 0, last = 0, new_count = 0;  // lines [first,last) replaced by
                                               // new_count lines; one apply call
                                               // emits one Change bounding all
                                               // its edits (§4 Rules)
  ChangeKind kind = ChangeKind::Insert;        // Insert: only bytes inserted;
                                               // Delete: only bytes deleted;
                                               // Replace: both. Save/Reload:
                                               // first=0, last=line_count before,
                                               // new_count=line_count after
};

struct Edit {
  int64_t line = 0, col = 0;       // 0-based; col = UTF-8 byte offset
  int64_t removed_len = 0;         // bytes; 0 for insert
  std::string_view text;           // borrowed for the call only
};

struct SaveInfo {
  const char* method = "none";     // write-set shape (see §5.5): none|clone_pwrite|
                                   // clone_pwrite_tail|copy_range|full_rewrite
  uint64_t bytes_written = 0;      // bytes passed to pwrite; 0 for "none"
  uint64_t first_changed = 0;      // [first_changed,last_changed) bounding the
  uint64_t last_changed = 0;       // CHANGED spans, raw offsets in the NEW file
                                   // (BOM included); empty (first==last) when the
                                   // edit only deletes bytes; 0,0 for "none" (and
                                   // for an empty interval at offset 0)
  bool in_place = false;           // echoes the request, even on a no-op
  bool durable = true;             // false -> Status::SavedButNotDurable
};

class Editor {
 public:
  static constexpr uint32_t kInterfaceVersion = 1;
  virtual ~Editor() = default;

  virtual Status open(std::string_view path_utf8) = 0;
  // Create a new empty file (mode 0666 & ~umask). Existing path -> E_CONFLICT.
  // Never truncates. The new file becomes the open document (empty, clean); the
  // path must be absolute. The parent is resolved and the new basename appended,
  // since the final component does not exist yet. No expected-rev check
  // (nothing existed).
  virtual Status create(std::string_view path_utf8) = 0;
  virtual Status info(Info* out) const = 0;
  virtual Rev rev() const = 0;

  // first inclusive, last exclusive. buf=nullptr -> size query
  // (`used`=needed, `out_n`=lines in [first,last) clamped). Reads the DOCUMENT
  // (base piece + add buffer), so unsaved edits are visible.
  // On success `used`=bytes written, `out_n`=lines read. On BufferTooSmall:
  // no bytes written, `used`=full needed, `out_n`=0. Returns line bytes
  // INCLUDING the terminator. `first >= last`, `first >= line_count`, or
  // `last == 0` -> empty/Ok; negative `first`/`last` -> `E_RANGE`. `last`
  // clamps to `line_count`.
  virtual Status read_lines(int64_t first, int64_t last, char* buf, size_t cap,
                            size_t* used, int64_t* out_n) const = 0;
  // buf=nullptr -> size query (`used`=needed); `off >= size` -> empty/Ok;
  // negative `off` -> `E_RANGE`; `len` clamps to EOF. On BufferTooSmall: no
  // bytes written, `used`=needed; else `used`=bytes read. `read_range` is raw
  // file bytes (BOM included). Reads the BASE FILE only: unsaved edits become
  // visible here after save.
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
  // save: checks the base identity under the lock. If `lstat(path)` or
  // `fstat(base fd)` no longer matches the base Rev (§4 Rules) -> E_CONFLICT,
  // clean or dirty; save never auto-reloads. `expected_rev`, if given, must
  // equal that base Rev; mismatch -> E_CONFLICT. If clean and the identity is
  // unchanged, no-op with method="none" (the check still runs). Auto-reload of
  // a clean file after external mutation is check_identity()'s job (§5.6).
  // in_place writes the original inode; otherwise clone/temp.
  virtual Status save(const Rev* expected_rev = nullptr, bool in_place = false,
                      SaveInfo* out = nullptr) = 0;
  // save_as: the clean same-file no-op is checked first and wins (method="none",
  // bytes_written=0, even with expected_rev=null); otherwise expected_rev is
  // checked even when clean. With in_place, dst must be an existing regular file
  // to rewrite in place (a nonexistent dst is E_NOT_FOUND; use in_place=false to
  // create), so expected_rev must be non-null — except for the same-file no-op
  // above. Refuses a symlink or non-regular dst (lstat, no-follow) with
  // E_CONFLICT; refuses to overwrite an existing dst with E_CONFLICT unless
  // expected_rev matches dst's identity; if dst does not exist, expected_rev
  // must be null (otherwise E_CONFLICT). On success the open path becomes dst
  // (relative dst resolves against the current dir; the parent is resolved and
  // the basename appended when dst does not exist yet). Metadata follows §5.5:
  // in_place=false preserves the dst's metadata (or uses 0666 & ~umask for a
  // new dst); in_place=true keeps the dst inode's metadata untouched.
  virtual Status save_as(std::string_view path_utf8,
                         const Rev* expected_rev = nullptr, bool in_place = false,
                         SaveInfo* out = nullptr) = 0;
  virtual Status reload() = 0;  // already current: no-op, keeps undo,
                                // emits no Change
  // Check the on-disk identity; unchanged -> Ok; changed + clean -> auto-reload;
  // changed + dirty -> E_STALE.
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
  // create=true is O_CREAT|O_EXCL (existing path -> E_CONFLICT); truncate is
  // only valid with create=false.
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
- `int64_t` for document lines/byte offsets (including `read_range::off`, so a
  negative input is representable and rejected as `E_RANGE`); `uint64_t` for file
  sizes and `Rev` fields; `uint16_t` for mode. `0` is a valid line/length, never
  an error.
  Negative document values → `E_RANGE`. A successful `open`/`create`/`save_as`
  stores an absolute path with every existing component symlink-resolved; for a
  path that does not exist yet, the parent is resolved and the basename
  appended. A relative `save_as` dst resolves against the current directory
  first.
- Core is 0-based. CLI lines are 1-based; CLI `col` and byte offsets are 0-based
  UTF-8 byte offsets unless a command says otherwise.
- `open` uses `nofollow=true` and rejects non-regular files
  (`Stat::regular=false` → `E_IO`); `create` is `O_CREAT|O_EXCL`, so an existing
  path (any type) is `E_CONFLICT`. `save`/`save_as` refuse a symlink or
  non-regular target with `E_CONFLICT` (§5.5).
- `apply` validates, in order: negative ranges, caller ordering, overlap (two
  edits at the same offset overlap, even two empty inserts), line/col bounds,
  then UTF-8; violations → `E_RANGE`/`E_INVALID_UTF8`. Validation runs first, so
  a call that both overlaps and is a no-op is `E_RANGE`, not `Ok`.
- A call whose edits all have **no effective edit** (empty `text` and
  `removed_len == 0`, for every edit) changes nothing: `Ok`, no `Change`, no
  undo step. A call with at least one effective edit pushes exactly one undo
  step and emits exactly one `Change` bounding all its edits: `first` is the
  lowest, and `last` one past the highest, **pre-call** line that an edit
  removes, modifies, or inserts into; an insertion at document end after a
  terminator (or in an empty document) is the virtual line `line_count` and adds
  lines without setting `last`. Then
  `new_count = (last - first) + (line_count after - before)`; `kind` is
  `Insert`, `Delete`, or `Replace` according to whether the call inserted,
  deleted, or both. Undo coalescing is the caller's job (§5.4), so core always
  maps one such `apply` call to one step.
- `read_lines`/`read_range` check negative `first`/`last`/`off` **before** the
  empty-range rules, so a negative value is always `E_RANGE`.
- `Info::dirty` is true iff the piece table is not exactly the original base
  piece (an effective edit has been applied since `open`/`save`/`reload`);
  `save`/`reload` clear it. A dirty document whose bytes happen to equal the
  base still has a non-empty write-set and is written out (§5.5).
- Core never calls the UI; it appends changes and the UI calls `drain()`.
- Callbacks must not call `on_change`/`off_change`/`apply`/`save`/`drain`; they
  may only copy the `Change`. Callbacks must not throw (they are not declared
  `noexcept`); `drain()` catches anything that escapes one and returns
  `Status::Internal`.
- Exceptions must not escape; catch at every boundary → `Status::Internal`. A
  half-applied edit must leave the editor consistent.
- One `Editor` per thread; mutations are serialized by the owning thread.
- `rev()`/`Info::rev` return the last on-disk identity observed: set by
  `open`/`create`, and after `save`/`save_as` returns `Ok` **or**
  `SavedButNotDurable`, and after `reload`; not updated by unsaved edits. This
  identity is the **base Rev** used by the `save`/`save_as` identity checks
  (§5.5). In the needs-reload state (§5.5) both are zeroed.
- `Status::BufferTooSmall` is returned by `read_lines`/`read_range` when a
  non-null buffer is smaller than needed (no bytes written, `used`=needed);
  `buf=nullptr` is the size query. `FileSystem::pread` clamps to EOF (short count
  in `used`, never `BufferTooSmall`). `FileSystem::pwrite` writes all `len` bytes
  at `off`, extending the file as needed; a short write is `E_IO`, never
  `BufferTooSmall`.
- `errmsg` is thread-local and borrowed; no retention.
- Version tag is `kInterfaceVersion`.
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
  reported as `bom: true`. `bom_len` is `3` for a UTF-8 BOM and `0` otherwise. A
  UTF-16 BOM → `E_UNSUPPORTED`. `col` is relative to the post-BOM start of line
  1. `read_lines` excludes the BOM; `read_range` is raw file bytes; `changed.from`/
  `to` are raw file offsets (an edit at line 0, col 0 reports
  `from = bom_len`). `Info::bytes` is the on-disk size including the BOM.
- `col` is a 0-based UTF-8 byte offset within the line (tabs = 1 byte).
- `line == line_count` with `col == 0` is valid (**append at EOF**); for an
  existing line, `0 <= col <= line_byte_length`.
- EOL is per-file (`lf`, `crlf`, `cr`, `mixed`); mixed endings are preserved.
  **Lone CR is a terminator.** Chunks are line-aligned, so a CRLF never spans a
  chunk boundary and no lookahead across chunks is needed.
- **Inserted newlines** (`\n`, `\r\n`, `\r` in `Edit::text`) are normalized to
  the file's **dominant EOL** (most frequent terminator; tie → first seen; empty
  file **or no terminators** → LF). Existing EOLs are preserved.
- Encoding: UTF-8 strict. `open` performs O(1) checks only (UTF-8/UTF-16 BOM,
  first-8 KB NUL); UTF-8 validity is lazy per touched line, so `open` returns Ok
  even if later bytes are invalid. `validate` scans the whole file and reports
  `invalid_utf8` with byte offsets; it is the exception to `open`'s BOM gate: a
  UTF-16 BOM becomes an `unsupported` finding instead of an `E_UNSUPPORTED`
  abort, so `validate` still returns a report (`ok:false`, exit 2, `encoding`
  from the BOM, `bytes` = file size, and one finding
  `{line: 1, col: 0, byte_offset: 0, code: "unsupported"}`). The scan stops at
  the BOM, so the line fields are placeholders, not measurements:
  `line_count: 0`, `eol: "lf"`, `final_newline: false`, `bom: false` (that flag
  means the UTF-8 BOM).
  Diagnostic `line`/`col`/`byte_offset` values are CLI conventions (line
  1-based; `col` and `byte_offset` 0-based raw file offsets, BOM included).
  `apply` validates inserted text, the resulting edited range, and the codepoint
  boundaries at both ends; bytes being removed need not be valid. Edits that
  split a UTF-8 codepoint → `E_RANGE`; invalid resulting bytes →
  `E_INVALID_UTF8`; inserted text must be valid UTF-8.
- Binary detection: a NUL byte in the first 8 KB → `E_BINARY` at `open`;
  `validate` reports it as a `binary` finding at that byte and stops scanning,
  with the same placeholder line fields as `unsupported` (§6.1) — except that
  `bom` still reflects a UTF-8 BOM if one was found.

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
  terminators). Each tree node stores subtree sums of both (8 B each) for
  order-statistic lookup.
- A chunk's **absolute line/byte start** = sum of preceding chunks' counts/
  lengths, computed during the O(log n) descent. No absolute offsets are stored.
- An edit updates the touched chunk's count/len and the subtree sums **on the
  path** (O(log n)); later chunks are not rewritten.
- In-chunk scan uses the chunk's start line/byte (from the descent) to find the
  line. Chunks are line-aligned; a CRLF is never split.
- Chunk descriptors (12 B each) are resident for the whole file; the in-chunk
  scan reads the mapping/add buffer on demand. "Lazy" means the index is not
  built until the first line query/edit, not that descriptors are evicted.
- **Incremental (first-paint) build.** A line query only needs the lines up to
  its `last`: `read_lines(0,K)` extends the index just far enough, and the
  prefix grows geometrically so a sequence of queries stays O(n) overall.
  `info`/`apply` still force the exact, complete build. This is what lets first
  paint show the first viewport without scanning the whole file (the M1 gate:
  open 1 GB/5 GB to first paint). "Lazy" therefore means both "not built until
  the first query" and "built only as far as queried".
- Node layout: 63-way B-tree — 62 chunk descriptors + 63 subtree sums per node:
  62 × 12 B + 63 × 16 B = 1752 B per node, i.e. **≈28 B per chunk**. (Each
  subtree sum is a line-count + byte-length pair, so 16 B each.) Chunks are split
  at 64 lines and merged below 32; the estimate assumes ~75% occupancy
  (≈48 lines per chunk, a design target), which gives **≈0.58 B per line**:
  5M lines ≈ 2.9 MB, 25M ≈ 14.6 MB. The §7 caps assume this average; a workload
  that keeps chunks near half-full (≈32 lines, ≈0.88 B/line → 4.4 MB / 22 MB)
  fails the budget and trips D7's revisit gate.
  Chunk split/merge is specified and property-tested; an edit rewrites the
  touched chunk's 12 B descriptor and the subtree sums on the path, rescanning
  at most 64 lines, and one `apply` costs O(changed_lines/64 + log n) chunk
  operations.

### 5.4 Undo/redo

- Undo = apply the **inverse edit** (delete inserted range, re-insert deleted text).
- One `apply` call with at least one effective edit (a non-empty insert or a
  non-zero delete, §4) = one undo step; a call with no effective edits pushes
  none. The **GUI session** (not core) coalesces typing runs (500 ms or word
  boundary) into calls; programmatic edits are never coalesced. Core has no
  origin flag on `Edit` (§4).
- Cap: 10k steps or **8 MB resident**. Larger inverse text spills to a temp file;
  if that is impossible, undo is disabled for that edit, `errmsg()` reports it,
  and `undo()` returns `Status::Unsupported`.
- Redo is cleared on any new edit. Undo/redo on an empty stack return `Ok`
  with no change (no-op).
- **Undo is GUI/conformance-only in v1**; the CLI is one-shot and has no `undo`.
- At save: flatten the piece table, clear the add buffer, drop undo older than the
  save (piece offsets are invalid after flatten). "Undo to last save" is the only
  v1 behavior.
- Undo/redo emit one `Change` for their inverse call, following §4's bounding
  rule; its `kind` is that of the inverse. A `save`/`save_as` that wrote
  (`method != "none"`) emits one `Save` change (geometry in §4) meaning
  "invalidate everything"; a no-op save emits none. `Reload` is emitted only by
  a real reload (§5.6).

### 5.5 Save path (honest cost model)

v1 targets macOS/APFS; Windows/Linux fallbacks are post-1.0. Every mutating path
performs the source expect-check, the write (plus `rename_at` on the clone/copy
paths), the post-write identity check, and the post-save `open_readonly` reopen.
`Editor::save`, and `save_as` to the same inode, hold `FileSystem::lock` on that
path for the whole section. `save_as` to a different inode holds the lock on
**dst** (its expect-check, `rename_at`, and post-check) and verifies the source
with `lstat(source)` + `fstat(src_fd)` without locking it, so no save ever holds
two locks. The core resolves the open path once, splits it into dir + name, and
opens the directory before the rename. Callers (CLI, batch) never nest a second
lock around a save (§6.4).

`SaveInfo::method` names the **write-set shape**, not the mechanism, and is
chosen as follows:
- `none` — nothing written (clean or no-op save).
- `clone_pwrite` — same-length edit: only the changed spans are written (into a
  clone of the base, or over the original inode for `in_place`).
- `clone_pwrite_tail` — length-changing edit with tail ≤25%: the changed span
  and the shifted tail are written (same clone/in-place mechanisms).
- `copy_range` — clone unavailable (e.g. non-APFS) or `save_as` to a different
  inode (never with `in_place=true`): the whole file is written, with the
  unchanged ranges streamed from the
  base around the edited span.
- `full_rewrite` — new file, or tail >25%: the whole file is written from the
  piece table (a temp then `rename_at`, or over the original inode for
  `in_place`).

So a same-length save is `clone_pwrite` on the clone/in-place paths but
`copy_range` on a copy path, and `copy_range` never appears with
`in_place=true`.

- **No-op save**: if the write-set is empty (equivalently, the document is
  clean), the base identity check (below) still runs (`lstat(path)` + `fstat`),
  then `SaveInfo::method = "none"`, `bytes_written = 0`,
  `first_changed = last_changed = 0`, and `dirty` is false afterwards. A changed
  base is `E_CONFLICT`, not a silent success; re-stat via `check_identity()`.
- **New file** (`create`, then `apply`, then `save`): the base is empty, so all
  bytes are new; write them in full, `method = "full_rewrite"`,
  `first_changed = 0`, `last_changed = new size`, `bytes_written = new size`.
  An empty new document is a clean no-op save (`method = "none"`,
  `bytes_written = 0`). If `create` succeeded but the save failed, the 0-byte
  file remains and the error is reported; the CLI does not roll it back.
- **Same-length edits**: `clone_to_new(dir, &tmp, src_fd, &tmp_fd)` (O(1) on APFS;
  O(file) elsewhere), `pwrite` changed spans into `tmp_fd`, `fsync`, `rename_at`,
  `fsync_dir`. `SaveInfo::method = "clone_pwrite"`.
- **Length-changing edits**: if the tail is ≤ 25% of the file, clone to a temp,
  `pwrite` the edited span + shifted tail into the **temp** at new offsets,
  `ftruncate(tmp_fd, new_size)`, `fsync`, `rename_at`, `fsync_dir`;
  `method = "clone_pwrite_tail"`. Otherwise `full_rewrite` (a temp written from
  the piece table, or the in-place rewrite below).
- **Source identity check** (every path): the core holds the original fd from
  `open`; before cloning or streaming from the base, require **both**
  `lstat(source)` (the open path; still a regular file with the base Rev's
  `dev`/`ino`) and
  `fstat(src_fd)` (inode not truncated/rewritten in place) to match the **base
  Rev** (§4). If either differs, fail `E_CONFLICT`; never mix old offsets with
  new bytes. The copy path re-runs `fstat(src_fd)` after streaming and fails
  `E_CONFLICT` (temp discarded) if the source changed mid-copy. `--force` skips
  the explicit `--expect` comparison, not this safety check.
- **Copy paths** (clone unavailable, non-APFS, or `save_as` to a different
  inode — never `in_place=true`): `create_temp` (0600), stream the unchanged
  prefix/suffix ranges via
  `pread`/`pwrite` into the temp at their new offsets, write the edited span,
  copy metadata (see below), `fsync`, `rename_at`, `fsync_dir`; O(file),
  `method = "copy_range"` — except tail >25%, which uses `full_rewrite` (all
  bytes from the piece table into the temp). Never copy only a prefix and leave
  the suffix unwritten. `bytes_written` is every byte passed to `pwrite` (so the
  shifted tail counts); `first_changed`/`last_changed` cover only the **changed**
  spans, not the shifted tail.
- **Metadata target**: for the `in_place=false` paths, `save`, and `save_as` to
  the same inode, preserve that inode's metadata (a clone carries it). `save_as`
  to a different inode uses the copy path: preserve an existing dst's metadata, or
  use `0666 & ~umask` for a new dst. Never clone the source's metadata onto a
  different destination. `in_place=true` rewrites the existing dst inode, so
  dst's mode/ownership are untouched.
  When copying, `copy_metadata` does `fchown` before `fchmod`; setuid/setgid are
  not preserved. Do **not** call `fchmod` after a clone.
- **In-place** (`--in-place`): `open_write(path, create=false, truncate=false,
  nofollow=true)`. **In-place requires an existing regular target**; a
  nonexistent `save_as` dst is `E_NOT_FOUND` in core / `E_USAGE` on the CLI
  (§6.1/§6.4). Creating a new file always uses `in_place=false` (temp + rename).
  Then `fstat` the new fd and require the same full `Rev`
  (`dev`, `ino`, `size`, `mtime_ns`, `ctime_ns`) as the applicable expected Rev
  — the **base Rev** for `save`, `expected_rev` (dst) for `save_as`
  (otherwise `E_CONFLICT`, no write). `pwrite` changed spans or rewrite in full,
  `ftruncate` for length changes, `fsync(fd, full=false)`, update identity/rev.
  No temp/rename; `SaveInfo::method` follows the partition above
  (`clone_pwrite` for a same-length edit, `clone_pwrite_tail` when the tail is
  ≤25%, `full_rewrite` otherwise) with `in_place=true`. Expect-check under the
  lock. A failure once the write has started may leave a partial file: report
  `E_IO` and enter the needs-reload state (below). Hardlinks and crash-safety
  are user opt-in.
- **Temp**: `create_temp` returns a 0600 path + fd in the same directory; hold the
  fd from create through rename. Apply the metadata target above to the temp
  before `rename_at`, so the delivered destination never ends up at 0600. On any
  in-process failure, close and `unlink` the temp. A crash may leave a stale
  `*.editor-tmp-*`; it is ignored (or swept at startup). A read-only destination
  directory → `E_IO`; `--in-place` is the fallback.
- **Durability**: normal saves call `fsync(fd, full=false)` on the temp (or the
  in-place fd), then `rename_at` (temp only) → `fsync_dir(dirfd)`; open the
  directory **before** the rename. `full=true` (power-loss) is used only by the
  measurement harness or a future opt-in (§7), so `durable=true` means "the
  requested level was reached", not a power-loss guarantee. If a `full=true`
  call returns `Status::Unsupported`, retry with `full=false` and set
  `durable=false`. A failure **before** the rename is a hard `E_IO` and the temp
  is unlinked. A post-rename `fsync_dir` failure → `SavedButNotDurable` warning,
  `ok: true`, `durable=false`; **never blind-retry**. The warning is reported
  only on the call that produced it. A failed post-save reopen is `E_IO`, not a
  durability warning (below).
- **Post-save transition**: with the lock still held, reopen the path with
  `open_readonly(path, nofollow=true, &new_fd)` **while the pre-save base fd and
  its mapping are still open**, and verify the `fstat` matches the new `Rev`. On
  success, adopt the new fd, then close and unmap the pre-save base, update
  identity and rev (also for `SavedButNotDurable`), and release the lock. A
  reopen failure (`E_IO`) or an `fstat` mismatch (`E_CONFLICT`) means the write
  succeeded but no new base could be established: return that status and enter
  the needs-reload state **keeping the pre-save base fd/mapping**, so base bytes
  remain readable. Atomic w.r.t. other threads, not w.r.t. process crash; a crash
  **before** rename leaves the old file intact; after rename the new file is
  visible. Double-save test.
- **Needs-reload state** (entered by an in-place failure once the write started,
  a failed post-save reopen, or a post-save `fstat` mismatch): the in-memory
  document is kept and its base fd/mapping is retained, but it no longer
  corresponds to a known on-disk identity. `apply`/`undo`/`redo`/`save`/
  `save_as`/`check_identity` return `E_STALE`, `read_range` returns `E_IO`, and
  the file-state fields `rev()`/`Info::rev` and `Info::bytes` are zeroed;
  `read_lines` and the document-state fields of `info` (`line_count`, `eol`,
  `encoding`, `bom`, `final_newline`, `dirty`) keep working from the retained
  base. Only `reload()` or `open()` leave the state. The CLI is one-shot and only
  reports the original error.
- **Gate**: same-length edit in 1 GB on APFS < 100 ms p99 with **crash-only
  `fsync`**. `F_FULLFSYNC` is measured separately (§7) and is **not gated in
  v1**. Length-changing edit measured separately.

### 5.6 External mutation and mmap

- Record `(dev, ino, size, mtime_ns, ctime_ns)` from `fstat(src_fd)` at
  `open`/`create`.
- Rename-based replacement is safe (old inode stays mapped). In-place truncate
  causes `SIGBUS` on macOS; handle with a signal handler + `sigsetjmp(env, 1)`/
  `siglongjmp` around risky accesses (**no locks or allocations in the handler**),
  or `pread` for at-risk files. The handler guards only the base read-only
  mapping and must not longjmp across core frames; never hold a `Mapping` across
  a `reload`. Prefer `pread` for at-risk files; mmap is the opt-in fast path.
  Windows `EXCEPTION_IN_PAGE_ERROR` is post-1.0.
- On identity mismatch: auto-reload if clean; if dirty, `E_STALE` with a reload
  hint. `reload()`/`check_identity()` on an already-current identity are no-ops
  (Ok).
  `reload()` discards undo, maps new content, and emits `ChangeKind::Reload`; the
  GUI maps the caret to the nearest valid position. `check_identity()` is the
  explicit trigger for the GUI.
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
cap, default 2 GB), `--allow-huge` (ignore `--max-file-bytes`). The cap applies
to `read`/`write`/`edit`/`info`/`validate` (the target's size at open, plus a
`write` payload) and to each `batch` unit; exceeding it (without `--allow-huge`)
→ `E_TOO_LARGE`. Core itself imposes no cap, so the GUI and the 5 GB read-only
demo are unaffected. Global flags may appear **before or after** the subcommand
(`editor --root R read f` ≡ `editor read f --root R`). Always JSON on stdout; no
`--json`. Help/usage text goes to **stderr** and exits 0. `editor --version` is
an alias of `version`;
`editor --help`/`editor help` and `editor <cmd> --help` print usage to stderr.
`--` ends option parsing.

**Units and defaults**

- `read`: `--from`/`--to` 1-based lines (defaults 1 and EOF; `--to` inclusive);
  `--from-offset`/`--length` 0-based bytes (defaults 0 and EOF; byte `to` is
  exclusive, `from + length`). The two axes are **mutually exclusive**; combining
  them → `E_USAGE`. `--from`/`--to` must be ≥1 and `--from-offset`/`--length`/
  `--max-bytes` ≥0; violations → `E_USAGE`. The byte axis reports 0-based
  `from`/`to`; the line axis reports 1-based. A line-axis `--from` past EOF, a
  byte-axis `--from-offset` at/past EOF, or a line-axis `--from > --to`, yields
  empty `content`, `truncated:false`, `ok:true`. An empty result reports
  `to < from` on the line axis (`to` clamps to `[0, line_count]`; `0` is
  output-only, appears only for an empty file, and means "before line 1") and
  `to == from` on the byte axis (both clamp to file size).
  `--max-bytes` is the **output cap** (default 256 KiB); if content exceeds it,
  return the first N **at a UTF-8 codepoint boundary** with `truncated:true`.
  `--max-bytes 0` = unlimited. It is the read output cap; `--max-file-bytes` is
  the input cap (§6.1).
  `content` is always valid UTF-8: a range that starts/ends inside a codepoint or
  contains invalid bytes → `E_INVALID_UTF8` at the first offending byte (use
  `validate` for a full report). Result: `content`; `from`/`to` delimit the
  **returned** range on the request axis (byte axis: `to` exclusive; line axis:
  inclusive), so a truncated read reports the shorter range; `truncated`, `rev`.
  Line-axis `content` includes the terminator; byte-axis offsets are raw (offset
  0 = BOM).
- `edit`: requires an existing regular file (`E_NOT_FOUND` otherwise; use
  `write` to create). Two mutually exclusive forms. (a) `--old TEXT|@FILE|-`
  `--new TEXT|@FILE|-` (literal, case-sensitive search/replace, with `--all`/
  `--count N`); (b) `--at LINE[:COL] --op insert|replace|delete` (lines 1-based,
  cols 0-based UTF-8 byte offsets; `COL` defaults to 0). `--old -` and `--new -`
  cannot both be `-` → `E_USAGE`. `--old` must be non-empty → `E_USAGE`; it is
  searched over the document bytes (BOM excluded) while reported offsets stay raw.
  `--op` is only valid with `--at`; `--at` requires `--op`; combining forms →
  `E_USAGE`. Form (a) rejects `--at`/`--op`/`--text`/`--length`; form (b) rejects
  `--old`/`--new`/`--all`/`--count` → `E_USAGE`. `--op` outside
  `insert|replace|delete` → `E_USAGE`. `--at LINE` must be ≥1 (0 → `E_USAGE`);
  `LINE = line_count + 1` with `COL = 0` appends at EOF; `LINE > line_count + 1`,
  `COL < 0`, or `COL` past that line → `E_USAGE` for a negative value and
  `E_RANGE` for an out-of-file position. `--all` and `--count` are mutually
  exclusive. With neither `--all` nor `--count`, exactly one match is required
  (multiple → `E_AMBIGUOUS`, zero → `E_NO_MATCH`); `--all` with zero matches →
  `E_NO_MATCH`; `--count N` replaces exactly the first N matches and requires at
  least N (fewer → `E_NO_MATCH`); `--count N` requires N ≥ 1 (`--count 0` →
  `E_USAGE`). Matches are non-overlapping, left-to-right. `--op insert|replace`
  require `--text`; `--op delete` ignores `--text`; `--op replace|delete` require
  `--length` (bytes, end-exclusive) ≥ 0; `--length` with `--op insert` →
  `E_USAGE`. `@FILE`/`-` read failures → `E_NOT_FOUND`/`E_IO`/`E_USAGE` as
  applicable; `@FILE` resolves against `--root` like `path`. `--expect` and
  `--force` are mutually exclusive, and one of them is required; missing →
  `E_USAGE` (exit 3). `--dry-run` reports the planned edit
  (`changed`, `bytes`, `line_count`, predicted `save` method) plus `rev_before`;
  `bytes`, `line_count`, `final_newline`, and `changed` are the predicted
  post-edit values; `eol`, `encoding`, and `bom` are unchanged by an edit. The
  predicted method is `none` for a no-op, else the §5.5 method for the save path
  (`clone_pwrite`/`clone_pwrite_tail` on a clone or in-place path,
  `full_rewrite` when the tail exceeds 25%). It is advisory: the dry run does
  not probe clone support, so an actual save on a copy path reports
  `copy_range`. A dry run **omits `rev`** (a standard file field) and
  `bytes_written`; the other
  standard file fields and `bytes_before` (existing target) are present;
  `in_place:true` appears only with `--in-place`. `changed = {from, to}` where
  `from` is the first affected byte and `to = max(old_end, new_end)` over the
  edited region (same rule as `write`; a pure insert at X of L bytes →
  `{X, X+L}`, a pure delete of R bytes at X → `{X, X+R}`). An edit whose result
  equals the original document bytes (identical `--new`, `--op delete --length
  0`, or an empty insert) is a **no-op**: the CLI skips `apply` and commits a
  clean no-op save, so the base identity check still runs (changed base →
  `E_CONFLICT`): `save: "none"`, `bytes_written: 0`, `changed` omitted, `rev` =
  the post-check identity (`rev_before` when the base is unchanged). A
  `--dry-run` of that no-op predicts `save: "none"` and omits `changed`, `rev`,
  and `bytes_written`.
  Result: `changed`, `rev`, `bytes_before`, `bytes`, `bytes_written`, `save`,
  `in_place` (when set).
- `write`: a mutation. Exactly one of `--stdin` or `@FILE` supplies the content
  (both/neither → `E_USAGE`; `@FILE` must be a readable regular file). If
  `<path>` exists, `--expect` (or `--force`) is **required**; if it does not
  exist, neither is required and `--expect` → `E_USAGE`. `--expect` and `--force`
  are mutually exclusive. Creates the file if missing (mode `0666 & ~umask`; an
  empty payload still creates a 0-byte file and reports `save: "none"`,
  `bytes_written: 0`, and no `changed`); otherwise replaces its contents
  atomically (temp + rename). `--in-place` for hardlinks and requires an existing
  target (`E_USAGE` otherwise). An existing target must open under the core rules
  (§5.1), so a UTF-16-BOM or early-NUL target is `E_UNSUPPORTED`/`E_BINARY`; a
  target with invalid UTF-8 can be replaced because removed bytes need not be
  valid. An existing target's UTF-8 BOM is preserved (`write` replaces document
  bytes, not raw file bytes); `bytes`/`bytes_before` still count it. An existing
  target whose document bytes already equal the payload is a **no-op**: the CLI
  skips `apply` and commits a clean no-op save, so the base identity check still
  runs (changed base → `E_CONFLICT`): `save: "none"`, `bytes_written: 0`,
  `changed` omitted, `rev` = the post-check identity (`rev_before` when
  unchanged).
  Result: `bytes` (resulting size), `bytes_written` (this call; 0 for a no-op),
  `rev`, `save` (the actual method; `full_rewrite` for a new non-empty file,
  `none` for a no-op or an empty new file), `in_place` (when set, echoed even on
  a no-op), `changed` (replacement: `{bom_len, max(bytes_before, bytes)}`, where
  `bom_len` is per §5.1; new file: `{0, bytes}`; omitted for a no-op and for a
  newly created empty file), `bytes_before` (existing target only).
- `info`: standard file fields plus `dirty` (always `false` for the CLI, which
  opens fresh).
- `validate`: standard file fields plus `errors[]` of `{line, col, byte_offset,
  code}`; capped at 100 with a top-level `truncated` flag. Codes: `invalid_utf8`,
  `unsupported`, `binary`. A UTF-16 BOM (`unsupported`) or an early NUL
  (`binary`, first 8 KB) stops the scan and leaves the placeholder line fields of
  §5.1. `ok` is `true` iff `errors[]` is empty; non-empty → `ok:false`,
  exit **2**, `error` = the first error.
- `batch`: program source is exactly one of stdin (default) or `--file F` (both
  → `E_USAGE`); `--check` validates only; see §6.4. Result: per-unit `results[]`
  + `rolled_back`; `--check` returns `results: []`, `rolled_back:false`,
  `valid`, `errors[]`, and writes nothing.
- `version`: `results[0].version = "0.1.0"`.
- `schema`: no argument → `results[0].commands[]` for all commands; with
  `COMMAND` → that command only, or `E_USAGE` (exit 3).

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
      "bytes_written": 4112,
      "line_count": 142,
      "eol": "lf",
      "encoding": "utf-8",
      "bom": false,
      "final_newline": true,
      "changed": {"from": 2100, "to": 2116},
      "save": "full_rewrite"
    }
  ],
  "error": null,
  "meta": {"editor_version": "0.1.0", "pid": 48211}
}
```

- The **top-level** envelope always has `schema`, `command`, `ok`, `duration_us`,
  `cwd`, `warnings`, `results`, `error`, `meta`. Commands may add conditional
  top-level fields: `truncated` (validate/batch — top-level; `read`'s `truncated`
  is a per-result field, §6.1), `valid` + `errors[]`
  (batch `--check`), `rolled_back` (batch). Each element of `results[]` for
  a **file command** (`read`/`write`/`edit`/`info`/`validate`, and each such unit
  of `batch`) always has `path`, `bytes`, `line_count`, `eol`, `encoding`,
  `bom`, `final_newline`, and `rev` (except `edit --dry-run` and batch `dry_run`
  ops, §6.1/§6.4); per-command sections list only extras. `validate` may report
  placeholder line fields when its scan stops early (§5.1).
- `version` and `schema` return a 1-element `results[]` containing only their
  documented fields (`version`, or `commands[]`) and **omit** the file fields
  (never emit `null`). `batch --check` returns `results: []` plus top-level
  `valid` and `errors[]`.
- `rev` is stat-only in v1. Each `rev`/`rev_before` field is a **decimal string**
  (nanosecond fields exceed 2^53):
  `{"dev":"1","ino":"2","size":"4096","mtime_ns":"…","ctime_ns":"…"}`.
  `--expect` on argv takes only the `stat:<dev>:<ino>:<size>:<mtime_ns>:<ctime_ns>`
  string; the object form with string values is the batch JSON `expect` field.
  Local/advisory; `--force` overrides.
- `path` is always the **absolute** path of the target with every existing
  component symlink-resolved; for a new file, the resolved parent plus the new
  basename. It is identical regardless of how it was spelled on argv. Relative
  paths resolve against `--root` (default cwd); the CLI resolves and hands core
  an absolute path (§4).
- `changed.from`/`to` are **raw file byte offsets** (BOM included); `line_count`
  is logical lines. `changed` is a single bounding interval
  `[first affected byte, max(old_end, new_end))` — it may include bytes no edit
  touched. `SaveInfo::first_changed`/`last_changed` (core-internal, never
  emitted by the CLI) report the **changed spans** — the union of the edited byte
  ranges mapped onto the new file, also as one bounding interval — which can be
  narrower than `changed`; a pure deletion leaves no changed bytes in the new
  file, so that interval is empty (`first_changed == last_changed` = the deletion
  point). The empty interval at offset 0 is `0,0`, the same values as
  `method = "none"`; `method` disambiguates them.
- `rev_before` is present for mutations on an existing target and omitted for
  new files; `bytes_before` likewise.
- All `write`/`edit` mutations (and each `batch` write/edit unit) always include
  `rev_before` (existing target), `rev`, `bytes_before` (existing target),
  `bytes`, `bytes_written`, `save`, `in_place` (when set), and `changed` (when
  applicable) — except `edit --dry-run` and batch `dry_run` ops, which omit
  `rev` and `bytes_written`.
- Deterministic: sort by path, then first byte offset; fixed field order; no
  `unordered_map` iteration; no wall-clock timestamps (file mtime/ctime are in
  `rev`); golden tests normalize `duration_us`/`meta.pid`/abs paths.
- Version: `editor.result/1`; additive within a major; unknown major → exit 3,
  `E_SCHEMA`. `editor schema [COMMAND]` dumps command schemas
  (`results[0].commands[]`); `editor version` returns `results[0].version`.
- EOL casing: enum `Eol` -> lowercase JSON (`lf`, `crlf`, `cr`, `mixed`).
  `Encoding` -> `"utf-8"`, `"utf-16le"`, `"utf-16be"`, `"other"`. `bom` is the
  UTF-8 BOM flag only, so an `encoding` of `utf-16le`/`utf-16be` reports
  `bom:false`.
- `warnings[]` elements are `{code, message, path?}`; `SavedButNotDurable` →
  `{"code":"E_SAVED_BUT_NOT_DURABLE","message":"save completed without the requested durability","path":…}`
  with `ok:true`, exit 0. It covers a downgraded `full=true` fsync and a
  post-rename `fsync_dir` failure (§5.5).

### 6.3 Errors and exit codes

Core `Status` covers document/IO; the CLI adds `E_USAGE`, `E_SCHEMA`, `E_PARSE`,
`E_OUTSIDE_ROOT`, `E_NO_MATCH`, `E_AMBIGUOUS`. `E_LOCKED` is `Status::Locked`.
`E_*` = SCREAMING_SNAKE_CASE of the Status name. The top-level `error` object
carries `code`, `message`, `retryable`, `path`, `hint`, and command-specific
fields (`retryable` is a fixed per-code boolean — true for the transient
`E_IO`/`E_LOCKED`/`E_CONFLICT` conditions, false otherwise; `hint` is a
free-form, optional human string); `batch` errors add `op_index` (0-based,
omitted for a whole-program parse failure, which is `E_PARSE`, exit 2).
`E_INVALID_UTF8` includes the byte offset; `E_NO_MATCH`/
`E_AMBIGUOUS` include at most 20 entries of `{line, col, byte_offset}` (CLI:
`line` 1-based; `col` and `byte_offset` 0-based).
`validate`'s `errors[]` are lightweight findings with lowercase `code`
(`invalid_utf8`, `unsupported`, `binary`; §6.1); when the first finding is
promoted to the top-level `error` (validate, or `batch --check` with failures),
`code` is the E_ form (`E_INVALID_UTF8`/`E_UNSUPPORTED`/`E_BINARY`) with the
full field set.

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
first error. `batch --check` with `valid:false` exits with the class of its first
error (`error.code`): exit **2** for a domain code, **3** for `E_USAGE`/`E_SCHEMA`.
The CLI always size-queries before a read, so a `Status::BufferTooSmall` that
reaches the CLI boundary is an internal invariant failure: it is reported as
`E_INTERNAL`, exit 4.

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

- Ops: `write`, `edit`. `write` uses exactly one of `content`/`content_file`
  (both/neither → `E_USAGE`); when the program itself came from stdin, inline
  `content` is not allowed and `content_file` is required. Plus
  `in_place`/`expect`/`force` (`in_place` requires an existing target, else
  `E_USAGE`). `content_file` resolves like `path` (against `--root`) and must be
  a readable regular file (`E_NOT_FOUND`/`E_IO`/`E_OUTSIDE_ROOT`). `edit` uses
  `old`/`new`/`text`/`at`/`op`/`length`/`count`/`all`/`dry_run`/`expect`/`force`;
  its field rules match the CLI `edit` forms (§6.1), except that `old`/`new`/
  `text` must be literals (`-` is `E_USAGE`; the program may itself be stdin) and
  `dry_run` reports the predicted result instead of writing. `expect` and
  `force` are mutually exclusive; `expect` is either the `stat:` string (as on
  argv) or the object form `{dev, ino, size, mtime_ns, ctime_ns}` with
  decimal-string values — the two are equivalent (§6.2). Each op's result
  fields, `changed`, and no-op rules match its CLI command (§6.1).
- **Validation-stage atomicity**: parse/validate every op against the original
  files; any validation failure → no writes. Each unit commits by calling core
  `open`/`create` + `save` (never `save_as`, which is GUI-only; §6.6), so the
  §5.5 metadata/durability rules apply unchanged. Before the first commit, take a
  same-dir **backup** of every file the batch will replace, using `clone_to_new`
  (O(1) on APFS) or, where cloning is unavailable, a byte copy (O(file)); never a
  hardlink, because `link()` bumps the inode's `ctime_ns` and would invalidate
  the base Rev the save is about to check. On a commit failure, restore every
  changed file from its backup and unlink files created by this batch; delete
  backups on success. `in_place` units can leave a partial file if rollback
  itself fails. Buffer responses until commit. `--check` validates only. **Not
  crash-atomic.**
- **Result shapes.** Success: `ok:true`, `rolled_back:false`, one `results[]`
  element per merged write unit (see below) **plus** one per `dry_run` op.
  `dry_run` ops are excluded from unit merging, backups, commits, and
  `rolled_back` accounting; each reports the §6.1 dry-run fields (no `rev`, no
  `bytes_written`). Validation failure: `ok:false`, `rolled_back:false`,
  `results: []`, `error` = the first failing op's error including `op_index`.
  Commit failure with a successful rollback: `ok:false`, `rolled_back:true`,
  `results: []` (nothing from this batch survives), `error` = the first commit
  error. Rollback failure: `ok:false`, `rolled_back:false`, `results[]` for the
  units that remain applied.
- Ops see the original files. A `write` op covers the whole file and overlaps
  every other op on that path. An `edit` op covers the matched `old` range (or
  the `--at`/`--length` range; `--op insert` is a zero-length range at the
  insertion point); overlap is byte-range intersection. Two inserts at the same
  offset overlap (`E_RANGE`), matching `apply`. Same-file ops must not overlap;
  disjoint edits are merged into one `apply`, and that merged unit yields one
  `results[]` element whose `changed` covers all merged edits.
- `batch` adds `rolled_back` (bool) and per-unit `results[]` to the envelope;
  `rolled_back` is `true` only when a commit failure triggered rollback and every
  changed file was restored; it is `false` on success, on validation failure (no
  writes), and when rollback fails. `results[]` follows the §6.2 deterministic
  order (path, then first byte offset; ties by op index); `results[]` and
  `--check`'s `errors[]` are capped at 1000 entries with top-level
  `"truncated": true`. `--check` returns `results: []`, `rolled_back:false`,
  `valid` + `errors[]` (each with `op_index`) and writes nothing; on
  `valid:false`, `error` is the first error.
- Mutations require a per-op `expect` on existing files; per-op `force`
  overrides. `write` to a non-existent path needs neither. Mismatch →
  `E_CONFLICT`, exit 2.
- Lock: `<path>.editor.lock`, `O_CREAT|O_EXCL`, 0600, random token + pid + start
  time; stale by pid liveness + token. Core's `save`/`save_as` own the lock for
  the whole expect-check + write + commit (rename or in-place rewrite) critical
  section (§5.5), so a batch unit does not take it again; `unlock` unlinks the
  lock file. Locks are never held across files, so concurrent batches cannot
  deadlock. Rollback reacquires each file's lock after its save released it.
  Every unit that completed its save is restored only if its post-commit Rev
  still matches; otherwise rollback fails. The failed unit is restored
  **unconditionally** when its save failed after the first `pwrite` (e.g.
  `E_IO`), because its own partial write changed the Rev; a unit that failed in
  the expect-check wrote nothing and is not restored (restoring it would clobber
  external changes). Restore via `rename_at` from the backup
  (non-`in_place`), or `pwrite` + `ftruncate` from it (`in_place`), then
  `fsync` + `fsync_dir`.
- Symlinks: `resolve` + re-check inside `--root`; open with `nofollow=true` and
  `lstat` the final component; never rename over a symlink.
- Refuse non-regular files; `--max-file-bytes` unless `--allow-huge`.

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
  `--allow-huge` (input), symlink-cycle detection. Post-1.0: search timeout,
  tree-walk depth.
- **`serve` (post-1.0)**: Unix socket 0600, per-session token, timeouts.

### 6.6 Binding invariant

Every `core_fn` has a `cli_binding` or `gui_binding` (or an explicit
`cli_excluded`); `editor schema` is generated from that table. `write` =
`(open|create)+apply+save`; `edit` = `open+apply+save`; `batch` = the ops it
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

**Budgets (per-component caps; the loaded-resident rows are separate combined caps)**

| Metric | Cap |
| --- | --- |
| Empty-window baseline | ≤ 12 MB footprint (RSS is higher; measured 40.6 MB) |
| + line cache (1024 entries **and** ≤ 6 MB; evict on either) | ≤ 6 MB |
| + undo/edit log (resident portion) | ≤ 8 MB (overflow spills to disk) |
| + add buffer (resident portion) | ≤ 8 MB (overflow spills to disk) |
| + sparse line index (5M lines, incl. nodes) | ≤ 3 MB |
| + sparse line index (25M lines, incl. nodes) | ≤ 15 MB |
| **Loaded, resident (1 GB file with ≤5M lines, i.e. ≥200 B/line; baseline + cache + undo + index + add buffer)** | **≤ 40 MB** |
| **Loaded, resident (1 GB file with ≤25M lines; same components)** | **≤ 50 MB** |

Spilled undo/add-buffer data is disk, not footprint. Both loaded-resident caps
are a total resident footprint: 12 (baseline) + 6 (cache) + 8 (undo) + 8 (add
buffer) = 34 MB, plus the index cap — 37 MB at 5M lines (cap 40 MB) and 49 MB at
25M lines (cap 50 MB). The §5.3 layout estimates at ~75% chunk occupancy are
~2.9 MB and ~14.6 MB. Line count is not separately capped.
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
- **Colour tokens are implemented natively** in `viewcore/include/theme.hpp`
  (`light_theme()` / `dark_theme()`), resolved from the design system's semantic
  colour tokens: surface/text, surface-muted (selection), surface-subtle
  (current line), page (gutter), text-muted (line numbers), border-subtle. The
  app offers System / Light / Dark and defaults to System. The design system's
  web CSS/JS is never vendored (D13).
- The view owns selection, caret, scroll; the core owns bytes and edits.
- **No IME and no accessibility in v1** — keyboard + mouse only. Both are
  significant platform work and are not needed for a personal editor.
- **Editing keys (v1, hardcoded).** Arrows, Home/End and Page Up/Down move the
  caret; Shift extends the selection. Option+Left/Right move by word (to the end
  of the run, as macOS does) and Option+Backspace/Forward-Delete delete by word.
  Command+Left/Right go to the line edges, Command+Up/Down to the document
  edges. Double click selects the word (dragging keeps word granularity), triple
  click the line; Escape collapses the selection. Command+A select all,
  Command+Z / Shift+Command+Z undo/redo, Command+X/C/V clipboard, Command+F
  find/replace.
- **Sidebar keys (v1, hardcoded).** With the sidebar focused: Up/Down move by
  row, Right opens a closed directory and then steps into its first child, Left
  closes an open directory and then steps out to its parent, Return activates
  (a directory toggles, a file opens). Clicking the sidebar gives it focus. The
  selection is held by **path**, not by node pointer (re-reading a directory
  frees its nodes), and each move scrolls the selection into view.
- `viewcore` API (sketch): `set_document`, `resize`, `set_wrap`, `set_font`,
  `set_decorations`, `selection`/`set_selection`, `first_visible_line`,
  `scroll_to`, `draw`, `key`, `mouse`. `textbackend`: `measure`, `shape` into
  runs, `draw` into a platform `Canvas`. `Canvas`: `fill_rect`, `draw_run`, …
- Line cache keyed by **(logical line, wrap width)**; one shaped run per *visual*
  line. **Bounded LRU, 1024 entries and ≤ 6 MB**; skip caching lines above ~1000
  visual lines (a 100 MB single line must not blow the cache).
- **Scrolling is measured in pixels, not lines.** The scroll position is a
  logical line plus a pixel offset into its block, and the scrollbar's range is
  the *pixel* content height, so a line that wraps into several screens scrolls
  correctly (a line-counted model says "one line fits" and offers no bar, and
  lets a wrapped document scroll past its end). The thumb tracks the pointer
  exactly; `max_top` walks back only a viewport of blocks, so the clamp stays
  off the wheel path.
- Invalidate at/after the first changed line; on a line-count-changing edit, clear
  from the first changed line onward (rekeyed). Invalidate all on width/font change.
- Do not enumerate fonts at launch; resolve one font and enumerate lazily when
  the font menu opens.
- Monospace default; OS fonts (no bundled font in v1).

## 9. Workspace and future

**Workspace, tree, and project search are post-1.0.** v1 has no workspace, tree
command, or **project** search. In-file find/replace is a v1 feature: the CLI
exposes the literal matcher through `editor edit --old/--new`; the GUI has the
same menu operation and scans logical lines with `read_lines`. There is no
separate `find` command and no core search API.

- **Post-1.0:** workspace/tree cache, project search (literal + glob only, no
  regex), syntax, Markdown preview, git, scripts, `serve`, `move`, native-control
  experiment. Detailed sizing and seams are intentionally unspecified here.

**Sidebar (shape decided 2026-11-18).** The GUI gets a file sidebar, and it is a
**recursive tree**, not a flat folder list. It stays post-1.0 (v1 is still
one-file-at-a-time), but the shape is fixed now so the work is not redone:

- **Lazy by construction.** A directory is read only when it is expanded (plus
  the open file's ancestors, so the tree can reveal it); collapsed subtrees are
  never walked. No background scanner, no whole-project walk, and no ignore-rule
  engine in the first cut. Relaxing this needs a measured reason.
- **Speed is a gate, not a goal.** Reading a folder and laying out its rows stay
  off the frame path: rows are drawn from a cached, sorted (folders first) list,
  and a folder with thousands of entries must not affect first paint or scroll
  cost. Any future rescan is incremental, not a full walk.
- **Read-only navigation.** Clicking a file opens it through the existing
  unsaved-changes prompt. No rename/delete/new-file until the workspace work
  (§2: thousands of files is a workspace goal, not v1).
- **Custom-drawn** (D2), tokens only: hairline separators, `color.secondary-hover`
  selection, `color.text`/`color.text-muted` labels in the system UI face. No
  `NSTableView`/`NSOutlineView` (a native-control hybrid).
- **Testable like `viewcore`:** the directory reads behind an injectable seam
  (POSIX `opendir`/`readdir` in the app layer, a fake in tests), so the tree and
  its layout are covered headlessly rather than only by looking at the window.

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

**v1 checklist:** open/save, edit, undo/redo, save_as/reload (GUI or
conformance-only), `create` (internal, used by `write`), find/replace (CLI
`edit --old/--new` + GUI), font size + tab
width + wrap settings, status bar, gutter, decorations, and the CLI (`read`,
`write`, `edit`, `info`, `validate`, `batch`, `version`, `schema`).

| Milestone | Scope | Est. | Gate |
| --- | --- | --- | --- |
| **Pre-M0.0** | Freeze `editor.hpp` + C++ conformance test + §5.1–5.3 data-model rules + test/JSON decisions. | 1 wk | Interface compiles; conformance skeleton runs. |
| **M0a** | Piece table + sparse index + core `apply` + `read`/`write`/`info` + root/symlink checks + same-length CoW save + JSON + basic Status→`E_*` mapping. | 3–4 wk | Open + read + write + reopen + read round-trip; golden JSON; 1 GB same-length `apply`+`save` < 100 ms p99 (APFS, crash-only `fsync`). |
| **M0b** | `edit` (basic `E_NO_MATCH`/`E_AMBIGUOUS`; full model in M0c) + undo + length-changing save + fault injection. | 3–4 wk | Edit/undo property tests; double-save and save-crash tests. |
| **M0c** | `batch` + `validate` + security + full error model (`E_NO_MATCH`/`E_AMBIGUOUS`) + batch symlink handling/TOCTOU + binding invariant. | 2–3 wk | Batch validation/rollback; fuzz; CLI golden. |
| **M0.5** | Headless render oracle + viewcore skeleton + **5 GB read-only demo** + C++ validation + conformance check. | 2 wk | 5 GB at 60 fps, flat footprint; sanitizers clean; conformance pass; D1 kill-gate decision recorded. |
| **M1** | macOS read-only custom view: AppKit + CoreText + viewcore; keyboard/mouse. | 4–6 wk | Open 1 GB to first paint < 100 ms; open 5 GB < 250 ms; scroll 5 GB at ≥60 fps. |
| **M2** | macOS editing + undo + save + **find/replace** (literal) + settings (font size, tab width, wrap via `NSUserDefaults`; keybindings hardcoded). | 4–6 wk | v1 checklist: edit, undo, find/replace, settings. |
| **M3** | macOS polish: status bar, gutter, decorations. | 3–4 wk | v1 checklist complete. |
| **M3.5** | Release engineering: notarization, packaging, CI perf gates. | 2–3 wk | Signed `.app`; CI green. |
| **Post-1.0** | Windows port, Linux port, workspace/tree, project search, syntax, Markdown, git, scripts, `serve`, `move`, diff/restore/patch-apply/gui/tree/search, native-control experiment. | — | — |

**Status (2026-11-18).** M0–M3 code is implemented: `core` + CLI
(M0), `viewcore` + render oracle + the lazy line index (M0.5/M1), the AppKit view
with CoreText/CoreGraphics backends, editing, find/replace, and settings (M1/M2),
and the gutter / status bar / cut-copy-paste polish (M3). The GUI also has
custom-drawn overlay scrollbars — vertical (drag, track paging, command-click to
jump; drawn from byte progress while the line count is still unknown) and
horizontal for long lines with wrap off (pan, drag, track paging, command-click)
— File → New / Open / Save As with a window title and dirty marker, and
incremental (run-loop dripped) indexing so a medium file never blocks the window. `make test` (plain +
ASan/UBSan), `make golden`, `make fuzz`, `make bench`, `make demo`, and
`make gui-smoke` (headless AppKit snapshots in both themes, pinned golden frames
in light/dark/small/scrolled, a real launch, a no-file launch, and a Finder-style
LaunchServices open) pass; the M1 first-paint
gates (1 GB < 100 ms, 5 GB < 250 ms) pass via `make demo`. **M3.5 (notarization,
packaging, CI perf gates) is not started** — it needs a signing identity and CI,
which are out of scope for the local build.

Launch paths: Finder / right-click “Open With” starts the app with no argv file
and delivers the document as an `odoc` Apple event, *after* the window's first
draw is already queued. So an untitled window is backed by a private temp file
(removed on open / Save As / quit) and the core reports `E_IO` for every
document call while nothing is open, instead of dereferencing an unset source —
that combination was the launch crash.

Known limits: for files above the discovery cap (64 MB) the vertical bar is
approximate — its thumb is sized and placed from byte progress, a drag only
previews until it is released, and the release then indexes up to the target
line (a jump to 90% of a 5 GB file does that much index work). With wrap on and
an uneven wrap height the thumb still tracks the pointer exactly, but a given
thumb position maps to the nearest *logical* line rather than a pixel-exact
place (the ends are exact; only the middle is approximate). The content height
is measured over at most 1000 visual lines and the rest extrapolated, so a
document far larger than that gets an approximate thumb size. The horizontal
range is the widest line measured so far, so it grows as you
scroll and does not shrink again until another document opens. The app has no
app icon yet (the design system's icon assets need an agreed raster step).

The sidebar is a v1 implementation of the §9 shape: rooted at the open
document's folder (v1 is one file at a time, so there is no workspace root),
read-only, with no rename/delete/new-file, no project-wide search, and no
icon/type badges. It refreshes when the window becomes key and after a save-as,
not continuously. Above the discovery cap the vertical scrollbar is approximate;
a sidebar drag is irrelevant there (the sidebar does not scroll the document).

**Total:** the table sums to **24–33 weeks of work**. With ~30% overhead that is
**31–43 weeks full-time (~7–10 months)**; solo at ~75% time, plan on **10–13
months**; half-time is **14–20 months**.

**Cut list if behind (v1 cuts first):**

1. M3 polish (decorations, status bar, gutter) → ship M2-level editor.
2. M2 GUI find/replace UI → ship without it (the CLI `edit --old/--new` matcher
   ships in M0b and stays).
3. Do **not** cut: save correctness, fault injection, basic editing/undo.

## 12. Risks and open decisions

| Risk | Mitigation |
| --- | --- |
| Length-changing save is O(tail) or O(file) | Honest cost model; tail/full-rewrite threshold; benchmark both. |
| Hand-rolled JSON bugs | Fuzz + property tests; keep the parser small and strict. |
| Schedule (~10–13 months solo at ~75%) | macOS-only MVP; cut list; post-1.0 fence. |
| Temptation to re-add hybrid/native control | D2 decision + post-1.0 trigger. |
| mmap SIGBUS on truncate | Signal handler/pread; identity check; reload. |
| External mutation race | ctime-aware rev; lock covers expect-check + write + commit (rename or in-place rewrite); auto-reload clean. |
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
live v1 decision; the post-1.0 bundled-font question remains open in §12. C++20
is pinned in §10.
