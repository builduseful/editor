//! Deterministic JSON-first CLI binary (`editor`) sharing `editor-core`.
//!
//! Commands:
//!   read     <path> [--from N --to N | --from-offset B --length L] [--max-bytes N]
//!   write    <path> (--stdin | @FILE) [--expect REV | --force] [--in-place]
//!   edit     <path> (--old T --new T [--all | --count N] | --at LINE[:COL] --op insert|replace|delete [--text T] [--length L])
//!                   (--expect REV | --force) [--dry-run] [--in-place]
//!   info     <path>
//!   validate <path>
//!   batch    [--file F | --stdin] [--check]
//!   version
//!   schema   [COMMAND]

mod json;
mod json_parse;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use editor_core::{Document, DocumentError, SearchOptions};
use editor_platform::{FileRev, FileSystem, StdFileSystem};

use crate::json::JsonBuf;
use crate::json_parse::{parse_json, JsonValue};

const VERSION: &str = env!("CARGO_PKG_VERSION");

struct GlobalOpts {
    root: PathBuf,
}

fn main() -> ExitCode {
    let start = Instant::now();
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    if raw_args.is_empty() || raw_args[0] == "--help" || raw_args[0] == "-h" || raw_args[0] == "help"
    {
        print_usage();
        return ExitCode::SUCCESS;
    }

    // Extract global `--root DIR` whether passed before or after the subcommand.
    let mut root: Option<PathBuf> = None;
    let mut args: Vec<String> = Vec::with_capacity(raw_args.len());
    let mut i = 0;
    while i < raw_args.len() {
        if raw_args[i] == "--root" && i + 1 < raw_args.len() {
            root = Some(PathBuf::from(&raw_args[i + 1]));
            i += 2;
        } else {
            args.push(raw_args[i].clone());
            i += 1;
        }
    }

    if args.is_empty() {
        print_usage();
        return ExitCode::SUCCESS;
    }
    if args[0] == "--version" || args[0] == "version" {
        let mut res = JsonBuf::new();
        res.push_raw("[{\"version\":");
        res.push_str(VERSION);
        res.push_raw("}]");
        emit_ok("version", start, &res.into_string());
        return ExitCode::SUCCESS;
    }

    let fs = StdFileSystem;
    let default_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let resolved_root = fs
        .canonicalize(root.as_deref().unwrap_or(&default_root))
        .unwrap_or_else(|_| root.unwrap_or(default_root));
    let globals = GlobalOpts {
        root: resolved_root,
    };

    let cmd = args[0].as_str();
    let rest = &args[1..];

    let code = match cmd {
        "schema" => cmd_schema(rest, start),
        "info" => cmd_info(&fs, &globals, rest, start),
        "read" => cmd_read(&fs, &globals, rest, start),
        "validate" => cmd_validate(&fs, &globals, rest, start),
        "write" => cmd_write(&fs, &globals, rest, start),
        "edit" => cmd_edit(&fs, &globals, rest, start),
        "batch" => cmd_batch(&fs, &globals, rest, start),
        "open" | "gui" => cmd_open(cmd, &fs, &globals, rest, start),
        "split" => cmd_split(&fs, &globals, rest, start),
        "drop" => cmd_drop(&fs, &globals, rest, start),
        "move-pane" => cmd_move_pane(rest, start),
        "unsplit" => send_live_gui_command("unsplit", &editor_platform::ipc::IpcCommand::Unsplit, start),
        "focus-pane" => cmd_focus_pane(rest, start),
        "resize" => cmd_resize(rest, start),
        "pin" => cmd_pin(&fs, &globals, rest, start),
        "unpin" => cmd_unpin(&fs, &globals, rest, start),
        "pins" => cmd_status(start),
        "buffers" => cmd_status(start),
        "zoom" => cmd_action_direct("zoom", start),
        "new-tab" => cmd_action_direct("new-tab", start),
        "close-tab" => cmd_action_direct("close-tab", start),
        "status" => cmd_status(start),
        "goto" => cmd_goto(rest, start),
        "select" => cmd_select(rest, start),
        "insert" => cmd_insert(&fs, &globals, rest, start),
        "find" => cmd_find(rest, start),
        "save" => cmd_save(&fs, &globals, rest, start),
        "undo" => cmd_action_direct("undo", start),
        "redo" => cmd_action_direct("redo", start),
        "syntax" => cmd_syntax(&fs, &globals, rest, start),
        "languages" => cmd_syntax_list(start),
        "action" => cmd_action(&fs, &globals, rest, start),
        "close" | "quit" => cmd_quit(cmd, rest, start),
        other => {
            emit_err(
                other,
                start,
                "E_USAGE",
                &format!("unknown command: {other}"),
                None,
            );
            3
        }
    };
    ExitCode::from(code)
}

fn print_usage() {
    eprintln!(
        "editor {VERSION} — fast, zero-dependency Rust text editor CLI\n\
         \n\
         Global flags: --root DIR\n\
         Commands:\n\
           editor read <path> [--from N --to N | --from-offset B --length L] [--max-bytes N]\n\
           editor info <path>\n\
           editor validate <path>\n\
           editor write <path> (--stdin | @FILE) [--expect REV | --force] [--in-place]\n\
           editor edit <path> (--old T --new T [--all | --count N]\n\
                             | --at LINE[:COL] --op insert|replace|delete [--text T] [--length L])\n\
                             (--expect REV | --force) [--dry-run] [--in-place]\n\
           editor batch [--file F | --stdin] [--check]\n\
           editor open [--sidebar-root DIR] [PATH]               (launch or focus GUI, alias: gui)\n\
           editor split [--down] [PATH]                          (split active pane right or down, up to 2x2)\n\
           editor drop <PATH> [--pane 0..3] [--zone center|left|right|top|bottom]\n\
           editor move-pane <SRC_IDX> <TARGET_IDX> [center|left|right|top|bottom]\n\
           editor unsplit                                        (close active split pane)\n\
           editor focus-pane <0..3|left|right|up|down|next>      (switch active split pane)\n\
           editor resize sidebar <WIDTH_PT|reset>                (resize sidebar width, 160..480 pt)\n\
           editor resize cols <RATIO|50%|reset>                  (resize left/right split ratio)\n\
           editor resize rows [0|1] <RATIO|50%|reset>            (resize top/bottom split ratio)\n\
           editor resize reset                                   (reset all pane split ratios to 50/50)\n\
           editor pin [PATH]                                     (pin current file or PATH to Harpoon 1..9)\n\
           editor unpin [SLOT|PATH]                              (unpin current file, slot 1..9, or PATH)\n\
           editor pins | buffers                                 (inspect Harpoon pins & open buffers)\n\
           editor zoom                                           (toggle fullscreen zoom on active pane)\n\
           editor new-tab | close-tab                            (create or close tab in Working-Set Strip)\n\
           editor status                                         (inspect live GUI state)\n\
           editor goto <LINE[:COL]>                              (jump live GUI caret)\n\
           editor select (<L1[:C1]> <L2[:C2]> | --all)           (set live GUI selection)\n\
           editor insert <TEXT|@FILE|->                          (insert at live GUI caret)\n\
           editor find <QUERY> [--replace T] [--case-sensitive] [--replace-all] [--close]\n\
           editor save [--as PATH]                               (save live GUI document)\n\
           editor undo | redo                                    (undo/redo in live GUI)\n\
           editor syntax list                                    (list compiled Rust syntax plugins, alias: languages)\n\
           editor syntax set <LANG_ID>                           (override active buffer language in live GUI)\n\
           editor syntax map <EXT> <LANG_ID|default>             (map file extension to plugin in live GUI)\n\
           editor syntax theme [PRESET|@FILE|--json J|--stdin]   (inspect or apply syntax theme/colors)\n\
           editor syntax color <TOK> <#HEX|reset> [STYLE] [--mode active|light|dark|both]\n\
           editor syntax reset [--mode all|light|dark]           (clear custom syntax colour overrides)\n\
           editor syntax style <editorial|monochrome|studio|warm>\n\
           editor action <NAME> [ARG]                            (run any UI command)\n\
           editor close [--force]   (alias: quit)\n\
           editor version\n\
           editor schema [COMMAND]"
    );
}

fn resolve_confined(
    fs: &dyn FileSystem,
    globals: &GlobalOpts,
    raw: &str,
) -> Result<PathBuf, (&'static str, String)> {
    let p = PathBuf::from(raw);
    let joined = if p.is_absolute() {
        p
    } else {
        globals.root.join(p)
    };
    let canonical = fs
        .canonicalize(&joined)
        .map_err(|e| ("E_NOT_FOUND", e.to_string()))?;
    if !canonical.starts_with(&globals.root) {
        return Err((
            "E_OUTSIDE_ROOT",
            format!(
                "path {} escapes root {}",
                canonical.display(),
                globals.root.display()
            ),
        ));
    }
    Ok(canonical)
}

fn load_text_arg(
    fs: &dyn FileSystem,
    globals: &GlobalOpts,
    raw: &str,
) -> Result<String, (&'static str, String)> {
    if raw == "-" {
        let mut s = String::new();
        std::io::stdin()
            .read_to_string(&mut s)
            .map_err(|e| ("E_IO", e.to_string()))?;
        Ok(s)
    } else if let Some(file_arg) = raw.strip_prefix('@') {
        let p = resolve_confined(fs, globals, file_arg)?;
        std::fs::read_to_string(&p).map_err(|e| ("E_IO", e.to_string()))
    } else {
        Ok(raw.to_string())
    }
}

fn cwd_string() -> String {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_string())
}

fn emit_ok(command: &str, start: Instant, results_json: &str) {
    emit_ok_extra(command, start, results_json, "");
}

