//! Zero-dependency, cross-platform GitHub Releases updater primitives (`std` only).
//!
//! Portable across macOS, Windows, and Linux:
//! - Static CDN manifest parsing (`release.json`) with strict HTTPS host allowlisting.
//! - Pure-Rust FIPS 180-4 SHA-256 verification (tested against NIST vectors).
//! - Platform-keyed asset selection (`macos-universal`, `windows-x86_64`, `linux-x86_64`, ...).
//! - Enclosing `.app` bundle detection, App Translocation & dev-build guards, and
//!   atomic sibling swap with automatic rollback on failure.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::version::{compare_semver, SemVer, APP_VERSION};

/// Public GitHub Releases static manifest URL (CDN-backed; bypasses the 60 req/hr API limit).
pub const RELEASE_MANIFEST_URL: &str =
    "https://github.com/builduseful/editor/releases/latest/download/release.json";

/// Maximum accepted size of `release.json` (64 KiB).
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;

/// Maximum accepted size of a release archive download (64 MiB).
pub const MAX_ARCHIVE_BYTES: usize = 64 * 1024 * 1024;

/// Minimum interval between passive on-launch update checks (24 hours).
pub const AUTO_CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseAsset {
    pub url: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseManifest {
    pub version: String,
    pub build: u32,
    pub notes_url: Option<String>,
    pub assets: BTreeMap<String, ReleaseAsset>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateInfo {
    pub version: String,
    pub build: u32,
    pub asset_url: String,
    pub sha256: String,
    pub notes_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateCheckOutcome {
    UpToDate { current: String, latest: String },
    Available(UpdateInfo),
}

/// Ordered candidate asset keys for the current OS and CPU architecture.
pub fn platform_asset_keys() -> &'static [&'static str] {
    #[cfg(target_os = "macos")]
    {
        #[cfg(target_arch = "aarch64")]
        {
            &["macos-universal", "macos-arm64"]
        }
        #[cfg(target_arch = "x86_64")]
        {
            &["macos-universal", "macos-x86_64"]
        }
        #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
        {
            &["macos-universal"]
        }
    }
    #[cfg(target_os = "windows")]
    {
        #[cfg(target_arch = "aarch64")]
        {
            &["windows-arm64", "windows-x86_64"]
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            &["windows-x86_64"]
        }
    }
    #[cfg(target_os = "linux")]
    {
        #[cfg(target_arch = "aarch64")]
        {
            &["linux-arm64"]
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            &["linux-x86_64"]
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        &[]
    }
}

/// Current Unix timestamp in seconds.
pub fn unix_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Returns true if an automatic launch check should run now.
pub fn should_auto_check(auto_enabled: bool, last_check_secs: u64, now_secs: u64) -> bool {
    auto_enabled && now_secs.saturating_sub(last_check_secs) >= AUTO_CHECK_INTERVAL_SECS
}

/// Strict URL allowlist for manifest downloads and release assets:
/// HTTPS only, no credentials/whitespace/control chars, host must be `github.com`
/// or a subdomain of `githubusercontent.com`.
pub fn validate_release_url(url: &str) -> Result<(), String> {
    if url.is_empty() || url.len() > 2048 {
        return Err("invalid release URL length".to_string());
    }
    if url.bytes().any(|b| b <= 0x20 || b == 0x7f) {
        return Err("release URL contains invalid characters".to_string());
    }
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| "release URL must use https://".to_string())?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') || authority.contains(':') {
        return Err("release URL has disallowed authority".to_string());
    }
    let host = authority.to_ascii_lowercase();
    let allowed = host == "github.com" || host.ends_with(".githubusercontent.com");
    if !allowed {
        return Err(format!("disallowed release host: {host}"));
    }
    Ok(())
}

/// Normalize and validate a 64-character hexadecimal SHA-256 digest.
pub fn normalize_sha256(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.len() != 64 || !trimmed.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid SHA-256 digest in release manifest".to_string());
    }
    Ok(trimmed.to_ascii_lowercase())
}

