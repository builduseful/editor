#!/bin/sh
# Minimal, self-contained macOS installer and uninstaller for editor.
#
# Everything lives inside one Editor.app bundle:
#   Editor.app/Contents/MacOS/editor-gui   (GUI)
#   Editor.app/Contents/MacOS/editor       (CLI)
#   Editor.app/Contents/Resources/AppIcon.icns
#   Editor.app/Contents/Info.plist
#
# The CLI in PATH is a single symlink pointing into Editor.app/Contents/MacOS/editor,
# so GUI and CLI stay version-locked with zero scattered files on disk.
#
# Works both inside a local git checkout (builds from source) and standalone via:
#   curl -fsSL https://raw.githubusercontent.com/builduseful/editor/main/scripts/install-macos.sh | sh

set -eu

REPO="builduseful/editor"
BUNDLE_ID="com.builduseful.editor"
APP_NAME="Editor.app"
BIN_NAME="editor"

MODE="install"
USER_INSTALL=0
NO_BUILD=0
FROM_RELEASE=0
RELEASE_TAG=""
NO_CLI=0
PURGE=0
FORCE=0
APP_DIR=""
BIN_DIR=""
SRC_APP=""

usage() {
    cat <<'EOF'
Usage: scripts/install-macos.sh [OPTIONS]

Install or remove editor on macOS with zero scattered files.

Options:
  --user              Install to ~/Applications and ~/.local/bin (no sudo required)
  --app-dir DIR       Target directory for Editor.app (default: /Applications)
  --bin-dir DIR       Target directory for the `editor` symlink (default: /usr/local/bin)
  --from-release      Download and verify the latest prebuilt GitHub Release
  --tag TAG           With --from-release, install a specific release tag (e.g. v0.2.0)
  --app PATH          Install an existing Editor.app bundle (implies --no-build)
  --no-build          Skip `cargo build` and install target/release/Editor.app
  --no-cli            Do not create or remove the `editor` CLI symlink
  --force             Replace an existing non-editor file at the CLI symlink path
  --uninstall         Remove Editor.app and its `editor` symlink
  --purge             With --uninstall, also remove saved preferences, backups, and IPC socket
  -h, --help          Show this help message
EOF
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --user) USER_INSTALL=1 ;;
        --app-dir)
            [ "$#" -ge 2 ] || { echo "error: --app-dir requires a path" >&2; exit 2; }
            APP_DIR="$2"
            shift
            ;;
        --bin-dir)
            [ "$#" -ge 2 ] || { echo "error: --bin-dir requires a path" >&2; exit 2; }
            BIN_DIR="$2"
            shift
            ;;
        --from-release) FROM_RELEASE=1 ;;
        --tag)
            [ "$#" -ge 2 ] || { echo "error: --tag requires a release tag" >&2; exit 2; }
            FROM_RELEASE=1
            RELEASE_TAG="$2"
            shift
            ;;
        --app)
            [ "$#" -ge 2 ] || { echo "error: --app requires a path" >&2; exit 2; }
            SRC_APP="$2"
            NO_BUILD=1
            shift
            ;;
        --no-build) NO_BUILD=1 ;;
        --no-cli) NO_CLI=1 ;;
        --force) FORCE=1 ;;
        --uninstall) MODE="uninstall" ;;
        --purge) PURGE=1 ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "error: unknown option: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
    shift
done

if [ "$(uname -s)" != "Darwin" ]; then
    echo "error: scripts/install-macos.sh is for macOS only" >&2
    exit 1
fi

if [ "$USER_INSTALL" -eq 1 ]; then
    [ -n "$APP_DIR" ] || APP_DIR="$HOME/Applications"
    [ -n "$BIN_DIR" ] || BIN_DIR="$HOME/.local/bin"
else
    [ -n "$APP_DIR" ] || APP_DIR="/Applications"
    [ -n "$BIN_DIR" ] || BIN_DIR="/usr/local/bin"
fi

# Detect whether running inside a local source checkout or standalone (e.g. `curl | sh`).
REPO_ROOT=""
if [ -n "${0:-}" ] && [ -f "$0" ]; then
    SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
    CANDIDATE_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
    if [ -f "$CANDIDATE_ROOT/Cargo.toml" ] && [ -d "$CANDIDATE_ROOT/crates/editor-macos" ]; then
        REPO_ROOT="$CANDIDATE_ROOT"
    fi
fi

if [ -z "$REPO_ROOT" ] && [ "$MODE" = "install" ] && [ -z "$SRC_APP" ]; then
    FROM_RELEASE=1
fi

DEST_APP="$APP_DIR/$APP_NAME"
LINK_PATH="$BIN_DIR/$BIN_NAME"
CLI_INSIDE_APP="$DEST_APP/Contents/MacOS/$BIN_NAME"

