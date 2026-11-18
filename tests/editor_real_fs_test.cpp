// tests/editor_real_fs_test.cpp — core Editor against the real macOS FileSystem.
#include <algorithm>
#include <chrono>
#include <cstdio>
#include <filesystem>
#include <memory>
#include <span>
#include <string>
#include <vector>

#include "check.hpp"
#include "editor.hpp"
#include "fault_fs.hpp"
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

std::string normalize(const std::string& s, Eol eol) {
  const char* rep = eol == Eol::CrLf ? "\r\n" : (eol == Eol::Cr ? "\r" : "\n");
  const size_t replen = eol == Eol::CrLf ? 2 : 1;
  std::string out;
  for (size_t i = 0; i < s.size(); ++i) {
    if (s[i] == '\r') {
      out.append(rep, replen);
      if (i + 1 < s.size() && s[i + 1] == '\n') i += 1;
    } else if (s[i] == '\n') {
      out.append(rep, replen);
    } else {
      out.push_back(s[i]);
    }
  }
  return out;
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

// A Fixture whose FileSystem can be armed to fail one operation on one call.
struct FaultFixture {
  std::string dir;
  std::unique_ptr<FileSystem> fs;
  ed_test::FaultFs* fault = nullptr;
  std::unique_ptr<Editor> ed;
  explicit FaultFixture(std::string d) : dir(std::move(d)) {
    std::unique_ptr<FileSystem> inner;
    create_file_system(&inner);
    auto ff = std::make_unique<ed_test::FaultFs>(std::move(inner));
    fault = ff.get();
    fs = std::move(ff);
    create_editor(*fs, &ed);
  }
  std::string path(const std::string& name) const { return dir + "/" + name; }
};

// Count leftover temp/clone files in `dir` (save must clean these up on any
// pre-rename failure; a crash may leave one, but an in-process failure may not).
int count_temps(const std::string& dir) {
  int n = 0;
  std::error_code ec;
  for (const auto& e : std::filesystem::directory_iterator(dir, ec)) {
    const std::string name = e.path().filename().string();
    if (name.rfind(".editor-tmp-", 0) == 0 ||
        name.rfind(".editor-clone-", 0) == 0) {
      ++n;
    }
  }
  return n;
}


}  // namespace

// A freshly created editor has no document (the GUI always creates one before
// it draws, but a Finder launch shows its window before the open-document event
// arrives). Every entry point must report that, not touch the unset sources.
TEST(editor_without_a_document_reports_errors_instead_of_crashing) {
  std::unique_ptr<FileSystem> fs;
  std::unique_ptr<Editor> ed;
  create_file_system(&fs);
  create_editor(*fs, &ed);
  CHECK(ed != nullptr);

  size_t used = 123;
  int64_t n = 123;
  CHECK(ed->read_lines(0, 1, nullptr, 0, &used, &n) != Status::Ok);
  CHECK(used == 0);
  CHECK(n == 0);
  CHECK(ed->read_range(0, 1, nullptr, 0, &used) != Status::Ok);
  CHECK(used == 0);
  CHECK(ed->apply(std::span<const Edit>{}) != Status::Ok);

  Info in{};
  CHECK(ed->info(&in) != Status::Ok);
  CHECK(ed->save(nullptr, false, nullptr) != Status::Ok);
  CHECK(ed->save_as("/tmp/editor-no-document.txt", nullptr, false, nullptr) !=
        Status::Ok);
  CHECK(!ed->errmsg().empty());  // the reason is reported, not just the status
}

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

TEST(editor_reopen_resets_lazy_index) {
  // A partial (lazy) index must not survive into a different document: the
  // next extend() would resume scanning at the old prefix offset.
  Fixture fx(make_temp_dir());
  const std::string a = fx.path("a.txt");
  const std::string b = fx.path("b.txt");
  std::string big_a;
  for (int i = 0; i < 500; ++i) big_a += "A" + std::to_string(i) + "\n";
  write_file(a, big_a);
  write_file(b, "one\ntwo\nthree\n");

  CHECK(fx.ed->open(a) == Status::Ok);
  size_t used = 0;
  int64_t n = 0;
  CHECK(fx.ed->read_lines(0, 2, nullptr, 0, &used, &n) == Status::Ok);
  CHECK_EQ(n, 2);

  CHECK(fx.ed->open(b) == Status::Ok);
  CHECK(fx.ed->read_lines(0, 3, nullptr, 0, &used, &n) == Status::Ok);
  CHECK_EQ(n, 3);
  std::string got(used, '\0');
  CHECK(fx.ed->read_lines(0, 3, got.data(), got.size(), &used, &n) == Status::Ok);
  got.resize(used);
  CHECK_EQ(got, std::string("one\ntwo\nthree\n"));
}

