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

  // The whole logical document via read_lines (includes terminators).
  std::string text() {
    Info in;
    ed->info(&in);
    size_t used = 0;
    int64_t n = 0;
    ed->read_lines(0, in.line_count, nullptr, 0, &used, &n);
    std::string s(used, '\0');
    if (used > 0) {
      size_t got = 0;
      ed->read_lines(0, in.line_count, s.data(), s.size(), &got, &n);
      s.resize(got);
    }
    return s;
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
  d.view->resize(80, 48);  // 3 lines tall
  d.view->scroll_to_line(1);
  CHECK_EQ(d.view->first_visible_line(), 1);
  d.view->scroll_lines(100);  // clamps to the last page (lines 2,3,4)
  CHECK_EQ(d.view->first_visible_line(), 2);
  d.view->scroll_lines(-100);
  CHECK_EQ(d.view->first_visible_line(), 0);
}

TEST(viewcore_scroll_never_leaves_blank_space) {
  // A viewport taller than the document never scrolls: no blank page below the
  // last line (this used to happen with PageDown / wheel-past-the-end).
  Doc d("a\nb\nc\n");
  d.view->scroll_lines(100);
  CHECK_EQ(d.view->first_visible_line(), 0);
  vc::KeyEvent page_down;
  page_down.key = vc::Key::PageDown;
  d.view->key(page_down);
  CHECK_EQ(d.view->first_visible_line(), 0);
}

TEST(viewcore_scroll_clamps_after_the_document_shrinks) {
  // Scrolled to the end of 60 lines, then the document shrinks to 5.
  std::string content;
  for (int i = 0; i < 60; ++i) content += "line\n";
  Doc d(content);
  d.view->resize(80, 160);  // 10 lines tall
  d.view->discover_all();
  d.view->scroll_to_line(50);
  CHECK_EQ(d.view->first_visible_line(), 50);

  // Delete everything but the first 5 lines through the core, then notify.
  std::vector<Edit> edits;
  edits.push_back(Edit{5, 0, 275, std::string_view()});  // 55 lines of "line\n"
  CHECK(d.ed->apply(edits) == Status::Ok);
  d.view->document_changed();
  CHECK_EQ(d.view->line_count(), 5);
  CHECK_EQ(d.view->first_visible_line(), 0);  // the whole document now fits
}

