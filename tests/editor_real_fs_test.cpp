// tests/editor_real_fs_test.cpp — core Editor against the real macOS FileSystem.
#include <algorithm>
#include <chrono>
#include <cstdio>
#include <filesystem>
#include <memory>
#include <string>
#include <vector>

#include "check.hpp"
#include "editor.hpp"
#include "prng.hpp"

namespace {

std::string make_temp_dir() {
  static int counter = 0;
  const auto now = std::chrono::steady_clock::now().time_since_epoch().count();
  const auto base = std::filesystem::temp_directory_path();
  const auto d = base / ("editor_test_" + std::to_string(now) + "_" +
                         std::to_string(counter++));
  std::error_code ec;
  std::filesystem::create_directory(d, ec);
  return ec ? std::string() : d.string();
}

void write_file(const std::string& path, const std::string& data) {
  std::FILE* f = std::fopen(path.c_str(), "wb");
  if (f == nullptr) return;
  std::fwrite(data.data(), 1, data.size(), f);
  std::fclose(f);
}

std::string read_file(const std::string& path) {
  std::FILE* f = std::fopen(path.c_str(), "rb");
  if (f == nullptr) return "<missing>";
  std::string out;
  char buf[4096];
  size_t n = 0;
  while ((n = std::fread(buf, 1, sizeof(buf), f)) > 0) out.append(buf, n);
  std::fclose(f);
  return out;
}

// Reads the whole logical document via read_lines.
std::string doc_of(Editor& ed) {
  Info in;
  ed.info(&in);
  size_t used = 0;
  int64_t n = 0;
  ed.read_lines(0, in.line_count, nullptr, 0, &used, &n);
  std::string out(used, '\0');
  if (used > 0) {
    ed.read_lines(0, in.line_count, out.data(), out.size(), &used, &n);
    out.resize(used);
  }
  return out;
}

std::vector<uint64_t> starts_of(const std::string& s) {
  std::vector<uint64_t> out;
  out.push_back(0);
  size_t i = 0;
  while (i < s.size()) {
    if (s[i] == '\n') {
      i += 1;
      out.push_back(i);
    } else if (s[i] == '\r') {
      i += (i + 1 < s.size() && s[i + 1] == '\n') ? 2 : 1;
      out.push_back(i);
    } else {
      i += 1;
    }
  }
  if (out.back() != s.size()) out.push_back(s.size());
  return out;
}

Eol naive_eol(const std::string& s) {
  uint64_t lf = 0, crlf = 0, cr = 0;
  Eol first = Eol::Lf;
  bool have = false;
  bool prev_cr = false;
  for (size_t i = 0; i < s.size(); ++i) {
    const char c = s[i];
    if (c == '\n') {
      if (prev_cr) {
        crlf += 1;
        if (!have) { first = Eol::CrLf; have = true; }
      } else {
        lf += 1;
        if (!have) { first = Eol::Lf; have = true; }
      }
      prev_cr = false;
    } else if (c == '\r') {
      if (prev_cr) {
        cr += 1;
        if (!have) { first = Eol::Cr; have = true; }
      }
      prev_cr = true;
    } else {
      if (prev_cr) {
        cr += 1;
        if (!have) { first = Eol::Cr; have = true; }
        prev_cr = false;
      }
    }
  }
  if (prev_cr) {
    cr += 1;
    if (!have) { first = Eol::Cr; have = true; }
  }
  if (!have) return Eol::Lf;
  const uint64_t mx = std::max(lf, std::max(crlf, cr));
  const int w = (lf == mx) + (crlf == mx) + (cr == mx);
  if (w == 1) {
    if (lf == mx) return Eol::Lf;
    if (crlf == mx) return Eol::CrLf;
    return Eol::Cr;
  }
  return first;
}

size_t line_of(const std::vector<uint64_t>& starts, uint64_t off) {
  size_t lo = 0, hi = starts.size();
  while (lo + 1 < hi) {
    const size_t mid = lo + (hi - lo) / 2;
    if (starts[mid] <= off) {
      lo = mid;
    } else {
      hi = mid;
    }
  }
  return lo;
}

struct Fixture {
  std::string dir;
  std::unique_ptr<FileSystem> fs;
  std::unique_ptr<Editor> ed;
  explicit Fixture(std::string d) : dir(std::move(d)) {
    create_file_system(&fs);
    create_editor(*fs, &ed);
  }
  std::string path(const std::string& name) const { return dir + "/" + name; }
};

}  // namespace

