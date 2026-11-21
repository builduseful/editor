//! Data-driven keymap and single-source-of-truth shortcut registry.
//!
//! Designed so user-customizable keybindings (`keybindings.json` / `rebind()`) slot in
//! cleanly while guaranteeing that active bindings and the `Cmd+?` Keyboard Shortcuts
//! overlay never drift out of sync.

use editor_platform::{AppCommand, Key, Modifiers};
use editor_render::ShortcutDisplayItem;

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

    pub fn matches(self, key: Key, mods: Modifiers) -> bool {
        if self.primary != mods.primary() || self.shift != mods.shift || self.alt != mods.alt {
            return false;
        }
        #[cfg(target_os = "macos")]
        if self.ctrl != mods.ctrl {
            return false;
        }
        match (self.key, key) {
            (Key::Char(a), Key::Char(b)) => a.eq_ignore_ascii_case(&b),
            (a, b) => a == b,
        }
    }

    /// Parse a human-friendly binding string like `"cmd+shift+p"` or `"alt+z"` for future
    /// custom keybinding files.
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
    pub chord: Option<KeyChord>,
    pub display_override: Option<&'static str>,
    pub featured: bool,
    pub command: Option<AppCommand>,
    pub runnable: bool,
}

impl ShortcutEntry {
    pub fn display_keys(&self) -> String {
        if let Some(ov) = self.display_override {
            ov.to_string()
        } else if let Some(ch) = self.chord {
            ch.format_display()
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

    /// Look up a global `AppCommand` matching `(key, mods)` from the active keymap.
    pub fn lookup_command(&self, key: Key, mods: Modifiers) -> Option<AppCommand> {
        for entry in &self.entries {
            if let (Some(chord), Some(cmd)) = (entry.chord, entry.command) {
                if chord.matches(key, mods) {
                    return Some(cmd);
                }
            }
        }
        None
    }

    /// Rebind an existing action by stable ID (foundation for user custom shortcuts).
    pub fn rebind(&mut self, id: &str, new_chord: KeyChord) -> bool {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.id == id) {
            entry.chord = Some(new_chord);
            entry.display_override = None;
            true
        } else {
            false
        }
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

fn cmd_entry(
    id: &'static str,
    label: &'static str,
    category: &'static str,
    chord: KeyChord,
    display_override: Option<&'static str>,
    featured: bool,
    command: AppCommand,
) -> ShortcutEntry {
    ShortcutEntry {
        id,
        label,
        category,
        chord: Some(chord),
        display_override,
        featured,
        command: Some(command),
        runnable: true,
    }
}

fn info_entry(
    id: &'static str,
    label: &'static str,
    category: &'static str,
    keys: &'static str,
    featured: bool,
    runnable: bool,
) -> ShortcutEntry {
    ShortcutEntry {
        id,
        label,
        category,
        chord: None,
        display_override: Some(keys),
        featured,
        command: None,
        runnable,
    }
}

fn default_shortcuts() -> Vec<ShortcutEntry> {
    vec![
        // ── Main Shortcuts (featured = true, shown immediately in Cmd+?) ──
        cmd_entry("quick-open", "Quick open file", "File & Navigation", KeyChord::primary(Key::Char('p')), None, true, AppCommand::QuickOpen),
        cmd_entry("project-search", "Search across project", "File & Navigation", KeyChord::primary_shift(Key::Char('f')), None, true, AppCommand::ProjectSearch),
        cmd_entry("find", "Find in file", "File & Navigation", KeyChord::primary(Key::Char('f')), None, true, AppCommand::Find),
        cmd_entry("goto-line", "Go to line", "File & Navigation", KeyChord::primary(Key::Char('g')), None, true, AppCommand::GoToLine),
        cmd_entry("split", "Split editor right", "Panes & View", KeyChord::primary(Key::Char('\\')), Some("⌘\\"), true, AppCommand::SplitPane),
        cmd_entry("toggle-sidebar", "Toggle file sidebar", "Panes & View", KeyChord::primary(Key::Char('b')), None, true, AppCommand::ToggleSidebar),
        cmd_entry("save", "Save file", "File & Navigation", KeyChord::primary(Key::Char('s')), None, true, AppCommand::Save),
        info_entry("toggle-comment", "Toggle line comment", "Editing", "⌘/", true, true),
        cmd_entry("toggle-vim", "Toggle Vim mode", "Vim Mode", KeyChord::primary_shift(Key::Char('v')), None, true, AppCommand::ToggleVimMode),
        cmd_entry("shortcuts", "Show keyboard shortcuts", "Panes & View", KeyChord::primary_shift(Key::Char('/')), Some("⇧⌘?"), true, AppCommand::ShowShortcuts),

        // ── File & Navigation (extra) ──
        cmd_entry("new", "New untitled file", "File & Navigation", KeyChord::primary(Key::Char('n')), None, false, AppCommand::NewFile),
        cmd_entry("open-dialog", "Open file…", "File & Navigation", KeyChord::primary(Key::Char('o')), None, false, AppCommand::OpenFile),
        cmd_entry("open-folder", "Open folder…", "File & Navigation", KeyChord::primary_shift(Key::Char('o')), None, false, AppCommand::OpenFolder),
        cmd_entry("save-as", "Save file as…", "File & Navigation", KeyChord::primary_shift(Key::Char('s')), None, false, AppCommand::SaveAs),
        cmd_entry("replace", "Find and replace", "File & Navigation", KeyChord::primary_alt(Key::Char('f')), None, false, AppCommand::Replace),
        info_entry("find-next", "Jump to next / previous match", "File & Navigation", "⌘G / ⇧⌘G", false, false),
        cmd_entry("quit", "Quit editor", "File & Navigation", KeyChord::primary(Key::Char('q')), None, false, AppCommand::Quit),

        // ── Editing ──
        info_entry("undo", "Undo / Redo", "Editing", "⌘Z / ⇧⌘Z", false, true),
        info_entry("duplicate-line", "Duplicate line or selection", "Editing", "⇧⌘D", false, true),
        info_entry("delete-line", "Delete current line", "Editing", "⇧⌘K", false, true),
        info_entry("move-line", "Move line up / down", "Editing", "⌥↑ / ⌥↓", false, false),
        info_entry("indent-line", "Indent / outdent selection", "Editing", "Tab / ⇧Tab", false, false),
        info_entry("select-all", "Select all", "Editing", "⌘A", false, true),

        // ── Panes & View ──
        cmd_entry("unsplit", "Close split pane", "Panes & View", KeyChord::primary_shift(Key::Char('\\')), Some("⇧⌘\\"), false, AppCommand::CloseSplit),
        cmd_entry("focus-left", "Focus left split pane", "Panes & View", KeyChord::primary_alt(Key::Left), Some("⌥⌘←"), false, AppCommand::FocusLeftPane),
        cmd_entry("focus-right", "Focus right split pane", "Panes & View", KeyChord::primary_alt(Key::Right), Some("⌥⌘→"), false, AppCommand::FocusRightPane),
        cmd_entry("toggle-wrap", "Toggle word wrap", "Panes & View", KeyChord::new(Key::Char('z'), false, false, true, false), Some("⌥Z"), false, AppCommand::ToggleWrap),
        cmd_entry("zoom-in", "Zoom in text", "Panes & View", KeyChord::primary(Key::Char('=')), Some("⌘+"), false, AppCommand::ZoomIn),
        cmd_entry("zoom-out", "Zoom out text", "Panes & View", KeyChord::primary(Key::Char('-')), Some("⌘-"), false, AppCommand::ZoomOut),
        cmd_entry("zoom-reset", "Reset zoom level", "Panes & View", KeyChord::primary(Key::Char('0')), Some("⌘0"), false, AppCommand::ZoomReset),

        // ── Vim Motions (Normal Mode) ──
        info_entry("vim.hjkl", "Move left / down / up / right", "Vim Motions", "h j k l", false, false),
        info_entry("vim.word", "Next word / previous word / word end", "Vim Motions", "w / b / e", false, false),
        info_entry("vim.line", "Line start / first non-blank / line end", "Vim Motions", "0 / ^ / $", false, false),
        info_entry("vim.file", "Top of file / bottom or line N", "Vim Motions", "gg / G", false, false),
        info_entry("vim.bracket", "Jump to matching bracket", "Vim Motions", "%", false, false),
        info_entry("vim.findchar", "Find / till character on line (repeat ; ,)", "Vim Motions", "f / F / t / T", false, false),
        info_entry("vim.paragraph", "Previous / next paragraph", "Vim Motions", "{ / }", false, false),
        info_entry("vim.halfpage", "Scroll half-page down / up", "Vim Motions", "⌃D / ⌃U", false, false),
        info_entry("vim.center", "Centre current line in viewport", "Vim Motions", "zz", false, false),
        info_entry("vim.search", "Search / next / prev / word under cursor", "Vim Motions", "/  n  N  *", false, false),

        // ── Vim Editing & Panes ──
        info_entry("vim.insert", "Insert before / after / line start / end", "Vim Editing & Panes", "i  a  I  A", false, false),
        info_entry("vim.openline", "Open new line below / above", "Vim Editing & Panes", "o / O", false, false),
        info_entry("vim.operators", "Delete / change / yank + motion or text object", "Vim Editing & Panes", "d / c / y", false, false),
        info_entry("vim.lineops", "Delete / change / yank whole line", "Vim Editing & Panes", "dd / cc / yy", false, false),
        info_entry("vim.textobj", "Inner word / quotes / brackets (e.g. ciw, di\")", "Vim Editing & Panes", "iw  i\"  i(  i{", false, false),
        info_entry("vim.charops", "Delete char / replace char / join lines", "Vim Editing & Panes", "x  r  J", false, false),
        info_entry("vim.paste", "Paste after / before cursor", "Vim Editing & Panes", "p / P", false, false),
        info_entry("vim.indent", "Indent / outdent line", "Vim Editing & Panes", ">> / <<", false, false),
        info_entry("vim.undo", "Undo / Redo", "Vim Editing & Panes", "u / ⌃R", false, false),
        info_entry("vim.visual", "Visual character / visual line mode", "Vim Editing & Panes", "v / V", false, false),
        info_entry("vim.panes", "Split right / switch pane / close split", "Vim Editing & Panes", "⌃W v / h l w / q", false, false),
    ]
}
