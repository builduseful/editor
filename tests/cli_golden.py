#!/usr/bin/env python3
"""Golden CLI JSON tests (PLAN.md §6.2).

Runs the `editor` binary and compares the normalized envelope against expected
values. Normalization follows §6.2: duration_us and meta.pid are zeroed,
absolute paths are replaced by "<T>", and rev objects (whose mtime/ctime/ino are
not reproducible) are replaced by "<REV>" after their shape is checked.
"""
import json
import os
import subprocess
import sys
import tempfile

ED = sys.argv[1] if len(sys.argv) > 1 else "./build/editor"

failures = []
checks = 0


def check(cond, what):
    global checks
    checks += 1
    if not cond:
        failures.append(what)


def run(args, cwd, expect_exit=None):
    p = subprocess.run([os.path.abspath(ED)] + args, cwd=cwd,
                       capture_output=True, text=True)
    if expect_exit is not None:
        check(p.returncode == expect_exit,
              f"{args}: exit {p.returncode} != {expect_exit} ({p.stderr.strip()})")
    try:
        env = json.loads(p.stdout)
    except json.JSONDecodeError as e:
        failures.append(f"{args}: bad JSON: {e}: {p.stdout[:200]}")
        return None
    return env


REV_KEYS = {"dev", "ino", "size", "mtime_ns", "ctime_ns"}


def norm(o, tmp):
    if isinstance(o, dict):
        if set(o.keys()) == REV_KEYS:
            # Shape check: every field is a decimal string.
            check(all(isinstance(v, str) and v.isdigit() for v in o.values()),
                  f"rev fields must be decimal strings: {o}")
            return "<REV>"
        return {k: norm(v, tmp) for k, v in o.items()}
    if isinstance(o, list):
        return [norm(x, tmp) for x in o]
    if isinstance(o, str):
        return o.replace(tmp, "<T>")
    return o


def envelope(env, tmp):
    e = dict(env)
    e["duration_us"] = 0
    if "meta" in e:
        e["meta"] = dict(e["meta"])
        e["meta"]["pid"] = 0
    return norm(e, tmp)


def result(env, tmp, i=0):
    return norm(env["results"][i], tmp)


