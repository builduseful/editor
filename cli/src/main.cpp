// cli/src/main.cpp — the `editor` CLI (M0a: version, schema, info, read, write).
#include <fcntl.h>
#include <limits.h>
#include <sys/stat.h>
#include <unistd.h>

#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <memory>
#include <string>
#include <vector>

#include "editor.hpp"
#include "json.hpp"

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

// Options that take a value.
bool takes_value(std::string_view k) {
  return k == "--from" || k == "--to" || k == "--from-offset" ||
         k == "--length" || k == "--max-bytes" || k == "--root" ||
         k == "--max-file-bytes";
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
  } else {
    r = fail_run(command, "E_USAGE", "unknown command");
  }

  std::fputs(r.out.c_str(), stdout);
  std::fputc('\n', stdout);
  return r.exit_code;
}
