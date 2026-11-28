//! Data-driven, context-scoped keymap and single-source-of-truth shortcut registry.
//!
//! Architecture:
//! - Every shortcut in the app (Global, FindBar, Document, and Vim reference) lives here.
//! - Zero hardcoded keybinding switches in `EditorApp::handle_key_down`: every action
//!   dispatches via `Keymap::resolve(scope, key, mods)` so user customization (`rebind` /
//!   `apply_overrides` / `unbind`) automatically applies everywhere AND stays 100% in sync
//!   with the `⌘K` / `⇧⌘/` Keyboard Shortcuts overlay.
//! - Multi-chord bindings (`Vec<KeyChord>`) cleanly handle secondary aliases (e.g. `⌘=` +
//!   `⌘+` for Zoom In, `⌘K` + `⇧⌘/` for Keyboard Shortcuts, `⌘W` + `⇧⌘\` for Close Pane).
//! - Built-in `Keymap::conflicts()` detects duplicate bindings within overlapping scopes.

use editor_platform::{AppCommand, Key, Modifiers};
use editor_render::ShortcutDisplayItem;

/// Context scope in which a shortcut is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyScope {
    /// Active everywhere in the window.
    Global,
    /// Active when the Find / Replace bar is visible (overrides Global, e.g. `⌘G` = Find Next).
    FindBar,
    /// Active when editing the document viewport (yields `⌘C`/`⌘V`/`⌘A` to focused `TextInput`s).
    Document,
    /// Reference-only entry for Vim modal commands (handled by `VimState`).
    Vim,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyChord {
    pub key: Key,
    pub primary: bool,
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

impl KeyChord {
    pub const fn new(key: Key, primary: bool, shift: bool, alt: bool, ctrl: bool) -> Self {
        Self {
            key,
            primary,
            shift,
            alt,
            ctrl,
        }
    }

    pub const fn primary(key: Key) -> Self {
        Self::new(key, true, false, false, false)
    }

    pub const fn primary_shift(key: Key) -> Self {
        Self::new(key, true, true, false, false)
    }

    pub const fn primary_alt(key: Key) -> Self {
        Self::new(key, true, false, true, false)
    }

    pub const fn primary_ctrl(key: Key) -> Self {
        Self::new(key, true, false, false, true)
    }

    pub const fn alt(key: Key) -> Self {
        Self::new(key, false, false, true, false)
    }

    pub const fn ctrl(key: Key) -> Self {
        Self::new(key, false, false, false, true)
    }

    pub fn matches(self, key: Key, mods: Modifiers) -> bool {
        if self.shift != mods.shift || self.alt != mods.alt {
            return false;
        }
        #[cfg(target_os = "macos")]
        {
            if self.primary != mods.primary() || self.ctrl != mods.ctrl {
                return false;
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            // On Windows/Linux, `primary` and `ctrl` both map to the physical Ctrl modifier.
            let want_ctrl = self.primary || self.ctrl;
            if want_ctrl != mods.ctrl || mods.meta {
                return false;
            }
        }
        match (self.key, key) {
            (Key::Char(a), Key::Char(b)) => a.eq_ignore_ascii_case(&b),
            (a, b) => a == b,
        }
    }

    /// Parse a portable human-readable binding string (e.g. `"cmd+shift+p"`, `"ctrl+cmd+\\"`
    /// or `"alt+up"`) for future user `keybindings.json` customization.
    pub fn parse(spec: &str) -> Option<Self> {
        let mut primary = false;
        let mut shift = false;
        let mut alt = false;
        let mut ctrl = false;
        let mut key: Option<Key> = None;

        for raw_part in spec.split('+') {
            let p = raw_part.trim().to_ascii_lowercase();
            match p.as_str() {
                "cmd" | "meta" | "super" | "primary" => primary = true,
                "shift" => shift = true,
                "alt" | "opt" | "option" => alt = true,
                "ctrl" | "control" => ctrl = true,
                "left" => key = Some(Key::Left),
                "right" => key = Some(Key::Right),
                "up" => key = Some(Key::Up),
                "down" => key = Some(Key::Down),
                "enter" | "return" => key = Some(Key::Enter),
                "tab" => key = Some(Key::Tab),
                "esc" | "escape" => key = Some(Key::Escape),
                "backspace" => key = Some(Key::Backspace),
                "delete" => key = Some(Key::Delete),
                "slash" => key = Some(Key::Char('/')),
                "backslash" => key = Some(Key::Char('\\')),
                "plus" => key = Some(Key::Char('=')),
                s if s.chars().count() == 1 => {
                    key = Some(Key::Char(s.chars().next().unwrap()));
                }
                _ => return None,
            }
        }
        Some(Self {
            key: key?,
            primary,
            shift,
            alt,
            ctrl,
        })
    }

    /// Format using standard macOS modifier order (`⌃⌥⇧⌘`).
    pub fn format_display(self) -> String {
        let mut out = String::new();
        if self.ctrl {
            out.push('⌃');
        }
        if self.alt {
            out.push('⌥');
        }
        if self.shift {
            out.push('⇧');
        }
        if self.primary {
            out.push('⌘');
        }
        match self.key {
            Key::Char(c) => out.push(c.to_ascii_uppercase()),
            Key::Left => out.push('←'),
            Key::Right => out.push('→'),
            Key::Up => out.push('↑'),
            Key::Down => out.push('↓'),
            Key::Enter => out.push('↵'),
            Key::Tab => out.push('⇥'),
            Key::Escape => out.push_str("Esc"),
            Key::Backspace => out.push('⌫'),
            Key::Delete => out.push('⌦'),
            Key::Home => out.push_str("Home"),
            Key::End => out.push_str("End"),
            Key::PageUp => out.push_str("PgUp"),
            Key::PageDown => out.push_str("PgDn"),
            Key::F(n) => out.push_str(&format!("F{n}")),
        }
        out
    }
}

#[derive(Debug, Clone)]
pub struct ShortcutEntry {
    pub id: &'static str,
    pub label: &'static str,
    pub category: &'static str,
    pub scope: KeyScope,
    pub chords: Vec<KeyChord>,
    pub display_override: Option<&'static str>,
    pub featured: bool,
    pub command: Option<AppCommand>,
    pub runnable: bool,
}

impl ShortcutEntry {
    pub fn display_keys(&self) -> String {
        if let Some(ov) = self.display_override {
            ov.to_string()
        } else if let Some(first) = self.chords.first() {
            first.format_display()
        } else {
            String::new()
        }
    }
}

pub struct Keymap {
    entries: Vec<ShortcutEntry>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self::new()
    }
}

impl Keymap {
    pub fn new() -> Self {
        Self {
            entries: default_shortcuts(),
        }
    }

    pub fn entries(&self) -> &[ShortcutEntry] {
        &self.entries
    }

    /// Resolve a key press strictly within `scope`.
    pub fn resolve(&self, scope: KeyScope, key: Key, mods: Modifiers) -> Option<AppCommand> {
        for entry in &self.entries {
            if entry.scope == scope {
                if let Some(cmd) = entry.command {
                    if entry.chords.iter().any(|ch| ch.matches(key, mods)) {
                        return Some(cmd);
                    }
                }
            }
        }
        None
    }

    /// Convenience alias for resolving Global commands.
    pub fn lookup_command(&self, key: Key, mods: Modifiers) -> Option<AppCommand> {
        self.resolve(KeyScope::Global, key, mods)
    }

    /// Rebind an action by stable ID to a new primary chord (custom-shortcut ready).
    pub fn rebind(&mut self, id: &str, new_chord: KeyChord) -> bool {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.id == id) {
            entry.chords = vec![new_chord];
            entry.display_override = None;
            true
        } else {
            false
        }
    }

    /// Unbind an action by stable ID (e.g. `"none"` in a user keybindings file).
    pub fn unbind(&mut self, id: &str) -> bool {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.id == id) {
            entry.chords.clear();
            entry.display_override = Some("Unbound");
            true
        } else {
            false
        }
    }

    /// Apply a list of `(action_id, chord_spec)` user overrides cleanly.
    /// Returns the number of overrides applied.
    pub fn apply_overrides(&mut self, overrides: &[(&str, &str)]) -> usize {
        let mut applied = 0;
        for &(id, spec) in overrides {
            if spec.eq_ignore_ascii_case("none") || spec.trim().is_empty() {
                if self.unbind(id) {
                    applied += 1;
                }
            } else if let Some(chord) = KeyChord::parse(spec) {
                if self.rebind(id, chord) {
                    applied += 1;
                }
            }
        }
        applied
    }

    /// Detect any conflicting chords within the same scope (or Global vs Document).
    /// Note: `FindBar` intentionally overrides `Global` (e.g. `⌘G` = Find Next while Find is open).
    pub fn conflicts(&self) -> Vec<(&'static str, &'static str, KeyChord)> {
        let mut out = Vec::new();
        for (i, a) in self.entries.iter().enumerate() {
            for b in &self.entries[i + 1..] {
                let overlaps = a.scope == b.scope
                    || matches!(
                        (a.scope, b.scope),
                        (KeyScope::Global, KeyScope::Document)
                            | (KeyScope::Document, KeyScope::Global)
                    );
                if !overlaps {
                    continue;
                }
                for &ca in &a.chords {
                    if b.chords.iter().any(|&cb| cb == ca) {
                        out.push((a.id, b.id, ca));
                    }
                }
            }
        }
        out
    }

    pub fn command_for_id(&self, id: &str) -> Option<AppCommand> {
        self.entries
            .iter()
            .find(|e| e.id == id && e.runnable)
            .and_then(|e| e.command)
    }

    pub fn to_display_items(&self) -> Vec<ShortcutDisplayItem> {
        self.entries
            .iter()
            .map(|e| ShortcutDisplayItem {
                id: e.id,
                label: e.label,
                category: e.category,
                keys: e.display_keys(),
                featured: e.featured,
                runnable: e.runnable,
            })
            .collect()
    }
}

