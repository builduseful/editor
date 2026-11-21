//! Pure, allocation-free pane grid model (up to a 2 × 2 grid).
//!
//! The grid is at most **2 columns**, and each column holds at most **2 rows**, for a
//! hard maximum of [`MAX_PANES`] panes. This covers every useful layout — single,
//! side-by-side, stacked, 3-up (one column split), and the full 2 × 2 grid — while
//! staying trivially predictable for users, agents and tests.
//!
//! Everything here is deterministic geometry with no I/O, no documents and no
//! allocation: `EditorApp` owns the documents and asks this module *where* things go.
//! Drag-and-drop previews are computed by simulating the drop on a copy of the
//! arrangement and laying it out, so the preview is always exactly the rectangle the
//! pane will occupy after the drop — the overlay can never lie about what will happen.

use editor_platform::PixelRect;

/// Hard upper bound on simultaneously visible panes (2 columns × 2 rows).
pub const MAX_PANES: usize = 4;

/// Stable identity of a pane for its whole lifetime (survives moves, swaps and focus changes).
pub type PaneId = u32;

/// Grid position of a pane: column `0..=1`, row `0..=1` within that column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct PaneSlot {
    pub col: u8,
    pub row: u8,
}

impl PaneSlot {
    pub const fn new(col: u8, row: u8) -> Self {
        Self { col, row }
    }
}

/// Where a drag is released relative to the pane underneath the pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DropZone {
    /// Replace the pane's file (file drags) or swap places (pane drags).
    Center,
    Left,
    Right,
    Top,
    Bottom,
}

impl DropZone {
    pub fn as_str(self) -> &'static str {
        match self {
            DropZone::Center => "center",
            DropZone::Left => "left",
            DropZone::Right => "right",
            DropZone::Top => "top",
            DropZone::Bottom => "bottom",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "center" | "centre" | "replace" | "here" => Some(DropZone::Center),
            "left" => Some(DropZone::Left),
            "right" => Some(DropZone::Right),
            "top" | "up" | "above" => Some(DropZone::Top),
            "bottom" | "down" | "below" => Some(DropZone::Bottom),
            _ => None,
        }
    }
}

/// Spatial direction for keyboard focus moves (`⌥⌘←↑↓→`, `⌃W h/j/k/l`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// Resolved rectangles for one visible pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PaneRect {
    pub id: PaneId,
    pub slot: PaneSlot,
    /// Header + body.
    pub frame: PixelRect,
    /// Quiet 24pt file-name header (zero-height when only one pane is visible).
    pub header: PixelRect,
    /// Document viewport area.
    pub body: PixelRect,
}

/// Which split a divider controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DividerAxis {
    /// Vertical hairline between the two columns (drag left/right).
    #[default]
    Columns,
    /// Horizontal hairline between the two rows of column `n` (drag up/down).
    Rows(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Divider {
    pub axis: DividerAxis,
    /// The visible 1px hairline.
    pub rect: PixelRect,
    /// The area this divider splits (used to convert pointer position into a ratio).
    pub span: PixelRect,
}

/// Fixed-capacity (`Copy`, zero-allocation) resolved layout of the whole pane area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GridLayout {
    pub area: PixelRect,
    rects: [PaneRect; MAX_PANES],
    rect_count: u8,
    dividers: [Divider; 3],
    divider_count: u8,
}

impl GridLayout {
    pub fn panes(&self) -> &[PaneRect] {
        &self.rects[..self.rect_count as usize]
    }

    pub fn dividers(&self) -> &[Divider] {
        &self.dividers[..self.divider_count as usize]
    }

    pub fn get(&self, id: PaneId) -> Option<&PaneRect> {
        self.panes().iter().find(|r| r.id == id)
    }

    /// Pane whose frame (header + body) contains the point.
    pub fn pane_at(&self, x: i32, y: i32) -> Option<&PaneRect> {
        self.panes().iter().find(|r| r.frame.contains(x, y))
    }

    /// Divider within `grab` pixels of the point (dividers are 1px; the grab band is wider).
    pub fn divider_at(&self, x: i32, y: i32, grab: i32) -> Option<Divider> {
        self.dividers().iter().copied().find(|d| match d.axis {
            DividerAxis::Columns => {
                (x - d.rect.x).abs() <= grab && y >= d.rect.y && y < d.rect.bottom()
            }
            DividerAxis::Rows(_) => {
                (y - d.rect.y).abs() <= grab && x >= d.rect.x && x < d.rect.right()
            }
        })
    }
}