TEST(editor_reload_resets_lazy_index) {
  Fixture fx(make_temp_dir());
  const std::string p = fx.path("r.txt");
  std::string big;
  for (int i = 0; i < 500; ++i) big += "old" + std::to_string(i) + "\n";
  write_file(p, big);
  CHECK(fx.ed->open(p) == Status::Ok);

  size_t used = 0;
  int64_t n = 0;
  CHECK(fx.ed->read_lines(0, 2, nullptr, 0, &used, &n) == Status::Ok);
  CHECK_EQ(n, 2);

  write_file(p, "new1\nnew2\n");
  CHECK(fx.ed->reload() == Status::Ok);
  CHECK(fx.ed->read_lines(0, 2, nullptr, 0, &used, &n) == Status::Ok);
  CHECK_EQ(n, 2);
  std::string got(used, '\0');
  CHECK(fx.ed->read_lines(0, 2, got.data(), got.size(), &used, &n) == Status::Ok);
  got.resize(used);
  CHECK_EQ(got, std::string("new1\nnew2\n"));
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

TEST(editor_undo_redo_basics) {
  Fixture fx(make_temp_dir());
  const std::string p = fx.path("u.txt");
  write_file(p, "abc\n");
  CHECK(fx.ed->open(p) == Status::Ok);

  CHECK(fx.ed->undo() == Status::Ok);  // empty stack: Ok, no change
  CHECK_EQ(doc_of(*fx.ed), std::string("abc\n"));
  CHECK(fx.ed->redo() == Status::Ok);
  CHECK_EQ(doc_of(*fx.ed), std::string("abc\n"));

  const Edit ins[1] = {Edit{0, 1, 0, std::string_view("X")}};
  CHECK(fx.ed->apply(ins) == Status::Ok);
  CHECK_EQ(doc_of(*fx.ed), std::string("aXbc\n"));

  CHECK(fx.ed->undo() == Status::Ok);
  CHECK_EQ(doc_of(*fx.ed), std::string("abc\n"));
  CHECK(fx.ed->undo() == Status::Ok);  // exhausted
  CHECK_EQ(doc_of(*fx.ed), std::string("abc\n"));

  CHECK(fx.ed->redo() == Status::Ok);
  CHECK_EQ(doc_of(*fx.ed), std::string("aXbc\n"));
  CHECK(fx.ed->redo() == Status::Ok);  // exhausted
  CHECK_EQ(doc_of(*fx.ed), std::string("aXbc\n"));

  // A new edit clears redo.
  const Edit del[1] = {Edit{0, 1, 2, std::string_view()}};
  CHECK(fx.ed->apply(del) == Status::Ok);
  CHECK_EQ(doc_of(*fx.ed), std::string("ac\n"));
  CHECK(fx.ed->redo() == Status::Ok);
  CHECK_EQ(doc_of(*fx.ed), std::string("ac\n"));

  // Undo a multi-edit call in one step.
  const Edit multi[2] = {Edit{0, 0, 1, std::string_view("A")},
                         Edit{0, 1, 1, std::string_view("B")}};
  CHECK(fx.ed->apply(multi) == Status::Ok);
  CHECK_EQ(doc_of(*fx.ed), std::string("AB\n"));
  CHECK(fx.ed->undo() == Status::Ok);
  CHECK_EQ(doc_of(*fx.ed), std::string("ac\n"));

  // Save drops the undo history ("undo to last save").
  SaveInfo si;
  CHECK(fx.ed->save(nullptr, false, &si) == Status::Ok);
  const Edit ins2[1] = {Edit{0, 0, 0, std::string_view("Z")}};
  CHECK(fx.ed->apply(ins2) == Status::Ok);
  CHECK(fx.ed->undo() == Status::Ok);
  CHECK_EQ(doc_of(*fx.ed), std::string("ac\n"));
  CHECK(fx.ed->undo() == Status::Ok);  // nothing before the save
  CHECK_EQ(doc_of(*fx.ed), std::string("ac\n"));
}

TEST(editor_undo_redo_property) {
  ed_test::Prng rng(0xABCDEF);
  Fixture fx(make_temp_dir());
  const std::string p = fx.path("uprop.txt");
  write_file(p, "start\n");
  CHECK(fx.ed->open(p) == Status::Ok);

  std::string model = "start\n";
  std::vector<std::string> undo_hist, redo_hist;
  const char* inserts[] = {"a", "\n", "zz", "Q\n", "\r\n"};

  for (int iter = 0; iter < 800; ++iter) {
    const uint64_t action = rng.below(10);
    if (action < 6) {
      const std::vector<uint64_t> starts = starts_of(model);
      const uint64_t off = rng.below(model.size() + 1);
      const size_t li = line_of(starts, off);
      const uint64_t col = off - starts[li];
      if (model.size() > 0 && rng.below(3) == 0) {
        const uint64_t maxrem = model.size() - off;
        const uint64_t rem = maxrem == 0 ? 0 : rng.below(maxrem) + 1;
        if (rem == 0) continue;
        const Edit e[1] = {Edit{static_cast<int64_t>(li),
                                static_cast<int64_t>(col),
                                static_cast<int64_t>(rem), std::string_view()}};
        const Status s = fx.ed->apply(e);
        CHECK(s == Status::Ok || s == Status::Range);
        if (s == Status::Ok) {
          undo_hist.push_back(model);
          redo_hist.clear();
          model.erase(off, rem);
        }
      } else {
        const std::string ins = inserts[rng.below(5)];
        const Edit e[1] = {Edit{static_cast<int64_t>(li),
                                static_cast<int64_t>(col), 0, ins}};
        const Status s = fx.ed->apply(e);
        CHECK(s == Status::Ok || s == Status::Range);
        if (s == Status::Ok) {
          undo_hist.push_back(model);
          redo_hist.clear();
          model.insert(off, normalize(ins, naive_eol(model)));
        }
      }
    } else if (action < 8) {
      CHECK(fx.ed->undo() == Status::Ok);
      if (!undo_hist.empty()) {
        redo_hist.push_back(model);
        model = undo_hist.back();
        undo_hist.pop_back();
      }
    } else {
      CHECK(fx.ed->redo() == Status::Ok);
      if (!redo_hist.empty()) {
        undo_hist.push_back(model);
        model = redo_hist.back();
        redo_hist.pop_back();
      }
    }
    CHECK_EQ(doc_of(*fx.ed), model);
    Info in;
    fx.ed->info(&in);
    CHECK_EQ(in.line_count, static_cast<int64_t>(starts_of(model).size()) - 1);
  }
}

TEST(editor_insert_at_start_keeps_content) {
  // Regression: the same index bug made an insert at offset 0 drop the tail.
  Fixture fx(make_temp_dir());
  const std::string p = fx.path("ins0.txt");
  write_file(p, "\nazz\nazz\n\n");
  CHECK(fx.ed->open(p) == Status::Ok);
  const Edit e[1] = {Edit{0, 0, 0, std::string_view("zz")}};
  CHECK(fx.ed->apply(e) == Status::Ok);
  CHECK_EQ(doc_of(*fx.ed), std::string("zz\nazz\nazz\n\n"));
  Info in;
  fx.ed->info(&in);
  CHECK_EQ(in.line_count, 4);
}

// --- Fault injection (§5.5, M0b) -------------------------------------------

// A failed pwrite on the temp path must leave the original untouched and clean
// up the temp; the document stays dirty and a retry succeeds.
TEST(editor_save_pwrite_failure_is_atomic) {
  FaultFixture fx(make_temp_dir());
  const std::string p = fx.path("pw.txt");
  write_file(p, "abcdef\n");
  CHECK(fx.ed->open(p) == Status::Ok);
  const Edit e[1] = {Edit{0, 0, 0, std::string_view("XYZ")}};  // tail > 25%: full_rewrite
  CHECK(fx.ed->apply(e) == Status::Ok);

  fx.fault->fail_op("pwrite", 1, Status::Io);
  SaveInfo si;
  CHECK(fx.ed->save(nullptr, false, &si) == Status::Io);
  CHECK_EQ(read_file(p), std::string("abcdef\n"));
  CHECK_EQ(count_temps(fx.dir), 0);

  Info in;
  fx.ed->info(&in);
  CHECK(in.dirty);
  CHECK(fx.ed->save(nullptr, false, &si) == Status::Ok);  // retry works
  CHECK_EQ(read_file(p), std::string("XYZabcdef\n"));
  CHECK_EQ(count_temps(fx.dir), 0);
}

TEST(editor_save_fsync_failure_is_atomic) {
  FaultFixture fx(make_temp_dir());
  const std::string p = fx.path("fs.txt");
  write_file(p, "abcdef\n");
  CHECK(fx.ed->open(p) == Status::Ok);
  const Edit e[1] = {Edit{0, 0, 0, std::string_view("XYZ")}};
  CHECK(fx.ed->apply(e) == Status::Ok);

  fx.fault->fail_op("fsync", 1, Status::Io);
  SaveInfo si;
  CHECK(fx.ed->save(nullptr, false, &si) == Status::Io);
  CHECK_EQ(read_file(p), std::string("abcdef\n"));
  CHECK_EQ(count_temps(fx.dir), 0);
}

TEST(editor_save_rename_failure_is_atomic) {
  FaultFixture fx(make_temp_dir());
  const std::string p = fx.path("rn.txt");
  write_file(p, "abcdef\n");
  CHECK(fx.ed->open(p) == Status::Ok);
  const Edit e[1] = {Edit{0, 0, 0, std::string_view("XYZ")}};
  CHECK(fx.ed->apply(e) == Status::Ok);

  // The crash-before-rename case: the temp is written and fsynced, then the
  // commit point fails. The old file must still be the visible one.
  fx.fault->fail_op("rename_at", 1, Status::Io);
  SaveInfo si;
  CHECK(fx.ed->save(nullptr, false, &si) == Status::Io);
  CHECK_EQ(read_file(p), std::string("abcdef\n"));
  CHECK_EQ(count_temps(fx.dir), 0);
}

TEST(editor_save_clone_rename_failure_is_atomic) {
  FaultFixture fx(make_temp_dir());
  const std::string p = fx.path("cl.txt");
  write_file(p, "abcdef\n");
  CHECK(fx.ed->open(p) == Status::Ok);
  const Edit e[1] = {Edit{0, 1, 1, std::string_view("X")}};  // same length: clone path
  CHECK(fx.ed->apply(e) == Status::Ok);

  fx.fault->fail_op("rename_at", 1, Status::Io);
  SaveInfo si;
  CHECK(fx.ed->save(nullptr, false, &si) == Status::Io);
  CHECK_EQ(read_file(p), std::string("abcdef\n"));
  CHECK_EQ(count_temps(fx.dir), 0);
}

// A post-rename directory-sync failure is non-fatal: the bytes are committed,
// reported as SavedButNotDurable with durable=false, and never blind-retried.
TEST(editor_save_fsync_dir_warns_not_durable) {
  FaultFixture fx(make_temp_dir());
  const std::string p = fx.path("dir.txt");
  write_file(p, "abcdef\n");
  CHECK(fx.ed->open(p) == Status::Ok);
  const Edit e[1] = {Edit{0, 0, 0, std::string_view("XYZ")}};
  CHECK(fx.ed->apply(e) == Status::Ok);

  fx.fault->fail_op("fsync_dir", 1, Status::Io);
  SaveInfo si;
  CHECK(fx.ed->save(nullptr, false, &si) == Status::SavedButNotDurable);
  CHECK(!si.durable);
  CHECK_EQ(read_file(p), std::string("XYZabcdef\n"));  // committed
  CHECK_EQ(count_temps(fx.dir), 0);                    // temp was renamed away
}

// In-place failure after the write starts must enter the needs-reload state and
// keep the retained base readable for line reads (§5.5).
TEST(editor_in_place_write_failure_needs_reload) {
  FaultFixture fx(make_temp_dir());
  const std::string p = fx.path("ip.txt");
  write_file(p, "abcdef\n");
  CHECK(fx.ed->open(p) == Status::Ok);
  const Edit e[1] = {Edit{0, 1, 1, std::string_view("X")}};
  CHECK(fx.ed->apply(e) == Status::Ok);

  fx.fault->fail_op("pwrite", 1, Status::Io);
  SaveInfo si;
  CHECK(fx.ed->save(nullptr, true, &si) == Status::Io);
  CHECK(fx.ed->save(nullptr, true, &si) == Status::Stale);
  CHECK(fx.ed->apply(e) == Status::Stale);
  CHECK_EQ(doc_of(*fx.ed), std::string("aXcdef\n"));  // retained base still reads
}

// Saving twice is safe: the second save is a clean no-op.
TEST(editor_double_save_is_noop) {
  Fixture fx(make_temp_dir());
  const std::string p = fx.path("twice.txt");
  write_file(p, "abcdef\n");
  CHECK(fx.ed->open(p) == Status::Ok);
  const Edit e[1] = {Edit{0, 1, 1, std::string_view("X")}};
  CHECK(fx.ed->apply(e) == Status::Ok);

  SaveInfo si;
  CHECK(fx.ed->save(nullptr, false, &si) == Status::Ok);
  CHECK_EQ(std::string(si.method), std::string("clone_pwrite"));
  CHECK_EQ(read_file(p), std::string("aXcdef\n"));

  SaveInfo si2;
  CHECK(fx.ed->save(nullptr, false, &si2) == Status::Ok);
  CHECK_EQ(std::string(si2.method), std::string("none"));
  CHECK_EQ(si2.bytes_written, 0u);
  CHECK_EQ(read_file(p), std::string("aXcdef\n"));
  CHECK_EQ(count_temps(fx.dir), 0);
}

// --- save_as (§4/§5.5) ------------------------------------------------------

static std::vector<Change> g_changes;
static void record_change(Editor*, const Change& c, void*) {
  g_changes.push_back(c);
}

TEST(editor_save_as_new_file) {
  Fixture fx(make_temp_dir());
  const std::string a = fx.path("a.txt");
  const std::string b = fx.path("b.txt");
  write_file(a, "abc\n");
  CHECK(fx.ed->open(a) == Status::Ok);
  const Edit e[1] = {Edit{0, 0, 0, std::string_view("X")}};
  CHECK(fx.ed->apply(e) == Status::Ok);

  SaveInfo si;
  CHECK(fx.ed->save_as(b, nullptr, false, &si) == Status::Ok);
  CHECK_EQ(std::string(si.method), std::string("copy_range"));
  CHECK_EQ(read_file(a), std::string("abc\n"));   // source untouched
  CHECK_EQ(read_file(b), std::string("Xabc\n"));

  // The open path is now dst: a later save writes there.
  Info in;
  CHECK(fx.ed->info(&in) == Status::Ok);
  CHECK_EQ(std::string(in.path),
           std::filesystem::weakly_canonical(b).string());
  const Edit e2[1] = {Edit{0, 0, 0, std::string_view("Y")}};
  CHECK(fx.ed->apply(e2) == Status::Ok);
  SaveInfo si2;
  CHECK(fx.ed->save(nullptr, false, &si2) == Status::Ok);
  CHECK_EQ(read_file(b), std::string("YXabc\n"));
  CHECK_EQ(read_file(a), std::string("abc\n"));
}

TEST(editor_save_as_same_file_clean_is_noop) {
  Fixture fx(make_temp_dir());
  const std::string a = fx.path("same.txt");
  write_file(a, "abc\n");
  CHECK(fx.ed->open(a) == Status::Ok);
  SaveInfo si;
  CHECK(fx.ed->save_as(a, nullptr, false, &si) == Status::Ok);
  CHECK_EQ(std::string(si.method), std::string("none"));
  CHECK_EQ(si.bytes_written, 0u);
}

TEST(editor_save_as_existing_requires_matching_rev) {
  Fixture fx(make_temp_dir());
  const std::string a = fx.path("src.txt");
  const std::string b = fx.path("dst.txt");
  write_file(a, "abc\n");
  write_file(b, "old\n");
  CHECK(fx.ed->open(a) == Status::Ok);
  const Edit e[1] = {Edit{0, 0, 0, std::string_view("X")}};
  CHECK(fx.ed->apply(e) == Status::Ok);

  SaveInfo si;
  CHECK(fx.ed->save_as(b, nullptr, false, &si) == Status::Conflict);
  CHECK_EQ(read_file(b), std::string("old\n"));

  Rev drev;
  CHECK(fx.fs->identity(b, &drev) == Status::Ok);
  CHECK(fx.ed->save_as(b, &drev, false, &si) == Status::Ok);
  CHECK_EQ(read_file(b), std::string("Xabc\n"));
  CHECK_EQ(read_file(a), std::string("abc\n"));
}

TEST(editor_save_as_refuses_symlink_and_bad_new_rev) {
  Fixture fx(make_temp_dir());
  const std::string a = fx.path("src.txt");
  const std::string target = fx.path("real.txt");
  const std::string link = fx.path("link.txt");
  write_file(a, "abc\n");
  write_file(target, "t\n");
  std::error_code ec;
  std::filesystem::create_symlink(target, link, ec);
  CHECK(fx.ed->open(a) == Status::Ok);

  SaveInfo si;
  // A symlink dst is refused (lstat, no-follow).
  CHECK(fx.ed->save_as(link, nullptr, false, &si) == Status::Conflict);

  // A new dst with expected_rev set is refused; in_place on a new dst is NotFound.
  Rev any{};
  CHECK(fx.ed->save_as(fx.path("new.txt"), &any, false, &si) == Status::Conflict);
  CHECK(fx.ed->save_as(fx.path("new.txt"), nullptr, true, &si) == Status::NotFound);
}

TEST(editor_save_as_in_place_preserves_inode) {
  Fixture fx(make_temp_dir());
  const std::string a = fx.path("src.txt");
  const std::string b = fx.path("dst.txt");
  write_file(a, "abc\n");
  write_file(b, "old content\n");
  CHECK(fx.ed->open(a) == Status::Ok);
  const Edit e[1] = {Edit{0, 0, 0, std::string_view("X")}};
  CHECK(fx.ed->apply(e) == Status::Ok);

  Rev drev;
  CHECK(fx.fs->identity(b, &drev) == Status::Ok);
  SaveInfo si;
  CHECK(fx.ed->save_as(b, &drev, true, &si) == Status::Ok);
  CHECK_EQ(read_file(b), std::string("Xabc\n"));
  Rev after;
  CHECK(fx.fs->identity(b, &after) == Status::Ok);
  CHECK_EQ(after.ino, drev.ino);  // inode preserved
}

TEST(editor_save_as_preserves_bom) {
  Fixture fx(make_temp_dir());
  const std::string a = fx.path("bom.txt");
  const std::string b = fx.path("bomout.txt");
  write_file(a, "\xEF\xBB\xBFhi\n");
  CHECK(fx.ed->open(a) == Status::Ok);
  SaveInfo si;
  CHECK(fx.ed->save_as(b, nullptr, false, &si) == Status::Ok);
  CHECK_EQ(read_file(b), std::string("\xEF\xBB\xBFhi\n"));
}

TEST(editor_save_and_save_as_emit_save_change) {
  Fixture fx(make_temp_dir());
  const std::string a = fx.path("chg.txt");
  const std::string b = fx.path("chg2.txt");
  write_file(a, "abc\n");
  CHECK(fx.ed->open(a) == Status::Ok);
  fx.ed->on_change(&record_change, nullptr);

  const Edit e[1] = {Edit{0, 0, 0, std::string_view("X")}};
  CHECK(fx.ed->apply(e) == Status::Ok);
  CHECK(fx.ed->drain() == Status::Ok);  // flush the apply change
  g_changes.clear();
  SaveInfo si;
  CHECK(fx.ed->save(nullptr, false, &si) == Status::Ok);
  CHECK(fx.ed->drain() == Status::Ok);
  CHECK_EQ(g_changes.size(), 1u);
  CHECK(g_changes[0].kind == ChangeKind::Save);

  const Edit e2[1] = {Edit{0, 0, 0, std::string_view("Y")}};
  CHECK(fx.ed->apply(e2) == Status::Ok);
  CHECK(fx.ed->drain() == Status::Ok);
  g_changes.clear();
  CHECK(fx.ed->save_as(b, nullptr, false, &si) == Status::Ok);
  CHECK(fx.ed->drain() == Status::Ok);
  CHECK_EQ(g_changes.size(), 1u);
  CHECK(g_changes[0].kind == ChangeKind::Save);
}
