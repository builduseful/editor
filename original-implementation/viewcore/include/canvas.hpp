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
  // Restricts subsequent drawing to `r` (view coordinates) until the matching
  // pop_clip(). Calls nest and must be balanced. Backends without a clip (the
  // headless recorder) treat them as no-ops.
  virtual void push_clip(const Rect& r) { (void)r; }
  virtual void pop_clip() {}
  // Sets the colour used by subsequent draw_run calls (a run carries no colour).
  virtual void set_text_color(Color c) = 0;
  virtual void draw_run(const Run& run, double x, double y) = 0;
  // Draws an image into the destination rect (view coordinates). `source` is an
  // opaque platform handle — CGImageRef on macOS — wrapped in Image so viewcore
  // never sees a platform type. The backends without image support (the headless
  // recorder) record the command but draw nothing.
  virtual void draw_image(const Image& img, const Rect& dst) { (void)img; (void)dst; }
};

}  // namespace vc
