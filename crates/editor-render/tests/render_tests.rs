use std::path::PathBuf;

use editor_platform::{
    BuiltinBitmapRasterizer, FontStyle, Framebuffer, Key, MemoryFileSystem, Modifiers, PixelRect,
};
use editor_render::{
    Color, GlyphAtlas, Painter, Sidebar, SidebarAction, TextInput, Theme,
};

#[test]
fn theme_tokens_match_build_useful_palette() {
    let light = Theme::light();
    assert_eq!(light.surface, Color::rgb(0xff, 0xff, 0xff));
    assert_eq!(light.text, Color::rgb(0x17, 0x1a, 0x1f));
    assert_eq!(light.selection, Color::rgb(0xdf, 0xe4, 0xea));
    assert_eq!(light.page, Color::rgb(0xf6, 0xf7, 0xf9));

    let dark = Theme::dark();
    assert_eq!(dark.surface, Color::rgb(0x1b, 0x1b, 0x1b));
    assert_eq!(dark.text, Color::rgb(0xf1, 0xf1, 0xf1));
    assert_eq!(dark.selection, Color::rgb(0x30, 0x30, 0x30));
    assert_eq!(dark.page, Color::rgb(0x11, 0x11, 0x11));
}

#[test]
fn painter_clipping_rounded_rect_scroll_blit_and_png_encoding() {
    let mut fb = Framebuffer::new(120, 80, 1.0);
    let mut atlas = GlyphAtlas::new();
    let mut raster = BuiltinBitmapRasterizer;
    let theme = Theme::light();

    {
        let mut p = Painter::new(&mut fb);
        p.fill_rect(PixelRect::new(0, 0, 120, 80), theme.surface);
        p.push_clip(PixelRect::new(10, 10, 40, 40));
        p.fill_rect(PixelRect::new(0, 0, 120, 80), theme.selection);
        p.pop_clip();
        p.fill_rounded_rect(PixelRect::new(60, 10, 40, 20), 6.0, theme.text);
        p.draw_text_ui(
            12.0,
            66.0,
            "Hello 🚀",
            FontStyle::Ui,
            14.0,
            theme.text,
            &mut atlas,
            &mut raster,
        );
    }

    // Outside clip stayed surface white; inside clip got selection fill.
    assert_eq!(fb.pixel_at(5, 5), theme.surface.to_premul_u32());
    assert_eq!(fb.pixel_at(20, 20), theme.selection.to_premul_u32());
    // Center of rounded rect is opaque text ink.
    assert_eq!(fb.pixel_at(80, 20), theme.text.to_premul_u32());

    // Valid PNG signature + IHDR + IEND!
    let png = fb.encode_png();
    assert_eq!(&png[0..8], &[137, 80, 78, 71, 13, 10, 26, 10]);
    assert_eq!(&png[12..16], b"IHDR");
    assert!(png.ends_with(b"IEND\xAE\x42\x60\x82"));
}

#[test]
fn sidebar_is_lazy_and_navigation_preserves_expansions_and_selection() {
    let fs = MemoryFileSystem::new();
    fs.add_file("/proj/README.md", b"# proj".to_vec());
    fs.add_file("/proj/src/main.rs", b"fn main() {}".to_vec());
    fs.add_file("/proj/src/nested/deep.rs", b"pub fn deep() {}".to_vec());
    fs.add_file("/proj/other/hidden.rs", b"".to_vec());

    let mut sb = Sidebar::new();
    sb.set_root(&fs, &PathBuf::from("/proj"));
    // Initially only top-level rows visible (other, src, README.md) — folders first!
    let names: Vec<&str> = sb.rows().iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, vec!["other", "src", "README.md"]);

    // Expand `src` inline.
    sb.toggle_expand(&fs, &PathBuf::from("/proj/src"));
    let names: Vec<&str> = sb.rows().iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, vec!["other", "src", "nested", "main.rs", "README.md"]);

    // Reveal a file under `src/nested`.
    sb.reveal(&fs, &PathBuf::from("/proj/src/nested/deep.rs"));
    assert_eq!(sb.selected(), Some(PathBuf::from("/proj/src/nested/deep.rs").as_path()));

    // Navigate INTO `/proj/src`, then back OUT to `/proj`: expansions & selection survive!
    sb.navigate_to(&fs, &PathBuf::from("/proj/src"));
    assert_eq!(sb.root(), Some(PathBuf::from("/proj/src").as_path()));
    sb.navigate_to(&fs, &PathBuf::from("/proj"));
    assert_eq!(sb.selected(), Some(PathBuf::from("/proj/src/nested/deep.rs").as_path()));
    assert!(sb.rows().iter().any(|r| r.name == "deep.rs"));

    // Width clamping: 160..=480 pt.
    sb.set_width_pt(50.0);
    assert_eq!(sb.width_pt(), 160.0);
    sb.set_width_pt(900.0);
    assert_eq!(sb.width_pt(), 480.0);

    // Keyboard Enter on selected file returns OpenFile; Space returns PreviewFile; Left does not escape root.
    let act = sb.handle_key(&fs, Key::Enter, Modifiers::NONE, 400);
    assert_eq!(act, SidebarAction::OpenFile(PathBuf::from("/proj/src/nested/deep.rs")));
    let prev = sb.handle_key(&fs, Key::Char(' '), Modifiers::NONE, 400);
    assert_eq!(prev, SidebarAction::PreviewFile(PathBuf::from("/proj/src/nested/deep.rs")));
    assert_eq!(sb.handle_key(&fs, Key::F(2), Modifiers::NONE, 400), SidebarAction::StartRename);
    assert_eq!(sb.handle_key(&fs, Key::Home, Modifiers::NONE, 400), SidebarAction::Changed);
    assert_eq!(sb.selected(), Some(PathBuf::from("/proj/other").as_path()));
    assert_eq!(sb.handle_key(&fs, Key::Left, Modifiers::NONE, 400), SidebarAction::None);
    assert_eq!(sb.root(), Some(PathBuf::from("/proj").as_path()));
    assert_eq!(sb.handle_key(&fs, Key::End, Modifiers::NONE, 400), SidebarAction::Changed);
    assert_eq!(sb.selected(), Some(PathBuf::from("/proj/README.md").as_path()));
}

