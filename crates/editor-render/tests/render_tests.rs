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

    // Keyboard Enter on selected file returns OpenFile.
    let act = sb.handle_key(&fs, Key::Enter, 400);
    assert_eq!(act, SidebarAction::OpenFile(PathBuf::from("/proj/src/nested/deep.rs")));
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
fn harpoon_overlay_navigation_reorder_split_and_remove() {
    use editor_render::{HarpoonAction, HarpoonEntry, HarpoonOverlay};

    let entries = vec![
        HarpoonEntry {
            id: 10,
            name: "a.rs".into(),
            rel_path: "src/a.rs".into(),
            pin_slot: Some(1),
            dirty: false,
            is_preview: false,
            is_active: true,
            in_other_pane: false,
        },
        HarpoonEntry {
            id: 20,
            name: "b.rs".into(),
            rel_path: "src/b.rs".into(),
            pin_slot: Some(2),
            dirty: true,
            is_preview: false,
            is_active: false,
            in_other_pane: false,
        },
        HarpoonEntry {
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

    let mut overlay = HarpoonOverlay::new();
    overlay.open(entries);
    assert!(overlay.visible);

    // Move down with `j`, then `J` reorders slot 2 up/down (`MovePin { slot_0based: 1, delta: 1 }`).
    assert_eq!(overlay.handle_key(Key::Char('j'), Modifiers::NONE), HarpoonAction::Changed);
    assert_eq!(
        overlay.handle_key(
            Key::Char('J'),
            Modifiers {
                shift: true,
                ..Modifiers::NONE
            }
        ),
        HarpoonAction::MovePin {
            slot_0based: 1,
            delta: 1,
        }
    );
    // `p` toggles pin on selected row (id 20); `d` removes/unpins it.
    assert_eq!(
        overlay.handle_key(Key::Char('p'), Modifiers::NONE),
        HarpoonAction::TogglePin(20)
    );
    assert_eq!(
        overlay.handle_key(Key::Char('d'), Modifiers::NONE),
        HarpoonAction::RemoveOrClose(20)
    );

    // `⌘Enter` splits right and closes overlay!
    let primary = Modifiers {
        #[cfg(target_os = "macos")]
        meta: true,
        #[cfg(not(target_os = "macos"))]
        ctrl: true,
        ..Modifiers::NONE
    };
    assert_eq!(
        overlay.handle_key(Key::Enter, primary),
        HarpoonAction::JumpToBuffer {
            id: 20,
            split_right: true,
        }
    );
    assert!(!overlay.visible);
}
