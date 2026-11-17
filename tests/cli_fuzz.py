#!/usr/bin/env python3
"""CLI argv/batch fuzzer (PLAN.md §9: fuzz argv; §6 DoS defaults).

Feeds deterministic random argument vectors and random `batch` programs to the
`editor` binary and asserts it never crashes, never hangs, always exits with a
known code, and always prints either nothing or one valid JSON envelope. No
third-party tooling: just the standard library.
"""
import json
import os
import random
import subprocess
import sys
import tempfile

ED = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else "./build/editor")
SEED = int(sys.argv[2]) if len(sys.argv) > 2 else 20261117
ITER = int(sys.argv[3]) if len(sys.argv) > 3 else 4000

ENVELOPE_KEYS = {"schema", "command", "ok", "duration_us", "cwd", "warnings",
                 "results", "error", "meta"}
EXIT_OK = {0, 2, 3, 4}

failures = []
checks = 0


def check(cond, what):
    global checks
    checks += 1
    if not cond:
        failures.append(what)


def run(args, cwd, stdin=None):
    try:
        return subprocess.run([ED] + args, cwd=cwd, capture_output=True,
                              text=True, input=stdin, timeout=10)
    except subprocess.TimeoutExpired:
        failures.append(f"timeout: {args}")
        return None


def check_output(args, p, tmp):
    if p is None:
        return
    check(p.returncode >= 0, f"signal/crash {p.returncode}: {args}")
    check(p.returncode in EXIT_OK, f"unexpected exit {p.returncode}: {args}")
    out = p.stdout.strip()
    if out == "":
        return  # usage/help path
    try:
        env = json.loads(out)
    except json.JSONDecodeError as e:
        failures.append(f"bad JSON ({e}): {args}: {out[:200]}")
        return
    check(isinstance(env, dict) and ENVELOPE_KEYS <= set(env.keys()),
          f"envelope keys: {args}: {sorted(env.keys())}")
    check(env.get("ok") in (True, False), f"ok not bool: {args}")
    check(isinstance(env.get("results"), list), f"results not list: {args}")
    check(isinstance(env.get("warnings"), list), f"warnings not list: {args}")


def main():
    tmp = tempfile.mkdtemp(prefix="editor_fuzz_")
    with open(os.path.join(tmp, "a.txt"), "w") as f:
        f.write("hello\nworld\n")
    with open(os.path.join(tmp, "b.txt"), "w") as f:
        f.write("x" * 100)

    rng = random.Random(SEED)

    commands = ["read", "write", "edit", "info", "validate", "batch",
                "version", "schema", "bogus", "--help", ""]
    flags = ["--from", "--to", "--from-offset", "--length", "--max-bytes",
             "--old", "--new", "--at", "--op", "--text", "--all", "--count",
             "--expect", "--force", "--in-place", "--dry-run", "--root",
             "--max-file-bytes", "--allow-huge", "--file", "--check",
             "--stdin", "-", "--", "--nope"]
    values = ["0", "1", "-1", "999999999999999999999", "", "a", "abc", "hello",
              "world", "insert", "replace", "delete", "@a.txt", "@b.txt",
              "/etc/hosts", "../escape", "a.txt", "b.txt", "--root", "\n",
              "\t", "\u00e9", "line\n"]
    paths = ["a.txt", "b.txt", "missing.txt", ".", "..", "/", "", "a/../a.txt"]

    for _ in range(ITER):
        argv = []
        if rng.random() < 0.9:
            argv.append(rng.choice(commands))
        # Sprinkle flags, values, and paths in random order.
        for _ in range(rng.randint(0, 5)):
            r = rng.random()
            if r < 0.45:
                argv.append(rng.choice(flags))
            elif r < 0.75:
                argv.append(rng.choice(values))
            else:
                argv.append(rng.choice(paths))
        p = run(argv, tmp)
        check_output(argv, p, tmp)

    # Fuzz `batch` programs: random JSON-ish lines on stdin.
    batch_tokens = ["{", "}", "[", "]", ":", ",", "\"", "\\", "op", "path",
                    "read", "write", "edit", "info", "validate", "content",
                    "content_file", "old", "new", "at", "text", "all",
                    "count", "expect", "force", "in_place", "dry_run", "null",
                    "true", "false", "0", "1", "-1", "\"a.txt\"", "\"b.txt\"",
                    "1.5", "\n", " ", "\u00e9"]
    for _ in range(ITER // 4):
        prog = "".join(rng.choice(batch_tokens) for _ in range(rng.randint(0, 40)))
        for extra in ([], ["--check"]):
            p = run(["batch"] + extra, tmp, stdin=prog)
            check_output(["batch"] + extra + ["<stdin>"], p, tmp)

    if failures:
        for f in failures[:20]:
            print("FAIL:", f, file=sys.stderr)
        print(f"cli_fuzz: {checks} checks, {len(failures)} failures", file=sys.stderr)
        return 1
    print(f"cli_fuzz: {checks} checks passed (seed {SEED})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
