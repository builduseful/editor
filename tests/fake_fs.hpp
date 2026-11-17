// tests/fake_fs.hpp — in-memory FileSystem for unit tests.
//
// Implements enough of §4 FileSystem to exercise core (temp files, pread/pwrite,
// ftruncate, metadata, rename). mmap and locking are inert; tests that need them
// come later.
#pragma once

#include <cstdint>
#include <cstring>
#include <map>
#include <string>
#include <vector>

#include "editor.hpp"

namespace ed_test {

class FakeFileSystem final : public FileSystem {
 public:
  Status open_readonly(std::string_view path, bool /*nofollow*/,
                       int* out_fd) override {
    auto it = files_.find(std::string(path));
    if (it == files_.end()) return Status::NotFound;
    *out_fd = new_fd(std::string(path));
    return Status::Ok;
  }

  Status open_write(std::string_view path, bool create, bool truncate,
                    bool /*nofollow*/, int* out_fd) override {
    const std::string p(path);
    auto it = files_.find(p);
    if (create && it != files_.end()) return Status::Conflict;
    if (!create && it == files_.end()) return Status::NotFound;
    if (create) files_[p] = {};
    if (truncate) files_[p].clear();
    *out_fd = new_fd(p);
    return Status::Ok;
  }

  Status open_scan(std::string_view path, bool nofollow, int* out_fd) override {
    return open_readonly(path, nofollow, out_fd);
  }

  Status open_dir(std::string_view path, int* out_fd) override {
    *out_fd = new_fd(std::string(path));
    return Status::Ok;
  }

  Status create_temp(std::string_view dir, std::string* out_path,
                     int* out_fd) override {
    const std::string p =
        std::string(dir) + "/.editor-tmp-" + std::to_string(temp_counter_++);
    files_[p] = {};
    *out_path = p;
    *out_fd = new_fd(p);
    return Status::Ok;
  }

  Status clone_to_new(std::string_view dst_dir, std::string* out_path,
                      int src_fd, int* out_fd) override {
    const std::string p = std::string(dst_dir) + "/.editor-clone-" +
                          std::to_string(temp_counter_++);
    files_[p] = files_[fds_[src_fd]];
    *out_path = p;
    *out_fd = new_fd(p);
    return Status::Ok;
  }

  void close_fd(int fd) override { fds_.erase(fd); }

  Status map_readonly(int /*fd*/, Mapping** /*out*/) override {
    return Status::Unsupported;
  }
  Status unmap(Mapping* /*m*/) override { return Status::Unsupported; }

  Status pread(int fd, uint64_t off, char* buf, size_t cap,
               size_t* used) override {
    *used = 0;
    auto fit = files_.find(fds_[fd]);
    if (fit == files_.end()) return Status::Io;
    const std::vector<char>& v = fit->second;
    if (off >= v.size()) return Status::Ok;
    const uint64_t n = std::min<uint64_t>(cap, v.size() - off);
    std::memcpy(buf, v.data() + off, static_cast<size_t>(n));
    *used = static_cast<size_t>(n);
    return Status::Ok;
  }

  Status pwrite(int fd, uint64_t off, const char* buf, size_t len,
                size_t* used) override {
    auto fit = files_.find(fds_[fd]);
    if (fit == files_.end()) return Status::Io;
    std::vector<char>& v = fit->second;
    if (v.size() < off + len) v.resize(static_cast<size_t>(off + len));
    std::memcpy(v.data() + off, buf, len);
    *used = len;
    return Status::Ok;
  }

  Status ftruncate(int fd, uint64_t size) override {
    auto fit = files_.find(fds_[fd]);
    if (fit == files_.end()) return Status::Io;
    fit->second.resize(static_cast<size_t>(size));
    return Status::Ok;
  }

  Status fchmod(int /*fd*/, uint16_t /*mode*/) override { return Status::Ok; }
  Status fchown(int /*fd*/, uint32_t /*uid*/, uint32_t /*gid*/) override {
    return Status::Ok;
  }

  Status fstat(int fd, Stat* out) override {
    auto fit = files_.find(fds_[fd]);
    if (fit == files_.end()) return Status::Io;
    *out = Stat{};
    out->size = fit->second.size();
    out->regular = true;
    return Status::Ok;
  }

  Status fsync(int /*fd*/, bool /*full*/) override { return Status::Ok; }

  Status copy_metadata(int /*src_fd*/, int /*dst_fd*/) override {
    return Status::Ok;
  }

  Status rename_at(int dst_dirfd, std::string_view dst_name,
                   std::string_view tmp_path) override {
    auto dit = fds_.find(dst_dirfd);
    if (dit == fds_.end()) return Status::Io;
    const std::string dst = dit->second + "/" + std::string(dst_name);
    auto it = files_.find(std::string(tmp_path));
    if (it == files_.end()) return Status::NotFound;
    files_[dst] = it->second;
    files_.erase(it);
    return Status::Ok;
  }

  Status fsync_dir(int /*dirfd*/) override { return Status::Ok; }

  Status unlink(std::string_view path) override {
    files_.erase(std::string(path));
    return Status::Ok;
  }

  Status identity(std::string_view path, Rev* out) override {
    auto it = files_.find(std::string(path));
    if (it == files_.end()) return Status::NotFound;
    *out = Rev{};
    out->size = it->second.size();
    return Status::Ok;
  }

  Status resolve(std::string_view path, std::string* out_realpath) override {
    *out_realpath = std::string(path);
    return Status::Ok;
  }

  Status lstat(std::string_view path, Stat* out) override {
    auto it = files_.find(std::string(path));
    if (it == files_.end()) return Status::NotFound;
    *out = Stat{};
    out->size = it->second.size();
    out->regular = true;
    return Status::Ok;
  }

  Status lock(std::string_view /*path*/, void** lock) override {
    *lock = this;
    return Status::Ok;
  }
  void unlock(void* /*lock*/) override {}

  // Test helpers.
  std::string contents(const std::string& path) const {
    auto it = files_.find(path);
    if (it == files_.end()) return {};
    return std::string(it->second.begin(), it->second.end());
  }
  bool exists(const std::string& path) const {
    return files_.find(path) != files_.end();
  }
  size_t file_count() const { return files_.size(); }

 private:
  int new_fd(const std::string& path) {
    const int fd = next_fd_++;
    fds_[fd] = path;
    return fd;
  }

  std::map<std::string, std::vector<char>> files_;
  std::map<int, std::string> fds_;
  int next_fd_ = 3;
  int temp_counter_ = 0;
};

}  // namespace ed_test