fn emit_ok_extra(command: &str, start: Instant, results_json: &str, extra_top_fields: &str) {
    let mut jb = JsonBuf::new();
    jb.push_raw("{\"schema\":\"editor.result/1\",\"command\":");
    jb.push_str(command);
    jb.push_raw(",\"ok\":true,\"duration_us\":");
    jb.push_u64(start.elapsed().as_micros() as u64);
    jb.push_raw(",\"cwd\":");
    jb.push_str(&cwd_string());
    if !extra_top_fields.is_empty() {
        jb.push_raw(",");
        jb.push_raw(extra_top_fields);
    }
    jb.push_raw(",\"warnings\":[],\"results\":");
    jb.push_raw(results_json);
    jb.push_raw(",\"error\":null,\"meta\":{\"editor_version\":");
    jb.push_str(VERSION);
    jb.push_raw(",\"pid\":");
    jb.push_u64(std::process::id() as u64);
    jb.push_raw("}}");
    println!("{}", jb.into_string());
}

fn emit_err(command: &str, start: Instant, code: &str, message: &str, path: Option<&Path>) {
    emit_err_extra(command, start, code, message, path, "");
}

fn emit_err_extra(
    command: &str,
    start: Instant,
    code: &str,
    message: &str,
    path: Option<&Path>,
    extra_top_fields: &str,
) {
    let mut jb = JsonBuf::new();
    jb.push_raw("{\"schema\":\"editor.result/1\",\"command\":");
    jb.push_str(command);
    jb.push_raw(",\"ok\":false,\"duration_us\":");
    jb.push_u64(start.elapsed().as_micros() as u64);
    jb.push_raw(",\"cwd\":");
    jb.push_str(&cwd_string());
    if !extra_top_fields.is_empty() {
        jb.push_raw(",");
        jb.push_raw(extra_top_fields);
    }
    jb.push_raw(",\"warnings\":[],\"results\":[],\"error\":{\"code\":");
    jb.push_str(code);
    jb.push_raw(",\"message\":");
    jb.push_str(message);
    if let Some(p) = path {
        jb.push_raw(",\"path\":");
        jb.push_str(&p.to_string_lossy());
    }
    jb.push_raw("},\"meta\":{\"editor_version\":");
    jb.push_str(VERSION);
    jb.push_raw(",\"pid\":");
    jb.push_u64(std::process::id() as u64);
    jb.push_raw("}}");
    println!("{}", jb.into_string());
}

fn doc_err_code(e: &DocumentError) -> &'static str {
    match e {
        DocumentError::Binary { .. } => "E_BINARY",
        DocumentError::UnsupportedEncoding(_) => "E_UNSUPPORTED",
        DocumentError::InvalidUtf8 { .. } => "E_INVALID_UTF8",
        DocumentError::Range(_) => "E_RANGE",
        DocumentError::Conflict { .. } => "E_CONFLICT",
        DocumentError::NoPath => "E_USAGE",
        DocumentError::Fs(fe) => match fe {
            editor_platform::FsError::NotFound(_) => "E_NOT_FOUND",
            editor_platform::FsError::AlreadyExists(_) | editor_platform::FsError::Conflict(_) => {
                "E_CONFLICT"
            }
            editor_platform::FsError::Locked(_) => "E_LOCKED",
            editor_platform::FsError::Unsupported(_) => "E_UNSUPPORTED",
            _ => "E_IO",
        },
    }
}

fn push_doc_fields(jb: &mut JsonBuf, info: &editor_core::DocumentInfo, include_rev: bool) {
    jb.push_raw("\"path\":");
    jb.push_str(
        &info
            .path
            .as_deref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
    );
    jb.push_raw(",\"bytes\":");
    jb.push_u64(info.bytes);
    jb.push_raw(",\"line_count\":");
    jb.push_usize(info.line_count);
    jb.push_raw(",\"eol\":");
    jb.push_str(info.eol.as_str());
    jb.push_raw(",\"encoding\":\"utf-8\",\"bom\":");
    jb.push_bool(info.bom);
    jb.push_raw(",\"final_newline\":");
    jb.push_bool(info.final_newline);
    if include_rev {
        jb.push_raw(",\"rev\":");
        jb.push_rev(info.rev);
    }
}

fn cmd_schema(args: &[String], start: Instant) -> u8 {
    let all = [
        "read",
        "write",
        "edit",
        "info",
        "validate",
        "batch",
        "open",
        "gui",
        "split",
        "drop",
        "move-pane",
        "unsplit",
        "focus-pane",
        "resize",
        "pin",
        "unpin",
        "pins",
        "buffers",
        "zoom",
        "new-tab",
        "close-tab",
        "status",
        "goto",
        "select",
        "insert",
        "find",
        "save",
        "undo",
        "redo",
        "syntax",
        "languages",
        "action",
        "close",
        "quit",
        "version",
        "schema",
    ];
    if let Some(target) = args.first() {
        if !all.contains(&target.as_str()) {
            emit_err("schema", start, "E_USAGE", &format!("unknown command: {target}"), None);
            return 3;
        }
        let mut jb = JsonBuf::new();
        jb.push_raw("[{\"commands\":[");
        jb.push_str(target);
        jb.push_raw("]}]");
        emit_ok("schema", start, &jb.into_string());
        return 0;
    }
    let mut jb = JsonBuf::new();
    jb.push_raw("[{\"commands\":[");
    for (idx, name) in all.iter().enumerate() {
        if idx > 0 {
            jb.push_raw(",");
        }
        jb.push_str(name);
    }
    jb.push_raw("]}]");
    emit_ok("schema", start, &jb.into_string());
    0
}

fn cmd_info(fs: &dyn FileSystem, globals: &GlobalOpts, args: &[String], start: Instant) -> u8 {
    let Some(path_arg) = args.first() else {
        emit_err("info", start, "E_USAGE", "missing <path>", None);
        return 3;
    };
    let path = match resolve_confined(fs, globals, path_arg) {
        Ok(p) => p,
        Err((code, msg)) => {
            emit_err("info", start, code, &msg, Some(Path::new(path_arg)));
            return 2;
        }
    };
    match Document::open(fs, &path) {
        Ok(mut doc) => {
            let info = doc.info();
            let mut jb = JsonBuf::new();
            jb.push_raw("[{");
            push_doc_fields(&mut jb, &info, true);
            jb.push_raw(",\"dirty\":false}]");
            emit_ok("info", start, &jb.into_string());
            0
        }
        Err(e) => {
            emit_err("info", start, doc_err_code(&e), &e.to_string(), Some(&path));
            2
        }
    }
}

