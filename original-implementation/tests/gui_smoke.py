#!/usr/bin/env python3
# tests/gui_smoke.py — the GUI smoke test.
#
#   headless: renders the real window (through AppKit) to a PNG with --snapshot:
#             both themes, a small window (the resize path), and a scrolled
#             document; checks that each frame is a valid PNG and that scrolling
#             actually changes the frame.
#   normal:   launches the app, checks it starts and stays alive, then stops it.
#
# Usage: gui_smoke.py <app-executable> [<sample-file>]
import os
import signal
import struct
import subprocess
import sys
import tempfile
import time

CHECKS = 0


def check(cond, what):
    global CHECKS
    CHECKS += 1
    if not cond:
        print("gui_smoke: FAIL: " + what)
        sys.exit(1)


def png_size(path):
    """Returns (w, h) from a PNG's IHDR, or None if it isn't a PNG."""
    with open(path, "rb") as f:
        head = f.read(24)
    if len(head) < 24 or head[:8] != b"\x89PNG\r\n\x1a\n" or head[12:16] != b"IHDR":
        return None
    return struct.unpack(">II", head[16:24])


def snapshot(app, src, out, theme="light", size=None, scroll=None, wheel=None,
             xscroll=None):
    cmd = [app, "--theme", theme, "--snapshot", out]
    if size is not None:
        cmd += ["--size", size]
    if scroll is not None:
        cmd += ["--scroll", str(scroll)]
    if xscroll is not None:
        cmd += ["--xscroll", str(xscroll)]
    if wheel is not None:
        cmd += ["--wheel", wheel]
    cmd.append(src)
    return subprocess.run(cmd, capture_output=True, text=True, timeout=60)


def bytes_of(path):
    with open(path, "rb") as f:
        return f.read()


