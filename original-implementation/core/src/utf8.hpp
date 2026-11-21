// core/src/utf8.hpp — UTF-8 validation and newline normalization (§5.1).
//
// Factored out of editor_impl.cpp so the validator can be fuzzed directly
// (PLAN.md §9: fuzz UTF-8). Small, self-contained, no OS access.
#pragma once

#include <cstddef>
#include <string>
#include <string_view>

#include "editor.hpp"

namespace ed {

inline bool is_continuation(char c) {
  return (static_cast<unsigned char>(c) & 0xC0) == 0x80;
}

// True iff `s` is well-formed UTF-8: no overlong forms, no surrogates, no
// codepoints above U+10FFFF, no truncated sequences.
inline bool valid_utf8(std::string_view s) {
  size_t i = 0;
  const size_t n = s.size();
  while (i < n) {
    const unsigned char c = static_cast<unsigned char>(s[i]);
    size_t extra = 0;
    unsigned int cp = 0;
    if (c < 0x80) {
      i += 1;
      continue;
    } else if ((c & 0xE0) == 0xC0) {
      extra = 1;
      cp = c & 0x1Fu;
    } else if ((c & 0xF0) == 0xE0) {
      extra = 2;
      cp = c & 0x0Fu;
    } else if ((c & 0xF8) == 0xF0) {
      extra = 3;
      cp = c & 0x07u;
    } else {
      return false;
    }
    if (i + extra >= n) return false;
    for (size_t k = 1; k <= extra; ++k) {
      const unsigned char cc = static_cast<unsigned char>(s[i + k]);
      if ((cc & 0xC0) != 0x80) return false;
      cp = (cp << 6) | (cc & 0x3Fu);
    }
    if (extra == 1 && cp < 0x80) return false;
    if (extra == 2 && cp < 0x800) return false;
    if (extra == 3 && cp < 0x10000) return false;
    if (cp > 0x10FFFF) return false;
    if (cp >= 0xD800 && cp <= 0xDFFF) return false;
    i += extra + 1;
  }
  return true;
}

// Rewrites CR, LF, and CRLF terminators to the dominant EOL (§5.1). Input is
// assumed valid UTF-8; terminators are ASCII so byte-wise rewriting is safe.
inline std::string normalize_newlines(std::string_view s, Eol eol) {
  const char* rep = eol == Eol::CrLf ? "\r\n" : (eol == Eol::Cr ? "\r" : "\n");
  const size_t replen = eol == Eol::CrLf ? 2 : 1;
  std::string out;
  out.reserve(s.size());
  for (size_t i = 0; i < s.size(); ++i) {
    const char c = s[i];
    if (c == '\r') {
      out.append(rep, replen);
      if (i + 1 < s.size() && s[i + 1] == '\n') i += 1;
    } else if (c == '\n') {
      out.append(rep, replen);
    } else {
      out.push_back(c);
    }
  }
  return out;
}

}  // namespace ed