/// The logical arrangement of panes: who lives in which slot, plus split ratios.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Arrangement {
    slots: [(PaneId, PaneSlot); MAX_PANES],
    len: u8,
    /// Width fraction of column 0 when two columns are visible.
    pub col_ratio: f32,
    /// Height fraction of row 0, per column, when that column holds two rows.
    pub row_ratio: [f32; 2],
}

impl Arrangement {
    pub fn new(first: PaneId) -> Self {
        let mut slots = [(0, PaneSlot::default()); MAX_PANES];
        slots[0] = (first, PaneSlot::new(0, 0));
        Self {
            slots,
            len: 1,
            col_ratio: 0.5,
            row_ratio: [0.5, 0.5],
        }
    }

    pub fn len(&self) -> usize {
        self.len as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn is_full(&self) -> bool {
        self.len() >= MAX_PANES
    }

    fn entries(&self) -> &[(PaneId, PaneSlot)] {
        &self.slots[..self.len as usize]
    }

    fn entries_mut(&mut self) -> &mut [(PaneId, PaneSlot)] {
        let n = self.len as usize;
        &mut self.slots[..n]
    }

    pub fn contains(&self, id: PaneId) -> bool {
        self.entries().iter().any(|(i, _)| *i == id)
    }

    pub fn slot_of(&self, id: PaneId) -> Option<PaneSlot> {
        self.entries().iter().find(|(i, _)| *i == id).map(|(_, s)| *s)
    }

    pub fn id_at(&self, slot: PaneSlot) -> Option<PaneId> {
        self.entries().iter().find(|(_, s)| *s == slot).map(|(i, _)| *i)
    }

    pub fn cols(&self) -> u8 {
        if self.entries().iter().any(|(_, s)| s.col == 1) {
            2
        } else {
            1
        }
    }

    pub fn rows_in(&self, col: u8) -> u8 {
        self.entries().iter().filter(|(_, s)| s.col == col).count() as u8
    }

    /// Pane ids in reading order (row-major: top-left, top-right, bottom-left, bottom-right).
    /// This is the stable public index used by `editor status`, `focus-pane N` and drops.
    pub fn ordered(&self) -> ([PaneId; MAX_PANES], usize) {
        let mut tmp = self.slots;
        let n = self.len as usize;
        tmp[..n].sort_by_key(|(_, s)| (s.row, s.col));
        let mut out = [0; MAX_PANES];
        for (o, (id, _)) in out.iter_mut().zip(tmp[..n].iter()) {
            *o = *id;
        }
        (out, n)
    }

    pub fn index_of(&self, id: PaneId) -> Option<usize> {
        let (ids, n) = self.ordered();
        ids[..n].iter().position(|i| *i == id)
    }

    pub fn id_at_index(&self, idx: usize) -> Option<PaneId> {
        let (ids, n) = self.ordered();
        (idx < n).then(|| ids[idx])
    }

    /// Whether a new pane can be created at `zone` of `target` without breaking the grid.
    pub fn can_insert(&self, target: PaneId, zone: DropZone) -> bool {
        if self.is_full() {
            return false;
        }
        let Some(slot) = self.slot_of(target) else {
            return false;
        };
        match zone {
            DropZone::Center => false,
            DropZone::Left | DropZone::Right => self.cols() == 1,
            DropZone::Top | DropZone::Bottom => self.rows_in(slot.col) == 1,
        }
    }

    /// Insert `new_id` beside `target`. Returns `false` (unchanged) if not possible.
    pub fn insert(&mut self, target: PaneId, zone: DropZone, new_id: PaneId) -> bool {
        if !self.can_insert(target, zone) || self.contains(new_id) {
            return false;
        }
        let slot = self.slot_of(target).unwrap_or_default();
        let new_slot = match zone {
            DropZone::Left => {
                for (_, s) in self.entries_mut() {
                    s.col = 1;
                }
                self.row_ratio = [0.5, self.row_ratio[0]];
                self.col_ratio = 0.5;
                PaneSlot::new(0, 0)
            }
            DropZone::Right => {
                self.row_ratio[1] = 0.5;
                self.col_ratio = 0.5;
                PaneSlot::new(1, 0)
            }
            DropZone::Top => {
                for (_, s) in self.entries_mut() {
                    if s.col == slot.col {
                        s.row = 1;
                    }
                }
                self.row_ratio[slot.col as usize] = 0.5;
                PaneSlot::new(slot.col, 0)
            }
            DropZone::Bottom => {
                self.row_ratio[slot.col as usize] = 0.5;
                PaneSlot::new(slot.col, 1)
            }
            DropZone::Center => return false,
        };
        self.slots[self.len as usize] = (new_id, new_slot);
        self.len += 1;
        true
    }

    /// Remove a pane, collapsing empty rows/columns so the grid stays dense.
    pub fn remove(&mut self, id: PaneId) -> bool {
        if self.len <= 1 {
            return false;
        }
        let Some(pos) = self.entries().iter().position(|(i, _)| *i == id) else {
            return false;
        };
        let n = self.len as usize;
        self.slots.copy_within(pos + 1..n, pos);
        self.len -= 1;
        self.normalize();
        true
    }

    fn normalize(&mut self) {
        // Collapse an empty column 0 by shifting column 1 left.
        if self.rows_in(0) == 0 && self.rows_in(1) > 0 {
            for (_, s) in self.entries_mut() {
                s.col = 0;
            }
            self.row_ratio[0] = self.row_ratio[1];
        }
        // Collapse an empty row 0 inside each column.
        for col in 0..2u8 {
            let has_top = self.entries().iter().any(|(_, s)| s.col == col && s.row == 0);
            if !has_top {
                for (_, s) in self.entries_mut() {
                    if s.col == col {
                        s.row = 0;
                    }
                }
            }
        }
    }

    /// Exchange the grid positions of two panes.
    pub fn swap(&mut self, a: PaneId, b: PaneId) -> bool {
        if a == b {
            return false;
        }
        let (Some(sa), Some(sb)) = (self.slot_of(a), self.slot_of(b)) else {
            return false;
        };
        for (i, s) in self.entries_mut() {
            if *i == a {
                *s = sb;
            } else if *i == b {
                *s = sa;
            }
        }
        true
    }

    /// Move pane `src` next to (or onto, for `Center` = swap) pane `target`.
    /// Atomic: returns `false` and leaves the arrangement untouched if the move is illegal.
    pub fn move_pane(&mut self, src: PaneId, target: PaneId, zone: DropZone) -> bool {
        if src == target || !self.contains(src) || !self.contains(target) {
            return false;
        }
        if zone == DropZone::Center {
            return self.swap(src, target);
        }
        let mut sim = *self;
        if !sim.remove(src) || !sim.insert(target, zone, src) {
            return false;
        }
        *self = sim;
        true
    }

    /// Neighbouring pane of `from` in `dir`, chosen geometrically from a resolved layout
    /// (works for any mix of row/column splits).
    pub fn neighbor(&self, layout: &GridLayout, from: PaneId, dir: Direction) -> Option<PaneId> {
        let src = layout.get(from)?.frame;
        let (cx, cy) = (src.x + src.w / 2, src.y + src.h / 2);
        let mut best: Option<(i64, PaneId)> = None;
        for r in layout.panes() {
            if r.id == from {
                continue;
            }
            let f = r.frame;
            let (ok, overlap) = match dir {
                Direction::Left => (f.right() <= src.x, overlap_1d(f.y, f.bottom(), src.y, src.bottom())),
                Direction::Right => (f.x >= src.right(), overlap_1d(f.y, f.bottom(), src.y, src.bottom())),
                Direction::Up => (f.bottom() <= src.y, overlap_1d(f.x, f.right(), src.x, src.right())),
                Direction::Down => (f.y >= src.bottom(), overlap_1d(f.x, f.right(), src.x, src.right())),
            };
            if !ok || overlap <= 0 {
                continue;
            }
            let (ox, oy) = (f.x + f.w / 2, f.y + f.h / 2);
            let dist = ((ox - cx) as i64).pow(2) + ((oy - cy) as i64).pow(2);
            if best.map_or(true, |(d, _)| dist < d) {
                best = Some((dist, r.id));
            }
        }
        best.map(|(_, id)| id)
    }

    /// Resolve every pane rectangle and divider for `area` (zero allocation).
    pub fn layout(&self, area: PixelRect, scale: f32) -> GridLayout {
        let mut out = GridLayout {
            area,
            ..GridLayout::default()
        };
        if self.is_empty() || area.is_empty() {
            return out;
        }
        let hair = scale.round().max(1.0) as i32;
        let header_h = if self.len > 1 { (22.0 * scale).round() as i32 } else { 0 };
        let min_w = (120.0 * scale) as i32;
        let min_h = (72.0 * scale) as i32 + header_h;

        let mut cols = [area, PixelRect::ZERO];
        if self.cols() == 2 {
            let left_w = split_len(area.w, self.col_ratio, min_w, hair);
            cols[0] = PixelRect::new(area.x, area.y, left_w, area.h);
            let div = PixelRect::new(area.x + left_w, area.y, hair, area.h);
            cols[1] = PixelRect::new(div.right(), area.y, (area.right() - div.right()).max(0), area.h);
            out.dividers[out.divider_count as usize] = Divider {
                axis: DividerAxis::Columns,
                rect: div,
                span: area,
            };
            out.divider_count += 1;
        }

        for col in 0..self.cols() {
            let c = cols[col as usize];
            let mut rows = [c, PixelRect::ZERO];
            if self.rows_in(col) == 2 {
                let top_h = split_len(c.h, self.row_ratio[col as usize], min_h, hair);
                rows[0] = PixelRect::new(c.x, c.y, c.w, top_h);
                let div = PixelRect::new(c.x, c.y + top_h, c.w, hair);
                rows[1] = PixelRect::new(c.x, div.bottom(), c.w, (c.bottom() - div.bottom()).max(0));
                out.dividers[out.divider_count as usize] = Divider {
                    axis: DividerAxis::Rows(col),
                    rect: div,
                    span: c,
                };
                out.divider_count += 1;
            }
            for row in 0..self.rows_in(col) {
                let slot = PaneSlot::new(col, row);
                let Some(id) = self.id_at(slot) else { continue };
                let frame = rows[row as usize];
                let hh = header_h.min(frame.h);
                out.rects[out.rect_count as usize] = PaneRect {
                    id,
                    slot,
                    frame,
                    header: PixelRect::new(frame.x, frame.y, frame.w, hh),
                    body: PixelRect::new(frame.x, frame.y + hh, frame.w, frame.h - hh),
                };
                out.rect_count += 1;
            }
        }
        out
    }

    /// Resolve a single fullscreen-zoomed pane covering the entire `area` with zero pane header.
    pub fn layout_zoomed(&self, area: PixelRect, active_id: PaneId) -> GridLayout {
        let mut out = GridLayout {
            area,
            ..GridLayout::default()
        };
        if area.is_empty() {
            return out;
        }
        let slot = self.slot_of(active_id).unwrap_or_default();
        out.rects[0] = PaneRect {
            id: active_id,
            slot,
            frame: area,
            header: PixelRect::ZERO,
            body: area,
        };
        out.rect_count = 1;
        out
    }

    /// Update a split ratio from a pointer position while dragging a divider.
    pub fn drag_divider(&mut self, div: &Divider, x: f32, y: f32) {
        match div.axis {
            DividerAxis::Columns if div.span.w > 0 => {
                self.col_ratio = ((x - div.span.x as f32) / div.span.w as f32).clamp(0.15, 0.85);
            }
            DividerAxis::Rows(col) if div.span.h > 0 => {
                self.row_ratio[col.min(1) as usize] =
                    ((y - div.span.y as f32) / div.span.h as f32).clamp(0.15, 0.85);
            }
            _ => {}
        }
    }
}

/// What a drag would do if released right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DropPlan {
    pub target: PaneId,
    /// Effective zone after legality fallback (an impossible edge split becomes `Center`).
    pub zone: DropZone,
    /// Exact rectangle the dropped content will occupy after the drop.
    pub preview: PixelRect,
}

