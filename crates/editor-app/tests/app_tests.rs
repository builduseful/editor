use std::path::PathBuf;
use std::sync::Arc;

use editor_app::EditorApp;
use editor_platform::{
    AppCommand, BuiltinBitmapRasterizer, Clipboard, ConfirmChoice, HeadlessHost, InputEvent, Key,
    MemoryClipboard, MemoryFileSystem, Modifiers, MouseButton, ThemePreference,
};

#[test]
fn full_headless_editing_find_replace_save_and_render_workflow() {
    let fs = Arc::new(MemoryFileSystem::new());
    fs.add_file(
        "/work/src/main.rs",
        "fn main() {\n    let msg = \"hello 🚀\";\n    println!(\"{msg}\");\n}\n"
            .as_bytes()
            .to_vec(),
    );
    fs.add_file("/work/src/other.rs", b"pub fn other() {}\n".to_vec());

    let mut app = EditorApp::new(fs.clone(), 1200, 800, 2.0);
    let mut raster = BuiltinBitmapRasterizer;
    let mut clip = MemoryClipboard::new();
    let mut host = HeadlessHost::new();

    app.open_file(&PathBuf::from("/work/src/main.rs"), &mut host)
        .unwrap();
    assert_eq!(app.doc.file_name(), "main.rs");
    assert!(app.render(&mut raster));
    assert!(!app.render(&mut raster), "idle frame should skip redraw!");

    // Move right and insert text + emoji, then undo & redo.
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Char('/'),
            mods: Modifiers::NONE,
            text: Some("// top 🚀\n".into()),
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.doc.is_dirty());
    assert_eq!(app.doc.line_content(0).unwrap(), "// top 🚀");

    app.handle_event(
        InputEvent::Command(AppCommand::Undo),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.doc.line_content(0).unwrap(), "fn main() {");

    app.handle_event(
        InputEvent::Command(AppCommand::Redo),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.doc.line_content(0).unwrap(), "// top 🚀");

    // Find & Replace via keyboard commands.
    app.handle_event(
        InputEvent::Command(AppCommand::Replace),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.find_bar.visible);
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Char('h'),
            mods: Modifiers::NONE,
            text: Some("hello".into()),
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.search_matches.len(), 1);
    app.find_bar.replace_input.set_text("greetings");
    // Render with find bar visible so button hit rects are laid out, then click "Replace All".
    assert!(app.render(&mut raster));
    let ra = app.find_bar.layout.btn_replace_all;
    app.handle_event(
        InputEvent::MouseDown {
            x_px: (ra.x + ra.w / 2) as f32,
            y_px: (ra.y + ra.h / 2) as f32,
            button: MouseButton::Left,
            click_count: 1,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.doc.to_string_lossy().contains("greetings 🚀"));

    // Save persists back to MemoryFileSystem and clears dirty flag.
    app.handle_event(
        InputEvent::Command(AppCommand::Save),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(!app.doc.is_dirty());
    let disk = String::from_utf8(fs.read_bytes(&PathBuf::from("/work/src/main.rs")).unwrap()).unwrap();
    assert!(disk.contains("greetings 🚀"));

    // Theme switching changes rendered pixels cleanly (Light vs Dark).
    app.set_theme_preference(ThemePreference::Light);
    app.render(&mut raster);
    let light_bg = app.framebuffer.pixel_at(900, 400);

    app.set_theme_preference(ThemePreference::Dark);
    app.render(&mut raster);
    let dark_bg = app.framebuffer.pixel_at(900, 400);
    assert_ne!(light_bg, dark_bg);

    // Close Find bar with Escape so focus returns to document, then test unsaved-changes guard.
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Escape,
            mods: Modifiers::NONE,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(!app.find_bar.visible);

    // Opening another file parks dirty `main.rs` safely without interrupting,
    // and closing a dirty tab guards with the unsaved-changes confirmation!
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Char('x'),
            mods: Modifiers::NONE,
            text: Some("x".into()),
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.doc.is_dirty());
    app.handle_event(
        InputEvent::OpenPath(PathBuf::from("/work/src/other.rs")),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.doc.file_name(), "other.rs");
    // Flick back to `main.rs` — dirty edits are still safely preserved in memory!
    app.handle_event(
        InputEvent::Command(AppCommand::ToggleLastBuffer),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.doc.file_name(), "main.rs");
    assert!(app.doc.is_dirty());
    host.next_confirm = Some(ConfirmChoice::Cancel);
    app.handle_event(
        InputEvent::Command(AppCommand::CloseTab),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.doc.file_name(), "main.rs", "Cancel must keep dirty tab open!");

    // Accessibility snapshot exposes all key regions.
    let a11y = app.a11y_snapshot();
    assert!(a11y.len() >= 4);
}

#[test]
fn pro_line_ops_menu_routing_confirm_modal_and_caret_blink_partial_redraw() {
    let fs = Arc::new(MemoryFileSystem::new());
    fs.add_file("/work/code.rs", b"let a = 1;\nlet b = 2;\nlet c = 3;\n".to_vec());
    fs.add_file("/work/next.rs", b"// next\n".to_vec());

    let mut app = EditorApp::new(fs.clone(), 1000, 600, 1.0);
    app.use_in_app_modal = true;
    let mut raster = BuiltinBitmapRasterizer;
    let mut clip = MemoryClipboard::new();
    let mut host = HeadlessHost::new();

    app.open_file(&PathBuf::from("/work/code.rs"), &mut host).unwrap();
    app.render(&mut raster);

    let primary = Modifiers {
        #[cfg(target_os = "macos")]
        meta: true,
        #[cfg(not(target_os = "macos"))]
        ctrl: true,
        ..Modifiers::NONE
    };
    let primary_shift = Modifiers {
        shift: true,
        ..primary
    };

    // 1. Toggle comment (Cmd+/)
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Char('/'),
            mods: primary,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.doc.line_content(0).unwrap(), "// let a = 1;");
    // Toggle comment off again!
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Char('/'),
            mods: primary,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.doc.line_content(0).unwrap(), "let a = 1;");

    // 2. Duplicate line (Cmd+Shift+D)
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Char('d'),
            mods: primary_shift,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.doc.line_content(0).unwrap(), "let a = 1;");
    assert_eq!(app.doc.line_content(1).unwrap(), "let a = 1;");

    // 3. Delete line (Cmd+Shift+K)
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Char('k'),
            mods: primary_shift,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.doc.line_content(1).unwrap(), "let b = 2;");

    // 4. Move line down (Option+Down)
    app.caret = 0;
    app.anchor = 0;
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Down,
            mods: Modifiers { alt: true, ..Modifiers::NONE },
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.doc.line_content(0).unwrap(), "let b = 2;");
    assert_eq!(app.doc.line_content(1).unwrap(), "let a = 1;");

    // 5. Menu Paste routes to Find input when Find bar is focused!
    clip.set_text("needle_from_menu");
    app.handle_event(
        InputEvent::Command(AppCommand::Find),
        &mut raster,
        &mut clip,
        &mut host,
    );
    app.handle_event(
        InputEvent::Command(AppCommand::Paste),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.find_bar.find_input.text, "needle_from_menu");
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Escape,
            mods: Modifiers::NONE,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );

    // 6. Caret blink triggers partial dirty-rect (NOT full redraw!).
    app.render(&mut raster);
    app.handle_event(InputEvent::Tick { now_ms: 600 }, &mut raster, &mut clip, &mut host);
    assert!(!app.framebuffer.dirty.is_full(), "caret blink should use partial dirty rect!");
    assert!(app.render(&mut raster));

    // 7. In-app confirmation modal opens on closing a dirty tab and discards when pressing 'd'.
    assert!(app.doc.is_dirty());
    app.open_file(&PathBuf::from("/work/next.rs"), &mut host).unwrap();
    assert_eq!(app.doc.file_name(), "next.rs");
    app.handle_event(
        InputEvent::Command(AppCommand::ToggleLastBuffer),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.doc.file_name(), "code.rs");
    app.handle_event(
        InputEvent::Command(AppCommand::CloseTab),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.confirm_modal.visible);
    app.render(&mut raster);
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Char('d'),
            mods: Modifiers::NONE,
            text: Some("d".into()),
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(!app.confirm_modal.visible);
    assert_eq!(app.doc.file_name(), "next.rs");
}

#[test]
fn quick_open_and_project_wide_swar_search() {
    let fs = Arc::new(MemoryFileSystem::new());
    fs.add_file("/repo/src/alpha.rs", b"fn alpha() {}\n".to_vec());
    fs.add_file(
        "/repo/src/nested/swar_engine.rs",
        b"line 1\nline 2\npub fn fast_needle_match() {}\n".to_vec(),
    );
    fs.add_file("/repo/target/ignored.rs", b"pub fn fast_needle_match() {}\n".to_vec());

    let mut app = EditorApp::new(fs.clone(), 1200, 800, 2.0);
    let mut raster = BuiltinBitmapRasterizer;
    let mut clip = MemoryClipboard::new();
    let mut host = HeadlessHost::new();

    app.set_sidebar_root(&PathBuf::from("/repo"));
    app.open_file(&PathBuf::from("/repo/src/alpha.rs"), &mut host).unwrap();

    // 1. Quick Open (Cmd+P) fuzzy-matches `swar_engine.rs` and ignores `target/`.
    app.handle_event(
        InputEvent::Command(AppCommand::QuickOpen),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.palette.is_open());
    assert!(
        !app.palette.items.iter().any(|it| it.secondary.contains("target")),
        "target/ should be skipped!"
    );
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Char('s'),
            mods: Modifiers::NONE,
            text: Some("swar".into()),
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.palette.items.len(), 1);
    assert_eq!(app.palette.items[0].primary, "swar_engine.rs");
    app.render(&mut raster);
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Enter,
            mods: Modifiers::NONE,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(!app.palette.is_open());
    assert_eq!(app.doc.file_name(), "swar_engine.rs");

    // 2. Project Search (Cmd+Shift+F) finds `fast_needle_match` on line 3 and jumps right to it!
    app.open_file(&PathBuf::from("/repo/src/alpha.rs"), &mut host).unwrap();
    app.handle_event(
        InputEvent::Command(AppCommand::ProjectSearch),
        &mut raster,
        &mut clip,
        &mut host,
    );
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Char('f'),
            mods: Modifiers::NONE,
            text: Some("fast_needle".into()),
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.palette.items.len(), 1);
    assert_eq!(app.palette.items[0].line_0based, Some(2));
    app.render(&mut raster);
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Enter,
            mods: Modifiers::NONE,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.doc.file_name(), "swar_engine.rs");
    let (line, _) = app.doc.byte_to_line_col(app.caret);
    assert_eq!(line, 2, "must jump directly to line 3 (0-based 2)!");
}