fn cmd_read(fs: &dyn FileSystem, globals: &GlobalOpts, args: &[String], start: Instant) -> u8 {
    let Some(path_arg) = args.first() else {
        emit_err("read", start, "E_USAGE", "missing <path>", None);
        return 3;
    };
    let mut from_line: Option<usize> = None;
    let mut to_line: Option<usize> = None;
    let mut from_offset: Option<u64> = None;
    let mut length: Option<usize> = None;
    let mut max_bytes: usize = 256 * 1024;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--from" if i + 1 < args.len() => {
                match args[i + 1].parse::<usize>() {
                    Ok(n) if n >= 1 => from_line = Some(n),
                    _ => {
                        emit_err("read", start, "E_USAGE", "--from must be >= 1", None);
                        return 3;
                    }
                }
                i += 2;
            }
            "--to" if i + 1 < args.len() => {
                match args[i + 1].parse::<usize>() {
                    Ok(n) if n >= 1 => to_line = Some(n),
                    _ => {
                        emit_err("read", start, "E_USAGE", "--to must be >= 1", None);
                        return 3;
                    }
                }
                i += 2;
            }
            "--from-offset" if i + 1 < args.len() => {
                match args[i + 1].parse::<u64>() {
                    Ok(n) => from_offset = Some(n),
                    _ => {
                        emit_err("read", start, "E_USAGE", "invalid --from-offset", None);
                        return 3;
                    }
                }
                i += 2;
            }
            "--length" if i + 1 < args.len() => {
                match args[i + 1].parse::<usize>() {
                    Ok(n) => length = Some(n),
                    _ => {
                        emit_err("read", start, "E_USAGE", "invalid --length", None);
                        return 3;
                    }
                }
                i += 2;
            }
            "--max-bytes" if i + 1 < args.len() => {
                match args[i + 1].parse::<usize>() {
                    Ok(n) => max_bytes = n,
                    _ => {
                        emit_err("read", start, "E_USAGE", "invalid --max-bytes", None);
                        return 3;
                    }
                }
                i += 2;
            }
            other => {
                emit_err("read", start, "E_USAGE", &format!("unexpected flag: {other}"), None);
                return 3;
            }
        }
    }

    let line_axis = from_line.is_some() || to_line.is_some();
    let byte_axis = from_offset.is_some() || length.is_some();
    if line_axis && byte_axis {
        emit_err(
            "read",
            start,
            "E_USAGE",
            "line flags (--from/--to) and byte flags (--from-offset/--length) are mutually exclusive",
            None,
        );
        return 3;
    }

    let path = match resolve_confined(fs, globals, path_arg) {
        Ok(p) => p,
        Err((code, msg)) => {
            emit_err("read", start, code, &msg, Some(Path::new(path_arg)));
            return 2;
        }
    };

    let mut doc = match Document::open(fs, &path) {
        Ok(d) => d,
        Err(e) => {
            emit_err("read", start, doc_err_code(&e), &e.to_string(), Some(&path));
            return 2;
        }
    };
    let info = doc.info();

    if byte_axis {
        let handle = match fs.open_read(&path) {
            Ok(h) => h,
            Err(e) => {
                emit_err("read", start, "E_IO", &e.to_string(), Some(&path));
                return 2;
            }
        };
        let total = handle.len();
        let off = from_offset.unwrap_or(0).min(total);
        let req_len = length.unwrap_or((total - off) as usize);
        let avail = (total - off) as usize;
        let mut to_read = req_len.min(avail);
        let mut truncated = false;
        if max_bytes > 0 && to_read > max_bytes {
            to_read = max_bytes;
            truncated = true;
        }
        let mut raw = vec![0u8; to_read];
        let mut read = 0;
        while read < to_read {
            match handle.read_at(off + read as u64, &mut raw[read..]) {
                Ok(0) => break,
                Ok(n) => read += n,
                Err(e) => {
                    emit_err("read", start, "E_IO", &e.to_string(), Some(&path));
                    return 2;
                }
            }
        }
        raw.truncate(read);
        // Truncate cleanly at a UTF-8 codepoint boundary when output cap fired.
        if truncated {
            while !raw.is_empty() && std::str::from_utf8(&raw).is_err() {
                raw.pop();
            }
        }
        let content = match String::from_utf8(raw) {
            Ok(s) => s,
            Err(e) => {
                emit_err(
                    "read",
                    start,
                    "E_INVALID_UTF8",
                    &format!("invalid UTF-8 at offset {}", off as usize + e.utf8_error().valid_up_to()),
                    Some(&path),
                );
                return 2;
            }
        };
        let end_off = off + content.len() as u64;
        let mut jb = JsonBuf::new();
        jb.push_raw("[{");
        push_doc_fields(&mut jb, &info, true);
        jb.push_raw(",\"from\":");
        jb.push_u64(off);
        jb.push_raw(",\"to\":");
        jb.push_u64(end_off);
        jb.push_raw(",\"truncated\":");
        jb.push_bool(truncated);
        jb.push_raw(",\"content\":");
        jb.push_str(&content);
        jb.push_raw("}]");
        emit_ok("read", start, &jb.into_string());
        return 0;
    }

    let start_l = from_line.unwrap_or(1);
    let end_1based = to_line.unwrap_or(info.line_count).min(info.line_count);
    let mut content_bytes = Vec::new();
    let mut truncated = false;
    let mut actual_to = 0usize;

    if info.line_count > 0 && start_l <= end_1based {
        for l1 in start_l..=end_1based {
            let prev = content_bytes.len();
            doc.tree_mut().line_with_eol_bytes(l1 - 1, &mut content_bytes);
            if max_bytes > 0 && content_bytes.len() > max_bytes {
                content_bytes.truncate(prev);
                truncated = true;
                break;
            }
            actual_to = l1;
        }
    }

    let content = match String::from_utf8(content_bytes) {
        Ok(s) => s,
        Err(e) => {
            emit_err(
                "read",
                start,
                "E_INVALID_UTF8",
                &format!("invalid UTF-8 at offset {}", e.utf8_error().valid_up_to()),
                Some(&path),
            );
            return 2;
        }
    };

    let mut jb = JsonBuf::new();
    jb.push_raw("[{");
    push_doc_fields(&mut jb, &info, true);
    jb.push_raw(",\"from\":");
    jb.push_usize(start_l);
    jb.push_raw(",\"to\":");
    jb.push_usize(actual_to);
    jb.push_raw(",\"truncated\":");
    jb.push_bool(truncated);
    jb.push_raw(",\"content\":");
    jb.push_str(&content);
    jb.push_raw("}]");
    emit_ok("read", start, &jb.into_string());
    0
}

fn cmd_validate(fs: &dyn FileSystem, globals: &GlobalOpts, args: &[String], start: Instant) -> u8 {
    let Some(path_arg) = args.first() else {
        emit_err("validate", start, "E_USAGE", "missing <path>", None);
        return 3;
    };
    let path = match resolve_confined(fs, globals, path_arg) {
        Ok(p) => p,
        Err((code, msg)) => {
            emit_err("validate", start, code, &msg, Some(Path::new(path_arg)));
            return 2;
        }
    };
    let mut doc = match Document::open(fs, &path) {
        Ok(d) => d,
        Err(e) => {
            emit_err("validate", start, doc_err_code(&e), &e.to_string(), Some(&path));
            return 2;
        }
    };
    let info = doc.info();
    match doc.validate_utf8() {
        Ok(()) => {
            let mut jb = JsonBuf::new();
            jb.push_raw("[{");
            push_doc_fields(&mut jb, &info, true);
            jb.push_raw(",\"errors\":[]}]");
            emit_ok("validate", start, &jb.into_string());
            0
        }
        Err(offset) => {
            emit_err(
                "validate",
                start,
                "E_INVALID_UTF8",
                &format!("invalid UTF-8 at byte offset {offset}"),
                Some(&path),
            );
            2
        }
    }
}

fn cmd_write(fs: &dyn FileSystem, globals: &GlobalOpts, args: &[String], start: Instant) -> u8 {
    let Some(path_arg) = args.first() else {
        emit_err("write", start, "E_USAGE", "missing <path>", None);
        return 3;
    };
    let path = match resolve_confined(fs, globals, path_arg) {
        Ok(p) => p,
        Err((code, msg)) => {
            emit_err("write", start, code, &msg, Some(Path::new(path_arg)));
            return 2;
        }
    };
    let mut use_stdin = false;
    let mut payload_file: Option<PathBuf> = None;
    let mut expect: Option<FileRev> = None;
    let mut force = false;
    let mut in_place = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--stdin" => {
                use_stdin = true;
                i += 1;
            }
            "--force" => {
                force = true;
                i += 1;
            }
            "--in-place" => {
                in_place = true;
                i += 1;
            }
            "--expect" if i + 1 < args.len() => {
                match FileRev::parse_stat_string(&args[i + 1]) {
                    Some(r) => expect = Some(r),
                    None => {
                        emit_err("write", start, "E_USAGE", "invalid --expect rev", Some(&path));
                        return 3;
                    }
                }
                i += 2;
            }
            s if s.starts_with('@') => {
                match resolve_confined(fs, globals, &s[1..]) {
                    Ok(p) => payload_file = Some(p),
                    Err((code, msg)) => {
                        emit_err("write", start, code, &msg, Some(&path));
                        return 2;
                    }
                }
                i += 1;
            }
            other => {
                emit_err("write", start, "E_USAGE", &format!("unexpected arg: {other}"), Some(&path));
                return 3;
            }
        }
    }

    if use_stdin == payload_file.is_some() {
        emit_err("write", start, "E_USAGE", "provide exactly one of --stdin or @FILE", Some(&path));
        return 3;
    }

    let payload = if use_stdin {
        let mut s = String::new();
        if let Err(e) = std::io::stdin().read_to_string(&mut s) {
            emit_err("write", start, "E_IO", &e.to_string(), Some(&path));
            return 2;
        }
        s
    } else {
        match std::fs::read_to_string(payload_file.as_ref().unwrap()) {
            Ok(s) => s,
            Err(e) => {
                emit_err("write", start, "E_IO", &e.to_string(), Some(&path));
                return 2;
            }
        }
    };

    let exists = fs.stat(&path).is_ok();
    if exists && expect.is_none() && !force {
        emit_err("write", start, "E_USAGE", "existing file requires --expect or --force", Some(&path));
        return 3;
    }

    let mut doc = if exists {
        match Document::open(fs, &path) {
            Ok(d) => d,
            Err(e) => {
                emit_err("write", start, doc_err_code(&e), &e.to_string(), Some(&path));
                return 2;
            }
        }
    } else {
        Document::new_untitled()
    };

    let len = doc.len_bytes();
    if let Err(e) = doc.edit(0, len, &payload, 0, false) {
        emit_err("write", start, doc_err_code(&e), &e.to_string(), Some(&path));
        return 2;
    }
    let save_res = if exists {
        doc.save(fs, expect, in_place)
    } else {
        doc.save_as(fs, &path, None, false)
    };

    match save_res {
        Ok(report) => {
            let info = doc.info();
            let mut jb = JsonBuf::new();
            jb.push_raw("[{");
            push_doc_fields(&mut jb, &info, true);
            jb.push_raw(",\"bytes_written\":");
            jb.push_u64(report.bytes_written);
            jb.push_raw(",\"save\":");
            jb.push_str(report.method.as_str());
            jb.push_raw("}]");
            emit_ok("write", start, &jb.into_string());
            0
        }
        Err(e) => {
            emit_err("write", start, doc_err_code(&e), &e.to_string(), Some(&path));
            2
        }
    }
}

fn parse_at_spec(spec: &str) -> Option<(usize, usize)> {
    if let Some((l, c)) = spec.split_once(':') {
        let line: usize = l.parse().ok()?;
        let col: usize = c.parse().ok()?;
        if line >= 1 { Some((line, col)) } else { None }
    } else {
        let line: usize = spec.parse().ok()?;
        if line >= 1 { Some((line, 0)) } else { None }
    }
}

