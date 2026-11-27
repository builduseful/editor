//! Platform-specific and `.gitignore`-aware workspace traversal rules.
//!
//! Each target OS declares its system vaults, package bundles, and home-directory
//! exclusions in one declarative table here so adding Linux, Windows, or BSD support
//! requires zero OS-specific branches in `editor-render` or `editor-app`.

use crate::fs::FileSystem;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Exact directory names ignored across every operating system (VCS internals, build outputs,
/// and package/tool caches that drown out real source files).
const COMMON_DIR_NAMES: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "node_modules",
    "target",
    "build",
    "build-asan",
    "dist",
    "__pycache__",
    "venv",
    ".venv",
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".parcel-cache",
    ".cache",
    ".turbo",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".tox",
    ".nox",
    ".gradle",
    ".yarn",
    ".pnpm-store",
    "DerivedData",
    "Pods",
];

/// macOS-specific directory names ignored everywhere.
const MACOS_EXTRA_DIR_NAMES: &[&str] = &[
    ".Trash",
    ".Trashes",
    ".Spotlight-V100",
    ".fseventsd",
    ".DocumentRevisions-V100",
    ".TemporaryItems",
    "Carthage",
];

/// macOS opaque package/media bundle directory suffixes (always binary/media vaults).
const MACOS_DIR_SUFFIXES: &[&str] = &[
    ".app",
    ".framework",
    ".bundle",
    ".kext",
    ".xpc",
    ".photoslibrary",
    ".musiclibrary",
    ".tvlibrary",
    ".migratedphotolibrary",
    ".xcarchive",
    ".dSYM",
];

/// Directories skipped ONLY when directly inside `$HOME` on macOS.
/// (A project folder like `~/projects/my-app/Library` is never skipped, and explicitly
/// opening `~/Library/Preferences` as the workspace root still searches inside it.)
const MACOS_HOME_DIR_NAMES: &[&str] = &[
    "Library",
    "Applications",
    "Pictures",
    "Music",
    "Movies",
    "Public",
];

/// Directories skipped ONLY when directly inside filesystem root `/` on macOS.
const MACOS_ROOT_DIR_NAMES: &[&str] = &["System", "Volumes", "dev", "cores", "net", "home"];

/// macOS metadata files ignored everywhere.
const MACOS_FILE_NAMES: &[&str] = &[".DS_Store", ".localized"];

/// Linux-specific directory names ignored everywhere.
const LINUX_EXTRA_DIR_NAMES: &[&str] = &["lost+found"];

/// Directories skipped ONLY when directly inside `$HOME` on Linux.
const LINUX_HOME_DIR_NAMES: &[&str] = &["snap", "Pictures", "Music", "Videos", "Templates"];

/// Virtual/system filesystems skipped ONLY when directly inside `/` on Linux.
const LINUX_ROOT_DIR_NAMES: &[&str] = &["proc", "sys", "dev", "run", "mnt", "media", "lost+found"];

/// Linux metadata files ignored everywhere.
const LINUX_FILE_NAMES: &[&str] = &[".directory"];

/// Windows-specific directory names ignored everywhere (matched ASCII case-insensitively).
const WINDOWS_EXTRA_DIR_NAMES: &[&str] = &["$RECYCLE.BIN", "System Volume Information"];

/// Directories skipped ONLY when directly inside `%USERPROFILE%` on Windows.
const WINDOWS_HOME_DIR_NAMES: &[&str] = &[
    "AppData",
    "Application Data",
    "Local Settings",
    "Pictures",
    "Music",
    "Videos",
    "Saved Games",
    "Links",
    "Searches",
];

/// System directories skipped ONLY when directly inside a drive root (e.g. `C:\`) on Windows.
const WINDOWS_ROOT_DIR_NAMES: &[&str] = &[
    "Windows",
    "Program Files",
    "Program Files (x86)",
    "ProgramData",
    "$RECYCLE.BIN",
    "System Volume Information",
    "Recovery",
];

/// Windows metadata files ignored everywhere.
const WINDOWS_FILE_NAMES: &[&str] = &["Thumbs.db", "desktop.ini", "NTUSER.DAT"];

