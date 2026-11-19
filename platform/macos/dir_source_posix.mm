// platform/macos/dir_source_posix.mm — the sidebar's DirSource over POSIX (§9).
//
// `opendir`/`readdir` in the app layer; the tree and its layout stay in
// `viewcore`, so this is the only place the sidebar touches the filesystem.
#include "dir_source_posix.hpp"

#import <dirent.h>
#import <sys/stat.h>

#include <algorithm>
#include <cstring>
#include <string>
#include <vector>

#include "sidebar.hpp"

namespace ed::mac {

namespace {

// Joins without doubling the separator ("/" + "etc" -> "/etc").
std::string join(const std::string& dir, const char* name) {
  if (dir.empty()) return std::string(name);
  if (dir.back() == '/') return dir + name;
  return dir + "/" + name;
}

}  // namespace

bool PosixDirSource::read_dir(std::string_view path,
                              std::vector<vc::DirEntry>* out) {
  if (out == nullptr) return false;
  out->clear();
  const std::string dir(path);
  DIR* d = opendir(dir.c_str());
  if (d == nullptr) return false;

  std::vector<vc::DirEntry> entries;
  errno = 0;
  for (struct dirent* ent = readdir(d); ent != nullptr; ent = readdir(d)) {
    const char* name = ent->d_name;
    // Skip "." and "..": the tree is rooted at a real directory and never needs
    // them, and showing them invites loops.
    if (std::strcmp(name, ".") == 0 || std::strcmp(name, "..") == 0) continue;

    vc::DirEntry e;
    e.name = name;
    e.path = join(dir, name);

    // d_type is cheap but not always populated (some filesystems return
    // DT_UNKNOWN); fall back to lstat when it is. A symlink is recorded as one
    // and never followed, so the tree cannot loop.
    if (ent->d_type == DT_DIR) {
      e.is_dir = true;
    } else if (ent->d_type == DT_LNK) {
      e.is_symlink = true;
    } else if (ent->d_type == DT_UNKNOWN) {
      struct stat st;
      if (lstat(e.path.c_str(), &st) == 0) {
        if (S_ISDIR(st.st_mode)) {
          e.is_dir = true;
        } else if (S_ISLNK(st.st_mode)) {
          e.is_symlink = true;
        }
      }
    }
    entries.push_back(std::move(e));
  }
  closedir(d);
  *out = std::move(entries);
  return true;
}

}  // namespace ed::mac