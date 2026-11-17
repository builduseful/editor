// platform/macos/fs_macos.cpp — the macOS FileSystem backend (§4).
//
// The only OS-dependent factory in the frozen interface. Uses POSIX + a couple
// of macOS extras (clonefile, F_FULLFSYNC, F_GETPATH).
#include <fcntl.h>
#include <sys/clonefile.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

#include <cerrno>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <memory>
#include <string>

#include "editor.hpp"

namespace {

Status errno_status(int e) {
  switch (e) {
    case 0: return Status::Ok;
    case ENOENT: return Status::NotFound;
    case EEXIST: return Status::Conflict;
    case EACCES:
    case EPERM:
    case EISDIR:
    case ENOTDIR:
    case ELOOP: return Status::Io;
    case ENOTSUP: return Status::Unsupported;
    case ENOSPC: return Status::Oom;
    default: return Status::Io;
  }
}

void fill_stat(const struct stat& st, Stat* out) {
  *out = Stat{};
  out->dev = static_cast<uint64_t>(st.st_dev);
  out->ino = static_cast<uint64_t>(st.st_ino);
  out->size = static_cast<uint64_t>(st.st_size);
  out->mode = static_cast<uint16_t>(st.st_mode & 07777);
  out->uid = static_cast<uint32_t>(st.st_uid);
  out->gid = static_cast<uint32_t>(st.st_gid);
  out->mtime_ns = static_cast<uint64_t>(st.st_mtimespec.tv_sec) * 1000000000ull +
                  static_cast<uint64_t>(st.st_mtimespec.tv_nsec);
  out->ctime_ns = static_cast<uint64_t>(st.st_ctimespec.tv_sec) * 1000000000ull +
                  static_cast<uint64_t>(st.st_ctimespec.tv_nsec);
  out->regular = S_ISREG(st.st_mode);
}

void fill_rev(const struct stat& st, Rev* out) {
  *out = Rev{};
  out->dev = static_cast<uint64_t>(st.st_dev);
  out->ino = static_cast<uint64_t>(st.st_ino);
  out->size = static_cast<uint64_t>(st.st_size);
  out->mtime_ns = static_cast<uint64_t>(st.st_mtimespec.tv_sec) * 1000000000ull +
                  static_cast<uint64_t>(st.st_mtimespec.tv_nsec);
  out->ctime_ns = static_cast<uint64_t>(st.st_ctimespec.tv_sec) * 1000000000ull +
                  static_cast<uint64_t>(st.st_ctimespec.tv_nsec);
}

class PosixMapping final : public Mapping {
 public:
  PosixMapping(void* base, size_t len) : base_(base), len_(len) {}
  ~PosixMapping() override {
    if (base_ != nullptr && base_ != MAP_FAILED) munmap(base_, len_);
  }
  Status window(uint64_t, size_t) override { return Status::Unsupported; }
  const char* data() const override { return static_cast<const char*>(base_); }
  size_t size() const override { return len_; }

 private:
  void* base_ = nullptr;
  size_t len_ = 0;
};

struct LockHandle {
  std::string path;
};

class PosixFileSystem final : public FileSystem {
 public:
  Status open_readonly(std::string_view path, bool nofollow,
                       int* out_fd) override {
    const int flags = O_RDONLY | (nofollow ? O_NOFOLLOW : 0);
    const int fd = ::open(std::string(path).c_str(), flags);
    if (fd < 0) return errno_status(errno);
    *out_fd = fd;
    return Status::Ok;
  }

  Status open_write(std::string_view path, bool create, bool truncate,
                    bool nofollow, int* out_fd) override {
    int flags = O_WRONLY;
    if (create) flags |= O_CREAT | O_EXCL;
    if (truncate) flags |= O_TRUNC;
    if (nofollow) flags |= O_NOFOLLOW;
    const int fd = ::open(std::string(path).c_str(), flags, 0666);
    if (fd < 0) return errno_status(errno);
    *out_fd = fd;
    return Status::Ok;
  }

