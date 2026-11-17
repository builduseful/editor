// tests/viewcore_test.cpp — viewcore conformance (§8, M0.5).
//
// Layout and drawing are checked through the headless render oracle, so the
// tests are platform-independent and deterministic.
#include <algorithm>
#include <memory>
#include <string>
#include <vector>

#include "check.hpp"
#include "editor.hpp"
#include "fake_fs.hpp"
#include "headless.hpp"
#include "viewcore.hpp"

namespace {

struct Doc {
  ed_test::FakeFileSystem fs;
  std::unique_ptr<Editor> ed;
  vc::HeadlessTextBackend tb;
  std::unique_ptr<vc::ViewCore> view;

  explicit Doc(const std::string& content) {
    int fd = -1;
    fs.open_write("/f.txt", true, false, false, &fd);
    if (!content.empty()) {
      size_t w = 0;
      fs.pwrite(fd, 0, content.data(), content.size(), &w);
    }
    fs.close_fd(fd);
    create_editor(fs, &ed);
    ed->open("/f.txt");
    view = std::make_unique<vc::ViewCore>(*ed, tb);
    view->resize(80, 100000);  // 10 monospace chars wide, tall enough for all
  }

  std::vector<std::string> drawn_runs() {
    vc::RecordingCanvas rc;
    view->draw(rc);
    std::vector<std::string> out;
    for (const vc::DrawCmd& d : rc.cmds) {
      if (d.kind == vc::DrawCmd::Kind::DrawRun) out.push_back(d.text);
    }
    return out;
  }
};

// Greedy wrap reference for the fixed-advance monospace backend (ASCII only).
std::vector<std::string> naive_wrap(const std::string& text, size_t per) {
  std::vector<std::string> out;
  size_t pos = 0;
  while (pos < text.size()) {
    size_t end = std::min(text.size(), pos + per);
    if (end < text.size()) {
      const size_t sp = text.rfind(' ', end - 1);
      if (sp != std::string::npos && sp >= pos) end = sp + 1;
    }
    out.push_back(text.substr(pos, end - pos));
    pos = end;
  }
  return out;
}

}  // namespace

TEST(viewcore_no_wrap_one_run_per_line) {
  Doc d("alpha\nbeta\ngamma\n");
  d.view->set_wrap(false);
  const std::vector<std::string> runs = d.drawn_runs();
  CHECK_EQ(runs.size(), 3u);
  CHECK_EQ(runs[0], std::string("alpha"));
  CHECK_EQ(runs[1], std::string("beta"));
  CHECK_EQ(runs[2], std::string("gamma"));
}

TEST(viewcore_wrap_matches_reference) {
  const std::string text = "the quick brown fox jumps over the lazy dog again";
  Doc d(text + "\n");
  d.view->set_wrap(true);
  const std::vector<std::string> runs = d.drawn_runs();
  const std::vector<std::string> want = naive_wrap(text, 10);
  CHECK_EQ(runs.size(), want.size());
  for (size_t i = 0; i < std::min(runs.size(), want.size()); ++i) {
    CHECK_EQ(runs[i], want[i]);
  }
}

TEST(viewcore_hit_test_and_caret_round_trip) {
  Doc d("hello world\nsecond line\nthird\n");
  d.view->set_wrap(false);
  const double adv = 8.0, lh = 16.0;
  for (int64_t line = 0; line < 3; ++line) {
    for (int64_t col = 0; col < 5; ++col) {
      const vc::Rect r = d.view->caret_rect(line, col);
      CHECK_EQ(r.x, static_cast<double>(col) * adv);
      CHECK_EQ(r.y, static_cast<double>(line) * lh);
      const vc::Hit h = d.view->hit_test(r.x, r.y + lh / 2);
      CHECK_EQ(h.line, line);
      CHECK_EQ(h.col, col);
    }
  }
}

TEST(viewcore_scroll_clamps) {
  Doc d("a\nb\nc\nd\ne\n");
  d.view->scroll_to_line(3);
  CHECK_EQ(d.view->first_visible_line(), 3);
  d.view->scroll_lines(100);
  CHECK_EQ(d.view->first_visible_line(), 5);  // clamped to line_count
  d.view->scroll_lines(-100);
  CHECK_EQ(d.view->first_visible_line(), 0);
}

TEST(viewcore_line_cache_stays_bounded) {
  std::string content;
  for (int i = 0; i < 3000; ++i) content += "line " + std::to_string(i) + "\n";
  Doc d(content);
  d.view->set_wrap(true);
  for (int i = 0; i < 3000; ++i) {
    d.view->scroll_to_line(i);
    vc::RecordingCanvas rc;
    d.view->draw(rc);
  }
  CHECK(d.view->cache_entries() <= 1024u);
  CHECK(d.view->cache_bytes() <= (6u << 20));
}

TEST(viewcore_huge_line_is_capped) {
  // A ~1 MB single line must not blow the cache or hang.
  std::string big(1u << 20, 'x');
  Doc d(big + "\n");
  d.view->set_wrap(true);
  d.view->resize(80, 1000);
  vc::RecordingCanvas rc;
  d.view->draw(rc);
  CHECK(d.view->cache_entries() <= 1024u);
  CHECK(d.view->cache_bytes() <= (6u << 20));
}

TEST(viewcore_draw_is_deterministic) {
  Doc d("one\ntwo\nthree\n");
  d.view->set_wrap(true);
  d.view->set_decorations({{1, vc::Color{200, 200, 200, 255}}});
  vc::RecordingCanvas a;
  vc::RecordingCanvas b;
  d.view->draw(a);
  d.view->draw(b);
  CHECK_EQ(a.dump(), b.dump());
  // The decoration on line 1 appears as a fill before its run.
  bool saw_fill = false;
  for (const vc::DrawCmd& c : a.cmds) {
    if (c.kind == vc::DrawCmd::Kind::FillRect && c.rect.y == 16.0) saw_fill = true;
  }
  CHECK(saw_fill);
}

TEST(viewcore_invalidates_on_font_and_width_change) {
  Doc d("wrap me please because this is long enough\n");
  d.view->set_wrap(true);
  const std::vector<std::string> before = d.drawn_runs();
  CHECK(before.size() > 1u);
  d.view->resize(800, 100000);  // wider: fewer wraps
  const std::vector<std::string> after = d.drawn_runs();
  CHECK(after.size() < before.size());
}