TEST(editor_create_apply_save_reopen) {
  Fixture fx(make_temp_dir());
  const std::string p = fx.path("new.txt");
  CHECK(fx.ed->create(p) == Status::Ok);
  const Edit arr[1] = {Edit{0, 0, 0, std::string_view("hello\nworld\n")}};
  CHECK(fx.ed->apply(arr) == Status::Ok);

  SaveInfo si;
  CHECK(fx.ed->save(nullptr, false, &si) == Status::Ok);
  CHECK_EQ(std::string(si.method), std::string("full_rewrite"));
  CHECK_EQ(si.bytes_written, 12u);
  CHECK_EQ(read_file(p), std::string("hello\nworld\n"));

  Fixture fx2(make_temp_dir());
  std::unique_ptr<Editor> ed2;
  create_editor(*fx2.fs, &ed2);
  CHECK(ed2->open(p) == Status::Ok);
  CHECK_EQ(doc_of(*ed2), std::string("hello\nworld\n"));
  Info in;
  CHECK(ed2->info(&in) == Status::Ok);
  CHECK_EQ(in.line_count, 2);
  CHECK(!in.dirty);
}

TEST(editor_same_length_edit_uses_clone_pwrite) {
  Fixture fx(make_temp_dir());
  const std::string p = fx.path("same.txt");
  write_file(p, "abcdef\n");
  CHECK(fx.ed->open(p) == Status::Ok);

  const Edit arr[1] = {Edit{0, 1, 1, std::string_view("X")}};
  CHECK(fx.ed->apply(arr) == Status::Ok);
  SaveInfo si;
  CHECK(fx.ed->save(nullptr, false, &si) == Status::Ok);
  CHECK_EQ(std::string(si.method), std::string("clone_pwrite"));
  CHECK_EQ(read_file(p), std::string("aXcdef\n"));
}

TEST(editor_length_changing_tail_uses_clone_pwrite_tail) {
  Fixture fx(make_temp_dir());
  const std::string p = fx.path("tail.txt");
  std::string big(999, 'a');
  big += "\n";
  write_file(p, big);
  CHECK(fx.ed->open(p) == Status::Ok);

  const Edit arr[1] = {Edit{0, 999, 0, std::string_view("XY")}};
  CHECK(fx.ed->apply(arr) == Status::Ok);
  SaveInfo si;
  CHECK(fx.ed->save(nullptr, false, &si) == Status::Ok);
  CHECK_EQ(std::string(si.method), std::string("clone_pwrite_tail"));
  std::string expect = big.substr(0, 999) + "XY\n";
  CHECK_EQ(read_file(p), expect);
}

TEST(editor_noop_save_reports_none) {
  Fixture fx(make_temp_dir());
  const std::string p = fx.path("clean.txt");
  write_file(p, "abc\n");
  CHECK(fx.ed->open(p) == Status::Ok);
  SaveInfo si;
  CHECK(fx.ed->save(nullptr, false, &si) == Status::Ok);
  CHECK_EQ(std::string(si.method), std::string("none"));
  CHECK_EQ(si.bytes_written, 0u);
}

TEST(editor_external_change_conflicts) {
  Fixture fx(make_temp_dir());
  const std::string p = fx.path("conflict.txt");
  write_file(p, "abc\n");
  CHECK(fx.ed->open(p) == Status::Ok);
  const Edit arr[1] = {Edit{0, 0, 0, std::string_view("Z")}};
  CHECK(fx.ed->apply(arr) == Status::Ok);
  // Mutate in place behind the editor's back.
  write_file(p, "different\n");
  SaveInfo si;
  CHECK(fx.ed->save(nullptr, false, &si) == Status::Conflict);
}

