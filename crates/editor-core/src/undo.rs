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

    /// Create an undo stack already marked dirty (used when restoring an unsaved Hot Exit draft).
    pub fn new_dirty() -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            resident_bytes: 0,
            next_rev: 1,
            current_rev: 0,
            saved_rev: None,
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

    /// Clear history without losing dirty state (used when rolling back a stale persisted undo op).
    pub fn clear_preserving_dirty(&mut self) {
        let was_dirty = self.is_dirty();
        self.undo.clear();
        self.redo.clear();
        self.resident_bytes = 0;
        self.current_rev = self.next_rev;
        self.next_rev += 1;
        self.saved_rev = if was_dirty {
            None
        } else {
            Some(self.current_rev)
        };
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

    pub fn is_empty(&self) -> bool {
        self.undo.is_empty() && self.redo.is_empty()
    }

    pub fn current_rev(&self) -> u64 {
        self.current_rev
    }

    /// Clear `saved_rev` so undoing never falsely marks a buffer clean against a newer disk file.
    pub fn clear_saved_rev(&mut self) {
        self.saved_rev = None;
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

    /// Encode the newest undo steps and nearest redo steps around `current_rev` into a compact,
    /// deterministic little-endian binary payload bounded by `max_steps` and `max_bytes`.
    /// Returns `None` if no entries fit (or the stack is empty).
    pub fn encode_bounded(&self, max_steps: usize, max_bytes: usize) -> Option<Vec<u8>> {
        if self.is_empty() || max_steps == 0 || max_bytes < 64 {
            return None;
        }
        // Select entries closest to `current_rev`: prioritize newest undo entries first, then
        // nearest redo entries, staying strictly within `max_steps` and `max_bytes`.
        let mut used_bytes = 25usize; // header: u64 + u8 + u64 + u32 + u32
        let mut used_steps = 0usize;

        let mut keep_undo_from = self.undo.len();
        for idx in (0..self.undo.len()).rev() {
            let entry_wire = entry_encoded_size(&self.undo[idx]);
            if used_steps >= max_steps || used_bytes.saturating_add(entry_wire) > max_bytes {
                break;
            }
            used_bytes += entry_wire;
            used_steps += 1;
            keep_undo_from = idx;
        }

        let mut keep_redo_from = self.redo.len();
        for idx in (0..self.redo.len()).rev() {
            let entry_wire = entry_encoded_size(&self.redo[idx]);
            if used_steps >= max_steps || used_bytes.saturating_add(entry_wire) > max_bytes {
                break;
            }
            used_bytes += entry_wire;
            used_steps += 1;
            keep_redo_from = idx;
        }

        let undo_slice = &self.undo[keep_undo_from..];
        let redo_slice = &self.redo[keep_redo_from..];
        if undo_slice.is_empty() && redo_slice.is_empty() {
            return None;
        }

        // Preserve `saved_rev` only if it equals `current_rev` or remains reachable in the kept window.
        let kept_saved_rev = self.saved_rev.filter(|&sr| {
            sr == self.current_rev
                || undo_slice
                    .iter()
                    .chain(redo_slice.iter())
                    .any(|e| e.rev_before == sr || e.rev_after == sr)
        });

        let mut out = Vec::with_capacity(used_bytes);
        out.extend_from_slice(&self.current_rev.to_le_bytes());
        match kept_saved_rev {
            Some(sr) => {
                out.push(1);
                out.extend_from_slice(&sr.to_le_bytes());
            }
            None => {
                out.push(0);
                out.extend_from_slice(&0u64.to_le_bytes());
            }
        }
        out.extend_from_slice(&(undo_slice.len() as u32).to_le_bytes());
        out.extend_from_slice(&(redo_slice.len() as u32).to_le_bytes());
        for e in undo_slice {
            encode_entry(e, &mut out);
        }
        for e in redo_slice {
            encode_entry(e, &mut out);
        }
        Some(out)
    }

    /// Decode a binary payload produced by [`Self::encode_bounded`] with strict bounds checking.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let mut cur = BinCursor(bytes);
        let current_rev = cur.read_u64()?;
        let has_saved = cur.read_u8()?;
        let raw_saved = cur.read_u64()?;
        let saved_rev = match has_saved {
            0 => None,
            1 => Some(raw_saved),
            _ => return None,
        };
        let undo_len = cur.read_u32()? as usize;
        let redo_len = cur.read_u32()? as usize;
        if undo_len > MAX_UNDO_STEPS || redo_len > MAX_UNDO_STEPS {
            return None;
        }

        let mut max_rev = current_rev.max(saved_rev.unwrap_or(0));
        let mut resident_bytes = 0usize;
        let mut undo = Vec::with_capacity(undo_len.min(256));
        for _ in 0..undo_len {
            let e = decode_entry(&mut cur)?;
            max_rev = max_rev.max(e.rev_before).max(e.rev_after);
            resident_bytes = resident_bytes.saturating_add(e.byte_cost);
            undo.push(e);
        }
        let mut redo = Vec::with_capacity(redo_len.min(256));
        for _ in 0..redo_len {
            let e = decode_entry(&mut cur)?;
            max_rev = max_rev.max(e.rev_before).max(e.rev_after);
            resident_bytes = resident_bytes.saturating_add(e.byte_cost);
            redo.push(e);
        }
        if !cur.0.is_empty() {
            return None;
        }
        Some(Self {
            undo,
            redo,
            resident_bytes,
            next_rev: max_rev.saturating_add(1).max(1),
            current_rev,
            saved_rev,
        })
    }
}

