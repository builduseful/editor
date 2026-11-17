// tests/lock_test.cpp — the FileSystem lock (§6.4): exclusive, stale-recovering,
// and held by save's critical section.
#include <signal.h>
#include <sys/wait.h>
#include <unistd.h>

#include <chrono>
#include <cstdio>
#include <filesystem>
#include <memory>
#include <string>

#include "check.hpp"
#include "editor.hpp"

namespace {

std::string lock_tmp_dir() {
  static int counter = 0;
  const auto now = std::chrono::steady_clock::now().time_since_epoch().count();
  const auto d = std::filesystem::temp_directory_path() /
                 ("editor_lock_" + std::to_string(now) + "_" +
                  std::to_string(counter++));
  std::error_code ec;
  std::filesystem::create_directory(d, ec);
  return ec ? std::string() : d.string();
}

void write_str(const std::string& path, const std::string& data) {
  std::FILE* f = std::fopen(path.c_str(), "wb");
  if (f == nullptr) return;
  std::fwrite(data.data(), 1, data.size(), f);
  std::fclose(f);
}

}  // namespace

TEST(fs_lock_is_exclusive) {
  const std::string d = lock_tmp_dir();
  const std::string p = d + "/f.txt";
  write_str(p, "x\n");
  std::unique_ptr<FileSystem> fs;
  CHECK(create_file_system(&fs) == Status::Ok);

  void* h1 = nullptr;
  CHECK(fs->lock(p, &h1) == Status::Ok);
  CHECK(h1 != nullptr);
  void* h2 = nullptr;
  CHECK(fs->lock(p, &h2) == Status::Locked);  // held by this live process
  CHECK(h2 == nullptr);
  fs->unlock(h1);
  void* h3 = nullptr;
  CHECK(fs->lock(p, &h3) == Status::Ok);  // released
  fs->unlock(h3);
  CHECK(!std::filesystem::exists(p + ".editor.lock"));
}

TEST(fs_lock_recovers_from_a_dead_owner) {
  const std::string d = lock_tmp_dir();
  const std::string p = d + "/f.txt";
  write_str(p, "x\n");

  // A pid that is definitely gone: fork a child and reap it.
  const pid_t child = ::fork();
  if (child == 0) ::_exit(0);
  int status = 0;
  ::waitpid(child, &status, 0);

  write_str(p + ".editor.lock",
            std::to_string(static_cast<int>(child)) + " 1 0");

  std::unique_ptr<FileSystem> fs;
  CHECK(create_file_system(&fs) == Status::Ok);
  void* h = nullptr;
  CHECK(fs->lock(p, &h) == Status::Ok);  // stale lock stolen
  fs->unlock(h);
}

TEST(editor_save_respects_a_live_lock) {
  const std::string d = lock_tmp_dir();
  const std::string p = d + "/f.txt";
  write_str(p, "abc\n");

  std::unique_ptr<FileSystem> fs;
  CHECK(create_file_system(&fs) == Status::Ok);
  std::unique_ptr<Editor> ed;
  CHECK(create_editor(*fs, &ed) == Status::Ok);
  CHECK(ed->open(p) == Status::Ok);
  const Edit e[1] = {Edit{0, 0, 0, std::string_view("X")}};
  CHECK(ed->apply(e) == Status::Ok);

  // A lock file naming a live process (us) blocks the save.
  write_str(p + ".editor.lock",
            std::to_string(static_cast<int>(::getpid())) + " 1 0");
  SaveInfo si;
  CHECK(ed->save(nullptr, false, &si) == Status::Locked);

  // Removing the lock lets the save proceed.
  std::filesystem::remove(p + ".editor.lock");
  CHECK(ed->save(nullptr, false, &si) == Status::Ok);
}