def main():
    with tempfile.TemporaryDirectory() as tmp:
        tmp = os.path.realpath(tmp)

        # --- version -------------------------------------------------------
        env = run(["version"], tmp, 0)
        check(env["schema"] == "editor.result/1", "schema tag")
        check(env["command"] == "version", "version command")
        check(env["ok"] is True, "version ok")
        check(env["results"] == [{"version": "0.1.0"}], "version result")
        check(env["error"] is None, "version error null")
        check(env["warnings"] == [], "version warnings")
        check(list(env.keys()) == ["schema", "command", "ok", "duration_us",
                                   "cwd", "warnings", "results", "error", "meta"],
              "top-level field order")

        # --- schema --------------------------------------------------------
        env = run(["schema"], tmp, 0)
        names = [c["name"] for c in env["results"][0]["commands"]]
        check("edit" in names and "write" in names, "schema lists commands")
        env = run(["schema", "nope"], tmp, 3)
        check(env["error"]["code"] == "E_USAGE", "schema unknown -> E_USAGE")

        # --- write a new file ---------------------------------------------
        with open(os.path.join(tmp, "payload"), "w") as f:
            f.write("alpha beta\ngamma\n")
        env = run(["write", "t.txt", "@payload"], tmp, 0)
        r = result(env, tmp)
        check(r["path"] == "<T>/t.txt", "write path")
        check(r["bytes"] == 17, "write bytes")
        check(r["line_count"] == 2, "write line_count")
        check(r["eol"] == "lf" and r["encoding"] == "utf-8" and r["bom"] is False,
              "write eol/encoding/bom")
        check(r["final_newline"] is True, "write final_newline")
        check(r["save"] == "full_rewrite", "write save method")
        check(r["changed"] == {"from": 0, "to": 17}, "write changed")
        check("rev_before" not in r and "bytes_before" not in r,
              "new file omits rev_before/bytes_before")

        # --- info ----------------------------------------------------------
        env = run(["info", "t.txt"], tmp, 0)
        r = result(env, tmp)
        check(r["dirty"] is False, "info dirty")
        check(set(r.keys()) >= {"path", "bytes", "line_count", "eol", "encoding",
                                "bom", "final_newline", "rev", "dirty"},
              "info fields")

        # --- read (line axis) ---------------------------------------------
        env = run(["read", "t.txt", "--from", "1", "--to", "1"], tmp, 0)
        r = result(env, tmp)
        check(r["content"] == "alpha beta\n", "read line content")
        check(r["from"] == 1 and r["to"] == 1, "read line range")
        check(r["truncated"] is False, "read not truncated")

        # --- read (byte axis) ---------------------------------------------
        env = run(["read", "t.txt", "--from-offset", "0", "--length", "5"], tmp, 0)
        r = result(env, tmp)
        check(r["content"] == "alpha", "read byte content")
        check(r["from"] == 0 and r["to"] == 5, "read byte range")

        # --- write to existing without --expect/--force --------------------
        env = run(["write", "t.txt", "@payload"], tmp, 3)
        check(env["error"]["code"] == "E_USAGE", "write existing -> E_USAGE")
        check(env["results"] == [], "error results empty")

        # --- edit: unique match -------------------------------------------
        env = run(["edit", "t.txt", "--old", "beta", "--new", "BETA", "--force"], tmp, 0)
        r = result(env, tmp)
        check(r["save"] == "clone_pwrite", "edit same-length save")
        check(r["changed"] == {"from": 6, "to": 10}, "edit changed")
        check(r["bytes_written"] == 4, "edit bytes_written")
        check(r["bytes"] == r["bytes_before"], "edit same length")
        check("rev_before" in r and "rev" in r, "edit has rev_before and rev")

        # --- edit: no match ------------------------------------------------
        env = run(["edit", "t.txt", "--old", "zzz", "--new", "y", "--force"], tmp, 2)
        check(env["error"]["code"] == "E_NO_MATCH", "edit no match")

        # --- edit: ambiguous ----------------------------------------------
        with open(os.path.join(tmp, "amb.txt"), "w") as f:
            f.write("x x\n")
        env = run(["edit", "amb.txt", "--old", "x", "--new", "y", "--force"], tmp, 2)
        check(env["error"]["code"] == "E_AMBIGUOUS", "edit ambiguous")

        # --- edit: missing --expect/--force -------------------------------
        env = run(["edit", "t.txt", "--old", "a", "--new", "b"], tmp, 3)
        check(env["error"]["code"] == "E_USAGE", "edit missing expect/force")

        # --- edit: --at insert (length-changing -> full_rewrite) ----------
        env = run(["edit", "amb.txt", "--at", "1:1", "--op", "insert",
                   "--text", "Q", "--force"], tmp, 0)
        r = result(env, tmp)
        check(r["save"] == "full_rewrite", "at-insert save method")
        check(r["changed"] == {"from": 1, "to": 2}, "at-insert changed")

        # --- edit: --at replace, same length -> clone_pwrite --------------
        env = run(["edit", "amb.txt", "--at", "1:0", "--op", "replace",
                   "--text", "W", "--length", "1", "--force"], tmp, 0)
        r = result(env, tmp)
        check(r["save"] == "clone_pwrite", "at-replace same length save")

        # --- edit: --dry-run omits rev/bytes_written ----------------------
        env = run(["edit", "t.txt", "--old", "gamma", "--new", "GAMMA", "--force",
                   "--dry-run"], tmp, 0)
        r = result(env, tmp)
        check("rev" not in r and "bytes_written" not in r, "dry-run omits rev/bytes_written")
        check("rev_before" in r and "bytes_before" in r, "dry-run keeps rev_before")
        check(r["save"] == "clone_pwrite", "dry-run predicted method")

        # --- validate ------------------------------------------------------
        with open(os.path.join(tmp, "ok.txt"), "w") as f:
            f.write("ok line\nsecond\n")
        env = run(["validate", "ok.txt"], tmp, 0)
        r = result(env, tmp)
        check(env["ok"] is True and r["errors"] == [], "validate ok")
        check("truncated" not in env, "validate ok omits truncated")

        with open(os.path.join(tmp, "bad.txt"), "wb") as f:
            f.write(b"bad \xff byte\n")
        env = run(["validate", "bad.txt"], tmp, 2)
        r = result(env, tmp)
        check(env["ok"] is False, "validate bad ok=false")
        check(r["errors"][0]["code"] == "invalid_utf8", "validate invalid_utf8")
        check(r["errors"][0]["byte_offset"] == 4, "validate byte_offset")
        check(env["error"]["code"] == "E_INVALID_UTF8", "validate promoted error")
        check(env["error"]["byte_offset"] == 4, "validate error field set")

        with open(os.path.join(tmp, "nul.txt"), "wb") as f:
            f.write(b"nul\x00here\n")
        env = run(["validate", "nul.txt"], tmp, 2)
        r = result(env, tmp)
        check(r["errors"][0]["code"] == "binary", "validate binary")
        check(r["line_count"] == 0 and r["final_newline"] is False,
              "validate binary placeholder fields")
        check(env["error"]["code"] == "E_BINARY", "validate binary promoted")

        with open(os.path.join(tmp, "u16.txt"), "wb") as f:
            f.write(b"\xff\xfea\x00")
        env = run(["validate", "u16.txt"], tmp, 2)
        r = result(env, tmp)
        check(r["errors"][0]["code"] == "unsupported", "validate unsupported")
        check(r["encoding"] == "utf-16le", "validate utf-16le encoding")
        check(r["bom"] is False, "validate utf-16 bom flag false")
        check(env["error"]["code"] == "E_UNSUPPORTED", "validate unsupported promoted")

        with open(os.path.join(tmp, "bom.txt"), "wb") as f:
            f.write(b"\xef\xbb\xbfbom\n")
        env = run(["validate", "bom.txt"], tmp, 0)
        r = result(env, tmp)
        check(r["bom"] is True, "validate utf-8 bom")
        check(r["bytes"] == 7, "validate bytes includes bom")

        # --- outside root --------------------------------------------------
        env = run(["info", "/etc/hosts"], tmp, 2)
        check(env["error"]["code"] == "E_OUTSIDE_ROOT", "outside root")

        # --- unknown command ----------------------------------------------
        env = run(["frobnicate"], tmp, 3)
        check(env["error"]["code"] == "E_USAGE", "unknown command")

    if failures:
        print(f"golden: {len(failures)} failure(s), {checks} checks")
        for f in failures:
            print("  FAIL:", f)
        return 1
    print(f"golden: {checks} checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
