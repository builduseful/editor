#!/usr/bin/env python3
# tests/gui_golden.py — golden-frame regression for the AppKit view.
#
# Renders fixed frames through the app's own `--snapshot` path (the real window,
# real CoreText/CoreGraphics) and compares them with the pinned PNGs in
# `tests/golden/`. The frames pin the things a unit test cannot see: the theme
# colours as applied, the gutter and status bar, the caret, both scrollbars, and
# the text clipping at the horizontal bar's strip.
#
# Usage: gui_golden.py <app-executable>            check
#        gui_golden.py --update <app-executable>   re-pin after an intended change
#
# Frames are compared with a small per-pixel colour tolerance and a tiny drift
# budget (antialiasing can differ across OS builds); layout or colour changes move
# far more pixels than that and fail.
import os
import struct
import subprocess
import sys
import tempfile
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
GOLDEN_DIR = os.path.join(HERE, "golden")

# The frames. `doc` selects a fixture built below, so the inputs are part of this
# file and a golden never depends on a machine-specific sample.
FRAMES = [
    # name        doc      theme    size       scroll  xscroll
    ("light", "sample", "light", "720x520", 0, 0),
    ("dark", "sample", "dark", "720x520", 0, 0),
    ("small", "sample", "light", "420x300", 0, 0),
    ("scrolled", "wide", "light", "640x400", 30, 160),
]

TOLERANCE = 6  # per-channel, out of 255
DRIFT = 5e-4   # share of pixels allowed to exceed the tolerance


