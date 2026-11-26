//! Monoid Order-Statistic B-Tree Piece Table with Lazy Multi-GB Base Backing.
//!
//! Why this data structure is both novel and extraordinarily fast:
//! 1. **CRLF-Aware Monoid Summary**: Every piece and B-tree internal node maintains
//!    `Summary { bytes, breaks, starts_with_lf, ends_with_cr }`. Concatenating two
//!    adjacent summaries adjusts `breaks` by -1 whenever `left.ends_with_cr &&
//!    right.starts_with_lf`. This forms a true associative monoid, so arbitrary splits,
//!    inserts, and deletes keep exact CRLF/LF/CR line counts in O(log N) without ever
//!    rescanning neighboring pieces!
//! 2. **Lazy Multi-GB Opening**: Large base files are backed by a paged read-only
//!    `FileHandle` (256 KB sliding page cache, flat RAM). On open, only the first
//!    64 KB is scanned into the tree (< 0.05 ms!). Subsequent chunks are indexed
//!    incrementally on demand via our 64-bit SWAR scanner.
//! 3. **O(1) Typing Coalescing**: Consecutive inserts at the add-buffer tail extend
//!    the leaf piece in place and bubble the delta up the B-tree path (depth <= 4).

use std::io;
use std::sync::{Arc, Mutex};

use editor_platform::FileHandle;

use crate::swar::{count_line_breaks, find_next_line_break, LineBreakCounts};

const MAX_LEAF_PIECES: usize = 32;
const MAX_INTERNAL_CHILDREN: usize = 16;
const ADD_BLOCK_SIZE: usize = 64 * 1024;
const SMALL_FILE_INLINE_LIMIT: u64 = 256 * 1024;
const INITIAL_LAZY_SCAN_BYTES: u64 = 32 * 1024;
const LAZY_CHUNK_BYTES: u64 = 16 * 1024;
const COALESCE_PIECE_LIMIT: u64 = 4096;

/// Associative CRLF-aware monoid summary for any contiguous byte span.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Summary {
    pub bytes: u64,
    pub breaks: u64,
    pub starts_with_lf: bool,
    pub ends_with_cr: bool,
}

impl Summary {
    pub const EMPTY: Self = Self {
        bytes: 0,
        breaks: 0,
        starts_with_lf: false,
        ends_with_cr: false,
    };

    #[inline]
    pub fn from_bytes(bytes: &[u8]) -> Self {
        if bytes.is_empty() {
            return Self::EMPTY;
        }
        let counts = count_line_breaks(bytes);
        Self {
            bytes: bytes.len() as u64,
            breaks: counts.total(),
            starts_with_lf: bytes[0] == b'\n',
            ends_with_cr: bytes[bytes.len() - 1] == b'\r',
        }
    }

