// tools/render_doc.cpp — render a document view to a PNG (developer preview).
//
// Draws exactly what the app's EditorView draws — viewcore over CoreText and
// CoreGraphics — into an offscreen bitmap, so the document view can be inspected
// without a window. This is a dev tool, not part of the app.
#include <CoreGraphics/CoreGraphics.h>
#include <ImageIO/ImageIO.h>

#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <filesystem>
#include <memory>
#include <string>
#include <vector>

#include "canvas_cg.hpp"
#include "editor.hpp"
#include "text_coretext.hpp"
#include "theme.hpp"
#include "viewcore.hpp"

int main(int argc, char** argv) {
  std::string file, out;
  double width = 900, height = 640, font_size = 14.0;
  int tab = 4;
  bool dark = false, wrap = false, gutter = true, caret = true;
  long sa = 0, sc = 0, ca = 0, cc = 0;
  bool has_sel = false;

  for (int i = 1; i < argc; ++i) {
    const std::string a = argv[i];
    auto next = [&]() -> std::string {
      return (i + 1 < argc) ? std::string(argv[++i]) : std::string();
    };
    if (a == "--file") file = next();
    else if (a == "--out") out = next();
    else if (a == "--width") width = std::atof(next().c_str());
    else if (a == "--height") height = std::atof(next().c_str());
    else if (a == "--font") font_size = std::atof(next().c_str());
    else if (a == "--tab") tab = std::atoi(next().c_str());
    else if (a == "--dark") dark = true;
    else if (a == "--wrap") wrap = true;
    else if (a == "--no-gutter") gutter = false;
    else if (a == "--no-caret") caret = false;
    else if (a == "--select") {
      const std::string s = next();
      if (std::sscanf(s.c_str(), "%ld:%ld-%ld:%ld", &sa, &sc, &ca, &cc) == 4) {
        has_sel = true;
      }
    }
  }
  if (file.empty()) {
    std::fprintf(stderr,
                 "usage: render_doc --file F [--out P] [--dark] [--wrap]\n"
                 "       [--width W] [--height H] [--font N] [--tab N]\n"
                 "       [--no-gutter] [--no-caret] [--select L:C-L:C]\n");
    return 2;
  }
  if (out.empty()) out = "build/renders/out.png";

  std::unique_ptr<FileSystem> fs;
  create_file_system(&fs);
  std::unique_ptr<Editor> ed;
  create_editor(*fs, &ed);
  const Status st = ed->open(file);
  if (st != Status::Ok) {
    std::fprintf(stderr, "open failed: status %d\n", static_cast<int>(st));
    return 1;
  }

  auto tb = ed::mac::create_coretext_backend();
  vc::ViewCore view(*ed, *tb);
  vc::FontSpec font;
  font.size = font_size;
  view.set_font(font);
  view.set_theme(dark ? vc::dark_theme() : vc::light_theme());
  view.set_gutter(gutter);
  view.set_tab_width(tab);
  view.set_wrap(wrap);
  view.resize(width, height);
  if (has_sel) {
    vc::Selection sel;
    sel.anchor_line = sa - 1;
    sel.anchor_col = sc - 1;
    sel.caret_line = ca - 1;
    sel.caret_col = cc - 1;
    view.set_selection(sel);
    view.set_caret_visible(false);
  } else {
    view.set_caret(0, 0);
    view.set_caret_visible(caret);
  }

  const size_t w = static_cast<size_t>(width);
  const size_t h = static_cast<size_t>(height);
  std::vector<uint8_t> px(w * h * 4, 0);
  CGColorSpaceRef cs = CGColorSpaceCreateDeviceRGB();
  CGContextRef ctx = CGBitmapContextCreate(px.data(), w, h, 8, w * 4, cs,
                                           kCGImageAlphaPremultipliedLast);
  if (ctx == nullptr) {
    std::fprintf(stderr, "bitmap context failed\n");
    return 1;
  }
  auto canvas = ed::mac::create_cg_canvas(ctx, width, height);
  view.draw(*canvas);

  std::filesystem::path p(out);
  if (p.has_parent_path()) {
    std::error_code ec;
    std::filesystem::create_directories(p.parent_path(), ec);
  }

  CGImageRef img = CGBitmapContextCreateImage(ctx);
  CFURLRef url = CFURLCreateFromFileSystemRepresentation(
      nullptr, reinterpret_cast<const UInt8*>(out.c_str()),
      static_cast<CFIndex>(out.size()), false);
  CGImageDestinationRef dest =
      CGImageDestinationCreateWithURL(url, CFSTR("public.png"), 1, nullptr);
  CGImageDestinationAddImage(dest, img, nullptr);
  const bool ok = CGImageDestinationFinalize(dest);
  CFRelease(dest);
  CFRelease(url);
  CFRelease(img);
  CGContextRelease(ctx);
  CGColorSpaceRelease(cs);
  if (!ok) {
    std::fprintf(stderr, "PNG write failed: %s\n", out.c_str());
    return 1;
  }
  std::printf("wrote %s  (%zux%zu, %s, %s, font %.0f)\n", out.c_str(), w, h,
              dark ? "dark" : "light", wrap ? "wrap" : "no-wrap", font_size);
  return 0;
}
