// Minimal macOS platform adapter (~420 lines):
// 1. CoreText single-glyph rasterizer (called ONLY on atlas cache misses)
// 2. Thin NSWindow + CALayer BGRA8 framebuffer presenter + raw event forwarder
// 3. NSPasteboard clipboard, native Open/Save/Confirm dialogs, APFS clonefile

#import <AppKit/AppKit.h>
#import <CoreGraphics/CoreGraphics.h>
#import <CoreText/CoreText.h>
#import <IOSurface/IOSurface.h>
#import <QuartzCore/QuartzCore.h>
#include <math.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/clonefile.h>

// ============================================================================
// 1. APFS CoW clonefile
// ============================================================================

int mac_clone_file(const char* src, const char* dst) {
    return clonefile(src, dst, 0);
}

// ============================================================================
// 2. CoreText Single-Cluster Glyph Rasterizer
// ============================================================================

// FontStyle enum matches Rust: 0=Mono, 1=MonoBold, 2=MonoItalic, 3=Ui, 4=UiBold
static CTFontRef resolve_ct_font(uint32_t style, float size_px) {
    NSFont* ns_font = nil;
    CGFloat sz = (CGFloat)(size_px > 4.0f ? size_px : 12.0f);
    switch (style) {
        case 0: // Mono
            ns_font = [NSFont monospacedSystemFontOfSize:sz weight:NSFontWeightRegular];
            break;
        case 1: // MonoBold
            ns_font = [NSFont monospacedSystemFontOfSize:sz weight:NSFontWeightSemibold];
            break;
        case 2: // MonoItalic
            ns_font = [NSFont monospacedSystemFontOfSize:sz weight:NSFontWeightRegular];
            break;
        case 4: // UiBold
            ns_font = [NSFont systemFontOfSize:sz weight:NSFontWeightSemibold];
            break;
        case 3: // Ui
        default:
            ns_font = [NSFont systemFontOfSize:sz weight:NSFontWeightRegular];
            break;
    }
    if (!ns_font) {
        ns_font = [NSFont userFixedPitchFontOfSize:sz];
    }
    return (__bridge CTFontRef)ns_font;
}

typedef struct {
    float ascent;
    float descent;
    float leading;
    uint32_t line_height;
    float cell_width;
} MacFontMetrics;

void mac_font_metrics(uint32_t style, float size_px, MacFontMetrics* out) {
    @autoreleasepool {
        CTFontRef font = resolve_ct_font(style, size_px);
        CGFloat asc = CTFontGetAscent(font);
        CGFloat desc = CTFontGetDescent(font);
        CGFloat lead = CTFontGetLeading(font);
        if (lead < size_px * 0.16) {
            lead = ceil(size_px * 0.22);
        }
        UniChar ch = 'M';
        CGGlyph glyph = 0;
        CGSize adv = CGSizeZero;
        if (CTFontGetGlyphsForCharacters(font, &ch, &glyph, 1)) {
            CTFontGetAdvancesForGlyphs(font, kCTFontOrientationHorizontal, &glyph, &adv, 1);
        } else {
            adv.width = size_px * 0.6;
        }
        out->ascent = (float)asc;
        out->descent = (float)desc;
        out->leading = (float)lead;
        out->line_height = (uint32_t)ceil(asc + desc + lead);
        if (out->line_height < 8) out->line_height = 8;
        out->cell_width = (float)adv.width;
    }
}

typedef struct {
    uint32_t width;
    uint32_t height;
    int32_t bearing_x;
    int32_t bearing_y;
    float advance;
    uint32_t is_color; // 0 = Alpha8, 1 = Bgra32
    uint8_t* pixels;
    size_t pixels_len;
} MacRasterGlyph;

void mac_free_glyph_pixels(uint8_t* ptr) {
    if (ptr) free(ptr);
}