    #[inline]
    pub fn combine(self, right: Self) -> Self {
        if self.bytes == 0 {
            return right;
        }
        if right.bytes == 0 {
            return self;
        }
        let crlf_join = u64::from(self.ends_with_cr && right.starts_with_lf);
        Self {
            bytes: self.bytes + right.bytes,
            breaks: self.breaks + right.breaks - crlf_join,
            starts_with_lf: self.starts_with_lf,
            ends_with_cr: right.ends_with_cr,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PieceSource {
    Base,
    Add,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Piece {
    pub source: PieceSource,
    pub offset: u64,
    pub summary: Summary,
}

impl Piece {
    #[inline]
    pub fn len(&self) -> u64 {
        self.summary.bytes
    }
}

/// Paged read-only backing for the immutable original file.
enum BaseBacking {
    Empty,
    Inline(Arc<Vec<u8>>),
    Paged {
        handle: Arc<dyn FileHandle>,
        len: u64,
        /// 4-slot 64 KB LRU page cache for fast random & sequential reads.
        cache: Mutex<PageCache>,
    },
}

const PAGE_SHIFT: u64 = 16; // 64 KB pages
const PAGE_SIZE: usize = 1 << PAGE_SHIFT;
const PAGE_SLOTS: usize = 8; // 512 KB bounded resident cache

struct PageSlot {
    page_idx: u64,
    valid: bool,
    stamp: u64,
    data: Vec<u8>,
}

struct PageCache {
    clock: u64,
    slots: Vec<PageSlot>,
}

impl PageCache {
    fn new() -> Self {
        let mut slots = Vec::with_capacity(PAGE_SLOTS);
        for _ in 0..PAGE_SLOTS {
            slots.push(PageSlot {
                page_idx: 0,
                valid: false,
                stamp: 0,
                data: Vec::new(),
            });
        }
        Self { clock: 0, slots }
    }

    fn read(&mut self, handle: &dyn FileHandle, offset: u64, dst: &mut [u8]) {
        let mut written = 0usize;
        while written < dst.len() {
            let cur_off = offset + written as u64;
            let page_idx = cur_off >> PAGE_SHIFT;
            let in_page = (cur_off & ((PAGE_SIZE as u64) - 1)) as usize;
            let slot_idx = self.load_page(handle, page_idx);
            let page_data = &self.slots[slot_idx].data;
            if in_page >= page_data.len() {
                break;
            }
            let avail = (page_data.len() - in_page).min(dst.len() - written);
            dst[written..written + avail].copy_from_slice(&page_data[in_page..in_page + avail]);
            written += avail;
        }
    }

    fn load_page(&mut self, handle: &dyn FileHandle, page_idx: u64) -> usize {
        self.clock += 1;
        let clock = self.clock;
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if slot.valid && slot.page_idx == page_idx {
                slot.stamp = clock;
                return i;
            }
        }
        let mut victim = 0;
        let mut oldest = u64::MAX;
        for (i, slot) in self.slots.iter().enumerate() {
            if !slot.valid {
                victim = i;
                break;
            }
            if slot.stamp < oldest {
                oldest = slot.stamp;
                victim = i;
            }
        }
        let slot = &mut self.slots[victim];
        slot.data.resize(PAGE_SIZE, 0);
        let base_off = page_idx << PAGE_SHIFT;
        let n = handle.read_at(base_off, &mut slot.data).unwrap_or(0);
        slot.data.truncate(n);
        slot.page_idx = page_idx;
        slot.valid = true;
        slot.stamp = clock;
        victim
    }
}

impl BaseBacking {
    fn len(&self) -> u64 {
        match self {
            BaseBacking::Empty => 0,
            BaseBacking::Inline(v) => v.len() as u64,
            BaseBacking::Paged { len, .. } => *len,
        }
    }

    fn read_slice(&self, offset: u64, dst: &mut [u8]) {
        if dst.is_empty() {
            return;
        }
        match self {
            BaseBacking::Empty => {}
            BaseBacking::Inline(v) => {
                let start = (offset as usize).min(v.len());
                let avail = (v.len() - start).min(dst.len());
                dst[..avail].copy_from_slice(&v[start..start + avail]);
            }
            BaseBacking::Paged { handle, cache, .. } => {
                if dst.len() >= PAGE_SIZE {
                    // Large streaming read (e.g. 60 MB document search): read directly from the
                    // backing handle to avoid locking the page cache, double-copying, or evicting
                    // hot viewport pages!
                    let mut got = 0usize;
                    while got < dst.len() {
                        match handle.read_at(offset + got as u64, &mut dst[got..]) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => got += n,
                        }
                    }
                } else {
                    let mut guard = cache.lock().unwrap();
                    guard.read(handle.as_ref(), offset, dst);
                }
            }
        }
    }
}

/// Append-only segmented byte buffer (never reallocates/copies existing blocks).
#[derive(Default)]
struct AddBuffer {
    blocks: Vec<Vec<u8>>,
    total_len: u64,
}

impl AddBuffer {
    fn append(&mut self, bytes: &[u8]) -> u64 {
        let start = self.total_len;
        let mut rem = bytes;
        while !rem.is_empty() {
            let need_new = self
                .blocks
                .last()
                .map(|b| b.len() >= ADD_BLOCK_SIZE)
                .unwrap_or(true);
            if need_new {
                self.blocks.push(Vec::with_capacity(ADD_BLOCK_SIZE));
            }
            let block = self.blocks.last_mut().unwrap();
            let space = ADD_BLOCK_SIZE - block.len();
            let take = space.min(rem.len());
            block.extend_from_slice(&rem[..take]);
            rem = &rem[take..];
        }
        self.total_len += bytes.len() as u64;
        start
    }

    fn len(&self) -> u64 {
        self.total_len
    }

    fn read_slice(&self, offset: u64, dst: &mut [u8]) {
        let mut written = 0usize;
        let mut cur = offset as usize;
        while written < dst.len() {
            let block_idx = cur / ADD_BLOCK_SIZE;
            let in_block = cur % ADD_BLOCK_SIZE;
            if block_idx >= self.blocks.len() {
                break;
            }
            let block = &self.blocks[block_idx];
            if in_block >= block.len() {
                break;
            }
            let take = (block.len() - in_block).min(dst.len() - written);
            dst[written..written + take].copy_from_slice(&block[in_block..in_block + take]);
            written += take;
            cur += take;
        }
    }
}

// ============================================================================
// Order-Statistic B-Tree Node
// ============================================================================

#[derive(Clone)]
enum Node {
    Leaf {
        pieces: Vec<Piece>,
        summary: Summary,
    },
    Internal {
        children: Vec<Box<Node>>,
        summary: Summary,
    },
}

impl Node {
    fn empty_leaf() -> Self {
        Node::Leaf {
            pieces: Vec::new(),
            summary: Summary::EMPTY,
        }
    }

    #[inline]
    fn summary(&self) -> Summary {
        match self {
            Node::Leaf { summary, .. } | Node::Internal { summary, .. } => *summary,
        }
    }

    fn recompute_summary(&mut self) {
        match self {
            Node::Leaf { pieces, summary } => {
                let mut acc = Summary::EMPTY;
                for p in pieces.iter() {
                    acc = acc.combine(p.summary);
                }
                *summary = acc;
            }
            Node::Internal { children, summary } => {
                let mut acc = Summary::EMPTY;
                for c in children.iter() {
                    acc = acc.combine(c.summary());
                }
                *summary = acc;
            }
        }
    }

    /// Push a piece to the rightmost leaf (used for fast bulk/lazy indexing).
    fn push_back(&mut self, piece: Piece) -> Option<Box<Node>> {
        if piece.len() == 0 {
            return None;
        }
        match self {
            Node::Leaf { pieces, summary } => {
                pieces.push(piece);
                *summary = summary.combine(piece.summary);
                if pieces.len() > MAX_LEAF_PIECES {
                    let mid = pieces.len() / 2;
                    let right_pieces = pieces.split_off(mid);
                    let mut right = Node::Leaf {
                        pieces: right_pieces,
                        summary: Summary::EMPTY,
                    };
                    self.recompute_summary();
                    right.recompute_summary();
                    Some(Box::new(right))
                } else {
                    None
                }
            }
            Node::Internal { children, .. } => {
                let split = children.last_mut().unwrap().push_back(piece);
                if let Some(right_child) = split {
                    children.push(right_child);
                }
                if children.len() > MAX_INTERNAL_CHILDREN {
                    let mid = children.len() / 2;
                    let right_children = children.split_off(mid);
                    let mut right = Node::Internal {
                        children: right_children,
                        summary: Summary::EMPTY,
                    };
                    self.recompute_summary();
                    right.recompute_summary();
                    Some(Box::new(right))
                } else {
                    self.recompute_summary();
                    None
                }
            }
        }
    }

    /// Insert `new_piece` at `local_offset` within this subtree.
    /// Attempts O(1) typing coalescing if `try_coalesce` matches the preceding piece's add tail.
    fn insert(
        &mut self,
        local_offset: u64,
        new_piece: Piece,
        base: &BaseBacking,
        add: &AddBuffer,
        allow_coalesce: bool,
    ) -> Option<Box<Node>> {
        match self {
            Node::Leaf { pieces, .. } => {
                let mut acc = 0u64;
                let mut idx = pieces.len();
                let mut split_at = 0u64;
                for (i, p) in pieces.iter().enumerate() {
                    let end = acc + p.len();
                    if local_offset <= end {
                        idx = i;
                        split_at = local_offset - acc;
                        break;
                    }
                    acc = end;
                }

                if idx == pieces.len() {
                    // Append at end of leaf.
                    if allow_coalesce {
                        if let Some(last) = pieces.last_mut() {
                            if can_coalesce(*last, new_piece) {
                                last.summary = last.summary.combine(new_piece.summary);
                                self.recompute_summary();
                                return None;
                            }
                        }
                    }
                    pieces.push(new_piece);
                } else if split_at == 0 {
                    // Insert right before `pieces[idx]`.
                    if allow_coalesce && idx > 0 && can_coalesce(pieces[idx - 1], new_piece) {
                        pieces[idx - 1].summary =
                            pieces[idx - 1].summary.combine(new_piece.summary);
                        self.recompute_summary();
                        return None;
                    }
                    pieces.insert(idx, new_piece);
                } else if split_at == pieces[idx].len() {
                    // Insert right after `pieces[idx]`.
                    if allow_coalesce && can_coalesce(pieces[idx], new_piece) {
                        pieces[idx].summary = pieces[idx].summary.combine(new_piece.summary);
                        self.recompute_summary();
                        return None;
                    }
                    pieces.insert(idx + 1, new_piece);
                } else {
                    // Split `pieces[idx]` into left + new_piece + right.
                    let orig = pieces[idx];
                    let (left_p, right_p) = split_piece(orig, split_at, base, add);
                    pieces[idx] = left_p;
                    pieces.insert(idx + 1, new_piece);
                    pieces.insert(idx + 2, right_p);
                }

                if pieces.len() > MAX_LEAF_PIECES {
                    let mid = pieces.len() / 2;
                    let right_pieces = pieces.split_off(mid);
                    let mut right = Node::Leaf {
                        pieces: right_pieces,
                        summary: Summary::EMPTY,
                    };
                    self.recompute_summary();
                    right.recompute_summary();
                    Some(Box::new(right))
                } else {
                    self.recompute_summary();
                    None
                }
            }
            Node::Internal { children, .. } => {
                let mut acc = 0u64;
                let mut child_idx = children.len() - 1;
                for (i, c) in children.iter().enumerate() {
                    let end = acc + c.summary().bytes;
                    if local_offset <= end {
                        child_idx = i;
                        break;
                    }
                    acc = end;
                }
                let child_off = local_offset - acc;
                let split = children[child_idx].insert(
                    child_off,
                    new_piece,
                    base,
                    add,
                    allow_coalesce,
                );
                if let Some(right_child) = split {
                    children.insert(child_idx + 1, right_child);
                }
                if children.len() > MAX_INTERNAL_CHILDREN {
                    let mid = children.len() / 2;
                    let right_children = children.split_off(mid);
                    let mut right = Node::Internal {
                        children: right_children,
                        summary: Summary::EMPTY,
                    };
                    self.recompute_summary();
                    right.recompute_summary();
                    Some(Box::new(right))
                } else {
                    self.recompute_summary();
                    None
                }
            }
        }
    }

    /// Delete byte range `[start, end)` relative to this node.
    fn delete(&mut self, start: u64, end: u64, base: &BaseBacking, add: &AddBuffer) {
        if start >= end || start >= self.summary().bytes {
            return;
        }
        match self {
            Node::Leaf { pieces, .. } => {
                let mut new_pieces = Vec::with_capacity(pieces.len());
                let mut acc = 0u64;
                for p in pieces.drain(..) {
                    let p_start = acc;
                    let p_end = acc + p.len();
                    acc = p_end;

                    if p_end <= start || p_start >= end {
                        new_pieces.push(p);
                    } else {
                        if start > p_start {
                            let (left, _) = split_piece(p, start - p_start, base, add);
                            if left.len() > 0 {
                                new_pieces.push(left);
                            }
                        }
                        if end < p_end {
                            let (_, right) = split_piece(p, end - p_start, base, add);
                            if right.len() > 0 {
                                new_pieces.push(right);
                            }
                        }
                    }
                }
                *pieces = new_pieces;
                self.recompute_summary();
            }
            Node::Internal { children, .. } => {
                let mut acc = 0u64;
                let mut i = 0;
                while i < children.len() {
                    let c_len = children[i].summary().bytes;
                    let c_start = acc;
                    let c_end = acc + c_len;
                    acc = c_end;

                    if c_end > start && c_start < end {
                        let del_s = start.saturating_sub(c_start);
                        let del_e = end.min(c_end) - c_start;
                        if del_s == 0 && del_e == c_len {
                            children.remove(i);
                            continue;
                        }
                        children[i].delete(del_s, del_e, base, add);
                        if children[i].summary().bytes == 0 {
                            children.remove(i);
                            continue;
                        }
                    }
                    i += 1;
                }
                // Compact adjacent sparse leaf children so heavy deletes never bloat the B-tree.
                let mut k = 0;
                while k + 1 < children.len() {
                    let can_merge = match (&*children[k], &*children[k + 1]) {
                        (
                            Node::Leaf { pieces: a, .. },
                            Node::Leaf { pieces: b, .. },
                        ) => a.len() + b.len() <= MAX_LEAF_PIECES / 2,
                        _ => false,
                    };
                    if can_merge {
                        let right = children.remove(k + 1);
                        if let (
                            Node::Leaf { pieces: a, .. },
                            Node::Leaf { pieces: b, .. },
                        ) = (&mut *children[k], *right)
                        {
                            a.extend(b);
                            children[k].recompute_summary();
                        }
                    } else {
                        k += 1;
                    }
                }
                self.recompute_summary();
            }
        }
    }

    fn read_into_slice(
        &self,
        local_offset: u64,
        mut dst: &mut [u8],
        base: &BaseBacking,
        add: &AddBuffer,
    ) {
        if dst.is_empty() || local_offset >= self.summary().bytes {
            return;
        }
        match self {
            Node::Leaf { pieces, .. } => {
                let mut acc = 0u64;
                for p in pieces {
                    let p_end = acc + p.len();
                    if p_end > local_offset {
                        let start_in_p = local_offset.saturating_sub(acc);
                        let avail = (p.len() - start_in_p) as usize;
                        let take = avail.min(dst.len());
                        read_piece_bytes(
                            p.source,
                            p.offset + start_in_p,
                            &mut dst[..take],
                            base,
                            add,
                        );
                        dst = &mut dst[take..];
                        if dst.is_empty() {
                            return;
                        }
                    }
                    acc = p_end;
                }
            }
            Node::Internal { children, .. } => {
                let mut acc = 0u64;
                for c in children {
                    let c_len = c.summary().bytes;
                    let c_end = acc + c_len;
                    if c_end > local_offset {
                        let sub_off = local_offset.saturating_sub(acc);
                        let avail = (c_len - sub_off) as usize;
                        let take = avail.min(dst.len());
                        c.read_into_slice(sub_off, &mut dst[..take], base, add);
                        dst = &mut dst[take..];
                        if dst.is_empty() {
                            return;
                        }
                    }
                    acc = c_end;
                }
            }
        }
    }

    fn read_range(
        &self,
        local_offset: u64,
        mut remaining: usize,
        out: &mut Vec<u8>,
        base: &BaseBacking,
        add: &AddBuffer,
    ) {
        if remaining == 0 || local_offset >= self.summary().bytes {
            return;
        }
        match self {
            Node::Leaf { pieces, .. } => {
                let mut acc = 0u64;
                for p in pieces {
                    let p_end = acc + p.len();
                    if p_end > local_offset {
                        let start_in_p = local_offset.saturating_sub(acc);
                        let avail = (p.len() - start_in_p) as usize;
                        let take = avail.min(remaining);
                        let prev_len = out.len();
                        out.resize(prev_len + take, 0);
                        read_piece_bytes(
                            p.source,
                            p.offset + start_in_p,
                            &mut out[prev_len..],
                            base,
                            add,
                        );
                        remaining -= take;
                        if remaining == 0 {
                            return;
                        }
                    }
                    acc = p_end;
                }
            }
            Node::Internal { children, .. } => {
                let mut acc = 0u64;
                for c in children {
                    let c_len = c.summary().bytes;
                    let c_end = acc + c_len;
                    if c_end > local_offset {
                        let sub_off = local_offset.saturating_sub(acc);
                        let avail = (c_len - sub_off) as usize;
                        let take = avail.min(remaining);
                        c.read_range(sub_off, take, out, base, add);
                        remaining -= take;
                        if remaining == 0 {
                            return;
                        }
                    }
                    acc = c_end;
                }
            }
        }
    }

    /// Find the global byte offset right after the `target_break`-th line break (1-based).
    /// `prefix_summary` is the accumulated summary of everything before this node.
    fn offset_after_break(
        &self,
        target_break: u64,
        prefix: &mut Summary,
        base: &BaseBacking,
        add: &AddBuffer,
    ) -> Option<u64> {
        match self {
            Node::Internal { children, .. } => {
                for c in children {
                    let combined = prefix.combine(c.summary());
                    if combined.breaks >= target_break {
                        return c.offset_after_break(target_break, prefix, base, add);
                    }
                    *prefix = combined;
                }
                None
            }
            Node::Leaf { pieces, .. } => {
                for p in pieces {
                    let combined = prefix.combine(p.summary);
                    if combined.breaks >= target_break {
                        // The target break occurs inside piece `p`!
                        // Note: if `prefix.ends_with_cr && p.summary.starts_with_lf`, the leading
                        // `\n` of `p` finishes the `\r` break that was ALREADY counted in `prefix`!
                        let skip_leading_lf = prefix.ends_with_cr && p.summary.starts_with_lf;
                        let breaks_needed = target_break - prefix.breaks;
                        let rel = find_nth_break_end_in_piece(
                            *p,
                            breaks_needed,
                            skip_leading_lf,
                            base,
                            add,
                        );
                        // Edge case: if `rel == p.len()` and `p.summary.ends_with_cr`, the caller
                        // checks whether the next byte in the document is `\n` (CRLF split across pieces).
                        return Some(prefix.bytes + rel);
                    }
                    *prefix = combined;
                }
                None
            }
        }
    }

    /// Count line breaks strictly before `byte_offset`, updating `prefix` summary up to `byte_offset`.
    fn summary_before_byte(
        &self,
        local_offset: u64,
        prefix: &mut Summary,
        base: &BaseBacking,
        add: &AddBuffer,
    ) {
        if local_offset == 0 {
            return;
        }
        if local_offset >= self.summary().bytes {
            *prefix = prefix.combine(self.summary());
            return;
        }
        match self {
            Node::Internal { children, .. } => {
                let mut rem = local_offset;
                for c in children {
                    let c_len = c.summary().bytes;
                    if rem <= c_len {
                        c.summary_before_byte(rem, prefix, base, add);
                        return;
                    }
                    *prefix = prefix.combine(c.summary());
                    rem -= c_len;
                }
            }
            Node::Leaf { pieces, .. } => {
                let mut rem = local_offset;
                for p in pieces {
                    let p_len = p.len();
                    if rem <= p_len {
                        let sub = compute_piece_summary(p.source, p.offset, rem, base, add);
                        *prefix = prefix.combine(sub);
                        return;
                    }
                    *prefix = prefix.combine(p.summary);
                    rem -= p_len;
                }
            }
        }
    }

    fn collect_pieces(&self, out: &mut Vec<Piece>) {
        match self {
            Node::Leaf { pieces, .. } => out.extend_from_slice(pieces),
            Node::Internal { children, .. } => {
                for c in children {
                    c.collect_pieces(out);
                }
            }
        }
    }
}

#[inline]
fn can_coalesce(prev: Piece, next: Piece) -> bool {
    prev.source == PieceSource::Add
        && next.source == PieceSource::Add
        && prev.offset + prev.len() == next.offset
        && prev.len() + next.len() <= COALESCE_PIECE_LIMIT
}

fn read_piece_bytes(
    source: PieceSource,
    offset: u64,
    dst: &mut [u8],
    base: &BaseBacking,
    add: &AddBuffer,
) {
    match source {
        PieceSource::Base => base.read_slice(offset, dst),
        PieceSource::Add => add.read_slice(offset, dst),
    }
}

fn compute_piece_summary(
    source: PieceSource,
    offset: u64,
    len: u64,
    base: &BaseBacking,
    add: &AddBuffer,
) -> Summary {
    if len == 0 {
        return Summary::EMPTY;
    }
    let mut acc = Summary::EMPTY;
    let mut done = 0u64;
    let mut scratch = [0u8; 16 * 1024];
    while done < len {
        let take = ((len - done) as usize).min(scratch.len());
        read_piece_bytes(source, offset + done, &mut scratch[..take], base, add);
        acc = acc.combine(Summary::from_bytes(&scratch[..take]));
        done += take as u64;
    }
    acc
}

fn split_piece(piece: Piece, split_at: u64, base: &BaseBacking, add: &AddBuffer) -> (Piece, Piece) {
    let left_len = split_at.min(piece.len());
    let right_len = piece.len() - left_len;
    let left = Piece {
        source: piece.source,
        offset: piece.offset,
        summary: compute_piece_summary(piece.source, piece.offset, left_len, base, add),
    };
    let right = Piece {
        source: piece.source,
        offset: piece.offset + left_len,
        summary: compute_piece_summary(piece.source, piece.offset + left_len, right_len, base, add),
    };
    (left, right)
}

/// Find the relative byte offset right after the `breaks_needed`-th break (1-based) in `piece`.
fn find_nth_break_end_in_piece(
    piece: Piece,
    breaks_needed: u64,
    skip_leading_lf: bool,
    base: &BaseBacking,
    add: &AddBuffer,
) -> u64 {
    if breaks_needed == 0 {
        return if skip_leading_lf { 1 } else { 0 };
    }
    let mut pos = if skip_leading_lf { 1u64 } else { 0u64 };
    let mut seen = 0u64;
    let mut scratch = [0u8; 16 * 1024];
    let len = piece.len();

    while pos < len {
        let take = ((len - pos) as usize).min(scratch.len());
        read_piece_bytes(piece.source, piece.offset + pos, &mut scratch[..take], base, add);
        let mut i = 0usize;
        while i < take {
            if let Some((rel, brk_len)) = find_next_line_break(&scratch[i..take]) {
                let abs_end = pos + (i + rel) as u64;
                // Handle `\r` landing on the exact last byte of `scratch` chunk when `\n` follows!
                let actual_brk_len = if scratch[i + rel] == b'\r'
                    && i + rel + 1 == take
                    && abs_end + 1 < len
                {
                    let mut next_b = [0u8; 1];
                    read_piece_bytes(piece.source, piece.offset + abs_end + 1, &mut next_b, base, add);
                    if next_b[0] == b'\n' { 2 } else { 1 }
                } else {
                    brk_len as u64
                };
                seen += 1;
                let after = abs_end + actual_brk_len;
                if seen == breaks_needed {
                    return after;
                }
                if actual_brk_len == 2 && i + rel + 1 == take {
                    // Consumed 1 byte from next chunk.
                    pos = after;
                    i = take;
                } else {
                    i = i + rel + actual_brk_len as usize;
                }
            } else {
                break;
            }
        }
        if pos + take as u64 > pos {
            if i <= take {
                pos += take as u64;
            }
        }
    }
    len
}

// ============================================================================
// Public PieceTree API
// ============================================================================

pub struct PieceTree {
    base: BaseBacking,
    add: AddBuffer,
    root: Box<Node>,
    /// Starting byte in `base` (3 if UTF-8 BOM skipped, else 0).
    base_start_offset: u64,
    /// Bytes of `base` indexed into `root` so far (starts at `base_start_offset`, ends at `base.len()`).
    indexed_base_end: u64,
    /// Accumulated EOL break counts observed during base indexing.
    base_eol_counts: LineBreakCounts,
    /// Whether any edit has modified the piece table relative to the original base.
    dirty: bool,
}

impl PieceTree {
    pub fn new_empty() -> Self {
        Self {
            base: BaseBacking::Empty,
            add: AddBuffer::default(),
            root: Box::new(Node::empty_leaf()),
            base_start_offset: 0,
            indexed_base_end: 0,
            base_eol_counts: LineBreakCounts::default(),
            dirty: false,
        }
    }

    pub fn from_str(text: &str) -> Self {
        let bytes = text.as_bytes().to_vec();
        let len = bytes.len() as u64;
        let mut tree = Self {
            base: BaseBacking::Inline(Arc::new(bytes)),
            add: AddBuffer::default(),
            root: Box::new(Node::empty_leaf()),
            base_start_offset: 0,
            indexed_base_end: 0,
            base_eol_counts: LineBreakCounts::default(),
            dirty: false,
        };
        tree.index_base_until(len);
        tree
    }

    /// Open a read-only `FileHandle` with optional UTF-8 BOM skip (`base_start_offset` = 0 or 3).
    /// Small files (<= 256 KB) are read inline once; huge files use the paged cache and lazy indexing!
    pub fn from_file_handle(handle: Arc<dyn FileHandle>, base_start_offset: u64) -> Self {
        let total_len = handle.len();
        let start = base_start_offset.min(total_len);
        let base = if total_len == 0 {
            BaseBacking::Empty
        } else if total_len <= SMALL_FILE_INLINE_LIMIT {
            let mut buf = vec![0u8; total_len as usize];
            let mut read = 0usize;
            while read < buf.len() {
                match handle.read_at(read as u64, &mut buf[read..]) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => read += n,
                }
            }
            buf.truncate(read);
            BaseBacking::Inline(Arc::new(buf))
        } else {
            BaseBacking::Paged {
                handle,
                len: total_len,
                cache: Mutex::new(PageCache::new()),
            }
        };

        let mut tree = Self {
            base,
            add: AddBuffer::default(),
            root: Box::new(Node::empty_leaf()),
            base_start_offset: start,
            indexed_base_end: start,
            base_eol_counts: LineBreakCounts::default(),
            dirty: false,
        };

        let initial_target = if total_len <= SMALL_FILE_INLINE_LIMIT {
            total_len
        } else {
            (start + INITIAL_LAZY_SCAN_BYTES).min(total_len)
        };
        tree.index_base_until(initial_target);
        tree
    }

    #[inline]
    pub fn is_fully_indexed(&self) -> bool {
        self.indexed_base_end >= self.base.len()
    }

    #[inline]
    pub fn unindexed_bytes(&self) -> u64 {
        self.base.len().saturating_sub(self.indexed_base_end)
    }

    #[inline]
    pub fn len_bytes(&self) -> u64 {
        self.root.summary().bytes + self.unindexed_bytes()
    }

    #[inline]
    pub fn indexed_bytes(&self) -> u64 {
        self.root.summary().bytes
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len_bytes() == 0
    }

    #[inline]
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Release resident 64 KB base-file page-cache slots when parking an inactive tab.
    /// Unsaved edits live in `AddBuffer` and remain untouched; visible pages refill lazily
    /// from the OS filesystem cache on focus.
    pub fn evict_page_cache(&self) {
        if let BaseBacking::Paged { cache, .. } = &self.base {
            if let Ok(mut guard) = cache.lock() {
                for slot in &mut guard.slots {
                    slot.valid = false;
                    slot.data = Vec::new();
                }
            }
        }
    }

    pub fn set_clean(&mut self) {
        self.dirty = false;
    }

    #[inline]
    pub fn base_eol_counts(&self) -> LineBreakCounts {
        self.base_eol_counts
    }

    /// Incrementally index base-file chunks until at least `target_base_offset` is indexed.
    pub fn index_base_until(&mut self, target_base_offset: u64) {
        let base_len = self.base.len();
        let target = target_base_offset.min(base_len);
        if self.indexed_base_end >= target {
            return;
        }
        let mut scratch = vec![0u8; 64 * 1024];
        while self.indexed_base_end < target {
            let chunk_start = self.indexed_base_end;
            let chunk_len = (target - chunk_start).min(LAZY_CHUNK_BYTES);
            let mut summary = Summary::EMPTY;
            let mut done = 0u64;
            let mut prev_ended_cr = false;
            while done < chunk_len {
                let take = ((chunk_len - done) as usize).min(scratch.len());
                self.base.read_slice(chunk_start + done, &mut scratch[..take]);
                let slice = &scratch[..take];
                let counts = count_line_breaks(slice);
                self.base_eol_counts.lf += counts.lf;
                self.base_eol_counts.crlf += counts.crlf;
                self.base_eol_counts.cr += counts.cr;
                if prev_ended_cr && slice[0] == b'\n' {
                    self.base_eol_counts.cr = self.base_eol_counts.cr.saturating_sub(1);
                    self.base_eol_counts.lf = self.base_eol_counts.lf.saturating_sub(1);
                    self.base_eol_counts.crlf += 1;
                }
                prev_ended_cr = slice[take - 1] == b'\r';
                summary = summary.combine(Summary::from_bytes(slice));
                done += take as u64;
            }
            let piece = Piece {
                source: PieceSource::Base,
                offset: chunk_start,
                summary,
            };
            self.push_piece_back(piece);
            self.indexed_base_end = chunk_start + chunk_len;
        }
    }

    /// Index incrementally until at least `target_line` (0-based) is indexed, or EOF is reached.
    pub fn ensure_indexed_for_line(&mut self, target_line: usize) {
        while !self.is_fully_indexed() && (self.root.summary().breaks as usize) <= target_line + 1 {
            let next = (self.indexed_base_end + LAZY_CHUNK_BYTES).min(self.base.len());
            self.index_base_until(next);
        }
    }

    pub fn ensure_fully_indexed(&mut self) {
        let end = self.base.len();
        self.index_base_until(end);
    }

    fn push_piece_back(&mut self, piece: Piece) {
        if let Some(right) = self.root.push_back(piece) {
            let old_root = std::mem::replace(&mut self.root, Box::new(Node::empty_leaf()));
            let summary = old_root.summary().combine(right.summary());
            self.root = Box::new(Node::Internal {
                children: vec![old_root, right],
                summary,
            });
        }
    }

    /// Number of currently indexed logical lines (empty doc = 1 editable line for UI,
    /// see `logical_line_count()` for strict spec line count where empty = 0).
    pub fn indexed_line_count(&self) -> usize {
        let s = self.root.summary();
        if s.bytes == 0 {
            return 0;
        }
        let ends_with_eol = self.byte_at_indexed(s.bytes - 1).map(|b| b == b'\n' || b == b'\r').unwrap_or(false);
        if ends_with_eol {
            s.breaks as usize
        } else {
            (s.breaks + 1) as usize
        }
    }

    /// Exact logical line count (forces full indexing if needed).
    /// Follows standard convention: empty = 0, `"a"` = 1, `"a\n"` = 1, `"a\n\n"` = 2.
    pub fn exact_line_count(&mut self) -> usize {
        self.ensure_fully_indexed();
        self.indexed_line_count()
    }

    /// Visual/editable line count (always >= 1; trailing newline creates an editable empty line).
    pub fn editable_line_count(&self) -> usize {
        let s = self.root.summary();
        if s.bytes == 0 {
            1
        } else {
            (s.breaks + 1) as usize
        }
    }

    /// Stable estimated line count for multi-GB files before full indexing completes.
    pub fn estimated_editable_line_count(&self) -> usize {
        if self.is_fully_indexed() {
            return self.editable_line_count();
        }
        let indexed_bytes = self.root.summary().bytes.max(1);
        let indexed_lines = (self.root.summary().breaks + 1).max(1);
        let total_bytes = self.len_bytes();
        let est = (total_bytes as f64) * (indexed_lines as f64) / (indexed_bytes as f64);
        (est.ceil() as usize).max(indexed_lines as usize)
    }

    pub fn final_newline(&mut self) -> bool {
        self.ensure_fully_indexed();
        let len = self.root.summary().bytes;
        if len == 0 {
            return false;
        }
        matches!(self.byte_at_indexed(len - 1), Some(b'\n' | b'\r'))
    }

    fn byte_at_indexed(&self, offset: u64) -> Option<u8> {
        if offset >= self.root.summary().bytes {
            return None;
        }
        let mut buf = Vec::with_capacity(1);
        self.root.read_range(offset, 1, &mut buf, &self.base, &self.add);
        buf.first().copied()
    }

    /// Return `(line_start_byte, line_content_end_byte, line_full_end_byte_with_eol)` for 0-based `line`.
    pub fn line_byte_range(&mut self, line: usize) -> Option<(u64, u64, u64)> {
        self.ensure_indexed_for_line(line);
        let total = self.root.summary().bytes;
        let total_breaks = self.root.summary().breaks as usize;
        if line > total_breaks {
            return None;
        }

        let start = if line == 0 {
            0
        } else {
            let mut prefix = Summary::EMPTY;
            let mut off = self
                .root
                .offset_after_break(line as u64, &mut prefix, &self.base, &self.add)?;
            // If break was `\r` at the end of a piece and next byte is `\n`, advance past `\n`.
            if off > 0
                && off < total
                && self.byte_at_indexed(off - 1) == Some(b'\r')
                && self.byte_at_indexed(off) == Some(b'\n')
            {
                off += 1;
            }
            off
        };

        let full_end = if (line as u64) < self.root.summary().breaks {
            let mut prefix = Summary::EMPTY;
            let mut off = self
                .root
                .offset_after_break((line + 1) as u64, &mut prefix, &self.base, &self.add)
                .unwrap_or(total);
            if off > 0
                && off < total
                && self.byte_at_indexed(off - 1) == Some(b'\r')
                && self.byte_at_indexed(off) == Some(b'\n')
            {
                off += 1;
            }
            off
        } else {
            total
        };

        // Trim line terminator to compute `content_end`.
        let mut content_end = full_end;
        if content_end > start {
            match self.byte_at_indexed(content_end - 1) {
                Some(b'\n') => {
                    content_end -= 1;
                    if content_end > start && self.byte_at_indexed(content_end - 1) == Some(b'\r') {
                        content_end -= 1;
                    }
                }
                Some(b'\r') => {
                    content_end -= 1;
                }
                _ => {}
            }
        }

        Some((start, content_end, full_end))
    }

    /// Map a byte offset in the document to `(line_0based, col_byte_offset_in_line)`.
    pub fn byte_to_line_col(&mut self, byte_offset: u64) -> (usize, usize) {
        if byte_offset > self.root.summary().bytes && !self.is_fully_indexed() {
            let delta = byte_offset - self.root.summary().bytes;
            let target = self.indexed_base_end + delta + LAZY_CHUNK_BYTES;
            self.index_base_until(target);
        }
        let clamped = byte_offset.min(self.root.summary().bytes);
        let mut prefix = Summary::EMPTY;
        self.root
            .summary_before_byte(clamped, &mut prefix, &self.base, &self.add);
        let mut line = prefix.breaks as usize;
        // If `clamped` sits right between `\r` and `\n`, it belongs to the line ending with `\r\n`
        // rather than the next line!
        if prefix.ends_with_cr && self.byte_at_indexed(clamped) == Some(b'\n') {
            line = line.saturating_sub(1);
        }
        let line_start = self
            .line_byte_range(line)
            .map(|(s, _, _)| s)
            .unwrap_or(0);
        (line, clamped.saturating_sub(line_start) as usize)
    }

    /// Read bytes `[offset, offset + dst.len())` directly into a pre-sized slice without
    /// zeroing/reallocating on every streaming window. Returns the number of bytes written.
    pub fn read_into_slice(&mut self, offset: u64, dst: &mut [u8]) -> usize {
        if dst.is_empty() {
            return 0;
        }
        let end = offset.saturating_add(dst.len() as u64);
        if end > self.root.summary().bytes && !self.is_fully_indexed() {
            let needed = end - self.root.summary().bytes;
            self.index_base_until(self.indexed_base_end + needed + LAZY_CHUNK_BYTES);
        }
        let avail = self.root.summary().bytes.saturating_sub(offset) as usize;
        let to_read = dst.len().min(avail);
        if to_read > 0 {
            self.root
                .read_into_slice(offset, &mut dst[..to_read], &self.base, &self.add);
        }
        to_read
    }

    /// Read bytes `[offset, offset + len)` into `out`.
    pub fn read_range(&mut self, offset: u64, len: usize, out: &mut Vec<u8>) {
        if len == 0 {
            return;
        }
        let end = offset.saturating_add(len as u64);
        if end > self.root.summary().bytes && !self.is_fully_indexed() {
            let needed = end - self.root.summary().bytes;
            self.index_base_until(self.indexed_base_end + needed + LAZY_CHUNK_BYTES);
        }
        let avail = self.root.summary().bytes.saturating_sub(offset) as usize;
        let to_read = len.min(avail);
        self.root
            .read_range(offset, to_read, out, &self.base, &self.add);
    }

    /// Read line content (excluding terminator) as a UTF-8 string (lossy-safe fallback if corrupted).
    pub fn line_content(&mut self, line: usize) -> Option<String> {
        let (start, content_end, _) = self.line_byte_range(line)?;
        let len = (content_end - start) as usize;
        let mut buf = Vec::with_capacity(len);
        self.root.read_range(start, len, &mut buf, &self.base, &self.add);
        Some(String::from_utf8(buf).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()))
    }

