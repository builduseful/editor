// tests/piece_table_test.cpp — §5.2 piece table + add buffer.
#include <string>

#include "add_buffer.hpp"
#include "check.hpp"
#include "fake_fs.hpp"
#include "mem_source.hpp"
#include "piece_table.hpp"
#include "prng.hpp"

using ed::AddBuffer;
using ed::PieceTable;
using ed::Source;
using ed_test::FakeFileSystem;
using ed_test::MemSource;

namespace {

std::string read_all(const Source& s) {
  std::string out(static_cast<size_t>(s.size()), '\0');
  if (s.size() == 0) return out;
  size_t used = 0;
  Status st = s.read(0, s.size(), out.data(), out.size(), &used);
  (void)st;
  out.resize(used);
  return out;
}

}  // namespace

TEST(piece_table_basic) {
  FakeFileSystem fs;
  MemSource base("hello world\nsecond line\n");
  AddBuffer add;
  add.init(&fs, "/tmp");
  PieceTable pt;
  pt.reset(&base, &add, base.size());

  CHECK(pt.pristine());
  CHECK_EQ(read_all(pt), std::string("hello world\nsecond line\n"));

  std::string model = "hello world\nsecond line\n";

  auto ins = [&](size_t off, const std::string& t) {
    uint64_t aoff = 0;
    Status s = add.append(t.data(), t.size(), &aoff);
    CHECK(s == Status::Ok);
    pt.insert(off, aoff, t.size());
    model.insert(off, t);
    CHECK_EQ(read_all(pt), model);
  };
  auto del = [&](size_t off, size_t len) {
    pt.remove(off, len);
    model.erase(off, len);
    CHECK_EQ(read_all(pt), model);
  };

  ins(0, "X");
  ins(model.size(), "tail");
  ins(5, "MID");
  del(0, 1);
  del(3, 2);
  ins(model.size(), "\n");
  CHECK(!pt.pristine());

  // Undo the whole thing back to the base shape: one coalesced base piece.
  pt.remove(0, model.size());
  CHECK_EQ(pt.size(), 0u);
}

TEST(piece_table_coalesces_tail_inserts) {
  FakeFileSystem fs;
  MemSource base("");
  AddBuffer add;
  add.init(&fs, "/tmp");
  PieceTable pt;
  pt.reset(&base, &add, base.size());

  for (int i = 0; i < 50; ++i) {
    const std::string ch = "a";
    uint64_t aoff = 0;
    add.append(ch.data(), ch.size(), &aoff);
    pt.insert(pt.size(), aoff, ch.size());
  }
  CHECK_EQ(read_all(pt), std::string(50, 'a'));
  // One add piece, not fifty.
  CHECK_EQ(pt.pieces().size(), 1u);
}

TEST(piece_table_restores_pristine_after_undo) {
  FakeFileSystem fs;
  MemSource base("abcdef");
  AddBuffer add;
  add.init(&fs, "/tmp");
  PieceTable pt;
  pt.reset(&base, &add, base.size());

  uint64_t aoff = 0;
  const std::string t = "XY";
  add.append(t.data(), t.size(), &aoff);
  pt.insert(3, aoff, t.size());
  CHECK(!pt.pristine());
  pt.remove(3, 2);
  CHECK(pt.pristine());
  CHECK_EQ(read_all(pt), std::string("abcdef"));
}

TEST(add_buffer_spills_over_cap) {
  FakeFileSystem fs;
  AddBuffer add;
  add.init(&fs, "/tmp");
  const std::string big(AddBuffer::kResidentCap + 1000, 'z');
  uint64_t off = 0;
  Status s = add.append(big.data(), big.size(), &off);
  CHECK(s == Status::Ok);
  CHECK(add.spilled());
  CHECK_EQ(off, 0u);
  CHECK_EQ(add.size(), big.size());

  // Read across the spill boundary.
  std::string got(big.size(), '\0');
  size_t used = 0;
  s = add.read(AddBuffer::kResidentCap - 10, 20, got.data(), 20, &used);
  CHECK(s == Status::Ok);
  CHECK_EQ(used, 20u);
  CHECK(got.compare(0, 20, big.substr(AddBuffer::kResidentCap - 10, 20)) == 0);
}

TEST(piece_table_random_model) {
  ed_test::Prng rng(0xC0FFEE);
  FakeFileSystem fs;
  MemSource base("line one\nline two\r\nline three\rline four\n");
  AddBuffer add;
  add.init(&fs, "/tmp");
  PieceTable pt;
  pt.reset(&base, &add, base.size());
  std::string model = "line one\nline two\r\nline three\rline four\n";

  for (int iter = 0; iter < 2000; ++iter) {
    const bool do_insert = rng.below(2) == 0;
    const size_t off = static_cast<size_t>(rng.below(model.size() + 1));
    if (do_insert) {
      const size_t n = static_cast<size_t>(rng.below(5));
      std::string t;
      for (size_t i = 0; i < n; ++i) {
        static const char alpha[] = "ab\n\r";
        t.push_back(alpha[rng.below(sizeof(alpha) - 1)]);
      }
      uint64_t aoff = 0;
      add.append(t.data(), t.size(), &aoff);
      pt.insert(off, aoff, t.size());
      model.insert(off, t);
    } else if (!model.empty()) {
      const size_t max_len = std::min<size_t>(4, model.size() - off);
      if (max_len == 0) continue;
      const size_t len = static_cast<size_t>(rng.below(max_len) + 1);
      pt.remove(off, len);
      model.erase(off, len);
    }
    if (iter % 50 == 0) CHECK_EQ(read_all(pt), model);
  }
  CHECK_EQ(read_all(pt), model);
}
