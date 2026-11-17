// core/src/add_buffer.hpp — append-only edit buffer (§5.2).
//
// Bytes are only appended. Resident up to kResidentCap; when an insert would
// push past the cap the whole buffer spills to a temp file and subsequent
// bytes go straight to disk. A single insert larger than the cap spills too.
#pragma once

#include <cstdint>
#include <string>
#include <vector>

#include "editor.hpp"
#include "owned_fd.hpp"
#include "source.hpp"

namespace ed {

class AddBuffer final : public Source {
 public:
  static constexpr uint64_t kResidentCap = 8ull << 20;  // 8 MB (§5.2)

  AddBuffer() = default;
  ~AddBuffer() override;

  Status init(FileSystem* fs, std::string temp_dir);
  uint64_t size() const override { return size_; }
  Status read(uint64_t off, uint64_t len, char* buf, size_t cap,
              size_t* used) const override;

  // Appends `len` bytes; returns the offset of the appended run.
  Status append(const char* data, uint64_t len, uint64_t* out_off);

  bool spilled() const { return spilled_; }
  uint64_t resident_bytes() const { return resident_.size(); }

 private:
  Status spill();

  FileSystem* fs_ = nullptr;
  std::string temp_dir_;
  std::vector<char> resident_;
  OwnedFd fd_;
  std::string path_;
  bool spilled_ = false;
  uint64_t size_ = 0;
};

}  // namespace ed
