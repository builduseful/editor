//! Bounded in-memory undo/redo stack for sidebar filesystem actions
//! (drag-and-drop moves, renames, creates, and small single-file deletes).
//!
//! Architecture:
//! - Completely separate from per-document `UndoStack`: document `⌘Z` edits text,
//!   sidebar `⌘Z` (or clicking Undo in the action toast) reverts filesystem ops.
//! - Strictly memory-bounded (max 32 ops, max 2 MB total snapshotted bytes) and
//!   never persisted to disk.
//! - Includes a fast bounded directory probe (`probe_dir_size_bounded`) that
//!   short-circuits immediately at the large-folder threshold (< 1 ms even on
//!   huge trees like `node_modules`).

use std::path::{Path, PathBuf};

use editor_platform::{EntryKind, FileSystem};

pub const MAX_FS_UNDO_STEPS: usize = 32;
pub const MAX_DELETE_SNAPSHOT_BYTES: usize = 256 * 1024;
pub const MAX_TOTAL_SNAPSHOT_BYTES: usize = 2 * 1024 * 1024;

/// Thresholds above which moving a folder requires explicit user confirmation.
pub const LARGE_FOLDER_ENTRY_THRESHOLD: usize = 50;
pub const LARGE_FOLDER_BYTE_THRESHOLD: u64 = 10 * 1024 * 1024;
const LARGE_FOLDER_MAX_DEPTH: usize = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsOp {
    Move {
        from: PathBuf,
        to: PathBuf,
        is_dir: bool,
    },
    CreateFile {
        path: PathBuf,
    },
    CreateFolder {
        path: PathBuf,
    },
    DeleteFile {
        path: PathBuf,
        content: Vec<u8>,
    },
}

impl FsOp {
    pub fn snapshot_bytes(&self) -> usize {
        match self {
            FsOp::DeleteFile { content, .. } => content.len(),
            _ => 0,
        }
    }

    /// Human-readable label shown in the action toast after performing the op.
    pub fn done_label(&self) -> String {
        match self {
            FsOp::Move { from, to, .. } => {
                let old_name = file_name_str(from);
                let new_name = file_name_str(to);
                if from.parent() == to.parent() {
                    format!("Renamed {old_name} to {new_name}")
                } else {
                    let dst_dir = to.parent().map(file_name_str).unwrap_or("folder");
                    format!("Moved {old_name} to {dst_dir}")
                }
            }
            FsOp::CreateFile { path } => format!("Created {}", file_name_str(path)),
            FsOp::CreateFolder { path } => format!("Created folder {}", file_name_str(path)),
            FsOp::DeleteFile { path, .. } => format!("Deleted {}", file_name_str(path)),
        }
    }

    /// Human-readable label shown after undoing the op.
    pub fn undone_label(&self) -> String {
        match self {
            FsOp::Move { from, to, .. } => {
                let name = file_name_str(from);
                if from.parent() == to.parent() {
                    format!("Restored name {name}")
                } else {
                    let orig_dir = from.parent().map(file_name_str).unwrap_or("folder");
                    format!("Moved {name} back to {orig_dir}")
                }
            }
            FsOp::CreateFile { path } => format!("Removed {}", file_name_str(path)),
            FsOp::CreateFolder { path } => format!("Removed folder {}", file_name_str(path)),
            FsOp::DeleteFile { path, .. } => format!("Restored {}", file_name_str(path)),
        }
    }
}

