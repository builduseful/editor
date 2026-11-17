// tests/utf8_test.cpp — UTF-8 validator and newline normalization (§5.1, §9 fuzz).
//
// The validator is checked against an independently-written reference: a
// permissive decoder that accepts any structurally-valid sequence, then
// requires canonical re-encoding (which rejects overlong forms, surrogates, and
// out-of-range codepoints). Random byte strings are compared against it.
#include <cstdint>
#include <string>
#include <vector>

#include "check.hpp"
#include "prng.hpp"
#include "utf8.hpp"

namespace {

// Permissive decode: checks only that continuation bytes are well-formed.
bool decode_permissive(const std::string& s, std::vector<uint32_t>* out) {
  out->clear();
  size_t i = 0;
  while (i < s.size()) {
    const unsigned char b = static_cast<unsigned char>(s[i]);
    int len = 0;
    uint32_t cp = 0;
    if (b < 0x80) {
      len = 1;
      cp = b;
    } else if ((b & 0xE0) == 0xC0) {
      len = 2;
      cp = b & 0x1Fu;
    } else if ((b & 0xF0) == 0xE0) {
      len = 3;
      cp = b & 0x0Fu;
    } else if ((b & 0xF8) == 0xF0) {
      len = 4;
      cp = b & 0x07u;
    } else {
      return false;
    }
    if (i + static_cast<size_t>(len) > s.size()) return false;
    for (int k = 1; k < len; ++k) {
      const unsigned char c = static_cast<unsigned char>(s[i + static_cast<size_t>(k)]);
      if ((c & 0xC0) != 0x80) return false;
      cp = (cp << 6) | (c & 0x3Fu);
    }
    out->push_back(cp);
    i += static_cast<size_t>(len);
  }
  return true;
}

std::string encode_canonical(const std::vector<uint32_t>& cps) {
  std::string out;
  for (uint32_t cp : cps) {
    if (cp < 0x80) {
      out.push_back(static_cast<char>(cp));
    } else if (cp < 0x800) {
      out.push_back(static_cast<char>(0xC0 | (cp >> 6)));
      out.push_back(static_cast<char>(0x80 | (cp & 0x3F)));
    } else if (cp < 0x10000) {
      out.push_back(static_cast<char>(0xE0 | (cp >> 12)));
      out.push_back(static_cast<char>(0x80 | ((cp >> 6) & 0x3F)));
      out.push_back(static_cast<char>(0x80 | (cp & 0x3F)));
    } else {
      out.push_back(static_cast<char>(0xF0 | (cp >> 18)));
      out.push_back(static_cast<char>(0x80 | ((cp >> 12) & 0x3F)));
      out.push_back(static_cast<char>(0x80 | ((cp >> 6) & 0x3F)));
      out.push_back(static_cast<char>(0x80 | (cp & 0x3F)));
    }
  }
  return out;
}

bool reference_valid(const std::string& s) {
  std::vector<uint32_t> cps;
  if (!decode_permissive(s, &cps)) return false;
  for (uint32_t cp : cps) {
    if (cp > 0x10FFFF) return false;
    if (cp >= 0xD800 && cp <= 0xDFFF) return false;
  }
  return encode_canonical(cps) == s;
}

}  // namespace

