//! Minimal, portable platform adapter interfaces.
//!
//! This crate defines the ONLY boundaries between the portable Rust editor
//! (`editor-core`, `editor-render`, `editor-app`) and any host operating system.
//! It has zero third-party dependencies and includes pure-Rust headless/mock
//! implementations so the entire application can be tested and benchmarked
//! without an OS windowing server.

pub mod fs;
pub mod git;
pub mod host;
pub mod ignore;
pub mod input;
pub mod ipc;
pub mod raster;
pub mod surface;
pub mod update;
pub mod version;

pub use fs::{
    ensure_private_dir, set_private_file_permissions, DirEntry, EntryKind, FileHandle, FileLock,
    FileRev, FileStat, FileSystem, FsError, FsResult, MemoryFileSystem, StdFileSystem,
};
pub use git::{GitFileStatus, GitRepo, GitStatusCache, GitStatusMap};
pub use host::{
    default_app_state_dir, default_reveal_in_file_manager_label, display_path_string, Clipboard,
    HeadlessHost, HttpFetcher, MainLoopWaker, MemoryClipboard, PlatformHost,
};
pub use ignore::{
    collect_ancestor_gitignores, is_gitignored, matches_path_filter, GitIgnoreLayer, IgnoreRules,
};
pub use input::{
    AppCommand, ConfirmChoice, CursorShape, DragPhase, InputEvent, Key, Modifiers, MouseButton,
    SystemTheme, ThemePreference,
};
pub use raster::{
    BuiltinBitmapRasterizer, FontMetrics, FontStyle, GlyphFormat, GlyphRasterizer, RasterizedGlyph,
};
pub use surface::{DirtyRegion, Framebuffer, PixelRect, SurfaceMut};
pub use update::{
    check_manifest_bytes, enclosing_app_bundle, sha256_bytes, sha256_hex, ReleaseAsset,
    ReleaseManifest, UpdateCheckOutcome, UpdateInfo, RELEASE_MANIFEST_URL,
};
pub use version::{compare_semver, SemVer, APP_VERSION, BUNDLE_IDENTIFIER};
