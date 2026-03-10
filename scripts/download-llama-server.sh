#!/usr/bin/env bash
# download-llama-server.sh
#
# Downloads the correct prebuilt llama-server binary from ggml-org/llama.cpp
# releases and places it in the Tauri sidecar binaries directory with the
# correct platform-specific filename.
#
# Usage:
#   ./scripts/download-llama-server.sh [--version b8233] [--force]
#
# The binary will be placed at:
#   crates/clawdesk-tauri/binaries/llama-server-{target-triple}
#
# Tauri expects sidecar binaries named with the Rust target triple suffix.

set -euo pipefail

# ── Configuration ──────────────────────────────────────────────────────────
LLAMA_CPP_VERSION="${LLAMA_CPP_VERSION:-b8233}"
FORCE="${FORCE:-false}"
REPO="ggml-org/llama.cpp"

# Parse CLI args
OVERRIDE_TARGET=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --version) LLAMA_CPP_VERSION="$2"; shift 2;;
    --force)   FORCE=true; shift;;
    --target)  OVERRIDE_TARGET="$2"; shift 2;;
    *)         echo "Unknown arg: $1"; exit 1;;
  esac
done

# ── Detect platform ───────────────────────────────────────────────────────
# If --target is given (e.g. x86_64-apple-darwin), derive OS/ARCH from it
# instead of from the host. This supports cross-compilation in CI.
if [[ -n "${OVERRIDE_TARGET}" ]]; then
  case "${OVERRIDE_TARGET}" in
    aarch64-apple-darwin)
      OS="Darwin"; ARCH="arm64";;
    x86_64-apple-darwin)
      OS="Darwin"; ARCH="x86_64";;
    x86_64-unknown-linux-gnu)
      OS="Linux"; ARCH="x86_64";;
    aarch64-unknown-linux-gnu)
      OS="Linux"; ARCH="aarch64";;
    x86_64-pc-windows-msvc)
      OS="MINGW64_NT"; ARCH="x86_64";;
    aarch64-pc-windows-msvc)
      OS="MINGW64_NT"; ARCH="aarch64";;
    *)
      echo "ERROR: Unsupported --target: ${OVERRIDE_TARGET}"
      exit 1;;
  esac
else
  OS="$(uname -s)"
  ARCH="$(uname -m)"
fi

case "${OS}-${ARCH}" in
  Darwin-arm64)
    ASSET="llama-${LLAMA_CPP_VERSION}-bin-macos-arm64.tar.gz"
    TARGET_TRIPLE="aarch64-apple-darwin"
    ;;
  Darwin-x86_64)
    ASSET="llama-${LLAMA_CPP_VERSION}-bin-macos-x64.tar.gz"
    TARGET_TRIPLE="x86_64-apple-darwin"
    ;;
  Linux-x86_64)
    # Use Vulkan build for GPU support across NVIDIA + AMD
    ASSET="llama-${LLAMA_CPP_VERSION}-bin-ubuntu-vulkan-x64.tar.gz"
    TARGET_TRIPLE="x86_64-unknown-linux-gnu"
    ;;
  Linux-aarch64)
    ASSET="llama-${LLAMA_CPP_VERSION}-bin-ubuntu-x64.tar.gz"
    TARGET_TRIPLE="aarch64-unknown-linux-gnu"
    ;;
  MINGW*|MSYS*|CYGWIN*)
    # Windows via Git Bash / MSYS2
    ASSET="llama-${LLAMA_CPP_VERSION}-bin-win-vulkan-x64.zip"
    TARGET_TRIPLE="x86_64-pc-windows-msvc"
    ;;
  *)
    echo "ERROR: Unsupported platform: ${OS}-${ARCH}"
    exit 1
    ;;
esac

# ── Paths ─────────────────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
BINARIES_DIR="${PROJECT_ROOT}/crates/clawdesk-tauri/binaries"

