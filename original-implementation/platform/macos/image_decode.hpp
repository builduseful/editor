// platform/macos/image_decode.hpp — decode an image file into a vc::Image.
//
// ImageIO is a system framework (already linked), so this adds no dependency and
// nothing is vendored. Anything ImageIO can read works here: PNG, JPEG, HEIF,
// GIF's first frame, TIFF, and so on.
#pragma once

#include <memory>

#include "types.hpp"

namespace ed::mac {

// Decodes `path` into a vc::Image whose `handle` is a retained CGImageRef, or an
// invalid Image if the file cannot be read or decoded. Never throws.
vc::Image decode_image(const char* path);

// Releases an Image's handle (the platform-side counterpart of decode_image).
void release_image(vc::Image* img);

}  // namespace ed::mac