/// Declarative per-OS directory/file ignore configuration for workspace walking & search.
#[derive(Debug, Clone)]
pub struct IgnoreRules {
    pub common_dir_names: &'static [&'static str],
    pub os_dir_names: &'static [&'static str],
    pub dir_suffixes: &'static [&'static str],
    pub home_dir_names: &'static [&'static str],
    pub root_dir_names: &'static [&'static str],
    pub file_names: &'static [&'static str],
    /// Skip all hidden dot-directories directly inside `$HOME` or `/` (`.cargo`, `.rustup`,
    /// `.npm`, `.local`, `.Trash`, ...) while keeping tracked project config dirs (`.github`,
    /// `.cargo/config.toml`, `.devcontainer`, `.vscode`, ...) searchable inside workspaces.
    pub skip_hidden_dirs_at_home_or_root: bool,
    pub case_insensitive_names: bool,
    pub home_dir: Option<PathBuf>,
}

impl IgnoreRules {
    /// Detect the current operating system's ignore configuration and home directory.
    pub fn for_current_os() -> Self {
        let home = detect_home_dir();
        #[cfg(target_os = "macos")]
        {
            Self::macos(home)
        }
        #[cfg(target_os = "linux")]
        {
            Self::linux(home)
        }
        #[cfg(target_os = "windows")]
        {
            Self::windows(home)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            Self::unix_generic(home)
        }
    }

    pub fn macos(home_dir: Option<PathBuf>) -> Self {
        Self {
            common_dir_names: COMMON_DIR_NAMES,
            os_dir_names: MACOS_EXTRA_DIR_NAMES,
            dir_suffixes: MACOS_DIR_SUFFIXES,
            home_dir_names: MACOS_HOME_DIR_NAMES,
            root_dir_names: MACOS_ROOT_DIR_NAMES,
            file_names: MACOS_FILE_NAMES,
            skip_hidden_dirs_at_home_or_root: true,
            case_insensitive_names: false,
            home_dir,
        }
    }

    pub fn linux(home_dir: Option<PathBuf>) -> Self {
        Self {
            common_dir_names: COMMON_DIR_NAMES,
            os_dir_names: LINUX_EXTRA_DIR_NAMES,
            dir_suffixes: &[],
            home_dir_names: LINUX_HOME_DIR_NAMES,
            root_dir_names: LINUX_ROOT_DIR_NAMES,
            file_names: LINUX_FILE_NAMES,
            skip_hidden_dirs_at_home_or_root: true,
            case_insensitive_names: false,
            home_dir,
        }
    }

    pub fn windows(home_dir: Option<PathBuf>) -> Self {
        Self {
            common_dir_names: COMMON_DIR_NAMES,
            os_dir_names: WINDOWS_EXTRA_DIR_NAMES,
            dir_suffixes: &[],
            home_dir_names: WINDOWS_HOME_DIR_NAMES,
            root_dir_names: WINDOWS_ROOT_DIR_NAMES,
            file_names: WINDOWS_FILE_NAMES,
            skip_hidden_dirs_at_home_or_root: true,
            case_insensitive_names: true,
            home_dir,
        }
    }

    pub fn unix_generic(home_dir: Option<PathBuf>) -> Self {
        Self {
            common_dir_names: COMMON_DIR_NAMES,
            os_dir_names: LINUX_EXTRA_DIR_NAMES,
            dir_suffixes: &[],
            home_dir_names: &[],
            root_dir_names: LINUX_ROOT_DIR_NAMES,
            file_names: &[],
            skip_hidden_dirs_at_home_or_root: true,
            case_insensitive_names: false,
            home_dir,
        }
    }

    #[inline]
    fn name_matches(&self, candidate: &str, rule: &str) -> bool {
        if self.case_insensitive_names {
            candidate.eq_ignore_ascii_case(rule)
        } else {
            candidate == rule
        }
    }

    #[inline]
    fn is_home_dir(&self, dir: &Path) -> bool {
        let Some(ref home) = self.home_dir else {
            return false;
        };
        if dir == home {
            return true;
        }
        // Handle case-insensitive filesystems (e.g. `/home/alice` vs `/Home/Alice`).
        dir.to_string_lossy()
            .eq_ignore_ascii_case(&home.to_string_lossy())
    }

    #[inline]
    fn is_fs_root(dir: &Path) -> bool {
        if dir.parent().is_none() {
            return true;
        }
        // Also recognize Windows drive roots (e.g. `C:\` or `C:/`) in cross-platform checks.
        let s = dir.to_string_lossy();
        let b = s.as_bytes();
        b.len() == 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')
    }