void mac_rasterize_cluster(uint32_t style, float size_px, const uint8_t* utf8, size_t utf8_len,
                           MacRasterGlyph* out) {
    memset(out, 0, sizeof(*out));
    @autoreleasepool {
        NSString* str = [[NSString alloc] initWithBytes:utf8
                                                 length:utf8_len
                                               encoding:NSUTF8StringEncoding];
        if (!str || str.length == 0) {
            return;
        }
        CTFontRef font = resolve_ct_font(style, size_px);
        NSDictionary* attrs = @{
            (id)kCTFontAttributeName: (__bridge id)font,
            (id)kCTForegroundColorAttributeName: (__bridge id)[NSColor whiteColor].CGColor,
        };
        NSAttributedString* attrStr = [[NSAttributedString alloc] initWithString:str attributes:attrs];
        CTLineRef line = CTLineCreateWithAttributedString((__bridge CFAttributedStringRef)attrStr);
        if (!line) return;

        CGFloat asc = 0, desc = 0, lead = 0;
        double adv = CTLineGetTypographicBounds(line, &asc, &desc, &lead);
        out->advance = (float)adv;

        // Detect whether CoreText fell back to a color emoji font.
        BOOL is_color = NO;
        CFArrayRef runs = CTLineGetGlyphRuns(line);
        CFIndex run_count = CFArrayGetCount(runs);
        for (CFIndex i = 0; i < run_count; i++) {
            CTRunRef run = (CTRunRef)CFArrayGetValueAtIndex(runs, i);
            CFDictionaryRef run_attrs = CTRunGetAttributes(run);
            CTFontRef run_font = (CTFontRef)CFDictionaryGetValue(run_attrs, kCTFontAttributeName);
            if (run_font) {
                CTFontSymbolicTraits traits = CTFontGetSymbolicTraits(run_font);
                if (traits & kCTFontTraitColorGlyphs) {
                    is_color = YES;
                    break;
                }
            }
        }

        CGRect bounds = CTLineGetBoundsWithOptions(line, kCTLineBoundsUseGlyphPathBounds);
        if (CGRectIsEmpty(bounds) || bounds.size.width <= 0.0 || bounds.size.height <= 0.0) {
            // Color emoji sometimes reports empty path bounds; use typographic bounds instead.
            if (is_color && adv > 0.0) {
                bounds = CGRectMake(0.0, -desc, adv, asc + desc);
            } else {
                CFRelease(line);
                return;
            }
        }

        int32_t pad = 1;
        int32_t min_x = (int32_t)floor(bounds.origin.x) - pad;
        int32_t min_y = (int32_t)floor(bounds.origin.y) - pad;
        int32_t max_x = (int32_t)ceil(bounds.origin.x + bounds.size.width) + pad;
        int32_t max_y = (int32_t)ceil(bounds.origin.y + bounds.size.height) + pad;

        uint32_t w = (uint32_t)(max_x > min_x ? max_x - min_x : 0);
        uint32_t h = (uint32_t)(max_y > min_y ? max_y - min_y : 0);
        if (w == 0 || h == 0 || w > 512 || h > 512) {
            CFRelease(line);
            return;
        }

        out->width = w;
        out->height = h;
        out->bearing_x = min_x;
        out->bearing_y = max_y;
        out->is_color = is_color ? 1 : 0;

        if (is_color) {
            size_t len = (size_t)w * (size_t)h * 4;
            uint8_t* buf = (uint8_t*)calloc(1, len);
            CGColorSpaceRef cs = CGColorSpaceCreateDeviceRGB();
            CGContextRef ctx = CGBitmapContextCreate(
                buf, w, h, 8, w * 4, cs,
                (CGBitmapInfo)kCGBitmapByteOrder32Little | (CGBitmapInfo)kCGImageAlphaPremultipliedFirst);
            CGColorSpaceRelease(cs);
            if (ctx) {
                CGContextSetAllowsAntialiasing(ctx, true);
                CGContextSetShouldAntialias(ctx, true);
                CGContextSetTextPosition(ctx, (CGFloat)(-min_x), (CGFloat)(-min_y));
                CTLineDraw(line, ctx);
                CGContextRelease(ctx);
            }
            out->pixels = buf;
            out->pixels_len = len;
        } else {
            size_t len = (size_t)w * (size_t)h;
            uint8_t* buf = (uint8_t*)calloc(1, len);
            CGColorSpaceRef cs = CGColorSpaceCreateDeviceGray();
            CGContextRef ctx = CGBitmapContextCreate(buf, w, h, 8, w, cs, (CGBitmapInfo)kCGImageAlphaNone);
            CGColorSpaceRelease(cs);
            if (ctx) {
                CGContextSetAllowsAntialiasing(ctx, true);
                CGContextSetShouldAntialias(ctx, true);
                CGContextSetAllowsFontSmoothing(ctx, true);
                CGContextSetShouldSmoothFonts(ctx, false);
                CGContextSetGrayFillColor(ctx, 1.0, 1.0);
                CGContextSetTextPosition(ctx, (CGFloat)(-min_x), (CGFloat)(-min_y));
                CTLineDraw(line, ctx);
                CGContextRelease(ctx);
            }
            out->pixels = buf;
            out->pixels_len = len;
        }

        CFRelease(line);
    }
}

