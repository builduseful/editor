// platform/macos/app/gui_main.mm — the macOS app shell (§3, §8, M1/M2).
//
// A real window whose content view is custom-drawn by `viewcore` through the
// CoreText/CoreGraphics backends. Editing, undo/redo, save, a literal find bar,
// and settings (font size / tab width / wrap) are wired here; the document model
// lives in `core`. No native text control, no IME, no accessibility (D2, §8).
#import <AppKit/AppKit.h>
#import <CoreGraphics/CoreGraphics.h>

#include <cstdio>
#include <cstdlib>
#include <memory>
#include <string>
#include <unistd.h>

#include "canvas_cg.hpp"
#include "editor.hpp"
#include "text_coretext.hpp"
#include "dir_source_posix.hpp"
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

// A private temp file backs an untitled window, so it is a real, editable,
// savable document from the first paint onward.
std::string untitled_path(int attempt) {
  NSString* dir = NSTemporaryDirectory();
  NSString* name = [NSString
      stringWithFormat:@"editor-untitled-%d-%llu-%d.txt", getpid(),
                       (unsigned long long)(NSDate.date.timeIntervalSince1970 * 1000),
                       attempt];
  return std::string([[dir stringByAppendingPathComponent:name] UTF8String]);
}

}  // namespace

NSColor* ns_color(vc::Color c) {
  return [NSColor colorWithSRGBRed:c.r / 255.0
                             green:c.g / 255.0
                              blue:c.b / 255.0
                             alpha:c.a / 255.0];
}

// The directory holding `path` (everything before the last '/'), or "" if it
// has none. Used to root the sidebar at the open document's folder.
static std::string parent_dir(const std::string& path) {
  const size_t slash = path.find_last_of('/');
  if (slash == std::string::npos) return std::string();
  if (slash == 0) return std::string("/");
  return path.substr(0, slash);
}

// Renders any view through AppKit's own context to a PNG (the `--snapshot`
// smoke test, so the real GUI output can be inspected without a display).
static BOOL write_view_png(NSView* v, NSString* path) {
  if (v == nil || path == nil) return NO;
  const NSRect b = v.bounds;
  NSBitmapImageRep* rep = [v bitmapImageRepForCachingDisplayInRect:b];
  [v cacheDisplayInRect:b toBitmapImageRep:rep];
  NSData* png = [rep representationUsingType:NSBitmapImageFileTypePNG
                                  properties:@{}];
  return [png writeToFile:path atomically:YES];
}

// The About panel's content view: closes the panel on Escape, so the panel
// behaves like a sheet rather than a window you must click away from.
@interface AboutPanelView : NSView
@end

@implementation AboutPanelView
- (BOOL)acceptsFirstResponder {
  return YES;
}
- (void)cancelOperation:(id)sender {
  (void)sender;
  [self.window close];
}
@end

// The find/replace bar (§9 literal find; no regex, no core search API).
@interface FindBar : NSView <NSSearchFieldDelegate>
- (instancetype)initWithEditorView:(EditorView*)view;
- (void)focusField;
- (void)setPageColor:(vc::Color)c;
- (NSString*)needle;
// Menu actions, forwarded to the editor view. They are repeated here because a
// menu item with no target travels the responder chain, and while the field is
// focused that chain runs through this bar, not through the editor view.
- (void)findNextMatch:(id)sender;
- (void)findPreviousMatch:(id)sender;
- (void)hideFind:(id)sender;
@end

// A thin bottom status bar (chrome; system UI font, §8).
@interface StatusBar : NSView
- (void)setText:(NSString*)text;
- (void)applyTheme:(vc::Theme)t;
@end

// The file sidebar (§9). A recursive, lazily-read tree, custom-drawn (D2) from
// the same tokens as the document view — no NSOutlineView, no native control.
// The tree itself lives in viewcore (headless-tested); this is its AppKit shell.
@interface SidebarView : NSView
// The document view to open a clicked file through (not retained).
@property(nonatomic, assign) EditorView* editorView;
- (void)applyTheme:(vc::Theme)t;
- (void)setRootPath:(NSString*)path;
- (NSString*)rootPath;
// Re-reads a loaded directory (its contents changed on disk).
- (void)refreshPath:(NSString*)path;
// The visible rows (for a caller that wants to refresh every loaded directory).
- (const std::vector<vc::Sidebar::Row>&)rows;
// Reveals `path` (expands its ancestors) and selects it.
- (void)revealPath:(NSString*)path;
@end

@interface EditorView : NSView
@property(nonatomic, assign) FindBar* findBar;
@property(nonatomic, assign) StatusBar* statusBar;
// The file sidebar (§9); nil until the window is built.
@property(nonatomic, assign) SidebarView* sidebar;
// The document's path (nil while untitled). Kept here because the core's
// `info()` is too expensive to call on every frame just to read the path.
@property(nonatomic, copy) NSString* documentPath;
// A temporary file backing an untitled window (nil once it has a real path).
// Removed when the buffer is replaced or saved somewhere real.
@property(nonatomic, copy) NSString* untitledTempPath;
- (instancetype)initWithEditor:(Editor*)ed;
- (void)reloadDocument;
- (void)beginDiscovery;
- (void)refreshTheme;
- (BOOL)writeSnapshotTo:(NSString*)path;
- (BOOL)openPath:(NSString*)path;
// Replaces the document with a fresh untitled buffer (callers confirm first).
- (BOOL)startUntitled;
- (void)updateTitle;
- (void)scrollToLine:(int64_t)line;
- (void)scrollToX:(double)px;
- (vc::ViewCore*)core;
- (void)handleMouse:(NSEvent*)event kind:(vc::MouseEvent::Kind)kind;
// Indexes the whole document (under the discovery cap) and shows the caret, so
// a snapshot is the final, steady frame rather than a mid-discovery one.
- (void)finishIndex;
- (void)discardUntitledTemp;
// YES if the document may be discarded (clean, or the user saved / chose not to).
- (BOOL)confirmDiscardChanges;
- (void)findText:(NSString*)needle backward:(BOOL)backward;
- (void)replaceText:(NSString*)needle with:(NSString*)replacement;
- (void)replaceAllText:(NSString*)needle with:(NSString*)replacement;
// ⌘G / ⇧⌘G: search for the bar's needle, else the find pasteboard's, wrapping.
- (void)findNextMatch:(id)sender;
- (void)findPreviousMatch:(id)sender;
// Escape: close the bar and give the document the keyboard back.
- (void)hideFind:(id)sender;
// File → Revert to Saved (asks first if there are unsaved changes).
- (void)revertDocument:(id)sender;
// View → Show Sidebar (hidden on a narrow window; the default is shown).
- (void)toggleSidebar:(id)sender;
// Re-reads the sidebar's open directories from disk (the window became key).
- (void)refreshSidebarFromDisk;
@end

