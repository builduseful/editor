// tests/binding_test.cpp — the §6.6 binding invariant.
//
// Every core Editor operation must be bound to a CLI command or a GUI surface,
// or be explicitly CLI-excluded, and the table must cover the whole §4 surface.
#include <cstddef>
#include <set>
#include <string>

#include "binding.hpp"
#include "check.hpp"

TEST(binding_invariant_holds) {
  std::size_t n = 0;
  const binding::Entry* t = binding::table(&n);
  CHECK(n > 0);

  std::set<std::string> fns;
  for (std::size_t i = 0; i < n; ++i) {
    const bool has_cli = t[i].cli != nullptr && t[i].cli[0] != '\0';
    const bool has_gui = t[i].gui != nullptr && t[i].gui[0] != '\0';
    // Every core_fn is bound (or explicitly excluded).
    CHECK(has_cli || has_gui || t[i].cli_excluded);
    // An explicit exclusion means no CLI binding.
    CHECK(!(t[i].cli_excluded && has_cli));
    // No duplicate core_fn entries.
    CHECK(fns.insert(t[i].core_fn).second);
  }

  // The table covers exactly the §4 Editor surface.
  const char* kOps[] = {
      "open",      "create",     "info",       "rev",         "read_lines",
      "read_range", "apply",     "undo",       "redo",        "save",
      "save_as",   "reload",     "check_identity", "on_change", "off_change",
      "drain",     "errmsg"};
  for (const char* op : kOps) {
    CHECK(fns.count(op) == 1);
  }
  CHECK_EQ(fns.size(), sizeof(kOps) / sizeof(kOps[0]));
}
