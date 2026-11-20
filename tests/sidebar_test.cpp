// tests/sidebar_test.cpp — the file sidebar's tree, laziness and layout (§9).
//
// The tree is platform-neutral and its directory reads sit behind `DirSource`,
// so the whole model — laziness, sorting, reveal, drawing — is checked here
// with an in-memory source rather than only by looking at the window.
#include <algorithm>
#include <map>
#include <memory>
#include <set>
#include <string>
#include <vector>

#include "check.hpp"
#include "headless.hpp"
#include "sidebar.hpp"

namespace {

// A DirSource over an in-memory tree: path -> children. Records which
// directories were read, so a test can assert that a collapsed subtree was
// never touched (the laziness guarantee).
class FakeDir : public vc::DirSource {
 public:
  std::map<std::string, std::vector<vc::DirEntry>> dirs;
  std::vector<std::string> reads;  // every directory read, in order

  bool read_dir(std::string_view path, std::vector<vc::DirEntry>* out) override {
    const std::string p(path);
    reads.push_back(p);
    auto it = dirs.find(p);
    if (it == dirs.end()) return false;
    *out = it->second;
    return true;
  }

  // A real readdir() returns entries in an arbitrary order. Add in reverse so
  // every fixture reaches the sidebar unsorted, and the sort is exercised.
  void add(const std::string& dir, std::string name, bool is_dir) {
    dirs[dir].insert(dirs[dir].begin(),
                     vc::DirEntry{name, dir + "/" + name, is_dir, false});
  }
};

struct Fixture {
  FakeDir src;
  vc::HeadlessTextBackend tb;
  std::unique_ptr<vc::Sidebar> side;

  Fixture() {
    // /src
    //   alpha.txt
    //   beta/            <- collapsed
    //     inner.txt
    //     deeper/        <- collapsed
    //       deep.txt
    //   zeta.txt
    src.add("/src", "alpha.txt", false);
    src.add("/src", "beta", true);
    src.add("/src", "zeta.txt", false);
    src.add("/src/beta", "inner.txt", false);
    src.add("/src/beta", "deeper", true);
    src.add("/src/beta/deeper", "deep.txt", false);
    side = std::make_unique<vc::Sidebar>(src, tb);
  }