def sample_doc():
    """A small markdown-ish document: headings, lists, blanks, long-ish lines."""
    out = ["# editor sample\n", "\n", "A native macOS text editor.\n", "\n"]
    for i in range(1, 61):
        if i % 12 == 0:
            out.append("\n## Section %d\n" % (i // 12))
        elif i % 4 == 0:
            out.append("- item %d: the quick brown fox jumps over the lazy dog\n" % i)
        elif i % 7 == 0:
            out.append("\n")
        else:
            out.append("line %d: %s\n" % (i, "lorem ipsum dolor sit amet " * (i % 3 + 1)))
    return "".join(out)


def wide_doc():
    """Long lines with no wrap off: pins the horizontal bar and the pan."""
    return "".join("line %03d %s\n" % (i, "x" * 140) for i in range(60))


DOCS = {"sample": sample_doc, "wide": wide_doc}


def read_png(path):
    """Decodes an 8-bit RGB/RGBA, non-interlaced PNG -> (w, h, bpp, pixels)."""
    with open(path, "rb") as f:
        data = f.read()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError("not a PNG: " + path)
    pos = 8
    idat = b""
    w = h = ctype = 0
    while pos < len(data):
        (n,) = struct.unpack(">I", data[pos:pos + 4])
        typ = data[pos + 4:pos + 8]
        body = data[pos + 8:pos + 8 + n]
        pos += 12 + n
        if typ == b"IHDR":
            w, h, depth, ctype, _comp, _filt, inter = struct.unpack(">IIBBBBB", body)
            if depth != 8 or inter != 0 or ctype not in (2, 6):
                raise ValueError("unsupported PNG (%d-bit, type %d)" % (depth, ctype))
        elif typ == b"IDAT":
            idat += body
        elif typ == b"IEND":
            break
    raw = zlib.decompress(idat)
    bpp = 3 if ctype == 2 else 4
    stride = w * bpp
    out = bytearray(h * stride)
    prev = bytearray(stride)
    p = 0
    for y in range(h):
        f = raw[p]
        p += 1
        line = bytearray(raw[p:p + stride])
        p += stride
        if f == 1:
            for i in range(bpp, stride):
                line[i] = (line[i] + line[i - bpp]) & 0xFF
        elif f == 2:
            for i in range(stride):
                line[i] = (line[i] + prev[i]) & 0xFF
        elif f == 3:
            for i in range(stride):
                a = line[i - bpp] if i >= bpp else 0
                line[i] = (line[i] + ((a + prev[i]) >> 1)) & 0xFF
        elif f == 4:
            for i in range(stride):
                a = line[i - bpp] if i >= bpp else 0
                b = prev[i]
                c = prev[i - bpp] if i >= bpp else 0
                pa, pb, pc = abs(b - c), abs(a - c), abs(a + b - 2 * c)
                pred = a if (pa <= pb and pa <= pc) else (b if pb <= pc else c)
                line[i] = (line[i] + pred) & 0xFF
        elif f != 0:
            raise ValueError("bad PNG filter %d" % f)
        out[y * stride:(y + 1) * stride] = line
        prev = line
    return w, h, bpp, out


def drift(a, b, rows):
    """Pixels in image rows [rows, h) that differ by more than the tolerance.

    The window's title bar is excluded: it is AppKit's own view, and its label
    can shift by a subpixel between launches (the content below it is ours and is
    deterministic).
    """
    w1, h1, bp1, p1 = a
    w2, h2, bp2, p2 = b
    if (w1, h1, bp1) != (w2, h2, bp2):
        return None
    start = w1 * bp1 * rows
    bad = 0
    for i in range(start, len(p1), bp1):
        for c in range(3):
            if abs(p1[i + c] - p2[i + c]) > TOLERANCE:
                bad += 1
                break
    return bad


def render(app, doc, out, theme, size, scroll, xscroll, tmpdir):
    path = os.path.join(tmpdir, doc + ".txt")
    if not os.path.exists(path):
        with open(path, "w") as f:
            f.write(DOCS[doc]())
    cmd = [app, "--snapshot", out, "--theme", theme, "--size", size, path]
    if scroll:
        cmd += ["--scroll", str(scroll)]
    if xscroll:
        cmd += ["--xscroll", str(xscroll)]
    proc = subprocess.run(cmd, capture_output=True, text=True, timeout=120)
    if proc.returncode != 0 or not os.path.exists(out):
        print("gui_golden: snapshot failed for %s (exit %d)\n%s"
              % (os.path.basename(out), proc.returncode, proc.stderr))
        sys.exit(1)


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    update = "--update" in sys.argv
    if len(args) != 1:
        print("usage: gui_golden.py [--update] <app-executable>")
        return 2
    app = os.path.abspath(args[0])
    if not os.path.exists(app):
        print("gui_golden: no such executable: " + app)
        return 2

    if update:
        os.makedirs(GOLDEN_DIR, exist_ok=True)
    tmpdir = tempfile.mkdtemp(prefix="editor_gui_golden_")

    failures = 0
    worst = (0, "")
    for name, doc, theme, size, scroll, xscroll in FRAMES:
        out = os.path.join(tmpdir, name + ".png")
        render(app, doc, out, theme, size, scroll, xscroll, tmpdir)
        ref_path = os.path.join(GOLDEN_DIR, name + ".png")
        if update:
            with open(out, "rb") as src, open(ref_path, "wb") as dst:
                dst.write(src.read())
            print("  %-8s pinned (%d bytes)" % (name, os.path.getsize(ref_path)))
            continue
        if not os.path.exists(ref_path):
            print("gui_golden: missing reference %s (run: make golden-update)"
                  % os.path.relpath(ref_path, HERE))
            failures += 1
            continue
        try:
            got = read_png(out)
            want = read_png(ref_path)
        except ValueError as e:
            print("gui_golden: %s: %s" % (name, e))
            failures += 1
            continue
        # The snapshot is the window's frame view: content height is the size
        # that was asked for (retina 2x), everything above it is the title bar.
        content_h = 2 * int(size.split("x")[1])
        bad = drift(want, got, max(0, got[1] - content_h))
        if bad is None:
            print("gui_golden: %s: frame size changed (%dx%d -> %dx%d)"
                  % (name, want[0], want[1], got[0], got[1]))
            failures += 1
            continue
        total = want[0] * content_h
        ratio = bad / float(total)
        worst = max(worst, (ratio, name))
        print("  %-8s %dx%d  drift %d px (%.4f%%)"
              % (name, got[0], got[1], bad, 100.0 * ratio))
        if ratio > DRIFT:
            print("gui_golden: %s differs from the pinned frame "
                  "(if this is intended, run: make golden-update)" % name)
            failures += 1
    if failures:
        print("gui_golden: %d frame(s) failed" % failures)
        return 1
    if not update:
        print("gui_golden: %d frames match (worst drift %.4f%%)"
              % (len(FRAMES), 100.0 * worst[0]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