/// Evaluate a parsed `ReleaseManifest` against `current_version` and `asset_keys`.
pub fn evaluate_manifest(
    manifest: &ReleaseManifest,
    current_version: &str,
    asset_keys: &[&str],
) -> Result<UpdateCheckOutcome, String> {
    let ord = compare_semver(current_version, &manifest.version)
        .ok_or_else(|| format!("invalid version in manifest: {}", manifest.version))?;
    if ord != Ordering::Less {
        return Ok(UpdateCheckOutcome::UpToDate {
            current: current_version.to_string(),
            latest: manifest.version.clone(),
        });
    }
    let asset = asset_keys
        .iter()
        .find_map(|k| manifest.assets.get(*k))
        .ok_or_else(|| {
            format!(
                "release {} has no download for this platform",
                manifest.version
            )
        })?;
    validate_release_url(&asset.url)?;
    let sha256 = normalize_sha256(&asset.sha256)?;
    if let Some(ref notes) = manifest.notes_url {
        validate_release_url(notes)?;
    }
    Ok(UpdateCheckOutcome::Available(UpdateInfo {
        version: manifest.version.clone(),
        build: manifest.build,
        asset_url: asset.url.clone(),
        sha256,
        notes_url: manifest.notes_url.clone(),
    }))
}

/// Evaluate `release.json` bytes for the current binary version and host platform.
pub fn check_manifest_bytes(bytes: &[u8]) -> Result<UpdateCheckOutcome, String> {
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err("release manifest exceeds size limit".to_string());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "release manifest is not valid UTF-8")?;
    let manifest = parse_release_manifest(text)?;
    evaluate_manifest(&manifest, APP_VERSION, platform_asset_keys())
}

/// Resolve the OS-bundled `curl` executable using an absolute system path when available
/// to avoid `PATH` hijacking.
fn resolve_curl_binary() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        const SYS_CURL: &str = r"C:\Windows\System32\curl.exe";
        if Path::new(SYS_CURL).exists() {
            SYS_CURL
        } else {
            "curl.exe"
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        if Path::new("/usr/bin/curl").exists() {
            "/usr/bin/curl"
        } else {
            "curl"
        }
    }
}

