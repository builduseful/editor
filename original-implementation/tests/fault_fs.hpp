// tests/fault_fs.hpp — FileSystem decorator that fails a chosen operation on a
// chosen call, for save fault-injection tests (§5.5, §9, M0b).
//
// Wraps a real backend (create_file_system) so the temp/rename dance runs
// against the actual filesystem; only the armed call is turned into an error.
#pragma once

#include <memory>
#include <string>
#include <string_view>
#include <utility>

#include "editor.hpp"

namespace ed_test {

class FaultFs final : public FileSystem {
 public:
  explicit FaultFs(std::unique_ptr<FileSystem> inner)
      : inner_(std::move(inner)) {}

  // Fail the `skip`-th (1-based) subsequent call to `op` with `st`. Any earlier
  // call to `op` passes through. Arming replaces any previous fault.
  void fail_op(std::string_view op, int skip, Status st) {
    op_.assign(op);
    skip_ = skip;
    st_ = st;
    calls_ = 0;
    armed_ = true;
  }
  void clear_faults() { armed_ = false; }
  bool armed() const { return armed_; }

  Status open_readonly(std::string_view p, bool nf, int* fd) override {
    return guard("open_readonly", [&] { return inner_->open_readonly(p, nf, fd); });
  }
  Status open_write(std::string_view p, bool c, bool t, bool nf,
                    int* fd) override {
    return guard("open_write",
                 [&] { return inner_->open_write(p, c, t, nf, fd); });
  }
  Status open_scan(std::string_view p, bool nf, int* fd) override {
    return guard("open_scan", [&] { return inner_->open_scan(p, nf, fd); });
  }
  Status open_dir(std::string_view p, int* fd) override {
    return guard("open_dir", [&] { return inner_->open_dir(p, fd); });
  }
  Status create_temp(std::string_view dir, std::string* path,
                     int* fd) override {
    return guard("create_temp",
                 [&] { return inner_->create_temp(dir, path, fd); });
  }
  Status clone_to_new(std::string_view dir, std::string* path, int src,
                      int* fd) override {
    return guard("clone_to_new",
                 [&] { return inner_->clone_to_new(dir, path, src, fd); });
  }
  void close_fd(int fd) override { inner_->close_fd(fd); }
  Status map_readonly(int fd, Mapping** out) override {
    return guard("map_readonly", [&] { return inner_->map_readonly(fd, out); });
  }
  Status unmap(Mapping* m) override { return inner_->unmap(m); }
  Status pread(int fd, uint64_t off, char* buf, size_t cap,
               size_t* used) override {
    return guard("pread",
                 [&] { return inner_->pread(fd, off, buf, cap, used); });
  }
  Status pwrite(int fd, uint64_t off, const char* buf, size_t len,
                size_t* used) override {
    return guard("pwrite",
                 [&] { return inner_->pwrite(fd, off, buf, len, used); });
  }
  Status ftruncate(int fd, uint64_t size) override {
    return guard("ftruncate", [&] { return inner_->ftruncate(fd, size); });
  }
  Status fchmod(int fd, uint16_t mode) override {
    return guard("fchmod", [&] { return inner_->fchmod(fd, mode); });
  }
  Status fchown(int fd, uint32_t uid, uint32_t gid) override {
    return guard("fchown", [&] { return inner_->fchown(fd, uid, gid); });
  }
  Status fstat(int fd, Stat* out) override {
    return guard("fstat", [&] { return inner_->fstat(fd, out); });
  }
  Status fsync(int fd, bool full) override {
    return guard("fsync", [&] { return inner_->fsync(fd, full); });
  }
  Status copy_metadata(int src, int dst) override {
    return guard("copy_metadata",
                 [&] { return inner_->copy_metadata(src, dst); });
  }
  Status rename_at(int dirfd, std::string_view name,
                   std::string_view tmp) override {
    return guard("rename_at",
                 [&] { return inner_->rename_at(dirfd, name, tmp); });
  }
  Status fsync_dir(int dirfd) override {
    return guard("fsync_dir", [&] { return inner_->fsync_dir(dirfd); });
  }
  Status unlink(std::string_view p) override {
    return guard("unlink", [&] { return inner_->unlink(p); });
  }
  Status identity(std::string_view p, Rev* out) override {
    return guard("identity", [&] { return inner_->identity(p, out); });
  }
  Status resolve(std::string_view p, std::string* out) override {
    return guard("resolve", [&] { return inner_->resolve(p, out); });
  }
  Status lstat(std::string_view p, Stat* out) override {
    return guard("lstat", [&] { return inner_->lstat(p, out); });
  }
  Status lock(std::string_view p, void** lock) override {
    return guard("lock", [&] { return inner_->lock(p, lock); });
  }
  void unlock(void* lock) override { inner_->unlock(lock); }

 private:
  template <class F>
  Status guard(const char* op, F&& call) {
    if (armed_ && op_.compare(op) == 0) {
      if (++calls_ == skip_) {
        armed_ = false;
        return st_;
      }
    }
    return call();
  }

  std::unique_ptr<FileSystem> inner_;
  std::string op_;
  int skip_ = 0;
  Status st_ = Status::Io;
  int calls_ = 0;
  bool armed_ = false;
};

}  // namespace ed_test