# Run a command directly if the target path (or its nearest existing parent) is writable;
# escalate with sudo only when genuinely required.
need_sudo_for() {
    p="$1"
    while [ ! -e "$p" ] && [ "$p" != "/" ]; do
        p=$(dirname -- "$p")
    done
    [ ! -w "$p" ]
}

run_priv() {
    target="$1"
    shift
    if need_sudo_for "$target"; then
        sudo "$@"
    else
        "$@"
    fi
}

is_our_symlink() {
    [ -L "$1" ] || return 1
    target=$(readlink "$1" 2>/dev/null || true)
    case "$target" in
        *"/$APP_NAME/Contents/MacOS/$BIN_NAME") return 0 ;;
        *) return 1 ;;
    esac
}

do_uninstall() {
    removed_any=0

    if [ "$NO_CLI" -eq 0 ]; then
        if is_our_symlink "$LINK_PATH"; then
            run_priv "$BIN_DIR" rm -f "$LINK_PATH"
            echo "Removed symlink $LINK_PATH"
            removed_any=1
        elif [ -e "$LINK_PATH" ] || [ -L "$LINK_PATH" ]; then
            echo "Skipped $LINK_PATH (does not point to $APP_NAME)"
        fi
    fi

    if [ -d "$DEST_APP" ]; then
        run_priv "$APP_DIR" rm -rf "$DEST_APP"
        echo "Removed $DEST_APP"
        removed_any=1
    fi

    if [ "$PURGE" -eq 1 ]; then
        defaults delete "$BUNDLE_ID" >/dev/null 2>&1 || true
        rm -rf "$HOME/Library/Application Support/$BUNDLE_ID" 2>/dev/null || true
        uid=$(id -u)
        rm -f "${TMPDIR:-/tmp}/editor-ipc-${uid}.sock" "/tmp/editor-ipc-${uid}.sock" 2>/dev/null || true
        echo "Cleared preferences ($BUNDLE_ID), app state, and IPC socket"
        removed_any=1
    fi

    if [ "$removed_any" -eq 0 ]; then
        echo "Nothing to remove at $DEST_APP"
    fi
}

STAGING_APP=""
PREV_APP=""
DL_TMP=""
cleanup() {
    if [ -n "$PREV_APP" ] && [ -d "$PREV_APP" ] && [ ! -d "$DEST_APP" ]; then
        run_priv "$APP_DIR" mv "$PREV_APP" "$DEST_APP" 2>/dev/null || true
    fi
    if [ -n "$STAGING_APP" ] && [ -d "$STAGING_APP" ]; then
        run_priv "$APP_DIR" rm -rf "$STAGING_APP" 2>/dev/null || true
    fi
    if [ -n "$DL_TMP" ] && [ -d "$DL_TMP" ]; then
        rm -rf "$DL_TMP" 2>/dev/null || true
    fi
}
trap cleanup EXIT INT TERM

download_release_app() {
    if [ -n "$RELEASE_TAG" ]; then
        MANIFEST_URL="https://github.com/${REPO}/releases/download/${RELEASE_TAG}/release.json"
    else
        MANIFEST_URL="https://github.com/${REPO}/releases/latest/download/release.json"
    fi

    DL_TMP=$(mktemp -d "${TMPDIR:-/tmp}/editor-install.XXXXXX")
    MANIFEST_FILE="$DL_TMP/release.json"

    echo "Fetching release manifest..."
    /usr/bin/curl -fsSL --proto =https --max-time 20 "$MANIFEST_URL" -o "$MANIFEST_FILE"

    # Extract macos-universal URL and SHA-256 using macOS's built-in Python 3 or plutil/awk.
    ARCH=$(uname -m)
    PARSED=$(/usr/bin/python3 - "$MANIFEST_FILE" "$ARCH" <<'PY'
import json, sys
with open(sys.argv[1], "r", encoding="utf-8") as f:
    m = json.load(f)
arch = sys.argv[2]
assets = m.get("assets", {})
arch_key = "macos-arm64" if arch == "arm64" else "macos-x86_64"
a = assets.get("macos-universal") or assets.get(arch_key)
if not a or "url" not in a or "sha256" not in a:
    sys.exit("release manifest missing macOS asset")
url = a["url"]
sha = a["sha256"].strip().lower()
ver = m.get("version", "")
if not url.startswith("https://github.com/") and not url.startswith("https://objects.githubusercontent.com/"):
    sys.exit("disallowed asset URL host")
print(ver)
print(url)
print(sha)
PY
)
    REL_VER=$(printf '%s\n' "$PARSED" | sed -n '1p')
    ASSET_URL=$(printf '%s\n' "$PARSED" | sed -n '2p')
    EXPECTED_SHA=$(printf '%s\n' "$PARSED" | sed -n '3p')

    ARCHIVE="$DL_TMP/editor.tar.gz"
    echo "Downloading editor v${REL_VER}..."
    /usr/bin/curl -fsSL --proto =https --max-time 60 "$ASSET_URL" -o "$ARCHIVE"

    ACTUAL_SHA=$(/usr/bin/shasum -a 256 "$ARCHIVE" | awk '{print $1}')
    if [ "$ACTUAL_SHA" != "$EXPECTED_SHA" ]; then
        echo "error: SHA-256 verification failed (expected $EXPECTED_SHA, got $ACTUAL_SHA)" >&2
        exit 1
    fi

    /usr/bin/tar -xzf "$ARCHIVE" -C "$DL_TMP"
    SRC_APP="$DL_TMP/$APP_NAME"
    /usr/bin/codesign --verify --deep --strict "$SRC_APP"
}

