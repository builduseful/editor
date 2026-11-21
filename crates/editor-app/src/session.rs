//! Bounded per-folder session persistence (zero external dependencies).
//!
//! Stores up to [`MAX_RECENT_FOLDERS`] (12) recent workspace folders in MRU order,
//! remembering each folder's:
//! - Harpoon `1..=9` pinned file paths (exact slot order)
//! - Kept open file paths
//! - Active file path + `(line, col)` cursor position
//!
//! Encoded into a compact, escape-safe string stored in host preferences (`FolderSessions`),
//! never touching tracked repository files.

use std::path::{Path, PathBuf};

pub const MAX_RECENT_FOLDERS: usize = 12;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderSession {
    pub folder: PathBuf,
    pub pins: Vec<PathBuf>,
    pub open: Vec<PathBuf>,
    pub active: Option<PathBuf>,
    pub active_line: usize,
    pub active_col: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionStore {
    /// MRU-ordered recent folder sessions (`folders[0]` is most recent).
    pub folders: Vec<FolderSession>,
}

impl SessionStore {
    pub fn upsert(&mut self, session: FolderSession) {
        self.folders.retain(|f| f.folder != session.folder);
        self.folders.insert(0, session);
        if self.folders.len() > MAX_RECENT_FOLDERS {
            self.folders.truncate(MAX_RECENT_FOLDERS);
        }
    }

    pub fn get(&self, folder: &Path) -> Option<&FolderSession> {
        self.folders.iter().find(|f| f.folder == folder)
    }

    /// Encode all folder sessions into a single deterministic, escape-safe string.
    /// Format per line (`\n`-separated):
    /// `<folder>\t<pin1|pin2>\t<open1|open2>\t<active>\t<line>\t<col>`
    pub fn encode(&self) -> String {
        let mut out = String::new();
        for (idx, fs) in self.folders.iter().take(MAX_RECENT_FOLDERS).enumerate() {
            if idx > 0 {
                out.push('\n');
            }
            push_escaped(&mut out, &fs.folder.to_string_lossy());
            out.push('\t');
            for (i, p) in fs.pins.iter().enumerate() {
                if i > 0 {
                    out.push('|');
                }
                push_escaped(&mut out, &p.to_string_lossy());
            }
            out.push('\t');
            for (i, p) in fs.open.iter().enumerate() {
                if i > 0 {
                    out.push('|');
                }
                push_escaped(&mut out, &p.to_string_lossy());
            }
            out.push('\t');
            if let Some(ref act) = fs.active {
                push_escaped(&mut out, &act.to_string_lossy());
            }
            out.push('\t');
            out.push_str(&fs.active_line.to_string());
            out.push('\t');
            out.push_str(&fs.active_col.to_string());
        }
        out
    }

    pub fn decode(raw: &str) -> Self {
        let mut folders = Vec::new();
        for line in raw.lines() {
            if line.is_empty() {
                continue;
            }
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() < 4 {
                continue;
            }
            let folder = PathBuf::from(unescape(parts[0]));
            if folder.as_os_str().is_empty() {
                continue;
            }
            let pins = if parts[1].is_empty() {
                Vec::new()
            } else {
                parts[1]
                    .split('|')
                    .filter(|s| !s.is_empty())
                    .map(|s| PathBuf::from(unescape(s)))
                    .collect()
            };
            let open = if parts[2].is_empty() {
                Vec::new()
            } else {
                parts[2]
                    .split('|')
                    .filter(|s| !s.is_empty())
                    .map(|s| PathBuf::from(unescape(s)))
                    .collect()
            };
            let active = if parts[3].is_empty() {
                None
            } else {
                Some(PathBuf::from(unescape(parts[3])))
            };
            let active_line = parts
                .get(4)
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(0);
            let active_col = parts
                .get(5)
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(0);
            folders.push(FolderSession {
                folder,
                pins,
                open,
                active,
                active_line,
                active_col,
            });
            if folders.len() >= MAX_RECENT_FOLDERS {
                break;
            }
        }
        Self { folders }
    }
}

fn push_escaped(out: &mut String, s: &str) {
    for ch in s.chars() {
        match ch {
            '%' => out.push_str("%25"),
            '\t' => out.push_str("%09"),
            '\n' => out.push_str("%0A"),
            '\r' => out.push_str("%0D"),
            '|' => out.push_str("%7C"),
            other => out.push(other),
        }
    }
}

fn unescape(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_nibble(bytes[i + 1]), hex_nibble(bytes[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_store_round_trips_special_chars_and_bounds_recent_folders() {
        let mut store = SessionStore::default();
        for i in 0..16 {
            store.upsert(FolderSession {
                folder: PathBuf::from(format!("/ws/proj {i} | 100% 🚀")),
                pins: vec![PathBuf::from(format!("/ws/proj {i}/a|b.rs"))],
                open: vec![
                    PathBuf::from(format!("/ws/proj {i}/main.rs")),
                    PathBuf::from(format!("/ws/proj {i}/lib.rs")),
                ],
                active: Some(PathBuf::from(format!("/ws/proj {i}/lib.rs"))),
                active_line: 42 + i,
                active_col: 7,
            });
        }
        assert_eq!(store.folders.len(), MAX_RECENT_FOLDERS);
        let encoded = store.encode();
        let decoded = SessionStore::decode(&encoded);
        assert_eq!(decoded, store);
        let top = decoded
            .get(Path::new("/ws/proj 15 | 100% 🚀"))
            .expect("latest folder present");
        assert_eq!(top.active_line, 57);
        assert_eq!(top.active_col, 7);
        assert_eq!(top.pins[0], PathBuf::from("/ws/proj 15/a|b.rs"));
    }
}
