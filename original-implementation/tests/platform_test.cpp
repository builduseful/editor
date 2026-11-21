// tests/platform_test.cpp — the macOS CoreText/CoreGraphics backends (§3, §8).
//
// CoreText and CoreGraphics both work without a window, so the platform layer is
// conformance-tested headlessly: shaping/metrics against the neutral Run model,
// and drawing into a bitmap checked at the pixel level.
#include <CoreGraphics/CoreGraphics.h>

#include <cmath>
#include <cstdint>
#include <memory>
#include <string>
#include <vector>

#include "canvas_cg.hpp"
#include "check.hpp"
#include "text_coretext.hpp"

namespace {

const vc::FontSpec kMono{"", 14.0, false, false};  // default mono face (Menlo)

// A CGBitmapContext in RGBA8, plus helpers to inspect it. Data rows are stored
// top-to-bottom while CoreGraphics coordinates are y-up.
struct Bitmap {
  size_t w = 0, h = 0;
  std::vector<uint8_t> px;
  CGContextRef ctx = nullptr;
  CGColorSpaceRef cs = nullptr;

  Bitmap(size_t width, size_t height)
      : w(width), h(height), px(width * height * 4, 0) {
    cs = CGColorSpaceCreateDeviceRGB();
    ctx = CGBitmapContextCreate(px.data(), w, h, 8, w * 4, cs,
                                kCGImageAlphaPremultipliedLast);
    CGContextSetShouldAntialias(ctx, false);
  }
  ~Bitmap() {
    if (ctx != nullptr) CGContextRelease(ctx);
    if (cs != nullptr) CGColorSpaceRelease(cs);
  }

  bool all_white() const {
    for (size_t i = 0; i < px.size(); i += 4) {
      if (px[i] != 255 || px[i + 1] != 255 || px[i + 2] != 255) return false;
    }
    return true;
  }
  int count_red() const {
    int n = 0;
    for (size_t i = 0; i < px.size(); i += 4) {
      if (px[i] > 200 && px[i + 1] < 60 && px[i + 2] < 60) ++n;
    }
    return n;
  }
  int count_dark() const {
    int n = 0;
    for (size_t i = 0; i < px.size(); i += 4) {
      if (px[i] < 128 && px[i + 1] < 128 && px[i + 2] < 128) ++n;
    }
    return n;
  }
};

}  // namespace

TEST(coretext_backend_metrics) {
  auto tb = ed::mac::create_coretext_backend();
  CHECK(tb != nullptr);
  CHECK(tb->line_height(kMono) > 0);
  double asc = 0, desc = 0;
  tb->vertical_metrics(kMono, &asc, &desc);
  CHECK(asc > 0);
  CHECK(desc > 0);
  CHECK(tb->line_height(kMono) >= asc + desc);  // leading may add
}

TEST(coretext_backend_shapes_one_glyph_per_codepoint) {
  auto tb = ed::mac::create_coretext_backend();
  const std::string text = "Hello, world";
  const vc::Run r = tb->shape(kMono, text);
  CHECK_EQ(r.glyphs.size(), text.size());  // ASCII: one codepoint each
  CHECK(r.width > 0);
  CHECK(std::abs(r.width - tb->measure(kMono, text)) < 0.5);
  double pen = 0;
  for (const vc::Glyph& g : r.glyphs) {
    CHECK(g.advance > 0);
    CHECK(std::abs(g.x - pen) < 1e-9);
    pen += g.advance;
  }
  CHECK(std::abs(pen - r.width) < 1e-9);
}

TEST(coretext_backend_is_monospaced_for_the_default_face) {
  auto tb = ed::mac::create_coretext_backend();
  const double a = tb->measure(kMono, "iiiii");
  const double b = tb->measure(kMono, "WWWWW");
  CHECK(std::abs(a - b) < 0.01);
  const vc::Run r = tb->shape(kMono, "iiiii");
  CHECK(r.glyphs.size() == 5u);
  for (const vc::Glyph& g : r.glyphs) {
    CHECK(std::abs(g.advance - r.glyphs[0].advance) < 1e-9);
  }
}

TEST(coretext_backend_handles_utf8_and_empty) {
  auto tb = ed::mac::create_coretext_backend();
  const vc::Run empty = tb->shape(kMono, "");
  CHECK_EQ(empty.glyphs.size(), 0u);
  CHECK_EQ(empty.width, 0.0);

  const std::string s = "a\xC3\xA9\xE2\x82\xAC";  // a, e-acute, euro
  const vc::Run r = tb->shape(kMono, s);
  CHECK_EQ(r.glyphs.size(), 3u);
  CHECK_EQ(r.glyphs[1].codepoint, 0xE9u);
  CHECK_EQ(r.glyphs[2].codepoint, 0x20ACu);
}

TEST(cg_canvas_clear_fills_every_pixel) {
  Bitmap bm(64, 32);
  auto canvas = ed::mac::create_cg_canvas(bm.ctx, 64, 32);
  canvas->clear(vc::Color{255, 255, 255, 255});
  CHECK(bm.all_white());
}

TEST(cg_canvas_fill_rect_paints_the_right_pixels) {
  Bitmap bm(64, 32);
  auto canvas = ed::mac::create_cg_canvas(bm.ctx, 64, 32);
  canvas->clear(vc::Color{255, 255, 255, 255});
  canvas->fill_rect(vc::Rect{10, 5, 10, 10}, vc::Color{255, 0, 0, 255});
  CHECK(bm.count_red() > 50);  // ~100 px of the 10x10 rect
}

TEST(cg_canvas_renders_a_dark_background) {
  Bitmap bm(32, 32);
  auto canvas = ed::mac::create_cg_canvas(bm.ctx, 32, 32);
  canvas->clear(vc::Color{27, 27, 27, 255});  // dark surface #1b1b1b
  CHECK(bm.px[0] < 60 && bm.px[1] < 60 && bm.px[2] < 60);
}

TEST(cg_canvas_draws_a_run) {
  Bitmap bm(200, 40);
  auto tb = ed::mac::create_coretext_backend();
  auto canvas = ed::mac::create_cg_canvas(bm.ctx, 200, 40);
  canvas->clear(vc::Color{255, 255, 255, 255});
  canvas->set_text_color(vc::Color{0, 0, 0, 255});
  const vc::Run r = tb->shape(kMono, "Hello");
  canvas->draw_run(r, 5, 20);  // baseline at viewcore y = 20
  CHECK(bm.count_dark() > 0);
}
