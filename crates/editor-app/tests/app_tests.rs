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
