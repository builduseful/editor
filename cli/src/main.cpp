// cli/src/main.cpp — the `editor` CLI (M0a: version, schema, info, read, write).
#include <fcntl.h>
#include <limits.h>
#include <sys/stat.h>
#include <unistd.h>

#include <algorithm>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <map>
#include <memory>
#include <string>
#include <vector>

#include "editor.hpp"
#include "json.hpp"
#include "json_parse.hpp"

namespace {

constexpr const char* kVersion = "0.1.0";
constexpr const char* kSchema = "editor.result/1";

struct Ctx {
  std::string root;
  uint64_t max_file_bytes = 2ull << 30;
  bool allow_huge = false;
};

std::string cwd_string() {
  char buf[4096];
  if (getcwd(buf, sizeof(buf)) == nullptr) return ".";
  return buf;
}

const char* code_of(Status s) {
  switch (s) {
    case Status::Ok: return nullptr;
    case Status::Io: return "E_IO";
    case Status::NotFound: return "E_NOT_FOUND";
    case Status::Range: return "E_RANGE";
    case Status::Conflict: return "E_CONFLICT";
    case Status::Locked: return "E_LOCKED";
    case Status::Stale: return "E_STALE";
    case Status::Oom: return "E_OOM";
    case Status::InvalidUtf8: return "E_INVALID_UTF8";
    case Status::Binary: return "E_BINARY";
    case Status::TooLarge: return "E_TOO_LARGE";
    case Status::Unsupported: return "E_UNSUPPORTED";
    case Status::Internal: return "E_INTERNAL";
    case Status::BufferTooSmall: return "E_INTERNAL";
    case Status::SavedButNotDurable: return "E_SAVED_BUT_NOT_DURABLE";
  }
  return "E_INTERNAL";
}

int exit_for(const char* code) {
  if (std::strcmp(code, "E_USAGE") == 0 || std::strcmp(code, "E_SCHEMA") == 0) return 3;
  if (std::strcmp(code, "E_OOM") == 0 || std::strcmp(code, "E_INTERNAL") == 0) return 4;
  return 2;
}

bool retryable(const char* code) {
  return std::strcmp(code, "E_IO") == 0 || std::strcmp(code, "E_LOCKED") == 0 ||
         std::strcmp(code, "E_CONFLICT") == 0;
}

std::string eol_str(Eol e) {
  switch (e) {
    case Eol::Lf: return "lf";
    case Eol::CrLf: return "crlf";
    case Eol::Cr: return "cr";
    case Eol::Mixed: return "mixed";
  }
  return "lf";
}

std::string encoding_str(Encoding e) {
  switch (e) {
    case Encoding::Utf8: return "utf-8";
    case Encoding::Utf16Le: return "utf-16le";
    case Encoding::Utf16Be: return "utf-16be";
    case Encoding::Other: return "other";
  }
  return "utf-8";
}

// --- Envelope ---------------------------------------------------------------

struct Env {
  std::string command;
  bool ok = true;
  std::string extra_top;   // e.g. "\"truncated\":true"
  std::string results = "[]";
  std::string error = "null";

  std::string dump() const {
    std::string out = "{";
    out += "\"schema\":" + json::str(kSchema);
    out += ",\"command\":" + json::str(command);
    out += ",\"ok\":" + json::boolean(ok);
    out += ",\"duration_us\":0";
    out += ",\"cwd\":" + json::str(cwd_string());
    out += ",\"warnings\":[]";
    if (!extra_top.empty()) out += "," + extra_top;
    out += ",\"results\":" + results;
    out += ",\"error\":" + error;
    out += ",\"meta\":{\"editor_version\":" + json::str(kVersion) +
           ",\"pid\":" + json::num(static_cast<int64_t>(::getpid())) + "}";
    out += "}";
    return out;
  }
};

std::string error_object(const char* code, const std::string& message,
                         const std::string& path = "",
                         const std::string& extra = "") {
  json::Object o;
  o.set("code", json::str(code));
  o.set("message", json::str(message));
  o.set("retryable", json::boolean(retryable(code)));
  o.set("path", path.empty() ? json::null() : json::str(path));
  if (!extra.empty()) o.set("detail", json::str(extra));
  o.set("hint", json::null());
  return o.dump();
}

struct Run {
  int exit_code = 0;
  std::string out;
};

Run fail_run(const std::string& command, const char* code,
             const std::string& message, const std::string& path = "") {
  Env env;
  env.command = command;
  env.ok = false;
  env.error = error_object(code, message, path);
  Run r;
  r.exit_code = exit_for(code);
  r.out = env.dump();
  return r;
}

// --- Filesystem helpers -----------------------------------------------------

std::string resolve_path(const Ctx& ctx, std::string p) {
  if (p.empty()) return p;
  if (p[0] != '/') p = ctx.root + "/" + p;
  char buf[PATH_MAX];
  if (realpath(p.c_str(), buf) != nullptr) return buf;
  const size_t slash = p.find_last_of('/');
  if (slash == std::string::npos) return p;
  const std::string parent = p.substr(0, slash);
  const std::string name = p.substr(slash + 1);
  if (realpath(parent.c_str(), buf) != nullptr) return std::string(buf) + "/" + name;
  return p;
}

bool file_exists(const std::string& p) {
  struct stat st;
  return ::stat(p.c_str(), &st) == 0;
}

bool inside_root(const std::string& real, const std::string& root) {
  if (root == "/") return !real.empty() && real[0] == '/';
  if (real == root) return true;
  return real.size() > root.size() &&
         real.compare(0, root.size(), root) == 0 && real[root.size()] == '/';
}

// Line starts of a logical document under the §5.1 convention.
std::vector<uint64_t> line_starts_of(const std::string& s) {
  std::vector<uint64_t> out;
  out.push_back(0);
  size_t i = 0;
  while (i < s.size()) {
    if (s[i] == '\n') {
      i += 1;
      out.push_back(i);
    } else if (s[i] == '\r') {
      i += (i + 1 < s.size() && s[i + 1] == '\n') ? 2 : 1;
      out.push_back(i);
    } else {
      i += 1;
    }
  }
  if (out.back() != s.size()) out.push_back(s.size());
  return out;
}

size_t line_index_of(const std::vector<uint64_t>& starts, uint64_t off) {
  // greatest i with starts[i] <= off
  size_t lo = 0, hi = starts.size();
  while (lo + 1 < hi) {
    const size_t mid = lo + (hi - lo) / 2;
    if (starts[mid] <= off) {
      lo = mid;
    } else {
      hi = mid;
    }
  }
  return lo;
}

bool read_stream(std::FILE* f, std::string* out) {
  char buf[65536];
  size_t n = 0;
  while ((n = std::fread(buf, 1, sizeof(buf), f)) > 0) out->append(buf, n);
  return std::ferror(f) == 0;
}

// --- Argument parsing -------------------------------------------------------

struct Args {
  std::vector<std::pair<std::string, std::string>> opts;  // --k v  (v may be "")
  std::vector<std::string> positional;