/// Hardened, cross-platform one-shot HTTPS GET via the OS-bundled `curl` subprocess.
///
/// Running HTTPS fetches out-of-process keeps TLS/HTTP frameworks (`CFNetwork`, `Security`,
/// `libboringssl`, `WinHTTP`, `OpenSSL`) out of the long-lived GUI process (~4–10 MB saved
/// permanently) while sharing one audited implementation across GUI and CLI:
/// - Strict host/scheme allowlist (`validate_release_url`)
/// - `-q` (`--disable`) first to ignore user `~/.curlrc`
/// - `--proto =https --proto-redir =https --max-redirs 5` (never downgrades off HTTPS)
/// - Bounded stdout read (`max_bytes + 1`) even on chunked transfers without `Content-Length`
pub fn https_get_via_curl(url: &str, max_bytes: usize) -> Result<Vec<u8>, String> {
    validate_release_url(url)?;
    let curl = resolve_curl_binary();
    let max_size_str = max_bytes.to_string();

    let mut cmd = Command::new(curl);
    cmd.args([
        "-q",
        "-fsSL",
        "--proto",
        "=https",
        "--proto-redir",
        "=https",
        "--max-redirs",
        "5",
        "--connect-timeout",
        "15",
        "--max-time",
        "60",
        "--user-agent",
        "editor-updater/1",
        "--max-filesize",
        &max_size_str,
        "--",
        url,
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to execute {curl}: {e}"))?;

    let mut body = Vec::new();
    let limit = (max_bytes as u64).saturating_add(1);
    if let Some(stdout) = child.stdout.take() {
        stdout
            .take(limit)
            .read_to_end(&mut body)
            .map_err(|e| format!("failed to read download stream: {e}"))?;
    }

    if body.len() > max_bytes {
        let _ = child.kill();
        let _ = child.wait();
        return Err("downloaded payload exceeded size limit".to_string());
    }

    let mut err_buf = Vec::new();
    if let Some(stderr) = child.stderr.take() {
        let _ = stderr.take(4096).read_to_end(&mut err_buf);
    }

    let status = child
        .wait()
        .map_err(|e| format!("failed waiting for {curl}: {e}"))?;

    if !status.success() {
        let stderr_str = String::from_utf8_lossy(&err_buf);
        let msg = stderr_str.trim();
        return Err(if msg.is_empty() {
            format!("download failed with status {status}")
        } else {
            msg.to_string()
        });
    }

    Ok(body)
}

// ============================================================================
// Minimal Zero-Dependency JSON Parser for `release.json`
// ============================================================================

struct JsonCursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> JsonCursor<'a> {
    fn new(s: &'a str) -> Self {
        Self {
            bytes: s.as_bytes(),
            pos: 0,
        }
    }

    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len() {
            match self.bytes[self.pos] {
                b' ' | b'\t' | b'\r' | b'\n' => self.pos += 1,
                _ => break,
            }
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip_ws();
        self.bytes.get(self.pos).copied()
    }

    fn expect_byte(&mut self, expected: u8) -> Result<(), String> {
        self.skip_ws();
        if self.bytes.get(self.pos) == Some(&expected) {
            self.pos += 1;
            Ok(())
        } else {
            Err(format!(
                "expected '{}' in release manifest at byte {}",
                expected as char, self.pos
            ))
        }
    }

    fn parse_string(&mut self) -> Result<String, String> {
        self.expect_byte(b'"')?;
        let mut out = String::new();
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            self.pos += 1;
            match b {
                b'"' => return Ok(out),
                b'\\' => {
                    if self.pos >= self.bytes.len() {
                        return Err("unterminated escape in JSON string".to_string());
                    }
                    let esc = self.bytes[self.pos];
                    self.pos += 1;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{0008}'),
                        b'f' => out.push('\u{000C}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            if self.pos + 4 > self.bytes.len() {
                                return Err("invalid \\u escape in JSON string".to_string());
                            }
                            let hex = std::str::from_utf8(&self.bytes[self.pos..self.pos + 4])
                                .map_err(|_| "invalid UTF-8 in \\u escape")?;
                            let code = u32::from_str_radix(hex, 16)
                                .map_err(|_| "invalid hex in \\u escape")?;
                            self.pos += 4;
                            let scalar = if (0xD800..=0xDBFF).contains(&code) {
                                // UTF-16 high surrogate: expect trailing `\uXXXX` low surrogate.
                                if self.pos + 6 > self.bytes.len()
                                    || self.bytes[self.pos] != b'\\'
                                    || self.bytes[self.pos + 1] != b'u'
                                {
                                    return Err("unpaired UTF-16 surrogate in JSON string".to_string());
                                }
                                let lo_hex = std::str::from_utf8(&self.bytes[self.pos + 2..self.pos + 6])
                                    .map_err(|_| "invalid UTF-8 in \\u escape")?;
                                let lo = u32::from_str_radix(lo_hex, 16)
                                    .map_err(|_| "invalid hex in \\u escape")?;
                                if !(0xDC00..=0xDFFF).contains(&lo) {
                                    return Err("invalid UTF-16 low surrogate in JSON string".to_string());
                                }
                                self.pos += 6;
                                0x10000 + (((code - 0xD800) << 10) | (lo - 0xDC00))
                            } else {
                                code
                            };
                            let ch = char::from_u32(scalar)
                                .ok_or("invalid Unicode scalar in \\u escape")?;
                            out.push(ch);
                        }
                        _ => return Err("unknown escape sequence in JSON string".to_string()),
                    }
                }
                c if c < 0x20 => return Err("control character in JSON string".to_string()),
                _ => {
                    // Decode UTF-8 sequence starting at self.pos - 1.
                    let start = self.pos - 1;
                    let mut end = self.pos;
                    while end < self.bytes.len() && (self.bytes[end] & 0xC0) == 0x80 {
                        end += 1;
                    }
                    let s = std::str::from_utf8(&self.bytes[start..end])
                        .map_err(|_| "invalid UTF-8 in JSON string")?;
                    out.push_str(s);
                    self.pos = end;
                }
            }
        }
        Err("unterminated JSON string".to_string())
    }

    fn parse_u64(&mut self) -> Result<u64, String> {
        self.skip_ws();
        let start = self.pos;
        while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_digit() {
            self.pos += 1;
        }
        if self.pos == start {
            return Err("expected integer in release manifest".to_string());
        }
        let s = std::str::from_utf8(&self.bytes[start..self.pos]).unwrap_or("");
        s.parse::<u64>()
            .map_err(|_| "integer overflow in release manifest".to_string())
    }

    fn skip_value(&mut self, depth: usize) -> Result<(), String> {
        if depth > 16 {
            return Err("JSON nesting too deep".to_string());
        }
        match self.peek() {
            Some(b'"') => {
                let _ = self.parse_string()?;
                Ok(())
            }
            Some(b'{') => {
                self.pos += 1;
                if self.peek() == Some(b'}') {
                    self.pos += 1;
                    return Ok(());
                }
                loop {
                    let _ = self.parse_string()?;
                    self.expect_byte(b':')?;
                    self.skip_value(depth + 1)?;
                    match self.peek() {
                        Some(b',') => self.pos += 1,
                        Some(b'}') => {
                            self.pos += 1;
                            return Ok(());
                        }
                        _ => return Err("malformed JSON object".to_string()),
                    }
                }
            }
            Some(b'[') => {
                self.pos += 1;
                if self.peek() == Some(b']') {
                    self.pos += 1;
                    return Ok(());
                }
                loop {
                    self.skip_value(depth + 1)?;
                    match self.peek() {
                        Some(b',') => self.pos += 1,
                        Some(b']') => {
                            self.pos += 1;
                            return Ok(());
                        }
                        _ => return Err("malformed JSON array".to_string()),
                    }
                }
            }
            Some(b't') => self.expect_lit(b"true"),
            Some(b'f') => self.expect_lit(b"false"),
            Some(b'n') => self.expect_lit(b"null"),
            Some(b'-' | b'0'..=b'9') => {
                if self.bytes[self.pos] == b'-' {
                    self.pos += 1;
                }
                while self.pos < self.bytes.len()
                    && matches!(
                        self.bytes[self.pos],
                        b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-'
                    )
                {
                    self.pos += 1;
                }
                Ok(())
            }
            _ => Err("unexpected token in JSON".to_string()),
        }
    }

    fn expect_lit(&mut self, lit: &[u8]) -> Result<(), String> {
        self.skip_ws();
        if self.bytes[self.pos..].starts_with(lit) {
            self.pos += lit.len();
            Ok(())
        } else {
            Err("unexpected literal in JSON".to_string())
        }
    }
}