def main():
    if len(sys.argv) < 2:
        print("usage: gui_smoke.py <app-executable> [<sample-file>]")
        return 2
    app = os.path.abspath(sys.argv[1])
    check(os.path.exists(app), "app exists: " + app)

    sample = sys.argv[2] if len(sys.argv) > 2 else None
    tmpdir = tempfile.mkdtemp(prefix="editor_gui_smoke_")
    if sample is None:
        sample = os.path.join(tmpdir, "sample.md")
        with open(sample, "w") as f:
            f.write("# Sample\n\n")
            for i in range(200):
                f.write("line %d — the quick brown fox\n" % i)
    check(os.path.exists(sample), "sample exists: " + sample)

    frames = {}
    # Headless: the real window rendered through AppKit, both themes.
    for theme in ("light", "dark"):
        out = os.path.join(tmpdir, "snap-%s.png" % theme)
        proc = snapshot(app, sample, out, theme=theme)
        check(proc.returncode == 0,
              "%s snapshot exit 0 (got %d)\n%s" % (theme, proc.returncode, proc.stderr))
        check(os.path.exists(out), "%s snapshot wrote a file" % theme)
        size = os.path.getsize(out)
        check(size > 4096, "%s snapshot is non-trivial (%d bytes)" % (theme, size))
        dims = png_size(out)
        check(dims is not None, "%s snapshot is a valid PNG" % theme)
        check(dims[0] > 0 and dims[1] > 0,
              "%s snapshot has size (%d x %d)" % (theme, dims[0], dims[1]))
        frames[theme] = bytes_of(out)
        print("  %-5s snapshot: %d bytes, %dx%d" % (theme, size, dims[0], dims[1]))

    # The two themes must actually differ.
    check(frames["light"] != frames["dark"], "light and dark frames differ")

    # A small window is the case where the user saw no scrollbar: it must render,
    # and a scrolled position must produce a different frame (the thumb moves).
    small = os.path.join(tmpdir, "small.png")
    proc = snapshot(app, sample, small, size="420x300")
    check(proc.returncode == 0, "small-window snapshot exit 0\n" + proc.stderr)
    check(os.path.getsize(small) > 4096, "small-window snapshot is non-trivial")
    small_top = bytes_of(small)

    small_scrolled = os.path.join(tmpdir, "small-scrolled.png")
    proc = snapshot(app, sample, small_scrolled, size="420x300", scroll=150)
    check(proc.returncode == 0, "scrolled snapshot exit 0\n" + proc.stderr)
    check(bytes_of(small_scrolled) != small_top,
          "scrolling changes the rendered frame")

    dims = png_size(small)
    big = png_size(os.path.join(tmpdir, "snap-light.png"))
    check(dims[1] < big[1], "the small snapshot is shorter than the default one")

    # The mouse/trackpad scroll path. These drive the app's own wheel handling
    # (the same entry point scrollWheel: reaches) and pin the result, so a
    # regression in scrolling cannot pass unnoticed. It also covers a real crash
    # this found: reading clickCount on a scroll-wheel event raises an
    # NSInternalInconsistencyException, which used to kill the app on every
    # scroll, so every wheel frame here must both render and differ from rest.
    #
    # Horizontal scrolling needs a line wider than the window AND wrap off, so
    # the sideways cases use a long-line document below; `sample` only exercises
    # the vertical axis.
    rest = os.path.join(tmpdir, "wheel-rest.png")
    proc = snapshot(app, sample, rest, size="700x400", wheel="0,0")
    check(proc.returncode == 0, "wheel at rest exits 0\n" + proc.stderr)
    check(os.path.exists(rest), "wheel at rest wrote a frame")
    rest_bytes = bytes_of(rest)

    # Scrolling down a notch must render and change the frame; a scroll that
    # silently does nothing is the old bug.
    for label, wheel in (("down", "1,0"), ("diagonal", "3,0")):
        out = os.path.join(tmpdir, "wheel-%s.png" % label)
        proc = snapshot(app, sample, out, size="700x400", wheel=wheel)
        check(proc.returncode == 0,
              "wheel %s (%s) exits 0\n%s" % (label, wheel, proc.stderr))
        check(os.path.exists(out), "wheel %s wrote a frame" % label)
        check(bytes_of(out) != rest_bytes,
              "wheel %s (%s) changes the frame" % (label, wheel))

    # Scrolling back up must render cleanly too (no crash, no drift check).
    out = os.path.join(tmpdir, "wheel-up.png")
    proc = snapshot(app, sample, out, size="700x400", wheel="-1,0")
    check(proc.returncode == 0, "wheel up exits 0\n" + proc.stderr)
    check(os.path.exists(out), "wheel up wrote a frame")

    # Horizontal: a long line, which only scrolls sideways with wrap off.
    wide = os.path.join(tmpdir, "wide.txt")
    with open(wide, "w") as f:
        for i in range(80):
            f.write("line %02d %s\n" % (i, "x" * 120))
    wide_rest = os.path.join(tmpdir, "wheel-wide-rest.png")
    proc = snapshot(app, wide, wide_rest, size="700x400", wheel="0,0")
    check(proc.returncode == 0, "wide at rest exits 0\n" + proc.stderr)
    wide_rest_bytes = bytes_of(wide_rest)
    wide_right = os.path.join(tmpdir, "wheel-wide-right.png")
    proc = snapshot(app, wide, wide_right, size="700x400", wheel="0,1")
    check(proc.returncode == 0, "wheel right exits 0\n" + proc.stderr)
    check(bytes_of(wide_right) != wide_rest_bytes,
          "wheel right pans a long line sideways")

    # Normal: launch the app and check it starts and survives, then stop it.
    proc = subprocess.Popen([app, sample],
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    time.sleep(2.5)
    alive = proc.poll() is None
    if not alive:
        _, err = proc.communicate()
        check(False, "app stayed alive after launch (exited %d)\n%s"
              % (proc.returncode, err.decode("utf-8", "replace")))
    check(alive, "app stayed alive after launch")
    proc.send_signal(signal.SIGTERM)
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
        check(False, "app terminated on SIGTERM")
    check(True, "app launched, stayed alive, and stopped")

    # Finder / "Open With" launches pass no argv file: the window is shown first
    # and the document arrives as an open-document Apple event. Both must work
    # (the first paint used to crash because no document existed yet).
    def wait_alive(cmd, what):
        p = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        time.sleep(2.5)
        if p.poll() is not None:
            _, err = p.communicate()
            check(False, "%s: exited %d\n%s"
                  % (what, p.returncode, err.decode("utf-8", "replace")))
        check(True, what)
        return p

    def stop(p):
        p.send_signal(signal.SIGTERM)
        try:
            p.wait(timeout=5)
        except subprocess.TimeoutExpired:
            p.kill()
            check(False, "app terminated on SIGTERM")

    stop(wait_alive([app], "app stays alive with no file (untitled window)"))

    bundle = os.path.dirname(os.path.dirname(os.path.dirname(app)))
    if bundle.endswith(".app") and os.path.isdir(bundle):
        # LaunchServices: the app is started by launchd and the file is sent as
        # an Apple event, exactly like right-click → Open With → editor.
        subprocess.run(["open", "-a", bundle, sample], check=False, timeout=60)
        time.sleep(3.0)
        pids = subprocess.run(["pgrep", "-f", app], capture_output=True,
                              text=True).stdout.split()
        if not pids:
            check(False, "Finder-style launch (open -a) stays alive")
        check(True, "Finder-style launch (open -a) stays alive")
        for pid in pids:
            try:
                os.kill(int(pid), signal.SIGTERM)
            except (ValueError, ProcessLookupError):
                pass

    print("gui_smoke: %d checks passed" % CHECKS)
    return 0


if __name__ == "__main__":
    sys.exit(main())
