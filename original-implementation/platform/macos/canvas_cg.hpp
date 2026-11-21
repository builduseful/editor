// platform/macos/canvas_cg.hpp — the CoreGraphics vc::Canvas (§3, §8).
#pragma once

#include <CoreGraphics/CoreGraphics.h>

#include <memory>

#include "canvas.hpp"

namespace ed::mac {

// A vc::Canvas drawing into `ctx`. The context keeps CoreGraphics' native
// bottom-left origin; viewcore's y-down coordinates are converted using
// `height` (the view height), so text stays upright and matches the headless
// render oracle's geometry.
std::unique_ptr<vc::Canvas> create_cg_canvas(CGContextRef ctx, double width,
                                             double height);

}  // namespace ed::mac