/// Parse a `release.json` document into a validated [`ReleaseManifest`].
pub fn parse_release_manifest(json: &str) -> Result<ReleaseManifest, String> {
    let mut c = JsonCursor::new(json);
    c.expect_byte(b'{')?;

    let mut version: Option<String> = None;
    let mut build: Option<u32> = None;
    let mut notes_url: Option<String> = None;
    let mut assets: BTreeMap<String, ReleaseAsset> = BTreeMap::new();

    if c.peek() != Some(b'}') {
        loop {
            let key = c.parse_string()?;
            c.expect_byte(b':')?;
            match key.as_str() {
                "version" => version = Some(c.parse_string()?),
                "build" => {
                    let n = c.parse_u64()?;
                    build = Some(n.min(u32::MAX as u64) as u32);
                }
                "notes_url" => {
                    if c.peek() == Some(b'n') {
                        c.expect_lit(b"null")?;
                    } else {
                        notes_url = Some(c.parse_string()?);
                    }
                }
                "assets" => {
                    c.expect_byte(b'{')?;
                    if c.peek() != Some(b'}') {
                        loop {
                            let plat = c.parse_string()?;
                            c.expect_byte(b':')?;
                            c.expect_byte(b'{')?;
                            let mut url: Option<String> = None;
                            let mut sha256: Option<String> = None;
                            if c.peek() != Some(b'}') {
                                loop {
                                    let ak = c.parse_string()?;
                                    c.expect_byte(b':')?;
                                    match ak.as_str() {
                                        "url" => url = Some(c.parse_string()?),
                                        "sha256" => sha256 = Some(c.parse_string()?),
                                        _ => c.skip_value(0)?,
                                    }
                                    match c.peek() {
                                        Some(b',') => c.pos += 1,
                                        Some(b'}') => break,
                                        _ => return Err("malformed asset object".to_string()),
                                    }
                                }
                            }
                            c.expect_byte(b'}')?;
                            if let (Some(u), Some(s)) = (url, sha256) {
                                assets.insert(plat, ReleaseAsset { url: u, sha256: s });
                            }
                            match c.peek() {
                                Some(b',') => c.pos += 1,
                                Some(b'}') => break,
                                _ => return Err("malformed assets map".to_string()),
                            }
                        }
                    }
                    c.expect_byte(b'}')?;
                }
                _ => c.skip_value(0)?,
            }
            match c.peek() {
                Some(b',') => c.pos += 1,
                Some(b'}') => break,
                _ => return Err("malformed release manifest JSON".to_string()),
            }
        }
    }
    c.expect_byte(b'}')?;

    let version = version.ok_or_else(|| "release manifest missing `version`".to_string())?;
    let parsed_ver = SemVer::parse(&version)
        .ok_or_else(|| format!("invalid SemVer `{version}` in release manifest"))?;
    let build = build.unwrap_or_else(|| parsed_ver.build_number());
    Ok(ReleaseManifest {
        version,
        build,
        notes_url,
        assets,
    })
}

