// tests/json_parse_test.cpp — the hand-rolled JSON reader (cli/src/json_parse.hpp).
#include <string>

#include "check.hpp"
#include "json_parse.hpp"

namespace {

bool ok(const std::string& s, json::Value* v) {
  std::string err;
  return json::parse(s, v, &err);
}

}  // namespace

TEST(json_parse_scalars) {
  json::Value v;
  CHECK(ok("true", &v) && v.is_bool() && v.boolean);
  CHECK(ok("false", &v) && v.is_bool() && !v.boolean);
  CHECK(ok("null", &v) && v.kind == json::Value::Kind::Null);
  CHECK(ok("-12.5e3", &v) && v.is_number() && v.text == "-12.5e3");
  CHECK(ok("\"hi\"", &v) && v.is_string() && v.text == "hi");
}

TEST(json_parse_objects_and_arrays) {
  json::Value v;
  CHECK(ok("{\"a\":1,\"b\":[1,2,3],\"c\":{\"d\":\"x\"}}", &v));
  CHECK(v.is_object());
  CHECK(v.find("a") != nullptr && v.find("a")->text == "1");
  CHECK(v.find("b") != nullptr && v.find("b")->array.size() == 3);
  CHECK(v.find("c") != nullptr && v.find("c")->find("d")->text == "x");
  CHECK(v.find("missing") == nullptr);
  CHECK(ok("{}", &v) && v.object.empty());
  CHECK(ok("[]", &v) && v.array.empty());
}

TEST(json_parse_string_escapes) {
  json::Value v;
  CHECK(ok("\"a\\nb\\t\\\"c\\\\d\"", &v));
  CHECK(v.text == "a\nb\t\"c\\d");
  CHECK(ok("\"\\u0041\\u00e9\"", &v));
  CHECK(v.text == "A\xC3\xA9");  // 'A' + U+00E9
  CHECK(ok("\"\\uD83D\\uDE00\"", &v));  // U+1F600
  CHECK(v.text == "\xF0\x9F\x98\x80");
}

TEST(json_parse_rejects_bad_input) {
  json::Value v;
  CHECK(!ok("", &v));
  CHECK(!ok("{", &v));
  CHECK(!ok("{\"a\":}", &v));
  CHECK(!ok("{\"a\":1,}", &v));
  CHECK(!ok("[1,2", &v));
  CHECK(!ok("\"unterminated", &v));
  CHECK(!ok("nope", &v));
  CHECK(!ok("{} extra", &v));
  CHECK(!ok("{\"a\" 1}", &v));
}

TEST(json_parse_expect_object_shape) {
  json::Value v;
  CHECK(ok("{\"dev\":\"1\",\"ino\":\"2\",\"size\":\"3\",\"mtime_ns\":\"4\","
           "\"ctime_ns\":\"5\"}",
           &v));
  for (const char* k : {"dev", "ino", "size", "mtime_ns", "ctime_ns"}) {
    CHECK(v.find(k) != nullptr && v.find(k)->is_string());
  }
}
