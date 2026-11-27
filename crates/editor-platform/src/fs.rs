use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsError {
    NotFound(PathBuf),
    AlreadyExists(PathBuf),
    PermissionDenied(PathBuf),
    NotRegularFile(PathBuf),
    Conflict(String),
    Locked(PathBuf),
    Unsupported(String),
    Io(String),
}

impl std::fmt::Display for FsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FsError::NotFound(p) => write!(f, "not found: {}", p.display()),
            FsError::AlreadyExists(p) => write!(f, "already exists: {}", p.display()),
            FsError::PermissionDenied(p) => write!(f, "permission denied: {}", p.display()),
            FsError::NotRegularFile(p) => write!(f, "not a regular file: {}", p.display()),
            FsError::Conflict(m) => write!(f, "conflict: {m}"),
            FsError::Locked(p) => write!(f, "locked: {}", p.display()),
            FsError::Unsupported(m) => write!(f, "unsupported: {m}"),
            FsError::Io(m) => write!(f, "i/o error: {m}"),
        }
    }
}

impl std::error::Error for FsError {}

pub type FsResult<T> = Result<T, FsError>;

/// Stat-based file identity used for optimistic concurrency / external-edit detection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct FileRev {
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mtime_ns: u64,
    pub ctime_ns: u64,
}

impl FileRev {
    pub fn to_stat_string(&self) -> String {
        format!(
            "stat:{}:{}:{}:{}:{}",
            self.dev, self.ino, self.size, self.mtime_ns, self.ctime_ns
        )
    }

