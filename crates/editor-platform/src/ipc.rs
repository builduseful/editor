//! Lightweight local IPC over a per-user `0600` Unix domain socket (zero external crates).
//!
//! Enables `editor open <path>`, `editor gui`, and `editor close` / `editor quit` to
//! communicate directly with a running `Editor.app` / `editor-gui` instance.

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
    Open(PathBuf),
    Quit { force: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IpcReply {
    Pong,
    Opened,
    Quitting,
    Prompting,
    Error(String),
}

impl IpcReply {
    fn to_wire(&self) -> String {
        match self {
            IpcReply::Pong => "OK PONG\n".to_string(),
            IpcReply::Opened => "OK OPENED\n".to_string(),
            IpcReply::Quitting => "OK QUITTING\n".to_string(),
            IpcReply::Prompting => "OK PROMPTING\n".to_string(),
            IpcReply::Error(m) => format!("ERR {}\n", m.replace('\n', " ")),
        }
    }

    fn from_wire(line: &str) -> Self {
        let trimmed = line.trim();
        match trimmed {
            "OK PONG" => IpcReply::Pong,
            "OK OPENED" => IpcReply::Opened,
            "OK QUITTING" => IpcReply::Quitting,
            "OK PROMPTING" => IpcReply::Prompting,
            other => {
                if let Some(msg) = other.strip_prefix("ERR ") {
                    IpcReply::Error(msg.to_string())
                } else {
                    IpcReply::Error(format!("unexpected reply: {other}"))
                }
            }
        }
    }
}

/// Resolve the default per-user IPC socket path (honors `EDITOR_IPC_SOCK` for isolated tests).
pub fn default_socket_path() -> PathBuf {
    if let Ok(override_path) = std::env::var("EDITOR_IPC_SOCK") {
        if !override_path.is_empty() {
            return PathBuf::from(override_path);
        }
    }
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "user".to_string());
    let safe_user: String = user
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    std::env::temp_dir().join(format!("editor-ipc-{safe_user}.sock"))
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
        // Remove stale socket if no live server answers PING.
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
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
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
        let cmd = if let Some(path) = trimmed.strip_prefix("OPEN ") {
            Some(IpcCommand::Open(PathBuf::from(path)))
        } else if trimmed == "QUIT" {
            Some(IpcCommand::Quit { force: false })
        } else if trimmed == "QUIT_FORCE" {
            Some(IpcCommand::Quit { force: true })
        } else {
            None
        };

        match cmd {
            None => IpcReply::Error("unknown IPC command".to_string()),
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
                    .wait_timeout_while(guard, Duration::from_secs(2), |r| r.is_none())
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
    let wire = match cmd {
        IpcCommand::Open(p) => format!("OPEN {}\n", p.to_string_lossy().replace('\n', "")),
        IpcCommand::Quit { force: false } => "QUIT\n".to_string(),
        IpcCommand::Quit { force: true } => "QUIT_FORCE\n".to_string(),
    };
    send_raw(socket_path, &wire)
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

        // Send OPEN from a client thread and answer on the main thread.
        let s2 = sock.clone();
        let client_t = thread::spawn(move || {
            send_command(&s2, &IpcCommand::Open(PathBuf::from("/tmp/hello.rs"))).unwrap()
        });

        // Poll briefly for the queued command.
        let mut received = None;
        for _ in 0..100 {
            if let Some(p) = server.try_recv() {
                received = Some(p);
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        let pending = received.expect("server should receive OPEN command");
        assert!(woken.load(Ordering::SeqCst));
        assert_eq!(pending.cmd, IpcCommand::Open(PathBuf::from("/tmp/hello.rs")));
        pending.respond(IpcReply::Opened);
        assert_eq!(client_t.join().unwrap(), IpcReply::Opened);

        drop(server);
        assert!(!sock.exists(), "socket should be removed on drop");
    }
}