// ============================================================================
// Pure-Rust FIPS 180-4 SHA-256 (zero external crates, portable to every OS)
// ============================================================================

const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// Compute the SHA-256 digest of `data` as 32 raw bytes.
pub fn sha256_bytes(data: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let compress = |h: &mut [u32; 8], block: &[u8]| {
        let mut w = [0u32; 64];
        for i in 0..16 {
            let j = i * 4;
            w[i] = u32::from_be_bytes([block[j], block[j + 1], block[j + 2], block[j + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = *h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(SHA256_K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    };

    let mut chunks = data.chunks_exact(64);
    for block in &mut chunks {
        compress(&mut h, block);
    }

    let rem = chunks.remainder();
    let mut tail = [0u8; 128];
    tail[..rem.len()].copy_from_slice(rem);
    tail[rem.len()] = 0x80;
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let padded_len = if rem.len() < 56 { 64 } else { 128 };
    tail[padded_len - 8..padded_len].copy_from_slice(&bit_len.to_be_bytes());
    compress(&mut h, &tail[..64]);
    if padded_len == 128 {
        compress(&mut h, &tail[64..128]);
    }

    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// Compute lowercase hexadecimal SHA-256 digest of `data`.
pub fn sha256_hex(data: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = sha256_bytes(data);
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

// ============================================================================
// Bundle Detection, Guards, Verification & Atomic Swap
// ============================================================================

/// If `exe_path` is inside `<bundle>.app/Contents/MacOS/<bin>`, return `<bundle>.app`.
pub fn enclosing_app_bundle(exe_path: &Path) -> Option<PathBuf> {
    let macos_dir = exe_path.parent()?;
    if macos_dir.file_name().and_then(|n| n.to_str()) != Some("MacOS") {
        return None;
    }
    let contents_dir = macos_dir.parent()?;
    if contents_dir.file_name().and_then(|n| n.to_str()) != Some("Contents") {
        return None;
    }
    let app_dir = contents_dir.parent()?;
    let ext = app_dir.extension().and_then(|e| e.to_str())?;
    if ext.eq_ignore_ascii_case("app") {
        Some(app_dir.to_path_buf())
    } else {
        None
    }
}

/// Returns true if `path` is running under macOS Gatekeeper App Translocation
/// (a read-only randomized path created when launched directly from `Downloads`).
pub fn is_app_translocated(path: &Path) -> bool {
    path.to_string_lossy().contains("/AppTranslocation/")
}

/// Returns true if `path` appears to be a Cargo build output inside a source checkout.
pub fn is_dev_build_path(path: &Path) -> bool {
    let s = path.to_string_lossy();
    s.contains("/target/debug/")
        || s.contains("/target/release/")
        || s.contains("\\target\\debug\\")
        || s.contains("\\target\\release\\")
}

/// Probe whether `dir` is genuinely writable by the current user (respecting ACLs/SIP).
pub fn is_dir_writable(dir: &Path) -> bool {
    use std::sync::atomic::{AtomicU64, Ordering};
    static PROBE_SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = PROBE_SEQ.fetch_add(1, Ordering::Relaxed);
    let probe = dir.join(format!(".editor-write-probe-{}-{seq}", std::process::id()));
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Resolve the macOS `Editor.app` target that can be updated in place from the running GUI.
pub fn resolve_updatable_macos_app() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate executable: {e}"))?;
    let canonical = fs::canonicalize(&exe).unwrap_or(exe);
    let Some(app) = enclosing_app_bundle(&canonical) else {
        return Err("Running outside Editor.app — install Editor.app to enable in-app updates"
            .to_string());
    };
    if is_app_translocated(&app) {
        return Err(
            "Move Editor.app to /Applications or ~/Applications before updating".to_string(),
        );
    }
    if is_dev_build_path(&app) {
        return Err("Running from a development build — install Editor.app to update".to_string());
    }
    let parent = app
        .parent()
        .ok_or_else(|| "cannot resolve Editor.app parent directory".to_string())?;
    if !is_dir_writable(parent) {
        return Err(format!(
            "{} is not writable — run `sudo editor update` or reinstall with `--user`",
            parent.display()
        ));
    }
    Ok(app)
}

/// Atomically replace `target_app` with `staged_app` (both must be on the same filesystem).
/// If replacing fails after moving `target_app` aside, restores the previous bundle.
pub fn atomic_swap_bundle(target_app: &Path, staged_app: &Path) -> Result<(), String> {
    let parent = target_app
        .parent()
        .ok_or_else(|| "invalid target app path".to_string())?;
    let name = target_app
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("Editor.app");
    let prev_app = parent.join(format!(".{name}.old.{}", std::process::id()));
    if prev_app.exists() {
        let _ = fs::remove_dir_all(&prev_app);
    }

    let had_existing = fs::symlink_metadata(target_app).is_ok();
    if had_existing {
        fs::rename(target_app, &prev_app)
            .map_err(|e| format!("failed to move current bundle aside: {e}"))?;
    }

    if let Err(e) = fs::rename(staged_app, target_app) {
        if had_existing {
            let _ = fs::rename(&prev_app, target_app);
        }
        return Err(format!("failed to move updated bundle into place: {e}"));
    }

    if had_existing {
        let _ = fs::remove_dir_all(&prev_app);
    }
    Ok(())
}

/// Extract, cryptographically verify, code-signature verify, version-verify, and atomically
/// swap a downloaded macOS `Editor.app` `.tar.gz` archive into `target_app`.
#[cfg(target_os = "macos")]
pub fn install_macos_tar_gz(
    archive_bytes: &[u8],
    expected_sha256: &str,
    expected_version: &str,
    target_app: &Path,
) -> Result<(), String> {
    use std::process::Command;

    let expected = normalize_sha256(expected_sha256)?;
    let actual = sha256_hex(archive_bytes);
    if actual != expected {
        return Err(format!(
            "SHA-256 mismatch (expected {}, got {})",
            &expected[..12],
            &actual[..12]
        ));
    }

    let parent = target_app
        .parent()
        .ok_or_else(|| "invalid target app path".to_string())?;
    if !is_dir_writable(parent) {
        return Err(format!("{} is not writable", parent.display()));
    }

    let staging_root = parent.join(format!(
        ".Editor.app.staging.{}.{}",
        std::process::id(),
        unix_now_secs()
    ));
    let _ = fs::remove_dir_all(&staging_root);
    fs::create_dir_all(&staging_root)
        .map_err(|e| format!("failed to create staging directory: {e}"))?;

    struct StagingCleanup(PathBuf);
    impl Drop for StagingCleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = StagingCleanup(staging_root.clone());

    let archive_path = staging_root.join("update.tar.gz");
    fs::write(&archive_path, archive_bytes)
        .map_err(|e| format!("failed to write staged archive: {e}"))?;

    let tar_status = Command::new("/usr/bin/tar")
        .args(["-xzf"])
        .arg(&archive_path)
        .arg("-C")
        .arg(&staging_root)
        .status()
        .map_err(|e| format!("failed to run tar: {e}"))?;
    let _ = fs::remove_file(&archive_path);
    if !tar_status.success() {
        return Err("failed to extract update archive".to_string());
    }

    let staged_app = staging_root.join("Editor.app");
    let meta = fs::symlink_metadata(&staged_app)
        .map_err(|_| "update archive does not contain Editor.app".to_string())?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err("staged Editor.app is not a valid directory".to_string());
    }

    let staged_gui = staged_app.join("Contents/MacOS/editor-gui");
    let staged_cli = staged_app.join("Contents/MacOS/editor");
    for bin in [&staged_gui, &staged_cli] {
        let bm = fs::symlink_metadata(bin)
            .map_err(|_| format!("update bundle missing {}", bin.display()))?;
        if !bm.is_file() || bm.file_type().is_symlink() {
            return Err(format!("invalid binary in update bundle: {}", bin.display()));
        }
    }

    // Verify bundle code signature integrity.
    let cs = Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(&staged_app)
        .output()
        .map_err(|e| format!("failed to invoke codesign: {e}"))?;
    if !cs.status.success() {
        return Err("update bundle failed code signature verification".to_string());
    }

    // If the currently installed app has a Developer ID TeamIdentifier, require the update
    // to match it (allows ad-hoc -> ad-hoc and ad-hoc -> Developer ID, blocks downgrade).
    if target_app.exists() {
        if let Some(cur_team) = codesign_team_id(target_app) {
            let new_team = codesign_team_id(&staged_app);
            if new_team.as_deref() != Some(cur_team.as_str()) {
                return Err("update bundle signing team does not match installed app".to_string());
            }
        }
    }

    // Smoke-test the staged binary and verify it reports the expected version before swapping!
    let ver_out = Command::new(&staged_cli)
        .arg("--version")
        .output()
        .map_err(|e| format!("failed to execute staged CLI: {e}"))?;
    if !ver_out.status.success() {
        return Err("staged binary failed version self-check".to_string());
    }
    let ver_stdout = String::from_utf8_lossy(&ver_out.stdout);
    let norm_expected = expected_version.trim_start_matches('v');
    let expected_needle = format!("\"version\":\"{norm_expected}\"");
    if !ver_stdout.contains(&expected_needle) {
        return Err(format!(
            "staged bundle version does not match manifest ({expected_version})"
        ));
    }

    atomic_swap_bundle(target_app, &staged_app)?;

    // Best-effort refresh of macOS LaunchServices metadata.
    let _ = Command::new(
        "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister",
    )
    .args(["-f"])
    .arg(target_app)
    .status();

    Ok(())
}

#[cfg(target_os = "macos")]
fn codesign_team_id(app_path: &Path) -> Option<String> {
    let out = std::process::Command::new("/usr/bin/codesign")
        .args(["-dvv"])
        .arg(app_path)
        .output()
        .ok()?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    for line in stderr.lines() {
        if let Some(rest) = line.strip_prefix("TeamIdentifier=") {
            let t = rest.trim();
            if !t.is_empty() && t != "not set" {
                return Some(t.to_string());
            }
        }
    }
    None
}

/// Spawn a detached helper that waits for the current process to exit, then launches `app_bundle`.
#[cfg(target_os = "macos")]
pub fn schedule_macos_relaunch(app_bundle: &Path) -> Result<(), String> {
    let pid = std::process::id().to_string();
    std::process::Command::new("/bin/sh")
        .args([
            "-c",
            "while kill -0 \"$1\" 2>/dev/null; do sleep 0.1; done; /usr/bin/open -n \"$2\"",
            "_",
        ])
        .arg(&pid)
        .arg(app_bundle)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to schedule relaunch: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_nist_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        let million_a = vec![b'a'; 1_000_000];
        assert_eq!(
            sha256_hex(&million_a),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn manifest_parsing_validation_and_host_allowlist() {
        let sample = r#"{
            "version": "0.3.0",
            "build": 300,
            "notes_url": "https://github.com/builduseful/editor/releases/tag/v0.3.0",
            "extra_future_field": [1, true, {"nested": "ok"}],
            "assets": {
                "macos-universal": {
                    "url": "https://github.com/builduseful/editor/releases/download/v0.3.0/Editor-v0.3.0-macos-universal.tar.gz",
                    "sha256": "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD"
                },
                "windows-x86_64": {
                    "url": "https://objects.githubusercontent.com/builduseful/editor/v0.3.0/editor.zip",
                    "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
                }
            }
        }"#;
        let m = parse_release_manifest(sample).expect("valid manifest");
        assert_eq!(m.version, "0.3.0");
        assert_eq!(m.build, 300);
        assert_eq!(m.assets.len(), 2);

        let avail = evaluate_manifest(&m, "0.2.0", &["macos-universal"]).unwrap();
        match avail {
            UpdateCheckOutcome::Available(info) => {
                assert_eq!(info.version, "0.3.0");
                assert_eq!(
                    info.sha256,
                    "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
                );
            }
            other => panic!("expected Available, got {other:?}"),
        }

        let up_to_date = evaluate_manifest(&m, "0.3.0", &["macos-universal"]).unwrap();
        assert!(matches!(up_to_date, UpdateCheckOutcome::UpToDate { .. }));

        // Reject non-HTTPS, evil hosts, and userinfo tricks.
        assert!(validate_release_url("http://github.com/builduseful/editor").is_err());
        assert!(validate_release_url("https://evil.example/Editor.tar.gz").is_err());
        assert!(validate_release_url("https://github.com@evil.example/x").is_err());
        assert!(validate_release_url("https://notgithub.com/x").is_err());
        assert!(validate_release_url("https://github.com/builduseful/editor").is_ok());
        assert!(validate_release_url("https://objects.githubusercontent.com/abc").is_ok());
    }

    #[test]
    fn bundle_guards_and_atomic_swap_work_cleanly() {
        assert_eq!(
            enclosing_app_bundle(Path::new("/Applications/Editor.app/Contents/MacOS/editor")),
            Some(PathBuf::from("/Applications/Editor.app"))
        );
        assert_eq!(enclosing_app_bundle(Path::new("/usr/local/bin/editor")), None);
        assert!(is_app_translocated(Path::new(
            "/private/var/folders/xx/AppTranslocation/1234/d/Editor.app"
        )));
        assert!(is_dev_build_path(Path::new(
            "/mock/project/target/release/Editor.app"
        )));
        assert!(!is_dev_build_path(Path::new("/Applications/Editor.app")));

        assert!(should_auto_check(true, 0, AUTO_CHECK_INTERVAL_SECS));
        assert!(!should_auto_check(true, 1000, 1000 + AUTO_CHECK_INTERVAL_SECS - 1));
        assert!(!should_auto_check(false, 0, AUTO_CHECK_INTERVAL_SECS * 10));

        let root = std::env::temp_dir().join(format!("editor_swap_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let cur = root.join("Editor.app");
        let staged = root.join(".Editor.app.new");
        fs::create_dir_all(&cur).unwrap();
        fs::create_dir_all(&staged).unwrap();
        fs::write(cur.join("ver.txt"), "0.2.0").unwrap();
        fs::write(staged.join("ver.txt"), "0.3.0").unwrap();

        atomic_swap_bundle(&cur, &staged).unwrap();
        assert_eq!(fs::read_to_string(cur.join("ver.txt")).unwrap(), "0.3.0");
        assert!(!staged.exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn https_get_via_curl_enforces_url_allowlist_before_spawning() {
        assert!(!resolve_curl_binary().is_empty());
        assert!(https_get_via_curl("http://github.com/builduseful/editor", 1024).is_err());
        assert!(https_get_via_curl("file:///etc/passwd", 1024).is_err());
        assert!(https_get_via_curl("https://evil.example/release.json", 1024).is_err());
        assert!(https_get_via_curl("--config=/tmp/evil", 1024).is_err());
    }

    #[test]
    fn audit_regressions_json_utf16_surrogate_pairs() {
        let sample = r#"{
            "version": "0.4.1",
            "build": 401,
            "notes_url": "https://github.com/builduseful/editor/releases/tag/v0.4.1",
            "emoji_note": "rocket \uD83D\uDE80 launch",
            "assets": {
                "macos-universal": {
                    "url": "https://github.com/builduseful/editor/releases/download/v0.4.1/Editor.tar.gz",
                    "sha256": "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
                }
            }
        }"#;
        let m = parse_release_manifest(sample).expect("UTF-16 surrogate pair in JSON must parse");
        assert_eq!(m.version, "0.4.1");
        assert_eq!(m.build, 401);
    }
}