#[test]
fn text_input_edits_selects_and_copies() {
    let mut input = TextInput::new("Find");
    input.focused = true;
    input.handle_key(Key::Char('a'), Modifiers::NONE, Some("hello 🚀"), None);
    assert_eq!(input.text, "hello 🚀");
    // Backspace deletes whole emoji grapheme, not 1 UTF-8 byte!
    input.handle_key(Key::Backspace, Modifiers::NONE, None, None);
    assert_eq!(input.text, "hello ");

    let primary = Modifiers {
        #[cfg(target_os = "macos")]
        meta: true,
        #[cfg(not(target_os = "macos"))]
        ctrl: true,
        ..Modifiers::NONE
    };
    input.handle_key(Key::Char('a'), primary, None, None);
    let (_, copied) = input.handle_key(Key::Char('c'), primary, None, None);
    assert_eq!(copied.as_deref(), Some("hello "));
}

#[test]
fn viewport_vertical_scroll_clamps_flush_to_bottom_with_no_excess_whitespace() {
    use editor_core::Document;
    use editor_platform::GlyphRasterizer;
    use editor_render::Viewport;

    let mut raster = BuiltinBitmapRasterizer;
    let metrics = raster.metrics(FontStyle::Mono, 13.5);
    let line_h = metrics.line_height as f32;
    let rect = PixelRect::new(0, 0, 800, 400);

    // 1. Short file (5 lines, fits inside 400px viewport) must NEVER scroll down!
    let mut short_doc = Document::from_str("a\nb\nc\nd\ne\n", None);
    let mut vp = Viewport::new();
    vp.compute_layout(rect, &short_doc, metrics, 1.0);
    vp.scroll_by(0.0, 5000.0, &mut short_doc, metrics, 1.0);
    assert_eq!(vp.scroll_top_line, 0);
    assert_eq!(vp.scroll_sub_y_px, 0.0);

    // 2. Long file (100 lines) scrolled way past bottom stops flush with last line at bottom!
    let mut text = String::new();
    for i in 0..100 {
        text.push_str(&format!("line {i}\n"));
    }
    let mut long_doc = Document::from_str(&text, None);
    vp.compute_layout(rect, &long_doc, metrics, 1.0);
    vp.scroll_by(0.0, 100_000.0, &mut long_doc, metrics, 1.0);

    let text_h = vp.layout.text.h as f32;
    let total_h = long_doc.editable_line_count() as f32 * line_h;
    let cur_scroll = vp.scroll_top_line as f32 * line_h + vp.scroll_sub_y_px;
    let expected_max = (total_h - text_h).max(0.0);
    assert!(
        (cur_scroll - expected_max).abs() <= 1.0,
        "scroll={cur_scroll} expected={expected_max} (should leave zero bottom void)"
    );

    // 3. Word-wrapped document with FEWER logical lines than viewport height, but MORE visual
    // wrapped lines than viewport height: MUST show a vertical scrollbar, scroll via wheel,
    // and auto-scroll when the caret moves down to the bottom wrapped rows!
    let mut wrapped_text = String::new();
    for i in 0..12 {
        wrapped_text.push_str(&format!(
            "Paragraph {i}: the quick brown fox jumps over the lazy dog repeatedly to force multiple wrapped visual lines inside a narrow pane.\n"
        ));
    }
    let mut wrapped_doc = Document::from_str(&wrapped_text, None);
    let narrow = PixelRect::new(0, 0, 320, 260);
    let mut wvp = Viewport::new();
    wvp.word_wrap = true;
    let mut fb = Framebuffer::new(320, 260, 1.0);
    let mut atlas = GlyphAtlas::new();
    let theme = Theme::light();
    {
        let mut p = Painter::new(&mut fb);
        wvp.draw(
            &mut p,
            narrow,
            &mut wrapped_doc,
            0,
            None,
            &[],
            None,
            None,
            &editor_render::ImeComposition::default(),
            true,
            true,
            &theme,
            &mut atlas,
            &mut raster,
        );
    }
    assert!(
        !wvp.layout.v_thumb.is_empty(),
        "wrapped document overflowing viewport visually must display vertical scrollbar thumb"
    );

    // Moving caret to the last logical line (11 < visible logical lines) MUST scroll down!
    let last_byte = wrapped_doc.len_bytes();
    wvp.ensure_caret_visible(&mut wrapped_doc, last_byte, metrics, 1.0);
    assert!(
        wvp.scroll_top_line > 0 || wvp.scroll_sub_y_px > 0.0,
        "ensure_caret_visible must scroll down when wrapped lines push caret past viewport bottom"
    );

    // Wheel scrolling down from top must advance smoothly through wrapped visual rows.
    wvp.scroll_top_line = 0;
    wvp.scroll_sub_y_px = 0.0;
    wvp.scroll_by(0.0, 200.0, &mut wrapped_doc, metrics, 1.0);
    assert!(
        wvp.scroll_top_line > 0 || wvp.scroll_sub_y_px > 0.0,
        "scroll_by must scroll wrapped content even when logical lines < viewport height"
    );
}

