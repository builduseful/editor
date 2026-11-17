// bench/bench_edit.cpp — the M0a save gate (§5.5/§7):
//
//   "same-length edit in 1 GB on APFS < 100 ms p99 with crash-only fsync"
//
// Creates the file if missing, then measures, over many iterations, a
// same-length edit + save through the frozen core interface. Reports p50/p99
// and max. Also reports the cold open (lazy index build) separately, per §7's
// "separate warm (index built) from cold (first random edit)".
#include <sys/resource.h>
#include <sys/stat.h>

#include <algorithm>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <memory>
#include <string>
#include <vector>

#include "editor.hpp"

namespace {

using Clock = std::chrono::steady_clock;

double ms(Clock::duration d) {
  return std::chrono::duration<double, std::milli>(d).count();
}

std::string arg(int argc, char** argv, const char* name, const char* def) {
  for (int i = 1; i + 1 < argc; ++i) {
    if (std::string(argv[i]) == name) return argv[i + 1];
  }
  return def;
}

// 199 'a' + '\n' == 200 B/line, so a ~1 GiB file is ~5.37M lines (>=200 B/line,
// matching the §7 loaded-resident row).
constexpr size_t kLineBytes = 200;

bool ensure_file(FileSystem& fs, const std::string& path, uint64_t target) {
  Stat st;
  if (fs.lstat(path, &st) == Status::Ok && st.regular && st.size >= target) {
    return true;
  }
  int fd = -1;
  if (fs.open_write(path, true, true, true, &fd) != Status::Ok) {
    // Existing file: truncate instead of O_EXCL.
    if (fs.open_write(path, false, true, true, &fd) != Status::Ok) return false;
  }
  std::string line(kLineBytes - 1, 'a');
  line.push_back('\n');
  std::string chunk;
  chunk.reserve(kLineBytes * 4096);
  for (int i = 0; i < 4096; ++i) chunk += line;
  uint64_t off = 0;
  while (off < target) {
    size_t w = 0;
    if (fs.pwrite(fd, off, chunk.data(), chunk.size(), &w) != Status::Ok) break;
    off += w;
  }
  fs.ftruncate(fd, target);
  fs.fsync(fd, false);
  fs.close_fd(fd);
  return true;
}

double pct(std::vector<double>& v, double p) {
  if (v.empty()) return 0.0;
  std::sort(v.begin(), v.end());
  const size_t i = static_cast<size_t>(p * static_cast<double>(v.size() - 1));
  return v[i];
}

}  // namespace

int main(int argc, char** argv) {
  const std::string path = arg(argc, argv, "--file", "/tmp/editor_bench_1g.txt");
  const uint64_t target = std::stoull(arg(argc, argv, "--bytes", "1073741824"));
  const int iters = std::atoi(arg(argc, argv, "--iters", "200").c_str());

  std::unique_ptr<FileSystem> fs;
  if (create_file_system(&fs) != Status::Ok) {
    std::fprintf(stderr, "no filesystem\n");
    return 1;
  }

  {
    auto t0 = Clock::now();
    if (!ensure_file(*fs, path, target)) {
      std::fprintf(stderr, "cannot create %s\n", path.c_str());
      return 1;
    }
    Stat st;
    fs->lstat(path, &st);
    std::printf("file: %s (%.2f GiB, %llu lines)\n", path.c_str(),
                static_cast<double>(st.size) / (1024.0 * 1024.0 * 1024.0),
                static_cast<unsigned long long>(st.size / kLineBytes));
    if (ms(Clock::now() - t0) > 50.0) {
      std::printf("(created in %.1f s)\n", ms(Clock::now() - t0) / 1000.0);
    }
  }

  std::unique_ptr<Editor> ed;
  if (create_editor(*fs, &ed) != Status::Ok) return 1;

  // Cold: open + first info (forces the lazy index build).
  {
    auto t0 = Clock::now();
    Status s = ed->open(path);
    if (s != Status::Ok) {
      std::fprintf(stderr, "open failed: %d\n", static_cast<int>(s));
      return 1;
    }
    Info info;
    ed->info(&info);
    const double cold = ms(Clock::now() - t0);
    std::printf("cold open+info (index build): %.1f ms (%lld lines)\n", cold,
                static_cast<long long>(info.line_count));
  }

  const uint64_t lines = 5368709;  // ~1 GiB / 200 B
  std::vector<double> apply_ms, save_ms, total_ms;
  apply_ms.reserve(static_cast<size_t>(iters));
  save_ms.reserve(static_cast<size_t>(iters));
  total_ms.reserve(static_cast<size_t>(iters));

  for (int i = 0; i < iters; ++i) {
    // Same-length edit: flip one interior byte of a pseudo-random line.
    const uint64_t line = (static_cast<uint64_t>(i) * 2654435761u) % lines;
    const uint64_t off = line * kLineBytes + 100;
    char cur = 'a';
    size_t g = 0;
    ed->read_range(static_cast<int64_t>(off), 1, &cur, 1, &g);
    const char next = (cur == 'a') ? 'b' : 'a';

    Edit e;
    e.line = static_cast<int64_t>(line);
    e.col = 100;
    e.removed_len = 1;
    e.text = std::string_view(&next, 1);

    auto t0 = Clock::now();
    const Status as = ed->apply({&e, 1});
    auto t1 = Clock::now();
    SaveInfo si;
    const Status ss = ed->save(nullptr, false, &si);
    auto t2 = Clock::now();
    if (as != Status::Ok || ss != Status::Ok) {
      std::fprintf(stderr, "iter %d: apply=%d save=%d\n", i, static_cast<int>(as),
                   static_cast<int>(ss));
      return 1;
    }
    if (i == 0) std::printf("save method: %s\n", si.method);
    apply_ms.push_back(ms(t1 - t0));
    save_ms.push_back(ms(t2 - t1));
    total_ms.push_back(ms(t2 - t0));
  }

  std::printf("same-length apply:  p50 %.2f ms  p99 %.2f ms\n", pct(apply_ms, 0.50),
              pct(apply_ms, 0.99));
  std::printf("same-length save:   p50 %.2f ms  p99 %.2f ms  (crash-only fsync)\n",
              pct(save_ms, 0.50), pct(save_ms, 0.99));
  std::printf("same-length total:  p50 %.2f ms  p99 %.2f ms  max %.2f ms\n",
              pct(total_ms, 0.50), pct(total_ms, 0.99),
              *std::max_element(total_ms.begin(), total_ms.end()));

  rusage ru{};
  getrusage(RUSAGE_SELF, &ru);
  std::printf("peak RSS: %.1f MB\n", static_cast<double>(ru.ru_maxrss) / (1024.0 * 1024.0));

  const double p99 = pct(total_ms, 0.99);
  std::printf("gate (p99 < 100 ms): %s\n", p99 < 100.0 ? "PASS" : "FAIL");
  return p99 < 100.0 ? 0 : 2;
}