  bool has(std::string_view k) const {
    for (const auto& o : opts) {
      if (o.first == k) return true;
    }
    return false;
  }
  bool get(std::string_view k, std::string* v) const {
    for (const auto& o : opts) {
      if (o.first == k) {
        *v = o.second;
        return true;
      }
    }
    return false;
  }
};

bool parse_i64(const std::string& s, int64_t* out) {
  if (s.empty()) return false;
  errno = 0;
  char* end = nullptr;
  const long long v = std::strtoll(s.c_str(), &end, 10);
  if (errno != 0 || end == nullptr || *end != '\0') return false;
  *out = v;
  return true;
}

bool parse_u64(const std::string& s, uint64_t* out) {
  if (s.empty() || s[0] == '-') return false;
  errno = 0;
  char* end = nullptr;
  const unsigned long long v = std::strtoull(s.c_str(), &end, 10);
  if (errno != 0 || end == nullptr || *end != '\0') return false;
  *out = v;
  return true;
}

bool parse_rev(const std::string& s, Rev* out) {
  if (s.rfind("stat:", 0) != 0) return false;
  std::vector<std::string> parts;
  size_t pos = 5;
  while (true) {
    const size_t c = s.find(':', pos);
    if (c == std::string::npos) {
      parts.push_back(s.substr(pos));
      break;
    }
    parts.push_back(s.substr(pos, c - pos));
    pos = c + 1;
  }
  if (parts.size() != 5) return false;
  uint64_t v[5] = {0, 0, 0, 0, 0};
  for (int i = 0; i < 5; ++i) {
    if (!parse_u64(parts[static_cast<size_t>(i)], &v[i])) return false;
  }
  out->dev = v[0];
  out->ino = v[1];
  out->size = v[2];
  out->mtime_ns = v[3];
  out->ctime_ns = v[4];
  return true;
}

bool rev_eq(const Rev& a, const Rev& b) {
  return a.dev == b.dev && a.ino == b.ino && a.size == b.size &&
         a.mtime_ns == b.mtime_ns && a.ctime_ns == b.ctime_ns;
}

bool read_text_value(const Ctx& ctx, const std::string& v, std::string* out) {
  if (v == "-") return read_stream(stdin, out);
  if (!v.empty() && v[0] == '@') {
    const std::string fp = resolve_path(ctx, v.substr(1));
    std::FILE* f = std::fopen(fp.c_str(), "rb");
    if (f == nullptr) return false;
    const bool ok = read_stream(f, out);
    std::fclose(f);
    return ok;
  }
  *out = v;
  return true;
}

// Options that take a value.
bool takes_value(std::string_view k) {
  return k == "--from" || k == "--to" || k == "--from-offset" ||
         k == "--length" || k == "--max-bytes" || k == "--root" ||
         k == "--max-file-bytes" || k == "--old" || k == "--new" ||
         k == "--at" || k == "--op" || k == "--text" || k == "--count" ||
         k == "--expect" || k == "--file";
}

// --- Commands ---------------------------------------------------------------

Run cmd_version() {
  Env env;
  env.command = "version";
  env.results = json::Array().add(json::Object().set("version", json::str(kVersion)).dump()).dump();
  Run r;
  r.out = env.dump();
  return r;
}

Run cmd_schema(const std::vector<std::string>& args) {
  const char* kCommands[] = {"read", "write", "edit", "info", "validate", "batch",
                             "version", "schema"};
  if (!args.empty()) {
    bool known = false;
    for (const char* c : kCommands) {
      if (args[0] == c) known = true;
    }
    if (!known) return fail_run("schema", "E_USAGE", "unknown command", args[0]);
  }
  json::Array commands;
  for (const char* c : kCommands) {
    if (!args.empty() && args[0] != c) continue;
    commands.add(json::Object().set("name", json::str(c)).dump());
  }
  Env env;
  env.command = "schema";
  env.results = json::Array().add(json::Object().set("commands", commands.dump()).dump()).dump();
  Run r;
  r.out = env.dump();
  return r;
}

std::string file_fields(const Info& info) {
  return json::Object()
      .set("path", json::str(info.path))
      .set("bytes", json::unum(info.bytes))
      .set("line_count", json::num(info.line_count))
      .set("eol", json::str(eol_str(info.eol)))
      .set("encoding", json::str(encoding_str(info.encoding)))
      .set("bom", json::boolean(info.bom))
      .set("final_newline", json::boolean(info.final_newline))
      .set("rev", json::rev_object(info.rev.dev, info.rev.ino, info.rev.size,
                                   info.rev.mtime_ns, info.rev.ctime_ns))
      .dump();
}

Run cmd_info(const Ctx& ctx, const std::string& raw_path) {
  const std::string path = resolve_path(ctx, raw_path);
  if (!inside_root(path, ctx.root)) {
    return fail_run("info", "E_OUTSIDE_ROOT", "path outside --root", path);
  }
  std::unique_ptr<FileSystem> fs;
  if (create_file_system(&fs) != Status::Ok) return fail_run("info", "E_INTERNAL", "fs");
  std::unique_ptr<Editor> ed;
  if (create_editor(*fs, &ed) != Status::Ok) return fail_run("info", "E_INTERNAL", "editor");
  const Status s = ed->open(path);
  if (s != Status::Ok) return fail_run("info", code_of(s), "cannot open", path);
  Info info;
  if (ed->info(&info) != Status::Ok) return fail_run("info", "E_INTERNAL", "info", path);

  Env env;
  env.command = "info";
  std::string fields = file_fields(info);
  // Append dirty.
  fields.pop_back();  // drop '}'
  fields += ",\"dirty\":" + json::boolean(info.dirty) + "}";
  env.results = json::Array().add(fields).dump();
  Run r;
  r.out = env.dump();
  return r;
}

// Truncate to a UTF-8 codepoint boundary at or below `cap`.
size_t utf8_truncate(const std::string& s, size_t cap) {
  if (cap >= s.size()) return s.size();
  size_t i = cap;
  while (i > 0 && (static_cast<unsigned char>(s[i]) & 0xC0) == 0x80) i -= 1;
  return i;
}

bool valid_utf8(std::string_view s) {
  size_t i = 0;
  while (i < s.size()) {
    const unsigned char c = static_cast<unsigned char>(s[i]);
    size_t extra = 0;
    if (c < 0x80) { i += 1; continue; }
    else if ((c & 0xE0) == 0xC0) extra = 1;
    else if ((c & 0xF0) == 0xE0) extra = 2;
    else if ((c & 0xF8) == 0xF0) extra = 3;
    else return false;
    if (i + extra >= s.size()) return false;
    for (size_t k = 1; k <= extra; ++k) {
      if ((static_cast<unsigned char>(s[i + k]) & 0xC0) != 0x80) return false;
    }
    i += extra + 1;
  }
  return true;
}

// Lines in `s` under the §5.1 convention (terminators + 1 if last non-empty).
int64_t count_lines(const std::string& s) {
  int64_t lines = 0;
  size_t i = 0;
  while (i < s.size()) {
    if (s[i] == '\n') { lines += 1; i += 1; }
    else if (s[i] == '\r') {
      lines += 1;
      i += (i + 1 < s.size() && s[i + 1] == '\n') ? 2 : 1;
    } else {
      i += 1;
    }
  }
  if (!s.empty() && s.back() != '\n' && s.back() != '\r') lines += 1;
  return lines;
}

Run cmd_read(const Ctx& ctx, const std::string& raw_path, const Args& args) {
  const std::string path = resolve_path(ctx, raw_path);
  if (!inside_root(path, ctx.root)) {
    return fail_run("read", "E_OUTSIDE_ROOT", "path outside --root", path);
  }
  std::unique_ptr<FileSystem> fs;
  create_file_system(&fs);
  std::unique_ptr<Editor> ed;
  create_editor(*fs, &ed);
  const Status os = ed->open(path);
  if (os != Status::Ok) return fail_run("read", code_of(os), "cannot open", path);
  Info info;
  ed->info(&info);

  const bool line_axis = args.has("--from") || args.has("--to");
  const bool byte_axis = args.has("--from-offset") || args.has("--length");
  if (line_axis && byte_axis) {
    return fail_run("read", "E_USAGE", "line and byte axes are mutually exclusive", path);
  }

  uint64_t max_bytes = 256 * 1024;
  std::string mv;
  if (args.get("--max-bytes", &mv) && !parse_u64(mv, &max_bytes)) {
    return fail_run("read", "E_USAGE", "--max-bytes must be >= 0", path);
  }

  std::string content;
  int64_t from = 0, to = 0;
  bool truncated = false;

  if (byte_axis) {
    int64_t off = 0;
    uint64_t length = UINT64_MAX;
    std::string v;
    if (args.get("--from-offset", &v)) {
      if (!parse_i64(v, &off) || off < 0) return fail_run("read", "E_USAGE", "bad --from-offset", path);
    }
    if (args.get("--length", &v)) {
      if (!parse_u64(v, &length)) return fail_run("read", "E_USAGE", "bad --length", path);
    }
    size_t need = 0;
    Status s = ed->read_range(off, length == UINT64_MAX ? SIZE_MAX : static_cast<size_t>(length),
                              nullptr, 0, &need);
    if (s != Status::Ok) return fail_run("read", code_of(s), "read", path);
    content.resize(need);
    size_t got = 0;
    s = ed->read_range(off, length == UINT64_MAX ? SIZE_MAX : static_cast<size_t>(length),
                       content.data(), content.size(), &got);
    if (s != Status::Ok) return fail_run("read", code_of(s), "read", path);
    content.resize(got);
    from = off;
    to = off + static_cast<int64_t>(got);
  } else {
    int64_t from_line = 1;
    int64_t to_line = info.line_count;
    std::string v;
    if (args.get("--from", &v)) {
      if (!parse_i64(v, &from_line) || from_line < 1) return fail_run("read", "E_USAGE", "bad --from", path);
    }
    if (args.get("--to", &v)) {
      if (!parse_i64(v, &to_line) || to_line < 1) return fail_run("read", "E_USAGE", "bad --to", path);
    }
    if (from_line > info.line_count) {
      from = from_line;
      to = 0;
    } else {
      int64_t last = to_line;
      if (last > info.line_count) last = info.line_count;
      if (from_line > last) {
        from = from_line;
        to = 0;
      } else {
        size_t need = 0;
        int64_t n = 0;
        Status s = ed->read_lines(from_line - 1, last, nullptr, 0, &need, &n);
        if (s != Status::Ok) return fail_run("read", code_of(s), "read", path);
        content.resize(need);
        size_t got = 0;
        s = ed->read_lines(from_line - 1, last, content.data(), content.size(), &got, &n);
        if (s != Status::Ok) return fail_run("read", code_of(s), "read", path);
        content.resize(got);
        from = from_line;
        to = last;
      }
    }
  }

  if (max_bytes != 0 && content.size() > max_bytes) {
    content.resize(utf8_truncate(content, static_cast<size_t>(max_bytes)));
    truncated = true;
  }
  if (!valid_utf8(content)) {
    return fail_run("read", "E_INVALID_UTF8", "content is not valid UTF-8", path);
  }

  // Report the range actually returned on the request axis.
  if (byte_axis) {
    to = from + static_cast<int64_t>(content.size());
  } else {
    const int64_t lines = count_lines(content);
    to = lines == 0 ? 0 : from + lines - 1;
  }

  Env env;
  env.command = "read";
  std::string fields = file_fields(info);
  fields.pop_back();
  fields += ",\"content\":" + json::str(content);
  fields += ",\"from\":" + json::num(from);
  fields += ",\"to\":" + json::num(to);
  fields += ",\"truncated\":" + json::boolean(truncated) + "}";
  env.results = json::Array().add(fields).dump();
  Run r;
  r.out = env.dump();
  return r;
}

Run cmd_write(const Ctx& ctx, const std::string& raw_path, const Args& args) {
  const std::string path = resolve_path(ctx, raw_path);
  if (!inside_root(path, ctx.root)) {
    return fail_run("write", "E_OUTSIDE_ROOT", "path outside --root", path);
  }
  const bool use_stdin = args.has("--stdin");
  std::string file_arg;
  const bool use_file = args.get("@file", &file_arg);
  if (use_stdin == use_file) {
    return fail_run("write", "E_USAGE", "exactly one of --stdin or @FILE", path);
  }
  if (args.has("--expect") && args.has("--force")) {
    return fail_run("write", "E_USAGE", "--expect and --force are mutually exclusive", path);
  }
  const bool in_place = args.has("--in-place");

  std::string payload;
  if (use_stdin) {
    if (!read_stream(stdin, &payload)) return fail_run("write", "E_IO", "stdin read failed", path);
  } else {
    const std::string fp = resolve_path(ctx, file_arg);
    std::FILE* f = std::fopen(fp.c_str(), "rb");
    if (f == nullptr) return fail_run("write", "E_NOT_FOUND", "payload file not found", fp);
    const bool ok = read_stream(f, &payload);
    std::fclose(f);
    if (!ok) return fail_run("write", "E_IO", "payload read failed", fp);
  }

  const bool exists = file_exists(path);
  if (exists && !args.has("--expect") && !args.has("--force")) {
    return fail_run("write", "E_USAGE", "existing target requires --expect or --force", path);
  }
  if (!exists && args.has("--expect")) {
    return fail_run("write", "E_USAGE", "--expect is not valid for a new file", path);
  }
  if (in_place && !exists) {
    return fail_run("write", "E_USAGE", "--in-place requires an existing target", path);
  }

  std::unique_ptr<FileSystem> fs;
  create_file_system(&fs);
  std::unique_ptr<Editor> ed;
  create_editor(*fs, &ed);

  const bool created = !exists;
  Status s = created ? ed->create(path) : ed->open(path);
  if (s != Status::Ok) return fail_run("write", code_of(s), "cannot open", path);

  Info before;
  ed->info(&before);

  // Current logical document size (BOM excluded).
  size_t logical = 0;
  int64_t n = 0;
  ed->read_lines(0, before.line_count, nullptr, 0, &logical, &n);

  bool noop = false;
  if (!created) {
    std::string cur(logical, '\0');
    if (logical > 0) {
      size_t got = 0;
      ed->read_lines(0, before.line_count, cur.data(), cur.size(), &got, &n);
      cur.resize(got);
    }
    noop = (cur == payload);
  }

  if (!noop) {
    Edit e;
    e.line = 0;
    e.col = 0;
    e.removed_len = static_cast<int64_t>(logical);
    e.text = payload;
    const Edit edits[1] = {e};
    s = ed->apply(edits);
    if (s != Status::Ok) return fail_run("write", code_of(s), "apply failed", path);
  }

  SaveInfo si;
  s = ed->save(nullptr, in_place, &si);
  if (s != Status::Ok) {
    const std::string msg = ed->errmsg().empty() ? std::string("save failed")
                                                 : std::string(ed->errmsg());
    return fail_run("write", code_of(s), msg, path);
  }

  Info after;
  ed->info(&after);

  Env env;
  env.command = "write";
  std::string fields = json::Object()
                           .set("path", json::str(after.path))
                           .set("bytes", json::unum(after.bytes))
                           .set("line_count", json::num(after.line_count))
                           .set("eol", json::str(eol_str(after.eol)))
                           .set("encoding", json::str(encoding_str(after.encoding)))
                           .set("bom", json::boolean(after.bom))
                           .set("final_newline", json::boolean(after.final_newline))
                           .set("rev", json::rev_object(after.rev.dev, after.rev.ino,
                                                       after.rev.size, after.rev.mtime_ns,
                                                       after.rev.ctime_ns))
                           .dump();
  fields.pop_back();
  if (!created) {
    fields += ",\"rev_before\":" + json::rev_object(before.rev.dev, before.rev.ino,
                                                    before.rev.size, before.rev.mtime_ns,
                                                    before.rev.ctime_ns);
    fields += ",\"bytes_before\":" + json::unum(before.bytes);
  }
  fields += ",\"bytes_written\":" + json::unum(si.bytes_written);
  fields += ",\"save\":" + json::str(si.method);
  if (in_place) fields += ",\"in_place\":true";
  if (!noop && !(created && after.bytes == 0)) {
    uint64_t from = created ? 0 : before.bom ? 3 : 0;
    uint64_t to = std::max(before.bytes, after.bytes);
    if (created) to = after.bytes;
    fields += ",\"changed\":{\"from\":" + json::unum(from) + ",\"to\":" + json::unum(to) + "}";
  }
  fields += "}";
  env.results = json::Array().add(fields).dump();
  Run r;
  r.out = env.dump();
  return r;
}

Run cmd_edit(const Ctx& ctx, const std::string& raw_path, const Args& args) {
  const std::string path = resolve_path(ctx, raw_path);
  if (!inside_root(path, ctx.root)) {
    return fail_run("edit", "E_OUTSIDE_ROOT", "path outside --root", path);
  }

  const bool has_expect = args.has("--expect");
  const bool has_force = args.has("--force");
  if (has_expect && has_force) {
    return fail_run("edit", "E_USAGE", "--expect and --force are mutually exclusive", path);
  }
  if (!has_expect && !has_force) {
    return fail_run("edit", "E_USAGE", "one of --expect or --force is required", path);
  }

  const bool form_a = args.has("--old") || args.has("--new");
  const bool form_b = args.has("--at") || args.has("--op");
  if (form_a && form_b) {
    return fail_run("edit", "E_USAGE", "the two edit forms are mutually exclusive", path);
  }
  if (!form_a && !form_b) return fail_run("edit", "E_USAGE", "an edit form is required", path);

  const bool in_place = args.has("--in-place");
  const bool dry_run = args.has("--dry-run");

  std::unique_ptr<FileSystem> fs;
  create_file_system(&fs);
  std::unique_ptr<Editor> ed;
  create_editor(*fs, &ed);
  const Status os = ed->open(path);
  if (os != Status::Ok) return fail_run("edit", code_of(os), "cannot open", path);
  Info before;
  ed->info(&before);

  if (has_expect) {
    std::string spec;
    args.get("--expect", &spec);
    Rev want{};
    if (!parse_rev(spec, &want)) return fail_run("edit", "E_USAGE", "bad --expect", path);
    if (!rev_eq(want, before.rev)) return fail_run("edit", "E_CONFLICT", "expected rev mismatch", path);
  }

  std::string doc;
  {
    size_t need = 0;
    int64_t n = 0;
    ed->read_lines(0, before.line_count, nullptr, 0, &need, &n);
    doc.resize(need);
    if (need > 0) {
      size_t got = 0;
      ed->read_lines(0, before.line_count, doc.data(), doc.size(), &got, &n);
      doc.resize(got);
    }
  }

  std::vector<Edit> edits;
  std::string edit_text;  // stable storage for edit text views
  uint64_t ch_from = 0, ch_to = 0;  // logical offsets
  bool noop = false;

  if (form_a) {
    for (const char* k : {"--at", "--op", "--text", "--length"}) {
      if (args.has(k)) return fail_run("edit", "E_USAGE", "flag not valid with --old/--new", path);
    }
    std::string oldv, newv;
    if (!args.get("--old", &oldv)) return fail_run("edit", "E_USAGE", "--old is required", path);
    if (!args.get("--new", &newv)) return fail_run("edit", "E_USAGE", "--new is required", path);
    if (oldv == "-" && newv == "-") {
      return fail_run("edit", "E_USAGE", "--old and --new cannot both be -", path);
    }
    std::string needle, repl;
    if (!read_text_value(ctx, oldv, &needle)) return fail_run("edit", "E_IO", "cannot read --old", path);
    if (!read_text_value(ctx, newv, &repl)) return fail_run("edit", "E_IO", "cannot read --new", path);
    if (needle.empty()) return fail_run("edit", "E_USAGE", "--old must be non-empty", path);

    const bool all = args.has("--all");
    std::string cv;
    const bool have_count = args.get("--count", &cv);
    if (all && have_count) {
      return fail_run("edit", "E_USAGE", "--all and --count are mutually exclusive", path);
    }
    int64_t count = 0;
    if (have_count && (!parse_i64(cv, &count) || count < 1)) {
      return fail_run("edit", "E_USAGE", "--count must be >= 1", path);
    }

    std::vector<size_t> matches;
    size_t at = 0;
    while (at + needle.size() <= doc.size()) {
      const size_t m = doc.find(needle, at);
      if (m == std::string::npos) break;
      matches.push_back(m);
      at = m + needle.size();
    }
    if (matches.empty()) return fail_run("edit", "E_NO_MATCH", "no match", path);
    if (!all && !have_count && matches.size() != 1) {
      return fail_run("edit", "E_AMBIGUOUS", "multiple matches", path);
    }
    if (have_count && matches.size() < static_cast<size_t>(count)) {
      return fail_run("edit", "E_NO_MATCH", "fewer matches than --count", path);
    }

    size_t take = all ? matches.size() : (have_count ? static_cast<size_t>(count) : 1);
    if (take > matches.size()) take = matches.size();

    const std::vector<uint64_t> starts = line_starts_of(doc);
    edit_text = repl;
    for (size_t i = 0; i < take; ++i) {
      const size_t m = matches[i];
      const size_t li = line_index_of(starts, m);
      Edit e;
      e.line = static_cast<int64_t>(li);
      e.col = static_cast<int64_t>(m - starts[li]);
      e.removed_len = static_cast<int64_t>(needle.size());
      e.text = edit_text;
      edits.push_back(e);
    }
    ch_from = matches[0];
    ch_to = matches[take - 1] + std::max(needle.size(), repl.size());
    noop = (needle == repl);
  } else {
    for (const char* k : {"--old", "--new", "--all", "--count"}) {
      if (args.has(k)) return fail_run("edit", "E_USAGE", "flag not valid with --at/--op", path);
    }
    std::string at_s, op;
    if (!args.get("--at", &at_s)) return fail_run("edit", "E_USAGE", "--at is required", path);
    if (!args.get("--op", &op)) return fail_run("edit", "E_USAGE", "--op is required", path);
    if (op != "insert" && op != "replace" && op != "delete") {
      return fail_run("edit", "E_USAGE", "bad --op", path);
    }
    int64_t line1 = 0, col = 0;
    const size_t colon = at_s.find(':');
    const std::string line_s = colon == std::string::npos ? at_s : at_s.substr(0, colon);
    if (!parse_i64(line_s, &line1)) return fail_run("edit", "E_USAGE", "bad --at", path);
    if (line1 < 1) return fail_run("edit", "E_USAGE", "--at line must be >= 1", path);
    if (colon != std::string::npos) {
      if (!parse_i64(at_s.substr(colon + 1), &col) || col < 0) {
        return fail_run("edit", "E_USAGE", "bad --at column", path);
      }
    }
    const bool has_text = args.has("--text");
    const bool has_length = args.has("--length");
    std::string text;
    if (has_text) {
      std::string tv;
      args.get("--text", &tv);
      if (!read_text_value(ctx, tv, &text)) return fail_run("edit", "E_IO", "cannot read --text", path);
    }
    uint64_t length = 0;
    if (has_length) {
      std::string lv;
      args.get("--length", &lv);
      if (!parse_u64(lv, &length)) return fail_run("edit", "E_USAGE", "bad --length", path);
    }
    if ((op == "insert" || op == "replace") && !has_text) {
      return fail_run("edit", "E_USAGE", "--op insert|replace require --text", path);
    }
    if ((op == "replace" || op == "delete") && !has_length) {
      return fail_run("edit", "E_USAGE", "--op replace|delete require --length", path);
    }
    if (op == "insert" && has_length) {
      return fail_run("edit", "E_USAGE", "--length is not valid with --op insert", path);
    }
    if (line1 > before.line_count + 1) return fail_run("edit", "E_RANGE", "line past EOF", path);
    if (line1 == before.line_count + 1 && col != 0) {
      return fail_run("edit", "E_RANGE", "column must be 0 when appending at EOF", path);
    }

    const std::vector<uint64_t> starts = line_starts_of(doc);
    const uint64_t off = starts[static_cast<size_t>(line1 - 1)] + static_cast<uint64_t>(col);
    if (off > doc.size()) return fail_run("edit", "E_RANGE", "column past end of line", path);

    Edit e;
    e.line = line1 - 1;
    e.col = col;
    e.removed_len = (op == "insert") ? 0 : static_cast<int64_t>(length);
    edit_text = text;
    if (op != "delete") e.text = edit_text;
    edits.push_back(e);

    ch_from = off;
    ch_to = off + std::max<uint64_t>(static_cast<uint64_t>(e.removed_len), text.size());
    if (op == "insert") {
      noop = text.empty();
    } else if (op == "delete") {
      noop = (length == 0);
    } else {
      noop = (length == text.size() && off + length <= doc.size() &&
              doc.compare(off, length, text) == 0);
    }
  }

  // Predicted post-edit document (only needed for --dry-run reporting).
  if (dry_run) {
    std::string predicted;
    {
      const std::vector<uint64_t> starts = line_starts_of(doc);
      size_t cursor = 0;
      for (const Edit& e : edits) {
        const uint64_t o =
            starts[static_cast<size_t>(e.line)] + static_cast<uint64_t>(e.col);
        predicted.append(doc, cursor, o - cursor);
        predicted.append(e.text.data(), e.text.size());
        cursor = o + static_cast<uint64_t>(e.removed_len);
      }
      predicted.append(doc, cursor, doc.size() - cursor);
    }
    const uint64_t new_logical = predicted.size();
    std::string method = "none";
    if (!noop) {
      if (new_logical == doc.size()) {
        method = "clone_pwrite";
      } else if ((new_logical - ch_to) * 4 <= new_logical) {
        method = "clone_pwrite_tail";
      } else {
        method = "full_rewrite";
      }
    }
    Env env;
    env.command = "edit";
    std::string fields = json::Object()
                             .set("path", json::str(path))
                             .set("bytes", json::unum(new_logical + (before.bom ? 3 : 0)))
                             .set("line_count", json::num(count_lines(predicted)))
                             .set("eol", json::str(eol_str(before.eol)))
                             .set("encoding", json::str(encoding_str(before.encoding)))
                             .set("bom", json::boolean(before.bom))
                             .set("final_newline",
                                  json::boolean(!predicted.empty() &&
                                                (predicted.back() == '\n' ||
                                                 predicted.back() == '\r')))
                             .set("rev_before", json::rev_object(
                                                    before.rev.dev, before.rev.ino,
                                                    before.rev.size, before.rev.mtime_ns,
                                                    before.rev.ctime_ns))
                             .set("bytes_before", json::unum(before.bytes))
                             .set("save", json::str(method))
                             .dump();
    fields.pop_back();
    if (!noop) {
      fields += ",\"changed\":{\"from\":" + json::unum(ch_from + (before.bom ? 3 : 0)) +
                ",\"to\":" + json::unum(ch_to + (before.bom ? 3 : 0)) + "}";
    }
    if (in_place) fields += ",\"in_place\":true";
    fields += "}";
    env.results = json::Array().add(fields).dump();
    Run r;
    r.out = env.dump();
    return r;
  }

  if (!noop) {
    const Status as = ed->apply(edits);
    if (as != Status::Ok) {
      const std::string msg = ed->errmsg().empty() ? std::string("apply failed")
                                                   : std::string(ed->errmsg());
      return fail_run("edit", code_of(as), msg, path);
    }
  }

  Rev expect{};
  const Rev* expect_ptr = nullptr;
  if (has_expect) {
    std::string spec;
    args.get("--expect", &spec);
    parse_rev(spec, &expect);
    expect_ptr = &expect;
  }
  SaveInfo si;
  const Status ss = ed->save(expect_ptr, in_place, &si);
  if (ss != Status::Ok) {
    const std::string msg = ed->errmsg().empty() ? std::string("save failed")
                                                 : std::string(ed->errmsg());
    return fail_run("edit", code_of(ss), msg, path);
  }

  Info after;
  ed->info(&after);

  Env env;
  env.command = "edit";
  std::string fields = json::Object()
                           .set("path", json::str(after.path))
                           .set("bytes", json::unum(after.bytes))
                           .set("line_count", json::num(after.line_count))
                           .set("eol", json::str(eol_str(after.eol)))
                           .set("encoding", json::str(encoding_str(after.encoding)))
                           .set("bom", json::boolean(after.bom))
                           .set("final_newline", json::boolean(after.final_newline))
                           .set("rev_before", json::rev_object(
                                                  before.rev.dev, before.rev.ino,
                                                  before.rev.size, before.rev.mtime_ns,
                                                  before.rev.ctime_ns))
                           .set("rev", json::rev_object(after.rev.dev, after.rev.ino,
                                                        after.rev.size, after.rev.mtime_ns,
                                                        after.rev.ctime_ns))
                           .set("bytes_before", json::unum(before.bytes))
                           .set("bytes_written", json::unum(si.bytes_written))
                           .set("save", json::str(si.method))
                           .dump();
  fields.pop_back();
  if (!noop) {
    fields += ",\"changed\":{\"from\":" + json::unum(ch_from + (before.bom ? 3 : 0)) +
              ",\"to\":" + json::unum(ch_to + (before.bom ? 3 : 0)) + "}";
  }
  if (in_place) fields += ",\"in_place\":true";
  fields += "}";
  env.results = json::Array().add(fields).dump();
  Run r;
  r.out = env.dump();
  return r;
}

// --- validate ---------------------------------------------------------------

struct Finding {
  int64_t line = 1;
  int64_t col = 0;
  uint64_t byte_offset = 0;
  const char* code = "invalid_utf8";
};

std::string finding_json(const Finding& f) {
  return json::Object()
      .set("line", json::num(f.line))
      .set("col", json::num(f.col))
      .set("byte_offset", json::unum(f.byte_offset))
      .set("code", json::str(f.code))
      .dump();
}

// Incremental UTF-8 validator: reports the raw offset of each malformed
// sequence. State persists across chunk boundaries.
struct Utf8Scan {
  unsigned need = 0;
  unsigned cp = 0;
  unsigned min_cp = 0;
  uint64_t seq_start = 0;