// ============================================================================
// 3. Clipboard & Dialog Helpers
// ============================================================================

char* mac_clipboard_get(void) {
    @autoreleasepool {
        NSPasteboard* pb = [NSPasteboard generalPasteboard];
        NSString* s = [pb stringForType:NSPasteboardTypeString];
        if (!s) return NULL;
        const char* utf8 = [s UTF8String];
        return utf8 ? strdup(utf8) : NULL;
    }
}

void mac_clipboard_set(const uint8_t* utf8, size_t len) {
    @autoreleasepool {
        NSString* s = [[NSString alloc] initWithBytes:utf8 length:len encoding:NSUTF8StringEncoding];
        if (!s) return;
        NSPasteboard* pb = [NSPasteboard generalPasteboard];
        [pb clearContents];
        [pb setString:s forType:NSPasteboardTypeString];
    }
}

void mac_free_cstr(char* p) {
    if (p) free(p);
}

char* mac_pref_get(const char* key) {
    if (!key) return NULL;
    @autoreleasepool {
        NSString* k = [NSString stringWithUTF8String:key];
        NSString* v = [[NSUserDefaults standardUserDefaults] stringForKey:k];
        if (!v) return NULL;
        const char* utf8 = [v UTF8String];
        return utf8 ? strdup(utf8) : NULL;
    }
}

void mac_pref_set(const char* key, const char* val) {
    if (!key || !val) return;
    @autoreleasepool {
        NSString* k = [NSString stringWithUTF8String:key];
        NSString* v = [NSString stringWithUTF8String:val];
        [[NSUserDefaults standardUserDefaults] setObject:v forKey:k];
    }
}

char* mac_open_file_dialog(int choose_dir) {
    @autoreleasepool {
        NSOpenPanel* panel = [NSOpenPanel openPanel];
        panel.canChooseFiles = choose_dir ? NO : YES;
        panel.canChooseDirectories = choose_dir ? YES : NO;
        panel.allowsMultipleSelection = NO;
        if ([panel runModal] == NSModalResponseOK) {
            NSURL* url = panel.URL;
            if (url && url.path) {
                return strdup([url.path UTF8String]);
            }
        }
        return NULL;
    }
}

char* mac_save_file_dialog(const char* default_name) {
    @autoreleasepool {
        NSSavePanel* panel = [NSSavePanel savePanel];
        if (default_name && default_name[0]) {
            panel.nameFieldStringValue = [NSString stringWithUTF8String:default_name];
        }
        if ([panel runModal] == NSModalResponseOK) {
            NSURL* url = panel.URL;
            if (url && url.path) {
                return strdup([url.path UTF8String]);
            }
        }
        return NULL;
    }
}

// Returns 0=Save, 1=Discard, 2=Cancel
int mac_confirm_discard(const char* file_name) {
    @autoreleasepool {
        NSAlert* alert = [[NSAlert alloc] init];
        alert.alertStyle = NSAlertStyleWarning;
        NSString* name = file_name ? [NSString stringWithUTF8String:file_name] : @"Untitled";
        alert.messageText = [NSString stringWithFormat:@"Save changes to \"%@\"?", name];
        alert.informativeText = @"Your changes will be lost if you do not save them.";
        [alert addButtonWithTitle:@"Save"];
        [alert addButtonWithTitle:@"Don't Save"];
        [alert addButtonWithTitle:@"Cancel"];
        NSModalResponse resp = [alert runModal];
        if (resp == NSAlertFirstButtonReturn) return 0;
        if (resp == NSAlertSecondButtonReturn) return 1;
        return 2;
    }
}

// ============================================================================
// 4. Minimal NSWindow + CALayer BGRA8 Framebuffer Presenter
// ============================================================================

typedef struct {
    void* userdata;
    void (*on_resize)(void* ud, uint32_t w, uint32_t h, float scale);
    void (*on_key)(void* ud, uint16_t keycode, uint32_t mods,
                   const uint8_t* key_utf8, size_t key_len,
                   const uint8_t* text_utf8, size_t text_len);
    void (*on_mouse)(void* ud, uint32_t kind, float x, float y, uint8_t button, uint8_t clicks, uint32_t mods);
    void (*on_scroll)(void* ud, float dx, float dy, float x, float y, uint32_t mods);
    void (*on_focus)(void* ud, uint32_t focused);
    void (*on_theme)(void* ud, uint32_t is_dark);
    void (*on_open_path)(void* ud, const char* path);
    void (*on_command)(void* ud, uint32_t cmd_id);
    void (*on_tick)(void* ud, uint64_t now_ms);
    // Zero-copy render: Rust paints directly into the locked IOSurface memory (no duplicate framebuffer!).
    int (*on_needs_render)(void* ud, uint32_t* out_w, uint32_t* out_h, float* out_scale);
    int (*on_render_into)(void* ud, uint32_t* dst_pixels, uint32_t w, uint32_t h,
                          size_t stride_u32, float scale, int force_full);
} MacCallbacks;

