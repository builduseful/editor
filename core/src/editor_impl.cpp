#include "editor_impl.hpp"

#include <algorithm>
#include <climits>
#include <cstdint>
#include <string>
#include <utility>

#include "line_stream.hpp"

namespace ed {

namespace {

std::string dirname_of(std::string_view p) {
  const size_t pos = p.find_last_of('/');
  if (pos == std::string_view::npos) return ".";
  if (pos == 0) return "/";
  return std::string(p.substr(0, pos));
}

std::string basename_of(std::string_view p) {
  const size_t pos = p.find_last_of('/');
  return std::string(pos == std::string_view::npos ? p : p.substr(pos + 1));
}

bool is_continuation(char c) {
  return (static_cast<unsigned char>(c) & 0xC0) == 0x80;
}

bool valid_utf8(std::string_view s) {
  size_t i = 0;
  const size_t n = s.size();
  while (i < n) {
    const unsigned char c = static_cast<unsigned char>(s[i]);
    size_t extra = 0;
    unsigned int cp = 0;
    if (c < 0x80) {
      i += 1;
      continue;
    } else if ((c & 0xE0) == 0xC0) {
      extra = 1;
      cp = c & 0x1Fu;
    } else if ((c & 0xF0) == 0xE0) {
      extra = 2;
      cp = c & 0x0Fu;
    } else if ((c & 0xF8) == 0xF0) {
      extra = 3;
      cp = c & 0x07u;
    } else {
      return false;
    }
    if (i + extra >= n) return false;
    for (size_t k = 1; k <= extra; ++k) {
      const unsigned char cc = static_cast<unsigned char>(s[i + k]);
      if ((cc & 0xC0) != 0x80) return false;
      cp = (cp << 6) | (cc & 0x3Fu);
    }
    if (extra == 1 && cp < 0x80) return false;
    if (extra == 2 && cp < 0x800) return false;
    if (extra == 3 && cp < 0x10000) return false;
    if (cp > 0x10FFFF) return false;
    if (cp >= 0xD800 && cp <= 0xDFFF) return false;
    i += extra + 1;
  }
  return true;
}

std::string normalize_newlines(std::string_view s, Eol eol) {
  const char* rep = eol == Eol::CrLf ? "\r\n" : (eol == Eol::Cr ? "\r" : "\n");
  const size_t replen = eol == Eol::CrLf ? 2 : 1;
  std::string out;
  out.reserve(s.size());
  for (size_t i = 0; i < s.size(); ++i) {
    const char c = s[i];
    if (c == '\r') {
      out.append(rep, replen);
      if (i + 1 < s.size() && s[i + 1] == '\n') i += 1;
    } else if (c == '\n') {
      out.append(rep, replen);
    } else {
      out.push_back(c);
    }
  }
  return out;
}

bool rev_equal(const Rev& a, const Rev& b) {
  return a.dev == b.dev && a.ino == b.ino && a.size == b.size &&
         a.mtime_ns == b.mtime_ns && a.ctime_ns == b.ctime_ns;
}

}  // namespace

EditorImpl::EditorImpl(FileSystem* fs) : fs_(fs) {}
EditorImpl::~EditorImpl() = default;

Status EditorImpl::fail(Status s, std::string msg) const {
  errmsg_ = std::move(msg);
  return s;
}

Status EditorImpl::install_fd(int fd, const Stat& st, const std::string& path) {
  base_fd_.adopt(fs_, fd);
  base_src_.reset(fs_, fd, st.size);
  base_size_ = st.size;
  path_ = path;
  dir_ = dirname_of(path);
  rev_ = Rev{st.dev, st.ino, st.size, st.mtime_ns, st.ctime_ns};
  add_.reset();
  add_.init(fs_, dir_);
  pieces_.reset(&base_src_, &add_, st.size);
  logical_src_.reset(&pieces_, bom_len_);
  index_built_ = false;
  eol_known_ = false;
  dirty_ = false;
  opened_ = true;
  needs_reload_ = false;
  return Status::Ok;
}

Status EditorImpl::open(std::string_view p) {
  int fd = -1;
  Status s = fs_->open_readonly(p, true, &fd);
  if (s != Status::Ok) return fail(s, "cannot open");
  Stat st;
  s = fs_->fstat(fd, &st);
  if (s != Status::Ok) {
    fs_->close_fd(fd);
    return fail(s, "fstat failed");
  }
  if (!st.regular) {
    fs_->close_fd(fd);
    return fail(Status::Io, "not a regular file");
  }
  char head[8192];
  size_t got = 0;
  s = fs_->pread(fd, 0, head, sizeof(head), &got);
  if (s != Status::Ok) {
    fs_->close_fd(fd);
    return fail(s, "read failed");
  }
  uint64_t bom = 0;
  if (got >= 3 && static_cast<unsigned char>(head[0]) == 0xEF &&
      static_cast<unsigned char>(head[1]) == 0xBB &&
      static_cast<unsigned char>(head[2]) == 0xBF) {
    bom = 3;
  } else if (got >= 2 &&
             ((static_cast<unsigned char>(head[0]) == 0xFF &&
               static_cast<unsigned char>(head[1]) == 0xFE) ||
              (static_cast<unsigned char>(head[0]) == 0xFE &&
               static_cast<unsigned char>(head[1]) == 0xFF))) {
    fs_->close_fd(fd);
    return fail(Status::Unsupported, "UTF-16 BOM");
  }
  for (size_t i = 0; i < got; ++i) {
    if (head[i] == '\0') {
      fs_->close_fd(fd);
      return fail(Status::Binary, "NUL byte in first 8 KB");
    }
  }
  std::string real(p);
  std::string rp;
  if (fs_->resolve(p, &rp) == Status::Ok) real = rp;
  bom_len_ = bom;
  return install_fd(fd, st, real);
}

Status EditorImpl::create(std::string_view p) {
  if (p.empty() || p[0] != '/') return fail(Status::Range, "path must be absolute");
  const std::string parent = dirname_of(p);
  const std::string name = basename_of(p);
  std::string real(p);
  std::string rp;
  if (fs_->resolve(parent, &rp) == Status::Ok) real = rp + "/" + name;
  int fd = -1;
  Status s = fs_->open_write(real, true, false, true, &fd);
  if (s != Status::Ok) return fail(s, "cannot create");
  Stat st;
  s = fs_->fstat(fd, &st);
  if (s != Status::Ok) {
    fs_->close_fd(fd);
    return fail(s, "fstat failed");
  }
  bom_len_ = 0;
  return install_fd(fd, st, real);
}

Status EditorImpl::ensure_index() const {
  if (index_built_) return Status::Ok;
  Status s = index_.build(logical_src_);
  if (s != Status::Ok) return s;
  index_built_ = true;
  return Status::Ok;
}

Status EditorImpl::ensure_eol() const {
  if (eol_known_) return Status::Ok;
  eol_ = detect_eol();
  eol_known_ = true;
  return Status::Ok;
}

Eol EditorImpl::detect_eol() const {
  uint64_t lf = 0, crlf = 0, cr = 0;
  Eol first_seen = Eol::Lf;
  bool have_first = false;
  const uint64_t n = logical_src_.size();
  char buf[65536];
  uint64_t pos = 0;
  bool prev_cr = false;
  while (pos < n) {
    const uint64_t want = std::min<uint64_t>(sizeof(buf), n - pos);
    size_t got = 0;
    if (logical_src_.read(pos, want, buf, sizeof(buf), &got) != Status::Ok) break;
    if (got == 0) break;
    for (size_t i = 0; i < got; ++i) {
      const char c = buf[i];
      if (c == '\n') {
        if (prev_cr) {
          crlf += 1;
          if (!have_first) {
            first_seen = Eol::CrLf;
            have_first = true;
          }
          prev_cr = false;
        } else {
          lf += 1;
          if (!have_first) {
            first_seen = Eol::Lf;
            have_first = true;
          }
        }
      } else if (c == '\r') {
        if (prev_cr) {
          cr += 1;
          if (!have_first) {
            first_seen = Eol::Cr;
            have_first = true;
          }
        }
        prev_cr = true;
      } else {
        if (prev_cr) {
          cr += 1;
          if (!have_first) {
            first_seen = Eol::Cr;
            have_first = true;
          }
          prev_cr = false;
        }
      }
    }
    pos += got;
  }
  if (prev_cr) {
    cr += 1;
    if (!have_first) {
      first_seen = Eol::Cr;
      have_first = true;
    }
  }
  if (!have_first) return Eol::Lf;
  const uint64_t mx = std::max(lf, std::max(crlf, cr));
  const int winners = (lf == mx ? 1 : 0) + (crlf == mx ? 1 : 0) + (cr == mx ? 1 : 0);
  if (winners == 1) {
    if (lf == mx) return Eol::Lf;
    if (crlf == mx) return Eol::CrLf;
    return Eol::Cr;
  }
  return first_seen;  // tie -> first seen
}

bool EditorImpl::final_newline() const {
  const uint64_t n = logical_src_.size();
  if (n == 0) return false;
  char c = 0;
  size_t g = 0;
  logical_src_.read(n - 1, 1, &c, 1, &g);
  return c == '\n' || c == '\r';
}

Status EditorImpl::line_content_len(int64_t line, uint64_t* out) const {
  uint64_t a = 0, b = 0;
  Status s = index_.line_start(logical_src_, line, &a);
  if (s != Status::Ok) return s;
  s = index_.line_start(logical_src_, line + 1, &b);
  if (s != Status::Ok) return s;
  uint64_t len = b - a;
  if (len == 0) {
    *out = 0;
    return Status::Ok;
  }
  const uint64_t start = len >= 2 ? b - 2 : a;
  char t[2] = {0, 0};
  size_t g = 0;
  s = logical_src_.read(start, b - start, t, sizeof(t), &g);
  if (s != Status::Ok) return s;
  if (g >= 1 && t[g - 1] == '\n') len -= 1;
  if (g >= 2 && t[g - 2] == '\r' && t[g - 1] == '\n') {
    len -= 1;
  } else if (g >= 1 && t[g - 1] == '\r') {
    len -= 1;
  }
  *out = len;
  return Status::Ok;
}

Status EditorImpl::line_of_byte(uint64_t off, int64_t* out) const {
  const uint64_t n = index_.byte_count();
  if (off >= n) {
    *out = index_.line_count();
    return Status::Ok;
  }
  LineStream ls(logical_src_, 0);
  uint64_t pos = 0;
  int64_t line = 0;
  while (pos <= off) {
    bool has = false;
    uint64_t lb = 0;
    bool term = false;
    Status s = ls.next(&has, &lb, &term);
    if (s != Status::Ok) return s;
    if (!has) break;
    if (pos + lb > off) {
      *out = line;
      return Status::Ok;
    }
    pos += lb;
    line += 1;
  }
  *out = line;
  return Status::Ok;
}

Status EditorImpl::logical_offset(int64_t line, int64_t col, uint64_t* out) const {
  if (line < 0 || col < 0) return Status::Range;
  const int64_t lc = index_.line_count();
  if (line > lc) return Status::Range;
  uint64_t start = 0;
  Status s = index_.line_start(logical_src_, line, &start);
  if (s != Status::Ok) return s;
  if (line == lc) {
    if (col != 0) return Status::Range;
    *out = start;
    return Status::Ok;
  }
  uint64_t clen = 0;
  s = line_content_len(line, &clen);
  if (s != Status::Ok) return s;
  if (static_cast<uint64_t>(col) > clen) return Status::Range;
  *out = start + static_cast<uint64_t>(col);
  return Status::Ok;
}

Status EditorImpl::info(Info* out) const {
  if (!opened_) return fail(Status::Io, "no document");
  Status s = ensure_index();
  if (s != Status::Ok) return fail(s, "index");
  s = ensure_eol();
  if (s != Status::Ok) return fail(s, "eol");
  out->path = path_;
  out->bytes = base_size_;
  out->line_count = index_.line_count();
  out->eol = eol_;
  out->encoding = Encoding::Utf8;
  out->bom = bom_len_ > 0;
  out->final_newline = final_newline();
  out->dirty = dirty_;
  out->rev = rev_;
  if (needs_reload_) {
    out->rev = Rev{};
    out->bytes = 0;
  }
  return Status::Ok;
}

Rev EditorImpl::rev() const { return needs_reload_ ? Rev{} : rev_; }

Status EditorImpl::read_lines(int64_t first, int64_t last, char* buf, size_t cap,
                              size_t* used, int64_t* out_n) const {
  *used = 0;
  *out_n = 0;
  if (first < 0 || last < 0) return fail(Status::Range, "negative line");
  Status s = ensure_index();
  if (s != Status::Ok) return fail(s, "index");
  const int64_t lc = index_.line_count();
  if (first >= last || first >= lc || last == 0) return Status::Ok;
  if (last > lc) last = lc;
  uint64_t a = 0, b = 0;
  s = index_.line_start(logical_src_, first, &a);
  if (s != Status::Ok) return fail(s, "line_start");
  s = index_.line_start(logical_src_, last, &b);
  if (s != Status::Ok) return fail(s, "line_start");
  const uint64_t need = b - a;
  const int64_t n = last - first;
  if (buf == nullptr) {
    *used = need;
    *out_n = n;
    return Status::Ok;
  }
  if (cap < need) {
    *used = need;
    *out_n = 0;
    return Status::BufferTooSmall;
  }
  size_t got = 0;
  s = logical_src_.read(a, need, buf, cap, &got);
  if (s != Status::Ok) return fail(s, "read");
  *used = got;
  *out_n = n;
  return Status::Ok;
}

Status EditorImpl::read_range(int64_t off, size_t len, char* buf, size_t cap,
                              size_t* used) const {
  *used = 0;
  if (off < 0) return fail(Status::Range, "negative offset");
  if (needs_reload_) return fail(Status::Io, "needs reload");
  if (!base_fd_.valid()) return fail(Status::Io, "no base");
  if (static_cast<uint64_t>(off) >= base_size_) return Status::Ok;
  const uint64_t n =
      std::min<uint64_t>(len, base_size_ - static_cast<uint64_t>(off));
  if (buf == nullptr) {
    *used = static_cast<size_t>(n);
    return Status::Ok;
  }
  if (cap < n) {
    *used = static_cast<size_t>(n);
    return Status::BufferTooSmall;
  }
  size_t got = 0;
  Status s = fs_->pread(base_fd_.get(), static_cast<uint64_t>(off), buf, cap, &got);
  if (s != Status::Ok) return fail(s, "pread");
  *used = got;
  return Status::Ok;
}

Status EditorImpl::apply(std::span<const Edit> edits) {
  if (needs_reload_) return fail(Status::Stale, "needs reload");
  if (edits.empty()) return Status::Ok;
  Status s = ensure_index();
  if (s != Status::Ok) return fail(s, "index");
  const int64_t lc = index_.line_count();
  const uint64_t doc_bytes = index_.byte_count();

  for (const Edit& e : edits) {
    if (e.line < 0 || e.col < 0 || e.removed_len < 0) {
      return fail(Status::Range, "negative range");
    }
  }

  struct Plan {
    uint64_t off = 0;
    uint64_t removed = 0;
    std::string text;
    int64_t pfirst = 0;
    int64_t plast = 0;
    bool virt = false;
  };
  std::vector<Plan> plan;
  plan.reserve(edits.size());
  for (const Edit& e : edits) {
    if (e.line > lc) return fail(Status::Range, "line out of range");
    uint64_t off = 0;
    s = logical_offset(e.line, e.col, &off);
    if (s != Status::Ok) return fail(s, "column out of range");
    if (off + static_cast<uint64_t>(e.removed_len) > doc_bytes) {
      return fail(Status::Range, "removed range past EOF");
    }
    Plan p;
    p.off = off;
    p.removed = static_cast<uint64_t>(e.removed_len);
    p.text.assign(e.text.data(), e.text.size());
    plan.push_back(std::move(p));
  }

  for (size_t i = 1; i < plan.size(); ++i) {
    if (plan[i].off <= plan[i - 1].off) return fail(Status::Range, "unsorted or overlapping edits");
    if (plan[i - 1].off + plan[i - 1].removed > plan[i].off) {
      return fail(Status::Range, "overlapping edits");
    }
  }

  for (const Plan& p : plan) {
    if (!valid_utf8(p.text)) return fail(Status::InvalidUtf8, "invalid UTF-8 in inserted text");
  }
  for (const Plan& p : plan) {
    char c = 0;
    size_t g = 0;
    if (p.off < doc_bytes) {
      s = logical_src_.read(p.off, 1, &c, 1, &g);
      if (s != Status::Ok) return fail(s, "read");
      if (is_continuation(c)) return fail(Status::Range, "edit splits a codepoint");
    }
    const uint64_t end = p.off + p.removed;
    if (end < doc_bytes) {
      s = logical_src_.read(end, 1, &c, 1, &g);
      if (s != Status::Ok) return fail(s, "read");
      if (is_continuation(c)) return fail(Status::Range, "edit splits a codepoint");
    }
  }

  bool effective = false;
  for (const Edit& e : edits) {
    if (!e.text.empty() || e.removed_len > 0) {
      effective = true;
      break;
    }
  }
  if (!effective) return Status::Ok;

  s = ensure_eol();
  if (s != Status::Ok) return fail(s, "eol");
  const int64_t before_lines = lc;
  const bool ends_with_term = final_newline();
  int64_t first = INT64_MAX;
  int64_t last = 0;
  for (Plan& p : plan) {
    int64_t pf = 0;
    bool virt = false;
    if (p.off < doc_bytes) {
      s = line_of_byte(p.off, &pf);
      if (s != Status::Ok) return fail(s, "line_of_byte");
    } else if (doc_bytes == 0 || ends_with_term) {
      pf = lc;
      virt = true;
    } else {
      pf = lc - 1;
    }
    p.pfirst = pf;
    p.virt = virt;
    if (p.removed > 0) {
      int64_t le = 0;
      s = line_of_byte(p.off + p.removed - 1, &le);
      if (s != Status::Ok) return fail(s, "line_of_byte");
      p.plast = le + 1;
    } else {
      p.plast = virt ? lc : pf + 1;
    }
    first = std::min(first, p.pfirst);
    last = std::max(last, p.plast);
  }
  if (first == INT64_MAX) first = 0;

  uint64_t delta = 0;
  for (Plan& p : plan) {
    const std::string text = normalize_newlines(p.text, eol_);
    uint64_t add_off = 0;
    s = add_.append(text.data(), text.size(), &add_off);
    if (s != Status::Ok) return fail(s, "add buffer");
    const uint64_t at = p.off + delta + bom_len_;
    if (p.removed > 0) pieces_.remove(at, p.removed);
    if (!text.empty()) pieces_.insert(at, add_off, text.size());
    s = index_.apply_edit(logical_src_, p.off + delta, p.removed, text.size());
    if (s != Status::Ok) return fail(s, "line index");
    delta += text.size() - p.removed;
    eol_known_ = false;
  }

  dirty_ = !pieces_.pristine();

  const int64_t after_lines = index_.line_count();
  Change ch;
  ch.first = first;
  ch.last = last;
  ch.new_count = (last - first) + (after_lines - before_lines);
  bool ins = false, del = false;
  for (const Edit& e : edits) {
    if (!e.text.empty()) ins = true;
    if (e.removed_len > 0) del = true;
  }
  ch.kind = (ins && del) ? ChangeKind::Replace
                         : (ins ? ChangeKind::Insert : ChangeKind::Delete);
  emit(ch);
  return Status::Ok;
}

void EditorImpl::emit(const Change& c) { pending_.push_back(c); }

Status EditorImpl::undo() { return fail(Status::Unsupported, "undo is M0b"); }
Status EditorImpl::redo() { return fail(Status::Unsupported, "redo is M0b"); }

Status EditorImpl::flatten_after_save(uint64_t new_size) {
  add_.reset();
  add_.init(fs_, dir_);
  base_size_ = new_size;
  pieces_.reset(&base_src_, &add_, new_size);
  logical_src_.reset(&pieces_, bom_len_);
  eol_known_ = false;
  dirty_ = false;
  return Status::Ok;
}

Status EditorImpl::do_save(const Rev* expected_rev, bool in_place, SaveInfo* out) {
  if (needs_reload_) return fail(Status::Stale, "needs reload");
  if (!opened_) return fail(Status::Io, "no document");

  SaveInfo si;
  si.in_place = in_place;

  Stat cur;
  Status s = fs_->lstat(path_, &cur);
  if (s != Status::Ok) return fail(Status::Conflict, "source missing");
  if (!cur.regular || cur.dev != rev_.dev || cur.ino != rev_.ino) {
    return fail(Status::Conflict, "source identity changed");
  }
  Stat bst;
  s = fs_->fstat(base_fd_.get(), &bst);
  if (s != Status::Ok) return fail(s, "fstat base");
  if (bst.size != rev_.size || bst.mtime_ns != rev_.mtime_ns ||
      bst.ctime_ns != rev_.ctime_ns) {
    return fail(Status::Conflict, "base changed");
  }
  if (expected_rev != nullptr && !rev_equal(*expected_rev, rev_)) {
    return fail(Status::Conflict, "expected rev mismatch");
  }

  if (pieces_.pristine()) {
    si.method = "none";
    if (out != nullptr) *out = si;
    return Status::Ok;
  }

  // Changed regions of the new file, derived from the piece structure.
  std::vector<std::pair<uint64_t, uint64_t>> spans;
  uint64_t expected_base = 0, doc_pos = 0;
  bool open_span = false;
  uint64_t os = 0, oe = 0;
  auto flush = [&]() {
    if (open_span) {
      spans.push_back({os, oe});
      open_span = false;
    }
  };
  for (const Piece& p : pieces_.pieces()) {
    if (p.src == Piece::Src::Base) {
      if (p.off == expected_base) {
        flush();
      } else if (p.off > expected_base) {
        spans.push_back({doc_pos, doc_pos});  // deletion point
        flush();
      }
      expected_base = p.off + p.len;
      doc_pos += p.len;
    } else {
      if (!open_span) {
        open_span = true;
        os = doc_pos;
      }
      oe = doc_pos + p.len;
      doc_pos += p.len;
    }
  }
  flush();

  uint64_t fc = 0, lc2 = 0;
  bool any = false;
  for (const auto& sp : spans) {
    if (!any) {
      fc = sp.first;
      lc2 = sp.second;
      any = true;
    } else {
      fc = std::min(fc, sp.first);
      lc2 = std::max(lc2, sp.second);
    }
  }
  (void)any;

  const uint64_t new_size = pieces_.size();
  const bool new_file = base_size_ == 0;
  std::string method;
  if (new_file) {
    method = "full_rewrite";
  } else if (new_size == base_size_) {
    method = "clone_pwrite";
  } else if ((new_size - lc2) * 4 <= new_size) {
    method = "clone_pwrite_tail";
  } else {
    method = "full_rewrite";
  }

  auto write_doc = [&](int fd, uint64_t off, uint64_t len) -> Status {
    char buf[65536];
    uint64_t done = 0;
    while (done < len) {
      const uint64_t want = std::min<uint64_t>(sizeof(buf), len - done);
      size_t got = 0;
      Status st = pieces_.read(off + done, want, buf, sizeof(buf), &got);
      if (st != Status::Ok) return st;
      if (got == 0) return Status::Internal;
      size_t w = 0;
      st = fs_->pwrite(fd, off + done, buf, got, &w);
      if (st != Status::Ok) return st;
      if (w != got) return Status::Io;
      done += got;
    }
    return Status::Ok;
  };
  auto write_base = [&](int fd, uint64_t base_off, uint64_t dst_off,
                        uint64_t len) -> Status {
    char buf[65536];
    uint64_t done = 0;
    while (done < len) {
      const uint64_t want = std::min<uint64_t>(sizeof(buf), len - done);
      size_t got = 0;
      Status st = fs_->pread(base_fd_.get(), base_off + done, buf, want, &got);
      if (st != Status::Ok) return st;
      if (got == 0) return Status::Internal;
      size_t w = 0;
      st = fs_->pwrite(fd, dst_off + done, buf, got, &w);
      if (st != Status::Ok) return st;
      if (w != got) return Status::Io;
      done += got;
    }
    return Status::Ok;
  };

  bool used_clone = false;
  int target_fd = -1;
  std::string tmp_path;
  int dirfd = -1;
  bool have_tmp = false;

  if (method == "clone_pwrite" || method == "clone_pwrite_tail") {
    if (in_place) {
      s = fs_->open_write(path_, false, false, true, &target_fd);
      if (s != Status::Ok) return fail(s, "in-place open");
      used_clone = true;
    } else {
      s = fs_->clone_to_new(dir_, &tmp_path, base_fd_.get(), &target_fd);
      if (s == Status::Ok) {
        used_clone = true;
        have_tmp = true;
      } else {
        method = "full_rewrite";  // clone unavailable -> full rewrite (M0a)
      }
    }
  }

  if (!used_clone) {
    method = "full_rewrite";
    if (in_place) {
      s = fs_->open_write(path_, false, false, true, &target_fd);
      if (s != Status::Ok) return fail(s, "in-place open");
    } else {
      s = fs_->create_temp(dir_, &tmp_path, &target_fd);
      if (s != Status::Ok) return fail(s, "temp");
      have_tmp = true;
      fs_->copy_metadata(base_fd_.get(), target_fd);
    }
    s = write_doc(target_fd, 0, new_size);
    if (s != Status::Ok) {
      if (have_tmp) {
        fs_->close_fd(target_fd);
        fs_->unlink(tmp_path);
      }
      return fail(s, "write");
    }
    if (new_size < base_size_) fs_->ftruncate(target_fd, new_size);
    si.bytes_written = new_size;
  } else {
    uint64_t written = 0;
    for (const auto& sp : spans) {
      if (sp.second <= sp.first) continue;
      s = write_doc(target_fd, sp.first, sp.second - sp.first);
      if (s != Status::Ok) return fail(s, "write span");
      written += sp.second - sp.first;
    }
    if (method == "clone_pwrite_tail") {
      const uint64_t tail_len = new_size - lc2;
      const uint64_t base_tail_off = base_size_ - tail_len;
      s = write_base(target_fd, base_tail_off, lc2, tail_len);
      if (s != Status::Ok) return fail(s, "write tail");
      written += tail_len;
    }
    if (new_size != base_size_) fs_->ftruncate(target_fd, new_size);
    si.bytes_written = written;
  }

  si.method = method == "clone_pwrite"      ? "clone_pwrite"
              : method == "clone_pwrite_tail" ? "clone_pwrite_tail"
              : method == "copy_range"         ? "copy_range"
                                               : "full_rewrite";
  si.first_changed = fc;
  si.last_changed = lc2;

  s = fs_->fsync(target_fd, false);
  if (s != Status::Ok) {
    if (have_tmp) {
      fs_->close_fd(target_fd);
      fs_->unlink(tmp_path);
    }
    return fail(s, "fsync");
  }

  if (have_tmp) {
    s = fs_->open_dir(dir_, &dirfd);
    if (s != Status::Ok) {
      fs_->close_fd(target_fd);
      fs_->unlink(tmp_path);
      return fail(s, "open dir");
    }
    s = fs_->rename_at(dirfd, basename_of(path_), tmp_path);
    fs_->close_fd(dirfd);
    if (s != Status::Ok) {
      fs_->close_fd(target_fd);
      fs_->unlink(tmp_path);
      return fail(s, "rename");
    }
  }
  fs_->close_fd(target_fd);

  int newfd = -1;
  s = fs_->open_readonly(path_, true, &newfd);
  if (s != Status::Ok) {
    needs_reload_ = true;
    return fail(Status::Io, "post-save reopen failed");
  }
  Stat nst;
  s = fs_->fstat(newfd, &nst);
  if (s != Status::Ok || nst.size != new_size) {
    fs_->close_fd(newfd);
    needs_reload_ = true;
    return fail(Status::Conflict, "post-save identity mismatch");
  }
  base_fd_.adopt(fs_, newfd);
  base_src_.reset(fs_, newfd, nst.size);
  rev_ = Rev{nst.dev, nst.ino, nst.size, nst.mtime_ns, nst.ctime_ns};
  flatten_after_save(new_size);

  if (out != nullptr) *out = si;
  return Status::Ok;
}

Status EditorImpl::save(const Rev* expected_rev, bool in_place, SaveInfo* out) {
  return do_save(expected_rev, in_place, out);
}

Status EditorImpl::save_as(std::string_view /*p*/, const Rev* /*expected_rev*/,
                           bool /*in_place*/, SaveInfo* /*out*/) {
  return fail(Status::Unsupported, "save_as is M0b");
}

Status EditorImpl::reload() {
  if (!opened_) return fail(Status::Io, "no document");
  int fd = -1;
  Status s = fs_->open_readonly(path_, true, &fd);
  if (s != Status::Ok) return fail(s, "reload open");
  Stat st;
  s = fs_->fstat(fd, &st);
  if (s != Status::Ok) {
    fs_->close_fd(fd);
    return fail(s, "reload fstat");
  }
  if (rev_equal(Rev{st.dev, st.ino, st.size, st.mtime_ns, st.ctime_ns}, rev_) &&
      !needs_reload_) {
    fs_->close_fd(fd);
    return Status::Ok;  // already current: no-op, keeps undo
  }
  base_fd_.adopt(fs_, fd);
  base_src_.reset(fs_, fd, st.size);
  rev_ = Rev{st.dev, st.ino, st.size, st.mtime_ns, st.ctime_ns};
  add_.reset();
  add_.init(fs_, dir_);
  pieces_.reset(&base_src_, &add_, st.size);
  logical_src_.reset(&pieces_, bom_len_);
  base_size_ = st.size;
  index_built_ = false;
  eol_known_ = false;
  dirty_ = false;
  needs_reload_ = false;
  Change ch;
  ch.first = 0;
  ch.last = 0;
  ch.new_count = 0;
  ch.kind = ChangeKind::Reload;
  emit(ch);
  return Status::Ok;
}

Status EditorImpl::check_identity() {
  if (!opened_) return fail(Status::Io, "no document");
  Stat cur;
  Status s = fs_->lstat(path_, &cur);
  if (s != Status::Ok) return fail(Status::Conflict, "source missing");
  const Rev now{cur.dev, cur.ino, cur.size, cur.mtime_ns, cur.ctime_ns};
  if (rev_equal(now, rev_)) return Status::Ok;
  if (!dirty_) return reload();
  return fail(Status::Stale, "external change while dirty");
}

uint64_t EditorImpl::on_change(ChangeFn fn, void* ud) {
  const uint64_t id = next_cb_id_++;
  cbs_.push_back(Cb{id, fn, ud});
  return id;
}

void EditorImpl::off_change(uint64_t id) {
  for (auto it = cbs_.begin(); it != cbs_.end(); ++it) {
    if (it->id == id) {
      cbs_.erase(it);
      return;
    }
  }
}

Status EditorImpl::drain() {
  std::vector<Change> c;
  c.swap(pending_);
  for (const Change& ch : c) {
    for (const Cb& cb : cbs_) {
      try {
        cb.fn(this, ch, cb.ud);
      } catch (...) {
        return Status::Internal;
      }
    }
  }
  return Status::Ok;
}

std::string_view EditorImpl::errmsg() const { return errmsg_; }

}  // namespace ed
