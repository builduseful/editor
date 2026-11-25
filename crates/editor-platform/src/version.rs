//! Single-source-of-truth application versioning derived from `[workspace.package] version`
//! in the workspace `Cargo.toml`.
//!
//! Every binary (`editor`, `editor-gui`, `bundle-app`), `Info.plist`, and future updater
//! reads version metadata from here so versions can never drift across artifacts.

use std::cmp::Ordering;

/// Semantic version string compiled from the workspace `Cargo.toml` (e.g. `"0.2.0"`).
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Canonical reverse-DNS application bundle identifier.
pub const BUNDLE_IDENTIFIER: &str = "com.builduseful.editor";

/// Parsed `MAJOR.MINOR.PATCH` semantic version with optional pre-release tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemVer<'a> {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    pub pre: Option<&'a str>,
}

impl<'a> SemVer<'a> {
    /// Parse a SemVer string, tolerating an optional leading `'v'` or `'V'`
    /// and stripping any `+<build>` metadata.
    pub fn parse(raw: &'a str) -> Option<Self> {
        let trimmed = raw.trim();
        let no_v = trimmed
            .strip_prefix('v')
            .or_else(|| trimmed.strip_prefix('V'))
            .unwrap_or(trimmed);
        let no_meta = no_v.split('+').next()?;
        let (core, pre) = match no_meta.split_once('-') {
            Some((c, p)) if !p.is_empty() => (c, Some(p)),
            Some(_) => return None,
            None => (no_meta, None),
        };
        let mut parts = core.split('.');
        let major = parts.next()?.parse::<u32>().ok()?;
        let minor = parts.next()?.parse::<u32>().ok()?;
        let patch = parts.next()?.parse::<u32>().ok()?;
        if parts.next().is_some() {
            return None;
        }
        Some(Self {
            major,
            minor,
            patch,
            pre,
        })
    }

    /// Monotonically increasing integer build number (`MAJOR * 10000 + MINOR * 100 + PATCH`).
    ///
    /// Suitable for macOS `CFBundleVersion` without maintaining a separate build counter file.
    pub fn build_number(&self) -> u32 {
        self.major
            .saturating_mul(10_000)
            .saturating_add(self.minor.saturating_mul(100))
            .saturating_add(self.patch)
            .max(1)
    }
}

impl Ord for SemVer<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (self.pre, other.pre) {
                (None, None) => Ordering::Equal,
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (Some(a), Some(b)) => compare_prerelease(a, b),
            })
    }
}

impl PartialOrd for SemVer<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn compare_prerelease(a: &str, b: &str) -> Ordering {
    let mut ia = a.split('.');
    let mut ib = b.split('.');
    loop {
        match (ia.next(), ib.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(pa), Some(pb)) => {
                let ord = match (pa.parse::<u64>(), pb.parse::<u64>()) {
                    (Ok(na), Ok(nb)) => na.cmp(&nb),
                    (Ok(_), Err(_)) => Ordering::Less,
                    (Err(_), Ok(_)) => Ordering::Greater,
                    (Err(_), Err(_)) => pa.cmp(pb),
                };
                if ord != Ordering::Equal {
                    return ord;
                }
            }
        }
    }
}

/// Compare two semantic version strings (e.g. `"0.2.0"` vs `"v0.3.0"`).
pub fn compare_semver(a: &str, b: &str) -> Option<Ordering> {
    Some(SemVer::parse(a)?.cmp(&SemVer::parse(b)?))
}

/// Build number derived from [`APP_VERSION`].
pub fn build_number() -> u32 {
    SemVer::parse(APP_VERSION)
        .map(|v| v.build_number())
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_orders_semver_cleanly() {
        let cur = SemVer::parse(APP_VERSION).expect("workspace version must be valid SemVer");
        assert!(cur.build_number() >= 1);

        assert_eq!(
            SemVer::parse("v0.2.0").unwrap().build_number(),
            200
        );
        assert_eq!(
            SemVer::parse("1.4.12+abc").unwrap().build_number(),
            10_412
        );
        assert_eq!(compare_semver("0.2.0", "v0.10.0"), Some(Ordering::Less));
        assert_eq!(compare_semver("0.3.0-rc.1", "0.3.0"), Some(Ordering::Less));
        assert_eq!(
            compare_semver("0.3.0-rc.2", "0.3.0-rc.10"),
            Some(Ordering::Less)
        );
        assert_eq!(compare_semver("0.2.0", "v0.2.0"), Some(Ordering::Equal));
        assert_eq!(SemVer::parse("not-a-version"), None);
    }
}