fn cmd_edit(fs: &dyn FileSystem, globals: &GlobalOpts, args: &[String], start: Instant) -> u8 {
    let Some(path_arg) = args.first() else {
        emit_err("edit", start, "E_USAGE", "missing <path>", None);
        return 3;
    };
    let path = match resolve_confined(fs, globals, path_arg) {
        Ok(p) => p,
        Err((code, msg)) => {
            emit_err("edit", start, code, &msg, Some(Path::new(path_arg)));
            return 2;
        }
    };

    let mut old_raw: Option<String> = None;
    let mut new_raw: Option<String> = None;
    let mut replace_all = false;
    let mut count_limit: Option<usize> = None;
    let mut at_spec: Option<(usize, usize)> = None;
    let mut pos_op: Option<String> = None;
    let mut text_raw: Option<String> = None;
    let mut del_len: Option<u64> = None;
    let mut expect: Option<FileRev> = None;
    let mut force = false;
    let mut dry_run = false;
    let mut in_place = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--old" if i + 1 < args.len() => {
                old_raw = Some(args[i + 1].clone());
                i += 2;
            }
            "--new" if i + 1 < args.len() => {
                new_raw = Some(args[i + 1].clone());
                i += 2;
            }
            "--all" => {
                replace_all = true;
                i += 1;
            }
            "--count" if i + 1 < args.len() => {
                match args[i + 1].parse::<usize>() {
                    Ok(n) if n >= 1 => count_limit = Some(n),
                    _ => {
                        emit_err("edit", start, "E_USAGE", "--count must be >= 1", Some(&path));
                        return 3;
                    }
                }
                i += 2;
            }
            "--at" if i + 1 < args.len() => {
                match parse_at_spec(&args[i + 1]) {
                    Some(v) => at_spec = Some(v),
                    None => {
                        emit_err("edit", start, "E_USAGE", "invalid --at LINE[:COL]", Some(&path));
                        return 3;
                    }
                }
                i += 2;
            }
            "--op" if i + 1 < args.len() => {
                pos_op = Some(args[i + 1].clone());
                i += 2;
            }
            "--text" if i + 1 < args.len() => {
                text_raw = Some(args[i + 1].clone());
                i += 2;
            }
            "--length" if i + 1 < args.len() => {
                match args[i + 1].parse::<u64>() {
                    Ok(n) => del_len = Some(n),
                    _ => {
                        emit_err("edit", start, "E_USAGE", "invalid --length", Some(&path));
                        return 3;
                    }
                }
                i += 2;
            }
            "--force" => {
                force = true;
                i += 1;
            }
            "--dry-run" => {
                dry_run = true;
                i += 1;
            }
            "--in-place" => {
                in_place = true;
                i += 1;
            }
            "--expect" if i + 1 < args.len() => {
                match FileRev::parse_stat_string(&args[i + 1]) {
                    Some(r) => expect = Some(r),
                    None => {
                        emit_err("edit", start, "E_USAGE", "invalid --expect rev", Some(&path));
                        return 3;
                    }
                }
                i += 2;
            }
            other => {
                emit_err("edit", start, "E_USAGE", &format!("unexpected arg: {other}"), Some(&path));
                return 3;
            }
        }
    }

    if expect.is_none() && !force {
        emit_err("edit", start, "E_USAGE", "edit requires --expect or --force", Some(&path));
        return 3;
    }

    let is_search_form = old_raw.is_some() || new_raw.is_some();
    let is_pos_form = at_spec.is_some() || pos_op.is_some();
    if is_search_form == is_pos_form {
        emit_err("edit", start, "E_USAGE", "specify either --old/--new OR --at/--op", Some(&path));
        return 3;
    }

    let mut doc = match Document::open(fs, &path) {
        Ok(d) => d,
        Err(e) => {
            emit_err("edit", start, doc_err_code(&e), &e.to_string(), Some(&path));
            return 2;
        }
    };
    let rev_before = doc.rev();
    let bytes_before = doc.len_bytes();

    if is_pos_form {
        let (Some((line1, col)), Some(op)) = (at_spec, pos_op.as_deref()) else {
            emit_err("edit", start, "E_USAGE", "--at and --op are both required", Some(&path));
            return 3;
        };
        let total_lines = doc.exact_line_count();
        let target_offset = if line1 == total_lines + 1 && col == 0 {
            doc.len_bytes()
        } else if line1 >= 1 && line1 <= total_lines {
            let (ls, content_end, _) = doc.line_byte_range(line1 - 1).unwrap();
            if col as u64 > content_end - ls {
                emit_err("edit", start, "E_RANGE", "column out of bounds", Some(&path));
                return 2;
            }
            ls + col as u64
        } else {
            emit_err("edit", start, "E_RANGE", "line out of bounds", Some(&path));
            return 2;
        };

        let (removed, insert_str) = match op {
            "insert" => {
                let Some(raw_t) = text_raw.as_deref() else {
                    emit_err("edit", start, "E_USAGE", "--op insert requires --text", Some(&path));
                    return 3;
                };
                match load_text_arg(fs, globals, raw_t) {
                    Ok(t) => (0u64, t),
                    Err((c, m)) => {
                        emit_err("edit", start, c, &m, Some(&path));
                        return 2;
                    }
                }
            }
            "replace" => {
                let (Some(raw_t), Some(l)) = (text_raw.as_deref(), del_len) else {
                    emit_err("edit", start, "E_USAGE", "--op replace requires --text and --length", Some(&path));
                    return 3;
                };
                match load_text_arg(fs, globals, raw_t) {
                    Ok(t) => (l, t),
                    Err((c, m)) => {
                        emit_err("edit", start, c, &m, Some(&path));
                        return 2;
                    }
                }
            }
            "delete" => {
                let Some(l) = del_len else {
                    emit_err("edit", start, "E_USAGE", "--op delete requires --length", Some(&path));
                    return 3;
                };
                (l, String::new())
            }
            _ => {
                emit_err("edit", start, "E_USAGE", "invalid --op (use insert|replace|delete)", Some(&path));
                return 3;
            }
        };

        if let Err(e) = doc.edit(target_offset, removed, &insert_str, 0, false) {
            emit_err("edit", start, doc_err_code(&e), &e.to_string(), Some(&path));
            return 2;
        }
    } else {
        let (Some(or), Some(nr)) = (old_raw.as_deref(), new_raw.as_deref()) else {
            emit_err("edit", start, "E_USAGE", "require both --old and --new", Some(&path));
            return 3;
        };
        if or == "-" && nr == "-" {
            emit_err("edit", start, "E_USAGE", "--old and --new cannot both read stdin", Some(&path));
            return 3;
        }
        let old = match load_text_arg(fs, globals, or) {
            Ok(s) => s,
            Err((c, m)) => {
                emit_err("edit", start, c, &m, Some(&path));
                return 2;
            }
        };
        let new = match load_text_arg(fs, globals, nr) {
            Ok(s) => s,
            Err((c, m)) => {
                emit_err("edit", start, c, &m, Some(&path));
                return 2;
            }
        };
        if old.is_empty() {
            emit_err("edit", start, "E_USAGE", "--old must be non-empty", Some(&path));
            return 3;
        }
        let matches = doc.find_all(&old, SearchOptions::default());
        if matches.is_empty() {
            emit_err("edit", start, "E_NO_MATCH", "no match found", Some(&path));
            return 2;
        }
        if let Some(n) = count_limit {
            if matches.len() < n {
                emit_err("edit", start, "E_NO_MATCH", "fewer matches than --count", Some(&path));
                return 2;
            }
            for m in matches.into_iter().take(n).rev() {
                let _ = doc.edit(m.start, m.end - m.start, &new, 0, false);
            }
        } else if !replace_all {
            if matches.len() > 1 {
                emit_err(
                    "edit",
                    start,
                    "E_AMBIGUOUS",
                    &format!("matched {} occurrences; pass --all or --count", matches.len()),
                    Some(&path),
                );
                return 2;
            }
            let m = matches[0];
            let _ = doc.edit(m.start, m.end - m.start, &new, 0, false);
        } else {
            doc.replace_all(&old, &new, SearchOptions::default(), 0);
        }
    }

    if dry_run {
        let info = doc.info();
        let mut jb = JsonBuf::new();
        jb.push_raw("[{");
        push_doc_fields(&mut jb, &info, false);
        jb.push_raw(",\"rev_before\":");
        jb.push_rev(rev_before);
        jb.push_raw(",\"bytes_before\":");
        jb.push_u64(bytes_before);
        jb.push_raw(",\"dry_run\":true}]");
        emit_ok("edit", start, &jb.into_string());
        return 0;
    }

    match doc.save(fs, expect, in_place) {
        Ok(report) => {
            let info = doc.info();
            let mut jb = JsonBuf::new();
            jb.push_raw("[{");
            push_doc_fields(&mut jb, &info, true);
            jb.push_raw(",\"rev_before\":");
            jb.push_rev(rev_before);
            jb.push_raw(",\"bytes_before\":");
            jb.push_u64(bytes_before);
            jb.push_raw(",\"bytes_written\":");
            jb.push_u64(report.bytes_written);
            jb.push_raw(",\"save\":");
            jb.push_str(report.method.as_str());
            jb.push_raw("}]");
            emit_ok("edit", start, &jb.into_string());
            0
        }
        Err(e) => {
            emit_err("edit", start, doc_err_code(&e), &e.to_string(), Some(&path));
            2
        }
    }
}

// ============================================================================
// Transactional JSON-Lines Batch Engine (Validate -> Backup -> Commit / Rollback)
// ============================================================================

enum BatchOpKind {
    Write { content: String },
    Edit { old: String, new: String, all: bool },
}

struct ParsedBatchOp {
    path: PathBuf,
    kind: BatchOpKind,
    expect: Option<FileRev>,
    in_place: bool,
}

fn parse_rev_value(val: &JsonValue) -> Option<FileRev> {
    if let Some(s) = val.as_str() {
        return FileRev::parse_stat_string(s);
    }
    let obj = val.as_object()?;
    Some(FileRev {
        dev: obj.get("dev")?.as_str()?.parse().ok()?,
        ino: obj.get("ino")?.as_str()?.parse().ok()?,
        size: obj.get("size")?.as_str()?.parse().ok()?,
        mtime_ns: obj.get("mtime_ns")?.as_str()?.parse().ok()?,
        ctime_ns: obj.get("ctime_ns")?.as_str()?.parse().ok()?,
    })
}

