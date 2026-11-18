// demo/view_demo.cpp — the M0.5 read-only view demo and footprint gate.
//
// Opens a (possibly very large) file, then scrolls a viewport and draws it
// through the headless render oracle, reporting first-paint time, frame times,
// and resident footprint. With no --file it generates a synthetic file so
// `make demo` is self-contained. Gates: first paint (open->draw) < 100 ms
// (< 250 ms for >=4 GB), and 60 fps (p99 frame < 16.7 ms) with a flat footprint.
#include <sys/resource.h>
#include <unistd.h>

#include <algorithm>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <filesystem>
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

// Writes a synthetic text file of ~`bytes` bytes and returns its path (empty on
// failure). Used when the demo is run without --file.
std::string make_synthetic(uint64_t bytes) {
  namespace fs = std::filesystem;
  std::error_code ec;
  const auto dir = fs::temp_directory_path(ec);
  const auto now = std::chrono::steady_clock::now().time_since_epoch().count();
  const auto path =
      dir / ("editor_view_demo_" + std::to_string(::getpid()) + "_" +
             std::to_string(now) + ".txt");
  std::FILE* f = std::fopen(path.c_str(), "wb");
  if (f == nullptr) return {};
  static const char kLine[] = "the quick brown fox jumps over the lazy dog\n";
  std::string block;
  block.reserve(1u << 20);
  while (block.size() < (1u << 20)) block.append(kLine, sizeof(kLine) - 1);
  uint64_t written = 0;
  while (written < bytes) {
    const size_t chunk =
        static_cast<size_t>(std::min<uint64_t>(block.size(), bytes - written));
    if (std::fwrite(block.data(), 1, chunk, f) != chunk) break;
    written += chunk;
  }
  std::fclose(f);
  return path.string();
}

struct TempFile {
  std::string path;
  ~TempFile() {
    if (!path.empty()) std::remove(path.c_str());
  }
};

}  // namespace

int main(int argc, char** argv) {
  std::string file;
  int frames = 600;
  int step = 20;
  double width = 1000, height = 800;
  double synth_mb = 256.0;
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
    else if (a == "--size") synth_mb = std::atof(next().c_str());
    else if (a == "--wrap") wrap = true;
  }
  TempFile synth;
  if (file.empty()) {
    synth.path = make_synthetic(static_cast<uint64_t>(synth_mb * 1024.0 * 1024.0));
    if (synth.path.empty()) {
      std::fprintf(stderr, "could not create a synthetic file\n");
      return 2;
    }
    file = synth.path;
  }

  std::unique_ptr<FileSystem> fs;
  create_file_system(&fs);
  std::unique_ptr<Editor> ed;
  create_editor(*fs, &ed);

  const auto o0 = Clock::now();
  const Status s = ed->open(file);
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

  // First paint — the M1 gate. Open -> first draw must not scan the document:
  // viewcore discovers only the visible lines through the lazy line index.
  view.draw(canvas);
  const auto f1 = Clock::now();
  const double first_paint_ms =
      std::chrono::duration<double, std::milli>(f1 - o0).count();
  const bool lazy = !view.line_count_known();
  const int64_t discovered = view.line_count();

  // Scroll immediately (index still lazy): each frame extends the line index
  // only as far as it is needed. This is the open-then-scroll experience.
  std::vector<double> frame_ms;
  frame_ms.reserve(static_cast<size_t>(frames));
  for (int f = 0; f < frames; ++f) {
    view.scroll_lines(step);
    if (view.line_count_known() &&
        view.first_visible_line() >= view.line_count()) {
      view.scroll_to_line(0);
    }
    canvas.cmds.clear();
    const auto a = Clock::now();
    view.draw(canvas);
    const auto b = Clock::now();
    frame_ms.push_back(std::chrono::duration<double, std::milli>(b - a).count());
  }
  const int64_t after_scroll_lines = view.line_count();
  const double rss_after_warm = rss_mb();

  // The exact, full-document scan (line count + EOL), reported separately.
  const auto i0 = Clock::now();
  Info info;
  ed->info(&info);
  const auto i1 = Clock::now();
  const double info_ms = std::chrono::duration<double, std::milli>(i1 - i0).count();
  const double rss_after = rss_mb();

  const double p50 = pct(frame_ms, 0.50);
  const double p99 = pct(frame_ms, 0.99);
  const double paint_gate = info.bytes >= (4ull << 30) ? 250.0 : 100.0;
  const bool paint_pass = first_paint_ms < paint_gate;
  std::printf("file: %s\n", file.c_str());
  std::printf("lines: %lld  wrap: %s  viewport: %.0fx%.0f\n",
              static_cast<long long>(info.line_count), wrap ? "on" : "off",
              width, height);
  std::printf("cold open: %.1f ms\n", open_ms);
  std::printf("first paint (open+draw): %.1f ms  (lazy=%s, %lld lines discovered)\n",
              first_paint_ms, lazy ? "yes" : "no",
              static_cast<long long>(discovered));
  std::printf("lazy scroll: %lld lines after %d frames\n",
              static_cast<long long>(after_scroll_lines), frames);
  std::printf("full index (info): %.1f ms\n", info_ms);
  std::printf("frame: p50 %.3f ms  p99 %.3f ms  (%.0f fps p50)\n", p50, p99,
              p50 > 0 ? 1000.0 / p50 : 0.0);
  std::printf("cache: %zu entries, %.1f KB\n", view.cache_entries(),
              static_cast<double>(view.cache_bytes()) / 1024.0);
  std::printf("peak RSS: %.1f MB (warm) -> %.1f MB\n", rss_after_warm, rss_after);
  std::printf("gate first paint (< %.0f ms): %s\n", paint_gate,
              paint_pass ? "PASS" : "FAIL");
  std::printf("gate scroll (p99 < 16.7 ms): %s\n", p99 < 16.7 ? "PASS" : "FAIL");
  return (paint_pass && p99 < 16.7) ? 0 : 1;
}
