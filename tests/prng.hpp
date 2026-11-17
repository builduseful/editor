// tests/prng.hpp — small deterministic PRNG (PLAN.md §10).
#pragma once

#include <cstdint>

namespace ed_test {

class Prng {
 public:
  explicit Prng(uint64_t seed) : state_(seed ? seed : 0x9e3779b97f4a7c15ull) {}
  uint64_t next() {
    uint64_t z = (state_ += 0x9e3779b97f4a7c15ull);
    z = (z ^ (z >> 30)) * 0xbf58476d1ce4e5b9ull;
    z = (z ^ (z >> 27)) * 0x94d049bb133111ebull;
    return z ^ (z >> 31);
  }
  // Uniform in [0, n).
  uint64_t below(uint64_t n) { return n == 0 ? 0 : next() % n; }

 private:
  uint64_t state_;
};

}  // namespace ed_test
