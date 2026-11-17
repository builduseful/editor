// core/src/line_index.hpp — sparse line index (§5.3).
//
// A balanced 63-way order-statistic B-tree of line-aligned chunks. Each chunk
// covers up to 64 lines and stores its line count and byte length; every node
// stores subtree sums of both so a line maps to a byte offset in O(log n) plus
// an in-chunk scan of at most 64 lines. Chunks split at 64 lines and are merged
// below 32. An edit rescanns the affected span and splices it back in, touching
// only the path to the edit (O(changed_lines/64 + log n) chunk operations).
#pragma once

#include <cstdint>
#include <memory>
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
  static constexpr size_t kMaxChunks = 62;        // 62 descriptors + 63 sums
  static constexpr size_t kMaxKids = 63;

  LineIndex();
  ~LineIndex();
  LineIndex(const LineIndex&) = delete;
  LineIndex& operator=(const LineIndex&) = delete;

  // Builds the index by scanning `doc` from the start.
  Status build(const Source& doc);

  int64_t line_count() const { return static_cast<int64_t>(total_lines_); }
  uint64_t byte_count() const { return total_bytes_; }

  // Byte offset of the start of line `line` (0-based). `line == line_count()`
  // yields the document size. Scans at most one chunk.
  Status line_start(const Source& doc, int64_t line, uint64_t* out) const;

  // 0-based line containing byte `off` (off == byte_count() -> line_count).
  // Scans at most one chunk.
  Status line_of_byte(const Source& doc, uint64_t off, int64_t* out) const;

  // Updates the index after replacing old-document bytes [off, off+removed)
  // with `inserted` bytes. `doc` is the NEW document.
  Status apply_edit(const Source& doc, uint64_t off, uint64_t removed,
                    uint64_t inserted);

  // Diagnostics for tests and the §7 budget checks.
  size_t chunk_count() const;
  size_t node_count() const;
  // Verifies node capacities, subtree sums, uniform leaf depth, and totals.
  bool validate_tree() const;

 private:
  struct Sum {
    uint64_t lines = 0;
    uint64_t bytes = 0;
  };
  struct Node {
    bool leaf = true;
    std::vector<LineChunk> chunks;            // leaf
    std::vector<std::unique_ptr<Node>> kids;  // internal
    std::vector<Sum> sums;                    // internal, parallel to kids
  };

  struct Loc {
    Node* leaf = nullptr;
    size_t index = 0;      // chunk index within the leaf
    uint64_t start_byte = 0;
    uint64_t start_line = 0;
  };

  static Sum chunk_sum(const LineChunk& c) { return Sum{c.lines, c.bytes}; }
  Sum node_sum(const Node* n) const;
  void recompute(Node* n);
  void adopt_root(std::vector<std::unique_ptr<Node>> level);

  Status rescan(const Source& doc, uint64_t start, uint64_t end,
                std::vector<LineChunk>* out) const;
  void locate_byte(uint64_t off, Loc* out) const;
  Status line_start_in(const Source& doc, uint64_t within, uint64_t start_byte,
                       uint64_t* out) const;

  // Replaces the chunks whose start byte lies in [from, to) with `ins`, which
  // is inserted at `from`. Returns the replacement nodes at `n`'s level.
  std::vector<std::unique_ptr<Node>> splice(Node* n, uint64_t base,
                                            uint64_t from, uint64_t to,
                                            const std::vector<LineChunk>& ins,
                                            bool* inserted);
  Status merge_boundary(const Source& doc, uint64_t boundary);

  static size_t count_nodes(const Node* n);
  static size_t count_chunks(const Node* n);
  bool check(const Node* n, size_t depth, size_t* leaf_depth) const;

  std::unique_ptr<Node> root_;
  uint64_t total_lines_ = 0;
  uint64_t total_bytes_ = 0;
};

}  // namespace ed
