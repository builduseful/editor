//! Bounded per-folder session persistence (zero external dependencies).
//!
//! Stores up to [`MAX_RECENT_FOLDERS`] (12) recent workspace folders in MRU order,
//! remembering each folder's:
//! - Numbered `1..=9` pinned file paths (exact slot order)
//! - Kept open file paths
//! - Active file path + `(line, col)` cursor position
//! - Visible split pane grid (up to 2 × 2 panes, per-pane cursor, focused pane, split ratios, zoom)
//!
//! Encoded into a compact, escape-safe, backwards-compatible string stored in host preferences
//! (`FolderSessions`), never touching tracked repository files.

use std::path::{Path, PathBuf};

pub const MAX_RECENT_FOLDERS: usize = 12;
pub const DEFAULT_RATIO_BP: u16 = 5000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedPane {
    pub col: u8,
    pub row: u8,
    pub path: PathBuf,
    pub line: usize,
    pub col_offset: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderSession {
    pub folder: PathBuf,
    pub pins: Vec<PathBuf>,
    pub open: Vec<PathBuf>,
    pub active: Option<PathBuf>,
    pub active_line: usize,
    pub active_col: usize,
    /// Saved visible split panes (`col`, `row`, file path, cursor). Empty for legacy single-pane sessions.
    pub panes: Vec<SavedPane>,
    /// Index into `panes` of the focused pane.
    pub active_pane_idx: usize,
    /// Column split ratio in basis points (`1500..=8500`, default `5000` = `0.50`).
    pub col_ratio_bp: u16,
    /// Per-column row split ratios in basis points (`1500..=8500`, default `[5000, 5000]`).
    pub row_ratio_bp: [u16; 2],
    /// Whether fullscreen pane zoom was active.
    pub zoomed: bool,
}

impl FolderSession {
    pub fn col_ratio(&self) -> f32 {
        (self.col_ratio_bp.clamp(1500, 8500) as f32) / 10_000.0
    }

    pub fn row_ratios(&self) -> [f32; 2] {
        [
            (self.row_ratio_bp[0].clamp(1500, 8500) as f32) / 10_000.0,
            (self.row_ratio_bp[1].clamp(1500, 8500) as f32) / 10_000.0,
        ]
    }
}

pub fn ratio_to_bp(r: f32) -> u16 {
    (r.clamp(0.15, 0.85) * 10_000.0).round() as u16
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
    /// `<folder>\t<pins>\t<open>\t<active>\t<line>\t<col>[\t<panes>\t<active_pane_idx>\t<ratios>\t<zoomed>]`
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
            if !fs.panes.is_empty() {
                out.push('\t');
                for (i, sp) in fs.panes.iter().enumerate() {
                    if i > 0 {
                        out.push('|');
                    }
                    out.push_str(&format!(
                        "{},{},{},{},",
                        sp.col, sp.row, sp.line, sp.col_offset
                    ));
                    push_escaped(&mut out, &sp.path.to_string_lossy());
                }
                out.push('\t');
                out.push_str(&fs.active_pane_idx.to_string());
                out.push('\t');
                out.push_str(&format!(
                    "{},{},{}",
                    fs.col_ratio_bp.clamp(1500, 8500),
                    fs.row_ratio_bp[0].clamp(1500, 8500),
                    fs.row_ratio_bp[1].clamp(1500, 8500),
                ));
                out.push('\t');
                out.push(if fs.zoomed { '1' } else { '0' });
            }
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
            let panes: Vec<SavedPane> = parts
                .get(6)
                .filter(|s| !s.is_empty())
                .map(|raw_panes| {
                    raw_panes
                        .split('|')
                        .filter_map(|tok| {
                            let mut fields = tok.splitn(5, ',');
                            let col = fields.next()?.parse::<u8>().ok()?.min(1);
                            let row = fields.next()?.parse::<u8>().ok()?.min(1);
                            let line = fields.next()?.parse::<usize>().ok()?;
                            let col_offset = fields.next()?.parse::<usize>().ok()?;
                            let path_str = unescape(fields.next()?);
                            if path_str.is_empty() {
                                return None;
                            }
                            Some(SavedPane {
                                col,
                                row,
                                path: PathBuf::from(path_str),
                                line,
                                col_offset,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            let active_pane_idx = parts
                .get(7)
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(0);
            let (col_ratio_bp, row_ratio_bp) = parts
                .get(8)
                .map(|s| {
                    let mut it = s.split(',');
                    let c = it
                        .next()
                        .and_then(|v| v.parse::<u16>().ok())
                        .unwrap_or(DEFAULT_RATIO_BP)
                        .clamp(1500, 8500);
                    let r0 = it
                        .next()
                        .and_then(|v| v.parse::<u16>().ok())
                        .unwrap_or(DEFAULT_RATIO_BP)
                        .clamp(1500, 8500);
                    let r1 = it
                        .next()
                        .and_then(|v| v.parse::<u16>().ok())
                        .unwrap_or(DEFAULT_RATIO_BP)
                        .clamp(1500, 8500);
                    (c, [r0, r1])
                })
                .unwrap_or((DEFAULT_RATIO_BP, [DEFAULT_RATIO_BP, DEFAULT_RATIO_BP]));
            let zoomed = parts.get(9).is_some_and(|s| *s == "1" || *s == "true");
            folders.push(FolderSession {
                folder,
                pins,
                open,
                active,
                active_line,
                active_col,
                panes,
                active_pane_idx,
                col_ratio_bp,
                row_ratio_bp,
                zoomed,
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
            ',' => out.push_str("%2C"),
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
                folder: PathBuf::from(format!("/ws/proj {i} | 100%, 🚀")),
                pins: vec![PathBuf::from(format!("/ws/proj {i}/a|b,c.rs"))],
                open: vec![
                    PathBuf::from(format!("/ws/proj {i}/main.rs")),
                    PathBuf::from(format!("/ws/proj {i}/lib.rs")),
                ],
                active: Some(PathBuf::from(format!("/ws/proj {i}/lib.rs"))),
                active_line: 42 + i,
                active_col: 7,
                panes: vec![
                    SavedPane {
                        col: 0,
                        row: 0,
                        path: PathBuf::from(format!("/ws/proj {i}/main,1.rs")),
                        line: 10,
                        col_offset: 2,
                    },
                    SavedPane {
                        col: 1,
                        row: 0,
                        path: PathBuf::from(format!("/ws/proj {i}/lib.rs")),
                        line: 42 + i,
                        col_offset: 7,
                    },
                ],
                active_pane_idx: 1,
                col_ratio_bp: 6250,
                row_ratio_bp: [5000, 3500],
                zoomed: i % 2 == 1,
            });
        }
        assert_eq!(store.folders.len(), MAX_RECENT_FOLDERS);
        let encoded = store.encode();
        let decoded = SessionStore::decode(&encoded);
        assert_eq!(decoded, store);
        let top = decoded
            .get(Path::new("/ws/proj 15 | 100%, 🚀"))
            .expect("latest folder present");
        assert_eq!(top.active_line, 57);
        assert_eq!(top.active_col, 7);
        assert_eq!(top.pins[0], PathBuf::from("/ws/proj 15/a|b,c.rs"));
        assert_eq!(top.panes.len(), 2);
        assert_eq!(top.panes[0].path, PathBuf::from("/ws/proj 15/main,1.rs"));
        assert_eq!(top.active_pane_idx, 1);
        assert_eq!(top.col_ratio_bp, 6250);
        assert_eq!(top.row_ratio_bp, [5000, 3500]);
        assert!(top.zoomed);

        // Legacy 6-column entries decode cleanly with default pane fields.
        let legacy = SessionStore::decode("/old/ws\t/old/ws/a.rs\t/old/ws/b.rs\t/old/ws/a.rs\t5\t2");
        assert_eq!(legacy.folders.len(), 1);
        assert!(legacy.folders[0].panes.is_empty());
        assert_eq!(legacy.folders[0].col_ratio_bp, DEFAULT_RATIO_BP);
        assert!(!legacy.folders[0].zoomed);
    }
}
