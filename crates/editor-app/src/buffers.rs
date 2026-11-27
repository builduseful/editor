//! In-memory Buffer Pool & Unified Working-Set model (numbered `1..9` pins + Open/Preview tabs).
//!
//! Invariants:
//! - Every open/edited/pinned file has a stable [`BufferId`].
//! - Switching files never prompts to discard unsaved changes: outgoing documents are
//!   parked safely in memory with their exact caret, anchor, scroll position, and undo history.
//! - Left side (`pinned`, up to [`MAX_PINS`] = 9): fixed numbered pin slots `1..=9`
//!   that never auto-evict and never shift when unpinned tabs open or close.
//! - Right side (`open` + optional single `preview`):
//!   - Single-clicking in the sidebar reuses the single `preview` slot (zero tab sprawl).
//!   - Editing, double-clicking, Quick Open (`⌘P`), `⌘T`, CLI `open`, or dragging graduates
//!     a file into `open`.
//!   - Saving (`⌘S`) NEVER removes a tab from `open` or `pinned`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use editor_core::Document;
use editor_render::{PinEntry, TabDisplayItem};

pub type BufferId = u32;

/// Maximum number of numbered pin slots (`1..=9`).
pub const MAX_PINS: usize = 9;
/// Default LRU cap for clean, unpinned Kept open tabs (`0` = unlimited).
/// Pinned (`1..9`), dirty (unsaved), and split-visible tabs are never evicted.
pub const DEFAULT_MAX_OPEN_TABS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenMode {
    /// Transient single-click browse slot (replaced in place on next single-click).
    Preview,
    /// Kept open tab (graduated; never auto-replaced and never vanishes on Save).
    Keep,
}

/// Saved document + view state for a buffer not currently displayed in any visible pane.
pub struct ParkedBuffer {
    pub doc: Document,
    pub caret: u64,
    pub anchor: u64,
    pub preferred_x_px: Option<f32>,
    pub scroll_top_line: usize,
    pub scroll_sub_y_px: f32,
    pub scroll_x_px: f32,
}

/// Lightweight metadata tracked for every buffer in the working set (active, split, or parked).
#[derive(Debug, Clone)]
pub struct BufferMeta {
    pub id: BufferId,
    pub path: Option<PathBuf>,
    pub file_name: String,
    pub dirty: bool,
    /// Stable Hot Exit draft ID once backed up (cleared when saved/discarded).
    pub draft_id: Option<u64>,
    /// `Document::generation()` at the time this buffer's current `.buf` snapshot was committed.
    pub last_backed_up_gen: u64,
    /// `(undo_rev, is_draft, binding_rev)` of the last persisted `.undo` snapshot so no-op blurs
    /// and quits do zero `.undo` writes.
    pub last_persisted_undo_sig: (u64, bool, u64),
}

pub struct WorkingSet {
    next_id: BufferId,
    meta: BTreeMap<BufferId, BufferMeta>,
    parked: BTreeMap<BufferId, ParkedBuffer>,
    /// Numbered pin slots `1..=9`.
    pub pinned: Vec<BufferId>,
    /// Kept unpinned open tabs.
    pub open: Vec<BufferId>,
    /// At most one transient single-click preview tab at the far right of the strip.
    pub preview: Option<BufferId>,
    /// Most-recently-used buffer order (`mru[0]` = current, `mru[1]` = previous for `⌃Tab`).
    pub mru: Vec<BufferId>,
}

impl WorkingSet {
    pub fn new(initial_id: BufferId) -> Self {
        let mut meta = BTreeMap::new();
        meta.insert(
            initial_id,
            BufferMeta {
                id: initial_id,
                path: None,
                file_name: "Untitled".to_string(),
                dirty: false,
                draft_id: None,
                last_backed_up_gen: 0,
                last_persisted_undo_sig: (0, false, 0),
            },
        );
        Self {
            next_id: initial_id + 1,
            meta,
            parked: BTreeMap::new(),
            pinned: Vec::new(),
            open: vec![initial_id],
            preview: None,
            mru: vec![initial_id],
        }
    }

