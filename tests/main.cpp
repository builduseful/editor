// tests/main.cpp — test runner.
//
// At Pre-M0.0 the conformance suite has no implementation to exercise, so this
// proves the harness runs (the "conformance skeleton runs" half of the gate).
// M0a wires run_editor_conformance() to a real Editor + FileSystem here.
#include "check.hpp"

int main() {
  CHECK(true);
  CHECK_EQ(1 + 1, 2);
  SKIP("Editor/FileSystem/Mapping conformance suite: pending M0a implementation");
  return check::report("editor_tests");
}
