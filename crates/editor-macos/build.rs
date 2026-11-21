use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=objc/mac_adapter.m");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "macos" {
        return;
    }

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let obj_path = out_dir.join("mac_adapter.o");
    let lib_path = out_dir.join("libmac_adapter.a");

    let status = Command::new("/usr/bin/clang")
        .args([
            "-c",
            "-fobjc-arc",
            "-O2",
            "-Wall",
            "-Wextra",
            "objc/mac_adapter.m",
            "-o",
        ])
        .arg(&obj_path)
        .status()
        .expect("failed to invoke clang");
    assert!(status.success(), "clang failed compiling objc/mac_adapter.m");

    let ar_status = Command::new("/usr/bin/ar")
        .args(["rcs"])
        .arg(&lib_path)
        .arg(&obj_path)
        .status()
        .expect("failed to invoke ar");
    assert!(ar_status.success(), "ar failed archiving libmac_adapter.a");

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=mac_adapter");
    println!("cargo:rustc-link-lib=framework=AppKit");
    println!("cargo:rustc-link-lib=framework=CoreGraphics");
    println!("cargo:rustc-link-lib=framework=CoreText");
    println!("cargo:rustc-link-lib=framework=QuartzCore");
    println!("cargo:rustc-link-lib=framework=Foundation");
}
