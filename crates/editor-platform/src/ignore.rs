//! Platform-specific and `.gitignore`-aware workspace traversal rules.
//!
//! Each target OS declares its system vaults, package bundles, and home-directory
//! exclusions in one declarative table here so adding Linux, Windows, or BSD support
//! requires zero OS-specific branches in `editor-render` or `editor-app`.

use std::path::{Path, PathBuf};

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
            for &suf in self.dir_suffixes {
                if name.len() > suf.len()
                    && name[name.len() - suf.len()..].eq_ignore_ascii_case(suf)
                {
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

/// Parsed rules from a single `.gitignore` file, scoped to its directory prefix (`base_rel`).
#[derive(Debug, Clone)]
pub struct GitIgnoreLayer {
    /// Forward-slash relative path from search root (`""` for root `.gitignore`).
    base_rel: String,
    rules: Vec<GitIgnoreRule>,
}

impl GitIgnoreLayer {
    /// Parse a `.gitignore` file located at `base_rel` relative to the workspace root.
    pub fn parse(base_rel: String, content: &str) -> Option<Self> {
        let mut rules = Vec::new();
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
            let anchored = leading_slash || (has_slash && !has_leading_dstar);
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
            Some(Self { base_rel, rules })
        }
    }

    /// Evaluate all rules in this layer against `rel_path` (relative to workspace root).
    /// Returns `Some(true)` if ignored, `Some(false)` if explicitly un-ignored (`!`), `None` if no rule matched.
    pub fn matches(&self, rel_path: &str, name: &str, is_dir: bool) -> Option<bool> {
        let local_rel = if self.base_rel.is_empty() {
            rel_path
        } else {
            let rest = rel_path.strip_prefix(&self.base_rel)?;
            rest.strip_prefix('/')?
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
        } else if pi < p.len() && (p[pi] == b'?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if let Some(sp) = star_pi {
            pi = sp + 1;
            star_ti += 1;
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
    while pi < p.len() && p[pi] == "**" {
        pi += 1;
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
}
