use std::path::PathBuf;
use std::sync::Arc;

use editor_app::EditorApp;
use editor_platform::{
    AppCommand, BuiltinBitmapRasterizer, ConfirmChoice, HeadlessHost, InputEvent, Key,
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