  template <class Report>
  void feed(const char* p, size_t n, uint64_t base, Report&& report) {
    size_t i = 0;
    while (i < n) {
      const unsigned char c = static_cast<unsigned char>(p[i]);
      if (need == 0) {
        if (c < 0x80) {
          i += 1;
          continue;
        }
        if ((c & 0xE0) == 0xC0) {
          need = 1; cp = c & 0x1Fu; min_cp = 0x80;
        } else if ((c & 0xF0) == 0xE0) {
          need = 2; cp = c & 0x0Fu; min_cp = 0x800;
        } else if ((c & 0xF8) == 0xF0) {
          need = 3; cp = c & 0x07u; min_cp = 0x10000;
        } else {
          report(base + i);
          i += 1;
          continue;
        }
        seq_start = base + i;
        i += 1;
        continue;
      }
      if ((c & 0xC0) != 0x80) {
        report(seq_start);
        need = 0;
        continue;  // reprocess this byte
      }
      cp = (cp << 6) | (c & 0x3Fu);
      need -= 1;
      i += 1;
      if (need == 0) {
        const bool bad = cp < min_cp || cp > 0x10FFFF ||
                         (cp >= 0xD800 && cp <= 0xDFFF);
        if (bad) report(seq_start);
      }
    }
  }
};

Run cmd_validate(const Ctx& ctx, const std::string& raw_path) {
  const std::string path = resolve_path(ctx, raw_path);
  if (!inside_root(path, ctx.root)) {
    return fail_run("validate", "E_OUTSIDE_ROOT", "path outside --root", path);
  }

  std::unique_ptr<FileSystem> fs;
  create_file_system(&fs);
  int fd = -1;
  Status s = fs->open_readonly(path, true, &fd);
  if (s != Status::Ok) return fail_run("validate", code_of(s), "cannot open", path);
  Stat st;
  s = fs->fstat(fd, &st);
  if (s != Status::Ok) {
    fs->close_fd(fd);
    return fail_run("validate", code_of(s), "fstat failed", path);
  }
  if (!st.regular) {
    fs->close_fd(fd);
    return fail_run("validate", "E_IO", "not a regular file", path);
  }
  if (!ctx.allow_huge && st.size > ctx.max_file_bytes) {
    fs->close_fd(fd);
    return fail_run("validate", "E_TOO_LARGE", "file exceeds --max-file-bytes", path);
  }

  char head[8192];
  size_t got = 0;
  s = fs->pread(fd, 0, head, sizeof(head), &got);
  if (s != Status::Ok) {
    fs->close_fd(fd);
    return fail_run("validate", code_of(s), "read failed", path);
  }

  bool utf8_bom = false;
  uint64_t bom_len = 0;
  if (got >= 3 && static_cast<unsigned char>(head[0]) == 0xEF &&
      static_cast<unsigned char>(head[1]) == 0xBB &&
      static_cast<unsigned char>(head[2]) == 0xBF) {
    utf8_bom = true;
    bom_len = 3;
  }

  Encoding enc = Encoding::Utf8;
  std::vector<Finding> errors;
  bool truncated = false;
  bool early = false;
  std::string early_code;

  if (got >= 2 && ((static_cast<unsigned char>(head[0]) == 0xFF &&
                    static_cast<unsigned char>(head[1]) == 0xFE) ||
                   (static_cast<unsigned char>(head[0]) == 0xFE &&
                    static_cast<unsigned char>(head[1]) == 0xFF))) {
    enc = static_cast<unsigned char>(head[0]) == 0xFF ? Encoding::Utf16Le
                                                      : Encoding::Utf16Be;
    errors.push_back(Finding{1, 0, 0, "unsupported"});
    early = true;
    early_code = "unsupported";
  } else {
    for (size_t i = 0; i < got; ++i) {
      if (head[i] == '\0') {
        errors.push_back(
            Finding{1, 0, static_cast<uint64_t>(i), "binary"});
        early = true;
        early_code = "binary";
        break;
      }
    }
  }

  int64_t line_count = 0;
  Eol eol = Eol::Lf;
  bool final_newline = false;
  bool bom = utf8_bom;

  if (!early) {
    // Single streaming pass: line/terminator tracking + UTF-8 validation.
    int64_t line = 1;
    uint64_t line_start = bom_len;
    bool prev_cr = false;
    uint64_t lf = 0, crlf = 0, cr = 0;
    Eol first_seen = Eol::Lf;
    bool have_first = false;
    bool any = false;
    uint64_t last_byte_off = 0;
    char last_byte = 0;
    Utf8Scan scan;

    auto report = [&](uint64_t off) {
      if (errors.size() >= 100) {
        truncated = true;
        return;
      }
      errors.push_back(Finding{line, static_cast<int64_t>(off - line_start), off,
                               "invalid_utf8"});
    };

    char buf[1 << 20];
    uint64_t pos = bom_len;
    while (pos < st.size) {
      const uint64_t want = std::min<uint64_t>(sizeof(buf), st.size - pos);
      size_t n = 0;
      s = fs->pread(fd, pos, buf, static_cast<size_t>(want), &n);
      if (s != Status::Ok) {
        fs->close_fd(fd);
        return fail_run("validate", code_of(s), "read failed", path);
      }
      if (n == 0) break;
      scan.feed(buf, n, pos, report);
      for (size_t i = 0; i < n; ++i) {
        const char c = buf[i];
        const uint64_t off = pos + i;
        any = true;
        last_byte = c;
        last_byte_off = off;
        if (c == '\n') {
          if (prev_cr) {
            crlf += 1;
          } else {
            lf += 1;
          }
          if (!have_first) {
            first_seen = prev_cr ? Eol::CrLf : Eol::Lf;
            have_first = true;
          }
          prev_cr = false;
          line += 1;
          line_start = off + 1;
        } else if (c == '\r') {
          if (prev_cr) {
            cr += 1;
            if (!have_first) {
              first_seen = Eol::Cr;
              have_first = true;
            }
            line += 1;
            line_start = off;
          }
          prev_cr = true;
        } else {
          if (prev_cr) {
            cr += 1;
            if (!have_first) {
              first_seen = Eol::Cr;
              have_first = true;
            }
            line += 1;
            line_start = off;
            prev_cr = false;
          }
        }
      }
      pos += n;
    }
    if (scan.need != 0) report(scan.seq_start);
    if (prev_cr) {
      cr += 1;
      if (!have_first) {
        first_seen = Eol::Cr;
        have_first = true;
      }
      line += 1;
      line_start = st.size;
    }
    (void)line_start;
    (void)any;

    const uint64_t terminators = lf + crlf + cr;
    final_newline = st.size > 0 &&
                    (last_byte == '\n' || last_byte == '\r');
    line_count = static_cast<int64_t>(terminators) + (final_newline ? 0 : 1);
    if (st.size == bom_len) line_count = 0;
    if (terminators == 0) {
      eol = Eol::Lf;
    } else {
      const uint64_t mx = std::max(lf, std::max(crlf, cr));
      const int winners =
          (lf == mx ? 1 : 0) + (crlf == mx ? 1 : 0) + (cr == mx ? 1 : 0);
      if (winners == 1) {
        eol = (lf == mx) ? Eol::Lf : (crlf == mx ? Eol::CrLf : Eol::Cr);
      } else {
        eol = first_seen;
      }
    }
    (void)last_byte_off;
  }

  fs->close_fd(fd);

  Env env;
  env.command = "validate";
  json::Array errs;
  for (const Finding& f : errors) errs.add(finding_json(f));

  std::string fields = json::Object()
                           .set("path", json::str(path))
                           .set("bytes", json::unum(st.size))
                           .set("line_count", json::num(line_count))
                           .set("eol", json::str(eol_str(eol)))
                           .set("encoding", json::str(encoding_str(enc)))
                           .set("bom", json::boolean(bom))
                           .set("final_newline", json::boolean(final_newline))
                           .set("rev", json::rev_object(
                                          st.dev, st.ino, st.size, st.mtime_ns,
                                          st.ctime_ns))
                           .dump();
  fields.pop_back();
  fields += ",\"errors\":" + errs.dump() + "}";
  env.results = json::Array().add(fields).dump();
  env.ok = errors.empty();
  if (truncated) env.extra_top = "\"truncated\":true";
  if (!errors.empty()) {
    const Finding& f = errors.front();
    const char* code = std::strcmp(f.code, "unsupported") == 0
                           ? "E_UNSUPPORTED"
                           : (std::strcmp(f.code, "binary") == 0 ? "E_BINARY"
                                                                 : "E_INVALID_UTF8");
    std::string e = "{";
    e += "\"code\":" + json::str(code);
    e += ",\"message\":" + json::str("validation failed");
    e += ",\"retryable\":false";
    e += ",\"path\":" + json::str(path);
    e += ",\"line\":" + json::num(f.line);
    e += ",\"col\":" + json::num(f.col);
    e += ",\"byte_offset\":" + json::unum(f.byte_offset);
    e += ",\"hint\":null}";
    env.error = e;
  }
  Run r;
  r.exit_code = env.ok ? 0 : 2;
  r.out = env.dump();
  return r;
}

// --- batch ------------------------------------------------------------------

std::string dir_name(const std::string& p) {
  const size_t s = p.find_last_of('/');
  if (s == std::string::npos) return ".";
  if (s == 0) return "/";
  return p.substr(0, s);
}

std::string base_name(const std::string& p) {
  const size_t s = p.find_last_of('/');
  return s == std::string::npos ? p : p.substr(s + 1);
}

std::string batch_error(const char* code, const std::string& message,
                        const std::string& path, int op_index) {
  std::string e = "{";
  e += "\"code\":" + json::str(code);
  e += ",\"message\":" + json::str(message);
  e += ",\"retryable\":" + json::boolean(retryable(code));
  e += ",\"path\":" + (path.empty() ? json::null() : json::str(path));
  if (op_index >= 0) e += ",\"op_index\":" + json::num(op_index);
  e += ",\"hint\":null}";
  return e;
}

struct OpError {
  const char* code = nullptr;  // null = ok
  std::string message;
};

struct BatchOp {
  int index = 0;
  std::string kind;  // "write" | "edit"
  std::string path;
  // write
  bool has_content = false;
  std::string content;
  bool has_content_file = false;
  std::string content_file;
  // edit
  bool has_old = false, has_new = false, has_text = false, has_at = false,
       has_opname = false, has_length = false, has_count = false;
  std::string oldv, newv, text, at, opname;
  uint64_t length = 0;
  int64_t count = 0;
  bool all = false;
  bool dry_run = false;
  // common
  bool in_place = false;
  bool force = false;
  bool has_expect = false;
  Rev expect{};
  // computed
  bool existed = false;
  std::vector<Edit> edits;
  std::string edit_text;
  uint64_t ch_from = 0, ch_to = 0;
  uint64_t cover_from = 0, cover_to = 0;
  bool noop = false;
  int64_t pred_line_count = 0;
  uint64_t pred_bytes = 0;
  bool pred_final_nl = false;
  std::string pred_method = "none";
};

bool parse_expect_value(const json::Value& v, Rev* out) {
  if (v.is_string()) return parse_rev(v.text, out);
  if (v.is_object()) {
    const char* keys[5] = {"dev", "ino", "size", "mtime_ns", "ctime_ns"};
    uint64_t vals[5] = {0, 0, 0, 0, 0};
    for (int i = 0; i < 5; ++i) {
      const json::Value* f = v.find(keys[i]);
      if (f == nullptr || !f->is_string() || !parse_u64(f->text, &vals[i])) return false;
    }
    out->dev = vals[0];
    out->ino = vals[1];
    out->size = vals[2];
    out->mtime_ns = vals[3];
    out->ctime_ns = vals[4];
    return true;
  }
  return false;
}

Status read_original(const std::string& path, bool* exists, Info* info,
                     std::string* doc) {
  std::unique_ptr<FileSystem> fs;
  create_file_system(&fs);
  std::unique_ptr<Editor> ed;
  create_editor(*fs, &ed);
  const Status s = ed->open(path);
  if (s == Status::NotFound) {
    *exists = false;
    return Status::Ok;
  }
  if (s != Status::Ok) return s;
  if (ed->info(info) != Status::Ok) return Status::Internal;
  size_t need = 0;
  int64_t n = 0;
  ed->read_lines(0, info->line_count, nullptr, 0, &need, &n);
  doc->resize(need);
  if (need > 0) {
    size_t got = 0;
    ed->read_lines(0, info->line_count, doc->data(), doc->size(), &got, &n);
    doc->resize(got);
  }
  *exists = true;
  return Status::Ok;
}

Run cmd_batch(const Ctx& ctx, const Args& args) {
  const bool have_file = args.has("--file");
  const bool have_stdin = args.has("--stdin");
  const bool check_only = args.has("--check");
  if (have_file && have_stdin) {
    return fail_run("batch", "E_USAGE", "program is stdin or --file, not both", "");
  }

  std::string program;
  bool from_stdin = !have_file;
  if (have_file) {
    std::string f;
    args.get("--file", &f);
    const std::string fp = resolve_path(ctx, f);
    std::FILE* fh = std::fopen(fp.c_str(), "rb");
    if (fh == nullptr) return fail_run("batch", "E_NOT_FOUND", "program file not found", fp);
    const bool ok = read_stream(fh, &program);
    std::fclose(fh);
    if (!ok) return fail_run("batch", "E_IO", "program read failed", fp);
  } else {
    if (!read_stream(stdin, &program)) {
      return fail_run("batch", "E_IO", "stdin read failed", "");
    }
  }

  // Parse JSON-lines.
  std::vector<BatchOp> ops;
  {
    size_t start = 0;
    int idx = 0;
    while (start <= program.size()) {
      size_t nl = program.find('\n', start);
      const std::string line =
          program.substr(start, nl == std::string::npos ? std::string::npos
                                                        : nl - start);
      start = nl == std::string::npos ? program.size() + 1 : nl + 1;
      // Skip blank lines.
      bool blank = true;
      for (char c : line) {
        if (c != ' ' && c != '\t' && c != '\r') {
          blank = false;
          break;
        }
      }
      if (blank) continue;
      json::Value v;
      std::string err;
      if (!json::parse(line, &v, &err) || !v.is_object()) {
        return fail_run("batch", "E_PARSE", "invalid program line: " + err, "");
      }
      BatchOp op;
      op.index = idx++;
      const json::Value* k = v.find("op");
      if (k == nullptr || !k->is_string()) {
        return fail_run("batch", "E_PARSE", "op requires a string \"op\"", "");
      }
      op.kind = k->text;
      if (op.kind != "write" && op.kind != "edit") {
        return fail_run("batch", "E_USAGE", "unknown op: " + op.kind, "");
      }
      const json::Value* p = v.find("path");
      if (p == nullptr || !p->is_string()) {
        return fail_run("batch", "E_USAGE", "op requires a string \"path\"", "");
      }
      ops.push_back(op);
      // keep the raw path for resolution below
      ops.back().path = p->text;
      BatchOp& o = ops.back();
      if (const json::Value* c = v.find("content")) {
        if (!c->is_string()) return fail_run("batch", "E_USAGE", "content must be a string", "");
        o.has_content = true;
        o.content = c->text;
      }
      if (const json::Value* c = v.find("content_file")) {
        if (!c->is_string()) return fail_run("batch", "E_USAGE", "content_file must be a string", "");
        o.has_content_file = true;
        o.content_file = c->text;
      }
      auto get_str = [&](const char* name, bool* has, std::string* dst) {
        if (const json::Value* f = v.find(name)) {
          if (!f->is_string()) return false;
          *has = true;
          *dst = f->text;
        }
        return true;
      };
      if (!get_str("old", &o.has_old, &o.oldv)) return fail_run("batch", "E_USAGE", "old must be a string", "");
      if (!get_str("new", &o.has_new, &o.newv)) return fail_run("batch", "E_USAGE", "new must be a string", "");
      if (!get_str("text", &o.has_text, &o.text)) return fail_run("batch", "E_USAGE", "text must be a string", "");
      if (!get_str("at", &o.has_at, &o.at)) return fail_run("batch", "E_USAGE", "at must be a string", "");
      if (!get_str("op_name", &o.has_opname, &o.opname)) {}
      if (const json::Value* f = v.find("op_flag")) {
        if (f->is_string()) { o.has_opname = true; o.opname = f->text; }
      }
      if (const json::Value* f = v.find("length")) {
        if (!f->is_number() || !parse_u64(f->text, &o.length)) {
          return fail_run("batch", "E_USAGE", "length must be a non-negative integer", "");
        }
        o.has_length = true;
      }
      if (const json::Value* f = v.find("count")) {
        if (!f->is_number() || !parse_i64(f->text, &o.count)) {
          return fail_run("batch", "E_USAGE", "count must be an integer", "");
        }
        o.has_count = true;
      }
      if (const json::Value* f = v.find("all")) {
        if (!f->is_bool()) return fail_run("batch", "E_USAGE", "all must be a boolean", "");
        o.all = f->boolean;
      }
      if (const json::Value* f = v.find("dry_run")) {
        if (!f->is_bool()) return fail_run("batch", "E_USAGE", "dry_run must be a boolean", "");
        o.dry_run = f->boolean;
      }
      if (const json::Value* f = v.find("in_place")) {
        if (!f->is_bool()) return fail_run("batch", "E_USAGE", "in_place must be a boolean", "");
        o.in_place = f->boolean;
      }
      if (const json::Value* f = v.find("force")) {
        if (!f->is_bool()) return fail_run("batch", "E_USAGE", "force must be a boolean", "");
        o.force = f->boolean;
      }
      if (const json::Value* f = v.find("expect")) {
        Rev r{};
        if (!parse_expect_value(*f, &r)) {
          return fail_run("batch", "E_USAGE", "bad expect value", "");
        }
        o.has_expect = true;
        o.expect = r;
      }
    }
  }

  // Validate + resolve each op against the original files. All ops are
  // validated against the originals before any write (validation-stage
  // atomicity); --check collects every failure instead of stopping at the
  // first.
  std::map<std::string, Info> info_by_path;
  std::map<std::string, std::string> doc_by_path;
  std::map<std::string, bool> exists_by_path;
  std::vector<std::pair<OpError, int>> verrors;  // (error, op index)

  auto validate_one = [&](BatchOp& o, bool exists, const Info& info,
                          const std::string& doc) -> OpError {
    if (o.has_expect && o.force) {
      return {"E_USAGE", "expect and force are mutually exclusive"};
    }
    if (o.kind == "write") {
      if (o.has_content == o.has_content_file) {
        return {"E_USAGE", "write needs exactly one of content/content_file"};
      }
      if (from_stdin && o.has_content) {
        return {"E_USAGE", "inline content is not allowed when the program is stdin"};
      }
      std::string payload;
      if (o.has_content_file) {
        const std::string cf = resolve_path(ctx, o.content_file);
        if (!inside_root(cf, ctx.root)) {
          return {"E_OUTSIDE_ROOT", "content_file outside --root"};
        }
        std::FILE* fh = std::fopen(cf.c_str(), "rb");
        if (fh == nullptr) return {"E_NOT_FOUND", "content_file not found"};
        const bool ok = read_stream(fh, &payload);
        std::fclose(fh);
        if (!ok) return {"E_IO", "content_file read failed"};
      } else {
        payload = o.content;
      }
      if (o.in_place && !exists) {
        return {"E_USAGE", "in_place requires an existing target"};
      }
      if (exists && !o.has_expect && !o.force) {
        return {"E_USAGE", "existing target requires expect or force"};
      }
      Edit e;
      e.line = 0;
      e.col = 0;
      e.removed_len = exists ? static_cast<int64_t>(doc.size()) : 0;
      o.edit_text = payload;
      e.text = o.edit_text;
      o.edits.push_back(e);
      o.cover_from = 0;
      o.cover_to = exists ? doc.size() : UINT64_MAX;
      o.ch_from = 0;
      o.ch_to = std::max<uint64_t>(exists ? doc.size() : 0, payload.size());
      o.noop = exists && doc == payload;
      return {};
    }

    const bool form_a = o.has_old || o.has_new;
    const bool form_b = o.has_at || o.has_opname;
    if (form_a == form_b) return {"E_USAGE", "edit needs exactly one form"};
    if (!exists) return {"E_NOT_FOUND", "edit target does not exist"};
    if (!o.has_expect && !o.force) return {"E_USAGE", "edit requires expect or force"};

    if (form_a) {
      if (!o.has_old || !o.has_new) return {"E_USAGE", "old and new are both required"};
      if (o.oldv == "-" || o.newv == "-" || o.has_text) {
        return {"E_USAGE", "old/new/text must be literals"};
      }
      if (o.oldv.empty()) return {"E_USAGE", "old must be non-empty"};
      if (o.all && o.has_count) return {"E_USAGE", "all and count are mutually exclusive"};
      if (o.has_count && o.count < 1) return {"E_USAGE", "count must be >= 1"};
      std::vector<size_t> matches;
      size_t at = 0;
      while (at + o.oldv.size() <= doc.size()) {
        const size_t m = doc.find(o.oldv, at);
        if (m == std::string::npos) break;
        matches.push_back(m);
        at = m + o.oldv.size();
      }
      if (matches.empty()) return {"E_NO_MATCH", "no match"};
      if (!o.all && !o.has_count && matches.size() != 1) {
        return {"E_AMBIGUOUS", "multiple matches"};
      }
      if (o.has_count && matches.size() < static_cast<size_t>(o.count)) {
        return {"E_NO_MATCH", "fewer matches than count"};
      }
      size_t take = o.all ? matches.size()
                          : (o.has_count ? static_cast<size_t>(o.count) : 1);
      if (take > matches.size()) take = matches.size();
      const std::vector<uint64_t> starts = line_starts_of(doc);
      o.edit_text = o.newv;
      for (size_t i = 0; i < take; ++i) {
        const size_t m = matches[i];
        const size_t li = line_index_of(starts, m);
        Edit e;
        e.line = static_cast<int64_t>(li);
        e.col = static_cast<int64_t>(m - starts[li]);
        e.removed_len = static_cast<int64_t>(o.oldv.size());
        e.text = o.edit_text;
        o.edits.push_back(e);
      }
      o.cover_from = matches[0];
      o.cover_to = matches[take - 1] + std::max(o.oldv.size(), o.newv.size());
      o.ch_from = matches[0];
      o.ch_to = o.cover_to;
      o.noop = (o.oldv == o.newv);
      return {};
    }

    if (!o.has_at || !o.has_opname) return {"E_USAGE", "at and op are both required"};
    if (o.opname != "insert" && o.opname != "replace" && o.opname != "delete") {
      return {"E_USAGE", "bad op"};
    }
    int64_t line1 = 0, col = 0;
    const size_t colon = o.at.find(':');
    const std::string ls = colon == std::string::npos ? o.at : o.at.substr(0, colon);
    if (!parse_i64(ls, &line1) || line1 < 1) return {"E_USAGE", "bad at line"};
    if (colon != std::string::npos) {
      if (!parse_i64(o.at.substr(colon + 1), &col) || col < 0) {
        return {"E_USAGE", "bad at column"};
      }
    }
    if ((o.opname == "insert" || o.opname == "replace") && !o.has_text) {
      return {"E_USAGE", "insert/replace require text"};
    }
    if ((o.opname == "replace" || o.opname == "delete") && !o.has_length) {
      return {"E_USAGE", "replace/delete require length"};
    }
    if (o.opname == "insert" && o.has_length) {
      return {"E_USAGE", "length is not valid with insert"};
    }
    if (line1 > info.line_count + 1) return {"E_RANGE", "line past EOF"};
    if (line1 == info.line_count + 1 && col != 0) {
      return {"E_RANGE", "column must be 0 at EOF"};
    }
    const std::vector<uint64_t> starts = line_starts_of(doc);
    const uint64_t off =
        starts[static_cast<size_t>(line1 - 1)] + static_cast<uint64_t>(col);
    if (off > doc.size()) return {"E_RANGE", "column past end of line"};
    Edit e;
    e.line = line1 - 1;
    e.col = col;
    e.removed_len = (o.opname == "insert") ? 0 : static_cast<int64_t>(o.length);
    o.edit_text = o.has_text ? o.text : std::string();
    if (o.opname != "delete") e.text = o.edit_text;
    o.edits.push_back(e);
    o.cover_from = off;
    o.cover_to = off + std::max<uint64_t>(
                           static_cast<uint64_t>(e.removed_len), o.edit_text.size());
    o.ch_from = off;
    o.ch_to = o.cover_to;
    if (o.opname == "insert") {
      o.noop = o.edit_text.empty();
    } else if (o.opname == "delete") {
      o.noop = (o.length == 0);
    } else {
      o.noop = (o.length == o.edit_text.size() && off + o.length <= doc.size() &&
                doc.compare(off, o.length, o.edit_text) == 0);
    }
    return {};
  };

  for (BatchOp& o : ops) {
    const std::string path = resolve_path(ctx, o.path);
    if (!inside_root(path, ctx.root)) {
      verrors.push_back({{"E_OUTSIDE_ROOT", "path outside --root"}, o.index});
      o.path = path;
      continue;
    }
    if (!exists_by_path.count(path)) {
      Info info;
      std::string doc;
      bool exists = false;
      const Status s = read_original(path, &exists, &info, &doc);
      if (s != Status::Ok) {
        verrors.push_back({{code_of(s), "cannot read"}, o.index});
        o.path = path;
        continue;
      }
      exists_by_path[path] = exists;
      info_by_path[path] = info;
      doc_by_path[path] = doc;
    }
    o.path = path;
    o.existed = exists_by_path[path];
    const OpError oe =
        validate_one(o, o.existed, info_by_path[path], doc_by_path[path]);
    if (oe.code != nullptr) verrors.push_back({oe, o.index});
  }

  if (!verrors.empty()) {
    Env env;
    env.command = "batch";
    env.ok = false;
    if (check_only) {
      json::Array errs;
      for (const auto& ve : verrors) {
        errs.add(batch_error(ve.first.code, ve.first.message,
                             ops[static_cast<size_t>(ve.second)].path, ve.second));
      }
      env.extra_top = "\"rolled_back\":false,\"valid\":false,\"errors\":" + errs.dump();
      env.results = "[]";
      const OpError& first = verrors.front().first;
      env.error = batch_error(first.code, first.message,
                              ops[static_cast<size_t>(verrors.front().second)].path,
                              verrors.front().second);
      Run r;
      r.exit_code = 2;
      r.out = env.dump();
      return r;
    }
    env.extra_top = "\"rolled_back\":false";
    env.results = "[]";
    const OpError& first = verrors.front().first;
    env.error = batch_error(first.code, first.message,
                            ops[static_cast<size_t>(verrors.front().second)].path,
                            verrors.front().second);
    Run r;
    r.exit_code = exit_for(first.code);
    r.out = env.dump();
    return r;
  }

  // Overlap check per path (writes cover the whole file).
  {
    std::map<std::string, std::vector<const BatchOp*>> by_path;
    for (const BatchOp& o : ops) by_path[o.path].push_back(&o);
    for (const auto& kv : by_path) {
      std::vector<const BatchOp*> v = kv.second;
      std::sort(v.begin(), v.end(), [](const BatchOp* a, const BatchOp* b) {
        if (a->cover_from != b->cover_from) return a->cover_from < b->cover_from;
        return a->index < b->index;
      });
      for (size_t i = 1; i < v.size(); ++i) {
        const BatchOp* a = v[i - 1];
        const BatchOp* b = v[i];
        if (b->cover_from < a->cover_to || b->cover_from == a->cover_from) {
          Env env;
          env.command = "batch";
          env.ok = false;
          if (check_only) {
            json::Array errs;
            errs.add(batch_error("E_RANGE", "overlapping ops on the same file",
                                 b->path, b->index));
            env.extra_top =
                "\"rolled_back\":false,\"valid\":false,\"errors\":" + errs.dump();
            env.error = batch_error("E_RANGE", "overlapping ops on the same file",
                                    b->path, b->index);
            Run r;
            r.exit_code = 2;
            r.out = env.dump();
            return r;
          }
          env.extra_top = "\"rolled_back\":false";
          env.error = batch_error("E_RANGE", "overlapping ops on the same file",
                                  b->path, b->index);
          Run r;
          r.exit_code = 2;
          r.out = env.dump();
          return r;
        }
      }
    }
  }

  // --check: report validation only.
  if (check_only) {
    Env env;
    env.command = "batch";
    env.extra_top = "\"rolled_back\":false,\"valid\":true,\"errors\":[]";
    env.results = "[]";
    Run r;
    r.out = env.dump();
    return r;
  }

  // Group into units: one write per path, or all edits merged per path.
  struct Unit {
    std::string path;
    std::vector<const BatchOp*> ops;
    bool is_write = false;
    uint64_t order_off = 0;
    int order_op = 0;
  };
  std::vector<Unit> units;
  {
    std::map<std::string, std::vector<const BatchOp*>> by_path;
    for (const BatchOp& o : ops) {
      if (o.dry_run) continue;  // dry-run ops are excluded from units/commits
      by_path[o.path].push_back(&o);
    }
    for (auto& kv : by_path) {
      Unit u;
      u.path = kv.first;
      bool has_write = false;
      for (const BatchOp* o : kv.second) {
        if (o->kind == "write") has_write = true;
      }
      if (has_write) {
        u.is_write = true;
        for (const BatchOp* o : kv.second) {
          if (o->kind == "write") u.ops.push_back(o);
        }
      } else {
        std::vector<const BatchOp*> v = kv.second;
        std::sort(v.begin(), v.end(), [](const BatchOp* a, const BatchOp* b) {
          if (a->cover_from != b->cover_from) return a->cover_from < b->cover_from;
          return a->index < b->index;
        });
        u.ops = v;
      }
      u.order_off = u.ops.empty() ? 0 : u.ops.front()->ch_from;
      u.order_op = u.ops.empty() ? 0 : u.ops.front()->index;
      units.push_back(std::move(u));
    }
  }
  std::sort(units.begin(), units.end(), [](const Unit& a, const Unit& b) {
    if (a.path != b.path) return a.path < b.path;
    if (a.order_off != b.order_off) return a.order_off < b.order_off;
    return a.order_op < b.order_op;
  });

  // Backups for existing targets that will be replaced.
  std::unique_ptr<FileSystem> fs;
  create_file_system(&fs);
  struct Backup {
    std::string path;
    std::string backup_path;
    bool in_place = false;
  };
  std::vector<Backup> backups;
  auto cleanup_backups = [&]() {
    for (const Backup& b : backups) {
      if (!b.backup_path.empty()) fs->unlink(b.backup_path);
    }
  };
  for (const Unit& u : units) {
    const bool exists = exists_by_path[u.path];
    if (!exists) continue;
    bool already = false;
    for (const Backup& b : backups) {
      if (b.path == u.path) already = true;
    }
    if (already) continue;
    int src = -1;
    Status s = fs->open_readonly(u.path, true, &src);
    if (s != Status::Ok) {
      cleanup_backups();
      return fail_run("batch", code_of(s), "backup open failed", u.path);
    }
    std::string bp;
    int bfd = -1;
    s = fs->clone_to_new(dir_name(u.path), &bp, src, &bfd);
    fs->close_fd(src);
    if (s != Status::Ok) {
      cleanup_backups();
      return fail_run("batch", code_of(s), "backup failed", u.path);
    }
    fs->close_fd(bfd);
    backups.push_back(Backup{u.path, bp, u.ops.front()->in_place});
  }

  // Commit.
  struct Applied {
    std::string path;
    bool existed = false;
    bool in_place = false;
    bool wrote = false;
    Rev post_rev{};
  };
  std::vector<Applied> applied;
  struct Out {
    std::string path;
    uint64_t off = 0;
    int op = 0;
    std::string json;
  };
  std::vector<Out> outs;
  bool outs_truncated = false;
  auto dump_outs = [&]() {
    std::vector<Out> v = outs;
    std::sort(v.begin(), v.end(), [](const Out& a, const Out& b) {
      if (a.path != b.path) return a.path < b.path;
      if (a.off != b.off) return a.off < b.off;
      return a.op < b.op;
    });
    json::Array arr;
    const size_t n = std::min<size_t>(v.size(), 1000);
    for (size_t i = 0; i < n; ++i) arr.add(v[i].json);
    return arr.dump();
  };

  for (const Unit& u : units) {
    const bool exists = exists_by_path[u.path];
    const Info& before = info_by_path[u.path];

    std::unique_ptr<Editor> ed;
    create_editor(*fs, &ed);
    Status s = exists ? ed->open(u.path) : ed->create(u.path);
    if (s != Status::Ok) {
      // Roll back.
      bool ok = true;
      for (auto it = applied.rbegin(); it != applied.rend(); ++it) {
        const std::string bp = [&]() {
          for (const Backup& b : backups) {
            if (b.path == it->path) return b.backup_path;
          }
          return std::string();
        }();
        if (!it->existed) {
          fs->unlink(it->path);
        } else if (!bp.empty()) {
          if (it->in_place) {
            int bfd2 = -1;
            if (fs->open_readonly(bp, true, &bfd2) == Status::Ok) {
              int dfd = -1;
              if (fs->open_write(it->path, false, true, true, &dfd) == Status::Ok) {
                char buf[65536];
                uint64_t off = 0;
                size_t n = 0;
                while (fs->pread(bfd2, off, buf, sizeof(buf), &n) == Status::Ok && n > 0) {
                  size_t w = 0;
                  fs->pwrite(dfd, off, buf, n, &w);
                  off += n;
                }
                fs->ftruncate(dfd, off);
                fs->fsync(dfd, false);
                fs->close_fd(dfd);
              }
              fs->close_fd(bfd2);
            } else {
              ok = false;
            }
          } else {
            int dfd = -1;
            if (fs->open_dir(dir_name(it->path), &dfd) == Status::Ok) {
              if (fs->rename_at(dfd, base_name(it->path), bp) != Status::Ok) ok = false;
              fs->fsync_dir(dfd);
              fs->close_fd(dfd);
            } else {
              ok = false;
            }
          }
        }
      }
      cleanup_backups();
      Env env;
      env.command = "batch";
      env.ok = false;
      env.extra_top = std::string("\"rolled_back\":") + json::boolean(ok);
      env.results = ok ? "[]" : dump_outs();
      env.error = batch_error(code_of(s), "commit failed", u.path, u.ops.front()->index);
      Run r;
      r.exit_code = 2;
      r.out = env.dump();
      return r;
    }

    // Build and apply the merged edits.
    std::vector<Edit> edits;
    for (const BatchOp* o : u.ops) {
      for (const Edit& e : o->edits) edits.push_back(e);
    }
    // Writes: replace the whole document (the op's single edit already does).
    bool all_noop = true;
    for (const BatchOp* o : u.ops) {
      if (!o->noop) all_noop = false;
    }
    if (!all_noop) {
      s = ed->apply(edits);
      if (s != Status::Ok) {
        cleanup_backups();
        Env env;
        env.command = "batch";
        env.ok = false;
        env.extra_top = "\"rolled_back\":false";
        env.results = dump_outs();
        env.error = batch_error(code_of(s), "apply failed", u.path, u.ops.front()->index);
        Run r;
        r.exit_code = 2;
        r.out = env.dump();
        return r;
      }
    }

    const Rev* expect_ptr = nullptr;
    Rev expect{};
    bool any_force = false;
    for (const BatchOp* o : u.ops) {
      if (o->force) any_force = true;
    }
    if (!any_force) {
      for (const BatchOp* o : u.ops) {
        if (o->has_expect) {
          expect = o->expect;
          expect_ptr = &expect;
          break;
        }
      }
    }

    SaveInfo si;
    s = ed->save(expect_ptr, u.ops.front()->in_place, &si);
    if (s != Status::Ok) {
      const bool wrote = (s == Status::Io);
      bool ok = true;
      // The failed unit: restore unconditionally only if it may have written.
      const std::string bp = [&]() {
        for (const Backup& b : backups) {
          if (b.path == u.path) return b.backup_path;
        }
        return std::string();
      }();
      if (wrote) {
        if (!exists) {
          fs->unlink(u.path);
        } else if (!bp.empty()) {
          if (u.ops.front()->in_place) {
            int bfd2 = -1;
            if (fs->open_readonly(bp, true, &bfd2) == Status::Ok) {
              int dfd = -1;
              if (fs->open_write(u.path, false, true, true, &dfd) == Status::Ok) {
                char buf[65536];
                uint64_t off = 0;
                size_t n = 0;
                while (fs->pread(bfd2, off, buf, sizeof(buf), &n) == Status::Ok && n > 0) {
                  size_t w = 0;
                  fs->pwrite(dfd, off, buf, n, &w);
                  off += n;
                }
                fs->ftruncate(dfd, off);
                fs->fsync(dfd, false);
                fs->close_fd(dfd);
              }
              fs->close_fd(bfd2);
            } else {
              ok = false;
            }
          } else {
            int dfd = -1;
            if (fs->open_dir(dir_name(u.path), &dfd) == Status::Ok) {
              if (fs->rename_at(dfd, base_name(u.path), bp) != Status::Ok) ok = false;
              fs->fsync_dir(dfd);
              fs->close_fd(dfd);
            } else {
              ok = false;
            }
          }
        }
      }
      for (auto it = applied.rbegin(); it != applied.rend(); ++it) {
        const std::string bp2 = [&]() {
          for (const Backup& b : backups) {
            if (b.path == it->path) return b.backup_path;
          }
          return std::string();
        }();
        if (!it->existed) {
          fs->unlink(it->path);
        } else if (!bp2.empty()) {
          int dfd = -1;
          if (fs->open_dir(dir_name(it->path), &dfd) == Status::Ok) {
            if (fs->rename_at(dfd, base_name(it->path), bp2) != Status::Ok) ok = false;
            fs->fsync_dir(dfd);
            fs->close_fd(dfd);
          } else {
            ok = false;
          }
        }
      }
      cleanup_backups();
      Env env;
      env.command = "batch";
      env.ok = false;
      env.extra_top = std::string("\"rolled_back\":") + json::boolean(ok);
      env.results = ok ? "[]" : dump_outs();
      env.error = batch_error(code_of(s), "save failed", u.path, u.ops.front()->index);
      Run r;
      r.exit_code = 2;
      r.out = env.dump();
      return r;
    }

    // Success: emit this unit's result.
    Info after;
    ed->info(&after);
    uint64_t fc = u.ops.front()->ch_from;
    uint64_t lc2 = u.ops.front()->ch_to;
    for (const BatchOp* o : u.ops) {
      fc = std::min(fc, o->ch_from);
      lc2 = std::max(lc2, o->ch_to);
    }
    std::string fields = json::Object()
                             .set("path", json::str(after.path))
                             .set("bytes", json::unum(after.bytes))
                             .set("line_count", json::num(after.line_count))
                             .set("eol", json::str(eol_str(after.eol)))
                             .set("encoding", json::str(encoding_str(after.encoding)))
                             .set("bom", json::boolean(after.bom))
                             .set("final_newline", json::boolean(after.final_newline))
                             .set("rev_before", json::rev_object(
                                                    before.rev.dev, before.rev.ino,
                                                    before.rev.size, before.rev.mtime_ns,
                                                    before.rev.ctime_ns))
                             .set("rev", json::rev_object(after.rev.dev, after.rev.ino,
                                                          after.rev.size, after.rev.mtime_ns,
                                                          after.rev.ctime_ns))
                             .set("bytes_written", json::unum(si.bytes_written))
                             .set("save", json::str(si.method))
                             .dump();
    fields.pop_back();
    if (!all_noop) {
      fields += ",\"changed\":{\"from\":" + json::unum(fc + (before.bom ? 3 : 0)) +
                ",\"to\":" + json::unum(lc2 + (before.bom ? 3 : 0)) + "}";
    }
    if (u.ops.front()->in_place) fields += ",\"in_place\":true";
    fields += "}";
    outs.push_back({u.path, fc, u.ops.front()->index, fields});

    Applied ap;
    ap.path = u.path;
    ap.existed = exists;
    ap.in_place = u.ops.front()->in_place;
    ap.wrote = !all_noop;
    ap.post_rev = after.rev;
    applied.push_back(ap);
  }

  // Dry-run ops: predicted result, no write.
  for (const BatchOp& o : ops) {
    if (!o.dry_run) continue;
    const std::string& doc = doc_by_path[o.path];
    const Info& before = info_by_path[o.path];
    std::string predicted;
    {
      const std::vector<uint64_t> starts = line_starts_of(doc);
      size_t cursor = 0;
      for (const Edit& e : o.edits) {
        const uint64_t at =
            starts[static_cast<size_t>(e.line)] + static_cast<uint64_t>(e.col);
        predicted.append(doc, cursor, at - cursor);
        predicted.append(e.text.data(), e.text.size());
        cursor = at + static_cast<uint64_t>(e.removed_len);
      }
      predicted.append(doc, cursor, doc.size() - cursor);
    }
    const uint64_t new_logical = predicted.size();
    std::string method = "none";
    if (!o.noop) {
      if (new_logical == doc.size()) {
        method = "clone_pwrite";
      } else if ((new_logical - o.ch_to) * 4 <= new_logical) {
        method = "clone_pwrite_tail";
      } else {
        method = "full_rewrite";
      }
    }
    std::string fields = json::Object()
                             .set("path", json::str(o.path))
                             .set("bytes", json::unum(new_logical + (before.bom ? 3 : 0)))
                             .set("line_count", json::num(count_lines(predicted)))
                             .set("eol", json::str(eol_str(before.eol)))
                             .set("encoding", json::str(encoding_str(before.encoding)))
                             .set("bom", json::boolean(before.bom))
                             .set("final_newline",
                                  json::boolean(!predicted.empty() &&
                                                (predicted.back() == '\n' ||
                                                 predicted.back() == '\r')))
                             .set("rev_before", json::rev_object(
                                                    before.rev.dev, before.rev.ino,
                                                    before.rev.size, before.rev.mtime_ns,
                                                    before.rev.ctime_ns))
                             .set("bytes_before", json::unum(before.bytes))
                             .set("save", json::str(method))
                             .dump();
    fields.pop_back();
    if (!o.noop) {
      fields += ",\"changed\":{\"from\":" + json::unum(o.ch_from + (before.bom ? 3 : 0)) +
                ",\"to\":" + json::unum(o.ch_to + (before.bom ? 3 : 0)) + "}";
    }
    if (o.in_place) fields += ",\"in_place\":true";
    fields += "}";
    outs.push_back({o.path, o.ch_from, o.index, fields});
  }

  cleanup_backups();

  if (outs.size() > 1000) outs_truncated = true;
  Env env;
  env.command = "batch";
  env.extra_top = "\"rolled_back\":false";
  if (outs_truncated) env.extra_top += ",\"truncated\":true";
  env.results = dump_outs();
  Run r;
  r.out = env.dump();
  return r;
}

}  // namespace

