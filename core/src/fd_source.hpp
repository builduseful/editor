// core/src/fd_source.hpp — Sources over a file descriptor and over a logical
// (post-BOM) view of another Source.
#pragma once

#include <cstdint>

#include "source.hpp"

namespace ed {

// Reads raw bytes of a file via FileSystem::pread.
class FdSource final : public Source {
 public:
  FdSource() = default;
  void reset(FileSystem* fs, int fd, uint64_t size) {
    fs_ = fs;
    fd_ = fd;
    size_ = size;
  }
  uint64_t size() const override { return size_; }
  Status read(uint64_t off, uint64_t len, char* buf, size_t cap,
              size_t* used) const override {
    *used = 0;
    if (off >= size_) return Status::Ok;
    const uint64_t n = len < size_ - off ? len : size_ - off;
    if (cap < n) return Status::BufferTooSmall;
    size_t got = 0;
    Status s = fs_->pread(fd_, off, buf, cap, &got);
    if (s != Status::Ok) return s;
    *used = got;
    return Status::Ok;
  }

 private:
  FileSystem* fs_ = nullptr;
  int fd_ = -1;
  uint64_t size_ = 0;
};

// Presents `inner[base, base+size)` as `[0, size)`. Used to hide a UTF-8 BOM
// from the logical document (§5.1).
class OffsetSource final : public Source {
 public:
  OffsetSource() = default;
  void reset(const Source* inner, uint64_t base) {
    inner_ = inner;
    base_ = base;
  }
  uint64_t size() const override { return inner_->size() - base_; }
  Status read(uint64_t off, uint64_t len, char* buf, size_t cap,
              size_t* used) const override {
    return inner_->read(base_ + off, len, buf, cap, used);
  }

 private:
  const Source* inner_ = nullptr;
  uint64_t base_ = 0;
};

}  // namespace ed
