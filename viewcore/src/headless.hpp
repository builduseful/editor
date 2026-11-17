// viewcore/src/headless.hpp — the headless render oracle (§8, M0.5).
//
// A deterministic monospace textbackend and a Canvas that records draw commands.
// Together they let viewcore's layout and drawing be validated without AppKit,
// and back the 5 GB read-only demo.
#pragma once

#include <cstdint>
#include <cstdio>
#include <string>
#include <string_view>
#include <vector>

#include "canvas.hpp"
#include "textbackend.hpp"
#include "types.hpp"

namespace vc {

namespace detail {
inline size_t cp_len(std::string_view s, size_t i) {
  const unsigned char c = static_cast<unsigned char>(s[i]);
  size_t n = 1;
  if ((c & 0xE0) == 0xC0) {
    n = 2;
  } else if ((c & 0xF0) == 0xE0) {
    n = 3;
  } else if ((c & 0xF8) == 0xF0) {
    n = 4;
  }
  if (i + n > s.size()) n = s.size() - i;
  return n;
}
}  // namespace detail

// Fixed-advance monospace backend: one glyph per codepoint.
class HeadlessTextBackend final : public TextBackend {
 public:
  explicit HeadlessTextBackend(double advance = 8.0, double line_h = 16.0)
      : advance_(advance), line_h_(line_h) {}

  double line_height(const FontSpec&) override { return line_h_; }

  double measure(const FontSpec&, std::string_view text) override {
    return static_cast<double>(count_cp(text)) * advance_;
  }

  Run shape(const FontSpec& f, std::string_view text) override {
    Run r;
    r.font = f;
    r.text.assign(text);
    double x = 0;
    for (size_t i = 0; i < text.size();) {
      const size_t n = detail::cp_len(text, i);
      Glyph g;
      g.codepoint = static_cast<unsigned char>(text[i]);
      g.x = x;
      g.advance = advance_;
      r.glyphs.push_back(g);
      x += advance_;
      i += n;
    }
    r.width = x;
    r.ascent = line_h_ * 0.8;
    r.descent = line_h_ - r.ascent;
    return r;
  }

  void vertical_metrics(const FontSpec&, double* ascent, double* descent) override {
    *ascent = line_h_ * 0.8;
    *descent = line_h_ - *ascent;
  }

 private:
  static size_t count_cp(std::string_view s) {
    size_t n = 0;
    for (size_t i = 0; i < s.size();) {
      i += detail::cp_len(s, i);
      n += 1;
    }
    return n;
  }
  double advance_, line_h_;
};

struct DrawCmd {
  enum class Kind { Clear, FillRect, DrawRun };
  Kind kind = Kind::Clear;
  Color color;
  Rect rect;
  std::string text;
  double x = 0, y = 0;
  int64_t line = -1;
};

// Records draw commands in order; `dump()` is a stable textual oracle.
class RecordingCanvas final : public Canvas {
 public:
  void clear(Color c) override {
    DrawCmd d;
    d.kind = DrawCmd::Kind::Clear;
    d.color = c;
    cmds.push_back(std::move(d));
  }
  void fill_rect(const Rect& r, Color c) override {
    DrawCmd d;
    d.kind = DrawCmd::Kind::FillRect;
    d.rect = r;
    d.color = c;
    cmds.push_back(std::move(d));
  }
  void draw_run(const Run& run, double x, double y) override {
    DrawCmd d;
    d.kind = DrawCmd::Kind::DrawRun;
    d.text = run.text;
    d.x = x;
    d.y = y;
    d.line = run.logical_line;
    cmds.push_back(std::move(d));
  }

  std::string dump() const {
    std::string out;
    char buf[256];
    for (const DrawCmd& d : cmds) {
      switch (d.kind) {
        case DrawCmd::Kind::Clear:
          std::snprintf(buf, sizeof(buf), "clear %u,%u,%u,%u\n", d.color.r,
                        d.color.g, d.color.b, d.color.a);
          out += buf;
          break;
        case DrawCmd::Kind::FillRect:
          std::snprintf(buf, sizeof(buf), "fill %.3f,%.3f,%.3f,%.3f %u,%u,%u,%u\n",
                        d.rect.x, d.rect.y, d.rect.w, d.rect.h, d.color.r,
                        d.color.g, d.color.b, d.color.a);
          out += buf;
          break;
        case DrawCmd::Kind::DrawRun:
          std::snprintf(buf, sizeof(buf), "run \"%s\" x=%.3f y=%.3f line=%lld\n",
                        d.text.c_str(), d.x, d.y,
                        static_cast<long long>(d.line));
          out += buf;
          break;
      }
    }
    return out;
  }

  std::vector<DrawCmd> cmds;
};

}  // namespace vc
