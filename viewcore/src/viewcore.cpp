// viewcore/src/viewcore.cpp — the viewcore skeleton (§8).
#include "viewcore.hpp"

#include <algorithm>
#include <cmath>
#include <limits>
#include <span>
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

// Decodes the codepoint starting at s[i] (invalid UTF-8 degrades to a byte).
uint32_t cp_at(std::string_view s, size_t i) {
  const unsigned char c = static_cast<unsigned char>(s[i]);
  const size_t n = cp_len(s, i);
  if (n == 1) return c;
  uint32_t v = c & (n == 2 ? 0x1Fu : (n == 3 ? 0x0Fu : 0x07u));
  for (size_t k = 1; k < n; ++k) {
    v = (v << 6) | (static_cast<unsigned char>(s[i + k]) & 0x3Fu);
  }
  return v;
}

// Start of the codepoint that ends at s[i].
size_t prev_cp(std::string_view s, size_t i) {
  size_t p = i;
  while (p > 0) {
    --p;
    if ((static_cast<unsigned char>(s[p]) & 0xC0) != 0x80) break;
  }
  return p;
}

enum class CharClass : uint32_t { Space, Word, Other };

CharClass char_class(uint32_t cp) {
  if (cp == ' ' || cp == '\t') return CharClass::Space;
  if (cp == '_' || (cp >= '0' && cp <= '9') || (cp >= 'A' && cp <= 'Z') ||
      (cp >= 'a' && cp <= 'z') || cp >= 0x80) {
    return CharClass::Word;  // non-ASCII counts as a letter (no Unicode tables)
  }
  return CharClass::Other;
}

char lower_ascii(char c) {
  return (c >= 'A' && c <= 'Z') ? static_cast<char>(c + 32) : c;
}

bool match_at(std::string_view h, size_t at, std::string_view n, bool cs) {
  if (at + n.size() > h.size()) return false;
  for (size_t i = 0; i < n.size(); ++i) {
    char a = h[at + i];
    char b = n[i];
    if (!cs) {
      a = lower_ascii(a);
      b = lower_ascii(b);
    }
    if (a != b) return false;
  }
  return true;
}

size_t find_in(std::string_view h, std::string_view n, size_t from, bool cs) {
  if (n.empty() || n.size() > h.size()) return std::string_view::npos;
  for (size_t i = from; i + n.size() <= h.size(); ++i) {
    if (match_at(h, i, n, cs)) return i;
  }
  return std::string_view::npos;
}

}  // namespace

ViewCore::ViewCore(Editor& ed, TextBackend& tb) : ed_(&ed), tb_(&tb) {
  set_theme(light_theme());
}
ViewCore::~ViewCore() = default;

void ViewCore::set_theme(const Theme& t) {
  background_color_ = t.background;
  text_color_ = t.text;
  caret_color_ = t.text;
  selection_color_ = t.selection;
  current_line_color_ = t.current_line;
  gutter_bg_ = t.page;
  gutter_fg_ = t.gutter_text;
  gutter_border_color_ = t.gutter_border;
  border_control_color_ = t.border_control;
}

void ViewCore::set_document(Editor& ed) {
  ed_ = &ed;
  first_line_ = 0;
  scroll_px_ = 0;
  known_lines_ = 0;
  eof_known_ = false;
  sb_drag_ = false;
  sb_drag_f_ = -1;
  pending_line_ = -1;
  content_w_ = 0;
  x_offset_ = 0;
  hsb_drag_ = false;
  invalidate_all();
}

void ViewCore::resize(double width, double height) {
  if (width != width_) invalidate_all();  // wrap width changed: rekey
  width_ = std::max(1.0, width);
  height_ = std::max(1.0, height);
  clamp_scroll();  // a taller viewport may need to pull the top line back
  clamp_x();       // a wider viewport may leave nothing to pan
}

