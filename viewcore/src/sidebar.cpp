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
  rows_valid_ = false;
  scroll_to(0);  // a new root starts at the top
  // The root itself is not one of its own rows, so a selection on it would be
  // invisible and the arrow keys would have nothing to move from. Select the
  // first row instead (nothing, for an empty or unreadable directory).
  selected_path_.clear();
  const std::vector<Row>& r = rows();
  if (!r.empty() && r[0].node != nullptr) selected_path_ = r[0].node->path;
  return root_loaded_;
}

void Sidebar::set_font(const FontSpec& f) { font_ = f; }

bool Sidebar::load(SidebarNode* dir) {
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

void Sidebar::sort_children(SidebarNode* dir) {
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
  if (root_node_ == nullptr || root_node_->children.empty()) {
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

bool Sidebar::reveal(std::string_view path) {
  if (root_node_ == nullptr) return false;
  const std::string target(path);
  if (target == root_) {
    selected_path_ = root_node_->path;
    return false;
  }
  // The components below the root, outermost first: <a>/<b>/file.txt.
  // Walking in that order is what lets each step find its children — the parent
  // must be loaded before the child can be looked up.
  std::vector<std::string> parts;
  // The root must be a *path* prefix: "/foo" is not a prefix of "/foobar".
  const bool under_root =
      target.size() > root_.size() &&
      target.compare(0, root_.size(), root_) == 0 &&
      target[root_.size()] == '/';
  if (under_root) {
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
  SidebarNode* mut = find(sel->path);
  if (mut == nullptr) return false;
  if (!mut->expanded) {
    // Closed: open it, keeping the selection on the directory.
    return toggle(mut);
  }
  // Open: move to its first child, if any.
  rows_valid_ = false;
  const std::vector<Row>& r = rows();
  for (size_t i = 0; i < r.size(); ++i) {
    if (r[i].node == mut && i + 1 < r.size()) {
      const SidebarNode* child = r[i + 1].node;
      if (child != nullptr) {
        selected_path_ = child->path;
        scroll_selection_into_view();
        return true;
      }
    }
  }
  return false;
}

bool Sidebar::move_left() {
  const SidebarNode* sel = selected();
  if (sel == nullptr) return false;
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
  const double over = content_height() - height_;
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
  if (y < 0) return -1;
  const int idx = static_cast<int>((y + scroll_y_) / kRowHeight);
  const std::vector<Row>& r = rows();
  if (idx < 0 || static_cast<size_t>(idx) >= r.size()) return -1;
  return idx;
}

void Sidebar::draw(Canvas& c, const Theme& t) const {
  const double w = width_;
  const double h = height_;
  c.fill_rect(Rect{0, 0, w, h}, t.page);  // the sidebar's ground
  c.fill_rect(Rect{w - 1, 0, 1, h}, t.gutter_border);  // the hairline

  const double row_h = kRowHeight;
  const std::vector<Row>& vis = rows();
  if (vis.empty()) {
    // Nothing to show: a directory that could not be read, or an empty one.
    // Say so rather than leaving a blank column (the row set is empty, so
    // there is nothing to select and nothing to open).
    const Run note = tb_.shape(font_, root_loaded_ ? "No files" : "Unreadable");
    c.set_text_color(t.gutter_text);
    c.draw_run(note, kPadLeft,
               (row_h - note.ascent - note.descent) * 0.5 + note.ascent);
    return;
  }
  // Rows are drawn from the top of the content, panned by the scroll offset.
  // Only the rows that can touch the viewport are considered, so a folder with
  // thousands of entries still costs only what is visible.
  const int first = static_cast<int>(scroll_y_ / row_h);
  const int last =
      static_cast<int>((scroll_y_ + h) / row_h) + 1;  // one past, for a partial
  c.push_clip(Rect{0, 0, w, h});
  for (int i = first; i < last; ++i) {
    if (i < 0 || static_cast<size_t>(i) >= vis.size()) continue;
    const Row& row = vis[static_cast<size_t>(i)];
    const SidebarNode* n = row.node;
    if (n == nullptr) continue;
    const double y = static_cast<double>(i) * row_h - scroll_y_;

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
  }
  c.pop_clip();
}

}  // namespace vc