// core/include/editor.hpp — v1
//
// The frozen core interface (PLAN.md §4). Frozen at Pre-M0.0; M0.5 is not a
// re-freeze. See PLAN.md §4 "Rules" for the normative behavior behind each
// declaration and §5 for the document model.
//
// Global scope on purpose: this matches the frozen listing in PLAN.md §4.
#pragma once

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

// --- Platform seam (Pre-M0.0) --------------------------------------------
//
// §4 declares only abstract types; these are the two entry points a caller uses
// to obtain them. The seam keeps `core` free of OS access and `#ifdef` (§3):
//
//   * `create_file_system` is the only OS-dependent factory in the interface;
//     each platform backend (v1: platform/macos) defines it. Test doubles in
//     tests/ define their own `FileSystem` directly and do not use it.
//   * `create_editor` is portable and defined by `core`; it borrows the
//     FileSystem, which must outlive the returned Editor.
//
// Both return Status rather than throwing (§4 Rules: exceptions must not escape
// a boundary). On failure `*out` is left null. Added at Pre-M0.0; §4 is not
// otherwise re-frozen.
Status create_file_system(std::unique_ptr<FileSystem>* out);
Status create_editor(FileSystem& fs, std::unique_ptr<Editor>* out);
