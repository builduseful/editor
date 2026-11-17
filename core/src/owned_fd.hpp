// core/src/owned_fd.hpp — RAII fd owned by core (§4 Ownership).
//
// Closing requires the FileSystem that produced the fd, so an OwnedFd carries
// both. Use it for the base fd, the add-buffer fd, and all spill fds.
#pragma once

namespace ed {

class OwnedFd {
 public:
  OwnedFd() = default;
  OwnedFd(FileSystem* fs, int fd) : fs_(fs), fd_(fd) {}
  ~OwnedFd() { reset(); }

  OwnedFd(OwnedFd&& o) noexcept : fs_(o.fs_), fd_(o.fd_) {
    o.fs_ = nullptr;
    o.fd_ = -1;
  }
  OwnedFd& operator=(OwnedFd&& o) noexcept {
    if (this != &o) {
      reset();
      fs_ = o.fs_;
      fd_ = o.fd_;
      o.fs_ = nullptr;
      o.fd_ = -1;
    }
    return *this;
  }
  OwnedFd(const OwnedFd&) = delete;
  OwnedFd& operator=(const OwnedFd&) = delete;

  void reset() {
    if (fd_ >= 0 && fs_ != nullptr) fs_->close_fd(fd_);
    fd_ = -1;
    fs_ = nullptr;
  }
  void adopt(FileSystem* fs, int fd) {
    reset();
    fs_ = fs;
    fd_ = fd;
  }
  int get() const { return fd_; }
  bool valid() const { return fd_ >= 0; }
  int release() {
    int f = fd_;
    fd_ = -1;
    fs_ = nullptr;
    return f;
  }

 private:
  FileSystem* fs_ = nullptr;
  int fd_ = -1;
};

}  // namespace ed