    /// Return true if subdirectory `name` inside `parent_dir` should be pruned.
    pub fn should_skip_dir(&self, parent_dir: &Path, name: &str) -> bool {
        if self
            .common_dir_names
            .iter()
            .any(|&r| self.name_matches(name, r))
            || self.os_dir_names.iter().any(|&r| self.name_matches(name, r))
        {
            return true;
        }

        if !self.dir_suffixes.is_empty() {
            let nb = name.as_bytes();
            for &suf in self.dir_suffixes {
                let sb = suf.as_bytes();
                if nb.len() > sb.len() && nb[nb.len() - sb.len()..].eq_ignore_ascii_case(sb) {
                    return true;
                }
            }
        }

        let in_home = self.is_home_dir(parent_dir);
        if in_home
            && self
                .home_dir_names
                .iter()
                .any(|&r| self.name_matches(name, r))
        {
            return true;
        }

        let in_root = Self::is_fs_root(parent_dir);
        if in_root
            && self
                .root_dir_names
                .iter()
                .any(|&r| self.name_matches(name, r))
        {
            return true;
        }

        if self.skip_hidden_dirs_at_home_or_root && (in_home || in_root) && name.starts_with('.') {
            return true;
        }

        false
    }

    /// Return true if file `name` should be skipped during workspace traversal.
    #[inline]
    pub fn should_skip_file(&self, name: &str) -> bool {
        // Submodules and worktrees store a `.git` file pointing to their gitdir; never index it.
        if self.name_matches(name, ".git") {
            return true;
        }
        self.file_names.iter().any(|&r| self.name_matches(name, r))
    }
}

fn detect_home_dir() -> Option<PathBuf> {
    let raw = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)?;
    Some(std::fs::canonicalize(&raw).unwrap_or(raw))
}

// ============================================================================
// Lightweight Hierarchical `.gitignore` Matcher (0 dependencies, 0 extra stats)
// ============================================================================

#[derive(Debug, Clone)]
struct GitIgnoreRule {
    pattern: String,
    negated: bool,
    dir_only: bool,
    anchored: bool,
    has_slash: bool,
}

#[derive(Debug, Clone)]
enum LayerScope {
    /// `.gitignore` lives at or below the traversal root (`""` for root, `"crates/foo"` for subdirs).
    Descendant(String),
    /// `.gitignore` lives strictly above the traversal root (e.g. a monorepo root `.gitignore`
    /// when the user opened a subdirectory). `prefix` is the traversal root's forward-slash path
    /// relative to this `.gitignore`'s directory (e.g. `"services/api"`).
    Ancestor(String),
}

/// Parsed rules from a single `.gitignore` file, scoped to its directory relative to the traversal root.
#[derive(Debug, Clone)]
pub struct GitIgnoreLayer {
    scope: LayerScope,
    has_path_rule: bool,
    rules: Vec<GitIgnoreRule>,
}

impl GitIgnoreLayer {
    /// Parse a `.gitignore` file located at `base_rel` at or below the workspace root.
    pub fn parse(base_rel: String, content: &str) -> Option<Self> {
        Self::parse_scoped(LayerScope::Descendant(base_rel), content)
    }

    /// Parse an ancestor `.gitignore` file located strictly above the workspace root, where
    /// `root_rel_to_ancestor` is the workspace root's forward-slash path relative to that ancestor
    /// directory (e.g. `"services/api"` when root is `/monorepo/services/api` and `.gitignore` is
    /// in `/monorepo`).
    pub fn parse_ancestor(root_rel_to_ancestor: String, content: &str) -> Option<Self> {
        if root_rel_to_ancestor.is_empty() {
            Self::parse(String::new(), content)
        } else {
            Self::parse_scoped(LayerScope::Ancestor(root_rel_to_ancestor), content)
        }
    }

    fn parse_scoped(scope: LayerScope, content: &str) -> Option<Self> {
        let mut rules = Vec::new();
        let mut has_path_rule = false;
        for raw_line in content.lines() {
            let line = raw_line.trim_end_matches('\r').trim_end_matches([' ', '\t']);
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (negated, rest) = if let Some(r) = line.strip_prefix('!') {
                (true, r)
            } else if let Some(r) = line.strip_prefix("\\#").or_else(|| line.strip_prefix("\\!")) {
                (false, r)
            } else {
                (false, line)
            };
            if rest.is_empty() {
                continue;
            }
            let (dir_only, rest) = if let Some(r) = rest.strip_suffix('/') {
                (true, r)
            } else {
                (false, rest)
            };
            let (leading_slash, rest) = if let Some(r) = rest.strip_prefix('/') {
                (true, r)
            } else {
                (false, rest)
            };
            let (has_leading_dstar, rest) = if let Some(r) = rest.strip_prefix("**/") {
                (true, r)
            } else {
                (false, rest)
            };
            if rest.is_empty() {
                continue;
            }
            let has_slash = rest.contains('/');
            let anchored = (leading_slash || has_slash) && !has_leading_dstar;
            if anchored || has_slash {
                has_path_rule = true;
            }
            rules.push(GitIgnoreRule {
                pattern: rest.to_string(),
                negated,
                dir_only,
                anchored,
                has_slash,
            });
        }
        if rules.is_empty() {
            None
        } else {
            Some(Self {
                scope,
                has_path_rule,
                rules,
            })
        }
    }