  Status open_scan(std::string_view path, bool nofollow, int* out_fd) override {
    const int flags = O_RDONLY | (nofollow ? O_NOFOLLOW : 0);
    const int fd = ::open(std::string(path).c_str(), flags);
    if (fd < 0) return errno_status(errno);
    (void)fcntl(fd, F_NOCACHE, 1);
    *out_fd = fd;
    return Status::Ok;
  }

  Status open_dir(std::string_view path, int* out_fd) override {
    const int fd = ::open(std::string(path).c_str(), O_RDONLY | O_DIRECTORY);
    if (fd < 0) return errno_status(errno);
    *out_fd = fd;
    return Status::Ok;
  }

  Status create_temp(std::string_view dir, std::string* out_path,
                     int* out_fd) override {
    static uint64_t counter = 0;
    for (int attempt = 0; attempt < 64; ++attempt) {
      char name[64];
      std::snprintf(name, sizeof(name), "/.editor-tmp-%d-%llu", ::getpid(),
                    static_cast<unsigned long long>(counter++));
      const std::string p = std::string(dir) + name;
      const int fd = ::open(p.c_str(), O_WRONLY | O_CREAT | O_EXCL, 0600);
      if (fd >= 0) {
        *out_path = p;
        *out_fd = fd;
        return Status::Ok;
      }
      if (errno != EEXIST) return errno_status(errno);
    }
    return Status::Io;
  }

  Status clone_to_new(std::string_view dst_dir, std::string* out_path,
                      int src_fd, int* out_fd) override {
    char src[4096];
    if (fcntl(src_fd, F_GETPATH, src) < 0) return errno_status(errno);
    static uint64_t counter = 0;
    for (int attempt = 0; attempt < 64; ++attempt) {
      char name[64];
      std::snprintf(name, sizeof(name), "/.editor-clone-%d-%llu", ::getpid(),
                    static_cast<unsigned long long>(counter++));
      const std::string p = std::string(dst_dir) + name;
      if (clonefile(src, p.c_str(), 0) == 0) {
        const int fd = ::open(p.c_str(), O_RDWR);
        if (fd < 0) {
          ::unlink(p.c_str());
          return errno_status(errno);
        }
        *out_path = p;
        *out_fd = fd;
        return Status::Ok;
      }
      if (errno != EEXIST) return errno_status(errno);
    }
    return Status::Io;
  }

  void close_fd(int fd) override {
    if (fd >= 0) ::close(fd);
  }

  Status map_readonly(int fd, Mapping** out) override {
    struct stat st;
    if (::fstat(fd, &st) < 0) return errno_status(errno);
    if (st.st_size == 0) {
      *out = new PosixMapping(nullptr, 0);
      return Status::Ok;
    }
    void* base = ::mmap(nullptr, static_cast<size_t>(st.st_size), PROT_READ,
                        MAP_PRIVATE, fd, 0);
    if (base == MAP_FAILED) return errno_status(errno);
    (void)madvise(base, static_cast<size_t>(st.st_size), MADV_SEQUENTIAL);
    *out = new PosixMapping(base, static_cast<size_t>(st.st_size));
    return Status::Ok;
  }

  Status unmap(Mapping* m) override {
    delete m;
    return Status::Ok;
  }

  Status pread(int fd, uint64_t off, char* buf, size_t cap,
               size_t* used) override {
    *used = 0;
    const ssize_t r = ::pread(fd, buf, cap, static_cast<off_t>(off));
    if (r < 0) return errno_status(errno);
    *used = static_cast<size_t>(r);
    return Status::Ok;
  }

  Status pwrite(int fd, uint64_t off, const char* buf, size_t len,
                size_t* used) override {
    *used = 0;
    size_t done = 0;
    while (done < len) {
      const ssize_t w =
          ::pwrite(fd, buf + done, len - done, static_cast<off_t>(off + done));
      if (w < 0) return errno_status(errno);
      if (w == 0) return Status::Io;
      done += static_cast<size_t>(w);
    }
    *used = done;
    return Status::Ok;
  }

