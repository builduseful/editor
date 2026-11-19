// viewcore/include/sidebar.hpp — the file sidebar's model and layout (§9).
//
// A recursive tree, lazy by construction (PLAN.md §9 "Sidebar"): a directory is
// read only when it is expanded, plus the open file's ancestors so the tree can
// reveal it. Collapsed subtrees are never walked, and there is no background
// scanner or ignore-rule engine.
//
// The directory reads sit behind `DirSource`, a pure virtual seam. `platform/macos`
// implements it with `opendir`/`readdir`; tests use a fake, so the tree, its
// layout and its hit-testing are covered headlessly rather than only by looking
// at the window.
//
// The sidebar is custom-drawn (D2) from the same tokens as the document view: it
// owns no native control. It knows nothing about AppKit or CoreText — it takes a
// TextBackend to measure labels and a Canvas to draw.
#pragma once

#include <cstdint>
#include <memory>
#include <string>
#include <string_view>
#include <vector>

#include "canvas.hpp"
#include "textbackend.hpp"
#include "theme.hpp"
#include "types.hpp"

namespace vc {

// One directory entry, as the source reports it. `name` is the last path
// component; `path` is the full path (the source builds it).
struct DirEntry {
  std::string name;
  std::string path;
  bool is_dir = false;
  // A symlink is neither followed nor expanded (v1): it is shown as a leaf, so
  // the tree cannot loop. Resolving them is a later decision.
  bool is_symlink = false;
};

// The directory-read seam. `read_dir` fills `out` with the direct children of
// `path` (no recursion), already unsorted; the sidebar sorts. Returning false
// means the directory could not be read — the sidebar keeps an empty list for
// it and does not retry on every draw.
class DirSource {
 public:
  virtual ~DirSource() = default;
  virtual bool read_dir(std::string_view path, std::vector<DirEntry>* out) = 0;
};

// A node in the tree. Children are owned by their parent; a directory's
// children are empty until it is expanded (and stay empty if the read failed).
struct SidebarNode {
  std::string name;  // last component; the root's is its full path
  std::string path;  // full path
  bool is_dir = false;
  bool is_symlink = false;
  bool expanded = false;  // a directory the user has opened (or that is revealed)
  bool loaded = false;    // a read has been attempted (even if it failed)
  bool read_ok = false;   // the last read of this directory succeeded
  std::vector<std::unique_ptr<SidebarNode>> children;
};

class Sidebar {
 public:
  // `src` and `tb` must outlive the sidebar (the app owns both).
  Sidebar(DirSource& src, TextBackend& tb);

  // Points the tree at `root_path`, an existing directory. Collapses everything
  // and selects the root. Returns false if the root itself cannot be read.
  bool set_root(std::string_view root_path);
  // Re-reads a directory that has already been loaded (its children changed on
  // disk), keeping it expanded and the selection. Returns true if the rows
  // changed. Used after a save-as that created the open file.
  bool refresh(const std::string& path);

  const std::string& root() const { return root_; }
  bool loaded_root() const { return root_loaded_; }

  // --- Rows -----------------------------------------------------------------
  // The flattened, visible rows: one per expanded node, in tree order, folders
  // first within each directory. Built on demand from the tree (cheap: it only
  // walks expanded nodes), so it is safe to call every frame.
  struct Row {
    const SidebarNode* node = nullptr;
    int depth = 0;
  };
  const std::vector<Row>& rows() const;

  // The selected node (nullptr when nothing is selected). The selection is held
  // by path, not by pointer: re-reading a directory destroys its child nodes,
  // so a stored pointer would dangle. `selected()` looks the node up each time.
  const SidebarNode* selected() const;
  // The selected node's path (empty when nothing is selected).
  const std::string& selected_path() const { return selected_path_; }
  void set_selected(const SidebarNode* n) {
    selected_path_ = n != nullptr ? n->path : std::string();
  }
  void set_selected_path(std::string_view path) { selected_path_ = path; }

  // Opens/closes a directory: reads it on the first expansion. Returns true if
  // the tree changed (a redraw is needed). A file (or a symlink) is a no-op.
  bool toggle(const SidebarNode* n);
  // Expands every ancestor of `path` and selects the node itself, so the tree
  // reveals the open file. Returns false if the path is not under the root.
  bool reveal(std::string_view path);

  // --- Layout ---------------------------------------------------------------
  // Fixed-width sidebar (chrome). Row height follows the UI font.
  static constexpr double kDefaultWidth = 220.0;
  static constexpr double kRowHeight = 22.0;
  static constexpr double kIndent = 14.0;  // one level per depth
  static constexpr double kPadLeft = 8.0;
  static constexpr double kPadRight = 8.0;

  void set_width(double w) { width_ = w > 80.0 ? w : 80.0; }
  double width() const { return width_; }
  // The sidebar scrolls when a folder holds more rows than fit. The offset is
  // in points from the top row, clamped to the content.
  void set_height(double h) { height_ = h > 0 ? h : 0; }
  double height() const { return height_; }
  void scroll_by(double dy);
  void scroll_to(double y);
  // Ensures the selected row is fully visible (used after reveal / toggle).
  void scroll_selection_into_view();
  double scroll_y() const { return scroll_y_; }
  double max_scroll() const;
  // Recomputes the label width for the current font (used for hit-testing).
  void set_font(const FontSpec& f);

  // Row index at a viewport point (y down, x within the sidebar), or -1. The
  // point is in the sidebar's coordinates, already scrolled.
  int row_at(double x, double y) const;
  // Vertical extent of the drawn rows.
  double content_height() const { return static_cast<double>(rows().size()) * kRowHeight; }

  // --- Drawing --------------------------------------------------------------
  void draw(Canvas& c, const Theme& t) const;

 private:
  // The on-screen node for `path` (expanded nodes only), or nullptr.
  SidebarNode* find(std::string_view path);
  // Reads a directory's children once, sorted (folders first, then names
  // case-insensitively). Marks the node loaded even on failure; returns whether
  // the read succeeded.
  bool load(SidebarNode* dir);
  void sort_children(SidebarNode* dir);

  DirSource& src_;
  TextBackend& tb_;
  FontSpec font_;
  double width_ = kDefaultWidth;
  std::string root_;
  bool root_loaded_ = false;
  std::unique_ptr<SidebarNode> root_node_;
  std::string selected_path_;  // the selection, by path (see `selected()`)
  mutable std::vector<Row> rows_;
  mutable bool rows_valid_ = false;
  double height_ = 600.0;
  double scroll_y_ = 0.0;
};

}  // namespace vc