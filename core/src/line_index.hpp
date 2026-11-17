// core/src/line_index.hpp — sparse line index (§5.3).
//
// Line-aligned chunks of up to 64 lines, with order-statistic prefix sums so a
// line maps to a byte offset in O(log n) plus an in-chunk scan of at most 64
// lines. Built lazily and maintained across edits by rescanning the affected
// span and splicing it back in.
//
// NOTE: this M0a implementation keeps chunk descriptors in a flat vector with
// prefix sums (28 B/chunk, the same per-chunk cost §5.3 computes for the
// 63-way B-tree, so the §7 memory budgets hold). Structural updates are
// O(#chunks) rather than the B-tree's O(log n); the API here is structure-
// agnostic so the B-tree can replace the internals without touching callers.
// See the D7 note in the M0a commit message.
#pragma once

#include <cstdint>
#include <vector>

#include "source.hpp"

namespace ed {

struct LineChunk {
  uint32_t lines = 0;
  uint64_t bytes = 0;  // bytes of those lines including terminators
};

class LineIndex {
 public:
  static constexpr uint32_t kChunkLines = 64;     // split at 64 (§5.3)
  static constexpr uint32_t kMinChunkLines = 32;  // merge below 32 (§5.3)

  LineIndex() = default;

  // Builds the index by scanning `doc` from the start.
  Status build(const Source& doc);

  int64_t line_count() const { return static_cast<int64_t>(total_lines_); }
  uint64_t byte_count() const { return total_bytes_; }

  // Byte offset of the start of line `line` (0-based). `line == line_count()`
  // yields the document size. Scans at most one chunk.
  Status line_start(const Source& doc, int64_t line, uint64_t* out) const;

  // Updates the index after replacing old-document bytes [off, off+removed)
  // with `inserted` bytes. `doc` is the NEW document.
  Status apply_edit(const Source& doc, uint64_t off, uint64_t removed,
                    uint64_t inserted);

 private:
  Status rescan(const Source& doc, uint64_t start, uint64_t end,
                std::vector<LineChunk>* out) const;
  Status merge_chunks(const Source& doc, size_t i);
  void rebuild_prefix();
  size_t chunk_for_line(uint64_t line) const;
  size_t chunk_for_byte(uint64_t off) const;

  std::vector<LineChunk> chunks_;
  std::vector<uint64_t> prefix_lines_;  // size chunks_.size()+1
  std::vector<uint64_t> prefix_bytes_;  // size chunks_.size()+1
  uint64_t total_lines_ = 0;
  uint64_t total_bytes_ = 0;
};

}  // namespace ed
