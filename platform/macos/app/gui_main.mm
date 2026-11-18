// platform/macos/app/gui_main.mm — the macOS app shell (§3, §8, M1/M2).
//
// A real window whose content view is custom-drawn by `viewcore` through the
// CoreText/CoreGraphics backends. Editing, undo/redo, save, a literal find bar,
// and settings (font size / tab width / wrap) are wired here; the document model
// lives in `core`. No native text control, no IME, no accessibility (D2, §8).
#import <AppKit/AppKit.h>

#include <cstdio>
#include <memory>
#include <string>

#include "canvas_cg.hpp"
#include "editor.hpp"
#include "text_coretext.hpp"
#include "viewcore.hpp"

@class EditorView;

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
  if (k.cmd || k.ctrl) {  // command keys are menu items
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

// The find/replace bar (§9 literal find; no regex, no core search API).
@interface FindBar : NSView
- (instancetype)initWithEditorView:(EditorView*)view;
- (void)focusField;
@end

// A thin bottom status bar (chrome; system UI font, §8).
@interface StatusBar : NSView
- (void)setText:(NSString*)text;
@end

@interface EditorView : NSView
@property(nonatomic, assign) FindBar* findBar;
@property(nonatomic, assign) StatusBar* statusBar;
- (instancetype)initWithEditor:(Editor*)ed;
- (void)reloadDocument;
- (void)findText:(NSString*)needle backward:(BOOL)backward;
- (void)replaceText:(NSString*)needle with:(NSString*)replacement;
- (void)replaceAllText:(NSString*)needle with:(NSString*)replacement;
@end

@implementation StatusBar {
  NSTextField* _label;
}
- (instancetype)initWithFrame:(NSRect)frame {
  self = [super initWithFrame:frame];
  if (self != nil) {
    _label = [NSTextField labelWithString:@""];
    _label.translatesAutoresizingMaskIntoConstraints = NO;
    _label.textColor = [NSColor secondaryLabelColor];
    [self addSubview:_label];
    [NSLayoutConstraint activateConstraints:@[
      [_label.leadingAnchor constraintEqualToAnchor:self.leadingAnchor constant:8],
      [_label.centerYAnchor constraintEqualToAnchor:self.centerYAnchor],
      [self.heightAnchor constraintEqualToConstant:22],
    ]];
  }
  return self;
}
- (void)setText:(NSString*)text {
  _label.stringValue = text != nil ? text : @"";
}
@end

@implementation FindBar {
  NSSearchField* _find;
  NSTextField* _replace;
  EditorView* _editorView;  // not retained (the stack owns both)
}

- (instancetype)initWithEditorView:(EditorView*)view {
  self = [super initWithFrame:NSMakeRect(0, 0, 900, 34)];
  if (self != nil) {
    _editorView = view;
    _find = [[NSSearchField alloc] init];
    _find.placeholderString = @"Find";
    _find.sendsSearchStringImmediately = NO;
    _find.target = self;
    _find.action = @selector(findNext:);
    _replace = [[NSTextField alloc] init];
    _replace.placeholderString = @"Replace";
    NSButton* prev =
        [NSButton buttonWithTitle:@"Prev" target:self action:@selector(findPrev:)];
    NSButton* next =
        [NSButton buttonWithTitle:@"Next" target:self action:@selector(findNext:)];
    NSButton* rep =
        [NSButton buttonWithTitle:@"Replace" target:self action:@selector(replaceOne:)];
    NSButton* repAll =
        [NSButton buttonWithTitle:@"All" target:self action:@selector(replaceAll:)];
    NSStackView* row =
        [NSStackView stackViewWithViews:@[_find, _replace, prev, next, rep, repAll]];
    row.orientation = NSUserInterfaceLayoutOrientationHorizontal;
    row.translatesAutoresizingMaskIntoConstraints = NO;
    [self addSubview:row];
    _find.translatesAutoresizingMaskIntoConstraints = NO;
    _replace.translatesAutoresizingMaskIntoConstraints = NO;
    [NSLayoutConstraint activateConstraints:@[
      [row.leadingAnchor constraintEqualToAnchor:self.leadingAnchor constant:8],
      [row.trailingAnchor constraintLessThanOrEqualToAnchor:self.trailingAnchor
                                                   constant:-8],
      [row.centerYAnchor constraintEqualToAnchor:self.centerYAnchor],
      [self.heightAnchor constraintEqualToConstant:34],
      [_find.widthAnchor constraintGreaterThanOrEqualToConstant:180],
      [_replace.widthAnchor constraintGreaterThanOrEqualToConstant:180],
    ]];
  }
  return self;
}

- (void)focusField {
  [self.window makeFirstResponder:_find];
}
- (void)findNext:(id)sender {
  (void)sender;
  [_editorView findText:_find.stringValue backward:NO];
}
- (void)findPrev:(id)sender {
  (void)sender;
  [_editorView findText:_find.stringValue backward:YES];
}
- (void)replaceOne:(id)sender {
  (void)sender;
  [_editorView replaceText:_find.stringValue with:_replace.stringValue];
}
- (void)replaceAll:(id)sender {
  (void)sender;
  [_editorView replaceAllText:_find.stringValue with:_replace.stringValue];
}
@end

@implementation EditorView {
  Editor* _ed;
  std::unique_ptr<vc::TextBackend> _tb;
  std::unique_ptr<vc::ViewCore> _view;
  NSTimer* _blink;
  BOOL _blinkOn;
  double _fontSize;
}

- (instancetype)initWithEditor:(Editor*)ed {
  self = [super initWithFrame:NSMakeRect(0, 0, 900, 700)];
  if (self != nil) {
    _ed = ed;
    _tb = ed::mac::create_coretext_backend();
    _view = std::make_unique<vc::ViewCore>(*ed, *_tb);
    _view->resize(self.bounds.size.width, self.bounds.size.height);
    [self applySettings];
    _view->set_gutter(true);
    _view->set_current_line_color(vc::Color{247, 247, 247, 255});
    _view->set_caret(0, 0);
    [self startBlink];
  }
  return self;
}

- (void)dealloc {
  [_blink invalidate];
}

// --- Settings (§11 M2: font size, tab width, wrap via NSUserDefaults) ------

- (void)applySettings {
  NSUserDefaults* d = [NSUserDefaults standardUserDefaults];
  const double fs = [d doubleForKey:@"FontSize"];
  _fontSize = fs > 0 ? fs : 14.0;
  vc::FontSpec f;
  f.size = _fontSize;
  _view->set_font(f);
  const int tw = static_cast<int>([d integerForKey:@"TabWidth"]);
  _view->set_tab_width(tw > 0 ? tw : 4);
  _view->set_wrap([d boolForKey:@"Wrap"]);
}

- (void)applyFontSize:(double)size {
  if (size < 6 || size > 96) return;
  _fontSize = size;
  vc::FontSpec f = _view->font();
  f.size = size;
  _view->set_font(f);
  [[NSUserDefaults standardUserDefaults] setDouble:size forKey:@"FontSize"];
  [self setNeedsDisplay:YES];
}

- (void)increaseFontSize:(id)sender {
  (void)sender;
  [self applyFontSize:_fontSize + 1];
}
- (void)decreaseFontSize:(id)sender {
  (void)sender;
  [self applyFontSize:_fontSize - 1];
}
- (void)toggleWrap:(id)sender {
  (void)sender;
  const bool on = !_view->wrap();
  _view->set_wrap(on);
  [[NSUserDefaults standardUserDefaults] setBool:on forKey:@"Wrap"];
  [self setNeedsDisplay:YES];
}

// --- Commands --------------------------------------------------------------

- (void)undo:(id)sender {
  (void)sender;
  if (_ed->undo() == Status::Ok) {
    _view->document_changed();
    [self setNeedsDisplay:YES];
  }
}
- (void)redo:(id)sender {
  (void)sender;
  if (_ed->redo() == Status::Ok) {
    _view->document_changed();
    [self setNeedsDisplay:YES];
  }
}
- (void)selectAll:(id)sender {
  (void)sender;
  vc::KeyEvent e;
  e.key = vc::Key::SelectAll;
  if (_view->key(e)) [self setNeedsDisplay:YES];
}
- (void)saveDocument:(id)sender {
  (void)sender;
  if (_ed->save(nullptr, false, nullptr) == Status::Ok) [self setNeedsDisplay:YES];
}
- (void)copy:(id)sender {
  (void)sender;
  const std::string sel = _view->selected_text();
  if (sel.empty()) return;
  NSPasteboard* pb = [NSPasteboard generalPasteboard];
  [pb clearContents];
  [pb setString:[NSString stringWithUTF8String:sel.c_str()]
        forType:NSPasteboardTypeString];
}
- (void)cut:(id)sender {
  [self copy:sender];
  if (_view->backspace()) [self setNeedsDisplay:YES];
}
- (void)paste:(id)sender {
  (void)sender;
  NSString* s = [[NSPasteboard generalPasteboard]
      stringForType:NSPasteboardTypeString];
  const char* u = s != nil ? s.UTF8String : nullptr;
  if (u != nullptr && _view->insert_text(u)) [self setNeedsDisplay:YES];
}
- (void)showFind:(id)sender {
  (void)sender;
  if (self.findBar == nil) return;
  self.findBar.hidden = !self.findBar.hidden;
  if (!self.findBar.hidden) [self.findBar focusField];
}

- (BOOL)validateMenuItem:(NSMenuItem*)item {
  if (item.action == @selector(toggleWrap:)) {
    item.state = _view->wrap() ? NSControlStateValueOn : NSControlStateValueOff;
  }
  return YES;
}

// --- Find/replace ----------------------------------------------------------

- (void)findText:(NSString*)needle backward:(BOOL)backward {
  if (needle == nil || needle.length == 0) return;
  if (_view->find(needle.UTF8String, !backward, false)) [self setNeedsDisplay:YES];
}
- (void)replaceText:(NSString*)needle with:(NSString*)replacement {
  if (needle == nil || needle.length == 0) return;
  const char* rep = replacement != nil ? replacement.UTF8String : "";
  _view->replace_current(needle.UTF8String, rep, false);
  [self setNeedsDisplay:YES];
}
- (void)replaceAllText:(NSString*)needle with:(NSString*)replacement {
  if (needle == nil || needle.length == 0) return;
  const char* rep = replacement != nil ? replacement.UTF8String : "";
  if (_view->replace_all(needle.UTF8String, rep, false) > 0) {
    [self setNeedsDisplay:YES];
  }
}

// --- Blink -----------------------------------------------------------------

- (void)startBlink {
  _blinkOn = YES;
  _view->set_caret_visible(YES);
  [_blink invalidate];
  _blink = [NSTimer scheduledTimerWithTimeInterval:0.5
                                            target:self
                                          selector:@selector(toggleCaret:)
                                          userInfo:nil
                                           repeats:YES];
}

- (void)toggleCaret:(NSTimer*)timer {
  (void)timer;
  _blinkOn = !_blinkOn;
  _view->set_caret_visible(_blinkOn);
  [self setNeedsDisplay:YES];
}

// --- View ------------------------------------------------------------------

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
  [self updateStatus];
}

