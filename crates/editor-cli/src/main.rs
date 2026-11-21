//! Deterministic JSON-first CLI binary (`editor`) sharing `editor-core`.

mod json;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use editor_core::{Document, DocumentError, SearchOptions};
use editor_platform::{FileRev, FileSystem, StdFileSystem};

use crate::json::JsonBuf;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> ExitCode {
    let start = Instant::now();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args[0] == "--help" || args[0] == "-h" || args[0] == "help" {
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
    let cmd = args[0].as_str();
    let rest = &args[1..];

    let code = match cmd {
        "schema" => {
            let mut res = JsonBuf::new();
            res.push_raw("[{\"commands\":[\"read\",\"write\",\"edit\",\"info\",\"validate\",\"version\",\"schema\"]}]");
            emit_ok("schema", start, &res.into_string());
            0
        }
        "info" => cmd_info(&fs, rest, start),
        "read" => cmd_read(&fs, rest, start),
        "validate" => cmd_validate(&fs, rest, start),
        "write" => cmd_write(&fs, rest, start),
        "edit" => cmd_edit(&fs, rest, start),
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
        "editor {VERSION} — fast, deterministic Rust text editor CLI\n\
         \n\
         Commands:\n\
           editor read <path> [--from N] [--to N] [--max-bytes N]\n\
           editor info <path>\n\
           editor validate <path>\n\
           editor write <path> (--stdin | @FILE) [--expect REV | --force] [--in-place]\n\
           editor edit <path> --old TEXT --new TEXT [--all] (--expect REV | --force) [--in-place]\n\
           editor version\n\
           editor schema"
    );
}

fn cwd_string() -> String {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_string())
}