static uint32_t encode_mods(NSEventModifierFlags f) {
    uint32_t m = 0;
    if (f & NSEventModifierFlagShift) m |= 1;
    if (f & NSEventModifierFlagControl) m |= 2;
    if (f & NSEventModifierFlagOption) m |= 4;
    if (f & NSEventModifierFlagCommand) m |= 8;
    return m;
}

static uint64_t monotonic_ms(void) {
    return (uint64_t)([[NSProcessInfo processInfo] systemUptime] * 1000.0);
}

static uint32_t detect_is_dark(NSAppearance* appearance) {
    NSAppearanceName match = [appearance bestMatchFromAppearancesWithNames:@[
        NSAppearanceNameAqua,
        NSAppearanceNameDarkAqua,
    ]];
    return [match isEqualToString:NSAppearanceNameDarkAqua] ? 1 : 0;
}

@interface EditorSurfaceView : NSView {
    IOSurfaceRef _surface;
    uint32_t _surfW;
    uint32_t _surfH;
}
@property (nonatomic) MacCallbacks cb;
- (void)presentFramebuffer;
- (void)notifyResize;
@end

@implementation EditorSurfaceView

- (void)dealloc {
    if (_surface) {
        CFRelease(_surface);
        _surface = NULL;
    }
}

- (BOOL)isFlipped { return YES; }
- (BOOL)acceptsFirstResponder { return YES; }

- (void)viewDidMoveToWindow {
    [super viewDidMoveToWindow];
    self.wantsLayer = YES;
    [self registerForDraggedTypes:@[NSPasteboardTypeFileURL]];
    [self notifyResize];
    [self presentFramebuffer];
}

- (NSDragOperation)draggingEntered:(id<NSDraggingInfo>)sender {
    NSPasteboard* pb = [sender draggingPasteboard];
    if ([pb canReadObjectForClasses:@[[NSURL class]] options:@{NSPasteboardURLReadingFileURLsOnlyKey: @YES}]) {
        return NSDragOperationCopy;
    }
    return NSDragOperationNone;
}

- (BOOL)performDragOperation:(id<NSDraggingInfo>)sender {
    NSPasteboard* pb = [sender draggingPasteboard];
    NSArray<NSURL*>* urls = [pb readObjectsForClasses:@[[NSURL class]]
                                              options:@{NSPasteboardURLReadingFileURLsOnlyKey: @YES}];
    if (urls.count > 0 && self.cb.on_open_path) {
        NSURL* url = urls.firstObject;
        if (url.path) {
            self.cb.on_open_path(self.cb.userdata, [url.path UTF8String]);
            [self presentFramebuffer];
            return YES;
        }
    }
    return NO;
}

- (void)updateTrackingAreas {
    [super updateTrackingAreas];
    for (NSTrackingArea* area in self.trackingAreas) {
        [self removeTrackingArea:area];
    }
    NSTrackingAreaOptions opts = NSTrackingMouseMoved | NSTrackingActiveInKeyWindow | NSTrackingInVisibleRect;
    NSTrackingArea* tracking = [[NSTrackingArea alloc] initWithRect:self.bounds
                                                            options:opts
                                                              owner:self
                                                           userInfo:nil];
    [self addTrackingArea:tracking];
}

- (void)setFrameSize:(NSSize)newSize {
    [super setFrameSize:newSize];
    [self notifyResize];
    [self presentFramebuffer];
}

- (void)viewDidChangeEffectiveAppearance {
    [super viewDidChangeEffectiveAppearance];
    if (self.cb.on_theme) {
        self.cb.on_theme(self.cb.userdata, detect_is_dark(self.effectiveAppearance));
    }
    [self presentFramebuffer];
}

