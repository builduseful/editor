//! Pure-Rust Build Useful App Icon renderer, `.icns` encoder, and macOS `Editor.app` packager.
//!
//! Zero external tools or crates required:
//! - Renders the Build Useful brand app icon (512pt dark tile `#111111` with 112pt radius,
//!   crisp `#f1f1f1` caret + line motif inside the 320x320 safe square) with true transparent
//!   anti-aliased corners at every macOS icon resolution.
//! - Packs a valid macOS `.icns` binary directly in pure Rust (no `iconutil` dependency!).
//! - Assembles `target/release/Editor.app` and ad-hoc signs it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use editor_platform::Framebuffer;

const TILE_R: u8 = 0x11;
const TILE_G: u8 = 0x11;
const TILE_B: u8 = 0x11;

const MARK_R: u8 = 0xf1;
const MARK_G: u8 = 0xf1;
const MARK_B: u8 = 0xf1;

/// Distance from point `(px, py)` to segment `(x0, y0)-(x1, y1)`.
fn seg_dist(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32) -> f32 {
    let vx = x1 - x0;
    let vy = y1 - y0;
    let len_sq = (vx * vx + vy * vy).max(1e-6);
    let t = (((px - x0) * vx + (py - y0) * vy) / len_sq).clamp(0.0, 1.0);
    let dx = px - (x0 + t * vx);
    let dy = py - (y0 + t * vy);
    (dx * dx + dy * dy).sqrt()
}

/// Render the Build Useful app icon at `size x size` with true transparent AA corners.
fn render_icon_png(size: u32) -> Vec<u8> {
    let mut fb = Framebuffer::new(size, size, 1.0);
    let s = size as f32 / 512.0;

    // Tile bounds: full 512x512 with subtle 16pt macOS icon padding and 112pt corner radius.
    let pad = 16.0 * s;
    let half = (size as f32) * 0.5;
    let box_half = half - pad;
    let radius = 112.0 * s;
    let bx = (box_half - radius).max(0.0);

    // Caret + text-line motif inside the 320x320 safe square.
    let half_stroke = (14.0 * s).max(0.8);
    let caret_x = 186.0 * s;
    let caret_top = 152.0 * s;
    let caret_bot = 360.0 * s;
    let line_y = 256.0 * s;
    let line_start_x = 232.0 * s;
    let line_end_x = 344.0 * s;

    for y in 0..size {
        let py = y as f32 + 0.5;
        let ry = (py - half).abs() - bx;
        for x in 0..size {
            let px = x as f32 + 0.5;
            let rx = (px - half).abs() - bx;
            let qx = rx.max(0.0);
            let qy = ry.max(0.0);
            let tile_sdf = (qx * qx + qy * qy).sqrt() + rx.max(ry).min(0.0) - radius;
            let tile_cov = (0.5 - tile_sdf).clamp(0.0, 1.0);
            if tile_cov <= 0.0 {
                fb.pixels[(y * size + x) as usize] = 0x0000_0000;
                continue;
            }

            // Round-capped SDF for the vertical caret and horizontal line motif.
            let d1 = seg_dist(px, py, caret_x, caret_top, caret_x, caret_bot) - half_stroke;
            let d2 = seg_dist(px, py, line_start_x, line_y, line_end_x, line_y) - half_stroke;
            let mark_cov = (0.5 - d1.min(d2)).clamp(0.0, 1.0);

            let r = (TILE_R as f32 * (1.0 - mark_cov) + MARK_R as f32 * mark_cov).round() as u32;
            let g = (TILE_G as f32 * (1.0 - mark_cov) + MARK_G as f32 * mark_cov).round() as u32;
            let b = (TILE_B as f32 * (1.0 - mark_cov) + MARK_B as f32 * mark_cov).round() as u32;
            let a = (tile_cov * 255.0).round() as u32;
            // Framebuffer stores 0xAARRGGBB; encode_png reads straight RGBA bytes from it.
            fb.pixels[(y * size + x) as usize] = (a << 24) | (r << 16) | (g << 8) | b;
        }
    }

    fb.encode_png()
}