    /// Evaluate all rules in this layer against `rel_path` (relative to workspace root).
    /// Returns `Some(true)` if ignored, `Some(false)` if explicitly un-ignored (`!`), `None` if no rule matched.
    pub fn matches(&self, rel_path: &str, name: &str, is_dir: bool) -> Option<bool> {
        let owned_local: String;
        let local_rel: &str = match &self.scope {
            LayerScope::Descendant(base_rel) => {
                if base_rel.is_empty() {
                    rel_path
                } else {
                    let rest = rel_path.strip_prefix(base_rel.as_str())?;
                    rest.strip_prefix('/')?
                }
            }
            LayerScope::Ancestor(prefix) => {
                if self.has_path_rule {
                    let mut s = String::with_capacity(prefix.len() + 1 + rel_path.len());
                    s.push_str(prefix);
                    s.push('/');
                    s.push_str(rel_path);
                    owned_local = s;
                    owned_local.as_str()
                } else {
                    ""
                }
            }
        };
        let mut decision: Option<bool> = None;
        for rule in &self.rules {
            if rule.dir_only && !is_dir {
                continue;
            }
            let hit = if rule.anchored || rule.has_slash {
                glob_match_path(&rule.pattern, local_rel)
                    || (!rule.anchored && glob_match_suffix(&rule.pattern, local_rel))
            } else {
                glob_match_segment(&rule.pattern, name)
            };
            if hit {
                decision = Some(!rule.negated);
            }
        }
        decision
    }
}

/// Collect `.gitignore` layers from ancestor directories strictly above `root` up to the enclosing
/// Git repository root (if any), plus `.git/info/exclude` at the repo root.
///
/// Cost: a single `stat` when `root` itself is a repo root, or `O(depth)` (typically 1–4 `stat`s)
/// when `root` is a subdirectory inside a monorepo. Never walks above the enclosing `.git` root.
pub fn collect_ancestor_gitignores(fs: &dyn FileSystem, root: &Path) -> Vec<Arc<GitIgnoreLayer>> {
    const MAX_ANCESTOR_DEPTH: usize = 32;
    const MAX_GITIGNORE_BYTES: usize = 32 * 1024;

    let canonical = fs.canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    // If `root` itself has a `.git` entry (dir or submodule/worktree file), it is already a repo
    // root — outer `.gitignore` files outside this repo must NOT bleed in, but this repo's own
    // `.git/info/exclude` still applies.
    if let Ok(st) = fs.stat(&canonical.join(".git")) {
        if st.is_dir {
            let exclude = canonical.join(".git").join("info").join("exclude");
            let mut buf = Vec::new();
            if let Ok(Some(len)) = fs.read_prefix_into(&exclude, MAX_GITIGNORE_BYTES, &mut buf) {
                if len > 0 {
                    let text = String::from_utf8_lossy(&buf);
                    if let Some(layer) = GitIgnoreLayer::parse(String::new(), &text) {
                        return vec![Arc::new(layer)];
                    }
                }
            }
        }
        return Vec::new();
    }

    let mut ancestors: Vec<PathBuf> = Vec::new();
    let mut cur = canonical.parent().map(Path::to_path_buf);
    let mut repo_root: Option<(PathBuf, bool)> = None;
    let mut steps = 0usize;
    while let Some(dir) = cur {
        if steps >= MAX_ANCESTOR_DEPTH {
            break;
        }
        steps += 1;
        ancestors.push(dir.clone());
        if let Ok(st) = fs.stat(&dir.join(".git")) {
            repo_root = Some((dir, st.is_dir));
            break;
        }
        cur = dir.parent().map(Path::to_path_buf);
    }

    let Some((workdir, git_is_dir)) = repo_root else {
        return Vec::new();
    };

    let mut layers = Vec::new();
    let mut buf = Vec::new();

    // Outermost (repo root) to innermost (immediate parent of `root`), matching Git precedence.
    for dir in ancestors.into_iter().rev() {
        let Ok(rel_root) = canonical.strip_prefix(&dir) else {
            continue;
        };
        let mut prefix = String::new();
        for (i, comp) in rel_root.components().enumerate() {
            if i > 0 {
                prefix.push('/');
            }
            prefix.push_str(&comp.as_os_str().to_string_lossy());
        }
        if prefix.is_empty() {
            continue;
        }
        if dir == workdir && git_is_dir {
            let exclude = dir.join(".git").join("info").join("exclude");
            if let Ok(Some(len)) = fs.read_prefix_into(&exclude, MAX_GITIGNORE_BYTES, &mut buf) {
                if len > 0 {
                    let text = String::from_utf8_lossy(&buf);
                    if let Some(layer) = GitIgnoreLayer::parse_ancestor(prefix.clone(), &text) {
                        layers.push(Arc::new(layer));
                    }
                }
            }
        }
        let gi = dir.join(".gitignore");
        if let Ok(Some(len)) = fs.read_prefix_into(&gi, MAX_GITIGNORE_BYTES, &mut buf) {
            if len > 0 {
                let text = String::from_utf8_lossy(&buf);
                if let Some(layer) = GitIgnoreLayer::parse_ancestor(prefix, &text) {
                    layers.push(Arc::new(layer));
                }
            }
        }
    }
    layers
}