    pub fn alloc_buffer(&mut self, path: Option<PathBuf>, file_name: String, dirty: bool) -> BufferId {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        self.meta.insert(
            id,
            BufferMeta {
                id,
                path,
                file_name,
                dirty,
                draft_id: None,
                last_backed_up_gen: 0,
                last_persisted_undo_sig: (0, false, 0),
            },
        );
        id
    }

    pub fn get_meta(&self, id: BufferId) -> Option<&BufferMeta> {
        self.meta.get(&id)
    }

    pub fn get_meta_mut(&mut self, id: BufferId) -> Option<&mut BufferMeta> {
        self.meta.get_mut(&id)
    }

    pub fn set_draft_state(&mut self, id: BufferId, draft_id: Option<u64>, backed_up_gen: u64) {
        if let Some(m) = self.meta.get_mut(&id) {
            m.draft_id = draft_id;
            m.last_backed_up_gen = backed_up_gen;
        }
    }

    pub fn set_undo_sig(&mut self, id: BufferId, sig: (u64, bool, u64)) {
        if let Some(m) = self.meta.get_mut(&id) {
            m.last_persisted_undo_sig = sig;
        }
    }

    pub fn find_by_draft_id(&self, draft_id: u64) -> Option<BufferId> {
        self.meta
            .values()
            .find(|m| m.draft_id == Some(draft_id))
            .map(|m| m.id)
    }

    pub fn parked_ids(&self) -> Vec<BufferId> {
        self.parked.keys().copied().collect()
    }

    pub fn parked_mut(&mut self, id: BufferId) -> Option<&mut ParkedBuffer> {
        self.parked.get_mut(&id)
    }

    pub fn parked_ref(&self, id: BufferId) -> Option<&ParkedBuffer> {
        self.parked.get(&id)
    }

    pub fn update_meta(&mut self, id: BufferId, path: Option<PathBuf>, file_name: String, dirty: bool) {
        if let Some(m) = self.meta.get_mut(&id) {
            m.path = path;
            m.file_name = file_name;
            m.dirty = dirty;
        } else {
            self.meta.insert(
                id,
                BufferMeta {
                    id,
                    path,
                    file_name,
                    dirty,
                    draft_id: None,
                    last_backed_up_gen: 0,
                    last_persisted_undo_sig: (0, false, 0),
                },
            );
        }
    }

    pub fn set_dirty(&mut self, id: BufferId, dirty: bool) {
        if let Some(m) = self.meta.get_mut(&id) {
            m.dirty = dirty;
        }
        if dirty && self.preview == Some(id) {
            self.graduate_preview(id);
        }
    }

    pub fn find_by_path(&self, path: &Path) -> Option<BufferId> {
        self.meta
            .values()
            .find(|m| m.path.as_deref() == Some(path))
            .map(|m| m.id)
    }

    pub fn ids_under_path(&self, path: &Path) -> Vec<BufferId> {
        self.meta
            .values()
            .filter(|m| m.path.as_deref().map(|p| p.starts_with(path)).unwrap_or(false))
            .map(|m| m.id)
            .collect()
    }

    /// Check clean parked buffers matching `changed` (or all clean parked buffers if `changed` is empty)
    /// and silently reload any whose on-disk identity changed. Never overwrites dirty buffers.
    /// Apply `replace_all_with_options` to a parked buffer matching `path` (keeps edits undoable!).
    /// Returns `Some(replaced_count)` if `path` was parked in the working set.
    pub fn replace_all_in_parked_by_path(
        &mut self,
        path: &Path,
        query: &str,
        replacement: &str,
        options: editor_core::SearchOptions,
        now_ms: u64,
    ) -> Option<usize> {
        let mut found: Option<(BufferId, usize)> = None;
        for (&id, parked) in &mut self.parked {
            if parked.doc.path() == Some(path) {
                let n = parked
                    .doc
                    .replace_all(query, replacement, options, now_ms);
                let len = parked.doc.len_bytes();
                parked.caret = parked.caret.min(len);
                parked.anchor = parked.anchor.min(len);
                found = Some((id, n));
                break;
            }
        }
        if let Some((id, n)) = found {
            if n > 0 {
                if let Some(m) = self.meta.get_mut(&id) {
                    m.dirty = true;
                }
                self.graduate_preview(id);
            }
            Some(n)
        } else {
            None
        }
    }