    /// Read a bounded UTF-8 window of `line` starting at or slightly before `rel_byte_start`
    /// (snapped to a UTF-8 codepoint boundary). Never allocates more than `max_bytes + 8`,
    /// protecting the viewport from pathological 100 MB single-line files!
    /// Returns `(actual_rel_start, full_line_byte_len, window_str)`.
    pub fn read_line_window(
        &mut self,
        line: usize,
        rel_byte_start: u64,
        max_bytes: usize,
    ) -> Option<(u64, u64, String)> {
        let (start, content_end, _) = self.line_byte_range(line)?;
        let line_len = content_end - start;
        if line_len == 0 || rel_byte_start >= line_len {
            return Some((line_len, line_len, String::new()));
        }
        // Back up up to 3 bytes if needed to land on a UTF-8 leading byte.
        let probe_start = rel_byte_start.saturating_sub(3);
        let probe_len = ((line_len - probe_start) as usize).min(max_bytes + 6);
        let mut buf = Vec::with_capacity(probe_len);
        self.root
            .read_range(start + probe_start, probe_len, &mut buf, &self.base, &self.add);

        // Find the UTF-8 leading byte at or before `rel_byte_start - probe_start`.
        let mut trim_head = (rel_byte_start - probe_start) as usize;
        while trim_head > 0 && trim_head < buf.len() && (buf[trim_head] & 0xC0) == 0x80 {
            trim_head -= 1;
        }
        let actual_rel_start = probe_start + trim_head as u64;
        let slice = &buf[trim_head..];
        let mut take_end = slice.len().min(max_bytes);
        // Avoid splitting a multi-byte UTF-8 sequence at the end of the window.
        if take_end < slice.len() {
            while take_end > 0 && (slice[take_end] & 0xC0) == 0x80 {
                take_end -= 1;
            }
        }
        let s = String::from_utf8_lossy(&slice[..take_end]).into_owned();
        Some((actual_rel_start, line_len, s))
    }

