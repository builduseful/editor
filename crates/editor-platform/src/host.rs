use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::input::{ConfirmChoice, CursorShape};

pub trait Clipboard {
    fn get_text(&self) -> Option<String>;
    fn set_text(&mut self, text: &str);
}

#[derive(Debug, Default, Clone)]
pub struct MemoryClipboard {
    content: Option<String>,
}

impl MemoryClipboard {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Clipboard for MemoryClipboard {
    fn get_text(&self) -> Option<String> {
        self.content.clone()
    }

    fn set_text(&mut self, text: &str) {
        self.content = Some(text.to_string());
    }
}

/// Minimal host-window operations requested by the portable application controller.
pub trait PlatformHost {
    fn set_title(&mut self, title: &str);
    fn set_cursor(&mut self, cursor: CursorShape);
    fn open_file_dialog(&mut self) -> Option<PathBuf>;
    fn open_folder_dialog(&mut self) -> Option<PathBuf>;
    fn save_file_dialog(&mut self, default_name: Option<&str>) -> Option<PathBuf>;
    fn confirm_discard(&mut self, file_name: &str) -> ConfirmChoice;
    fn request_redraw(&mut self);
    fn request_quit(&mut self);
    /// Close the host window using OS-idiomatic semantics.
    ///
    /// Default (Windows/Linux/headless): closing the last window terminates the app.
    /// macOS overrides this to hide the window, stop idle timers, and release GPU surfaces
    /// while remaining resident in the Dock until `request_quit()` (`⌘Q`) is invoked.
    fn request_close_window(&mut self) {
        self.request_quit();
    }
    fn load_pref(&self, _key: &str) -> Option<String> {
        None
    }
    fn save_pref(&mut self, _key: &str, _value: &str) {}
    /// Watch a workspace directory using the OS's kernel event-driven watcher (e.g. `FSEvents`,
    /// `inotify`, `ReadDirectoryChangesW`). Passing `None` stops watching.
    /// Default is a no-op for headless/test hosts.
    fn watch_directory(&mut self, _dir: Option<&Path>) {}
}

#[derive(Debug, Default)]
pub struct HeadlessHost {
    pub title: String,
    pub cursor: Option<CursorShape>,
    pub redraw_requests: u64,
    pub quit_requested: bool,
    pub close_window_requested: bool,
    /// Test knob: when `true`, mimics macOS stay-in-Dock semantics (`request_close_window` does not quit).
    pub stay_alive_on_close: bool,
    pub watched_dir: Option<PathBuf>,
    pub next_open_file: Option<PathBuf>,
    pub next_open_folder: Option<PathBuf>,
    pub next_save_file: Option<PathBuf>,
    pub next_confirm: Option<ConfirmChoice>,
    pub prefs: BTreeMap<String, String>,
}

impl HeadlessHost {
    pub fn new() -> Self {
        Self::default()
    }
}

impl PlatformHost for HeadlessHost {
    fn set_title(&mut self, title: &str) {
        self.title = title.to_string();
    }

    fn set_cursor(&mut self, cursor: CursorShape) {
        self.cursor = Some(cursor);
    }

    fn open_file_dialog(&mut self) -> Option<PathBuf> {
        self.next_open_file.take()
    }

    fn open_folder_dialog(&mut self) -> Option<PathBuf> {
        self.next_open_folder.take()
    }

    fn save_file_dialog(&mut self, _default_name: Option<&str>) -> Option<PathBuf> {
        self.next_save_file.take()
    }

    fn confirm_discard(&mut self, _file_name: &str) -> ConfirmChoice {
        self.next_confirm.take().unwrap_or(ConfirmChoice::Discard)
    }

    fn request_redraw(&mut self) {
        self.redraw_requests += 1;
    }

    fn request_quit(&mut self) {
        self.quit_requested = true;
    }

    fn request_close_window(&mut self) {
        self.close_window_requested = true;
        if !self.stay_alive_on_close {
            self.quit_requested = true;
        }
    }

    fn load_pref(&self, key: &str) -> Option<String> {
        self.prefs.get(key).cloned()
    }

    fn save_pref(&mut self, key: &str, value: &str) {
        self.prefs.insert(key.to_string(), value.to_string());
    }

    fn watch_directory(&mut self, dir: Option<&Path>) {
        self.watched_dir = dir.map(Path::to_path_buf);
    }
}