fn entry_encoded_size(e: &UndoEntry) -> usize {
    let mut n = 8 + 8 + 4;
    for op in &e.ops {
        n += 8 + 8 + 8 + 4 + op.deleted.len() + 4 + op.inserted.len();
    }
    n
}

fn encode_entry(e: &UndoEntry, out: &mut Vec<u8>) {
    out.extend_from_slice(&e.rev_before.to_le_bytes());
    out.extend_from_slice(&e.rev_after.to_le_bytes());
    out.extend_from_slice(&(e.ops.len() as u32).to_le_bytes());
    for op in &e.ops {
        out.extend_from_slice(&op.offset.to_le_bytes());
        out.extend_from_slice(&op.cursor_before.to_le_bytes());
        out.extend_from_slice(&op.cursor_after.to_le_bytes());
        out.extend_from_slice(&(op.deleted.len() as u32).to_le_bytes());
        out.extend_from_slice(&op.deleted);
        out.extend_from_slice(&(op.inserted.len() as u32).to_le_bytes());
        out.extend_from_slice(&op.inserted);
    }
}

fn decode_entry(cur: &mut BinCursor<'_>) -> Option<UndoEntry> {
    let rev_before = cur.read_u64()?;
    let rev_after = cur.read_u64()?;
    let op_count = cur.read_u32()? as usize;
    if op_count == 0 || op_count > cur.0.len() / 32 + 1 {
        return None;
    }
    let mut ops = Vec::with_capacity(op_count.min(64));
    let mut byte_cost = 0usize;
    for _ in 0..op_count {
        let offset = cur.read_u64()?;
        let cursor_before = cur.read_u64()?;
        let cursor_after = cur.read_u64()?;
        let del_len = cur.read_u32()? as usize;
        let deleted = cur.read_bytes(del_len)?.to_vec();
        let ins_len = cur.read_u32()? as usize;
        let inserted = cur.read_bytes(ins_len)?.to_vec();
        byte_cost = byte_cost.saturating_add(deleted.len() + inserted.len());
        ops.push(EditOp {
            offset,
            deleted,
            inserted,
            cursor_before,
            cursor_after,
        });
    }
    Some(UndoEntry {
        ops,
        timestamp_ms: 0,
        coalescable: false,
        byte_cost,
        rev_before,
        rev_after,
    })
}

struct BinCursor<'a>(&'a [u8]);

impl<'a> BinCursor<'a> {
    fn read_u8(&mut self) -> Option<u8> {
        let (&b, rest) = self.0.split_first()?;
        self.0 = rest;
        Some(b)
    }

    fn read_u32(&mut self) -> Option<u32> {
        if self.0.len() < 4 {
            return None;
        }
        let (a, rest) = self.0.split_at(4);
        self.0 = rest;
        Some(u32::from_le_bytes(a.try_into().ok()?))
    }

    fn read_u64(&mut self) -> Option<u64> {
        if self.0.len() < 8 {
            return None;
        }
        let (a, rest) = self.0.split_at(8);
        self.0 = rest;
        Some(u64::from_le_bytes(a.try_into().ok()?))
    }

    fn read_bytes(&mut self, len: usize) -> Option<&'a [u8]> {
        if self.0.len() < len {
            return None;
        }
        let (a, rest) = self.0.split_at(len);
        self.0 = rest;
        Some(a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_round_trip_and_direction_aware_trimming() {
        let mut stack = UndoStack::new();
        for i in 0..5u64 {
            stack.record(
                EditOp {
                    offset: i,
                    deleted: Vec::new(),
                    inserted: vec![b'a' + (i as u8)],
                    cursor_before: i,
                    cursor_after: i + 1,
                },
                i * 1000,
                false,
            );
        }
        // Mark saved after step 3, undo 1 step so both undo and redo are non-empty.
        stack.mark_saved();
        let _ = stack.pop_undo();
        assert!(stack.can_undo() && stack.can_redo() && stack.is_dirty());

        let encoded = stack.encode_bounded(256, 512 * 1024).unwrap();
        let mut decoded = UndoStack::decode(&encoded).unwrap();
        assert!(decoded.can_undo() && decoded.can_redo() && decoded.is_dirty());
        // Redo returns to saved_rev -> clean!
        let _ = decoded.pop_redo().unwrap();
        assert!(!decoded.is_dirty());

        // Tail-trim to max_steps=2 keeps 2 newest undo steps and drops saved_rev if unreachable.
        let trimmed = stack.encode_bounded(2, 512 * 1024).unwrap();
        let decoded_trim = UndoStack::decode(&trimmed).unwrap();
        assert_eq!(decoded_trim.undo.len(), 2);
        assert_eq!(decoded_trim.redo.len(), 0);
        assert_eq!(decoded_trim.saved_rev, None);

        // Truncated/corrupt bytes fail cleanly without panicking.
        assert!(UndoStack::decode(&encoded[..encoded.len() - 1]).is_none());
        assert!(UndoStack::decode(&[0xFF; 8]).is_none());
    }
}
