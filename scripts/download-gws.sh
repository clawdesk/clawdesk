#!/usr/bin/env bash
# download-gws.sh
#
# Builds or downloads the Google Workspace CLI (gws) binary and places it
# in the Tauri sidecar binaries directory with the correct platform-specific
# filename.
#
# Original project: https://github.com/googleworkspace/cli
# License: Apache-2.0 — Google LLC / Justin Poehnelt
#
# Usage:
#   ./scripts/download-gws.sh                            # Build from local source
#   ./scripts/download-gws.sh --target aarch64-apple-darwin
#   ./scripts/download-gws.sh --download                 # Download from GitHub Releases
#   ./scripts/download-gws.sh --download --version v0.11.1
#
# The binary will be placed at:
#   crates/clawdesk-tauri/binaries/gws-{target-triple}
#
# Tauri expects sidecar binaries named with the Rust target triple suffix.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
GWS_SOURCE="${GWS_SOURCE:-$ROOT/../cli}"
GWS_VERSION="${GWS_VERSION:-v0.11.1}"
BINARIES_DIR="$ROOT/crates/clawdesk-tauri/binaries"
MODE="build"  # "build" or "download"

# Parse CLI args
OVERRIDE_TARGET=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --target)   OVERRIDE_TARGET="$2"; shift 2;;
    --download) MODE="download"; shift;;
    --version)  GWS_VERSION="$2"; shift 2;;
    --source)   GWS_SOURCE="$2"; shift 2;;
    *)          echo "Unknown arg: $1"; exit 1;;
  esac
done

# Detect target triple
if [[ -n "$OVERRIDE_TARGET" ]]; then
  TARGET="$OVERRIDE_TARGET"
else
  ARCH="$(uname -m)"
  OS="$(uname -s)"
  case "$OS" in
    Darwin)
      case "$ARCH" in
        arm64)  TARGET="aarch64-apple-darwin" ;;
        x86_64) TARGET="x86_64-apple-darwin" ;;
        *)      echo "Unsupported arch: $ARCH"; exit 1 ;;
      esac ;;
    Linux)
      case "$ARCH" in
        x86_64)  TARGET="x86_64-unknown-linux-gnu" ;;
        aarch64) TARGET="aarch64-unknown-linux-gnu" ;;
        *)       echo "Unsupported arch: $ARCH"; exit 1 ;;
      esac ;;
    *)
      echo "Unsupported OS: $OS"; exit 1 ;;
  esac
fi

SIDECAR_NAME="gws-${TARGET}"
DEST="$BINARIES_DIR/$SIDECAR_NAME"
mkdir -p "$BINARIES_DIR"

echo "=== Google Workspace CLI (gws) ==="
echo "    License: Apache-2.0 (Google LLC / Justin Poehnelt)"
echo "    Target:  $TARGET"
echo "    Mode:    $MODE"

if [[ "$MODE" == "build" ]]; then
  # Build from local source
  if [[ ! -f "$GWS_SOURCE/Cargo.toml" ]]; then
    echo ""
    echo "ERROR: gws source not found at: $GWS_SOURCE"
    echo "  Clone it:  git clone https://github.com/googleworkspace/cli $GWS_SOURCE"
    echo "  Or use:    --download (fetch from GitHub Releases)"
    echo "  Or set:    GWS_SOURCE=/path/to/cli"
    exit 1
  fi

  echo "    Source:  $GWS_SOURCE"
  echo ""

  cd "$GWS_SOURCE"
  cargo build --release --target "$TARGET" 2>&1 | tail -5

  SRC="$GWS_SOURCE/target/$TARGET/release/gws"
  if [[ ! -f "$SRC" ]]; then
    echo "ERROR: Binary not found at $SRC"
    exit 1
  fi
  cp "$SRC" "$DEST"

elif [[ "$MODE" == "download" ]]; then
  # Download from GitHub Releases
  REPO="googleworkspace/cli"
  echo "    Version: $GWS_VERSION"
  echo ""

  # Map target to GitHub release asset name
  case "$TARGET" in
    aarch64-apple-darwin)       ASSET="gws-aarch64-apple-darwin.tar.gz" ;;
    x86_64-apple-darwin)        ASSET="gws-x86_64-apple-darwin.tar.gz" ;;
    x86_64-unknown-linux-gnu)   ASSET="gws-x86_64-unknown-linux-gnu.tar.gz" ;;
    aarch64-unknown-linux-gnu)  ASSET="gws-aarch64-unknown-linux-gnu.tar.gz" ;;
    x86_64-pc-windows-msvc)     ASSET="gws-x86_64-pc-windows-msvc.zip" ;;
    *)
      echo "ERROR: No pre-built binary for target: $TARGET"
      echo "  Use --source /path/to/cli to build from source instead"
      exit 1 ;;
  esac

  URL="https://github.com/$REPO/releases/download/$GWS_VERSION/$ASSET"
  echo "    URL: $URL"

  TMPDIR=$(mktemp -d)
  trap "rm -rf $TMPDIR" EXIT

  curl -fsSL "$URL" -o "$TMPDIR/$ASSET"

  if [[ "$ASSET" == *.tar.gz ]]; then
    tar -xzf "$TMPDIR/$ASSET" -C "$TMPDIR"
    cp "$TMPDIR/gws" "$DEST"
  elif [[ "$ASSET" == *.zip ]]; then
    unzip -o "$TMPDIR/$ASSET" -d "$TMPDIR"
    cp "$TMPDIR/gws.exe" "$DEST"
  fi
fi

chmod +x "$DEST"

# Copy license alongside (Apache-2.0 requires distribution of license)
if [[ -f "$GWS_SOURCE/LICENSE" ]]; then
  cp "$GWS_SOURCE/LICENSE" "$BINARIES_DIR/GWS-LICENSE"
fi

echo ""
echo "✅ gws sidecar placed at: $DEST"
echo "   Size: $(du -h "$DEST" | cut -f1)"
ls -la "$DEST"