fn cmd_batch(fs: &dyn FileSystem, globals: &GlobalOpts, args: &[String], start: Instant) -> u8 {
    let mut file_path: Option<PathBuf> = None;
    let mut check_only = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--file" if i + 1 < args.len() => {
                file_path = Some(PathBuf::from(&args[i + 1]));
                i += 2;
            }
            "--stdin" => i += 1,
            "--check" => {
                check_only = true;
                i += 1;
            }
            other => {
                emit_err("batch", start, "E_USAGE", &format!("unexpected arg: {other}"), None);
                return 3;
            }
        }
    }

    let program = if let Some(ref fp) = file_path {
        match std::fs::read_to_string(fp) {
            Ok(s) => s,
            Err(e) => {
                emit_err("batch", start, "E_IO", &e.to_string(), Some(fp));
                return 2;
            }
        }
    } else {
        let mut s = String::new();
        if let Err(e) = std::io::stdin().read_to_string(&mut s) {
            emit_err("batch", start, "E_IO", &e.to_string(), None);
            return 2;
        }
        s
    };

    // Stage 1: Parse and validate EVERY op before modifying any file.
    let mut ops = Vec::new();
    for (idx, line) in program.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let val = match parse_json(trimmed) {
            Ok(v) => v,
            Err(e) => {
                emit_err_extra(
                    "batch",
                    start,
                    "E_PARSE",
                    &format!("op {idx}: {e}"),
                    None,
                    "\"rolled_back\":false",
                );
                return 2;
            }
        };
        let Some(obj) = val.as_object() else {
            emit_err_extra("batch", start, "E_USAGE", "each batch line must be a JSON object", None, "\"rolled_back\":false");
            return 3;
        };
        let Some(op_name) = obj.get("op").and_then(|v| v.as_str()) else {
            emit_err_extra("batch", start, "E_USAGE", "missing op field", None, "\"rolled_back\":false");
            return 3;
        };
        let Some(raw_path) = obj.get("path").and_then(|v| v.as_str()) else {
            emit_err_extra("batch", start, "E_USAGE", "missing path field", None, "\"rolled_back\":false");
            return 3;
        };
        let path = match resolve_confined(fs, globals, raw_path) {
            Ok(p) => p,
            Err((c, m)) => {
                emit_err_extra("batch", start, c, &m, Some(Path::new(raw_path)), "\"rolled_back\":false");
                return 2;
            }
        };
        let force = obj.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
        let in_place = obj.get("in_place").and_then(|v| v.as_bool()).unwrap_or(false);
        let expect = match obj.get("expect") {
            Some(v) => match parse_rev_value(v) {
                Some(r) => Some(r),
                None => {
                    emit_err_extra("batch", start, "E_USAGE", "invalid expect rev", Some(&path), "\"rolled_back\":false");
                    return 3;
                }
            },
            None => None,
        };
        let exists = fs.stat(&path).is_ok();
        if exists && expect.is_none() && !force {
            emit_err_extra(
                "batch",
                start,
                "E_USAGE",
                "existing file requires expect or force",
                Some(&path),
                "\"rolled_back\":false",
            );
            return 3;
        }
        let kind = match op_name {
            "write" => {
                let content = if let Some(c) = obj.get("content").and_then(|v| v.as_str()) {
                    c.to_string()
                } else if let Some(cf) = obj.get("content_file").and_then(|v| v.as_str()) {
                    match resolve_confined(fs, globals, cf).and_then(|p| {
                        std::fs::read_to_string(&p).map_err(|e| ("E_IO", e.to_string()))
                    }) {
                        Ok(s) => s,
                        Err((c, m)) => {
                            emit_err_extra("batch", start, c, &m, Some(&path), "\"rolled_back\":false");
                            return 2;
                        }
                    }
                } else {
                    emit_err_extra("batch", start, "E_USAGE", "write requires content or content_file", Some(&path), "\"rolled_back\":false");
                    return 3;
                };
                BatchOpKind::Write { content }
            }
            "edit" => {
                let (Some(old), Some(new)) = (
                    obj.get("old").and_then(|v| v.as_str()),
                    obj.get("new").and_then(|v| v.as_str()),
                ) else {
                    emit_err_extra("batch", start, "E_USAGE", "edit requires old and new", Some(&path), "\"rolled_back\":false");
                    return 3;
                };
                let all = obj.get("all").and_then(|v| v.as_bool()).unwrap_or(false);
                BatchOpKind::Edit {
                    old: old.to_string(),
                    new: new.to_string(),
                    all,
                }
            }
            _ => {
                emit_err_extra("batch", start, "E_USAGE", "unknown batch op", Some(&path), "\"rolled_back\":false");
                return 3;
            }
        };
        ops.push(ParsedBatchOp {
            path,
            kind,
            expect,
            in_place,
        });
    }

    // Dry-validate each op against original files.
    let mut prepared: Vec<(ParsedBatchOp, Document, bool)> = Vec::with_capacity(ops.len());
    for op in ops {
        let existed = fs.stat(&op.path).is_ok();
        let mut doc = if existed {
            match Document::open(fs, &op.path) {
                Ok(d) => d,
                Err(e) => {
                    emit_err_extra("batch", start, doc_err_code(&e), &e.to_string(), Some(&op.path), "\"rolled_back\":false");
                    return 2;
                }
            }
        } else if matches!(op.kind, BatchOpKind::Write { .. }) {
            Document::new_untitled()
        } else {
            emit_err_extra("batch", start, "E_NOT_FOUND", "edit target not found", Some(&op.path), "\"rolled_back\":false");
            return 2;
        };
        if let Some(exp) = op.expect {
            if doc.rev() != exp {
                emit_err_extra("batch", start, "E_CONFLICT", "expect rev mismatch", Some(&op.path), "\"rolled_back\":false");
                return 2;
            }
        }
        match &op.kind {
            BatchOpKind::Write { content } => {
                let len = doc.len_bytes();
                let _ = doc.edit(0, len, content, 0, false);
            }
            BatchOpKind::Edit { old, new, all } => {
                let matches = doc.find_all(old, SearchOptions::default());
                if matches.is_empty() {
                    emit_err_extra("batch", start, "E_NO_MATCH", "no match in batch edit", Some(&op.path), "\"rolled_back\":false");
                    return 2;
                }
                if !all && matches.len() > 1 {
                    emit_err_extra("batch", start, "E_AMBIGUOUS", "ambiguous match in batch edit", Some(&op.path), "\"rolled_back\":false");
                    return 2;
                }
                doc.replace_all(old, new, SearchOptions::default(), 0);
            }
        }
        prepared.push((op, doc, existed));
    }

    if check_only {
        emit_ok_extra("batch", start, "[]", "\"rolled_back\":false,\"valid\":true,\"errors\":[]");
        return 0;
    }

    // Stage 2: Take backups of every existing file before committing any unit.
    let mut backups: Vec<(PathBuf, Option<Vec<u8>>)> = Vec::with_capacity(prepared.len());
    for (op, _, existed) in &prepared {
        let orig = if *existed {
            std::fs::read(&op.path).ok()
        } else {
            None
        };
        backups.push((op.path.clone(), orig));
    }

    // Stage 3: Commit each unit; on failure, roll back all earlier committed units!
    let mut results_json = JsonBuf::new();
    results_json.push_raw("[");
    let mut committed = 0usize;

    for (idx, (op, mut doc, existed)) in prepared.into_iter().enumerate() {
        let save_res = if existed {
            doc.save(fs, op.expect, op.in_place)
        } else {
            doc.save_as(fs, &op.path, None, false)
        };
        match save_res {
            Ok(report) => {
                if idx > 0 {
                    results_json.push_raw(",");
                }
                let info = doc.info();
                results_json.push_raw("{");
                push_doc_fields(&mut results_json, &info, true);
                results_json.push_raw(",\"bytes_written\":");
                results_json.push_u64(report.bytes_written);
                results_json.push_raw(",\"save\":");
                results_json.push_str(report.method.as_str());
                results_json.push_raw("}");
                committed += 1;
            }
            Err(e) => {
                let mut rolled_back = true;
                for (path, orig_opt) in backups.iter().take(committed) {
                    let res = match orig_opt {
                        Some(bytes) => std::fs::write(path, bytes),
                        None => std::fs::remove_file(path),
                    };
                    if res.is_err() {
                        rolled_back = false;
                    }
                }
                let extra = format!("\"rolled_back\":{}", if rolled_back { "true" } else { "false" });
                emit_err_extra("batch", start, doc_err_code(&e), &e.to_string(), Some(&op.path), &extra);
                return 2;
            }
        }
    }
    results_json.push_raw("]");
    emit_ok_extra("batch", start, &results_json.into_string(), "\"rolled_back\":false");
    0
}

// ============================================================================
// GUI IPC Control Commands: `open`, `gui`, `close` / `quit`
// ============================================================================

fn launch_gui_process(sidebar_root: Option<&Path>, target: Option<&Path>) -> Result<(), String> {
    // Prefer sibling `Editor.app` bundle if present next to `editor`, else sibling `editor-gui`.
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));

    let mut gui_args: Vec<String> = Vec::new();
    if let Some(root) = sidebar_root {
        gui_args.push("--sidebar-root".to_string());
        gui_args.push(root.to_string_lossy().into_owned());
    }
    if let Some(t) = target {
        gui_args.push(t.to_string_lossy().into_owned());
    }

    #[cfg(target_os = "macos")]
    if let Some(ref dir) = exe_dir {
        let app_bundle = dir.join("Editor.app");
        if app_bundle.exists() {
            let mut cmd = std::process::Command::new("/usr/bin/open");
            cmd.arg("-n").arg(&app_bundle);
            if !gui_args.is_empty() {
                cmd.arg("--args").args(&gui_args);
            }
            let status = cmd.status().map_err(|e| e.to_string())?;
            if !status.success() {
                return Err(format!("open failed with status {status}"));
            }
            let sock = editor_platform::ipc::default_socket_path();
            for _ in 0..80 {
                if editor_platform::ipc::send_ping(&sock).is_ok() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            return Ok(());
        }
    }

    let gui_bin = exe_dir
        .map(|d| d.join("editor-gui"))
        .filter(|p| p.exists())
        .unwrap_or_else(|| PathBuf::from("editor-gui"));
    std::process::Command::new(&gui_bin)
        .args(&gui_args)
        .spawn()
        .map_err(|e| format!("failed to launch {}: {e}", gui_bin.display()))?;
    Ok(())
}

