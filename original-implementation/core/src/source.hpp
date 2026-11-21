// core/src/source.hpp — internal byte source (base file or add buffer).
//
// Not part of the frozen §4 interface. A Source is a read-only byte sequence
// with an exact size; the piece table reads through it.
#pragma once

#include <cstddef>
#include <cstdint>

#include "editor.hpp"

namespace ed {

struct Source {
  virtual ~Source() = default;
  virtual uint64_t size() const = 0;
  // Copies min(len, size()-off) bytes into buf and sets *used to that count.
  // Internal contract: cap >= that count; never returns BufferTooSmall.
  virtual Status read(uint64_t off, uint64_t len, char* buf, size_t cap,
                      size_t* used) const = 0;
};

}  // namespace ed
