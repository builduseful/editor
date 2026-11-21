// platform/macos/text_coretext.hpp — the CoreText vc::TextBackend (§3, §8).
#pragma once

#include <memory>

#include "textbackend.hpp"

namespace ed::mac {

// A vc::TextBackend implemented with CoreText. Shaping, measuring, and vertical
// metrics all come from CoreText; one Glyph is emitted per codepoint, in text
// order (the v1 document is LTR).
std::unique_ptr<vc::TextBackend> create_coretext_backend();

}  // namespace ed::mac
