use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    /// Command on macOS, Super/Meta on Linux/Windows.
    pub meta: bool,
}

impl Modifiers {
    pub const NONE: Self = Self {
        shift: false,
        ctrl: false,
        alt: false,
        meta: false,
    };

    /// Primary shortcut modifier (Cmd on macOS, Ctrl elsewhere).
    #[inline]
    pub fn primary(self) -> bool {
        #[cfg(target_os = "macos")]
        {
            self.meta
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.ctrl
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    Char(char),
    Enter,
    Tab,
    Backspace,
    Delete,
    Escape,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    F(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CursorShape {
    Arrow,
    IBeam,
    Pointer,
    ResizeHorizontal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SystemTheme {
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThemePreference {
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConfirmChoice {
    Save,
    Discard,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AppCommand {
    NewFile,
    OpenFile,
    OpenFolder,
    Save,
    SaveAs,
    Reload,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
    Find,
    Replace,
    GoToLine,
    ToggleWrap,
    ToggleSidebar,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    SetTheme(ThemePreference),
    Quit,
}

/// Platform-neutral input & lifecycle events delivered from the host to `editor-app`.
#[derive(Debug, Clone, PartialEq)]
pub enum InputEvent {
    Resize {
        width_px: u32,
        height_px: u32,
        scale: f32,
    },
    KeyDown {
        key: Key,
        mods: Modifiers,
        /// Committed UTF-8 text if this keystroke inserts characters (future IME hook compatible).
        text: Option<String>,
    },
    MouseDown {
        x_px: f32,
        y_px: f32,
        button: MouseButton,
        click_count: u8,
        mods: Modifiers,
    },
    MouseMove {
        x_px: f32,
        y_px: f32,
        mods: Modifiers,
    },
    MouseUp {
        x_px: f32,
        y_px: f32,
        button: MouseButton,
        mods: Modifiers,
    },
    Scroll {
        dx_px: f32,
        dy_px: f32,
        x_px: f32,
        y_px: f32,
        mods: Modifiers,
    },
    FocusChanged(bool),
    SystemThemeChanged(SystemTheme),
    OpenPath(PathBuf),
    Command(AppCommand),
    /// Periodic caret blink / debounce timer tick (`now_ms` monotonic milliseconds).
    Tick {
        now_ms: u64,
    },
}
