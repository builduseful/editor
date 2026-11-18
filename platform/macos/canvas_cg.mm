// platform/macos/canvas_cg.mm — the CoreGraphics vc::Canvas (§3, §8).
#include "canvas_cg.hpp"

#import <CoreText/CoreText.h>

#include <string_view>

#include "ct_font.hpp"

namespace ed::mac {
namespace {

class CgCanvas final : public vc::Canvas {
 public:
  CgCanvas(CGContextRef ctx, double width, double height)
      : ctx_(ctx), width_(width), height_(height) {}

  void clear(vc::Color c) override {
    set_fill(c);
    CGContextFillRect(ctx_, CGRectMake(0, 0, width_, height_));
  }

  void fill_rect(const vc::Rect& r, vc::Color c) override {
    set_fill(c);
    // viewcore y-down -> CoreGraphics y-up.
    CGContextFillRect(ctx_, CGRectMake(r.x, height_ - r.y - r.h, r.w, r.h));
  }

  void set_text_color(vc::Color c) override { ink_ = c; }

  void draw_run(const vc::Run& run, double x, double y) override {
    CTFontRef font = ct_font(run.font);
    if (font == nullptr) return;
    CFStringRef s = CFStringCreateWithBytes(
        nullptr, reinterpret_cast<const UInt8*>(run.text.data()),
        static_cast<CFIndex>(run.text.size()), kCFStringEncodingUTF8, false);
    if (s == nullptr) return;
    const void* keys[] = {kCTFontAttributeName};
    const void* vals[] = {font};
    CFDictionaryRef attrs =
        CFDictionaryCreate(nullptr, keys, vals, 1, &kCFTypeDictionaryKeyCallBacks,
                           &kCFTypeDictionaryValueCallBacks);
    if (attrs == nullptr) {
      CFRelease(s);
      return;
    }
    CFAttributedStringRef a = CFAttributedStringCreate(nullptr, s, attrs);
    CTLineRef line = a != nullptr ? CTLineCreateWithAttributedString(a) : nullptr;
    if (line != nullptr) {
      set_fill(ink_);  // CoreText draws with the context's fill colour
      CGContextSetTextPosition(ctx_, x, height_ - y);  // y is the baseline
      CTLineDraw(line, ctx_);
      CFRelease(line);
    }
    if (a != nullptr) CFRelease(a);
    CFRelease(attrs);
    CFRelease(s);
  }

 private:
  void set_fill(vc::Color c) {
    CGContextSetRGBFillColor(ctx_, c.r / 255.0, c.g / 255.0, c.b / 255.0,
                             c.a / 255.0);
  }

  CGContextRef ctx_;
  double width_, height_;
  vc::Color ink_{26, 26, 26, 255};
};

}  // namespace

std::unique_ptr<vc::Canvas> create_cg_canvas(CGContextRef ctx, double width,
                                             double height) {
  return std::make_unique<CgCanvas>(ctx, width, height);
}

}  // namespace ed::mac
