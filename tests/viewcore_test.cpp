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

TEST(viewcore_first_paint_is_lazy) {
  // First paint must discover only the visible lines, not the whole document.
  std::string content;
  for (int i = 0; i < 100000; ++i) content += "line " + std::to_string(i) + "\n";
  Doc d(content);
  d.view->resize(80, 160);  // 10 monospace lines tall
  vc::RecordingCanvas rc;
  d.view->draw(rc);
  CHECK(!d.view->line_count_known());
  CHECK(d.view->line_count() <= 64);  // a viewport, not 100k lines
  d.view->discover_all();
  CHECK(d.view->line_count_known());
  CHECK_EQ(d.view->line_count(), 100000);
}

TEST(viewcore_draw_discovers_eof_on_short_document) {
  Doc d("only\n");
  d.view->resize(80, 1000);
  vc::RecordingCanvas rc;
  d.view->draw(rc);
  CHECK(d.view->line_count_known());
  CHECK_EQ(d.view->line_count(), 1);
}

TEST(viewcore_scroll_discovers_eof_and_clamps) {
  Doc d("a\nb\nc\n");
  d.view->scroll_to_line(1000);
  CHECK(d.view->line_count_known());
  CHECK_EQ(d.view->line_count(), 3);
  CHECK_EQ(d.view->first_visible_line(), 3);  // clamped past the last line
}

TEST(viewcore_hit_test_is_lazy) {
  std::string content;
  for (int i = 0; i < 50000; ++i) content += "row " + std::to_string(i) + "\n";
  Doc d(content);
  d.view->resize(80, 160);
  const vc::Hit h = d.view->hit_test(0, 8);  // first visual line
  CHECK_EQ(h.line, 0);
  CHECK(!d.view->line_count_known());
  CHECK(d.view->line_count() <= 64);
}

TEST(viewcore_input_horizontal) {
  Doc d("abc\ndef\n");
  d.view->resize(800, 1000);
  vc::KeyEvent right;
  right.key = vc::Key::Right;
  d.view->set_caret(0, 0);
  CHECK(d.view->key(right));
  CHECK_EQ(d.view->selection().caret_line, 0);
  CHECK_EQ(d.view->selection().caret_col, 1);

  d.view->set_caret(0, 3);  // end of "abc"
  CHECK(d.view->key(right));
  CHECK_EQ(d.view->selection().caret_line, 1);
  CHECK_EQ(d.view->selection().caret_col, 0);

  vc::KeyEvent left;
  left.key = vc::Key::Left;
  d.view->set_caret(1, 0);
  CHECK(d.view->key(left));
  CHECK_EQ(d.view->selection().caret_line, 0);
  CHECK_EQ(d.view->selection().caret_col, 3);
}

TEST(viewcore_input_shift_extends_selection) {
  Doc d("hello\n");
  d.view->resize(800, 1000);
  d.view->set_caret(0, 1);
  vc::KeyEvent right;
  right.key = vc::Key::Right;
  right.shift = true;
  CHECK(d.view->key(right));
  const vc::Selection s = d.view->selection();
  CHECK_EQ(s.anchor_line, 0);
  CHECK_EQ(s.anchor_col, 1);
  CHECK_EQ(s.caret_line, 0);
  CHECK_EQ(s.caret_col, 2);
}

TEST(viewcore_input_vertical_keeps_column) {
  Doc d("abcdef\nxy\nabcdef\n");
  d.view->resize(800, 1000);
  d.view->set_caret(0, 4);
  vc::KeyEvent down;
  down.key = vc::Key::Down;
  CHECK(d.view->key(down));
  CHECK_EQ(d.view->selection().caret_line, 1);
  CHECK_EQ(d.view->selection().caret_col, 2);  // clamped to the shorter line
  CHECK(d.view->key(down));
  CHECK_EQ(d.view->selection().caret_line, 2);
  CHECK_EQ(d.view->selection().caret_col, 4);  // goal column restored
}

