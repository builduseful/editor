// viewcore/include/theme.hpp — the Build Useful design system, as native tokens.
//
// The design system is a brand reference (PLAN.md D13): its web CSS/JS is not
// vendored into this app. Instead these are the semantic colour tokens, resolved
// for the light and dark themes, implemented natively. Keep them in sync with
// the design system's colour tokens; do not invent values here.
//
// Token mapping (design-system name -> field):
//   color.surface        -> background   (the document ground)
//   color.text           -> text         (ink)
//   color.surface-muted  -> selection    ("quiet fills: … selects")
//   color.surface-subtle -> current_line (faintest fill: rows inside a card)
//   color.page           -> page         (the ground behind the margin)
//   color.text-muted     -> gutter_text  (supporting text; meets 4.5:1)
//   color.border-subtle  -> gutter_border (a hairline that separates)
#pragma once

#include "types.hpp"

namespace vc {

struct Theme {
  Color background;
  Color text;
  Color selection;
  Color current_line;
  Color page;  // color.page: the ground behind the margin / chrome
  Color gutter_text;
  Color gutter_border;
};

Theme light_theme();
Theme dark_theme();

}  // namespace vc
