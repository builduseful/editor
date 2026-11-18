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
  double y = -scroll_px_;
  for (int64_t line = first_line_; y < height_; ++line) {
    if (!line_exists(line)) break;
    const LineEntry& e = entry_for(line);
    for (const VisualLine& v : e.visual) {
      if (y + lh > 0) {
        for (const Decoration& d : decorations_) {
          if (d.line == line) {
            c.fill_rect(Rect{0, y, width_, lh}, d.color);
          }
        }
        c.draw_run(v.run, 0, y + v.run.ascent);
      }
      y += lh;
      if (y >= height_) break;
    }
  }
}

}  // namespace vc