TEST(editor_bom_is_excluded_from_lines) {
  Fixture fx(make_temp_dir());
  const std::string p = fx.path("bom.txt");
  write_file(p, "\xEF\xBB\xBFhi\n");
  CHECK(fx.ed->open(p) == Status::Ok);
  Info in;
  CHECK(fx.ed->info(&in) == Status::Ok);
  CHECK(in.bom);
  CHECK_EQ(in.bytes, 6u);
  CHECK_EQ(in.line_count, 1);
  CHECK_EQ(doc_of(*fx.ed), std::string("hi\n"));

  // read_range is raw file bytes (BOM included).
  char buf[3] = {0, 0, 0};
  size_t used = 0;
  CHECK(fx.ed->read_range(0, 3, buf, sizeof(buf), &used) == Status::Ok);
  CHECK_EQ(used, 3u);
  CHECK_EQ(static_cast<unsigned char>(buf[0]), 0xEFu);
}

TEST(editor_inserted_newlines_use_dominant_eol) {
  Fixture fx(make_temp_dir());
  const std::string p = fx.path("crlf.txt");
  write_file(p, "a\r\nb\r\n");
  CHECK(fx.ed->open(p) == Status::Ok);
  const Edit arr[1] = {Edit{0, 0, 0, std::string_view("x\ny\n")}};
  CHECK(fx.ed->apply(arr) == Status::Ok);
  SaveInfo si;
  CHECK(fx.ed->save(nullptr, false, &si) == Status::Ok);
  CHECK_EQ(read_file(p), std::string("x\r\ny\r\na\r\nb\r\n"));
}

TEST(editor_read_lines_and_range_semantics) {
  Fixture fx(make_temp_dir());
  const std::string p = fx.path("read.txt");
  write_file(p, "one\ntwo\nthree");
  CHECK(fx.ed->open(p) == Status::Ok);

  size_t need = 0;
  int64_t n = 0;
  CHECK(fx.ed->read_lines(0, 2, nullptr, 0, &need, &n) == Status::Ok);
  CHECK_EQ(need, 8u);  // "one\ntwo\n"
  CHECK_EQ(n, 2);

  char small[1];
  CHECK(fx.ed->read_lines(0, 2, small, 1, &need, &n) == Status::BufferTooSmall);
  CHECK_EQ(need, 8u);
  CHECK_EQ(n, 0);

  CHECK(fx.ed->read_lines(0, 0, nullptr, 0, &need, &n) == Status::Ok);
  CHECK_EQ(need, 0u);

  CHECK(fx.ed->read_lines(-1, 1, nullptr, 0, &need, &n) == Status::Range);

  // read_range size query and EOF clamp.
  CHECK(fx.ed->read_range(0, 1000, nullptr, 0, &need) == Status::Ok);
  CHECK_EQ(need, 13u);
  CHECK(fx.ed->read_range(13, 5, nullptr, 0, &need) == Status::Ok);
  CHECK_EQ(need, 0u);
  CHECK(fx.ed->read_range(-1, 5, nullptr, 0, &need) == Status::Range);
}

TEST(editor_apply_validation) {
  Fixture fx(make_temp_dir());
  const std::string p = fx.path("validate.txt");
  write_file(p, "hello\n");
  CHECK(fx.ed->open(p) == Status::Ok);

  // Column past the line.
  const Edit bad_col[1] = {Edit{0, 99, 0, std::string_view("x")}};
  CHECK(fx.ed->apply(bad_col) == Status::Range);

  // Unsorted/overlapping edits.
  const Edit overlap[2] = {Edit{0, 0, 3, std::string_view("x")},
                           Edit{0, 1, 0, std::string_view("y")}};
  CHECK(fx.ed->apply(overlap) == Status::Range);

  // Invalid UTF-8 in inserted text.
  const Edit bad_utf8[1] = {Edit{0, 0, 0, std::string_view("\xFF")}};
  CHECK(fx.ed->apply(bad_utf8) == Status::InvalidUtf8);

  // Splitting a codepoint: insert in the middle of a 2-byte sequence.
  Fixture fx2(make_temp_dir());
  const std::string p2 = fx2.path("utf8.txt");
  write_file(p2, "\xC3\xA9\n");  // "é\n"
  CHECK(fx2.ed->open(p2) == Status::Ok);
  const Edit split[1] = {Edit{0, 1, 0, std::string_view("x")}};
  CHECK(fx2.ed->apply(split) == Status::Range);

  // Effective no-op: empty insert, zero removed -> Ok, still clean.
  const Edit noop[1] = {Edit{0, 0, 0, std::string_view("")}};
  CHECK(fx.ed->apply(noop) == Status::Ok);
  Info in;
  fx.ed->info(&in);
  CHECK(!in.dirty);
}