/// Encode a standard macOS `.icns` archive directly from PNG payloads (zero external tools).
fn build_icns() -> Vec<u8> {
    let slots: &[(&[u8; 4], u32)] = &[
        (b"icp4", 16),
        (b"ic11", 32),  // 16@2x
        (b"icp5", 32),
        (b"ic12", 64),  // 32@2x
        (b"ic07", 128),
        (b"ic13", 256), // 128@2x
        (b"ic08", 256),
        (b"ic14", 512), // 256@2x
        (b"ic09", 512),
        (b"ic10", 1024), // 512@2x
    ];

    let mut body = Vec::new();
    for &(tag, sz) in slots {
        let png = render_icon_png(sz);
        let chunk_len = (8 + png.len()) as u32;
        body.extend_from_slice(tag);
        body.extend_from_slice(&chunk_len.to_be_bytes());
        body.extend_from_slice(&png);
    }

    let total_len = (8 + body.len()) as u32;
    let mut out = Vec::with_capacity(total_len as usize);
    out.extend_from_slice(b"icns");
    out.extend_from_slice(&total_len.to_be_bytes());
    out.extend_from_slice(&body);
    out
}

fn write_info_plist(dst: &Path) {
    let plist = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>en</string>
    <key>CFBundleDisplayName</key>
    <string>editor</string>
    <key>CFBundleExecutable</key>
    <string>editor-gui</string>
    <key>CFBundleIconFile</key>
    <string>AppIcon</string>
    <key>CFBundleIdentifier</key>
    <string>com.builduseful.editor</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>editor</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>0.2.0</string>
    <key>CFBundleVersion</key>
    <string>2</string>
    <key>LSMinimumSystemVersion</key>
    <string>13.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>CFBundleDocumentTypes</key>
    <array>
        <dict>
            <key>CFBundleTypeName</key>
            <string>Text Document</string>
            <key>CFBundleTypeRole</key>
            <string>Editor</string>
            <key>LSHandlerRank</key>
            <string>Alternate</string>
            <key>LSItemContentTypes</key>
            <array>
                <string>public.text</string>
                <string>public.plain-text</string>
                <string>public.source-code</string>
                <string>public.utf8-plain-text</string>
                <string>public.folder</string>
            </array>
        </dict>
    </array>
</dict>
</plist>
"#;
    fs::write(dst, plist).expect("failed to write Info.plist");
}

fn main() {
    let target_dir = PathBuf::from("target/release");
    let bin_src = target_dir.join("editor-gui");
    if !bin_src.exists() {
        eprintln!("error: build release binary first (`cargo build --release --workspace`)");
        std::process::exit(1);
    }

    let app_dir = target_dir.join("Editor.app");
    let contents = app_dir.join("Contents");
    let macos_dir = contents.join("MacOS");
    let res_dir = contents.join("Resources");

    let _ = fs::remove_dir_all(&app_dir);
    fs::create_dir_all(&macos_dir).expect("create MacOS dir");
    fs::create_dir_all(&res_dir).expect("create Resources dir");

    fs::copy(&bin_src, macos_dir.join("editor-gui")).expect("copy editor-gui binary");
    write_info_plist(&contents.join("Info.plist"));
    fs::write(res_dir.join("AppIcon.icns"), build_icns()).expect("write AppIcon.icns");

    // Also emit a preview PNG if requested via `--preview-icon PATH`.
    let args: Vec<String> = std::env::args().collect();
    if let Some(pos) = args.iter().position(|a| a == "--preview-icon") {
        if let Some(out) = args.get(pos + 1) {
            fs::write(out, render_icon_png(512)).expect("write preview icon");
        }
    }

    // Best-effort ad-hoc signing so macOS LaunchServices accepts the bundle cleanly.
    let _ = Command::new("/usr/bin/codesign")
        .args(["--force", "--deep", "--sign", "-"])
        .arg(&app_dir)
        .status();

    println!("Packaged {}", app_dir.display());
}