#[test]
fn tab_strip_hit_testing_drop_targets_and_overflow_scroll() {
    use editor_render::{StripDropTarget, TabDisplayItem, TabStrip, TabStripHit};

    let mut fb = Framebuffer::new(900, 80, 1.0);
    let mut atlas = GlyphAtlas::new();
    let mut raster = BuiltinBitmapRasterizer;
    let theme = Theme::light();
    let rect = PixelRect::new(0, 0, 900, 30);

    let items = vec![
        TabDisplayItem {
            id: 1,
            label: "alpha.rs".into(),
            pin_slot: Some(1),
            dirty: false,
            is_preview: false,
            is_active: true,
            in_other_pane: false,
        },
        TabDisplayItem {
            id: 2,
            label: "beta.rs".into(),
            pin_slot: Some(2),
            dirty: true,
            is_preview: false,
            is_active: false,
            in_other_pane: true,
        },
        TabDisplayItem {
            id: 3,
            label: "gamma.rs".into(),
            pin_slot: None,
            dirty: false,
            is_preview: false,
            is_active: false,
            in_other_pane: false,
        },
        TabDisplayItem {
            id: 4,
            label: "preview.md".into(),
            pin_slot: None,
            dirty: false,
            is_preview: true,
            is_active: false,
            in_other_pane: false,
        },
    ];

    let mut strip = TabStrip::new();
    {
        let mut p = Painter::new(&mut fb);
        strip.draw(&mut p, rect, &items, true, false, false, None, None, &theme, &mut atlas, &mut raster);
    }

    // First tab hit & right-edge utility buttons (menu, zoom, new tab) all hit-test accurately!
    assert_eq!(strip.hit_test(35.0, 15.0), TabStripHit::Tab(1));
    assert_eq!(strip.hit_test(884.0, 15.0), TabStripHit::OpenMenu);
    assert_eq!(strip.hit_test(858.0, 15.0), TabStripHit::ToggleZoom);
    assert_eq!(strip.hit_test(832.0, 15.0), TabStripHit::NewTab);

    // When sidebar is minimized on Left or Right, the quiet expand button hit-tests cleanly!
    {
        let mut p = Painter::new(&mut fb);
        strip.draw(
            &mut p,
            rect,
            &items,
            false,
            false,
            true,
            None,
            Some(editor_render::SidebarSide::Left),
            &theme,
            &mut atlas,
            &mut raster,
        );
    }
    assert_eq!(strip.hit_test(14.0, 15.0), TabStripHit::ToggleSidebar);
    {
        let mut p = Painter::new(&mut fb);
        strip.draw(
            &mut p,
            rect,
            &items,
            false,
            false,
            true,
            None,
            Some(editor_render::SidebarSide::Right),
            &theme,
            &mut atlas,
            &mut raster,
        );
    }
    assert_eq!(strip.hit_test(884.0, 15.0), TabStripHit::ToggleSidebar);
    {
        let mut p = Painter::new(&mut fb);
        strip.draw(&mut p, rect, &items, true, false, false, None, None, &theme, &mut atlas, &mut raster);
    }

    // Drop target left of divider resolves PinnedAt, right of divider resolves OpenAt!
    let (left_target, _) = strip.drop_target_at(20.0, 15.0).expect("left drop");
    assert_eq!(left_target, StripDropTarget::PinnedAt(0));
    let (right_target, _) = strip.drop_target_at(380.0, 15.0).expect("right drop");
    assert!(matches!(right_target, StripDropTarget::OpenAt(_)));

    // Overflow scroll clamps cleanly when many tabs are open.
    let many: Vec<TabDisplayItem> = (1..=16)
        .map(|i| TabDisplayItem {
            id: i,
            label: format!("module_{i:02}.rs"),
            pin_slot: None,
            dirty: false,
            is_preview: false,
            is_active: i == 1,
            in_other_pane: false,
        })
        .collect();
    {
        let mut p = Painter::new(&mut fb);
        strip.draw(&mut p, rect, &many, false, false, true, None, None, &theme, &mut atlas, &mut raster);
    }
    strip.scroll_by(10_000.0);
    assert!(strip.scroll_x_px > 0.0);
    strip.scroll_by(-10_000.0);
    assert_eq!(strip.scroll_x_px, 0.0);
}

