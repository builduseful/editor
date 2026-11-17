// tests/conformance/editor_conformance.cpp — §4 conformance suite skeleton.
//
// One suite per §4 interface (`Editor`, `Mapping`, `FileSystem`), run against
// any implementation. At Pre-M0.0 there is no implementation yet, so the body
// is a placeholder; the value now is that this translation unit *compiles*
// against core/include/editor.hpp, which is the Pre-M0.0 gate ("interface
// compiles; conformance skeleton runs"). M0a fills the body.
#include <type_traits>

#include "check.hpp"
#include "editor.hpp"

// --- Compile-time shape checks (the "interface compiles" half of the gate) ---

static_assert(std::is_same_v<std::underlying_type_t<Status>, std::uint32_t>,
              "Status must be uint32_t-backed (§4)");
static_assert(std::is_same_v<std::underlying_type_t<Eol>, std::uint32_t>);
static_assert(std::is_same_v<std::underlying_type_t<Encoding>, std::uint32_t>);
static_assert(std::is_same_v<std::underlying_type_t<ChangeKind>, std::uint32_t>);

static_assert(Editor::kInterfaceVersion == 1, "version tag is kInterfaceVersion");

static_assert(std::has_virtual_destructor_v<Editor>);
static_assert(std::has_virtual_destructor_v<Mapping>);
static_assert(std::has_virtual_destructor_v<FileSystem>);

static_assert(std::is_abstract_v<Editor>);
static_assert(std::is_abstract_v<Mapping>);
static_assert(std::is_abstract_v<FileSystem>);

static_assert(!std::is_copy_constructible_v<Editor>);
static_assert(!std::is_copy_constructible_v<FileSystem>);

// --- Runtime suite ----------------------------------------------------------

// Runs the `Editor`/`FileSystem`/`Mapping` conformance suite against a live
// implementation. Not called yet: no implementation exists at Pre-M0.0.
void run_editor_conformance(Editor& /*editor*/, FileSystem& /*fs*/) {
  SKIP("Pre-M0.0: conformance body lands with the M0a implementation");
}