    pub fn parse_stat_string(s: &str) -> Option<Self> {
        let rest = s.strip_prefix("stat:")?;
        let mut parts = rest.split(':');
        let dev = parts.next()?.parse().ok()?;
        let ino = parts.next()?.parse().ok()?;
        let size = parts.next()?.parse().ok()?;
        let mtime_ns = parts.next()?.parse().ok()?;
        let ctime_ns = parts.next()?.parse().ok()?;
        if parts.next().is_some() {
            return None;
        }
        Some(Self {
            dev,
            ino,
            size,
            mtime_ns,
            ctime_ns,
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FileStat {
    pub rev: FileRev,
    pub mode: u32,
    pub is_file: bool,
    pub is_dir: bool,
    pub is_symlink: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub path: PathBuf,
    pub kind: EntryKind,
}

/// RAII advisory lockfile guard (`<path>.editor.lock`), automatically released on Drop.
pub struct FileLock {
    unlock_fn: Option<Box<dyn FnOnce() + Send>>,
}

impl FileLock {
    pub fn new(unlock_fn: impl FnOnce() + Send + 'static) -> Self {
        Self {
            unlock_fn: Some(Box::new(unlock_fn)),
        }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        if let Some(f) = self.unlock_fn.take() {
            f();
        }
    }
}

/// Random-access read-only file handle for zero-load multi-GB document backing.
pub trait FileHandle: Send + Sync {
    fn len(&self) -> u64;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn stat(&self) -> FsResult<FileStat>;
    /// Read up to `buf.len()` bytes starting at `offset`. Returns bytes read (0 at EOF).
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> FsResult<usize>;
}

/// Minimal OS filesystem adapter.
pub trait FileSystem: Send + Sync {
    fn open_read(&self, path: &Path) -> FsResult<Arc<dyn FileHandle>>;

    /// Fast zero-handle prefix read into a caller-owned buffer (avoids `Arc`/`PathBuf` allocs
    /// when scanning thousands of files). Returns `Ok(Some(total_file_len))` for regular files
    /// (with `out` holding `min(total_file_len, max_bytes)` bytes), or `Ok(None)` if non-regular.
    fn read_prefix_into(
        &self,
        path: &Path,
        max_bytes: usize,
        out: &mut Vec<u8>,
    ) -> FsResult<Option<u64>> {
        let handle = match self.open_read(path) {
            Ok(h) => h,
            Err(FsError::NotRegularFile(_)) => return Ok(None),
            Err(e) => return Err(e),
        };
        let total = handle.len();
        let to_read = (total as usize).min(max_bytes);
        out.clear();
        out.resize(to_read, 0);
        let mut got = 0usize;
        while got < to_read {
            match handle.read_at(got as u64, &mut out[got..]) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(e) => return Err(e),
            }
        }
        out.truncate(got);
        Ok(Some(total))
    }

    fn stat(&self, path: &Path) -> FsResult<FileStat>;
    fn lstat(&self, path: &Path) -> FsResult<FileStat>;
    fn canonicalize(&self, path: &Path) -> FsResult<PathBuf>;
    fn read_dir(&self, path: &Path) -> FsResult<Vec<DirEntry>>;

    /// Atomically replace or create `path` with the contents streamed by `write_fn`.
    /// Platform adapters (e.g. macOS APFS) may also override `clone_file` for O(1) CoW.
    fn write_atomic(
        &self,
        path: &Path,
        write_fn: &mut dyn FnMut(&mut dyn io::Write) -> io::Result<()>,
    ) -> FsResult<FileStat>;

    /// Write bytes directly into an existing file at `offset`, optionally truncating to `new_len`.
    fn write_in_place(
        &self,
        path: &Path,
        ranges: &[(u64, &[u8])],
        new_len: Option<u64>,
    ) -> FsResult<FileStat>;

    /// Fast copy-on-write clone (APFS `clonefile` / Btrfs `FICLONE`).
    /// Returns `Err(FsError::Unsupported(_))` if the platform/FS does not support it.
    fn clone_file(&self, _src: &Path, _dst: &Path) -> FsResult<()> {
        Err(FsError::Unsupported("clone_file not supported".into()))
    }

    fn create_dir(&self, path: &Path) -> FsResult<()>;
    fn rename(&self, from: &Path, to: &Path) -> FsResult<()>;
    fn remove_file(&self, path: &Path) -> FsResult<()>;
    fn remove_dir_all(&self, path: &Path) -> FsResult<()>;
    fn temp_dir(&self) -> PathBuf;

    /// Acquire an exclusive advisory lockfile (`<path>.editor.lock`, `O_CREAT|O_EXCL`).
    fn lock_file(&self, path: &Path) -> FsResult<FileLock>;
}

// ============================================================================
// Portable std::fs implementation (Unix + Windows)
// ============================================================================

/// Recursively ensure `dir` exists through `fs` (works on both `StdFileSystem` and
/// `MemoryFileSystem`) and tighten directory permissions to `0o700` on Unix.
pub fn ensure_private_dir(fs: &dyn FileSystem, dir: &Path) -> FsResult<()> {
    if fs.stat(dir).map(|s| s.is_dir).unwrap_or(false) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
        return Ok(());
    }
    let mut missing: Vec<PathBuf> = Vec::new();
    let mut cur = Some(dir.to_path_buf());
    while let Some(p) = cur {
        if fs.stat(&p).map(|s| s.is_dir).unwrap_or(false) {
            break;
        }
        let parent = p.parent().map(Path::to_path_buf);
        if parent.as_deref() == Some(p.as_path()) {
            break;
        }
        missing.push(p);
        cur = parent;
    }
    for p in missing.into_iter().rev() {
        match fs.create_dir(&p) {
            Ok(()) => {}
            Err(FsError::AlreadyExists(_)) if fs.stat(&p).map(|s| s.is_dir).unwrap_or(false) => {}
            Err(e) => return Err(e),
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

/// Best-effort tighten a private app-state file to `0o600` on Unix (no-op on Windows / memory FS).
pub fn set_private_file_permissions(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

#[derive(Debug, Default)]
pub struct StdFileSystem;

struct StdFileHandle {
    file: File,
    len: u64,
    path: PathBuf,
}

fn map_io_err(path: &Path, err: io::Error) -> FsError {
    match err.kind() {
        io::ErrorKind::NotFound => FsError::NotFound(path.to_path_buf()),
        io::ErrorKind::AlreadyExists => FsError::AlreadyExists(path.to_path_buf()),
        io::ErrorKind::PermissionDenied => FsError::PermissionDenied(path.to_path_buf()),
        _ => FsError::Io(format!("{}: {}", path.display(), err)),
    }
}

fn metadata_to_stat(meta: &std::fs::Metadata) -> FileStat {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let mtime_ns = (meta.mtime() as u64)
            .wrapping_mul(1_000_000_000)
            .wrapping_add(meta.mtime_nsec() as u64);
        let ctime_ns = (meta.ctime() as u64)
            .wrapping_mul(1_000_000_000)
            .wrapping_add(meta.ctime_nsec() as u64);
        FileStat {
            rev: FileRev {
                dev: meta.dev(),
                ino: meta.ino(),
                size: meta.len(),
                mtime_ns,
                ctime_ns,
            },
            mode: meta.mode(),
            is_file: meta.file_type().is_file(),
            is_dir: meta.file_type().is_dir(),
            is_symlink: meta.file_type().is_symlink(),
        }
    }
    #[cfg(not(unix))]
    {
        let mtime_ns = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        FileStat {
            rev: FileRev {
                dev: 0,
                ino: 0,
                size: meta.len(),
                mtime_ns,
                ctime_ns: mtime_ns,
            },
            mode: 0o644,
            is_file: meta.file_type().is_file(),
            is_dir: meta.file_type().is_dir(),
            is_symlink: meta.file_type().is_symlink(),
        }
    }
}

impl FileHandle for StdFileHandle {
    fn len(&self) -> u64 {
        self.len
    }

    fn stat(&self) -> FsResult<FileStat> {
        let meta = self
            .file
            .metadata()
            .map_err(|e| map_io_err(&self.path, e))?;
        Ok(metadata_to_stat(&meta))
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> FsResult<usize> {
        if buf.is_empty() || offset >= self.len {
            return Ok(0);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            loop {
                match self.file.read_at(buf, offset) {
                    Ok(n) => return Ok(n),
                    Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(map_io_err(&self.path, e)),
                }
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::FileExt;
            self.file
                .seek_read(buf, offset)
                .map_err(|e| map_io_err(&self.path, e))
        }
    }
}

fn unique_temp_path(target: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let raw_base = target
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file");
    // Cap base prefix at <= 80 UTF-8 bytes so `.{base}.editor-tmp-{pid}-{nanos}-{seq}` stays well under NAME_MAX (255).
    let mut base_end = raw_base.len().min(80);
    while base_end > 0 && !raw_base.is_char_boundary(base_end) {
        base_end -= 1;
    }
    let base = if base_end == 0 { "file" } else { &raw_base[..base_end] };
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    parent.join(format!(".{base}.editor-tmp-{pid}-{nanos}-{seq}"))
}

impl FileSystem for StdFileSystem {
    fn open_read(&self, path: &Path) -> FsResult<Arc<dyn FileHandle>> {
        let file = File::open(path).map_err(|e| map_io_err(path, e))?;
        let fmeta = file.metadata().map_err(|e| map_io_err(path, e))?;
        if !fmeta.file_type().is_file() {
            return Err(FsError::NotRegularFile(path.to_path_buf()));
        }
        Ok(Arc::new(StdFileHandle {
            file,
            len: fmeta.len(),
            path: path.to_path_buf(),
        }))
    }

    fn read_prefix_into(
        &self,
        path: &Path,
        max_bytes: usize,
        out: &mut Vec<u8>,
    ) -> FsResult<Option<u64>> {
        use std::io::Read;
        let mut file = File::open(path).map_err(|e| map_io_err(path, e))?;
        let fmeta = file.metadata().map_err(|e| map_io_err(path, e))?;
        if !fmeta.file_type().is_file() {
            return Ok(None);
        }
        let total = fmeta.len();
        let to_read = (total as usize).min(max_bytes);
        if out.len() < to_read {
            out.resize(to_read, 0);
        }
        let mut got = 0usize;
        while got < to_read {
            match file.read(&mut out[got..to_read]) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(map_io_err(path, e)),
            }
        }
        out.truncate(got);
        Ok(Some(total))
    }

    fn stat(&self, path: &Path) -> FsResult<FileStat> {
        let meta = std::fs::metadata(path).map_err(|e| map_io_err(path, e))?;
        Ok(metadata_to_stat(&meta))
    }

    fn lstat(&self, path: &Path) -> FsResult<FileStat> {
        let meta = std::fs::symlink_metadata(path).map_err(|e| map_io_err(path, e))?;
        Ok(metadata_to_stat(&meta))
    }

    fn canonicalize(&self, path: &Path) -> FsResult<PathBuf> {
        if path.exists() {
            std::fs::canonicalize(path).map_err(|e| map_io_err(path, e))
        } else if let (Some(parent), Some(name)) = (path.parent(), path.file_name()) {
            let resolved_parent = if parent.as_os_str().is_empty() {
                let cwd = std::env::current_dir().map_err(|e| map_io_err(path, e))?;
                std::fs::canonicalize(&cwd).unwrap_or(cwd)
            } else {
                std::fs::canonicalize(parent).map_err(|e| map_io_err(parent, e))?
            };
            Ok(resolved_parent.join(name))
        } else {
            Err(FsError::NotFound(path.to_path_buf()))
        }
    }

    fn read_dir(&self, path: &Path) -> FsResult<Vec<DirEntry>> {
        let rd = std::fs::read_dir(path).map_err(|e| map_io_err(path, e))?;
        let mut entries = Vec::new();
        for item in rd {
            let entry = match item {
                Ok(e) => e,
                Err(_) => continue,
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            let full = entry.path();
            let ft = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            let kind = if ft.is_symlink() {
                EntryKind::Symlink
            } else if ft.is_dir() {
                EntryKind::Directory
            } else if ft.is_file() {
                EntryKind::File
            } else {
                EntryKind::Other
            };
            entries.push(DirEntry {
                name,
                path: full,
                kind,
            });
        }
        // Deterministic ordering: directories first, then ASCII case-insensitive name (zero-alloc), then byte name.
        entries.sort_unstable_by(|a, b| {
            let a_dir = a.kind == EntryKind::Directory;
            let b_dir = b.kind == EntryKind::Directory;
            b_dir
                .cmp(&a_dir)
                .then_with(|| {
                    a.name
                        .bytes()
                        .map(|c| c.to_ascii_lowercase())
                        .cmp(b.name.bytes().map(|c| c.to_ascii_lowercase()))
                })
                .then_with(|| a.name.cmp(&b.name))
        });
        Ok(entries)
    }

    fn write_atomic(
        &self,
        path: &Path,
        write_fn: &mut dyn FnMut(&mut dyn io::Write) -> io::Result<()>,
    ) -> FsResult<FileStat> {
        let existing_perms = std::fs::metadata(path).ok().map(|m| m.permissions());
        let tmp_path = unique_temp_path(path);
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .map_err(|e| map_io_err(path, e))?;
        let write_result = (|| -> io::Result<()> {
            {
                let mut writer = io::BufWriter::with_capacity(64 * 1024, &mut f);
                write_fn(&mut writer)?;
                io::Write::flush(&mut writer)?;
            }
            if let Some(perms) = existing_perms {
                let _ = f.set_permissions(perms);
            }
            f.sync_data()?;
            Ok(())
        })();
        drop(f);

        if let Err(err) = write_result {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(map_io_err(path, err));
        }

        #[cfg(target_os = "windows")]
        let rename_res = {
            let mut last = std::fs::rename(&tmp_path, path);
            for delay_ms in [5u64, 15, 30] {
                match &last {
                    Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                        std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                        last = std::fs::rename(&tmp_path, path);
                    }
                    _ => break,
                }
            }
            last
        };
        #[cfg(not(target_os = "windows"))]
        let rename_res = std::fs::rename(&tmp_path, path);

        if let Err(err) = rename_res {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(map_io_err(path, err));
        }

        // Best-effort parent directory fsync for durability.
        #[cfg(unix)]
        if let Some(parent) = path.parent() {
            if let Ok(dir) = File::open(parent) {
                let _ = dir.sync_all();
            }
        }

        self.stat(path)
    }

    fn write_in_place(
        &self,
        path: &Path,
        ranges: &[(u64, &[u8])],
        new_len: Option<u64>,
    ) -> FsResult<FileStat> {
        let file = OpenOptions::new()
            .write(true)
            .open(path)
            .map_err(|e| map_io_err(path, e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            for (offset, data) in ranges {
                let mut written = 0;
                while written < data.len() {
                    let n = match file.write_at(&data[written..], offset + written as u64) {
                        Ok(n) => n,
                        Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(e) => return Err(map_io_err(path, e)),
                    };
                    if n == 0 {
                        return Err(FsError::Io("short write_at".into()));
                    }
                    written += n;
                }
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::FileExt;
            for (offset, data) in ranges {
                let mut written = 0;
                while written < data.len() {
                    let n = file
                        .seek_write(&data[written..], offset + written as u64)
                        .map_err(|e| map_io_err(path, e))?;
                    if n == 0 {
                        return Err(FsError::Io("short seek_write".into()));
                    }
                    written += n;
                }
            }
        }
        if let Some(len) = new_len {
            file.set_len(len).map_err(|e| map_io_err(path, e))?;
        }
        file.sync_data().map_err(|e| map_io_err(path, e))?;
        let meta = file.metadata().map_err(|e| map_io_err(path, e))?;
        Ok(metadata_to_stat(&meta))
    }

    fn create_dir(&self, path: &Path) -> FsResult<()> {
        std::fs::create_dir(path).map_err(|e| map_io_err(path, e))
    }

    fn rename(&self, from: &Path, to: &Path) -> FsResult<()> {
        std::fs::rename(from, to).map_err(|e| map_io_err(to, e))
    }

    fn remove_file(&self, path: &Path) -> FsResult<()> {
        std::fs::remove_file(path).map_err(|e| map_io_err(path, e))
    }

    fn remove_dir_all(&self, path: &Path) -> FsResult<()> {
        std::fs::remove_dir_all(path).map_err(|e| map_io_err(path, e))
    }

    fn temp_dir(&self) -> PathBuf {
        std::env::temp_dir()
    }

    fn lock_file(&self, path: &Path) -> FsResult<FileLock> {
        let mut lock_os = path.as_os_str().to_os_string();
        lock_os.push(".editor.lock");
        let lock_path = PathBuf::from(lock_os);
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
            .map_err(|e| {
                if e.kind() == io::ErrorKind::AlreadyExists {
                    FsError::Locked(path.to_path_buf())
                } else {
                    map_io_err(&lock_path, e)
                }
            })?;
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let _ = io::Write::write_all(&mut f, format!("pid={}\nts={}\n", std::process::id(), nanos).as_bytes());
        drop(f);
        Ok(FileLock::new(move || {
            let _ = std::fs::remove_file(&lock_path);
        }))
    }
}

// ============================================================================
// Deterministic In-Memory FileSystem (for fast headless tests)
// ============================================================================

#[derive(Debug, Clone)]
struct MemFile {
    data: Arc<Vec<u8>>,
    ino: u64,
    mtime_ns: u64,
}

#[derive(Debug, Default)]
struct MemState {
    files: BTreeMap<PathBuf, MemFile>,
    dirs: BTreeMap<PathBuf, ()>,
    locks: BTreeMap<PathBuf, ()>,
    next_ino: u64,
    clock_ns: u64,
}

#[derive(Debug, Clone, Default)]
pub struct MemoryFileSystem {
    state: Arc<Mutex<MemState>>,
}

impl MemoryFileSystem {
    pub fn new() -> Self {
        let fs = Self::default();
        {
            let mut s = fs.state.lock().unwrap();
            s.next_ino = 1;
            s.clock_ns = 1_000;
            s.dirs.insert(PathBuf::from("/"), ());
        }
        fs
    }

    pub fn add_dir(&self, path: impl Into<PathBuf>) {
        let path = normalize_mem_path(&path.into());
        let mut s = self.state.lock().unwrap();
        let mut cur = PathBuf::new();
        for comp in path.components() {
            cur.push(comp);
            s.dirs.insert(cur.clone(), ());
        }
    }

    pub fn add_file(&self, path: impl Into<PathBuf>, content: impl Into<Vec<u8>>) -> FileRev {
        let path = normalize_mem_path(&path.into());
        if let Some(parent) = path.parent() {
            self.add_dir(parent);
        }
        let mut s = self.state.lock().unwrap();
        s.next_ino += 1;
        s.clock_ns += 1_000;
        let data = Arc::new(content.into());
        let len = data.len() as u64;
        let ino = s.next_ino;
        let mtime_ns = s.clock_ns;
        s.files.insert(
            path,
            MemFile {
                data,
                ino,
                mtime_ns,
            },
        );
        FileRev {
            dev: 1,
            ino,
            size: len,
            mtime_ns,
            ctime_ns: mtime_ns,
        }
    }

    pub fn read_bytes(&self, path: &Path) -> Option<Vec<u8>> {
        let path = normalize_mem_path(path);
        let s = self.state.lock().unwrap();
        s.files.get(&path).map(|f| (*f.data).clone())
    }
}

fn normalize_mem_path(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            std::path::Component::RootDir => out.push("/"),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::Normal(seg) => out.push(seg),
            std::path::Component::Prefix(pre) => out.push(pre.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        PathBuf::from("/")
    } else {
        out
    }
}

struct MemFileHandle {
    data: Arc<Vec<u8>>,
    stat: FileStat,
}

impl FileHandle for MemFileHandle {
    fn len(&self) -> u64 {
        self.data.len() as u64
    }

    fn stat(&self) -> FsResult<FileStat> {
        Ok(self.stat)
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> FsResult<usize> {
        let off = offset as usize;
        if off >= self.data.len() || buf.is_empty() {
            return Ok(0);
        }
        let avail = (self.data.len() - off).min(buf.len());
        buf[..avail].copy_from_slice(&self.data[off..off + avail]);
        Ok(avail)
    }
}

impl FileSystem for MemoryFileSystem {
    fn open_read(&self, path: &Path) -> FsResult<Arc<dyn FileHandle>> {
        let p = normalize_mem_path(path);
        let s = self.state.lock().unwrap();
        let f = s
            .files
            .get(&p)
            .ok_or_else(|| FsError::NotFound(p.clone()))?;
        let stat = FileStat {
            rev: FileRev {
                dev: 1,
                ino: f.ino,
                size: f.data.len() as u64,
                mtime_ns: f.mtime_ns,
                ctime_ns: f.mtime_ns,
            },
            mode: 0o644,
            is_file: true,
            is_dir: false,
            is_symlink: false,
        };
        Ok(Arc::new(MemFileHandle {
            data: Arc::clone(&f.data),
            stat,
        }))
    }

    fn stat(&self, path: &Path) -> FsResult<FileStat> {
        let p = normalize_mem_path(path);
        let s = self.state.lock().unwrap();
        if let Some(f) = s.files.get(&p) {
            return Ok(FileStat {
                rev: FileRev {
                    dev: 1,
                    ino: f.ino,
                    size: f.data.len() as u64,
                    mtime_ns: f.mtime_ns,
                    ctime_ns: f.mtime_ns,
                },
                mode: 0o644,
                is_file: true,
                is_dir: false,
                is_symlink: false,
            });
        }
        if s.dirs.contains_key(&p) {
            return Ok(FileStat {
                rev: FileRev::default(),
                mode: 0o755,
                is_file: false,
                is_dir: true,
                is_symlink: false,
            });
        }
        Err(FsError::NotFound(p))
    }

    fn lstat(&self, path: &Path) -> FsResult<FileStat> {
        self.stat(path)
    }

    fn canonicalize(&self, path: &Path) -> FsResult<PathBuf> {
        Ok(normalize_mem_path(path))
    }

    fn read_dir(&self, path: &Path) -> FsResult<Vec<DirEntry>> {
        let dir = normalize_mem_path(path);
        let s = self.state.lock().unwrap();
        if !s.dirs.contains_key(&dir) {
            return Err(FsError::NotFound(dir));
        }
        let mut entries = Vec::new();
        for d in s.dirs.keys() {
            if d != &dir && d.parent() == Some(dir.as_path()) {
                if let Some(name) = d.file_name().and_then(|n| n.to_str()) {
                    entries.push(DirEntry {
                        name: name.to_string(),
                        path: d.clone(),
                        kind: EntryKind::Directory,
                    });
                }
            }
        }
        for f in s.files.keys() {
            if f.parent() == Some(dir.as_path()) {
                if let Some(name) = f.file_name().and_then(|n| n.to_str()) {
                    entries.push(DirEntry {
                        name: name.to_string(),
                        path: f.clone(),
                        kind: EntryKind::File,
                    });
                }
            }
        }
        entries.sort_by(|a, b| {
            let a_dir = a.kind == EntryKind::Directory;
            let b_dir = b.kind == EntryKind::Directory;
            b_dir
                .cmp(&a_dir)
                .then_with(|| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()))
                .then_with(|| a.name.cmp(&b.name))
        });
        Ok(entries)
    }

    fn write_atomic(
        &self,
        path: &Path,
        write_fn: &mut dyn FnMut(&mut dyn io::Write) -> io::Result<()>,
    ) -> FsResult<FileStat> {
        let mut buf = Vec::new();
        write_fn(&mut buf).map_err(|e| FsError::Io(e.to_string()))?;
        let p = normalize_mem_path(path);
        let rev = self.add_file(p.clone(), buf);
        Ok(FileStat {
            rev,
            mode: 0o644,
            is_file: true,
            is_dir: false,
            is_symlink: false,
        })
    }

    fn write_in_place(
        &self,
        path: &Path,
        ranges: &[(u64, &[u8])],
        new_len: Option<u64>,
    ) -> FsResult<FileStat> {
        let p = normalize_mem_path(path);
        let mut s = self.state.lock().unwrap();
        let f = s
            .files
            .get(&p)
            .ok_or_else(|| FsError::NotFound(p.clone()))?;
        let mut data = (*f.data).clone();
        let ino = f.ino;
        for (offset, bytes) in ranges {
            let end = (*offset as usize) + bytes.len();
            if end > data.len() {
                data.resize(end, 0);
            }
            data[*offset as usize..end].copy_from_slice(bytes);
        }
        if let Some(len) = new_len {
            data.resize(len as usize, 0);
        }
        s.clock_ns += 1_000;
        let mtime_ns = s.clock_ns;
        let len = data.len() as u64;
        s.files.insert(
            p,
            MemFile {
                data: Arc::new(data),
                ino,
                mtime_ns,
            },
        );
        Ok(FileStat {
            rev: FileRev {
                dev: 1,
                ino,
                size: len,
                mtime_ns,
                ctime_ns: mtime_ns,
            },
            mode: 0o644,
            is_file: true,
            is_dir: false,
            is_symlink: false,
        })
    }

    fn create_dir(&self, path: &Path) -> FsResult<()> {
        let p = normalize_mem_path(path);
        let mut s = self.state.lock().unwrap();
        if s.files.contains_key(&p) || s.dirs.contains_key(&p) {
            return Err(FsError::AlreadyExists(p));
        }
        let mut cur = p.as_path();
        loop {
            s.dirs.insert(cur.to_path_buf(), ());
            match cur.parent() {
                Some(parent) if parent != cur => cur = parent,
                _ => break,
            }
        }
        Ok(())
    }

    fn rename(&self, from: &Path, to: &Path) -> FsResult<()> {
        let f = normalize_mem_path(from);
        let t = normalize_mem_path(to);
        let mut s = self.state.lock().unwrap();
        if let Some(entry) = s.files.remove(&f) {
            if let Some(parent) = t.parent() {
                let mut cur = parent;
                loop {
                    s.dirs.insert(cur.to_path_buf(), ());
                    match cur.parent() {
                        Some(p) if p != cur => cur = p,
                        _ => break,
                    }
                }
            }
            s.files.insert(t, entry);
            return Ok(());
        }
        if s.dirs.contains_key(&f) {
            let moved_dirs: Vec<PathBuf> = s
                .dirs
                .keys()
                .filter(|d| d.starts_with(&f))
                .cloned()
                .collect();
            for old_d in moved_dirs {
                s.dirs.remove(&old_d);
                let rel = old_d.strip_prefix(&f).unwrap_or(Path::new(""));
                let new_d = if rel.as_os_str().is_empty() {
                    t.clone()
                } else {
                    t.join(rel)
                };
                s.dirs.insert(new_d, ());
            }
            let moved_files: Vec<PathBuf> = s
                .files
                .keys()
                .filter(|fp| fp.starts_with(&f))
                .cloned()
                .collect();
            for old_f in moved_files {
                if let Some(mf) = s.files.remove(&old_f) {
                    let rel = old_f.strip_prefix(&f).unwrap_or(Path::new(""));
                    s.files.insert(t.join(rel), mf);
                }
            }
            return Ok(());
        }
        Err(FsError::NotFound(f))
    }

    fn remove_file(&self, path: &Path) -> FsResult<()> {
        let p = normalize_mem_path(path);
        let mut s = self.state.lock().unwrap();
        s.files.remove(&p).ok_or_else(|| FsError::NotFound(p))?;
        Ok(())
    }

    fn remove_dir_all(&self, path: &Path) -> FsResult<()> {
        let p = normalize_mem_path(path);
        let mut s = self.state.lock().unwrap();
        if !s.dirs.contains_key(&p) {
            return Err(FsError::NotFound(p));
        }
        s.dirs.retain(|d, _| !d.starts_with(&p));
        s.files.retain(|f, _| !f.starts_with(&p));
        Ok(())
    }

    fn temp_dir(&self) -> PathBuf {
        PathBuf::from("/tmp")
    }

    fn lock_file(&self, path: &Path) -> FsResult<FileLock> {
        let p = normalize_mem_path(path);
        {
            let mut s = self.state.lock().unwrap();
            if s.locks.contains_key(&p) {
                return Err(FsError::Locked(p));
            }
            s.locks.insert(p.clone(), ());
        }
        let state = Arc::clone(&self.state);
        Ok(FileLock::new(move || {
            let mut s = state.lock().unwrap();
            s.locks.remove(&p);
        }))
    }
}