TEST(utf8_validator_known_cases) {
  CHECK(ed::valid_utf8(""));
  CHECK(ed::valid_utf8("hello\n"));
  CHECK(ed::valid_utf8("\xC3\xA9"));                 // é
  CHECK(ed::valid_utf8("\xE2\x82\xAC"));             // €
  CHECK(ed::valid_utf8("\xF0\x9F\x98\x80"));         // emoji U+1F600
  CHECK(ed::valid_utf8("\xF4\x8F\xBF\xBF"));         // U+10FFFF (max)
  CHECK(!ed::valid_utf8("\x80"));                    // lone continuation
  CHECK(!ed::valid_utf8("\xC0\x80"));                // overlong NUL
  CHECK(!ed::valid_utf8("\xC1\xBF"));                // overlong
  CHECK(!ed::valid_utf8("\xE0\x80\x80"));            // overlong 3-byte
  CHECK(!ed::valid_utf8("\xF0\x80\x80\x80"));        // overlong 4-byte
  CHECK(!ed::valid_utf8("\xED\xA0\x80"));            // surrogate U+D800
  CHECK(!ed::valid_utf8("\xED\xBF\xBF"));            // surrogate U+DFFF
  CHECK(!ed::valid_utf8("\xF5\x80\x80\x80"));        // > U+10FFFF
  CHECK(!ed::valid_utf8("\xC3"));                    // truncated
  CHECK(!ed::valid_utf8("\xE2\x82"));                // truncated
  CHECK(!ed::valid_utf8("\xF0\x9F\x98"));            // truncated
  CHECK(!ed::valid_utf8("\xFF"));                    // invalid lead
  CHECK(!ed::valid_utf8("\xFE"));
  CHECK(!ed::valid_utf8("a\xC3\xA9" "b\x80" "c"));  // bad byte mid-string

  CHECK(ed::is_continuation('\x80'));
  CHECK(ed::is_continuation('\xBF'));
  CHECK(!ed::is_continuation('\x7F'));
  CHECK(!ed::is_continuation('\xC0'));
}

TEST(utf8_validator_fuzz_matches_reference) {
  ed_test::Prng rng(0x5EED1234);
  // A byte alphabet biased toward UTF-8 lead/continuation bytes so random
  // strings actually exercise multi-byte paths.
  const unsigned char alpha[] = {0x00, 0x41, 0x7F, 0x80, 0x8F, 0xBF, 0xC0,
                                 0xC2, 0xDF, 0xE0, 0xE1, 0xED, 0xEF, 0xF0,
                                 0xF4, 0xF5, 0xFF};
  const size_t alen = sizeof(alpha) / sizeof(alpha[0]);
  int mismatches = 0;
  for (int iter = 0; iter < 200000; ++iter) {
    const size_t n = rng.below(12);
    std::string s;
    s.reserve(n);
    for (size_t i = 0; i < n; ++i) {
      s.push_back(static_cast<char>(alpha[rng.below(alen)]));
    }
    const bool a = ed::valid_utf8(s);
    const bool b = reference_valid(s);
    if (a != b) {
      ++mismatches;
      if (mismatches <= 3) {
        std::printf("mismatch: valid_utf8=%d ref=%d bytes=", a ? 1 : 0, b ? 1 : 0);
        for (char c : s) std::printf("%02X", static_cast<unsigned char>(c));
        std::printf("\n");
      }
    }
  }
  CHECK_EQ(mismatches, 0);
}

TEST(utf8_normalize_newlines_properties) {
  ed_test::Prng rng(0xC0FFEE);
  const char* pieces[] = {"a", "b", "\n", "\r", "\r\n", "\xE2\x82\xAC", "z\n\r"};
  const Eol eols[] = {Eol::Lf, Eol::CrLf, Eol::Cr};
  for (int iter = 0; iter < 20000; ++iter) {
    std::string s;
    const size_t n = rng.below(10);
    for (size_t i = 0; i < n; ++i) s += pieces[rng.below(7)];
    for (Eol e : eols) {
      const std::string out = ed::normalize_newlines(s, e);
      // Normalizing an already-normalized string is a no-op.
      CHECK(ed::normalize_newlines(out, e) == out);
      // The result is valid UTF-8 iff the input is (terminators are ASCII).
      CHECK_EQ(ed::valid_utf8(out), ed::valid_utf8(s));
      // Every terminator is the target; no CR/LF runs remain.
      for (size_t i = 0; i < out.size(); ++i) {
        if (out[i] == '\r') {
          CHECK(e == Eol::CrLf || e == Eol::Cr);
          if (e == Eol::CrLf) {
            CHECK(i + 1 < out.size() && out[i + 1] == '\n');
            i += 1;
          }
        } else if (out[i] == '\n') {
          CHECK(e == Eol::CrLf || e == Eol::Lf);
        }
      }
    }
  }
}