fn cmd_open(
    command_name: &str,
    fs: &dyn FileSystem,
    globals: &GlobalOpts,
    args: &[String],
    start: Instant,
) -> u8 {
    let mut sidebar_root: Option<PathBuf> = None;
    let mut target: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--sidebar-root" if i + 1 < args.len() => {
                match resolve_confined(fs, globals, &args[i + 1]) {
                    Ok(p) => sidebar_root = Some(p),
                    Err((c, m)) => {
                        emit_err(command_name, start, c, &m, Some(Path::new(&args[i + 1])));
                        return 2;
                    }
                }
                i += 2;
            }
            other if !other.starts_with('-') && target.is_none() => {
                match resolve_confined(fs, globals, other) {
                    Ok(p) => target = Some(p),
                    Err((c, m)) => {
                        emit_err(command_name, start, c, &m, Some(Path::new(other)));
                        return 2;
                    }
                }
                i += 1;
            }
            other => {
                emit_err(
                    command_name,
                    start,
                    "E_USAGE",
                    &format!("unexpected arg: {other}"),
                    None,
                );
                return 3;
            }
        }
    }

    let sock = editor_platform::ipc::default_socket_path();
    let gui_running = editor_platform::ipc::send_ping(&sock).is_ok();
    let (launched, status) = if gui_running {
        if let Some(ref root) = sidebar_root {
            if let Ok(editor_platform::ipc::IpcReply::Error(m)) = editor_platform::ipc::send_command(
                &sock,
                &editor_platform::ipc::IpcCommand::OpenFolder(root.clone()),
            ) {
                emit_err(command_name, start, "E_IO", &m, Some(root));
                return 2;
            }
        }
        if let Some(ref p) = target {
            match editor_platform::ipc::send_command(
                &sock,
                &editor_platform::ipc::IpcCommand::Open(p.clone()),
            ) {
                Ok(editor_platform::ipc::IpcReply::Error(m)) => {
                    emit_err(command_name, start, "E_IO", &m, Some(p));
                    return 2;
                }
                Ok(_) => (false, "opened"),
                Err(e) => {
                    emit_err(command_name, start, "E_IO", &e.to_string(), Some(p));
                    return 2;
                }
            }
        } else {
            let _ = editor_platform::ipc::send_command(
                &sock,
                &editor_platform::ipc::IpcCommand::Action("focus".to_string()),
            );
            (false, "focused")
        }
    } else {
        if let Err(e) =
            launch_gui_process(sidebar_root.as_deref().or(Some(&globals.root)), target.as_deref())
        {
            emit_err(command_name, start, "E_IO", &e, target.as_deref());
            return 2;
        }
        (true, "launched")
    };

    let mut jb = JsonBuf::new();
    jb.push_raw("[{");
    if let Some(ref p) = target {
        jb.push_raw("\"path\":");
        jb.push_str(&p.to_string_lossy());
        jb.push_raw(",");
    }
    jb.push_raw("\"launched\":");
    jb.push_bool(launched);
    jb.push_raw(",\"status\":");
    jb.push_str(status);
    jb.push_raw("}]");
    emit_ok(command_name, start, &jb.into_string());
    0
}

fn cmd_split(fs: &dyn FileSystem, globals: &GlobalOpts, args: &[String], start: Instant) -> u8 {
    let mut down = false;
    let mut raw_path: Option<&str> = None;
    for arg in args {
        match arg.as_str() {
            "--down" | "-d" => down = true,
            "--right" | "-r" => down = false,
            other if !other.starts_with('-') && raw_path.is_none() => raw_path = Some(other),
            other => {
                emit_err("split", start, "E_USAGE", &format!("unexpected arg: {other}"), None);
                return 3;
            }
        }
    }
    let path = if let Some(arg) = raw_path {
        match resolve_confined(fs, globals, arg) {
            Ok(p) => Some(p),
            Err((c, m)) => {
                emit_err("split", start, c, &m, Some(Path::new(arg)));
                return 2;
            }
        }
    } else {
        None
    };
    if down {
        let act = match path {
            Some(p) => format!("split-down:{}", p.to_string_lossy()),
            None => "split-down".to_string(),
        };
        send_live_gui_command(
            "split",
            &editor_platform::ipc::IpcCommand::Action(act),
            start,
        )
    } else {
        send_live_gui_command(
            "split",
            &editor_platform::ipc::IpcCommand::Split { path },
            start,
        )
    }
}

fn cmd_drop(fs: &dyn FileSystem, globals: &GlobalOpts, args: &[String], start: Instant) -> u8 {
    let mut raw_path: Option<&str> = None;
    let mut pane_idx: usize = 0;
    let mut zone = "right";
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--pane" if i + 1 < args.len() => {
                let Ok(idx) = args[i + 1].parse::<usize>() else {
                    emit_err("drop", start, "E_USAGE", "invalid --pane index (0..3)", None);
                    return 3;
                };
                pane_idx = idx;
                i += 2;
            }
            "--zone" if i + 1 < args.len() => {
                zone = args[i + 1].as_str();
                i += 2;
            }
            other if !other.starts_with('-') && raw_path.is_none() => {
                raw_path = Some(other);
                i += 1;
            }
            other => {
                emit_err("drop", start, "E_USAGE", &format!("unexpected arg: {other}"), None);
                return 3;
            }
        }
    }
    let Some(raw) = raw_path else {
        emit_err(
            "drop",
            start,
            "E_USAGE",
            "usage: editor drop <PATH> [--pane 0..3] [--zone center|left|right|top|bottom]",
            None,
        );
        return 3;
    };
    let path = match resolve_confined(fs, globals, raw) {
        Ok(p) => p,
        Err((c, m)) => {
            emit_err("drop", start, c, &m, Some(Path::new(raw)));
            return 2;
        }
    };
    send_live_gui_command(
        "drop",
        &editor_platform::ipc::IpcCommand::Action(format!(
            "drop:{pane_idx}:{zone}:{}",
            path.to_string_lossy()
        )),
        start,
    )
}

fn cmd_move_pane(args: &[String], start: Instant) -> u8 {
    if args.len() < 2 {
        emit_err(
            "move-pane",
            start,
            "E_USAGE",
            "usage: editor move-pane <SRC_IDX> <TARGET_IDX> [center|left|right|top|bottom]",
            None,
        );
        return 3;
    }
    let (Ok(src), Ok(dst)) = (args[0].parse::<usize>(), args[1].parse::<usize>()) else {
        emit_err("move-pane", start, "E_USAGE", "pane indices must be 0..3", None);
        return 3;
    };
    let zone = args.get(2).map(String::as_str).unwrap_or("center");
    send_live_gui_command(
        "move-pane",
        &editor_platform::ipc::IpcCommand::Action(format!("move-pane:{src}:{dst}:{zone}")),
        start,
    )
}

fn cmd_pin(fs: &dyn FileSystem, globals: &GlobalOpts, args: &[String], start: Instant) -> u8 {
    if let Some(raw) = args.first() {
        let path = match resolve_confined(fs, globals, raw) {
            Ok(p) => p,
            Err((c, m)) => {
                emit_err("pin", start, c, &m, Some(Path::new(raw)));
                return 2;
            }
        };
        send_live_gui_command(
            "pin",
            &editor_platform::ipc::IpcCommand::Action(format!("pin:{}", path.to_string_lossy())),
            start,
        )
    } else {
        send_live_gui_command(
            "pin",
            &editor_platform::ipc::IpcCommand::Action("pin".to_string()),
            start,
        )
    }
}

fn cmd_unpin(fs: &dyn FileSystem, globals: &GlobalOpts, args: &[String], start: Instant) -> u8 {
    if let Some(raw) = args.first() {
        let arg = if raw.parse::<usize>().is_ok() {
            raw.clone()
        } else {
            resolve_confined(fs, globals, raw)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| raw.clone())
        };
        send_live_gui_command(
            "unpin",
            &editor_platform::ipc::IpcCommand::Action(format!("unpin:{arg}")),
            start,
        )
    } else {
        send_live_gui_command(
            "unpin",
            &editor_platform::ipc::IpcCommand::Action("unpin".to_string()),
            start,
        )
    }
}

fn cmd_focus_pane(args: &[String], start: Instant) -> u8 {
    let Some(arg) = args.first() else {
        emit_err(
            "focus-pane",
            start,
            "E_USAGE",
            "usage: editor focus-pane <0..3|left|right|up|down|next>",
            None,
        );
        return 3;
    };
    match arg.as_str() {
        "0" | "tl" | "top-left" => send_live_gui_command(
            "focus-pane",
            &editor_platform::ipc::IpcCommand::FocusPane(0),
            start,
        ),
        "1" | "tr" | "top-right" => send_live_gui_command(
            "focus-pane",
            &editor_platform::ipc::IpcCommand::FocusPane(1),
            start,
        ),
        "2" | "bl" | "bottom-left" => send_live_gui_command(
            "focus-pane",
            &editor_platform::ipc::IpcCommand::FocusPane(2),
            start,
        ),
        "3" | "br" | "bottom-right" => send_live_gui_command(
            "focus-pane",
            &editor_platform::ipc::IpcCommand::FocusPane(3),
            start,
        ),
        "left" => send_live_gui_command(
            "focus-pane",
            &editor_platform::ipc::IpcCommand::FocusPane(0),
            start,
        ),
        "right" => send_live_gui_command(
            "focus-pane",
            &editor_platform::ipc::IpcCommand::FocusPane(1),
            start,
        ),
        "up" | "top" => send_live_gui_command(
            "focus-pane",
            &editor_platform::ipc::IpcCommand::Action("focus-up".to_string()),
            start,
        ),
        "down" | "bottom" => send_live_gui_command(
            "focus-pane",
            &editor_platform::ipc::IpcCommand::Action("focus-down".to_string()),
            start,
        ),
        "next" => send_live_gui_command(
            "focus-pane",
            &editor_platform::ipc::IpcCommand::Action("focus-next".to_string()),
            start,
        ),
        _ => {
            emit_err(
                "focus-pane",
                start,
                "E_USAGE",
                "pane must be 0..3, left, right, up, down, or next",
                None,
            );
            3
        }
    }
}

