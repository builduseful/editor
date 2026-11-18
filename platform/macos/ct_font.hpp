// platform/macos/ct_font.hpp — shared CoreText font cache (§8).
//
// `text_coretext.mm` and `canvas_cg.mm` both need a CTFont for a FontSpec.
// Resolving a font is comparatively expensive, so it is cached process-wide.
#pragma once

#include <CoreText/CoreText.h>

#include "types.hpp"

namespace ed::mac {

// Returns a cached CTFont for `f`, owned by the cache. The caller must NOT
// release it. Returns nullptr only if CoreText cannot create the font.
CTFontRef ct_font(const vc::FontSpec& f);

}  // namespace ed::mac