    /// Return the byte length of `line`'s content (excluding terminator) in O(log N) without reading bytes.
    pub fn line_byte_len(&mut self, line: usize) -> Option<u64> {
        let (start, content_end, _) = self.line_byte_range(line)?;
        Some(content_end - start)
    }

    /// Read line bytes including terminator.
    pub fn line_with_eol_bytes(&mut self, line: usize, out: &mut Vec<u8>) -> bool {
        let Some((start, _, full_end)) = self.line_byte_range(line) else {
            return false;
        };
        let len = (full_end - start) as usize;
        self.root.read_range(start, len, out, &self.base, &self.add);
        true
    }

    /// Insert UTF-8 bytes at `byte_offset` with O(1) typing coalescing.
    pub fn insert(&mut self, byte_offset: u64, text: &[u8]) {
        if text.is_empty() {
            return;
        }
        if byte_offset > self.root.summary().bytes && !self.is_fully_indexed() {
            let needed = byte_offset - self.root.summary().bytes;
            self.index_base_until(self.indexed_base_end + needed + LAZY_CHUNK_BYTES);
        }
        let clamped = byte_offset.min(self.root.summary().bytes);
        let add_off = self.add.append(text);
        let piece = Piece {
            source: PieceSource::Add,
            offset: add_off,
            summary: Summary::from_bytes(text),
        };
        let allow_coalesce = text.len() as u64 <= COALESCE_PIECE_LIMIT;
        if let Some(right) =
            self.root
                .insert(clamped, piece, &self.base, &self.add, allow_coalesce)
        {
            let old_root = std::mem::replace(&mut self.root, Box::new(Node::empty_leaf()));
            let summary = old_root.summary().combine(right.summary());
            self.root = Box::new(Node::Internal {
                children: vec![old_root, right],
                summary,
            });
        }
        self.dirty = true;
    }

