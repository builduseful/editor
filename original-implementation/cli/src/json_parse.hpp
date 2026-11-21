// cli/src/json_parse.hpp — small strict JSON reader (PLAN.md §10: hand-rolled).
//
// Numbers keep their raw text so batch op integers and the decimal-string rev
// fields round-trip without floating-point precision loss.
#pragma once

#include <cstdint>
#include <string>
#include <utility>
#include <vector>

namespace json {

struct Value {
  enum class Kind { Null, Bool, Number, String, Array, Object };
  Kind kind = Kind::Null;
  bool boolean = false;
  std::string text;  // Number: raw literal; String: decoded value
  std::vector<Value> array;
  std::vector<std::pair<std::string, Value>> object;

  bool is_object() const { return kind == Kind::Object; }
  bool is_array() const { return kind == Kind::Array; }
  bool is_string() const { return kind == Kind::String; }
  bool is_bool() const { return kind == Kind::Bool; }
  bool is_number() const { return kind == Kind::Number; }

  const Value* find(const std::string& key) const {
    for (const auto& kv : object) {
      if (kv.first == key) return &kv.second;
    }
    return nullptr;
  }
};

class Parser {
 public:
  explicit Parser(const std::string& s) : s_(s) {}

  bool parse(Value* out) {
    skip_ws();
    if (!parse_value(out)) return false;
    skip_ws();
    if (pos_ != s_.size()) {
      err_ = "trailing characters";
      return false;
    }
    return true;
  }

  const std::string& error() const { return err_; }

 private:
  void skip_ws() {
    while (pos_ < s_.size() &&
           (s_[pos_] == ' ' || s_[pos_] == '\t' || s_[pos_] == '\n' ||
            s_[pos_] == '\r')) {
      pos_ += 1;
    }
  }

  bool fail(const char* m) {
    if (err_.empty()) err_ = m;
    return false;
  }

  bool parse_value(Value* out) {
    if (depth_ >= kMaxDepth) return fail("nesting too deep");
    ++depth_;
    const bool ok = parse_value_inner(out);
    --depth_;
    return ok;
  }

  bool parse_value_inner(Value* out) {
    *out = Value{};  // reset so a reused Value does not accumulate
    if (pos_ >= s_.size()) return fail("unexpected end of input");
    const char c = s_[pos_];
    if (c == '{') return parse_object(out);
    if (c == '[') return parse_array(out);
    if (c == '"') {
      out->kind = Value::Kind::String;
      return parse_string(&out->text);
    }
    if (c == 't' || c == 'f') return parse_bool(out);
    if (c == 'n') return parse_null(out);
    if (c == '-' || (c >= '0' && c <= '9')) return parse_number(out);
    return fail("unexpected character");
  }

  bool parse_object(Value* out) {
    out->kind = Value::Kind::Object;
    pos_ += 1;  // {
    skip_ws();
    if (pos_ < s_.size() && s_[pos_] == '}') {
      pos_ += 1;
      return true;
    }
    while (true) {
      skip_ws();
      if (pos_ >= s_.size() || s_[pos_] != '"') return fail("expected key");
      std::string key;
      if (!parse_string(&key)) return false;
      skip_ws();
      if (pos_ >= s_.size() || s_[pos_] != ':') return fail("expected ':'");
      pos_ += 1;
      skip_ws();
      Value v;
      if (!parse_value(&v)) return false;
      out->object.emplace_back(std::move(key), std::move(v));
      skip_ws();
      if (pos_ < s_.size() && s_[pos_] == ',') {
        pos_ += 1;
        continue;
      }
      if (pos_ < s_.size() && s_[pos_] == '}') {
        pos_ += 1;
        return true;
      }
      return fail("expected ',' or '}'");
    }
  }

  bool parse_array(Value* out) {
    out->kind = Value::Kind::Array;
    pos_ += 1;  // [
    skip_ws();
    if (pos_ < s_.size() && s_[pos_] == ']') {
      pos_ += 1;
      return true;
    }
    while (true) {
      skip_ws();
      Value v;
      if (!parse_value(&v)) return false;
      out->array.push_back(std::move(v));
      skip_ws();
      if (pos_ < s_.size() && s_[pos_] == ',') {
        pos_ += 1;
        continue;
      }
      if (pos_ < s_.size() && s_[pos_] == ']') {
        pos_ += 1;
        return true;
      }
      return fail("expected ',' or ']'");
    }
  }

  static void append_utf8(std::string* out, uint32_t cp) {
    if (cp < 0x80) {
      out->push_back(static_cast<char>(cp));
    } else if (cp < 0x800) {
      out->push_back(static_cast<char>(0xC0 | (cp >> 6)));
      out->push_back(static_cast<char>(0x80 | (cp & 0x3F)));
    } else if (cp < 0x10000) {
      out->push_back(static_cast<char>(0xE0 | (cp >> 12)));
      out->push_back(static_cast<char>(0x80 | ((cp >> 6) & 0x3F)));
      out->push_back(static_cast<char>(0x80 | (cp & 0x3F)));
    } else {
      out->push_back(static_cast<char>(0xF0 | (cp >> 18)));
      out->push_back(static_cast<char>(0x80 | ((cp >> 12) & 0x3F)));
      out->push_back(static_cast<char>(0x80 | ((cp >> 6) & 0x3F)));
      out->push_back(static_cast<char>(0x80 | (cp & 0x3F)));
    }
  }

