# AGENTS.md

## Project

`editor` — a native macOS text editor with a C++20 core, a custom-drawn view, and
a CLI-first surface. `PLAN.md` is the source of truth for scope, decisions, and
architecture. Read it before changing scope.

## Design and brand

This app follows the **Build Useful design system** for brand consistency: one ink,
a short ladder of greys, the operating system's UI typeface, a 2px spacing rhythm,
quiet controls, and colour only for state (danger / success / info). It is a brand
reference, not a component library.

- **Do not vendor the design system's web CSS/JS into this native app.** Implement
  the look natively with AppKit/CoreText.
- Use its tokens as the reference: ink and grey palette, light/dark themes, spacing
  scale, type ramp, subtle borders, minimal chrome. No accent hue.
- The resolved colour tokens live in one place: `viewcore/include/theme.hpp` /
  `viewcore/src/theme.cpp` (`light_theme()` / `dark_theme()`). Keep them in sync with
  the design system's colour tokens; never hard-code a colour elsewhere. The app
  offers System / Light / Dark and applies the matching theme.
- Use the system UI font for chrome; the system fixed-pitch (mono) face for the
  document and technical values.
- Ask the user for the current design-system location/access before UI work. Never
  commit local paths, personal data, or proprietary design-system files.
- `LICENSE` follows the design system's app-licence template (the system is used
  under its own licence, not vendored). The About panel carries the same notice.

### Rules that apply to this app

The design system's guide is written for its web components; these are the parts
that carry over to a native, custom-drawn editor. When in doubt, read the guide's
own tokens rather than inventing a value.

- **Tokens for every value.** Colours come from `theme.hpp`; spacing follows the
  system's rhythm (2px ladder); borders are hairlines. No hex, `rgb()` or named
  colour anywhere else in UI code.
- **No accent hue.** Colour marks state only (danger / success / info). Selection,
  current line, gutter and scrollbar are neutral greys — not blue.
- **Both themes, always.** Anything you draw must look right in light and dark;
  check both (`--theme light|dark` renders a snapshot, or switch in the app menu).
  Draw text with the theme ink, never the default black.
- **Quiet, minimal chrome.** No gradients, spinners, illustrations or decorative
  motion. Motion, when added, only explains a change and is short.
- **Copy style:** actions in sentence case starting with a verb; headings in Title
  Case; 24-hour times with a spaced en dash ("09:00 – 17:00"); no exclamation marks.
- **One primary action per view**; name every icon-only control; keep keyboard
  paths working and focus visible. (IME and accessibility are explicitly out of v1,
  §8 — note the exception where it bites.)
- **Never add a pattern only because the guide has one.** Use a component where it
  fits the screen; otherwise compose from the same tokens.
- **Document-view controls are custom-drawn** (D2), including the scrollbar — no
  native-control hybrid.

## Engineering

- C++20, macOS-only v1, Apple Silicon.
- No vendored/bundled third-party libraries; OS system libraries and the C++ standard
  library are allowed.
- Custom-drawn view only; no native-control hybrid.
- The window always has a document before it draws. An untitled window is backed
  by a private temp file (unlinked on open / Save As / quit); with nothing open
  the core returns `E_IO` rather than touching unset sources. A Finder “Open
  With” launch has no argv file — the document arrives as an `odoc` Apple event
  after the first frame — so no path may assume the file was passed in.
- Decisions D1–D13 in `PLAN.md` are locked unless the plan says otherwise.
- Build/test commands: `make` (builds `build/editor_tests`, plus `libcore.a`/`editor`
  once those sources exist) and `make test` (runs the suite, including the
  deterministic C++ fuzzers); `make golden` runs the CLI golden-JSON tests against
  `build/editor`; `make fuzz` runs the CLI argv/batch fuzzer
  (`tests/cli_fuzz.py`); `make bench` builds and runs the §5.5/§7 save-gate
  benchmark (`bench/bench_edit.cpp`); `make demo` builds and runs the M0.5
  read-only view demo / footprint gate — first paint (open→draw) and scroll fps
  (`demo/view_demo.cpp`, `libviewcore.a`); `make app` builds `build/editor.app`
  (and `build/editor_gui`), the AppKit view (CoreText + CoreGraphics +
  `viewcore`); `make gui-smoke` runs the GUI smoke test (`tests/gui_smoke.py`):
  headless AppKit snapshots (`--snapshot PATH --theme light|dark --size WxH
  --scroll LINE`), a real launch, a no-file launch, and a Finder-style
  LaunchServices open (`tests/render_smoke_test.cpp` covers the same render path
  with pixel assertions); `make render` renders a document to a
  PNG for inspection (`tools/render_doc.cpp`);
  `make SANITIZE=1 test` adds ASan+UBSan; `make clean`. Apple clang + make only,
  no third-party tooling. (libFuzzer is not in the Command Line Tools clang, and
  we do not install it, so "fuzzing" here means seeded randomized property tests,
  not a libFuzzer target.) This plain Makefile is a Pre-M0.0 stand-in for the
  `PLAN.md` §10 CMake graph, which wraps the same sources when the GUI/`.app`/
  notarization work begins; the M0.5 `viewcore` library and the M1
  `platform/macos` ObjC++ backends are already built this way.

## Repo hygiene

- No personal paths, local machine details, or secrets in tracked files.
- Keep tracked files minimal; `PLAN.md` and this file are the current documents.