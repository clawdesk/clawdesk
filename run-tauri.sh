#!/bin/bash

# ClawDesk Tauri Runner
# Builds the frontend and launches the Tauri desktop app in dev mode.

set -e

cd "$(dirname "$0")"
ROOT="$(pwd)"

echo ""
echo "=== ClawDesk Tauri Desktop App ==="
echo ""

# -- Cleanup ------------------------------------------------------------------
echo "[cleanup] Killing stale processes..."
lsof -ti :1420 2>/dev/null | xargs kill -9 2>/dev/null || true   # Vite dev server
pkill -f "clawdesk" 2>/dev/null || true
pkill -f "vite" 2>/dev/null || true
sleep 1
echo "[cleanup] Done"
echo ""

# -- Environment --------------------------------------------------------------
export CLAWDESK_CONFIG_PATH="$ROOT/clawdesk-config.toml"
echo "[config]  $CLAWDESK_CONFIG_PATH"
echo "[vite]    http://localhost:1420"
echo ""

# -- Install UI dependencies if needed ----------------------------------------
UI_DIR="$ROOT/crates/ui"
if [ ! -d "$UI_DIR/node_modules" ]; then
    echo "[npm] Installing UI dependencies..."
    (cd "$UI_DIR" && pnpm install)
    echo "[npm] Done"
    echo ""
fi

# -- Launch Tauri dev ----------------------------------------------------------
# Tauri will auto-start Vite via beforeDevCommand in tauri.conf.json
echo "[tauri] Building and running Tauri app..."
echo "        (Vite dev server starts automatically)"
echo ""

cd "$ROOT/crates/clawdesk-tauri"
cargo tauri dev