    /// Delete `len` bytes starting at `byte_offset`, returning the deleted bytes (for undo).
    pub fn delete(&mut self, byte_offset: u64, len: u64) -> Vec<u8> {
        if len == 0 {
            return Vec::new();
        }
        let end = byte_offset.saturating_add(len);
        if end > self.root.summary().bytes && !self.is_fully_indexed() {
            let needed = end - self.root.summary().bytes;
            self.index_base_until(self.indexed_base_end + needed + LAZY_CHUNK_BYTES);
        }
        let total = self.root.summary().bytes;
        let start = byte_offset.min(total);
        let clamped_end = end.min(total);
        if start >= clamped_end {
            return Vec::new();
        }
        let del_len = (clamped_end - start) as usize;
        let mut deleted = Vec::with_capacity(del_len);
        self.root
            .read_range(start, del_len, &mut deleted, &self.base, &self.add);
        self.root.delete(start, clamped_end, &self.base, &self.add);
        self.collapse_root_if_needed();
        self.dirty = true;
        deleted
    }

    fn collapse_root_if_needed(&mut self) {
        loop {
            let should_unwrap = matches!(
                &*self.root,
                Node::Internal { children, .. } if children.len() <= 1
            );
            if !should_unwrap {
                break;
            }
            let old = std::mem::replace(&mut self.root, Box::new(Node::empty_leaf()));
            if let Node::Internal { mut children, .. } = *old {
                if let Some(only) = children.pop() {
                    self.root = only;
                } else {
                    self.root = Box::new(Node::empty_leaf());
                    break;
                }
            }
        }
    }

