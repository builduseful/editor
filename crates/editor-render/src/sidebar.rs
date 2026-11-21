//! Custom-drawn, lazy recursive file sidebar (zero `NSOutlineView`).
//!
//! Architecture & UX invariants:
//! - Lazy by construction: directories are read ONLY when expanded or navigated into.
//! - Selection is held by `PathBuf` (never by pointer), surviving directory refreshes.
//! - Two gestures on directory rows:
//!     1. Left chevron -> expands/collapses inline in the tree
//!     2. Right-edge enter chevron -> navigates into that folder as the current view
//! - Breadcrumb strip at top lets the user jump back to any ancestor folder.
//! - Drag-resizable right hairline clamped to `160..=480` pt.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use editor_platform::{DirEntry, EntryKind, FileSystem, FontStyle, GlyphRasterizer, Key, PixelRect};

use crate::atlas::GlyphAtlas;
use crate::painter::Painter;
use crate::theme::{px, Theme};

pub const SIDEBAR_MIN_WIDTH_PT: f32 = 160.0;
pub const SIDEBAR_MAX_WIDTH_PT: f32 = 480.0;
pub const SIDEBAR_DEFAULT_WIDTH_PT: f32 = 220.0;
/// Window logical width (pt) below which the sidebar automatically minimizes so the editor stays usable.
pub const SIDEBAR_AUTO_MINIMIZE_WIDTH_PT: f32 = 580.0;

/// Docked edge for the file navigation sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SidebarSide {
    #[default]
    Left,
    Right,
}

impl SidebarSide {
    #[inline]
    pub const fn toggle(self) -> Self {
        match self {
            SidebarSide::Left => SidebarSide::Right,
            SidebarSide::Right => SidebarSide::Left,
        }
    }

