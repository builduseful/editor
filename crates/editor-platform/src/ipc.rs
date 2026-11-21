//! Lightweight local IPC over a per-user `0600` Unix domain socket (zero external crates).
//!
//! Gives the `editor` CLI (and AI agents driving it) full deterministic inspection and
//! control over a running GUI instance: `status`, `open`, `goto`, `select`, `insert`,
//! `find`, `save`, `undo`, `redo`, `action`, and `close`/`quit`.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IpcCommand {
    Status,
    Open(PathBuf),
    OpenFolder(PathBuf),
    Goto {
        line_1based: usize,
        col_bytes: usize,
    },
    Select {
        start_line: usize,
        start_col: usize,
        end_line: usize,
        end_col: usize,
    },
    Insert {
        text: String,
    },
    Find {
        query: String,
        replace: Option<String>,
        case_sensitive: bool,
        replace_all: bool,
        close: bool,
    },
    Save {
        as_path: Option<PathBuf>,
    },
    Action(String),
    Quit {
        force: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IpcReply {
    Pong,
    Opened,
    Quitting,
    Prompting,
    Json(String),
    Error(String),
}

/// Lossless single-line escaping for wire payloads (`\` -> `\\`, `\n` -> `\n`, `\r` -> `\r`, `\t` -> `\t`).
fn escape_field(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}

fn unescape_field(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('\\') => out.push('\\'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(ch);
        }
    }
    out
}

impl IpcCommand {
    fn to_wire(&self) -> String {
        match self {
            IpcCommand::Status => "STATUS\n".to_string(),
            IpcCommand::Open(p) => format!("OPEN {}\n", escape_field(&p.to_string_lossy())),
            IpcCommand::OpenFolder(p) => {
                format!("OPEN_FOLDER {}\n", escape_field(&p.to_string_lossy()))
            }
            IpcCommand::Goto {
                line_1based,
                col_bytes,
            } => format!("GOTO {line_1based} {col_bytes}\n"),
            IpcCommand::Select {
                start_line,
                start_col,
                end_line,
                end_col,
            } => format!("SELECT {start_line} {start_col} {end_line} {end_col}\n"),
            IpcCommand::Insert { text } => format!("INSERT {}\n", escape_field(text)),
            IpcCommand::Find {
                query,
                replace,
                case_sensitive,
                replace_all,
                close,
            } => {
                let flags = u8::from(*case_sensitive)
                    | (u8::from(*replace_all) << 1)
                    | (u8::from(*close) << 2)
                    | (u8::from(replace.is_some()) << 3);
                let q = escape_field(query);
                if let Some(rep) = replace {
                    format!("FIND {flags}\t{q}\t{}\n", escape_field(rep))
                } else {
                    format!("FIND {flags}\t{q}\n")
                }
            }
            IpcCommand::Save { as_path: None } => "SAVE\n".to_string(),
            IpcCommand::Save { as_path: Some(p) } => {
                format!("SAVE_AS {}\n", escape_field(&p.to_string_lossy()))
            }
            IpcCommand::Action(name) => format!("ACTION {}\n", escape_field(name)),
            IpcCommand::Quit { force: false } => "QUIT\n".to_string(),
            IpcCommand::Quit { force: true } => "QUIT_FORCE\n".to_string(),
        }
    }

    fn from_wire(line: &str) -> Option<Self> {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed == "STATUS" {
            return Some(IpcCommand::Status);
        }
        if trimmed == "SAVE" {
            return Some(IpcCommand::Save { as_path: None });
        }
        if trimmed == "QUIT" {
            return Some(IpcCommand::Quit { force: false });
        }
        if trimmed == "QUIT_FORCE" {
            return Some(IpcCommand::Quit { force: true });
        }
        if let Some(rest) = trimmed.strip_prefix("OPEN ") {
            return Some(IpcCommand::Open(PathBuf::from(unescape_field(rest))));
        }
        if let Some(rest) = trimmed.strip_prefix("OPEN_FOLDER ") {
            return Some(IpcCommand::OpenFolder(PathBuf::from(unescape_field(rest))));
        }
        if let Some(rest) = trimmed.strip_prefix("SAVE_AS ") {
            return Some(IpcCommand::Save {
                as_path: Some(PathBuf::from(unescape_field(rest))),
            });
        }
        if let Some(rest) = trimmed.strip_prefix("GOTO ") {
            let mut it = rest.split_whitespace();
            let line_1based = it.next()?.parse().ok()?;
            let col_bytes = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            return Some(IpcCommand::Goto {
                line_1based,
                col_bytes,
            });
        }
        if let Some(rest) = trimmed.strip_prefix("SELECT ") {
            let mut it = rest.split_whitespace();
            let start_line = it.next()?.parse().ok()?;
            let start_col = it.next()?.parse().ok()?;
            let end_line = it.next()?.parse().ok()?;
            let end_col = it.next()?.parse().ok()?;
            return Some(IpcCommand::Select {
                start_line,
                start_col,
                end_line,
                end_col,
            });
        }
        if let Some(rest) = trimmed.strip_prefix("INSERT ") {
            return Some(IpcCommand::Insert {
                text: unescape_field(rest),
            });
        }
        if let Some(rest) = trimmed.strip_prefix("FIND ") {
            let mut parts = rest.split('\t');
            let flags: u8 = parts.next()?.parse().ok()?;
            let query = unescape_field(parts.next().unwrap_or(""));
            let has_replace = (flags & 8) != 0;
            let replace = if has_replace {
                Some(unescape_field(parts.next().unwrap_or("")))
            } else {
                None
            };
            return Some(IpcCommand::Find {
                query,
                replace,
                case_sensitive: (flags & 1) != 0,
                replace_all: (flags & 2) != 0,
                close: (flags & 4) != 0,
            });
        }
        if let Some(rest) = trimmed.strip_prefix("ACTION ") {
            return Some(IpcCommand::Action(unescape_field(rest)));
        }
        None
    }
}

