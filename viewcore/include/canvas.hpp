// viewcore/include/canvas.hpp — platform-neutral draw surface (§3).
//
// `platform/macos` implements this over CoreGraphics (M1); the headless backend
// implements it by recording draw commands (the render oracle, M0.5).
#pragma once

#include "types.hpp"

namespace vc {

class Canvas {
 public:
  virtual ~Canvas() = default;
  virtual void clear(Color c) = 0;
  virtual void fill_rect(const Rect& r, Color c) = 0;
  virtual void draw_run(const Run& run, double x, double y) = 0;
};

}  // namespace vc