    /// Snapshot in-memory bytes of dirty parked buffers (bounded by `max_bytes`) so
    /// Live Grep and Live Preview reflect unsaved edits.
    pub fn dirty_parked_snapshots(&mut self, max_bytes: u64) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        for parked in self.parked.values_mut() {
            if !parked.doc.is_dirty() {
                continue;
            }
            let Some(path) = parked.doc.path().map(Path::to_path_buf) else {
                continue;
            };
            let len = parked.doc.len_bytes();
            if len == 0 || len > max_bytes {
                continue;
            }
            let mut buf = Vec::with_capacity(len as usize);
            parked.doc.tree_mut().read_range(0, len as usize, &mut buf);
            out.push((path, buf));
        }
        out
    }

    pub fn remap_paths(
        &mut self,
        fs: &dyn editor_platform::FileSystem,
        from: &Path,
        to: &Path,
        is_dir: bool,
        syntax_ext_map: &BTreeMap<String, String>,
    ) {
        let remap = |p: &Path| -> Option<PathBuf> {
            if !is_dir {
                (p == from).then(|| to.to_path_buf())
            } else if p.starts_with(from) {
                let rel = p.strip_prefix(from).ok()?;
                Some(if rel.as_os_str().is_empty() {
                    to.to_path_buf()
                } else {
                    to.join(rel)
                })
            } else {
                None
            }
        };
        for parked in self.parked.values_mut() {
            if let Some(new_p) = parked.doc.path().and_then(remap) {
                let old_default = editor_core::Language::from_path_with_overrides(
                    parked.doc.path(),
                    syntax_ext_map,
                );
                let cur_rev = parked.doc.rev();
                parked.doc.set_path(new_p.clone());
                if let Ok(st) = fs.stat(&new_p) {
                    if st.rev.dev == cur_rev.dev
                        && st.rev.ino == cur_rev.ino
                        && st.rev.size == cur_rev.size
                        && st.rev.mtime_ns == cur_rev.mtime_ns
                    {
                        parked.doc.set_rev(st.rev);
                    }
                }
                let new_default = editor_core::Language::from_path_with_overrides(
                    parked.doc.path(),
                    syntax_ext_map,
                );
                if old_default != new_default {
                    parked.doc.set_language(new_default);
                }
            }
        }
        for meta in self.meta.values_mut() {
            if let Some(new_p) = meta.path.as_deref().and_then(remap) {
                meta.file_name = new_p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("Untitled")
                    .to_string();
                meta.path = Some(new_p);
                meta.last_persisted_undo_sig = (0, false, 0);
            }
        }
    }

    pub fn pin_slot(&self, id: BufferId) -> Option<u8> {
        self.pinned
            .iter()
            .position(|&b| b == id)
            .map(|i| (i + 1) as u8)
    }

    pub fn pin_slot_for_path(&self, path: &Path) -> Option<u8> {
        let id = self.find_by_path(path)?;
        self.pin_slot(id)
    }

    pub fn is_dirty_path(&self, path: &Path) -> bool {
        self.find_by_path(path)
            .and_then(|id| self.meta.get(&id))
            .is_some_and(|m| m.dirty)
    }

    pub fn any_dirty(&self) -> bool {
        self.meta.values().any(|m| m.dirty)
    }

    pub fn first_dirty(&self) -> Option<BufferId> {
        self.ordered_ids()
            .into_iter()
            .find(|id| self.meta.get(id).is_some_and(|m| m.dirty))
    }

    pub fn is_pinned(&self, id: BufferId) -> bool {
        self.pinned.contains(&id)
    }

    pub fn is_preview(&self, id: BufferId) -> bool {
        self.preview == Some(id)
    }

    pub fn contains(&self, id: BufferId) -> bool {
        self.meta.contains_key(&id)
    }

    pub fn len(&self) -> usize {
        self.pinned.len() + self.open.len() + usize::from(self.preview.is_some())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// All tabs in visual strip order: `pinned (1..9)` → `open` → `preview`.
    pub fn ordered_ids(&self) -> Vec<BufferId> {
        let mut out = Vec::with_capacity(self.len());
        out.extend_from_slice(&self.pinned);
        out.extend_from_slice(&self.open);
        if let Some(p) = self.preview {
            out.push(p);
        }
        out
    }

    pub fn park(&mut self, id: BufferId, parked: ParkedBuffer) {
        if self.meta.contains_key(&id) {
            self.update_meta(
                id,
                parked.doc.path().map(Path::to_path_buf),
                parked.doc.file_name().to_string(),
                parked.doc.is_dirty(),
            );
            self.parked.insert(id, parked);
        }
    }

    pub fn take_parked(&mut self, id: BufferId) -> Option<ParkedBuffer> {
        self.parked.remove(&id)
    }

    pub fn get_parked(&self, id: BufferId) -> Option<&ParkedBuffer> {
        self.parked.get(&id)
    }

    pub fn get_parked_mut(&mut self, id: BufferId) -> Option<&mut ParkedBuffer> {
        self.parked.get_mut(&id)
    }

    /// Record `id` as the most recently used buffer (for `⌃Tab` / `⌃^` ping-pong).
    pub fn touch_mru(&mut self, id: BufferId) {
        if !self.meta.contains_key(&id) {
            return;
        }
        self.mru.retain(|&b| b != id);
        self.mru.insert(0, id);
    }

    /// Return the second-most-recently-used buffer (the target for `⌃Tab` ping-pong).
    pub fn previous_mru(&self, current: BufferId) -> Option<BufferId> {
        self.mru
            .iter()
            .copied()
            .find(|&id| id != current && self.meta.contains_key(&id))
    }

    /// Next or previous buffer along the visual Working-Set Strip (wrapping cleanly).
    pub fn step_strip(&self, current: BufferId, delta: isize) -> Option<BufferId> {
        let ids = self.ordered_ids();
        if ids.len() <= 1 {
            return None;
        }
        let pos = ids.iter().position(|&b| b == current).unwrap_or(0) as isize;
        let len = ids.len() as isize;
        let next = (pos + delta).rem_euclid(len) as usize;
        Some(ids[next])
    }

    /// Place a freshly created buffer into `open` or `preview` (returning any evicted preview id).
    pub fn register_buffer(&mut self, id: BufferId, mode: OpenMode) -> Option<BufferId> {
        if self.pinned.contains(&id) || self.open.contains(&id) {
            return None;
        }
        match mode {
            OpenMode::Keep => {
                if self.preview == Some(id) {
                    self.preview = None;
                }
                self.open.push(id);
                None
            }
            OpenMode::Preview => {
                if self.preview == Some(id) {
                    return None;
                }
                let evicted = self.preview.replace(id);
                if let Some(old) = evicted {
                    // Caller checks if old is visible in another split pane before removing meta.
                    return Some(old);
                }
                None
            }
        }
    }

    /// Promote the preview tab (if it matches `id`) into a Kept open tab.
    pub fn graduate_preview(&mut self, id: BufferId) -> bool {
        if self.preview == Some(id) {
            self.preview = None;
            if !self.pinned.contains(&id) && !self.open.contains(&id) {
                self.open.push(id);
            }
            return true;
        }
        false
    }

    /// Toggle pin status for `id`. Returns `Ok(Some(slot))` if newly pinned,
    /// `Ok(None)` if unpinned, or `Err(())` if already at [`MAX_PINS`].
    pub fn toggle_pin(&mut self, id: BufferId) -> Result<Option<u8>, ()> {
        if let Some(pos) = self.pinned.iter().position(|&b| b == id) {
            self.pinned.remove(pos);
            if !self.open.contains(&id) {
                self.open.insert(0, id);
            }
            return Ok(None);
        }
        if self.pinned.len() >= MAX_PINS {
            return Err(());
        }
        self.open.retain(|&b| b != id);
        if self.preview == Some(id) {
            self.preview = None;
        }
        self.pinned.push(id);
        Ok(Some(self.pinned.len() as u8))
    }

    /// Pin `id` at 0-based `index` (or move an existing pin to `index`). Returns `false` if full.
    pub fn move_to_pinned(&mut self, id: BufferId, index: usize) -> bool {
        if !self.meta.contains_key(&id) {
            return false;
        }
        if let Some(cur) = self.pinned.iter().position(|&b| b == id) {
            self.pinned.remove(cur);
            let dst = index.min(self.pinned.len());
            self.pinned.insert(dst, id);
            return true;
        }
        if self.pinned.len() >= MAX_PINS {
            return false;
        }
        self.open.retain(|&b| b != id);
        if self.preview == Some(id) {
            self.preview = None;
        }
        let dst = index.min(self.pinned.len());
        self.pinned.insert(dst, id);
        true
    }

    /// Move `id` into the unpinned `open` section at 0-based `index` (unpinning or graduating).
    pub fn move_to_open(&mut self, id: BufferId, index: usize) -> bool {
        if !self.meta.contains_key(&id) {
            return false;
        }
        self.pinned.retain(|&b| b != id);
        if self.preview == Some(id) {
            self.preview = None;
        }
        self.open.retain(|&b| b != id);
        let dst = index.min(self.open.len());
        self.open.insert(dst, id);
        true
    }

    /// Swap a pinned slot up (`delta = -1`) or down (`delta = 1`) for `⌘E` pin reordering.
    pub fn nudge_pin(&mut self, slot_0based: usize, delta: isize) -> bool {
        if slot_0based >= self.pinned.len() {
            return false;
        }
        let target = (slot_0based as isize + delta)
            .clamp(0, self.pinned.len().saturating_sub(1) as isize) as usize;
        if target == slot_0based {
            return false;
        }
        self.pinned.swap(slot_0based, target);
        true
    }

    /// Remove `id` completely from the working set (meta, parked, pinned, open, preview, mru).
    pub fn remove_buffer(&mut self, id: BufferId) {
        self.pinned.retain(|&b| b != id);
        self.open.retain(|&b| b != id);
        if self.preview == Some(id) {
            self.preview = None;
        }
        self.mru.retain(|&b| b != id);
        self.parked.remove(&id);
        self.meta.remove(&id);
    }

    /// Enforce an LRU cap on unpinned Kept (`open`) tabs.
    ///
    /// Evicts least-recently-used clean tabs that are NOT in `protected_ids` (active or visible
    /// split panes), NEVER evicting pinned (`1..9`) or dirty (unsaved) buffers.
    /// Returns each evicted buffer's `(id, path, parked_buffer)` so the app can retain
    /// RAM-only cursor/scroll recall for the current session.
    pub fn evict_lru_open(
        &mut self,
        max_open: usize,
        protected_ids: &[BufferId],
    ) -> Vec<(BufferId, Option<PathBuf>, Option<ParkedBuffer>)> {
        let mut evicted = Vec::new();
        if max_open == 0 || self.open.len() <= max_open {
            return evicted;
        }
        while self.open.len() > max_open {
            // Find the least-recently-used evictable unpinned clean open buffer.
            let candidate = self
                .open
                .iter()
                .copied()
                .filter(|id| {
                    !protected_ids.contains(id)
                        && !self.meta.get(id).is_some_and(|m| m.dirty)
                })
                .max_by_key(|id| {
                    self.mru
                        .iter()
                        .position(|&m| m == *id)
                        .unwrap_or(usize::MAX)
                });
            let Some(victim) = candidate else {
                break;
            };
            let path = self.meta.get(&victim).and_then(|m| m.path.clone());
            let parked = self.parked.remove(&victim);
            self.remove_buffer(victim);
            evicted.push((victim, path, parked));
        }
        evicted
    }

    /// Disambiguated short labels for all tabs (e.g. `app/lib.rs` vs `render/lib.rs` on collision).
    fn disambiguated_labels(&self) -> HashMap<BufferId, String> {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for m in self.meta.values() {
            *counts.entry(m.file_name.as_str()).or_insert(0) += 1;
        }
        let mut out = HashMap::with_capacity(self.meta.len());
        for m in self.meta.values() {
            let label = if counts.get(m.file_name.as_str()).copied().unwrap_or(0) > 1 {
                if let Some(parent) = m
                    .path
                    .as_deref()
                    .and_then(|p| p.parent())
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                {
                    format!("{parent}/{}", m.file_name)
                } else {
                    m.file_name.clone()
                }
            } else {
                m.file_name.clone()
            };
            out.insert(m.id, label);
        }
        out
    }

    /// Build display items for [`editor_render::TabStrip`].
    pub fn to_tab_items(
        &self,
        active_id: BufferId,
        other_visible_ids: &[BufferId],
    ) -> Vec<TabDisplayItem> {
        let labels = self.disambiguated_labels();
        let mut out = Vec::with_capacity(self.len());
        for (i, &id) in self.pinned.iter().enumerate() {
            if let Some(m) = self.meta.get(&id) {
                out.push(TabDisplayItem {
                    id,
                    label: labels.get(&id).cloned().unwrap_or_else(|| m.file_name.clone()),
                    pin_slot: Some((i + 1) as u8),
                    dirty: m.dirty,
                    is_preview: false,
                    is_active: id == active_id,
                    in_other_pane: id != active_id && other_visible_ids.contains(&id),
                });
            }
        }
        for &id in &self.open {
            if let Some(m) = self.meta.get(&id) {
                out.push(TabDisplayItem {
                    id,
                    label: labels.get(&id).cloned().unwrap_or_else(|| m.file_name.clone()),
                    pin_slot: None,
                    dirty: m.dirty,
                    is_preview: false,
                    is_active: id == active_id,
                    in_other_pane: id != active_id && other_visible_ids.contains(&id),
                });
            }
        }
        if let Some(id) = self.preview {
            if let Some(m) = self.meta.get(&id) {
                out.push(TabDisplayItem {
                    id,
                    label: labels.get(&id).cloned().unwrap_or_else(|| m.file_name.clone()),
                    pin_slot: None,
                    dirty: m.dirty,
                    is_preview: true,
                    is_active: id == active_id,
                    in_other_pane: id != active_id && other_visible_ids.contains(&id),
                });
            }
        }
        out
    }

    /// Build entries for the `⌘E` [`editor_render::PinsOverlay`].
    pub fn to_pin_entries(
        &self,
        active_id: BufferId,
        other_visible_ids: &[BufferId],
        root: Option<&Path>,
    ) -> Vec<PinEntry> {
        let mut out = Vec::with_capacity(self.len());
        for id in self.ordered_ids() {
            let Some(m) = self.meta.get(&id) else {
                continue;
            };
            let rel_path = match (&m.path, root) {
                (Some(p), Some(r)) => p
                    .strip_prefix(r)
                    .unwrap_or(p)
                    .to_string_lossy()
                    .into_owned(),
                (Some(p), None) => p.to_string_lossy().into_owned(),
                (None, _) => String::new(),
            };
            out.push(PinEntry {
                id,
                name: m.file_name.clone(),
                rel_path,
                pin_slot: self.pin_slot(id),
                dirty: m.dirty,
                is_preview: self.preview == Some(id),
                is_active: id == active_id,
                in_other_pane: id != active_id && other_visible_ids.contains(&id),
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_slot_reuses_and_graduates_on_dirty_or_keep() {
        let mut ws = WorkingSet::new(1);
        let a = ws.alloc_buffer(Some(PathBuf::from("/r/a.rs")), "a.rs".into(), false);
        assert_eq!(ws.register_buffer(a, OpenMode::Preview), None);
        assert_eq!(ws.preview, Some(a));

        let b = ws.alloc_buffer(Some(PathBuf::from("/r/b.rs")), "b.rs".into(), false);
        assert_eq!(ws.register_buffer(b, OpenMode::Preview), Some(a));
        assert_eq!(ws.preview, Some(b));

        // Editing graduates `b` to `open`; saving never removes it!
        ws.set_dirty(b, true);
        assert_eq!(ws.preview, None);
        assert!(ws.open.contains(&b));
        ws.set_dirty(b, false);
        assert!(ws.open.contains(&b));
    }

    #[test]
    fn pinned_slots_enforce_max_9_and_keep_stable_numbers() {
        let mut ws = WorkingSet::new(1);
        for i in 0..9 {
            let id = ws.alloc_buffer(Some(PathBuf::from(format!("/r/f{i}.rs"))), format!("f{i}.rs"), false);
            ws.register_buffer(id, OpenMode::Keep);
            assert_eq!(ws.toggle_pin(id), Ok(Some((i + 1) as u8)));
        }
        let extra = ws.alloc_buffer(Some(PathBuf::from("/r/f10.rs")), "f10.rs".into(), false);
        ws.register_buffer(extra, OpenMode::Keep);
        assert_eq!(ws.toggle_pin(extra), Err(()));

        // Reorder slot 0 down -> swaps with slot 1.
        let p0 = ws.pinned[0];
        let p1 = ws.pinned[1];
        assert!(ws.nudge_pin(0, 1));
        assert_eq!(ws.pinned[0], p1);
        assert_eq!(ws.pinned[1], p0);
    }

    #[test]
    fn disambiguates_duplicate_filenames() {
        let mut ws = WorkingSet::new(1);
        let a = ws.alloc_buffer(Some(PathBuf::from("/r/app/lib.rs")), "lib.rs".into(), false);
        let b = ws.alloc_buffer(Some(PathBuf::from("/r/render/lib.rs")), "lib.rs".into(), false);
        ws.register_buffer(a, OpenMode::Keep);
        ws.register_buffer(b, OpenMode::Keep);
        let items = ws.to_tab_items(a, &[]);
        assert!(items.iter().any(|it| it.id == a && it.label == "app/lib.rs"));
        assert!(items.iter().any(|it| it.id == b && it.label == "render/lib.rs"));
    }

    #[test]
    fn evicts_oldest_clean_unpinned_tab_never_pins_dirty_or_visible() {
        let mut ws = WorkingSet::new(1);
        ws.remove_buffer(1);

        let p = ws.alloc_buffer(Some(PathBuf::from("/r/pin.rs")), "pin.rs".into(), false);
        ws.register_buffer(p, OpenMode::Keep);
        ws.toggle_pin(p).unwrap();

        let d = ws.alloc_buffer(Some(PathBuf::from("/r/dirty.rs")), "dirty.rs".into(), true);
        ws.register_buffer(d, OpenMode::Keep);
        ws.touch_mru(d);

        let a = ws.alloc_buffer(Some(PathBuf::from("/r/a.rs")), "a.rs".into(), false);
        ws.register_buffer(a, OpenMode::Keep);
        ws.touch_mru(a);

        let b = ws.alloc_buffer(Some(PathBuf::from("/r/b.rs")), "b.rs".into(), false);
        ws.register_buffer(b, OpenMode::Keep);
        ws.touch_mru(b);

        let c = ws.alloc_buffer(Some(PathBuf::from("/r/c.rs")), "c.rs".into(), false);
        ws.register_buffer(c, OpenMode::Keep);
        ws.touch_mru(c);

        // Cap open tabs to 2 while `c` (active) and `a` (visible in split) are protected!
        // `d` is dirty so it must NOT be evicted; `b` is the only clean non-visible candidate -> evicted!
        let evicted = ws.evict_lru_open(2, &[c, a]);
        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0].0, b);
        assert!(ws.contains(p), "pinned buffer must never be evicted");
        assert!(ws.contains(d), "dirty buffer must never be evicted");
        assert!(ws.contains(a), "split-visible buffer must never be evicted");
        assert!(ws.contains(c), "active buffer must never be evicted");
        assert!(!ws.contains(b));
    }
}
