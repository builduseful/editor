#include "add_buffer.hpp"

#include <algorithm>
#include <cstring>

namespace ed {

AddBuffer::~AddBuffer() {
  if (spilled_ && fs_ != nullptr) {
    fd_.reset();
    if (!path_.empty()) fs_->unlink(path_);
  }
}

Status AddBuffer::init(FileSystem* fs, std::string temp_dir) {
  fs_ = fs;
  temp_dir_ = std::move(temp_dir);
  return Status::Ok;
}

Status AddBuffer::spill() {
  if (spilled_) return Status::Ok;
  if (fs_ == nullptr) return Status::Internal;

  std::string path;
  int fd = -1;
  Status s = fs_->create_temp(temp_dir_, &path, &fd);
  if (s != Status::Ok) return s;

  if (!resident_.empty()) {
    size_t used = 0;
    s = fs_->pwrite(fd, 0, resident_.data(), resident_.size(), &used);
    if (s != Status::Ok || used != resident_.size()) {
      fs_->close_fd(fd);
      fs_->unlink(path);
      return s != Status::Ok ? s : Status::Io;
    }
  }

  fd_.adopt(fs_, fd);
  path_ = path;
  spilled_ = true;
  resident_.clear();
  resident_.shrink_to_fit();
  return Status::Ok;
}

Status AddBuffer::append(const char* data, uint64_t len, uint64_t* out_off) {
  *out_off = size_;
  if (len == 0) return Status::Ok;

  if (!spilled_ && size_ + len <= kResidentCap) {
    resident_.insert(resident_.end(), data, data + len);
    size_ += len;
    return Status::Ok;
  }

  if (!spilled_) {
    Status s = spill();
    if (s != Status::Ok) return s;
  }

  size_t used = 0;
  Status s = fs_->pwrite(fd_.get(), size_, data, len, &used);
  if (s != Status::Ok) return s;
  if (used != len) return Status::Io;
  size_ += len;
  return Status::Ok;
}

Status AddBuffer::read(uint64_t off, uint64_t len, char* buf, size_t cap,
                       size_t* used) const {
  *used = 0;
  if (off >= size_) return Status::Ok;
  const uint64_t n = std::min<uint64_t>(len, size_ - off);
  if (cap < n) return Status::BufferTooSmall;

  if (!spilled_) {
    std::memcpy(buf, resident_.data() + off, static_cast<size_t>(n));
    *used = static_cast<size_t>(n);
    return Status::Ok;
  }

  size_t got = 0;
  Status s = fs_->pread(fd_.get(), off, buf, cap, &got);
  if (s != Status::Ok) return s;
  if (got != n) return Status::Internal;
  *used = got;
  return Status::Ok;
}

}  // namespace ed
