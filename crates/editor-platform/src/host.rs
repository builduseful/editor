use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::input::{ConfirmChoice, CursorShape};

/// Thread-safe callback that wakes the host event loop (delivering `InputEvent::Tick`)
/// when a short-lived background worker finishes a batch.
pub type MainLoopWaker = Arc<dyn Fn() + Send + Sync>;

/// Thread-safe HTTPS GET callback `(url, max_bytes) -> Result<Vec<u8>, String>` backed by a
/// short-lived OS `curl` subprocess (keeping TLS/HTTP frameworks out of the GUI process).
pub type HttpFetcher = Arc<dyn Fn(&str, usize) -> Result<Vec<u8>, String> + Send + Sync>;

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
    /// Watch an optional secondary metadata directory (e.g. an enclosing monorepo's or worktree's
    /// `.git` directory when it lies outside the watched workspace root) on the same OS event
    /// stream. Passing `None` clears the secondary path. Default is a no-op.
    fn watch_extra_directory(&mut self, _dir: Option<&Path>) {}
    /// Return a thread-safe waker for the host's main runloop (delivering `InputEvent::Tick`).
    /// Returns `None` on headless/test hosts so operations stay synchronous and deterministic.
    fn main_loop_waker(&self) -> Option<MainLoopWaker> {
        None
    }
    /// Return a thread-safe native HTTPS fetcher for update checks and downloads.
    /// Returns `None` on headless/test hosts so tests never touch the network.
    fn http_fetcher(&self) -> Option<HttpFetcher> {
        None
    }
    /// Schedule relaunching the newly swapped application bundle right after this process exits.
    fn relaunch_after_update(&mut self, _app_bundle: &Path) {}
    /// Optional OS-standard persistent app state directory for Hot Exit unsaved drafts
    /// (never inside user repositories).
    /// Returns `None` on default headless/snapshot hosts so tests stay isolated unless configured.
    fn state_dir(&self) -> Option<PathBuf> {
        None
    }
    /// OS-idiomatic label for revealing a file or folder in the host's native file manager
    /// (`"Reveal in Finder"` on macOS, `"Reveal in File Explorer"` on Windows,
    /// `"Open Containing Folder"` on Linux/BSD).
    fn reveal_in_file_manager_label(&self) -> &'static str {
        default_reveal_in_file_manager_label()
    }
    /// Reveal a file or directory in the host OS's native file manager (selecting the item
    /// where supported). Returns `Err(reason)` if unavailable or the path cannot be revealed.
    fn reveal_in_file_manager(&mut self, _path: &Path) -> Result<(), String> {
        Err("Reveal in file manager is unavailable".to_string())
    }
}

/// Default OS-idiomatic label for revealing a file or directory in the native file manager.
pub fn default_reveal_in_file_manager_label() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "Reveal in Finder"
    }
    #[cfg(target_os = "windows")]
    {
        "Reveal in File Explorer"
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        "Open Containing Folder"
    }
}

/// Format a filesystem path cleanly for clipboard copy and user-facing display.
///
/// On Windows, strips the `\\?\` and `\\?\UNC\` verbatim prefixes that `std::fs::canonicalize`
/// produces so copied paths paste cleanly into shells, terminals, and dialogs.
/// On Unix, returns the lossy UTF-8 string unchanged.
pub fn display_path_string(path: &Path) -> String {
    let s = path.to_string_lossy();
    #[cfg(target_os = "windows")]
    {
        strip_windows_verbatim_prefix(&s).into_owned()
    }
    #[cfg(not(target_os = "windows"))]
    {
        s.into_owned()
    }
}

#[cfg(any(target_os = "windows", test))]
fn strip_windows_verbatim_prefix(s: &str) -> std::borrow::Cow<'_, str> {
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return std::borrow::Cow::Owned(format!(r"\\{rest}"));
    }
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        return std::borrow::Cow::Borrowed(rest);
    }
    std::borrow::Cow::Borrowed(s)
}

