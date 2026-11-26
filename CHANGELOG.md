# Changelog

All notable user-facing changes to `editor` are documented here.
The workspace version in `Cargo.toml` (`[workspace.package] version`) is the
single source of truth for `editor`, `editor-gui`, `Editor.app`, and GitHub Releases.

## [0.3.0] - 2026-11-26

### Added
- Zero-idle-CPU Git awareness: branch indicator in the status bar, sidebar status cues, monorepo/nested-repo & worktree support, and live Myers gutter diff ticks.
- Multi-cursor editing operations (`⌘D`, `⌥Click`, atomic duplicate/delete/comment/outdent/paste/undo).
- Workspace Search & Replace in Files with path glob/exclusion filters and confirmation modal.
- Split-pane layout persistence (grid arrangement, split ratios, zoom state, and per-pane cursors) across restarts.

### Fixed
- Vertical scrollbar thumb sizing, wheel scrolling, and caret auto-scroll in word-wrap and lazy-indexed large-file modes.
- macOS `IOSurface` retention leak on window close (`⌘W`), app hide (`⌘H`), and window resize.

### Changed
- Reduced steady focused memory footprint and idle CPU by settling the caret solidly visible after 10 s of inactivity (pausing the periodic blink timer when no toast or notice is active).
- Moved the 24-hour GitHub Releases update check onto one shared, hardened out-of-process `curl` helper in `editor-platform`, removing in-process `NSURLSession` glue so TLS/HTTP frameworks never load into `editor-gui`.
- Eliminated per-frame heap allocations in `GlyphAtlas` lookups (`ClusterKey` + ASCII UI fast loop), compacted `CommandPalette` file index entries (`Box<str>`), evicted base `PageCache` buffers on parked tabs, made `Viewport::wrap_cache` lazy, and enabled fat LTO (`editor` 735 KB, `editor-gui` 1.5 MB).

## [0.2.0] - 2026-11-26

### Added
- Pure-Rust (`std`-only, zero external crates) native text editor (`editor-gui`) and deterministic CLI (`editor`).
- Monoid B-tree piece table with lazy multi-GB file opening, 32-byte SWAR search, and linear-time Thompson NFA / PikeVM regex engine.
- 17-language incremental syntax highlighting, custom-drawn Build Useful light/dark UI, lazy file sidebar with FSEvents watcher, Quick Open, Command Palette, Working-Set pins/tabs, split panes, and Vim mode.
- Single-source SemVer versioning, self-contained `Editor.app` packager, macOS installer, and GitHub Releases auto-updater (`editor update`).