impl IpcReply {
    fn to_wire(&self) -> String {
        match self {
            IpcReply::Pong => "OK PONG\n".to_string(),
            IpcReply::Opened => "OK OPENED\n".to_string(),
            IpcReply::Quitting => "OK QUITTING\n".to_string(),
            IpcReply::Prompting => "OK PROMPTING\n".to_string(),
            IpcReply::Json(j) => format!("OK_JSON {}\n", j.replace('\n', " ")),
            IpcReply::Error(m) => format!("ERR {}\n", m.replace('\n', " ")),
        }
    }

    fn from_wire(line: &str) -> Self {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        match trimmed {
            "OK PONG" => IpcReply::Pong,
            "OK OPENED" => IpcReply::Opened,
            "OK QUITTING" => IpcReply::Quitting,
            "OK PROMPTING" => IpcReply::Prompting,
            other => {
                if let Some(j) = other.strip_prefix("OK_JSON ") {
                    IpcReply::Json(j.to_string())
                } else if let Some(msg) = other.strip_prefix("ERR ") {
                    IpcReply::Error(msg.to_string())
                } else {
                    IpcReply::Error(format!("unexpected reply: {other}"))
                }
            }
        }
    }
}

/// Resolve the canonical per-user IPC socket path (honors `EDITOR_IPC_SOCK` for isolated tests).
/// Uses `/tmp/editor-ipc-<uid>.sock` on Unix so GUI apps launched via macOS LaunchServices
/// (which have a different `$TMPDIR` than terminal shells) and CLI processes always meet!
pub fn default_socket_path() -> PathBuf {
    if let Ok(override_path) = std::env::var("EDITOR_IPC_SOCK") {
        if !override_path.is_empty() {
            return PathBuf::from(override_path);
        }
    }
    #[cfg(unix)]
    {
        extern "C" {
            fn getuid() -> u32;
        }
        let uid = unsafe { getuid() };
        PathBuf::from(format!("/tmp/editor-ipc-{uid}.sock"))
    }
    #[cfg(not(unix))]
    {
        std::env::temp_dir().join("editor-ipc.sock")
    }
}

pub struct PendingIpc {
    pub cmd: IpcCommand,
    reply_slot: Arc<(Mutex<Option<IpcReply>>, std::sync::Condvar)>,
}

impl PendingIpc {
    pub fn respond(self, reply: IpcReply) {
        let (lock, cvar) = &*self.reply_slot;
        let mut guard = lock.lock().unwrap();
        *guard = Some(reply);
        cvar.notify_one();
    }
}

/// Non-blocking IPC server running a tiny background accept thread.
/// Removes its socket file cleanly on Drop.
pub struct IpcServer {
    socket_path: PathBuf,
    queue: Arc<Mutex<VecDeque<PendingIpc>>>,
    stop: Arc<AtomicBool>,
}

