// viewcore/include/imageview.hpp — the image document view (PNG, JPEG, …).
//
// The editor's document is a text buffer; an image is a different kind of
// document, so it gets its own view rather than a special case inside
// `ViewCore`. It is platform-independent: the pixels are decoded by the platform
// layer (ImageIO on macOS) and handed over as an opaque `Image`, so this file
// knows nothing about CoreGraphics and can be tested headlessly.
//
// Interaction is deliberately small and matches the document view's feel:
//   - Fit      the whole image inside the viewport (the default on open)
//   - Actual   one image pixel per point, top-left at the origin
//   - Scroll   wheel pans (no text scroll model here)
//   - Drag     pans; there is no selection, caret or undo
//
// There is no zoom-into-a-point gesture beyond Fit/Actual: v1 keeps it to what
// a viewer needs, and a wheel-with-modifier zoom is easy to add on top.
#pragma once

#include <string>

#include "canvas.hpp"
#include "theme.hpp"
#include "types.hpp"

namespace vc {

class ImageView {
 public:
  void set_image(Image img);
  const Image& image() const { return img_; }
  bool has_image() const { return img_.valid(); }
  // The file the image came from, for the status bar and the title.
  void set_path(std::string path) { path_ = std::move(path); }
  const std::string& path() const { return path_; }

  void resize(double w, double h);
  double width() const { return width_; }
  double height() const { return height_; }

  // --- Zoom -----------------------------------------------------------------
  // Fit the whole image inside the viewport; Actual size shows one pixel per
  // point. `scale` is the current zoom (1.0 = actual size).
  void set_fit();
  void set_actual();
  bool fit() const { return fit_; }
  double scale() const { return scale_; }
  // A zoom level that shows the image at an exact scale (used by the menu).
  void set_scale(double s);

  // --- Pan ------------------------------------------------------------------
  void scroll_by(double dx, double dy);
  void scroll_to(double x, double y);
  double x_offset() const { return x_offset_; }
  double y_offset() const { return y_offset_; }
  // The image's size in points at the current scale.
  double draw_width() const { return img_.width * scale_; }
  double draw_height() const { return img_.height * scale_; }
  void clamp_scroll();

  // Where the image should be drawn, in view coordinates (y down). Centred when
  // it is smaller than the viewport, so a small image sits in the middle of the
  // page rather than jammed into the corner.
  Rect dest_rect() const;

  // The viewport rect as the user sees it (inside the chrome).
  void set_viewport(double w, double h);

  // --- Drawing --------------------------------------------------------------
  void draw(Canvas& c, const Theme& t) const;

 private:
  Image img_;
  std::string path_;
  double width_ = 800.0, height_ = 600.0;  // the view (chrome included)
  double scale_ = 1.0;
  bool fit_ = true;
  double x_offset_ = 0.0, y_offset_ = 0.0;  // pan, in points
  // A checkerboard behind a transparent image, so transparency is visible.
  static constexpr int kCheckSize = 8;
};

}  // namespace vc