do_install() {
    if [ "$FROM_RELEASE" -eq 1 ]; then
        download_release_app
    elif [ "$NO_BUILD" -eq 0 ]; then
        if [ -z "$REPO_ROOT" ]; then
            echo "error: not inside an editor checkout (pass --from-release or --app PATH)" >&2
            exit 1
        fi
        echo "Building release binaries..."
        (
            cd "$REPO_ROOT"
            cargo build --release --workspace --bins
            ./target/release/bundle-app
        )
    fi

    if [ -z "$SRC_APP" ]; then
        SRC_APP="$REPO_ROOT/target/release/$APP_NAME"
    fi

    if [ ! -d "$SRC_APP" ] || [ ! -x "$SRC_APP/Contents/MacOS/editor-gui" ]; then
        echo "error: valid $APP_NAME not found at $SRC_APP" >&2
        exit 1
    fi

    run_priv "$APP_DIR" mkdir -p "$APP_DIR"
    APP_DIR=$(cd "$APP_DIR" && pwd)
    DEST_APP="$APP_DIR/$APP_NAME"
    CLI_INSIDE_APP="$DEST_APP/Contents/MacOS/$BIN_NAME"

    # Stage inside the same filesystem as $APP_DIR so the swap is atomic.
    STAGING_APP="$APP_DIR/.${APP_NAME}.staging.$$"
    PREV_APP="$APP_DIR/.${APP_NAME}.prev.$$"

    run_priv "$APP_DIR" rm -rf "$STAGING_APP" "$PREV_APP"
    run_priv "$APP_DIR" ditto "$SRC_APP" "$STAGING_APP"

    if [ -d "$DEST_APP" ]; then
        run_priv "$APP_DIR" mv "$DEST_APP" "$PREV_APP"
    fi

    if run_priv "$APP_DIR" mv "$STAGING_APP" "$DEST_APP"; then
        STAGING_APP=""
        if [ -d "$PREV_APP" ]; then
            run_priv "$APP_DIR" rm -rf "$PREV_APP"
        fi
    else
        # Restore previous installation if the swap failed.
        if [ -d "$PREV_APP" ] && [ ! -d "$DEST_APP" ]; then
            run_priv "$APP_DIR" mv "$PREV_APP" "$DEST_APP" || true
        fi
        echo "error: failed to install $DEST_APP" >&2
        exit 1
    fi

    echo "Installed $DEST_APP"

    if [ "$NO_CLI" -eq 0 ]; then
        if [ ! -x "$CLI_INSIDE_APP" ]; then
            echo "warning: bundled CLI not found at $CLI_INSIDE_APP; skipping symlink" >&2
            return 0
        fi
        run_priv "$BIN_DIR" mkdir -p "$BIN_DIR"
        if [ -e "$LINK_PATH" ] || [ -L "$LINK_PATH" ]; then
            if is_our_symlink "$LINK_PATH" || [ "$FORCE" -eq 1 ]; then
                run_priv "$BIN_DIR" rm -f "$LINK_PATH"
            else
                echo "error: $LINK_PATH already exists and does not point to $APP_NAME (pass --force or --bin-dir)" >&2
                exit 1
            fi
        fi
        run_priv "$BIN_DIR" ln -s "$CLI_INSIDE_APP" "$LINK_PATH"
        echo "Linked $LINK_PATH -> $CLI_INSIDE_APP"

        case ":${PATH:-}:" in
            *":$BIN_DIR:"*) ;;
            *)
                echo "Note: add $BIN_DIR to your PATH to invoke \`$BIN_NAME\` from the terminal."
                ;;
        esac
    fi
}

case "$MODE" in
    install) do_install ;;
    uninstall) do_uninstall ;;
esac