TEST(viewcore_line_cache_stays_bounded) {
  std::string content;
  for (int i = 0; i < 3000; ++i) content += "line " + std::to_string(i) + "\n";
  Doc d(content);
  d.view->set_wrap(true);
  d.view->resize(80, 160);  // keep the viewport small so scrolling is real
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

// The window can draw before any document exists: a Finder / "Open With" launch
// shows the window first and delivers the file as an Apple event afterwards.
// That frame must be blank, not a crash (it used to dereference the unset
// document source).
TEST(viewcore_draws_blank_without_a_document) {
  ed_test::FakeFileSystem fs;
  std::unique_ptr<Editor> ed;
  create_editor(fs, &ed);
  vc::HeadlessTextBackend tb;
  vc::ViewCore view(*ed, tb);
  view.resize(80, 100);
  vc::RecordingCanvas rc;
  view.draw(rc);
  CHECK_EQ(view.line_count(), 0);
  for (const vc::DrawCmd& d : rc.cmds) {
    CHECK(d.kind != vc::DrawCmd::Kind::DrawRun);  // no text to draw
  }
  vc::KeyEvent k;
  k.key = vc::Key::Down;
  (void)view.key(k);  // input is ignored, not a crash
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
  CHECK_EQ(d.view->first_visible_line(), 0);  // the whole document fits
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

TEST(viewcore_discover_step_is_incremental) {
  std::string content;
  for (int i = 0; i < 100000; ++i) content += "line " + std::to_string(i) + "\n";
  Doc d(content);
  d.view->resize(80, 160);
  vc::RecordingCanvas rc;
  d.view->draw(rc);  // lazy first paint
  CHECK(!d.view->line_count_known());
  const int64_t before = d.view->line_count();

  CHECK(d.view->discover_step(100));  // more to come
  CHECK(!d.view->line_count_known());
  CHECK(d.view->line_count() > before);

  int steps = 0;
  while (d.view->discover_step(10000)) {
    CHECK(++steps < 100);  // it must make progress, not loop
  }
  CHECK(d.view->line_count_known());
  CHECK_EQ(d.view->line_count(), 100000);
  // Once complete, stepping is a cheap no-op.
  CHECK(!d.view->discover_step(1000));
}

// --- The overlay scrollbar ------------------------------------------------

namespace {
std::string n_lines(int n, const std::string& body = "line\n") {
  std::string s;
  for (int i = 0; i < n; ++i) s += body;
  return s;
}
}  // namespace

TEST(viewcore_scrollbar_appears_once_the_count_is_known) {
  Doc d(n_lines(200));
  d.view->resize(80, 160);  // 10 lines tall
  vc::RecordingCanvas rc;
  d.view->draw(rc);  // lazy first paint: the total is unknown
  CHECK(!d.view->line_count_known());
  // ... but the bar is already there, driven by byte progress, so a big file can
  // be navigated before the index finishes.
  const vc::ViewCore::ScrollBar approx = d.view->scrollbar();
  CHECK(approx.visible);
  CHECK(approx.approximate);
  CHECK_EQ(approx.thumb_y, 0.0);   // at the top
  CHECK_EQ(approx.thumb_h, 28.0);  // the minimum thumb

  d.view->discover_all();
  const vc::ViewCore::ScrollBar sb = d.view->scrollbar();
  CHECK(sb.visible);
  CHECK(!sb.approximate);  // exact line count now
  CHECK_EQ(sb.track_x, 80.0 - vc::ViewCore::kScrollbarWidth);
  CHECK_EQ(sb.track_w, vc::ViewCore::kScrollbarWidth);
  CHECK_EQ(sb.thumb_y, 0.0);   // at the top
  CHECK_EQ(sb.thumb_h, 28.0);  // the minimum: 200 lines far exceed the viewport
}

// A drag on an approximate bar (unknown line count) previews the thumb and
// commits one target on release; the caller's discovery loop indexes to it.
TEST(viewcore_approximate_scrollbar_jumps_once_indexed) {
  Doc d(n_lines(2000));
  d.view->resize(80, 160);
  vc::RecordingCanvas rc;
  d.view->draw(rc);
  CHECK(!d.view->line_count_known());
  const vc::ViewCore::ScrollBar sb = d.view->scrollbar();
  CHECK(sb.visible);
  CHECK(sb.approximate);

  vc::MouseEvent down;
  down.kind = vc::MouseEvent::Kind::Down;
  down.x = 76;  // over the right-edge bar
  down.y = sb.thumb_y + sb.thumb_h / 2;
  d.view->mouse(down);
  vc::MouseEvent drag = down;
  drag.kind = vc::MouseEvent::Kind::Drag;
  drag.y = 80;  // halfway down the track
  d.view->mouse(drag);
  CHECK_EQ(d.view->first_visible_line(), 0);  // preview only: nothing jumped yet

  vc::MouseEvent up = drag;
  up.kind = vc::MouseEvent::Kind::Up;
  d.view->mouse(up);
  CHECK(d.view->has_pending_scroll());

  int guard = 0;
  while (d.view->has_pending_scroll() && guard++ < 100000) {
    d.view->discover_step(256);
    d.view->apply_pending_scroll();
  }
  CHECK(!d.view->has_pending_scroll());
  CHECK(d.view->first_visible_line() > 300);  // landed in the middle
}

// A queued jump belongs to the drag that made it: scrolling (or any direct
// scroll) while the index is still catching up drops it, so the view never
// yanks itself somewhere the user has already scrolled away from.
TEST(viewcore_pending_jump_is_dropped_when_the_user_scrolls) {
  Doc d(n_lines(2000));
  d.view->resize(80, 160);
  vc::RecordingCanvas rc;
  d.view->draw(rc);
  const vc::ViewCore::ScrollBar sb = d.view->scrollbar();
  vc::MouseEvent down;
  down.kind = vc::MouseEvent::Kind::Down;
  down.x = 76;
  down.y = sb.thumb_y + sb.thumb_h / 2;
  d.view->mouse(down);
  vc::MouseEvent drag = down;
  drag.kind = vc::MouseEvent::Kind::Drag;
  drag.y = 80;
  d.view->mouse(drag);
  vc::MouseEvent up = drag;
  up.kind = vc::MouseEvent::Kind::Up;
  d.view->mouse(up);
  CHECK(d.view->has_pending_scroll());

  vc::MouseEvent wheel;
  wheel.kind = vc::MouseEvent::Kind::Scroll;
  wheel.dy = 40;
  d.view->mouse(wheel);
  CHECK(!d.view->has_pending_scroll());
  CHECK(!d.view->apply_pending_scroll());
}

TEST(viewcore_scrollbar_hidden_when_the_document_fits) {
  Doc d(n_lines(5));
  d.view->resize(80, 160);
  d.view->discover_all();
  CHECK(!d.view->scrollbar().visible);
}

TEST(viewcore_scrollbar_thumb_tracks_the_position) {
  Doc d(n_lines(200));
  d.view->resize(80, 160);
  d.view->discover_all();
  const double top = d.view->scrollbar().thumb_y;
  d.view->scroll_to_line(100);
  const double mid = d.view->scrollbar().thumb_y;
  d.view->scroll_to_line(200);
  const double bot = d.view->scrollbar().thumb_y;
  CHECK_EQ(top, 0.0);
  CHECK(mid > top);
  CHECK(bot > mid);
  CHECK_EQ(bot, 160.0 - 28.0);  // bottom-aligned at the last scroll position
}

TEST(viewcore_scrollbar_drag_scrolls_the_document) {
  Doc d(n_lines(200));
  d.view->resize(80, 160);
  d.view->discover_all();
  d.view->set_caret(3, 2);  // a drag must not move the caret

  vc::MouseEvent down;
  down.kind = vc::MouseEvent::Kind::Down;
  down.x = 80 - 4;
  down.y = 5;  // inside the thumb
  d.view->mouse(down);

  vc::MouseEvent drag;
  drag.kind = vc::MouseEvent::Kind::Drag;
  drag.x = down.x;
  drag.y = 155;  // the bottom of the track
  CHECK(d.view->mouse(drag));
  CHECK_EQ(d.view->first_visible_line(), 190);  // the last page (200 - 10 visible)
  CHECK_EQ(d.view->selection().caret_line, 3);

  // Dragging back up returns to the top.
  drag.y = 0;
  d.view->mouse(drag);
  CHECK_EQ(d.view->first_visible_line(), 0);

  vc::MouseEvent up;
  up.kind = vc::MouseEvent::Kind::Up;
  up.x = down.x;
  up.y = 0;
  d.view->mouse(up);
  drag.y = 155;  // after mouse-up a drag is no longer a scrollbar drag
  d.view->mouse(drag);
  CHECK_EQ(d.view->first_visible_line(), 0);
}

TEST(viewcore_scrollbar_track_click_pages) {
  Doc d(n_lines(200));
  d.view->resize(80, 160);
  d.view->discover_all();

  vc::MouseEvent click;
  click.kind = vc::MouseEvent::Kind::Down;
  click.x = 80 - 4;
  click.y = 150;  // below the thumb
  CHECK(d.view->mouse(click));
  CHECK_EQ(d.view->first_visible_line(), 9);  // one viewport (10) minus the overlap

  // Command-click jumps toward the pointer instead.
  click.cmd = true;
  click.y = 80;
  d.view->mouse(click);
  const int64_t jumped = d.view->first_visible_line();
  CHECK(jumped > 9);
  CHECK(jumped < 200);
}

TEST(viewcore_scrollbar_accounts_for_wrap) {
  std::string content;
  for (int i = 0; i < 20; ++i) {
    content += "the quick brown fox jumps over the lazy dog again and again\n";
  }
  Doc d(content);
  d.view->resize(80, 160);  // 10 monospace columns: each line wraps ~3 times
  d.view->discover_all();
  const double plain = d.view->scrollbar().thumb_h;
  d.view->set_wrap(true);
  const double wrapped = d.view->scrollbar().thumb_h;
  // Fewer logical lines fit when wrapped, so the thumb is proportionally shorter.
  CHECK(wrapped < plain);
}

// --- Vertical scrolling in pixels (wrap, and the very end of a document) -----

TEST(viewcore_wrapped_single_line_has_a_scrollbar_and_stops_at_the_end) {
  // One logical line that wraps into many screens. Counting logical lines says
  // "one line fits", which used to mean no bar at all; the scroll range has to
  // be measured in pixels.
  std::string content(4000, 'x');
  content += "\n";
  Doc d(content);
  d.view->resize(80, 160);
  d.view->set_wrap(true);
  d.view->discover_all();
  CHECK(d.view->scrollbar().visible);

  vc::MouseEvent wheel;
  wheel.kind = vc::MouseEvent::Kind::Scroll;
  wheel.dy = 1e6;  // far past the end
  d.view->mouse(wheel);
  // The last legal position shows the end of the text, with no blank space
  // below it (the old model let a wrapped document scroll on forever).
  CHECK_EQ(d.view->first_visible_line(), 0);
  const double top = d.view->scroll_px();
  const double content_px = d.view->content_height();
  CHECK(top > 0);
  CHECK(top + 160.0 <= content_px + 1e-6);
  CHECK(top + 160.0 >= content_px - 1e-6);
  // And the thumb sits at the bottom of the track.
  const vc::ViewCore::ScrollBar sb = d.view->scrollbar();
  CHECK_EQ(sb.thumb_y + sb.thumb_h, 160.0);
}

TEST(viewcore_wrapped_drag_follows_the_pointer_and_stops_inside_a_line) {
  // Lines that each wrap into many rows: a drag must be able to land part-way
  // through one (not snap to logical line boundaries), and the thumb must stay
  // under the pointer.
  std::string content;
  for (int i = 0; i < 40; ++i) content += std::string(600, 'x') + "\n";
  Doc d(content);
  d.view->resize(80, 160);
  d.view->set_wrap(true);
  d.view->discover_all();
  const vc::ViewCore::ScrollBar sb = d.view->scrollbar();
  CHECK(sb.visible);
  const double travel = 160.0 - sb.thumb_h;

  vc::MouseEvent down;
  down.kind = vc::MouseEvent::Kind::Down;
  down.x = 78;
  down.y = sb.thumb_y + 2;  // grab the thumb
  const double grab = 2;
  d.view->mouse(down);

  vc::MouseEvent drag;
  drag.kind = vc::MouseEvent::Kind::Drag;
  drag.x = 78;
  drag.y = sb.thumb_y + 2 + travel / 2;  // half way down the track
  d.view->mouse(drag);

  const vc::ViewCore::ScrollBar moved = d.view->scrollbar();
  CHECK(std::abs(moved.thumb_y - (drag.y - grab)) < 0.51);  // under the pointer
  CHECK(d.view->scroll_px() > 0.5);  // inside a wrapped line, not on a boundary
  CHECK(d.view->first_visible_line() > 0);
}

TEST(viewcore_last_points_are_reachable_when_the_viewport_is_not_a_line_multiple) {
  // A 165 px viewport over 16 px lines: the old line-based clamp stopped with
  // 11 px of the document still below the viewport, unreachable.
  Doc d(n_lines(20));
  d.view->resize(80, 165);
  d.view->discover_all();
  vc::MouseEvent wheel;
  wheel.kind = vc::MouseEvent::Kind::Scroll;
  wheel.dy = 1e6;
  d.view->mouse(wheel);
  const double lh = d.view->line_height();
  const double top = static_cast<double>(d.view->first_visible_line()) * lh +
                     d.view->scroll_px();
  const double content_px = d.view->content_height();
  CHECK(top + 165.0 >= content_px - 1e-6);  // the end is visible
  CHECK(top + 165.0 <= content_px + 1e-6);  // and nothing is blank past it
}

TEST(viewcore_wrapped_track_click_pages_by_a_screen_not_a_logical_line) {
  // A wrapped logical line is ~60 rows tall; paging by "logical lines in the
  // viewport" would jump most of a screen's worth of them at once.
  std::string content;
  for (int i = 0; i < 40; ++i) content += std::string(600, 'x') + "\n";
  Doc d(content);
  d.view->resize(80, 160);
  d.view->set_wrap(true);
  d.view->discover_all();

  vc::MouseEvent click;
  click.kind = vc::MouseEvent::Kind::Down;
  click.x = 78;
  click.y = 159;  // the track, below the thumb
  d.view->mouse(click);
  // One page of rows: 160/16 - 1 = 9 rows, i.e. 144 px, still inside the first
  // (very tall) logical line.
  CHECK_EQ(d.view->first_visible_line(), 0);
  CHECK_EQ(d.view->scroll_px(), 144.0);
}

TEST(viewcore_content_height_accounts_for_wrap) {
  Doc d(n_lines(20, "the quick brown fox jumps over the lazy dog again\n"));
  d.view->resize(80, 160);
  d.view->discover_all();
  const double plain = d.view->content_height();
  d.view->set_wrap(true);
  const double wrapped = d.view->content_height();
  CHECK(plain > 0);
  CHECK(wrapped > plain);  // wrapped rows are taller than the logical lines
}

// --- Page movement and caret visibility (wrap) -----------------------------

TEST(viewcore_page_move_is_a_screen_under_wrap) {
  // Every 5th line is very tall; a page must be a screen of rows, not a count
  // of logical lines, and the caret must land inside the viewport.
  std::string content;
  for (int i = 0; i < 60; ++i) {
    const int chars = (i % 5 == 0) ? 600 : 30;
    content.append(static_cast<size_t>(chars), 'x');
    content += "\n";
  }
  Doc d(content);
  d.view->resize(80, 160);
  d.view->set_wrap(true);
  d.view->discover_all();

  vc::KeyEvent k;
  k.key = vc::Key::PageDown;
  const int64_t first = d.view->first_visible_line();
  const double px = d.view->scroll_px();
  CHECK(d.view->key(k));
  // Moved down by about a screen of rows, not by a screen of logical lines.
  const double moved = (static_cast<double>(d.view->first_visible_line() -
                                            first) *
                           16.0 +
                        d.view->scroll_px() - px);
  CHECK(moved > 100.0);
  CHECK(moved <= 161.0);
  // The caret is on screen (the fix: it used to be placed on a line that had
  // just been scrolled past, and ensure_caret_visible undid the page).
  const vc::Rect cr = d.view->caret_rect(d.view->selection().caret_line,
                                         d.view->selection().caret_col);
  CHECK(cr.y >= 0.0);
  CHECK(cr.y + cr.h <= d.view->viewport_height() + 0.01);

  // And back up again.
  k.key = vc::Key::PageUp;
  CHECK(d.view->key(k));
  const vc::Rect up = d.view->caret_rect(d.view->selection().caret_line,
                                         d.view->selection().caret_col);
  CHECK(up.y >= 0.0);
  CHECK(up.y + up.h <= d.view->viewport_height() + 0.01);
}

TEST(viewcore_caret_stays_put_when_its_row_is_visible_under_wrap) {
  // The caret's logical line starts above the viewport but the caret's own row
  // is visible: scrolling it to the top would be wrong (the whole point of
  // measuring the caret rect instead of counting logical lines).
  std::string content;
  for (int i = 0; i < 20; ++i) {
    const int chars = (i % 3 == 0) ? 400 : 30;  // every 3rd line is tall
    content.append(static_cast<size_t>(chars), 'x');
    content += "\n";
  }
  Doc d(content);
  d.view->resize(80, 160);
  d.view->set_wrap(true);
  d.view->discover_all();
  // Scroll into the middle of the tall line 3.
  d.view->scroll_to_line(3);
  vc::MouseEvent w;
  w.kind = vc::MouseEvent::Kind::Scroll;
  w.dy = 500;
  d.view->mouse(w);
  // A row that is actually on screen, on a line that starts above the viewport.
  const vc::Hit h =
      d.view->hit_test(0, d.view->viewport_height() / 2);
  CHECK(h.line == 3);
  d.view->set_caret(h.line, h.col);
  const int64_t first = d.view->first_visible_line();
  const double px = d.view->scroll_px();

  vc::KeyEvent k;
  k.key = vc::Key::Right;  // moves the caret, which calls ensure_caret_visible
  d.view->key(k);
  CHECK_EQ(d.view->first_visible_line(), first);
  CHECK_EQ(d.view->scroll_px(), px);
}

// --- Horizontal scrolling (wrap off) --------------------------------------

TEST(viewcore_horizontal_bar_appears_for_a_long_line) {
  Doc d("0123456789abcdefghij0123456789abcdefghij\n");  // 40 chars = 320 px
  d.view->resize(80, 160);                              // 10 characters wide
  vc::RecordingCanvas rc;
  d.view->draw(rc);
  const vc::ViewCore::HScrollBar hb = d.view->hscrollbar();
  CHECK(hb.visible);
  CHECK_EQ(d.view->x_offset(), 0.0);
  CHECK_EQ(hb.track_y, 160.0 - vc::ViewCore::kScrollbarWidth);
  CHECK_EQ(hb.track_h, vc::ViewCore::kScrollbarWidth);
  CHECK_EQ(hb.thumb_x, 0.0);   // text_left() == 0 (no gutter)
  CHECK_EQ(hb.thumb_w, 28.0);  // minimum thumb

  d.view->scroll_x_by(40);
  CHECK_EQ(d.view->x_offset(), 40.0);
  d.view->scroll_x_by(1e6);
  CHECK(d.view->x_offset() > 200.0);  // content 320 + pad 24 - viewport 80
  const vc::ViewCore::HScrollBar end = d.view->hscrollbar();
  CHECK_EQ(end.thumb_x + end.thumb_w, 80.0);  // pinned to the right edge
  d.view->scroll_x_by(-1e6);
  CHECK_EQ(d.view->x_offset(), 0.0);
}

TEST(viewcore_horizontal_pan_shifts_text_and_hit_test) {
  Doc d("0123456789abcdefghij\n");
  d.view->resize(80, 100);
  vc::RecordingCanvas rc;
  d.view->draw(rc);         // measure the line: 20 chars = 160 px
  d.view->scroll_x_by(40);  // pan by five characters
  CHECK_EQ(d.view->x_offset(), 40.0);

  const vc::Hit h = d.view->hit_test(0, 2);
  CHECK_EQ(h.line, 0);
  CHECK_EQ(h.col, 5);  // the fifth character now sits at the left edge

  const vc::Rect r = d.view->caret_rect(0, 5);
  CHECK_EQ(r.x, 0.0);

  vc::RecordingCanvas panned;
  d.view->draw(panned);
  bool saw_run = false;
  for (const vc::DrawCmd& c : panned.cmds) {
    if (c.kind == vc::DrawCmd::Kind::DrawRun &&
        c.text == "0123456789abcdefghij") {
      CHECK_EQ(c.x, -40.0);  // the run starts one pan to the left
      saw_run = true;
    }
  }
  CHECK(saw_run);
}

// An edit re-measures the lines; that must not snap a panned view back to the
// left edge (the horizontal range is a running maximum, not rebuilt per key).
TEST(viewcore_horizontal_pan_survives_typing) {
  Doc d("0123456789abcdefghij\n");
  d.view->resize(80, 100);
  vc::RecordingCanvas rc;
  d.view->draw(rc);
  vc::KeyEvent right;
  right.key = vc::Key::Right;
  for (int i = 0; i < 6; ++i) d.view->key(right);  // caret to column 6
  d.view->scroll_x_by(40);
  CHECK_EQ(d.view->x_offset(), 40.0);
  CHECK(d.view->hscrollbar().visible);

  CHECK(d.view->insert_text("Z"));
  CHECK_EQ(d.view->x_offset(), 40.0);
  CHECK(d.view->hscrollbar().visible);
}

TEST(viewcore_wrap_turns_horizontal_scrolling_off) {
  Doc d("0123456789abcdefghij0123456789abcdefghij\n");
  d.view->resize(80, 160);
  vc::RecordingCanvas rc;
  d.view->draw(rc);  // measures the line, so there is something to pan
  d.view->scroll_x_by(50);
  CHECK(d.view->hscrollbar().visible);
  d.view->set_wrap(true);
  CHECK_EQ(d.view->x_offset(), 0.0);
  CHECK(!d.view->hscrollbar().visible);
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

TEST(viewcore_caret_blink_toggle) {
  Doc d("hello\n");
  d.view->resize(800, 1000);
  d.view->set_caret(0, 3);
  d.view->set_caret_visible(false);
  vc::RecordingCanvas rc;
  d.view->draw(rc);
  bool caret = false;
  for (const vc::DrawCmd& cmd : rc.cmds) {
    if (cmd.kind == vc::DrawCmd::Kind::FillRect && cmd.rect.w == 1.5) caret = true;
  }
  CHECK(!caret);
}

TEST(viewcore_tab_width_advances_to_stop) {
  Doc d("a\tb\n");
  d.view->resize(800, 1000);
  d.view->set_wrap(false);
  d.view->set_tab_width(4);
  CHECK_EQ(d.view->caret_rect(0, 1).x, 8.0);   // at the tab
  CHECK_EQ(d.view->caret_rect(0, 2).x, 32.0);  // 'b' at column 4
  d.view->set_tab_width(8);
  CHECK_EQ(d.view->caret_rect(0, 2).x, 64.0);  // 'b' at column 8
}

TEST(viewcore_gutter_draws_line_numbers_and_offsets_text) {
  Doc d("a\nb\nc\n");
  d.view->resize(800, 1000);
  d.view->set_gutter(true);
  const double gw = d.view->gutter_width();
  CHECK(gw > 0);
  vc::RecordingCanvas rc;
  d.view->draw(rc);
  int numbers = 0;
  bool text_offset = false;
  for (const vc::DrawCmd& cmd : rc.cmds) {
    if (cmd.kind != vc::DrawCmd::Kind::DrawRun) continue;
    if (cmd.text == "1" || cmd.text == "2" || cmd.text == "3") ++numbers;
    if (cmd.text == "a" && cmd.x == gw) text_offset = true;
  }
  CHECK_EQ(numbers, 3);
  CHECK(text_offset);
}

TEST(viewcore_gutter_hit_test_offsets_x) {
  Doc d("hello\n");
  d.view->resize(800, 1000);
  d.view->set_gutter(true);
  const double gw = d.view->gutter_width();
  const vc::Hit h = d.view->hit_test(gw + 8 * 2 + 1, 8);
  CHECK_EQ(h.line, 0);
  CHECK_EQ(h.col, 2);
}

TEST(viewcore_selected_text) {
  Doc d("hello world\nsecond\n");
  d.view->resize(800, 1000);
  d.view->set_caret(0, 0);
  vc::KeyEvent right;
  right.key = vc::Key::Right;
  right.shift = true;
  for (int i = 0; i < 5; ++i) d.view->key(right);
  CHECK_EQ(d.view->selected_text(), std::string("hello"));

  // Across a line boundary.
  d.view->set_caret(0, 6);
  vc::KeyEvent down;
  down.key = vc::Key::Down;
  down.shift = true;
  d.view->key(down);
  CHECK_EQ(d.view->selected_text(), std::string("world\nsecond"));
}

// --- Word motion, click granularity, command keys --------------------------

TEST(viewcore_word_motion_stops_at_run_ends) {
  Doc d("foo bar-baz\nsecond line\n");
  d.view->resize(800, 1000);
  d.view->set_caret(0, 0);
  vc::KeyEvent fwd;
  fwd.key = vc::Key::Right;
  fwd.alt = true;
  CHECK(d.view->key(fwd));
  CHECK_EQ(d.view->selection().caret_col, 3);  // end of "foo", as macOS does
  d.view->key(fwd);
  CHECK_EQ(d.view->selection().caret_col, 7);  // end of "bar"
  d.view->key(fwd);
  CHECK_EQ(d.view->selection().caret_col, 8);  // the "-" run
  d.view->key(fwd);
  CHECK_EQ(d.view->selection().caret_col, 11);  // end of "baz"
  d.view->key(fwd);  // the line ends: step onto the next line
  CHECK_EQ(d.view->selection().caret_line, 1);
  CHECK_EQ(d.view->selection().caret_col, 0);

  vc::KeyEvent back;
  back.key = vc::Key::Left;
  back.alt = true;
  d.view->key(back);
  CHECK_EQ(d.view->selection().caret_line, 0);
  CHECK_EQ(d.view->selection().caret_col, 11);  // previous line end
  d.view->key(back);
  CHECK_EQ(d.view->selection().caret_col, 8);  // start of "baz"
  d.view->key(back);
  CHECK_EQ(d.view->selection().caret_col, 7);  // start of "-"
  d.view->key(back);
  CHECK_EQ(d.view->selection().caret_col, 4);  // start of "bar"
  d.view->key(back);
  CHECK_EQ(d.view->selection().caret_col, 0);  // start of "foo"
}

TEST(viewcore_word_motion_extends_a_selection) {
  Doc d("alpha beta\n");
  d.view->resize(800, 1000);
  d.view->set_caret(0, 0);
  vc::KeyEvent fwd;
  fwd.key = vc::Key::Right;
  fwd.alt = true;
  fwd.shift = true;
  d.view->key(fwd);
  CHECK_EQ(d.view->selected_text(), std::string("alpha"));
  d.view->key(fwd);
  CHECK_EQ(d.view->selected_text(), std::string("alpha beta"));
}

TEST(viewcore_word_motion_counts_non_ascii_as_letters) {
  // "héllo wörld": é and ö are two bytes each, so the word ends at byte 6.
  Doc d("h\xc3\xa9llo w\xc3\xb6rld\n");
  d.view->resize(800, 1000);
  d.view->set_caret(0, 0);
  vc::KeyEvent fwd;
  fwd.key = vc::Key::Right;
  fwd.alt = true;
  d.view->key(fwd);
  CHECK_EQ(d.view->selection().caret_col, 6);
  d.view->key(fwd);
  CHECK_EQ(d.view->selection().caret_col, 13);  // end of "wörld"
  CHECK_EQ(d.view->selected_text(), std::string());
}

TEST(viewcore_word_delete) {
  Doc d("foo bar baz\n");
  d.view->resize(800, 1000);
  d.view->set_caret(0, 7);  // end of "bar"
  vc::KeyEvent bs;
  bs.key = vc::Key::Backspace;
  bs.alt = true;
  CHECK(d.view->key(bs));
  CHECK_EQ(d.text(), std::string("foo  baz\n"));
  CHECK_EQ(d.view->selection().caret_col, 4);

  Doc e("foo bar baz\n");
  e.view->resize(800, 1000);
  e.view->set_caret(0, 0);
  vc::KeyEvent del;
  del.key = vc::Key::Delete;
  del.alt = true;
  CHECK(e.view->key(del));
  CHECK_EQ(e.text(), std::string(" bar baz\n"));
  CHECK_EQ(e.view->selection().caret_col, 0);
}

TEST(viewcore_double_click_selects_the_word_and_drag_keeps_it) {
  Doc d("alpha beta gamma\n");
  d.view->resize(800, 1000);
  vc::MouseEvent down;
  down.kind = vc::MouseEvent::Kind::Down;
  down.clicks = 2;
  down.x = 6 * 8 + 2;  // inside "beta"
  down.y = 8;
  d.view->mouse(down);
  CHECK_EQ(d.view->selected_text(), std::string("beta"));

  vc::MouseEvent drag = down;
  drag.kind = vc::MouseEvent::Kind::Drag;
  drag.x = 13 * 8 + 2;  // inside "gamma"
  d.view->mouse(drag);
  CHECK_EQ(d.view->selected_text(), std::string("beta gamma"));

  drag.x = 2;  // back over the first word
  d.view->mouse(drag);
  CHECK_EQ(d.view->selected_text(), std::string("alpha "));
}

TEST(viewcore_triple_click_selects_the_line) {
  Doc d("one\ntwo\nthree\n");
  d.view->resize(800, 1000);
  vc::MouseEvent down;
  down.kind = vc::MouseEvent::Kind::Down;
  down.clicks = 3;
  down.x = 8;
  down.y = 16 + 8;  // the second line
  d.view->mouse(down);
  CHECK_EQ(d.view->selected_text(), std::string("two"));

  vc::MouseEvent drag = down;
  drag.kind = vc::MouseEvent::Kind::Drag;
  drag.y = 32 + 8;  // onto the third line
  d.view->mouse(drag);
  CHECK_EQ(d.view->selected_text(), std::string("two\nthree"));

  drag.y = 8;  // and back up onto the first
  d.view->mouse(drag);
  CHECK_EQ(d.view->selected_text(), std::string("one\ntwo"));
}

TEST(viewcore_command_arrows_reach_the_line_and_document_edges) {
  Doc d("first\nsecond\nthird\n");
  d.view->resize(800, 1000);
  d.view->set_caret(1, 3);
  vc::KeyEvent right;
  right.key = vc::Key::Right;
  right.cmd = true;
  d.view->key(right);
  CHECK_EQ(d.view->selection().caret_line, 1);
  CHECK_EQ(d.view->selection().caret_col, 6);  // end of "second"
  vc::KeyEvent left;
  left.key = vc::Key::Left;
  left.cmd = true;
  d.view->key(left);
  CHECK_EQ(d.view->selection().caret_col, 0);

  vc::KeyEvent down;
  down.key = vc::Key::Down;
  down.cmd = true;
  d.view->key(down);
  CHECK_EQ(d.view->selection().caret_line, 2);
  CHECK_EQ(d.view->selection().caret_col, 5);  // end of the document
  vc::KeyEvent up;
  up.key = vc::Key::Up;
  up.cmd = true;
  d.view->key(up);
  CHECK_EQ(d.view->selection().caret_line, 0);
  CHECK_EQ(d.view->selection().caret_col, 0);
}

TEST(viewcore_theme_light_and_dark) {
  Doc d("hello\n");
  d.view->resize(800, 1000);
  d.view->set_theme(vc::light_theme());
  vc::RecordingCanvas lc;
  d.view->draw(lc);
  CHECK(!lc.cmds.empty());
  CHECK(lc.cmds.front().kind == vc::DrawCmd::Kind::Clear);
  CHECK_EQ(lc.cmds.front().color.r, 255);  // light surface #ffffff
  CHECK_EQ(lc.cmds.front().color.g, 255);

  d.view->set_theme(vc::dark_theme());
  vc::RecordingCanvas dc;
  d.view->draw(dc);
  CHECK_EQ(dc.cmds.front().color.r, 27);  // dark surface #1b1b1b
  CHECK_EQ(dc.cmds.front().color.g, 27);
  CHECK_EQ(dc.cmds.front().color.b, 27);
}

TEST(viewcore_edit_insert_text) {
  Doc d("abc\n");
  d.view->resize(800, 1000);
  d.view->set_caret(0, 1);
  CHECK(d.view->insert_text("X"));
  CHECK_EQ(d.text(), std::string("aXbc\n"));
  CHECK_EQ(d.view->selection().caret_line, 0);
  CHECK_EQ(d.view->selection().caret_col, 2);
}

TEST(viewcore_edit_insert_replaces_selection) {
  Doc d("hello world\n");
  d.view->resize(800, 1000);
  d.view->set_caret(0, 0);
  vc::KeyEvent right;
  right.key = vc::Key::Right;
  right.shift = true;
  for (int i = 0; i < 5; ++i) d.view->key(right);  // select "hello"
  CHECK(d.view->insert_text("bye"));
  CHECK_EQ(d.text(), std::string("bye world\n"));
  CHECK_EQ(d.view->selection().caret_col, 3);
}

TEST(viewcore_edit_enter_splits_line) {
  Doc d("abcd\n");
  d.view->resize(800, 1000);
  d.view->set_caret(0, 2);
  vc::KeyEvent enter;
  enter.key = vc::Key::Enter;
  CHECK(d.view->key(enter));
  CHECK_EQ(d.text(), std::string("ab\ncd\n"));
  CHECK_EQ(d.view->selection().caret_line, 1);
  CHECK_EQ(d.view->selection().caret_col, 0);
}

TEST(viewcore_edit_backspace_merges_lines) {
  Doc d("ab\ncd\n");
  d.view->resize(800, 1000);
  d.view->set_caret(1, 0);
  CHECK(d.view->backspace());
  CHECK_EQ(d.text(), std::string("abcd\n"));
  CHECK_EQ(d.view->selection().caret_line, 0);
  CHECK_EQ(d.view->selection().caret_col, 2);
}

TEST(viewcore_edit_backspace_deletes_a_codepoint) {
  Doc d("a\xC3\xA9z\n");  // a, e-acute, z
  d.view->resize(800, 1000);
  d.view->set_caret(0, 3);  // just past the two-byte e-acute
  CHECK(d.view->backspace());
  CHECK_EQ(d.text(), std::string("az\n"));
  CHECK_EQ(d.view->selection().caret_col, 1);
}

TEST(viewcore_edit_delete_forward) {
  Doc d("abc\n");
  d.view->resize(800, 1000);
  d.view->set_caret(0, 0);
  CHECK(d.view->delete_forward());
  CHECK_EQ(d.text(), std::string("bc\n"));
  CHECK_EQ(d.view->selection().caret_col, 0);
}

TEST(viewcore_edit_undo_via_document_changed) {
  Doc d("abc\n");
  d.view->resize(800, 1000);
  d.view->set_caret(0, 3);
  CHECK(d.view->insert_text("!"));
  CHECK_EQ(d.text(), std::string("abc!\n"));
  CHECK(d.ed->undo() == Status::Ok);
  d.view->document_changed();
  CHECK_EQ(d.text(), std::string("abc\n"));
  CHECK(d.view->line_count_known());
  CHECK_EQ(d.view->line_count(), 1);
}

TEST(viewcore_find_literal_wraps) {
  Doc d("alpha beta\ngamma beta\n");
  d.view->resize(800, 1000);
  d.view->set_caret(0, 0);
  CHECK(d.view->find("beta", true, true));
  CHECK_EQ(d.view->selection().anchor_line, 0);
  CHECK_EQ(d.view->selection().anchor_col, 6);
  CHECK_EQ(d.view->selection().caret_col, 10);
  CHECK(d.view->find("beta", true, true));
  CHECK_EQ(d.view->selection().anchor_line, 1);
  CHECK_EQ(d.view->selection().anchor_col, 6);
  CHECK(d.view->find("beta", true, true));  // wraps to the top
  CHECK_EQ(d.view->selection().anchor_line, 0);
}

TEST(viewcore_find_case_insensitive) {
  Doc d("Hello\n");
  d.view->resize(800, 1000);
  d.view->set_caret(0, 0);
  CHECK(d.view->find("hello", true, false));
  CHECK_EQ(d.view->selection().anchor_col, 0);
  CHECK(!d.view->find("hello", true, true));  // case-sensitive: no match
}

TEST(viewcore_find_backward) {
  Doc d("a b a b a\n");
  d.view->resize(800, 1000);
  d.view->set_caret(0, 9);  // end of the line
  CHECK(d.view->find("a", false, true));
  CHECK_EQ(d.view->selection().anchor_col, 8);
}

TEST(viewcore_replace_current) {
  Doc d("foo bar foo\n");
  d.view->resize(800, 1000);
  d.view->set_caret(0, 0);
  CHECK(d.view->find("foo", true, true));
  CHECK(d.view->replace_current("foo", "baz", true));
  CHECK_EQ(d.text(), std::string("baz bar foo\n"));
  CHECK_EQ(d.view->selection().anchor_col, 8);  // the next match
}

TEST(viewcore_replace_all) {
  Doc d("a a a\na a\n");
  d.view->resize(800, 1000);
  CHECK_EQ(d.view->replace_all("a", "bb", true), 5);
  CHECK_EQ(d.text(), std::string("bb bb bb\nbb bb\n"));
}

