// core/src/piece_table.hpp — document as pieces over base + add buffer (§5.2).
//
// The base is an immutable piece; edits append to the add buffer and add pieces
// that point into it. Adjacent inserts at the add-buffer tail are coalesced so
// typing does not create unbounded pieces.
#pragma once

#include <cstdint>
#include <vector>

#include "source.hpp"

namespace ed {

struct Piece {
  enum class Src : uint8_t { Base = 0, Add = 1 };
  Src src = Src::Base;
  uint64_t off = 0;  // offset within the source
  uint64_t len = 0;  // bytes
};

class PieceTable final : public Source {
 public:
  PieceTable() = default;

  // Points the table at a base and an add buffer. The base piece covers the
  // whole base file.
  void reset(const Source* base, const Source* add, uint64_t base_size);

  uint64_t size() const override { return doc_size_; }
  Status read(uint64_t off, uint64_t len, char* buf, size_t cap,
              size_t* used) const override;

  // True iff the table is exactly the original base piece (§4 dirty rule).
  bool pristine() const;
  const std::vector<Piece>& pieces() const { return pieces_; }

  // Insert `len` add-buffer bytes at add offset `add_off` at document offset
  // `off`. Remove `len` document bytes at `off`.
  void insert(uint64_t off, uint64_t add_off, uint64_t len);
  void remove(uint64_t off, uint64_t len);

 private:
  size_t split_at(uint64_t off);
  void coalesce();

  const Source* base_ = nullptr;
  const Source* add_ = nullptr;
  uint64_t base_size_ = 0;
  std::vector<Piece> pieces_;
  uint64_t doc_size_ = 0;
};

}  // namespace ed
