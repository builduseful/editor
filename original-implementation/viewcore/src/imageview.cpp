// viewcore/src/imageview.cpp — the image document view (see imageview.hpp).
#include "imageview.hpp"

#include <algorithm>
#include <cmath>

namespace vc {

namespace {
// The ground behind a transparent image: a small checkerboard, drawn in two
// neutrals from the theme so it reads in both light and dark. It is chrome
// (the image's backdrop), so it is deliberately faint.
constexpr double kMargin = 8.0;
}  // namespace

void ImageView::set_image(Image img) {
  img_ = std::move(img);
  x_offset_ = 0;
  y_offset_ = 0;
  set_fit();
}

void ImageView::resize(double w, double h) {
  width_ = w > 1 ? w : 1;
  height_ = h > 1 ? h : 1;
  if (fit_) set_fit();  // keep the whole image visible on a resize
  clamp_scroll();
}

void ImageView::set_viewport(double w, double h) {
  width_ = w > 1 ? w : 1;
  height_ = h > 1 ? h : 1;
  if (fit_) set_fit();
  clamp_scroll();
}

void ImageView::set_fit() {
  fit_ = true;
  x_offset_ = 0;
  y_offset_ = 0;
  if (!img_.valid()) {
    scale_ = 1.0;
    return;
  }
  // Fit inside the viewport with a small margin, and never enlarge beyond the
  // image's own pixels (an 80x40 icon should not blow up to fill the window).
  const double avail_w = width_ - 2 * kMargin;
  const double avail_h = height_ - 2 * kMargin;
  if (avail_w <= 0 || avail_h <= 0) {
    scale_ = 1.0;
    return;
  }
  const double s = std::min(avail_w / img_.width, avail_h / img_.height);
  scale_ = std::min(s, 1.0);  // fit, but do not upscale past actual size
  if (scale_ < 0.01) scale_ = 0.01;
}

void ImageView::set_actual() {
  fit_ = false;
  scale_ = 1.0;
  x_offset_ = 0;
  y_offset_ = 0;
}

void ImageView::set_scale(double s) {
  if (s < 0.05) s = 0.05;
  if (s > 32.0) s = 32.0;
  fit_ = false;
  // Keep the centre of the viewport over the centre of the image when zooming,
  // which is what a menu-driven zoom should do.
  const double cx = x_offset_ + width_ * 0.5;
  const double cy = y_offset_ + height_ * 0.5;
  scale_ = s;
  x_offset_ = cx - width_ * 0.5;
  y_offset_ = cy - height_ * 0.5;
  clamp_scroll();
}

void ImageView::scroll_by(double dx, double dy) {
  x_offset_ += dx;
  y_offset_ += dy;
  clamp_scroll();
}

void ImageView::scroll_to(double x, double y) {
  x_offset_ = x;
  y_offset_ = y;
  clamp_scroll();
}

void ImageView::clamp_scroll() {
  if (!img_.valid()) {
    x_offset_ = 0;
    y_offset_ = 0;
    return;
  }
  const double dw = draw_width();
  const double dh = draw_height();
  // When the image is smaller than the viewport it is centred (dest_rect), so
  // there is nothing to pan; clamp to zero in both axes.
  if (dw <= width_) {
    x_offset_ = 0;
  } else if (x_offset_ > 0) {
    x_offset_ = 0;
  } else if (x_offset_ < width_ - dw) {
    x_offset_ = width_ - dw;
  }
  if (dh <= height_) {
    y_offset_ = 0;
  } else if (y_offset_ > 0) {
    y_offset_ = 0;
  } else if (y_offset_ < height_ - dh) {
    y_offset_ = height_ - dh;
  }
}

Rect ImageView::dest_rect() const {
  if (!img_.valid()) return Rect{};
  const double dw = draw_width();
  const double dh = draw_height();
  double x = x_offset_;
  double y = y_offset_;
  // Centre when smaller than the viewport, rather than pinning to the origin.
  if (dw <= width_) x = (width_ - dw) * 0.5;
  if (dh <= height_) y = (height_ - dh) * 0.5;
  return Rect{x, y, dw, dh};
}

void ImageView::draw(Canvas& c, const Theme& t) const {
  c.clear(t.background);
  if (!img_.valid()) return;

  const Rect d = dest_rect();
  // The checkerboard, clipped to the image's rect, so a transparent PNG shows
  // its transparency instead of the page colour.
  c.push_clip(d);
  c.fill_rect(d, t.current_line);
  const double cell = kCheckSize;
  const double right = d.x + d.w;
  const double bottom = d.y + d.h;
  for (double y = d.y; y < bottom; y += cell) {
    for (double x = d.x; x < right; x += cell) {
      const bool odd =
          (static_cast<int64_t>((x - d.x) / cell) +
           static_cast<int64_t>((y - d.y) / cell)) %
              2 ==
          0;
      if (odd) {
        c.fill_rect(
            Rect{x, y, std::min(cell, right - x), std::min(cell, bottom - y)},
            t.page);
      }
    }
  }
  c.pop_clip();

  c.draw_image(img_, d);
}

}  // namespace vc