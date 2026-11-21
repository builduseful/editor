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

    // Unsaved changes guard when opening another file via OpenPath.
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
    host.next_confirm = Some(ConfirmChoice::Cancel);
    app.handle_event(
        InputEvent::OpenPath(PathBuf::from("/work/src/other.rs")),
        &mut raster,
        &mut clip,
        &mut host,
    );
    assert_eq!(app.doc.file_name(), "main.rs", "Cancel must keep current dirty doc!");

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

    // 7. In-app confirmation modal opens on dirty doc switch and discards when pressing 'd'.
    assert!(app.doc.is_dirty());
    app.handle_event(
        InputEvent::OpenPath(PathBuf::from("/work/next.rs")),
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
