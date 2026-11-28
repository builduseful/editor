# Changelog

All notable user-facing changes to `editor` are documented here.
The workspace version in `Cargo.toml` (`[workspace.package] version`) is the
single source of truth for `editor`, `editor-gui`, `Editor.app`, and GitHub Releases.

## [0.4.3] - 2026-11-28

### Fixed
- **Split-pane focus & Quick Open split:** clicking a split pane's header now clears sidebar, Find/Replace, and Go-To-Line focus so keyboard and Vim shortcuts (`<Space>1`–`9`, `g1`–`g9`) route immediately to the clicked pane; Quick Open and Search in Files split actions (`⌃V` / `⌃X`) now open already-open files directly in the new split pane instead of teleporting back to the existing pane.

## [0.4.2] - 2026-11-28

### Fixed
- **macOS FFI & adapter hardening:** added null/nil guards for UTF-8 strings, preferences, window titles, discard alerts, and FSEvents watchers in `mac_adapter.m`, clamped `IOSurface` and framebuffer dimensions (`<= 16384` / `--size <= 8192`), forwarded `rightMouseUp:`, added `@autoreleasepool` and bounds checks in `fsevents_callback`, and cleared callbacks/timers/FSEvents on teardown.
- **Core engine, search, diff & syntax:** indexed lazy piece-tree regions before sampling `lines_before` in `Document::edit` so `SyntaxCache` stays aligned, preserved adjacent regex match context and CRLF `$` capture expansion in `Document::replace_all`, matched `PieceTree` line breaks (including lone `\r`) in `diff::hash_lines_bytes`, added a 4096-byte overlap window to `find_regex_in_tree` for >256 KB single-line and multiline matches, and preserved multiline lexer state across multi-line HTML `<script>`/`<style>` tags, Markdown fenced C++ raw strings, and JS block comments inside `${ ... }`.
- **Split panes, multi-cursor, Vim & keymap:** prevented same-buffer split-pane data loss across Hot Exit backups, undo sync, and pane closing, wired full post-save housekeeping for `IpcCommand::Save`, cancelled in-flight drags on focus loss, propagated zoom changes to all split panes, synced word-wrap on file rename, included 0-byte dirty files in Project Search overrides, fixed multi-cursor `Shift+Tab` (preserving per-cursor columns and leaving clean unindented files untouched), fixed rightward tab-drag off-by-one, snapped UTF-8 carets on parked `replace_all` and `move_current_line`, cleared `extra_cursors` on Go-To-Line and IPC navigation/insert, exited Vim Visual mode on unmodified single-click, fixed Vim `2d3w`, `Vjj`/`Vjk`, `dd` at EOF, and linewise `p` at EOF, and resolved the non-macOS `swap-panes` chord collision.
- **Renderer, theme tokens & overlays:** moved modal scrim colors into `Theme::scrim`, made `SyntaxThemeConfig::apply_json` atomic on error, fixed windowed >16 KB line caret/selection boundaries, clamped horizontal scroll on zoom-out and window widen, preserved `last_caret_rect` during partial redraws, added vertical scrolling and scrollbar to `PinsOverlay`, fixed Project Search >4 KB single-line previews, indentation highlight end-offsets, chunk-overlap deduplication, glyph advance, and narrow-window card clipping across `CommandPalette`, `TabStrip`, `FindBarState`, and `ShortcutsOverlay`.

## [0.4.1] - 2026-11-28

### Fixed
- **Security hardening:** prevented untrusted Git repositories from executing repo-configured programs on folder open by passing `-c core.fsmonitor=false -c core.untrackedCache=false`, switching `HEAD` reads to filter-free `git cat-file blob`, and skipping Git queries when repo-local config defines `[include]`/`[includeIf]` or non-LFS `[filter]` drivers; restricted updater release URL hostnames to strict ASCII hostname characters; verified Unix IPC socket type and UID ownership in `/tmp` on both client and server.
- **Regex & CLI:** fixed `$` matching twice per CRLF line ending (between `\r` and `\n`), fixed `editor batch` applying multiple operations on the same file sequentially and aborting cleanly if backup read fails, enforced `--expect REV` in `editor edit --dry-run`, and fixed `editor-cli` compilation on Linux and Windows.

## [0.4.0] - 2026-11-27

### Added
- Hot Exit: unsaved `Untitled` drafts and modified files persist cleanly across app quit, Dock quit, window close, and folder switches via integrity-verified (`SHA-256`) immutable backups in the OS application state directory, with zero idle CPU, zero leftover files once saved, and a 3-button Save Conflict modal (`Overwrite` / `Reload` / `Cancel`) if a file changed on disk.
- Persistent open-tab Undo/Redo history across restarts and folder switches (even across `⌘S` saves and `⌘Z`-to-clean), stored in manifest-free, `SHA-256` and content-fingerprint verified bounded `.undo` snapshots (`256` steps / `512 KiB` per tab, `8 MiB` total cap) that are deleted immediately on `⌘W` or reload.
- Exact cursor, selection anchor, and vertical/horizontal scroll memory for open tabs and split panes across tab switches, folder switches, and full app restarts (automatically dropped on `⌘W` with zero closed-file accumulation on disk).
- Unified custom-drawn right-click context menus across the sidebar (rows and empty space), Working-Set tabs, and split-pane headers — plus Command Palette entries — with `Copy Path`, `Copy Relative Path`, and OS-idiomatic file manager reveal (`Reveal in Finder` on macOS, `Reveal in File Explorer` on Windows, `Open Containing Folder` on Linux/BSD), dynamic menu width sizing, and keyboard navigation (`↑`/`↓`/`Enter`/`Esc`).

### Fixed
- **Save integrity & core text engine:** gated APFS CoW and in-place delta save fast paths strictly on file-backed, non-forced documents so recovered Hot Exit drafts and forced overwrites always write full atomic contents; fixed `\r\n` straddling 16 KiB lazy-index boundaries, multi-byte UTF-8 escape lexing across syntax plugins, Markdown fenced embedded states, regex CRLF `$` / `[\W]` / `[\S]` / `\B` capture replacement, and `utf8_floor` / grapheme boundary edge cases.
- **Platform, Git, ignore & macOS adapter:** fixed UTF-8 directory-name panics in macOS bundle-suffix checks, `.gitignore` `foo/**` / `/**/foo` / root `.git/info/exclude` / Unicode `?` globs, atomic temp-path uniqueness and `EINTR` retries, capped Git `HEAD` reads and untracked-directory status tracking, eliminated Objective-C `CTFontRef` ARC lifetime hazards and re-entrant `Bridge` aliasing on macOS, added multi-monitor Retina backing-scale updates, guaranteed IPC socket cleanup on quit, and preserved non-US keyboard layouts.
- **Renderer, editing, Vim & CLI:** fixed word-wrap hit-testing on the last logical line and stale wrap-cache invalidation, oversized glyph atlas fallback, manual scroll preservation in `TabStrip` and `ShortcutsOverlay`, `CommandPalette` wheel scroll direction and search-slab epoch isolation, CRLF `Enter` caret placement, multi-cursor cleanup across tab/pane transitions, zero-length regex replace advancement, Vim visual-line (`Vd`/`Vy`/`Vc`) / `dt<ch>` / UTF-8 text-object and paste edge cases, non-macOS `Ctrl+Tab` matching, and CLI directory launch / `--root` flag parsing / `utf8_floor` byte reads.

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
