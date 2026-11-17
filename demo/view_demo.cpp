// demo/view_demo.cpp — the M0.5 read-only view demo and footprint gate.
//
// Opens a (possibly very large) file, then scrolls a viewport and draws it
// through the headless render oracle, reporting frame times and resident
// footprint. Gate: 60 fps (p99 frame < 16.7 ms) with a flat footprint.
#include <sys/resource.h>

#include <algorithm>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <memory>
#include <string>
#include <vector>

#include "editor.hpp"
#include "headless.hpp"
#include "viewcore.hpp"

namespace {

using Clock = std::chrono::steady_clock;

double rss_mb() {
  struct rusage ru;
  getrusage(RUSAGE_SELF, &ru);
  return static_cast<double>(ru.ru_maxrss) / (1024.0 * 1024.0);
}

double pct(std::vector<double> v, double p) {
  if (v.empty()) return 0;
  std::sort(v.begin(), v.end());
  const size_t i = static_cast<size_t>(p * static_cast<double>(v.size() - 1));
  return v[i];
}

}  // namespace

int main(int argc, char** argv) {
  std::string file;
  int frames = 600;
  int step = 20;
  double width = 1000, height = 800;
  bool wrap = false;
  for (int i = 1; i < argc; ++i) {
    const std::string a = argv[i];
    auto next = [&]() -> std::string {
      return (i + 1 < argc) ? std::string(argv[++i]) : std::string();
    };
    if (a == "--file") file = next();
    else if (a == "--frames") frames = std::atoi(next().c_str());
    else if (a == "--step") step = std::atoi(next().c_str());
    else if (a == "--width") width = std::atof(next().c_str());
    else if (a == "--height") height = std::atof(next().c_str());
    else if (a == "--wrap") wrap = true;
  }
  if (file.empty()) {
    std::fprintf(stderr,
                 "usage: view_demo --file F [--frames N] [--step N] [--wrap]\n");
    return 2;
  }

  std::unique_ptr<FileSystem> fs;
  create_file_system(&fs);
  std::unique_ptr<Editor> ed;
  create_editor(*fs, &ed);

  const auto o0 = Clock::now();
  const Status s = ed->open(file);
  Info info;
  if (s == Status::Ok) ed->info(&info);
  const auto o1 = Clock::now();
  if (s != Status::Ok) {
    std::fprintf(stderr, "open failed: status %d\n", static_cast<int>(s));
    return 1;
  }
  const double open_ms =
      std::chrono::duration<double, std::milli>(o1 - o0).count();

  vc::HeadlessTextBackend tb;
  vc::ViewCore view(*ed, tb);
  view.resize(width, height);
  view.set_wrap(wrap);
  vc::RecordingCanvas canvas;

  // Warm-up: build the first viewport's cache entries.
  view.draw(canvas);
  const double rss_after_warm = rss_mb();

  std::vector<double> frame_ms;
  frame_ms.reserve(static_cast<size_t>(frames));
  for (int f = 0; f < frames; ++f) {
    view.scroll_lines(step);
    if (view.first_visible_line() >= info.line_count) view.scroll_to_line(0);
    canvas.cmds.clear();
    const auto a = Clock::now();
    view.draw(canvas);
    const auto b = Clock::now();
    frame_ms.push_back(std::chrono::duration<double, std::milli>(b - a).count());
  }
  const double rss_after = rss_mb();

  const double p50 = pct(frame_ms, 0.50);
  const double p99 = pct(frame_ms, 0.99);
  std::printf("file: %s\n", file.c_str());
  std::printf("lines: %lld  wrap: %s  viewport: %.0fx%.0f\n",
              static_cast<long long>(info.line_count), wrap ? "on" : "off",
              width, height);
  std::printf("cold open+info: %.1f ms\n", open_ms);
  std::printf("frame: p50 %.3f ms  p99 %.3f ms  (%.0f fps p50)\n", p50, p99,
              p50 > 0 ? 1000.0 / p50 : 0.0);
  std::printf("cache: %zu entries, %.1f KB\n", view.cache_entries(),
              static_cast<double>(view.cache_bytes()) / 1024.0);
  std::printf("peak RSS: %.1f MB (warm) -> %.1f MB\n", rss_after_warm, rss_after);
  std::printf("gate (p99 < 16.7 ms): %s\n", p99 < 16.7 ? "PASS" : "FAIL");
  return p99 < 16.7 ? 0 : 1;
}
