// cli/src/json.hpp — tiny ordered JSON writer (PLAN.md §10: hand-rolled).
#pragma once

#include <cstdint>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

namespace json {

inline std::string escape(std::string_view s) {
  std::string out;
  out.reserve(s.size() + 2);
  out.push_back('"');
  for (const char c : s) {
    switch (c) {
      case '"': out += "\\\""; break;
      case '\\': out += "\\\\"; break;
      case '\n': out += "\\n"; break;
      case '\r': out += "\\r"; break;
      case '\t': out += "\\t"; break;
      case '\b': out += "\\b"; break;
      case '\f': out += "\\f"; break;
      default:
        if (static_cast<unsigned char>(c) < 0x20) {
          char buf[8];
          std::snprintf(buf, sizeof(buf), "\\u%04x", static_cast<unsigned>(c) & 0xFF);
          out += buf;
        } else {
          out.push_back(c);
        }
    }
  }
  out.push_back('"');
  return out;
}

inline std::string str(std::string_view s) { return escape(s); }
inline std::string num(int64_t v) { return std::to_string(v); }
inline std::string unum(uint64_t v) { return std::to_string(v); }
inline std::string boolean(bool b) { return b ? "true" : "false"; }
inline std::string null() { return "null"; }

class Object {
 public:
  Object& set(std::string_view key, std::string raw_value) {
    kv_.emplace_back(std::string(key), std::move(raw_value));
    return *this;
  }
  bool has(std::string_view key) const {
    for (const auto& p : kv_) {
      if (p.first == key) return true;
    }
    return false;
  }
  std::string dump() const {
    std::string out = "{";
    bool first = true;
    for (const auto& p : kv_) {
      if (!first) out.push_back(',');
      first = false;
      out += escape(p.first);
      out.push_back(':');
      out += p.second;
    }
    out.push_back('}');
    return out;
  }

 private:
  std::vector<std::pair<std::string, std::string>> kv_;
};

class Array {
 public:
  Array& add(std::string raw_value) {
    items_.push_back(std::move(raw_value));
    return *this;
  }
  std::string dump() const {
    std::string out = "[";
    for (size_t i = 0; i < items_.size(); ++i) {
      if (i != 0) out.push_back(',');
      out += items_[i];
    }
    out.push_back(']');
    return out;
  }

 private:
  std::vector<std::string> items_;
};

inline std::string rev_object(uint64_t dev, uint64_t ino, uint64_t size,
                              uint64_t mtime_ns, uint64_t ctime_ns) {
  return Object()
      .set("dev", str(std::to_string(dev)))
      .set("ino", str(std::to_string(ino)))
      .set("size", str(std::to_string(size)))
      .set("mtime_ns", str(std::to_string(mtime_ns)))
      .set("ctime_ns", str(std::to_string(ctime_ns)))
      .dump();
}

}  // namespace json
