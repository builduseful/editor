#include "line_index.hpp"

#include <algorithm>
#include <utility>

#include "line_stream.hpp"

namespace ed {

LineIndex::LineIndex() {
  root_ = std::make_unique<Node>();
  root_->leaf = true;
}

LineIndex::~LineIndex() = default;

LineIndex::Sum LineIndex::node_sum(const Node* n) const {
  Sum s;
  if (n->leaf) {
    for (const LineChunk& c : n->chunks) {
      s.lines += c.lines;
      s.bytes += c.bytes;
    }
  } else {
    for (const Sum& k : n->sums) {
      s.lines += k.lines;
      s.bytes += k.bytes;
    }
  }
  return s;
}

void LineIndex::recompute(Node* n) {
  if (n->leaf) return;
  for (std::unique_ptr<Node>& k : n->kids) recompute(k.get());
  n->sums.assign(n->kids.size(), Sum{});
  for (size_t i = 0; i < n->kids.size(); ++i) {
    n->sums[i] = node_sum(n->kids[i].get());
  }
}

void LineIndex::adopt_root(std::vector<std::unique_ptr<Node>> level) {
  if (level.empty()) {
    root_ = std::make_unique<Node>();
    root_->leaf = true;
  } else {
    while (level.size() > 1) {
      std::vector<std::unique_ptr<Node>> next;
      for (size_t i = 0; i < level.size(); i += kMaxKids) {
        auto node = std::make_unique<Node>();
        node->leaf = false;
        const size_t end = std::min(level.size(), i + kMaxKids);
        for (size_t j = i; j < end; ++j) node->kids.push_back(std::move(level[j]));
        next.push_back(std::move(node));
      }
      level = std::move(next);
    }
    root_ = std::move(level[0]);
  }
  recompute(root_.get());
  const Sum s = node_sum(root_.get());
  total_lines_ = s.lines;
  total_bytes_ = s.bytes;
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

Status LineIndex::build(const Source& doc) {
  std::vector<LineChunk> flat;
  Status s = rescan(doc, 0, doc.size(), &flat);
  if (s != Status::Ok) return s;

  std::vector<std::unique_ptr<Node>> level;
  for (size_t i = 0; i < flat.size(); i += kMaxChunks) {
    auto leaf = std::make_unique<Node>();
    leaf->leaf = true;
    const size_t end = std::min(flat.size(), i + kMaxChunks);
    leaf->chunks.assign(flat.begin() + static_cast<std::ptrdiff_t>(i),
                        flat.begin() + static_cast<std::ptrdiff_t>(end));
    level.push_back(std::move(leaf));
  }
  adopt_root(std::move(level));
  return Status::Ok;
}

void LineIndex::locate_byte(uint64_t off, Loc* out) const {
  Node* n = root_.get();
  uint64_t acc_b = 0, acc_l = 0;
  while (!n->leaf) {
    size_t i = 0;
    while (i + 1 < n->kids.size() && off >= acc_b + n->sums[i].bytes) {
      acc_b += n->sums[i].bytes;
      acc_l += n->sums[i].lines;
      ++i;
    }
    n = n->kids[i].get();
  }
  size_t i = 0;
  while (i + 1 < n->chunks.size() && off >= acc_b + n->chunks[i].bytes) {
    acc_b += n->chunks[i].bytes;
    acc_l += n->chunks[i].lines;
    ++i;
  }
  out->leaf = n;
  out->index = i;
  out->start_byte = acc_b;
  out->start_line = acc_l;
}

Status LineIndex::line_start_in(const Source& doc, uint64_t within,
                                uint64_t start_byte, uint64_t* out) const {
  LineStream ls(doc, start_byte);
  uint64_t pos = start_byte;
  for (uint64_t i = 0; i < within; ++i) {
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

Status LineIndex::line_start(const Source& doc, int64_t line,
                             uint64_t* out) const {
  if (line < 0 || static_cast<uint64_t>(line) > total_lines_) {
    return Status::Range;
  }
  if (static_cast<uint64_t>(line) == total_lines_) {
    *out = total_bytes_;
    return Status::Ok;
  }
  const uint64_t want = static_cast<uint64_t>(line);
  Node* n = root_.get();
  uint64_t acc_lines = 0, acc_bytes = 0;
  while (!n->leaf) {
    size_t i = 0;
    while (i + 1 < n->kids.size() && want >= acc_lines + n->sums[i].lines) {
      acc_lines += n->sums[i].lines;
      acc_bytes += n->sums[i].bytes;
      ++i;
    }
    n = n->kids[i].get();
  }
  size_t i = 0;
  while (i + 1 < n->chunks.size() && want >= acc_lines + n->chunks[i].lines) {
    acc_lines += n->chunks[i].lines;
    acc_bytes += n->chunks[i].bytes;
    ++i;
  }
  return line_start_in(doc, want - acc_lines, acc_bytes, out);
}

Status LineIndex::line_of_byte(const Source& doc, uint64_t off,
                               int64_t* out) const {
  if (off >= total_bytes_) {
    *out = static_cast<int64_t>(total_lines_);
    return Status::Ok;
  }
  Loc loc;
  locate_byte(off, &loc);
  LineStream ls(doc, loc.start_byte);
  uint64_t pos = loc.start_byte;
  uint64_t line = loc.start_line;
  while (true) {
    bool has_line = false;
    uint64_t line_bytes = 0;
    bool has_term = false;
    Status s = ls.next(&has_line, &line_bytes, &has_term);
    if (s != Status::Ok) return s;
    if (!has_line) break;
    if (pos + line_bytes > off) {
      *out = static_cast<int64_t>(line);
      return Status::Ok;
    }
    pos += line_bytes;
    line += 1;
  }
  *out = static_cast<int64_t>(line);
  return Status::Ok;
}

std::vector<std::unique_ptr<LineIndex::Node>> LineIndex::splice(
    Node* n, uint64_t base, uint64_t from, uint64_t to,
    const std::vector<LineChunk>& ins, bool* inserted) {
  std::vector<std::unique_ptr<Node>> result;

  if (n->leaf) {
    uint64_t leaf_end = base;
    for (const LineChunk& c : n->chunks) leaf_end += c.bytes;
    const bool insert_here = !*inserted && from >= base && from < leaf_end;

    std::vector<LineChunk> content;
    content.reserve(n->chunks.size() + ins.size());
    uint64_t cb = base;
    size_t before_count = 0;
    for (const LineChunk& c : n->chunks) {
      if (cb < from) {
        content.push_back(c);
        ++before_count;
      } else if (cb >= to) {
        content.push_back(c);
      }
      cb += c.bytes;
    }
    if (insert_here) {
      content.insert(content.begin() + static_cast<std::ptrdiff_t>(before_count),
                     ins.begin(), ins.end());
      *inserted = true;
    }
    for (size_t i = 0; i < content.size(); i += kMaxChunks) {
      auto leaf = std::make_unique<Node>();
      leaf->leaf = true;
      const size_t end = std::min(content.size(), i + kMaxChunks);
      leaf->chunks.assign(content.begin() + static_cast<std::ptrdiff_t>(i),
                          content.begin() + static_cast<std::ptrdiff_t>(end));
      result.push_back(std::move(leaf));
    }
    return result;
  }

  std::vector<std::unique_ptr<Node>> kids;
  uint64_t cb = base;
  for (size_t i = 0; i < n->kids.size(); ++i) {
    const uint64_t kid_end = cb + n->sums[i].bytes;
    const bool removal = kid_end > from && cb < to;
    const bool insertion = !*inserted && from >= cb && from < kid_end;
    if (removal || insertion) {
      auto sub = splice(n->kids[i].get(), cb, from, to, ins, inserted);
      for (std::unique_ptr<Node>& s : sub) kids.push_back(std::move(s));
    } else {
      kids.push_back(std::move(n->kids[i]));
    }
    cb = kid_end;
  }
  for (size_t i = 0; i < kids.size(); i += kMaxKids) {
    auto node = std::make_unique<Node>();
    node->leaf = false;
    const size_t end = std::min(kids.size(), i + kMaxKids);
    for (size_t j = i; j < end; ++j) node->kids.push_back(std::move(kids[j]));
    result.push_back(std::move(node));
  }
  return result;
}

Status LineIndex::merge_boundary(const Source& doc, uint64_t boundary) {
  if (boundary == 0 || boundary >= total_bytes_) return Status::Ok;
  Loc a;
  locate_byte(boundary - 1, &a);
  Loc b;
  locate_byte(boundary, &b);
  const LineChunk before = a.leaf->chunks[a.index];
  const LineChunk after = b.leaf->chunks[b.index];
  if (before.lines >= kMinChunkLines) return Status::Ok;
  if (static_cast<uint64_t>(before.lines) + after.lines > kChunkLines) {
    return Status::Ok;
  }
  const uint64_t end = boundary + after.bytes;
  std::vector<LineChunk> merged;
  Status s = rescan(doc, a.start_byte, end, &merged);
  if (s != Status::Ok) return s;
  bool inserted = false;
  auto level = splice(root_.get(), 0, a.start_byte, end, merged, &inserted);
  adopt_root(std::move(level));
  return Status::Ok;
}

size_t LineIndex::count_nodes(const Node* n) {
  if (n->leaf) return 1;
  size_t total = 1;
  for (const std::unique_ptr<Node>& k : n->kids) total += count_nodes(k.get());
  return total;
}

size_t LineIndex::count_chunks(const Node* n) {
  if (n->leaf) return n->chunks.size();
  size_t total = 0;
  for (const std::unique_ptr<Node>& k : n->kids) total += count_chunks(k.get());
  return total;
}

size_t LineIndex::chunk_count() const { return count_chunks(root_.get()); }
size_t LineIndex::node_count() const { return count_nodes(root_.get()); }

bool LineIndex::check(const Node* n, size_t depth, size_t* leaf_depth) const {
  if (n->leaf) {
    if (*leaf_depth == SIZE_MAX) {
      *leaf_depth = depth;
    } else if (*leaf_depth != depth) {
      return false;
    }
    if (n->chunks.size() > kMaxChunks) return false;
    for (const LineChunk& c : n->chunks) {
      if (c.lines == 0 || c.lines > kChunkLines) return false;
    }
    return true;
  }
  if (n->kids.empty() || n->kids.size() > kMaxKids) return false;
  if (n->sums.size() != n->kids.size()) return false;
  for (size_t i = 0; i < n->kids.size(); ++i) {
    const Sum want = node_sum(n->kids[i].get());
    if (n->sums[i].lines != want.lines || n->sums[i].bytes != want.bytes) {
      return false;
    }
    if (!check(n->kids[i].get(), depth + 1, leaf_depth)) return false;
  }
  return true;
}

bool LineIndex::validate_tree() const {
  if (root_ == nullptr) return false;
  size_t leaf_depth = SIZE_MAX;
  if (!check(root_.get(), 0, &leaf_depth)) return false;
  const Sum s = node_sum(root_.get());
  return s.lines == total_lines_ && s.bytes == total_bytes_;
}

Status LineIndex::apply_edit(const Source& doc, uint64_t off, uint64_t removed,
                             uint64_t inserted) {
  if (removed == 0 && inserted == 0) return Status::Ok;
  const uint64_t old_size = total_bytes_;
  if (off > old_size || removed > old_size - off) return Status::Range;

  // Empty document: rebuild from the inserted bytes.
  if (total_bytes_ == 0) {
    std::vector<LineChunk> fresh;
    Status s = rescan(doc, 0, inserted, &fresh);
    if (s != Status::Ok) return s;
    std::vector<std::unique_ptr<Node>> level;
    for (size_t i = 0; i < fresh.size(); i += kMaxChunks) {
      auto leaf = std::make_unique<Node>();
      leaf->leaf = true;
      const size_t end = std::min(fresh.size(), i + kMaxChunks);
      leaf->chunks.assign(fresh.begin() + static_cast<std::ptrdiff_t>(i),
                          fresh.begin() + static_cast<std::ptrdiff_t>(end));
      level.push_back(std::move(leaf));
    }
    adopt_root(std::move(level));
    return Status::Ok;
  }

  Loc start;
  locate_byte(off, &start);
  const uint64_t s0 = start.start_byte;

  // The rescan must cover the whole of the last affected line, so `s1` is the
  // end of the chunk containing the edit's last byte (or the chunk containing
  // the insertion point, for a pure insert). Taking "the first chunk start at or
  // after the edit end" instead would leave an empty range for a pure insert at
  // a chunk boundary, splicing a second chunk over the same bytes.
  const uint64_t last_off = removed > 0 ? off + removed - 1 : off;
  Loc last;
  locate_byte(last_off, &last);
  uint64_t s1 = last.start_byte + last.leaf->chunks[last.index].bytes;
  if (off + removed >= old_size) s1 = old_size;

  const uint64_t new_end = s1 - removed + inserted;
  std::vector<LineChunk> fresh;
  Status s = rescan(doc, s0, new_end, &fresh);
  if (s != Status::Ok) return s;

  bool inserted_flag = false;
  auto level = splice(root_.get(), 0, s0, s1, fresh, &inserted_flag);
  if (!inserted_flag && !fresh.empty()) {
    auto leaf = std::make_unique<Node>();
    leaf->leaf = true;
    leaf->chunks = fresh;
    level.push_back(std::move(leaf));
  }
  adopt_root(std::move(level));

  Status ms = merge_boundary(doc, s0);
  if (ms != Status::Ok) return ms;
  ms = merge_boundary(doc, new_end);
  if (ms != Status::Ok) return ms;
  return Status::Ok;
}

}  // namespace ed
