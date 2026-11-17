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
- Decisions D1–D12 in `PLAN.md` are locked unless the plan says otherwise.
- Build/test commands: none yet (Pre-M0.0).

## Repo hygiene

- No personal paths, local machine details, or secrets in tracked files.
- Keep tracked files minimal; `PLAN.md` and this file are the current documents.