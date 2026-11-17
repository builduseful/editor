// tests/json_fuzz_test.cpp — the hand-rolled JSON reader/writer (cli/src).
//
// §9: fuzz the JSON reader and writer. The reader must never crash on arbitrary
// input, and anything it accepts must re-serialize and re-parse identically.
#include <cstdint>
#include <string>
#include <vector>

#include "check.hpp"
#include "json.hpp"
#include "json_parse.hpp"
#include "prng.hpp"

namespace {

bool parse_ok(const std::string& s, json::Value* v) {
  std::string err;
  return json::parse(s, v, &err);
}

// A generic Value -> JSON serializer used only to check reader round-trips.
std::string dump_value(const json::Value& v) {
  switch (v.kind) {
    case json::Value::Kind::Null:
      return "null";
    case json::Value::Kind::Bool:
      return v.boolean ? "true" : "false";
    case json::Value::Kind::Number:
      return v.text;
    case json::Value::Kind::String:
      return json::escape(v.text);
    case json::Value::Kind::Array: {
      std::string out = "[";
      for (size_t i = 0; i < v.array.size(); ++i) {
        if (i != 0) out.push_back(',');
        out += dump_value(v.array[i]);
      }
      out.push_back(']');
      return out;
    }
    case json::Value::Kind::Object: {
      std::string out = "{";
      for (size_t i = 0; i < v.object.size(); ++i) {
        if (i != 0) out.push_back(',');
        out += json::escape(v.object[i].first);
        out.push_back(':');
        out += dump_value(v.object[i].second);
      }
      out.push_back('}');
      return out;
    }
  }
  return "null";
}

}  // namespace

TEST(json_parse_is_strict) {
  json::Value v;
  CHECK(!parse_ok("", &v));
  CHECK(!parse_ok("-", &v));       // no digits
  CHECK(!parse_ok("1.", &v));      // empty fraction
  CHECK(!parse_ok("1e", &v));      // empty exponent
  CHECK(!parse_ok("1e+", &v));     // empty exponent
  CHECK(!parse_ok("01", &v));      // leading zero
  CHECK(!parse_ok(".5", &v));      // no integer part
  CHECK(!parse_ok("1 2", &v));     // trailing
  CHECK(!parse_ok("tru", &v));     // bad literal
  CHECK(!parse_ok("[1,]", &v));    // trailing comma
  CHECK(!parse_ok("{\"a\":1,}", &v));
  CHECK(!parse_ok("{a:1}", &v));   // unquoted key
  CHECK(!parse_ok("\"a\nb\"", &v));  // raw control char in a string
  CHECK(!parse_ok("\"\\uD800\"", &v));  // unpaired high surrogate
  CHECK(!parse_ok("\"\\uDC00\"", &v));  // unpaired low surrogate
  CHECK(!parse_ok("\"\\uD800\\u0041\"", &v));  // high then non-low

  CHECK(parse_ok("0", &v) && v.text == "0");
  CHECK(parse_ok("-0", &v) && v.text == "-0");
  CHECK(parse_ok("10", &v) && v.text == "10");
  CHECK(parse_ok("1.5e-3", &v) && v.text == "1.5e-3");
  CHECK(parse_ok("1E+9", &v) && v.text == "1E+9");
  CHECK(parse_ok("\"\\uD83D\\uDE00\"", &v) && v.text == "\xF0\x9F\x98\x80");
}

TEST(json_parse_depth_limit) {
  json::Value v;
  std::string deep;
  for (int i = 0; i < 300; ++i) deep += "[";
  CHECK(!parse_ok(deep, &v));  // over the cap: error, not a crash
  std::string ok;
  for (int i = 0; i < 200; ++i) ok += "[";
  for (int i = 0; i < 200; ++i) ok += "]";
  CHECK(parse_ok(ok, &v));
}

TEST(json_parse_fuzz_never_crashes_and_round_trips) {
  ed_test::Prng rng(0xBADF00D);
  const char* alpha[] = {"{",  "}",  "[",  "]",  ":",  ",",  "\"", "\\",
                         "n",  "t",  "f",  "1",  "0",  "-",  "e",  "+",
                         ".",  "u",  "D",  "8",  " ",  "\n", "\t", "x",
                         "\"a\"", "null", "true", "1.5", "{}", "[]"};
  const size_t alen = sizeof(alpha) / sizeof(alpha[0]);
  int roundtrip_fail = 0;
  for (int iter = 0; iter < 100000; ++iter) {
    std::string s;
    const size_t n = rng.below(20);
    for (size_t i = 0; i < n; ++i) s += alpha[rng.below(alen)];
    json::Value v;
    if (!parse_ok(s, &v)) continue;
    // Anything accepted must serialize and re-parse identically.
    const std::string again = dump_value(v);
    json::Value v2;
    if (!parse_ok(again, &v2) || dump_value(v2) != again) {
      ++roundtrip_fail;
      if (roundtrip_fail <= 3) {
        std::printf("round-trip fail: input=[%s] again=[%s]\n", s.c_str(),
                    again.c_str());
      }
    }
  }
  CHECK_EQ(roundtrip_fail, 0);
}

TEST(json_escape_round_trips_arbitrary_strings) {
  ed_test::Prng rng(0x1234ABCD);
  for (int iter = 0; iter < 100000; ++iter) {
    std::string s;
    const size_t n = rng.below(24);
    for (size_t i = 0; i < n; ++i) {
      // Mix ASCII, control bytes, quotes/backslashes, and high bytes.
      const uint64_t r = rng.below(5);
      if (r == 0) {
        s.push_back(static_cast<char>(rng.below(0x20)));
      } else if (r == 1) {
        s.push_back(static_cast<char>('"' + rng.below(60)));
      } else if (r == 2) {
        s.push_back(static_cast<char>(0x80 + rng.below(0x80)));
      } else {
        s.push_back(static_cast<char>('a' + rng.below(26)));
      }
    }
    const std::string enc = json::str(s);
    json::Value v;
    CHECK(parse_ok(enc, &v));
    CHECK(v.is_string());
    CHECK_EQ(v.text, s);
  }
}