int main(int argc, char** argv) {
  Ctx ctx;
  ctx.root = cwd_string();

  std::vector<std::string> tokens;
  for (int i = 1; i < argc; ++i) tokens.emplace_back(argv[i]);

  // Extract global flags anywhere.
  std::vector<std::string> rest;
  for (size_t i = 0; i < tokens.size(); ++i) {
    const std::string& t = tokens[i];
    if (t == "--root" && i + 1 < tokens.size()) {
      ctx.root = tokens[++i];
      char buf[PATH_MAX];
      if (realpath(ctx.root.c_str(), buf)) ctx.root = buf;
    } else if (t == "--max-file-bytes" && i + 1 < tokens.size()) {
      parse_u64(tokens[++i], &ctx.max_file_bytes);
    } else if (t == "--allow-huge") {
      ctx.allow_huge = true;
    } else {
      rest.push_back(t);
    }
  }

  if (rest.empty() || rest[0] == "--help" || rest[0] == "help") {
    std::fprintf(stderr, "usage: editor <command> [args]\n");
    return 0;
  }
  if (rest[0] == "--version") rest[0] = "version";

  const std::string command = rest[0];
  std::vector<std::string> tail(rest.begin() + 1, rest.end());

  Args args;
  std::vector<std::string> positional;
  bool no_more_opts = false;
  for (size_t i = 0; i < tail.size(); ++i) {
    const std::string& t = tail[i];
    if (!no_more_opts && t == "--") { no_more_opts = true; continue; }
    if (!no_more_opts && t.size() > 2 && t[0] == '-' && t[1] == '-') {
      if (takes_value(t) && i + 1 < tail.size()) {
        args.opts.emplace_back(t, tail[++i]);
      } else {
        args.opts.emplace_back(t, "");
      }
    } else if (!no_more_opts && !t.empty() && t[0] == '@') {
      args.opts.emplace_back("@file", t.substr(1));
    } else {
      positional.push_back(t);
    }
  }

  Run r;
  if (command == "version") {
    r = cmd_version();
  } else if (command == "schema") {
    r = cmd_schema(positional);
  } else if (command == "info") {
    if (positional.empty()) r = fail_run("info", "E_USAGE", "missing path");
    else r = cmd_info(ctx, positional[0]);
  } else if (command == "read") {
    if (positional.empty()) r = fail_run("read", "E_USAGE", "missing path");
    else r = cmd_read(ctx, positional[0], args);
  } else if (command == "write") {
    if (positional.empty()) r = fail_run("write", "E_USAGE", "missing path");
    else r = cmd_write(ctx, positional[0], args);
  } else if (command == "edit") {
    if (positional.empty()) r = fail_run("edit", "E_USAGE", "missing path");
    else r = cmd_edit(ctx, positional[0], args);
  } else if (command == "validate") {
    if (positional.empty()) r = fail_run("validate", "E_USAGE", "missing path");
    else r = cmd_validate(ctx, positional[0]);
  } else if (command == "batch") {
    r = cmd_batch(ctx, args);
  } else {
    r = fail_run(command, "E_USAGE", "unknown command");
  }

  std::fputs(r.out.c_str(), stdout);
  std::fputc('\n', stdout);
  return r.exit_code;
}
