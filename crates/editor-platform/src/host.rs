use std::path::PathBuf;

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
}

#[derive(Debug, Default)]
pub struct HeadlessHost {
    pub title: String,
    pub cursor: Option<CursorShape>,
    pub redraw_requests: u64,
    pub quit_requested: bool,
    pub next_open_file: Option<PathBuf>,
    pub next_open_folder: Option<PathBuf>,
    pub next_save_file: Option<PathBuf>,
    pub next_confirm: Option<ConfirmChoice>,
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
}
