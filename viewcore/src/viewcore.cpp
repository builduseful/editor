// viewcore/src/viewcore.cpp — the viewcore skeleton (§8).
#include "viewcore.hpp"

#include <algorithm>
#include <string_view>

namespace vc {

namespace {

// Byte length of the UTF-8 codepoint starting at s[i], clamped to the end.
size_t cp_len(std::string_view s, size_t i) {
  const unsigned char c = static_cast<unsigned char>(s[i]);
  size_t n = 1;
  if ((c & 0xE0) == 0xC0) {
    n = 2;
  } else if ((c & 0xF0) == 0xE0) {
    n = 3;
  } else if ((c & 0xF8) == 0xF0) {
    n = 4;
  }
  if (i + n > s.size()) n = s.size() - i;
  return n;
}

}  // namespace

ViewCore::ViewCore(Editor& ed, TextBackend& tb) : ed_(&ed), tb_(&tb) {}
ViewCore::~ViewCore() = default;

void ViewCore::set_document(Editor& ed) {
  ed_ = &ed;
  first_line_ = 0;
  scroll_px_ = 0;
  known_lines_ = 0;
  eof_known_ = false;
  invalidate_all();
}

void ViewCore::resize(double width, double height) {
  if (width != width_) invalidate_all();  // wrap width changed: rekey
  width_ = std::max(1.0, width);
  height_ = std::max(1.0, height);
}

void ViewCore::set_wrap(bool on) {
  if (on != wrap_) invalidate_all();
  wrap_ = on;
}

void ViewCore::set_font(const FontSpec& f) {
  if (f != font_) invalidate_all();
  font_ = f;
}

void ViewCore::set_decorations(std::vector<Decoration> d) {
  decorations_ = std::move(d);
}

double ViewCore::line_height() const { return tb_->line_height(font_); }

// The lazy discovery path. `grow_to` asks the core for one more line than the
// caller needs; read_lines extends the line index only that far (or to EOF) and
// reports how many lines exist, so first paint never scans the whole document.
void ViewCore::grow_to(int64_t min_lines) const {
  if (eof_known_ || min_lines <= known_lines_) return;
  size_t need = 0;
  int64_t n = 0;
  ed_->read_lines(0, min_lines, nullptr, 0, &need, &n);
  if (n < min_lines) {
    eof_known_ = true;
    known_lines_ = n;
  } else {
    known_lines_ = min_lines;
  }
}

bool ViewCore::line_exists(int64_t line) const {
  if (line < 0) return false;
  grow_to(line + 1);
  return line < known_lines_;
}

void ViewCore::discover_all() const {
  if (eof_known_) return;
  Info in;
  ed_->info(&in);  // the exact line count (full index build)
  eof_known_ = true;
  known_lines_ = in.line_count;
}

std::string ViewCore::line_text(int64_t line) const {
  if (!line_exists(line)) return {};
  size_t need = 0;
  int64_t n = 0;
  ed_->read_lines(line, line + 1, nullptr, 0, &need, &n);
  std::string s(need, '\0');
  if (need > 0) {
    size_t got = 0;
    ed_->read_lines(line, line + 1, s.data(), s.size(), &got, &n);
    s.resize(got);
  }
  // Strip the line terminator; the view draws content only.
  if (!s.empty() && s.back() == '\n') s.pop_back();
  if (!s.empty() && s.back() == '\r') s.pop_back();
  return s;
}

// Largest byte offset <= text.size() whose prefix [pos, off) fits in maxw.
static size_t fit_prefix(TextBackend& tb, const FontSpec& f,
                         std::string_view text, size_t pos, double maxw) {
  std::vector<size_t> b;
  for (size_t i = pos; i < text.size();) {
    b.push_back(i);
    i += cp_len(text, i);
  }
  b.push_back(text.size());
  if (tb.measure(f, text.substr(pos, text.size() - pos)) <= maxw) {
    return text.size();
  }
  size_t lo = 0, hi = b.size() - 1;  // lo: fits; hi: does not
  while (lo + 1 < hi) {
    const size_t mid = lo + (hi - lo) / 2;
    if (tb.measure(f, text.substr(pos, b[mid] - pos)) <= maxw) {
      lo = mid;
    } else {
      hi = mid;
    }
  }
  return b[lo];
}

const ViewCore::LineEntry& ViewCore::entry_for(int64_t line) const {
  auto it = cache_.find(line);
  if (it != cache_.end()) {
    touch(line);
    return it->second;
  }

  LineEntry e;
  const std::string text = line_text(line);
  if (!wrap_) {
    VisualLine v;
    v.line = line;
    v.byte_start = 0;
    v.byte_end = text.size();
    v.run = tb_->shape(font_, text);
    v.run.logical_line = line;
    v.run.byte_start = 0;
    e.bytes = text.size() + v.run.glyphs.size() * sizeof(Glyph) + 96;
    e.visual.push_back(std::move(v));
  } else {
    const double maxw = width_;
    size_t pos = 0;
    while (pos < text.size()) {
      if (e.visual.size() >= kMaxVisualPerLine) {
        e.truncated = true;
        break;
      }
      size_t end = fit_prefix(*tb_, font_, text, pos, maxw);
      if (end <= pos) end = pos + cp_len(text, pos);  // always make progress
      if (end < text.size()) {
        // Prefer to break just after the last space so words stay whole.
        const size_t sp = text.rfind(' ', end - 1);
        if (sp != std::string::npos && sp >= pos) end = sp + 1;
      }
      VisualLine v;
      v.line = line;
      v.byte_start = pos;
      v.byte_end = end;
      v.run = tb_->shape(font_, std::string_view(text).substr(pos, end - pos));
      v.run.logical_line = line;
      v.run.byte_start = pos;
      e.bytes = e.bytes + (end - pos) + v.run.glyphs.size() * sizeof(Glyph) + 96;
      e.visual.push_back(std::move(v));
      pos = end;
    }
    if (text.empty()) {
      VisualLine v;
      v.line = line;
      v.run = tb_->shape(font_, std::string_view());
      v.run.logical_line = line;
      e.visual.push_back(std::move(v));
      e.bytes = 96;
    }
  }

  auto res = cache_.emplace(line, std::move(e));
  lru_.push_front(line);
  cache_bytes_ += res.first->second.bytes;
  evict_if_needed();
  return res.first->second;
}

void ViewCore::touch(int64_t line) const {
  lru_.remove(line);
  lru_.push_front(line);
}

void ViewCore::evict_if_needed() const {
  while (!lru_.empty() && (cache_.size() > kMaxCacheEntries ||
                           cache_bytes_ > kMaxCacheBytes)) {
    const int64_t victim = lru_.back();
    lru_.pop_back();
    auto it = cache_.find(victim);
    if (it != cache_.end()) {
      cache_bytes_ -= it->second.bytes;
      cache_.erase(it);
    }
  }
}

void ViewCore::invalidate_all() const {
  cache_.clear();
  lru_.clear();
  cache_bytes_ = 0;
}

double ViewCore::block_height(int64_t line) const {
  return static_cast<double>(entry_for(line).visual.size()) * line_height();
}

void ViewCore::scroll_to_line(int64_t line) {
  if (line < 0) line = 0;
  grow_to(line + 1);
  // If the document ended before `line`, clamp to the last line; otherwise the
  // requested line is known to exist.
  first_line_ = eof_known_ ? std::min<int64_t>(line, known_lines_) : line;
  scroll_px_ = 0;
}

void ViewCore::scroll_lines(int64_t n) { scroll_to_line(first_line_ + n); }

void ViewCore::scroll_by(double dy) {
  if (known_lines_ == 0 && eof_known_) {
    first_line_ = 0;
    scroll_px_ = 0;
    return;
  }
  scroll_px_ += dy;
  while (scroll_px_ >= block_height(first_line_) && line_exists(first_line_ + 1)) {
    scroll_px_ -= block_height(first_line_);
    first_line_ += 1;
  }
  while (first_line_ > 0 && scroll_px_ < 0) {
    first_line_ -= 1;
    scroll_px_ += block_height(first_line_);
  }
  if (scroll_px_ < 0) scroll_px_ = 0;
}

double ViewCore::content_height() const {
  return static_cast<double>(line_count()) * line_height();
}

int64_t ViewCore::col_at_x(const VisualLine& v, double x) const {
  double pen = 0;
  size_t rel = 0;  // byte offset within v.run.text
  for (const Glyph& g : v.run.glyphs) {
    if (x < pen + g.advance / 2.0) {
      return static_cast<int64_t>(v.byte_start + rel);
    }
    pen += g.advance;
    rel += cp_len(v.run.text, rel);
  }
  return static_cast<int64_t>(v.byte_start + rel);
}

double ViewCore::x_of_col(const VisualLine& v, int64_t col) const {
  double pen = 0;
  size_t rel = 0;
  for (const Glyph& g : v.run.glyphs) {
    if (static_cast<int64_t>(v.byte_start + rel) >= col) break;
    pen += g.advance;
    rel += cp_len(v.run.text, rel);
  }
  return pen;
}

Hit ViewCore::hit_test(double x, double y) const {
  const double lh = line_height();
  double top = -scroll_px_;
  for (int64_t line = first_line_;; ++line) {
    if (!line_exists(line)) break;
    const LineEntry& e = entry_for(line);
    for (const VisualLine& v : e.visual) {
      if (y < top + lh) return Hit{line, col_at_x(v, x)};
      top += lh;
    }
  }
  return Hit{known_lines_ > 0 ? known_lines_ - 1 : 0, 0};
}

Rect ViewCore::caret_rect(int64_t line, int64_t col) const {
  const double lh = line_height();
  if (line < first_line_) return Rect{0, -1e9, 1, lh};  // above the viewport
  if (!line_exists(line)) return Rect{0, 0, 1, lh};
  double top = -scroll_px_;
  for (int64_t l = first_line_; l < line; ++l) top += block_height(l);
  const LineEntry& e = entry_for(line);
  double y = top;
  for (const VisualLine& v : e.visual) {
    if (col >= static_cast<int64_t>(v.byte_start) &&
        col <= static_cast<int64_t>(v.byte_end)) {
      double pen = 0;
      size_t rel = 0;
      for (const Glyph& g : v.run.glyphs) {
        if (static_cast<int64_t>(v.byte_start + rel) >= col) break;
        pen += g.advance;
        rel += cp_len(v.run.text, rel);
      }
      return Rect{pen, y, 1, lh};
    }
    y += lh;
  }
  return Rect{0, y, 1, lh};
}

void ViewCore::draw(Canvas& c) const {
  const double lh = line_height();
  c.clear(Color{255, 255, 255, 255});
  c.set_text_color(text_color_);

  const bool has_sel = sel_.anchor_line != sel_.caret_line ||
                       sel_.anchor_col != sel_.caret_col;
  int64_t sline = 0, scol = 0, eline = 0, ecol = 0;
  if (has_sel) {
    const bool fwd = sel_.anchor_line < sel_.caret_line ||
                     (sel_.anchor_line == sel_.caret_line &&
                      sel_.anchor_col <= sel_.caret_col);
    sline = fwd ? sel_.anchor_line : sel_.caret_line;
    scol = fwd ? sel_.anchor_col : sel_.caret_col;
    eline = fwd ? sel_.caret_line : sel_.anchor_line;
    ecol = fwd ? sel_.caret_col : sel_.anchor_col;
  }

  double caret_x = -1, caret_y = 0;
  double y = -scroll_px_;
  for (int64_t line = first_line_; y < height_; ++line) {
    if (!line_exists(line)) break;
    const LineEntry& e = entry_for(line);
    const int64_t line_content = static_cast<int64_t>(e.visual.back().byte_end);
    const bool line_sel = has_sel && line >= sline && line <= eline;
    for (const VisualLine& v : e.visual) {
      if (y + lh > 0) {
        for (const Decoration& d : decorations_) {
          if (d.line == line) c.fill_rect(Rect{0, y, width_, lh}, d.color);
        }
        if (line_sel) {
          int64_t lo = line == sline ? scol : 0;
          int64_t hi = line == eline ? ecol : line_content;
          if (lo < static_cast<int64_t>(v.byte_start)) {
            lo = static_cast<int64_t>(v.byte_start);
          }
          if (hi > static_cast<int64_t>(v.byte_end)) {
            hi = static_cast<int64_t>(v.byte_end);
          }
          if (hi > lo) {
            const double x0 = x_of_col(v, lo);
            const double x1 = x_of_col(v, hi);
            c.fill_rect(Rect{x0, y, x1 - x0, lh}, selection_color_);
          }
        }
        if (!has_sel && line == sel_.caret_line &&
            sel_.caret_col >= static_cast<int64_t>(v.byte_start) &&
            sel_.caret_col <= static_cast<int64_t>(v.byte_end)) {
          caret_x = x_of_col(v, sel_.caret_col);
          caret_y = y;
        }
        c.draw_run(v.run, 0, y + v.run.ascent);
      }
      y += lh;
      if (y >= height_) break;
    }
  }

  if (caret_x >= 0 && caret_y >= 0 && caret_y < height_) {
    c.fill_rect(Rect{caret_x, caret_y, 1.5, lh}, caret_color_);
  }
}

// --- Input (M1: read-only movement and selection) --------------------------

int64_t ViewCore::line_len(int64_t line) const {
  return static_cast<int64_t>(line_text(line).size());
}

void ViewCore::set_caret_extend(int64_t line, int64_t col, bool extend) {
  if (extend) {
    sel_.caret_line = line;
    sel_.caret_col = col;
  } else {
    sel_.anchor_line = sel_.caret_line = line;
    sel_.anchor_col = sel_.caret_col = col;
  }
}

void ViewCore::caret_within(const LineEntry& e, int64_t col, size_t* vi,
                            double* x) const {
  for (size_t i = 0; i < e.visual.size(); ++i) {
    const VisualLine& v = e.visual[i];
    const int64_t end = (i + 1 < e.visual.size())
                            ? static_cast<int64_t>(e.visual[i + 1].byte_start)
                            : static_cast<int64_t>(v.byte_end);
    if (col >= static_cast<int64_t>(v.byte_start) && col <= end) {
      double pen = 0;
      size_t rel = 0;
      for (const Glyph& g : v.run.glyphs) {
        if (static_cast<int64_t>(v.byte_start + rel) >= col) break;
        pen += g.advance;
        rel += cp_len(v.run.text, rel);
      }
      *vi = i;
      *x = pen;
      return;
    }
  }
  *vi = e.visual.empty() ? 0 : e.visual.size() - 1;
  *x = 0;
}

void ViewCore::move_horizontal(int dir, bool extend) {
  int64_t line = sel_.caret_line;
  int64_t col = sel_.caret_col;
  const std::string text = line_text(line);
  if (dir > 0) {
    if (static_cast<size_t>(col) < text.size()) {
      col += static_cast<int64_t>(cp_len(text, static_cast<size_t>(col)));
    } else if (line_exists(line + 1)) {
      line += 1;
      col = 0;
    }
  } else if (col > 0) {
    size_t p = static_cast<size_t>(col) - 1;
    while (p > 0 && (static_cast<unsigned char>(text[p]) & 0xC0) == 0x80) --p;
    col = static_cast<int64_t>(p);
  } else if (line > 0) {
    line -= 1;
    col = line_len(line);
  }
  v_moving_ = false;
  set_caret_extend(line, col, extend);
}

void ViewCore::move_vertical(int dir, bool extend) {
  int64_t line = sel_.caret_line;
  const LineEntry& e = entry_for(line);
  size_t vi = 0;
  double x = 0;
  caret_within(e, sel_.caret_col, &vi, &x);
  if (!v_moving_) goal_x_ = x;
  const int64_t nvi = static_cast<int64_t>(vi) + dir;
  if (nvi >= 0 && nvi < static_cast<int64_t>(e.visual.size())) {
    sel_.caret_col = col_at_x(e.visual[static_cast<size_t>(nvi)], goal_x_);
  } else if (dir > 0) {
    if (!line_exists(line + 1)) return;  // already at the last line
    line += 1;
    sel_.caret_col = col_at_x(entry_for(line).visual.front(), goal_x_);
  } else {
    if (line <= 0) return;  // already at the first line
    line -= 1;
    sel_.caret_col = col_at_x(entry_for(line).visual.back(), goal_x_);
  }
  v_moving_ = true;
  set_caret_extend(line, sel_.caret_col, extend);
}

void ViewCore::move_line_edge(bool to_end, bool extend) {
  const int64_t line = sel_.caret_line;
  const int64_t col = to_end ? line_len(line) : 0;
  v_moving_ = false;
  set_caret_extend(line, col, extend);
}

void ViewCore::page_move(int dir, bool extend) {
  const int64_t lines =
      std::max<int64_t>(1, static_cast<int64_t>(height_ / line_height()));
  scroll_lines(dir * lines);
  int64_t line = first_line_;
  if (!line_exists(line)) line = known_lines_ > 0 ? known_lines_ - 1 : 0;
  const LineEntry& e = entry_for(line);
  sel_.caret_col = col_at_x(e.visual.front(), goal_x_);
  v_moving_ = true;
  set_caret_extend(line, sel_.caret_col, extend);
}

void ViewCore::ensure_caret_visible() {
  const int64_t line = sel_.caret_line;
  if (line < first_line_) {
    scroll_to_line(line);
    return;
  }
  const int64_t vis =
      std::max<int64_t>(1, static_cast<int64_t>(height_ / line_height()));
  if (line >= first_line_ + vis) scroll_to_line(line - vis + 1);
}

bool ViewCore::key(const KeyEvent& e) {
  const Selection before = sel_;
  const int64_t before_first = first_line_;
  const double before_px = scroll_px_;
  switch (e.key) {
    case Key::Left: move_horizontal(-1, e.shift); break;
    case Key::Right: move_horizontal(1, e.shift); break;
    case Key::Up: move_vertical(-1, e.shift); break;
    case Key::Down: move_vertical(1, e.shift); break;
    case Key::Home: move_line_edge(false, e.shift); break;
    case Key::End: move_line_edge(true, e.shift); break;
    case Key::PageUp: page_move(-1, e.shift); break;
    case Key::PageDown: page_move(1, e.shift); break;
    case Key::SelectAll:
      discover_all();
      sel_.anchor_line = 0;
      sel_.anchor_col = 0;
      sel_.caret_line = known_lines_ > 0 ? known_lines_ - 1 : 0;
      sel_.caret_col = line_len(sel_.caret_line);
      v_moving_ = false;
      break;
    case Key::Escape:
      sel_.anchor_line = sel_.caret_line;
      sel_.anchor_col = sel_.caret_col;
      break;
    case Key::None: return false;
  }
  ensure_caret_visible();
  return !(sel_ == before) || first_line_ != before_first ||
         scroll_px_ != before_px;
}

bool ViewCore::mouse(const MouseEvent& e) {
  const Selection before = sel_;
  const int64_t before_first = first_line_;
  const double before_px = scroll_px_;
  switch (e.kind) {
    case MouseEvent::Kind::Scroll:
      scroll_by(e.dy);
      break;
    case MouseEvent::Kind::Down: {
      const Hit h = hit_test(e.x, e.y);
      v_moving_ = false;
      sel_.anchor_line = sel_.caret_line = h.line;
      sel_.anchor_col = sel_.caret_col = h.col;
      break;
    }
    case MouseEvent::Kind::Drag: {
      const Hit h = hit_test(e.x, e.y);
      sel_.caret_line = h.line;
      sel_.caret_col = h.col;
      break;
    }
    case MouseEvent::Kind::Up:
      break;
  }
  return !(sel_ == before) || first_line_ != before_first ||
         scroll_px_ != before_px;
}

}  // namespace vc