  std::vector<std::string> names() {
    std::vector<std::string> out;
    for (const vc::Sidebar::Row& r : side->rows()) out.push_back(r.node->name);
    return out;
  }
};

// --- Laziness ---------------------------------------------------------------

TEST(sidebar_reads_only_the_root_until_expanded) {
  Fixture f;
  CHECK(f.side->set_root("/src"));
  CHECK(f.side->loaded_root());
  // The root was read; no child directory was.
  CHECK(f.src.reads.size() == 1);
  CHECK(f.src.reads[0] == "/src");
  // Folders first: "beta" leads, then the files.
  CHECK(f.names() == std::vector<std::string>({"beta", "alpha.txt", "zeta.txt"}));
}

TEST(sidebar_collapsed_subtree_is_never_walked) {
  Fixture f;
  f.side->set_root("/src");
  // Read everything on screen a few times (the draw path does this per frame).
  for (int i = 0; i < 5; ++i) (void)f.side->rows();
  // Only /src: "beta" is collapsed, so /src/beta was never read.
  CHECK(f.src.reads.size() == 1);
  CHECK(std::find(f.src.reads.begin(), f.src.reads.end(), "/src/beta") ==
        f.src.reads.end());
}

TEST(sidebar_expanding_reads_that_directory_only) {
  Fixture f;
  f.side->set_root("/src");
  const vc::SidebarNode* beta = nullptr;
  for (const vc::Sidebar::Row& r : f.side->rows()) {
    if (r.node->name == "beta") beta = r.node;
  }
  CHECK(beta != nullptr);
  CHECK(f.side->toggle(beta));
  // "beta" is open, so its children (folders first) follow it; the files of
  // /src come after, still in order.
  CHECK(f.names() == std::vector<std::string>({"beta", "deeper", "inner.txt",
                                               "alpha.txt", "zeta.txt"}));
  // /src/beta was read, and /src/beta/deeper (collapsed) was not.
  CHECK(std::find(f.src.reads.begin(), f.src.reads.end(), "/src/beta") !=
        f.src.reads.end());
  CHECK(std::find(f.src.reads.begin(), f.src.reads.end(),
                  "/src/beta/deeper") == f.src.reads.end());
}

TEST(sidebar_collapsing_again_does_not_reread) {
  Fixture f;
  f.side->set_root("/src");
  const vc::SidebarNode* beta = nullptr;
  for (const vc::Sidebar::Row& r : f.side->rows()) {
    if (r.node->name == "beta") beta = r.node;
  }
  f.side->toggle(beta);  // expand: reads /src/beta
  f.side->toggle(beta);  // collapse
  f.side->toggle(beta);  // expand again: children are cached, no new read
  const int beta_reads = static_cast<int>(std::count(
      f.src.reads.begin(), f.src.reads.end(), "/src/beta"));
  CHECK_EQ(beta_reads, 1);
}

TEST(sidebar_an_unreadable_root_reports_failure_and_stays_empty) {
  vc::HeadlessTextBackend tb;

  // A source with no directories at all: every read fails.
  FakeDir empty;
  vc::Sidebar side(empty, tb);
  CHECK(!side.set_root("/nothing"));
  CHECK(!side.loaded_root());
  CHECK(side.rows().empty());

  // An empty directory is a *successful* read with no children — that must not
  // be confused with a failure.
  FakeDir ok;
  ok.dirs["/empty"] = {};
  vc::Sidebar s2(ok, tb);
  CHECK(s2.set_root("/empty"));
  CHECK(s2.loaded_root());
  CHECK(s2.rows().empty());
}

// --- Sorting ----------------------------------------------------------------

TEST(sidebar_sorts_folders_first_then_case_insensitively) {
  FakeDir src;
  src.add("/r", "Zebra.txt", false);
  src.add("/r", "apple.txt", false);
  src.add("/r", "Mango", true);
  src.add("/r", "banana", true);
  vc::HeadlessTextBackend tb;
  vc::Sidebar side(src, tb);
  side.set_root("/r");
  // Folders first (banana, Mango — case-insensitive, so banana < Mango), then
  // files (apple.txt, Zebra.txt).
  CHECK(side.rows().size() == 4);
  CHECK(side.rows()[0].node->name == std::string("banana"));
  CHECK(side.rows()[1].node->name == std::string("Mango"));
  CHECK(side.rows()[2].node->name == std::string("apple.txt"));
  CHECK(side.rows()[3].node->name == std::string("Zebra.txt"));
}

TEST(sidebar_sort_is_stable_for_names_equal_ignoring_case) {
  FakeDir src;
  src.add("/r", "b.txt", false);
  src.add("/r", "B.txt", false);
  vc::HeadlessTextBackend tb;
  vc::Sidebar side(src, tb);
  side.set_root("/r");
  // Both orders are valid; what matters is that two runs agree.
  const std::string first = side.rows()[0].node->name;
  const std::string second = side.rows()[1].node->name;
  CHECK(first != second);
}

// --- Reveal -----------------------------------------------------------------

TEST(sidebar_reveal_expands_ancestors_and_selects_the_file) {
  Fixture f;
  f.side->set_root("/src");
  CHECK(f.side->reveal("/src/beta/deeper/deep.txt"));
  CHECK(f.side->selected_path() == "/src/beta/deeper/deep.txt");
  // The chain is on screen now, in order.
  const std::vector<std::string> want = {"beta", "deeper", "deep.txt",
                                         "inner.txt", "alpha.txt", "zeta.txt"};
  CHECK(f.names() == want);
  // Both directories along the way were read.
  CHECK(std::find(f.src.reads.begin(), f.src.reads.end(), "/src/beta") !=
        f.src.reads.end());
  CHECK(std::find(f.src.reads.begin(), f.src.reads.end(),
                  "/src/beta/deeper") != f.src.reads.end());
}

TEST(sidebar_reveal_is_idempotent) {
  Fixture f;
  f.side->set_root("/src");
  CHECK(f.side->reveal("/src/beta/deeper/deep.txt"));
  const size_t reads_after_first = f.src.reads.size();
  CHECK(!f.side->reveal("/src/beta/deeper/deep.txt"));  // nothing changed
  CHECK(f.src.reads.size() == reads_after_first);
  CHECK(!f.side->selected_path().empty());
}

TEST(sidebar_reveal_rejects_a_path_outside_the_root) {
  Fixture f;
  f.side->set_root("/src");
  CHECK(!f.side->reveal("/etc/passwd"));
  CHECK(!f.side->selected_path().empty());  // selection unchanged (the root)
}

TEST(sidebar_reveal_of_the_root_selects_the_root) {
  Fixture f;
  f.side->set_root("/src");
  CHECK(!f.side->reveal("/src"));
  CHECK(!f.side->selected_path().empty());
}

// --- Hit-testing ------------------------------------------------------------

TEST(sidebar_row_at_maps_y_to_a_row) {
  Fixture f;
  f.side->set_root("/src");
  CHECK_EQ(f.side->row_at(10, 0), 0);
  CHECK_EQ(f.side->row_at(10, vc::Sidebar::kRowHeight - 1), 0);
  CHECK_EQ(f.side->row_at(10, vc::Sidebar::kRowHeight), 1);
  CHECK_EQ(f.side->row_at(10, 2 * vc::Sidebar::kRowHeight + 3), 2);
  CHECK_EQ(f.side->row_at(10, -1), -1);  // above the rows
  CHECK_EQ(f.side->row_at(10, 99999), -1);  // below them
}

TEST(sidebar_toggling_a_file_is_a_noop) {
  Fixture f;
  f.side->set_root("/src");
  const vc::SidebarNode* alpha = nullptr;
  for (const vc::Sidebar::Row& r : f.side->rows()) {
    if (r.node->name == "alpha.txt") alpha = r.node;
  }
  CHECK(alpha != nullptr);
  CHECK(!f.side->toggle(alpha));
  // Folders first: "beta" leads, then the files.
  CHECK(f.names() == std::vector<std::string>({"beta", "alpha.txt", "zeta.txt"}));
}

TEST(sidebar_a_symlink_is_a_leaf) {
  FakeDir src;
  src.add("/r", "link", false);
  src.dirs["/r"][0].is_symlink = true;
  vc::HeadlessTextBackend tb;
  vc::Sidebar side(src, tb);
  side.set_root("/r");
  CHECK(side.rows().size() == 1);
  const vc::SidebarNode* link = side.rows()[0].node;
  CHECK(link->is_symlink);
  CHECK(!side.toggle(link));  // not a directory: nothing to expand
}

// --- Drawing ----------------------------------------------------------------

TEST(sidebar_draws_a_row_per_visible_node_with_labels) {
  Fixture f;
  f.side->set_root("/src");
  vc::RecordingCanvas rc;
  f.side->draw(rc, vc::light_theme());
  std::vector<std::string> runs;
  for (const vc::DrawCmd& d : rc.cmds) {
    if (d.kind == vc::DrawCmd::Kind::DrawRun) runs.push_back(d.text);
  }
  // One label per row, plus a chevron for the one directory (drawn as a run).
  CHECK(runs.size() == 4);
  CHECK(std::find(runs.begin(), runs.end(), "alpha.txt") != runs.end());
  CHECK(std::find(runs.begin(), runs.end(), "beta") != runs.end());
  CHECK(std::find(runs.begin(), runs.end(), "zeta.txt") != runs.end());
}

TEST(sidebar_drawing_uses_the_theme_ground_and_a_hairline) {
  Fixture f;
  f.side->set_root("/src");
  vc::RecordingCanvas rc;
  const vc::Theme dark = vc::dark_theme();
  f.side->draw(rc, dark);
  // The ground is color.page, and there is a 1pt hairline at the right edge.
  bool ground = false, hairline = false;
  for (const vc::DrawCmd& d : rc.cmds) {
    if (d.kind != vc::DrawCmd::Kind::FillRect) continue;
    if (d.color.r == dark.page.r && d.color.g == dark.page.g &&
        d.color.b == dark.page.b && d.rect.w > f.side->width() - 1) {
      ground = true;
    }
    if (d.rect.w <= 1.0 && d.rect.x >= f.side->width() - 2) hairline = true;
  }
  CHECK(ground);
  CHECK(hairline);
}

TEST(sidebar_empty_directory_says_so_instead_of_drawing_a_blank_column) {
  FakeDir src;
  src.dirs["/empty"] = {};  // readable, no children
  vc::HeadlessTextBackend tb;
  vc::Sidebar side(src, tb);
  CHECK(side.set_root("/empty"));
  CHECK(side.rows().empty());
  vc::RecordingCanvas rc;
  side.draw(rc, vc::light_theme());
  bool has_note = false;
  for (const vc::DrawCmd& d : rc.cmds) {
    if (d.kind == vc::DrawCmd::Kind::DrawRun && d.text == "No files") {
      has_note = true;
    }
  }
  CHECK(has_note);
}

TEST(sidebar_unreadable_root_says_so) {
  FakeDir src;  // no directories at all: every read fails
  vc::HeadlessTextBackend tb;
  vc::Sidebar side(src, tb);
  CHECK(!side.set_root("/nope"));
  CHECK(side.rows().empty());
  vc::RecordingCanvas rc;
  side.draw(rc, vc::light_theme());
  bool has_note = false;
  for (const vc::DrawCmd& d : rc.cmds) {
    if (d.kind == vc::DrawCmd::Kind::DrawRun && d.text == "Unreadable") {
      has_note = true;
    }
  }
  CHECK(has_note);
}

TEST(sidebar_selected_row_is_highlighted) {
  Fixture f;
  f.side->set_root("/src");
  const vc::SidebarNode* beta = nullptr;
  for (const vc::Sidebar::Row& r : f.side->rows()) {
    if (r.node->name == "beta") beta = r.node;
  }
  f.side->set_selected(beta);
  vc::RecordingCanvas rc;
  const vc::Theme t = vc::light_theme();
  f.side->draw(rc, t);
  // The selection fill (color.secondary-hover) is drawn over that row's band.
  bool selected_band = false;
  for (const vc::DrawCmd& d : rc.cmds) {
    if (d.kind == vc::DrawCmd::Kind::FillRect && d.color.r == t.selection.r &&
        d.color.g == t.selection.g && d.color.b == t.selection.b &&
        d.rect.h == vc::Sidebar::kRowHeight) {
      selected_band = true;
    }
  }
  CHECK(selected_band);
}

TEST(sidebar_rows_are_bounded_by_the_expanded_set) {
  Fixture f;
  f.side->set_root("/src");
  // Expand everything: the count is then every node in the tree.
  // Expand to a fixed point: opening `beta` reveals `deeper`, which was not on
  // screen when the first pass collected the directories.
  bool changed = true;
  while (changed) {
    changed = false;
    std::vector<const vc::SidebarNode*> dirs;
    for (const vc::Sidebar::Row& r : f.side->rows()) {
      if (r.node->is_dir && !r.node->expanded) dirs.push_back(r.node);
    }
    for (const vc::SidebarNode* d : dirs) changed = f.side->toggle(d) || changed;
  }
  // Every node is visible: beta, deeper, deep.txt, inner.txt, alpha.txt, zeta.txt.
  CHECK(f.names().size() == 6);
}

// --- Scrolling --------------------------------------------------------------

TEST(sidebar_scrolls_only_when_the_content_overflows) {
  Fixture f;
  f.side->set_root("/src");
  f.side->set_height(1000);  // far taller than the three rows
  CHECK_EQ(f.side->max_scroll(), 0.0);
  f.side->scroll_by(500);
  CHECK_EQ(f.side->scroll_y(), 0.0);  // nothing to scroll to

  f.side->set_height(3 * vc::Sidebar::kRowHeight);  // exactly the content
  CHECK_EQ(f.side->max_scroll(), 0.0);

  f.side->set_height(1 * vc::Sidebar::kRowHeight);  // one row visible
  const double max = f.side->max_scroll();
  CHECK(max > 0);
  f.side->scroll_by(1000);
  CHECK_EQ(f.side->scroll_y(), max);  // clamped to the bottom
  f.side->scroll_by(-1000);
  CHECK_EQ(f.side->scroll_y(), 0.0);  // and to the top
}

TEST(sidebar_row_at_accounts_for_the_scroll) {
  Fixture f;
  f.side->set_root("/src");
  f.side->set_height(vc::Sidebar::kRowHeight);  // one row at a time
  f.side->scroll_to(vc::Sidebar::kRowHeight);   // scrolled down one row
  // The first visible row is now index 1.
  CHECK_EQ(f.side->row_at(10, 0), 1);
  CHECK_EQ(f.side->row_at(10, vc::Sidebar::kRowHeight - 1), 1);
}

TEST(sidebar_scrolling_draws_only_the_visible_rows) {
  // A folder with many entries: only the rows in the viewport are drawn, so the
  // draw cost stays flat as the folder grows.
  FakeDir src;
  for (int i = 0; i < 200; ++i) {
    src.add("/many", "file-" + std::to_string(i) + ".txt", false);
  }
  vc::HeadlessTextBackend tb;
  vc::Sidebar side(src, tb);
  side.set_root("/many");
  side.set_height(5 * vc::Sidebar::kRowHeight);

  vc::RecordingCanvas rc;
  side.draw(rc, vc::light_theme());
  int labels = 0;
  for (const vc::DrawCmd& d : rc.cmds) {
    if (d.kind == vc::DrawCmd::Kind::DrawRun && d.text.rfind("file-", 0) == 0) {
      ++labels;
    }
  }
  // Five rows fit (plus at most one partial), not two hundred.
  CHECK(labels >= 5);
  CHECK(labels <= 6);

  // Scrolling to the bottom shows the last rows instead. Names sort
  // lexicographically (file-1, file-10, file-100, ...), so the last row is
  // file-99.txt.
  side.scroll_to(side.max_scroll());
  vc::RecordingCanvas rc2;
  side.draw(rc2, vc::light_theme());
  bool saw_last = false;
  for (const vc::DrawCmd& d : rc2.cmds) {
    if (d.kind == vc::DrawCmd::Kind::DrawRun && d.text == "file-99.txt") {
      saw_last = true;
    }
  }
  CHECK(saw_last);
}

TEST(sidebar_a_new_root_resets_the_scroll) {
  Fixture f;
  f.side->set_root("/src");
  f.side->set_height(vc::Sidebar::kRowHeight);
  f.side->scroll_to(f.side->max_scroll());
  CHECK(f.side->scroll_y() > 0);
  f.side->set_root("/src");
  CHECK_EQ(f.side->scroll_y(), 0.0);
}

// --- Path boundaries and scrolling the selection into view -------------------

TEST(sidebar_reveal_rejects_a_sibling_with_the_root_as_a_string_prefix) {
  // "/foo" is not a path prefix of "/foobar": a naive string compare would
  // treat "bar" as a component of the root and reveal the wrong thing.
  FakeDir src;
  src.add("/foo", "a.txt", false);
  vc::HeadlessTextBackend tb;
  vc::Sidebar side(src, tb);
  side.set_root("/foo");
  CHECK(!side.reveal("/foobar/a.txt"));
  CHECK(side.rows().size() == 1);  // unchanged: just the root's own child
}

TEST(sidebar_reveal_walks_a_real_subdirectory) {
  // A nested path under the root is walked; the return is "did the tree change".
  FakeDir src;
  src.add("/foo", "bar", true);
  src.add("/foo/bar", "b.txt", false);
  vc::HeadlessTextBackend tb;
  vc::Sidebar side(src, tb);
  side.set_root("/foo");
  (void)side.reveal("/foo/bar/b.txt");
  CHECK(side.selected_path() == "/foo/bar/b.txt");
  // The intermediate directory is on screen and expanded.
  bool saw_bar = false, saw_file = false;
  for (const vc::Sidebar::Row& r : side.rows()) {
    if (r.node->name == "bar" && r.node->expanded) saw_bar = true;
    if (r.node->name == "b.txt") saw_file = true;
  }
  CHECK(saw_bar);
  CHECK(saw_file);
}

TEST(sidebar_reveal_scrolls_the_selected_row_into_view) {
  // A folder with more rows than fit: revealing a file far down must scroll it
  // into view, not select it off-screen.
  FakeDir src;
  for (int i = 0; i < 60; ++i) {
    src.add("/many", "file-" + std::to_string(i) + ".txt", false);
  }
  vc::HeadlessTextBackend tb;
  vc::Sidebar side(src, tb);
  side.set_root("/many");
  side.set_height(5 * vc::Sidebar::kRowHeight);
  CHECK_EQ(side.scroll_y(), 0.0);

  // The lexicographically last entry of the 60 (file-9.txt, after file-59).
  const std::string target = "/many/file-9.txt";
  (void)side.reveal(target);  // the return is "changed", not "found"
  CHECK(side.selected_path() == target);
  // Its row is now fully inside the viewport.
  const std::vector<vc::Sidebar::Row>& rows = side.rows();
  int index = -1;
  for (size_t i = 0; i < rows.size(); ++i) {
    if (rows[i].node->path == target) index = static_cast<int>(i);
  }
  CHECK(index >= 0);
  const double top = index * vc::Sidebar::kRowHeight;
  CHECK(top >= side.scroll_y());
  CHECK(top + vc::Sidebar::kRowHeight <= side.scroll_y() + side.height() + 0.01);
}

TEST(sidebar_revealing_the_first_row_does_not_scroll) {
  FakeDir src;
  src.add("/r", "a.txt", false);
  src.add("/r", "b.txt", false);
  vc::HeadlessTextBackend tb;
  vc::Sidebar side(src, tb);
  side.set_root("/r");
  side.set_height(10 * vc::Sidebar::kRowHeight);
  (void)side.reveal("/r/a.txt");
  CHECK(side.selected_path() == "/r/a.txt");
  CHECK_EQ(side.scroll_y(), 0.0);
}

// --- Refresh after the directory changed on disk ----------------------------

TEST(sidebar_refresh_picks_up_a_new_file_and_stays_expanded) {
  FakeDir src;
  src.add("/r", "a.txt", false);
  src.add("/r", "sub", true);
  src.add("/r/sub", "old.txt", false);
  vc::HeadlessTextBackend tb;
  vc::Sidebar side(src, tb);
  side.set_root("/r");
  const vc::SidebarNode* sub = nullptr;
  for (const vc::Sidebar::Row& r : side.rows()) {
    if (r.node->name == "sub") sub = r.node;
  }
  side.toggle(sub);  // expand
  CHECK(side.rows().size() == 3);

  // Someone creates a file while the sidebar is open.
  src.add("/r/sub", "new.txt", false);
  (void)side.refresh("/r/sub");

  bool expanded = false, saw_new = false;
  for (const vc::Sidebar::Row& r : side.rows()) {
    if (r.node->name == "sub") expanded = r.node->expanded;
    if (r.node->name == "new.txt") saw_new = true;
  }
  CHECK(expanded);  // a refresh is not a collapse
  CHECK(saw_new);
}

TEST(sidebar_refresh_drops_a_deleted_file) {
  FakeDir src;
  src.add("/r", "a.txt", false);
  src.add("/r", "gone.txt", false);
  vc::HeadlessTextBackend tb;
  vc::Sidebar side(src, tb);
  side.set_root("/r");
  CHECK(side.rows().size() == 2);

  src.dirs["/r"].erase(
      std::remove_if(src.dirs["/r"].begin(), src.dirs["/r"].end(),
                     [](const vc::DirEntry& e) { return e.name == "gone.txt"; }),
      src.dirs["/r"].end());
  (void)side.refresh("/r");
  bool still = false;
  for (const vc::Sidebar::Row& r : side.rows()) {
    if (r.node->name == "gone.txt") still = true;
  }
  CHECK(!still);
}

TEST(sidebar_refresh_keeps_the_selection_when_it_survives) {
  FakeDir src;
  src.add("/r", "a.txt", false);
  src.add("/r", "b.txt", false);
  vc::HeadlessTextBackend tb;
  vc::Sidebar side(src, tb);
  side.set_root("/r");
  const std::string target = "/r/b.txt";
  (void)side.reveal(target);
  CHECK(side.selected_path() == target);

  src.add("/r", "c.txt", false);
  (void)side.refresh("/r");
  CHECK(side.selected_path() == target);
}

TEST(sidebar_refresh_falls_back_when_the_selected_file_vanished) {
  FakeDir src;
  src.add("/r", "a.txt", false);
  vc::HeadlessTextBackend tb;
  vc::Sidebar side(src, tb);
  side.set_root("/r");
  const vc::SidebarNode* a = side.rows()[0].node;
  side.set_selected(a);

  src.dirs["/r"].clear();
  (void)side.refresh("/r");
  // The file is gone: the selection falls back to the directory itself.
  CHECK(side.selected() != a);
  CHECK(side.selected_path() == "/r");
}

TEST(sidebar_refresh_of_the_root_re_reads_it) {
  // The root is not one of its own rows, so a refresh has to reach it directly.
  FakeDir src;
  src.add("/r", "a.txt", false);
  vc::HeadlessTextBackend tb;
  vc::Sidebar side(src, tb);
  side.set_root("/r");
  CHECK(side.rows().size() == 1);

  src.add("/r", "b.txt", false);
  (void)side.refresh("/r");
  bool saw_b = false;
  for (const vc::Sidebar::Row& r : side.rows()) {
    if (r.node->name == "b.txt") saw_b = true;
  }
  CHECK(saw_b);
}

TEST(sidebar_refresh_of_an_unloaded_path_is_a_noop) {
  Fixture f;
  f.side->set_root("/src");
  const size_t before = f.src.reads.size();
  CHECK(!f.side->refresh("/src/beta"));  // beta is collapsed: not loaded
  CHECK(f.src.reads.size() == before);
}

// --- Keyboard navigation ----------------------------------------------------

TEST(sidebar_arrow_keys_walk_the_visible_rows) {
  Fixture f;
  f.side->set_root("/src");
  // Rows: beta, alpha.txt, zeta.txt (folders first).
  const std::string first = f.side->selected_path();
  CHECK(!first.empty());

  CHECK(f.side->move_down());
  CHECK(f.side->selected_path() == "/src/alpha.txt");
  CHECK(f.side->move_down());
  CHECK(f.side->selected_path() == "/src/zeta.txt");
  CHECK(!f.side->move_down());  // already at the last row

  CHECK(f.side->move_up());
  CHECK(f.side->selected_path() == "/src/alpha.txt");
  CHECK(f.side->move_up());
  CHECK(f.side->selected_path() == "/src/beta");
  CHECK(!f.side->move_up());  // already at the first row
}

TEST(sidebar_right_opens_a_directory_then_steps_into_it) {
  Fixture f;
  f.side->set_root("/src");
  // set_root selects the first row, which is beta (folders first).
  const vc::SidebarNode* beta = f.side->selected();
  CHECK(beta != nullptr && beta->name == "beta");
  CHECK(f.side->move_right());  // expands
  CHECK(f.side->selected_path() == "/src/beta");  // selection stays on it
  // Open now: Right moves to the first child (deeper, folders first).
  CHECK(f.side->move_right());
  CHECK(f.side->selected_path() == "/src/beta/deeper");
}

TEST(sidebar_left_closes_then_steps_out) {
  Fixture f;
  f.side->set_root("/src");
  f.side->reveal("/src/beta/deeper/deep.txt");
  CHECK(f.side->selected_path() == "/src/beta/deeper/deep.txt");

  // Left on an open directory closes it and keeps the selection there; Left on
  // a closed one (or a file) steps out to its parent. So the walk out from a
  // file two levels down is: close deeper, step to beta, close beta, step to the
  // root. Assert the invariant (each press is legal and ends at the root) rather
  // than counting presses, which is brittle to read.
  int presses = 0;
  while (f.side->selected_path() != "/src") {
    CHECK(presses < 8);  // it must terminate
    if (presses >= 8) break;
    CHECK(f.side->move_left());
    ++presses;
  }
  CHECK(f.side->selected_path() == "/src");
  CHECK(presses > 0);
  CHECK(!f.side->move_left());  // the root has no parent
  CHECK(f.side->selected_path() == "/src");
}
TEST(sidebar_left_from_a_file_one_level_down) {
  Fixture f;
  f.side->set_root("/src");
  f.side->reveal("/src/beta/inner.txt");
  CHECK(f.side->selected_path() == "/src/beta/inner.txt");
  // One level down: select the parent, close it, then step out to the root.
  CHECK(f.side->move_left());  // the file's parent
  CHECK(f.side->selected_path() == "/src/beta");
  CHECK(f.side->move_left());  // close beta
  CHECK(f.side->selected_path() == "/src/beta");
  CHECK(f.side->move_left());  // step out to the root
  CHECK(f.side->selected_path() == "/src");
  CHECK(!f.side->move_left());  // the root has no parent
}


TEST(sidebar_left_on_the_root_is_a_noop) {
  Fixture f;
  f.side->set_root("/src");
  f.side->set_selected_path("/src");
  CHECK(!f.side->move_left());  // the root has no parent
  CHECK(f.side->selected_path() == "/src");
}

TEST(sidebar_activate_toggles_a_directory_and_returns_a_file_path) {
  Fixture f;
  f.side->set_root("/src");

  f.side->set_selected_path("/src/beta");
  const std::string* path = nullptr;
  CHECK(f.side->activate(&path));
  CHECK(path == nullptr);  // a directory: no file to open
  CHECK(f.side->selected_path() == "/src/beta");

  f.side->set_selected_path("/src/alpha.txt");
  path = nullptr;
  CHECK(f.side->activate(&path));
  CHECK(path != nullptr && *path == "/src/alpha.txt");
}

TEST(sidebar_keyboard_navigation_scrolls_the_selection_into_view) {
  FakeDir src;
  for (int i = 0; i < 40; ++i) {
    src.add("/many", "f-" + std::to_string(i) + ".txt", false);
  }
  vc::HeadlessTextBackend tb;
  vc::Sidebar side(src, tb);
  side.set_root("/many");
  side.set_height(3 * vc::Sidebar::kRowHeight);
  // Walk down past the bottom; the selection must stay visible.
  for (int i = 0; i < 10; ++i) side.move_down();
  const vc::SidebarNode* sel = side.selected();
  CHECK(sel != nullptr);
  int index = -1;
  const std::vector<vc::Sidebar::Row>& rows = side.rows();
  for (size_t i = 0; i < rows.size(); ++i) {
    if (rows[i].node == sel) index = static_cast<int>(i);
  }
  CHECK(index >= 0);
  const double top = index * vc::Sidebar::kRowHeight;
  CHECK(top >= side.scroll_y());
  CHECK(top + vc::Sidebar::kRowHeight <= side.scroll_y() + side.height() + 0.01);
}

// --- Width (drag-resize) ----------------------------------------------------

TEST(sidebar_width_clamps_to_the_usable_range) {
  Fixture f;
  f.side->set_width(1000.0);
  CHECK_EQ(f.side->width(), vc::Sidebar::kMaxWidth);
  f.side->set_width(10.0);
  CHECK_EQ(f.side->width(), vc::Sidebar::kMinWidth);
  // Inside the range it is kept exactly.
  f.side->set_width(333.0);
  CHECK_EQ(f.side->width(), 333.0);
}

TEST(sidebar_width_is_reflected_in_the_drawing) {
  // The ground and the hairline follow the width: a resized column must not
  // keep drawing at the old width.
  Fixture f;
  f.side->set_root("/src");
  f.side->set_height(200.0);
  f.side->set_width(300.0);
  vc::RecordingCanvas rc;
  f.side->draw(rc, vc::light_theme());
  bool ground = false, hairline = false;
  for (const vc::DrawCmd& d : rc.cmds) {
    if (d.kind != vc::DrawCmd::Kind::FillRect) continue;
    if (std::abs(d.rect.w - 300.0) < 0.01 && d.rect.x == 0.0) ground = true;
    if (d.rect.w <= 1.0 && d.rect.x >= 298.0) hairline = true;
  }
  CHECK(ground);
  CHECK(hairline);
}

TEST(sidebar_width_does_not_move_the_selection_or_the_scroll) {
  // Resizing is a layout change: the tree state has to survive it.
  Fixture f;
  f.side->set_root("/src");
  f.side->reveal("/src/beta/deeper/deep.txt");
  const std::string sel = f.side->selected_path();
  const double scroll = f.side->scroll_y();
  f.side->set_width(400.0);
  CHECK(f.side->selected_path() == sel);
  CHECK_EQ(f.side->scroll_y(), scroll);
}

}  // namespace