TEST(viewcore_input_vertical_wraps) {
  Doc d(std::string(30, 'x') + "\n");  // wraps into 3 visual lines at width 80
  d.view->set_wrap(true);
  d.view->resize(80, 1000);
  d.view->set_caret(0, 0);
  vc::KeyEvent down;
  down.key = vc::Key::Down;
  CHECK(d.view->key(down));
  CHECK_EQ(d.view->selection().caret_line, 0);  // still the same logical line
  CHECK_EQ(d.view->selection().caret_col, 10);  // next visual line, same x
}

TEST(viewcore_input_home_end) {
  Doc d("hello\nworld\n");
  d.view->resize(800, 1000);
  d.view->set_caret(0, 3);
  vc::KeyEvent end;
  end.key = vc::Key::End;
  CHECK(d.view->key(end));
  CHECK_EQ(d.view->selection().caret_col, 5);
  vc::KeyEvent home;
  home.key = vc::Key::Home;
  CHECK(d.view->key(home));
  CHECK_EQ(d.view->selection().caret_col, 0);
}

TEST(viewcore_input_mouse_selects) {
  Doc d("hello world\nsecond\n");
  d.view->resize(800, 1000);
  vc::MouseEvent down;
  down.kind = vc::MouseEvent::Kind::Down;
  down.x = 8 * 2 + 1;
  down.y = 8;
  CHECK(d.view->mouse(down));
  CHECK_EQ(d.view->selection().caret_line, 0);
  CHECK_EQ(d.view->selection().caret_col, 2);

  vc::MouseEvent drag;
  drag.kind = vc::MouseEvent::Kind::Drag;
  drag.x = 8 * 5 + 1;
  drag.y = 8;
  CHECK(d.view->mouse(drag));
  CHECK_EQ(d.view->selection().anchor_col, 2);
  CHECK_EQ(d.view->selection().caret_col, 5);
}

TEST(viewcore_input_mouse_scroll) {
  std::string content;
  for (int i = 0; i < 100; ++i) content += "line\n";
  Doc d(content);
  d.view->resize(800, 160);  // 10 lines tall
  vc::MouseEvent scroll;
  scroll.kind = vc::MouseEvent::Kind::Scroll;
  scroll.dy = 5 * 16.0;
  CHECK(d.view->mouse(scroll));
  CHECK_EQ(d.view->first_visible_line(), 5);
}

TEST(viewcore_input_select_all) {
  Doc d("aa\nbbb\ncc\n");
  d.view->resize(800, 1000);
  vc::KeyEvent all;
  all.key = vc::Key::SelectAll;
  all.cmd = true;
  CHECK(d.view->key(all));
  const vc::Selection s = d.view->selection();
  CHECK_EQ(s.anchor_line, 0);
  CHECK_EQ(s.anchor_col, 0);
  CHECK_EQ(s.caret_line, 2);
  CHECK_EQ(s.caret_col, 2);
}

TEST(viewcore_input_none_is_noop) {
  Doc d("x\n");
  vc::KeyEvent none;
  CHECK(!d.view->key(none));
}

TEST(viewcore_draw_renders_selection) {
  Doc d("hello world\n");
  d.view->resize(800, 1000);
  d.view->set_caret(0, 2);
  vc::KeyEvent right;
  right.key = vc::Key::Right;
  right.shift = true;
  d.view->key(right);
  d.view->key(right);
  d.view->key(right);  // select bytes [2, 5)
  vc::RecordingCanvas rc;
  d.view->draw(rc);
  bool sel = false;
  for (const vc::DrawCmd& cmd : rc.cmds) {
    if (cmd.kind == vc::DrawCmd::Kind::FillRect && cmd.rect.x == 16.0 &&
        cmd.rect.w == 24.0 && cmd.rect.h == 16.0) {
      sel = true;
    }
  }
  CHECK(sel);
}

TEST(viewcore_draw_renders_caret) {
  Doc d("hello\n");
  d.view->resize(800, 1000);
  d.view->set_caret(0, 3);
  vc::RecordingCanvas rc;
  d.view->draw(rc);
  bool caret = false;
  for (const vc::DrawCmd& cmd : rc.cmds) {
    if (cmd.kind == vc::DrawCmd::Kind::FillRect && cmd.rect.x == 24.0 &&
        cmd.rect.w == 1.5 && cmd.rect.h == 16.0) {
      caret = true;
    }
  }
  CHECK(caret);
}