- (void)notifyResize {
    CGFloat scale = self.window.backingScaleFactor;
    if (scale < 1.0) scale = 1.0;
    uint32_t w = (uint32_t)fmax(1.0, round(self.bounds.size.width * scale));
    uint32_t h = (uint32_t)fmax(1.0, round(self.bounds.size.height * scale));
    if (self.cb.on_resize) {
        self.cb.on_resize(self.cb.userdata, w, h, (float)scale);
    }
}

- (void)ensureSurfaceWidth:(uint32_t)w height:(uint32_t)h {
    if (_surface && _surfW == w && _surfH == h) return;
    if (_surface) {
        CFRelease(_surface);
        _surface = NULL;
    }
    size_t bpr = IOSurfaceAlignProperty(kIOSurfaceBytesPerRow, (size_t)w * 4);
    size_t alloc_size = IOSurfaceAlignProperty(kIOSurfaceAllocSize, bpr * (size_t)h);
    NSDictionary* props = @{
        (id)kIOSurfaceWidth: @(w),
        (id)kIOSurfaceHeight: @(h),
        (id)kIOSurfaceBytesPerElement: @(4),
        (id)kIOSurfaceBytesPerRow: @(bpr),
        (id)kIOSurfaceAllocSize: @(alloc_size),
        (id)kIOSurfacePixelFormat: @(0x42475241u), // 'BGRA'
    };
    _surface = IOSurfaceCreate((__bridge CFDictionaryRef)props);
    if (_surface) {
        IOSurfaceSetValue(_surface, kIOSurfaceColorSpace, kCGColorSpaceSRGB);
    }
    _surfW = w;
    _surfH = h;
}

- (void)presentFramebuffer {
    if (!self.cb.on_needs_render || !self.cb.on_render_into) return;
    uint32_t w = 0, h = 0;
    float scale = 1.0f;
    if (!self.cb.on_needs_render(self.cb.userdata, &w, &h, &scale) || w == 0 || h == 0) {
        return;
    }

    BOOL recreated = (!_surface || _surfW != w || _surfH != h);
    [self ensureSurfaceWidth:w height:h];
    if (!_surface) return;

    int painted = 0;
    if (IOSurfaceLock(_surface, 0, NULL) == kIOReturnSuccess) {
        uint32_t* dst = (uint32_t*)IOSurfaceGetBaseAddress(_surface);
        size_t stride_u32 = IOSurfaceGetBytesPerRow(_surface) / 4;
        if (dst && stride_u32 >= w) {
            painted = self.cb.on_render_into(
                self.cb.userdata, dst, w, h, stride_u32, scale, recreated ? 1 : 0);
        }
        IOSurfaceUnlock(_surface, 0, NULL);
    }

    if (painted) {
        [CATransaction begin];
        [CATransaction setDisableActions:YES];
        self.layer.contentsScale = self.window.backingScaleFactor > 0 ? self.window.backingScaleFactor : 1.0;
        self.layer.contents = nil;
        self.layer.contents = (__bridge id)_surface;
        [CATransaction commit];
    }
}

- (NSPoint)pixelPointFromEvent:(NSEvent*)event {
    NSPoint pt = [self convertPoint:event.locationInWindow fromView:nil];
    CGFloat scale = self.window.backingScaleFactor > 0 ? self.window.backingScaleFactor : 1.0;
    return NSMakePoint(pt.x * scale, pt.y * scale);
}

- (void)keyDown:(NSEvent*)event {
    if (!self.cb.on_key) return;
    NSString* rawKey = event.charactersIgnoringModifiers;
    const char* key_utf8 = rawKey ? [rawKey UTF8String] : "";
    size_t key_len = key_utf8 ? strlen(key_utf8) : 0;

    NSString* chars = event.characters;
    const char* text_utf8 = chars ? [chars UTF8String] : "";
    size_t text_len = text_utf8 ? strlen(text_utf8) : 0;

    self.cb.on_key(self.cb.userdata, event.keyCode, encode_mods(event.modifierFlags),
                   (const uint8_t*)key_utf8, key_len,
                   (const uint8_t*)text_utf8, text_len);
    [self presentFramebuffer];
}

- (void)mouseDown:(NSEvent*)event {
    [self.window makeFirstResponder:self];
    NSPoint p = [self pixelPointFromEvent:event];
    self.cb.on_mouse(self.cb.userdata, 0, (float)p.x, (float)p.y, 0,
                     (uint8_t)MIN(event.clickCount, 3), encode_mods(event.modifierFlags));
    [self presentFramebuffer];
}