/// Evaluate a stack of ancestor `.gitignore` layers (root-most to deepest); deepest match wins.
pub fn is_gitignored(
    layers: &[std::sync::Arc<GitIgnoreLayer>],
    rel_path: &str,
    name: &str,
    is_dir: bool,
) -> bool {
    let mut ignored = false;
    for layer in layers {
        if let Some(d) = layer.matches(rel_path, name, is_dir) {
            ignored = d;
        }
    }
    ignored
}

/// Match a `[abc]` / `[a-z]` / `[!a-z]` bracket class at `p[pi..]` against byte `ch`.
/// Returns `Some((matched, next_pi))` if a valid closing `]` exists, or `None` for literal `[`.
#[inline]
fn match_bracket_class(p: &[u8], start: usize, ch: u8) -> Option<(bool, usize)> {
    let mut i = start + 1;
    if i >= p.len() {
        return None;
    }
    let negated = if p[i] == b'!' || p[i] == b'^' {
        i += 1;
        true
    } else {
        false
    };
    let mut matched = false;
    let first_idx = i;
    while i < p.len() {
        if p[i] == b']' && i > first_idx {
            return Some((matched ^ negated, i + 1));
        }
        if i + 2 < p.len() && p[i + 1] == b'-' && p[i + 2] != b']' {
            let (lo, hi) = if p[i] <= p[i + 2] {
                (p[i], p[i + 2])
            } else {
                (p[i + 2], p[i])
            };
            if ch >= lo && ch <= hi {
                matched = true;
            }
            i += 3;
        } else {
            if p[i] == ch {
                matched = true;
            }
            i += 1;
        }
    }
    None
}

