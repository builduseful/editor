//! Coalescing undo/redo history with bounded resident byte budget.

const MAX_UNDO_STEPS: usize = 10_000;
const MAX_UNDO_BYTES: usize = 8 * 1024 * 1024; // 8 MB resident cap

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditOp {
    pub offset: u64,
    pub deleted: Vec<u8>,
    pub inserted: Vec<u8>,
    pub cursor_before: u64,
    pub cursor_after: u64,
}

#[derive(Debug, Clone)]
struct UndoEntry {
    ops: Vec<EditOp>,
    timestamp_ms: u64,
    coalescable: bool,
    byte_cost: usize,
    rev_before: u64,
    rev_after: u64,
}

#[derive(Debug)]
pub struct UndoStack {
    undo: Vec<UndoEntry>,
    redo: Vec<UndoEntry>,
    resident_bytes: usize,
    next_rev: u64,
    current_rev: u64,
    saved_rev: Option<u64>,
}

impl Default for UndoStack {
    fn default() -> Self {
        Self::new()
    }
}

impl UndoStack {
    pub fn new() -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            resident_bytes: 0,
            next_rev: 1,
            current_rev: 0,
            saved_rev: Some(0),
        }
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.resident_bytes = 0;
        self.current_rev = self.next_rev;
        self.next_rev += 1;
        self.saved_rev = Some(self.current_rev);
    }

    pub fn mark_saved(&mut self) {
        self.break_coalescing();
        self.saved_rev = Some(self.current_rev);
    }

    /// True whenever the current document revision differs from the last saved (or opened) revision.
    /// Undoing all the way back to the saved state cleanly restores `is_dirty() == false`.
    pub fn is_dirty(&self) -> bool {
        self.saved_rev != Some(self.current_rev)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn resident_bytes(&self) -> usize {
        self.resident_bytes
    }

    /// Record an edit operation. Consecutive single-character typing or backspacing within
    /// 500 ms coalesces cleanly into one logical undo step.
    pub fn record(&mut self, op: EditOp, timestamp_ms: u64, coalescable: bool) {
        for dropped in self.redo.drain(..) {
            self.resident_bytes = self.resident_bytes.saturating_sub(dropped.byte_cost);
        }

        let cost = op.deleted.len() + op.inserted.len();

        if coalescable {
            if let Some(last) = self.undo.last_mut() {
                if last.coalescable
                    && timestamp_ms.saturating_sub(last.timestamp_ms) <= 500
                    && last.ops.len() == 1
                {
                    let prev = &mut last.ops[0];
                    // Coalesce contiguous typing (pure insert right at previous insert end).
                    if prev.deleted.is_empty()
                        && op.deleted.is_empty()
                        && op.offset == prev.offset + prev.inserted.len() as u64
                        && !op.inserted.contains(&b'\n')
                        && !prev.inserted.contains(&b'\n')
                    {
                        prev.inserted.extend_from_slice(&op.inserted);
                        prev.cursor_after = op.cursor_after;
                        last.timestamp_ms = timestamp_ms;
                        last.byte_cost += cost;
                        let rev = self.next_rev;
                        self.next_rev += 1;
                        last.rev_after = rev;
                        self.current_rev = rev;
                        self.resident_bytes += cost;
                        self.enforce_budget();
                        return;
                    }
                    // Coalesce contiguous backspace (pure delete ending right at previous delete start).
                    if prev.inserted.is_empty()
                        && op.inserted.is_empty()
                        && op.offset + op.deleted.len() as u64 == prev.offset
                        && !op.deleted.contains(&b'\n')
                    {
                        let mut merged = op.deleted;
                        merged.extend_from_slice(&prev.deleted);
                        prev.offset = op.offset;
                        prev.deleted = merged;
                        prev.cursor_after = op.cursor_after;
                        last.timestamp_ms = timestamp_ms;
                        last.byte_cost += cost;
                        let rev = self.next_rev;
                        self.next_rev += 1;
                        last.rev_after = rev;
                        self.current_rev = rev;
                        self.resident_bytes += cost;
                        self.enforce_budget();
                        return;
                    }
                }
            }
        }

        let rev_before = self.current_rev;
        let rev_after = self.next_rev;
        self.next_rev += 1;
        self.current_rev = rev_after;
        self.undo.push(UndoEntry {
            ops: vec![op],
            timestamp_ms,
            coalescable,
            byte_cost: cost,
            rev_before,
            rev_after,
        });
        self.resident_bytes += cost;
        self.enforce_budget();
    }

    pub fn record_batch(&mut self, ops: Vec<EditOp>, timestamp_ms: u64) {
        if ops.is_empty() {
            return;
        }
        for dropped in self.redo.drain(..) {
            self.resident_bytes = self.resident_bytes.saturating_sub(dropped.byte_cost);
        }
        let cost: usize = ops.iter().map(|o| o.deleted.len() + o.inserted.len()).sum();
        let rev_before = self.current_rev;
        let rev_after = self.next_rev;
        self.next_rev += 1;
        self.current_rev = rev_after;
        self.undo.push(UndoEntry {
            ops,
            timestamp_ms,
            coalescable: false,
            byte_cost: cost,
            rev_before,
            rev_after,
        });
        self.resident_bytes += cost;
        self.enforce_budget();
    }

    /// Break coalescing (e.g. on cursor move, save, or focus change).
    pub fn break_coalescing(&mut self) {
        if let Some(last) = self.undo.last_mut() {
            last.coalescable = false;
        }
    }

    /// Pop the top undo entry, moving it to the redo stack, and return its ops to invert.
    pub fn pop_undo(&mut self) -> Option<Vec<EditOp>> {
        let entry = self.undo.pop()?;
        let ops = entry.ops.clone();
        self.current_rev = entry.rev_before;
        self.redo.push(entry);
        Some(ops)
    }

    /// Pop the top redo entry, moving it back to the undo stack, and return its ops to replay.
    pub fn pop_redo(&mut self) -> Option<Vec<EditOp>> {
        let entry = self.redo.pop()?;
        let ops = entry.ops.clone();
        self.current_rev = entry.rev_after;
        self.undo.push(entry);
        Some(ops)
    }

    fn enforce_budget(&mut self) {
        while self.undo.len() > MAX_UNDO_STEPS
            || (self.resident_bytes > MAX_UNDO_BYTES && self.undo.len() > 1)
        {
            let removed = self.undo.remove(0);
            if self.saved_rev == Some(removed.rev_before) && self.current_rev != removed.rev_before
            {
                self.saved_rev = None;
            }
            self.resident_bytes = self.resident_bytes.saturating_sub(removed.byte_cost);
        }
    }
}