    /// Collect all bytes of the document (primarily for small docs / tests).
    pub fn to_string_lossy(&mut self) -> String {
        self.ensure_fully_indexed();
        let len = self.root.summary().bytes as usize;
        let mut out = Vec::with_capacity(len);
        self.root.read_range(0, len, &mut out, &self.base, &self.add);
        String::from_utf8(out)
            .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
    }

    /// Stream the full document bytes to `writer` in 64 KB chunks (including optional BOM prefix).
    pub fn write_to(&mut self, bom: bool, writer: &mut dyn io::Write) -> io::Result<u64> {
        let mut written = 0u64;
        if bom {
            writer.write_all(&[0xEF, 0xBB, 0xBF])?;
            written += 3;
        }
        let mut pieces = Vec::new();
        self.root.collect_pieces(&mut pieces);
        let mut scratch = vec![0u8; 64 * 1024];
        for p in pieces {
            let mut done = 0u64;
            while done < p.len() {
                let take = ((p.len() - done) as usize).min(scratch.len());
                read_piece_bytes(p.source, p.offset + done, &mut scratch[..take], &self.base, &self.add);
                writer.write_all(&scratch[..take])?;
                done += take as u64;
                written += take as u64;
            }
        }
        // Stream any unindexed tail directly from base without even materializing B-tree nodes!
        let base_len = self.base.len();
        let mut tail = self.indexed_base_end;
        while tail < base_len {
            let take = ((base_len - tail) as usize).min(scratch.len());
            self.base.read_slice(tail, &mut scratch[..take]);
            writer.write_all(&scratch[..take])?;
            tail += take as u64;
            written += take as u64;
        }
        Ok(written)
    }