#[allow(clippy::too_many_arguments)]
fn bind(
    id: &'static str,
    label: &'static str,
    category: &'static str,
    scope: KeyScope,
    chords: Vec<KeyChord>,
    display_override: Option<&'static str>,
    featured: bool,
    command: AppCommand,
) -> ShortcutEntry {
    ShortcutEntry {
        id,
        label,
        category,
        scope,
        chords,
        display_override,
        featured,
        command: Some(command),
        runnable: true,
    }
}

fn vim_ref(
    id: &'static str,
    label: &'static str,
    category: &'static str,
    keys: &'static str,
) -> ShortcutEntry {
    ShortcutEntry {
        id,
        label,
        category,
        scope: KeyScope::Vim,
        chords: Vec::new(),
        display_override: Some(keys),
        featured: false,
        command: None,
        runnable: false,
    }
}

fn default_shortcuts() -> Vec<ShortcutEntry> {
    vec![
        // ── Main Shortcuts (featured = true, shown immediately in ⌘K / ⇧⌘/) ──
        bind("quick-open", "Quick open file", "File & Navigation", KeyScope::Global, vec![KeyChord::primary(Key::Char('p'))], None, true, AppCommand::QuickOpen),
        bind("command-palette", "Command palette", "File & Navigation", KeyScope::Global, vec![KeyChord::primary_shift(Key::Char('p'))], None, true, AppCommand::CommandPalette),
        bind("pinned-files", "Pinned & open files", "File & Navigation", KeyScope::Global, vec![KeyChord::primary(Key::Char('e'))], None, true, AppCommand::ShowPins),
        bind("pin-file", "Pin or unpin current file (1–9)", "File & Navigation", KeyScope::Global, vec![KeyChord::primary(Key::Char('m'))], None, true, AppCommand::TogglePin),
        bind("project-search", "Search in files", "File & Navigation", KeyScope::Global, vec![KeyChord::primary_shift(Key::Char('f'))], None, true, AppCommand::ProjectSearch),
        bind("find", "Find in file", "File & Navigation", KeyScope::Global, vec![KeyChord::primary(Key::Char('f'))], None, true, AppCommand::Find),
        bind("goto-line", "Go to line", "File & Navigation", KeyScope::Global, vec![KeyChord::primary(Key::Char('g'))], None, true, AppCommand::GoToLine),
        bind("split", "Split editor right", "Panes & View", KeyScope::Global, vec![KeyChord::primary(Key::Char('\\'))], Some("⌘\\"), true, AppCommand::SplitPane),
        bind("zoom-pane", "Toggle fullscreen pane zoom", "Panes & View", KeyScope::Global, vec![KeyChord::primary_shift(Key::Enter)], Some("⇧⌘↵"), true, AppCommand::ToggleZoomPane),
        bind("close-tab", "Close tab or active split pane", "Panes & View", KeyScope::Global, vec![KeyChord::primary(Key::Char('w'))], Some("⌘W"), true, AppCommand::CloseTab),
        bind("toggle-sidebar", "Toggle file sidebar", "Panes & View", KeyScope::Global, vec![KeyChord::primary(Key::Char('b'))], None, true, AppCommand::ToggleSidebar),
        bind("focus-sidebar", "Focus file sidebar", "Panes & View", KeyScope::Global, vec![KeyChord::primary_shift(Key::Char('e'))], None, false, AppCommand::FocusSidebar),
        bind("toggle-sidebar-side", "Move sidebar left / right", "Panes & View", KeyScope::Global, vec![KeyChord::primary_alt(Key::Char('b'))], None, false, AppCommand::ToggleSidebarSide),
        bind("save", "Save file", "File & Navigation", KeyScope::Global, vec![KeyChord::primary(Key::Char('s'))], None, true, AppCommand::Save),
        bind("toggle-comment", "Toggle line comment", "Editing", KeyScope::Document, vec![KeyChord::primary(Key::Char('/'))], Some("⌘/"), true, AppCommand::ToggleComment),
        bind("select-next-occurrence", "Select next occurrence", "Editing", KeyScope::Document, vec![KeyChord::primary(Key::Char('d'))], None, true, AppCommand::SelectNextOccurrence),
        bind("toggle-vim", "Toggle Vim mode", "Panes & View", KeyScope::Global, vec![KeyChord::primary_shift(Key::Char('v'))], None, true, AppCommand::ToggleVimMode),
        bind("shortcuts", "Show keyboard shortcuts", "Panes & View", KeyScope::Global, vec![KeyChord::primary(Key::Char('k')), KeyChord::primary_shift(Key::Char('/')), KeyChord::primary_shift(Key::Char('?')), KeyChord::primary(Key::Char('?'))], Some("⌘K"), true, AppCommand::ShowShortcuts),

        // ── File & Navigation (extra) ──
        bind("new-tab", "New untitled tab", "File & Navigation", KeyScope::Global, vec![KeyChord::primary(Key::Char('t'))], None, false, AppCommand::NewTab),
        bind("new", "New untitled file", "File & Navigation", KeyScope::Global, vec![KeyChord::primary(Key::Char('n'))], None, false, AppCommand::NewFile),
        bind("open-dialog", "Open file…", "File & Navigation", KeyScope::Global, vec![KeyChord::primary(Key::Char('o'))], None, false, AppCommand::OpenFile),
        bind("open-folder", "Open folder…", "File & Navigation", KeyScope::Global, vec![KeyChord::primary_shift(Key::Char('o'))], None, false, AppCommand::OpenFolder),
        bind("save-as", "Save file as…", "File & Navigation", KeyScope::Global, vec![KeyChord::primary_shift(Key::Char('s'))], None, false, AppCommand::SaveAs),
        bind("last-buffer", "Switch to previous file", "File & Navigation", KeyScope::Global, vec![KeyChord::ctrl(Key::Tab)], Some("⌃⇥"), false, AppCommand::ToggleLastBuffer),
        bind("prev-tab", "Previous tab", "File & Navigation", KeyScope::Global, vec![KeyChord::primary_shift(Key::Char('['))], Some("⇧⌘["), false, AppCommand::PrevTab),
        bind("next-tab", "Next tab", "File & Navigation", KeyScope::Global, vec![KeyChord::primary_shift(Key::Char(']'))], Some("⇧⌘]"), false, AppCommand::NextTab),
        bind("jump-pin-1", "Jump to pinned file 1", "File & Navigation", KeyScope::Global, vec![KeyChord::primary(Key::Char('1'))], Some("⌘1"), false, AppCommand::JumpPin(1)),
        bind("jump-pin-2", "Jump to pinned file 2", "File & Navigation", KeyScope::Global, vec![KeyChord::primary(Key::Char('2'))], Some("⌘2"), false, AppCommand::JumpPin(2)),
        bind("jump-pin-3", "Jump to pinned file 3", "File & Navigation", KeyScope::Global, vec![KeyChord::primary(Key::Char('3'))], Some("⌘3"), false, AppCommand::JumpPin(3)),
        bind("jump-pin-4", "Jump to pinned file 4", "File & Navigation", KeyScope::Global, vec![KeyChord::primary(Key::Char('4'))], Some("⌘4"), false, AppCommand::JumpPin(4)),
        bind("jump-pin-5", "Jump to pinned file 5", "File & Navigation", KeyScope::Global, vec![KeyChord::primary(Key::Char('5'))], Some("⌘5"), false, AppCommand::JumpPin(5)),
        bind("jump-pin-6", "Jump to pinned file 6", "File & Navigation", KeyScope::Global, vec![KeyChord::primary(Key::Char('6'))], Some("⌘6"), false, AppCommand::JumpPin(6)),
        bind("jump-pin-7", "Jump to pinned file 7", "File & Navigation", KeyScope::Global, vec![KeyChord::primary(Key::Char('7'))], Some("⌘7"), false, AppCommand::JumpPin(7)),
        bind("jump-pin-8", "Jump to pinned file 8", "File & Navigation", KeyScope::Global, vec![KeyChord::primary(Key::Char('8'))], Some("⌘8"), false, AppCommand::JumpPin(8)),
        bind("jump-pin-9", "Jump to pinned file 9", "File & Navigation", KeyScope::Global, vec![KeyChord::primary(Key::Char('9'))], Some("⌘9"), false, AppCommand::JumpPin(9)),
        bind("replace", "Find and replace", "File & Navigation", KeyScope::Global, vec![KeyChord::primary_alt(Key::Char('f'))], None, false, AppCommand::Replace),
        bind("find-next", "Find next match (when Find is open)", "File & Navigation", KeyScope::FindBar, vec![KeyChord::primary(Key::Char('g'))], Some("⌘G"), false, AppCommand::FindNext),
        bind("find-prev", "Find previous match (when Find is open)", "File & Navigation", KeyScope::FindBar, vec![KeyChord::primary_shift(Key::Char('g'))], Some("⇧⌘G"), false, AppCommand::FindPrev),
        bind("next-git-change", "Jump to next Git change", "File & Navigation", KeyScope::Global, vec![KeyChord::new(Key::F(5), false, false, true, false)], Some("⌥F5"), false, AppCommand::NextGitChange),
        bind("prev-git-change", "Jump to previous Git change", "File & Navigation", KeyScope::Global, vec![KeyChord::new(Key::F(5), false, true, true, false)], Some("⇧⌥F5"), false, AppCommand::PrevGitChange),
        bind("quit", "Quit editor", "File & Navigation", KeyScope::Global, vec![KeyChord::primary(Key::Char('q'))], None, false, AppCommand::Quit),

        // ── Editing (Document scope — 100% Keymap-dispatched!) ──
        bind("undo", "Undo", "Editing", KeyScope::Document, vec![KeyChord::primary(Key::Char('z'))], None, false, AppCommand::Undo),
        bind("redo", "Redo", "Editing", KeyScope::Document, vec![KeyChord::primary_shift(Key::Char('z'))], None, false, AppCommand::Redo),
        bind("cut", "Cut selection", "Editing", KeyScope::Document, vec![KeyChord::primary(Key::Char('x'))], None, false, AppCommand::Cut),
        bind("copy", "Copy selection", "Editing", KeyScope::Document, vec![KeyChord::primary(Key::Char('c'))], None, false, AppCommand::Copy),
        bind("paste", "Paste", "Editing", KeyScope::Document, vec![KeyChord::primary(Key::Char('v'))], None, false, AppCommand::Paste),
        bind("select-all", "Select all", "Editing", KeyScope::Document, vec![KeyChord::primary(Key::Char('a'))], None, false, AppCommand::SelectAll),
        bind("duplicate-line", "Duplicate current line", "Editing", KeyScope::Document, vec![KeyChord::primary_shift(Key::Char('d'))], None, false, AppCommand::DuplicateLine),
        bind("delete-line", "Delete current line", "Editing", KeyScope::Document, vec![KeyChord::primary_shift(Key::Char('k'))], None, false, AppCommand::DeleteLine),
        bind("move-line-up", "Move line up", "Editing", KeyScope::Document, vec![KeyChord::alt(Key::Up)], None, false, AppCommand::MoveLineUp),
        bind("move-line-down", "Move line down", "Editing", KeyScope::Document, vec![KeyChord::alt(Key::Down)], None, false, AppCommand::MoveLineDown),

        // ── Panes & View ──
        bind("split-down", "Split editor down", "Panes & View", KeyScope::Global, vec![KeyChord::primary_alt(Key::Char('\\'))], Some("⌥⌘\\"), false, AppCommand::SplitPaneDown),
        bind("close-pane", "Close active split pane", "Panes & View", KeyScope::Global, vec![KeyChord::primary_shift(Key::Char('\\'))], Some("⇧⌘\\"), false, AppCommand::CloseSplit),
        bind(
            "swap-panes",
            "Swap split panes",
            "Panes & View",
            KeyScope::Global,
            vec![{
                #[cfg(target_os = "macos")]
                { KeyChord::primary_ctrl(Key::Char('\\')) }
                #[cfg(not(target_os = "macos"))]
                { KeyChord::new(Key::Char('\\'), true, true, true, false) }
            }],
            {
                #[cfg(target_os = "macos")]
                { Some("⌃⌘\\") }
                #[cfg(not(target_os = "macos"))]
                { Some("Ctrl+Alt+Shift+\\") }
            },
            false,
            AppCommand::SwapPanes,
        ),
        bind("focus-left", "Focus left split pane", "Panes & View", KeyScope::Global, vec![KeyChord::primary_alt(Key::Left)], Some("⌥⌘←"), false, AppCommand::FocusLeftPane),
        bind("focus-right", "Focus right split pane", "Panes & View", KeyScope::Global, vec![KeyChord::primary_alt(Key::Right)], Some("⌥⌘→"), false, AppCommand::FocusRightPane),
        bind("focus-up", "Focus upper split pane", "Panes & View", KeyScope::Global, vec![KeyChord::primary_alt(Key::Up)], Some("⌥⌘↑"), false, AppCommand::FocusUpPane),
        bind("focus-down", "Focus lower split pane", "Panes & View", KeyScope::Global, vec![KeyChord::primary_alt(Key::Down)], Some("⌥⌘↓"), false, AppCommand::FocusDownPane),
        bind("toggle-wrap", "Toggle word wrap", "Panes & View", KeyScope::Global, vec![KeyChord::alt(Key::Char('z'))], Some("⌥Z"), false, AppCommand::ToggleWrap),
        bind("zoom-in", "Zoom in text", "Panes & View", KeyScope::Global, vec![KeyChord::primary(Key::Char('=')), KeyChord::primary_shift(Key::Char('=')), KeyChord::primary(Key::Char('+'))], Some("⌘+"), false, AppCommand::ZoomIn),
        bind("zoom-out", "Zoom out text", "Panes & View", KeyScope::Global, vec![KeyChord::primary(Key::Char('-'))], Some("⌘-"), false, AppCommand::ZoomOut),
        bind("zoom-reset", "Reset zoom level", "Panes & View", KeyScope::Global, vec![KeyChord::primary(Key::Char('0'))], Some("⌘0"), false, AppCommand::ZoomReset),

        // ── Vim Motions (Normal Mode) ──
        vim_ref("vim.hjkl", "Move left / down / up / right", "Vim Motions", "h j k l"),
        vim_ref("vim.word", "Next word / previous word / word end", "Vim Motions", "w / b / e"),
        vim_ref("vim.line", "Line start / first non-blank / line end", "Vim Motions", "0 / ^ / $"),
        vim_ref("vim.file", "Top of file / bottom or line N", "Vim Motions", "gg / G"),
        vim_ref("vim.bracket", "Jump to matching bracket", "Vim Motions", "%"),
        vim_ref("vim.findchar", "Find / till character on line (repeat ; ,)", "Vim Motions", "f / F / t / T"),
        vim_ref("vim.paragraph", "Previous / next paragraph", "Vim Motions", "{ / }"),
        vim_ref("vim.halfpage", "Scroll half-page down / up", "Vim Motions", "⌃D / ⌃U"),
        vim_ref("vim.center", "Centre current line in viewport", "Vim Motions", "zz"),
        vim_ref("vim.search", "Search / next / prev / word under cursor", "Vim Motions", "/  n  N  *"),

        // ── Vim Editing & Panes ──
        vim_ref("vim.insert", "Insert before / after / line start / end", "Vim Editing & Panes", "i  a  I  A"),
        vim_ref("vim.openline", "Open new line below / above", "Vim Editing & Panes", "o / O"),
        vim_ref("vim.operators", "Delete / change / yank + motion or text object", "Vim Editing & Panes", "d / c / y"),
        vim_ref("vim.lineops", "Delete / change / yank whole line", "Vim Editing & Panes", "dd / cc / yy"),
        vim_ref("vim.textobj", "Inner word / quotes / brackets (e.g. ciw, di\")", "Vim Editing & Panes", "iw  i\"  i(  i{"),
        vim_ref("vim.charops", "Delete char / replace char / join lines", "Vim Editing & Panes", "x  r  J"),
        vim_ref("vim.paste", "Paste after / before cursor", "Vim Editing & Panes", "p / P"),
        vim_ref("vim.indent", "Indent / outdent line", "Vim Editing & Panes", ">> / <<"),
        vim_ref("vim.undo", "Undo / Redo", "Vim Editing & Panes", "u / ⌃R"),
        vim_ref("vim.visual", "Visual character / visual line mode", "Vim Editing & Panes", "v / V"),
        vim_ref("vim.pins", "Pin / pinned & open files / jump 1–9 / last file", "Vim Editing & Panes", "Space a / e / 1–9 / Space"),
        vim_ref("vim.find", "Quick open / search in files / buffers / sidebar", "Vim Editing & Panes", "Space ff / fg / / / fb / fe"),
        vim_ref("vim.tabs", "Next / prev tab / jump pin / alternate file", "Vim Editing & Panes", "gt / gT / g1–9 / ⌃^"),
        vim_ref("vim.panes", "Split / zoom / only / swap / switch / close", "Vim Editing & Panes", "⌃W v s / z / o / x / hjklw / q"),
    ]
}
