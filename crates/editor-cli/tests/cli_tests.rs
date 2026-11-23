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

    // 1. `editor status` when no GUI is running -> ok:true, running:false; `editor close` -> E_NOT_RUNNING.
    let out = Command::new(bin)
        .env("EDITOR_IPC_SOCK", &sock)
        .arg("status")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout).unwrap().contains("\"running\":false"));

    let out = Command::new(bin)
        .env("EDITOR_IPC_SOCK", &sock)
        .arg("close")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8(out.stdout).unwrap().contains("E_NOT_RUNNING"));

    // 2. Start a mock IPC server mimicking a running GUI and verify `status`, `open`, `goto`, `close`, `quit --force`.
    let server = IpcServer::bind(sock.clone(), Arc::new(|| {})).unwrap();
    let srv_thread = thread::spawn(move || {
        let mut handled = 0;
        for _ in 0..400 {
            if let Some(pending) = server.try_recv() {
                match &pending.cmd {
                    IpcCommand::Status => {
                        pending.respond(IpcReply::Json(
                            "{\"running\":true,\"path\":\"/tmp/hello.rs\",\"dirty\":false}".into(),
                        ));
                    }
                    IpcCommand::Open(p) => {
                        assert!(p.ends_with("hello.rs") || p.is_dir());
                        pending.respond(IpcReply::Opened);
                    }
                    IpcCommand::Goto {
                        line_1based: 5,
                        col_bytes: 2,
                    } => {
                        pending.respond(IpcReply::Json(
                            "{\"running\":true,\"cursor\":{\"line\":5,\"col\":2}}".into(),
                        ));
                    }
                    IpcCommand::Split { path: Some(p) } => {
                        assert!(p.ends_with("hello.rs"));
                        pending.respond(IpcReply::Json("{\"running\":true,\"split\":true,\"active_pane\":1}".into()));
                    }
                    IpcCommand::FocusPane(0) => {
                        pending.respond(IpcReply::Json("{\"running\":true,\"split\":true,\"active_pane\":0}".into()));
                    }
                    IpcCommand::Unsplit => {
                        pending.respond(IpcReply::Json("{\"running\":true,\"split\":false}".into()));
                    }
                    IpcCommand::Action(act) if act.starts_with("pin:") => {
                        assert!(act.ends_with("hello.rs"));
                        pending.respond(IpcReply::Json(
                            "{\"running\":true,\"pins\":[{\"slot\":1,\"file_name\":\"hello.rs\"}]}".into(),
                        ));
                    }
                    IpcCommand::Action(act) if act == "unpin:1" => {
                        pending.respond(IpcReply::Json("{\"running\":true,\"pins\":[]}".into()));
                    }
                    IpcCommand::Action(act) if act == "zoom" => {
                        pending.respond(IpcReply::Json("{\"running\":true,\"zoomed\":true}".into()));
                    }
                    IpcCommand::Action(act) if act == "new-tab" => {
                        pending.respond(IpcReply::Json("{\"running\":true,\"file_name\":\"Untitled\"}".into()));
                    }
                    IpcCommand::Action(act) if act == "close-tab" => {
                        pending.respond(IpcReply::Json("{\"running\":true,\"file_name\":\"hello.rs\"}".into()));
                    }
                    IpcCommand::Action(act) if act == "syntax-set:ts" => {
                        pending.respond(IpcReply::Json("{\"running\":true,\"language_id\":\"typescript\"}".into()));
                    }
                    IpcCommand::Action(act) if act == "syntax-style:monochrome" => {
                        pending.respond(IpcReply::Json("{\"running\":true,\"view\":{\"syntax_style\":\"monochrome\"}}".into()));
                    }
                    IpcCommand::Action(act) if act == "syntax-map:ino=cpp" => {
                        pending.respond(IpcReply::Json("{\"running\":true,\"view\":{\"syntax_ext_map\":{\"ino\":\"cpp\"}}}".into()));
                    }
                    IpcCommand::Action(act) if act == "syntax-theme" => {
                        pending.respond(IpcReply::Json("{\"preset\":\"editorial\",\"active_mode\":\"light\"}".into()));
                    }
                    IpcCommand::Action(act) if act == "syntax-color:dark:comment:#55aa77:italic" => {
                        pending.respond(IpcReply::Json("{\"preset\":\"editorial\",\"dark_overrides\":{\"comment\":\"#55aa77:italic\"}}".into()));
                    }
                    IpcCommand::Action(act) if act == "syntax-reset:all" => {
                        pending.respond(IpcReply::Json("{\"preset\":\"editorial\",\"dark_overrides\":{}}".into()));
                    }
                    IpcCommand::Action(act) if act == "sidebar-width:320" => {
                        pending.respond(IpcReply::Json("{\"running\":true,\"sidebar\":{\"width_pt\":320}}".into()));
                    }
                    IpcCommand::Action(act) if act == "pane-ratio:cols=35%" => {
                        pending.respond(IpcReply::Json("{\"running\":true,\"split_ratios\":{\"col\":0.350}}".into()));
                    }
                    IpcCommand::Action(act) if act == "pane-ratio:reset" => {
                        pending.respond(IpcReply::Json("{\"running\":true,\"split_ratios\":{\"col\":0.500}}".into()));
                    }
                    IpcCommand::Quit { force: false } => {
                        pending.respond(IpcReply::Prompting);
                    }
                    IpcCommand::Quit { force: true } => {
                        pending.respond(IpcReply::Quitting);
                    }
                    other => panic!("unexpected ipc command: {other:?}"),
                }
                handled += 1;
                if handled == 24 {
                    break;
                }
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(handled, 24);
    });

    // `editor status`
    let out = Command::new(bin)
        .env("EDITOR_IPC_SOCK", &sock)
        .arg("status")
        .output()
        .unwrap();
    assert!(out.status.success());
    let s = String::from_utf8(out.stdout).unwrap();
    assert!(s.contains("\"running\":true"));
    assert!(s.contains("/tmp/hello.rs"));

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

    // VS Code-style shorthand: `editor .` (opens current working directory) and `editor hello.rs`
    let out = Command::new(bin)
        .env("EDITOR_IPC_SOCK", &sock)
        .current_dir(&root)
        .arg(".")
        .output()
        .unwrap();
    assert!(out.status.success(), "editor . failed: {}", String::from_utf8_lossy(&out.stdout));
    assert!(String::from_utf8(out.stdout).unwrap().contains("\"status\":\"opened\""));

    let out = Command::new(bin)
        .env("EDITOR_IPC_SOCK", &sock)
        .current_dir(&root)
        .arg("hello.rs")
        .output()
        .unwrap();
    assert!(out.status.success(), "editor hello.rs failed: {}", String::from_utf8_lossy(&out.stdout));
    assert!(String::from_utf8(out.stdout).unwrap().contains("\"status\":\"opened\""));

    // Unknown non-path command still fails cleanly with E_USAGE
    let out = Command::new(bin)
        .env("EDITOR_IPC_SOCK", &sock)
        .current_dir(&root)
        .arg("definitely-not-a-command")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8(out.stdout).unwrap().contains("\"E_USAGE\""));

    // `editor goto 5:2`
    let out = Command::new(bin)
        .env("EDITOR_IPC_SOCK", &sock)
        .args(["goto", "5:2"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout).unwrap().contains("\"line\":5,\"col\":2"));

    // `editor split hello.rs`, `editor focus-pane left`, `editor unsplit`
    let out = Command::new(bin)
        .env("EDITOR_IPC_SOCK", &sock)
        .args(["--root", root.to_str().unwrap(), "split", "hello.rs"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout).unwrap().contains("\"split\":true"));

    let out = Command::new(bin)
        .env("EDITOR_IPC_SOCK", &sock)
        .args(["focus-pane", "left"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout).unwrap().contains("\"active_pane\":0"));

    let out = Command::new(bin)
        .env("EDITOR_IPC_SOCK", &sock)
        .arg("unsplit")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout).unwrap().contains("\"split\":false"));

    // `editor pin hello.rs`, `editor unpin 1`, `editor zoom`, `editor new-tab`, `editor close-tab`
    let out = Command::new(bin)
        .env("EDITOR_IPC_SOCK", &sock)
        .args(["--root", root.to_str().unwrap(), "pin", "hello.rs"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout).unwrap().contains("\"slot\":1"));

    let out = Command::new(bin)
        .env("EDITOR_IPC_SOCK", &sock)
        .args(["unpin", "1"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout).unwrap().contains("\"pins\":[]"));

    let out = Command::new(bin)
        .env("EDITOR_IPC_SOCK", &sock)
        .arg("zoom")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout).unwrap().contains("\"zoomed\":true"));

    let out = Command::new(bin)
        .env("EDITOR_IPC_SOCK", &sock)
        .arg("new-tab")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout).unwrap().contains("\"Untitled\""));

    let out = Command::new(bin)
        .env("EDITOR_IPC_SOCK", &sock)
        .arg("close-tab")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout).unwrap().contains("\"hello.rs\""));

    // Offline `editor syntax list` / `editor languages`
    let out = Command::new(bin).args(["syntax", "list"]).output().unwrap();
    assert!(out.status.success());
    let langs_json = String::from_utf8(out.stdout).unwrap();
    for id in [
        "cpp", "csharp", "rust", "markdown", "html", "css", "javascript", "typescript",
        "python", "java", "shell", "powershell", "go", "sql", "php", "c", "kotlin",
    ] {
        assert!(langs_json.contains(&format!("\"id\":\"{id}\"")), "missing {id} in syntax list");
    }

    let out = Command::new(bin).arg("languages").output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout).unwrap().contains("\"id\":\"rust\""));

    // Live GUI IPC `editor syntax set|style|map`
    let out = Command::new(bin)
        .env("EDITOR_IPC_SOCK", &sock)
        .args(["syntax", "set", "ts"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout).unwrap().contains("\"typescript\""));

    let out = Command::new(bin)
        .env("EDITOR_IPC_SOCK", &sock)
        .args(["syntax", "style", "monochrome"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout).unwrap().contains("\"monochrome\""));

    let out = Command::new(bin)
        .env("EDITOR_IPC_SOCK", &sock)
        .args(["syntax", "map", "ino", "cpp"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout).unwrap().contains("\"ino\":\"cpp\""));

    let out = Command::new(bin)
        .env("EDITOR_IPC_SOCK", &sock)
        .args(["syntax", "theme"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout).unwrap().contains("\"preset\":\"editorial\""));

    let out = Command::new(bin)
        .env("EDITOR_IPC_SOCK", &sock)
        .args(["syntax", "color", "comment", "#55aa77", "italic", "--mode", "dark"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout).unwrap().contains("#55aa77:italic"));

    let out = Command::new(bin)
        .env("EDITOR_IPC_SOCK", &sock)
        .args(["syntax", "reset"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout).unwrap().contains("\"dark_overrides\":{}"));

    // Live GUI IPC `editor resize sidebar|cols|reset`
    let out = Command::new(bin)
        .env("EDITOR_IPC_SOCK", &sock)
        .args(["resize", "sidebar", "320"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout).unwrap().contains("\"width_pt\":320"));

    let out = Command::new(bin)
        .env("EDITOR_IPC_SOCK", &sock)
        .args(["resize", "cols", "35%"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout).unwrap().contains("\"col\":0.350"));

    let out = Command::new(bin)
        .env("EDITOR_IPC_SOCK", &sock)
        .args(["resize", "reset"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout).unwrap().contains("\"col\":0.500"));

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
