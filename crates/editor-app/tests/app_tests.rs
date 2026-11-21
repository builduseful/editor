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
fn harpoon_working_set_fullscreen_flicking_zoom_overlay_and_tab_drag_to_split() {
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

    // 3. Pin 3 files into Harpoon slots 1, 2, 3 and flick between them in fullscreen!
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

    // 5. `⌘E` Harpoon Overlay: reorder Pin 1 <-> Pin 2 with `⌥↓`, then jump via `1`!
    app.handle_event(
        InputEvent::Command(AppCommand::JumpPin(1)),
        &mut raster,
        &mut clip,
        &mut host,
    );
    app.handle_event(
        InputEvent::Command(AppCommand::ShowHarpoon),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert!(app.harpoon.visible);
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
        "Option+Down in Harpoon overlay must swap slot 1 and slot 2"
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
    assert!(!app.harpoon.visible);
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
    assert_eq!(app1.working_set.pinned.len(), 2, "switching back to Project A restores its Harpoon pins");
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
