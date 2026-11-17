// tests/mem_source.hpp — in-memory Source for tests.
#pragma once

#include <cstdint>
#include <cstring>
#include <string>

#include "source.hpp"

namespace ed_test {

class MemSource final : public ed::Source {
 public:
  explicit MemSource(std::string data) : data_(std::move(data)) {}
  uint64_t size() const override { return data_.size(); }
  Status read(uint64_t off, uint64_t len, char* buf, size_t cap,
              size_t* used) const override {
    *used = 0;
    if (off >= data_.size()) return Status::Ok;
    const uint64_t n = std::min<uint64_t>(len, data_.size() - off);
    if (cap < n) return Status::BufferTooSmall;
    std::memcpy(buf, data_.data() + off, static_cast<size_t>(n));
    *used = static_cast<size_t>(n);
    return Status::Ok;
  }

 private:
  std::string data_;
};

}  // namespace ed_test
