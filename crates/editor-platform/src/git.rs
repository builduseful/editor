//! Lightweight, OS-agnostic, zero-idle-CPU Git awareness (`std` only).
//!
//! Design:
//! - Repo discovery (`discover_repo`) and branch detection (`read_branch_label`) are 100%
//!   pure Rust file reads (`< 50 µs`, zero subprocesses, works even without `git` installed,
//!   supports normal repos, worktrees, and submodules).
//! - Working-tree file status (`query_status`) and `HEAD` file baseline (`read_head_file`)
//!   use `git --no-optional-locks` with `GIT_OPTIONAL_LOCKS=0` so `editor` NEVER races with
//!   the user's own `git commit`/`rebase` over `.git/index.lock`.
//! - Callers run `query_status` / `read_head_file` on short-lived background threads strictly
//!   in response to OS events (folder open, file open, save, focus, `FsChanged`) — zero timers.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Maximum file size fetched from `git show HEAD:<rel>` for live gutter diff (2 MiB).
pub const MAX_GIT_HEAD_FILE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitRepo {
    /// Canonicalized working-tree root directory containing `.git`.
    pub workdir: PathBuf,
    /// Resolved `.git` metadata directory (handles both normal dirs and worktree `gitdir:` files).
    pub git_dir: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GitFileStatus {
    Modified,
    Added,
    Untracked,
    Conflicted,
}

#[derive(Debug, Clone, Default)]
pub struct GitStatusMap {
    pub files: HashMap<PathBuf, GitFileStatus>,
    /// Ancestor directories containing at least one modified/untracked/conflicted file.
    pub dirty_dirs: HashSet<PathBuf>,
}

impl GitStatusMap {
    pub fn file_status(&self, path: &Path) -> Option<GitFileStatus> {
        self.files.get(path).copied()
    }

    pub fn dir_has_changes(&self, dir: &Path) -> bool {
        self.dirty_dirs.contains(dir)
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// Bounded multi-repo Git status cache (at most 4 active repositories).
///
/// Handles single repos, monorepo subdirectories, nested repos/submodules, and multi-repo parent
/// workspaces cleanly by resolving every lookup to the deepest enclosing cached repository.
#[derive(Debug, Clone, Default)]
pub struct GitStatusCache {
    repos: Vec<(PathBuf, GitStatusMap)>,
}

const MAX_CACHED_GIT_REPOS: usize = 4;

impl GitStatusCache {
    pub fn is_empty(&self) -> bool {
        self.repos.is_empty()
    }

    pub fn clear(&mut self) {
        self.repos.clear();
    }

    pub fn contains_repo(&self, workdir: &Path) -> bool {
        self.repos.iter().any(|(w, _)| w == workdir)
    }

    pub fn upsert(&mut self, workdir: PathBuf, map: GitStatusMap) {
        self.repos.retain(|(w, _)| w != &workdir);
        self.repos.insert(0, (workdir, map));
        if self.repos.len() > MAX_CACHED_GIT_REPOS {
            self.repos.truncate(MAX_CACHED_GIT_REPOS);
        }
    }

    fn deepest_for(&self, path: &Path) -> Option<&(PathBuf, GitStatusMap)> {
        self.repos
            .iter()
            .filter(|(w, _)| path.starts_with(w))
            .max_by_key(|(w, _)| w.as_os_str().len())
    }

    /// Look up the Git status of `path` using the deepest enclosing cached repository.
    pub fn file_status(&self, path: &Path) -> Option<GitFileStatus> {
        let (_, map) = self.deepest_for(path)?;
        map.file_status(path)
    }

    /// Return true if directory `dir` contains changes in its own deepest repository, is flagged
    /// as a modified submodule by an enclosing outer repository, or is a non-repo ancestor folder
    /// containing a cached dirty child repository.
    pub fn dir_has_changes(&self, dir: &Path) -> bool {
        if let Some((workdir, map)) = self.deepest_for(dir) {
            if map.dir_has_changes(dir) {
                return true;
            }
            // If `dir` is a nested repo/submodule root itself, also check whether an outer repo
            // recorded a changed submodule HEAD pointer for `dir`.
            if dir == workdir {
                if let Some((_, outer)) = self
                    .repos
                    .iter()
                    .filter(|(w, _)| w != dir && dir.starts_with(w))
                    .max_by_key(|(w, _)| w.as_os_str().len())
                {
                    if outer.file_status(dir).is_some() {
                        return true;
                    }
                }
            }
            // Also roll up any cached dirty nested repo strictly beneath `dir`.
            return self
                .repos
                .iter()
                .any(|(w, m)| w != dir && w.starts_with(dir) && !m.is_empty());
        }
        // Non-repo parent directory (e.g. multi-repo workspace folder): roll up dirty child repos.
        self.repos
            .iter()
            .any(|(w, m)| w.starts_with(dir) && !m.is_empty())
    }
}

/// Walk upward from `start` looking for a `.git` directory or worktree/submodule `.git` file.
pub fn discover_repo(start: &Path) -> Option<GitRepo> {
    let canonical_start = fs::canonicalize(start).unwrap_or_else(|_| start.to_path_buf());
    let mut cur = if canonical_start.is_file() {
        canonical_start.parent()?.to_path_buf()
    } else {
        canonical_start
    };

    loop {
        let dot_git = cur.join(".git");
        if let Ok(meta) = fs::metadata(&dot_git) {
            if meta.is_dir() {
                return Some(GitRepo {
                    workdir: cur,
                    git_dir: dot_git,
                });
            }
            if meta.is_file() {
                if let Ok(raw) = fs::read_to_string(&dot_git) {
                    if let Some(rest) = raw
                        .lines()
                        .next()
                        .and_then(|l| l.trim().strip_prefix("gitdir:"))
                    {
                        let p = PathBuf::from(rest.trim());
                        let resolved = if p.is_absolute() { p } else { cur.join(p) };
                        let git_dir = fs::canonicalize(&resolved).unwrap_or(resolved);
                        return Some(GitRepo {
                            workdir: cur,
                            git_dir,
                        });
                    }
                }
            }
        }
        if !cur.pop() {
            return None;
        }
    }
}

/// Parse `<git_dir>/HEAD` directly in pure Rust (`< 50 µs`, zero subprocesses).
/// Returns a concise status-bar label such as `"main"`, `"feat/x"`, `"6b66e84"`, or `"main (rebase)"`.
pub fn read_branch_label(git_dir: &Path) -> Option<String> {
    let head = fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let mut label = parse_head_contents(&head)?;
    if git_dir.join("rebase-merge").exists() || git_dir.join("rebase-apply").exists() {
        label.push_str(" (rebase)");
    } else if git_dir.join("MERGE_HEAD").exists() {
        label.push_str(" (merge)");
    } else if git_dir.join("CHERRY_PICK_HEAD").exists() {
        label.push_str(" (cherry-pick)");
    }
    Some(label)
}

pub fn parse_head_contents(raw: &str) -> Option<String> {
    let line = raw.lines().next()?.trim();
    if line.is_empty() {
        return None;
    }
    if let Some(ref_path) = line.strip_prefix("ref:") {
        let r = ref_path.trim();
        let name = r
            .strip_prefix("refs/heads/")
            .or_else(|| r.strip_prefix("refs/tags/"))
            .unwrap_or(r);
        if name.is_empty() {
            return None;
        }
        return Some(name.to_string());
    }
    // Detached HEAD commit hash -> short 7-char prefix.
    if line.len() >= 7 && line.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Some(line[..7].to_string());
    }
    Some(line.to_string())
}

/// Convert `path` inside `workdir` into Git's canonical forward-slash relative path (`a/b/c.rs`).
pub fn repo_relative_slash_path(workdir: &Path, path: &Path) -> Option<String> {
    let owned_canonical: PathBuf;
    let rel = match path.strip_prefix(workdir) {
        Ok(r) => r,
        Err(_) => {
            owned_canonical = fs::canonicalize(path).ok()?;
            owned_canonical.strip_prefix(workdir).ok()?
        }
    };
    let mut out = String::new();
    for (i, comp) in rel.components().enumerate() {
        if i > 0 {
            out.push('/');
        }
        out.push_str(&comp.as_os_str().to_string_lossy());
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Parse `git status --porcelain=v1 -z` raw NUL-delimited output into a [`GitStatusMap`].
pub fn parse_porcelain_z(workdir: &Path, bytes: &[u8]) -> GitStatusMap {
    let mut map = GitStatusMap::default();
    let mut i = 0usize;
    while i < bytes.len() {
        let end = bytes[i..]
            .iter()
            .position(|&b| b == 0)
            .map(|rel| i + rel)
            .unwrap_or(bytes.len());
        let entry = &bytes[i..end];
        i = (end + 1).min(bytes.len());

        if entry.len() < 4 || entry[2] != b' ' {
            continue;
        }
        let x = entry[0];
        let y = entry[1];
        let path_bytes = &entry[3..];

        // Renames/copies (`R` or `C` in X or Y) emit a second NUL-terminated old-path token;
        // always consume it before any UTF-8 early `continue` so subsequent records stay aligned.
        if matches!(x, b'R' | b'C') || matches!(y, b'R' | b'C') {
            if i < bytes.len() {
                let next_end = bytes[i..]
                    .iter()
                    .position(|&b| b == 0)
                    .map(|rel| i + rel)
                    .unwrap_or(bytes.len());
                i = (next_end + 1).min(bytes.len());
            }
        }

        let Ok(rel_str) = std::str::from_utf8(path_bytes) else {
            continue;
        };

        let status = classify_xy(x, y);
        let Some(status) = status else {
            continue;
        };

        let is_untracked_dir = rel_str.ends_with('/');
        let rel_trimmed = rel_str.trim_end_matches('/');
        if rel_trimmed.is_empty() {
            continue;
        }
        let mut full = workdir.to_path_buf();
        for seg in rel_trimmed.split('/') {
            if !seg.is_empty() {
                full.push(seg);
            }
        }
        map.files.insert(full.clone(), status);
        if is_untracked_dir {
            map.dirty_dirs.insert(full.clone());
        }

        // Roll up dirty indicator to ancestor directories up to `workdir`.
        let mut anc = full.parent();
        while let Some(dir) = anc {
            if !dir.starts_with(workdir) {
                break;
            }
            if !map.dirty_dirs.insert(dir.to_path_buf()) {
                break;
            }
            if dir == workdir {
                break;
            }
            anc = dir.parent();
        }
    }
    map
}

fn classify_xy(x: u8, y: u8) -> Option<GitFileStatus> {
    if matches!(
        (x, y),
        (b'U', _) | (_, b'U') | (b'A', b'A') | (b'D', b'D')
    ) {
        return Some(GitFileStatus::Conflicted);
    }
    if x == b'?' && y == b'?' {
        return Some(GitFileStatus::Untracked);
    }
    if x == b'!' && y == b'!' {
        return None;
    }
    if x == b'A' && y == b' ' {
        return Some(GitFileStatus::Added);
    }
    if x != b' ' || y != b' ' {
        return Some(GitFileStatus::Modified);
    }
    None
}

/// Config overrides that neutralise repo-controlled command execution (`core.fsmonitor` runs an
/// arbitrary program on every `git status`). `-c` takes precedence over repo-local config.
const HARDENING_ARGS: [&str; 4] = ["-c", "core.fsmonitor=false", "-c", "core.untrackedCache=false"];

/// Opening an untrusted folder must never run code. Besides the `-c` overrides above, refuse to
/// run `git` at all when the repo-local config (or `info/attributes`) could make Git spawn a
/// repo-chosen program: `[include]` / `[includeIf]` (can pull in unscanned files) and `[filter]`
/// drivers whose `clean` / `smudge` / `process` command is not Git LFS. Normal repositories are
/// unaffected; hostile ones simply get no Git decorations.
pub fn repo_config_is_trusted(workdir: &Path) -> bool {
    let Some(repo) = discover_repo(workdir) else {
        return true;
    };
    let mut dirs = vec![repo.git_dir.clone()];
    if let Ok(raw) = fs::read_to_string(repo.git_dir.join("commondir")) {
        let p = PathBuf::from(raw.trim());
        dirs.push(if p.is_absolute() { p } else { repo.git_dir.join(p) });
    }
    for dir in dirs {
        for name in ["config", "config.worktree"] {
            if let Ok(text) = fs::read_to_string(dir.join(name)) {
                if !config_text_is_trusted(&text) {
                    return false;
                }
            }
        }
        if let Ok(text) = fs::read_to_string(dir.join("info/attributes")) {
            if text.to_ascii_lowercase().contains("filter=") && !text.contains("filter=lfs") {
                return false;
            }
        }
    }
    true
}

fn config_text_is_trusted(text: &str) -> bool {
    let mut section = String::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('[') {
            section = line.to_ascii_lowercase();
            if section.starts_with("[include") {
                return false;
            }
            continue;
        }
        if section.starts_with("[filter") {
            let lower = line.to_ascii_lowercase();
            let is_cmd = ["clean", "smudge", "process"].iter().any(|k| {
                lower
                    .strip_prefix(k)
                    .is_some_and(|r| r.trim_start().starts_with('='))
            });
            if is_cmd {
                let value = line.split_once('=').map(|(_, v)| v.trim()).unwrap_or("");
                let value = value.trim_matches('"').trim();
                if !(value.starts_with("git-lfs ") || value.starts_with("git lfs ")) {
                    return false;
                }
            }
        }
    }
    true
}

/// Run `git --no-optional-locks status --porcelain=v1 -z --untracked-files=normal --ignore-submodules=dirty`
/// inside `workdir`. When `scope` is strictly a subdirectory inside `workdir` (e.g. an open
/// monorepo package folder), passes `-- <rel_scope>` so Git only walks that subtree.
/// Returns `None` if `git` is unavailable or fails.
pub fn query_status(workdir: &Path, scope: Option<&Path>) -> Option<GitStatusMap> {
    if !repo_config_is_trusted(workdir) {
        return None;
    }
    let mut cmd = Command::new("git");
    cmd.env("GIT_OPTIONAL_LOCKS", "0")
        .arg("--no-optional-locks")
        .args(HARDENING_ARGS)
        .arg("-C")
        .arg(workdir)
        .args([
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=normal",
            "--ignore-submodules=dirty",
        ]);
    if let Some(rel_scope) = scope.and_then(|s| repo_relative_slash_path(workdir, s)) {
        cmd.arg("--").arg(rel_scope);
    }
    cmd.stdin(Stdio::null()).stderr(Stdio::null());
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(parse_porcelain_z(workdir, &out.stdout))
}

/// Fetch `HEAD:<rel_slash>` content bytes via `git --no-optional-locks show`, capped at
/// [`MAX_GIT_HEAD_FILE_BYTES`]. Returns `None` for untracked, binary/oversized, or unavailable files.
pub fn read_head_file(workdir: &Path, path: &Path) -> Option<Vec<u8>> {
    if !repo_config_is_trusted(workdir) {
        return None;
    }
    let rel = repo_relative_slash_path(workdir, path)?;
    let spec = format!("HEAD:{rel}");
    let mut cmd = Command::new("git");
    // `cat-file blob` never applies textconv / smudge / clean filters (unlike `show`).
    cmd.env("GIT_OPTIONAL_LOCKS", "0")
        .arg("--no-optional-locks")
        .args(HARDENING_ARGS)
        .arg("-C")
        .arg(workdir)
        .args(["cat-file", "blob", &spec])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let mut buf = Vec::new();
    let limit = (MAX_GIT_HEAD_FILE_BYTES + 1) as u64;
    let read_ok = std::io::Read::read_to_end(&mut std::io::Read::take(&mut stdout, limit), &mut buf).is_ok();
    drop(stdout);
    if !read_ok || buf.len() > MAX_GIT_HEAD_FILE_BYTES {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }
    let status = child.wait().ok()?;
    if !status.success() {
        return None;
    }
    Some(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_head_refs_and_detached_shas() {
        assert_eq!(
            parse_head_contents("ref: refs/heads/main\n"),
            Some("main".to_string())
        );
        assert_eq!(
            parse_head_contents("ref: refs/heads/feat/git-gutter\n"),
            Some("feat/git-gutter".to_string())
        );
        assert_eq!(
            parse_head_contents("6b66e84f1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d\n"),
            Some("6b66e84".to_string())
        );
        assert_eq!(parse_head_contents(""), None);
    }

    #[test]
    fn parses_porcelain_z_statuses_renames_and_ancestor_dirs() {
        let root = PathBuf::from("/mock/repo");
        let raw = b" M crates/editor-app/src/lib.rs\0?? notes.txt\0A  new.rs\0R  dst.rs\0src.rs\0UU conflict.rs\0";
        let st = parse_porcelain_z(&root, raw);

        assert_eq!(
            st.file_status(&root.join("crates/editor-app/src/lib.rs")),
            Some(GitFileStatus::Modified)
        );
        assert_eq!(
            st.file_status(&root.join("notes.txt")),
            Some(GitFileStatus::Untracked)
        );
        assert_eq!(
            st.file_status(&root.join("new.rs")),
            Some(GitFileStatus::Added)
        );
        assert_eq!(
            st.file_status(&root.join("dst.rs")),
            Some(GitFileStatus::Modified)
        );
        assert_eq!(st.file_status(&root.join("src.rs")), None);
        assert_eq!(
            st.file_status(&root.join("conflict.rs")),
            Some(GitFileStatus::Conflicted)
        );
        assert!(st.dir_has_changes(&root.join("crates/editor-app/src")));
        assert!(st.dir_has_changes(&root.join("crates/editor-app")));
        assert!(st.dir_has_changes(&root.join("crates")));
        assert!(!st.dir_has_changes(&root.join("docs")));
    }

    #[test]
    fn discovers_worktree_gitdir_file_and_reads_branch() {
        let base = std::env::temp_dir().join(format!("editor_git_wt_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let real_git = base.join("real_git");
        let wt = base.join("worktree");
        let sub = wt.join("src");
        fs::create_dir_all(&real_git).unwrap();
        fs::create_dir_all(&sub).unwrap();
        fs::write(real_git.join("HEAD"), "ref: refs/heads/dev\n").unwrap();
        fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", real_git.to_string_lossy()),
        )
        .unwrap();

        let repo = discover_repo(&sub).expect("should discover worktree repo");
        assert_eq!(read_branch_label(&repo.git_dir), Some("dev".to_string()));
        fs::write(real_git.join("MERGE_HEAD"), "0123456789abcdef\n").unwrap();
        assert_eq!(
            read_branch_label(&repo.git_dir),
            Some("dev (merge)".to_string())
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn git_status_cache_handles_nested_repos_submodules_and_multi_repo_workspaces() {
        let outer = PathBuf::from("/ws/outer");
        let inner = PathBuf::from("/ws/outer/sub");
        let sibling = PathBuf::from("/ws/other");

        let outer_map = parse_porcelain_z(&outer, b" M README.md\0 M sub\0");
        let inner_map = parse_porcelain_z(&inner, b" M src/lib.rs\0?? scratch.txt\0");
        let sibling_map = parse_porcelain_z(&sibling, b"A  new.rs\0");

        let mut cache = GitStatusCache::default();
        cache.upsert(outer.clone(), outer_map);
        cache.upsert(inner.clone(), inner_map);
        cache.upsert(sibling.clone(), sibling_map);

        // Deepest repo wins for files inside nested repo `outer/sub`.
        assert_eq!(
            cache.file_status(&outer.join("README.md")),
            Some(GitFileStatus::Modified)
        );
        assert_eq!(
            cache.file_status(&inner.join("src/lib.rs")),
            Some(GitFileStatus::Modified)
        );
        assert_eq!(
            cache.file_status(&inner.join("scratch.txt")),
            Some(GitFileStatus::Untracked)
        );
        assert!(cache.dir_has_changes(&inner.join("src")));
        assert!(cache.dir_has_changes(&inner));
        // Non-repo ancestor `/ws` rolls up dirty child repos cleanly.
        assert!(cache.dir_has_changes(Path::new("/ws")));
        assert_eq!(
            cache.file_status(&sibling.join("new.rs")),
            Some(GitFileStatus::Added)
        );

        // Even if `inner` working tree is clean, outer repo's modified submodule pointer marks `inner` dirty.
        cache.upsert(inner.clone(), GitStatusMap::default());
        assert!(!cache.dir_has_changes(&inner.join("src")));
        assert!(cache.dir_has_changes(&inner));
    }

    #[test]
    fn security_untrusted_repo_config_never_reaches_git() {
        // Repo-chosen programs (fsmonitor is overridden via -c; filters/includes make us skip git).
        assert!(config_text_is_trusted("[core]\n\trepositoryformatversion = 0\n[remote \"origin\"]\n\turl = x\n"));
        assert!(!config_text_is_trusted("[include]\n\tpath = ../evil\n"));
        assert!(!config_text_is_trusted("[includeIf \"gitdir:/\"]\n\tpath = x\n"));
        assert!(!config_text_is_trusted("[filter \"x\"]\n\tclean = sh -c 'touch /tmp/pwn'\n"));
        assert!(!config_text_is_trusted("[filter \"x\"]\n\tSmudge=/tmp/evil\n"));
        assert!(config_text_is_trusted("[filter \"lfs\"]\n\tclean = git-lfs clean -- %f\n\trequired = true\n"));
        assert!(HARDENING_ARGS.contains(&"core.fsmonitor=false"));

        let base = std::env::temp_dir().join(format!("editor_git_sec_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join(".git")).unwrap();
        fs::write(base.join(".git/config"), "[core]\n\tfsmonitor = touch /tmp/pwned\n[filter \"a\"]\n\tclean = evil\n").unwrap();
        assert!(!repo_config_is_trusted(&base));
        assert!(query_status(&base, None).is_none());
        assert!(read_head_file(&base, &base.join("f.txt")).is_none());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn audit_regressions_untracked_dir_and_non_utf8_rename() {
        let root = PathBuf::from("/ws/repo");
        // Untracked directory entry (`?? new_dir/`) marks `new_dir` itself in `dirty_dirs`.
        let m1 = parse_porcelain_z(&root, b"?? new_dir/\0");
        assert!(m1.dirty_dirs.contains(&root.join("new_dir")));
        assert!(m1.dirty_dirs.contains(&root));

        // Non-UTF-8 rename still consumes its second NUL-delimited old-path token so subsequent
        // entries parse cleanly without desynchronizing.
        let m2 = parse_porcelain_z(&root, b"R  \xff\xfe\0old_name.rs\0 M after.rs\0");
        assert_eq!(
            m2.files.get(&root.join("after.rs")),
            Some(&GitFileStatus::Modified)
        );
    }
}
