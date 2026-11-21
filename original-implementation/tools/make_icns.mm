// tools/make_icns.mm — renders the app icon and packs it into an .icns.
//
// The construction is the Build Useful one (design-system/docs/brand.md,
// templates/app-icon.svg), drawn natively with CoreGraphics rather than
// rasterised from the SVG: a 512x512 tile in --color-brand-tile (#111111) with
// a 112 pt corner radius, and one stroke motif in --color-brand-mark (#f1f1f1),
// 28 units wide, round caps and joins, kept inside the central 320x320 safe
// square. There is no second colour, no fill, no gradient and no text, so the
// icon sits in the same family as the rest of the system.
//
// The motif is a text cursor: a vertical caret with a line running out of it,
// the one gesture that says "editing text" rather than "a document" (the brand
// guide's own examples: a wave for sound, a clock face for time). It is drawn
// on whole coordinates so it stays crisp at the small sizes.
//
// Sizes are the ones macOS asks for, not the web set: 16, 32, 128, 256 and 512
// points at 1x and 2x, which is every slot an .icns can hold. Rendering at the
// target size (rather than scaling one big image) is what keeps the 16 pt icon
// from turning to mush; it is also why the tile is drawn, not imported.
//
// Build-time only: `make app` runs this and bundles the result. Nothing here
// runs in the app.

#import <CoreGraphics/CoreGraphics.h>
#import <ImageIO/ImageIO.h>
#import <UniformTypeIdentifiers/UniformTypeIdentifiers.h>

#include <cstdint>
#include <cstdio>
#include <cstring>
#include <string>
#include <vector>

namespace {

// Brand tokens (docs/brand.md). The tile is the same dark in both themes.
constexpr double kTileR = 0x11 / 255.0;
constexpr double kTileG = 0x11 / 255.0;
constexpr double kTileB = 0x11 / 255.0;
constexpr double kMarkR = 0xf1 / 255.0;
constexpr double kMarkG = 0xf1 / 255.0;
constexpr double kMarkB = 0xf1 / 255.0;

// The 512-unit design grid the brand specifies.
constexpr double kGrid = 512.0;
constexpr double kTileRadius = 112.0;
constexpr double kStroke = 28.0;

// A solid CGColor in the sRGB space the design tokens are defined in.
CGColorRef solid_color(double r, double g, double b, double a) {
  const CGFloat comps[4] = {static_cast<CGFloat>(r), static_cast<CGFloat>(g),
                            static_cast<CGFloat>(b), static_cast<CGFloat>(a)};
  CGColorSpaceRef cs = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
  CGColorRef c = CGColorCreate(cs, comps);
  CGColorSpaceRelease(cs);
  return c;
}

// The motif, in grid units: a caret from top to bottom with a line leaving it
// to the right. Whole coordinates, well inside 96..416 on both axes.
//
// The stroke is 28 grid units (the brand's figure weight), but at 16 pt that is
// 0.44 of a pixel: a stroke that thin never covers a whole pixel, so the glyph
// renders as mid-grey mush instead of the mark colour. Small sizes therefore
// keep a stroke at least one *device* pixel wide. That is the same trade the
// brand's "no detail that disappears at 16px" rule makes for its line icons,
// and it only ever thickens the mark — it never changes its shape.
void draw_mark(CGContextRef ctx, double min_px, double px) {
  // Stroke width in grid units, for the current CTM.
  const double ctm_scale = CGContextGetCTM(ctx).a;
  double stroke = kStroke;
  if (ctm_scale > 0 && min_px > stroke * ctm_scale) {
    stroke = min_px / ctm_scale;
  }

  // At small sizes, snap each stroke's centre line to the pixel grid. A 0.44 px
  // stroke straddling two columns is drawn as two half-covered columns, which
  // reads as a grey smudge; the same stroke centred on a column covers one
  // column fully and reads as the mark colour. The strokes are axis-aligned at
  // these sizes anyway, so snapping costs nothing in shape.
  const bool snap = px <= 32 && px > 0;
  const double grid = kGrid / static_cast<double>(px);
  // Centre of the pixel column nearest `v` (grid units).
  auto snap_axis = [&](double v) {
    return (std::floor(v / grid) + 0.5) * grid;
  };
  const double cx = snap ? snap_axis(160.0) : 160.0;
  const double cy_top = snap ? snap_axis(132.0) : 132.0;
  const double cy_bot = snap ? snap_axis(380.0) : 380.0;
  const double tx0 = snap ? snap_axis(232.0) : 232.0;
  const double tx1 = snap ? snap_axis(352.0) : 352.0;
  const double mid_y = snap ? snap_axis(256.0) : 256.0;

  CGContextSetStrokeColorWithColor(ctx, solid_color(kMarkR, kMarkG, kMarkB, 1));
  CGContextSetLineWidth(ctx, stroke);
  CGContextSetLineCap(ctx, kCGLineCapRound);
  CGContextSetLineJoin(ctx, kCGLineJoinRound);

  // The caret: the vertical stroke, inset so its cap stays in the safe square.
  CGContextBeginPath(ctx);
  CGContextMoveToPoint(ctx, cx, cy_top);
  CGContextAddLineToPoint(ctx, cx, cy_bot);
  CGContextStrokePath(ctx);

  // The text running out of it, one line at mid height.
  CGContextBeginPath(ctx);
  CGContextMoveToPoint(ctx, tx0, mid_y);
  CGContextAddLineToPoint(ctx, tx1, mid_y);
  CGContextStrokePath(ctx);
}

// One size of the icon: the tile, then the mark.
void render(int px, const std::string& path) {
  CGColorSpaceRef cs = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
  CGContextRef ctx = CGBitmapContextCreate(
      nullptr, static_cast<size_t>(px), static_cast<size_t>(px), 8, 0, cs,
      static_cast<uint32_t>(kCGImageAlphaPremultipliedLast));
  if (ctx == nullptr) {
    std::fprintf(stderr, "make_icns: cannot create a %d px context\n", px);
    CGColorSpaceRelease(cs);
    return;
  }

  // Scale the 512-unit grid to this size. The mark keeps a minimum stroke so it
  // does not vanish at 16 pt, where 28/512 would be under one pixel.
  const double s = static_cast<double>(px) / kGrid;
  CGContextScaleCTM(ctx, s, s);

  CGColorRef tile = solid_color(kTileR, kTileG, kTileB, 1);
  CGContextSetFillColorWithColor(ctx, tile);
  CGColorRelease(tile);
  // CGContextAddRoundedRect is 10.14+, but the SDK's own declaration is gated
  // oddly in the Command Line Tools; the explicit path works everywhere.
  const CGFloat r = kTileRadius;
  const CGFloat x0 = 0, y0 = 0, x1 = kGrid, y1 = kGrid;
  CGContextBeginPath(ctx);
  CGContextMoveToPoint(ctx, x0 + r, y0);
  CGContextAddArcToPoint(ctx, x1, y0, x1, y0 + r, r);
  CGContextAddArcToPoint(ctx, x1, y1, x1 - r, y1, r);
  CGContextAddArcToPoint(ctx, x0, y1, x0, y1 - r, r);
  CGContextAddArcToPoint(ctx, x0, y0, x0 + r, y0, r);
  CGContextClosePath(ctx);
  CGContextFillPath(ctx);

  draw_mark(ctx, 1.0, static_cast<double>(px));  // min one device pixel

  CGImageRef img = CGBitmapContextCreateImage(ctx);
  if (img == nullptr) {
    std::fprintf(stderr, "make_icns: cannot make an image at %d px\n", px);
    CGContextRelease(ctx);
    CGColorSpaceRelease(cs);
    return;
  }

  // Write through a data object: simpler than juggling a destination URL.
  CFMutableDataRef data = CFDataCreateMutable(kCFAllocatorDefault, 0);
  CGImageDestinationRef out = CGImageDestinationCreateWithData(
      data, CFSTR("public.png"), 1, nullptr);
  if (out == nullptr) {
    std::fprintf(stderr, "make_icns: cannot encode a PNG at %d px\n", px);
    CGImageRelease(img);
    CGContextRelease(ctx);
    CGColorSpaceRelease(cs);
    return;
  }
  CGImageDestinationAddImage(out, img, nullptr);
  if (!CGImageDestinationFinalize(out)) {
    std::fprintf(stderr, "make_icns: PNG encode failed at %d px\n", px);
  }
  CFRelease(out);

  std::FILE* f = std::fopen(path.c_str(), "wb");
  if (f == nullptr) {
    std::fprintf(stderr, "make_icns: cannot write %s\n", path.c_str());
  } else {
    std::fwrite(CFDataGetBytePtr(data), 1,
                static_cast<size_t>(CFDataGetLength(data)), f);
    std::fclose(f);
  }
  CFRelease(data);
  CGImageRelease(img);
  CGContextRelease(ctx);
  CGColorSpaceRelease(cs);
}

}  // namespace

