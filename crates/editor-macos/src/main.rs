//! Minimal macOS host binary (`editor-gui`) binding `editor-app` to the thin ObjC adapter.
//!
//! Also supports headless snapshot rendering via CoreText for deterministic visual inspection:
//!   editor-gui --snapshot out.png --theme light|dark --size 1200x800 [--sidebar-root DIR] [FILE]

use std::ffi::{CStr, CString};
use std::io;
use std::os::raw::{c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use editor_app::EditorApp;
use editor_platform::{
    AppCommand, Clipboard, ConfirmChoice, CursorShape, DirEntry, FileHandle, FileStat, FileSystem,
    FontMetrics, FontStyle, FsError, FsResult, GlyphFormat, GlyphRasterizer, HeadlessHost,
    InputEvent, Key, Modifiers, MouseButton, PlatformHost, RasterizedGlyph, StdFileSystem,
    SystemTheme, ThemePreference,
};

#[repr(C)]
struct MacFontMetrics {
    ascent: f32,
    descent: f32,
    leading: f32,
    line_height: u32,
    cell_width: f32,
}

#[repr(C)]
struct MacRasterGlyph {
    width: u32,
    height: u32,
    bearing_x: i32,
    bearing_y: i32,
    advance: f32,
    is_color: u32,
    pixels: *mut u8,
    pixels_len: usize,
}

#[repr(C)]
struct MacCallbacks {
    userdata: *mut c_void,
    on_resize: extern "C" fn(*mut c_void, u32, u32, f32),
    on_key: extern "C" fn(*mut c_void, u16, u32, *const u8, usize, *const u8, usize),
    on_mouse: extern "C" fn(*mut c_void, u32, f32, f32, u8, u8, u32),
    on_scroll: extern "C" fn(*mut c_void, f32, f32, f32, f32, u32),
    on_focus: extern "C" fn(*mut c_void, u32),
    on_theme: extern "C" fn(*mut c_void, u32),
    on_open_path: extern "C" fn(*mut c_void, *const c_char),
    on_command: extern "C" fn(*mut c_void, u32),
    on_tick: extern "C" fn(*mut c_void, u64),
    on_needs_render: extern "C" fn(*mut c_void, *mut u32, *mut u32, *mut f32) -> c_int,
    on_render_into: extern "C" fn(
        *mut c_void,
        *mut u32,
        u32,
        u32,
        usize,
        f32,
        c_int,
        i32,
        i32,
        i32,
        i32,
        *mut i32,
        *mut i32,
        *mut i32,
        *mut i32,
    ) -> c_int,
}

extern "C" {
    fn mac_clone_file(src: *const c_char, dst: *const c_char) -> c_int;
    fn mac_font_metrics(style: u32, size_px: f32, out: *mut MacFontMetrics);
    fn mac_rasterize_cluster(
        style: u32,
        size_px: f32,
        utf8: *const u8,
        utf8_len: usize,
        out: *mut MacRasterGlyph,
    );
    fn mac_free_glyph_pixels(ptr: *mut u8);
    fn mac_clipboard_get() -> *mut c_char;
    fn mac_clipboard_set(utf8: *const u8, len: usize);
    fn mac_free_cstr(ptr: *mut c_char);
    fn mac_pref_get(key: *const c_char) -> *mut c_char;
    fn mac_pref_set(key: *const c_char, val: *const c_char);
    fn mac_open_file_dialog(choose_dir: c_int) -> *mut c_char;
    fn mac_save_file_dialog(default_name: *const c_char) -> *mut c_char;
    fn mac_confirm_discard(file_name: *const c_char) -> c_int;
    fn mac_set_title(title: *const c_char);
    fn mac_set_cursor(cursor_id: u32);
    fn mac_request_redraw();
    fn mac_request_quit();
    fn mac_activate_app();
    fn mac_wake_main_loop();
    fn mac_run_app(cb: MacCallbacks);
}

// ============================================================================
// MacFileSystem (wraps portable StdFileSystem + adds O(1) APFS clonefile)
// ============================================================================

struct MacFileSystem {
    inner: StdFileSystem,
}

impl FileSystem for MacFileSystem {
    fn open_read(&self, path: &Path) -> FsResult<Arc<dyn FileHandle>> {
        self.inner.open_read(path)
    }
    fn stat(&self, path: &Path) -> FsResult<FileStat> {
        self.inner.stat(path)
    }
    fn lstat(&self, path: &Path) -> FsResult<FileStat> {
        self.inner.lstat(path)
    }
    fn canonicalize(&self, path: &Path) -> FsResult<PathBuf> {
        self.inner.canonicalize(path)
    }
    fn read_dir(&self, path: &Path) -> FsResult<Vec<DirEntry>> {
        self.inner.read_dir(path)
    }
    fn write_atomic(
        &self,
        path: &Path,
        write_fn: &mut dyn FnMut(&mut dyn io::Write) -> io::Result<()>,
    ) -> FsResult<FileStat> {
        self.inner.write_atomic(path, write_fn)
    }
    fn write_in_place(
        &self,
        path: &Path,
        ranges: &[(u64, &[u8])],
        new_len: Option<u64>,
    ) -> FsResult<FileStat> {
        self.inner.write_in_place(path, ranges, new_len)
    }
    fn clone_file(&self, src: &Path, dst: &Path) -> FsResult<()> {
        let s = CString::new(src.to_string_lossy().as_bytes())
            .map_err(|_| FsError::Io("invalid src path".into()))?;
        let d = CString::new(dst.to_string_lossy().as_bytes())
            .map_err(|_| FsError::Io("invalid dst path".into()))?;
        let rc = unsafe { mac_clone_file(s.as_ptr(), d.as_ptr()) };
        if rc == 0 {
            Ok(())
        } else {
            Err(FsError::Unsupported("APFS clonefile failed".into()))
        }
    }
    fn rename(&self, from: &Path, to: &Path) -> FsResult<()> {
        self.inner.rename(from, to)
    }
    fn remove_file(&self, path: &Path) -> FsResult<()> {
        self.inner.remove_file(path)
    }
    fn temp_dir(&self) -> PathBuf {
        self.inner.temp_dir()
    }
    fn lock_file(&self, path: &Path) -> FsResult<editor_platform::FileLock> {
        self.inner.lock_file(path)
    }
}

// ============================================================================
// MacGlyphRasterizer (CoreText, called ONLY on atlas misses)
// ============================================================================

struct MacGlyphRasterizer;

fn style_id(s: FontStyle) -> u32 {
    match s {
        FontStyle::Mono => 0,
        FontStyle::MonoBold => 1,
        FontStyle::MonoItalic => 2,
        FontStyle::Ui => 3,
        FontStyle::UiBold => 4,
    }
}

impl GlyphRasterizer for MacGlyphRasterizer {
    fn metrics(&mut self, style: FontStyle, size_px: f32) -> FontMetrics {
        let mut out = MacFontMetrics {
            ascent: 12.0,
            descent: 3.0,
            leading: 3.0,
            line_height: 18,
            cell_width: 8.0,
        };
        unsafe { mac_font_metrics(style_id(style), size_px, &mut out) };
        FontMetrics {
            ascent: out.ascent,
            descent: out.descent,
            leading: out.leading,
            line_height: out.line_height,
            cell_width: out.cell_width,
        }
    }

    fn rasterize(&mut self, style: FontStyle, size_px: f32, cluster: &str) -> RasterizedGlyph {
        let mut out = MacRasterGlyph {
            width: 0,
            height: 0,
            bearing_x: 0,
            bearing_y: 0,
            advance: 0.0,
            is_color: 0,
            pixels: std::ptr::null_mut(),
            pixels_len: 0,
        };
        unsafe {
            mac_rasterize_cluster(
                style_id(style),
                size_px,
                cluster.as_ptr(),
                cluster.len(),
                &mut out,
            );
        }
        let pixels = if !out.pixels.is_null() && out.pixels_len > 0 {
            let slice = unsafe { std::slice::from_raw_parts(out.pixels, out.pixels_len) };
            let vec = slice.to_vec();
            unsafe { mac_free_glyph_pixels(out.pixels) };
            vec
        } else {
            Vec::new()
        };
        RasterizedGlyph {
            width: out.width,
            height: out.height,
            bearing_x: out.bearing_x,
            bearing_y: out.bearing_y,
            advance: out.advance,
            format: if out.is_color != 0 {
                GlyphFormat::Bgra32
            } else {
                GlyphFormat::Alpha8
            },
            pixels,
        }
    }
}

// ============================================================================
// MacClipboard & MacHost
// ============================================================================

struct MacClipboard;

impl Clipboard for MacClipboard {
    fn get_text(&self) -> Option<String> {
        let ptr = unsafe { mac_clipboard_get() };
        if ptr.is_null() {
            return None;
        }
        let s = unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned();
        unsafe { mac_free_cstr(ptr) };
        Some(s)
    }

    fn set_text(&mut self, text: &str) {
        unsafe { mac_clipboard_set(text.as_ptr(), text.len()) };
    }
}

struct MacHost;

impl PlatformHost for MacHost {
    fn set_title(&mut self, title: &str) {
        if let Ok(cs) = CString::new(title) {
            unsafe { mac_set_title(cs.as_ptr()) };
        }
    }

    fn set_cursor(&mut self, cursor: CursorShape) {
        let id = match cursor {
            CursorShape::Arrow => 0,
            CursorShape::IBeam => 1,
            CursorShape::Pointer => 2,
            CursorShape::ResizeHorizontal => 3,
        };
        unsafe { mac_set_cursor(id) };
    }

    fn open_file_dialog(&mut self) -> Option<PathBuf> {
        let ptr = unsafe { mac_open_file_dialog(0) };
        if ptr.is_null() {
            return None;
        }
        let s = unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned();
        unsafe { mac_free_cstr(ptr) };
        Some(PathBuf::from(s))
    }

    fn open_folder_dialog(&mut self) -> Option<PathBuf> {
        let ptr = unsafe { mac_open_file_dialog(1) };
        if ptr.is_null() {
            return None;
        }
        let s = unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned();
        unsafe { mac_free_cstr(ptr) };
        Some(PathBuf::from(s))
    }

    fn save_file_dialog(&mut self, default_name: Option<&str>) -> Option<PathBuf> {
        let cs = default_name.and_then(|n| CString::new(n).ok());
        let ptr = unsafe {
            mac_save_file_dialog(cs.as_ref().map(|c| c.as_ptr()).unwrap_or(std::ptr::null()))
        };
        if ptr.is_null() {
            return None;
        }
        let s = unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned();
        unsafe { mac_free_cstr(ptr) };
        Some(PathBuf::from(s))
    }

    fn confirm_discard(&mut self, file_name: &str) -> ConfirmChoice {
        let cs = CString::new(file_name).unwrap_or_else(|_| CString::new("Untitled").unwrap());
        match unsafe { mac_confirm_discard(cs.as_ptr()) } {
            0 => ConfirmChoice::Save,
            1 => ConfirmChoice::Discard,
            _ => ConfirmChoice::Cancel,
        }
    }

    fn request_redraw(&mut self) {
        unsafe { mac_request_redraw() };
    }

    fn request_quit(&mut self) {
        unsafe { mac_request_quit() };
    }

    fn load_pref(&self, key: &str) -> Option<String> {
        let k = CString::new(key).ok()?;
        let ptr = unsafe { mac_pref_get(k.as_ptr()) };
        if ptr.is_null() {
            return None;
        }
        let s = unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned();
        unsafe { mac_free_cstr(ptr) };
        Some(s)
    }

    fn save_pref(&mut self, key: &str, value: &str) {
        if let (Ok(k), Ok(v)) = (CString::new(key), CString::new(value)) {
            unsafe { mac_pref_set(k.as_ptr(), v.as_ptr()) };
        }
    }
}

// ============================================================================
// Event Bridge & Keycode Decoding
// ============================================================================

struct Bridge {
    app: EditorApp,
    rasterizer: MacGlyphRasterizer,
    clipboard: MacClipboard,
    host: MacHost,
    ipc: Option<editor_platform::ipc::IpcServer>,
}

fn decode_mods(bits: u32) -> Modifiers {
    Modifiers {
        shift: bits & 1 != 0,
        ctrl: bits & 2 != 0,
        alt: bits & 4 != 0,
        meta: bits & 8 != 0,
    }
}

fn decode_key(keycode: u16, text: &str) -> Option<Key> {
    match keycode {
        36 | 76 => Some(Key::Enter),
        48 => Some(Key::Tab),
        51 => Some(Key::Backspace),
        117 => Some(Key::Delete),
        53 => Some(Key::Escape),
        123 => Some(Key::Left),
        124 => Some(Key::Right),
        126 => Some(Key::Up),
        125 => Some(Key::Down),
        115 => Some(Key::Home),
        119 => Some(Key::End),
        116 => Some(Key::PageUp),
        121 => Some(Key::PageDown),
        _ => text.chars().next().map(Key::Char),
    }
}

fn decode_menu_cmd(tag: u32) -> Option<AppCommand> {
    match tag {
        0 => Some(AppCommand::NewFile),
        1 => Some(AppCommand::OpenFile),
        2 => Some(AppCommand::OpenFolder),
        3 => Some(AppCommand::Save),
        4 => Some(AppCommand::SaveAs),
        5 => Some(AppCommand::Reload),
        6 => Some(AppCommand::Undo),
        7 => Some(AppCommand::Redo),
        8 => Some(AppCommand::Cut),
        9 => Some(AppCommand::Copy),
        10 => Some(AppCommand::Paste),
        11 => Some(AppCommand::SelectAll),
        12 => Some(AppCommand::Find),
        13 => Some(AppCommand::Replace),
        14 => Some(AppCommand::GoToLine),
        15 => Some(AppCommand::ToggleWrap),
        16 => Some(AppCommand::ToggleSidebar),
        17 => Some(AppCommand::ZoomIn),
        18 => Some(AppCommand::ZoomOut),
        19 => Some(AppCommand::ZoomReset),
        21 => Some(AppCommand::Quit),
        22 => Some(AppCommand::QuickOpen),
        23 => Some(AppCommand::ProjectSearch),
        24 => Some(AppCommand::SplitPane),
        25 => Some(AppCommand::CloseSplit),
        26 => Some(AppCommand::ToggleVimMode),
        27 => Some(AppCommand::ShowShortcuts),
        100 => Some(AppCommand::SetTheme(ThemePreference::System)),
        101 => Some(AppCommand::SetTheme(ThemePreference::Light)),
        102 => Some(AppCommand::SetTheme(ThemePreference::Dark)),
        _ => None,
    }
}

extern "C" fn cb_resize(ud: *mut c_void, w: u32, h: u32, scale: f32) {
    let b = unsafe { &mut *(ud as *mut Bridge) };
    b.app.handle_event(
        InputEvent::Resize {
            width_px: w,
            height_px: h,
            scale,
        },
        &mut b.rasterizer,
        &mut b.clipboard,
        &mut b.host,
    );
}

extern "C" fn cb_key(
    ud: *mut c_void,
    keycode: u16,
    mods: u32,
    key_ptr: *const u8,
    key_len: usize,
    text_ptr: *const u8,
    text_len: usize,
) {
    let b = unsafe { &mut *(ud as *mut Bridge) };
    let key_str = if !key_ptr.is_null() && key_len > 0 {
        let slice = unsafe { std::slice::from_raw_parts(key_ptr, key_len) };
        std::str::from_utf8(slice).unwrap_or("")
    } else {
        ""
    };
    let text_str = if !text_ptr.is_null() && text_len > 0 {
        let slice = unsafe { std::slice::from_raw_parts(text_ptr, text_len) };
        std::str::from_utf8(slice).unwrap_or("")
    } else {
        ""
    };
    let Some(key) = decode_key(keycode, if key_str.is_empty() { text_str } else { key_str }) else {
        return;
    };
    let text_opt = if text_str.is_empty() {
        None
    } else {
        Some(text_str.to_string())
    };
    b.app.handle_event(
        InputEvent::KeyDown {
            key,
            mods: decode_mods(mods),
            text: text_opt,
        },
        &mut b.rasterizer,
        &mut b.clipboard,
        &mut b.host,
    );
}

extern "C" fn cb_mouse(
    ud: *mut c_void,
    kind: u32,
    x: f32,
    y: f32,
    button: u8,
    clicks: u8,
    mods: u32,
) {
    let b = unsafe { &mut *(ud as *mut Bridge) };
    let btn = match button {
        1 => MouseButton::Right,
        2 => MouseButton::Middle,
        _ => MouseButton::Left,
    };
    let ev = match kind {
        0 => InputEvent::MouseDown {
            x_px: x,
            y_px: y,
            button: btn,
            click_count: clicks,
            mods: decode_mods(mods),
        },
        1 => InputEvent::MouseMove {
            x_px: x,
            y_px: y,
            mods: decode_mods(mods),
        },
        _ => InputEvent::MouseUp {
            x_px: x,
            y_px: y,
            button: btn,
            mods: decode_mods(mods),
        },
    };
    b.app
        .handle_event(ev, &mut b.rasterizer, &mut b.clipboard, &mut b.host);
}

extern "C" fn cb_scroll(ud: *mut c_void, dx: f32, dy: f32, x: f32, y: f32, mods: u32) {
    let b = unsafe { &mut *(ud as *mut Bridge) };
    b.app.handle_event(
        InputEvent::Scroll {
            dx_px: dx,
            dy_px: dy,
            x_px: x,
            y_px: y,
            mods: decode_mods(mods),
        },
        &mut b.rasterizer,
        &mut b.clipboard,
        &mut b.host,
    );
}

extern "C" fn cb_focus(ud: *mut c_void, focused: u32) {
    let b = unsafe { &mut *(ud as *mut Bridge) };
    b.app.handle_event(
        InputEvent::FocusChanged(focused != 0),
        &mut b.rasterizer,
        &mut b.clipboard,
        &mut b.host,
    );
}

extern "C" fn cb_theme(ud: *mut c_void, is_dark: u32) {
    let b = unsafe { &mut *(ud as *mut Bridge) };
    let st = if is_dark != 0 {
        SystemTheme::Dark
    } else {
        SystemTheme::Light
    };
    b.app.handle_event(
        InputEvent::SystemThemeChanged(st),
        &mut b.rasterizer,
        &mut b.clipboard,
        &mut b.host,
    );
}

extern "C" fn cb_open_path(ud: *mut c_void, path_ptr: *const c_char) {
    if path_ptr.is_null() {
        return;
    }
    let b = unsafe { &mut *(ud as *mut Bridge) };
    let p = PathBuf::from(unsafe { CStr::from_ptr(path_ptr) }.to_string_lossy().into_owned());
    b.app.handle_event(
        InputEvent::OpenPath(p),
        &mut b.rasterizer,
        &mut b.clipboard,
        &mut b.host,
    );
}

extern "C" fn cb_command(ud: *mut c_void, cmd_id: u32) {
    let b = unsafe { &mut *(ud as *mut Bridge) };
    if let Some(cmd) = decode_menu_cmd(cmd_id) {
        b.app.handle_event(
            InputEvent::Command(cmd),
            &mut b.rasterizer,
            &mut b.clipboard,
            &mut b.host,
        );
    }
}

extern "C" fn cb_tick(ud: *mut c_void, now_ms: u64) {
    let b = unsafe { &mut *(ud as *mut Bridge) };
    // Drain any incoming CLI/agent IPC commands (`status`, `open`, `goto`, `select`,
    // `insert`, `find`, `save`, `undo`, `redo`, `action`, `close`/`quit`).
    while let Some(pending) = b.ipc.as_ref().and_then(|s| s.try_recv()) {
        let (reply, activate, should_quit) = b.app.execute_ipc(
            &pending.cmd,
            &mut b.rasterizer,
            &mut b.clipboard,
            &mut b.host,
        );
        if activate {
            unsafe { mac_activate_app() };
        }
        pending.respond(reply);
        if should_quit {
            b.ipc = None;
            b.host.request_quit();
            return;
        }
    }
    b.app.handle_event(
        InputEvent::Tick { now_ms },
        &mut b.rasterizer,
        &mut b.clipboard,
        &mut b.host,
    );
}

extern "C" fn cb_needs_render(
    ud: *mut c_void,
    out_w: *mut u32,
    out_h: *mut u32,
    out_scale: *mut f32,
) -> c_int {
    let b = unsafe { &mut *(ud as *mut Bridge) };
    if !b.app.is_dirty_frame() {
        return 0;
    }
    unsafe {
        *out_w = b.app.framebuffer.width;
        *out_h = b.app.framebuffer.height;
        *out_scale = b.app.framebuffer.scale;
    }
    1
}

extern "C" fn cb_render_into(
    ud: *mut c_void,
    dst_pixels: *mut u32,
    w: u32,
    h: u32,
    stride_u32: usize,
    scale: f32,
    force_full: c_int,
    extra_x: i32,
    extra_y: i32,
    extra_w: i32,
    extra_h: i32,
    out_x: *mut i32,
    out_y: *mut i32,
    out_w: *mut i32,
    out_h: *mut i32,
) -> c_int {
    let b = unsafe { &mut *(ud as *mut Bridge) };
    if dst_pixels.is_null() || w == 0 || h == 0 || stride_u32 < w as usize {
        return 0;
    }
    let slice = unsafe { std::slice::from_raw_parts_mut(dst_pixels, stride_u32 * (h as usize)) };
    let surf = editor_platform::SurfaceMut {
        width: w,
        height: h,
        stride: stride_u32,
        scale,
        pixels: slice,
    };
    let extra_dirty = editor_platform::PixelRect::new(extra_x, extra_y, extra_w, extra_h);
    if let Some(painted) =
        b.app
            .render_into_with_damage(surf, &mut b.rasterizer, extra_dirty, force_full != 0)
    {
        unsafe {
            if !out_x.is_null() {
                *out_x = painted.x;
            }
            if !out_y.is_null() {
                *out_y = painted.y;
            }
            if !out_w.is_null() {
                *out_w = painted.w;
            }
            if !out_h.is_null() {
                *out_h = painted.h;
            }
        }
        1
    } else {
        0
    }
}

// ============================================================================
// Entrypoint (Live Window OR Headless CoreText PNG Snapshot)
// ============================================================================

fn parse_size(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut snapshot_path: Option<PathBuf> = None;
    let mut theme_override: Option<ThemePreference> = None;
    let mut size: (u32, u32) = (1120, 740);
    let mut scale: f32 = 2.0;
    let mut sidebar_root: Option<PathBuf> = None;
    let mut wrap = false;
    let mut open_find = false;
    let mut open_quick = false;
    let mut open_project_search: Option<String> = None;
    let mut split_file: Option<PathBuf> = None;
    let mut open_shortcuts = false;
    let mut open_shortcuts_all = false;
    let mut enable_vim = false;
    let mut positional: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--snapshot" if i + 1 < args.len() => {
                snapshot_path = Some(PathBuf::from(&args[i + 1]));
                i += 2;
            }
            "--theme" if i + 1 < args.len() => {
                theme_override = match args[i + 1].as_str() {
                    "light" => Some(ThemePreference::Light),
                    "dark" => Some(ThemePreference::Dark),
                    _ => Some(ThemePreference::System),
                };
                i += 2;
            }
            "--size" if i + 1 < args.len() => {
                if let Some(sz) = parse_size(&args[i + 1]) {
                    size = sz;
                }
                i += 2;
            }
            "--scale" if i + 1 < args.len() => {
                if let Ok(sc) = args[i + 1].parse::<f32>() {
                    scale = sc.clamp(1.0, 3.0);
                }
                i += 2;
            }
            "--sidebar-root" if i + 1 < args.len() => {
                sidebar_root = Some(PathBuf::from(&args[i + 1]));
                i += 2;
            }
            "--wrap" => {
                wrap = true;
                i += 1;
            }
            "--find" => {
                open_find = true;
                i += 1;
            }
            "--quick-open" => {
                open_quick = true;
                i += 1;
            }
            "--project-search" if i + 1 < args.len() => {
                open_project_search = Some(args[i + 1].clone());
                i += 2;
            }
            "--split" if i + 1 < args.len() => {
                split_file = Some(PathBuf::from(&args[i + 1]));
                i += 2;
            }
            "--shortcuts" => {
                open_shortcuts = true;
                i += 1;
            }
            "--shortcuts-all" => {
                open_shortcuts = true;
                open_shortcuts_all = true;
                i += 1;
            }
            "--vim" => {
                enable_vim = true;
                i += 1;
            }
            arg if !arg.starts_with('-') => {
                positional = Some(PathBuf::from(arg));
                i += 1;
            }
            _ => i += 1,
        }
    }

    let fs: Arc<dyn FileSystem> = Arc::new(MacFileSystem {
        inner: StdFileSystem,
    });
    let fs_clone = Arc::clone(&fs);

    // Headless snapshot mode (renders with REAL CoreText + Apple Color Emoji directly to PNG!)
    if let Some(out_png) = snapshot_path {
        let mut host = HeadlessHost::new();
        let mut rasterizer = MacGlyphRasterizer;
        let w_px = (size.0 as f32 * scale).round() as u32;
        let h_px = (size.1 as f32 * scale).round() as u32;
        let mut app = EditorApp::new(fs, w_px, h_px, scale);
        if let Some(pref) = theme_override {
            app.set_theme_preference(pref);
        }
        if wrap {
            app.viewport.word_wrap = true;
        }
        if let Some(dir) = sidebar_root {
            app.set_sidebar_root(&dir);
        }
        if let Some(p) = positional {
            if let Err(e) = app.open_path_or_dir(&p, &mut host) {
                eprintln!("warning: could not open {}: {e}", p.display());
            }
        }
        if let Some(sp) = split_file {
            app.split_pane_with(Some(&sp), &mut host);
        }
        if open_find {
            app.find_bar.open(true, Some("fn"));
            app.refresh_search();
        }
        if open_quick {
            let root = app.sidebar.root().map(Path::to_path_buf);
            app.palette.open_quick_open(fs_clone.as_ref(), root.as_deref());
        } else if let Some(ref q) = open_project_search {
            let root = app.sidebar.root().map(Path::to_path_buf);
            app.palette
                .open_project_search(fs_clone.as_ref(), root.as_deref(), Some(q));
        }
        if enable_vim {
            app.vim.set_enabled(true);
        }
        if open_shortcuts {
            app.shortcuts.open(app.keymap.to_display_items());
            if open_shortcuts_all {
                app.shortcuts.toggle_expanded();
            }
        }
        app.mark_dirty();
        app.render(&mut rasterizer);
        if let Err(e) = app.framebuffer.save_png(&out_png) {
            eprintln!("failed to save snapshot {}: {e}", out_png.display());
            std::process::exit(1);
        }
        return;
    }

    // Bind per-user 0600 local IPC socket so `editor open` / `editor close` / `editor quit` work.
    let ipc = editor_platform::ipc::IpcServer::bind(
        editor_platform::ipc::default_socket_path(),
        Arc::new(|| unsafe { mac_wake_main_loop() }),
    )
    .ok();

    // Live native macOS GUI launch (0-byte virtual CPU framebuffer: paints directly into IOSurface!).
    let mut bridge = Box::new(Bridge {
        app: EditorApp::new_zero_copy(fs, size.0 * 2, size.1 * 2, 2.0),
        rasterizer: MacGlyphRasterizer,
        clipboard: MacClipboard,
        host: MacHost,
        ipc,
    });
    // Use our 100% custom-drawn Build Useful confirmation modal instead of native NSAlert.
    bridge.app.use_in_app_modal = true;
    bridge.app.load_preferences(&bridge.host);
    if let Some(pref) = theme_override {
        bridge.app.set_theme_preference(pref);
    }
    if wrap {
        bridge.app.viewport.word_wrap = true;
    }
    if let Some(dir) = sidebar_root {
        bridge.app.set_sidebar_root(&dir);
    }
    if let Some(p) = positional {
        let _ = bridge.app.open_path_or_dir(&p, &mut bridge.host);
    }

    let ud = (&mut *bridge as *mut Bridge) as *mut c_void;
    let cbs = MacCallbacks {
        userdata: ud,
        on_resize: cb_resize,
        on_key: cb_key,
        on_mouse: cb_mouse,
        on_scroll: cb_scroll,
        on_focus: cb_focus,
        on_theme: cb_theme,
        on_open_path: cb_open_path,
        on_command: cb_command,
        on_tick: cb_tick,
        on_needs_render: cb_needs_render,
        on_render_into: cb_render_into,
    };

    unsafe { mac_run_app(cbs) };
}
