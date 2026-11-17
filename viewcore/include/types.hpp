// viewcore/include/types.hpp — platform-neutral view types (§8).
//
// Shared by `Canvas`, `textbackend`, and `viewcore`. No OS access, no AppKit.
#pragma once

#include <cstdint>
#include <string>
#include <vector>

namespace vc {

struct Color {
  uint8_t r = 0, g = 0, b = 0, a = 255;
};

struct Rect {
  double x = 0, y = 0, w = 0, h = 0;
};

struct FontSpec {
  std::string family;  // "" -> the backend's default mono face
  double size = 14.0;
  bool bold = false;
  bool italic = false;

  bool operator==(const FontSpec& o) const {
    return family == o.family && size == o.size && bold == o.bold &&
           italic == o.italic;
  }
  bool operator!=(const FontSpec& o) const { return !(*this == o); }
};

struct Glyph {
  uint32_t codepoint = 0;
  double x = 0;        // pen offset within the run
  double advance = 0;  // pen advance for this glyph
};

// A shaped run: one piece of text measured by the backend. The skeleton backend
// emits one glyph per codepoint; a real backend (CoreText) may merge or reorder.
struct Run {
  FontSpec font;
  std::string text;           // the source UTF-8 text of the run
  std::vector<Glyph> glyphs;  // per-codepoint metrics, in text order
  double width = 0;           // total advance
  double ascent = 0, descent = 0;
  int64_t logical_line = -1;  // source logical line (viewcore bookkeeping)
  uint64_t byte_start = 0;    // start byte within the logical line
};

}  // namespace vc