fn cmd_quit(command_name: &str, args: &[String], start: Instant) -> u8 {
    let mut force = false;
    for arg in args {
        match arg.as_str() {
            "--force" => force = true,
            other => {
                emit_err(
                    command_name,
                    start,
                    "E_USAGE",
                    &format!("unexpected flag: {other}"),
                    None,
                );
                return 3;
            }
        }
    }

    let sock = editor_platform::ipc::default_socket_path();
    match editor_platform::ipc::send_command(
        &sock,
        &editor_platform::ipc::IpcCommand::Quit { force },
    ) {
        Ok(editor_platform::ipc::IpcReply::Quitting) => {
            emit_ok(
                command_name,
                start,
                "[{\"closed\":true,\"prompting\":false}]",
            );
            0
        }
        Ok(editor_platform::ipc::IpcReply::Prompting) => {
            emit_ok(
                command_name,
                start,
                "[{\"closed\":false,\"prompting\":true}]",
            );
            0
        }
        Ok(editor_platform::ipc::IpcReply::Error(m)) => {
            emit_err(command_name, start, "E_IO", &m, None);
            2
        }
        Ok(_) => {
            emit_ok(
                command_name,
                start,
                "[{\"closed\":true,\"prompting\":false}]",
            );
            0
        }
        Err(_) => {
            emit_err(
                command_name,
                start,
                "E_NOT_RUNNING",
                "no running editor GUI instance found",
                None,
            );
            2
        }
    }
}

fn send_live_gui_command(
    command_name: &str,
    cmd: &editor_platform::ipc::IpcCommand,
    start: Instant,
) -> u8 {
    let sock = editor_platform::ipc::default_socket_path();
    match editor_platform::ipc::send_command(&sock, cmd) {
        Ok(editor_platform::ipc::IpcReply::Json(state_json)) => {
            emit_ok(command_name, start, &format!("[{state_json}]"));
            0
        }
        Ok(editor_platform::ipc::IpcReply::Error(m)) => {
            emit_err(command_name, start, "E_IO", &m, None);
            2
        }
        Ok(_) => {
            emit_ok(command_name, start, "[{\"ok\":true}]");
            0
        }
        Err(_) => {
            emit_err(
                command_name,
                start,
                "E_NOT_RUNNING",
                "no running editor GUI instance found",
                None,
            );
            2
        }
    }
}

fn cmd_status(start: Instant) -> u8 {
    let sock = editor_platform::ipc::default_socket_path();
    match editor_platform::ipc::send_command(&sock, &editor_platform::ipc::IpcCommand::Status) {
        Ok(editor_platform::ipc::IpcReply::Json(state_json)) => {
            emit_ok("status", start, &format!("[{state_json}]"));
            0
        }
        Ok(editor_platform::ipc::IpcReply::Error(m)) => {
            emit_err("status", start, "E_IO", &m, None);
            2
        }
        Ok(_) => {
            emit_ok("status", start, "[{\"running\":true}]");
            0
        }
        Err(_) => {
            emit_ok("status", start, "[{\"running\":false}]");
            0
        }
    }
}

fn cmd_goto(args: &[String], start: Instant) -> u8 {
    let Some(spec) = args.first() else {
        emit_err("goto", start, "E_USAGE", "usage: editor goto <LINE[:COL]>", None);
        return 3;
    };
    let Some((line_1based, col_bytes)) = parse_at_spec(spec) else {
        emit_err("goto", start, "E_USAGE", "invalid LINE[:COL] (line must be >= 1)", None);
        return 3;
    };
    send_live_gui_command(
        "goto",
        &editor_platform::ipc::IpcCommand::Goto {
            line_1based,
            col_bytes,
        },
        start,
    )
}

fn cmd_select(args: &[String], start: Instant) -> u8 {
    if args.first().map(|s| s.as_str()) == Some("--all") {
        return send_live_gui_command(
            "select",
            &editor_platform::ipc::IpcCommand::Action("select-all".to_string()),
            start,
        );
    }
    if args.len() < 2 {
        emit_err(
            "select",
            start,
            "E_USAGE",
            "usage: editor select (<LINE[:COL]> <LINE[:COL]> | --all)",
            None,
        );
        return 3;
    }
    let (Some((sl, sc)), Some((el, ec))) = (parse_at_spec(&args[0]), parse_at_spec(&args[1]))
    else {
        emit_err("select", start, "E_USAGE", "invalid LINE[:COL] coordinates", None);
        return 3;
    };
    send_live_gui_command(
        "select",
        &editor_platform::ipc::IpcCommand::Select {
            start_line: sl,
            start_col: sc,
            end_line: el,
            end_col: ec,
        },
        start,
    )
}

fn cmd_insert(fs: &dyn FileSystem, globals: &GlobalOpts, args: &[String], start: Instant) -> u8 {
    let Some(raw) = args.first() else {
        emit_err("insert", start, "E_USAGE", "usage: editor insert <TEXT|@FILE|->", None);
        return 3;
    };
    let text = match load_text_arg(fs, globals, raw) {
        Ok(t) => t,
        Err((c, m)) => {
            emit_err("insert", start, c, &m, None);
            return 2;
        }
    };
    send_live_gui_command(
        "insert",
        &editor_platform::ipc::IpcCommand::Insert { text },
        start,
    )
}

fn cmd_find(args: &[String], start: Instant) -> u8 {
    let mut query: Option<String> = None;
    let mut replace: Option<String> = None;
    let mut case_sensitive = false;
    let mut replace_all = false;
    let mut close = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--replace" if i + 1 < args.len() => {
                replace = Some(args[i + 1].clone());
                i += 2;
            }
            "--case-sensitive" => {
                case_sensitive = true;
                i += 1;
            }
            "--replace-all" => {
                replace_all = true;
                i += 1;
            }
            "--close" => {
                close = true;
                i += 1;
            }
            other if !other.starts_with('-') && query.is_none() => {
                query = Some(other.to_string());
                i += 1;
            }
            other => {
                emit_err("find", start, "E_USAGE", &format!("unexpected arg: {other}"), None);
                return 3;
            }
        }
    }
    if !close && query.is_none() {
        emit_err("find", start, "E_USAGE", "require <QUERY> or --close", None);
        return 3;
    }
    send_live_gui_command(
        "find",
        &editor_platform::ipc::IpcCommand::Find {
            query: query.unwrap_or_default(),
            replace,
            case_sensitive,
            replace_all,
            close,
        },
        start,
    )
}

fn cmd_save(fs: &dyn FileSystem, globals: &GlobalOpts, args: &[String], start: Instant) -> u8 {
    let mut as_path: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--as" if i + 1 < args.len() => {
                match resolve_confined(fs, globals, &args[i + 1]) {
                    Ok(p) => as_path = Some(p),
                    Err((c, m)) => {
                        emit_err("save", start, c, &m, Some(Path::new(&args[i + 1])));
                        return 2;
                    }
                }
                i += 2;
            }
            other => {
                emit_err("save", start, "E_USAGE", &format!("unexpected arg: {other}"), None);
                return 3;
            }
        }
    }
    send_live_gui_command(
        "save",
        &editor_platform::ipc::IpcCommand::Save { as_path },
        start,
    )
}

fn cmd_syntax_list(start: Instant) -> u8 {
    let reg = editor_core::LanguageRegistry::global();
    let mut jb = JsonBuf::new();
    jb.push_raw("[{\"languages\":[");
    for (i, lang) in reg.all().enumerate() {
        if i > 0 {
            jb.push_raw(",");
        }
        let m = lang.meta();
        jb.push_raw("{\"id\":");
        jb.push_str(m.id);
        jb.push_raw(",\"name\":");
        jb.push_str(m.name);
        jb.push_raw(",\"extensions\":[");
        for (j, ext) in m.extensions.iter().enumerate() {
            if j > 0 {
                jb.push_raw(",");
            }
            jb.push_str(ext);
        }
        jb.push_raw("],\"aliases\":[");
        for (j, alias) in m.aliases.iter().enumerate() {
            if j > 0 {
                jb.push_raw(",");
            }
            jb.push_str(alias);
        }
        jb.push_raw("],\"line_comment\":");
        match m.line_comment {
            Some(lc) => jb.push_str(lc),
            None => jb.push_raw("null"),
        }
        jb.push_raw(",\"default_word_wrap\":");
        jb.push_bool(m.default_word_wrap);
        jb.push_raw("}");
    }
    jb.push_raw("]}]");
    emit_ok("syntax", start, &jb.into_string());
    0
}

