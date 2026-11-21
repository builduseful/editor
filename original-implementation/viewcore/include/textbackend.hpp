// viewcore/include/textbackend.hpp — the only place that shapes glyphs (§3).
//
// Pure virtual: `viewcore` calls it, `platform/*` implements it (v1: CoreText).
// The headless backend implements it with fixed advances for the render oracle.
#pragma once

#include <string_view>

#include "types.hpp"

namespace vc {

class TextBackend {
 public:
  virtual ~TextBackend() = default;

  // Distance between baselines for `f`.
  virtual double line_height(const FontSpec& f) = 0;
  // Advance width of `text` in `f` (no shaping side effects).
  virtual double measure(const FontSpec& f, std::string_view text) = 0;
  // Shape `text` into a run. The run's glyphs are in text order.
  virtual Run shape(const FontSpec& f, std::string_view text) = 0;
  // Ascent above / descent below the baseline for `f`.
  virtual void vertical_metrics(const FontSpec& f, double* ascent,
                                double* descent) = 0;
};

}  // namespace vc