- (void)mouseDragged:(NSEvent*)event {
    NSPoint p = [self pixelPointFromEvent:event];
    self.cb.on_mouse(self.cb.userdata, 1, (float)p.x, (float)p.y, 0, 1,
                     encode_mods(event.modifierFlags));
    [self presentFramebuffer];
}

- (void)mouseMoved:(NSEvent*)event {
    NSPoint p = [self pixelPointFromEvent:event];
    self.cb.on_mouse(self.cb.userdata, 1, (float)p.x, (float)p.y, 0, 0,
                     encode_mods(event.modifierFlags));
    [self presentFramebuffer];
}

- (void)mouseUp:(NSEvent*)event {
    NSPoint p = [self pixelPointFromEvent:event];
    self.cb.on_mouse(self.cb.userdata, 2, (float)p.x, (float)p.y, 0, 1,
                     encode_mods(event.modifierFlags));
    [self presentFramebuffer];
}

- (void)scrollWheel:(NSEvent*)event {
    NSPoint p = [self pixelPointFromEvent:event];
    CGFloat scale = self.window.backingScaleFactor > 0 ? self.window.backingScaleFactor : 1.0;
    CGFloat mult = event.hasPreciseScrollingDeltas ? scale : 18.0 * scale;
    float dx = (float)(-event.scrollingDeltaX * mult);
    float dy = (float)(-event.scrollingDeltaY * mult);
    self.cb.on_scroll(self.cb.userdata, dx, dy, (float)p.x, (float)p.y,
                      encode_mods(event.modifierFlags));
    [self presentFramebuffer];
}

@end

static NSWindow* g_window = nil;
static EditorSurfaceView* g_view = nil;

void mac_set_title(const char* title) {
    if (!g_window || !title) return;
    @autoreleasepool {
        g_window.title = [NSString stringWithUTF8String:title];
    }
}

void mac_set_cursor(uint32_t cursor_id) {
    switch (cursor_id) {
        case 1: [[NSCursor IBeamCursor] set]; break;
        case 2: [[NSCursor pointingHandCursor] set]; break;
        case 3: [[NSCursor resizeLeftRightCursor] set]; break;
        default: [[NSCursor arrowCursor] set]; break;
    }
}

void mac_request_redraw(void) {
    if (g_view) {
        [g_view presentFramebuffer];
    }
}

void mac_request_quit(void) {
    [NSApp terminate:nil];
}

void mac_activate_app(void) {
    dispatch_async(dispatch_get_main_queue(), ^{
        [NSApp activateIgnoringOtherApps:YES];
        if (g_window) {
            [g_window makeKeyAndOrderFront:nil];
        }
    });
}

@interface EditorAppDelegate : NSObject <NSApplicationDelegate, NSWindowDelegate>
@property (nonatomic) MacCallbacks cb;
@property (nonatomic, strong) NSTimer* timer;
@end

@implementation EditorAppDelegate

- (void)applicationDidFinishLaunching:(NSNotification*)note {
    (void)note;
    [self buildMenus];

    NSRect frame = NSMakeRect(0, 0, 1120, 740);
    NSWindowStyleMask style = NSWindowStyleMaskTitled | NSWindowStyleMaskClosable |
                              NSWindowStyleMaskMiniaturizable | NSWindowStyleMaskResizable;
    g_window = [[NSWindow alloc] initWithContentRect:frame
                                           styleMask:style
                                             backing:NSBackingStoreBuffered
                                               defer:NO];
    g_window.minSize = NSMakeSize(520, 340);
    g_window.title = @"editor";
    g_window.delegate = self;
    g_window.acceptsMouseMovedEvents = YES;

    g_view = [[EditorSurfaceView alloc] initWithFrame:frame];
    g_view.cb = self.cb;
    g_window.contentView = g_view;

    if (self.cb.on_theme) {
        self.cb.on_theme(self.cb.userdata, detect_is_dark(g_view.effectiveAppearance));
    }

    [g_window center];
    [g_window makeKeyAndOrderFront:nil];
    [g_window makeFirstResponder:g_view];
    [NSApp activateIgnoringOtherApps:YES];

    [g_view notifyResize];
    [g_view presentFramebuffer];

    [self startBlinkTimer];
}

- (void)startBlinkTimer {
    if (self.timer) return;
    // Quiet 530ms caret-blink timer (paused completely while unfocused -> 0.00% idle CPU!).
    self.timer = [NSTimer scheduledTimerWithTimeInterval:0.53
                                                  target:self
                                                selector:@selector(onTick:)
                                                userInfo:nil
                                                 repeats:YES];
    self.timer.tolerance = 0.05;
}