TEST(editor_eol_counts_track_edits) {
  Fixture fx(make_temp_dir());
  const std::string p = fx.path("eol.txt");
  write_file(p, "a\r\nb\n");
  CHECK(fx.ed->open(p) == Status::Ok);
  Info in;
  fx.ed->info(&in);
  CHECK(in.eol == Eol::CrLf);  // 1 CRLF vs 1 LF -> tie -> first seen

  const Edit e1[1] = {Edit{1, 0, 0, std::string_view("x\ny\n")}};
  CHECK(fx.ed->apply(e1) == Status::Ok);
  SaveInfo si;
  CHECK(fx.ed->save(nullptr, false, &si) == Status::Ok);
  CHECK_EQ(read_file(p), std::string("a\r\nx\r\ny\r\nb\n"));
  fx.ed->info(&in);
  CHECK(in.eol == Eol::CrLf);

  const Edit e2[1] = {Edit{3, 1, 1, std::string_view("")}};
  CHECK(fx.ed->apply(e2) == Status::Ok);
  CHECK(fx.ed->save(nullptr, false, &si) == Status::Ok);
  CHECK_EQ(read_file(p), std::string("a\r\nx\r\ny\r\nb"));
  fx.ed->info(&in);
  CHECK(in.eol == Eol::CrLf);
  CHECK(!in.final_newline);
}

TEST(editor_eol_matches_naive_under_random_edits) {
  ed_test::Prng rng(0x5151);
  Fixture fx(make_temp_dir());
  const std::string p = fx.path("eolrand.txt");
  write_file(p, "a\r\nb\nc\r\n\r\nd\n");
  CHECK(fx.ed->open(p) == Status::Ok);
  const char* inserts[] = {"\n", "\r", "\r\n", "x", "a\n", "\n\n", "zz"};

  for (int iter = 0; iter < 400; ++iter) {
    const std::string doc = doc_of(*fx.ed);
    const std::vector<uint64_t> starts = starts_of(doc);
    const uint64_t off = rng.below(doc.size() + 1);
    const size_t li = line_of(starts, off);
    const uint64_t col = off - starts[li];
    if (doc.size() > 0 && rng.below(3) == 0) {
      const uint64_t maxrem = doc.size() - off;
      const uint64_t rem = maxrem == 0 ? 0 : rng.below(maxrem) + 1;
      if (rem == 0) continue;
      const Edit e[1] = {Edit{static_cast<int64_t>(li),
                              static_cast<int64_t>(col),
                              static_cast<int64_t>(rem), std::string_view()}};
      const Status s = fx.ed->apply(e);
      CHECK(s == Status::Ok || s == Status::Range);
    } else {
      const std::string ins = inserts[rng.below(7)];
      const Edit e[1] = {Edit{static_cast<int64_t>(li),
                              static_cast<int64_t>(col), 0, ins}};
      const Status s = fx.ed->apply(e);
      CHECK(s == Status::Ok || s == Status::Range);
    }

    Info in;
    fx.ed->info(&in);
    CHECK(in.eol == naive_eol(doc_of(*fx.ed)));

    if (iter % 50 == 49) {
      SaveInfo si;
      CHECK(fx.ed->save(nullptr, false, &si) == Status::Ok);
      Info in2;
      fx.ed->info(&in2);
      CHECK(in2.eol == naive_eol(doc_of(*fx.ed)));
    }
  }
}
