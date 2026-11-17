// tests/naive_lines.hpp — reference line splitting for property tests (§5.1).
#pragma once

#include <cstdint>
#include <string>
#include <vector>

namespace ed_test {

// Returns line starts: element L is the byte start of line L, and the last
// element is the document size. line_count = size() - 1.
inline std::vector<uint64_t> naive_line_starts(const std::string& s) {
  std::vector<uint64_t> out;
  out.push_back(0);
  size_t i = 0;
  while (i < s.size()) {
    if (s[i] == '\n') {
      i += 1;
      out.push_back(i);
    } else if (s[i] == '\r') {
      i += (i + 1 < s.size() && s[i + 1] == '\n') ? 2 : 1;
      out.push_back(i);
    } else {
      i += 1;
    }
  }
  if (out.back() != s.size()) out.push_back(s.size());
  return out;
}

inline int64_t naive_line_count(const std::string& s) {
  return static_cast<int64_t>(naive_line_starts(s).size()) - 1;
}

}  // namespace ed_test
