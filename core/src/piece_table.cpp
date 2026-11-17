#include "piece_table.hpp"

#include <algorithm>

namespace ed {

void PieceTable::reset(const Source* base, const Source* add,
                       uint64_t base_size) {
  base_ = base;
  add_ = add;
  base_size_ = base_size;
  pieces_.clear();
  pieces_.push_back(Piece{Piece::Src::Base, 0, base_size});
  doc_size_ = base_size;
}

bool PieceTable::pristine() const {
  return pieces_.size() == 1 && pieces_[0].src == Piece::Src::Base &&
         pieces_[0].off == 0 && pieces_[0].len == base_size_;
}

size_t PieceTable::split_at(uint64_t off) {
  uint64_t acc = 0;
  for (size_t i = 0; i < pieces_.size(); ++i) {
    Piece& p = pieces_[i];
    if (off == acc) return i;
    if (off < acc + p.len) {
      const uint64_t intra = off - acc;
      Piece tail{p.src, p.off + intra, p.len - intra};
      p.len = intra;
      pieces_.insert(pieces_.begin() + static_cast<std::ptrdiff_t>(i) + 1, tail);
      return i + 1;
    }
    acc += p.len;
  }
  return pieces_.size();  // off == doc_size_
}

void PieceTable::insert(uint64_t off, uint64_t add_off, uint64_t len) {
  if (len == 0) return;
  const size_t idx = split_at(off);
  if (idx > 0 && pieces_[idx - 1].src == Piece::Src::Add &&
      pieces_[idx - 1].off + pieces_[idx - 1].len == add_off) {
    pieces_[idx - 1].len += len;  // coalesce at the add-buffer tail
  } else {
    pieces_.insert(pieces_.begin() + static_cast<std::ptrdiff_t>(idx),
                   Piece{Piece::Src::Add, add_off, len});
  }
  doc_size_ += len;
  coalesce();
}

void PieceTable::coalesce() {
  std::vector<Piece> out;
  out.reserve(pieces_.size());
  for (const Piece& p : pieces_) {
    if (p.len == 0) continue;
    if (!out.empty() && out.back().src == p.src &&
        out.back().off + out.back().len == p.off) {
      out.back().len += p.len;
    } else {
      out.push_back(p);
    }
  }
  if (out.empty()) out.push_back(Piece{Piece::Src::Base, 0, 0});
  pieces_.swap(out);
}

void PieceTable::remove(uint64_t off, uint64_t len) {
  if (len == 0) return;
  const size_t a = split_at(off);
  const size_t b = split_at(off + len);
  pieces_.erase(pieces_.begin() + static_cast<std::ptrdiff_t>(a),
                pieces_.begin() + static_cast<std::ptrdiff_t>(b));
  doc_size_ -= len;
  coalesce();
}

Status PieceTable::read(uint64_t off, uint64_t len, char* buf, size_t cap,
                        size_t* used) const {
  *used = 0;
  if (off >= doc_size_) return Status::Ok;
  const uint64_t n = std::min<uint64_t>(len, doc_size_ - off);
  if (cap < n) return Status::BufferTooSmall;

  uint64_t acc = 0;       // document offset at the start of `p`
  uint64_t remaining = n;
  uint64_t dst = 0;
  bool started = false;   // past the piece that contains `off`
  for (const Piece& p : pieces_) {
    if (remaining == 0) break;
    if (p.len == 0) continue;
    uint64_t from = 0;
    if (!started) {
      if (off >= acc + p.len) {
        acc += p.len;
        continue;
      }
      from = off - acc;
      started = true;
    }
    const uint64_t take = std::min<uint64_t>(p.len - from, remaining);
    const Source* src = (p.src == Piece::Src::Base) ? base_ : add_;
    size_t got = 0;
    Status s = src->read(p.off + from, take, buf + dst,
                         static_cast<size_t>(take), &got);
    if (s != Status::Ok) return s;
    if (got != take) return Status::Internal;
    dst += take;
    remaining -= take;
    acc += p.len;
  }
  *used = static_cast<size_t>(n);
  return Status::Ok;
}

}  // namespace ed
