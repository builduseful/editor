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