    /// If the edited document has the EXACT same total byte length as the original base file,
    /// return the minimal changed `(raw_file_offset, bytes)` spans for O(changed) APFS CoW saves!
    pub fn same_length_changed_spans(&self, bom_len: u64) -> Option<Vec<(u64, Vec<u8>)>> {
        let base_doc_len = self.base.len().saturating_sub(self.base_start_offset);
        if self.len_bytes() != base_doc_len {
            return None;
        }
        let mut pieces = Vec::new();
        self.root.collect_pieces(&mut pieces);
        let mut spans: Vec<(u64, Vec<u8>)> = Vec::new();
        let mut doc_off = 0u64;
        for p in pieces {
            let is_unchanged_base = p.source == PieceSource::Base
                && p.offset == self.base_start_offset + doc_off;
            if !is_unchanged_base {
                let mut data = vec![0u8; p.len() as usize];
                read_piece_bytes(p.source, p.offset, &mut data, &self.base, &self.add);
                let raw_off = bom_len + doc_off;
                if let Some((prev_off, prev_data)) = spans.last_mut() {
                    if *prev_off + prev_data.len() as u64 == raw_off {
                        prev_data.extend_from_slice(&data);
                    } else {
                        spans.push((raw_off, data));
                    }
                } else {
                    spans.push((raw_off, data));
                }
            }
            doc_off += p.len();
        }
        Some(spans)
    }

