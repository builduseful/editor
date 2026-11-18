// viewcore/include/viewcore.hpp — platform-independent view logic (§3, §8).
//
// Wrapping, a bounded line cache, hit-testing, selection, scroll, caret, and
// decorations. One implementation, one conformance suite. `viewcore` depends on
// `core` (for the document) and on `textbackend`/`Canvas` (pure virtual).
#pragma once

#include <cstdint>
#include <list>
#include <map>
#include <string>
#include <string_view>
#include <vector>

#include "canvas.hpp"
#include "editor.hpp"
#include "textbackend.hpp"
#include "theme.hpp"
#include "types.hpp"

namespace vc {

struct Decoration {
  int64_t line = 0;
  Color color;
  bool operator==(const Decoration& o) const {
    return line == o.line && color.r == o.color.r && color.g == o.color.g &&
           color.b == o.color.b && color.a == o.color.a;
  }
};

struct Selection {
  int64_t anchor_line = 0, anchor_col = 0;
  int64_t caret_line = 0, caret_col = 0;
  bool operator==(const Selection& o) const {
    return anchor_line == o.anchor_line && anchor_col == o.anchor_col &&
           caret_line == o.caret_line && caret_col == o.caret_col;
  }
};

struct Hit {
  int64_t line = 0, col = 0;  // logical line; col = UTF-8 byte offset in the line
};

// Platform-neutral input. The macOS view translates NSEvent into these (§8).
enum class Key : uint32_t {
  None, Left, Right, Up, Down, Home, End, PageUp, PageDown, SelectAll, Escape,
  Backspace, Delete, Enter, InsertText
};

struct KeyEvent {
  Key key = Key::None;
  std::string_view text;  // for Key::InsertText; borrowed for the call only
  bool shift = false, alt = false, ctrl = false, cmd = false;
};

struct MouseEvent {
  enum class Kind : uint32_t { Down, Drag, Up, Scroll };
  Kind kind = Kind::Down;
  double x = 0, y = 0;  // viewport coordinates, y down
  double dy = 0;        // scroll amount (Kind::Scroll)
  bool shift = false, cmd = false;
};

class ViewCore {
 public:
  ViewCore(Editor& ed, TextBackend& tb);
  ~ViewCore();
  ViewCore(const ViewCore&) = delete;
  ViewCore& operator=(const ViewCore&) = delete;

  void set_document(Editor& ed);
  void resize(double width, double height);
  void set_wrap(bool on);
  void set_font(const FontSpec& f);
  void set_decorations(std::vector<Decoration> d);
  // Applies a Build Useful theme (all colours at once). The platform picks
  // light/dark from the system appearance and calls this.
  void set_theme(const Theme& t);
  // Ink used to draw text (the Canvas sets this before each run).
  void set_text_color(Color c) { text_color_ = c; }
  Color text_color() const { return text_color_; }
  // Selection highlight and caret colours.
  void set_selection_color(Color c) { selection_color_ = c; }
  void set_caret_color(Color c) { caret_color_ = c; }
  // Blink state (the platform drives the timer; viewcore just draws or not).
  void set_caret_visible(bool v) { caret_visible_ = v; }
  bool caret_visible() const { return caret_visible_; }

  // Tab stop width in columns (a tab advances to the next multiple).
  void set_tab_width(int columns) {
    const int c = columns > 0 ? columns : 1;
    if (c != tab_width_) invalidate_all();
    tab_width_ = c;
  }
  int tab_width() const { return tab_width_; }

  // Gutter (line numbers) — M3. When on, text is drawn to the right of it.
  void set_gutter(bool on) {
    if (on != gutter_) {
      if (wrap_) invalidate_all();
      gutter_ = on;
    }
  }
  bool gutter() const { return gutter_; }
  double gutter_width() const {
    update_gutter();
    return gutter_width_;
  }
  void set_gutter_colors(Color bg, Color fg) {
    gutter_bg_ = bg;
    gutter_fg_ = fg;
  }
  // Current-line highlight; alpha 0 disables it.
  void set_current_line_color(Color c) { current_line_color_ = c; }

  double line_height() const;
  // Number of lines discovered so far. The view grows this lazily as it draws or
  // scrolls, so before the document has been fully traversed it is a lower bound
  // on the true line count (call `discover_all()` to force the exact count).
  int64_t line_count() const { return known_lines_; }
  bool line_count_known() const { return eof_known_; }
  // Extends the line index through the end of the document (an exact count, and
  // the whole-document scan the first-paint path avoids).
  void discover_all() const;
  bool wrap() const { return wrap_; }
  const FontSpec& font() const { return font_; }
  double viewport_width() const { return width_; }
  double viewport_height() const { return height_; }

  int64_t first_visible_line() const { return first_line_; }
  void scroll_to_line(int64_t line);
  void scroll_lines(int64_t n);
  void scroll_by(double dy);
  double scroll_px() const { return scroll_px_; }
  // Estimated content height (exact without wrap; an upper estimate with wrap,
  // since a 5 GB document is not fully measured).
  double content_height() const;

  Hit hit_test(double x, double y) const;
  Rect caret_rect(int64_t line, int64_t col) const;

  Selection selection() const { return sel_; }
  void set_selection(const Selection& s) { sel_ = s; }
  void set_caret(int64_t line, int64_t col) {
    sel_.anchor_line = sel_.caret_line = line;
    sel_.anchor_col = sel_.caret_col = col;
  }

