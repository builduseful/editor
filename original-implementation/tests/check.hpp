// tests/check.hpp — hand-rolled test macros (PLAN.md §10: no vendored framework).
//
// TEST(name) registers a function; the runner in tests/main.cpp calls them all.
#pragma once

#include <cstdio>
#include <string>
#include <vector>

namespace check {

using TestFn = void (*)();

inline std::vector<TestFn>& tests() {
  static std::vector<TestFn> t;
  return t;
}

struct Registrar {
  explicit Registrar(TestFn f) { tests().push_back(f); }
};

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

inline int run_all() {
  for (TestFn f : tests()) f();
  Context& c = ctx();
  for (const std::string& f : c.failures) std::fprintf(stderr, "%s\n", f.c_str());
  std::printf("editor_tests: %d passed, %d failed, %d skipped\n", c.passed,
              c.failed, c.skipped);
  return c.failed == 0 ? 0 : 1;
}

}  // namespace check

#define CHECK(cond) ::check::ctx().ok((cond), #cond, __FILE__, __LINE__)

#define CHECK_EQ(a, b)                                        \
  do {                                                        \
    auto check_lhs_ = (a);                                    \
    auto check_rhs_ = (b);                                    \
    ::check::ctx().eq(check_lhs_ == check_rhs_, #a " == " #b, \
                      __FILE__, __LINE__);                    \
  } while (0)

#define SKIP(what) ::check::ctx().skip((what), __FILE__, __LINE__)

#define TEST(name)                                    \
  static void name();                                 \
  static ::check::Registrar check_reg_##name(&name);  \
  static void name()