  bool parse_hex4(uint32_t* out) {
    if (pos_ + 4 > s_.size()) return fail("bad \\u escape");
    uint32_t v = 0;
    for (int i = 0; i < 4; ++i) {
      const char c = s_[pos_ + static_cast<size_t>(i)];
      v <<= 4;
      if (c >= '0' && c <= '9') {
        v |= static_cast<uint32_t>(c - '0');
      } else if (c >= 'a' && c <= 'f') {
        v |= static_cast<uint32_t>(c - 'a' + 10);
      } else if (c >= 'A' && c <= 'F') {
        v |= static_cast<uint32_t>(c - 'A' + 10);
      } else {
        return fail("bad \\u escape");
      }
    }
    pos_ += 4;
    *out = v;
    return true;
  }

  bool parse_string(std::string* out) {
    pos_ += 1;  // opening quote
    out->clear();
    while (pos_ < s_.size()) {
      const char c = s_[pos_];
      if (c == '"') {
        pos_ += 1;
        return true;
      }
      if (c == '\\') {
        pos_ += 1;
        if (pos_ >= s_.size()) return fail("bad escape");
        const char e = s_[pos_++];
        switch (e) {
          case '"': out->push_back('"'); break;
          case '\\': out->push_back('\\'); break;
          case '/': out->push_back('/'); break;
          case 'b': out->push_back('\b'); break;
          case 'f': out->push_back('\f'); break;
          case 'n': out->push_back('\n'); break;
          case 'r': out->push_back('\r'); break;
          case 't': out->push_back('\t'); break;
          case 'u': {
            uint32_t cp = 0;
            if (!parse_hex4(&cp)) return false;
            if (cp >= 0xD800 && cp <= 0xDBFF) {
              // High surrogate: require an immediately following low surrogate.
              if (pos_ + 1 < s_.size() && s_[pos_] == '\\' && s_[pos_ + 1] == 'u') {
                pos_ += 2;
                uint32_t lo = 0;
                if (!parse_hex4(&lo)) return false;
                if (lo < 0xDC00 || lo > 0xDFFF) return fail("unpaired surrogate");
                cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
              } else {
                return fail("unpaired surrogate");
              }
            } else if (cp >= 0xDC00 && cp <= 0xDFFF) {
              return fail("unpaired surrogate");
            }
            append_utf8(out, cp);
            break;
          }
          default: return fail("bad escape");
        }
        continue;
      }
      if (static_cast<unsigned char>(c) < 0x20) {
        return fail("control character in string");
      }
      out->push_back(c);
      pos_ += 1;
    }
    return fail("unterminated string");
  }

  bool parse_bool(Value* out) {
    if (s_.compare(pos_, 4, "true") == 0) {
      out->kind = Value::Kind::Bool;
      out->boolean = true;
      pos_ += 4;
      return true;
    }
    if (s_.compare(pos_, 5, "false") == 0) {
      out->kind = Value::Kind::Bool;
      out->boolean = false;
      pos_ += 5;
      return true;
    }
    return fail("bad literal");
  }

  bool parse_null(Value* out) {
    if (s_.compare(pos_, 4, "null") == 0) {
      out->kind = Value::Kind::Null;
      pos_ += 4;
      return true;
    }
    return fail("bad literal");
  }

  bool parse_number(Value* out) {
    const size_t start = pos_;
    if (pos_ < s_.size() && s_[pos_] == '-') pos_ += 1;
    // Integer part: 0, or [1-9][0-9]* (no leading zeros).
    if (pos_ >= s_.size() || s_[pos_] < '0' || s_[pos_] > '9') {
      return fail("bad number");
    }
    if (s_[pos_] == '0') {
      pos_ += 1;
    } else {
      while (pos_ < s_.size() && s_[pos_] >= '0' && s_[pos_] <= '9') pos_ += 1;
    }
    // Fraction: '.' [0-9]+
    if (pos_ < s_.size() && s_[pos_] == '.') {
      pos_ += 1;
      if (pos_ >= s_.size() || s_[pos_] < '0' || s_[pos_] > '9') {
        return fail("bad number");
      }
      while (pos_ < s_.size() && s_[pos_] >= '0' && s_[pos_] <= '9') pos_ += 1;
    }
    // Exponent: [eE] [+-]? [0-9]+
    if (pos_ < s_.size() && (s_[pos_] == 'e' || s_[pos_] == 'E')) {
      pos_ += 1;
      if (pos_ < s_.size() && (s_[pos_] == '+' || s_[pos_] == '-')) pos_ += 1;
      if (pos_ >= s_.size() || s_[pos_] < '0' || s_[pos_] > '9') {
        return fail("bad number");
      }
      while (pos_ < s_.size() && s_[pos_] >= '0' && s_[pos_] <= '9') pos_ += 1;
    }
    out->kind = Value::Kind::Number;
    out->text = s_.substr(start, pos_ - start);
    return true;
  }

  const std::string& s_;
  size_t pos_ = 0;
  int depth_ = 0;
  std::string err_;

  // The reader is recursive; cap nesting so a hostile program cannot exhaust the
  // stack (§6.5 DoS defaults).
  static constexpr int kMaxDepth = 256;
};

inline bool parse(const std::string& text, Value* out, std::string* err) {
  Parser p(text);
  if (!p.parse(out)) {
    if (err != nullptr) *err = p.error();
    return false;
  }
  return true;
}

}  // namespace json
