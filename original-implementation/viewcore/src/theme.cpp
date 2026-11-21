// viewcore/src/theme.cpp — resolved Build Useful colour tokens (light/dark).
#include "theme.hpp"

namespace vc {

namespace {

constexpr Color rgb(unsigned r, unsigned g, unsigned b) {
  return Color{static_cast<uint8_t>(r), static_cast<uint8_t>(g),
               static_cast<uint8_t>(b), 255};
}

}  // namespace

// Light theme (from the design system's colour tokens).
Theme light_theme() {
  Theme t;
  t.background = rgb(0xff, 0xff, 0xff);     // color.surface      #ffffff
  t.text = rgb(0x17, 0x1a, 0x1f);           // color.text         #171a1f
  t.selection = rgb(0xdf, 0xe4, 0xea);      // color.secondary-hover #dfe4ea
  t.current_line = rgb(0xf4, 0xf6, 0xf8);   // color.surface-subtle #f4f6f8
  t.page = rgb(0xf6, 0xf7, 0xf9);      // color.page         #f6f7f9
  t.gutter_text = rgb(0x62, 0x6a, 0x76);    // color.text-muted   #626a76
  t.gutter_border = rgb(0xe6, 0xe9, 0xee);  // color.border-subtle #e6e9ee
  t.border_control = rgb(0x7e, 0x87, 0x94); // color.border-control #7e8794
  return t;
}
// Dark theme.
Theme dark_theme() {
  Theme t;
  t.background = rgb(0x1b, 0x1b, 0x1b);     // color.surface      #1b1b1b
  t.text = rgb(0xf1, 0xf1, 0xf1);           // color.text         #f1f1f1
  t.selection = rgb(0x30, 0x30, 0x30);      // color.secondary-hover #303030
  t.current_line = rgb(0x20, 0x20, 0x20);   // color.surface-subtle #202020
  t.page = rgb(0x11, 0x11, 0x11);      // color.page         #111111
  t.gutter_text = rgb(0xb8, 0xb8, 0xb8);    // color.text-muted   #b8b8b8
  t.gutter_border = rgb(0x2a, 0x2a, 0x2a);  // color.border-subtle #2a2a2a
  t.border_control = rgb(0x6d, 0x6d, 0x6d); // color.border-control #6d6d6d
  return t;
}

}  // namespace vc