/// Raw zone under the pointer: outer 30% bands are edges, the middle is `Center`.
pub fn zone_in(frame: PixelRect, x: i32, y: i32) -> DropZone {
    if frame.w <= 0 || frame.h <= 0 {
        return DropZone::Center;
    }
    let fx = (x - frame.x) as f32 / frame.w as f32;
    let fy = (y - frame.y) as f32 / frame.h as f32;
    let candidates = [
        (fx, DropZone::Left),
        (1.0 - fx, DropZone::Right),
        (fy, DropZone::Top),
        (1.0 - fy, DropZone::Bottom),
    ];
    let (d, z) = candidates
        .iter()
        .copied()
        .fold((f32::MAX, DropZone::Center), |acc, c| if c.0 < acc.0 { c } else { acc });
    if d < 0.30 {
        z
    } else {
        DropZone::Center
    }
}

/// Sentinel id used when simulating a brand-new pane during preview.
const PREVIEW_ID: PaneId = PaneId::MAX;

/// Compute what releasing a drag at `(x, y)` would do.
///
/// - `moving`: `Some(id)` when dragging an existing pane by its header, `None` for files.
/// - `zone_override`: force a zone (used by the CLI `drop` command); `None` = from pointer.
pub fn plan_drop(
    arr: &Arrangement,
    layout: &GridLayout,
    target: PaneId,
    zone: DropZone,
    moving: Option<PaneId>,
    scale: f32,
) -> Option<DropPlan> {
    let target_rect = layout.get(target)?;
    let center = |t: PaneId| -> Option<DropPlan> {
        if moving == Some(t) {
            return None;
        }
        Some(DropPlan {
            target: t,
            zone: DropZone::Center,
            preview: target_rect.frame,
        })
    };
    if zone == DropZone::Center {
        return center(target);
    }
    let mut sim = *arr;
    let new_id = match moving {
        Some(src) => {
            if !sim.move_pane(src, target, zone) {
                return center(target);
            }
            src
        }
        None => {
            if !sim.insert(target, zone, PREVIEW_ID) {
                return center(target);
            }
            PREVIEW_ID
        }
    };
    let preview = sim.layout(layout.area, scale).get(new_id)?.frame;
    Some(DropPlan {
        target,
        zone,
        preview,
    })
}