int main(int argc, char** argv) {
  @autoreleasepool {
    if (argc < 3) {
      std::fprintf(stderr, "usage: make_icns OUT.icns ICONSET_DIR\n");
      return 2;
    }
    const std::string icns_path = argv[1];
    const std::string set_dir = argv[2];

    // name in the iconset -> pixel size. Every slot an .icns can hold, plus a
    // 1024 px PNG. The .icns caps at 1024 for the @2x 512 slot anyway, but the
    // Dock and Finder read the PNG for large views and would otherwise scale
    // the 512 down — or up, which is what makes a big icon look soft.
    struct Size {
      const char* name;
      int px;
    };
    static const Size sizes[] = {
        {"icon_16x16.png", 16},       {"icon_16x16@2x.png", 32},
        {"icon_32x32.png", 32},       {"icon_32x32@2x.png", 64},
        {"icon_128x128.png", 128},    {"icon_128x128@2x.png", 256},
        {"icon_256x256.png", 256},    {"icon_256x256@2x.png", 512},
        {"icon_512x512.png", 512},    {"icon_512x512@2x.png", 1024},
    };

    const std::string mkdir = "mkdir -p '" + set_dir + "'";
    if (std::system(mkdir.c_str()) != 0) {
      std::fprintf(stderr, "make_icns: cannot create %s\n", set_dir.c_str());
      return 1;
    }
    for (const Size& s : sizes) {
      render(s.px, set_dir + "/" + s.name);
    }

    // iconutil is part of the Command Line Tools, so this adds no dependency.
    const std::string cmd = "iconutil -c icns '" + set_dir + "' -o '" +
                            icns_path + "'";
    if (std::system(cmd.c_str()) != 0) {
      std::fprintf(stderr, "make_icns: iconutil failed\n");
      return 1;
    }
    std::printf("make_icns: wrote %s\n", icns_path.c_str());
    return 0;
  }
}