  // Draw the visible viewport into `c`.
  void draw(Canvas& c) const;

  // Input. Both return true if the view changed (a redraw is needed).
  bool key(const KeyEvent& e);
  bool mouse(const MouseEvent& e);

  // Editing (M2). Each returns true if the document changed.
  bool insert_text(std::string_view utf8);  // replaces the selection, or inserts
  bool backspace();                         // delete selection / previous codepoint
  bool delete_forward();                    // delete selection / next codepoint
  // The selected text (empty when the selection is collapsed).
  std::string selected_text() const;
  // Call after the document changes outside viewcore (undo/redo/save/reload):
  // drops the line cache and refreshes the line count.
  void document_changed();

  // Literal find/replace (§9: no regex; scans logical lines via read_lines).
  // `find` moves the selection to the next match (wrapping) and returns true.
  bool find(std::string_view needle, bool forward, bool case_sensitive);
  // Replaces the current selection if it matches, then finds the next match.
  bool replace_current(std::string_view needle, std::string_view replacement,
                       bool case_sensitive);
  // Replaces every match; returns the number replaced.
  int64_t replace_all(std::string_view needle, std::string_view replacement,
                      bool case_sensitive);

  // Cache statistics (for tests and the footprint gate).
  size_t cache_entries() const { return cache_.size(); }
  size_t cache_bytes() const { return cache_bytes_; }

 private:
  struct VisualLine {
    int64_t line = 0;
    uint64_t byte_start = 0, byte_end = 0;
    Run run;
  };
  struct LineEntry {
    std::vector<VisualLine> visual;
    size_t bytes = 0;
    bool truncated = false;  // a >1000-visual-line line is only partly cached
  };

  static constexpr size_t kMaxCacheEntries = 1024;
  static constexpr size_t kMaxCacheBytes = 6ull << 20;  // 6 MB (§8)
  static constexpr size_t kMaxVisualPerLine = 1000;     // skip caching beyond

  const LineEntry& entry_for(int64_t line) const;
  void touch(int64_t line) const;
  void evict_if_needed() const;
  void invalidate_all() const;
  // Ensures at least `min_lines` lines are known (extends the core index),
  // recording EOF once reached.
  void grow_to(int64_t min_lines) const;
  // True if `line` exists; grows the index as needed.
  bool line_exists(int64_t line) const;
  double block_height(int64_t line) const;
  int64_t col_at_x(const VisualLine& v, double x) const;
  // Pen x of byte offset `col` within a visual line.
  double x_of_col(const VisualLine& v, int64_t col) const;
  // Widens '\t' glyphs to the next tab stop (keeps one glyph per codepoint).
  void apply_tabs(Run& run) const;
  // Recomputes the gutter width from the known line count (and re-wraps if the
  // width changed with wrap on).
  void update_gutter() const;
  double text_left() const { return gutter_width_; }
  double text_width() const {
    const double w = width_ - gutter_width_;
    return w > 1.0 ? w : 1.0;
  }
  std::string line_text(int64_t line) const;  // logical line, terminator stripped

  // Input helpers.
  int64_t line_len(int64_t line) const;  // logical line length in bytes
  void set_caret_extend(int64_t line, int64_t col, bool extend);
  void move_horizontal(int dir, bool extend);
  void move_vertical(int dir, bool extend);
  void move_line_edge(bool to_end, bool extend);
  void page_move(int dir, bool extend);
  void ensure_caret_visible();
  // Visual line within `line`'s entry holding byte `col`, and its x offset.
  void caret_within(const LineEntry& e, int64_t col, size_t* vi, double* x) const;

  // Editing helpers.
  bool has_selection() const;
  void selection_range(int64_t* sl, int64_t* sc, int64_t* el,
                       int64_t* ec) const;
  int64_t byte_span(int64_t sl, int64_t sc, int64_t el, int64_t ec) const;
  bool delete_range(int64_t sl, int64_t sc, int64_t el, int64_t ec);
  void refresh_line_count();

  Editor* ed_ = nullptr;
  TextBackend* tb_ = nullptr;
  FontSpec font_;
  double width_ = 800, height_ = 600;
  bool wrap_ = false;

  int64_t first_line_ = 0;
  double scroll_px_ = 0;
  double goal_x_ = 0;      // desired x for vertical caret movement
  bool v_moving_ = false;  // a run of vertical moves (keeps goal_x_)
  mutable int64_t known_lines_ = 0;  // lines discovered so far (lazy)
  mutable bool eof_known_ = false;   // the whole document has been indexed
  std::vector<Decoration> decorations_;
  Color background_color_{255, 255, 255, 255};  // color.surface
  Color text_color_{23, 26, 31, 255};           // color.text (ink)
  Color selection_color_{238, 241, 244, 255};   // color.surface-muted
  Color caret_color_{23, 26, 31, 255};
  bool caret_visible_ = true;
  int tab_width_ = 4;
  bool gutter_ = false;
  mutable double gutter_width_ = 0;
  Color gutter_bg_{246, 247, 249, 255};         // color.page
  Color gutter_fg_{98, 106, 118, 255};          // color.text-muted
  Color gutter_border_color_{230, 233, 238, 255};  // color.border-subtle
  Color current_line_color_{0, 0, 0, 0};        // disabled
  Selection sel_;

  mutable std::map<int64_t, LineEntry> cache_;
  mutable std::list<int64_t> lru_;  // front = most recent
  mutable size_t cache_bytes_ = 0;
};

}  // namespace vc
