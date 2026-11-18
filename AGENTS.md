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
- Use the system UI font for chrome; monospace only for the document and technical
  values.
- Ask the user for the current design-system location/access before UI work. Never
  commit local paths, personal data, or proprietary design-system files.

## Engineering

- C++20, macOS-only v1, Apple Silicon.
- No vendored/bundled third-party libraries; OS system libraries and the C++ standard
  library are allowed.
- Custom-drawn view only; no native-control hybrid.
- Decisions D1–D13 in `PLAN.md` are locked unless the plan says otherwise.
- Build/test commands: `make` (builds `build/editor_tests`, plus `libcore.a`/`editor`
  once those sources exist) and `make test` (runs the suite, including the
  deterministic C++ fuzzers); `make golden` runs the CLI golden-JSON tests against
  `build/editor`; `make fuzz` runs the CLI argv/batch fuzzer
  (`tests/cli_fuzz.py`); `make bench` builds and runs the §5.5/§7 save-gate
  benchmark (`bench/bench_edit.cpp`); `make demo` builds and runs the M0.5
  read-only view demo / footprint gate — first paint (open→draw) and scroll fps
  (`demo/view_demo.cpp`, `libviewcore.a`); `make app` builds `build/editor_gui`,
  the M1 AppKit read-only view (CoreText + CoreGraphics + `viewcore`);
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