// platform/macos/text_coretext.mm — the CoreText vc::TextBackend (§3, §8).
//
// The only place glyphs are shaped. CoreText returns glyphs in a run; we map
// each glyph back to its source codepoint (via CTRunGetStringIndices) so the
// platform-neutral Run exposes one Glyph per codepoint, in text order.
#include "text_coretext.hpp"

#import <CoreText/CoreText.h>

#include <cstdint>
#include <string>
#include <string_view>
#include <vector>

#include "ct_font.hpp"

namespace ed::mac {
namespace {

struct Utf16 {
  std::vector<UniChar> units;
  // u16_to_u8[i] = UTF-8 byte offset of the codepoint that produced unit i;
  // the last entry is text.size() (the end sentinel).
  std::vector<size_t> u16_to_u8;
};

// UTF-8 -> UTF-16 with a reverse index into the UTF-8 bytes. Malformed bytes are
// treated as Latin-1 so shaping never fails on damaged input.
Utf16 to_utf16(std::string_view s) {
  Utf16 out;
  out.units.reserve(s.size());
  out.u16_to_u8.reserve(s.size() + 1);
  size_t i = 0;
  while (i < s.size()) {
    const unsigned char c = static_cast<unsigned char>(s[i]);
    uint32_t cp = c;
    size_t n = 1;
    if ((c & 0xE0) == 0xC0) {
      cp = c & 0x1Fu;
      n = 2;
    } else if ((c & 0xF0) == 0xE0) {
      cp = c & 0x0Fu;
      n = 3;
    } else if ((c & 0xF8) == 0xF0) {
      cp = c & 0x07u;
      n = 4;
    }
    if (i + n > s.size()) {
      n = 1;
      cp = c;
    } else {
      for (size_t k = 1; k < n; ++k) {
        cp = (cp << 6) | (static_cast<unsigned char>(s[i + k]) & 0x3Fu);
      }
    }
    if (cp <= 0xFFFF) {
      out.u16_to_u8.push_back(i);
      out.units.push_back(static_cast<UniChar>(cp));
    } else {
      cp -= 0x10000;
      out.u16_to_u8.push_back(i);
      out.units.push_back(static_cast<UniChar>(0xD800 + (cp >> 10)));
      out.u16_to_u8.push_back(i);
      out.units.push_back(static_cast<UniChar>(0xDC00 + (cp & 0x3FF)));
    }
    i += n;
  }
  out.u16_to_u8.push_back(s.size());
  return out;
}

uint32_t codepoint_at(std::string_view s, size_t off) {
  if (off >= s.size()) return 0;
  const unsigned char c = static_cast<unsigned char>(s[off]);
  uint32_t cp = c;
  size_t n = 1;
  if ((c & 0xE0) == 0xC0) {
    cp = c & 0x1Fu;
    n = 2;
  } else if ((c & 0xF0) == 0xE0) {
    cp = c & 0x0Fu;
    n = 3;
  } else if ((c & 0xF8) == 0xF0) {
    cp = c & 0x07u;
    n = 4;
  }
  if (off + n > s.size()) return c;
  for (size_t k = 1; k < n; ++k) {
    cp = (cp << 6) | (static_cast<unsigned char>(s[off + k]) & 0x3Fu);
  }
  return cp;
}

class CoreTextBackend final : public vc::TextBackend {
 public:
  double line_height(const vc::FontSpec& f) override {
    CTFontRef font = ct_font(f);
    if (font == nullptr) return 0;
    return CTFontGetAscent(font) + CTFontGetDescent(font) +
           CTFontGetLeading(font);
  }

  double measure(const vc::FontSpec& f, std::string_view text) override {
    CTLineRef line = make_line(f, text);
    if (line == nullptr) return 0;
    const double w = CTLineGetTypographicBounds(line, nullptr, nullptr, nullptr);
    CFRelease(line);
    return w;
  }

  vc::Run shape(const vc::FontSpec& f, std::string_view text) override {
    vc::Run r;
    r.font = f;
    r.text.assign(text);
    CTLineRef line = make_line(f, text);
    if (line == nullptr) return r;

    CGFloat ascent = 0, descent = 0, leading = 0;
    CTLineGetTypographicBounds(line, &ascent, &descent, &leading);
    r.ascent = ascent;
    r.descent = descent;

    const Utf16 u16 = to_utf16(text);
    CFArrayRef runs = CTLineGetGlyphRuns(line);
    const CFIndex nruns = runs != nullptr ? CFArrayGetCount(runs) : 0;
    double pen = 0;
    for (CFIndex ri = 0; ri < nruns; ++ri) {
      auto run = static_cast<CTRunRef>(
          const_cast<void*>(CFArrayGetValueAtIndex(runs, ri)));
      const CFIndex n = CTRunGetGlyphCount(run);
      if (n <= 0) continue;
      std::vector<CGSize> adv(static_cast<size_t>(n));
      CTRunGetAdvances(run, CFRangeMake(0, n), adv.data());
      std::vector<CFIndex> idx(static_cast<size_t>(n));
      CTRunGetStringIndices(run, CFRangeMake(0, n), idx.data());
      for (CFIndex gi = 0; gi < n; ++gi) {
        const size_t u16i = static_cast<size_t>(idx[static_cast<size_t>(gi)]);
        const size_t byte =
            u16i < u16.u16_to_u8.size() ? u16.u16_to_u8[u16i] : text.size();
        vc::Glyph g;
        g.codepoint = codepoint_at(text, byte);
        g.x = pen;
        g.advance = adv[static_cast<size_t>(gi)].width;
        pen += g.advance;
        r.glyphs.push_back(g);
      }
    }
    r.width = pen;  // sum of advances == typographic width for LTR text
    CFRelease(line);
    return r;
  }

  void vertical_metrics(const vc::FontSpec& f, double* ascent,
                        double* descent) override {
    CTFontRef font = ct_font(f);
    *ascent = font != nullptr ? CTFontGetAscent(font) : 0;
    *descent = font != nullptr ? CTFontGetDescent(font) : 0;
  }

 private:
  static CTLineRef make_line(const vc::FontSpec& f, std::string_view text) {
    CTFontRef font = ct_font(f);
    if (font == nullptr) return nullptr;
    CFStringRef s = CFStringCreateWithBytes(
        nullptr, reinterpret_cast<const UInt8*>(text.data()),
        static_cast<CFIndex>(text.size()), kCFStringEncodingUTF8, false);
    if (s == nullptr) return nullptr;
    const void* keys[] = {kCTFontAttributeName};
    const void* vals[] = {font};
    CFDictionaryRef attrs =
        CFDictionaryCreate(nullptr, keys, vals, 1, &kCFTypeDictionaryKeyCallBacks,
                           &kCFTypeDictionaryValueCallBacks);
    if (attrs == nullptr) {
      CFRelease(s);
      return nullptr;
    }
    CFAttributedStringRef a = CFAttributedStringCreate(nullptr, s, attrs);
    CTLineRef line = a != nullptr ? CTLineCreateWithAttributedString(a) : nullptr;
    if (a != nullptr) CFRelease(a);
    CFRelease(attrs);
    CFRelease(s);
    return line;
  }
};

}  // namespace

std::unique_ptr<vc::TextBackend> create_coretext_backend() {
  return std::make_unique<CoreTextBackend>();
}

}  // namespace ed::mac
