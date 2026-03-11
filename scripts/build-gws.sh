#!/bin/bash
# ============================================================================
# Build Google Workspace CLI (gws) for ClawDesk distribution
# ============================================================================
#
# This script builds the `gws` binary from the local source checkout and
# places it in the ClawDesk tools/bundled/ directory for Tauri sidecar
# distribution.
#
# Original project: https://github.com/googleworkspace/cli
# License: Apache-2.0
# Author: Justin Poehnelt / Google LLC
#
# Usage:
#   ./scripts/build-gws.sh              # Build for current platform
#   ./scripts/build-gws.sh --release    # Release build
#
# The gws binary is shipped alongside ClawDesk with its original license
# and authorship intact. No modifications are made to the gws source code.
# ============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CLAWDESK_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
GWS_DIR="${GWS_SOURCE:-$CLAWDESK_ROOT/../cli}"
BUNDLED_DIR="$CLAWDESK_ROOT/tools/bundled"

# Verify gws source exists
if [ ! -f "$GWS_DIR/Cargo.toml" ]; then
    echo "ERROR: gws source not found at $GWS_DIR"
    echo "  Clone it: git clone https://github.com/googleworkspace/cli ../cli"
    echo "  Or set GWS_SOURCE=/path/to/cli"
    exit 1
fi

# Parse args
PROFILE="debug"
CARGO_FLAGS=""
if [[ "${1:-}" == "--release" ]]; then
    PROFILE="release"
    CARGO_FLAGS="--release"
fi

echo "Building gws (Google Workspace CLI) from: $GWS_DIR"
echo "  Profile: $PROFILE"
echo "  License: Apache-2.0 (Google LLC)"

# Build gws from its own project directory (no workspace modifications)
cd "$GWS_DIR"
cargo build $CARGO_FLAGS

# Copy binary to bundled tools directory
mkdir -p "$BUNDLED_DIR"
GWS_BIN="$GWS_DIR/target/$PROFILE/gws"

if [ ! -f "$GWS_BIN" ]; then
    echo "ERROR: gws binary not found at $GWS_BIN"
    exit 1
fi

cp "$GWS_BIN" "$BUNDLED_DIR/gws"
chmod +x "$BUNDLED_DIR/gws"

# Copy license alongside binary (Apache-2.0 requires this)
cp "$GWS_DIR/LICENSE" "$BUNDLED_DIR/GWS-LICENSE"

echo ""
echo "✅ gws binary built and placed at: $BUNDLED_DIR/gws"
echo "   License: $BUNDLED_DIR/GWS-LICENSE"
echo "   Version: $(grep '^version' "$GWS_DIR/Cargo.toml" | head -1)"
echo ""
echo "The gws binary ships alongside ClawDesk. Original authorship and"
echo "Apache-2.0 license by Google LLC / Justin Poehnelt remain intact."