#[test]
fn pins_overlay_navigation_reorder_split_and_remove() {
    use editor_render::{PaletteOpenTarget, PinEntry, PinsAction, PinsOverlay};

    let entries = vec![
        PinEntry {
            id: 10,
            name: "a.rs".into(),
            rel_path: "src/a.rs".into(),
            pin_slot: Some(1),
            dirty: false,
            is_preview: false,
            is_active: true,
            in_other_pane: false,
        },
        PinEntry {
            id: 20,
            name: "b.rs".into(),
            rel_path: "src/b.rs".into(),
            pin_slot: Some(2),
            dirty: true,
            is_preview: false,
            is_active: false,
            in_other_pane: false,
        },
        PinEntry {
            id: 30,
            name: "c.rs".into(),
            rel_path: "src/c.rs".into(),
            pin_slot: None,
            dirty: false,
            is_preview: true,
            is_active: false,
            in_other_pane: false,
        },
    ];

    let mut overlay = PinsOverlay::new();
    overlay.open(entries.clone());
    assert!(overlay.visible);

    // Move down with `j`, then `J` reorders slot 2 up/down (`MovePin { slot_0based: 1, delta: 1 }`).
    assert_eq!(overlay.handle_key(Key::Char('j'), Modifiers::NONE), PinsAction::Changed);
    assert_eq!(
        overlay.handle_key(
            Key::Char('J'),
            Modifiers {
                shift: true,
                ..Modifiers::NONE
            }
        ),
        PinsAction::MovePin {
            slot_0based: 1,
            delta: 1,
        }
    );
    // `p` toggles pin on selected row (id 20); `d` removes/unpins it.
    assert_eq!(
        overlay.handle_key(Key::Char('p'), Modifiers::NONE),
        PinsAction::TogglePin(20)
    );
    assert_eq!(
        overlay.handle_key(Key::Char('d'), Modifiers::NONE),
        PinsAction::RemoveOrClose(20)
    );

    // `⌃N` / `⌃P` navigate consistently with Quick Open, and `⌃X` splits down!
    let ctrl_only = Modifiers {
        ctrl: true,
        ..Modifiers::NONE
    };
    assert_eq!(overlay.handle_key(Key::Char('n'), ctrl_only), PinsAction::Changed);
    assert_eq!(overlay.handle_key(Key::Char('p'), ctrl_only), PinsAction::Changed);
    assert_eq!(
        overlay.handle_key(Key::Char('x'), ctrl_only),
        PinsAction::JumpToBuffer {
            id: 20,
            target: PaletteOpenTarget::SplitDown,
        }
    );
    assert!(!overlay.visible);

    // Reopen and verify `⌘Enter` / `⌃V` split right and close overlay!
    overlay.open(entries);
    overlay.handle_key(Key::Char('j'), Modifiers::NONE);
    let primary = Modifiers {
        #[cfg(target_os = "macos")]
        meta: true,
        #[cfg(not(target_os = "macos"))]
        ctrl: true,
        ..Modifiers::NONE
    };
    assert_eq!(
        overlay.handle_key(Key::Enter, primary),
        PinsAction::JumpToBuffer {
            id: 20,
            target: PaletteOpenTarget::SplitRight,
        }
    );
    assert!(!overlay.visible);
}

