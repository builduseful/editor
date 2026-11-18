// platform/macos/ct_font.mm — CoreText font resolution + cache (§8).
#include "ct_font.hpp"

#import <CoreText/CoreText.h>

#include <map>
#include <string>

namespace ed::mac {
namespace {

std::string key_of(const vc::FontSpec& f) {
  std::string k = f.family;
  k.push_back('\x1f');
  k += std::to_string(f.size);
  k += f.bold ? 'B' : '-';
  k += f.italic ? 'I' : '-';
  return k;
}

}  // namespace

CTFontRef ct_font(const vc::FontSpec& f) {
  // Leaked on purpose: the cache lives for the process and avoids static
  // destruction-order issues with CoreFoundation.
  static std::map<std::string, CTFontRef>* cache =
      new std::map<std::string, CTFontRef>();
  const std::string k = key_of(f);
  auto it = cache->find(k);
  if (it != cache->end()) return it->second;

  const double size = f.size > 0 ? f.size : 14.0;
  CTFontRef font = nullptr;
  if (f.family.empty()) {
    // The design system's mono stack (ui-monospace …) resolves to the system
    // fixed-pitch face; use it rather than naming Menlo.
    font = CTFontCreateUIFontForLanguage(kCTFontUIFontUserFixedPitch, size,
                                         nullptr);
  } else {
    CFStringRef name = CFStringCreateWithCString(nullptr, f.family.c_str(),
                                                 kCFStringEncodingUTF8);
    if (name == nullptr) return nullptr;
    font = CTFontCreateWithName(name, size, nullptr);
    CFRelease(name);
  }

  const CTFontSymbolicTraits want = static_cast<CTFontSymbolicTraits>(
      (f.bold ? kCTFontBoldTrait : 0) | (f.italic ? kCTFontItalicTrait : 0));
  if (want != 0 && font != nullptr) {
    CTFontRef styled =
        CTFontCreateCopyWithSymbolicTraits(font, 0, nullptr, want, want);
    if (styled != nullptr) {
      CFRelease(font);
      font = styled;
    }
  }
  (*cache)[k] = font;
  return font;
}

}  // namespace ed::mac