/// Match a single path segment against a `*` / `?` / `[...]` glob (linear-time two-pointer).
fn glob_match_segment(pat: &str, text: &str) -> bool {
    let p = pat.as_bytes();
    let t = text.as_bytes();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star_pi: Option<usize> = None;
    let mut star_ti = 0usize;

    while ti < t.len() {
        if pi < p.len() && p[pi] == b'*' {
            star_pi = Some(pi);
            star_ti = ti;
            pi += 1;
        } else if pi < p.len() && p[pi] == b'[' {
            if let Some((ok, next_pi)) = match_bracket_class(p, pi, t[ti]) {
                if ok {
                    pi = next_pi;
                    ti += 1;
                    continue;
                }
            } else if t[ti] == b'[' {
                pi += 1;
                ti += 1;
                continue;
            }
            if let Some(sp) = star_pi {
                pi = sp + 1;
                star_ti += 1;
                ti = star_ti;
            } else {
                return false;
            }
        } else if pi < p.len() && p[pi] == b'?' {
            let ch_len = text[ti..].chars().next().map_or(1, char::len_utf8);
            pi += 1;
            ti += ch_len;
        } else if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if let Some(sp) = star_pi {
            pi = sp + 1;
            let ch_len = text[star_ti..].chars().next().map_or(1, char::len_utf8);
            star_ti += ch_len;
            ti = star_ti;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

/// Iterative linear-time multi-segment path matcher supporting `**`, `*`, `?`, and `[...]`.
/// Zero recursion — immune to pathological `**/**/**/**` patterns.
/// Per `gitignore(5)`, a trailing `/**` matches entries inside the directory, not the directory itself.
fn glob_match_path(pat: &str, path: &str) -> bool {
    let p: Vec<&str> = pat.split('/').collect();
    let t: Vec<&str> = path.split('/').collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut dstar_pi: Option<usize> = None;
    let mut dstar_ti = 0usize;

    while ti < t.len() {
        if pi < p.len() && p[pi] == "**" {
            dstar_pi = Some(pi);
            dstar_ti = ti;
            pi += 1;
        } else if pi < p.len() && glob_match_segment(p[pi], t[ti]) {
            pi += 1;
            ti += 1;
        } else if let Some(sp) = dstar_pi {
            pi = sp + 1;
            dstar_ti += 1;
            ti = dstar_ti;
        } else {
            return false;
        }
    }
    pi == p.len()
}

fn glob_match_suffix(pat: &str, path: &str) -> bool {
    for (idx, b) in path.bytes().enumerate() {
        if b == b'/' && glob_match_path(pat, &path[idx + 1..]) {
            return true;
        }
    }
    false
}

/// Return true if a workspace-relative path (`crates/editor-app/src/lib.rs`) matches
/// an interactive comma-separated path filter expression (e.g. `"*.rs, crates/, !tests/"`).
///
/// Semantics:
/// - Empty / whitespace-only filter matches everything (`O(1)`).
/// - Patterns prefixed with `!` exclude matching paths.
/// - If any positive (include) patterns are present, at least one must match.
/// - Patterns containing `*`, `?`, or `[` use our iterative glob matcher against the full path,
///   any suffix, or the basename; patterns ending with `/` match that directory prefix/segment;
///   plain words without globs or `/` perform an ergonomic ASCII case-insensitive substring match.
pub fn matches_path_filter(rel_path: &str, filter_expr: &str) -> bool {
    let trimmed = filter_expr.trim();
    if trimmed.is_empty() {
        return true;
    }
    let norm = rel_path.trim_start_matches("./").trim_start_matches('/');
    let mut has_include = false;
    let mut included = false;

    for raw_tok in trimmed.split(',') {
        let tok = raw_tok.trim();
        if tok.is_empty() {
            continue;
        }
        let (neg, raw_pat) = match tok.strip_prefix('!') {
            Some(rest) => (true, rest.trim()),
            None => (false, tok),
        };
        if raw_pat.is_empty() {
            continue;
        }
        let matched = match_filter_token(norm, raw_pat);
        if neg {
            if matched {
                return false;
            }
        } else {
            has_include = true;
            if matched {
                included = true;
            }
        }
    }
    !has_include || included
}

fn match_filter_token(rel_path: &str, raw_pat: &str) -> bool {
    let pat = raw_pat.trim_start_matches("./").trim_start_matches('/');
    if pat.is_empty() {
        return true;
    }
    let has_glob = pat.bytes().any(|b| matches!(b, b'*' | b'?' | b'['));
    if let Some(dir_pat) = pat.strip_suffix('/') {
        if dir_pat.is_empty() {
            return true;
        }
        if has_glob {
            let full = format!("{dir_pat}/**");
            return glob_match_path(&full, rel_path) || glob_match_suffix(&full, rel_path);
        }
        return rel_path.starts_with(pat)
            || rel_path.contains(&format!("/{pat}"))
            || rel_path == dir_pat;
    }
    if has_glob {
        if pat.contains('/') {
            glob_match_path(pat, rel_path) || glob_match_suffix(pat, rel_path)
        } else {
            let base = rel_path.rsplit('/').next().unwrap_or(rel_path);
            glob_match_segment(pat, base) || glob_match_path(pat, rel_path)
        }
    } else if pat.contains('/') {
        rel_path.starts_with(pat) || rel_path.contains(pat)
    } else {
        // Ergonomic ASCII case-insensitive substring match for plain terms like `palette` or `rs`.
        contains_ascii_ci(rel_path.as_bytes(), pat.as_bytes())
    }
}

fn contains_ascii_ci(hay: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if needle.len() > hay.len() {
        return false;
    }
    hay.windows(needle.len()).any(|w| {
        w.iter()
            .zip(needle.iter())
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn platform_ignore_rules_scope_home_and_os_vaults_cleanly() {
        let home = PathBuf::from("/mock_home/alice");
        let mac = IgnoreRules::macos(Some(home.clone()));
        // Skips OS vaults and hidden toolchains directly inside `$HOME`.
        assert!(mac.should_skip_dir(&home, "Library"));
        assert!(mac.should_skip_dir(&home, "Pictures"));
        assert!(mac.should_skip_dir(&home, ".cargo"));
        assert!(mac.should_skip_dir(&home, ".github"));
        // Keeps user workspaces directly inside `$HOME`.
        assert!(!mac.should_skip_dir(&home, "Projects"));
        assert!(!mac.should_skip_dir(&home, "Desktop"));
        // Never skips a project folder literally named `Library`, and keeps tracked project config
        // hidden dirs (`.github`, `.cargo`, `.devcontainer`) inside repos while still skipping caches.
        let repo = PathBuf::from("/mock_home/alice/Projects/app");
        assert!(!mac.should_skip_dir(&repo, "Library"));
        assert!(!mac.should_skip_dir(&repo, ".github"));
        assert!(!mac.should_skip_dir(&repo, ".cargo"));
        assert!(!mac.should_skip_dir(&repo, ".devcontainer"));
        assert!(mac.should_skip_dir(&repo, ".cache"));
        assert!(mac.should_skip_dir(&repo, ".git"));
        // Skips macOS bundles anywhere.
        assert!(mac.should_skip_dir(&repo, "Editor.app"));
        assert!(mac.should_skip_dir(&repo, "Photos.photoslibrary"));
        assert!(mac.should_skip_file(".DS_Store"));
        assert!(!mac.should_skip_file(".gitignore"));
        // Skips `/` system roots.
        assert!(mac.should_skip_dir(Path::new("/"), "System"));
        assert!(mac.should_skip_dir(Path::new("/"), "Volumes"));

        // Linux rules.
        let lin = IgnoreRules::linux(Some(PathBuf::from("/home/alice")));
        assert!(lin.should_skip_dir(Path::new("/"), "proc"));
        assert!(lin.should_skip_dir(Path::new("/"), "sys"));
        assert!(lin.should_skip_dir(Path::new("/home/alice"), "snap"));
        assert!(lin.should_skip_file(".directory"));

        // Windows rules (case-insensitive).
        let win = IgnoreRules::windows(Some(PathBuf::from("C:\\MockUsers\\Alice")));
        assert!(win.should_skip_dir(Path::new("C:\\MockUsers\\Alice"), "appdata"));
        assert!(win.should_skip_dir(Path::new("C:\\"), "windows"));
        assert!(win.should_skip_file("thumbs.db"));
        assert!(win.should_skip_file("Desktop.ini"));
    }

    #[test]
    fn gitignore_matcher_handles_globs_negation_dir_only_and_double_star() {
        let root_layer = Arc::new(
            GitIgnoreLayer::parse(
                String::new(),
                "# comment\n*.log\n!keep.log\nout/\n/top_only.txt\n**/generated/*.rs\n!**/generated/keep.rs\n*.py[cod]\n*.[oa]\n**/**/**/**/deep.tmp\n",
            )
            .unwrap(),
        );
        let sub_layer = Arc::new(
            GitIgnoreLayer::parse("sub".to_string(), "local_tmp\n!keep_sub.log\n").unwrap(),
        );
        let layers = vec![root_layer, sub_layer];

        assert!(is_gitignored(&layers, "debug.log", "debug.log", false));
        assert!(!is_gitignored(&layers, "keep.log", "keep.log", false));
        assert!(is_gitignored(&layers, "sub/a.log", "a.log", false));
        assert!(!is_gitignored(&layers, "sub/keep_sub.log", "keep_sub.log", false));
        assert!(is_gitignored(&layers, "out", "out", true));
        assert!(!is_gitignored(&layers, "out", "out", false));
        assert!(is_gitignored(&layers, "top_only.txt", "top_only.txt", false));
        assert!(!is_gitignored(&layers, "sub/top_only.txt", "top_only.txt", false));
        assert!(is_gitignored(&layers, "a/b/generated/foo.rs", "foo.rs", false));
        assert!(!is_gitignored(&layers, "a/b/generated/keep.rs", "keep.rs", false));
        assert!(is_gitignored(&layers, "mod.pyc", "mod.pyc", false));
        assert!(!is_gitignored(&layers, "mod.py", "mod.py", false));
        assert!(is_gitignored(&layers, "lib.o", "lib.o", false));
        assert!(is_gitignored(&layers, "a/b/c/d/deep.tmp", "deep.tmp", false));
        assert!(is_gitignored(&layers, "sub/local_tmp", "local_tmp", false));
    }

    #[test]
    fn path_filter_supports_globs_dirs_negation_and_substrings() {
        assert!(matches_path_filter("crates/editor-app/src/lib.rs", ""));
        assert!(matches_path_filter("crates/editor-app/src/lib.rs", "*.rs"));
        assert!(!matches_path_filter("README.md", "*.rs"));
        assert!(matches_path_filter("crates/editor-app/src/lib.rs", "*.rs, !tests/"));
        assert!(!matches_path_filter("crates/editor-app/tests/app_tests.rs", "*.rs, !tests/"));
        assert!(matches_path_filter("crates/editor-render/src/palette.rs", "palette"));
        assert!(matches_path_filter("crates/editor-render/src/palette.rs", "crates/editor-render/"));
        assert!(!matches_path_filter("crates/editor-core/src/lib.rs", "crates/editor-render/"));
        assert!(!matches_path_filter("target/debug/out.rs", "!target/"));
        assert!(matches_path_filter("src/main.rs", "!target/"));
    }

    #[test]
    fn ancestor_gitignores_apply_to_monorepo_subfolders_and_respect_anchoring() {
        use crate::fs::MemoryFileSystem;

        let fs = MemoryFileSystem::new();
        fs.add_file("/mono/.git/HEAD", b"ref: refs/heads/main\n".to_vec());
        fs.add_file(
            "/mono/.gitignore",
            b"*.log\n/top_only.txt\nservices/api/gen/\n!services/api/keep.log\n".to_vec(),
        );
        fs.add_file("/mono/services/.gitignore", b"local_tmp/\n".to_vec());
        fs.add_file("/mono/services/api/src/main.rs", b"fn main() {}\n".to_vec());

        // Opening the monorepo root has zero ancestor layers above it.
        assert!(collect_ancestor_gitignores(&fs, Path::new("/mono")).is_empty());

        // Opening `/mono/services/api` loads `/mono/.gitignore` + `/mono/services/.gitignore` in order.
        let layers = collect_ancestor_gitignores(&fs, Path::new("/mono/services/api"));
        assert_eq!(layers.len(), 2);

        // Unanchored rule `*.log` from monorepo root applies, and negated path rule overrides it.
        assert!(is_gitignored(&layers, "debug.log", "debug.log", false));
        assert!(!is_gitignored(&layers, "keep.log", "keep.log", false));
        // Anchored `/top_only.txt` at `/mono` must NOT ignore `/mono/services/api/top_only.txt`.
        assert!(!is_gitignored(&layers, "top_only.txt", "top_only.txt", false));
        // Path-anchored `services/api/gen/` from `/mono/.gitignore` matches `gen/` inside subfolder!
        assert!(is_gitignored(&layers, "gen", "gen", true));
        assert!(!is_gitignored(&layers, "gen", "gen", false));
        // Intermediate ancestor `/mono/services/.gitignore` rule `local_tmp/` applies too.
        assert!(is_gitignored(&layers, "local_tmp", "local_tmp", true));

        // Submodule / worktree `.git` pointer file is skipped by `should_skip_file`.
        let rules = IgnoreRules::macos(None);
        assert!(rules.should_skip_file(".git"));
    }

    #[test]
    fn audit_regressions_non_ascii_dirs_double_star_and_char_globs() {
        // Non-ASCII directory names whose byte length >= `.app`/`.bundle` must never panic.
        let mac = IgnoreRules::macos(None);
        let proj = Path::new("/proj");
        for name in ["über", "日本語", "📁docs", "café", "a.app"] {
            let skipped = mac.should_skip_dir(proj, name);
            assert_eq!(skipped, name == "a.app");
        }

        // `foo/**` ignores contents under `foo/`, NOT directory `foo` itself.
        let layer = Arc::new(
            GitIgnoreLayer::parse(String::new(), "foo/**\n/**/deep.log\na?c.txt\n").unwrap(),
        );
        let layers = vec![layer];
        assert!(!is_gitignored(&layers, "foo", "foo", true));
        assert!(is_gitignored(&layers, "foo/bar.txt", "bar.txt", false));
        assert!(is_gitignored(&layers, "foo/sub/bar.txt", "bar.txt", false));

        // `/**/deep.log` matches at any depth.
        assert!(is_gitignored(&layers, "deep.log", "deep.log", false));
        assert!(is_gitignored(&layers, "x/y/deep.log", "deep.log", false));

        // `?` matches a single Unicode scalar (multi-byte UTF-8).
        assert!(is_gitignored(&layers, "aéc.txt", "aéc.txt", false));
        assert!(!is_gitignored(&layers, "aéé.txt", "aéé.txt", false));
    }
}