#[test]
fn palette_respects_os_home_ignore_rules_gitignore_and_async_streaming() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use editor_platform::IgnoreRules;
    use editor_render::CommandPalette;

    let fs = Arc::new(MemoryFileSystem::new());
    let home = PathBuf::from("/mock_home/dev");
    // Home-dir OS vaults & hidden toolchains must be skipped at `$HOME`.
    fs.add_file(home.join("Library/Caches/huge.txt"), b"needle_ignored_lib".to_vec());
    fs.add_file(home.join(".cargo/registry/crate.rs"), b"needle_ignored_cargo".to_vec());
    fs.add_file(home.join("Apps/MyTool.app/Contents/Info.plist"), b"needle_ignored_app".to_vec());
    // Real user workspace files (including a project's own `Library/` and `.github/`!) must be kept.
    fs.add_file(home.join("Projects/app/.gitignore"), b"*.log\ngenerated/\n!keep.log\n".to_vec());
    fs.add_file(home.join("Projects/app/src/main.rs"), b"pub fn needle_real() {}\n".to_vec());
    fs.add_file(home.join("Projects/app/Library/book.rs"), b"pub fn needle_project_lib() {}\n".to_vec());
    fs.add_file(home.join("Projects/app/.github/ci.yml"), b"name: needle_ci\n".to_vec());
    fs.add_file(home.join("Projects/app/debug.log"), b"needle_gitignored_log\n".to_vec());
    fs.add_file(home.join("Projects/app/keep.log"), b"needle_unignored_log\n".to_vec());
    fs.add_file(home.join("Projects/app/generated/out.rs"), b"needle_gitignored_dir\n".to_vec());

    let mut palette = CommandPalette::new();
    palette.set_ignore_rules(IgnoreRules::macos(Some(home.clone())));

    // 1. Verify Quick Open & Search in Files prune OS vaults + hidden toolchains + `.gitignore`.
    palette.open_project_search(fs.as_ref(), Some(&home), Some("needle_"));
    let paths: Vec<String> = palette
        .items
        .iter()
        .map(|it| it.path.to_string_lossy().into_owned())
        .collect();
    assert!(paths.iter().any(|p| p.ends_with("src/main.rs")));
    assert!(
        paths.iter().any(|p| p.ends_with("Projects/app/Library/book.rs")),
        "project subfolder named Library must NOT be skipped: {paths:?}"
    );
    assert!(
        paths.iter().any(|p| p.ends_with(".github/ci.yml")),
        "project .github folder should stay searchable: {paths:?}"
    );
    assert!(
        paths.iter().any(|p| p.ends_with("keep.log")),
        "negated !keep.log in .gitignore should stay searchable: {paths:?}"
    );
    assert!(
        !paths.iter().any(|p| p.contains("/Library/Caches/")),
        "$HOME/Library must be pruned: {paths:?}"
    );
    assert!(
        !paths.iter().any(|p| p.contains(".cargo")),
        "$HOME/.cargo must be pruned: {paths:?}"
    );
    assert!(
        !paths.iter().any(|p| p.contains(".app/")),
        "macOS .app bundles must be pruned: {paths:?}"
    );
    assert!(
        !paths.iter().any(|p| p.ends_with("debug.log") || p.contains("/generated/")),
        ".gitignore rules must be respected: {paths:?}"
    );
    palette.close();

    // 2. Verify async runtime path (with waker) opens cleanly, streams to completion, and stops on close.
    let wakes = Arc::new(AtomicUsize::new(0));
    let wakes_clone = Arc::clone(&wakes);
    palette.set_async_runtime(Some((
        fs.clone(),
        Arc::new(move || {
            wakes_clone.fetch_add(1, Ordering::Relaxed);
        }),
    )));
    palette.invalidate_file_index();
    palette.open_project_search(fs.as_ref(), Some(&home), Some("needle_real"));
    for _ in 0..50 {
        palette.poll_background(fs.as_ref());
        if !palette.is_streaming() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(palette.items.len(), 1);
    assert!(palette.items[0].path.ends_with("src/main.rs"));
    palette.close();
    assert!(!palette.is_streaming());
}