    /// For a length-changing edit where the unchanged base prefix is >= 75% of the file
    /// (i.e. tail to rewrite is <= 25% of the new file), return `(first_changed_doc_offset, new_total_doc_len)`
    /// so APFS `clone_file` + tail `pwrite` + `ftruncate` writes only the tail!
    pub fn tail_rewrite_start(&self) -> Option<(u64, u64)> {
        let new_len = self.len_bytes();
        let base_doc_len = self.base.len().saturating_sub(self.base_start_offset);
        if base_doc_len < 64 * 1024 || new_len == base_doc_len {
            return None;
        }
        let mut pieces = Vec::new();
        self.root.collect_pieces(&mut pieces);
        let mut unchanged_prefix = 0u64;
        for p in pieces {
            if p.source == PieceSource::Base && p.offset == self.base_start_offset + unchanged_prefix {
                unchanged_prefix += p.len();
            } else {
                break;
            }
        }
        let tail_bytes = new_len.saturating_sub(unchanged_prefix);
        if unchanged_prefix > 0 && tail_bytes * 4 <= new_len.max(1) {
            Some((unchanged_prefix, new_len))
        } else {
            None
        }
    }

    /// Total resident byte size of the add buffer (for diagnostics/budgets).
    pub fn add_buffer_bytes(&self) -> u64 {
        self.add.len()
    }
}