- (void)stopBlinkTimer {
    if (self.timer) {
        [self.timer invalidate];
        self.timer = nil;
    }
}

- (void)onTick:(NSTimer*)t {
    (void)t;
    if (self.cb.on_tick) {
        self.cb.on_tick(self.cb.userdata, monotonic_ms());
    }
    if (g_view) {
        [g_view presentFramebuffer];
    }
}

- (void)windowDidBecomeKey:(NSNotification*)note {
    (void)note;
    [self startBlinkTimer];
    if (self.cb.on_focus) self.cb.on_focus(self.cb.userdata, 1);
    if (g_view) [g_view presentFramebuffer];
}

- (void)windowDidResignKey:(NSNotification*)note {
    (void)note;
    [self stopBlinkTimer];
    if (self.cb.on_focus) self.cb.on_focus(self.cb.userdata, 0);
    if (g_view) [g_view presentFramebuffer];
}

- (BOOL)application:(NSApplication*)sender openFile:(NSString*)filename {
    (void)sender;
    if (self.cb.on_open_path && filename) {
        self.cb.on_open_path(self.cb.userdata, [filename UTF8String]);
        if (g_view) [g_view presentFramebuffer];
        return YES;
    }
    return NO;
}

- (void)application:(NSApplication*)sender openFiles:(NSArray<NSString*>*)filenames {
    (void)sender;
    if (filenames.count > 0 && self.cb.on_open_path) {
        self.cb.on_open_path(self.cb.userdata, [filenames.firstObject UTF8String]);
        if (g_view) [g_view presentFramebuffer];
        [NSApp replyToOpenOrPrint:NSApplicationDelegateReplySuccess];
        return;
    }
    [NSApp replyToOpenOrPrint:NSApplicationDelegateReplyFailure];
}

- (BOOL)applicationShouldTerminateAfterLastWindowClosed:(NSApplication*)sender {
    (void)sender;
    return YES;
}

- (void)dispatchMenuCommand:(NSMenuItem*)item {
    if (self.cb.on_command) {
        self.cb.on_command(self.cb.userdata, (uint32_t)item.tag);
    }
    if (g_view) [g_view presentFramebuffer];
}

- (NSMenuItem*)menuItem:(NSString*)title key:(NSString*)key tag:(NSInteger)tag
                   mods:(NSEventModifierFlags)mods {
    NSMenuItem* it = [[NSMenuItem alloc] initWithTitle:title
                                                action:@selector(dispatchMenuCommand:)
                                         keyEquivalent:key];
    it.target = self;
    it.tag = tag;
    if (mods != 0) it.keyEquivalentModifierMask = mods;
    return it;
}