/// Pointer-driven variant of [`plan_drop`].
pub fn plan_drop_at(
    arr: &Arrangement,
    layout: &GridLayout,
    x: i32,
    y: i32,
    moving: Option<PaneId>,
    scale: f32,
) -> Option<DropPlan> {
    let pane = layout.pane_at(x, y)?;
    let zone = zone_in(pane.frame, x, y);
    plan_drop(arr, layout, pane.id, zone, moving, scale)
}

fn split_len(total: i32, ratio: f32, min: i32, hair: i32) -> i32 {
    let lo = min.min((total - hair) / 2).max(0);
    let hi = (total - hair - lo).max(lo);
    ((total as f32 * ratio).round() as i32).clamp(lo, hi)
}

fn overlap_1d(a0: i32, a1: i32, b0: i32, b1: i32) -> i32 {
    a1.min(b1) - a0.max(b0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: PixelRect = PixelRect {
        x: 0,
        y: 0,
        w: 2000,
        h: 1200,
    };

    #[test]
    fn grows_to_a_2x2_grid_and_refuses_a_fifth_pane() {
        let mut a = Arrangement::new(1);
        assert!(a.insert(1, DropZone::Right, 2));
        assert!(a.insert(1, DropZone::Bottom, 3));
        assert!(a.insert(2, DropZone::Bottom, 4));
        assert_eq!(a.len(), 4);
        assert_eq!(a.slot_of(1), Some(PaneSlot::new(0, 0)));
        assert_eq!(a.slot_of(2), Some(PaneSlot::new(1, 0)));
        assert_eq!(a.slot_of(3), Some(PaneSlot::new(0, 1)));
        assert_eq!(a.slot_of(4), Some(PaneSlot::new(1, 1)));
        for z in [DropZone::Left, DropZone::Right, DropZone::Top, DropZone::Bottom] {
            assert!(!a.can_insert(1, z));
        }
        // Reading order: TL, TR, BL, BR.
        let (ids, n) = a.ordered();
        assert_eq!(&ids[..n], &[1, 2, 3, 4]);
        let l = a.layout(AREA, 2.0);
        assert_eq!(l.panes().len(), 4);
        assert_eq!(l.dividers().len(), 3);
        // No overlaps, and every pane has a header in multi-pane mode.
        for p in l.panes() {
            assert!(p.header.h > 0);
            for q in l.panes() {
                if p.id != q.id {
                    assert!(p.frame.intersect(q.frame).is_empty());
                }
            }
        }
    }

    #[test]
    fn left_and_top_inserts_shift_existing_panes() {
        let mut a = Arrangement::new(1);
        assert!(a.insert(1, DropZone::Left, 2));
        assert_eq!(a.slot_of(2), Some(PaneSlot::new(0, 0)));
        assert_eq!(a.slot_of(1), Some(PaneSlot::new(1, 0)));
        assert!(a.insert(1, DropZone::Top, 3));
        assert_eq!(a.slot_of(3), Some(PaneSlot::new(1, 0)));
        assert_eq!(a.slot_of(1), Some(PaneSlot::new(1, 1)));
        // Second column already exists: no third column.
        assert!(!a.can_insert(2, DropZone::Right));
        assert!(a.can_insert(2, DropZone::Bottom));
    }

    #[test]
    fn remove_collapses_columns_and_rows() {
        let mut a = Arrangement::new(1);
        a.insert(1, DropZone::Right, 2);
        a.insert(2, DropZone::Bottom, 3);
        assert!(a.remove(1));
        assert_eq!(a.cols(), 1);
        assert_eq!(a.slot_of(2), Some(PaneSlot::new(0, 0)));
        assert_eq!(a.slot_of(3), Some(PaneSlot::new(0, 1)));
        assert!(a.remove(2));
        assert_eq!(a.slot_of(3), Some(PaneSlot::new(0, 0)));
        assert!(!a.remove(3), "last pane can never be removed");
    }

    #[test]
    fn move_pane_is_atomic_and_swaps_on_center() {
        let mut a = Arrangement::new(1);
        a.insert(1, DropZone::Right, 2);
        let before = a;
        assert!(!a.move_pane(1, 1, DropZone::Left));
        assert_eq!(a, before);
        assert!(a.move_pane(1, 2, DropZone::Center));
        assert_eq!(a.slot_of(1), Some(PaneSlot::new(1, 0)));
        // Move pane 1 below pane 2 → single column, stacked.
        assert!(a.move_pane(1, 2, DropZone::Bottom));
        assert_eq!(a.cols(), 1);
        assert_eq!(a.slot_of(2), Some(PaneSlot::new(0, 0)));
        assert_eq!(a.slot_of(1), Some(PaneSlot::new(0, 1)));
    }

    #[test]
    fn drop_preview_matches_post_drop_layout_exactly() {
        let mut a = Arrangement::new(1);
        let l = a.layout(AREA, 2.0);
        // Pointer near the right edge of the only pane.
        let plan = plan_drop_at(&a, &l, 1900, 600, None, 2.0).unwrap();
        assert_eq!(plan.zone, DropZone::Right);
        assert!(a.insert(1, DropZone::Right, 7));
        let after = a.layout(AREA, 2.0);
        assert_eq!(plan.preview, after.get(7).unwrap().frame);
        // Middle of a pane → replace in place.
        let l2 = a.layout(AREA, 2.0);
        let p2 = plan_drop_at(&a, &l2, 500, 600, None, 2.0).unwrap();
        assert_eq!(p2.zone, DropZone::Center);
        assert_eq!(p2.preview, l2.get(1).unwrap().frame);
        // Illegal edge (third column) falls back to Center.
        let p3 = plan_drop_at(&a, &l2, 20, 600, None, 2.0).unwrap();
        assert_eq!(p3.zone, DropZone::Center);
        // Dragging a pane onto itself does nothing.
        assert!(plan_drop_at(&a, &l2, 500, 600, Some(1), 2.0).is_none());
    }

    #[test]
    fn geometric_neighbors_and_divider_drag() {
        let mut a = Arrangement::new(1);
        a.insert(1, DropZone::Right, 2);
        a.insert(1, DropZone::Bottom, 3);
        let l = a.layout(AREA, 2.0);
        assert_eq!(a.neighbor(&l, 1, Direction::Right), Some(2));
        assert_eq!(a.neighbor(&l, 1, Direction::Down), Some(3));
        assert_eq!(a.neighbor(&l, 3, Direction::Right), Some(2));
        assert_eq!(a.neighbor(&l, 2, Direction::Left).is_some(), true);
        assert_eq!(a.neighbor(&l, 1, Direction::Up), None);
        let div = l.dividers()[0];
        a.drag_divider(&div, 1500.0, 0.0);
        assert!((a.col_ratio - 0.75).abs() < 1e-3);
        assert_eq!(zone_in(PixelRect::new(0, 0, 100, 100), 50, 50), DropZone::Center);
        assert_eq!(zone_in(PixelRect::new(0, 0, 100, 100), 50, 95), DropZone::Bottom);
    }
}