    #[inline]
    pub const fn as_str(self) -> &'static str {
        match self {
            SidebarSide::Left => "left",
            SidebarSide::Right => "right",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "left" | "l" => Some(SidebarSide::Left),
            "right" | "r" => Some(SidebarSide::Right),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FlatRow {
    pub path: PathBuf,
    pub name: String,
    pub kind: EntryKind,
    pub depth: usize,
    pub expanded: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SidebarBadge {
    pub pin_slot: Option<u8>,
    pub dirty: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarAction {
    None,
    OpenFile(PathBuf),
    Changed,
    Minimize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarHit {
    None,
    ResizeEdge,
    Minimize,
    Breadcrumb(PathBuf),
    RowDisclosure(usize),
    RowEnter(usize),
    RowBody(usize),
}

pub struct Sidebar {
    visible: bool,
    focused: bool,
    side: SidebarSide,
    width_pt: f32,
    /// Current root directory displayed in the sidebar view.
    root: Option<PathBuf>,
    /// Expanded directories (preserved across folder drill-in / drill-out).
    expanded: HashSet<PathBuf>,
    /// Cached immediate children of read directories (lazy!).
    dir_cache: BTreeMap<PathBuf, Vec<DirEntry>>,
    /// Flattened visible rows derived from `root`, `expanded`, and `dir_cache`.
    rows: Vec<FlatRow>,
    /// Currently selected entry held strictly by path.
    selected: Option<PathBuf>,
    /// Hovered row index (for subtle hover & enter-chevron affordance).
    hovered_row: Option<usize>,
    /// Hover state for the header sidebar-minimize button.
    minimize_hovered: bool,
    /// Hover / active drag state for the sidebar resize handle edge.
    edge_hovered: bool,
    edge_active: bool,
    /// Vertical scroll offset in physical pixels.
    scroll_y_px: f32,
    /// Cached breadcrumb rects from last paint for exact hit-testing.
    crumb_rects: Vec<(PixelRect, PathBuf)>,
    /// Cached header minimize button rect from last paint for exact hit-testing.
    minimize_rect: PixelRect,
    header_height_px: i32,
    row_height_px: i32,
}

impl Default for Sidebar {
    fn default() -> Self {
        Self::new()
    }
}

impl Sidebar {
    pub fn new() -> Self {
        Self {
            visible: true,
            focused: false,
            side: SidebarSide::Left,
            width_pt: SIDEBAR_DEFAULT_WIDTH_PT,
            root: None,
            expanded: HashSet::new(),
            dir_cache: BTreeMap::new(),
            rows: Vec::new(),
            selected: None,
            hovered_row: None,
            minimize_hovered: false,
            edge_hovered: false,
            edge_active: false,
            scroll_y_px: 0.0,
            crumb_rects: Vec::new(),
            minimize_rect: PixelRect::ZERO,
            header_height_px: 30,
            row_height_px: 24,
        }
    }

    pub fn side(&self) -> SidebarSide {
        self.side
    }

    pub fn set_side(&mut self, side: SidebarSide) {
        self.side = side;
    }

    pub fn toggle_side(&mut self) -> SidebarSide {
        self.side = self.side.toggle();
        self.side
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
        if !visible {
            self.focused = false;
            self.hovered_row = None;
            self.minimize_hovered = false;
            self.edge_hovered = false;
            self.edge_active = false;
        }
    }

    pub fn set_edge_active(&mut self, active: bool) {
        self.edge_active = active;
    }

    pub fn toggle_visible(&mut self) {
        self.set_visible(!self.visible);
    }

    pub fn is_focused(&self) -> bool {
        self.focused
    }

    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused && self.visible;
    }

    pub fn width_pt(&self) -> f32 {
        if self.visible {
            self.width_pt
        } else {
            0.0
        }
    }

    pub fn set_width_pt(&mut self, width_pt: f32) {
        self.width_pt = width_pt.clamp(SIDEBAR_MIN_WIDTH_PT, SIDEBAR_MAX_WIDTH_PT);
    }

    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    pub fn selected(&self) -> Option<&Path> {
        self.selected.as_deref()
    }

    pub fn rows(&self) -> &[FlatRow] {
        &self.rows
    }

    pub fn is_under_root(&self, path: &Path) -> bool {
        match &self.root {
            Some(r) => path.starts_with(r),
            None => false,
        }
    }

    /// Set the sidebar root directory (reads ONLY the root's immediate children).
    pub fn set_root(&mut self, fs: &dyn FileSystem, dir: &Path) {
        let canonical = fs.canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        self.root = Some(canonical.clone());
        self.ensure_dir_loaded(fs, &canonical);
        self.scroll_y_px = 0.0;
        self.rebuild_rows();
    }

    /// Navigate into a subfolder or ancestor without losing existing tree expansions or selection!
    pub fn navigate_to(&mut self, fs: &dyn FileSystem, dir: &Path) {
        let canonical = fs.canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        self.root = Some(canonical.clone());
        self.ensure_dir_loaded(fs, &canonical);
        self.scroll_y_px = 0.0;
        self.rebuild_rows();
    }

    /// Reveal `file_path` if it lies under the current root (expanding ONLY its ancestors).
    pub fn reveal(&mut self, fs: &dyn FileSystem, file_path: &Path) {
        let canonical = fs
            .canonicalize(file_path)
            .unwrap_or_else(|_| file_path.to_path_buf());
        let Some(root) = self.root.clone() else {
            return;
        };
        if !canonical.starts_with(&root) {
            return;
        }
        let mut cur = canonical.parent();
        while let Some(anc) = cur {
            if anc == root {
                break;
            }
            self.expanded.insert(anc.to_path_buf());
            self.ensure_dir_loaded(fs, anc);
            cur = anc.parent();
        }
        self.ensure_dir_loaded(fs, &root);
        self.selected = Some(canonical);
        self.rebuild_rows();
    }

    pub fn refresh(&mut self, fs: &dyn FileSystem) {
        let keys: Vec<PathBuf> = self.dir_cache.keys().cloned().collect();
        self.dir_cache.clear();
        if let Some(root) = self.root.clone() {
            self.ensure_dir_loaded(fs, &root);
        }
        for k in keys {
            if self.expanded.contains(&k) {
                self.ensure_dir_loaded(fs, &k);
            }
        }
        self.rebuild_rows();
    }

    fn ensure_dir_loaded(&mut self, fs: &dyn FileSystem, dir: &Path) {
        if self.dir_cache.contains_key(dir) {
            return;
        }
        if let Ok(entries) = fs.read_dir(dir) {
            self.dir_cache.insert(dir.to_path_buf(), entries);
        } else {
            self.dir_cache.insert(dir.to_path_buf(), Vec::new());
        }
    }

    fn rebuild_rows(&mut self) {
        self.rows.clear();
        let Some(root) = self.root.clone() else {
            return;
        };
        self.push_children(&root, 0);
    }

    fn push_children(&mut self, dir: &Path, depth: usize) {
        let Some(entries) = self.dir_cache.get(dir).cloned() else {
            return;
        };
        for entry in entries {
            let is_dir = entry.kind == EntryKind::Directory;
            let is_expanded = is_dir && self.expanded.contains(&entry.path);
            self.rows.push(FlatRow {
                path: entry.path.clone(),
                name: entry.name,
                kind: entry.kind,
                depth,
                expanded: is_expanded,
            });
            if is_expanded {
                self.push_children(&entry.path, depth + 1);
            }
        }
    }

    pub fn toggle_expand(&mut self, fs: &dyn FileSystem, path: &Path) {
        if self.expanded.contains(path) {
            self.expanded.remove(path);
        } else {
            self.expanded.insert(path.to_path_buf());
            self.ensure_dir_loaded(fs, path);
        }
        self.rebuild_rows();
    }

    pub fn scroll_by(&mut self, dy_px: f32, viewport_h_px: i32) {
        let content_h = (self.rows.len() as i32) * self.row_height_px;
        let visible_h = (viewport_h_px - self.header_height_px).max(0);
        let max_scroll = (content_h - visible_h).max(0) as f32;
        self.scroll_y_px = (self.scroll_y_px + dy_px).clamp(0.0, max_scroll);
    }

    /// Update hover state from pointer coordinates. Returns `(changed, hit)` so the caller
    /// always invalidates the sidebar on hover entry, row/button change, AND hover exit — while
    /// avoiding redundant repaints when moving within the same row.
    pub fn set_hovered(
        &mut self,
        rect: PixelRect,
        x_px: f32,
        y_px: f32,
        scale: f32,
    ) -> (bool, SidebarHit) {
        let prev_row = self.hovered_row;
        let prev_min = self.minimize_hovered;
        let prev_edge = self.edge_hovered;
        let hit = self.hit_test(rect, x_px, y_px, scale);
        self.hovered_row = match hit {
            SidebarHit::RowBody(i) | SidebarHit::RowDisclosure(i) | SidebarHit::RowEnter(i) => {
                Some(i)
            }
            _ => None,
        };
        self.minimize_hovered = hit == SidebarHit::Minimize;
        self.edge_hovered = hit == SidebarHit::ResizeEdge;
        (
            self.hovered_row != prev_row
                || self.minimize_hovered != prev_min
                || self.edge_hovered != prev_edge,
            hit,
        )
    }

    /// Clear any active row/header/edge hover highlight, returning `true` if visual state changed.
    pub fn clear_hover(&mut self) -> bool {
        let r = self.hovered_row.take().is_some();
        let b = std::mem::replace(&mut self.minimize_hovered, false);
        let e = std::mem::replace(&mut self.edge_hovered, false);
        r || b || e
    }

    pub fn hit_test(&self, rect: PixelRect, x_px: f32, y_px: f32, scale: f32) -> SidebarHit {
        if !self.visible || rect.is_empty() {
            return SidebarHit::None;
        }
        let xi = x_px.round() as i32;
        let yi = y_px.round() as i32;
        let edge_grab = px(6.0, scale);
        let resize_edge_x = match self.side {
            SidebarSide::Left => rect.right(),
            SidebarSide::Right => rect.x,
        };
        if (xi - resize_edge_x).abs() <= edge_grab && yi >= rect.y && yi < rect.bottom() {
            return SidebarHit::ResizeEdge;
        }
        if !rect.contains(xi, yi) {
            return SidebarHit::None;
        }
        // Check header minimize button & breadcrumb buttons.
        if yi < rect.y + self.header_height_px {
            if !self.minimize_rect.is_empty() && self.minimize_rect.contains(xi, yi) {
                return SidebarHit::Minimize;
            }
            for (r, path) in &self.crumb_rects {
                if r.contains(xi, yi) {
                    return SidebarHit::Breadcrumb(path.clone());
                }
            }
            return SidebarHit::None;
        }
        let rel_y = (yi - (rect.y + self.header_height_px)) as f32 + self.scroll_y_px;
        if rel_y < 0.0 || self.row_height_px <= 0 {
            return SidebarHit::None;
        }
        let idx = (rel_y / self.row_height_px as f32).floor() as usize;
        let Some(row) = self.rows.get(idx) else {
            return SidebarHit::None;
        };
        if row.kind == EntryKind::Directory {
            let indent = px(10.0 + row.depth as f32 * 14.0, scale);
            let disclosure_right = rect.x + indent + px(18.0, scale);
            let enter_left = rect.right() - px(24.0, scale);
            if xi <= disclosure_right {
                return SidebarHit::RowDisclosure(idx);
            }
            if xi >= enter_left {
                return SidebarHit::RowEnter(idx);
            }
        }
        SidebarHit::RowBody(idx)
    }

    pub fn handle_click(
        &mut self,
        fs: &dyn FileSystem,
        rect: PixelRect,
        x_px: f32,
        y_px: f32,
        scale: f32,
    ) -> SidebarAction {
        match self.hit_test(rect, x_px, y_px, scale) {
            SidebarHit::Minimize => SidebarAction::Minimize,
            SidebarHit::Breadcrumb(dir) => {
                self.navigate_to(fs, &dir);
                SidebarAction::Changed
            }
            SidebarHit::RowDisclosure(idx) => {
                if let Some(row) = self.rows.get(idx).cloned() {
                    self.selected = Some(row.path.clone());
                    self.toggle_expand(fs, &row.path);
                    return SidebarAction::Changed;
                }
                SidebarAction::None
            }
            SidebarHit::RowEnter(idx) => {
                if let Some(row) = self.rows.get(idx).cloned() {
                    if row.kind == EntryKind::Directory {
                        self.selected = Some(row.path.clone());
                        self.navigate_to(fs, &row.path);
                        return SidebarAction::Changed;
                    }
                }
                SidebarAction::None
            }
            SidebarHit::RowBody(idx) => {
                if let Some(row) = self.rows.get(idx).cloned() {
                    self.selected = Some(row.path.clone());
                    match row.kind {
                        EntryKind::Directory => {
                            self.toggle_expand(fs, &row.path);
                            return SidebarAction::Changed;
                        }
                        EntryKind::File | EntryKind::Symlink => {
                            return SidebarAction::OpenFile(row.path);
                        }
                        EntryKind::Other => return SidebarAction::Changed,
                    }
                }
                SidebarAction::None
            }
            _ => SidebarAction::None,
        }
    }

    pub fn handle_key(
        &mut self,
        fs: &dyn FileSystem,
        key: Key,
        viewport_h_px: i32,
    ) -> SidebarAction {
        if self.rows.is_empty() {
            if key == Key::Left {
                if let Some(parent) = self.root.as_ref().and_then(|r| r.parent()).map(PathBuf::from) {
                    self.navigate_to(fs, &parent);
                    return SidebarAction::Changed;
                }
            }
            return SidebarAction::None;
        }

        let cur_idx = self
            .selected
            .as_ref()
            .and_then(|sel| self.rows.iter().position(|r| &r.path == sel))
            .unwrap_or(0);

        match key {
            Key::Up => {
                let next = cur_idx.saturating_sub(1);
                self.selected = Some(self.rows[next].path.clone());
                self.ensure_row_visible(next, viewport_h_px);
                SidebarAction::Changed
            }
            Key::Down => {
                let next = (cur_idx + 1).min(self.rows.len() - 1);
                self.selected = Some(self.rows[next].path.clone());
                self.ensure_row_visible(next, viewport_h_px);
                SidebarAction::Changed
            }
            Key::Right => {
                let row = self.rows[cur_idx].clone();
                if row.kind == EntryKind::Directory {
                    if !row.expanded {
                        self.toggle_expand(fs, &row.path);
                    } else if cur_idx + 1 < self.rows.len()
                        && self.rows[cur_idx + 1].depth > row.depth
                    {
                        self.selected = Some(self.rows[cur_idx + 1].path.clone());
                        self.ensure_row_visible(cur_idx + 1, viewport_h_px);
                    }
                    return SidebarAction::Changed;
                }
                SidebarAction::None
            }
            Key::Left => {
                let row = self.rows[cur_idx].clone();
                if row.kind == EntryKind::Directory && row.expanded {
                    self.toggle_expand(fs, &row.path);
                    return SidebarAction::Changed;
                }
                if let Some(parent) = row.path.parent() {
                    if let Some(p_idx) = self.rows.iter().position(|r| r.path == parent) {
                        self.selected = Some(self.rows[p_idx].path.clone());
                        self.ensure_row_visible(p_idx, viewport_h_px);
                        return SidebarAction::Changed;
                    }
                }
                if let Some(parent) = self.root.as_ref().and_then(|r| r.parent()).map(PathBuf::from) {
                    self.navigate_to(fs, &parent);
                    return SidebarAction::Changed;
                }
                SidebarAction::None
            }
            Key::Enter => {
                let row = self.rows[cur_idx].clone();
                match row.kind {
                    EntryKind::Directory => {
                        self.toggle_expand(fs, &row.path);
                        SidebarAction::Changed
                    }
                    EntryKind::File | EntryKind::Symlink => SidebarAction::OpenFile(row.path),
                    EntryKind::Other => SidebarAction::None,
                }
            }
            _ => SidebarAction::None,
        }
    }

    fn ensure_row_visible(&mut self, idx: usize, viewport_h_px: i32) {
        let top = (idx as i32 * self.row_height_px) as f32;
        let bot = top + self.row_height_px as f32;
        let visible_h = (viewport_h_px - self.header_height_px).max(1) as f32;
        if top < self.scroll_y_px {
            self.scroll_y_px = top;
        } else if bot > self.scroll_y_px + visible_h {
            self.scroll_y_px = (bot - visible_h).max(0.0);
        }
    }

    pub fn draw(
        &mut self,
        painter: &mut Painter,
        rect: PixelRect,
        theme: &Theme,
        atlas: &mut GlyphAtlas,
        rasterizer: &mut dyn GlyphRasterizer,
    ) {
        self.draw_with_badges(painter, rect, theme, atlas, rasterizer, |_| SidebarBadge::default());
    }

    pub fn draw_with_badges<F>(
        &mut self,
        painter: &mut Painter,
        rect: PixelRect,
        theme: &Theme,
        atlas: &mut GlyphAtlas,
        rasterizer: &mut dyn GlyphRasterizer,
        badge_for: F,
    ) where
        F: Fn(&Path) -> SidebarBadge,
    {
        if !self.visible || rect.is_empty() {
            return;
        }
        let scale = painter.scale();
        self.header_height_px = px(30.0, scale);
        self.row_height_px = px(24.0, scale);

        // Background: color.page, inner-edge hairline + quiet tactile resize handle.
        painter.fill_rect(rect, theme.page);
        let hairline = (1.0 * scale).round().max(1.0) as i32;
        let edge_hi = self.edge_hovered || self.edge_active;
        let sep_w = if edge_hi { (2.0 * scale).round().max(2.0) as i32 } else { hairline };
        let (sep_x, content_x) = match self.side {
            SidebarSide::Left => (rect.right() - sep_w, rect.x),
            SidebarSide::Right => (rect.x, rect.x + hairline),
        };
        let sep_col = if edge_hi {
            theme.border_control
        } else {
            theme.border_subtle
        };
        painter.fill_rect(PixelRect::new(sep_x, rect.y, sep_w, rect.h), sep_col);
        // Subtle centered vertical grip pill on the resize edge (crisper on hover/drag).
        if rect.h > px(64.0, scale) {
            let grip_w = if edge_hi { px(3.0, scale).max(2) } else { px(2.0, scale).max(1) };
            let grip_h = if edge_hi { px(30.0, scale) } else { px(22.0, scale) };
            let grip_x = match self.side {
                SidebarSide::Left => rect.right() - grip_w - px(1.0, scale),
                SidebarSide::Right => rect.x + px(1.0, scale),
            };
            let grip_y = rect.y + (rect.h - grip_h) / 2;
            let grip_col = if edge_hi {
                theme.text_muted
            } else {
                theme.border_control.with_alpha(120)
            };
            painter.fill_rounded_rect(
                PixelRect::new(grip_x, grip_y, grip_w, grip_h),
                grip_w as f32 * 0.5,
                grip_col,
            );
        }

        let content_rect = PixelRect::new(content_x, rect.y, (rect.w - hairline).max(0), rect.h);
        painter.push_clip(content_rect);

        // 1. Breadcrumb header bar + quiet sidebar-minimize toggle icon.
        let header_rect = PixelRect::new(
            content_rect.x,
            content_rect.y,
            content_rect.w,
            self.header_height_px,
        );
        let btn_w = px(24.0, scale);
        let btn_h = (self.header_height_px - hairline - px(6.0, scale)).max(12);
        let btn_y = header_rect.y + ((self.header_height_px - hairline) - btn_h) / 2;
        let pad_r = px(4.0, scale);
        let min_x = header_rect.right() - pad_r - btn_w;
        self.minimize_rect = PixelRect::new(min_x, btn_y, btn_w, btn_h);

        let crumb_clip = PixelRect::new(
            header_rect.x,
            header_rect.y,
            (min_x - px(4.0, scale) - header_rect.x).max(0),
            header_rect.h - hairline,
        );
        painter.push_clip(crumb_clip);
        self.draw_breadcrumbs(painter, crumb_clip, theme, atlas, rasterizer);
        painter.pop_clip();

        // Minimize / hide sidebar button (shaded sidebar panel matching the docked side).
        if self.minimize_hovered {
            painter.fill_rounded_rect(self.minimize_rect, 4.0 * scale, theme.current_line);
        }
        let min_col = if self.minimize_hovered {
            theme.text
        } else {
            theme.text_muted
        };
        draw_sidebar_panel_icon(painter, self.minimize_rect, self.side, true, min_col, scale);

        painter.fill_rect(
            PixelRect::new(
                header_rect.x,
                header_rect.bottom() - hairline,
                header_rect.w,
                hairline,
            ),
            theme.border_subtle,
        );

        // 2. Scrollable rows list.
        let list_rect = PixelRect::new(
            content_rect.x,
            header_rect.bottom(),
            content_rect.w,
            (content_rect.h - self.header_height_px).max(0),
        );
        painter.push_clip(list_rect);

        let font_px = 12.5 * scale;
        let badge_px = 10.0 * scale;
        let metrics = atlas.metrics(rasterizer, FontStyle::Ui, font_px);
        let bmetrics = atlas.metrics(rasterizer, FontStyle::UiBold, badge_px);
        let first_row = (self.scroll_y_px / self.row_height_px as f32).floor().max(0.0) as usize;
        let visible_rows = (list_rect.h / self.row_height_px) as usize + 2;
        let end_row = (first_row + visible_rows).min(self.rows.len());

        for idx in first_row..end_row {
            let row = &self.rows[idx];
            let ry = list_rect.y + idx as i32 * self.row_height_px - self.scroll_y_px.round() as i32;
            let row_rect = PixelRect::new(list_rect.x, ry, list_rect.w, self.row_height_px);
            let is_selected = self.selected.as_deref() == Some(row.path.as_path());
            let is_hovered = self.hovered_row == Some(idx);
            let badge = if row.kind == EntryKind::Directory {
                SidebarBadge::default()
            } else {
                badge_for(&row.path)
            };

            if is_selected {
                let pill = PixelRect::new(
                    row_rect.x + px(4.0, scale),
                    row_rect.y + px(1.0, scale),
                    (row_rect.w - px(8.0, scale)).max(0),
                    (row_rect.h - px(2.0, scale)).max(0),
                );
                painter.fill_rounded_rect(pill, 4.0 * scale, theme.selection);
            } else if is_hovered {
                let pill = PixelRect::new(
                    row_rect.x + px(4.0, scale),
                    row_rect.y + px(1.0, scale),
                    (row_rect.w - px(8.0, scale)).max(0),
                    (row_rect.h - px(2.0, scale)).max(0),
                );
                painter.fill_rounded_rect(pill, 4.0 * scale, theme.current_line);
            }

            let indent = px(10.0 + row.depth as f32 * 14.0, scale);
            let cy = row_rect.y as f32 + row_rect.h as f32 * 0.5;
            let text_x = if row.kind == EntryKind::Directory {
                let chev_x = (row_rect.x + indent + px(5.0, scale)) as f32;
                painter.draw_chevron(chev_x, cy, 5.5 * scale, row.expanded, theme.text_muted);
                (row_rect.x + indent + px(16.0, scale)) as f32
            } else {
                (row_rect.x + indent + px(16.0, scale)) as f32
            };

            let baseline =
                row_rect.y as f32 + (row_rect.h as f32 + metrics.ascent - metrics.descent) * 0.5;
            let label_color = if row.kind == EntryKind::Directory || is_selected || badge.pin_slot.is_some() {
                theme.text
            } else {
                theme.text_muted
            };
            let style = if is_selected && self.focused {
                FontStyle::UiBold
            } else {
                FontStyle::Ui
            };
            let mut right_reserve = px(8.0, scale);
            if row.kind == EntryKind::Directory {
                right_reserve = px(24.0, scale);
            } else {
                if badge.pin_slot.is_some() {
                    right_reserve += px(20.0, scale);
                }
                if badge.dirty {
                    right_reserve += px(10.0, scale);
                }
            }
            let max_text_right = row_rect.right() - right_reserve;
            painter.push_clip(PixelRect::new(
                row_rect.x,
                row_rect.y,
                (max_text_right - row_rect.x).max(0),
                row_rect.h,
            ));
            painter.draw_text_ui(
                text_x,
                baseline,
                &row.name,
                style,
                font_px,
                label_color,
                atlas,
                rasterizer,
            );
            painter.pop_clip();

            // Quiet right-aligned dirty dot & Harpoon slot badge (`1`..`9`) on file rows.
            if row.kind != EntryKind::Directory {
                let mut rx = row_rect.right() - px(8.0, scale);
                if let Some(slot) = badge.pin_slot {
                    let bsz = px(15.0, scale);
                    rx -= bsz;
                    let brect = PixelRect::new(rx, (cy - bsz as f32 * 0.5).round() as i32, bsz, bsz);
                    let bg = if is_selected { theme.surface } else { theme.current_line };
                    painter.fill_rounded_rect(brect, 3.0 * scale, bg);
                    painter.stroke_rounded_rect(brect, 3.0 * scale, hairline as f32, theme.border_subtle);
                    let s = slot.to_string();
                    let sw = Painter::measure_text_ui(&s, FontStyle::UiBold, badge_px, atlas, rasterizer);
                    let bbase = brect.y as f32 + (brect.h as f32 + bmetrics.ascent - bmetrics.descent) * 0.5;
                    painter.draw_text_ui(
                        brect.x as f32 + (brect.w as f32 - sw) * 0.5,
                        bbase,
                        &s,
                        FontStyle::UiBold,
                        badge_px,
                        theme.text_muted,
                        atlas,
                        rasterizer,
                    );
                    rx -= px(5.0, scale);
                }
                if badge.dirty {
                    let dot = px(4.0, scale).max(2);
                    rx -= dot;
                    painter.fill_rounded_rect(
                        PixelRect::new(rx, (cy - dot as f32 * 0.5).round() as i32, dot, dot),
                        dot as f32 * 0.5,
                        theme.text,
                    );
                }
            }

            // Right-edge enter-folder affordance on hovered/selected directory rows.
            if row.kind == EntryKind::Directory && (is_hovered || is_selected) {
                let enter_x = (row_rect.right() - px(13.0, scale)) as f32;
                painter.draw_chevron(enter_x, cy, 5.0 * scale, false, theme.text_muted);
            }
        }

        painter.pop_clip(); // list_rect
        painter.pop_clip(); // content_rect
    }

    fn draw_breadcrumbs(
        &mut self,
        painter: &mut Painter,
        rect: PixelRect,
        theme: &Theme,
        atlas: &mut GlyphAtlas,
        rasterizer: &mut dyn GlyphRasterizer,
    ) {
        self.crumb_rects.clear();
        let Some(root) = self.root.clone() else {
            return;
        };
        let scale = painter.scale();
        let font_px = 11.5 * scale;
        let metrics = atlas.metrics(rasterizer, FontStyle::Ui, font_px);
        let baseline = rect.y as f32 + (rect.h as f32 + metrics.ascent - metrics.descent) * 0.5;

        // Show up to the last 3 path segments so the current folder is always clearly visible.
        let mut ancestors: Vec<PathBuf> = root.ancestors().map(PathBuf::from).collect();
        ancestors.reverse();
        let start_idx = ancestors.len().saturating_sub(3);
        let shown = &ancestors[start_idx..];

        let mut x = (rect.x + px(8.0, scale)) as f32;
        for (i, path) in shown.iter().enumerate() {
            if i > 0 {
                x = painter.draw_text_ui(
                    x,
                    baseline,
                    " / ",
                    FontStyle::Ui,
                    font_px,
                    theme.text_muted,
                    atlas,
                    rasterizer,
                );
            }
            let label = path
                .file_name()
                .and_then(|n| n.to_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("/");
            let is_last = i + 1 == shown.len();
            let style = if is_last {
                FontStyle::UiBold
            } else {
                FontStyle::Ui
            };
            let color = if is_last { theme.text } else { theme.text_muted };
            let start_x = x;
            x = painter.draw_text_ui(x, baseline, label, style, font_px, color, atlas, rasterizer);
            self.crumb_rects.push((
                PixelRect::new(
                    start_x.floor() as i32 - px(2.0, scale),
                    rect.y,
                    (x - start_x).ceil() as i32 + px(4.0, scale),
                    rect.h,
                ),
                path.clone(),
            ));
        }
    }
}

/// Crisp 1px vector sidebar-toggle icon (rounded window frame with a sidebar column on `side`).
///
/// - `expanded == true` (in sidebar header): shaded sidebar column indicates the sidebar is open
///   and clicking will minimize/hide it.
/// - `expanded == false` (in TabStrip when minimized): hollow sidebar column with two quiet row
///   ticks indicates the sidebar is collapsed and clicking will expand/show it.
pub(crate) fn draw_sidebar_panel_icon(
    painter: &mut Painter,
    rect: PixelRect,
    side: SidebarSide,
    expanded: bool,
    color: crate::theme::Color,
    scale: f32,
) {
    let t = (1.0 * scale).round().max(1.0) as i32;
    let iw = px(14.0, scale).max(10);
    let ih = px(11.0, scale).max(8);
    let x = rect.x + (rect.w - iw) / 2;
    let y = rect.y + (rect.h - ih) / 2;
    let box_rect = PixelRect::new(x, y, iw, ih);
    painter.stroke_rounded_rect(box_rect, 2.0 * scale, t as f32, color);
    let pane_w = (iw * 5 / 14).max(t * 3);
    let div_x = match side {
        SidebarSide::Left => x + pane_w,
        SidebarSide::Right => box_rect.right() - pane_w - t,
    };
    painter.fill_rect(PixelRect::new(div_x, y + t, t, (ih - 2 * t).max(1)), color);
    let inner_x = match side {
        SidebarSide::Left => x + t,
        SidebarSide::Right => div_x + t,
    };
    let inner_w = (pane_w - t).max(1);
    if expanded {
        painter.fill_rect(
            PixelRect::new(inner_x, y + t, inner_w, (ih - 2 * t).max(1)),
            color.with_alpha(120),
        );
    } else if inner_w >= t * 2 {
        // Two subtle horizontal row ticks inside the hollow sidebar column (classic macOS sidebar glyph).
        let tick_pad = (0.8 * scale).round().max(1.0) as i32;
        let tick_w = (inner_w - tick_pad * 2).max(t);
        let tx = inner_x + (inner_w - tick_w) / 2;
        let ty1 = y + (ih / 3);
        let ty2 = y + (ih * 2 / 3) - t / 2;
        painter.fill_rect(PixelRect::new(tx, ty1, tick_w, t), color.with_alpha(165));
        painter.fill_rect(PixelRect::new(tx, ty2, tick_w, t), color.with_alpha(165));
    }
}