- (void)updateStatus {
  if (self.statusBar == nil) return;
  const vc::Selection s = _view->selection();
  const int64_t lines = _view->line_count();
  NSString* text = [NSString
      stringWithFormat:@"Ln %lld, Col %lld    %lld lines%@",
                       static_cast<long long>(s.caret_line + 1),
                       static_cast<long long>(s.caret_col + 1),
                       static_cast<long long>(lines),
                       _view->line_count_known() ? @"" : @"+"];
  [self.statusBar setText:text];
}

- (void)keyDown:(NSEvent*)event {
  [self startBlink];  // solid caret while typing
  std::string text;
  const vc::KeyEvent k = translate_key(event, &text);
  vc::KeyEvent ev = k;
  ev.text = text;  // borrowed for the call
  if (ev.key != vc::Key::None && _view->key(ev)) [self setNeedsDisplay:YES];
}

- (void)handleMouse:(NSEvent*)event kind:(vc::MouseEvent::Kind)kind {
  if (kind != vc::MouseEvent::Kind::Scroll) [self startBlink];
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

- (BOOL)applicationShouldTerminateAfterLastWindowClosed:(NSApplication*)sender {
  (void)sender;
  return YES;
}

// Prompt before discarding unsaved edits (§5.6-ish; the GUI owns this).
- (BOOL)windowShouldClose:(NSWindow*)sender {
  (void)sender;
  if (self.editor == nullptr) return YES;
  Info in;
  if (self.editor->info(&in) != Status::Ok || !in.dirty) return YES;
  NSAlert* a = [[NSAlert alloc] init];
  a.messageText = @"Do you want to save the changes?";
  a.informativeText = @"Your changes will be lost if you don't save them.";
  [a addButtonWithTitle:@"Save"];
  [a addButtonWithTitle:@"Don't Save"];
  [a addButtonWithTitle:@"Cancel"];
  const NSModalResponse r = [a runModal];
  if (r == NSAlertFirstButtonReturn) {
    return self.editor->save(nullptr, false, nullptr) == Status::Ok;
  }
  if (r == NSAlertSecondButtonReturn) return YES;
  return NO;
}

- (NSApplicationTerminateReply)applicationShouldTerminate:(NSApplication*)sender {
  for (NSWindow* w in sender.windows) {
    if (![self windowShouldClose:w]) return NSTerminateCancel;
  }
  return NSTerminateNow;
}
@end

namespace {

NSMenuItem* item(NSString* title, SEL action, NSString* key) {
  return [[NSMenuItem alloc] initWithTitle:title
                                    action:action
                             keyEquivalent:key];
}

NSMenu* submenu(NSString* title, NSArray<NSMenuItem*>* items) {
  NSMenu* m = [[NSMenu alloc] initWithTitle:title];
  for (NSMenuItem* it in items) [m addItem:it];
  return m;
}

void build_menu_bar(NSApplication* app) {
  NSMenu* menubar = [[NSMenu alloc] init];

  NSMenuItem* appItem = [[NSMenuItem alloc] init];
  [menubar addItem:appItem];
  [appItem setSubmenu:submenu(@"editor", @[
    item(@"About editor", @selector(orderFrontStandardAboutPanel:), @""),
    [NSMenuItem separatorItem],
    item(@"Quit editor", @selector(terminate:), @"q"),
  ])];

  NSMenuItem* fileItem = [[NSMenuItem alloc] init];
  [menubar addItem:fileItem];
  [fileItem setSubmenu:submenu(@"File", @[
    item(@"Save", @selector(saveDocument:), @"s"),
  ])];

  NSMenuItem* editItem = [[NSMenuItem alloc] init];
  [menubar addItem:editItem];
  NSMenuItem* redo = item(@"Redo", @selector(redo:), @"z");
  redo.keyEquivalentModifierMask =
      NSEventModifierFlagCommand | NSEventModifierFlagShift;
  [editItem setSubmenu:submenu(@"Edit", @[
    item(@"Undo", @selector(undo:), @"z"),
    redo,
    [NSMenuItem separatorItem],
    item(@"Cut", @selector(cut:), @"x"),
    item(@"Copy", @selector(copy:), @"c"),
    item(@"Paste", @selector(paste:), @"v"),
    item(@"Select All", @selector(selectAll:), @"a"),
    [NSMenuItem separatorItem],
    item(@"Find\u2026", @selector(showFind:), @"f"),
  ])];

  NSMenuItem* viewItem = [[NSMenuItem alloc] init];
  [menubar addItem:viewItem];
  [viewItem setSubmenu:submenu(@"View", @[
    item(@"Wrap Lines", @selector(toggleWrap:), @""),
    [NSMenuItem separatorItem],
    item(@"Bigger", @selector(increaseFontSize:), @"+"),
    item(@"Smaller", @selector(decreaseFontSize:), @"-"),
  ])];

  [app setMainMenu:menubar];
}

}  // namespace

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
    FindBar* bar = [[FindBar alloc] initWithEditorView:view];
    StatusBar* status = [[StatusBar alloc] initWithFrame:NSMakeRect(0, 0, 900, 22)];
    view.findBar = bar;
    view.statusBar = status;
    bar.hidden = YES;

    NSStackView* stack = [NSStackView stackViewWithViews:@[ bar, view, status ]];
    stack.orientation = NSUserInterfaceLayoutOrientationVertical;
    stack.distribution = NSStackViewDistributionFill;
    stack.autoresizingMask = NSViewWidthSizable | NSViewHeightSizable;
    [bar setContentHuggingPriority:NSLayoutPriorityRequired
                    forOrientation:NSLayoutConstraintOrientationVertical];
    [status setContentHuggingPriority:NSLayoutPriorityRequired
                       forOrientation:NSLayoutConstraintOrientationVertical];
    [view setContentHuggingPriority:NSLayoutPriorityDefaultLow
                     forOrientation:NSLayoutConstraintOrientationVertical];

    [win setContentView:stack];
    [win makeFirstResponder:view];
    [win center];
    [win makeKeyAndOrderFront:nil];

    AppDelegate* delegate = [[AppDelegate alloc] init];
    delegate.editor = ed.get();
    delegate.view = view;
    [app setDelegate:delegate];

    build_menu_bar(app);

    [app activateIgnoringOtherApps:YES];
    [app run];
  }
  return 0;
}
