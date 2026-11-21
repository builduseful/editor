use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn cli_round_trip_write_info_read_edit_validate_and_batch() {
    let bin = env!("CARGO_BIN_EXE_editor");
    let root = std::env::temp_dir().join(format!("editor_cli_suite_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();

    let target = root.join("main.rs");
    let payload = root.join("payload.txt");
    std::fs::write(&payload, "fn main() {\n    println!(\"hello 🚀\");\n}\n").unwrap();

    // 1. Version & Schema
    let out = Command::new(bin).arg("version").output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout).unwrap().contains("\"ok\":true"));

    let out = Command::new(bin).args(["schema", "batch"]).output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout).unwrap().contains("\"batch\""));

    // 2. Write new file confined under --root
    let out = Command::new(bin)
        .args([
            "--root",
            root.to_str().unwrap(),
            "write",
            "main.rs",
            "@payload.txt",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "write failed: {}", String::from_utf8_lossy(&out.stdout));

    // 3. Confinement rejects escaping --root with E_OUTSIDE_ROOT
    let out = Command::new(bin)
        .args(["--root", root.to_str().unwrap(), "info", "../outside.txt"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8(out.stdout).unwrap().contains("E_OUTSIDE_ROOT"));

    // 4. Read byte axis & line axis
    let out = Command::new(bin)
        .args(["--root", root.to_str().unwrap(), "read", "main.rs", "--from", "1", "--to", "2"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout).unwrap().contains("hello 🚀"));

    let out = Command::new(bin)
        .args(["--root", root.to_str().unwrap(), "read", "main.rs", "--from-offset", "0", "--length", "9"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout).unwrap().contains("\"content\":\"fn main()\""));

    // 5. Positional edit --at LINE:COL --op insert + --dry-run first
    let out = Command::new(bin)
        .args([
            "--root",
            root.to_str().unwrap(),
            "edit",
            "main.rs",
            "--at",
            "1:0",
            "--op",
            "insert",
            "--text",
            "// header\n",
            "--force",
            "--dry-run",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout).unwrap().contains("\"line_count\":4"));
    // Dry-run must not modify file on disk!
    assert!(!std::fs::read_to_string(&target).unwrap().contains("// header"));

    // Now apply for real!
    let out = Command::new(bin)
        .args([
            "--root",
            root.to_str().unwrap(),
            "edit",
            "main.rs",
            "--at",
            "1:0",
            "--op",
            "insert",
            "--text",
            "// header\n",
            "--force",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(std::fs::read_to_string(&target).unwrap().starts_with("// header\n"));

    // 6. Batch --check + multi-file commit via stdin JSON-lines
    let batch_prog = "{\"op\":\"edit\",\"path\":\"main.rs\",\"old\":\"hello\",\"new\":\"world\",\"force\":true}\n\
                      {\"op\":\"write\",\"path\":\"second.rs\",\"content\":\"pub const X: u32 = 7;\\n\"}\n";

    let mut child = Command::new(bin)
        .args(["--root", root.to_str().unwrap(), "batch", "--check"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(batch_prog.as_bytes()).unwrap();
    let check_out = child.wait_with_output().unwrap();
    assert!(check_out.status.success());
    assert!(String::from_utf8(check_out.stdout).unwrap().contains("\"valid\":true"));
    assert!(!root.join("second.rs").exists(), "--check must not write!");

    let mut child = Command::new(bin)
        .args(["--root", root.to_str().unwrap(), "batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(batch_prog.as_bytes()).unwrap();
    let run_out = child.wait_with_output().unwrap();
    assert!(run_out.status.success(), "batch failed: {}", String::from_utf8_lossy(&run_out.stdout));
    assert!(std::fs::read_to_string(&target).unwrap().contains("world 🚀"));
    assert_eq!(std::fs::read_to_string(root.join("second.rs")).unwrap(), "pub const X: u32 = 7;\n");

    // 7. Validate
    let out = Command::new(bin)
        .args(["--root", root.to_str().unwrap(), "validate", "main.rs"])
        .output()
        .unwrap();
    assert!(out.status.success());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn cli_ipc_open_close_and_quit_commands() {
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    use editor_platform::ipc::{IpcCommand, IpcReply, IpcServer};

    let bin = env!("CARGO_BIN_EXE_editor");
    let root = std::env::temp_dir().join(format!("editor_cli_ipc_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let sample = root.join("hello.rs");
    std::fs::write(&sample, "fn main() {}\n").unwrap();
    let sock = root.join("test.sock");

    // 1. `editor close` when no GUI is running -> E_NOT_RUNNING (exit 2).
    let out = Command::new(bin)
        .env("EDITOR_IPC_SOCK", &sock)
        .arg("close")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8(out.stdout).unwrap().contains("E_NOT_RUNNING"));

    // 2. Start a mock IPC server mimicking a running GUI and verify `open`, `close`, `quit --force`.
    let server = IpcServer::bind(sock.clone(), Arc::new(|| {})).unwrap();
    let srv_thread = thread::spawn(move || {
        let mut handled = 0;
        for _ in 0..400 {
            if let Some(pending) = server.try_recv() {
                match &pending.cmd {
                    IpcCommand::Open(p) => {
                        assert!(p.ends_with("hello.rs"));
                        pending.respond(IpcReply::Opened);
                    }
                    IpcCommand::Quit { force: false } => {
                        pending.respond(IpcReply::Prompting);
                    }
                    IpcCommand::Quit { force: true } => {
                        pending.respond(IpcReply::Quitting);
                    }
                }
                handled += 1;
                if handled == 3 {
                    break;
                }
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(handled, 3);
    });

    // `editor open hello.rs`
    let out = Command::new(bin)
        .env("EDITOR_IPC_SOCK", &sock)
        .args(["--root", root.to_str().unwrap(), "open", "hello.rs"])
        .output()
        .unwrap();
    assert!(out.status.success(), "open failed: {}", String::from_utf8_lossy(&out.stdout));
    let s = String::from_utf8(out.stdout).unwrap();
    assert!(s.contains("\"launched\":false"));
    assert!(s.contains("\"status\":\"opened\""));

    // `editor close` (mock GUI reports dirty -> prompting: true)
    let out = Command::new(bin)
        .env("EDITOR_IPC_SOCK", &sock)
        .arg("close")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout).unwrap().contains("\"prompting\":true"));

    // `editor quit --force` (mock GUI reports quitting -> closed: true)
    let out = Command::new(bin)
        .env("EDITOR_IPC_SOCK", &sock)
        .args(["quit", "--force"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout).unwrap().contains("\"closed\":true"));

    srv_thread.join().unwrap();
    let _ = std::fs::remove_dir_all(&root);
}
