// platform/macos/app/gui_main.mm — the M1 AppKit read-only view shell (§3, §8).
//
// A real window whose content view is custom-drawn by `viewcore` through the
// CoreText/CoreGraphics backends. M1 is read-only: scrolling, caret movement,
// and selection. No native controls, no IME, no accessibility (D2, §8).
#import <AppKit/AppKit.h>

#include <cstdio>
#include <memory>
#include <string>

#include "canvas_cg.hpp"
#include "editor.hpp"
#include "text_coretext.hpp"
#include "viewcore.hpp"

namespace {

vc::KeyEvent translate_key(NSEvent* e, std::string* text) {
  vc::KeyEvent k;
  const NSEventModifierFlags m = e.modifierFlags;
  k.shift = (m & NSEventModifierFlagShift) != 0;
  k.cmd = (m & NSEventModifierFlagCommand) != 0;
  k.alt = (m & NSEventModifierFlagOption) != 0;
  k.ctrl = (m & NSEventModifierFlagControl) != 0;
  switch (e.keyCode) {
    case 123: k.key = vc::Key::Left; return k;
    case 124: k.key = vc::Key::Right; return k;
    case 125: k.key = vc::Key::Down; return k;
    case 126: k.key = vc::Key::Up; return k;
    case 115: k.key = vc::Key::Home; return k;
    case 119: k.key = vc::Key::End; return k;
    case 116: k.key = vc::Key::PageUp; return k;
    case 121: k.key = vc::Key::PageDown; return k;
    case 51: k.key = vc::Key::Backspace; return k;
    case 117: k.key = vc::Key::Delete; return k;
    case 36:
    case 76: k.key = vc::Key::Enter; return k;
    case 53: k.key = vc::Key::Escape; return k;
    default: break;
  }
  NSString* plain = e.charactersIgnoringModifiers;
  if (k.cmd && plain.length == 1) {
    const unichar c = [plain characterAtIndex:0];
    k.key = (c == 'a' || c == 'A') ? vc::Key::SelectAll : vc::Key::None;
    return k;
  }
  if (k.cmd || k.ctrl) {
    k.key = vc::Key::None;
    return k;
  }
  NSString* chars = e.characters;
  const char* u = chars.length > 0 ? [chars UTF8String] : nullptr;
  if (u == nullptr) {
    k.key = vc::Key::None;
    return k;
  }
  const unsigned char b = static_cast<unsigned char>(u[0]);
  if (b >= 0x20 || b == '\t') {
    text->assign(u);
    k.key = vc::Key::InsertText;
  } else {
    k.key = vc::Key::None;
  }
  return k;
}

}  // namespace

@interface EditorView : NSView
- (instancetype)initWithEditor:(Editor*)ed;
- (void)reloadDocument;
@end

@implementation EditorView {
  Editor* _ed;
  std::unique_ptr<vc::TextBackend> _tb;
  std::unique_ptr<vc::ViewCore> _view;
}

- (instancetype)initWithEditor:(Editor*)ed {
  self = [super initWithFrame:NSMakeRect(0, 0, 900, 700)];
  if (self != nil) {
    _ed = ed;
    _tb = ed::mac::create_coretext_backend();
    _view = std::make_unique<vc::ViewCore>(*ed, *_tb);
    _view->resize(self.bounds.size.width, self.bounds.size.height);
    _view->set_caret(0, 0);
  }
  return self;
}

- (BOOL)isFlipped {
  return NO;  // CoreGraphics native orientation; the canvas converts y.
}

- (BOOL)acceptsFirstResponder {
  return YES;
}

- (void)reloadDocument {
  _view->set_document(*_ed);
  _view->resize(self.bounds.size.width, self.bounds.size.height);
  _view->set_caret(0, 0);
  [self setNeedsDisplay:YES];
}

- (void)setFrameSize:(NSSize)newSize {
  [super setFrameSize:newSize];
  _view->resize(newSize.width, newSize.height);
  [self setNeedsDisplay:YES];
}

- (void)drawRect:(NSRect)dirtyRect {
  (void)dirtyRect;
  CGContextRef ctx = [[NSGraphicsContext currentContext] CGContext];
  auto canvas = ed::mac::create_cg_canvas(ctx, self.bounds.size.width,
                                          self.bounds.size.height);
  _view->draw(*canvas);
}

