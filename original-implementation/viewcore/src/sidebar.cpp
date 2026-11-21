// viewcore/src/sidebar.cpp — the file sidebar's tree, layout and drawing (§9).
#include "sidebar.hpp"

#include <algorithm>
#include <cctype>
#include <utility>

namespace vc {

namespace {

// Case-insensitive comparison, with a stable tie-break so two names that differ
// only by case still sort deterministically.
bool name_less(const std::string& a, const std::string& b) {
  const size_t n = std::min(a.size(), b.size());
  for (size_t i = 0; i < n; ++i) {
    const unsigned char ca = static_cast<unsigned char>(a[i]);
    const unsigned char cb = static_cast<unsigned char>(b[i]);
    const int la = std::tolower(ca);
    const int lb = std::tolower(cb);
    if (la != lb) return la < lb;
  }
  if (a.size() != b.size()) return a.size() < b.size();
  return a < b;
}

// The parent of `path` (everything before the last '/'), or "" for a root.
std::string parent_of(const std::string& path) {
  const size_t slash = path.find_last_of('/');
  if (slash == std::string::npos) return std::string();
  if (slash == 0) return std::string("/");
  return path.substr(0, slash);
}

}  // namespace

Sidebar::Sidebar(DirSource& src, TextBackend& tb) : src_(src), tb_(tb) {
  font_.size = 13.0;
  font_.ui = true;  // chrome, not document text (§8)
}

const SidebarNode* Sidebar::selected() const {
  if (selected_path_.empty()) return nullptr;
  if (root_node_ != nullptr && root_node_->path == selected_path_) {
    return root_node_.get();
  }
  const std::vector<Row>& r = rows();
  for (const Row& row : r) {
    if (row.node != nullptr && row.node->path == selected_path_) {
      return row.node;
    }
  }
  return nullptr;
}

bool Sidebar::set_root(std::string_view root_path) {
  root_ = std::string(root_path);
  root_node_ = std::make_unique<SidebarNode>();
  root_node_->name = root_.empty() ? std::string("/") : root_;
  root_node_->path = root_;
  root_node_->is_dir = true;
  root_node_->expanded = true;
  root_loaded_ = load(root_node_.get());
  // A new root starts in tree mode (the whole neighbourhood, expanded), not
  // navigated into the root itself.
  view_.clear();
  crumbs_valid_ = false;
  rows_valid_ = false;
  scroll_to(0);
  // The root itself is not one of its own rows, so a selection on it would be
  // invisible and the arrow keys would have nothing to move from. Select the
  // first row instead (nothing, for an empty or unreadable directory).
  selected_path_.clear();
  const std::vector<Row>& r = rows();
  if (!r.empty() && r[0].node != nullptr) selected_path_ = r[0].node->path;
  return root_loaded_;
}

void Sidebar::set_font(const FontSpec& f) { font_ = f; }

bool Sidebar::load(SidebarNode* dir) const {
  if (dir == nullptr || dir->loaded) return dir->read_ok;
  dir->loaded = true;  // set first: a failed read is not retried every frame
  if (!dir->is_dir) return false;
  std::vector<DirEntry> entries;
  if (!src_.read_dir(dir->path, &entries)) return false;
  dir->read_ok = true;
  dir->children.clear();
  for (DirEntry& e : entries) {
    auto child = std::make_unique<SidebarNode>();
    child->name = std::move(e.name);
    child->path = std::move(e.path);
    child->is_dir = e.is_dir;
    child->is_symlink = e.is_symlink;
    dir->children.push_back(std::move(child));
  }
  sort_children(dir);
  return true;
}

void Sidebar::sort_children(SidebarNode* dir) const {
  if (dir == nullptr) return;
  std::stable_sort(dir->children.begin(), dir->children.end(),
                   [](const std::unique_ptr<SidebarNode>& a,
                      const std::unique_ptr<SidebarNode>& b) {
                     // Folders first, then files; each group by name.
                     if (a->is_dir != b->is_dir) return a->is_dir;
                     return name_less(a->name, b->name);
                   });
}

const std::vector<Sidebar::Row>& Sidebar::rows() const {
  if (rows_valid_) return rows_;
  rows_.clear();
  if (root_node_ == nullptr) {
    rows_valid_ = true;
    return rows_;
  }

  // Navigating: show just the view folder's children, one level, no expansion.
  // This is the "go into that folder" mode; the tree behind it is untouched.
  if (!view_.empty()) {
    // Resolve the view through the tree rather than re-walking the path here:
    // the walk has to strip the root prefix and read on the way, and having two
    // copies of that is how they drift.
    SidebarNode* view_dir = resolve(view_);
    if (view_dir == nullptr || view_dir == root_node_.get()) {
      // The view is gone (a folder removed under us, or the root changed):
      // fall back to the tree rather than drawing an empty column.
      view_.clear();
      rows_valid_ = false;
      crumbs_valid_ = false;
      return rows();
    }
    // Reading the view's children on demand is what makes navigating instant
    // and lazy: nothing is walked until it is shown. `expanded` is left alone —
    // the view is a lens over the folder, not an expansion of the tree.
    if (!view_dir->loaded) load(view_dir);
    for (const auto& c : view_dir->children) {
      rows_.push_back(Row{c.get(), 0});
    }
    rows_valid_ = true;
    return rows_;
  }

  if (root_node_->children.empty()) {
    rows_valid_ = true;
    return rows_;
  }
  // Depth-first over expanded nodes only: a collapsed subtree is never walked,
  // so a huge directory costs only its own rows.
  struct Pending {
    const SidebarNode* node;
    int depth;
  };
  // Depth-first with an explicit stack. The stack is LIFO, so children are
  // pushed in reverse and pop in order; a collapsed node's children are never
  // pushed, which is what keeps a collapsed subtree unread.
  std::vector<Pending> stack;
  for (auto it = root_node_->children.rbegin();
       it != root_node_->children.rend(); ++it) {
    stack.push_back({it->get(), 0});
  }
  while (!stack.empty()) {
    const Pending p = stack.back();
    stack.pop_back();
    rows_.push_back(Row{p.node, p.depth});
    if (!p.node->is_dir || !p.node->expanded) continue;
    for (auto it = p.node->children.rbegin(); it != p.node->children.rend();
         ++it) {
      stack.push_back({it->get(), p.depth + 1});
    }
  }
  rows_valid_ = true;
  return rows_;
}

bool Sidebar::navigate(std::string_view path) {
  if (root_node_ == nullptr) return false;

  // The root, or an empty path, means "the tree": the root's neighbourhood with
  // its expansions, which is what the user came back to.
  const bool to_root = path.empty() || path == root_;
  if (!to_root) {
    // Resolve the folder from the root, reading as needed. It does not have to
    // be expanded or on screen: navigating is the gesture for going into a
    // folder, and the breadcrumb offers folders the user has navigated away
    // from.
    SidebarNode* dir = resolve(std::string(path));  // resolve() is const
    if (dir == nullptr || dir == root_node_.get()) return false;
    if (!dir->is_dir || dir->is_symlink) return false;
    // Read it *before* setting view_: rows() resolves the view through the
    // tree, and an unread directory makes that resolution fail — which would
    // silently drop the view we just set.
    if (!dir->loaded) load(dir);
    view_ = std::string(path);
  } else {
    if (view_.empty()) return false;  // already the tree
    view_.clear();
  }
  // Navigating is a change of view, not of what the user has selected: keep the
  // same selection path. The selected row may not be in the new view (it is
  // usually the child the user just left), and that is fine — `selected_path_`
  // is held by path and `selected()` resolves it against whatever is on screen,
  // so going back out finds it again. Only when the path is genuinely gone from
  // the tree does the selection fall back to the first entry.
  const std::string wanted = selected_path_;
  rows_valid_ = false;
  crumbs_valid_ = false;
  scroll_to(0);
  selected_path_ = wanted;
  if (selected() == nullptr) {
    // Not visible from here. Keep it only if it is still in the tree at all
    // (an ancestor of the new view, say); otherwise start at the top.
    SidebarNode* still = resolve(wanted);
    if (still == nullptr) {
      selected_path_.clear();
      const std::vector<Row>& r = rows();
      if (!r.empty() && r[0].node != nullptr) selected_path_ = r[0].node->path;
    }
  }
  scroll_selection_into_view();
  return true;
}

bool Sidebar::navigate_up() {
  if (view_.empty()) return false;  // already the tree
  const std::string parent = parent_of(view_);
  if (parent.empty() || parent == view_) return navigate_to_root();
  // The parent may be the root, which navigate() handles.
  return navigate(parent);
}

bool Sidebar::navigate_to_root() { return navigate(std::string()); }

std::vector<Sidebar::Crumb> Sidebar::breadcrumb() const {
  std::vector<Crumb> out;
  if (root_.empty()) return out;
  // The root's own label: its last component, or "/" for the filesystem root.
  std::string root_label = root_;
  const size_t slash = root_.find_last_of('/');
  if (slash != std::string::npos) {
    root_label = root_.substr(slash + 1);
  }
  if (root_label.empty()) root_label = "/";
  out.push_back(Crumb{root_label, root_});
  if (view_.empty() || view_ == root_) return out;

  // Each component between the root and the view, with the path to reach it.
  std::string prefix = root_;
  if (!prefix.empty() && prefix.back() != '/') prefix += '/';
  std::string_view rest(view_);
  if (view_.size() > root_.size() &&
      view_.compare(0, root_.size(), root_) == 0) {
    rest = std::string_view(view_).substr(root_.size());
  }
  while (!rest.empty() && rest.front() == '/') rest.remove_prefix(1);
  size_t pos = 0;
  while (pos < rest.size()) {
    const size_t slash = rest.find('/', pos);
    const std::string part =
        std::string(rest.substr(pos, slash == std::string_view::npos
                                         ? std::string_view::npos
                                         : slash - pos));
    if (part.empty()) break;
    prefix += part;
    out.push_back(Crumb{part, prefix});
    if (slash == std::string_view::npos) break;
    prefix += '/';
    pos = slash + 1;
  }
  return out;
}

bool Sidebar::refresh(const std::string& path) {
  // The root is not one of its own rows, so `find` cannot reach it.
  SidebarNode* dir = (root_node_ != nullptr && root_node_->path == path)
                         ? root_node_.get()
                         : find(path);
  if (dir == nullptr || !dir->is_dir) return false;
  if (!dir->loaded) return false;  // collapsed: nothing to refresh yet
  // Re-reading destroys the child nodes, so keep the selection by path (it is
  // stored that way) and re-resolve it against the new rows below.
  const std::string wanted = selected_path_;
  // Re-read, keeping the node and its expanded state: a refresh is not a
  // collapse. Children that vanished disappear; new ones appear.
  dir->loaded = false;
  dir->children.clear();
  load(dir);
  // Invalidate before looking at the rows, or the stale cache is returned.
  rows_valid_ = false;
  (void)rows();  // rebuild now, so the selection check sees the new rows
  // The file is gone under us only if no row carries its path; then the
  // selection falls back to the directory itself.
  if (!wanted.empty() && selected() == nullptr) selected_path_ = dir->path;
  scroll_selection_into_view();
  return true;
}

bool Sidebar::toggle(const SidebarNode* n) {
  if (n == nullptr || !n->is_dir || n->is_symlink) return false;
  SidebarNode* mut = find(n->path);
  if (mut == nullptr || !mut->is_dir) return false;
  mut->expanded = !mut->expanded;
  if (mut->expanded) load(mut);
  rows_valid_ = false;
  selected_path_ = mut->path;
  return true;
}

bool Sidebar::is_under_root(std::string_view path) const {
  if (root_.empty()) return false;
  if (path == root_) return true;
  // The root must be a *path* prefix: "/foo" is not a prefix of "/foobar".
  return path.size() > root_.size() &&
         path.compare(0, root_.size(), root_) == 0 &&
         path[root_.size()] == '/';
}

bool Sidebar::reveal(std::string_view path) {
  if (root_node_ == nullptr) return false;
  // Revealing is a tree operation: if the user has navigated into a folder,
  // come back to the tree first, or the ancestors are not on screen to expand.
  if (!view_.empty()) {
    view_.clear();
    rows_valid_ = false;
    crumbs_valid_ = false;
  }
  const std::string target(path);
  if (target == root_) {
    selected_path_ = root_node_->path;
    return false;
  }
  // The components below the root, outermost first: <a>/<b>/file.txt.
  // Walking in that order is what lets each step find its children — the parent
  // must be loaded before the child can be looked up.
  std::vector<std::string> parts;
  if (is_under_root(target)) {
    std::string rel = target.substr(root_.size());
    while (!rel.empty() && rel.front() == '/') rel.erase(rel.begin());
    size_t pos = 0;
    while (pos <= rel.size()) {
      const size_t slash = rel.find('/', pos);
      const std::string part = rel.substr(
          pos, slash == std::string::npos ? std::string::npos : slash - pos);
      if (!part.empty()) parts.push_back(part);
      if (slash == std::string::npos) break;
      pos = slash + 1;
    }
  }

  SidebarNode* cur = root_node_.get();
  bool changed = false;
  for (size_t i = 0; i < parts.size(); ++i) {
    const bool last = i + 1 == parts.size();
    SidebarNode* next = nullptr;
    for (const auto& c : cur->children) {
      if (c->name == parts[i]) {
        next = c.get();
        break;
      }
    }
    if (next == nullptr) return false;  // not under the root
    if (!last) {
      // Every ancestor must be expanded (and loaded) for the next step to find
      // its children.
      if (!next->expanded) {
        next->expanded = true;
        changed = true;
      }
      if (!next->loaded) {
        load(next);
        changed = true;
      }
    }
    cur = next;
  }
  selected_path_ = cur->path;
  rows_valid_ = false;
  scroll_selection_into_view();
  return changed;
}

// The on-screen node for `path`, or nullptr.
// The node for `path`, resolving it from the root and loading directories as it
// goes. Returns nullptr unless `path` is the root or a real directory under it.
// Reading on the way is what lets a breadcrumb jump to a folder the user has
// navigated away from; it costs one readdir per level, and only for the path
// asked for.
SidebarNode* Sidebar::resolve(const std::string& path) const {
  if (root_node_ == nullptr || root_.empty()) return nullptr;
  if (path == root_) return root_node_.get();
  if (!is_under_root(path)) return nullptr;

  SidebarNode* cur = root_node_.get();
  std::string_view rest(path);
  rest = rest.substr(root_.size());  // is_under_root guarantees the prefix
  for (;;) {
    while (!rest.empty() && rest.front() == '/') rest.remove_prefix(1);
    if (rest.empty()) break;
    const size_t slash = rest.find('/');
    const std::string part = std::string(
        rest.substr(0, slash == std::string_view::npos ? rest.size() : slash));
    rest = slash == std::string_view::npos ? std::string_view()
                                           : rest.substr(slash + 1);
    if (!cur->loaded) load(cur);
    SidebarNode* next = nullptr;
    for (const auto& c : cur->children) {
      if (c->name == part) {
        next = c.get();
        break;
      }
    }
    if (next == nullptr) return nullptr;  // gone, or never existed
    cur = next;
  }
  return cur;
}

// The on-screen node for `path`, or nullptr.
SidebarNode* Sidebar::find(std::string_view path) {
  // Only the nodes that are actually on screen: a collapsed directory's
  // children have not been read, and reaching them would defeat laziness.
  const std::vector<Row>& r = rows();
  for (const Row& row : r) {
    if (row.node != nullptr && row.node->path == path) {
      return const_cast<SidebarNode*>(row.node);
    }
  }
  return nullptr;
}

void Sidebar::scroll_selection_into_view() {
  const SidebarNode* sel = selected();
  if (sel == nullptr) return;
  const std::vector<Row>& r = rows();
  int index = -1;
  for (size_t i = 0; i < r.size(); ++i) {
    if (r[i].node == sel) {
      index = static_cast<int>(i);
      break;
    }
  }
  if (index < 0) return;
  const double top = static_cast<double>(index) * kRowHeight;
  const double bottom = top + kRowHeight;
  // Scroll the minimum that makes the row fully visible.
  if (top < scroll_y_) {
    scroll_to(top);
  } else if (bottom > scroll_y_ + height_) {
    scroll_to(bottom - height_);
  }
}

bool Sidebar::move_down() {
  const std::vector<Row>& r = rows();
  if (r.empty()) return false;
  int index = -1;
  const SidebarNode* sel = selected();
  for (size_t i = 0; i < r.size(); ++i) {
    if (r[i].node == sel) index = static_cast<int>(i);
  }
  if (index + 1 >= static_cast<int>(r.size())) return false;  // already last
  selected_path_ = r[static_cast<size_t>(index + 1)].node->path;
  scroll_selection_into_view();
  return true;
}

bool Sidebar::move_up() {
  const std::vector<Row>& r = rows();
  if (r.empty()) return false;
  int index = -1;
  const SidebarNode* sel = selected();
  for (size_t i = 0; i < r.size(); ++i) {
    if (r[i].node == sel) index = static_cast<int>(i);
  }
  if (index <= 0) return false;  // nothing selected, or already first
  selected_path_ = r[static_cast<size_t>(index - 1)].node->path;
  scroll_selection_into_view();
  return true;
}

bool Sidebar::move_right() {
  const SidebarNode* sel = selected();
  if (sel == nullptr || !sel->is_dir || sel->is_symlink) return false;
  // In tree mode a closed folder opens, and an open one is entered — the same
  // two steps the affordance gives the mouse. Navigated, Right always enters.
  if (view_.empty()) {
    SidebarNode* mut = find(sel->path);
    if (mut == nullptr) return false;
    if (!mut->expanded) return toggle(mut);
  }
  return navigate(sel->path);
}

bool Sidebar::move_left() {
  const SidebarNode* sel = selected();
  if (sel == nullptr) return false;
  // Navigated: Left always goes back up, whatever is selected.
  if (!view_.empty()) return navigate_up();
  SidebarNode* mut = find(sel->path);
  if (mut == nullptr) return false;
  if (mut->is_dir && mut->expanded) {
    // Open: close it, keeping the selection on the directory.
    return toggle(mut);
  }
  // Closed (or a file): move to the parent directory.
  if (mut == root_node_.get()) return false;  // the root has no parent here
  const std::string parent = parent_of(mut->path);
  if (parent.empty()) return false;
  const SidebarNode* p = (root_node_ != nullptr && root_node_->path == parent)
                             ? root_node_.get()
                             : find(parent);
  if (p == nullptr) return false;
  selected_path_ = p->path;
  scroll_selection_into_view();
  return true;
}

bool Sidebar::activate(const std::string** out_path) {
  if (out_path != nullptr) *out_path = nullptr;
  const SidebarNode* sel = selected();
  if (sel == nullptr) return false;
  if (sel->is_dir && !sel->is_symlink) {
    SidebarNode* mut = find(sel->path);
    if (mut == nullptr) return false;
    return toggle(mut);
  }
  if (out_path != nullptr) *out_path = &sel->path;
  return true;
}

double Sidebar::max_scroll() const {
  // The header is fixed, so only the rows below it scroll.
  const double over = content_height() - (height_ - kHeaderHeight);
  return over > 0 ? over : 0.0;
}

void Sidebar::scroll_to(double y) {
  if (y < 0) y = 0;
  const double max = max_scroll();
  if (y > max) y = max;
  scroll_y_ = y;
}

void Sidebar::scroll_by(double dy) { scroll_to(scroll_y_ + dy); }

int Sidebar::row_at(double x, double y) const {
  (void)x;
  // The header strip is not a row.
  if (y < kHeaderHeight) return -1;
  const int idx =
      static_cast<int>((y - kHeaderHeight + scroll_y_) / kRowHeight);
  const std::vector<Row>& r = rows();
  if (idx < 0 || static_cast<size_t>(idx) >= r.size()) return -1;
  return idx;
}

void Sidebar::enter_affordance(const Row& row, double* x0, double* x1) const {
  if (x0 != nullptr) *x0 = 0;
  if (x1 != nullptr) *x1 = 0;
  if (row.node == nullptr || !row.node->is_dir || row.node->is_symlink) return;
  // The affordance sits at the right edge, clear of the name and the scrollbar
  // overlay. It is deliberately wide enough to hit with a mouse.
  const double w = 22.0;
  if (x0 != nullptr) *x0 = width_ - kPadRight - w;
  if (x1 != nullptr) *x1 = width_ - kPadRight;
}

int Sidebar::crumb_at(double x, double y) const {
  if (y < 0 || y > kHeaderHeight) return -1;
  if (crumb_x_.size() != crumb_index_.size()) return -1;  // draw() has not run
  for (size_t i = 0; i < crumb_x_.size(); ++i) {
    if (x >= crumb_x_[i].first && x < crumb_x_[i].second) {
      return static_cast<int>(crumb_index_[i]);
    }
  }
  return -1;
}

void Sidebar::set_width(double w) {
  // Clamp rather than clamp-and-ignore: the caller (a resize drag) wants the
  // width it can actually have, so it reads it back.
  if (w < kMinWidth) w = kMinWidth;
  if (w > kMaxWidth) w = kMaxWidth;
  width_ = w;
}

void Sidebar::draw_header(Canvas& c, const Theme& t) const {
  const double w = width_;
  // The strip's own ground and its bottom hairline, so the rows read as
  // scrolling under a fixed bar.
  c.fill_rect(Rect{0, 0, w, kHeaderHeight}, t.page);
  c.fill_rect(Rect{0, kHeaderHeight - 1, w, 1}, t.gutter_border);

  const std::vector<Crumb> crumbs = breadcrumb();
  crumbs_ = crumbs;
  crumb_x_.clear();
  crumb_index_.clear();
  crumb_x_.reserve(crumbs.size());

  // Right-to-left layout: the current folder sits at the right edge (next to
  // the scrollbar it will sit under), and ancestors run leftwards, so a deep
  // path loses its *oldest* crumbs first rather than the one the user is in.
  // Each crumb is measured, then placed; the whole run is clipped to the
  // strip, and any crumb that does not fit is simply not drawn (it is still in
  // crumbs_, so hit-testing stays consistent with what is on screen).
  double x = w - kPadRight;
  const double cy = (kHeaderHeight - 14.0) * 0.5;
  for (size_t i = crumbs.size(); i-- > 0;) {
    const Run label = tb_.shape(font_, crumbs[i].label);
    const bool last = i + 1 == crumbs.size();
    // Separator chevron between crumbs (drawn to the left of this one).
    const double sep_w = last ? 0.0 : 10.0;
    const double x1 = x;
    const double x0 = x1 - sep_w - label.width;
    if (x0 < kPadLeft) break;  // no room: stop, the rest are off-screen left
    crumb_x_.push_back({x0, x1});
    crumb_index_.push_back(i);
    // The crumb that is the current view is emphasised; the rest are the muted
    // ink, so the eye lands on where it is.
    c.set_text_color(last ? t.text : t.gutter_text);
    c.draw_run(label, x0 + sep_w, cy + label.ascent);
    if (!last) {
      const Run chev = tb_.shape(font_, "\\u203a");
      c.set_text_color(t.gutter_text);
      c.draw_run(chev, x0, cy + chev.ascent);
    }
    x = x0;
  }
  // Built right-to-left, so each entry records which crumb it belongs to; the
  // hit test reads crumb_index_ rather than assuming the order.
  crumbs_valid_ = true;
}

void Sidebar::draw(Canvas& c, const Theme& t) const {
  const double w = width_;
  const double h = height_;
  c.fill_rect(Rect{0, 0, w, h}, t.page);  // the sidebar's ground
  c.fill_rect(Rect{w - 1, 0, 1, h}, t.gutter_border);  // the hairline

  // The breadcrumb strip, pinned above the scrolling rows. It is chrome: the
  // current location and every ancestor, each a button back to that folder.
  draw_header(c, t);

  const double row_h = kRowHeight;
  const std::vector<Row>& vis = rows();
  if (vis.empty()) {
    // Nothing to show: a directory that could not be read, or an empty one.
    // Say so rather than leaving a blank column (the row set is empty, so
    // there is nothing to select and nothing to open). The header still shows
    // where the user is, so they can navigate back out.
    const Run note = tb_.shape(font_, root_loaded_ ? "No files" : "Unreadable");
    c.set_text_color(t.gutter_text);
    c.draw_run(note, kPadLeft,
               kHeaderHeight +
                   (row_h - note.ascent - note.descent) * 0.5 + note.ascent);
    return;
  }
  // Rows are drawn from the top of the content, panned by the scroll offset.
  // Only the rows that can touch the viewport are considered, so a folder with
  // thousands of entries still costs only what is visible.
  const int first = static_cast<int>(scroll_y_ / row_h);
  const int last = static_cast<int>((scroll_y_ + (h - kHeaderHeight)) / row_h) +
                   1;  // one past, for a partial
  // Rows are clipped to the area below the breadcrumb, so they never draw over
  // it while scrolling.
  c.push_clip(Rect{0, kHeaderHeight, w, h - kHeaderHeight});
  for (int i = first; i < last; ++i) {
    if (i < 0 || static_cast<size_t>(i) >= vis.size()) continue;
    const Row& row = vis[static_cast<size_t>(i)];
    const SidebarNode* n = row.node;
    if (n == nullptr) continue;
    const double y = kHeaderHeight + static_cast<double>(i) * row_h -
                     scroll_y_;

    if (n == selected()) {
      c.fill_rect(Rect{0, y, w, row_h}, t.selection);
    }

    // The disclosure chevron for directories, the indent for everything.
    const double x0 = kPadLeft + static_cast<double>(row.depth) * kIndent;
    if (n->is_dir) {
      const Run arrow = tb_.shape(font_, n->expanded ? "\u2304" : "\u203a");
      c.set_text_color(t.gutter_text);
      c.draw_run(arrow, x0, y + (row_h - arrow.ascent - arrow.descent) * 0.5 +
                                arrow.ascent);
    }

    // Truncate a long name with an ellipsis rather than drawing over the edge.
    const double avail = w - x0 - (n->is_dir ? 14.0 : 0.0) - kPadRight;
    std::string label = n->name;
    Run run = tb_.shape(font_, label);
    if (run.width > avail && avail > 0) {
      while (!label.empty() && run.width > avail) {
        label.pop_back();
        // Do not split a UTF-8 sequence: drop continuation bytes too.
        while (!label.empty() &&
               (static_cast<unsigned char>(label.back()) & 0xC0) == 0x80) {
          label.pop_back();
        }
        run = tb_.shape(font_, label + "\u2026");
      }
      label += "\u2026";
      run = tb_.shape(font_, label);
    }

    const bool muted = n->is_symlink;
    c.set_text_color(muted ? t.gutter_text : t.text);
    c.draw_run(run, x0 + (n->is_dir ? 14.0 : 0.0),
               y + (row_h - run.ascent - run.descent) * 0.5 + run.ascent);

    // The enter affordance on a directory: a small chevron at the right edge.
    // It is the difference between "show me what is inside" (enter) and "open
    // this folder in the tree" (the disclosure chevron on the left). Files have
    // no affordance, and neither does a symlink, which is never followed.
    if (n->is_dir && !n->is_symlink) {
      double ax0 = 0, ax1 = 0;
      enter_affordance(row, &ax0, &ax1);
      const Run enter = tb_.shape(font_, "\u203a");
      c.set_text_color(t.gutter_text);
      c.draw_run(enter, (ax0 + ax1) * 0.5 - enter.width * 0.5,
                 y + (row_h - enter.ascent - enter.descent) * 0.5 +
                     enter.ascent);
    }
  }
  c.pop_clip();
}

}  // namespace vc