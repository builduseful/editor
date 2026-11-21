use std::process::Command;

#[test]
fn cli_round_trip_write_info_read_edit_validate() {
    let bin = env!("CARGO_BIN_EXE_editor");
    let tmp = std::env::temp_dir().join(format!("editor_cli_test_{}.rs", std::process::id()));
    let _ = std::fs::remove_file(&tmp);

    // 1. Version
    let out = Command::new(bin).arg("version").output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("\"ok\":true"));

    // 2. Write new file from @FILE
    let payload = std::env::temp_dir().join(format!("editor_cli_payload_{}.txt", std::process::id()));
    std::fs::write(&payload, "fn main() {\n    println!(\"hello 🚀\");\n}\n").unwrap();
    let out = Command::new(bin)
        .args(["write", tmp.to_str().unwrap(), &format!("@{}", payload.display())])
        .output()
        .unwrap();
    assert!(out.status.success(), "write failed: {}", String::from_utf8_lossy(&out.stdout));
    let _ = std::fs::remove_file(&payload);

    // 3. Info
    let out = Command::new(bin)
        .args(["info", tmp.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let info_json = String::from_utf8(out.stdout).unwrap();
    assert!(info_json.contains("\"line_count\":3"));
    assert!(info_json.contains("\"eol\":\"lf\""));

    // 4. Read lines 1..2
    let out = Command::new(bin)
        .args(["read", tmp.to_str().unwrap(), "--from", "1", "--to", "2"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let read_json = String::from_utf8(out.stdout).unwrap();
    assert!(read_json.contains("hello 🚀"));

    // 5. Edit with --force
    let out = Command::new(bin)
        .args([
            "edit",
            tmp.to_str().unwrap(),
            "--old",
            "hello",
            "--new",
            "world",
            "--force",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "edit failed: {}", String::from_utf8_lossy(&out.stdout));

    // 6. Validate
    let out = Command::new(bin)
        .args(["validate", tmp.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());

    let final_text = std::fs::read_to_string(&tmp).unwrap();
    assert!(final_text.contains("world 🚀"));
    let _ = std::fs::remove_file(&tmp);
}