impl IpcServer {
    #[cfg(unix)]
    pub fn bind(
        socket_path: PathBuf,
        on_wake: Arc<dyn Fn() + Send + Sync>,
    ) -> std::io::Result<Self> {
        if socket_path.exists() {
            if send_ping(&socket_path).is_ok() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    "another editor GUI instance is already listening",
                ));
            }
            let _ = std::fs::remove_file(&socket_path);
        }

        let listener = UnixListener::bind(&socket_path)?;
        let _ = std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600));
        listener.set_nonblocking(true)?;

        let queue: Arc<Mutex<VecDeque<PendingIpc>>> = Arc::new(Mutex::new(VecDeque::new()));
        let stop = Arc::new(AtomicBool::new(false));

        let q_clone = Arc::clone(&queue);
        let stop_clone = Arc::clone(&stop);

        thread::spawn(move || {
            while !stop_clone.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
                        let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
                        handle_client(stream, &q_clone, &on_wake);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(15));
                    }
                    Err(_) => {
                        thread::sleep(Duration::from_millis(50));
                    }
                }
            }
        });

        Ok(Self {
            socket_path,
            queue,
            stop,
        })
    }

    pub fn try_recv(&self) -> Option<PendingIpc> {
        self.queue.lock().unwrap().pop_front()
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

#[cfg(unix)]
fn handle_client(
    mut stream: UnixStream,
    queue: &Arc<Mutex<VecDeque<PendingIpc>>>,
    on_wake: &Arc<dyn Fn() + Send + Sync>,
) {
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let trimmed = line.trim_end_matches(['\r', '\n']);
    let reply = if trimmed == "PING" {
        IpcReply::Pong
    } else {
        match IpcCommand::from_wire(trimmed) {
            None => IpcReply::Error(format!("unknown IPC command: {trimmed}")),
            Some(cmd) => {
                let slot = Arc::new((Mutex::new(None), std::sync::Condvar::new()));
                {
                    let mut q = queue.lock().unwrap();
                    q.push_back(PendingIpc {
                        cmd,
                        reply_slot: Arc::clone(&slot),
                    });
                }
                on_wake();
                let (lock, cvar) = &*slot;
                let guard = lock.lock().unwrap();
                let (mut guard, _) = cvar
                    .wait_timeout_while(guard, Duration::from_secs(3), |r| r.is_none())
                    .unwrap();
                guard
                    .take()
                    .unwrap_or_else(|| IpcReply::Error("GUI did not respond in time".to_string()))
            }
        }
    };
    let _ = stream.write_all(reply.to_wire().as_bytes());
    let _ = stream.flush();
}

#[cfg(unix)]
pub fn send_ping(socket_path: &Path) -> std::io::Result<IpcReply> {
    send_raw(socket_path, "PING\n")
}

#[cfg(unix)]
pub fn send_command(socket_path: &Path, cmd: &IpcCommand) -> std::io::Result<IpcReply> {
    send_raw(socket_path, &cmd.to_wire())
}

#[cfg(unix)]
fn send_raw(socket_path: &Path, wire: &str) -> std::io::Result<IpcReply> {
    let mut stream = UnixStream::connect(socket_path)?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    stream.write_all(wire.as_bytes())?;
    stream.flush()?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    Ok(IpcReply::from_wire(&line))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_encoding_round_trips_multiline_text_and_find() {
        let cmds = [
            IpcCommand::Status,
            IpcCommand::Open(PathBuf::from("/tmp/a b/file.rs")),
            IpcCommand::Goto {
                line_1based: 42,
                col_bytes: 8,
            },
            IpcCommand::Select {
                start_line: 1,
                start_col: 0,
                end_line: 3,
                end_col: 5,
            },
            IpcCommand::Insert {
                text: "line1\n\tline2 🚀\\end".to_string(),
            },
            IpcCommand::Find {
                query: "foo\tbar".to_string(),
                replace: Some("baz\nqux".to_string()),
                case_sensitive: true,
                replace_all: true,
                close: false,
            },
            IpcCommand::Save {
                as_path: Some(PathBuf::from("/tmp/out.rs")),
            },
            IpcCommand::Action("toggle-wrap".to_string()),
            IpcCommand::Quit { force: true },
        ];
        for c in cmds {
            let wire = c.to_wire();
            assert_eq!(IpcCommand::from_wire(&wire), Some(c));
        }
    }

    #[test]
    fn ipc_server_and_client_round_trip() {
        let sock = std::env::temp_dir().join(format!("editor_ipc_unit_{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&sock);
        let woken = Arc::new(AtomicBool::new(false));
        let w2 = Arc::clone(&woken);
        let server = IpcServer::bind(
            sock.clone(),
            Arc::new(move || {
                w2.store(true, Ordering::SeqCst);
            }),
        )
        .unwrap();

        assert_eq!(send_ping(&sock).unwrap(), IpcReply::Pong);

        let s2 = sock.clone();
        let client_t = thread::spawn(move || send_command(&s2, &IpcCommand::Status).unwrap());

        let mut received = None;
        for _ in 0..100 {
            if let Some(p) = server.try_recv() {
                received = Some(p);
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        let pending = received.expect("server should receive STATUS command");
        assert!(woken.load(Ordering::SeqCst));
        assert_eq!(pending.cmd, IpcCommand::Status);
        pending.respond(IpcReply::Json("{\"dirty\":false}".to_string()));
        assert_eq!(
            client_t.join().unwrap(),
            IpcReply::Json("{\"dirty\":false}".to_string())
        );

        drop(server);
        assert!(!sock.exists(), "socket should be removed on drop");
    }
}
