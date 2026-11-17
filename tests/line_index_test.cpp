// tests/line_index_test.cpp — §5.3 sparse line index.
#include <algorithm>
#include <string>

#include "check.hpp"
#include "line_index.hpp"
#include "mem_source.hpp"
#include "naive_lines.hpp"
#include "prng.hpp"

using ed::LineIndex;
using ed_test::MemSource;

namespace {

void expect_matches(const LineIndex& idx, const std::string& s) {
  CHECK(idx.validate_tree());
  const std::vector<uint64_t> starts = ed_test::naive_line_starts(s);
  CHECK_EQ(idx.line_count(), static_cast<int64_t>(starts.size()) - 1);
  CHECK_EQ(idx.byte_count(), static_cast<uint64_t>(s.size()));
  MemSource src(s);
  for (int64_t L = 0; L <= idx.line_count(); ++L) {
    uint64_t off = 0;
    Status st = idx.line_start(src, L, &off);
    CHECK(st == Status::Ok);
    CHECK_EQ(off, starts[static_cast<size_t>(L)]);
  }
}

}  // namespace

TEST(line_index_conventions) {
  const char* cases[] = {"",         "a",     "a\n",    "a\n\n", "\n",
                         "a\r\n",    "a\r",   "a\r\nb", "x\ny\n", "a\r\n\r\n"};
  for (const char* c : cases) {
    std::string s(c);
    MemSource src(s);
    LineIndex idx;
    CHECK(idx.build(src) == Status::Ok);
    expect_matches(idx, s);
  }
}

TEST(line_index_large_document) {
  std::string s;
  for (int i = 0; i < 5000; ++i) {
    s += "line ";
    s += std::to_string(i);
    s += (i % 3 == 0) ? "\r\n" : "\n";
  }
  MemSource src(s);
  LineIndex idx;
  CHECK(idx.build(src) == Status::Ok);
  CHECK_EQ(idx.line_count(), 5000);
  expect_matches(idx, s);
}

TEST(line_index_random_edits) {
  ed_test::Prng rng(0xBADC0DE);
  std::string model = "alpha\nbeta\r\ngamma\rdelta\n";
  {
    MemSource src(model);
    LineIndex idx;
    CHECK(idx.build(src) == Status::Ok);
    expect_matches(idx, model);

    for (int iter = 0; iter < 1500; ++iter) {
      const size_t off = static_cast<size_t>(rng.below(model.size() + 1));
      const size_t max_rem = std::min<size_t>(6, model.size() - off);
      const size_t removed =
          max_rem == 0 ? 0 : static_cast<size_t>(rng.below(max_rem) + 1);
      const size_t n = static_cast<size_t>(rng.below(5));
      std::string inserted;
      for (size_t i = 0; i < n; ++i) {
        static const char alpha[] = "ab\n\r";
        inserted.push_back(alpha[rng.below(sizeof(alpha) - 1)]);
      }
      model = model.substr(0, off) + inserted + model.substr(off + removed);

      MemSource next(model);
      Status st = idx.apply_edit(next, off, removed, inserted.size());
      CHECK(st == Status::Ok);
      if (iter % 25 == 0) expect_matches(idx, model);
    }
    expect_matches(idx, model);
  }
}

TEST(line_index_grows_from_empty) {
  std::string model;
  MemSource empty(model);
  LineIndex idx;
  CHECK(idx.build(empty) == Status::Ok);
  CHECK_EQ(idx.line_count(), 0);

  const std::string inserted = "hello\nworld\n";
  model += inserted;
  MemSource next(model);
  CHECK(idx.apply_edit(next, 0, 0, inserted.size()) == Status::Ok);
  CHECK_EQ(idx.line_count(), 2);
  expect_matches(idx, model);
}

TEST(line_index_chunk_occupancy_stays_bounded) {
  // Typing one line at a time at EOF must not create a chunk per line.
  std::string model;
  {
    MemSource src(model);
    LineIndex idx;
    CHECK(idx.build(src) == Status::Ok);
    for (int i = 0; i < 2000; ++i) {
      const std::string inserted = "x\n";
      const size_t off = model.size();
      model += inserted;
      MemSource next(model);
      CHECK(idx.apply_edit(next, off, 0, inserted.size()) == Status::Ok);
    }
    CHECK_EQ(idx.line_count(), 2000);
    expect_matches(idx, model);
    // 2000 lines at <= 64 lines/chunk is at most ~32 chunks plus slack.
    CHECK(idx.chunk_count() <= 64);
    CHECK(idx.node_count() <= 8);
  }
}

TEST(line_index_crlf_across_buffer_boundary) {
  // A CRLF whose '\r' is the last byte of the scanner buffer must still count
  // as one terminator (regression: refill saw scan_ == buf_len_ + 1).
  const size_t block = 16 * 1024;
  for (size_t lead : {block - 1, block - 2, block, 2 * block - 1}) {
    std::string s(lead, 'a');
    s += "\r\n";
    s += "tail\n";
    MemSource src(s);
    LineIndex idx;
    CHECK(idx.build(src) == Status::Ok);
    expect_matches(idx, s);
    CHECK_EQ(idx.line_count(), 2);
  }
}

TEST(line_index_multi_chunk_edits) {
  // Edits that add and remove many lines at once exercise splits and merges.
  ed_test::Prng rng(0x1234ABCD);
  std::string model;
  for (int i = 0; i < 500; ++i) model += "seed\n";
  MemSource src(model);
  LineIndex idx;
  CHECK(idx.build(src) == Status::Ok);
  expect_matches(idx, model);

  for (int iter = 0; iter < 200; ++iter) {
    const size_t off = static_cast<size_t>(rng.below(model.size() + 1));
    const size_t max_rem = std::min<size_t>(200, model.size() - off);
    const size_t removed =
        max_rem == 0 ? 0 : static_cast<size_t>(rng.below(max_rem) + 1);
    std::string inserted;
    const size_t n = static_cast<size_t>(rng.below(120));
    for (size_t i = 0; i < n; ++i) inserted += (rng.below(4) == 0) ? "long\n" : "z\n";
    model = model.substr(0, off) + inserted + model.substr(off + removed);

    MemSource next(model);
    CHECK(idx.apply_edit(next, off, removed, inserted.size()) == Status::Ok);
    if (iter % 10 == 0) expect_matches(idx, model);
  }
  expect_matches(idx, model);
}

TEST(line_index_pure_insert_at_chunk_boundary) {
  // Regression: a pure insert at a chunk start must rescan the whole affected
  // line. Taking "the first chunk start at or after the edit end" gave an empty
  // range and spliced a second chunk over the same bytes, corrupting the index.
  std::string model;
  for (int i = 0; i < 200; ++i) model += "line\n";
  MemSource src(model);
  LineIndex idx;
  CHECK(idx.build(src) == Status::Ok);
  expect_matches(idx, model);

  for (int i = 0; i < 10; ++i) {  // at the very start (chunk 0's boundary)
    const std::string ins = "Z";
    model.insert(0, ins);
    MemSource next(model);
    CHECK(idx.apply_edit(next, 0, 0, ins.size()) == Status::Ok);
    expect_matches(idx, model);
  }
  for (int i = 0; i < 10; ++i) {  // at a mid-document line start (64-line chunk)
    const std::vector<uint64_t> starts = ed_test::naive_line_starts(model);
    const uint64_t off = starts[64];
    const std::string ins = "Q\n";
    model.insert(off, ins);
    MemSource next(model);
    CHECK(idx.apply_edit(next, off, 0, ins.size()) == Status::Ok);
    expect_matches(idx, model);
  }
}