#[test]
fn ipc_full_ui_inspection_and_control() {
    use editor_platform::ipc::{IpcCommand, IpcReply};

    let fs = Arc::new(MemoryFileSystem::new());
    fs.add_file("/repo/demo.rs", b"fn first() {}\nfn second() {}\n".to_vec());

    let mut app = EditorApp::new(fs.clone(), 1200, 800, 2.0);
    let mut raster = BuiltinBitmapRasterizer;
    let mut clip = MemoryClipboard::new();
    let mut host = HeadlessHost::new();

    app.set_sidebar_root(&PathBuf::from("/repo"));
    let (reply, activate, quit) = app.execute_ipc(
        &IpcCommand::Open(PathBuf::from("/repo/demo.rs")),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(activate && !quit);
    let IpcReply::Json(status) = reply else { panic!("expected json status") };
    assert!(status.contains("\"path\":\"/repo/demo.rs\""));
    assert!(status.contains("\"dirty\":false"));

    // Jump to line 2 col 3, insert text, verify dirty & cursor in status JSON!
    app.execute_ipc(
        &IpcCommand::Goto {
            line_1based: 2,
            col_bytes: 3,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    let (reply, _, _) = app.execute_ipc(
        &IpcCommand::Insert {
            text: "pub_".into(),
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    let IpcReply::Json(s) = reply else { panic!("expected json") };
    assert!(s.contains("\"dirty\":true"));
    assert!(s.contains("\"line\":2,\"col\":7"));
    assert_eq!(app.doc.line_content(1).unwrap(), "fn pub_second() {}");

    // Select range and check selection text in status JSON!
    let (reply, _, _) = app.execute_ipc(
        &IpcCommand::Select {
            start_line: 2,
            start_col: 3,
            end_line: 2,
            end_col: 13,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    let IpcReply::Json(s) = reply else { panic!("expected json") };
    assert!(s.contains("\"text\":\"pub_second\""));

    // Undo restores clean line; toggle wrap & dark theme via `Action`!
    app.execute_ipc(&IpcCommand::Action("undo".into()), &mut raster, &mut clip, &mut host);
    assert_eq!(app.doc.line_content(1).unwrap(), "fn second() {}");
    let (reply, _, _) = app.execute_ipc(
        &IpcCommand::Action("theme-dark".into()),
        &mut raster,
        &mut clip,
        &mut host,
    );
    let IpcReply::Json(s) = reply else { panic!("expected json") };
    assert!(s.contains("\"active_theme\":\"dark\""));

    // Find & replace-all via IPC, then save!
    app.execute_ipc(
        &IpcCommand::Find {
            query: "fn".into(),
            replace: Some("pub fn".into()),
            case_sensitive: true,
            replace_all: true,
            close: false,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.doc.line_content(0).unwrap(), "pub fn first() {}");

    let (reply, _, _) = app.execute_ipc(
        &IpcCommand::Save { as_path: None },
        &mut raster,
        &mut clip,
        &mut host,
    );
    let IpcReply::Json(s) = reply else { panic!("expected json") };
    assert!(s.contains("\"dirty\":false"));
}

#[test]
fn split_panes_independent_editing_and_ipc() {
    use editor_platform::ipc::{IpcCommand, IpcReply};

    let fs = Arc::new(MemoryFileSystem::new());
    fs.add_file("/repo/left.rs", b"fn left_fn() {}\n".to_vec());
    fs.add_file("/repo/right.rs", b"fn right_fn() {}\n".to_vec());

    let mut app = EditorApp::new(fs.clone(), 1200, 800, 2.0);
    let mut raster = BuiltinBitmapRasterizer;
    let mut clip = MemoryClipboard::new();
    let mut host = HeadlessHost::new();

    app.open_path_or_dir(&PathBuf::from("/repo/left.rs"), &mut host)
        .unwrap();
    assert!(!app.is_split());

    // Open right pane via IPC Split.
    let (reply, _, _) = app.execute_ipc(
        &IpcCommand::Split {
            path: Some(PathBuf::from("/repo/right.rs")),
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    let IpcReply::Json(s) = reply else { panic!("expected json") };
    assert!(app.is_split());
    assert!(s.contains("\"split\":true"));
    assert!(s.contains("\"active_pane\":1"));
    assert!(s.contains("/repo/left.rs"));
    assert!(s.contains("/repo/right.rs"));

    // Edit in active right pane without touching left pane.
    app.execute_ipc(
        &IpcCommand::Insert {
            text: "// right\n".into(),
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.doc.line_content(0).unwrap(), "// right");

    // Switch focus back to left pane and verify its content is untouched.
    let (reply, _, _) = app.execute_ipc(
        &IpcCommand::FocusPane(0),
        &mut raster,
        &mut clip,
        &mut host,
    );
    let IpcReply::Json(s) = reply else { panic!("expected json") };
    assert!(s.contains("\"active_pane\":0"));
    assert_eq!(app.doc.line_content(0).unwrap(), "fn left_fn() {}");

    // Render dual-pane frame to ensure layout & paint succeed cleanly.
    assert!(app.render(&mut raster));

    // Close split pane.
    let (reply, _, _) = app.execute_ipc(&IpcCommand::Unsplit, &mut raster, &mut clip, &mut host);
    let IpcReply::Json(s) = reply else { panic!("expected json") };
    assert!(!app.is_split());
    assert!(s.contains("\"split\":false"));
}

#[test]
fn split_pane_double_buffer_damage_tracking_is_bit_exact() {
    use editor_platform::{InputEvent, PixelRect, SurfaceMut};

    let fs = Arc::new(MemoryFileSystem::new());
    let mut left_text = String::new();
    let mut right_text = String::new();
    for i in 1..=120 {
        left_text.push_str(&format!("fn left_line_{i}() -> usize {{ {i} }}\n"));
        right_text.push_str(&format!("fn right_line_{i}() -> usize {{ {} }}\n", i * 10));
    }
    fs.add_file("/repo/left.rs", left_text.into_bytes());
    fs.add_file("/repo/right.rs", right_text.into_bytes());

    let (w, h, scale) = (800u32, 500u32, 2.0f32);
    let mut app = EditorApp::new(fs.clone(), w, h, scale);
    let mut raster = BuiltinBitmapRasterizer;
    let mut clip = MemoryClipboard::new();
    let mut host = HeadlessHost::new();

    app.open_path_or_dir(&PathBuf::from("/repo/left.rs"), &mut host)
        .unwrap();
    app.split_pane_with(Some(&PathBuf::from("/repo/right.rs")), &mut host);

    // Simulate a 2-slot double-buffered swapchain + buffer-age damage propagation.
    let mut slots = [vec![0u32; (w * h) as usize], vec![0u32; (w * h) as usize]];
    let mut needs_full = [true, true];
    let mut dmg = [PixelRect::ZERO, PixelRect::ZERO];
    let mut front = 1usize;

    let mut present_and_verify = |app: &mut EditorApp, raster: &mut BuiltinBitmapRasterizer| {
        let back = 1 - front;
        let surf = SurfaceMut {
            width: w,
            height: h,
            stride: w as usize,
            scale,
            pixels: &mut slots[back],
        };
        let painted = app
            .render_into_with_damage(surf, raster, dmg[back], needs_full[back])
            .expect("frame should paint");
        needs_full[back] = false;
        dmg[back] = PixelRect::ZERO;
        if !needs_full[front] {
            dmg[front] = dmg[front].union(painted);
        }
        front = back;

        // Compare bit-for-bit against a fresh full-redraw reference surface!
        let mut ref_buf = vec![0u32; (w * h) as usize];
        let ref_surf = SurfaceMut {
            width: w,
            height: h,
            stride: w as usize,
            scale,
            pixels: &mut ref_buf,
        };
        app.render_into_with_damage(ref_surf, raster, PixelRect::ZERO, true)
            .expect("reference full render");
        assert_eq!(slots[front], ref_buf, "swapchain back-buffer must match full redraw bit-for-bit");
    };

    // Frame 0: initial split render.
    present_and_verify(&mut app, &mut raster);

    // Frame 1: scroll right (active) pane down.
    let rp = app.layout.pane_body(&app.arrangement, 1);
    let rx = (rp.x + 40) as f32;
    let ry = (rp.y + 40) as f32;
    app.handle_event(
        InputEvent::Scroll {
            dx_px: 0.0,
            dy_px: 120.0,
            x_px: rx,
            y_px: ry,
            mods: Default::default(),
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    present_and_verify(&mut app, &mut raster);

    // Frame 2: scroll left (inactive) pane down while pointer is over left pane!
    let lp = app.layout.pane_body(&app.arrangement, 0);
    let lx = (lp.x + 40) as f32;
    let ly = (lp.y + 40) as f32;
    app.handle_event(
        InputEvent::Scroll {
            dx_px: 0.0,
            dy_px: 95.0,
            x_px: lx,
            y_px: ly,
            mods: Default::default(),
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    present_and_verify(&mut app, &mut raster);

    // Frame 3: caret blink partial dirty rect in active right pane.
    app.handle_event(
        InputEvent::Tick { now_ms: 1000 },
        &mut raster,
        &mut clip,
        &mut host,
    );
    present_and_verify(&mut app, &mut raster);

    // Frame 4: click to focus left pane + scroll again.
    app.focus_pane(0, &mut host);
    present_and_verify(&mut app, &mut raster);
}

#[test]
fn keymap_rebind_and_shortcuts_overlay_search_expand_and_run() {
    use editor_app::KeyChord;
    use editor_platform::{AppCommand, InputEvent, Key, Modifiers};

    let fs = Arc::new(MemoryFileSystem::new());
    fs.add_file("/repo/a.rs", b"fn main() {}\n".to_vec());
    let mut app = EditorApp::new(fs, 1120, 740, 2.0);
    let mut raster = BuiltinBitmapRasterizer;
    let mut clip = MemoryClipboard::new();
    let mut host = HeadlessHost::new();

    // Custom keybinding foundation: parse + rebind an action and verify lookup updates live!
    let chord = KeyChord::parse("cmd+shift+k").expect("parse chord");
    assert!(app.keymap.rebind("quick-open", chord));
    let mut m = Modifiers::NONE;
    #[cfg(target_os = "macos")]
    {
        m.meta = true;
    }
    #[cfg(not(target_os = "macos"))]
    {
        m.ctrl = true;
    }
    m.shift = true;
    assert_eq!(app.keymap.lookup_command(Key::Char('k'), m), Some(AppCommand::QuickOpen));

    // Open Keyboard Shortcuts overlay via `Cmd+?`, expand via `Tab`, search "vim", press `Enter` to toggle Vim!
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Char('?'),
            mods: m,
            text: Some("?".into()),
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.shortcuts.visible);
    assert!(!app.shortcuts.expanded);

    // Tab toggles expanded state to show all shortcuts.
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Tab,
            mods: Modifiers::NONE,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.shortcuts.expanded);
    assert!(app.render(&mut raster));

    // Type "Toggle Vim" and press Enter -> closes overlay and enables Vim mode!
    for ch in "Toggle Vim".chars() {
        app.handle_event(
            InputEvent::KeyDown {
                key: Key::Char(ch),
                mods: Modifiers::NONE,
                text: Some(ch.to_string()),
            },
            &mut raster,
            &mut clip,
            &mut host,
        );
    }
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Enter,
            mods: Modifiers::NONE,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(!app.shortcuts.visible);
    assert!(app.vim.enabled);
}

#[test]
fn vim_mode_motions_operators_text_objects_visual_and_splits() {
    use editor_app::VimMode;
    use editor_platform::{InputEvent, Key, Modifiers};

    let fs = Arc::new(MemoryFileSystem::new());
    fs.add_file(
        "/repo/vim.rs",
        b"fn alpha() {\n    let name = \"hello\";\n    let x = 1;\n}\n".to_vec(),
    );
    let mut app = EditorApp::new(fs, 1000, 600, 2.0);
    let mut raster = BuiltinBitmapRasterizer;
    let mut clip = MemoryClipboard::new();
    let mut host = HeadlessHost::new();

    app.open_path_or_dir(&PathBuf::from("/repo/vim.rs"), &mut host)
        .unwrap();
    app.vim.set_enabled(true);

    let send_ch = |app: &mut EditorApp,
                   ch: char,
                   raster: &mut BuiltinBitmapRasterizer,
                   clip: &mut MemoryClipboard,
                   host: &mut HeadlessHost| {
        app.handle_event(
            InputEvent::KeyDown {
                key: Key::Char(ch),
                mods: Modifiers::NONE,
                text: Some(ch.to_string()),
            },
            raster,
            clip,
            host,
        );
    };

    // `j` moves to line 2 (`    let name = "hello";`), `f"` jumps to opening quote, `ci"` changes inside quotes!
    send_ch(&mut app, 'j', &mut raster, &mut clip, &mut host);
    assert_eq!(app.doc.byte_to_line_col(app.caret).0, 1);

    send_ch(&mut app, 'f', &mut raster, &mut clip, &mut host);
    send_ch(&mut app, '"', &mut raster, &mut clip, &mut host);
    send_ch(&mut app, 'c', &mut raster, &mut clip, &mut host);
    send_ch(&mut app, 'i', &mut raster, &mut clip, &mut host);
    send_ch(&mut app, '"', &mut raster, &mut clip, &mut host);
    assert_eq!(app.vim.mode, VimMode::Insert);
    for ch in "world".chars() {
        send_ch(&mut app, ch, &mut raster, &mut clip, &mut host);
    }
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Escape,
            mods: Modifiers::NONE,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.vim.mode, VimMode::Normal);
    assert_eq!(app.doc.line_content(1).unwrap(), "    let name = \"world\";");

    // `j` -> `dd` deletes line 3, `p` pastes it back below!
    send_ch(&mut app, 'j', &mut raster, &mut clip, &mut host);
    send_ch(&mut app, 'd', &mut raster, &mut clip, &mut host);
    send_ch(&mut app, 'd', &mut raster, &mut clip, &mut host);
    assert_eq!(app.doc.line_content(2).unwrap(), "}");

    // `u` undoes `dd`!
    send_ch(&mut app, 'u', &mut raster, &mut clip, &mut host);
    assert_eq!(app.doc.line_content(2).unwrap(), "    let x = 1;");

    // `gg` jumps to top, `G` jumps to bottom.
    send_ch(&mut app, 'g', &mut raster, &mut clip, &mut host);
    send_ch(&mut app, 'g', &mut raster, &mut clip, &mut host);
    assert_eq!(app.doc.byte_to_line_col(app.caret).0, 0);
    send_ch(&mut app, 'G', &mut raster, &mut clip, &mut host);
    assert!(app.doc.byte_to_line_col(app.caret).0 >= 3);

    // `Ctrl+W v` splits pane right!
    let mut ctrl = Modifiers::NONE;
    ctrl.ctrl = true;
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Char('w'),
            mods: ctrl,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    send_ch(&mut app, 'v', &mut raster, &mut clip, &mut host);
    assert!(app.is_split());
}

#[test]
fn keymap_has_zero_conflicts_and_undo_to_clean_and_pane_swap_close() {
    use editor_app::Keymap;

    // 1. Single-source-of-truth keymap must NEVER ship conflicting chords in overlapping scopes!
    let km = Keymap::new();
    assert!(
        km.conflicts().is_empty(),
        "unexpected shortcut conflicts: {:?}",
        km.conflicts()
    );

    let fs = Arc::new(MemoryFileSystem::new());
    fs.add_file("/repo/left.rs", b"fn left() {}\n".to_vec());
    fs.add_file("/repo/right.rs", b"fn right() {}\n".to_vec());

    let mut app = EditorApp::new(fs.clone(), 1000, 600, 2.0);
    let mut host = HeadlessHost::new();
    app.open_path_or_dir(&PathBuf::from("/repo/left.rs"), &mut host)
        .unwrap();
    assert!(!app.doc.is_dirty());

    // 2. Edit -> dirty; Undo all the way -> cleanly NOT dirty; Redo -> dirty again!
    app.doc.edit(0, 0, "// edit\n", 100, false).unwrap();
    assert!(app.doc.is_dirty());
    app.doc.undo().unwrap();
    assert!(!app.doc.is_dirty(), "undoing all the way back must clear dirty flag");
    app.doc.redo().unwrap();
    assert!(app.doc.is_dirty());

    // Save at edited state -> clean; undo -> dirty; redo back to saved state -> clean again!
    app.doc.save(fs.as_ref(), None, false).unwrap();
    assert!(!app.doc.is_dirty());
    app.doc.undo().unwrap();
    assert!(app.doc.is_dirty());
    app.doc.redo().unwrap();
    assert!(!app.doc.is_dirty(), "redoing back to saved revision must clear dirty flag");

    // 3. Split, swap left<->right, and close active pane (keeps remaining pane!).
    app.split_pane_with(Some(&PathBuf::from("/repo/right.rs")), &mut host);
    assert_eq!(app.active_pane_index(), 1);
    assert_eq!(app.doc.file_name(), "right.rs");

    app.swap_panes(&mut host);
    assert_eq!(app.active_pane_index(), 0);
    assert_eq!(app.doc.file_name(), "right.rs");

    // Closing active pane (`right.rs`) promotes remaining pane (`left.rs`)!
    app.request_close_active_pane(&mut host);
    assert!(!app.is_split());
    assert_eq!(app.doc.file_name(), "left.rs");
}

#[test]
fn per_file_type_word_wrap_applies_and_persists_per_extension() {
    use editor_platform::ipc::{IpcCommand, IpcReply};

    let fs = Arc::new(MemoryFileSystem::new());
    fs.add_file("/repo/README.md", b"# Heading\nSome prose.\n".to_vec());
    fs.add_file("/repo/main.rs", b"fn main() {}\n".to_vec());

    let mut app = EditorApp::new(fs.clone(), 1000, 600, 2.0);
    let mut raster = BuiltinBitmapRasterizer;
    let mut clip = MemoryClipboard::new();
    let mut host = HeadlessHost::new();

    // Markdown wraps by default, Rust does not!
    app.open_path_or_dir(&PathBuf::from("/repo/README.md"), &mut host)
        .unwrap();
    assert!(app.viewport.word_wrap);

    app.open_path_or_dir(&PathBuf::from("/repo/main.rs"), &mut host)
        .unwrap();
    assert!(!app.viewport.word_wrap);

    // Toggle wrap on `main.rs` -> persists specifically for `.rs`, leaving `.md` unchanged!
    let (reply, _, _) = app.execute_ipc(
        &IpcCommand::Action("toggle-wrap".into()),
        &mut raster,
        &mut clip,
        &mut host,
    );
    let IpcReply::Json(s) = reply else { panic!("expected json") };
    assert!(app.viewport.word_wrap);
    assert!(s.contains("\"rs\":true"));

    // Configure via CLI action `wrap-ext:rs=off` and verify live update!
    let (reply, _, _) = app.execute_ipc(
        &IpcCommand::Action("wrap-ext:rs=off".into()),
        &mut raster,
        &mut clip,
        &mut host,
    );
    let IpcReply::Json(s) = reply else { panic!("expected json") };
    assert!(!app.viewport.word_wrap);
    assert!(s.contains("\"rs\":false"));
}

#[test]
fn grid_2x2_drag_and_drop_files_and_pane_headers_with_bit_exact_swapchain() {
    use editor_app::DropZone;
    use editor_platform::{DragPhase, InputEvent, Modifiers, MouseButton, PixelRect, SurfaceMut};

    let fs = Arc::new(MemoryFileSystem::new());
    fs.add_file("/repo/tl.rs", b"fn tl() -> u32 { 1 }\n".to_vec());
    fs.add_file("/repo/tr.rs", b"fn tr() -> u32 { 2 }\n".to_vec());
    fs.add_file("/repo/bl.rs", b"fn bl() -> u32 { 3 }\n".to_vec());
    fs.add_file("/repo/br.rs", b"fn br() -> u32 { 4 }\n".to_vec());

    let (w, h, scale) = (1000u32, 640u32, 2.0f32);
    let mut app = EditorApp::new(fs.clone(), w, h, scale);
    let mut raster = BuiltinBitmapRasterizer;
    let mut clip = MemoryClipboard::new();
    let mut host = HeadlessHost::new();

    app.set_sidebar_root(std::path::Path::new("/repo"));
    app.open_path_or_dir(&PathBuf::from("/repo/tl.rs"), &mut host)
        .unwrap();

    // 2-slot swapchain verifier: ensures every partial drag/split/scroll repaint matches full redraw!
    let mut slots = [vec![0u32; (w * h) as usize], vec![0u32; (w * h) as usize]];
    let mut needs_full = [true, true];
    let mut dmg = [PixelRect::ZERO, PixelRect::ZERO];
    let mut front = 1usize;

    let mut verify_frame = |app: &mut EditorApp, raster: &mut BuiltinBitmapRasterizer| {
        if !app.is_dirty_frame() && !needs_full[1 - front] {
            return;
        }
        let back = 1 - front;
        let surf = SurfaceMut {
            width: w,
            height: h,
            stride: w as usize,
            scale,
            pixels: &mut slots[back],
        };
        if let Some(painted) =
            app.render_into_with_damage(surf, raster, dmg[back], needs_full[back])
        {
            needs_full[back] = false;
            dmg[back] = PixelRect::ZERO;
            if !needs_full[front] {
                dmg[front] = dmg[front].union(painted);
            }
            front = back;
        }
        let mut ref_buf = vec![0u32; (w * h) as usize];
        let ref_surf = SurfaceMut {
            width: w,
            height: h,
            stride: w as usize,
            scale,
            pixels: &mut ref_buf,
        };
        app.render_into_with_damage(ref_surf, raster, PixelRect::ZERO, true)
            .expect("reference full render");
        assert_eq!(
            slots[front], ref_buf,
            "swapchain back-buffer must match full redraw bit-for-bit during drag & 2x2 grid"
        );
    };

    verify_frame(&mut app, &mut raster);

    // 1. External Finder drag of `tr.rs` onto the right edge of pane 0 -> splits right!
    let p0 = app.layout.grid.panes()[0].frame;
    let (rx, ry) = ((p0.right() - 24) as f32, (p0.y + p0.h / 2) as f32);
    app.handle_event(
        InputEvent::ExternalDrag {
            phase: DragPhase::Enter,
            x_px: rx,
            y_px: ry,
            path: Some(PathBuf::from("/repo/tr.rs")),
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.is_dragging());
    assert_eq!(app.drag_plan().unwrap().zone, DropZone::Right);
    verify_frame(&mut app, &mut raster);

    // Move drag slightly within the right edge and verify partial-damage preview repaint!
    app.handle_event(
        InputEvent::ExternalDrag {
            phase: DragPhase::Move,
            x_px: rx - 10.0,
            y_px: ry + 20.0,
            path: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    verify_frame(&mut app, &mut raster);

    // Drop to commit the right split!
    app.handle_event(
        InputEvent::ExternalDrag {
            phase: DragPhase::Drop,
            x_px: rx - 10.0,
            y_px: ry + 20.0,
            path: Some(PathBuf::from("/repo/tr.rs")),
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(!app.is_dragging());
    assert_eq!(app.pane_count(), 2);
    assert_eq!(app.doc.file_name(), "tr.rs");
    verify_frame(&mut app, &mut raster);

    // 2. Drag `bl.rs` onto the bottom edge of the left pane (index 0) -> creates bottom-left pane!
    let left_frame = app
        .layout
        .grid
        .get(app.arrangement.id_at_index(0).unwrap())
        .unwrap()
        .frame;
    let (bx, by) = (
        (left_frame.x + left_frame.w / 2) as f32,
        (left_frame.bottom() - 20) as f32,
    );
    app.handle_event(
        InputEvent::ExternalDrag {
            phase: DragPhase::Enter,
            x_px: bx,
            y_px: by,
            path: Some(PathBuf::from("/repo/bl.rs")),
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.drag_plan().unwrap().zone, DropZone::Bottom);
    verify_frame(&mut app, &mut raster);
    app.handle_event(
        InputEvent::ExternalDrag {
            phase: DragPhase::Drop,
            x_px: bx,
            y_px: by,
            path: Some(PathBuf::from("/repo/bl.rs")),
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.pane_count(), 3);
    assert_eq!(app.doc.file_name(), "bl.rs");
    verify_frame(&mut app, &mut raster);

    // 3. Drag `br.rs` onto the bottom edge of the right pane (index 1) -> full 2x2 grid (4 panes)!
    let right_frame = app
        .layout
        .grid
        .get(app.arrangement.id_at_index(1).unwrap())
        .unwrap()
        .frame;
    let (brx, bry) = (
        (right_frame.x + right_frame.w / 2) as f32,
        (right_frame.bottom() - 20) as f32,
    );
    app.handle_event(
        InputEvent::ExternalDrag {
            phase: DragPhase::Enter,
            x_px: brx,
            y_px: bry,
            path: Some(PathBuf::from("/repo/br.rs")),
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.drag_plan().unwrap().zone, DropZone::Bottom);
    verify_frame(&mut app, &mut raster);
    app.handle_event(
        InputEvent::ExternalDrag {
            phase: DragPhase::Drop,
            x_px: brx,
            y_px: bry,
            path: Some(PathBuf::from("/repo/br.rs")),
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.pane_count(), 4);
    assert_eq!(app.active_pane_index(), 3);
    assert_eq!(app.doc.file_name(), "br.rs");
    verify_frame(&mut app, &mut raster);

    // 4. Dragging a 5th file onto an edge when already at 2x2 (4 panes) cleanly falls back to Center!
    let tl_frame = app
        .layout
        .grid
        .get(app.arrangement.id_at_index(0).unwrap())
        .unwrap()
        .frame;
    app.handle_event(
        InputEvent::ExternalDrag {
            phase: DragPhase::Enter,
            x_px: (tl_frame.x + 8) as f32,
            y_px: (tl_frame.y + tl_frame.h / 2) as f32,
            path: Some(PathBuf::from("/repo/br.rs")),
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(
        app.drag_plan().unwrap().zone,
        DropZone::Center,
        "when 4 panes are already open, edge drops must preview Center replacement"
    );
    app.handle_event(
        InputEvent::ExternalDrag {
            phase: DragPhase::Exit,
            x_px: 0.0,
            y_px: 0.0,
            path: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    verify_frame(&mut app, &mut raster);

    // 5. Drag top-left pane header onto bottom-right pane center -> swaps TL and BR!
    let tl_hdr = app
        .layout
        .grid
        .get(app.arrangement.id_at_index(0).unwrap())
        .unwrap()
        .header;
    let br_body = app
        .layout
        .grid
        .get(app.arrangement.id_at_index(3).unwrap())
        .unwrap()
        .body;
    let (hx, hy) = ((tl_hdr.x + 40) as f32, (tl_hdr.y + tl_hdr.h / 2) as f32);
    let (tx, ty) = (
        (br_body.x + br_body.w / 2) as f32,
        (br_body.y + br_body.h / 2) as f32,
    );
    app.handle_event(
        InputEvent::MouseDown {
            x_px: hx,
            y_px: hy,
            button: MouseButton::Left,
            click_count: 1,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    app.handle_event(
        InputEvent::MouseMove {
            x_px: tx,
            y_px: ty,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.is_dragging());
    verify_frame(&mut app, &mut raster);
    app.handle_event(
        InputEvent::MouseUp {
            x_px: tx,
            y_px: ty,
            button: MouseButton::Left,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.active_pane_index(), 3);
    assert_eq!(app.doc.file_name(), "tl.rs");
    verify_frame(&mut app, &mut raster);
}

#[test]
fn pinned_working_set_fullscreen_flicking_zoom_overlay_and_tab_drag_to_split() {
    use editor_app::OpenMode;
    use editor_platform::{
        AppCommand, InputEvent, Key, Modifiers, MouseButton, PixelRect, SurfaceMut,
    };

    let fs = Arc::new(MemoryFileSystem::new());
    fs.add_file("/repo/a.rs", b"fn a() {\n    let x = 1;\n}\n".to_vec());
    fs.add_file("/repo/b.rs", b"fn b() {\n    let y = 2;\n}\n".to_vec());
    fs.add_file("/repo/c.rs", b"fn c() {\n    let z = 3;\n}\n".to_vec());
    fs.add_file("/repo/d.rs", b"fn d() {\n    let w = 4;\n}\n".to_vec());
    fs.add_file("/repo/e.rs", b"fn e() {}\n".to_vec());

    let (w, h, scale) = (1120u32, 740u32, 2.0f32);
    let mut app = EditorApp::new(fs.clone(), w, h, scale);
    let mut raster = BuiltinBitmapRasterizer;
    let mut clip = MemoryClipboard::new();
    let mut host = HeadlessHost::new();

    app.set_sidebar_root(std::path::Path::new("/repo"));

    // 1. Single-click preview slot reuses cleanly without tab sprawl!
    let a_id = app
        .open_file_with_mode(&PathBuf::from("/repo/a.rs"), OpenMode::Preview, &mut host)
        .unwrap();
    assert_eq!(app.working_set.preview, Some(a_id));
    assert_eq!(app.working_set.len(), 1);

    let b_id = app
        .open_file_with_mode(&PathBuf::from("/repo/b.rs"), OpenMode::Preview, &mut host)
        .unwrap();
    assert_eq!(app.working_set.preview, Some(b_id));
    assert_eq!(app.working_set.len(), 1, "preview click replaces previous clean preview");

    // 2. Editing graduates preview -> Kept open tab, and saving NEVER removes it!
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Char('/'),
            mods: Modifiers::NONE,
            text: Some("// edited\n".into()),
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.working_set.preview, None);
    assert!(app.working_set.open.contains(&b_id));
    app.handle_event(
        InputEvent::Command(AppCommand::Save),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(!app.doc.is_dirty());
    assert!(
        app.working_set.open.contains(&b_id),
        "saving a Kept tab must NEVER make it disappear"
    );

    // 3. Pin 3 files into numbered slots 1, 2, 3 and flick between them in fullscreen!
    app.toggle_pin_buffer(b_id, &mut host);
    let a_id = app
        .open_file_with_mode(&PathBuf::from("/repo/a.rs"), OpenMode::Keep, &mut host)
        .unwrap();
    app.toggle_pin_buffer(a_id, &mut host);
    let c_id = app
        .open_file_with_mode(&PathBuf::from("/repo/c.rs"), OpenMode::Keep, &mut host)
        .unwrap();
    app.toggle_pin_buffer(c_id, &mut host);
    assert_eq!(app.working_set.pinned, vec![b_id, a_id, c_id]);

    // Move caret in `c.rs` and type dirty text; flick to Pin 1 (`b.rs`) and back to Pin 3 (`c.rs`):
    app.caret = 5;
    app.anchor = 5;
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Char('!'),
            mods: Modifiers::NONE,
            text: Some("/*pin3*/".into()),
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    let saved_caret = app.caret;
    app.handle_event(
        InputEvent::Command(AppCommand::JumpPin(1)),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.doc.file_name(), "b.rs");
    app.handle_event(
        InputEvent::Command(AppCommand::JumpPin(3)),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.doc.file_name(), "c.rs");
    assert_eq!(app.caret, saved_caret, "flicking must restore exact caret position");
    assert!(app.doc.is_dirty(), "flicking must preserve unsaved edits in memory");

    // 4. `Ctrl+Tab` MRU ping-pong toggles cleanly between `c.rs` and `b.rs`!
    app.handle_event(
        InputEvent::Command(AppCommand::ToggleLastBuffer),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.doc.file_name(), "b.rs");
    app.handle_event(
        InputEvent::Command(AppCommand::ToggleLastBuffer),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.doc.file_name(), "c.rs");

    // 5. `⌘E` Pinned & Open Files overlay: reorder Pin 1 <-> Pin 2 with `⌥↓`, then jump via `1`!
    app.handle_event(
        InputEvent::Command(AppCommand::JumpPin(1)),
        &mut raster,
        &mut clip,
        &mut host,
    );
    app.handle_event(
        InputEvent::Command(AppCommand::ShowPins),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.pins_overlay.visible);
    app.render(&mut raster);
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Down,
            mods: Modifiers {
                alt: true,
                ..Modifiers::NONE
            },
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(
        app.working_set.pinned,
        vec![a_id, b_id, c_id],
        "Option+Down in Pinned & Open Files overlay must swap slot 1 and slot 2"
    );
    // Press `1` to jump directly to new Pin 1 (`a.rs`) and close overlay!
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Char('1'),
            mods: Modifiers::NONE,
            text: Some("1".into()),
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(!app.pins_overlay.visible);
    assert_eq!(app.doc.file_name(), "a.rs");

    // 6. Fullscreen Zoom on a 1x2 split + `⌘2` teleport + unzoom restores exact grid!
    app.split_pane_with(Some(&PathBuf::from("/repo/b.rs")), &mut host);
    assert_eq!(app.pane_count(), 2);
    assert_eq!(app.doc.file_name(), "b.rs");

    // `⌘1` teleports focus to left pane (`a.rs`) when unzoomed!
    app.handle_event(
        InputEvent::Command(AppCommand::JumpPin(1)),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.active_pane_index(), 0);
    assert_eq!(app.doc.file_name(), "a.rs");

    // Zoom (`⇧⌘Enter`): layout shows only 1 fullscreen pane with zero pane header!
    app.handle_event(
        InputEvent::Command(AppCommand::ToggleZoomPane),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.zoomed);
    assert_eq!(app.layout.grid.panes().len(), 1);
    assert!(app.layout.grid.panes()[0].header.is_empty());

    // While zoomed, pressing `⌘2` smoothly switches fullscreen to `b.rs`!
    app.handle_event(
        InputEvent::Command(AppCommand::JumpPin(2)),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.zoomed);
    assert_eq!(app.doc.file_name(), "b.rs");

    // Unzooming restores the 2-pane split cleanly!
    app.handle_event(
        InputEvent::Command(AppCommand::ToggleZoomPane),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(!app.zoomed);
    assert_eq!(app.layout.grid.panes().len(), 2);

    // 7. Double-buffered swapchain bit-exact check while dragging a tab pill down onto a pane edge!
    let mut slots = [vec![0u32; (w * h) as usize], vec![0u32; (w * h) as usize]];
    let mut needs_full = [true, true];
    let mut dmg = [PixelRect::ZERO, PixelRect::ZERO];
    let mut front = 1usize;
    let mut verify = |app: &mut EditorApp, raster: &mut BuiltinBitmapRasterizer| {
        let back = 1 - front;
        let surf = SurfaceMut {
            width: w,
            height: h,
            stride: w as usize,
            scale,
            pixels: &mut slots[back],
        };
        if let Some(painted) =
            app.render_into_with_damage(surf, raster, dmg[back], needs_full[back])
        {
            needs_full[back] = false;
            dmg[back] = PixelRect::ZERO;
            if !needs_full[front] {
                dmg[front] = dmg[front].union(painted);
            }
            front = back;
        }
        let mut ref_buf = vec![0u32; (w * h) as usize];
        let ref_surf = SurfaceMut {
            width: w,
            height: h,
            stride: w as usize,
            scale,
            pixels: &mut ref_buf,
        };
        app.render_into_with_damage(ref_surf, raster, PixelRect::ZERO, true)
            .expect("ref full render");
        assert_eq!(slots[front], ref_buf, "tab strip + drag swapchain must be bit-exact");
    };
    verify(&mut app, &mut raster);

    // Find tab 3 (`c.rs`) in the top strip and drag it onto the bottom edge of pane 1 -> 3 panes!
    let strip = app.layout.tab_strip;
    let tab3_x = (strip.x + 280) as f32;
    let tab_y = (strip.y + strip.h / 2) as f32;
    let p1 = app
        .layout
        .grid
        .get(app.arrangement.id_at_index(1).unwrap())
        .unwrap()
        .frame;
    let (drop_x, drop_y) = ((p1.x + p1.w / 2) as f32, (p1.bottom() - 24) as f32);
    app.handle_event(
        InputEvent::MouseDown {
            x_px: tab3_x,
            y_px: tab_y,
            button: MouseButton::Left,
            click_count: 1,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    app.handle_event(
        InputEvent::MouseMove {
            x_px: drop_x,
            y_px: drop_y,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    verify(&mut app, &mut raster);
    app.handle_event(
        InputEvent::MouseUp {
            x_px: drop_x,
            y_px: drop_y,
            button: MouseButton::Left,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    verify(&mut app, &mut raster);

    // 8. Verify Neovim `<Space>a`, `<Space>1`, `<Space><Space>`, `gt`, `⌃W z`, `⌃W o`!
    app.vim.set_enabled(true);
    let send_ch = |app: &mut EditorApp,
                   ch: char,
                   raster: &mut BuiltinBitmapRasterizer,
                   clip: &mut MemoryClipboard,
                   host: &mut HeadlessHost| {
        app.handle_event(
            InputEvent::KeyDown {
                key: Key::Char(ch),
                mods: Modifiers::NONE,
                text: Some(ch.to_string()),
            },
            raster,
            clip,
            host,
        );
    };
    send_ch(&mut app, ' ', &mut raster, &mut clip, &mut host);
    send_ch(&mut app, '1', &mut raster, &mut clip, &mut host);
    assert_eq!(app.doc.file_name(), "a.rs");

    // `Ctrl+W o` collapses to single pane cleanly!
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Char('w'),
            mods: Modifiers {
                ctrl: true,
                ..Modifiers::NONE
            },
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    send_ch(&mut app, 'o', &mut raster, &mut clip, &mut host);
    assert_eq!(app.pane_count(), 1);
    assert!(!app.zoomed);
    verify(&mut app, &mut raster);
}

#[test]
fn buffer_pool_dirty_quit_guard_strip_drag_reordering_and_ipc_pins() {
    use editor_platform::ipc::{IpcCommand, IpcReply};
    use editor_platform::{AppCommand, InputEvent, Key, Modifiers, MouseButton};

    let fs = Arc::new(MemoryFileSystem::new());
    fs.add_file("/repo/one.rs", b"fn one() {}\n".to_vec());
    fs.add_file("/repo/two.rs", b"fn two() {}\n".to_vec());
    fs.add_file("/repo/three.rs", b"fn three() {}\n".to_vec());

    let mut app = EditorApp::new(fs.clone(), 1120, 740, 2.0);
    app.use_in_app_modal = true;
    let mut raster = BuiltinBitmapRasterizer;
    let mut clip = MemoryClipboard::new();
    let mut host = HeadlessHost::new();

    app.set_sidebar_root(std::path::Path::new("/repo"));

    // 1. Open and pin `one.rs` (slot 1) & `two.rs` (slot 2) via IPC, plus keep `three.rs` unpinned!
    let (reply, _, _) = app.execute_ipc(
        &IpcCommand::Action("pin:/repo/one.rs".into()),
        &mut raster,
        &mut clip,
        &mut host,
    );
    let IpcReply::Json(s) = reply else { panic!("expected json") };
    assert!(s.contains("\"slot\":1"));

    app.execute_ipc(
        &IpcCommand::Action("pin:/repo/two.rs".into()),
        &mut raster,
        &mut clip,
        &mut host,
    );
    app.open_file(&PathBuf::from("/repo/three.rs"), &mut host).unwrap();
    app.render(&mut raster);
    assert_eq!(app.working_set.pinned.len(), 2);
    assert_eq!(app.working_set.open.len(), 1);

    // 2. Drag unpinned `three.rs` horizontally across `│` to the far left of the strip -> pins as slot 1!
    let strip = app.layout.tab_strip;
    let cy = (strip.y + strip.h / 2) as f32;
    let mut three_x = None;
    for probe in (strip.x + 10..strip.right() - 120).step_by(8) {
        if let editor_render::TabStripHit::Tab(id) = app.tab_strip.hit_test(probe as f32, cy) {
            if app.working_set.get_meta(id).is_some_and(|m| m.file_name == "three.rs") {
                three_x = Some(probe as f32);
                break;
            }
        }
    }
    let sx = three_x.expect("three.rs tab must be hit-testable in top strip");
    let dst_x = (strip.x + 12) as f32;
    app.handle_event(
        InputEvent::MouseDown {
            x_px: sx,
            y_px: cy,
            button: MouseButton::Left,
            click_count: 1,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    app.handle_event(
        InputEvent::MouseMove {
            x_px: dst_x,
            y_px: cy,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.is_dragging());
    app.handle_event(
        InputEvent::MouseUp {
            x_px: dst_x,
            y_px: cy,
            button: MouseButton::Left,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.working_set.pinned.len(), 3);
    assert_eq!(
        app.working_set
            .get_meta(app.working_set.pinned[0])
            .unwrap()
            .file_name,
        "three.rs",
        "dragging unpinned tab to left edge of strip must pin it into slot 1"
    );

    // 3. IPC `unpin:1`, `jump-pin:1`, `zoom`, `new-tab`, `close-tab`!
    app.execute_ipc(
        &IpcCommand::Action("unpin:1".into()),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.working_set.pinned.len(), 2);
    app.execute_ipc(
        &IpcCommand::Action("jump-pin:1".into()),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.doc.file_name(), "one.rs");

    // 4. Split `two.rs`, edit it so it's dirty, then close split pane (`⌘W`):
    //    split closes cleanly AND dirty edits stay safely parked in memory!
    app.split_pane_with(Some(&PathBuf::from("/repo/two.rs")), &mut host);
    app.execute_ipc(
        &IpcCommand::Action("zoom".into()),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.zoomed);
    app.execute_ipc(
        &IpcCommand::Action("zoom".into()),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(!app.zoomed);
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Char('/'),
            mods: Modifiers::NONE,
            text: Some("// dirty_split\n".into()),
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.doc.is_dirty());
    app.handle_event(
        InputEvent::Command(AppCommand::CloseTab),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(!app.is_split(), "Cmd+W in unzoomed split closes split pane");
    assert_eq!(app.doc.file_name(), "one.rs");

    // Also make `one.rs` dirty, then switch to clean `three.rs`!
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Char('/'),
            mods: Modifiers::NONE,
            text: Some("// dirty_one\n".into()),
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    app.open_file(&PathBuf::from("/repo/three.rs"), &mut host).unwrap();
    assert!(!app.doc.is_dirty(), "active buffer three.rs is clean");

    // 5. Quitting while TWO backgrounded buffers (`one.rs` & `two.rs`) are dirty MUST prompt for EACH!
    let (reply, _, quit_now) = app.execute_ipc(
        &IpcCommand::Quit { force: false },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(!quit_now);
    assert_eq!(reply, IpcReply::Prompting);
    assert!(app.confirm_modal.visible);
    assert!(app.doc.is_dirty(), "quit guard must activate first dirty parked buffer");

    // Discard first dirty buffer -> modal automatically advances to second dirty buffer!
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Char('d'),
            mods: Modifiers::NONE,
            text: Some("d".into()),
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.confirm_modal.visible, "must prompt for second dirty parked buffer too");
    assert!(!host.quit_requested);

    // Discard second dirty buffer -> all buffers clean -> quits!
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Char('d'),
            mods: Modifiers::NONE,
            text: Some("d".into()),
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(!app.confirm_modal.visible);
    assert!(host.quit_requested);

    // 6. Closing the last remaining buffer (`three.rs`) resets cleanly to a fresh Untitled buffer!
    app.handle_event(
        InputEvent::Command(AppCommand::CloseTab),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.working_set.len(), 1);
    assert_eq!(app.doc.file_name(), "Untitled");
}

#[test]
fn syntax_cli_ipc_overrides_styles_ext_map_and_plugin_line_comments() {
    use editor_platform::ipc::{IpcCommand, IpcReply};
    use editor_platform::{AppCommand, InputEvent};

    let fs = Arc::new(MemoryFileSystem::new());
    fs.add_file("/repo/query.sql", b"SELECT 1;\n".to_vec());
    fs.add_file("/repo/sketch.ino", b"void setup() { int x = 42; }\n".to_vec());

    let mut app = EditorApp::new(fs.clone(), 900, 600, 2.0);
    let mut raster = BuiltinBitmapRasterizer;
    let mut clip = MemoryClipboard::new();
    let mut host = HeadlessHost::new();

    // 1. SQL file auto-detects `sql` plugin and `⌘/` uses `-- ` from LanguageMeta!
    app.open_file(&PathBuf::from("/repo/query.sql"), &mut host).unwrap();
    assert_eq!(app.doc.language().id(), "sql");
    app.handle_event(
        InputEvent::Command(AppCommand::ToggleComment),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.doc.line_content(0).unwrap(), "-- SELECT 1;");
    app.handle_event(
        InputEvent::Command(AppCommand::ToggleComment),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.doc.line_content(0).unwrap(), "SELECT 1;");

    // 2. IPC `syntax-list` returns compiled plugins JSON.
    let (reply, _, _) = app.execute_ipc(
        &IpcCommand::Action("syntax-list".into()),
        &mut raster,
        &mut clip,
        &mut host,
    );
    match reply {
        IpcReply::Json(j) => {
            assert!(j.contains("\"id\":\"cpp\""));
            assert!(j.contains("\"id\":\"csharp\""));
            assert!(j.contains("\"id\":\"typescript\""));
            assert!(j.contains("\"id\":\"powershell\""));
            assert!(j.contains("\"id\":\"kotlin\""));
        }
        other => panic!("expected Json reply, got {other:?}"),
    }

    // 3. IPC `syntax-map:ino=cpp` maps custom `.ino` extension to C++ and persists preference!
    app.open_file(&PathBuf::from("/repo/sketch.ino"), &mut host).unwrap();
    assert_eq!(app.doc.language().id(), "plaintext");
    let (reply, _, _) = app.execute_ipc(
        &IpcCommand::Action("syntax-map:ino=cpp".into()),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(matches!(reply, IpcReply::Json(_)));
    assert_eq!(app.doc.language().id(), "cpp");

    // 4. IPC `syntax-style:studio` switches curated preset and persists.
    let (reply, _, _) = app.execute_ipc(
        &IpcCommand::Action("syntax-style:studio".into()),
        &mut raster,
        &mut clip,
        &mut host,
    );
    match reply {
        IpcReply::Json(j) => assert!(j.contains("\"preset\":\"studio\"")),
        other => panic!("unexpected reply: {other:?}"),
    }
    assert_eq!(app.syntax_theme.preset, editor_render::SyntaxThemePreset::Studio);

    // 5. IPC `syntax-color` customizes individual token colour + font style cleanly per mode!
    let (reply, _, _) = app.execute_ipc(
        &IpcCommand::Action("syntax-color:dark:comment:#55aa77:italic".into()),
        &mut raster,
        &mut clip,
        &mut host,
    );
    match reply {
        IpcReply::Json(j) => assert!(j.contains("\"#55aa77:italic\"")),
        other => panic!("unexpected reply: {other:?}"),
    }

    // 6. IPC `syntax-theme-json` atomically applies a complete custom theme in one call!
    let (reply, _, _) = app.execute_ipc(
        &IpcCommand::Action(
            "syntax-theme-json:{\"preset\":\"warm\",\"light\":{\"keyword\":\"#772244 bold\",\"string\":{\"color\":\"#115533\",\"style\":\"normal\"}},\"dark\":{\"keyword\":\"#ffaa88 bold\"}}".into(),
        ),
        &mut raster,
        &mut clip,
        &mut host,
    );
    match reply {
        IpcReply::Json(j) => {
            assert!(j.contains("\"preset\":\"warm\""));
            assert!(j.contains("\"#772244\""));
            assert!(j.contains("\"#ffaa88\""));
        }
        other => panic!("unexpected reply: {other:?}"),
    }
    assert!(app.render(&mut raster));

    // Verify O(1) flat palette lookup reflects the exact custom colour & style!
    app.set_theme_preference(editor_platform::ThemePreference::Light);
    let (kw_col, kw_font) = app.active_theme().syntax_font_style(editor_core::TokenKind::Keyword);
    assert_eq!(kw_col, editor_render::Color::rgb(0x77, 0x22, 0x44));
    assert_eq!(kw_font, editor_platform::FontStyle::MonoBold);

    // 7. Reloading preferences into a fresh EditorApp restores preset, overrides, AND ext map!
    let mut app2 = EditorApp::new(fs, 900, 600, 2.0);
    app2.load_preferences(&host);
    assert_eq!(app2.syntax_theme.preset, editor_render::SyntaxThemePreset::Warm);
    app2.set_theme_preference(editor_platform::ThemePreference::Light);
    let (kw2, _) = app2.active_theme().syntax_font_style(editor_core::TokenKind::Keyword);
    assert_eq!(kw2, editor_render::Color::rgb(0x77, 0x22, 0x44));
    app2.set_theme_preference(editor_platform::ThemePreference::Dark);
    let (cm2, cm2_font) = app2.active_theme().syntax_font_style(editor_core::TokenKind::Comment);
    assert_eq!(cm2, editor_render::Color::rgb(0x55, 0xaa, 0x77));
    assert_eq!(cm2_font, editor_platform::FontStyle::MonoItalic);
    app2.open_file(&PathBuf::from("/repo/sketch.ino"), &mut host).unwrap();
    assert_eq!(app2.doc.language().id(), "cpp");

    // 8. IPC `syntax-reset:all` clears custom overrides cleanly, and `syntax-set:rust` overrides language.
    let (reply, _, _) = app2.execute_ipc(
        &IpcCommand::Action("syntax-reset:all".into()),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(matches!(reply, IpcReply::Json(_)));
    assert!(app2.syntax_theme.light_overrides.is_empty());
    assert!(app2.syntax_theme.dark_overrides.is_empty());

    let (reply, _, _) = app2.execute_ipc(
        &IpcCommand::Action("syntax-set:rust".into()),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(matches!(reply, IpcReply::Json(_)));
    assert_eq!(app2.doc.language().id(), "rust");
}

#[test]
fn remembers_last_opened_folder_across_restarts_and_handles_missing_dir() {
    use editor_platform::ipc::IpcCommand;
    use editor_platform::PlatformHost;
    use std::path::Path;

    let fs = Arc::new(MemoryFileSystem::new());
    fs.add_file("/workspace/project_a/main.rs", b"fn main() {}\n".to_vec());
    fs.add_file("/workspace/project_b/sub/lib.rs", b"pub fn x() {}\n".to_vec());

    let mut host = HeadlessHost::new();
    let mut raster = BuiltinBitmapRasterizer;
    let mut clip = MemoryClipboard::new();

    // 1. Opening a folder persists `LastFolder` in host preferences.
    let mut app1 = EditorApp::new(fs.clone(), 900, 600, 2.0);
    app1.open_path_or_dir(&PathBuf::from("/workspace/project_a"), &mut host)
        .unwrap();
    assert_eq!(app1.sidebar.root(), Some(Path::new("/workspace/project_a")));
    assert_eq!(
        host.load_pref("LastFolder").as_deref(),
        Some("/workspace/project_a")
    );

    // 2. Switching folder via IPC `OpenFolder` updates `LastFolder` immediately.
    let _ = app1.execute_ipc(
        &IpcCommand::OpenFolder(PathBuf::from("/workspace/project_b/sub")),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(
        host.load_pref("LastFolder").as_deref(),
        Some("/workspace/project_b/sub")
    );

    // 3. Launching a fresh EditorApp restores the exact last opened folder cleanly!
    let mut app2 = EditorApp::new(fs.clone(), 1400, 900, 2.0);
    assert_eq!(app2.sidebar.root(), None);
    app2.load_preferences(&host);
    assert_eq!(
        app2.sidebar.root(),
        Some(Path::new("/workspace/project_b/sub"))
    );
    assert!(app2.sidebar.is_visible());

    // 4. If the persisted folder was deleted before next launch, startup degrades gracefully!
    host.save_pref("LastFolder", "/workspace/deleted_folder");
    let mut app3 = EditorApp::new(fs, 900, 600, 2.0);
    app3.load_preferences(&host);
    assert_eq!(app3.sidebar.root(), None);
}

#[test]
fn sidebar_hover_exit_and_caret_blink_never_desync_swapchain() {
    use editor_platform::{InputEvent, Modifiers, PixelRect, SurfaceMut};

    let fs = Arc::new(MemoryFileSystem::new());
    fs.add_file("/repo/alpha.rs", b"fn alpha() { let x = 1; }\n".to_vec());
    fs.add_file("/repo/beta.rs", b"fn beta() { let y = 2; }\n".to_vec());

    let (w, h, scale) = (1400u32, 800u32, 2.0f32);
    let mut app = EditorApp::new(fs.clone(), w, h, scale);
    let mut raster = BuiltinBitmapRasterizer;
    let mut clip = MemoryClipboard::new();
    let mut host = HeadlessHost::new();

    app.open_path_or_dir(&PathBuf::from("/repo/alpha.rs"), &mut host)
        .unwrap();

    // Simulate a 2-slot double-buffered IOSurface swapchain identical to `mac_adapter.m`!
    let mut slots = [vec![0u32; (w * h) as usize], vec![0u32; (w * h) as usize]];
    let mut needs_full = [true, true];
    let mut dmg = [PixelRect::ZERO, PixelRect::ZERO];
    let mut front = 1usize;

    let mut present_if_dirty_and_verify =
        |app: &mut EditorApp, raster: &mut BuiltinBitmapRasterizer, step: &str| {
            if app.is_dirty_frame() {
                let back = 1 - front;
                let surf = SurfaceMut {
                    width: w,
                    height: h,
                    stride: w as usize,
                    scale,
                    pixels: &mut slots[back],
                };
                if let Some(painted) =
                    app.render_into_with_damage(surf, raster, dmg[back], needs_full[back])
                {
                    needs_full[back] = false;
                    dmg[back] = PixelRect::ZERO;
                    if !needs_full[front] {
                        dmg[front] = dmg[front].union(painted);
                    }
                    front = back;
                }
            }
            // The displayed front buffer MUST ALWAYS match a fresh full-redraw bit-for-bit!
            let mut ref_buf = vec![0u32; (w * h) as usize];
            let ref_surf = SurfaceMut {
                width: w,
                height: h,
                stride: w as usize,
                scale,
                pixels: &mut ref_buf,
            };
            app.render_into_with_damage(ref_surf, raster, PixelRect::ZERO, true)
                .expect("reference full render");
            assert_eq!(
                slots[front], ref_buf,
                "swapchain front buffer diverged from reference at step: {step}"
            );
        };

    // Frame 0: initial full paint.
    present_if_dirty_and_verify(&mut app, &mut raster, "initial");

    // 1. Hover over the second sidebar file (`beta.rs`).
    let sb = app.layout.sidebar;
    let row2_x = (sb.x + 60) as f32;
    let row2_y = (sb.y + (28.0 * scale) as i32 + (24.0 * scale) as i32 + 10) as f32;
    app.handle_event(
        InputEvent::MouseMove {
            x_px: row2_x,
            y_px: row2_y,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.is_dirty_frame(), "hovering sidebar row must mark dirty");
    present_if_dirty_and_verify(&mut app, &mut raster, "hover_sidebar_beta");

    // Moving 2px within the SAME row should NOT trigger redundant repaints!
    app.handle_event(
        InputEvent::MouseMove {
            x_px: row2_x + 2.0,
            y_px: row2_y,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(!app.is_dirty_frame(), "same-row mouse move must not repaint redundantly");

    // 2. Move across the sidebar resize edge directly into the editor pane!
    app.handle_event(
        InputEvent::MouseMove {
            x_px: sb.right() as f32,
            y_px: row2_y,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.is_dirty_frame(), "moving onto resize edge must clear sidebar hover & mark dirty");
    present_if_dirty_and_verify(&mut app, &mut raster, "hover_resize_edge");

    let vp = app.layout.viewport;
    app.handle_event(
        InputEvent::MouseMove {
            x_px: (vp.x + 120) as f32,
            y_px: (vp.y + 80) as f32,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    present_if_dirty_and_verify(&mut app, &mut raster, "enter_editor_pane");

    // 3. Re-hover sidebar directly, then jump straight into editor pane (skipping resize edge!),
    //    then blink the caret 6 times! Before the fix, this permanently ping-ponged the hovered row!
    app.handle_event(
        InputEvent::MouseMove {
            x_px: row2_x,
            y_px: row2_y,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    present_if_dirty_and_verify(&mut app, &mut raster, "rehover_sidebar_beta");

    app.handle_event(
        InputEvent::MouseMove {
            x_px: (vp.x + 160) as f32,
            y_px: (vp.y + 80) as f32,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.is_dirty_frame(), "jumping from sidebar to editor must clear hover & mark dirty");
    present_if_dirty_and_verify(&mut app, &mut raster, "jump_to_editor");

    for tick in 1..=6u64 {
        app.handle_event(
            InputEvent::Tick {
                now_ms: tick * 600,
            },
            &mut raster,
            &mut clip,
            &mut host,
        );
        present_if_dirty_and_verify(&mut app, &mut raster, &format!("caret_blink_{tick}"));
    }

    // 4. Hover tab strip, move directly to sidebar, then exit window (`-1000, -1000`).
    let ts = app.layout.tab_strip;
    app.handle_event(
        InputEvent::MouseMove {
            x_px: (ts.x + 40) as f32,
            y_px: (ts.y + ts.h / 2) as f32,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    present_if_dirty_and_verify(&mut app, &mut raster, "hover_tab_strip");

    app.handle_event(
        InputEvent::MouseMove {
            x_px: row2_x,
            y_px: row2_y,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    present_if_dirty_and_verify(&mut app, &mut raster, "tab_strip_to_sidebar");

    app.handle_event(
        InputEvent::MouseMove {
            x_px: -1000.0,
            y_px: -1000.0,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.is_dirty_frame(), "window mouseExited must clear sidebar hover");
    present_if_dirty_and_verify(&mut app, &mut raster, "mouse_exited_window");
}

#[test]
fn per_folder_session_restore_lru_tab_cap_and_cursor_memory() {
    use editor_platform::ipc::IpcCommand;
    use editor_platform::{InputEvent, Key, Modifiers};
    use std::path::Path;

    let fs = Arc::new(MemoryFileSystem::new());
    for i in 1..=10 {
        fs.add_file(
            &format!("/ws/proj_a/f{i}.rs"),
            format!("// file {i}\nline two of f{i}\nline three of f{i}\n").into_bytes(),
        );
    }
    fs.add_file("/ws/proj_b/app.ts", b"const x = 1;\nconst y = 2;\n".to_vec());
    fs.add_file("/ws/proj_b/util.ts", b"export const z = 3;\n".to_vec());

    let mut host = HeadlessHost::new();
    let mut raster = BuiltinBitmapRasterizer;
    let mut clip = MemoryClipboard::new();

    // 1. Open Project A, pin f1 & f2, move cursor in f3 to line 2, and open f1..f10 (default cap = 8)!
    let mut app1 = EditorApp::new(fs.clone(), 900, 600, 2.0);
    assert_eq!(app1.max_open_tabs, 8);
    app1.open_folder(Path::new("/ws/proj_a"), &mut host);

    let f1 = app1
        .open_file_with_mode(Path::new("/ws/proj_a/f1.rs"), editor_app::OpenMode::Keep, &mut host)
        .unwrap();
    app1.toggle_pin_buffer(f1, &mut host);
    let f2 = app1
        .open_file_with_mode(Path::new("/ws/proj_a/f2.rs"), editor_app::OpenMode::Keep, &mut host)
        .unwrap();
    app1.toggle_pin_buffer(f2, &mut host);

    // Open f3 and move caret onto line 2 (byte offset 15), then open f4..f10 -> f3 gets LRU-evicted
    // when 9th unpinned tab opens (wait: f3..f10 = 8 unpinned tabs! Let's add f11 to trigger eviction!).
    fs.add_file("/ws/proj_a/f11.rs", b"// file 11\n".to_vec());
    app1.open_file(Path::new("/ws/proj_a/f3.rs"), &mut host).unwrap();
    app1.caret = 15;
    app1.anchor = 15;

    // Make f4 dirty so it must NEVER be evicted even if oldest!
    app1.open_file(Path::new("/ws/proj_a/f4.rs"), &mut host).unwrap();
    app1.handle_event(
        InputEvent::KeyDown {
            key: Key::Char('x'),
            mods: Modifiers::NONE,
            text: Some("// dirty\n".into()),
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app1.doc.is_dirty());

    for i in 5..=11 {
        app1.open_file(&PathBuf::from(format!("/ws/proj_a/f{i}.rs")), &mut host)
            .unwrap();
    }
    // Unpinned clean `f3.rs` was oldest MRU and evicted to enforce cap=8, while pinned f1/f2 and dirty f4 stayed!
    assert_eq!(app1.working_set.pinned.len(), 2);
    assert_eq!(app1.working_set.open.len(), 8);
    assert!(app1.working_set.find_by_path(Path::new("/ws/proj_a/f3.rs")).is_none());
    assert!(app1.working_set.find_by_path(Path::new("/ws/proj_a/f4.rs")).is_some());

    // Re-opening LRU-evicted `f3.rs` restores its remembered cursor position (offset 15)!
    app1.open_file(Path::new("/ws/proj_a/f3.rs"), &mut host).unwrap();
    assert_eq!(app1.caret, 15, "evicted clean file should recall cursor position on reopen");

    // Save dirty f4 so we can switch cleanly between Project A and Project B.
    let f4_id = app1.working_set.find_by_path(Path::new("/ws/proj_a/f4.rs")).unwrap();
    app1.activate_buffer(f4_id, &mut host);
    app1.doc.save(fs.as_ref(), None, false).unwrap();
    app1.activate_buffer(f1, &mut host);
    app1.caret = 12; // line 1, col 2
    app1.anchor = 12;
    app1.save_session(&mut host);

    // 2. Switch to Project B, pin `app.ts`, open `util.ts`, then switch BACK to Project A!
    app1.open_folder(Path::new("/ws/proj_b"), &mut host);
    let b_app = app1
        .open_file_with_mode(Path::new("/ws/proj_b/app.ts"), editor_app::OpenMode::Keep, &mut host)
        .unwrap();
    app1.toggle_pin_buffer(b_app, &mut host);
    app1.open_file(Path::new("/ws/proj_b/util.ts"), &mut host).unwrap();

    app1.open_folder(Path::new("/ws/proj_a"), &mut host);
    assert_eq!(app1.working_set.pinned.len(), 2, "switching back to Project A restores its pinned files");
    assert_eq!(app1.doc.path(), Some(Path::new("/ws/proj_a/f1.rs")));

    // 3. Simulate full app restart (`EditorApp::new` + `load_preferences`):
    //    restores LastFolder (Project A), pins f1/f2 in exact order, open tabs, active file & cursor!
    let mut app2 = EditorApp::new(fs.clone(), 900, 600, 2.0);
    app2.load_preferences(&host);
    assert_eq!(app2.sidebar.root(), Some(Path::new("/ws/proj_a")));
    assert_eq!(app2.working_set.pinned.len(), 2);
    assert_eq!(app2.doc.path(), Some(Path::new("/ws/proj_a/f1.rs")));
    assert_eq!(app2.doc.byte_to_line_col(app2.caret), (1, 2));

    // 4. Configurable max-tabs via IPC (`max-tabs:4`) immediately trims clean unpinned open tabs to 4!
    let _ = app2.execute_ipc(
        &IpcCommand::Action("max-tabs:4".into()),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app2.max_open_tabs, 4);
    assert!(app2.working_set.open.len() <= 4);
    assert_eq!(app2.working_set.pinned.len(), 2, "lowering max-tabs must never evict pins");
}

#[test]
fn close_window_respects_dirty_guard_saves_session_and_uses_os_host_policy() {
    use editor_platform::{AppCommand, InputEvent, Key, Modifiers};
    use std::path::Path;

    let fs = Arc::new(MemoryFileSystem::new());
    fs.add_file("/ws/a.rs", b"fn main() {}\n".to_vec());

    // 1. macOS policy (`stay_alive_on_close = true`): CloseWindow hides window WITHOUT quitting,
    //    while checking dirty buffers first and saving session!
    let mut mac_host = HeadlessHost::new();
    mac_host.stay_alive_on_close = true;
    let mut raster = BuiltinBitmapRasterizer;
    let mut clip = MemoryClipboard::new();

    let mut app = EditorApp::new(fs.clone(), 900, 600, 2.0);
    app.use_in_app_modal = true;
    app.open_file(Path::new("/ws/a.rs"), &mut mac_host).unwrap();

    // Make dirty -> CloseWindow MUST prompt instead of closing immediately!
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Char('a'),
            mods: Modifiers::NONE,
            text: Some("// edit\n".into()),
        },
        &mut raster,
        &mut clip,
        &mut mac_host,
    );
    app.handle_event(
        InputEvent::Command(AppCommand::CloseWindow),
        &mut raster,
        &mut clip,
        &mut mac_host,
    );
    assert!(app.confirm_modal.visible, "CloseWindow must guard dirty buffers");
    assert!(!mac_host.close_window_requested);
    assert!(!mac_host.quit_requested);

    // Discard -> closes window while keeping app resident in Dock!
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Char('d'),
            mods: Modifiers::NONE,
            text: Some("d".into()),
        },
        &mut raster,
        &mut clip,
        &mut mac_host,
    );
    assert!(!app.confirm_modal.visible);
    assert!(mac_host.close_window_requested);
    assert!(!mac_host.quit_requested, "macOS CloseWindow must stay alive in Dock");

    // Explicit Cmd+Q (`Quit`) terminates the process!
    app.handle_event(
        InputEvent::Command(AppCommand::Quit),
        &mut raster,
        &mut clip,
        &mut mac_host,
    );
    assert!(mac_host.quit_requested);

    // 2. Default Linux/Windows policy (`stay_alive_on_close = false`):
    //    Final Cmd+W on a clean empty Untitled buffer closes the window AND quits the app!
    let mut win_linux_host = HeadlessHost::new();
    let mut app2 = EditorApp::new(fs, 900, 600, 2.0);
    app2.handle_event(
        InputEvent::Command(AppCommand::CloseTab),
        &mut raster,
        &mut clip,
        &mut win_linux_host,
    );
    assert!(win_linux_host.close_window_requested);
    assert!(
        win_linux_host.quit_requested,
        "non-macOS hosts default to quitting when closing the last window"
    );
}

#[test]
fn sidebar_moves_left_right_minimizes_and_auto_minimizes_on_narrow_window() {
    use editor_platform::ipc::IpcCommand;
    use editor_platform::{AppCommand, InputEvent, Modifiers, MouseButton};
    use editor_render::SidebarSide;
    use std::path::Path;

    let fs = Arc::new(MemoryFileSystem::new());
    fs.add_file("/repo/main.rs", b"fn main() {}\n".to_vec());

    // Start wide (`1000x600` pt @ 2x -> `2000x1200` px).
    let mut app = EditorApp::new(fs.clone(), 2000, 1200, 2.0);
    let mut raster = BuiltinBitmapRasterizer;
    let mut clip = MemoryClipboard::new();
    let mut host = HeadlessHost::new();

    app.open_file(Path::new("/repo/main.rs"), &mut host).unwrap();
    app.render(&mut raster);
    assert!(app.sidebar.is_visible());
    assert_eq!(app.sidebar.side(), SidebarSide::Left);
    assert_eq!(app.layout.sidebar.x, 0);
    assert!(app.layout.viewport.x >= app.layout.sidebar.right());

    // 1. Move sidebar to Right side via `ToggleSidebarSide` (`⌥⌘B`) -> persists `SidebarSide = right`!
    app.handle_event(
        InputEvent::Command(AppCommand::ToggleSidebarSide),
        &mut raster,
        &mut clip,
        &mut host,
    );
    app.render(&mut raster);
    assert_eq!(app.sidebar.side(), SidebarSide::Right);
    assert_eq!(app.layout.viewport.x, 0);
    assert_eq!(app.layout.sidebar.right(), 2000);
    assert!(app.layout.sidebar.x > 0);

    // Dragging the LEFT edge of a right-docked sidebar resizes width cleanly!
    let edge_x = app.layout.sidebar.x as f32;
    let edge_y = (app.layout.sidebar.y + 120) as f32;
    app.handle_event(
        InputEvent::MouseDown {
            x_px: edge_x,
            y_px: edge_y,
            button: MouseButton::Left,
            click_count: 1,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    app.handle_event(
        InputEvent::MouseMove {
            x_px: 2000.0 - 300.0 * 2.0,
            y_px: edge_y,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    app.handle_event(
        InputEvent::MouseUp {
            x_px: 2000.0 - 300.0 * 2.0,
            y_px: edge_y,
            button: MouseButton::Left,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!((app.sidebar.width_pt() - 300.0).abs() <= 1.0);

    // 2. Shrink window below 640 pt (`560x400` pt @ 2x -> `1120x800` px) -> auto-minimizes cleanly!
    app.handle_event(
        InputEvent::Resize {
            width_px: 1120,
            height_px: 800,
            scale: 2.0,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    app.render(&mut raster);
    assert!(!app.sidebar.is_visible(), "narrow window must auto-minimize sidebar");
    assert!(app.is_sidebar_auto_minimized());
    assert_eq!(app.layout.sidebar.w, 0);
    assert_eq!(app.layout.tab_strip.w, 1120);

    // Clicking the expand icon in TabStrip (right edge since docked Right!) overrides narrow auto-minimize!
    let sb_btn_x = (app.layout.tab_strip.right() - 24) as f32;
    let sb_btn_y = (app.layout.tab_strip.y + app.layout.tab_strip.h / 2) as f32;
    app.handle_event(
        InputEvent::MouseDown {
            x_px: sb_btn_x,
            y_px: sb_btn_y,
            button: MouseButton::Left,
            click_count: 1,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.sidebar.is_visible(), "explicit expand click while narrow must open sidebar");

    // 3. Widen back to `1000pt`, then shrink again -> auto-minimizes again; widen back -> auto-restores!
    app.handle_event(
        InputEvent::Resize {
            width_px: 2000,
            height_px: 1200,
            scale: 2.0,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.sidebar.is_visible());
    assert!(!app.is_sidebar_auto_minimized());

    app.handle_event(
        InputEvent::Resize {
            width_px: 1120,
            height_px: 800,
            scale: 2.0,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(!app.sidebar.is_visible());
    assert!(app.is_sidebar_auto_minimized());

    app.handle_event(
        InputEvent::Resize {
            width_px: 2000,
            height_px: 1200,
            scale: 2.0,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.sidebar.is_visible(), "widening back must auto-restore sidebar");

    // 4. Manual minimize (`⌘B`) stays minimized even when resizing narrow -> wide!
    app.handle_event(
        InputEvent::Command(AppCommand::ToggleSidebar),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(!app.sidebar.is_visible());
    app.handle_event(
        InputEvent::Resize {
            width_px: 1120,
            height_px: 800,
            scale: 2.0,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    app.handle_event(
        InputEvent::Resize {
            width_px: 2000,
            height_px: 1200,
            scale: 2.0,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(
        !app.sidebar.is_visible(),
        "manual minimize must stay respected after window resize"
    );

    // 5. Preference persistence restores `SidebarSide::Right`, and IPC `sidebar-left` switches back.
    let mut app2 = EditorApp::new(fs, 2000, 1200, 2.0);
    app2.load_preferences(&host);
    assert_eq!(app2.sidebar.side(), SidebarSide::Right);
    let _ = app2.execute_ipc(
        &IpcCommand::Action("sidebar-left".into()),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app2.sidebar.side(), SidebarSide::Left);
}

#[test]
fn pane_dividers_and_sidebar_resize_handles_double_click_reset_and_ipc() {
    use editor_platform::ipc::{IpcCommand, IpcReply};
    use editor_platform::{CursorShape, InputEvent, Modifiers, MouseButton};
    use std::path::Path;

    let fs = Arc::new(MemoryFileSystem::new());
    fs.add_file("/repo/a.rs", b"fn a() {}\n".to_vec());
    fs.add_file("/repo/b.rs", b"fn b() {}\n".to_vec());
    fs.add_file("/repo/c.rs", b"fn c() {}\n".to_vec());

    let mut app = EditorApp::new(fs.clone(), 2000, 1200, 2.0);
    let mut raster = BuiltinBitmapRasterizer;
    let mut clip = MemoryClipboard::new();
    let mut host = HeadlessHost::new();

    app.open_file(Path::new("/repo/a.rs"), &mut host).unwrap();
    app.split_pane_with(Some(Path::new("/repo/b.rs")), &mut host);
    app.split_pane_down(Some(Path::new("/repo/c.rs")), &mut host);
    app.render(&mut raster);
    assert_eq!(app.layout.grid.dividers().len(), 2);

    // 1. Hovering the column divider sets ResizeHorizontal cursor and marks dirty for handle highlight!
    let col_div = app
        .layout
        .grid
        .dividers()
        .iter()
        .copied()
        .find(|d| d.axis == editor_app::DividerAxis::Columns)
        .expect("column divider");
    let dx = col_div.rect.x as f32;
    let dy = (col_div.rect.y + col_div.rect.h / 2) as f32;
    app.handle_event(
        InputEvent::MouseMove {
            x_px: dx,
            y_px: dy,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(host.cursor, Some(CursorShape::ResizeHorizontal));
    assert!(app.is_dirty_frame());
    app.render(&mut raster);

    // Drag column divider to ~30% and release!
    let target_x = col_div.span.x as f32 + col_div.span.w as f32 * 0.30;
    app.handle_event(
        InputEvent::MouseDown {
            x_px: dx,
            y_px: dy,
            button: MouseButton::Left,
            click_count: 1,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    app.handle_event(
        InputEvent::MouseMove {
            x_px: target_x,
            y_px: dy,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    app.handle_event(
        InputEvent::MouseUp {
            x_px: target_x,
            y_px: dy,
            button: MouseButton::Left,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!((app.arrangement.col_ratio - 0.30).abs() < 0.02);

    // Double-clicking the column divider resets it cleanly back to 50/50!
    let new_col_div = app
        .layout
        .grid
        .dividers()
        .iter()
        .copied()
        .find(|d| d.axis == editor_app::DividerAxis::Columns)
        .unwrap();
    app.handle_event(
        InputEvent::MouseDown {
            x_px: new_col_div.rect.x as f32,
            y_px: dy,
            button: MouseButton::Left,
            click_count: 2,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!((app.arrangement.col_ratio - 0.50).abs() < 0.001);
    assert_eq!(host.cursor, Some(CursorShape::Arrow));

    // Also verify horizontal (row) divider drag + double-click reset back to 50/50!
    let row_div = app
        .layout
        .grid
        .dividers()
        .iter()
        .copied()
        .find(|d| matches!(d.axis, editor_app::DividerAxis::Rows(_)))
        .expect("row divider");
    let rx = (row_div.rect.x + row_div.rect.w / 2) as f32;
    let ry = row_div.rect.y as f32;
    let target_y = row_div.span.y as f32 + row_div.span.h as f32 * 0.72;
    app.handle_event(
        InputEvent::MouseDown {
            x_px: rx,
            y_px: ry,
            button: MouseButton::Left,
            click_count: 1,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    app.handle_event(
        InputEvent::MouseMove {
            x_px: rx,
            y_px: target_y,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    app.handle_event(
        InputEvent::MouseUp {
            x_px: rx,
            y_px: target_y,
            button: MouseButton::Left,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    let row_col = match row_div.axis {
        editor_app::DividerAxis::Rows(c) => c as usize,
        _ => unreachable!(),
    };
    assert!((app.arrangement.row_ratio[row_col] - 0.72).abs() < 0.02);
    let new_row_div = app
        .layout
        .grid
        .dividers()
        .iter()
        .copied()
        .find(|d| d.axis == row_div.axis)
        .unwrap();
    app.handle_event(
        InputEvent::MouseDown {
            x_px: rx,
            y_px: new_row_div.rect.y as f32,
            button: MouseButton::Left,
            click_count: 2,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!((app.arrangement.row_ratio[row_col] - 0.50).abs() < 0.001);
    assert_eq!(host.cursor, Some(CursorShape::Arrow));

    // 2. Agent/IPC resize commands for sidebar width & pane split ratios + status_json inspection!
    let (reply, _, _) = app.execute_ipc(
        &IpcCommand::Action("sidebar-width:340".into()),
        &mut raster,
        &mut clip,
        &mut host,
    );
    match reply {
        IpcReply::Json(j) => assert!(j.contains("\"width_pt\":340")),
        other => panic!("unexpected reply: {other:?}"),
    }
    assert!((app.sidebar.width_pt() - 340.0).abs() < 0.5);

    // Double-clicking sidebar edge resets width back to 220 pt and restores arrow cursor!
    app.handle_event(
        InputEvent::MouseDown {
            x_px: app.layout.sidebar.right() as f32,
            y_px: 200.0,
            button: MouseButton::Left,
            click_count: 2,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!((app.sidebar.width_pt() - 220.0).abs() < 0.5);
    assert_eq!(host.cursor, Some(CursorShape::Arrow));

    // Clicking the sidebar header's single minimize icon hides the sidebar cleanly.
    app.render(&mut raster);
    let min_x = (app.layout.sidebar.right() - 24) as f32;
    let min_y = (app.layout.sidebar.y + 30) as f32;
    app.handle_event(
        InputEvent::MouseDown {
            x_px: min_x,
            y_px: min_y,
            button: MouseButton::Left,
            click_count: 1,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(!app.sidebar.is_visible(), "clicking header minimize icon hides sidebar");
    app.toggle_sidebar(&mut host);
    assert!(app.sidebar.is_visible());

    let (reply, _, _) = app.execute_ipc(
        &IpcCommand::Action("pane-ratio:cols=35%".into()),
        &mut raster,
        &mut clip,
        &mut host,
    );
    match reply {
        IpcReply::Json(j) => assert!(j.contains("\"col\":0.350")),
        other => panic!("unexpected reply: {other:?}"),
    }
    let _ = app.execute_ipc(
        &IpcCommand::Action("pane-ratio:rows:1=0.7".into()),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!((app.arrangement.row_ratio[1] - 0.70).abs() < 0.001);

    let _ = app.execute_ipc(
        &IpcCommand::Action("pane-ratio:reset".into()),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!((app.arrangement.col_ratio - 0.5).abs() < 0.001);
    assert!((app.arrangement.row_ratio[1] - 0.5).abs() < 0.001);
}

#[test]
fn command_palette_fuzzy_searches_and_runs_keymap_and_extra_commands() {
    use editor_platform::{InputEvent, Key, Modifiers};
    use editor_render::{PaletteMode, SidebarSide};

    let fs = Arc::new(MemoryFileSystem::new());
    let mut app = EditorApp::new(fs, 1400, 900, 2.0);
    let mut raster = BuiltinBitmapRasterizer;
    let mut clip = MemoryClipboard::new();
    let mut host = HeadlessHost::new();

    // Open Command Palette via `⇧⌘P` (or `Ctrl+Shift+P` on non-macOS).
    let mut open_mods = Modifiers::NONE;
    #[cfg(target_os = "macos")]
    {
        open_mods.meta = true;
    }
    #[cfg(not(target_os = "macos"))]
    {
        open_mods.ctrl = true;
    }
    open_mods.shift = true;
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Char('p'),
            mods: open_mods,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.palette.mode, PaletteMode::Commands);
    assert!(app.palette.items.len() >= 30);
    app.render(&mut raster);

    // Filter for "monochrome" and hit Enter -> switches syntax preset to Monochrome!
    for ch in "monochrome".chars() {
        app.handle_event(
            InputEvent::KeyDown {
                key: Key::Char(ch),
                mods: Modifiers::NONE,
                text: Some(ch.to_string()),
            },
            &mut raster,
            &mut clip,
            &mut host,
        );
    }
    assert!(!app.palette.items.is_empty());
    assert!(app.palette.items[0].primary.to_ascii_lowercase().contains("monochrome"));
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Enter,
            mods: Modifiers::NONE,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.palette.mode, PaletteMode::Closed);
    assert_eq!(app.syntax_theme.preset.as_str(), "monochrome");

    // Reopen Command Palette via IPC, run "Move sidebar to right", and verify!
    let _ = app.execute_ipc(
        &editor_platform::ipc::IpcCommand::Action("command-palette".into()),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.palette.mode, PaletteMode::Commands);
    for ch in "sidebar to right".chars() {
        app.handle_event(
            InputEvent::KeyDown {
                key: Key::Char(ch),
                mods: Modifiers::NONE,
                text: Some(ch.to_string()),
            },
            &mut raster,
            &mut clip,
            &mut host,
        );
    }
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Enter,
            mods: Modifiers::NONE,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.sidebar.side(), SidebarSide::Right);
}

#[test]
fn auto_pairs_skip_over_wrap_paired_backspace_and_smart_enter() {
    use editor_platform::{InputEvent, Key, Modifiers};

    let fs = Arc::new(MemoryFileSystem::new());
    let mut app = EditorApp::new(fs, 1200, 800, 2.0);
    let mut raster = BuiltinBitmapRasterizer;
    let mut clip = MemoryClipboard::new();
    let mut host = HeadlessHost::new();

    let type_char = |app: &mut EditorApp,
                     ch: char,
                     raster: &mut BuiltinBitmapRasterizer,
                     clip: &mut MemoryClipboard,
                     host: &mut HeadlessHost| {
        app.handle_event(
            InputEvent::KeyDown {
                key: Key::Char(ch),
                mods: Modifiers::NONE,
                text: Some(ch.to_string()),
            },
            raster,
            clip,
            host,
        );
    };

    let doc_text = |app: &mut EditorApp| -> String {
        let len = app.doc.len_bytes() as usize;
        let mut buf = Vec::with_capacity(len);
        app.doc.tree_mut().read_range(0, len, &mut buf);
        String::from_utf8(buf).unwrap()
    };

    // 1. Auto-close `{` and smart Enter between `{|}` -> indented body line + closing brace!
    for ch in "fn f() {".chars() {
        type_char(&mut app, ch, &mut raster, &mut clip, &mut host);
    }
    assert_eq!(doc_text(&mut app), "fn f() {}");
    assert_eq!(app.caret, 8);

    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Enter,
            mods: Modifiers::NONE,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(doc_text(&mut app), "fn f() {\n    \n}");
    assert_eq!(app.doc.byte_to_line_col(app.caret), (1, 4));

    // 2. Skip-over closing delimiter + paired Backspace.
    type_char(&mut app, '(', &mut raster, &mut clip, &mut host);
    assert_eq!(doc_text(&mut app), "fn f() {\n    ()\n}");
    // Backspace deletes both `(` and `)`!
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Backspace,
            mods: Modifiers::NONE,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(doc_text(&mut app), "fn f() {\n    \n}");

    // Type `("ok")` -> typing closing `"` and `)` skips over without doubling!
    for ch in "(\"ok\")".chars() {
        type_char(&mut app, ch, &mut raster, &mut clip, &mut host);
    }
    assert_eq!(doc_text(&mut app), "fn f() {\n    (\"ok\")\n}");

    // 3. Wrapping an active selection with `[` keeps inner text selected!
    let pos = doc_text(&mut app).find("ok").unwrap() as u64;
    app.anchor = pos;
    app.caret = pos + 2;
    type_char(&mut app, '[', &mut raster, &mut clip, &mut host);
    assert_eq!(doc_text(&mut app), "fn f() {\n    (\"[ok]\")\n}");
    assert_eq!(app.anchor, pos + 1);
    assert_eq!(app.caret, pos + 3);

    // 4. Contractions (`don't`) do not insert stray closing apostrophes.
    app.doc = editor_core::Document::new_untitled();
    app.caret = 0;
    app.anchor = 0;
    for ch in "don't".chars() {
        type_char(&mut app, ch, &mut raster, &mut clip, &mut host);
    }
    assert_eq!(doc_text(&mut app), "don't");
}

#[test]
fn sidebar_file_operations_create_rename_delete_and_context_menu() {
    use editor_platform::{FileSystem, InputEvent, Key, Modifiers, MouseButton};
    use std::path::Path;

    let fs = Arc::new(MemoryFileSystem::new());
    fs.add_file("/proj/main.rs", b"fn main() {}\n".to_vec());

    let mut app = EditorApp::new(fs.clone(), 1400, 900, 2.0);
    let mut raster = BuiltinBitmapRasterizer;
    let mut clip = MemoryClipboard::new();
    let mut host = HeadlessHost::new();

    app.open_folder(Path::new("/proj"), &mut host);
    app.open_file(Path::new("/proj/main.rs"), &mut host).unwrap();
    app.render(&mut raster);

    // 1. Create a new file via inline prompt (`sidebar-new-file`) -> creates on disk, refreshes lazily & opens!
    let _ = app.execute_ipc(
        &editor_platform::ipc::IpcCommand::Action("sidebar-new-file".into()),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.sidebar.is_prompt_open());
    app.render(&mut raster);
    for ch in "notes.md".chars() {
        app.handle_event(
            InputEvent::KeyDown {
                key: Key::Char(ch),
                mods: Modifiers::NONE,
                text: Some(ch.to_string()),
            },
            &mut raster,
            &mut clip,
            &mut host,
        );
    }
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Enter,
            mods: Modifiers::NONE,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(!app.sidebar.is_prompt_open());
    assert!(fs.stat(Path::new("/proj/notes.md")).is_ok());
    assert_eq!(app.doc.path(), Some(Path::new("/proj/notes.md")));

    // 2. Rename selected file (`notes.md` -> `readme.md`):
    //    - In-place at the row's exact Y (NOT in a disconnected top strip!)
    //    - Smart stem pre-selection selects only `"notes"`, leaving `.md` intact!
    let _ = app.execute_ipc(
        &editor_platform::ipc::IpcCommand::Action("sidebar-rename".into()),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.sidebar.is_prompt_open());
    app.render(&mut raster);
    let in_rect = app.sidebar.prompt_input_rect();
    assert!(!in_rect.is_empty(), "in-place rename input rect must be populated");
    // Row 1 (`notes.md`, after `main.rs`) sits below the 30pt header + row 0 (24pt) -> y >= 100px @ 2x!
    assert!(
        in_rect.y >= 100,
        "in-place rename must render at the target row's position, not the top of the sidebar (got y={})",
        in_rect.y
    );
    // Because `"notes"` is pre-selected (leaving `.md` untouched), typing `"readme"` produces `"readme.md"`!
    for ch in "readme".chars() {
        app.handle_event(
            InputEvent::KeyDown {
                key: Key::Char(ch),
                mods: Modifiers::NONE,
                text: Some(ch.to_string()),
            },
            &mut raster,
            &mut clip,
            &mut host,
        );
    }
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Enter,
            mods: Modifiers::NONE,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(fs.stat(Path::new("/proj/notes.md")).is_err());
    assert!(fs.stat(Path::new("/proj/readme.md")).is_ok());
    assert_eq!(app.doc.path(), Some(Path::new("/proj/readme.md")));

    // 3. Create a new folder via inline prompt (`sidebar-new-folder`).
    let _ = app.execute_ipc(
        &editor_platform::ipc::IpcCommand::Action("sidebar-new-folder".into()),
        &mut raster,
        &mut clip,
        &mut host,
    );
    for ch in "docs".chars() {
        app.handle_event(
            InputEvent::KeyDown {
                key: Key::Char(ch),
                mods: Modifiers::NONE,
                text: Some(ch.to_string()),
            },
            &mut raster,
            &mut clip,
            &mut host,
        );
    }
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Enter,
            mods: Modifiers::NONE,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(fs.stat(Path::new("/proj/docs")).map(|s| s.is_dir).unwrap_or(false));

    // 4. Right-click inside sidebar opens custom context menu; Copy Path & Copy Relative Path
    //    copy the clicked item's paths to the clipboard, and Escape dismisses cleanly.
    //    Row 0 is `docs/` (y = 60..108 px), Row 1 is `main.rs` (y = 108..156 px @ 2x).
    app.handle_event(
        InputEvent::MouseDown {
            x_px: 80.0,
            y_px: 120.0,
            button: MouseButton::Right,
            click_count: 1,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.context_menu.is_open());
    app.render(&mut raster);
    // Click "Reveal in Finder" (first item after separator).
    let expected_reveal = editor_platform::PlatformHost::reveal_in_file_manager_label(&host);
    let reveal_id = app
        .context_menu
        .entries()
        .iter()
        .find_map(|e| match e {
            editor_render::ContextMenuEntry::Item { id, label, .. }
                if label == expected_reveal =>
            {
                Some(*id)
            }
            _ => None,
        })
        .expect("sidebar context menu must include OS reveal item");
    let reveal_rect = app.context_menu.item_rect(reveal_id).unwrap();
    app.handle_event(
        InputEvent::MouseDown {
            x_px: (reveal_rect.x + reveal_rect.w / 2) as f32,
            y_px: (reveal_rect.y + reveal_rect.h / 2) as f32,
            button: MouseButton::Left,
            click_count: 1,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(!app.context_menu.is_open());
    assert_eq!(host.last_revealed_path.as_deref(), Some(Path::new("/proj/main.rs")));

    // Right-click again and click "Copy Path".
    app.handle_event(
        InputEvent::MouseDown {
            x_px: 80.0,
            y_px: 120.0,
            button: MouseButton::Right,
            click_count: 1,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.context_menu.is_open());
    app.render(&mut raster);
    let copy_path_id = app
        .context_menu
        .entries()
        .iter()
        .find_map(|e| match e {
            editor_render::ContextMenuEntry::Item { id, label, .. } if label == "Copy Path" => {
                Some(*id)
            }
            _ => None,
        })
        .expect("Copy Path entry");
    let copy_rect = app.context_menu.item_rect(copy_path_id).unwrap();
    app.handle_event(
        InputEvent::MouseDown {
            x_px: (copy_rect.x + copy_rect.w / 2) as f32,
            y_px: (copy_rect.y + copy_rect.h / 2) as f32,
            button: MouseButton::Left,
            click_count: 1,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(!app.context_menu.is_open());
    assert_eq!(clip.get_text().as_deref(), Some("/proj/main.rs"));

    // Right-click again and click "Copy Relative Path".
    app.handle_event(
        InputEvent::MouseDown {
            x_px: 80.0,
            y_px: 120.0,
            button: MouseButton::Right,
            click_count: 1,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    app.render(&mut raster);
    let copy_rel_id = app
        .context_menu
        .entries()
        .iter()
        .find_map(|e| match e {
            editor_render::ContextMenuEntry::Item { id, label, .. }
                if label == "Copy Relative Path" =>
            {
                Some(*id)
            }
            _ => None,
        })
        .expect("Copy Relative Path entry");
    let rel_rect = app.context_menu.item_rect(copy_rel_id).unwrap();
    app.handle_event(
        InputEvent::MouseDown {
            x_px: (rel_rect.x + rel_rect.w / 2) as f32,
            y_px: (rel_rect.y + rel_rect.h / 2) as f32,
            button: MouseButton::Left,
            click_count: 1,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(clip.get_text().as_deref(), Some("main.rs"));

    // Right-click again; MouseUp, Tick, and MouseMove hover must keep it open cleanly.
    app.handle_event(
        InputEvent::MouseDown {
            x_px: 80.0,
            y_px: 100.0,
            button: MouseButton::Right,
            click_count: 1,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.context_menu.is_open());
    app.render(&mut raster);
    app.handle_event(
        InputEvent::MouseUp {
            x_px: 80.0,
            y_px: 100.0,
            button: MouseButton::Right,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    app.handle_event(
        InputEvent::Tick { now_ms: 10_000 },
        &mut raster,
        &mut clip,
        &mut host,
    );
    app.handle_event(
        InputEvent::MouseMove {
            x_px: 120.0,
            y_px: 120.0,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(
        app.context_menu.is_open(),
        "context menu must survive MouseUp, Tick, and MouseMove hover"
    );
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Escape,
            mods: Modifiers::NONE,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(!app.context_menu.is_open());

    // 5. Delete selected folder (`docs`) goes through ConfirmModal -> Enter confirms deletion!
    let _ = app.execute_ipc(
        &editor_platform::ipc::IpcCommand::Action("sidebar-delete".into()),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.confirm_modal.visible);
    assert!(app.confirm_modal.is_delete);
    app.render(&mut raster);
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Enter,
            mods: Modifiers::NONE,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(!app.confirm_modal.visible);
    assert!(fs.stat(Path::new("/proj/docs")).is_err());
}

#[test]
fn multi_cursor_cmd_d_alt_click_atomic_edit_paste_and_undo() {
    use editor_platform::{AppCommand, InputEvent, Key, Modifiers, MouseButton};
    use std::path::Path;

    let fs = Arc::new(MemoryFileSystem::new());
    fs.add_file(
        "/proj/mc.rs",
        b"let foo = 1;\nlet bar = foo + foo;\n".to_vec(),
    );

    let mut app = EditorApp::new(fs, 1400, 900, 2.0);
    let mut raster = BuiltinBitmapRasterizer;
    let mut clip = MemoryClipboard::new();
    let mut host = HeadlessHost::new();

    app.open_file(Path::new("/proj/mc.rs"), &mut host).unwrap();
    app.caret = 4; // inside first `foo`
    app.anchor = 4;

    // 1. First `⌘D` selects `foo`, second & third `⌘D` add the next two occurrences!
    for _ in 0..3 {
        app.handle_event(
            InputEvent::Command(AppCommand::SelectNextOccurrence),
            &mut raster,
            &mut clip,
            &mut host,
        );
    }
    assert_eq!(app.extra_cursors.len(), 2, "should have 3 total cursors on `foo`");
    app.render(&mut raster);

    // 2. Typing `baz` replaces ALL 3 occurrences simultaneously in ONE atomic undo step!
    for ch in "baz".chars() {
        app.handle_event(
            InputEvent::KeyDown {
                key: Key::Char(ch),
                mods: Modifiers::NONE,
                text: Some(ch.to_string()),
            },
            &mut raster,
            &mut clip,
            &mut host,
        );
    }
    let read_all = |app: &mut EditorApp| -> String {
        let len = app.doc.len_bytes() as usize;
        let mut buf = Vec::with_capacity(len);
        app.doc.tree_mut().read_range(0, len, &mut buf);
        String::from_utf8(buf).unwrap()
    };
    assert_eq!(read_all(&mut app), "let baz = 1;\nlet bar = baz + baz;\n");

    // 3. Multi-cursor line-distributed paste: 3 clipboard lines -> 1 per cursor!
    clip.set_text("a\nb\nc");
    app.handle_event(
        InputEvent::Command(AppCommand::Paste),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(read_all(&mut app), "let baza = 1;\nlet bar = bazb + bazc;\n");

    // Single Undo reverts all 3 pasted edits atomically!
    app.handle_event(
        InputEvent::Command(AppCommand::Undo),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(read_all(&mut app), "let baz = 1;\nlet bar = baz + baz;\n");

    // 4. Multi-cursor Up/Down navigation moves every caret cleanly and NEVER inserts
    //    macOS AppKit PUA function-key codepoints (`\u{F700}` / `\u{F701}`) as text!
    let fs2 = Arc::new(MemoryFileSystem::new());
    fs2.add_file("/proj/lines.rs", b"aaaa\nbbbb\ncccc\ndddd\n".to_vec());
    let mut app2 = EditorApp::new(fs2, 1400, 900, 2.0);
    app2.open_file(Path::new("/proj/lines.rs"), &mut host).unwrap();
    // Place two carets: line 1 col 2 (`bb|bb`) and line 2 col 2 (`cc|cc`).
    app2.caret = 7;
    app2.anchor = 7;
    app2.extra_cursors.push((12, 12));
    // Press Down (simulating AppKit passing PUA `\u{F701}` in `text`!) -> moves to lines 2 & 3!
    app2.handle_event(
        InputEvent::KeyDown {
            key: Key::Down,
            mods: Modifiers::NONE,
            text: Some("\u{F701}".into()),
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(read_all(&mut app2), "aaaa\nbbbb\ncccc\ndddd\n");
    assert_eq!(app2.doc.byte_to_line_col(app2.caret), (2, 2));
    assert_eq!(app2.extra_cursors.len(), 1);
    assert_eq!(app2.doc.byte_to_line_col(app2.extra_cursors[0].1), (3, 2));
    // Press Up twice (simulating AppKit passing PUA `\u{F700}`) -> moves to lines 0 & 1!
    for _ in 0..2 {
        app2.handle_event(
            InputEvent::KeyDown {
                key: Key::Up,
                mods: Modifiers::NONE,
                text: Some("\u{F700}".into()),
            },
            &mut raster,
            &mut clip,
            &mut host,
        );
    }
    assert_eq!(read_all(&mut app2), "aaaa\nbbbb\ncccc\ndddd\n");
    assert_eq!(app2.doc.byte_to_line_col(app2.caret), (0, 2));
    assert_eq!(app2.doc.byte_to_line_col(app2.extra_cursors[0].1), (1, 2));

    // 5. Alt-click adds an extra cursor, Escape collapses back to single cursor!
    app.render(&mut raster);
    let mut alt = Modifiers::NONE;
    alt.alt = true;
    app.handle_event(
        InputEvent::MouseDown {
            x_px: (app.layout.viewport.x + 120) as f32,
            y_px: (app.layout.viewport.y + 8) as f32,
            button: MouseButton::Left,
            click_count: 1,
            mods: alt,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.extra_cursors.len(), 1);
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Escape,
            mods: Modifiers::NONE,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.extra_cursors.is_empty());
}

#[test]
fn live_fs_watcher_reloads_clean_buffers_preserves_dirty_and_stays_lazy() {
    use editor_platform::{InputEvent, Key, Modifiers};
    use std::path::{Path, PathBuf};

    let fs = Arc::new(MemoryFileSystem::new());
    fs.add_file("/ws/clean.rs", b"fn v1() {}\n".to_vec());
    fs.add_file("/ws/dirty.rs", b"fn orig() {}\n".to_vec());
    fs.add_file("/ws/sub/deep.rs", b"fn deep() {}\n".to_vec());

    let mut app = EditorApp::new(fs.clone(), 1400, 900, 2.0);
    let mut raster = BuiltinBitmapRasterizer;
    let mut clip = MemoryClipboard::new();
    let mut host = HeadlessHost::new();

    // 1. Opening a folder registers it with the OS watcher via `PlatformHost::watch_directory`.
    app.open_folder(Path::new("/ws"), &mut host);
    assert_eq!(host.watched_dir.as_deref(), Some(Path::new("/ws")));

    // Open clean.rs, then modify it externally (simulating an agent CLI edit or git checkout)
    // and deliver a coalesced `InputEvent::FsChanged`.
    app.open_file(Path::new("/ws/clean.rs"), &mut host).unwrap();
    fs.add_file("/ws/clean.rs", b"fn v2_from_agent() {}\n".to_vec());
    fs.add_file("/ws/created_by_agent.rs", b"// new\n".to_vec());

    app.handle_event(
        InputEvent::FsChanged(vec![
            PathBuf::from("/ws/clean.rs"),
            PathBuf::from("/ws/created_by_agent.rs"),
        ]),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.doc.line_content(0).unwrap(), "fn v2_from_agent() {}");
    assert!(
        app.sidebar
            .rows()
            .iter()
            .any(|r| r.name == "created_by_agent.rs"),
        "sidebar root should refresh lazily and show newly created sibling file"
    );
    // Unexpanded subdirectory `/ws/sub` MUST remain collapsed & lazy!
    assert!(
        !app.sidebar.rows().iter().any(|r| r.name == "deep.rs"),
        "collapsed subdirectories must never be eagerly scanned by FS events"
    );

    // 2. Dirty buffer is NEVER overwritten by external FS changes!
    app.open_file(Path::new("/ws/dirty.rs"), &mut host).unwrap();
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Char('x'),
            mods: Modifiers::NONE,
            text: Some("// local unsaved\n".into()),
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.doc.is_dirty());
    fs.add_file("/ws/dirty.rs", b"fn overwritten_on_disk() {}\n".to_vec());
    app.handle_event(
        InputEvent::FsChanged(vec![PathBuf::from("/ws/dirty.rs")]),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.doc.line_content(0).unwrap(), "// local unsaved");
    assert!(app.doc.is_dirty());
}

#[test]
fn sidebar_drag_drop_move_large_folder_guard_scoped_undo_and_toast() {
    use editor_platform::{AppCommand, FileSystem, InputEvent, Key, Modifiers, MouseButton};
    use std::path::Path;

    let fs = Arc::new(MemoryFileSystem::new());
    fs.add_file("/proj/alpha.rs", b"fn alpha() {}\n".to_vec());
    fs.add_file("/proj/sub/keep.rs", b"fn keep() {}\n".to_vec());
    // Large folder (> 50 files) to verify pre-move confirmation guard!
    for i in 0..55 {
        fs.add_file(format!("/proj/big_dir/f{i}.txt"), b"x".to_vec());
    }
    fs.add_file("/proj/dest/placeholder.txt", b"ok".to_vec());

    let mut app = EditorApp::new(fs.clone(), 1400, 900, 2.0);
    let mut raster = BuiltinBitmapRasterizer;
    let mut clip = MemoryClipboard::new();
    let mut host = HeadlessHost::new();

    app.open_folder(Path::new("/proj"), &mut host);
    app.open_file(Path::new("/proj/alpha.rs"), &mut host).unwrap();
    app.render(&mut raster);

    // 1. Type an edit inside `alpha.rs` (document undo stack has 1 text edit).
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Char('a'),
            mods: Modifiers::NONE,
            text: Some("// edited\n".into()),
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.doc.line_content(0).unwrap(), "// edited");

    // 2. Drag `alpha.rs` onto `sub/` inside the sidebar!
    let alpha_idx = app
        .sidebar
        .rows()
        .iter()
        .position(|r| r.name == "alpha.rs")
        .unwrap();
    let sub_idx = app
        .sidebar
        .rows()
        .iter()
        .position(|r| r.name == "sub")
        .unwrap();
    let row_y = |idx: usize| -> f32 {
        // 30pt header + idx * 24pt + 12pt center, at 2x scale
        (30.0 + idx as f32 * 24.0 + 12.0) * 2.0
    };
    app.handle_event(
        InputEvent::MouseDown {
            x_px: 120.0,
            y_px: row_y(alpha_idx),
            button: MouseButton::Left,
            click_count: 1,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    app.handle_event(
        InputEvent::MouseMove {
            x_px: 120.0,
            y_px: row_y(sub_idx),
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.is_dragging());
    app.handle_event(
        InputEvent::MouseUp {
            x_px: 120.0,
            y_px: row_y(sub_idx),
            button: MouseButton::Left,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(fs.stat(Path::new("/proj/alpha.rs")).is_err());
    assert!(fs.stat(Path::new("/proj/sub/alpha.rs")).is_ok());
    assert_eq!(
        app.doc.path(),
        Some(Path::new("/proj/sub/alpha.rs")),
        "moving an open file updates its buffer path seamlessly"
    );
    assert!(app.toast.visible && app.toast.can_undo);
    assert!(app.toast.message.contains("Moved alpha.rs to sub"));
    app.render(&mut raster);

    // 3. Scoped Undo isolation:
    //    - While editor is focused, `AppCommand::Undo` undoes ONLY the text edit (file stays in `sub/`!).
    app.sidebar.set_focused(false);
    app.handle_event(
        InputEvent::Command(AppCommand::Undo),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.doc.line_content(0).unwrap(), "fn alpha() {}");
    assert!(
        fs.stat(Path::new("/proj/sub/alpha.rs")).is_ok(),
        "document undo must NEVER revert sidebar moves"
    );

    //    - While sidebar is focused, `AppCommand::Undo` undoes ONLY the sidebar move (doc stays untouched)!
    app.sidebar.set_focused(true);
    app.handle_event(
        InputEvent::Command(AppCommand::Undo),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(fs.stat(Path::new("/proj/alpha.rs")).is_ok());
    assert!(fs.stat(Path::new("/proj/sub/alpha.rs")).is_err());
    assert_eq!(app.doc.path(), Some(Path::new("/proj/alpha.rs")));

    //    - Sidebar Redo moves it right back into `sub/`, then clicking Undo on the toast reverts it!
    app.handle_event(
        InputEvent::Command(AppCommand::Redo),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(fs.stat(Path::new("/proj/sub/alpha.rs")).is_ok());
    assert!(app.toast.visible && app.toast.can_undo);
    app.render(&mut raster);
    // Click the toast's Undo button (even when document is focused!)
    app.sidebar.set_focused(false);
    let card = app.toast.card_rect;
    // Undo button sits just left of the 20pt close `×` button at right side of toast card.
    let undo_x = (card.right() - 78) as f32;
    let undo_y = (card.y + card.h / 2) as f32;
    app.handle_event(
        InputEvent::MouseDown {
            x_px: undo_x,
            y_px: undo_y,
            button: MouseButton::Left,
            click_count: 1,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(fs.stat(Path::new("/proj/alpha.rs")).is_ok());
    assert!(fs.stat(Path::new("/proj/sub/alpha.rs")).is_err());

    // 4. Large folder move guard (> 50 files in `big_dir`): warns via ConfirmModal before moving!
    let _ = app.execute_ipc(
        &editor_platform::ipc::IpcCommand::Action("sidebar-move:/proj/big_dir|/proj/dest".into()),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.confirm_modal.visible && app.confirm_modal.is_move_confirm);
    assert!(fs.stat(Path::new("/proj/big_dir")).is_ok(), "must not move before confirmation");
    app.render(&mut raster);
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Enter,
            mods: Modifiers::NONE,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(!app.confirm_modal.visible);
    assert!(fs.stat(Path::new("/proj/dest/big_dir")).is_ok());
    assert!(app.toast.visible && app.toast.can_undo);
}

#[test]
fn sidebar_single_click_keeps_focus_for_arrow_navigation_while_enter_or_double_click_focuses_editor()
{
    use editor_platform::{InputEvent, Key, Modifiers, MouseButton};
    use std::path::Path;

    let fs = Arc::new(MemoryFileSystem::new());
    fs.add_file("/proj/a.rs", b"line1\nline2\nline3\n".to_vec());
    fs.add_file("/proj/b.rs", b"second1\nsecond2\n".to_vec());

    let mut app = EditorApp::new(fs, 1400, 900, 2.0);
    let mut raster = BuiltinBitmapRasterizer;
    let mut clip = MemoryClipboard::new();
    let mut host = HeadlessHost::new();

    app.open_folder(Path::new("/proj"), &mut host);
    app.render(&mut raster);

    // 1. Single-click `a.rs` (row 0 in sidebar) -> opens preview AND keeps sidebar focused!
    let row0_y = (30.0 + 12.0) * 2.0;
    app.handle_event(
        InputEvent::MouseDown {
            x_px: 100.0,
            y_px: row0_y,
            button: MouseButton::Left,
            click_count: 1,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    app.handle_event(
        InputEvent::MouseUp {
            x_px: 100.0,
            y_px: row0_y,
            button: MouseButton::Left,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.doc.path(), Some(Path::new("/proj/a.rs")));
    assert!(app.sidebar.is_focused(), "single-click in sidebar must keep sidebar focus");

    // 2. Pressing Down arrow moves selection to `b.rs` in sidebar (NOT line 2 in `a.rs`)!
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Down,
            mods: Modifiers::NONE,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.sidebar.selected(), Some(Path::new("/proj/b.rs")));
    assert_eq!(app.doc.byte_to_line_col(app.caret).0, 0, "editor caret must not move");

    // 3. 1st Enter on `b.rs` (not active yet) previews `b.rs` AND keeps sidebar focused so
    //    browsing with arrows continues without losing sidebar focus; 2nd Enter graduates & focuses editor!
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Enter,
            mods: Modifiers::NONE,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.doc.path(), Some(Path::new("/proj/b.rs")));
    assert!(app.sidebar.is_focused(), "1st Enter on non-active file must keep sidebar focus");

    // 2nd Enter on already-active `b.rs` hands focus to the editor -> Down arrow now moves in editor!
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Enter,
            mods: Modifiers::NONE,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.doc.path(), Some(Path::new("/proj/b.rs")));
    assert!(!app.sidebar.is_focused(), "2nd Enter on active file must hand focus to editor");
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Down,
            mods: Modifiers::NONE,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.doc.byte_to_line_col(app.caret).0, 1);

    // 4. `FocusSidebar` (`⇧⌘E`) focuses sidebar; `Space` previews `a.rs` without leaving sidebar;
    //    `F2` starts in-place rename; bare `Backspace` does NOT pop delete modal while `⌘Backspace` DOES!
    app.handle_event(
        InputEvent::Command(editor_platform::AppCommand::FocusSidebar),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.sidebar.is_focused());
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Up,
            mods: Modifiers::NONE,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.sidebar.selected(), Some(Path::new("/proj/a.rs")));
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Char(' '),
            mods: Modifiers::NONE,
            text: Some(" ".to_string()),
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.doc.path(), Some(Path::new("/proj/a.rs")));
    assert!(app.sidebar.is_focused(), "Space must preview while keeping sidebar focus");

    // Bare Backspace must NOT trigger delete modal; primary+Backspace MUST.
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Backspace,
            mods: Modifiers::NONE,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(!app.confirm_modal.visible);
    let mut primary = Modifiers::NONE;
    #[cfg(target_os = "macos")]
    {
        primary.meta = true;
    }
    #[cfg(not(target_os = "macos"))]
    {
        primary.ctrl = true;
    }
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Backspace,
            mods: primary,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.confirm_modal.visible);
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Escape,
            mods: Modifiers::NONE,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(!app.confirm_modal.visible);

    // F2 starts in-place rename!
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::F(2),
            mods: Modifiers::NONE,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.sidebar.is_prompt_open(), "F2 must open in-place rename prompt");
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Escape,
            mods: Modifiers::NONE,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(!app.sidebar.is_prompt_open());

    // `NextTab` hands focus back to the editor cleanly.
    app.handle_event(
        InputEvent::Command(editor_platform::AppCommand::NextTab),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(!app.sidebar.is_focused(), "tab navigation must clear sidebar focus");

    // 5. Double-clicking `a.rs` in sidebar opens it as Kept AND focuses the editor!
    app.handle_event(
        InputEvent::MouseDown {
            x_px: 100.0,
            y_px: row0_y,
            button: MouseButton::Left,
            click_count: 2,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    app.handle_event(
        InputEvent::MouseUp {
            x_px: 100.0,
            y_px: row0_y,
            button: MouseButton::Left,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.doc.path(), Some(Path::new("/proj/a.rs")));
    assert!(!app.sidebar.is_focused(), "double-click must focus editor");
}

#[test]
fn find_bar_whole_word_regex_capture_replace_and_two_pane_picker_preview_resume_and_splits() {
    use editor_platform::{AppCommand, InputEvent, Key, Modifiers};
    use std::path::Path;

    let fs = Arc::new(MemoryFileSystem::new());
    let mut long_file = String::new();
    for i in 1..=60 {
        if i == 30 {
            long_file.push_str("pub fn compute_alpha(val: i32) -> i32 { val + 1 }\n");
        } else {
            long_file.push_str(&format!("// filler line {i}\n"));
        }
    }
    fs.add_file("/proj/src/alpha.rs", long_file.into_bytes());
    fs.add_file(
        "/proj/src/beta.rs",
        b"fn foo() {}\nfn foobar() {}\nfn compute_beta(n: u64) -> u64 { n }\n".to_vec(),
    );

    let mut app = EditorApp::new(fs.clone(), 1600, 1000, 2.0);
    let mut raster = BuiltinBitmapRasterizer;
    let mut clip = MemoryClipboard::new();
    let mut host = HeadlessHost::new();

    app.open_folder(Path::new("/proj"), &mut host);
    app.open_file(Path::new("/proj/src/beta.rs"), &mut host).unwrap();
    app.render(&mut raster);

    let mut primary_alt = Modifiers::NONE;
    primary_alt.alt = true;
    #[cfg(target_os = "macos")]
    {
        primary_alt.meta = true;
    }
    #[cfg(not(target_os = "macos"))]
    {
        primary_alt.ctrl = true;
    }

    // 1. Inline Find/Replace bar: Whole Word (`⌥⌘W`) excludes `foobar` when searching `foo`.
    app.handle_event(InputEvent::Command(AppCommand::Replace), &mut raster, &mut clip, &mut host);
    app.find_bar.find_input.set_text("foo");
    app.refresh_search();
    assert_eq!(app.find_bar.match_count, 2);

    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Char('w'),
            mods: primary_alt,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.find_bar.whole_word);
    assert_eq!(app.find_bar.match_count, 1, "whole-word must match `foo` and skip `foobar`");

    // 2. Inline Find/Replace bar: Regex (`⌥⌘R`), invalid regex error reporting, and `$1` capture replace!
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Char('w'),
            mods: primary_alt,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Char('r'),
            mods: primary_alt,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.find_bar.regex);

    app.find_bar.find_input.set_text("(unclosed");
    app.refresh_search();
    assert!(app.find_bar.regex_error.is_some());
    assert_eq!(app.find_bar.match_count, 0);

    app.find_bar.find_input.set_text(r"fn\s+(foo\w*)\(\)");
    app.find_bar.replace_input.set_text("pub fn $1()");
    app.refresh_search();
    assert!(app.find_bar.regex_error.is_none());
    assert_eq!(app.find_bar.match_count, 2);
    // Render FindBar with all 3 toggles visible (sanity check zero panic).
    app.render(&mut raster);

    // Replace All with capture group `$1` + atomic Undo.
    let replace_all_btn = app.find_bar.layout.btn_replace_all;
    app.handle_event(
        InputEvent::MouseDown {
            x_px: (replace_all_btn.x + replace_all_btn.w / 2) as f32,
            y_px: (replace_all_btn.y + replace_all_btn.h / 2) as f32,
            button: editor_platform::MouseButton::Left,
            click_count: 1,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    let text_after = app.doc.to_string_lossy();
    assert!(text_after.contains("pub fn foo() {}"));
    assert!(text_after.contains("pub fn foobar() {}"));

    // Close Find bar cleanly.
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Escape,
            mods: Modifiers::NONE,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(!app.find_bar.visible);

    // 3. Two-pane Search in Files (`⇧⌘F`): sees unsaved dirty edits in `beta.rs`,
    //    supports regex `⌥⌘R`, live-previews the selected match, scrolls preview (`⌃D`),
    //    automatically resumes on reopen, and opens into a vertical split (`⌃V`)!
    app.anchor = app.caret; // ensure no selection so ProjectSearch starts clean
    app.handle_event(
        InputEvent::Command(AppCommand::ProjectSearch),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.palette.mode, editor_render::PaletteMode::ProjectSearch);

    // Toggle Regex mode via `⌥⌘R` and search for `compute_\w+`.
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Char('r'),
            mods: primary_alt,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.palette.regex);
    for ch in r"compute_\w+".chars() {
        app.handle_event(
            InputEvent::KeyDown {
                key: Key::Char(ch),
                mods: Modifiers::NONE,
                text: Some(ch.to_string()),
            },
            &mut raster,
            &mut clip,
            &mut host,
        );
    }
    assert_eq!(app.palette.items.len(), 2, "must find compute_alpha and compute_beta");
    app.render(&mut raster);

    // Verify live preview loaded for item 0 (alpha.rs line 30 -> 0-based 29) and `⌃D` scrolls preview!
    let (p0_path, p0_line, p0_scroll_before) = app.palette.preview_info().expect("preview loaded");
    assert_eq!(p0_path, Path::new("/proj/src/alpha.rs"));
    assert_eq!(p0_line, 29);
    let ctrl_only = Modifiers {
        ctrl: true,
        ..Modifiers::NONE
    };
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Char('d'),
            mods: ctrl_only,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    let (_, _, p0_scroll_after) = app.palette.preview_info().expect("preview loaded");
    assert!(p0_scroll_after > p0_scroll_before, "Ctrl+D must scroll preview down");

    // Move to item 1 via `⌃N` (`beta.rs`), verify preview switches, then Escape and reopen `⇧⌘F`:
    // automatic resume restores query, regex toggle, AND selected index 1!
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Char('n'),
            mods: ctrl_only,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.palette.selected, 1);
    assert_eq!(
        app.palette.preview_info().map(|(p, _, _)| p),
        Some(Path::new("/proj/src/beta.rs"))
    );

    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Escape,
            mods: Modifiers::NONE,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(!app.palette.is_open());

    // Reopen `⇧⌘F` with no selection -> resumes query, regex mode, and selection!
    app.handle_event(
        InputEvent::Command(AppCommand::ProjectSearch),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.palette.input.text, r"compute_\w+");
    assert!(app.palette.regex);
    assert_eq!(app.palette.selected, 1);

    // Move back to item 0 (`alpha.rs`) via `⌃P` and open in a vertical split via `⌃V`!
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Char('p'),
            mods: ctrl_only,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.palette.selected, 0);
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Char('v'),
            mods: ctrl_only,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(!app.palette.is_open());
    assert!(app.is_split(), "Ctrl+V from picker must open a split pane");
    assert_eq!(app.doc.path(), Some(Path::new("/proj/src/alpha.rs")));
    assert_eq!(app.doc.byte_to_line_col(app.caret).0, 29);

    // 4. Live Grep also finds unsaved edits in dirty open buffers (`pub fn foo()` we replaced earlier!).
    app.anchor = app.caret;
    app.handle_event(
        InputEvent::Command(AppCommand::ProjectSearch),
        &mut raster,
        &mut clip,
        &mut host,
    );
    // Toggle regex off (`⌥⌘R`) and type literal `pub fn foobar` (which only exists in memory!).
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Char('r'),
            mods: primary_alt,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    app.palette.input.set_text("pub fn foobar");
    app.palette.refresh_results(fs.as_ref());
    assert_eq!(app.palette.items.len(), 1, "Live Grep must see unsaved dirty buffer edits");
    assert_eq!(app.palette.items[0].path, Path::new("/proj/src/beta.rs"));

    // 5. `FsChanged` invalidates cached file index so newly added files appear in Quick Open!
    fs.add_file("/proj/src/gamma_new.rs", b"pub fn gamma() {}\n".to_vec());
    app.palette.close();
    app.handle_event(
        InputEvent::FsChanged(vec![PathBuf::from("/proj/src/gamma_new.rs")]),
        &mut raster,
        &mut clip,
        &mut host,
    );
    app.handle_event(
        InputEvent::Command(AppCommand::QuickOpen),
        &mut raster,
        &mut clip,
        &mut host,
    );
    for ch in "gamma_new".chars() {
        app.handle_event(
            InputEvent::KeyDown {
                key: Key::Char(ch),
                mods: Modifiers::NONE,
                text: Some(ch.to_string()),
            },
            &mut raster,
            &mut clip,
            &mut host,
        );
    }
    assert_eq!(app.palette.items.len(), 1);
    assert_eq!(app.palette.items[0].path, Path::new("/proj/src/gamma_new.rs"));
    app.render(&mut raster);
    app.palette.close();

    // 6. Vim mode leader chords: `<Space>ff` (Quick Open), `<Space>fg` & `<Space>/` (Search in Files),
    //    `<Space>fb` (Pinned & Open Files), `<Space>fe` (Focus Sidebar)!
    app.handle_event(InputEvent::Command(AppCommand::ToggleVimMode), &mut raster, &mut clip, &mut host);
    assert!(app.vim.enabled);

    let send_char = |app: &mut EditorApp,
                     ch: char,
                     raster: &mut BuiltinBitmapRasterizer,
                     clip: &mut MemoryClipboard,
                     host: &mut HeadlessHost| {
        app.handle_event(
            InputEvent::KeyDown {
                key: Key::Char(ch),
                mods: Modifiers::NONE,
                text: Some(ch.to_string()),
            },
            raster,
            clip,
            host,
        );
    };
    let send_esc = |app: &mut EditorApp,
                    raster: &mut BuiltinBitmapRasterizer,
                    clip: &mut MemoryClipboard,
                    host: &mut HeadlessHost| {
        app.handle_event(
            InputEvent::KeyDown {
                key: Key::Escape,
                mods: Modifiers::NONE,
                text: None,
            },
            raster,
            clip,
            host,
        );
    };

    // `<Space>ff` -> Quick Open
    send_char(&mut app, ' ', &mut raster, &mut clip, &mut host);
    send_char(&mut app, 'f', &mut raster, &mut clip, &mut host);
    send_char(&mut app, 'f', &mut raster, &mut clip, &mut host);
    assert_eq!(app.palette.mode, editor_render::PaletteMode::QuickOpen);
    send_esc(&mut app, &mut raster, &mut clip, &mut host);

    // `<Space>fg` -> Search in Files
    send_char(&mut app, ' ', &mut raster, &mut clip, &mut host);
    send_char(&mut app, 'f', &mut raster, &mut clip, &mut host);
    send_char(&mut app, 'g', &mut raster, &mut clip, &mut host);
    assert_eq!(app.palette.mode, editor_render::PaletteMode::ProjectSearch);
    send_esc(&mut app, &mut raster, &mut clip, &mut host);

    // `<Space>/` -> Search in Files shorthand
    send_char(&mut app, ' ', &mut raster, &mut clip, &mut host);
    send_char(&mut app, '/', &mut raster, &mut clip, &mut host);
    assert_eq!(app.palette.mode, editor_render::PaletteMode::ProjectSearch);
    send_esc(&mut app, &mut raster, &mut clip, &mut host);

    // `<Space>fb` -> Pinned & Open Files
    send_char(&mut app, ' ', &mut raster, &mut clip, &mut host);
    send_char(&mut app, 'f', &mut raster, &mut clip, &mut host);
    send_char(&mut app, 'b', &mut raster, &mut clip, &mut host);
    assert!(app.pins_overlay.visible);
    send_esc(&mut app, &mut raster, &mut clip, &mut host);

    // `<Space>fe` -> Focus File Sidebar
    send_char(&mut app, ' ', &mut raster, &mut clip, &mut host);
    send_char(&mut app, 'f', &mut raster, &mut clip, &mut host);
    send_char(&mut app, 'e', &mut raster, &mut clip, &mut host);
    assert!(app.sidebar.is_focused());
}

#[test]
fn in_app_updater_checks_manifest_opens_modal_and_respects_24h_throttle() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let fs = Arc::new(MemoryFileSystem::new());
    let mut app = EditorApp::new(fs, 1280, 800, 1.0);
    app.use_in_app_modal = true;
    let mut raster = BuiltinBitmapRasterizer;
    let mut clip = MemoryClipboard::new();
    let mut host = HeadlessHost::new();

    let hits = Arc::new(AtomicUsize::new(0));
    let hits_bg = Arc::clone(&hits);
    let manifest_json = r#"{
        "version": "9.9.9",
        "build": 90909,
        "notes_url": "https://github.com/builduseful/editor/releases/tag/v9.9.9",
        "assets": {
            "macos-universal": {
                "url": "https://github.com/builduseful/editor/releases/download/v9.9.9/Editor-v9.9.9-macos-universal.tar.gz",
                "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
            },
            "windows-x86_64": {
                "url": "https://github.com/builduseful/editor/releases/download/v9.9.9/editor-v9.9.9-windows-x86_64.zip",
                "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
            },
            "linux-x86_64": {
                "url": "https://github.com/builduseful/editor/releases/download/v9.9.9/editor-v9.9.9-linux-x86_64.tar.gz",
                "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
            }
        }
    }"#;
    host.mock_http_fetcher = Some(Arc::new(move |_url: &str, _max: usize| {
        hits_bg.fetch_add(1, Ordering::SeqCst);
        Ok(manifest_json.as_bytes().to_vec())
    }));

    // 1. Passive launch check runs once when never checked before, then throttles for 24h!
    app.maybe_start_auto_update_check(&mut host);
    for tick in 1..=80 {
        std::thread::sleep(std::time::Duration::from_millis(5));
        app.handle_event(
            InputEvent::Tick { now_ms: tick * 10 },
            &mut raster,
            &mut clip,
            &mut host,
        );
        if hits.load(Ordering::SeqCst) > 0 && host.prefs.contains_key("LastUpdateCheckEpoch") {
            break;
        }
    }
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert!(
        !app.confirm_modal.visible,
        "passive check must never interrupt with a modal"
    );

    // Repeating passive check immediately makes ZERO additional network requests.
    app.maybe_start_auto_update_check(&mut host);
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    // 2. Manual `CheckForUpdates` opens our custom Build Useful confirmation modal immediately!
    app.handle_event(
        InputEvent::Command(AppCommand::CheckForUpdates),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.confirm_modal.visible);
    assert!(app.confirm_modal.is_update_confirm);
    assert_eq!(app.confirm_modal.file_name, "9.9.9");
    app.render(&mut raster);

    // Dismissing with Escape closes the modal cleanly.
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Escape,
            mods: Modifiers::NONE,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(!app.confirm_modal.visible);

    // 3. Toggling `AutoCheckUpdates` persists `"0"` to host preferences.
    let _ = app.execute_ipc(
        &editor_platform::ipc::IpcCommand::Action("toggle-auto-updates".to_string()),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(!app.auto_check_updates);
    assert_eq!(host.prefs.get("AutoCheckUpdates").map(String::as_str), Some("0"));
}

#[test]
fn git_awareness_branch_sidebar_dimming_and_live_myers_gutter_diff() {
    use std::path::Path;

    let fs = Arc::new(MemoryFileSystem::new());
    fs.add_file("/ws/.gitignore", b"*.log\nbuild/\n".to_vec());
    fs.add_file(
        "/ws/src/main.rs",
        b"fn main() {\n    let x = 1;\n    let y = 2;\n}\n".to_vec(),
    );
    fs.add_file("/ws/debug.log", b"noise\n".to_vec());
    fs.add_file("/ws/build/out.txt", b"artifact\n".to_vec());

    let mut app = EditorApp::new(fs.clone(), 1280, 800, 1.0);
    let mut raster = BuiltinBitmapRasterizer;
    let mut clip = MemoryClipboard::new();
    let mut host = HeadlessHost::new();

    app.set_sidebar_root(Path::new("/ws"));
    app.open_file(Path::new("/ws/src/main.rs"), &mut host).unwrap();

    // 1. Sidebar lazily parses `.gitignore` and flags ignored rows cleanly.
    let rows = app.sidebar.rows();
    assert!(rows.iter().any(|r| r.name == "debug.log" && r.ignored));
    assert!(rows.iter().any(|r| r.name == "build" && r.ignored));
    assert!(rows.iter().any(|r| r.name == "src" && !r.ignored));

    // 2. Attach Git HEAD baseline + status map and edit line 2 -> live gutter diff updates immediately!
    app.git_branch = Some("main".to_string());
    app.git_status.upsert(
        std::path::PathBuf::from("/ws"),
        editor_platform::git::parse_porcelain_z(
            Path::new("/ws"),
            b" M src/main.rs\0?? notes.txt\0",
        ),
    );
    app.set_git_head_baseline(b"fn main() {\n    let x = 1;\n    let y = 2;\n}\n");
    assert!(app.viewport.git_diff.is_empty());

    // Edit `let x = 1;` -> `let x = 99;` and add a new line.
    app.caret = 20;
    app.anchor = 20;
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Char('9'),
            mods: Modifiers::NONE,
            text: Some("9".to_string()),
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(
        editor_core::lookup_line_diff(&app.viewport.git_diff, 1),
        Some(editor_core::LineDiffKind::Modified)
    );
    app.render(&mut raster);
}

#[test]
fn multi_cursor_duplicate_delete_comment_outdent_and_cmd_backspace_are_atomic() {
    use std::path::Path;

    let fs = Arc::new(MemoryFileSystem::new());
    fs.add_file("/mc.rs", b"    alpha\n    beta\n    gamma\n".to_vec());

    let mut app = EditorApp::new(fs.clone(), 1280, 800, 1.0);
    let mut raster = BuiltinBitmapRasterizer;
    let mut clip = MemoryClipboard::new();
    let mut host = HeadlessHost::new();
    app.open_file(Path::new("/mc.rs"), &mut host).unwrap();

    // Place carets at the end of line 0 ("    alpha") and line 1 ("    beta").
    let l0_end = app.doc.line_byte_range(0).unwrap().1;
    let l1_end = app.doc.line_byte_range(1).unwrap().1;
    app.caret = l0_end;
    app.anchor = l0_end;
    app.extra_cursors = vec![(l1_end, l1_end)];

    // 1. Toggle comment across both carets -> comments both lines in one undo step!
    app.handle_event(
        InputEvent::Command(AppCommand::ToggleComment),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.doc.to_string_lossy(), "    // alpha\n    // beta\n    gamma\n");

    // Undo reverts BOTH lines atomically!
    app.handle_event(
        InputEvent::Command(AppCommand::Undo),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.doc.to_string_lossy(), "    alpha\n    beta\n    gamma\n");

    // 2. Shift+Tab outdents BOTH lines atomically!
    app.caret = l0_end;
    app.anchor = l0_end;
    app.extra_cursors = vec![(l1_end, l1_end)];
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Tab,
            mods: Modifiers {
                shift: true,
                ..Modifiers::NONE
            },
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.doc.to_string_lossy(), "alpha\nbeta\n    gamma\n");

    // 3. DuplicateLine duplicates BOTH lines atomically!
    app.handle_event(
        InputEvent::Command(AppCommand::DuplicateLine),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(
        app.doc.to_string_lossy(),
        "alpha\nalpha\nbeta\nbeta\n    gamma\n"
    );

    // 4. DeleteLine deletes BOTH duplicated lines atomically!
    app.handle_event(
        InputEvent::Command(AppCommand::DeleteLine),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.doc.to_string_lossy(), "alpha\nbeta\n    gamma\n");

    // 5. Cmd+Backspace deletes to line start across BOTH carets atomically!
    let l0_end = app.doc.line_byte_range(0).unwrap().1;
    let l1_end = app.doc.line_byte_range(1).unwrap().1;
    app.caret = l0_end;
    app.anchor = l0_end;
    app.extra_cursors = vec![(l1_end, l1_end)];
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Backspace,
            mods: Modifiers {
                meta: true,
                ..Modifiers::NONE
            },
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.doc.to_string_lossy(), "\n\n    gamma\n");
}

#[test]
fn project_search_path_filter_and_confirmed_replace_in_files() {
    use editor_platform::FileSystem;
    use std::path::Path;

    let fs = Arc::new(MemoryFileSystem::new());
    fs.add_file("/proj/src/a.rs", b"let old_val = 1;\n".to_vec());
    fs.add_file("/proj/src/b.rs", b"let old_val = 2;\nlet old_val = 3;\n".to_vec());
    fs.add_file("/proj/tests/t.rs", b"let old_val = 99;\n".to_vec());
    fs.add_file("/proj/README.md", b"old_val docs\n".to_vec());

    let mut app = EditorApp::new(fs.clone(), 1280, 800, 1.0);
    app.use_in_app_modal = true;
    let mut raster = BuiltinBitmapRasterizer;
    let mut clip = MemoryClipboard::new();
    let mut host = HeadlessHost::new();

    app.set_sidebar_root(Path::new("/proj"));
    // Keep `a.rs` open in the editor so we test in-memory undoable replacement alongside atomic disk replacement!
    app.open_file(Path::new("/proj/src/a.rs"), &mut host).unwrap();

    app.handle_event(
        InputEvent::Command(AppCommand::ProjectSearch),
        &mut raster,
        &mut clip,
        &mut host,
    );
    app.palette.input.set_text("old_val");
    app.palette.set_path_filter(fs.as_ref(), "*.rs, !tests/");
    assert_eq!(
        app.palette.items.len(),
        3,
        "filter `*.rs, !tests/` must match only src/a.rs (1) + src/b.rs (2), excluding tests/t.rs & README.md"
    );

    app.palette.show_replace = true;
    app.palette.replace_input.set_text("new_val");
    app.render(&mut raster);
    let act = app.palette.prepare_replace_all(fs.as_ref());
    match act {
        editor_render::PaletteAction::RequestReplaceAll {
            match_count,
            ref files,
            ..
        } => {
            assert_eq!(match_count, 3);
            assert_eq!(files.len(), 2);
        }
        other => panic!("expected RequestReplaceAll, got {other:?}"),
    }

    // Trigger via Cmd+Enter in Replace field -> opens custom ConfirmModal!
    // Focus Replace input by toggling filter/replace or pressing Tab twice.
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Tab,
            mods: Modifiers::NONE,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Tab,
            mods: Modifiers::NONE,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Enter,
            mods: Modifiers {
                meta: cfg!(target_os = "macos"),
                ctrl: !cfg!(target_os = "macos"),
                ..Modifiers::NONE
            },
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.confirm_modal.visible);
    assert!(app.confirm_modal.is_replace_all_confirm);

    // Confirm with Enter!
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Enter,
            mods: Modifiers::NONE,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(!app.confirm_modal.visible);

    // Open buffer `src/a.rs` updated in-memory and stays undoable!
    assert_eq!(app.doc.to_string_lossy(), "let new_val = 1;\n");
    assert!(app.doc.is_dirty());
    app.handle_event(
        InputEvent::Command(AppCommand::Undo),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.doc.to_string_lossy(), "let old_val = 1;\n");

    // Unopened `src/b.rs` updated atomically on disk; filtered-out `tests/t.rs` and `README.md` untouched!
    let read_str = |p: &str| {
        let mut v = Vec::new();
        fs.read_prefix_into(Path::new(p), 4096, &mut v).unwrap();
        String::from_utf8(v).unwrap()
    };
    assert_eq!(
        read_str("/proj/src/b.rs"),
        "let new_val = 2;\nlet new_val = 3;\n"
    );
    assert_eq!(read_str("/proj/tests/t.rs"), "let old_val = 99;\n");
    assert_eq!(read_str("/proj/README.md"), "old_val docs\n");
}

#[test]
fn monorepo_subfolder_and_nested_repo_gitignore_isolation() {
    use std::path::Path;

    let fs = Arc::new(MemoryFileSystem::new());
    fs.add_file("/mono/.git/HEAD", b"ref: refs/heads/main\n".to_vec());
    fs.add_file(
        "/mono/.gitignore",
        b"*.log\n/root_only.txt\nservices/api/gen/\n".to_vec(),
    );
    fs.add_file("/mono/root_only.txt", b"ignored at root\n".to_vec());
    fs.add_file("/mono/services/api/src/main.rs", b"fn main() {}\n".to_vec());
    fs.add_file("/mono/services/api/app.log", b"noise\n".to_vec());
    fs.add_file("/mono/services/api/root_only.txt", b"kept in subfolder\n".to_vec());
    fs.add_file("/mono/services/api/gen/auto.rs", b"// generated\n".to_vec());
    // Nested repo inside `/mono/vendor/inner` with its own `.git` and NO `.gitignore` ->
    // outer `/mono/.gitignore`'s `*.log` must NOT bleed into `inner/`!
    fs.add_file("/mono/vendor/inner/.git", b"gitdir: /somewhere\n".to_vec());
    fs.add_file("/mono/vendor/inner/audit.log", b"keep me\n".to_vec());

    let mut app = EditorApp::new(fs.clone(), 1280, 800, 1.0);
    let mut raster = BuiltinBitmapRasterizer;
    let mut clip = MemoryClipboard::new();
    let mut host = HeadlessHost::new();

    // 1. Open monorepo subdirectory `/mono/services/api`.
    app.set_sidebar_root(Path::new("/mono/services/api"));
    let rows = app.sidebar.rows();
    assert!(rows.iter().any(|r| r.name == "app.log" && r.ignored));
    assert!(rows.iter().any(|r| r.name == "gen" && r.ignored));
    assert!(rows.iter().any(|r| r.name == "root_only.txt" && !r.ignored));
    assert!(rows.iter().any(|r| r.name == "src" && !r.ignored));

    // Quick Open inside `/mono/services/api` respects ancestor `/mono/.gitignore` too!
    app.handle_event(
        InputEvent::Command(AppCommand::QuickOpen),
        &mut raster,
        &mut clip,
        &mut host,
    );
    let indexed: Vec<String> = app
        .palette
        .items
        .iter()
        .map(|it| it.path.to_string_lossy().to_string())
        .collect();
    assert!(indexed.iter().any(|p| p.ends_with("/src/main.rs")));
    assert!(indexed.iter().any(|p| p.ends_with("/root_only.txt")));
    assert!(!indexed.iter().any(|p| p.ends_with("app.log")));
    assert!(!indexed.iter().any(|p| p.contains("gen/auto.rs")));

    // 2. Navigate up to `/mono` via sidebar `navigate_to`: root `.gitignore` re-keys cleanly,
    // and expanding nested repo `/mono/vendor/inner` resets outer `.gitignore` rules!
    app.sidebar.navigate_to(fs.as_ref(), Path::new("/mono"));
    app.sidebar.toggle_expand(fs.as_ref(), Path::new("/mono/vendor"));
    app.sidebar
        .toggle_expand(fs.as_ref(), Path::new("/mono/vendor/inner"));
    let rows = app.sidebar.rows();
    assert!(rows
        .iter()
        .any(|r| r.path == Path::new("/mono/root_only.txt") && r.ignored));
    assert!(rows
        .iter()
        .any(|r| r.path == Path::new("/mono/vendor/inner/audit.log") && !r.ignored));
}

#[test]
fn nested_repos_and_multi_repo_workspace_track_active_file_branch() {
    use editor_platform::StdFileSystem;

    let base = std::env::temp_dir().join(format!("editor_nested_git_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let outer = base.join("outer");
    let inner = outer.join("sub");
    let multi = base.join("multi");
    let repo_a = multi.join("repo-a");
    let repo_b = multi.join("repo-b");

    std::fs::create_dir_all(outer.join(".git")).unwrap();
    std::fs::create_dir_all(inner.join(".git")).unwrap();
    std::fs::create_dir_all(repo_a.join(".git")).unwrap();
    std::fs::create_dir_all(repo_b.join(".git")).unwrap();

    std::fs::write(outer.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::write(inner.join(".git/HEAD"), "ref: refs/heads/feat/inner\n").unwrap();
    std::fs::write(repo_a.join(".git/HEAD"), "ref: refs/heads/alpha\n").unwrap();
    std::fs::write(repo_b.join(".git/HEAD"), "ref: refs/heads/beta\n").unwrap();

    std::fs::write(outer.join("README.md"), "# outer\n").unwrap();
    std::fs::write(inner.join("lib.rs"), "pub fn inner() {}\n").unwrap();
    std::fs::write(repo_a.join("a.rs"), "fn a() {}\n").unwrap();
    std::fs::write(repo_b.join("b.rs"), "fn b() {}\n").unwrap();

    let fs = Arc::new(StdFileSystem);
    let mut app = EditorApp::new(fs, 1280, 800, 1.0);
    let mut host = HeadlessHost::new();

    // 1. Repo within repo: status bar branch follows the active file's innermost repo!
    app.open_folder(&outer, &mut host);
    assert_eq!(app.git_branch.as_deref(), Some("main"));

    app.open_file(&outer.join("README.md"), &mut host).unwrap();
    assert_eq!(app.git_branch.as_deref(), Some("main"));

    app.open_file(&inner.join("lib.rs"), &mut host).unwrap();
    assert_eq!(app.git_branch.as_deref(), Some("feat/inner"));

    // Switching back to outer's tab restores outer's branch immediately.
    app.open_file(&outer.join("README.md"), &mut host).unwrap();
    assert_eq!(app.git_branch.as_deref(), Some("main"));

    // 2. Non-repo parent workspace containing multiple repos: branch tracks each opened repo!
    app.open_folder(&multi, &mut host);
    app.open_file(&repo_a.join("a.rs"), &mut host).unwrap();
    assert_eq!(app.git_branch.as_deref(), Some("alpha"));
    app.open_file(&repo_b.join("b.rs"), &mut host).unwrap();
    assert_eq!(app.git_branch.as_deref(), Some("beta"));

    // 3. Monorepo subfolder watches enclosing `.git` as `watched_extra_dir`, whereas repo root does not.
    let sub_pkg = outer.join("pkg");
    std::fs::create_dir_all(&sub_pkg).unwrap();
    std::fs::write(sub_pkg.join("mod.rs"), "fn m() {}\n").unwrap();
    app.open_folder(&outer, &mut host);
    app.open_file(&outer.join("README.md"), &mut host).unwrap();
    assert_eq!(host.watched_extra_dir, None);

    app.open_folder(&sub_pkg, &mut host);
    app.open_file(&sub_pkg.join("mod.rs"), &mut host).unwrap();
    let expected_git_dir = std::fs::canonicalize(outer.join(".git")).unwrap();
    assert_eq!(host.watched_extra_dir.as_deref(), Some(expected_git_dir.as_path()));

    // Simulate external `git checkout` writing `.git/HEAD` -> live FsChanged updates branch immediately!
    std::fs::write(outer.join(".git/HEAD"), "ref: refs/heads/release/0.3\n").unwrap();
    let mut raster = BuiltinBitmapRasterizer;
    let mut clip = MemoryClipboard::new();
    app.handle_event(
        InputEvent::FsChanged(vec![expected_git_dir.join("HEAD")]),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.git_branch.as_deref(), Some("release/0.3"));

    // 4. NextGitChange / PrevGitChange jumps between contiguous diff hunks and wraps cleanly.
    let mem_fs = Arc::new(MemoryFileSystem::new());
    mem_fs.add_file(
        "/diff.rs",
        b"line0\nNEW1\nNEW2\nline3\nline4\nMOD5\nline6\n".to_vec(),
    );
    let mut dapp = EditorApp::new(mem_fs, 1280, 800, 1.0);
    dapp.open_file(std::path::Path::new("/diff.rs"), &mut host)
        .unwrap();
    dapp.set_git_head_baseline(b"line0\nline3\nline4\nold5\nline6\n");
    dapp.caret = 0;
    dapp.anchor = 0;

    // Hunks start at line 1 (NEW1..NEW2) and line 5 (MOD5).
    dapp.handle_event(
        InputEvent::Command(AppCommand::NextGitChange),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(dapp.doc.byte_to_line_col(dapp.caret).0, 1);
    dapp.handle_event(
        InputEvent::Command(AppCommand::NextGitChange),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(dapp.doc.byte_to_line_col(dapp.caret).0, 5);
    // Wraps to first hunk!
    dapp.handle_event(
        InputEvent::Command(AppCommand::NextGitChange),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(dapp.doc.byte_to_line_col(dapp.caret).0, 1);
    // Prev wraps back to last hunk!
    dapp.handle_event(
        InputEvent::Command(AppCommand::PrevGitChange),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(dapp.doc.byte_to_line_col(dapp.caret).0, 5);

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn split_pane_layout_ratios_and_cursors_persist_across_full_app_restart() {
    use editor_platform::FileSystem;
    use std::path::Path;

    let fs = Arc::new(MemoryFileSystem::new());
    fs.add_file("/ws/a.rs", b"a0\na1\na2\na3\n".to_vec());
    fs.add_file("/ws/b.rs", b"b0\nb1\nb2\nb3\nb4\n".to_vec());
    fs.add_file("/ws/c.rs", b"c0\nc1\nc2\n".to_vec());
    fs.add_file("/other/x.rs", b"x0\n".to_vec());

    let mut host = HeadlessHost::new();
    host.prefs.insert("FontSize".to_string(), "15".to_string());
    let mut raster = BuiltinBitmapRasterizer;
    let mut clip = MemoryClipboard::new();

    // 1. Open `/ws`, arrange a 3-pane layout: a.rs (0,0), b.rs (1,0), c.rs (1,1),
    // customize split ratios, position distinct cursors, focus b.rs, and Quit completely.
    {
        let mut app = EditorApp::new(fs.clone(), 1280, 800, 1.0);
        app.load_preferences(&host);
        app.open_folder(Path::new("/ws"), &mut host);
        app.open_file(Path::new("/ws/a.rs"), &mut host).unwrap();
        let a_off = app.doc.line_byte_range(2).unwrap().0 + 1;
        app.caret = a_off;
        app.anchor = a_off;

        app.split_pane_with(Some(Path::new("/ws/b.rs")), &mut host);
        let b_id = app.arrangement.id_at_index(app.active_pane_index()).unwrap();
        let b_off = app.doc.line_byte_range(3).unwrap().0 + 2;
        app.caret = b_off;
        app.anchor = b_off;

        app.split_pane_down(Some(Path::new("/ws/c.rs")), &mut host);
        let c_off = app.doc.line_byte_range(1).unwrap().0 + 1;
        app.caret = c_off;
        app.anchor = c_off;

        app.set_pane_col_ratio(0.65, &mut host);
        app.set_pane_row_ratio(1, 0.35, &mut host);
        app.focus_pane_id(b_id, &mut host);

        app.handle_event(
            InputEvent::Command(AppCommand::Quit),
            &mut raster,
            &mut clip,
            &mut host,
        );
        assert!(host.quit_requested);
    }

    // 2. Cold restart: brand-new EditorApp loads prefs -> exact 3-pane grid, ratios, focus & cursors!
    {
        host.quit_requested = false;
        let mut app = EditorApp::new(fs.clone(), 1280, 800, 1.0);
        app.load_preferences(&host);

        assert_eq!(app.pane_count(), 3);
        assert_eq!(app.doc.path(), Some(Path::new("/ws/b.rs")));
        assert_eq!(app.doc.byte_to_line_col(app.caret), (3, 2));
        assert!((app.arrangement.col_ratio - 0.65).abs() < 0.005);
        assert!((app.arrangement.row_ratio[1] - 0.35).abs() < 0.005);
        assert_eq!(app.viewport.font_size_pt, 15.0);

        // Verify slot placement via reading order (row-major: (0,0)=a.rs, (1,0)=b.rs, (1,1)=c.rs).
        let (ids, n) = app.arrangement.ordered();
        assert_eq!(n, 3);
        assert_eq!(app.arrangement.slot_of(ids[0]), Some(editor_app::PaneSlot::new(0, 0)));
        assert_eq!(app.arrangement.slot_of(ids[1]), Some(editor_app::PaneSlot::new(1, 0)));
        assert_eq!(app.arrangement.slot_of(ids[2]), Some(editor_app::PaneSlot::new(1, 1)));
        assert_eq!(app.active_pane_index(), 1);

        // Focus a.rs & c.rs panes and verify their restored cursors + font size.
        app.focus_pane_id(ids[0], &mut host);
        assert_eq!(app.doc.path(), Some(Path::new("/ws/a.rs")));
        assert_eq!(app.doc.byte_to_line_col(app.caret), (2, 1));
        assert_eq!(app.viewport.font_size_pt, 15.0);

        app.focus_pane_id(ids[2], &mut host);
        assert_eq!(app.doc.path(), Some(Path::new("/ws/c.rs")));
        assert_eq!(app.doc.byte_to_line_col(app.caret), (1, 1));

        // Switch away to `/other`, keep a single pane there, then switch back to `/ws` ->
        // `/ws`'s 3-pane split layout is restored cleanly, and `/other` remembers its 1-pane layout!
        app.open_folder(Path::new("/other"), &mut host);
        app.only_active_pane(&mut host);
        app.open_file(Path::new("/other/x.rs"), &mut host).unwrap();
        assert_eq!(app.pane_count(), 1);
        app.open_folder(Path::new("/ws"), &mut host);
        assert_eq!(app.pane_count(), 3);
        app.open_folder(Path::new("/other"), &mut host);
        assert_eq!(app.pane_count(), 1);
        app.open_folder(Path::new("/ws"), &mut host);
        assert_eq!(app.pane_count(), 3);
    }

    // 3. If one saved pane's file is deleted before restart, remaining panes restore cleanly!
    fs.remove_file(Path::new("/ws/c.rs")).unwrap();
    {
        let mut app = EditorApp::new(fs.clone(), 1280, 800, 1.0);
        app.load_preferences(&host);
        assert_eq!(app.pane_count(), 2);
        assert_eq!(app.arrangement.cols(), 2);
        assert_eq!(app.arrangement.rows_in(0), 1);
        assert_eq!(app.arrangement.rows_in(1), 1);
    }
}

#[test]
fn idle_caret_settles_solid_pauses_timer_respects_notices_and_resumes_on_input() {
    let fs = Arc::new(MemoryFileSystem::new());
    let mut app = EditorApp::new(fs, 1000, 700, 1.0);
    let mut raster = BuiltinBitmapRasterizer;
    let mut clip = MemoryClipboard::new();
    let mut host = HeadlessHost::new();

    // 1. Fresh focused app wants the periodic tick timer and blinks on 530 ms intervals.
    assert!(app.wants_tick_timer());
    assert!(app.is_caret_visible());
    app.handle_event(InputEvent::Tick { now_ms: 1_000 }, &mut raster, &mut clip, &mut host);
    assert!(!app.is_caret_visible(), "first tick >= 530ms toggles caret off");
    assert!(app.wants_tick_timer());

    // 2. After CARET_BLINK_IDLE_MS (10_000 ms) without user input, caret settles solidly VISIBLE
    //    and wants_tick_timer() flips to false so the OS pauses its timer!
    let settled_ms = 1_000 + editor_app::CARET_BLINK_IDLE_MS;
    let redraws_before = host.redraw_requests;
    app.handle_event(InputEvent::Tick { now_ms: settled_ms }, &mut raster, &mut clip, &mut host);
    assert!(app.is_caret_visible(), "idle caret must settle visible, never invisible");
    assert_eq!(host.redraw_requests, redraws_before + 1);
    assert!(!app.wants_tick_timer(), "timer should pause once settled solid with no notice/toast");

    // 3. Subsequent ticks while idle cause zero toggles and zero redraws.
    let redraws_settled = host.redraw_requests;
    app.handle_event(
        InputEvent::Tick { now_ms: settled_ms + 2_000 },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.is_caret_visible());
    assert_eq!(host.redraw_requests, redraws_settled);
    assert!(!app.wants_tick_timer());

    // 4. An active status notice keeps the timer alive until it expires, then pauses again.
    app.set_notice("Saved", false);
    assert!(app.wants_tick_timer(), "active notice must keep timer running so it can expire");
    app.handle_event(
        InputEvent::Tick { now_ms: settled_ms + 10_000 },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(!app.wants_tick_timer(), "timer stops again once notice expires");

    // 5. Any user input resumes blinking and re-enables wants_tick_timer().
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Char('a'),
            mods: Modifiers::NONE,
            text: Some("a".to_string()),
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.wants_tick_timer());
    app.handle_event(
        InputEvent::Tick { now_ms: settled_ms + 10_600 },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(!app.is_caret_visible(), "blinking resumes cleanly after input");
    assert!(app.wants_tick_timer());

    // 6. Unfocusing immediately turns off wants_tick_timer().
    app.handle_event(InputEvent::FocusChanged(false), &mut raster, &mut clip, &mut host);
    assert!(!app.wants_tick_timer());
}

#[test]
fn hot_exit_persists_untitled_and_modified_files_across_restart_and_cleans_up() {
    use editor_platform::{AppCommand, FileSystem, InputEvent, Key, Modifiers};
    use std::path::{Path, PathBuf};

    let fs = Arc::new(MemoryFileSystem::new());
    fs.add_file(
        "/proj/a.rs",
        b"line 0\nline 1\nline 2 alpha\nline 3\nline 4 beta\nline 5\n".to_vec(),
    );
    fs.add_file(
        "/proj/b.rs",
        b"fn one() {}\nfn two() {}\nfn three() {}\n".to_vec(),
    );
    fs.add_file("/other/x.rs", b"fn other() {}\n".to_vec());

    let state_dir = PathBuf::from("/state");
    let mut host = HeadlessHost::new();
    host.state_dir = Some(state_dir.clone());
    let mut raster = BuiltinBitmapRasterizer;
    let mut clip = MemoryClipboard::new();

    // 1. Open `/proj`, dirty pre-existing `a.rs` with selection + scroll, create dirty Untitled draft,
    //    split right with dirty `b.rs`, then Quit (`⌘Q`) — must exit silently without modal!
    {
        let mut app = EditorApp::new(fs.clone(), 1280, 800, 1.0);
        app.use_in_app_modal = true;
        app.load_preferences(&host);
        app.open_folder(Path::new("/proj"), &mut host);

        app.open_file(Path::new("/proj/a.rs"), &mut host).unwrap();
        app.doc.edit(0, 0, "// dirty a\n", 1, false).unwrap();
        let a_anchor = app.doc.line_byte_range(2).unwrap().0 + 3;
        let a_caret = app.doc.line_byte_range(4).unwrap().0 + 5;
        app.anchor = a_anchor;
        app.caret = a_caret;
        app.viewport.scroll_top_line = 2;
        app.viewport.scroll_sub_y_px = 5.5;
        app.viewport.scroll_x_px = 24.0;

        // New Untitled tab with unsaved text, selection, and scroll.
        app.handle_event(
            InputEvent::Command(AppCommand::NewTab),
            &mut raster,
            &mut clip,
            &mut host,
        );
        app.doc
            .edit(
                0,
                0,
                "unsaved draft line 0\nunsaved draft line 1\nunsaved draft line 2\n",
                2,
                false,
            )
            .unwrap();
        let u_anchor = app.doc.line_byte_range(0).unwrap().0 + 4;
        let u_caret = app.doc.line_byte_range(1).unwrap().0 + 7;
        app.anchor = u_anchor;
        app.caret = u_caret;
        app.viewport.scroll_top_line = 1;
        app.viewport.scroll_sub_y_px = 3.0;
        app.viewport.scroll_x_px = 12.5;

        // Split right with `/proj/b.rs` and dirty it as well.
        app.split_pane_with(Some(Path::new("/proj/b.rs")), &mut host);
        app.doc.edit(0, 0, "// dirty b\n", 3, false).unwrap();

        app.handle_event(
            InputEvent::Command(AppCommand::Quit),
            &mut raster,
            &mut clip,
            &mut host,
        );
        assert!(host.quit_requested, "Hot Exit must quit silently when backups succeed");
        assert!(!app.confirm_modal.visible, "Hot Exit must not pop discard modal on Quit");
    }

    // Verify ONLY backup files exist in state_dir (zero `view_state.tsv` side-table!).
    let backup_dir = state_dir.join("backups");
    assert!(fs.stat(&backup_dir.join("index.tsv")).is_ok());
    assert!(
        fs.stat(&state_dir.join("view_state.tsv")).is_err(),
        "view_state.tsv must not exist; open-tab views live directly on FolderSession"
    );
    let mut initial_bufs: Vec<String> = fs
        .read_dir(&backup_dir)
        .unwrap()
        .into_iter()
        .filter(|e| e.name.ends_with(".buf"))
        .map(|e| e.name)
        .collect();
    initial_bufs.sort();
    assert_eq!(initial_bufs.len(), 3, "expected 3 .buf drafts (a.rs, Untitled, b.rs)");

    // 2. Cold restart #1: verify BOTH split panes + parked a.rs restored dirty with exact text & views!
    {
        host.quit_requested = false;
        let mut app = EditorApp::new(fs.clone(), 1280, 800, 1.0);
        app.use_in_app_modal = true;
        app.load_preferences(&host);

        assert_eq!(app.pane_count(), 2);
        assert_eq!(app.doc.path(), Some(Path::new("/proj/b.rs")));
        assert!(app.doc.is_dirty());
        assert!(app.doc.to_string_lossy().starts_with("// dirty b\n"));

        // Focus left pane -> Untitled draft with exact text, selection, and scroll!
        let (ids, n) = app.arrangement.ordered();
        assert_eq!(n, 2);
        app.focus_pane_id(ids[0], &mut host);
        assert!(app.doc.path().is_none(), "left pane must be restored Untitled draft");
        assert!(app.doc.is_dirty());
        assert_eq!(
            app.doc.to_string_lossy(),
            "unsaved draft line 0\nunsaved draft line 1\nunsaved draft line 2\n"
        );
        assert_eq!(app.doc.byte_to_line_col(app.anchor), (0, 4));
        assert_eq!(app.doc.byte_to_line_col(app.caret), (1, 7));
        assert_eq!(app.viewport.scroll_top_line, 1);
        assert!((app.viewport.scroll_sub_y_px - 3.0).abs() < 0.2);
        assert!((app.viewport.scroll_x_px - 12.5).abs() < 0.2);

        // Open parked `a.rs` -> dirty with exact unsaved edits, selection, and scroll!
        app.open_file(Path::new("/proj/a.rs"), &mut host).unwrap();
        assert!(app.doc.is_dirty());
        assert!(app.doc.to_string_lossy().starts_with("// dirty a\n"));
        assert_eq!(app.doc.byte_to_line_col(app.anchor), (2, 3));
        assert_eq!(app.doc.byte_to_line_col(app.caret), (4, 5));
        assert_eq!(app.viewport.scroll_top_line, 2);
        assert!((app.viewport.scroll_sub_y_px - 5.5).abs() < 0.2);
        assert!((app.viewport.scroll_x_px - 24.0).abs() < 0.2);

        // Quit without editing -> verify ZERO `.buf` revision churn!
        app.handle_event(
            InputEvent::Command(AppCommand::Quit),
            &mut raster,
            &mut clip,
            &mut host,
        );
        assert!(host.quit_requested);
    }

    let mut bufs_after_noop_quit: Vec<String> = fs
        .read_dir(&backup_dir)
        .unwrap()
        .into_iter()
        .filter(|e| e.name.ends_with(".buf"))
        .map(|e| e.name)
        .collect();
    bufs_after_noop_quit.sort();
    assert_eq!(
        bufs_after_noop_quit, initial_bufs,
        "re-quitting without edits must not churn immutable .buf revisions"
    );

    // 3. Cold restart #2: test Save cleanup, Save-Conflict 3-button modal, and explicit Cmd+W discard!
    {
        host.quit_requested = false;
        let mut app = EditorApp::new(fs.clone(), 1280, 800, 1.0);
        app.use_in_app_modal = true;
        app.load_preferences(&host);

        // Save `a.rs` -> writes disk, marks clean, immediately removes its `.buf`!
        app.open_file(Path::new("/proj/a.rs"), &mut host).unwrap();
        app.handle_event(
            InputEvent::Command(AppCommand::Save),
            &mut raster,
            &mut clip,
            &mut host,
        );
        assert!(!app.doc.is_dirty());
        let saved_a = fs.open_read(Path::new("/proj/a.rs")).unwrap();
        let mut buf_a = vec![0u8; saved_a.len() as usize];
        saved_a.read_at(0, &mut buf_a).unwrap();
        assert!(String::from_utf8(buf_a).unwrap().starts_with("// dirty a\n"));
        let remaining_bufs: Vec<_> = fs
            .read_dir(&backup_dir)
            .unwrap()
            .into_iter()
            .filter(|e| e.name.ends_with(".buf"))
            .collect();
        assert_eq!(remaining_bufs.len(), 2, "saving a.rs must immediately GC its .buf");

        // Externally modify `b.rs` on disk -> Cmd+S on restored dirty `b.rs` opens Save-Conflict modal!
        fs.add_file("/proj/b.rs", b"// external change on disk\n".to_vec());
        app.open_file(Path::new("/proj/b.rs"), &mut host).unwrap();
        assert!(app.doc.is_dirty());
        app.handle_event(
            InputEvent::Command(AppCommand::Save),
            &mut raster,
            &mut clip,
            &mut host,
        );
        assert!(app.confirm_modal.visible);
        assert!(app.confirm_modal.is_save_conflict);

        // Cancel (`Escape`) keeps draft dirty.
        app.handle_event(
            InputEvent::KeyDown {
                key: Key::Escape,
                mods: Modifiers::NONE,
                text: None,
            },
            &mut raster,
            &mut clip,
            &mut host,
        );
        assert!(!app.confirm_modal.visible);
        assert!(app.doc.is_dirty());

        // Trigger Cmd+S again and confirm Overwrite (`Enter`) -> writes draft & removes `.buf`!
        app.handle_event(
            InputEvent::Command(AppCommand::Save),
            &mut raster,
            &mut clip,
            &mut host,
        );
        assert!(app.confirm_modal.is_save_conflict);
        app.handle_event(
            InputEvent::KeyDown {
                key: Key::Enter,
                mods: Modifiers::NONE,
                text: None,
            },
            &mut raster,
            &mut clip,
            &mut host,
        );
        assert!(!app.confirm_modal.visible);
        assert!(!app.doc.is_dirty());
        let saved_b = fs.open_read(Path::new("/proj/b.rs")).unwrap();
        let mut buf_b = vec![0u8; saved_b.len() as usize];
        saved_b.read_at(0, &mut buf_b).unwrap();
        assert!(String::from_utf8(buf_b).unwrap().starts_with("// dirty b\n"));

        // Explicitly closing dirty Untitled (`⌘W` in single-pane mode) MUST prompt Save / Don't Save / Cancel!
        app.only_active_pane(&mut host);
        let untitled_id = app
            .working_set
            .ordered_ids()
            .into_iter()
            .find(|&id| app.working_set.get_meta(id).is_some_and(|m| m.path.is_none() && m.dirty))
            .expect("restored dirty Untitled must be in working set");
        app.activate_buffer(untitled_id, &mut host);
        assert!(app.doc.path().is_none() && app.doc.is_dirty());
        app.handle_event(
            InputEvent::Command(AppCommand::CloseTab),
            &mut raster,
            &mut clip,
            &mut host,
        );
        assert!(app.confirm_modal.visible && !app.confirm_modal.is_save_conflict);

        // Choose Don't Save (`d`) -> closes Untitled and cleans up the final `.buf`!
        app.handle_event(
            InputEvent::KeyDown {
                key: Key::Char('d'),
                mods: Modifiers::NONE,
                text: None,
            },
            &mut raster,
            &mut clip,
            &mut host,
        );
        assert!(!app.confirm_modal.visible);
        let remaining_bufs: Vec<_> = fs
            .read_dir(&backup_dir)
            .unwrap()
            .into_iter()
            .filter(|e| e.name.ends_with(".buf"))
            .collect();
        assert!(remaining_bufs.is_empty(), "all .buf drafts must be deleted after save/discard");

        // 4. Folder isolation: dirty `/proj/a.rs`, switch to brand-new `/other` (must start clean
        //    with zero leaked tabs!), then return to `/proj` and verify dirty `a.rs` comes back.
        app.open_file(Path::new("/proj/a.rs"), &mut host).unwrap();
        app.doc.edit(0, 0, "// folder-isolated edit\n", 4, false).unwrap();
        app.open_folder(Path::new("/other"), &mut host);
        assert!(
            app.doc.path().is_none() && !app.doc.is_dirty(),
            "switching to a brand-new folder must not leak outgoing tabs or drafts"
        );
        app.open_file(Path::new("/other/x.rs"), &mut host).unwrap();
        assert!(!app.doc.is_dirty());
        assert_eq!(app.doc.path(), Some(Path::new("/other/x.rs")));

        app.open_folder(Path::new("/proj"), &mut host);
        assert_eq!(app.doc.path(), Some(Path::new("/proj/a.rs")));
        assert!(app.doc.is_dirty());
        assert!(app.doc.to_string_lossy().starts_with("// folder-isolated edit\n"));

        // 5. Undo (`⌘Z`) back to clean on a live edited buffer immediately deletes its `.buf` backup!
        app.handle_event(InputEvent::Command(AppCommand::Save), &mut raster, &mut clip, &mut host);
        assert!(!app.doc.is_dirty());
        app.doc.edit(0, 0, "// temp edit\n", 5, false).unwrap();
        assert!(app.doc.is_dirty());
        app.handle_event(InputEvent::FocusChanged(false), &mut raster, &mut clip, &mut host);
        assert!(fs.stat(&backup_dir.join("index.tsv")).is_ok());
        app.handle_event(InputEvent::Command(AppCommand::Undo), &mut raster, &mut clip, &mut host);
        assert!(!app.doc.is_dirty());
        let after_undo: Vec<_> = fs
            .read_dir(&backup_dir)
            .unwrap_or_default()
            .into_iter()
            .filter(|e| e.name.ends_with(".buf"))
            .collect();
        assert!(after_undo.is_empty(), "undoing back to clean must immediately GC its .buf");
    }

    // 6. Standalone (no-folder) Untitled draft survives `handle_will_terminate` (Dock Quit),
    //    and closing it with `⌘W` removes `@standalone` so it does not zombie-reopen!
    {
        let mut standalone_host = HeadlessHost::new();
        standalone_host.state_dir = Some(PathBuf::from("/standalone_state"));
        let mut app = EditorApp::new(fs.clone(), 1000, 700, 1.0);
        app.use_in_app_modal = true;
        app.load_preferences(&standalone_host);
        assert!(app.sidebar.root().is_none());
        app.doc.edit(0, 0, "standalone scratchpad note\n", 5, false).unwrap();
        app.handle_will_terminate(&mut standalone_host);

        let mut app2 = EditorApp::new(fs.clone(), 1000, 700, 1.0);
        app2.use_in_app_modal = true;
        app2.load_preferences(&standalone_host);
        assert!(app2.doc.path().is_none());
        assert!(app2.doc.is_dirty());
        assert_eq!(app2.doc.to_string_lossy(), "standalone scratchpad note\n");

        // Close standalone draft via Cmd+W -> Don't Save -> Quit -> must NOT zombie-reopen!
        app2.handle_event(
            InputEvent::Command(AppCommand::CloseTab),
            &mut raster,
            &mut clip,
            &mut standalone_host,
        );
        app2.handle_event(
            InputEvent::KeyDown {
                key: Key::Char('d'),
                mods: Modifiers::NONE,
                text: None,
            },
            &mut raster,
            &mut clip,
            &mut standalone_host,
        );
        app2.handle_will_terminate(&mut standalone_host);

        let mut app3 = EditorApp::new(fs.clone(), 1000, 700, 1.0);
        app3.load_preferences(&standalone_host);
        assert!(!app3.doc.is_dirty() && app3.doc.is_empty());
    }
}

#[test]
fn open_tab_cursor_selection_and_scroll_persist_across_restart_and_forget_on_close() {
    use editor_platform::{AppCommand, InputEvent};
    use std::path::{Path, PathBuf};

    let fs = Arc::new(MemoryFileSystem::new());
    let mut content = String::new();
    for i in 0..80 {
        if i == 39 {
            content.push_str("// café 🚀 line 39 with wide content for horizontal scroll\n");
        } else {
            content.push_str(&format!("let line_{i:02} = \"value_{i:02}\";\n"));
        }
    }
    fs.add_file("/ws/long.rs", content.into_bytes());
    fs.add_file("/ws/other.rs", b"fn other() {}\n".to_vec());

    let mut host = HeadlessHost::new();
    host.state_dir = Some(PathBuf::from("/view_state_root"));
    let mut raster = BuiltinBitmapRasterizer;
    let mut clip = MemoryClipboard::new();

    // 1. Open `long.rs` (parked) and `other.rs` (active) with exact selection & fractional scroll,
    //    then Quit (`⌘Q`) — both open tabs must keep their exact view across restart!
    {
        let mut app = EditorApp::new(fs.clone(), 1200, 760, 1.0);
        app.load_preferences(&host);
        app.open_folder(Path::new("/ws"), &mut host);
        app.open_file(Path::new("/ws/long.rs"), &mut host).unwrap();

        let anchor_off = app.doc.line_byte_range(35).unwrap().0 + 4;
        // Byte col 9 on "// café 🚀" lands on a valid char boundary (`// caf` + `é` = 8 bytes, ` ` = 9).
        let caret_off = app.doc.line_byte_range(39).unwrap().0 + 9;
        app.anchor = anchor_off;
        app.caret = caret_off;
        app.viewport.scroll_top_line = 25;
        app.viewport.scroll_sub_y_px = 7.5;
        app.viewport.scroll_x_px = 42.5;

        // Switch to `other.rs` so `long.rs` is a parked open tab, then Quit.
        app.open_file(Path::new("/ws/other.rs"), &mut host).unwrap();
        app.handle_event(
            InputEvent::Command(AppCommand::Quit),
            &mut raster,
            &mut clip,
            &mut host,
        );
        assert!(host.quit_requested);
    }

    // 2. Cold restart: parked `long.rs` restores exact selection + scroll!
    {
        host.quit_requested = false;
        let mut app = EditorApp::new(fs.clone(), 1200, 760, 1.0);
        app.load_preferences(&host);
        assert_eq!(app.doc.path(), Some(Path::new("/ws/other.rs")));

        app.open_file(Path::new("/ws/long.rs"), &mut host).unwrap();
        assert_eq!(app.doc.byte_to_line_col(app.anchor), (35, 4));
        assert_eq!(app.doc.byte_to_line_col(app.caret), (39, 9));
        assert_eq!(app.viewport.scroll_top_line, 25);
        assert!((app.viewport.scroll_sub_y_px - 7.5).abs() < 0.2);
        assert!((app.viewport.scroll_x_px - 42.5).abs() < 0.2);

        // 3. Explicit `⌘W` immediately forgets `long.rs`'s cursor & scroll (no closed-file pileup!).
        app.handle_event(
            InputEvent::Command(AppCommand::CloseTab),
            &mut raster,
            &mut clip,
            &mut host,
        );
        assert_eq!(app.doc.path(), Some(Path::new("/ws/other.rs")));

        app.open_file(Path::new("/ws/long.rs"), &mut host).unwrap();
        assert_eq!(app.caret, 0, "explicitly closed tab must reopen cleanly at line 1, col 1");
        assert_eq!(app.anchor, 0);
        assert_eq!(app.viewport.scroll_top_line, 0);

        // Position deep again and Quit so we can test UTF-8 clamping on external truncation.
        let caret_off = app.doc.line_byte_range(39).unwrap().0 + 9;
        app.anchor = caret_off;
        app.caret = caret_off;
        app.viewport.scroll_top_line = 25;
        app.handle_event(
            InputEvent::Command(AppCommand::Quit),
            &mut raster,
            &mut clip,
            &mut host,
        );
    }

    // 4. External truncation with multi-byte UTF-8: clamping stays on valid char boundary with zero panic!
    fs.add_file("/ws/long.rs", "é\n🚀\n".as_bytes().to_vec());
    {
        let mut app = EditorApp::new(fs.clone(), 1200, 760, 1.0);
        app.load_preferences(&host);
        assert_eq!(app.doc.path(), Some(Path::new("/ws/long.rs")));
        let text = app.doc.to_string_lossy();
        assert!(text.is_char_boundary(app.caret as usize));
        assert!(text.is_char_boundary(app.anchor as usize));
        assert!(app.viewport.scroll_top_line < app.doc.editable_line_count());
    }
}

#[test]
fn persisted_undo_redo_survives_restart_across_save_drafts_external_changes_and_cleans_up_on_close() {
    use editor_platform::{AppCommand, FileSystem, InputEvent};
    use std::path::{Path, PathBuf};

    let fs = Arc::new(MemoryFileSystem::new());
    fs.add_file("/uproj/a.rs", b"fn a() {}\n".to_vec());
    fs.add_file("/uproj/b.rs", b"fn b() {}\n".to_vec());

    let state_dir = PathBuf::from("/undo_state");
    let mut host = HeadlessHost::new();
    host.state_dir = Some(state_dir.clone());
    let mut raster = BuiltinBitmapRasterizer;
    let mut clip = MemoryClipboard::new();

    // 1. Edit `a.rs` (2 steps) and SAVE (`⌘S`), edit `b.rs` (2 steps, leave dirty),
    //    and create a dirty Untitled draft (2 steps), then Quit (`⌘Q`).
    {
        let mut app = EditorApp::new(fs.clone(), 1200, 760, 1.0);
        app.use_in_app_modal = true;
        app.load_preferences(&host);
        app.open_folder(Path::new("/uproj"), &mut host);

        app.open_file(Path::new("/uproj/a.rs"), &mut host).unwrap();
        app.doc.edit(0, 0, "// a1\n", 10, false).unwrap();
        app.doc.edit(0, 0, "// a2\n", 20, false).unwrap();
        app.handle_event(
            InputEvent::Command(AppCommand::Save),
            &mut raster,
            &mut clip,
            &mut host,
        );
        assert!(!app.doc.is_dirty(), "a.rs must be saved clean before quit");

        app.open_file(Path::new("/uproj/b.rs"), &mut host).unwrap();
        app.doc.edit(0, 0, "// b1\n", 30, false).unwrap();
        app.doc.edit(0, 0, "// b2\n", 40, false).unwrap();
        assert!(app.doc.is_dirty());

        app.handle_event(
            InputEvent::Command(AppCommand::NewTab),
            &mut raster,
            &mut clip,
            &mut host,
        );
        app.doc.edit(0, 0, "draft step 1\n", 50, false).unwrap();
        app.doc.edit(0, 0, "draft step 2\n", 60, false).unwrap();

        app.handle_event(
            InputEvent::Command(AppCommand::Quit),
            &mut raster,
            &mut clip,
            &mut host,
        );
        assert!(host.quit_requested);
    }

    // 2. Cold restart #1:
    //    - Untitled draft undoes & redoes cleanly.
    //    - Clean saved `a.rs` undoes past `⌘S` (becoming dirty) and redoes back to clean!
    //    - Dirty `b.rs` undoes all the way back to clean (deleting its `.buf`), and keeps redo across quit!
    {
        host.quit_requested = false;
        let mut app = EditorApp::new(fs.clone(), 1200, 760, 1.0);
        app.use_in_app_modal = true;
        app.load_preferences(&host);

        // Active tab is the Untitled draft.
        assert!(app.doc.path().is_none() && app.doc.is_dirty());
        assert_eq!(app.doc.to_string_lossy(), "draft step 2\ndraft step 1\n");
        app.handle_event(InputEvent::Command(AppCommand::Undo), &mut raster, &mut clip, &mut host);
        assert_eq!(app.doc.to_string_lossy(), "draft step 1\n");
        app.handle_event(InputEvent::Command(AppCommand::Redo), &mut raster, &mut clip, &mut host);
        assert_eq!(app.doc.to_string_lossy(), "draft step 2\ndraft step 1\n");

        // Saved clean `a.rs`: `⌘Z` undoes past the `⌘S` save, `⇧⌘Z` returns to clean!
        app.open_file(Path::new("/uproj/a.rs"), &mut host).unwrap();
        assert!(!app.doc.is_dirty());
        assert_eq!(app.doc.to_string_lossy(), "// a2\n// a1\nfn a() {}\n");
        app.handle_event(InputEvent::Command(AppCommand::Undo), &mut raster, &mut clip, &mut host);
        assert!(app.doc.is_dirty(), "undoing past save must mark file dirty");
        assert_eq!(app.doc.to_string_lossy(), "// a1\nfn a() {}\n");
        app.handle_event(InputEvent::Command(AppCommand::Redo), &mut raster, &mut clip, &mut host);
        assert!(!app.doc.is_dirty(), "redoing back to saved rev must mark file clean");
        assert_eq!(app.doc.to_string_lossy(), "// a2\n// a1\nfn a() {}\n");

        // Dirty `b.rs`: 2 Undos return to original clean disk content and keep Redo across next restart!
        app.open_file(Path::new("/uproj/b.rs"), &mut host).unwrap();
        assert!(app.doc.is_dirty());
        app.handle_event(InputEvent::Command(AppCommand::Undo), &mut raster, &mut clip, &mut host);
        assert!(app.doc.is_dirty());
        assert_eq!(app.doc.to_string_lossy(), "// b1\nfn b() {}\n");
        app.handle_event(InputEvent::Command(AppCommand::Undo), &mut raster, &mut clip, &mut host);
        assert!(!app.doc.is_dirty(), "undoing all the way to original must mark b.rs clean");
        assert_eq!(app.doc.to_string_lossy(), "fn b() {}\n");

        app.handle_event(
            InputEvent::Command(AppCommand::Quit),
            &mut raster,
            &mut clip,
            &mut host,
        );
        assert!(host.quit_requested);
    }

    // 3. Cold restart #2: `b.rs` (clean after Undo) still Redoes via `⇧⌘Z`, and `⌘W` on `a.rs`
    //    deletes `a.rs`'s `.undo` immediately so reopening `a.rs` starts with empty undo!
    {
        host.quit_requested = false;
        let mut app = EditorApp::new(fs.clone(), 1200, 760, 1.0);
        app.use_in_app_modal = true;
        app.load_preferences(&host);

        assert_eq!(app.doc.path(), Some(Path::new("/uproj/b.rs")));
        assert!(!app.doc.is_dirty());
        app.handle_event(InputEvent::Command(AppCommand::Redo), &mut raster, &mut clip, &mut host);
        assert!(app.doc.is_dirty());
        assert_eq!(app.doc.to_string_lossy(), "// b1\nfn b() {}\n");

        // Close clean `a.rs` with `⌘W` -> immediately deletes its `.undo` file!
        let a_key = editor_app::UndoStore::key_for_path(Path::new("/uproj/a.rs"));
        assert!(fs.stat(&editor_app::UndoStore::undo_dir(&state_dir).join(&a_key)).is_ok());
        app.open_file(Path::new("/uproj/a.rs"), &mut host).unwrap();
        app.handle_event(
            InputEvent::Command(AppCommand::CloseTab),
            &mut raster,
            &mut clip,
            &mut host,
        );
        assert!(
            fs.stat(&editor_app::UndoStore::undo_dir(&state_dir).join(&a_key)).is_err(),
            "closing a saved edited tab via Cmd+W must immediately remove its .undo file"
        );
        app.open_file(Path::new("/uproj/a.rs"), &mut host).unwrap();
        assert!(!app.doc.undo_stack().can_undo(), "reopening closed tab must start with empty undo");

        // Re-edit `a.rs` (save clean) and keep `b.rs` dirty, then Quit to test external edits!
        app.doc.edit(0, 0, "// fresh\n", 70, false).unwrap();
        app.handle_event(InputEvent::Command(AppCommand::Save), &mut raster, &mut clip, &mut host);
        app.handle_event(InputEvent::Command(AppCommand::Quit), &mut raster, &mut clip, &mut host);
    }

    // 4. External disk changes while closed:
    //    - Clean `a.rs` modified externally -> stale `.undo` silently ignored (zero corruption).
    //    - Dirty `b.rs` modified externally -> `⌘Z` still works, AND `saved_rev` is cleared so
    //      undoing back to pre-edit text keeps `b.rs` marked dirty!
    fs.add_file("/uproj/a.rs", b"// external git pull on a\n".to_vec());
    fs.add_file("/uproj/b.rs", b"// external git pull on b\n".to_vec());
    {
        host.quit_requested = false;
        let mut app = EditorApp::new(fs.clone(), 1200, 760, 1.0);
        app.use_in_app_modal = true;
        app.load_preferences(&host);

        app.open_file(Path::new("/uproj/a.rs"), &mut host).unwrap();
        assert_eq!(app.doc.to_string_lossy(), "// external git pull on a\n");
        assert!(
            !app.doc.undo_stack().can_undo(),
            "stale .undo on externally changed clean file must be ignored"
        );

        app.open_file(Path::new("/uproj/b.rs"), &mut host).unwrap();
        assert!(app.doc.is_dirty());
        assert_eq!(app.doc.to_string_lossy(), "// b1\nfn b() {}\n");
        app.handle_event(InputEvent::Command(AppCommand::Undo), &mut raster, &mut clip, &mut host);
        assert_eq!(app.doc.to_string_lossy(), "fn b() {}\n");
        assert!(
            app.doc.is_dirty(),
            "undoing a draft whose disk file changed externally must stay dirty"
        );
    }
}

#[test]
fn external_agent_edit_preserves_utf8_cursor_scroll_language_syncs_splits_and_cleans_undo() {
    use editor_platform::{AppCommand, FileSystem, InputEvent};
    use std::path::{Path, PathBuf};

    let fs = Arc::new(MemoryFileSystem::new());
    let mut big = String::new();
    for i in 0..40 {
        big.push_str(&format!("line_{i:02}_abcdef\n"));
    }
    fs.add_file("/ext/notes.txt", big.into_bytes());
    fs.add_file("/ext/src/nested.rs", b"fn old_nested() {}\n".to_vec());
    fs.add_file("/ext/parked.rs", b"fn old_parked() {}\n".to_vec());

    let state_dir = PathBuf::from("/ext_state");
    let mut host = HeadlessHost::new();
    host.state_dir = Some(state_dir.clone());
    let mut raster = BuiltinBitmapRasterizer;
    let mut clip = MemoryClipboard::new();

    let mut app = EditorApp::new(fs.clone(), 1200, 760, 1.0);
    app.load_preferences(&host);
    app.open_folder(Path::new("/ext"), &mut host);

    // 1. Open parked.rs (parked), nested.rs (right split), and notes.txt (active left pane).
    app.open_file(Path::new("/ext/parked.rs"), &mut host).unwrap();
    app.open_file(Path::new("/ext/notes.txt"), &mut host).unwrap();
    // Apply custom Language override (JSON on `.txt`), make a saved edit so `.undo` exists on disk!
    app.set_active_language("json", &mut host).unwrap();
    app.doc.edit(0, 0, "// edit\n", 1, false).unwrap();
    app.handle_event(InputEvent::Command(AppCommand::Save), &mut raster, &mut clip, &mut host);
    let notes_undo_key = editor_app::UndoStore::key_for_path(Path::new("/ext/notes.txt"));
    assert!(fs.stat(&editor_app::UndoStore::undo_dir(&state_dir).join(&notes_undo_key)).is_ok());

    // Position deep in notes.txt with a selection and scroll offset.
    app.anchor = app.doc.line_byte_range(10).unwrap().0 + 3;
    app.caret = app.doc.line_byte_range(30).unwrap().0 + 5;
    app.viewport.scroll_top_line = 25;

    // Split right with `/ext/src/nested.rs`, then focus left pane (`notes.txt`) again.
    app.split_pane_with(Some(Path::new("/ext/src/nested.rs")), &mut host);
    let (ids, _) = app.arrangement.ordered();
    app.focus_pane_id(ids[0], &mut host);
    assert_eq!(app.doc.path(), Some(Path::new("/ext/notes.txt")));

    // 2. Agent overwrites `notes.txt` with short multi-byte UTF-8 AND updates `nested.rs` & `parked.rs`!
    //    Deliver a coalesced ancestor directory `FsChanged(["/ext"])` -> MUST reload all 3!
    fs.add_file("/ext/notes.txt", "é\n🚀🎉\n".as_bytes().to_vec());
    fs.add_file("/ext/src/nested.rs", b"fn new_nested() {}\n".to_vec());
    fs.add_file("/ext/parked.rs", b"fn new_parked() {}\n".to_vec());

    app.handle_event(
        InputEvent::FsChanged(vec![PathBuf::from("/ext")]),
        &mut raster,
        &mut clip,
        &mut host,
    );

    // Verify UTF-8 char-boundary safety, scroll clamping, collapsed selection, preserved JSON language,
    // AND immediate stale `.undo` file removal!
    let text = app.doc.to_string_lossy();
    assert_eq!(text, "é\n🚀🎉\n");
    assert!(text.is_char_boundary(app.caret as usize));
    assert_eq!(app.anchor, app.caret, "external reload must collapse stale selection");
    assert!(app.viewport.scroll_top_line < app.doc.editable_line_count());
    assert_eq!(app.doc.language().id(), "json", "custom Language override must survive reload");
    assert!(
        fs.stat(&editor_app::UndoStore::undo_dir(&state_dir).join(&notes_undo_key))
            .is_err(),
        "stale .undo must be GC'd immediately on external reload"
    );

    // Secondary split pane (`nested.rs`) and parked tab (`parked.rs`) both reloaded!
    assert_eq!(app.other_panes[0].doc.to_string_lossy(), "fn new_nested() {}\n");
    app.open_file(Path::new("/ext/parked.rs"), &mut host).unwrap();
    assert_eq!(app.doc.to_string_lossy(), "fn new_parked() {}\n");

    // 3. `FocusChanged(true)` (without FsChanged!) also reloads split panes & parked tabs!
    fs.add_file("/ext/src/nested.rs", b"fn nested_on_focus() {}\n".to_vec());
    app.handle_event(InputEvent::FocusChanged(true), &mut raster, &mut clip, &mut host);
    assert_eq!(
        app.other_panes[0].doc.to_string_lossy(),
        "fn nested_on_focus() {}\n"
    );

    // 4. Manual `Reload` syncs two split panes viewing the same file!
    app.only_active_pane(&mut host);
    app.open_file(Path::new("/ext/parked.rs"), &mut host).unwrap();
    app.split_pane_with(None, &mut host);
    fs.add_file("/ext/parked.rs", b"fn manual_reloaded() {}\n".to_vec());
    app.handle_event(InputEvent::Command(AppCommand::Reload), &mut raster, &mut clip, &mut host);
    assert_eq!(app.doc.to_string_lossy(), "fn manual_reloaded() {}\n");
    assert_eq!(app.other_panes[0].doc.to_string_lossy(), "fn manual_reloaded() {}\n");
}

#[test]
fn context_menu_on_tabs_pane_headers_sidebar_empty_and_command_palette() {
    use editor_render::ContextMenuEntry;
    use std::path::Path;

    let fs = Arc::new(MemoryFileSystem::new());
    fs.add_file("/proj/src/a.rs", b"fn a() {}\n".to_vec());
    fs.add_file("/proj/src/b.rs", b"fn b() {}\n".to_vec());
    fs.add_file("/external/c.rs", b"fn c() {}\n".to_vec());

    let mut app = EditorApp::new(fs, 1600, 1000, 2.0);
    let mut raster = BuiltinBitmapRasterizer;
    let mut clip = MemoryClipboard::new();
    let mut host = HeadlessHost::new();
    host.reveal_label_override = Some("Reveal in File Explorer");

    app.open_folder(Path::new("/proj"), &mut host);
    app.open_file(Path::new("/proj/src/a.rs"), &mut host).unwrap();
    app.open_file(Path::new("/proj/src/b.rs"), &mut host).unwrap();
    app.open_file(Path::new("/external/c.rs"), &mut host).unwrap();
    app.new_tab(&mut host);
    // Switch active tab back to `b.rs` so `a.rs`, `c.rs`, and `Untitled` are inactive tabs.
    let buf_a = app.working_set.find_by_path(Path::new("/proj/src/a.rs")).unwrap();
    let buf_b = app.working_set.find_by_path(Path::new("/proj/src/b.rs")).unwrap();
    let buf_c = app.working_set.find_by_path(Path::new("/external/c.rs")).unwrap();
    let buf_untitled = app.active_buf_id;
    assert!(app.activate_buffer(buf_b, &mut host));
    app.render(&mut raster);

    let find_tab_center = |app: &EditorApp, target: editor_app::BufferId| -> (f32, f32) {
        let strip = app.layout.tab_strip;
        let y = (strip.y + strip.h / 2) as f32;
        for x in (strip.x + 4..strip.right() - 4).step_by(4) {
            if app.tab_strip.hit_test(x as f32, y) == editor_render::TabStripHit::Tab(target) {
                return (x as f32, y);
            }
        }
        panic!("tab {target:?} not found in tab strip");
    };

    let labels_of = |app: &EditorApp| -> Vec<String> {
        app.context_menu
            .entries()
            .iter()
            .filter_map(|e| match e {
                ContextMenuEntry::Item { label, .. } => Some(label.clone()),
                ContextMenuEntry::Separator => None,
            })
            .collect()
    };

    let click_menu_label = |app: &mut EditorApp,
                            target_label: &str,
                            raster: &mut BuiltinBitmapRasterizer,
                            clip: &mut MemoryClipboard,
                            host: &mut HeadlessHost| {
        app.render(raster);
        let id = app
            .context_menu
            .entries()
            .iter()
            .find_map(|e| match e {
                ContextMenuEntry::Item { id, label, .. } if label == target_label => Some(*id),
                _ => None,
            })
            .unwrap_or_else(|| panic!("menu label `{target_label}` not found"));
        let r = app.context_menu.item_rect(id).unwrap();
        app.handle_event(
            InputEvent::MouseDown {
                x_px: (r.x + r.w / 2) as f32,
                y_px: (r.y + r.h / 2) as f32,
                button: MouseButton::Left,
                click_count: 1,
                mods: Modifiers::NONE,
            },
            raster,
            clip,
            host,
        );
    };

    // 1. Right-click INACTIVE tab `a.rs`: must NOT steal focus from `b.rs`!
    let (ax, ay) = find_tab_center(&app, buf_a);
    app.handle_event(
        InputEvent::MouseDown {
            x_px: ax,
            y_px: ay,
            button: MouseButton::Right,
            click_count: 1,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.context_menu.is_open());
    assert_eq!(app.active_buf_id, buf_b, "right-clicking inactive tab must not steal focus");
    assert_eq!(
        labels_of(&app),
        vec![
            "Reveal in File Explorer",
            "Copy Path",
            "Copy Relative Path",
            "Pin Tab",
            "Split Right",
            "Close",
        ]
    );
    click_menu_label(&mut app, "Copy Relative Path", &mut raster, &mut clip, &mut host);
    assert_eq!(clip.get_text().as_deref(), Some("src/a.rs"));
    assert_eq!(app.active_buf_id, buf_b);

    // 2. Keyboard navigation (`Down` + `Enter`) activates first item ("Reveal in File Explorer").
    app.handle_event(
        InputEvent::MouseDown {
            x_px: ax,
            y_px: ay,
            button: MouseButton::Right,
            click_count: 1,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Down,
            mods: Modifiers::NONE,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Enter,
            mods: Modifiers::NONE,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(!app.context_menu.is_open());
    assert_eq!(
        host.last_revealed_path.as_deref(),
        Some(Path::new("/proj/src/a.rs"))
    );

    // 3. Pin inactive tab `a.rs` via context menu, then split right!
    app.handle_event(
        InputEvent::MouseDown {
            x_px: ax,
            y_px: ay,
            button: MouseButton::Right,
            click_count: 1,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    click_menu_label(&mut app, "Pin Tab", &mut raster, &mut clip, &mut host);
    assert!(app.working_set.is_pinned(buf_a));
    assert_eq!(app.active_buf_id, buf_b);
    app.render(&mut raster);

    let (ax2, ay2) = find_tab_center(&app, buf_a);
    app.handle_event(
        InputEvent::MouseDown {
            x_px: ax2,
            y_px: ay2,
            button: MouseButton::Right,
            click_count: 1,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(labels_of(&app).iter().any(|l| l == "Unpin Tab"));
    click_menu_label(&mut app, "Split Right", &mut raster, &mut clip, &mut host);
    assert_eq!(app.other_panes.len(), 1);
    assert_eq!(app.active_buf_id, buf_a);
    app.render(&mut raster);

    // 4. Right-click INACTIVE split-pane header (`b.rs`) -> Copy Path targets `b.rs`, active stays `a.rs`!
    let inactive_pane_id = app.other_panes[0].id;
    let header_rect = app.layout.grid.get(inactive_pane_id).unwrap().header;
    app.handle_event(
        InputEvent::MouseDown {
            x_px: (header_rect.x + header_rect.w / 2) as f32,
            y_px: (header_rect.y + header_rect.h / 2) as f32,
            button: MouseButton::Right,
            click_count: 1,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.context_menu.is_open());
    assert_eq!(app.active_buf_id, buf_a);
    click_menu_label(&mut app, "Copy Path", &mut raster, &mut clip, &mut host);
    assert_eq!(clip.get_text().as_deref(), Some("/proj/src/b.rs"));
    assert_eq!(app.active_buf_id, buf_a);

    // 5. External file tab (`/external/c.rs`): omits "Copy Relative Path".
    let (cx, cy) = find_tab_center(&app, buf_c);
    app.handle_event(
        InputEvent::MouseDown {
            x_px: cx,
            y_px: cy,
            button: MouseButton::Right,
            click_count: 1,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    let c_labels = labels_of(&app);
    assert!(c_labels.iter().any(|l| l == "Reveal in File Explorer"));
    assert!(c_labels.iter().any(|l| l == "Copy Path"));
    assert!(!c_labels.iter().any(|l| l == "Copy Relative Path"));
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Escape,
            mods: Modifiers::NONE,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );

    // 6. Untitled tab: omits Reveal, Copy Path, and Copy Relative Path cleanly.
    let (ux, uy) = find_tab_center(&app, buf_untitled);
    app.handle_event(
        InputEvent::MouseDown {
            x_px: ux,
            y_px: uy,
            button: MouseButton::Right,
            click_count: 1,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    let u_labels = labels_of(&app);
    assert!(!u_labels.iter().any(|l| l == "Reveal in File Explorer"));
    assert!(!u_labels.iter().any(|l| l == "Copy Path"));
    assert!(!u_labels.iter().any(|l| l == "Copy Relative Path"));
    assert!(u_labels.iter().any(|l| l == "Pin Tab"));
    assert!(u_labels.iter().any(|l| l == "Close"));
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Escape,
            mods: Modifiers::NONE,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );

    // 7. Empty space in sidebar: clears stale row selection so New File targets root,
    //    reveals/copies workspace root, and omits Copy Relative Path / Rename / Delete.
    app.sidebar
        .set_selected(Some(std::path::PathBuf::from("/proj/src/a.rs")));
    let sb = app.layout.sidebar;
    app.handle_event(
        InputEvent::MouseDown {
            x_px: (sb.x + sb.w / 2) as f32,
            y_px: (sb.bottom() - 40) as f32,
            button: MouseButton::Right,
            click_count: 1,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.context_menu.is_open());
    assert_eq!(
        app.sidebar.selected(),
        None,
        "right-clicking empty sidebar space must clear stale subfolder selection"
    );
    assert_eq!(
        labels_of(&app),
        vec![
            "New File…",
            "New Folder…",
            "Reveal in File Explorer",
            "Copy Path",
        ]
    );
    click_menu_label(
        &mut app,
        "Reveal in File Explorer",
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(host.last_revealed_path.as_deref(), Some(Path::new("/proj")));

    // 7b. Splitting an INACTIVE dirty split-pane buffer via header context menu preserves unsaved text!
    let pane_b_id = app.other_panes[0].id;
    app.other_panes[0]
        .doc
        .edit(0, 0, "// dirty in pane b\n", 1, false)
        .unwrap();
    let header_b = app.layout.grid.get(pane_b_id).unwrap().header;
    app.handle_event(
        InputEvent::MouseDown {
            x_px: (header_b.x + header_b.w / 2) as f32,
            y_px: (header_b.y + header_b.h / 2) as f32,
            button: MouseButton::Right,
            click_count: 1,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    click_menu_label(&mut app, "Split Down", &mut raster, &mut clip, &mut host);
    assert!(
        app.doc.to_string_lossy().starts_with("// dirty in pane b\n"),
        "splitting an inactive dirty pane must preserve its unsaved in-memory edits"
    );
    // Switch active back to `a.rs` for Command Palette check.
    assert!(app.activate_buffer(buf_a, &mut host));

    // 8. Command Palette exposes Copy path, Copy relative path, and OS reveal label!
    app.handle_event(
        InputEvent::Command(AppCommand::CommandPalette),
        &mut raster,
        &mut clip,
        &mut host,
    );
    let pal_titles: Vec<String> = app.palette.items.iter().map(|i| i.primary.clone()).collect();
    assert!(pal_titles.iter().any(|t| t == "Copy path"));
    assert!(pal_titles.iter().any(|t| t == "Copy relative path"));
    assert!(pal_titles.iter().any(|t| t == "Reveal in File Explorer"));
}

#[test]
fn real_disk_hot_exit_permissions_rename_undo_rekey_and_sibling_pane_save_sync() {
    use editor_platform::StdFileSystem;
    use std::path::PathBuf;

    let uniq = format!(
        "editor-audit-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let base = std::env::temp_dir().join(uniq);
    let proj = base.join("proj");
    let state_dir = base.join("state");
    std::fs::create_dir_all(&proj).unwrap();
    let notes_path = proj.join("notes.txt");
    let renamed_path = proj.join("renamed.txt");
    std::fs::write(&notes_path, "line1\nline2\n").unwrap();

    let fs: Arc<dyn editor_platform::FileSystem> = Arc::new(StdFileSystem);
    let mut raster = BuiltinBitmapRasterizer;
    let mut clip = MemoryClipboard::new();
    let mut host = HeadlessHost::new();
    host.state_dir = Some(state_dir.clone());

    {
        let mut app = EditorApp::new(Arc::clone(&fs), 1200, 800, 2.0);
        app.load_preferences(&host);
        app.open_folder(&proj, &mut host);
        app.open_file(&notes_path, &mut host).unwrap();

        // Set a manual JSON language override on `.txt`, edit + save so undo persists.
        app.doc
            .set_language(editor_core::Language::from_id("json").unwrap());
        app.handle_event(
            InputEvent::KeyDown {
                key: Key::Char('a'),
                mods: Modifiers::NONE,
                text: Some("{\"k\":1}\n".into()),
            },
            &mut raster,
            &mut clip,
            &mut host,
        );
        // Split right to view the same file in Pane 2 (clean once we save Pane 1).
        app.split_pane_with(Some(&notes_path), &mut host);
        // Focus back to Pane 1 (dirty) and save -> Pane 2 (clean) must sync immediately!
        let pane1_id = app.other_panes[0].id;
        app.focus_pane_id(pane1_id, &mut host);
        assert!(app.doc.is_dirty());
        app.handle_event(
            InputEvent::Command(AppCommand::Save),
            &mut raster,
            &mut clip,
            &mut host,
        );
        assert!(!app.doc.is_dirty());
        assert!(
            app.other_panes[0].doc.to_string_lossy().starts_with("{\"k\":1}\n"),
            "saving Pane 1 must immediately refresh clean sibling Pane 2 viewing the same file"
        );
        app.only_active_pane(&mut host);

        // Verify private 0o700/0o600 permissions on real disk undo dir & file (Unix).
        let undo_dir = editor_app::UndoStore::undo_dir(&state_dir);
        let canonical_notes = std::fs::canonicalize(&notes_path).unwrap();
        let old_key = editor_app::UndoStore::key_for_path(&canonical_notes);
        let old_undo_file = undo_dir.join(&old_key);
        assert!(old_undo_file.exists(), "undo file must exist on real disk after save");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir_mode = std::fs::metadata(&undo_dir).unwrap().permissions().mode() & 0o777;
            let file_mode = std::fs::metadata(&old_undo_file).unwrap().permissions().mode() & 0o777;
            assert_eq!(dir_mode, 0o700);
            assert_eq!(file_mode, 0o600);
        }

        // Rename `notes.txt` -> `renamed.txt` (same extension) via sidebar inline rename!
        app.sidebar.set_selected(Some(canonical_notes.clone()));
        assert!(app.sidebar.start_rename());
        app.handle_event(
            InputEvent::KeyDown {
                key: Key::Char('r'),
                mods: Modifiers::NONE,
                text: Some("renamed".into()),
            },
            &mut raster,
            &mut clip,
            &mut host,
        );
        app.handle_event(
            InputEvent::KeyDown {
                key: Key::Enter,
                mods: Modifiers::NONE,
                text: None,
            },
            &mut raster,
            &mut clip,
            &mut host,
        );
        let canonical_renamed = std::fs::canonicalize(&renamed_path).unwrap();
        assert_eq!(app.doc.path(), Some(canonical_renamed.as_path()));
        assert_eq!(
            app.doc.language().id(),
            "json",
            "same-extension rename must preserve manual Language override"
        );
        // Undo file must have been re-keyed to `renamed.txt` and old key GC'd!
        let new_key = editor_app::UndoStore::key_for_path(&canonical_renamed);
        assert!(undo_dir.join(&new_key).exists());
        assert!(!old_undo_file.exists());

        // Quit cleanly via will-terminate hook.
        app.handle_will_terminate(&mut host);
    }

    // Restart: open app and verify persisted undo survives across the rename!
    {
        let mut app2 = EditorApp::new(Arc::clone(&fs), 1200, 800, 2.0);
        app2.load_preferences(&host);
        app2.open_folder(&proj, &mut host);
        let canonical_renamed = std::fs::canonicalize(&renamed_path).unwrap();
        assert_eq!(app2.doc.path(), Some(canonical_renamed.as_path()));
        assert!(app2.doc.can_undo(), "re-keyed undo history must survive restart after rename");
        app2.handle_event(
            InputEvent::Command(AppCommand::Undo),
            &mut raster,
            &mut clip,
            &mut host,
        );
        assert_eq!(app2.doc.to_string_lossy(), "line1\nline2\n");

        // Close & discard -> zero leftover backup/undo files on disk!
        app2.handle_event(
            InputEvent::Command(AppCommand::Save),
            &mut raster,
            &mut clip,
            &mut host,
        );
        app2.request_close_buffer(app2.active_buf_id, &mut host);
        let undo_dir = editor_app::UndoStore::undo_dir(&state_dir);
        let backups_dir = editor_app::DraftStore::backups_dir(&state_dir);
        assert!(
            !undo_dir.exists() || std::fs::read_dir(&undo_dir).unwrap().count() == 0,
            "closing last file must leave zero stale .undo files"
        );
        assert!(
            !backups_dir.exists() || std::fs::read_dir(&backups_dir).unwrap().count() == 0,
            "saving and closing must leave zero stale .buf files"
        );
    }

    let _ = std::fs::remove_dir_all(PathBuf::from(&base));
}

#[test]
fn audit_regressions_crlf_enter_vim_visual_text_objects_and_zero_len_regex() {
    use std::path::Path;
    use editor_core::Document;
    use editor_platform::ipc::{IpcCommand, IpcReply};

    let fs = Arc::new(MemoryFileSystem::new());
    fs.add_file("/crlf.txt", b"hello\r\nworld\r\n".to_vec());
    let mut raster = BuiltinBitmapRasterizer;
    let mut clip = MemoryClipboard::new();
    let mut host = HeadlessHost::new();
    let mut app = EditorApp::new(fs.clone(), 960, 640, 2.0);
    app.open_file(Path::new("/crlf.txt"), &mut host).unwrap();

    // 1. Pressing Enter in a CRLF file places caret after `\r\n`, never between `\r` and `\n`.
    app.caret = 5;
    app.anchor = 5;
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Enter,
            mods: Modifiers::NONE,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.doc.to_string_lossy(), "hello\r\n\r\nworld\r\n");
    assert_eq!(app.caret, 7, "caret must land after full CRLF, not between \\r and \\n");

    // 2. Non-finite IPC floats (`NaN` / `inf`) are rejected cleanly without corrupting layout.
    let (r_nan, _, _) = app.execute_ipc(
        &IpcCommand::Action("sidebar-width:NaN".to_string()),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(matches!(r_nan, IpcReply::Error(_)));
    let (r_inf, _, _) = app.execute_ipc(
        &IpcCommand::Action("pane-ratio:cols=inf".to_string()),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(matches!(r_inf, IpcReply::Error(_)));

    // 3. NewTab clears extra_cursors so multi-cursor state never leaks into the new document.
    app.doc = Document::from_str("foo bar foo\n", None);
    app.caret = 0;
    app.anchor = 0;
    app.handle_event(
        InputEvent::Command(AppCommand::SelectNextOccurrence),
        &mut raster,
        &mut clip,
        &mut host,
    );
    app.handle_event(
        InputEvent::Command(AppCommand::SelectNextOccurrence),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(!app.extra_cursors.is_empty());
    app.handle_event(
        InputEvent::Command(AppCommand::NewTab),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.extra_cursors.is_empty());

    // 4. Zero-length regex replace (`^`) advances cleanly without infinite loop.
    app.doc = Document::from_str("aaa\nbbb\n", None);
    app.caret = 0;
    app.anchor = 0;
    app.handle_event(
        InputEvent::Command(AppCommand::Replace),
        &mut raster,
        &mut clip,
        &mut host,
    );
    app.find_bar.regex = true;
    app.find_bar.find_input.set_text("^");
    app.find_bar.replace_input.set_text("> ");
    app.refresh_search();
    assert_eq!(app.search_matches.len(), 3);
    app.render(&mut raster);
    let rbtn = app.find_bar.layout.btn_replace;
    app.handle_event(
        InputEvent::MouseDown {
            x_px: (rbtn.x + rbtn.w / 2) as f32,
            y_px: (rbtn.y + rbtn.h / 2) as f32,
            button: MouseButton::Left,
            click_count: 1,
            mods: Modifiers::NONE,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.doc.to_string_lossy(), "> aaa\nbbb\n");
    assert_eq!(app.active_match, Some(1), "must advance to second line match");
    // Close find bar before testing Vim viewport keys.
    app.handle_event(
        InputEvent::KeyDown {
            key: Key::Escape,
            mods: Modifiers::NONE,
            text: None,
        },
        &mut raster,
        &mut clip,
        &mut host,
    );

    // 5. Vim fixes: `di(` on empty line & multibyte `(café)`, charwise `p` UTF-8 boundary,
    // `dt)` deleting up to `)`, immediate `Vd` linewise delete, and failed `cfz` staying Normal.
    app.vim.set_enabled(true);
    let send_key = |app: &mut EditorApp,
                    c: char,
                    r: &mut BuiltinBitmapRasterizer,
                    cl: &mut MemoryClipboard,
                    h: &mut HeadlessHost| {
        let mut s = String::new();
        s.push(c);
        app.handle_event(
            InputEvent::KeyDown {
                key: Key::Char(c.to_ascii_lowercase()),
                mods: if c.is_ascii_uppercase() {
                    Modifiers {
                        shift: true,
                        ..Modifiers::NONE
                    }
                } else {
                    Modifiers::NONE
                },
                text: Some(s),
            },
            r,
            cl,
            h,
        );
    };

    // 5a. `di(` on empty line does not panic.
    app.doc = Document::from_str("\n", None);
    app.caret = 0;
    app.anchor = 0;
    for c in ['d', 'i', '('] {
        send_key(&mut app, c, &mut raster, &mut clip, &mut host);
    }
    assert_eq!(app.doc.to_string_lossy(), "\n");

    // 5b. `di(` on `(café)` deletes `"café"` cleanly.
    app.doc = Document::from_str("(café)", None);
    app.caret = 4; // inside `café` (at `é` byte offset 4)
    app.anchor = 4;
    for c in ['d', 'i', '('] {
        send_key(&mut app, c, &mut raster, &mut clip, &mut host);
    }
    assert_eq!(app.doc.to_string_lossy(), "()");

    // 5c. Charwise `p` with multi-byte `"café"` lands caret on a valid UTF-8 boundary.
    clip.set_text("café");
    app.vim.last_yank_linewise = false;
    app.doc = Document::from_str("ab", None);
    app.caret = 0;
    app.anchor = 0;
    send_key(&mut app, 'p', &mut raster, &mut clip, &mut host);
    assert_eq!(app.doc.to_string_lossy(), "acaféb");
    let text_now = app.doc.to_string_lossy();
    assert!(text_now.is_char_boundary(app.caret as usize));

    // 5d. `dt)` on `foo(bar)` from `'b'` deletes `"bar"`, keeping `)`.
    app.doc = Document::from_str("foo(bar)", None);
    app.caret = 4;
    app.anchor = 4;
    for c in ['d', 't', ')'] {
        send_key(&mut app, c, &mut raster, &mut clip, &mut host);
    }
    assert_eq!(app.doc.to_string_lossy(), "foo()");

    // 5e. Immediate `Vd` deletes the current line cleanly without needing a motion first.
    app.doc = Document::from_str("line1\nline2\n", None);
    app.caret = 0;
    app.anchor = 0;
    for c in ['V', 'd'] {
        send_key(&mut app, c, &mut raster, &mut clip, &mut host);
    }
    assert_eq!(app.doc.to_string_lossy(), "line2\n");
    assert!(app.vim.last_yank_linewise);

    // 5f. Failed `cfz` stays in Normal mode instead of dropping into Insert mode.
    app.doc = Document::from_str("hello", None);
    app.caret = 0;
    app.anchor = 0;
    for c in ['c', 'f', 'z'] {
        send_key(&mut app, c, &mut raster, &mut clip, &mut host);
    }
    assert_eq!(app.vim.mode, editor_app::VimMode::Normal);
    assert_eq!(app.doc.to_string_lossy(), "hello");
}