/// Resolve the OS-standard per-user application state directory (`std` only, zero crates):
/// - macOS: `$HOME/Library/Application Support/com.builduseful.editor`
/// - Linux/BSD: `$XDG_STATE_HOME/editor` (only when absolute; fallback `$HOME/.local/state/editor`)
/// - Windows: `%LOCALAPPDATA%\BuildUseful\editor` (fallback `%APPDATA%\BuildUseful\editor`)
pub fn default_app_state_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())?;
        Some(
            home.join("Library")
                .join("Application Support")
                .join(crate::version::BUNDLE_IDENTIFIER),
        )
    }
    #[cfg(target_os = "windows")]
    {
        let base = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| {
                std::env::var_os("APPDATA")
                    .map(PathBuf::from)
                    .filter(|p| p.is_absolute())
            })?;
        Some(base.join("BuildUseful").join("editor"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        if let Some(xdg) = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
        {
            return Some(xdg.join("editor"));
        }
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())?;
        Some(home.join(".local").join("state").join("editor"))
    }
}

#[derive(Default)]
pub struct HeadlessHost {
    pub title: String,
    pub cursor: Option<CursorShape>,
    pub redraw_requests: u64,
    pub quit_requested: bool,
    pub close_window_requested: bool,
    /// Test knob: when `true`, mimics macOS stay-in-Dock semantics (`request_close_window` does not quit).
    pub stay_alive_on_close: bool,
    pub watched_dir: Option<PathBuf>,
    pub watched_extra_dir: Option<PathBuf>,
    pub next_open_file: Option<PathBuf>,
    pub next_open_folder: Option<PathBuf>,
    pub next_save_file: Option<PathBuf>,
    pub next_confirm: Option<ConfirmChoice>,
    pub prefs: BTreeMap<String, String>,
    /// Optional mock HTTPS fetcher for offline updater tests.
    pub mock_http_fetcher: Option<HttpFetcher>,
    /// Recorded app bundle path if `relaunch_after_update` was invoked.
    pub relaunched_app: Option<PathBuf>,
    /// Optional state directory for deterministic Hot Exit & view-memory tests.
    pub state_dir: Option<PathBuf>,
    /// Last path passed to `reveal_in_file_manager` (for headless/integration tests).
    pub last_revealed_path: Option<PathBuf>,
    /// Optional override for `reveal_in_file_manager_label` so tests can verify OS-agnostic
    /// label sizing and dispatch across platforms.
    pub reveal_label_override: Option<&'static str>,
}

impl std::fmt::Debug for HeadlessHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HeadlessHost")
            .field("title", &self.title)
            .field("quit_requested", &self.quit_requested)
            .field("prefs_len", &self.prefs.len())
            .finish()
    }
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

    fn watch_extra_directory(&mut self, dir: Option<&Path>) {
        self.watched_extra_dir = dir.map(Path::to_path_buf);
    }

    fn http_fetcher(&self) -> Option<HttpFetcher> {
        self.mock_http_fetcher.clone()
    }

    fn relaunch_after_update(&mut self, app_bundle: &Path) {
        self.relaunched_app = Some(app_bundle.to_path_buf());
    }

    fn state_dir(&self) -> Option<PathBuf> {
        self.state_dir.clone()
    }

    fn reveal_in_file_manager_label(&self) -> &'static str {
        self.reveal_label_override
            .unwrap_or_else(default_reveal_in_file_manager_label)
    }

    fn reveal_in_file_manager(&mut self, path: &Path) -> Result<(), String> {
        self.last_revealed_path = Some(path.to_path_buf());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_verbatim_prefix_is_stripped_cleanly() {
        assert_eq!(
            strip_windows_verbatim_prefix(r"\\?\C:\src\main.rs"),
            r"C:\src\main.rs"
        );
        assert_eq!(
            strip_windows_verbatim_prefix(r"\\?\UNC\server\share\file.txt"),
            r"\\server\share\file.txt"
        );
        assert_eq!(
            strip_windows_verbatim_prefix(r"C:\plain\path.rs"),
            r"C:\plain\path.rs"
        );
        assert_eq!(
            strip_windows_verbatim_prefix("/workspace/src/main.rs"),
            "/workspace/src/main.rs"
        );
    }

    #[test]
    fn display_path_string_preserves_unix_paths() {
        let p = Path::new("/workspace/src/lib.rs");
        assert_eq!(display_path_string(p), "/workspace/src/lib.rs");
    }
}
