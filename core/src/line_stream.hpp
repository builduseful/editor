// core/src/line_stream.hpp — sequential line scanner over a Source.
//
// Yields one line at a time, including its terminator. A lone CR is a
// terminator; CRLF is one terminator. Used by the line index (build + in-chunk
// scan) and by the editor for line-shaped reads (§5.1).
#pragma once

#include <algorithm>
#include <cstddef>
#include <cstdint>

#include "source.hpp"

namespace ed {

class LineStream {
 public:
  static constexpr size_t kBlock = 64 * 1024;

  LineStream(const Source& doc, uint64_t start)
      : doc_(doc), size_(doc.size()), line_start_(start) {}

  // Produces the line starting at the current position. `has_line` is false at
  // EOF. `line_bytes` counts the line including its terminator; `has_term` is
  // false for a final line with no terminator.
  Status next(bool* has_line, uint64_t* line_bytes, bool* has_term) {
    if (line_start_ >= size_) {
      *has_line = false;
      *line_bytes = 0;
      *has_term = false;
      return Status::Ok;
    }
    *has_line = true;
    char buf[kBlock];
    uint64_t pos = line_start_;
    while (pos < size_) {
      uint64_t want = std::min<uint64_t>(kBlock, size_ - pos);
      size_t got = 0;
      Status s = doc_.read(pos, want, buf, sizeof(buf), &got);
      if (s != Status::Ok) return s;
      if (got == 0) break;
      for (size_t i = 0; i < got; ++i) {
        const char c = buf[i];
        if (c == '\n') {
          *line_bytes = pos + i + 1 - line_start_;
          *has_term = true;
          line_start_ = pos + i + 1;
          return Status::Ok;
        }
        if (c == '\r') {
          uint64_t tlen = 1;
          if (i + 1 < got) {
            if (buf[i + 1] == '\n') tlen = 2;
          } else if (pos + i + 1 < size_) {
            char nb = 0;
            size_t g2 = 0;
            Status s2 = doc_.read(pos + i + 1, 1, &nb, 1, &g2);
            if (s2 != Status::Ok) return s2;
            if (g2 == 1 && nb == '\n') tlen = 2;
          }
          *line_bytes = pos + i + tlen - line_start_;
          *has_term = true;
          line_start_ = pos + i + tlen;
          return Status::Ok;
        }
      }
      pos += got;
    }
    *line_bytes = size_ - line_start_;
    *has_term = false;
    line_start_ = size_;
    return Status::Ok;
  }

  uint64_t position() const { return line_start_; }

 private:
  const Source& doc_;
  uint64_t size_;
  uint64_t line_start_;
};

}  // namespace ed