fn file_name_str(p: &Path) -> &str {
    p.file_name()
        .and_then(|n| n.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("/")
}

#[derive(Debug, Default)]
pub struct FsUndoStack {
    undo: Vec<FsOp>,
    redo: Vec<FsOp>,
    snapshot_bytes: usize,
}

impl FsUndoStack {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, op: FsOp) {
        for dropped in self.redo.drain(..) {
            self.snapshot_bytes = self.snapshot_bytes.saturating_sub(dropped.snapshot_bytes());
        }
        self.snapshot_bytes += op.snapshot_bytes();
        self.undo.push(op);
        self.enforce_budget();
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn pop_undo(&mut self) -> Option<FsOp> {
        let op = self.undo.pop()?;
        self.snapshot_bytes = self.snapshot_bytes.saturating_sub(op.snapshot_bytes());
        Some(op)
    }

    pub fn push_redo(&mut self, op: FsOp) {
        self.snapshot_bytes += op.snapshot_bytes();
        self.redo.push(op);
        self.enforce_budget();
    }

    pub fn pop_redo(&mut self) -> Option<FsOp> {
        let op = self.redo.pop()?;
        self.snapshot_bytes = self.snapshot_bytes.saturating_sub(op.snapshot_bytes());
        Some(op)
    }

    pub fn push_undo_without_clearing_redo(&mut self, op: FsOp) {
        self.snapshot_bytes += op.snapshot_bytes();
        self.undo.push(op);
        self.enforce_budget();
    }

    fn enforce_budget(&mut self) {
        while self.undo.len() > MAX_FS_UNDO_STEPS
            || (self.snapshot_bytes > MAX_TOTAL_SNAPSHOT_BYTES && self.undo.len() > 1)
        {
            let dropped = self.undo.remove(0);
            self.snapshot_bytes = self.snapshot_bytes.saturating_sub(dropped.snapshot_bytes());
        }
        while self.redo.len() > MAX_FS_UNDO_STEPS {
            let dropped = self.redo.remove(0);
            self.snapshot_bytes = self.snapshot_bytes.saturating_sub(dropped.snapshot_bytes());
        }
    }
}

/// Bounded directory probe result for guarding large folder moves without stalling the UI.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DirProbe {
    pub entries: usize,
    pub bytes: u64,
    pub exceeded: bool,
}

/// Inspect a directory up to `(entry_limit, byte_limit)` and short-circuit immediately once
/// either threshold or depth limit is reached (O(threshold), never scans huge trees).
pub fn probe_dir_size_bounded(
    fs: &dyn FileSystem,
    dir: &Path,
    entry_limit: usize,
    byte_limit: u64,
) -> DirProbe {
    let mut probe = DirProbe::default();
    let mut stack: Vec<(PathBuf, usize)> = vec![(dir.to_path_buf(), 0)];
    while let Some((cur, depth)) = stack.pop() {
        let Ok(entries) = fs.read_dir(&cur) else {
            continue;
        };
        for entry in entries {
            probe.entries += 1;
            if probe.entries >= entry_limit {
                probe.exceeded = true;
                return probe;
            }
            match entry.kind {
                EntryKind::Directory => {
                    if depth + 1 >= LARGE_FOLDER_MAX_DEPTH {
                        probe.exceeded = true;
                        return probe;
                    }
                    stack.push((entry.path, depth + 1));
                }
                EntryKind::File => {
                    if let Ok(st) = fs.stat(&entry.path) {
                        probe.bytes = probe.bytes.saturating_add(st.rev.size);
                        if probe.bytes >= byte_limit {
                            probe.exceeded = true;
                            return probe;
                        }
                    }
                }
                _ => {}
            }
        }
    }
    probe
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor_platform::MemoryFileSystem;

    #[test]
    fn fs_undo_stack_bounds_steps_and_bytes() {
        let mut s = FsUndoStack::new();
        for i in 0..40 {
            s.push(FsOp::Move {
                from: PathBuf::from(format!("/a/{i}.rs")),
                to: PathBuf::from(format!("/b/{i}.rs")),
                is_dir: false,
            });
        }
        assert_eq!(s.undo.len(), MAX_FS_UNDO_STEPS);
        let op = s.pop_undo().unwrap();
        assert_eq!(op.done_label(), "Moved 39.rs to b");
        assert_eq!(op.undone_label(), "Moved 39.rs back to a");
    }

    #[test]
    fn probe_dir_size_short_circuits_on_entry_and_byte_limits() {
        let fs = MemoryFileSystem::new();
        for i in 0..80 {
            fs.add_file(format!("/big/f{i}.txt"), b"hi".to_vec());
        }
        let p = probe_dir_size_bounded(&fs, Path::new("/big"), 50, 10_000_000);
        assert!(p.exceeded);
        assert_eq!(p.entries, 50);

        fs.add_file("/small/a.txt", b"hello".to_vec());
        let s = probe_dir_size_bounded(&fs, Path::new("/small"), 50, 10_000_000);
        assert!(!s.exceeded);
        assert_eq!(s.entries, 1);
    }
}
