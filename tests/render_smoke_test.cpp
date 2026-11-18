// tests/render_smoke_test.cpp — headless end-to-end render smoke test.
//
// Composes the exact pieces the app uses — the core Editor, viewcore, the
// CoreText backend and the CoreGraphics canvas — and checks the resulting
// pixels, so the GUI's render path is exercised without a window.
#include <CoreGraphics/CoreGraphics.h>

#include <cstdint>
#include <memory>
#include <string>
#include <vector>

#include "canvas_cg.hpp"
#include "check.hpp"
#include "editor.hpp"
#include "fake_fs.hpp"
#include "text_coretext.hpp"
#include "theme.hpp"
#include "viewcore.hpp"

namespace {

bool eq(const uint8_t* p, int r, int g, int b) {
  return p[0] == r && p[1] == g && p[2] == b;
}

struct Bmp {
  size_t w = 0, h = 0;
  std::vector<uint8_t> px;
  CGContextRef ctx = nullptr;
  CGColorSpaceRef cs = nullptr;

  Bmp(size_t width, size_t height) : w(width), h(height), px(width * height * 4, 0) {
    cs = CGColorSpaceCreateDeviceRGB();
    ctx = CGBitmapContextCreate(px.data(), w, h, 8, w * 4, cs,
                                kCGImageAlphaPremultipliedLast);
  }
  ~Bmp() {
    if (ctx != nullptr) CGContextRelease(ctx);
    if (cs != nullptr) CGColorSpaceRelease(cs);
  }
  const uint8_t* at(size_t x, size_t y_from_top) const {
    return &px[(y_from_top * w + x) * 4];
  }
  int count_eq(int r, int g, int b) const {
    int n = 0;
    for (size_t i = 0; i < px.size(); i += 4) {
      if (eq(&px[i], r, g, b)) ++n;
    }
    return n;
  }
  // Counts exact-colour pixels in rows [y0, y1).
  int count_eq_rows(int r, int g, int b, size_t y0, size_t y1) const {
    int n = 0;
    for (size_t y = y0; y < y1 && y < h; ++y) {
      for (size_t x = 0; x < w; ++x) {
        if (eq(at(x, y), r, g, b)) ++n;
      }
    }
    return n;
  }
  int count_dark() const {
    int n = 0;
    for (size_t i = 0; i < px.size(); i += 4) {
      if (px[i] < 80 && px[i + 1] < 80 && px[i + 2] < 80) ++n;
    }
    return n;
  }
  int count_light() const {
    int n = 0;
    for (size_t i = 0; i < px.size(); i += 4) {
      if (px[i] > 200 && px[i + 1] > 200 && px[i + 2] > 200) ++n;
    }
    return n;
  }
};

struct Doc {
  ed_test::FakeFileSystem fs;
  std::unique_ptr<Editor> ed;
  std::unique_ptr<vc::TextBackend> tb;
  std::unique_ptr<vc::ViewCore> view;

  explicit Doc(const std::string& content) {
    int fd = -1;
    fs.open_write("/f.txt", true, false, false, &fd);
    if (!content.empty()) {
      size_t w = 0;
      fs.pwrite(fd, 0, content.data(), content.size(), &w);
    }
    fs.close_fd(fd);
    create_editor(fs, &ed);
    ed->open("/f.txt");
    tb = ed::mac::create_coretext_backend();
    view = std::make_unique<vc::ViewCore>(*ed, *tb);
  }
};

}  // namespace

TEST(render_smoke_light_theme) {
  Doc d("hello world\nsecond line\nthird line here\n");
  d.view->resize(400, 200);
  d.view->set_gutter(true);
  d.view->set_theme(vc::light_theme());
  Bmp bmp(400, 200);
  auto canvas = ed::mac::create_cg_canvas(bmp.ctx, 400, 200);
  d.view->draw(*canvas);

  CHECK(eq(bmp.at(390, 190), 255, 255, 255));  // surface #ffffff
  CHECK(eq(bmp.at(2, 190), 246, 247, 249));    // gutter ground = color.page
  CHECK(bmp.count_dark() > 50);                // ink pixels
}

TEST(render_smoke_dark_theme) {
  Doc d("hello world\nsecond line\nthird line here\n");
  d.view->resize(400, 200);
  d.view->set_gutter(true);
  d.view->set_theme(vc::dark_theme());
  Bmp bmp(400, 200);
  auto canvas = ed::mac::create_cg_canvas(bmp.ctx, 400, 200);
  d.view->draw(*canvas);

  CHECK(eq(bmp.at(390, 190), 27, 27, 27));  // surface #1b1b1b
  CHECK(eq(bmp.at(2, 190), 17, 17, 17));    // gutter ground = color.page #111111
  CHECK(bmp.count_light() > 50);            // light ink pixels
}

TEST(render_smoke_selection_pixels) {
  Doc d("hello world\nsecond line\n");
  d.view->resize(400, 200);
  d.view->set_theme(vc::light_theme());
  vc::Selection sel;
  sel.anchor_line = 0;
  sel.anchor_col = 0;
  sel.caret_line = 0;
  sel.caret_col = 11;
  d.view->set_selection(sel);
  d.view->set_caret_visible(false);
  Bmp bmp(400, 200);
  auto canvas = ed::mac::create_cg_canvas(bmp.ctx, 400, 200);
  d.view->draw(*canvas);
  CHECK(bmp.count_eq(223, 228, 234) > 40);  // color.secondary-hover #dfe4ea
}

TEST(render_smoke_scrollbar_dark) {
  std::string content;
  for (int i = 0; i < 200; ++i) content += "line\n";
  Doc d(content);
  d.view->resize(400, 200);
  d.view->set_theme(vc::dark_theme());
  d.view->discover_all();
  Bmp bmp(400, 200);
  {
    auto canvas = ed::mac::create_cg_canvas(bmp.ctx, 400, 200);
    d.view->draw(*canvas);
  }
  // Thumb = color.border-control #6d6d6d in dark, at the top.
  CHECK(bmp.count_eq_rows(109, 109, 109, 0, 100) > 20);
  // Only the thumb is drawn (no track band over the text): the strip below the
  // thumb stays the surface colour.
  CHECK(eq(bmp.at(396, 190), 27, 27, 27));
}

TEST(render_smoke_scrollbar_pixels) {
  std::string content;
  for (int i = 0; i < 200; ++i) content += "line\n";
  Doc d(content);
  d.view->resize(400, 200);
  d.view->set_theme(vc::light_theme());
  d.view->discover_all();  // the scrollbar needs the total line count
  Bmp bmp(400, 200);
  {
    auto canvas = ed::mac::create_cg_canvas(bmp.ctx, 400, 200);
    d.view->draw(*canvas);
  }
  // Thumb = color.border-control #7e8794, at the top of the track.
  const int r = 126, g = 135, b = 148;
  CHECK(bmp.count_eq(r, g, b) > 20);
  CHECK(bmp.count_eq_rows(r, g, b, 0, 100) > 20);
  CHECK_EQ(bmp.count_eq_rows(r, g, b, 100, 200), 0);

  // At the end of the document the thumb is at the bottom of the track.
  d.view->scroll_to_line(200);
  Bmp bottom(400, 200);
  {
    auto canvas = ed::mac::create_cg_canvas(bottom.ctx, 400, 200);
    d.view->draw(*canvas);
  }
  CHECK_EQ(bottom.count_eq_rows(r, g, b, 0, 100), 0);
  CHECK(bottom.count_eq_rows(r, g, b, 100, 200) > 20);
}