@implementation StatusBar {
  NSTextField* _label;
}
- (instancetype)initWithFrame:(NSRect)frame {
  self = [super initWithFrame:frame];
  if (self != nil) {
    _label = [NSTextField labelWithString:@""];
    // Chrome meta type: the system UI face at the small end of the ramp.
    _label.font = [NSFont systemFontOfSize:11];
    _label.translatesAutoresizingMaskIntoConstraints = NO;
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
- (void)applyTheme:(vc::Theme)t {
  self.wantsLayer = YES;
  self.layer.backgroundColor = ns_color(t.page).CGColor;
  _label.textColor = ns_color(t.gutter_text);  // color.text-muted
}
@end

@implementation SidebarView {
  ed::mac::PosixDirSource _src;
  std::unique_ptr<vc::TextBackend> _tb;
  std::unique_ptr<vc::Sidebar> _sidebar;
  vc::Theme _theme;
  BOOL _hasRoot;
}

- (instancetype)initWithFrame:(NSRect)frame {
  self = [super initWithFrame:frame];
  if (self != nil) {
    _tb = ed::mac::create_coretext_backend();
    _sidebar = std::make_unique<vc::Sidebar>(_src, *_tb);
    vc::FontSpec f;
    f.size = 13.0;
    f.ui = true;  // chrome, not document text
    _sidebar->set_font(f);
    _theme = vc::light_theme();
    self.wantsLayer = YES;
  }
  return self;
}

- (BOOL)isFlipped {
  return NO;  // CoreGraphics native orientation; the canvas converts y
}

- (BOOL)acceptsFirstResponder {
  return YES;
}

// Arrow keys, Return and Escape drive the tree (the document view has its own
// key handling, so this only runs while the sidebar is focused).
- (void)keyDown:(NSEvent*)event {
  const bool changed = [self handleKey:event];
  if (changed) [self setNeedsDisplay:YES];
  if (event.keyCode == 36 || event.keyCode == 76) {
    // Return / keypad Enter: activate the row.
    [self activateSelected];
  }
}

- (void)applyTheme:(vc::Theme)t {
  _theme = t;
  self.layer.backgroundColor = ns_color(t.page).CGColor;
  [self setNeedsDisplay:YES];
}

- (void)setRootPath:(NSString*)path {
  _hasRoot = _sidebar->set_root(path != nil ? path.UTF8String : "");
  [self setNeedsDisplay:YES];
}

- (NSString*)rootPath {
  const std::string& root = _sidebar->root();
  return root.empty() ? nil : [NSString stringWithUTF8String:root.c_str()];
}

- (void)refreshPath:(NSString*)path {
  if (path == nil) return;
  if (_sidebar->refresh(path.UTF8String)) [self setNeedsDisplay:YES];
}

- (const std::vector<vc::Sidebar::Row>&)rows {
  return _sidebar->rows();
}

- (void)revealPath:(NSString*)path {
  if (path == nil) return;
  _sidebar->reveal(path.UTF8String);
  [self setNeedsDisplay:YES];
}

- (void)drawRect:(NSRect)dirtyRect {
  (void)dirtyRect;
  const NSSize size = self.bounds.size;
  _sidebar->set_height(size.height);
  CGContextRef ctx = [[NSGraphicsContext currentContext] CGContext];
  auto canvas = ed::mac::create_cg_canvas(ctx, size.width, size.height);
  _sidebar->draw(*canvas, _theme);
}

// Returns true if the tree changed (a redraw is needed).
- (BOOL)handleKey:(NSEvent*)event {
  switch (event.keyCode) {
    case 125: return _sidebar->move_down();   // Down
    case 126: return _sidebar->move_up();     // Up
    case 123: return _sidebar->move_left();   // Left
    case 124: return _sidebar->move_right();  // Right
    default: return NO;
  }
}

// Return on a directory toggles it; on a file it opens through the same
// confirm-then-open path a click uses.
- (void)activateSelected {
  const std::string* path = nullptr;
  if (!_sidebar->activate(&path)) {
    [self setNeedsDisplay:YES];
    return;
  }
  if (path == nullptr) {
    [self setNeedsDisplay:YES];  // a directory was toggled
    return;
  }
  if (self.editorView == nil) return;
  if (![self.editorView confirmDiscardChanges]) return;
  if (![self.editorView openPath:[NSString stringWithUTF8String:path->c_str()]]) {
    [self.editorView.sidebar revealPath:self.editorView.documentPath];
  }
  [self setNeedsDisplay:YES];
}

// The sidebar scrolls independently of the document.
- (void)scrollWheel:(NSEvent*)event {
  const double scale =
      event.hasPreciseScrollingDeltas ? 1.0 : 3.0 * vc::Sidebar::kRowHeight;
  _sidebar->scroll_by(-event.scrollingDeltaY * scale);
  [self setNeedsDisplay:YES];
}

// A click selects the row, opens a file, or toggles a directory.
- (void)mouseDown:(NSEvent*)event {
  [self.window makeFirstResponder:self];  // the arrow keys follow the click
  const NSPoint p = [self convertPoint:event.locationInWindow fromView:nil];
  const double y = self.bounds.size.height - p.y;  // AppKit y-up -> viewcore y-down
  const int row = _sidebar->row_at(p.x, y);
  if (row < 0) return;
  const std::vector<vc::Sidebar::Row>& rows = _sidebar->rows();
  if (static_cast<size_t>(row) >= rows.size()) return;
  const vc::SidebarNode* node = rows[static_cast<size_t>(row)].node;
  if (node == nullptr) return;
  if (node->is_dir && !node->is_symlink) {
    if (_sidebar->toggle(node)) [self setNeedsDisplay:YES];
    return;
  }
  // Opening a file replaces the document, so the user is warned about unsaved
  // edits first — the same prompt File > Open gets (§9 "read-only navigation").
  if (self.editorView == nil) return;
  if (![self.editorView confirmDiscardChanges]) return;
  _sidebar->set_selected(node);
  [self setNeedsDisplay:YES];
  if (![self.editorView openPath:[NSString stringWithUTF8String:node->path.c_str()]]) {
    // The open failed (a race with the file being removed, say): put the
    // selection back on the document that is still open.
    [self.editorView.sidebar revealPath:self.editorView.documentPath];
  }
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
    _find.delegate = self;  // Escape / Shift+Return, below
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
  // Start from the last needle (the find pasteboard), so ⌘F after a search with
  // the bar closed picks up where the search left off.
  if (_find.stringValue.length == 0) {
    NSString* last = [[NSPasteboard pasteboardWithName:NSPasteboardNameFind]
        stringForType:NSPasteboardTypeString];
    if (last.length > 0) _find.stringValue = last;
  }
  [self.window makeFirstResponder:_find];
}
- (NSString*)needle {
  return _find.stringValue;
}
// Shift+Return searches backward; plain Return returns NO so the field's own
// action (findNext:) runs once, as it always has. Escape closes the bar and
// hands the keyboard back to the document. Everything else (arrows, delete,
// copy/paste) stays the field's own business.
- (BOOL)control:(NSControl*)control
              textView:(NSTextView*)textView
    doCommandBySelector:(SEL)command {
  (void)control;
  if (command == @selector(cancelOperation:)) {
    [_editorView hideFind:nil];
    return YES;
  }
  if (command == @selector(insertNewline:)) {
    const BOOL shift =
        ([NSApp currentEvent].modifierFlags & NSEventModifierFlagShift) != 0;
    if (!shift) return NO;  // the field's action searches forward
    NSString* needle = textView.string;
    if (needle.length > 0) [_editorView findText:needle backward:YES];
    return YES;
  }
  return NO;
}
- (void)findNextMatch:(id)sender {
  [_editorView findNextMatch:sender];
}
- (void)findPreviousMatch:(id)sender {
  [_editorView findPreviousMatch:sender];
}
- (void)hideFind:(id)sender {
  [_editorView hideFind:sender];
}
- (void)setPageColor:(vc::Color)c {
  self.wantsLayer = YES;
  self.layer.backgroundColor = ns_color(c).CGColor;
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
  BOOL _maybeDirty;      // conservative: set on edits, cleared on save/open
  BOOL _discovering;     // an incremental index pass is in flight
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
    [self refreshTheme];
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
// The window became key: the user has been elsewhere (another app, Finder), so
// the files may have changed. Re-read the open directories — incremental, not a
// whole-tree walk, and it happens off the frame path.
- (void)refreshSidebarFromDisk {
  if (self.sidebar == nil) return;
  for (const vc::Sidebar::Row& r : self.sidebar.rows) {
    if (r.node != nullptr && r.node->is_dir && r.node->loaded) {
      [self.sidebar refreshPath:[NSString stringWithUTF8String:r.node->path.c_str()]];
    }
  }
}

// The sidebar is a column in the window's top-level stack; hiding it is a
// layout change, not a document one.
- (void)toggleSidebar:(id)sender {
  (void)sender;
  if (self.sidebar == nil) return;
  const BOOL show = !self.sidebar.isHidden;
  self.sidebar.hidden = show;
  [[NSUserDefaults standardUserDefaults] setBool:!show forKey:@"ShowSidebar"];
}

// --- Theme (Build Useful tokens; System / Light / Dark, §8) ----------------

- (BOOL)isDark {
  NSAppearanceName name = [self.effectiveAppearance
      bestMatchFromAppearancesWithNames:@[ NSAppearanceNameAqua,
                                           NSAppearanceNameDarkAqua ]];
  return [name isEqualToString:NSAppearanceNameDarkAqua];
}
- (void)applyThemeColors {
  const vc::Theme t = [self isDark] ? vc::dark_theme() : vc::light_theme();
  _view->set_theme(t);
  [self.findBar setPageColor:t.page];
  [self.statusBar applyTheme:t];
  if (self.sidebar != nil) [self.sidebar applyTheme:t];
  [self setNeedsDisplay:YES];
}
- (void)refreshTheme {
  NSString* pref = [[NSUserDefaults standardUserDefaults] stringForKey:@"Theme"];
  NSAppearance* ap = nil;
  if ([pref isEqualToString:@"light"]) {
    ap = [NSAppearance appearanceNamed:NSAppearanceNameAqua];
  } else if ([pref isEqualToString:@"dark"]) {
    ap = [NSAppearance appearanceNamed:NSAppearanceNameDarkAqua];
  }
  NSApp.appearance = ap;  // nil follows the system
  [self applyThemeColors];
}
- (void)viewDidChangeEffectiveAppearance {
  [super viewDidChangeEffectiveAppearance];
  [self applyThemeColors];
}
- (void)setThemeSystem:(id)sender {
  (void)sender;
  [[NSUserDefaults standardUserDefaults] setObject:@"system" forKey:@"Theme"];
  [self refreshTheme];
}
- (void)setThemeLight:(id)sender {
  (void)sender;
  [[NSUserDefaults standardUserDefaults] setObject:@"light" forKey:@"Theme"];
  [self refreshTheme];
}
- (void)setThemeDark:(id)sender {
  (void)sender;
  [[NSUserDefaults standardUserDefaults] setObject:@"dark" forKey:@"Theme"];
  [self refreshTheme];
}

// --- Commands --------------------------------------------------------------

- (void)undo:(id)sender {
  (void)sender;
  if (_ed->undo() == Status::Ok) {
    _view->document_changed();
    [self markEdited];
  }
}
- (void)redo:(id)sender {
  (void)sender;
  if (_ed->redo() == Status::Ok) {
    _view->document_changed();
    [self markEdited];
  }
}
- (void)selectAll:(id)sender {
  (void)sender;
  vc::KeyEvent e;
  e.key = vc::Key::SelectAll;
  if (_view->key(e)) [self setNeedsDisplay:YES];
}

// --- Files ----------------------------------------------------------------

- (void)openDocument:(id)sender {
  (void)sender;
  if (![self confirmDiscardChanges]) return;
  NSOpenPanel* p = [NSOpenPanel openPanel];
  p.canChooseFiles = YES;
  p.canChooseDirectories = NO;
  p.allowsMultipleSelection = NO;
  p.resolvesAliases = YES;
  if ([p runModal] != NSModalResponseOK || p.URL == nil) return;
  [self openPath:p.URL.path];
}

// Opens `path` without prompting (the callers confirm first). Returns NO and
// shows an alert if the core refuses it.
- (BOOL)openPath:(NSString*)path {
  if (path == nil) return NO;
  if (_ed->open(path.UTF8String) != Status::Ok) {
    [self showError:@"Could not open the file."];
    return NO;
  }
  [self discardUntitledTemp];  // the core moved off the untitled buffer
  self.documentPath = path;
  _maybeDirty = NO;
  [self reloadDocument];
  [self syncSidebarToDocument];
  // First paint stays lazy; fill in the exact count (and the scrollbar) once the
  // window has drawn.
  [self performSelector:@selector(beginDiscovery)
             withObject:nil
             afterDelay:0.1];
  [self updateTitle];
  return YES;
}

// Points the sidebar at the open file's folder and reveals the file. Called
// after a document is opened (open, Revert to Saved, a click in the sidebar).
- (void)syncSidebarToDocument {
  if (self.sidebar == nil || self.documentPath == nil) return;
  const std::string path = self.documentPath.UTF8String;
  const std::string parent = parent_dir(path);
  if (parent.empty()) return;
  // Re-rooting collapses the tree, so only do it when the folder changed. When
  // it did not (Save As inside the same folder, say) the file may still be new
  // on disk, so re-read the directory rather than leaving a stale row set.
  NSString* root = [NSString stringWithUTF8String:parent.c_str()];
  if (![root isEqualToString:self.sidebar.rootPath]) {
    [self.sidebar setRootPath:root];
  } else {
    [self.sidebar refreshPath:root];
  }
  [self.sidebar revealPath:self.documentPath];
}

// An untitled window is backed by a temp file so it is a real, editable document.
// Remove it once the document has a path of its own (open / Save As) or on quit.
- (void)discardUntitledTemp {
  if (self.untitledTempPath == nil) return;
  std::remove(self.untitledTempPath.UTF8String);
  self.untitledTempPath = nil;
}

// File > New (and the first launch with no file): a document with no path yet,
// backed by a temp file so it can be edited, undone, and saved.
- (BOOL)startUntitled {
  for (int attempt = 0; attempt < 3; ++attempt) {
    const std::string tmp = untitled_path(attempt);
    if (_ed->create(tmp) != Status::Ok) continue;
    [self discardUntitledTemp];  // the core moved off the previous buffer
    self.untitledTempPath = [NSString stringWithUTF8String:tmp.c_str()];
    self.documentPath = nil;
    _maybeDirty = NO;
    [self reloadDocument];
    [self updateTitle];
    [self beginDiscovery];
    return YES;
  }
  [self showError:@"Could not create a new document."];
  return NO;
}

- (void)newDocument:(id)sender {
  (void)sender;
  if (![self confirmDiscardChanges]) return;
  [self startUntitled];
  [self setNeedsDisplay:YES];
}

- (void)saveDocument:(id)sender {
  (void)sender;
  (void)[self saveNow];
  [self setNeedsDisplay:YES];
}

- (void)saveAsDocument:(id)sender {
  (void)sender;
  if ([self saveAsNow]) [self setNeedsDisplay:YES];
}

// Discard everything since the last save and reopen the file from disk. Only
// named documents have something to revert to; the user is warned first, since
// this throws away unsaved edits.
- (void)revertDocument:(id)sender {
  (void)sender;
  if (self.documentPath == nil) return;
  if (![self confirmDiscardChanges]) return;
  if (![self openPath:self.documentPath]) return;
  _maybeDirty = NO;
  [self updateTitle];
  [self setNeedsDisplay:YES];
}

// Save in place; an untitled document asks for a destination instead.
- (BOOL)saveNow {
  if (self.documentPath == nil) return [self saveAsNow];
  if (_ed->save(nullptr, false, nullptr) != Status::Ok) {
    [self showError:@"Could not save the file."];
    return NO;
  }
  _maybeDirty = NO;
  [self updateTitle];
  return YES;
}

// Save under a (possibly new) name. The core needs the destination's current
// identity to replace an existing file; NSSavePanel has already confirmed the
// overwrite, so read that identity and pass it as `expected_rev` (otherwise the
// core refuses to clobber an existing path with E_CONFLICT).
- (BOOL)saveAsNow {
  NSSavePanel* p = [NSSavePanel savePanel];
  p.canCreateDirectories = YES;
  p.nameFieldStringValue = self.documentPath.lastPathComponent != nil
                               ? self.documentPath.lastPathComponent
                               : @"Untitled.txt";
  if ([p runModal] != NSModalResponseOK || p.URL == nil) return NO;

  std::unique_ptr<FileSystem> fs;
  create_file_system(&fs);
  Rev rev{};
  const Rev* expected = nullptr;
  Stat st;
  if (fs != nullptr && fs->lstat(p.URL.path.UTF8String, &st) == Status::Ok) {
    rev = Rev{st.dev, st.ino, st.size, st.mtime_ns, st.ctime_ns};
    expected = &rev;
  }
  if (_ed->save_as(p.URL.path.UTF8String, expected, false, nullptr) !=
      Status::Ok) {
    [self showError:@"Could not save the file."];
    return NO;
  }
  [self discardUntitledTemp];
  self.documentPath = p.URL.path;
  _maybeDirty = NO;
  [self updateTitle];
  [self syncSidebarToDocument];  // the document may have moved folder
  return YES;
}

// The unsaved-changes prompt (§5.6-ish; the GUI owns it). Used on close and
// before opening another file.
- (BOOL)confirmDiscardChanges {
  if (!_maybeDirty) return YES;  // cheap: no core call on a clean document
  Info in;
  if (_ed->info(&in) != Status::Ok || !in.dirty) {
    _maybeDirty = NO;  // e.g. everything was undone
    [self updateTitle];
    return YES;
  }
  NSAlert* a = [[NSAlert alloc] init];
  a.messageText = @"Do you want to save the changes?";
  a.informativeText = @"Your changes will be lost if you don't save them.";
  [a addButtonWithTitle:@"Save"];
  [a addButtonWithTitle:@"Don't Save"];
  [a addButtonWithTitle:@"Cancel"];
  const NSModalResponse r = [a runModal];
  if (r == NSAlertFirstButtonReturn) return [self saveNow];
  if (r == NSAlertSecondButtonReturn) return YES;
  return NO;
}

- (void)showError:(NSString*)message {
  NSAlert* a = [[NSAlert alloc] init];
  a.messageText = message;
  const std::string err(_ed->errmsg());
  if (!err.empty()) {
    a.informativeText = [NSString stringWithUTF8String:err.c_str()];
  }
  [a addButtonWithTitle:@"OK"];
  [a runModal];
}

// Window title + dirty dot. Called after actions that can change the document,
// never per frame. The dot comes from our own edit tracking rather than
// `info()`, which would have to finish the index and the EOL scan (seconds on a
// huge file) just to flip a dot.
- (void)updateTitle {
  NSWindow* w = self.window;
  if (w == nil) return;
  w.title = self.documentPath != nil ? self.documentPath.lastPathComponent
                                     : @"Untitled";
  w.representedURL = self.documentPath != nil
                         ? [NSURL fileURLWithPath:self.documentPath]
                         : nil;
  w.documentEdited = _maybeDirty;
}

// Records a change to the document and refreshes the title/dot.
- (void)markEdited {
  _maybeDirty = YES;
  [self setNeedsDisplay:YES];
  [self updateTitle];
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
  if (_view->backspace()) [self markEdited];
}
- (void)paste:(id)sender {
  (void)sender;
  NSString* s = [[NSPasteboard generalPasteboard]
      stringForType:NSPasteboardTypeString];
  const char* u = s != nil ? s.UTF8String : nullptr;
  if (u != nullptr && _view->insert_text(u)) [self markEdited];
}
- (void)showFind:(id)sender {
  (void)sender;
  if (self.findBar == nil) return;
  self.findBar.hidden = !self.findBar.hidden;
  if (!self.findBar.hidden) {
    [self.findBar focusField];
  } else {
    [self.window makeFirstResponder:self];  // Escape's exit: the document again
  }
}

// Escape: close the find bar and give the document the keyboard back.
- (void)hideFind:(id)sender {
  (void)sender;
  if (self.findBar != nil) self.findBar.hidden = YES;
  [self.window makeFirstResponder:self];
  [self setNeedsDisplay:YES];
}

- (BOOL)validateMenuItem:(NSMenuItem*)item {
  if (item.action == @selector(toggleWrap:)) {
    item.state = _view->wrap() ? NSControlStateValueOn : NSControlStateValueOff;
  } else if (item.action == @selector(setThemeSystem:) ||
             item.action == @selector(setThemeLight:) ||
             item.action == @selector(setThemeDark:)) {
    NSString* pref =
        [[NSUserDefaults standardUserDefaults] stringForKey:@"Theme"];
    if (pref == nil) pref = @"system";
    NSString* want = item.action == @selector(setThemeLight:)
                         ? @"light"
                         : (item.action == @selector(setThemeDark:) ? @"dark"
                                                                    : @"system");
    item.state = [pref isEqualToString:want] ? NSControlStateValueOn
                                             : NSControlStateValueOff;
  } else if (item.action == @selector(revertDocument:)) {
    item.enabled = self.documentPath != nil;  // untitled has no saved version
  } else if (item.action == @selector(toggleSidebar:)) {
    item.state = (self.sidebar != nil && !self.sidebar.isHidden)
                     ? NSControlStateValueOn
                     : NSControlStateValueOff;
  }
  return YES;
}

// --- Find/replace ----------------------------------------------------------

// ⌘G / ⇧⌘G. Uses the bar's field when the find bar has a needle; otherwise the
// last search (the find pasteboard). A failed search beeps rather than silently
// doing nothing, as the system's Find does.
- (void)findNextMatch:(id)sender {
  NSString* needle = self.findBar != nil ? [self.findBar needle] : nil;
  if (needle == nil || needle.length == 0) {
    needle = [[NSPasteboard pasteboardWithName:NSPasteboardNameFind]
        stringForType:NSPasteboardTypeString];
  }
  if (needle == nil || needle.length == 0) return;
  [self findText:needle backward:NO];
}
- (void)findPreviousMatch:(id)sender {
  NSString* needle = self.findBar != nil ? [self.findBar needle] : nil;
  if (needle == nil || needle.length == 0) {
    needle = [[NSPasteboard pasteboardWithName:NSPasteboardNameFind]
        stringForType:NSPasteboardTypeString];
  }
  if (needle == nil || needle.length == 0) return;
  [self findText:needle backward:YES];
}
- (void)findText:(NSString*)needle backward:(BOOL)backward {
  if (needle == nil || needle.length == 0) return;
  // Remember it, so ⌘F and ⌘G pick up where the search left off.
  NSPasteboard* find = [NSPasteboard pasteboardWithName:NSPasteboardNameFind];
  [find clearContents];
  [find setString:needle forType:NSPasteboardTypeString];
  if (_view->find(needle.UTF8String, !backward, false)) {
    [self setNeedsDisplay:YES];
  } else {
    NSBeep();  // wrapped all the way round: nothing found
  }
}
- (void)replaceText:(NSString*)needle with:(NSString*)replacement {
  if (needle == nil || needle.length == 0) return;
  const char* rep = replacement != nil ? replacement.UTF8String : "";
  if (_view->replace_current(needle.UTF8String, rep, false)) [self markEdited];
}
- (void)replaceAllText:(NSString*)needle with:(NSString*)replacement {
  if (needle == nil || needle.length == 0) return;
  const char* rep = replacement != nil ? replacement.UTF8String : "";
  if (_view->replace_all(needle.UTF8String, rep, false) > 0) [self markEdited];
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
  _discovering = NO;  // a pending discovery step belongs to the old document
  _view->set_document(*_ed);
  _view->resize(self.bounds.size.width, self.bounds.size.height);
  _view->set_caret(0, 0);
  [self setNeedsDisplay:YES];
}
// Scrolls the top of the viewport to `line` (used by the `--scroll` snapshot).
- (void)scrollToLine:(int64_t)line {
  _view->scroll_to_line(line);
  [self setNeedsDisplay:YES];
}
// Pans horizontally (used by the `--xscroll` snapshot of the bottom bar).
- (void)scrollToX:(double)px {
  _view->scroll_x_by(px);
  [self setNeedsDisplay:YES];
}
- (vc::ViewCore*)core {
  return _view.get();
}
- (void)finishIndex {
  if (_ed->rev().size <= (64ull << 20)) {  // above the cap the bar stays approximate
    _view->discover_all();
    _view->apply_pending_scroll();
  }
  [self startBlink];  // the blink phase must not leak into a reference frame
}
// First paint stays lazy. Then index small/medium files in bounded steps (one
// run-loop turn each) so the exact line count and the scrollbar appear without
// ever blocking the window; huge files stay lazy (a full index of 5 GB is ~8 s).
- (void)beginDiscovery {
  const Rev r = _ed->rev();
  if (_discovering) return;
  // A scrollbar jump in a huge document must index toward its target, even
  // though the background drip deliberately skips such files.
  const bool pending = _view->has_pending_scroll();
  if (!pending && (r.size == 0 || r.size > (64ull << 20))) return;
  _discovering = YES;
  [self discoverStep];
}
- (void)discoverStep {
  if (!_discovering) return;
  const int64_t step = _view->has_pending_scroll() ? 32768 : 4096;
  const bool more = _view->discover_step(step);
  if (_view->apply_pending_scroll()) {  // the jump target is known: land it
    _discovering = NO;
    [self setNeedsDisplay:YES];
    return;
  }
  if (!more) {
    _discovering = NO;
    [self setNeedsDisplay:YES];  // complete: the scrollbar is exact now
    return;
  }
  [self setNeedsDisplay:YES];  // the count (and possibly the bar) grows
  [self performSelector:@selector(discoverStep) withObject:nil afterDelay:0.0];
}
// Renders the view exactly as the window does (through AppKit's context) to a
// PNG. Used by the `--snapshot` smoke test so the GUI can be inspected headless.
- (BOOL)writeSnapshotTo:(NSString*)path {
  return write_view_png(self, path);
}
- (void)setFrameSize:(NSSize)newSize {
  [super setFrameSize:newSize];
  _view->resize(newSize.width, newSize.height);
  [self setNeedsDisplay:YES];
}
- (void)drawRect:(NSRect)dirtyRect {
  (void)dirtyRect;
  // Keep viewcore's size in step with the view: auto layout may not call
  // setFrameSize:, which would leave the background under-covered.
  const NSSize size = self.bounds.size;
  _view->resize(size.width, size.height);
  CGContextRef ctx = [[NSGraphicsContext currentContext] CGContext];
  auto canvas = ed::mac::create_cg_canvas(ctx, size.width, size.height);
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
  if (ev.key == vc::Key::None) return;
  const bool edited = ev.key == vc::Key::InsertText ||
                      ev.key == vc::Key::Enter ||
                      ev.key == vc::Key::Backspace ||
                      ev.key == vc::Key::Delete;
  if (_view->key(ev)) {
    if (edited) {
      [self markEdited];
    } else {
      [self setNeedsDisplay:YES];
    }
  }
}

- (void)handleMouse:(NSEvent*)event kind:(vc::MouseEvent::Kind)kind {
  if (kind != vc::MouseEvent::Kind::Scroll) [self startBlink];
  // Scroll deltas do not depend on the pointer, and a synthetic event (tests)
  // has no window for convertPoint: — fall back to the view's own centre.
  NSPoint p = NSMakePoint(NSMidX(self.bounds), NSMidY(self.bounds));
  if (event.window != nil) {
    p = [self convertPoint:event.locationInWindow fromView:nil];
  }
  vc::MouseEvent m;
  m.kind = kind;
  m.x = p.x;
  m.y = self.bounds.size.height - p.y;  // AppKit y-up -> viewcore y-down
  m.shift = (event.modifierFlags & NSEventModifierFlagShift) != 0;
  m.cmd = (event.modifierFlags & NSEventModifierFlagCommand) != 0;
  // Double click selects a word, triple click a line (viewcore widens the drag).
  // clickCount is only defined for mouse events; asking a scroll-wheel event for
  // it raises an exception, so read it only where it means something.
  if (kind != vc::MouseEvent::Kind::Scroll) {
    m.clicks = static_cast<int>(event.clickCount);
  }
  if (kind == vc::MouseEvent::Kind::Scroll) {
    // Trackpads and recent mice report pixels (scroll in points); a classic
    // wheel reports lines, so scale it to the page. AppKit has already applied
    // the user's "natural scrolling" preference to these deltas.
    const double scale =
        event.hasPreciseScrollingDeltas ? 1.0 : 3.0 * _view->line_height();
    m.dy = -event.scrollingDeltaY * scale;  // + is down: later lines
    m.dx = -event.scrollingDeltaX * scale;  // + is right: later columns
  }
  if (_view->mouse(m)) [self setNeedsDisplay:YES];
  // A drag released on an approximate scrollbar leaves a target to index to.
  if (_view->has_pending_scroll() && !_discovering) [self beginDiscovery];
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

@interface AppDelegate : NSObject <NSApplicationDelegate, NSWindowDelegate>
@property(nonatomic, assign) Editor* editor;
@property(nonatomic, assign) EditorView* view;
@property(nonatomic, strong) NSWindow* aboutWindow;
@property(nonatomic, copy) NSString* snapshotPath;
@property(nonatomic, assign) int64_t snapshotScrollLine;
@property(nonatomic, assign) double snapshotScrollX;
// Test hook: "DY,DX" wheel notches (line units) fed through the app's own
// scroll path before the snapshot, so `--snapshot` can pin the result of a
// real scroll without needing a display.
@property(nonatomic, copy) NSString* wheelNotches;
- (void)postWheelNotchesY:(int)dy x:(int)dx;
- (void)showAbout:(id)sender;
@end

@implementation AppDelegate
- (void)applicationDidFinishLaunching:(NSNotification*)note {
  (void)note;
  [self.view updateTitle];
  [self.view performSelector:@selector(beginDiscovery)
                  withObject:nil
                  afterDelay:0.1];
  if (self.snapshotPath != nil) {
    // Let the first frame draw, then snapshot and exit (the `--snapshot` test).
    [NSTimer scheduledTimerWithTimeInterval:0.5
                                     target:self
                                   selector:@selector(runSnapshot:)
                                   userInfo:nil
                                    repeats:NO];
  }
}

// Test hook: scroll by whole wheel notches through the same viewcore entry
// point scrollWheel: uses. A classic wheel reports lines, so one notch is
// `kWheelLine` points; a trackpad reports points directly.
- (void)postWheelNotchesY:(int)dy x:(int)dx {
  static constexpr double kWheelLine = 48.9;  // 3 lines at a 16.3 pt line height
  vc::MouseEvent m;
  m.kind = vc::MouseEvent::Kind::Scroll;
  m.dy = static_cast<double>(dy) * kWheelLine;
  m.dx = static_cast<double>(dx) * kWheelLine;
  if ([self.view core]->mouse(m)) [self.view setNeedsDisplay:YES];
}

- (void)runSnapshot:(NSTimer*)timer {
  (void)timer;
  [self.view finishIndex];  // a steady frame: exact scrollbars, caret on
  if (self.snapshotScrollLine > 0) {
    [self.view scrollToLine:self.snapshotScrollLine];
  }
  if (self.snapshotScrollX > 0) {
    [self.view scrollToX:self.snapshotScrollX];
  }
  if (self.wheelNotches.length > 0) {
    NSArray<NSString*>* parts = [self.wheelNotches
        componentsSeparatedByString:@","];
    const int dy = parts.count > 0 ? parts[0].intValue : 0;
    const int dx = parts.count > 1 ? parts[1].intValue : 0;
    [self postWheelNotchesY:dy x:dx];
  }
  // The window's frame view, so the title bar (file name, dirty dot) is included.
  NSView* content = self.view.window.contentView;
  NSView* frameView = content != nil ? content.superview : nil;
  const BOOL ok = write_view_png(frameView != nil ? frameView : self.view,
                                 self.snapshotPath);
  std::fprintf(stderr, "[snapshot] %s -> %s\n",
               ok ? "wrote" : "FAILED", self.snapshotPath.UTF8String);
  [NSApp terminate:nil];
}
- (void)showAbout:(id)sender {
  (void)sender;
  if (self.aboutWindow == nil) {
    // A panel, not a window: it floats over the document without taking focus
    // or appearing in the Window menu, and Escape closes it.
    NSPanel* w = [[NSPanel alloc]
        initWithContentRect:NSMakeRect(0, 0, 420, 300)
                  styleMask:(NSWindowStyleMaskTitled | NSWindowStyleMaskClosable |
                             NSWindowStyleMaskUtilityWindow)
                    backing:NSBackingStoreBuffered
                      defer:NO];
    w.title = @"About editor";
    w.level = NSFloatingWindowLevel;
    w.hidesOnDeactivate = NO;
    w.becomesKeyOnlyIfNeeded = YES;
    w.collectionBehavior = NSWindowCollectionBehaviorCanJoinAllSpaces |
                           NSWindowCollectionBehaviorFullScreenAuxiliary;

    NSStackView* s = [[NSStackView alloc] init];
    s.orientation = NSUserInterfaceLayoutOrientationVertical;
    s.alignment = NSLayoutAttributeLeading;
    s.edgeInsets = NSEdgeInsetsMake(20, 20, 20, 20);
    s.spacing = 8;
    s.translatesAutoresizingMaskIntoConstraints = NO;
    AboutPanelView* content = [[AboutPanelView alloc]
        initWithFrame:NSMakeRect(0, 0, 420, 300)];
    w.contentView = content;
    [content addSubview:s];
    [NSLayoutConstraint activateConstraints:@[
      [s.leadingAnchor constraintEqualToAnchor:content.leadingAnchor],
      [s.trailingAnchor constraintEqualToAnchor:content.trailingAnchor],
      [s.topAnchor constraintEqualToAnchor:content.topAnchor],
      [s.bottomAnchor constraintLessThanOrEqualToAnchor:content.bottomAnchor],
    ]];

    NSTextField* name = [NSTextField labelWithString:@"editor"];
    name.font = [NSFont systemFontOfSize:20 weight:NSFontWeightBold];
    [s addArrangedSubview:name];
    [s addArrangedSubview:[NSTextField labelWithString:@"Version 0.1"]];

    NSTextField* licence = [NSTextField
        wrappingLabelWithString:
            @"Copyright © 2026 James de Klerk, trading as Build Useful. All "
            @"rights reserved.\n\nBuilt with the Build Useful design system, used "
            @"under its own licence. The Build Useful name and logo, and the "
            @"\u201Ceditor by Build Useful\u201D name, logo and app icons, "
            @"aren\u2019t licensed for other use."];
    licence.preferredMaxLayoutWidth = 380;
    [s addArrangedSubview:licence];

    NSTextField* third = [NSTextField labelWithString:@"Third-party software"];
    third.font = [NSFont systemFontOfSize:13 weight:NSFontWeightSemibold];
    [s addArrangedSubview:third];
    [s addArrangedSubview:[NSTextField
                            wrappingLabelWithString:@"None. editor uses only the "
                                                    @"macOS system frameworks."]];

    w.releasedWhenClosed = NO;
    self.aboutWindow = w;
  }
  [self.aboutWindow center];
  [self.aboutWindow makeKeyAndOrderFront:nil];
  [self.aboutWindow makeFirstResponder:self.aboutWindow.contentView];
}

// The document window became key: the user was in another app (Finder, say),
// so re-read the sidebar's open folders.
- (void)windowDidBecomeKey:(NSNotification*)note {
  (void)note;
  [self.view refreshSidebarFromDisk];
}

- (BOOL)application:(NSApplication*)sender openFile:(NSString*)filename {
  if (self.editor == nullptr || filename == nil) return NO;
  if (![self.view confirmDiscardChanges]) return NO;
  if (![self.view openPath:filename]) return NO;
  [sender activateIgnoringOtherApps:YES];
  return YES;
}

- (BOOL)applicationShouldTerminateAfterLastWindowClosed:(NSApplication*)sender {
  (void)sender;
  return YES;
}

- (void)applicationWillTerminate:(NSNotification*)note {
  (void)note;
  [self.view discardUntitledTemp];
}

// Prompt before discarding unsaved edits (§5.6-ish; the GUI owns this). Only the
// document window matters; the About panel closes freely.
- (BOOL)windowShouldClose:(NSWindow*)sender {
  if (sender == self.aboutWindow) return YES;
  return [self.view confirmDiscardChanges];
}

// Escape in the About panel closes it (the panel is otherwise key only for the
// panel itself).
- (void)windowWillClose:(NSNotification*)note {
  (void)note;
  if (note.object == self.aboutWindow) [self.view.window makeKeyAndOrderFront:nil];
}

- (NSApplicationTerminateReply)applicationShouldTerminate:(NSApplication*)sender {
  (void)sender;
  if (self.view.window != nil && ![self.view confirmDiscardChanges]) {
    return NSTerminateCancel;
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

void build_menu_bar(NSApplication* app, id aboutTarget) {
  NSMenu* menubar = [[NSMenu alloc] init];

  NSMenuItem* appItem = [[NSMenuItem alloc] init];
  [menubar addItem:appItem];
  NSMenuItem* about = item(@"About editor", @selector(showAbout:), @"");
  about.target = aboutTarget;
  NSMenuItem* themeItem = [[NSMenuItem alloc] initWithTitle:@"Theme"
                                                     action:nil
                                              keyEquivalent:@""];
  [themeItem setSubmenu:submenu(@"Theme", @[
    item(@"System", @selector(setThemeSystem:), @""),
    item(@"Light", @selector(setThemeLight:), @""),
    item(@"Dark", @selector(setThemeDark:), @""),
  ])];
  [appItem setSubmenu:submenu(@"editor", @[
    about,
    [NSMenuItem separatorItem],
    themeItem,
    [NSMenuItem separatorItem],
    item(@"Quit editor", @selector(terminate:), @"q"),
  ])];

  NSMenuItem* fileItem = [[NSMenuItem alloc] init];
  [menubar addItem:fileItem];
  NSMenuItem* saveAs = item(@"Save As\u2026", @selector(saveAsDocument:), @"s");
  saveAs.keyEquivalentModifierMask =
      NSEventModifierFlagCommand | NSEventModifierFlagShift;
  [fileItem setSubmenu:submenu(@"File", @[
    item(@"New", @selector(newDocument:), @"n"),
    item(@"Open\u2026", @selector(openDocument:), @"o"),
    [NSMenuItem separatorItem],
    item(@"Save", @selector(saveDocument:), @"s"),
    saveAs,
    item(@"Revert to Saved", @selector(revertDocument:), @""),
    [NSMenuItem separatorItem],
    item(@"Close", @selector(performClose:), @"w"),
  ])];

  NSMenuItem* editItem = [[NSMenuItem alloc] init];
  [menubar addItem:editItem];
  NSMenuItem* redo = item(@"Redo", @selector(redo:), @"z");
  redo.keyEquivalentModifierMask =
      NSEventModifierFlagCommand | NSEventModifierFlagShift;
  NSMenuItem* findPrev = item(@"Find Previous", @selector(findPreviousMatch:), @"g");
  findPrev.keyEquivalentModifierMask =
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
    item(@"Find Next", @selector(findNextMatch:), @"g"),
    findPrev,
  ])];

  NSMenuItem* viewItem = [[NSMenuItem alloc] init];
  [menubar addItem:viewItem];
  [viewItem setSubmenu:submenu(@"View", @[
    item(@"Show Sidebar", @selector(toggleSidebar:), @""),
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
    std::string path, snapshot, wheel_notches;
    int64_t scroll_line = 0;
    double scroll_x = 0;
    double win_w = 900, win_h = 700;
    for (int i = 1; i < argc; ++i) {
      const std::string a = argv[i];
      if (a == "--snapshot" && i + 1 < argc) {
        snapshot = argv[++i];
      } else if (a == "--theme" && i + 1 < argc) {
        [[NSUserDefaults standardUserDefaults]
            setObject:[NSString stringWithUTF8String:argv[++i]]
               forKey:@"Theme"];
      } else if (a == "--scroll" && i + 1 < argc) {
        scroll_line = std::strtoll(argv[++i], nullptr, 10);
      } else if (a == "--xscroll" && i + 1 < argc) {
        scroll_x = std::strtod(argv[++i], nullptr);
      } else if (a == "--wheel" && i + 1 < argc) {
        wheel_notches = argv[++i];
      } else if (a == "--size" && i + 1 < argc) {
        // WIDTHxHEIGHT in points, e.g. 480x320 (the smoke test's small window).
        const std::string s = argv[++i];
        const size_t x = s.find('x');
        if (x != std::string::npos) {
          win_w = std::strtod(s.substr(0, x).c_str(), nullptr);
          win_h = std::strtod(s.substr(x + 1).c_str(), nullptr);
        }
      } else if (!a.empty() && a[0] != '-') {
        path = a;
      }
    }
    if (win_w < 200) win_w = 200;
    if (win_h < 120) win_h = 120;

    std::unique_ptr<FileSystem> fs;
    create_file_system(&fs);
    std::unique_ptr<Editor> ed;
    create_editor(*fs, &ed);
    bool opened = false;
    if (!path.empty()) {
      const Status s = ed->open(path);
      if (s != Status::Ok) {
        std::fprintf(stderr, "editor: cannot open %s (status %d)\n", path.c_str(),
                     static_cast<int>(s));
      } else {
        opened = true;
      }
    }
    // A Finder / "Open With" launch starts with no argv file and delivers the
    // document as an Apple event a moment later. Start an untitled buffer so the
    // window always has a real document to draw and edit (the event then opens
    // the file over it). The buffer is backed by a private temp file that is
    // removed once the document gets a path of its own, or on quit.
    std::string untitled;
    if (!opened) {
      for (int attempt = 0; attempt < 3 && !opened; ++attempt) {
        untitled = untitled_path(attempt);
        if (ed->create(untitled) == Status::Ok) opened = true;
      }
      if (!opened) untitled.clear();
    }

    NSApplication* app = [NSApplication sharedApplication];
    [app setActivationPolicy:NSApplicationActivationPolicyRegular];

    const NSWindowStyleMask style = NSWindowStyleMaskTitled |
                                    NSWindowStyleMaskClosable |
                                    NSWindowStyleMaskResizable |
                                    NSWindowStyleMaskMiniaturizable;
    NSWindow* win = [[NSWindow alloc] initWithContentRect:NSMakeRect(0, 0, win_w, win_h)
                                                styleMask:style
                                                  backing:NSBackingStoreBuffered
                                                    defer:NO];
    [win setTitle:path.empty() || !opened
                      ? @"Untitled"
                      : [[NSString stringWithUTF8String:path.c_str()]
                            lastPathComponent]];

    EditorView* view = [[EditorView alloc] initWithEditor:ed.get()];
    if (opened && !path.empty()) {
      view.documentPath = [NSString stringWithUTF8String:path.c_str()];
    }
    if (!untitled.empty()) {
      view.untitledTempPath = [NSString stringWithUTF8String:untitled.c_str()];
    }
    FindBar* bar = [[FindBar alloc] initWithEditorView:view];
    StatusBar* status = [[StatusBar alloc] initWithFrame:NSMakeRect(0, 0, 900, 22)];
    view.findBar = bar;
    view.statusBar = status;
    bar.hidden = YES;
    // The file sidebar: a directory tree to the left of the document. Rooted at
    // the open file's folder (v1: one folder, not a workspace).
    SidebarView* sidebar = [[SidebarView alloc]
        initWithFrame:NSMakeRect(0, 0, vc::Sidebar::kDefaultWidth, 700)];
    sidebar.editorView = view;
    if (opened && !path.empty()) {
      const std::string parent = parent_dir(std::string(path.c_str()));
      if (!parent.empty()) {
        [sidebar setRootPath:[NSString stringWithUTF8String:parent.c_str()]];
        // Reveal the open file: expand its ancestors and select its row.
        [sidebar revealPath:[NSString stringWithUTF8String:path.c_str()]];
      }
    } else if (untitled.size() > 0) {
      const std::string parent = parent_dir(untitled);
      if (!parent.empty()) {
        [sidebar setRootPath:[NSString stringWithUTF8String:parent.c_str()]];
      }
    }
    // The saved preference (default: shown). A narrow window starts without it.
    const BOOL wantSidebar =
        [[NSUserDefaults standardUserDefaults] objectForKey:@"ShowSidebar"] == nil
            ? YES
            : [[NSUserDefaults standardUserDefaults] boolForKey:@"ShowSidebar"];
    sidebar.hidden = !wantSidebar;
    view.sidebar = sidebar;  // so the theme reaches it
    [view refreshTheme];  // give the chrome its page colour now the bars exist

    // The editor's own column: find bar, document, status bar.
    NSStackView* stack = [NSStackView stackViewWithViews:@[ bar, view, status ]];
    stack.orientation = NSUserInterfaceLayoutOrientationVertical;
    stack.distribution = NSStackViewDistributionFill;
    // Flush bands: the editor's bottom strip (the horizontal bar) sits right on
    // the status bar, with no stack spacing showing as a blank gap.
    stack.spacing = 0;
    stack.autoresizingMask = NSViewWidthSizable | NSViewHeightSizable;
    [bar setContentHuggingPriority:NSLayoutPriorityRequired
                    forOrientation:NSLayoutConstraintOrientationVertical];
    [status setContentHuggingPriority:NSLayoutPriorityRequired
                       forOrientation:NSLayoutConstraintOrientationVertical];
    [view setContentHuggingPriority:NSLayoutPriorityDefaultLow
                     forOrientation:NSLayoutConstraintOrientationVertical];

    // Sidebar | editor column, in one row. The sidebar is a fixed-width column
    // with a hairline; the editor takes the rest.
    NSStackView* root = [NSStackView stackViewWithViews:@[ sidebar, stack ]];
    root.orientation = NSUserInterfaceLayoutOrientationHorizontal;
    root.distribution = NSStackViewDistributionFill;
    root.spacing = 0;
    root.translatesAutoresizingMaskIntoConstraints = NO;
    [sidebar setContentHuggingPriority:NSLayoutPriorityRequired
                     forOrientation:NSLayoutConstraintOrientationHorizontal];
    [sidebar.widthAnchor constraintEqualToConstant:vc::Sidebar::kDefaultWidth]
        .active = YES;

    [win setContentView:root];
    [win makeFirstResponder:view];
    // Remember the window's frame between launches. Skipped for a snapshot:
    // there the requested size is authoritative (a restored frame would undo
    // `--size` and shift the goldens).
    if (snapshot.empty()) {
      [win setFrameAutosaveName:@"EditorWindow"];
      if (![win setFrameUsingName:@"EditorWindow"]) [win center];
    } else {
      [win center];
    }

    AppDelegate* delegate = [[AppDelegate alloc] init];
    delegate.editor = ed.get();
    delegate.view = view;
    delegate.snapshotScrollLine = scroll_line;
    delegate.snapshotScrollX = scroll_x;
    if (!wheel_notches.empty()) {
      delegate.wheelNotches =
          [NSString stringWithUTF8String:wheel_notches.c_str()];
    }
    if (!snapshot.empty()) {
      delegate.snapshotPath = [NSString stringWithUTF8String:snapshot.c_str()];
    }
    [app setDelegate:delegate];
    win.delegate = delegate;  // for the unsaved-changes prompt on close

    build_menu_bar(app, delegate);

    // Activate before ordering the window front: a window ordered front while
    // another app is active does not become key, and AppKit then routes key and
    // wheel events elsewhere (the editor appeared but would not scroll).
    [app activateIgnoringOtherApps:YES];
    [win makeKeyAndOrderFront:nil];
    [app run];
  }
  return 0;
}
