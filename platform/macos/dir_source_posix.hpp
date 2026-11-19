// platform/macos/dir_source_posix.hpp — the sidebar's directory source (§9).
#pragma once

#include "sidebar.hpp"

namespace ed::mac {

// Reads directories with opendir/readdir. Stateless, so one instance can serve
// every sidebar.
class PosixDirSource final : public vc::DirSource {
 public:
  bool read_dir(std::string_view path, std::vector<vc::DirEntry>* out) override;
};

}  // namespace ed::mac