  Status ftruncate(int fd, uint64_t size) override {
    if (::ftruncate(fd, static_cast<off_t>(size)) < 0) return errno_status(errno);
    return Status::Ok;
  }

  Status fchmod(int fd, uint16_t mode) override {
    if (::fchmod(fd, static_cast<mode_t>(mode)) < 0) return errno_status(errno);
    return Status::Ok;
  }

  Status fchown(int fd, uint32_t uid, uint32_t gid) override {
    if (::fchown(fd, static_cast<uid_t>(uid), static_cast<gid_t>(gid)) < 0) {
      return errno_status(errno);
    }
    return Status::Ok;
  }

  Status fstat(int fd, Stat* out) override {
    struct stat st;
    if (::fstat(fd, &st) < 0) return errno_status(errno);
    fill_stat(st, out);
    return Status::Ok;
  }

  Status fsync(int fd, bool full) override {
    if (full) {
      if (fcntl(fd, F_FULLFSYNC) < 0) {
        if (errno == ENOTSUP) return Status::Unsupported;
        return errno_status(errno);
      }
      return Status::Ok;
    }
    if (::fsync(fd) < 0) return errno_status(errno);
    return Status::Ok;
  }

  Status copy_metadata(int src_fd, int dst_fd) override {
    struct stat st;
    if (::fstat(src_fd, &st) < 0) return errno_status(errno);
    if (::fchown(dst_fd, st.st_uid, st.st_gid) < 0) return errno_status(errno);
    if (::fchmod(dst_fd, st.st_mode & 07777) < 0) return errno_status(errno);
    return Status::Ok;
  }

  Status rename_at(int dst_dirfd, std::string_view dst_name,
                   std::string_view tmp_path) override {
    const std::string name(dst_name);
    const std::string tmp(tmp_path);
    // Rename tmp_path -> dst_dirfd/name, replacing dst atomically.
    if (::renameat(AT_FDCWD, tmp.c_str(), dst_dirfd, name.c_str()) < 0) {
      return errno_status(errno);
    }
    return Status::Ok;
  }

  Status fsync_dir(int dirfd) override {
    if (::fsync(dirfd) < 0) return errno_status(errno);
    return Status::Ok;
  }

  Status unlink(std::string_view path) override {
    if (::unlink(std::string(path).c_str()) < 0) return errno_status(errno);
    return Status::Ok;
  }

  Status identity(std::string_view path, Rev* out) override {
    struct stat st;
    if (::stat(std::string(path).c_str(), &st) < 0) return errno_status(errno);
    fill_rev(st, out);
    return Status::Ok;
  }

  Status resolve(std::string_view path, std::string* out_realpath) override {
    char buf[4096];
    if (realpath(std::string(path).c_str(), buf) == nullptr) {
      return errno_status(errno);
    }
    *out_realpath = buf;
    return Status::Ok;
  }

  Status lstat(std::string_view path, Stat* out) override {
    struct stat st;
    if (::lstat(std::string(path).c_str(), &st) < 0) return errno_status(errno);
    fill_stat(st, out);
    return Status::Ok;
  }

  Status lock(std::string_view path, void** out) override {
    const std::string lp = std::string(path) + ".editor.lock";
    const int fd = ::open(lp.c_str(), O_WRONLY | O_CREAT | O_EXCL, 0600);
    if (fd < 0) return errno_status(errno);
    const std::string token = std::to_string(::getpid());
    (void)::write(fd, token.data(), token.size());
    ::close(fd);
    auto* h = new LockHandle{lp};
    *out = h;
    return Status::Ok;
  }

  void unlock(void* lock) override {
    if (lock == nullptr) return;
    auto* h = static_cast<LockHandle*>(lock);
    ::unlink(h->path.c_str());
    delete h;
  }
};

}  // namespace

Status create_file_system(std::unique_ptr<FileSystem>* out) {
  if (out == nullptr) return Status::Internal;
  *out = std::make_unique<PosixFileSystem>();
  return Status::Ok;
}
