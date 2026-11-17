#include "line_index.hpp"

#include <algorithm>

#include "line_stream.hpp"

namespace ed {

Status LineIndex::build(const Source& doc) {
  chunks_.clear();
  Status s = rescan(doc, 0, doc.size(), &chunks_);
  if (s != Status::Ok) return s;
  rebuild_prefix();
  return Status::Ok;
}

Status LineIndex::rescan(const Source& doc, uint64_t start, uint64_t end,
                         std::vector<LineChunk>* out) const {
  LineStream ls(doc, start);
  LineChunk cur;
  uint64_t pos = start;
  while (pos < end) {
    bool has_line = false;
    uint64_t line_bytes = 0;
    bool has_term = false;
    Status s = ls.next(&has_line, &line_bytes, &has_term);
    if (s != Status::Ok) return s;
    if (!has_line) break;
    cur.lines += 1;
    cur.bytes += line_bytes;
    pos += line_bytes;
    if (cur.lines == kChunkLines) {
      out->push_back(cur);
      cur = LineChunk{};
    }
  }
  if (cur.lines > 0) out->push_back(cur);
  return Status::Ok;
}

void LineIndex::rebuild_prefix() {
  prefix_lines_.assign(chunks_.size() + 1, 0);
  prefix_bytes_.assign(chunks_.size() + 1, 0);
  for (size_t i = 0; i < chunks_.size(); ++i) {
    prefix_lines_[i + 1] = prefix_lines_[i] + chunks_[i].lines;
    prefix_bytes_[i + 1] = prefix_bytes_[i] + chunks_[i].bytes;
  }
  total_lines_ = prefix_lines_.back();
  total_bytes_ = prefix_bytes_.back();
}

size_t LineIndex::chunk_for_line(uint64_t line) const {
  // greatest c with prefix_lines_[c] <= line
  size_t lo = 0, hi = chunks_.size();
  while (lo + 1 < hi) {
    size_t mid = lo + (hi - lo) / 2;
    if (prefix_lines_[mid] <= line) {
      lo = mid;
    } else {
      hi = mid;
    }
  }
  return lo;
}

size_t LineIndex::chunk_for_byte(uint64_t off) const {
  size_t lo = 0, hi = chunks_.size();
  while (lo + 1 < hi) {
    size_t mid = lo + (hi - lo) / 2;
    if (prefix_bytes_[mid] <= off) {
      lo = mid;
    } else {
      hi = mid;
    }
  }
  return lo;
}

Status LineIndex::line_start(const Source& doc, int64_t line,
                             uint64_t* out) const {
  if (line < 0 || static_cast<uint64_t>(line) > total_lines_) {
    return Status::Range;
  }
  if (static_cast<uint64_t>(line) == total_lines_) {
    *out = total_bytes_;
    return Status::Ok;
  }
  const size_t c = chunk_for_line(static_cast<uint64_t>(line));
  const uint64_t walk = static_cast<uint64_t>(line) - prefix_lines_[c];
  uint64_t pos = prefix_bytes_[c];
  LineStream ls(doc, pos);
  for (uint64_t i = 0; i < walk; ++i) {
    bool has_line = false;
    uint64_t line_bytes = 0;
    bool has_term = false;
    Status s = ls.next(&has_line, &line_bytes, &has_term);
    if (s != Status::Ok) return s;
    if (!has_line) return Status::Internal;
    pos += line_bytes;
  }
  *out = pos;
  return Status::Ok;
}

Status LineIndex::merge_chunks(const Source& doc, size_t i) {
  // Rescans [start_i, start_{i+2}) and replaces chunks i and i+1.
  std::vector<LineChunk> merged;
  Status s = rescan(doc, prefix_bytes_[i], prefix_bytes_[i + 2], &merged);
  if (s != Status::Ok) return s;
  chunks_.erase(chunks_.begin() + static_cast<std::ptrdiff_t>(i),
                chunks_.begin() + static_cast<std::ptrdiff_t>(i) + 2);
  chunks_.insert(chunks_.begin() + static_cast<std::ptrdiff_t>(i),
                 merged.begin(), merged.end());
  rebuild_prefix();
  return Status::Ok;
}

Status LineIndex::apply_edit(const Source& doc, uint64_t off, uint64_t removed,
                             uint64_t inserted) {
  if (removed == 0 && inserted == 0) return Status::Ok;
  const uint64_t old_size = total_bytes_;
  if (off > old_size || removed > old_size - off) return Status::Range;

  // First chunk to rebuild: the one containing `off` (or the last, if the edit
  // is an append at EOF).
  size_t c0 = 0;
  uint64_t s0 = 0;
  if (chunks_.empty()) {
    c0 = 0;
    s0 = 0;
  } else if (off < old_size) {
    c0 = chunk_for_byte(off);
    s0 = prefix_bytes_[c0];
  } else {
    c0 = chunks_.size() - 1;
    s0 = prefix_bytes_[c0];
  }

  // Smallest old chunk start >= off+removed (the unchanged tail begins there).
  const uint64_t edit_end = off + removed;
  size_t c1 = 0;
  uint64_t s1 = old_size;
  if (!chunks_.empty() && edit_end < old_size) {
    const size_t c = chunk_for_byte(edit_end);
    if (prefix_bytes_[c] == edit_end) {
      c1 = c;
      s1 = edit_end;
    } else {
      c1 = c + 1;
      s1 = prefix_bytes_[c + 1];
    }
  } else {
    c1 = chunks_.size();
    s1 = old_size;
  }

  const uint64_t new_end = s1 - removed + inserted;
  std::vector<LineChunk> fresh;
  Status s = rescan(doc, s0, new_end, &fresh);
  if (s != Status::Ok) return s;

  chunks_.erase(chunks_.begin() + static_cast<std::ptrdiff_t>(c0),
                chunks_.begin() + static_cast<std::ptrdiff_t>(c1));
  chunks_.insert(chunks_.begin() + static_cast<std::ptrdiff_t>(c0),
                 fresh.begin(), fresh.end());
  rebuild_prefix();

  // Local fixups: re-merge a boundary chunk that dropped below the minimum.
  if (!fresh.empty()) {
    const size_t last = c0 + fresh.size() - 1;
    if (last + 1 < chunks_.size() &&
        chunks_[last].lines < kMinChunkLines &&
        chunks_[last].lines + chunks_[last + 1].lines <= kChunkLines) {
      Status ms = merge_chunks(doc, last);
      if (ms != Status::Ok) return ms;
    }
    if (c0 > 0 && chunks_[c0].lines < kMinChunkLines &&
        chunks_[c0 - 1].lines + chunks_[c0].lines <= kChunkLines) {
      Status ms = merge_chunks(doc, c0 - 1);
      if (ms != Status::Ok) return ms;
    }
  }
  return Status::Ok;
}

}  // namespace ed