- (void)buildMenus {
    NSMenu* mainMenu = [[NSMenu alloc] init];

    // App Menu
    NSMenuItem* appItem = [[NSMenuItem alloc] init];
    NSMenu* appMenu = [[NSMenu alloc] initWithTitle:@"editor"];
    [appMenu addItem:[self menuItem:@"Quit editor" key:@"q" tag:21 mods:NSEventModifierFlagCommand]];
    appItem.submenu = appMenu;
    [mainMenu addItem:appItem];

    // File Menu
    NSMenuItem* fileItem = [[NSMenuItem alloc] init];
    NSMenu* fileMenu = [[NSMenu alloc] initWithTitle:@"File"];
    [fileMenu addItem:[self menuItem:@"New" key:@"n" tag:0 mods:NSEventModifierFlagCommand]];
    [fileMenu addItem:[self menuItem:@"Open…" key:@"o" tag:1 mods:NSEventModifierFlagCommand]];
    [fileMenu addItem:[self menuItem:@"Open Folder…" key:@"o" tag:2
                                mods:NSEventModifierFlagCommand | NSEventModifierFlagShift]];
    [fileMenu addItem:[self menuItem:@"Quick Open…" key:@"p" tag:22 mods:NSEventModifierFlagCommand]];
    [fileMenu addItem:[NSMenuItem separatorItem]];
    [fileMenu addItem:[self menuItem:@"Save" key:@"s" tag:3 mods:NSEventModifierFlagCommand]];
    [fileMenu addItem:[self menuItem:@"Save As…" key:@"s" tag:4
                                mods:NSEventModifierFlagCommand | NSEventModifierFlagShift]];
    [fileMenu addItem:[self menuItem:@"Reload from Disk" key:@"" tag:5 mods:0]];
    fileItem.submenu = fileMenu;
    [mainMenu addItem:fileItem];

    // Edit Menu
    NSMenuItem* editItem = [[NSMenuItem alloc] init];
    NSMenu* editMenu = [[NSMenu alloc] initWithTitle:@"Edit"];
    [editMenu addItem:[self menuItem:@"Undo" key:@"z" tag:6 mods:NSEventModifierFlagCommand]];
    [editMenu addItem:[self menuItem:@"Redo" key:@"z" tag:7
                                mods:NSEventModifierFlagCommand | NSEventModifierFlagShift]];
    [editMenu addItem:[NSMenuItem separatorItem]];
    [editMenu addItem:[self menuItem:@"Cut" key:@"x" tag:8 mods:NSEventModifierFlagCommand]];
    [editMenu addItem:[self menuItem:@"Copy" key:@"c" tag:9 mods:NSEventModifierFlagCommand]];
    [editMenu addItem:[self menuItem:@"Paste" key:@"v" tag:10 mods:NSEventModifierFlagCommand]];
    [editMenu addItem:[self menuItem:@"Select All" key:@"a" tag:11 mods:NSEventModifierFlagCommand]];
    [editMenu addItem:[NSMenuItem separatorItem]];
    [editMenu addItem:[self menuItem:@"Find…" key:@"f" tag:12 mods:NSEventModifierFlagCommand]];
    [editMenu addItem:[self menuItem:@"Find in Folder…" key:@"f" tag:23
                                mods:NSEventModifierFlagCommand | NSEventModifierFlagShift]];
    [editMenu addItem:[self menuItem:@"Replace…" key:@"f" tag:13
                                mods:NSEventModifierFlagCommand | NSEventModifierFlagOption]];
    [editMenu addItem:[self menuItem:@"Go to Line…" key:@"g" tag:14 mods:NSEventModifierFlagCommand]];
    editItem.submenu = editMenu;
    [mainMenu addItem:editItem];

    // View Menu
    NSMenuItem* viewItem = [[NSMenuItem alloc] init];
    NSMenu* viewMenu = [[NSMenu alloc] initWithTitle:@"View"];
    [viewMenu addItem:[self menuItem:@"Toggle Sidebar" key:@"b" tag:16 mods:NSEventModifierFlagCommand]];
    [viewMenu addItem:[self menuItem:@"Toggle Word Wrap" key:@"z" tag:15 mods:NSEventModifierFlagOption]];
    [viewMenu addItem:[NSMenuItem separatorItem]];
    [viewMenu addItem:[self menuItem:@"Split Editor Right" key:@"\\" tag:24 mods:NSEventModifierFlagCommand]];
    [viewMenu addItem:[self menuItem:@"Close Split Pane" key:@"\\" tag:25
                                mods:NSEventModifierFlagCommand | NSEventModifierFlagShift]];
    [viewMenu addItem:[NSMenuItem separatorItem]];
    [viewMenu addItem:[self menuItem:@"Zoom In" key:@"=" tag:17 mods:NSEventModifierFlagCommand]];
    [viewMenu addItem:[self menuItem:@"Zoom Out" key:@"-" tag:18 mods:NSEventModifierFlagCommand]];
    [viewMenu addItem:[self menuItem:@"Reset Zoom" key:@"0" tag:19 mods:NSEventModifierFlagCommand]];
    [viewMenu addItem:[NSMenuItem separatorItem]];
    [viewMenu addItem:[self menuItem:@"Theme: System" key:@"" tag:100 mods:0]];
    [viewMenu addItem:[self menuItem:@"Theme: Light" key:@"" tag:101 mods:0]];
    [viewMenu addItem:[self menuItem:@"Theme: Dark" key:@"" tag:102 mods:0]];
    viewItem.submenu = viewMenu;
    [mainMenu addItem:viewItem];

    NSApp.mainMenu = mainMenu;
}

@end

static EditorAppDelegate* g_delegate = nil;

void mac_wake_main_loop(void) {
    dispatch_async(dispatch_get_main_queue(), ^{
        if (g_delegate) {
            [g_delegate onTick:nil];
        }
    });
}

void mac_run_app(MacCallbacks cb) {
    @autoreleasepool {
        [NSApplication sharedApplication];
        [NSApp setActivationPolicy:NSApplicationActivationPolicyRegular];
        EditorAppDelegate* del = [[EditorAppDelegate alloc] init];
        del.cb = cb;
        g_delegate = del;
        [NSApp setDelegate:del];
        [NSApp run];
        g_delegate = nil;
    }
}