void ViewCore::set_wrap(bool on) {
  if (on != wrap_) invalidate_all();
  wrap_ = on;
  if (wrap_) x_offset_ = 0;  // wrapped text always starts at the left edge
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

bool ViewCore::discover_step(int64_t max_lines) const {
  if (eof_known_) return false;
  if (max_lines < 1) max_lines = 1;
  grow_to(known_lines_ + max_lines);
  return !eof_known_;
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
    update_content_width(it->second);
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
    apply_tabs(v.run);
    e.bytes = text.size() + v.run.glyphs.size() * sizeof(Glyph) + 96;
    e.visual.push_back(std::move(v));
  } else {
    const double maxw = text_width();
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
      apply_tabs(v.run);
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
  update_content_width(res.first->second);
  evict_if_needed();
  return res.first->second;
}

void ViewCore::update_content_width(const LineEntry& e) const {
  for (const VisualLine& v : e.visual) {
    if (v.run.width > content_w_) content_w_ = v.run.width;
  }
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
  wrap_total_valid_ = false;  // block heights changed: re-measure the total
}

double ViewCore::block_height(int64_t line) const {
  return static_cast<double>(entry_for(line).visual.size()) * line_height();
}

void ViewCore::scroll_to_line(int64_t line) {
  if (line < 0) line = 0;
  pending_line_ = -1;  // a direct scroll supersedes a queued jump
  grow_to(line + 1);
  // If the document ended before `line`, clamp to the last line; otherwise the
  // requested line is known to exist.
  first_line_ = eof_known_ ? std::min<int64_t>(line, known_lines_) : line;
  scroll_px_ = 0;
  clamp_scroll();
}

void ViewCore::scroll_lines(int64_t n) { scroll_to_line(first_line_ + n); }

void ViewCore::scroll_by(double dy) {
  pending_line_ = -1;  // the user scrolled: abandon a queued jump
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
  clamp_scroll();
}

// The furthest legal top position: walk back from the end accumulating block
// heights until the viewport is full. Measured backwards so it costs a viewport
// of blocks and not the document. In pixels, because under wrap a single line
// can be several screens tall (and, without wrap, a document whose height is not
// a whole multiple of the line height would otherwise leave the last few points
// unreachable).
ViewCore::ScrollPos ViewCore::max_top() const {
  ScrollPos out;
  if (!eof_known_ || known_lines_ <= 0) return out;
  const double view = text_height();
  double h = 0;
  int64_t line = known_lines_;
  while (line > 0 && h < view) {
    --line;
    h += block_height(line);
  }
  out.line = line;
  out.px = h - view;
  if (out.px < 0) out.px = 0;
  // The walk stops at the first block whose tail fills the viewport, so `px` is
  // inside that block. Guard the boundary anyway (a zero-height block, say).
  const double bh = block_height(line);
  if (bh > 0 && out.px >= bh && line + 1 < known_lines_) {
    out.line = line + 1;
    out.px = 0;
  }
  return out;
}

double ViewCore::content_px() const {
  const double lh = line_height();
  if (!wrap_ || !eof_known_ || known_lines_ <= 0 || lh <= 0) {
    return static_cast<double>(line_count()) * lh;  // one visual line each
  }
  if (wrap_total_valid_) return wrap_total_px_;
  // Measure in visual lines, not logical ones: a single long line can wrap into
  // hundreds of rows, and the cost here is one shaped row. Bounded so a resize
  // on a large wrapped document stays a frame's work rather than seconds; the
  // remainder is extrapolated from the average of what was measured.
  double h = 0;
  int64_t visual = 0;
  int64_t line = 0;
  for (; line < known_lines_ && visual < kMaxMeasuredVisualLines; ++line) {
    const int64_t v = static_cast<int64_t>(entry_for(line).visual.size());
    visual += v;
    h += static_cast<double>(v) * lh;
  }
  if (line > 0 && line < known_lines_) {
    h += h / static_cast<double>(line) * static_cast<double>(known_lines_ - line);
  }
  wrap_total_px_ = h;
  wrap_total_valid_ = true;
  return h;
}

double ViewCore::scroll_units(int64_t line, double px) const {
  const double bh = block_height(line);
  return static_cast<double>(line) + (bh > 0 ? px / bh : 0.0);
}

double ViewCore::content_height() const { return content_px(); }

// Where the top sits through the scrollable range, 0..1, in *logical line*
// units: `line` plus the fraction of that line's block already scrolled. The
// ends are exact (0 at the document start, 1 at the last legal position), which
// is what makes a drag stop precisely at either end. The middle is therefore
// approximate when wrap heights vary from line to line — the thumb still tracks
// the pointer exactly (see scroll_to_fraction), but a given thumb position maps
// to the nearest logical line rather than a pixel-exact place.
double ViewCore::scroll_fraction() const {
  if (!eof_known_ || known_lines_ <= 0) return 0;
  const ScrollPos end = max_top();
  const double mx = scroll_units(end.line, end.px);
  if (mx <= 0) return 0;
  const double cur = scroll_units(first_line_, scroll_px_);
  double f = cur / mx;
  if (f < 0) f = 0;
  if (f > 1) f = 1;
  return f;
}

void ViewCore::clamp_scroll() {
  if (!eof_known_ || known_lines_ <= 0) return;
  if (first_line_ < 0) {
    first_line_ = 0;
    scroll_px_ = 0;
    return;
  }
  // Far from the end every line is at least `line_height` tall, so the remaining
  // content is taller than the viewport: nothing to clamp. This keeps the
  // backwards walk off the wheel path for a large document.
  const double lh = line_height();
  if (lh > 0 && static_cast<double>(known_lines_ - first_line_) * lh >
                    2.0 * text_height()) {
    return;
  }
  const ScrollPos end = max_top();
  if (first_line_ > end.line ||
      (first_line_ == end.line && scroll_px_ > end.px + 1e-9)) {
    first_line_ = end.line;
    scroll_px_ = end.px;
  }
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

void ViewCore::update_gutter() const {
  double w = 0;
  if (gutter_) {
    int64_t n = known_lines_ > 0 ? known_lines_ : 1;
    int digits = 1;
    while (n >= 10) {
      n /= 10;
      ++digits;
    }
    w = static_cast<double>(digits) * tb_->measure(font_, "0") + 16.0;
  }
  if (std::abs(w - gutter_width_) > 0.01) {
    gutter_width_ = w;
    if (wrap_) invalidate_all();
  }
}

void ViewCore::draw_scrollbar(Canvas& c) const {
  const ScrollBar sb = scrollbar();
  if (!sb.visible) return;
  // Only the thumb: no track band, so wrapped text at the right edge is not
  // covered by a solid strip (the thumb itself is an overlay, as on macOS).
  c.fill_rect(Rect{sb.track_x + 2, sb.thumb_y + 1, sb.track_w - 4, sb.thumb_h - 2},
              border_control_color_);
}

int64_t ViewCore::visible_logical_lines() const {
  const double lh = line_height();
  const int64_t fallback =
      lh > 0 ? std::max<int64_t>(1, static_cast<int64_t>(height_ / lh)) : 1;
  if (scroll_px_ < 0) return fallback;
  double y = -scroll_px_;
  int64_t n = 0;
  for (int64_t line = first_line_; y < text_height(); ++line) {
    if (!line_exists(line)) break;
    y += block_height(line);
    ++n;
  }
  // Past the last line (the existing scroll model allows first_line_ == count)
  // there is nothing to measure, so fall back to the viewport height in lines.
  return n > 0 ? n : fallback;
}

ViewCore::ScrollBar ViewCore::scrollbar() const {
  ScrollBar sb;
  sb.track_w = kScrollbarWidth;
  sb.track_x = width_ - sb.track_w;
  const double track_h = height_ - (hbar_visible() ? kScrollbarWidth : 0.0);
  if (track_h <= 0) return sb;

  if (!eof_known_) {
    // Above the discovery cap the line count is unknown, so the bar is driven by
    // byte progress: where the top line starts, and how many bytes the viewport
    // covers. It becomes exact once the drip index completes.
    const uint64_t total = ed_->rev().size;
    if (total == 0 || known_lines_ <= 0) return sb;
    size_t top = 0, vis_bytes = 0;
    int64_t n = 0;
    ed_->read_lines(0, first_line_, nullptr, 0, &top, &n);
    ed_->read_lines(first_line_, first_line_ + visible_logical_lines(), nullptr,
                    0, &vis_bytes, &n);
    if (vis_bytes == 0 || vis_bytes >= total) return sb;
    const double span = static_cast<double>(total - vis_bytes);
    sb.visible = true;
    sb.approximate = true;
    double f = static_cast<double>(top) / span;
    if (sb_drag_f_ >= 0) f = sb_drag_f_;  // preview while dragging
    if (f < 0) f = 0;
    if (f > 1) f = 1;
    sb.thumb_h = track_h * static_cast<double>(vis_bytes) /
                 static_cast<double>(total);
    if (sb.thumb_h < 28) sb.thumb_h = 28;
    if (sb.thumb_h > track_h) sb.thumb_h = track_h;
    sb.thumb_y = f * (track_h - sb.thumb_h);
    return sb;
  }

  if (known_lines_ <= 0) return sb;  // empty document
  const ScrollPos end = max_top();
  if (end.line <= 0 && end.px <= 0.0) return sb;  // the whole document fits

  sb.visible = true;
  // Thumb size: the share of the content the viewport shows. Measured from the
  // content height, so a wrapped document (or one very tall wrapped line) gets
  // the short thumb it deserves rather than one sized in logical lines.
  const double total = content_px();
  if (total > 0) {
    sb.thumb_h = track_h * (text_height() / total);
  } else {
    const int64_t vis = visible_logical_lines();
    sb.thumb_h = track_h * static_cast<double>(vis) /
                 static_cast<double>(std::max<int64_t>(1, known_lines_));
  }
  if (sb.thumb_h < 28) sb.thumb_h = 28;
  if (sb.thumb_h > track_h) sb.thumb_h = track_h;
  const double travel = track_h - sb.thumb_h;
  sb.thumb_y = scroll_fraction() * travel;
  return sb;
}

void ViewCore::scroll_to_fraction(double f) {
  if (f < 0) f = 0;
  if (f > 1) f = 1;
  if (eof_known_) {
    // Place the top of the viewport at `f` through the scrollable range, in the
    // same units scroll_fraction() measures, so the thumb follows the pointer
    // and can stop part-way through a wrapped line.
    const ScrollPos end = max_top();
    const double mx = scroll_units(end.line, end.px);
    double target = f * mx;
    int64_t line = static_cast<int64_t>(std::floor(target));
    if (line > end.line) {
      line = end.line;
      target = mx;
    }
    if (line < 0) {
      line = 0;
      target = 0;
    }
    double px = (target - static_cast<double>(line)) * block_height(line);
    if (line == end.line && px > end.px) px = end.px;
    if (px < 0) px = 0;
    pending_line_ = -1;
    first_line_ = line;
    scroll_px_ = px;
    clamp_scroll();
    return;
  }
  // The line count is unknown: estimate it from the bytes-per-line of the
  // indexed prefix and let the caller's discovery loop reach that line (the
  // index work of one jump, not a full build).
  const uint64_t total = ed_->rev().size;
  if (total == 0) return;
  size_t head_bytes = 0;
  int64_t n = 0;
  ed_->read_lines(0, known_lines_, nullptr, 0, &head_bytes, &n);
  double est = 0;
  if (known_lines_ > 0 && head_bytes > 0) {
    const double avg =
        static_cast<double>(head_bytes) / static_cast<double>(known_lines_);
    est = static_cast<double>(total) / avg;
  }
  pending_line_ = static_cast<int64_t>(std::llround(f * est));
  if (pending_line_ < 0) pending_line_ = 0;
}

bool ViewCore::apply_pending_scroll() {
  if (pending_line_ < 0) return false;
  if (eof_known_ && known_lines_ > 0 && pending_line_ > known_lines_ - 1) {
    pending_line_ = known_lines_ - 1;  // the estimate overshot the real end
  }
  if (pending_line_ >= known_lines_) return false;  // still indexing toward it
  const int64_t target = pending_line_;
  pending_line_ = -1;
  scroll_to_line(target);
  return true;
}

// --- Horizontal scrolling --------------------------------------------------

double ViewCore::max_x_offset() const {
  const double pad = 24.0;  // breathing room past the last glyph
  const double need = content_w_ + pad - text_width();
  return need > 0 ? need : 0.0;
}

void ViewCore::clamp_x() {
  if (wrap_) {
    x_offset_ = 0;
    return;
  }
  const double max = max_x_offset();
  if (x_offset_ > max) x_offset_ = max;
  if (x_offset_ < 0) x_offset_ = 0;
}

void ViewCore::scroll_x_by(double dx) {
  if (wrap_) return;
  x_offset_ += dx;
  clamp_x();
}

ViewCore::HScrollBar ViewCore::hscrollbar() const {
  HScrollBar hb;
  if (!hbar_visible()) return hb;
  const double avail = text_width();
  const double max = max_x_offset();
  hb.visible = true;
  hb.track_h = kScrollbarWidth;
  hb.track_y = height_ - kScrollbarWidth;
  // thumb / track = visible / (visible + scrollable)
  hb.thumb_w = avail * avail / (avail + max);
  if (hb.thumb_w < 28) hb.thumb_w = 28;
  if (hb.thumb_w > avail) hb.thumb_w = avail;
  const double travel = avail - hb.thumb_w;
  double f = max > 0 ? x_offset_ / max : 0;
  if (f < 0) f = 0;
  if (f > 1) f = 1;
  hb.thumb_x = text_left() + f * travel;
  return hb;
}

void ViewCore::scroll_x_to_fraction(double f) {
  if (f < 0) f = 0;
  if (f > 1) f = 1;
  if (!hbar_visible()) return;
  const double avail = text_width();
  const double travel = avail - hscrollbar().thumb_w;
  if (travel <= 0) return;
  x_offset_ = f * max_x_offset();
  clamp_x();
}

void ViewCore::draw_hscrollbar(Canvas& c) const {
  const HScrollBar hb = hscrollbar();
  if (!hb.visible) return;
  c.fill_rect(Rect{hb.thumb_x + 1, hb.track_y + 2, hb.thumb_w - 2,
                   hb.track_h - 4},
              border_control_color_);
}

void ViewCore::apply_tabs(Run& run) const {
  if (tab_width_ <= 0) return;
  const double space = tb_->measure(run.font, " ");
  if (space <= 0) return;
  const double stop = static_cast<double>(tab_width_) * space;
  double pen = 0;
  bool any = false;
  for (Glyph& g : run.glyphs) {
    g.x = pen;
    if (g.codepoint == '\t') {
      double adv = stop - std::fmod(pen, stop);
      if (adv <= 0) adv = stop;
      g.advance = adv;
      any = true;
    }
    pen += g.advance;
  }
  if (any) run.width = pen;
}

Hit ViewCore::hit_test(double x, double y) const {
  size_t vi = 0;
  return hit_test_row(x, y, &vi);
}

// `vi` (may be null) receives the index of the visual row within its logical
// line, so a caller can place a caret on the row under the pointer rather than
// at the start of a logical line that is only partly on screen.
Hit ViewCore::hit_test_row(double x, double y, size_t* vi) const {
  update_gutter();
  if (x < text_left()) x = text_left();  // a click in the gutter is column 0
  x -= text_left() - x_offset_;          // undo the horizontal pan
  if (x < 0) x = 0;
  const double lh = line_height();
  double top = -scroll_px_;
  for (int64_t line = first_line_;; ++line) {
    if (!line_exists(line)) break;
    const LineEntry& e = entry_for(line);
    for (size_t i = 0; i < e.visual.size(); ++i) {
      if (y < top + lh) {
        if (vi != nullptr) *vi = i;
        return Hit{line, col_at_x(e.visual[i], x)};
      }
      top += lh;
    }
  }
  if (vi != nullptr) *vi = 0;
  return Hit{known_lines_ > 0 ? known_lines_ - 1 : 0, 0};
}

Rect ViewCore::caret_rect(int64_t line, int64_t col) const {
  update_gutter();
  const double lh = line_height();
  if (line < first_line_) return Rect{0, -1e9, 1, lh};  // above the viewport
  if (!line_exists(line)) return Rect{0, 0, 1, lh};
  double top = -scroll_px_;
  for (int64_t l = first_line_; l < line; ++l) top += block_height(l);
  const LineEntry& e = entry_for(line);
  const double pan = text_left() - x_offset_;
  double y = top;
  for (const VisualLine& v : e.visual) {
    if (col >= static_cast<int64_t>(v.byte_start) &&
        col <= static_cast<int64_t>(v.byte_end)) {
      return Rect{x_of_col(v, col) + pan, y, 1, lh};
    }
    y += lh;
  }
  return Rect{text_left() - x_offset_, y, 1, lh};
}

void ViewCore::draw(Canvas& c) const {
  update_gutter();
  const double lh = line_height();
  const double left = text_left();
  c.clear(background_color_);
  if (gutter_width_ > 0) {
    c.fill_rect(Rect{0, 0, gutter_width_, height_}, gutter_bg_);
    c.fill_rect(Rect{gutter_width_ - 1, 0, 1, height_}, gutter_border_color_);
  }

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
  // Freeze the text viewport for the whole pass: laying out lines widens
  // content_w_, which can bring the horizontal bar in mid-draw, and the rows and
  // the clip must agree on where the text area ends.
  const double text_h = text_height();
  const double pan = left - x_offset_;  // text is panned; the gutter is not
  std::vector<std::pair<int64_t, double>> numbers;  // gutter labels, drawn last
  // Text lives in the area above the horizontal bar. Clipping (rather than
  // hiding the last row) cuts a partially scrolled row at that boundary, so it
  // never paints under the bar or past the view.
  c.push_clip(Rect{0, 0, width_, text_h});
  for (int64_t line = first_line_; y < text_h; ++line) {
    if (!line_exists(line)) break;
    const LineEntry& e = entry_for(line);
    const int64_t line_content = static_cast<int64_t>(e.visual.back().byte_end);
    const bool line_sel = has_sel && line >= sline && line <= eline;
    bool first_visual = true;
    for (const VisualLine& v : e.visual) {
      if (y + lh > 0) {
        if (current_line_color_.a != 0 && line == sel_.caret_line) {
          c.fill_rect(Rect{left, y, text_width(), lh}, current_line_color_);
        }
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
            const double x0 = x_of_col(v, lo) + pan;
            const double x1 = x_of_col(v, hi) + pan;
            c.fill_rect(Rect{x0, y, x1 - x0, lh}, selection_color_);
          }
        }
        if (!has_sel && line == sel_.caret_line &&
            sel_.caret_col >= static_cast<int64_t>(v.byte_start) &&
            sel_.caret_col <= static_cast<int64_t>(v.byte_end)) {
          caret_x = x_of_col(v, sel_.caret_col) + pan;
          caret_y = y;
        }
        c.set_text_color(text_color_);
        c.draw_run(v.run, pan, y + v.run.ascent);
        if (gutter_width_ > 0 && first_visual) {
          numbers.emplace_back(line, y);
        }
      }
      first_visual = false;
      y += lh;
      if (y >= text_h) break;
    }
  }

  if (caret_visible_ && caret_x >= 0 && caret_y >= 0 && caret_y < text_h) {
    c.fill_rect(Rect{caret_x, caret_y, 1.5, lh}, caret_color_);
  }
  c.pop_clip();

  // The gutter is painted over the panned text (it is a fixed column, not part
  // of the scrolled surface), then its labels go on top.
  if (gutter_width_ > 0) {
    c.fill_rect(Rect{0, 0, gutter_width_, height_}, gutter_bg_);
    c.fill_rect(Rect{gutter_width_ - 1, 0, 1, height_}, gutter_border_color_);
    // The numbers belong to the text viewport too: a partly scrolled last row
    // must not drop its number into the reserved strip.
    c.push_clip(Rect{0, 0, gutter_width_, text_h});
    c.set_text_color(gutter_fg_);
    for (const auto& n : numbers) {
      const Run num = tb_->shape(font_, std::to_string(n.first + 1));
      c.draw_run(num, gutter_width_ - 8 - num.width, n.second + num.ascent);
    }
    c.pop_clip();
  }

  draw_scrollbar(c);
  draw_hscrollbar(c);
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

void ViewCore::move_document_edge(bool to_end, bool extend) {
  int64_t line = 0, col = 0;
  if (to_end) {
    discover_all();
    line = known_lines_ > 0 ? known_lines_ - 1 : 0;
    col = line_len(line);
  }
  v_moving_ = false;
  set_caret_extend(line, col, extend);
}

void ViewCore::word_bounds(int64_t line, int64_t col, int64_t* begin,
                           int64_t* end) const {
  const std::string text = line_text(line);
  const int64_t n = static_cast<int64_t>(text.size());
  *begin = *end = col < 0 ? 0 : std::min(col, n);
  if (text.empty()) return;
  // The codepoint under the caret (a caret at the line end uses the last one).
  size_t at = 0;
  for (size_t pos = 0; pos < text.size(); pos += cp_len(text, pos)) {
    at = pos;
    if (static_cast<int64_t>(pos) >= col) break;
  }
  const CharClass cls = char_class(cp_at(text, at));
  size_t lo = at, hi = at + cp_len(text, at);
  while (lo > 0) {
    const size_t p = prev_cp(text, lo);
    if (char_class(cp_at(text, p)) != cls) break;
    lo = p;
  }
  while (hi < text.size()) {
    if (char_class(cp_at(text, hi)) != cls) break;
    hi += cp_len(text, hi);
  }
  *begin = static_cast<int64_t>(lo);
  *end = static_cast<int64_t>(hi);
}

void ViewCore::move_word(int dir, bool extend) {
  int64_t line = sel_.caret_line;
  int64_t col = sel_.caret_col;
  const std::string text = line_text(line);
  const int64_t n = static_cast<int64_t>(text.size());
  if (dir > 0) {
    if (col >= n) {  // line end: step onto the next line
      if (!line_exists(line + 1)) return;
      line += 1;
      col = 0;
    } else {
      while (col < n) {  // cross the space to the next run first
        if (char_class(cp_at(text, static_cast<size_t>(col))) != CharClass::Space) {
          break;
        }
        col += static_cast<int64_t>(cp_len(text, static_cast<size_t>(col)));
      }
      if (col < n) {  // then land at the end of that run (macOS' Option+Right)
        const CharClass cls =
            char_class(cp_at(text, static_cast<size_t>(col)));
        while (col < n) {
          if (char_class(cp_at(text, static_cast<size_t>(col))) != cls) break;
          col += static_cast<int64_t>(cp_len(text, static_cast<size_t>(col)));
        }
      }
    }
  } else {
    if (col <= 0) {  // line start: step onto the end of the previous line
      if (line == 0) return;
      line -= 1;
      col = line_len(line);
    } else {
      while (col > 0) {  // skip the space to the left
        const size_t p = prev_cp(text, static_cast<size_t>(col));
        if (char_class(cp_at(text, p)) != CharClass::Space) break;
        col = static_cast<int64_t>(p);
      }
      if (col > 0) {
        const CharClass cls =
            char_class(cp_at(text, prev_cp(text, static_cast<size_t>(col))));
        while (col > 0) {  // then take the whole run
          const size_t p = prev_cp(text, static_cast<size_t>(col));
          if (char_class(cp_at(text, p)) != cls) break;
          col = static_cast<int64_t>(p);
        }
      }
    }
  }
  v_moving_ = false;
  set_caret_extend(line, col, extend);
}

void ViewCore::page_move(int dir, bool extend) {
  // A page is a screenful of rows, not logical lines: under wrap one logical
  // line can be several screens tall, so counting lines would move far less
  // (or much more) than a page.
  scroll_by(dir * text_height());
  // The caret goes to the row at the leading edge of the new viewport — the top
  // row after a page down, the bottom row after a page up. Placing it at the
  // start of the first logical line would leave it scrolled above (or below) the
  // viewport, and ensure_caret_visible would scroll straight back.
  const double y = dir > 0 ? 1.0 : text_height() - 1.0;
  if (!v_moving_) {
    // Keep the horizontal position vertical motion would keep.
    const LineEntry& cur = entry_for(sel_.caret_line);
    size_t vi = 0;
    double x = 0;
    caret_within(cur, sel_.caret_col, &vi, &x);
    goal_x_ = x;
  }
  size_t vi = 0;
  const Hit h = hit_test_row(text_left(), y, &vi);
  const LineEntry& e = entry_for(h.line);
  const size_t row = vi < e.visual.size() ? vi : 0;
  sel_.caret_line = h.line;
  sel_.caret_col = static_cast<int64_t>(col_at_x(e.visual[row], goal_x_));
  v_moving_ = true;
  set_caret_extend(sel_.caret_line, sel_.caret_col, extend);
}

void ViewCore::ensure_caret_visible() {
  const int64_t line = sel_.caret_line;
  if (line < first_line_) {
    scroll_to_line(line);
  } else {
    // Use the caret's own rectangle: it is wrap-aware, so a caret on a logical
    // line whose block is several screens tall is only scrolled when the caret
    // itself is off screen, not when its line's first row is.
    const Rect cr = caret_rect(line, sel_.caret_col);
    if (cr.y < 0) {
      scroll_by(cr.y);
    } else if (cr.y + cr.h > text_height()) {
      scroll_by(cr.y + cr.h - text_height());
    }
  }
  // Keep the caret inside the text area horizontally (wrap off), clear of the
  // overlay scrollbar down the right edge.
  if (wrap_) return;
  const Rect cr = caret_rect(sel_.caret_line, sel_.caret_col);
  const double left = text_left();
  const double right = width_ - kScrollbarWidth - 2;
  if (cr.x < left) {
    scroll_x_by(cr.x - left);
  } else if (cr.x > right) {
    scroll_x_by(cr.x - right);
  }
}

bool ViewCore::key(const KeyEvent& e) {
  switch (e.key) {
    case Key::Backspace: return backspace(e.alt);
    case Key::Delete: return delete_forward(e.alt);
    case Key::Enter: return insert_text("\n");
    case Key::InsertText: return insert_text(e.text);
    default: break;
  }
  const Selection before = sel_;
  const int64_t before_first = first_line_;
  const double before_px = scroll_px_;
  switch (e.key) {
    case Key::Left:
      if (e.cmd) {
        move_line_edge(false, e.shift);  // Command+Left: line start
      } else if (e.alt) {
        move_word(-1, e.shift);  // Option+Left: previous word
      } else {
        move_horizontal(-1, e.shift);
      }
      break;
    case Key::Right:
      if (e.cmd) {
        move_line_edge(true, e.shift);
      } else if (e.alt) {
        move_word(1, e.shift);
      } else {
        move_horizontal(1, e.shift);
      }
      break;
    case Key::Up:
      if (e.cmd) {
        move_document_edge(false, e.shift);  // Command+Up: document start
      } else {
        move_vertical(-1, e.shift);
      }
      break;
    case Key::Down:
      if (e.cmd) {
        move_document_edge(true, e.shift);  // Command+Down: document end
      } else {
        move_vertical(1, e.shift);
      }
      break;
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
    default: return false;
  }
  ensure_caret_visible();
  return !(sel_ == before) || first_line_ != before_first ||
         scroll_px_ != before_px;
}

// Height of the vertical scrollbar's track (shorter when the horizontal bar is
// showing along the bottom edge).
double ViewCore::text_height() const {
  const double h = height_ - (hbar_visible() ? kScrollbarWidth : 0.0);
  const double lh = line_height();
  return h > lh ? h : lh;
}

bool ViewCore::mouse(const MouseEvent& e) {
  const Selection before = sel_;
  const int64_t before_first = first_line_;
  const double before_px = scroll_px_;
  const double before_x = x_offset_;
  const double before_f = sb_drag_f_;
  switch (e.kind) {
    case MouseEvent::Kind::Scroll:
      if (e.dy != 0) scroll_by(e.dy);
      if (e.dx != 0) scroll_x_by(e.dx);
      break;
    case MouseEvent::Kind::Down: {
      const HScrollBar hb = hscrollbar();
      if (hb.visible && e.y >= hb.track_y && e.x >= text_left()) {
        v_moving_ = false;
        sb_drag_ = false;
        const double travel = text_width() - hb.thumb_w;
        if (e.x >= hb.thumb_x && e.x < hb.thumb_x + hb.thumb_w) {
          hsb_drag_ = true;  // grab the thumb
          hsb_grab_ = e.x - hb.thumb_x;
        } else if (e.cmd && travel > 0) {
          // Command-click jumps so the thumb centres on the pointer.
          scroll_x_to_fraction(
              (e.x - text_left() - hb.thumb_w / 2) / travel);
        } else {
          // A plain track click pages toward the click.
          scroll_x_by(e.x < hb.thumb_x ? -text_width() : text_width());
        }
        break;
      }
      hsb_drag_ = false;
      const ScrollBar sb = scrollbar();
      if (sb.visible && e.x >= width_ - kScrollbarWidth) {
        v_moving_ = false;
        if (e.y >= sb.thumb_y && e.y < sb.thumb_y + sb.thumb_h) {
          sb_drag_ = true;  // grab the thumb and drag it
          sb_grab_ = e.y - sb.thumb_y;
          if (sb.approximate) {
            const double travel = text_height() - sb.thumb_h;
            sb_drag_f_ = travel > 0 ? sb.thumb_y / travel : 0;
          }
        } else {
          const double travel = text_height() - sb.thumb_h;
          if (e.cmd && travel > 0) {
            // Command-click jumps so the thumb centres on the pointer.
            scroll_to_fraction((e.y - sb.thumb_h / 2) / travel);
          } else {
            // A plain track click pages toward the click. Page in visual lines,
            // not logical ones: with wrap on, a logical line can be several
            // screens tall, so counting logical lines would jump far past a
            // page. A whole number of line heights keeps wrap-off paging
            // line-aligned, as before.
            const double lh = line_height();
            const int64_t page = std::max<int64_t>(
                1, static_cast<int64_t>(text_height() / (lh > 0 ? lh : 1)) - 1);
            const double dy = static_cast<double>(page) * lh;
            scroll_by(e.y < sb.thumb_y ? -dy : dy);
          }
        }
        break;
      }
      sb_drag_ = false;
      const Hit h = hit_test(e.x, e.y);
      v_moving_ = false;
      sel_.anchor_line = sel_.caret_line = h.line;
      sel_.anchor_col = sel_.caret_col = h.col;
      // Double click selects the word (and its punctuation run), triple click
      // the whole line; a following drag keeps that granularity.
      drag_unit_ = DragUnit::Char;
      drag_anchor_line_ = h.line;
      if (e.clicks >= 3) {
        drag_unit_ = DragUnit::Line;
        sel_.anchor_col = 0;
        sel_.caret_col = line_len(h.line);
      } else if (e.clicks == 2) {
        drag_unit_ = DragUnit::Word;
        int64_t begin = 0, end = 0;
        word_bounds(h.line, h.col, &begin, &end);
        sel_.anchor_col = begin;
        sel_.caret_col = end;
      }
      break;
    }
    case MouseEvent::Kind::Drag: {
      if (hsb_drag_) {
        const HScrollBar hb = hscrollbar();
        const double travel = text_width() - hb.thumb_w;
        if (travel > 0) {
          scroll_x_to_fraction((e.x - hsb_grab_ - text_left()) / travel);
        }
        break;
      }
      if (sb_drag_) {
        const ScrollBar sb = scrollbar();
        const double travel = text_height() - sb.thumb_h;
        if (travel > 0) {
          double f = (e.y - sb_grab_) / travel;
          if (f < 0) f = 0;
          if (f > 1) f = 1;
          if (eof_known_) {
            scroll_to_fraction(f);
          } else {
            sb_drag_f_ = f;  // unknown total: preview, commit on release
          }
        }
        break;
      }
      const Hit h = hit_test(e.x, e.y);
      if (drag_unit_ == DragUnit::Line) {
        // Grow to the whole span between the anchor line and the pointer, in
        // whichever direction the drag went.
        const int64_t lo = std::min(drag_anchor_line_, h.line);
        const int64_t hi = std::max(drag_anchor_line_, h.line);
        sel_.anchor_line = lo;
        sel_.anchor_col = 0;
        sel_.caret_line = hi;
        sel_.caret_col = line_len(hi);
        break;
      }
      if (drag_unit_ == DragUnit::Word) {
        int64_t begin = 0, end = 0;
        word_bounds(h.line, h.col, &begin, &end);
        const bool before = h.line < sel_.anchor_line ||
                            (h.line == sel_.anchor_line &&
                             h.col < sel_.anchor_col);
        sel_.caret_line = h.line;
        sel_.caret_col = before ? begin : end;
        break;
      }
      sel_.caret_line = h.line;
      sel_.caret_col = h.col;
      break;
    }
    case MouseEvent::Kind::Up:
      if (sb_drag_ && sb_drag_f_ >= 0) scroll_to_fraction(sb_drag_f_);
      sb_drag_ = false;
      sb_drag_f_ = -1;
      hsb_drag_ = false;
      break;
  }
  return !(sel_ == before) || first_line_ != before_first ||
         scroll_px_ != before_px || x_offset_ != before_x ||
         sb_drag_f_ != before_f;
}

// --- Editing (M2) ----------------------------------------------------------

bool ViewCore::has_selection() const {
  return sel_.anchor_line != sel_.caret_line ||
         sel_.anchor_col != sel_.caret_col;
}

void ViewCore::selection_range(int64_t* sl, int64_t* sc, int64_t* el,
                               int64_t* ec) const {
  const bool fwd = sel_.anchor_line < sel_.caret_line ||
                   (sel_.anchor_line == sel_.caret_line &&
                    sel_.anchor_col <= sel_.caret_col);
  *sl = fwd ? sel_.anchor_line : sel_.caret_line;
  *sc = fwd ? sel_.anchor_col : sel_.caret_col;
  *el = fwd ? sel_.caret_line : sel_.anchor_line;
  *ec = fwd ? sel_.caret_col : sel_.anchor_col;
}

int64_t ViewCore::byte_span(int64_t sl, int64_t sc, int64_t el,
                            int64_t ec) const {
  size_t block = 0;
  int64_t n = 0;
  ed_->read_lines(sl, el + 1, nullptr, 0, &block, &n);
  size_t last = 0;
  ed_->read_lines(el, el + 1, nullptr, 0, &last, &n);
  return static_cast<int64_t>(block - static_cast<size_t>(sc) -
                              (last - static_cast<size_t>(ec)));
}

void ViewCore::refresh_line_count() {
  size_t need = 0;
  int64_t n = 0;
  // read_lines does not trigger the EOL scan, and after an edit the index is
  // complete, so this is O(log n).
  ed_->read_lines(0, std::numeric_limits<int64_t>::max(), nullptr, 0, &need, &n);
  eof_known_ = true;
  known_lines_ = n;
}

void ViewCore::document_changed() {
  invalidate_all();
  // content_w_ is deliberately kept: it is a running maximum, so re-measuring
  // from zero would snap a panned view back to the left edge on every keystroke.
  // It resets when a new document replaces this one (set_document).
  pending_line_ = -1;
  refresh_line_count();
  sel_.anchor_line = sel_.caret_line;
  sel_.anchor_col = sel_.caret_col;
  clamp_scroll();  // the document may have shrunk under the viewport
  clamp_x();
  ensure_caret_visible();
}

bool ViewCore::delete_range(int64_t sl, int64_t sc, int64_t el, int64_t ec) {
  const int64_t removed = byte_span(sl, sc, el, ec);
  if (removed <= 0) return false;
  Edit e;
  e.line = sl;
  e.col = sc;
  e.removed_len = removed;
  e.text = std::string_view();
  const Status s = ed_->apply(std::span<const Edit>(&e, 1));
  if (s != Status::Ok) return false;
  sel_.caret_line = sl;
  sel_.caret_col = sc;
  document_changed();
  return true;
}

bool ViewCore::insert_text(std::string_view text) {
  if (text.empty()) return false;
  int64_t sl = 0, sc = 0, el = 0, ec = 0;
  if (has_selection()) {
    selection_range(&sl, &sc, &el, &ec);
  } else {
    sl = el = sel_.caret_line;
    sc = ec = sel_.caret_col;
  }
  const int64_t removed = byte_span(sl, sc, el, ec);
  Edit e;
  e.line = sl;
  e.col = sc;
  e.removed_len = removed;
  e.text = text;
  const Status s = ed_->apply(std::span<const Edit>(&e, 1));
  if (s != Status::Ok) return false;

  // The caret lands after the inserted text. Count terminators and the length of
  // the trailing (terminator-free) segment: normalizing to the file EOL changes
  // only the terminator bytes, not that segment.
  int64_t terms = 0;
  size_t last_term_end = 0;
  for (size_t i = 0; i < text.size(); ++i) {
    if (text[i] == '\r') {
      ++terms;
      if (i + 1 < text.size() && text[i + 1] == '\n') ++i;
      last_term_end = i + 1;
    } else if (text[i] == '\n') {
      ++terms;
      last_term_end = i + 1;
    }
  }
  sel_.caret_line = sl + terms;
  sel_.caret_col = (terms == 0 ? sc : 0) +
                   static_cast<int64_t>(text.size() - last_term_end);
  document_changed();
  return true;
}

bool ViewCore::backspace(bool by_word) {
  if (has_selection()) {
    int64_t sl = 0, sc = 0, el = 0, ec = 0;
    selection_range(&sl, &sc, &el, &ec);
    return delete_range(sl, sc, el, ec);
  }
  const int64_t line = sel_.caret_line;
  const int64_t col = sel_.caret_col;
  if (col == 0 && line == 0) return false;
  if (col > 0) {
    if (by_word) {  // Option+Delete: back to the start of the previous word
      move_word(-1, false);
      const int64_t to_line = sel_.caret_line;
      const int64_t to_col = sel_.caret_col;
      set_caret_extend(line, col, false);
      if (to_line == line) return delete_range(line, to_col, line, col);
    } else {
      const std::string t = line_text(line);
      const size_t p = prev_cp(t, static_cast<size_t>(col));
      return delete_range(line, static_cast<int64_t>(p), line, col);
    }
  }
  const int64_t prev = line - 1;
  return delete_range(prev, line_len(prev), line, 0);
}

std::string ViewCore::selected_text() const {
  if (!has_selection()) return {};
  int64_t sl = 0, sc = 0, el = 0, ec = 0;
  selection_range(&sl, &sc, &el, &ec);
  size_t block = 0;
  int64_t n = 0;
  ed_->read_lines(sl, el + 1, nullptr, 0, &block, &n);
  std::string buf(block, '\0');
  if (block > 0) {
    size_t got = 0;
    ed_->read_lines(sl, el + 1, buf.data(), buf.size(), &got, &n);
    buf.resize(got);
  }
  size_t last = 0;
  ed_->read_lines(el, el + 1, nullptr, 0, &last, &n);
  const size_t start = static_cast<size_t>(sc);
  const size_t end = buf.size() - (last - static_cast<size_t>(ec));
  if (end <= start) return {};
  return buf.substr(start, end - start);
}

bool ViewCore::delete_forward(bool by_word) {
  if (has_selection()) {
    int64_t sl = 0, sc = 0, el = 0, ec = 0;
    selection_range(&sl, &sc, &el, &ec);
    return delete_range(sl, sc, el, ec);
  }
  const int64_t line = sel_.caret_line;
  const int64_t col = sel_.caret_col;
  const std::string t = line_text(line);
  if (static_cast<size_t>(col) < t.size()) {
    if (by_word) {  // Option+Forward-Delete: through the next word
      move_word(1, false);
      const int64_t to_line = sel_.caret_line;
      const int64_t to_col = sel_.caret_col;
      set_caret_extend(line, col, false);
      if (to_line == line) return delete_range(line, col, line, to_col);
      return delete_range(line, col, to_line, to_col);
    }
    const int64_t nc = col + static_cast<int64_t>(cp_len(t, static_cast<size_t>(col)));
    return delete_range(line, col, line, nc);
  }
  if (line_exists(line + 1)) return delete_range(line, col, line + 1, 0);
  return false;
}

bool ViewCore::find(std::string_view needle, bool forward, bool case_sensitive) {
  if (needle.empty()) return false;
  refresh_line_count();
  const int64_t lc = known_lines_;
  if (lc == 0) return false;
  const int64_t cline = std::clamp<int64_t>(sel_.caret_line, 0, lc);
  const int64_t ccol = sel_.caret_col;

  auto select = [&](int64_t line, size_t pos) {
    sel_.anchor_line = line;
    sel_.anchor_col = static_cast<int64_t>(pos);
    sel_.caret_line = line;
    sel_.caret_col = static_cast<int64_t>(pos + needle.size());
    v_moving_ = false;
    ensure_caret_visible();
  };

  if (forward) {
    for (int64_t line = cline; line < lc; ++line) {
      const std::string t = line_text(line);
      const size_t from =
          (line == cline && ccol > 0) ? static_cast<size_t>(ccol) : 0;
      const size_t p = find_in(t, needle, from, case_sensitive);
      if (p != std::string_view::npos) {
        select(line, p);
        return true;
      }
    }
    for (int64_t line = 0; line <= cline && line < lc; ++line) {
      const std::string t = line_text(line);
      const size_t p = find_in(t, needle, 0, case_sensitive);
      if (p != std::string_view::npos) {
        select(line, p);
        return true;
      }
    }
    return false;
  }

  auto last_in = [&](const std::string& t, size_t limit, size_t* out) {
    size_t best = std::string_view::npos;
    size_t i = find_in(t, needle, 0, case_sensitive);
    while (i != std::string_view::npos && i + needle.size() <= limit) {
      best = i;
      i = find_in(t, needle, i + 1, case_sensitive);
    }
    if (best == std::string_view::npos) return false;
    *out = best;
    return true;
  };
  for (int64_t line = cline; line >= 0; --line) {
    const std::string t = line_text(line);
    const size_t limit =
        (line == cline) ? static_cast<size_t>(ccol) : t.size();
    size_t p = 0;
    if (last_in(t, limit, &p)) {
      select(line, p);
      return true;
    }
  }
  for (int64_t line = lc - 1; line > cline; --line) {
    const std::string t = line_text(line);
    size_t p = 0;
    if (last_in(t, t.size(), &p)) {
      select(line, p);
      return true;
    }
  }
  return false;
}

bool ViewCore::replace_current(std::string_view needle,
                               std::string_view replacement,
                               bool case_sensitive) {
  if (needle.empty()) return false;
  bool replaced = false;
  if (has_selection()) {
    int64_t sl = 0, sc = 0, el = 0, ec = 0;
    selection_range(&sl, &sc, &el, &ec);
    if (sl == el) {
      const std::string t = line_text(sl);
      const std::string_view sel =
          std::string_view(t).substr(static_cast<size_t>(sc),
                                     static_cast<size_t>(ec - sc));
      if (sel.size() == needle.size() &&
          match_at(sel, 0, needle, case_sensitive)) {
        insert_text(replacement);
        replaced = true;
      }
    }
  }
  find(needle, true, case_sensitive);
  return replaced;
}

int64_t ViewCore::replace_all(std::string_view needle,
                              std::string_view replacement,
                              bool case_sensitive) {
  if (needle.empty()) return 0;
  refresh_line_count();
  const int64_t lc = known_lines_;
  std::vector<Edit> edits;
  for (int64_t line = 0; line < lc; ++line) {
    const std::string t = line_text(line);
    size_t i = find_in(t, needle, 0, case_sensitive);
    while (i != std::string_view::npos) {
      Edit e;
      e.line = line;
      e.col = static_cast<int64_t>(i);
      e.removed_len = static_cast<int64_t>(needle.size());
      e.text = replacement;
      edits.push_back(e);
      i = find_in(t, needle, i + needle.size(), case_sensitive);
    }
  }
  if (edits.empty()) return 0;
  const Status s = ed_->apply(std::span<const Edit>(edits.data(), edits.size()));
  if (s != Status::Ok) return 0;
  document_changed();
  return static_cast<int64_t>(edits.size());
}

}  // namespace vc
