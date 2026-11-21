// core/src/line_stream.hpp — buffered sequential line scanner over a Source.
//
// Yields one line at a time, including its terminator. A lone CR is a
// terminator; CRLF is one terminator. The buffer persists across calls, so a
// sequential scan reads each source byte once (used by the line index for both
// building and in-chunk scans).
#pragma once

#include <algorithm>
#include <cstddef>
#include <cstdint>
#include <cstring>

#include "source.hpp"

namespace ed {

class LineStream {
 public:
  static constexpr size_t kBlock = 16 * 1024;

  LineStream(const Source& doc, uint64_t start)
      : doc_(doc), size_(doc.size()), line_start_(start), buf_start_(start) {}

  // Produces the line starting at the current position. `has_line` is false at
  // EOF. `line_bytes` counts the line including its terminator; `has_term` is
  // false for a final line with no terminator.
  Status next(bool* has_line, uint64_t* line_bytes, bool* has_term) {
    *has_line = false;
    *line_bytes = 0;
    *has_term = false;
    while (true) {
      if (scan_ >= buf_len_) {
        Status s = refill();
        if (s != Status::Ok) return s;
        if (scan_ >= buf_len_) {
          if (line_start_ >= size_) return Status::Ok;  // EOF: no line
          *has_line = true;
          *line_bytes = size_ - line_start_;
          *has_term = false;
          line_start_ = size_;
          return Status::Ok;
        }
      }
      for (size_t i = scan_; i < buf_len_; ++i) {
        const char c = buf_[i];
        if (c != '\n' && c != '\r') continue;
        uint64_t tlen = 1;
        if (c == '\r') {
          if (i + 1 < buf_len_) {
            if (buf_[i + 1] == '\n') tlen = 2;
          } else if (buf_start_ + i + 1 < size_) {
            char nb = 0;
            size_t g = 0;
            Status s = doc_.read(buf_start_ + i + 1, 1, &nb, 1, &g);
            if (s != Status::Ok) return s;
            if (g == 1 && nb == '\n') tlen = 2;
          }
        }
        const uint64_t end = buf_start_ + i + tlen;
        *has_line = true;
        *line_bytes = end - line_start_;
        *has_term = true;
        line_start_ = end;
        scan_ = i + static_cast<size_t>(tlen);
        return Status::Ok;
      }
      scan_ = buf_len_;
    }
  }

  uint64_t position() const { return line_start_; }

 private:
  Status refill() {
    if (scan_ > 0) {
      if (scan_ >= buf_len_) {
        // Everything consumed; `scan_` may be buf_len_+1 when a CRLF straddled
        // the buffer end (the '\n' was read by lookahead), so skip past it.
        buf_start_ += scan_;
        buf_len_ = 0;
      } else {
        const size_t rem = buf_len_ - scan_;
        std::memmove(buf_, buf_ + scan_, rem);
        buf_start_ += scan_;
        buf_len_ = rem;
      }
      scan_ = 0;
    }
    if (buf_start_ + buf_len_ >= size_) return Status::Ok;
    const uint64_t want =
        std::min<uint64_t>(kBlock - buf_len_, size_ - (buf_start_ + buf_len_));
    size_t got = 0;
    Status s = doc_.read(buf_start_ + buf_len_, want, buf_ + buf_len_,
                         kBlock - buf_len_, &got);
    if (s != Status::Ok) return s;
    buf_len_ += got;
    return Status::Ok;
  }

  const Source& doc_;
  uint64_t size_;
  uint64_t line_start_;
  uint64_t buf_start_ = 0;
  char buf_[kBlock];
  size_t buf_len_ = 0;
  size_t scan_ = 0;
};

}  // namespace ed