EXT=""
[[ "${OS}" == MINGW* || "${OS}" == MSYS* || "${OS}" == CYGWIN* ]] && EXT=".exe"
TARGET_PATH="${BINARIES_DIR}/llama-server-${TARGET_TRIPLE}${EXT}"

# ── Check if already present ──────────────────────────────────────────────
if [[ -f "${TARGET_PATH}" && "${FORCE}" != "true" ]]; then
  echo "✓ llama-server already exists at ${TARGET_PATH}"
  echo "  Use --force to re-download."
  exit 0
fi

# ── Download ──────────────────────────────────────────────────────────────
DOWNLOAD_URL="https://github.com/${REPO}/releases/download/${LLAMA_CPP_VERSION}/${ASSET}"
TMP_DIR="$(mktemp -d)"

echo "╔══════════════════════════════════════════════════════════════════╗"
echo "║  Downloading llama-server (${LLAMA_CPP_VERSION})                         ║"
echo "╚══════════════════════════════════════════════════════════════════╝"
echo ""
echo "  Platform:  ${OS}-${ARCH}"
echo "  Triple:    ${TARGET_TRIPLE}"
echo "  Asset:     ${ASSET}"
echo "  URL:       ${DOWNLOAD_URL}"
echo ""

curl -fSL --progress-bar "${DOWNLOAD_URL}" -o "${TMP_DIR}/${ASSET}"

# ── Extract ───────────────────────────────────────────────────────────────
echo "Extracting..."
if [[ "${ASSET}" == *.tar.gz ]]; then
  tar xzf "${TMP_DIR}/${ASSET}" -C "${TMP_DIR}"
elif [[ "${ASSET}" == *.zip ]]; then
  unzip -q -o "${TMP_DIR}/${ASSET}" -d "${TMP_DIR}"
fi

# ── Find llama-server in extracted files ──────────────────────────────────
FOUND=""
while IFS= read -r -d '' f; do
  BASENAME="$(basename "$f")"
  if [[ "${BASENAME}" == "llama-server" || "${BASENAME}" == "llama-server.exe" ]]; then
    FOUND="$f"
    break
  fi
done < <(find "${TMP_DIR}" -type f -print0)

if [[ -z "${FOUND}" ]]; then
  echo "ERROR: llama-server binary not found in archive"
  rm -rf "${TMP_DIR}"
  exit 1
fi

# ── Install ───────────────────────────────────────────────────────────────
mkdir -p "${BINARIES_DIR}"
cp "${FOUND}" "${TARGET_PATH}"
chmod +x "${TARGET_PATH}"

# Also copy shared libraries that llama-server may need at runtime
# (e.g., libllama.dylib on macOS, libggml*.dylib, etc.)
# Libraries must be in the same directory as the binary for @rpath resolution
FOUND_DIR="$(dirname "${FOUND}")"

for lib in "${FOUND_DIR}"/*.dylib "${FOUND_DIR}"/*.so "${FOUND_DIR}"/*.so.* "${FOUND_DIR}"/*.dll; do
  [[ -f "$lib" ]] && cp "$lib" "${BINARIES_DIR}/" && echo "  Copied lib: $(basename "$lib")"
done

# ── Cleanup ───────────────────────────────────────────────────────────────
rm -rf "${TMP_DIR}"

# ── Verify ────────────────────────────────────────────────────────────────
SIZE=$(du -h "${TARGET_PATH}" | cut -f1)
echo ""
echo "╔══════════════════════════════════════════════════════════════════╗"
echo "║  ✓ llama-server installed successfully                         ║"
echo "╚══════════════════════════════════════════════════════════════════╝"
echo ""
echo "  Path:  ${TARGET_PATH}"
echo "  Size:  ${SIZE}"
echo "  Triple: ${TARGET_TRIPLE}"
echo ""

# Write a version marker for tracking
echo "${LLAMA_CPP_VERSION}" > "${BINARIES_DIR}/.llama-server-version"