- (void)keyDown:(NSEvent*)event {
  const NSEventModifierFlags m = event.modifierFlags;
  if (m & NSEventModifierFlagCommand) {
    NSString* s = event.charactersIgnoringModifiers;
    if (s.length == 1) {
      const unichar c = [s characterAtIndex:0];
      if (c == 'z' || c == 'Z') {
        const bool redo = (m & NSEventModifierFlagShift) != 0;
        const Status st = redo ? _ed->redo() : _ed->undo();
        if (st == Status::Ok) {
          _view->document_changed();
          [self setNeedsDisplay:YES];
        }
        return;
      }
      if (c == 's' || c == 'S') {
        if (_ed->save(nullptr, false, nullptr) == Status::Ok) {
          [self setNeedsDisplay:YES];
        }
        return;
      }
    }
  }
  std::string text;
  const vc::KeyEvent k = translate_key(event, &text);
  vc::KeyEvent ev = k;
  ev.text = text;  // borrowed for the call
  if (ev.key != vc::Key::None && _view->key(ev)) [self setNeedsDisplay:YES];
}

- (void)handleMouse:(NSEvent*)event kind:(vc::MouseEvent::Kind)kind {
  const NSPoint p = [self convertPoint:event.locationInWindow fromView:nil];
  vc::MouseEvent m;
  m.kind = kind;
  m.x = p.x;
  m.y = self.bounds.size.height - p.y;  // AppKit y-up -> viewcore y-down
  m.shift = (event.modifierFlags & NSEventModifierFlagShift) != 0;
  m.cmd = (event.modifierFlags & NSEventModifierFlagCommand) != 0;
  if (_view->mouse(m)) [self setNeedsDisplay:YES];
}

- (void)mouseDown:(NSEvent*)event {
  [self handleMouse:event kind:vc::MouseEvent::Kind::Down];
}
- (void)mouseDragged:(NSEvent*)event {
  [self handleMouse:event kind:vc::MouseEvent::Kind::Drag];
}
- (void)mouseUp:(NSEvent*)event {
  [self handleMouse:event kind:vc::MouseEvent::Kind::Up];
}
- (void)scrollWheel:(NSEvent*)event {
  [self handleMouse:event kind:vc::MouseEvent::Kind::Scroll];
}
@end

@interface AppDelegate : NSObject <NSApplicationDelegate>
@property(nonatomic, assign) Editor* editor;
@property(nonatomic, assign) EditorView* view;
@end

@implementation AppDelegate
- (BOOL)application:(NSApplication*)sender openFile:(NSString*)filename {
  if (self.editor == nullptr || filename == nil) return NO;
  const Status s = self.editor->open(filename.UTF8String);
  if (s != Status::Ok) return NO;
  [self.view reloadDocument];
  [sender activateIgnoringOtherApps:YES];
  return YES;
}
@end

int main(int argc, char** argv) {
  @autoreleasepool {
    std::string path;
    for (int i = 1; i < argc; ++i) {
      const std::string a = argv[i];
      if (!a.empty() && a[0] != '-') {
        path = a;
        break;
      }
    }

    std::unique_ptr<FileSystem> fs;
    create_file_system(&fs);
    std::unique_ptr<Editor> ed;
    create_editor(*fs, &ed);
    if (!path.empty()) {
      const Status s = ed->open(path);
      if (s != Status::Ok) {
        std::fprintf(stderr, "editor: cannot open %s (status %d)\n", path.c_str(),
                     static_cast<int>(s));
      }
    }

    NSApplication* app = [NSApplication sharedApplication];
    [app setActivationPolicy:NSApplicationActivationPolicyRegular];

    const NSWindowStyleMask style = NSWindowStyleMaskTitled |
                                    NSWindowStyleMaskClosable |
                                    NSWindowStyleMaskResizable |
                                    NSWindowStyleMaskMiniaturizable;
    NSWindow* win = [[NSWindow alloc] initWithContentRect:NSMakeRect(0, 0, 900, 700)
                                                styleMask:style
                                                  backing:NSBackingStoreBuffered
                                                    defer:NO];
    [win setTitle:path.empty() ? @"editor"
                               : [NSString stringWithUTF8String:path.c_str()]];

    EditorView* view = [[EditorView alloc] initWithEditor:ed.get()];
    [win setContentView:view];
    [win makeFirstResponder:view];
    [win center];
    [win makeKeyAndOrderFront:nil];

    AppDelegate* delegate = [[AppDelegate alloc] init];
    delegate.editor = ed.get();
    delegate.view = view;
    [app setDelegate:delegate];

    NSMenu* menubar = [[NSMenu alloc] init];
    NSMenuItem* appItem = [[NSMenuItem alloc] init];
    [menubar addItem:appItem];
    NSMenu* appMenu = [[NSMenu alloc] init];
    [appMenu addItemWithTitle:@"Quit editor"
                       action:@selector(terminate:)
                keyEquivalent:@"q"];
    [appItem setSubmenu:appMenu];
    [app setMainMenu:menubar];

    [app activateIgnoringOtherApps:YES];
    [app run];
  }
  return 0;
}