fn emit_ok(command: &str, start: Instant, results_json: &str) {
    let mut jb = JsonBuf::new();
    jb.push_raw("{\"schema\":\"editor.result/1\",\"command\":");
    jb.push_str(command);
    jb.push_raw(",\"ok\":true,\"duration_us\":");
    jb.push_u64(start.elapsed().as_micros() as u64);
    jb.push_raw(",\"cwd\":");
    jb.push_str(&cwd_string());
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
    let mut jb = JsonBuf::new();
    jb.push_raw("{\"schema\":\"editor.result/1\",\"command\":");
    jb.push_str(command);
    jb.push_raw(",\"ok\":false,\"duration_us\":");
    jb.push_u64(start.elapsed().as_micros() as u64);
    jb.push_raw(",\"cwd\":");
    jb.push_str(&cwd_string());
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

fn push_doc_fields(jb: &mut JsonBuf, info: &editor_core::DocumentInfo) {
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
    jb.push_raw(",\"rev\":");
    jb.push_rev(info.rev);
}

fn cmd_info(fs: &dyn FileSystem, args: &[String], start: Instant) -> u8 {
    let Some(path_arg) = args.first() else {
        emit_err("info", start, "E_USAGE", "missing <path>", None);
        return 3;
    };
    let path = PathBuf::from(path_arg);
    match Document::open(fs, &path) {
        Ok(mut doc) => {
            let info = doc.info();
            let mut jb = JsonBuf::new();
            jb.push_raw("[{");
            push_doc_fields(&mut jb, &info);
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

fn cmd_read(fs: &dyn FileSystem, args: &[String], start: Instant) -> u8 {
    let Some(path_arg) = args.first() else {
        emit_err("read", start, "E_USAGE", "missing <path>", None);
        return 3;
    };
    let path = PathBuf::from(path_arg);
    let mut from_line: usize = 1;
    let mut to_line: Option<usize> = None;
    let mut max_bytes: usize = 256 * 1024;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--from" if i + 1 < args.len() => {
                match args[i + 1].parse::<usize>() {
                    Ok(n) if n >= 1 => from_line = n,
                    _ => {
                        emit_err("read", start, "E_USAGE", "--from must be >= 1", Some(&path));
                        return 3;
                    }
                }
                i += 2;
            }
            "--to" if i + 1 < args.len() => {
                match args[i + 1].parse::<usize>() {
                    Ok(n) if n >= 1 => to_line = Some(n),
                    _ => {
                        emit_err("read", start, "E_USAGE", "--to must be >= 1", Some(&path));
                        return 3;
                    }
                }
                i += 2;
            }
            "--max-bytes" if i + 1 < args.len() => {
                match args[i + 1].parse::<usize>() {
                    Ok(n) => max_bytes = n,
                    _ => {
                        emit_err("read", start, "E_USAGE", "invalid --max-bytes", Some(&path));
                        return 3;
                    }
                }
                i += 2;
            }
            other => {
                emit_err(
                    "read",
                    start,
                    "E_USAGE",
                    &format!("unexpected flag: {other}"),
                    Some(&path),
                );
                return 3;
            }
        }
    }

    let mut doc = match Document::open(fs, &path) {
        Ok(d) => d,
        Err(e) => {
            emit_err("read", start, doc_err_code(&e), &e.to_string(), Some(&path));
            return 2;
        }
    };

    let info = doc.info();
    let end_1based = to_line.unwrap_or(info.line_count).min(info.line_count);
    let mut content_bytes = Vec::new();
    let mut truncated = false;
    let mut actual_to = 0usize;

    if info.line_count > 0 && from_line <= end_1based {
        for l1 in from_line..=end_1based {
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
    push_doc_fields(&mut jb, &info);
    jb.push_raw(",\"from\":");
    jb.push_usize(from_line);
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

fn cmd_validate(fs: &dyn FileSystem, args: &[String], start: Instant) -> u8 {
    let Some(path_arg) = args.first() else {
        emit_err("validate", start, "E_USAGE", "missing <path>", None);
        return 3;
    };
    let path = PathBuf::from(path_arg);
    let mut doc = match Document::open(fs, &path) {
        Ok(d) => d,
        Err(e) => {
            emit_err(
                "validate",
                start,
                doc_err_code(&e),
                &e.to_string(),
                Some(&path),
            );
            return 2;
        }
    };
    let info = doc.info();
    match doc.validate_utf8() {
        Ok(()) => {
            let mut jb = JsonBuf::new();
            jb.push_raw("[{");
            push_doc_fields(&mut jb, &info);
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

fn cmd_write(fs: &dyn FileSystem, args: &[String], start: Instant) -> u8 {
    let Some(path_arg) = args.first() else {
        emit_err("write", start, "E_USAGE", "missing <path>", None);
        return 3;
    };
    let path = PathBuf::from(path_arg);
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
                payload_file = Some(PathBuf::from(&s[1..]));
                i += 1;
            }
            other => {
                emit_err(
                    "write",
                    start,
                    "E_USAGE",
                    &format!("unexpected arg: {other}"),
                    Some(&path),
                );
                return 3;
            }
        }
    }

    if use_stdin == payload_file.is_some() {
        emit_err(
            "write",
            start,
            "E_USAGE",
            "provide exactly one of --stdin or @FILE",
            Some(&path),
        );
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
        emit_err(
            "write",
            start,
            "E_USAGE",
            "existing file requires --expect or --force",
            Some(&path),
        );
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
            push_doc_fields(&mut jb, &info);
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

fn cmd_edit(fs: &dyn FileSystem, args: &[String], start: Instant) -> u8 {
    let Some(path_arg) = args.first() else {
        emit_err("edit", start, "E_USAGE", "missing <path>", None);
        return 3;
    };
    let path = PathBuf::from(path_arg);
    let mut old_text: Option<String> = None;
    let mut new_text: Option<String> = None;
    let mut replace_all = false;
    let mut expect: Option<FileRev> = None;
    let mut force = false;
    let mut in_place = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--old" if i + 1 < args.len() => {
                old_text = Some(args[i + 1].clone());
                i += 2;
            }
            "--new" if i + 1 < args.len() => {
                new_text = Some(args[i + 1].clone());
                i += 2;
            }
            "--all" => {
                replace_all = true;
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
                        emit_err("edit", start, "E_USAGE", "invalid --expect rev", Some(&path));
                        return 3;
                    }
                }
                i += 2;
            }
            other => {
                emit_err(
                    "edit",
                    start,
                    "E_USAGE",
                    &format!("unexpected arg: {other}"),
                    Some(&path),
                );
                return 3;
            }
        }
    }

    let (Some(old), Some(new)) = (old_text, new_text) else {
        emit_err("edit", start, "E_USAGE", "require --old and --new", Some(&path));
        return 3;
    };
    if old.is_empty() {
        emit_err("edit", start, "E_USAGE", "--old must be non-empty", Some(&path));
        return 3;
    }
    if expect.is_none() && !force {
        emit_err(
            "edit",
            start,
            "E_USAGE",
            "edit requires --expect or --force",
            Some(&path),
        );
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

    let matches = doc.find_all(&old, SearchOptions::default());
    if matches.is_empty() {
        emit_err("edit", start, "E_NO_MATCH", "no match found", Some(&path));
        return 2;
    }
    if !replace_all && matches.len() > 1 {
        emit_err(
            "edit",
            start,
            "E_AMBIGUOUS",
            &format!("matched {} occurrences; pass --all to replace all", matches.len()),
            Some(&path),
        );
        return 2;
    }

    let count = doc.replace_all(&old, &new, SearchOptions::default(), 0);
    let _ = count;

    match doc.save(fs, expect, in_place) {
        Ok(report) => {
            let info = doc.info();
            let mut jb = JsonBuf::new();
            jb.push_raw("[{");
            push_doc_fields(&mut jb, &info);
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
