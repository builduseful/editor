// core/src/editor_factory.cpp — portable Editor factory (§4 platform seam).
#include <memory>

#include "editor.hpp"
#include "editor_impl.hpp"

Status create_editor(FileSystem& fs, std::unique_ptr<Editor>* out) {
  if (out == nullptr) return Status::Internal;
  *out = std::make_unique<ed::EditorImpl>(&fs);
  return Status::Ok;
}
