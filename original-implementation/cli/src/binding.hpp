// cli/src/binding.hpp — the §6.6 binding invariant.
//
// Every §4 core operation is bound to a CLI command, a GUI surface, or is
// explicitly excluded from the CLI. `editor schema` is generated from this
// table, and tests/binding_test.cpp checks that the invariant holds and that the
// table covers every Editor operation.
#pragma once

#include <cstddef>

namespace binding {

struct Entry {
  const char* core_fn;  // §4 Editor method
  const char* cli;      // comma-separated CLI commands ("" if none)
  const char* gui;      // GUI surface ("" if none)
  bool cli_excluded;    // deliberately not a CLI command
};

inline const Entry* table(std::size_t* n) {
  static const Entry kTable[] = {
      {"open", "read,write,edit,info,validate", "open", false},
      {"create", "", "new", true},  // internal to write; no CLI command
      {"info", "info", "status", false},
      {"rev", "info,write,edit,batch", "status", false},
      {"read_lines", "read", "view", false},
      {"read_range", "read", "view", false},
      {"apply", "write,edit,batch", "edit", false},
      {"undo", "", "undo", false},   // GUI/conformance-only in v1
      {"redo", "", "redo", false},   // GUI/conformance-only in v1
      {"save", "write,edit,batch", "save", false},
      {"save_as", "", "save-as", false},  // GUI/conformance-only in v1
      {"reload", "", "reload", false},    // GUI/conformance-only in v1
      {"check_identity", "", "watch", false},
      {"on_change", "", "edit", false},
      {"off_change", "", "edit", false},
      {"drain", "", "edit", false},
      {"errmsg", "", "status", false},
  };
  *n = sizeof(kTable) / sizeof(kTable[0]);
  return kTable;
}

}  // namespace binding
