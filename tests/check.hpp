// tests/check.hpp — hand-rolled test macros (PLAN.md §10: no vendored framework).
//
// Deliberately tiny: a CHECK/CHECK_EQ pair, a SKIP, and a counter. Property
// tests add their own PRNG separately (§10).
#pragma once

#include <cstdio>
#include <string>
#include <vector>

namespace check {

struct Context {
  int passed = 0;
  int failed = 0;
  int skipped = 0;
  std::vector<std::string> failures;

  void ok(bool cond, const char* expr, const char* file, int line) {
    if (cond) {
      ++passed;
      return;
    }
    ++failed;
    char buf[512];
    std::snprintf(buf, sizeof(buf), "%s:%d: CHECK failed: %s", file, line, expr);
    failures.emplace_back(buf);
  }

  void eq(bool equal, const char* expr, const char* file, int line) {
    ok(equal, expr, file, line);
  }

  void skip(const char* what, const char* file, int line) {
    ++skipped;
    std::printf("  skip: %s (%s:%d)\n", what, file, line);
  }
};

inline Context& ctx() {
  static Context c;
  return c;
}

inline int report(const char* suite) {
  Context& c = ctx();
  for (const std::string& f : c.failures) std::fprintf(stderr, "%s\n", f.c_str());
  std::printf("%s: %d passed, %d failed, %d skipped\n", suite, c.passed, c.failed,
              c.skipped);
  return c.failed == 0 ? 0 : 1;
}

}  // namespace check

#define CHECK(cond) ::check::ctx().ok((cond), #cond, __FILE__, __LINE__)

#define CHECK_EQ(a, b)                          \
  do {                                          \
    auto check_eq_lhs_ = (a);                   \
    auto check_eq_rhs_ = (b);                   \
    ::check::ctx().eq(check_eq_lhs_ == check_eq_rhs_, \
                      #a " == " #b, __FILE__, __LINE__); \
  } while (0)

#define SKIP(what) ::check::ctx().skip((what), __FILE__, __LINE__)