fn cmd_syntax(fs: &dyn FileSystem, globals: &GlobalOpts, args: &[String], start: Instant) -> u8 {
    let Some(sub) = args.first().map(|s| s.as_str()) else {
        emit_err(
            "syntax",
            start,
            "E_USAGE",
            "usage: editor syntax <list|set|map|theme|color|reset|style>",
            None,
        );
        return 3;
    };
    match sub {
        "list" => cmd_syntax_list(start),
        "set" => {
            let Some(lang_id) = args.get(1) else {
                emit_err("syntax", start, "E_USAGE", "usage: editor syntax set <LANG_ID>", None);
                return 3;
            };
            send_live_gui_command(
                "syntax",
                &editor_platform::ipc::IpcCommand::Action(format!("syntax-set:{lang_id}")),
                start,
            )
        }
        "style" => {
            let Some(style) = args.get(1) else {
                emit_err(
                    "syntax",
                    start,
                    "E_USAGE",
                    "usage: editor syntax style <editorial|monochrome|studio|warm>",
                    None,
                );
                return 3;
            };
            send_live_gui_command(
                "syntax",
                &editor_platform::ipc::IpcCommand::Action(format!("syntax-style:{style}")),
                start,
            )
        }
        "theme" => {
            let rest = &args[1..];
            if rest.is_empty() {
                return send_live_gui_command(
                    "syntax",
                    &editor_platform::ipc::IpcCommand::Action("syntax-theme".to_string()),
                    start,
                );
            }
            if rest[0] == "--json" {
                let Some(raw) = rest.get(1) else {
                    emit_err("syntax", start, "E_USAGE", "usage: editor syntax theme --json <JSON>", None);
                    return 3;
                };
                return send_live_gui_command(
                    "syntax",
                    &editor_platform::ipc::IpcCommand::Action(format!("syntax-theme-json:{raw}")),
                    start,
                );
            }
            if rest[0] == "--stdin" || rest[0] == "-" || rest[0].starts_with('@') {
                let raw_arg = if rest[0] == "--stdin" { "-" } else { rest[0].as_str() };
                let payload = match load_text_arg(fs, globals, raw_arg) {
                    Ok(s) => s,
                    Err((c, m)) => {
                        emit_err("syntax", start, c, &m, None);
                        return 2;
                    }
                };
                return send_live_gui_command(
                    "syntax",
                    &editor_platform::ipc::IpcCommand::Action(format!("syntax-theme-json:{payload}")),
                    start,
                );
            }
            let arg = rest[0].trim();
            if arg.starts_with('{') {
                return send_live_gui_command(
                    "syntax",
                    &editor_platform::ipc::IpcCommand::Action(format!("syntax-theme-json:{arg}")),
                    start,
                );
            }
            send_live_gui_command(
                "syntax",
                &editor_platform::ipc::IpcCommand::Action(format!("syntax-theme-preset:{arg}")),
                start,
            )
        }
        "color" | "colour" => {
            let mut mode = "active".to_string();
            let mut pos: Vec<&str> = Vec::new();
            let mut i = 1;
            while i < args.len() {
                if args[i] == "--mode" && i + 1 < args.len() {
                    mode = args[i + 1].clone();
                    i += 2;
                } else {
                    pos.push(args[i].as_str());
                    i += 1;
                }
            }
            if pos.len() < 2 {
                emit_err(
                    "syntax",
                    start,
                    "E_USAGE",
                    "usage: editor syntax color <TOKEN> <#RRGGBB|reset> [normal|bold|italic] [--mode active|light|dark|both]",
                    None,
                );
                return 3;
            }
            let token = pos[0];
            let (color, style) = if pos.len() >= 3 {
                (pos[1], pos[2])
            } else if matches!(
                pos[1].to_ascii_lowercase().as_str(),
                "normal" | "regular" | "plain" | "bold" | "italic" | "italics"
            ) {
                ("keep", pos[1])
            } else {
                (pos[1], "keep")
            };
            send_live_gui_command(
                "syntax",
                &editor_platform::ipc::IpcCommand::Action(format!(
                    "syntax-color:{mode}:{token}:{color}:{style}"
                )),
                start,
            )
        }
        "reset" => {
            let mut mode = "all";
            if args.get(1).map(|s| s.as_str()) == Some("--mode") {
                if let Some(m) = args.get(2) {
                    mode = m.as_str();
                }
            } else if let Some(m) = args.get(1) {
                mode = m.as_str();
            }
            send_live_gui_command(
                "syntax",
                &editor_platform::ipc::IpcCommand::Action(format!("syntax-reset:{mode}")),
                start,
            )
        }
        "map" => {
            let spec = match (args.get(1), args.get(2)) {
                (Some(ext), Some(lang)) => format!("{ext}={lang}"),
                (Some(pair), None) if pair.contains('=') => pair.clone(),
                _ => {
                    emit_err(
                        "syntax",
                        start,
                        "E_USAGE",
                        "usage: editor syntax map <EXT> <LANG_ID|default>",
                        None,
                    );
                    return 3;
                }
            };
            send_live_gui_command(
                "syntax",
                &editor_platform::ipc::IpcCommand::Action(format!("syntax-map:{spec}")),
                start,
            )
        }
        other => {
            emit_err(
                "syntax",
                start,
                "E_USAGE",
                &format!("unknown syntax subcommand `{other}` (use list|set|map|theme|color|reset|style)"),
                None,
            );
            3
        }
    }
}

fn cmd_resize(args: &[String], start: Instant) -> u8 {
    let Some(target) = args.first().map(|s| s.as_str()) else {
        emit_err(
            "resize",
            start,
            "E_USAGE",
            "usage: editor resize <sidebar <WIDTH_PT|reset> | cols <RATIO|reset> | rows [0|1] <RATIO|reset> | reset>",
            None,
        );
        return 3;
    };
    let action = match target {
        "sidebar" | "menu" => {
            let Some(val) = args.get(1) else {
                emit_err("resize", start, "E_USAGE", "usage: editor resize sidebar <WIDTH_PT|reset>", None);
                return 3;
            };
            format!("sidebar-width:{val}")
        }
        "cols" | "col" | "horizontal" => {
            let Some(val) = args.get(1) else {
                emit_err("resize", start, "E_USAGE", "usage: editor resize cols <RATIO|50%|reset>", None);
                return 3;
            };
            format!("pane-ratio:cols={val}")
        }
        "rows" | "row" | "vertical" => match (args.get(1), args.get(2)) {
            (Some(col), Some(val)) if col == "0" || col == "1" => format!("pane-ratio:rows:{col}={val}"),
            (Some(val), None) => format!("pane-ratio:rows:all={val}"),
            _ => {
                emit_err("resize", start, "E_USAGE", "usage: editor resize rows [0|1] <RATIO|50%|reset>", None);
                return 3;
            }
        },
        "reset" => "pane-ratio:reset".to_string(),
        other => {
            emit_err(
                "resize",
                start,
                "E_USAGE",
                &format!("unknown resize target `{other}` (use sidebar|cols|rows|reset)"),
                None,
            );
            return 3;
        }
    };
    send_live_gui_command(
        "resize",
        &editor_platform::ipc::IpcCommand::Action(action),
        start,
    )
}

fn cmd_action_direct(name: &str, start: Instant) -> u8 {
    send_live_gui_command(
        name,
        &editor_platform::ipc::IpcCommand::Action(name.to_string()),
        start,
    )
}

fn cmd_action(fs: &dyn FileSystem, globals: &GlobalOpts, args: &[String], start: Instant) -> u8 {
    let Some(name) = args.first() else {
        emit_err(
            "action",
            start,
            "E_USAGE",
            "usage: editor action <new|reload|save|undo|redo|cut|copy|paste|select-all|find|replace|goto-line|quick-open|project-search|toggle-wrap|toggle-sidebar|sidebar-show|sidebar-hide|sidebar-root DIR|zoom-in|zoom-out|zoom-reset|theme-system|theme-light|theme-dark|duplicate-line|delete-line|toggle-comment|focus>",
            None,
        );
        return 3;
    };
    if name == "sidebar-width" {
        let Some(w) = args.get(1) else {
            emit_err(
                "action",
                start,
                "E_USAGE",
                "usage: editor action sidebar-width <WIDTH_PT|reset>",
                None,
            );
            return 3;
        };
        return send_live_gui_command(
            "action",
            &editor_platform::ipc::IpcCommand::Action(format!("sidebar-width:{w}")),
            start,
        );
    }
    if name == "max-tabs" {
        let Some(n) = args.get(1) else {
            emit_err(
                "action",
                start,
                "E_USAGE",
                "usage: editor action max-tabs <N> (0 = unlimited)",
                None,
            );
            return 3;
        };
        return send_live_gui_command(
            "action",
            &editor_platform::ipc::IpcCommand::Action(format!("max-tabs:{n}")),
            start,
        );
    }
    if name == "wrap-ext" {
        let spec = match (args.get(1), args.get(2)) {
            (Some(a), Some(b)) => format!("{a}={b}"),
            (Some(a), None) => a.clone(),
            _ => {
                emit_err(
                    "action",
                    start,
                    "E_USAGE",
                    "usage: editor action wrap-ext <EXT>=<on|off|default>",
                    None,
                );
                return 3;
            }
        };
        return send_live_gui_command(
            "action",
            &editor_platform::ipc::IpcCommand::Action(format!("wrap-ext:{spec}")),
            start,
        );
    }
    if name == "sidebar-root" {
        let Some(dir_arg) = args.get(1) else {
            emit_err("action", start, "E_USAGE", "sidebar-root requires <DIR>", None);
            return 3;
        };
        let dir = match resolve_confined(fs, globals, dir_arg) {
            Ok(p) => p,
            Err((c, m)) => {
                emit_err("action", start, c, &m, Some(Path::new(dir_arg)));
                return 2;
            }
        };
        return send_live_gui_command(
            "action",
            &editor_platform::ipc::IpcCommand::OpenFolder(dir),
            start,
        );
    }
    send_live_gui_command(
        "action",
        &editor_platform::ipc::IpcCommand::Action(name.clone()),
        start,
    )
}
