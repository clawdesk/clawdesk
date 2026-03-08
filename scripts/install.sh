#!/bin/sh
# ClawDesk — Universal Install Script
#
# Usage:
#   curl -fsSL https://get.clawdesk.dev | sh
#
# This script:
#   1. Detects platform (OS + architecture)
#   2. Downloads the pre-built binary from GitHub Releases
#   3. Verifies SHA-256 checksum
#   4. Installs to /usr/local/bin (or ~/.local/bin if no permission)
#   5. Registers the background daemon (launchd/systemd)
#   6. Starts the daemon
#   7. Runs diagnostics
#
# Environment variables:
#   CLAWDESK_VERSION   — Pin a specific version (default: latest)
#   CLAWDESK_INSTALL   — Custom install directory
#   CLAWDESK_NO_DAEMON — Skip daemon installation (set to 1)
#   CLAWDESK_NO_MODIFY_PATH — Skip PATH modification (set to 1)

set -e

# ---- Constants ---------------------------------------------------------------

REPO="clawdesk/clawdesk"
BINARY_NAME="clawdesk"
BASE_URL="https://github.com/${REPO}/releases"

# Colors (disabled if not a terminal).
if [ -t 1 ]; then
    RED='\033[0;31m'
    GREEN='\033[0;32m'
    YELLOW='\033[0;33m'
    BLUE='\033[0;34m'
    BOLD='\033[1m'
    RESET='\033[0m'
else
    RED='' GREEN='' YELLOW='' BLUE='' BOLD='' RESET=''
fi

# ---- Utility Functions -------------------------------------------------------

info() {
    printf "${BLUE}info${RESET}  %s\n" "$1"
}

success() {
    printf "${GREEN}  ✓${RESET}  %s\n" "$1"
}

warn() {
    printf "${YELLOW}warn${RESET}  %s\n" "$1"
}

error() {
    printf "${RED}error${RESET} %s\n" "$1" >&2
}

die() {
    error "$1"
    exit 1
}

need_cmd() {
    if ! command -v "$1" > /dev/null 2>&1; then
        die "Required command not found: $1"
    fi
}

# ---- Platform Detection ------------------------------------------------------

detect_platform() {
    OS="$(uname -s)"
    ARCH="$(uname -m)"

    case "$OS" in
        Darwin)  PLATFORM="darwin" ;;
        Linux)   PLATFORM="linux" ;;
        MINGW*|MSYS*|CYGWIN*) PLATFORM="windows" ;;
        *)       die "Unsupported operating system: $OS" ;;
    esac

    case "$ARCH" in
        x86_64|amd64)   ARCH="amd64" ;;
        aarch64|arm64)  ARCH="arm64" ;;
        *)              die "Unsupported architecture: $ARCH" ;;
    esac

    TARGET="${PLATFORM}-${ARCH}"
    info "Detected platform: ${BOLD}${TARGET}${RESET}"
}

# ---- Version Resolution ------------------------------------------------------

resolve_version() {
    if [ -n "$CLAWDESK_VERSION" ]; then
        VERSION="$CLAWDESK_VERSION"
        info "Using pinned version: ${BOLD}${VERSION}${RESET}"
        return
    fi

    info "Resolving latest version..."

    if command -v curl > /dev/null 2>&1; then
        VERSION=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
            | grep '"tag_name"' | head -1 | sed 's/.*"v\?\([^"]*\)".*/\1/')
    elif command -v wget > /dev/null 2>&1; then
        VERSION=$(wget -qO- "https://api.github.com/repos/${REPO}/releases/latest" \
            | grep '"tag_name"' | head -1 | sed 's/.*"v\?\([^"]*\)".*/\1/')
    else
        die "Neither curl nor wget found — cannot download"
    fi

    if [ -z "$VERSION" ]; then
        # Fallback: use hard-coded version.
        VERSION="0.1.0"
        warn "Could not resolve latest version, using ${VERSION}"
    fi

    info "Latest version: ${BOLD}${VERSION}${RESET}"
}

# ---- Download ----------------------------------------------------------------

download_binary() {
    BINARY_FILE="${BINARY_NAME}-${TARGET}"
    if [ "$PLATFORM" = "windows" ]; then
        BINARY_FILE="${BINARY_FILE}.exe"
    fi

    DOWNLOAD_URL="${BASE_URL}/download/v${VERSION}/${BINARY_FILE}"
    CHECKSUM_URL="${BASE_URL}/download/v${VERSION}/checksums.sha256"

    TMPDIR=$(mktemp -d)
    trap 'rm -rf "$TMPDIR"' EXIT

    info "Downloading ${BOLD}${BINARY_FILE}${RESET}..."

    if command -v curl > /dev/null 2>&1; then
        curl -fsSL -o "${TMPDIR}/${BINARY_FILE}" "$DOWNLOAD_URL" || \
            die "Download failed: ${DOWNLOAD_URL}"
        curl -fsSL -o "${TMPDIR}/checksums.sha256" "$CHECKSUM_URL" 2>/dev/null || true
    elif command -v wget > /dev/null 2>&1; then
        wget -q -O "${TMPDIR}/${BINARY_FILE}" "$DOWNLOAD_URL" || \
            die "Download failed: ${DOWNLOAD_URL}"
        wget -q -O "${TMPDIR}/checksums.sha256" "$CHECKSUM_URL" 2>/dev/null || true
    fi

    success "Downloaded $(du -h "${TMPDIR}/${BINARY_FILE}" | awk '{print $1}')"

    # Verify SHA-256 checksum if available.
    if [ -f "${TMPDIR}/checksums.sha256" ]; then
        EXPECTED=$(grep "${BINARY_FILE}" "${TMPDIR}/checksums.sha256" | awk '{print $1}')
        if [ -n "$EXPECTED" ]; then
            if command -v sha256sum > /dev/null 2>&1; then
                ACTUAL=$(sha256sum "${TMPDIR}/${BINARY_FILE}" | awk '{print $1}')
            elif command -v shasum > /dev/null 2>&1; then
                ACTUAL=$(shasum -a 256 "${TMPDIR}/${BINARY_FILE}" | awk '{print $1}')
            else
                warn "No SHA-256 tool found — skipping verification"
                ACTUAL=""
            fi

            if [ -n "$ACTUAL" ]; then
                if [ "$ACTUAL" = "$EXPECTED" ]; then
                    success "SHA-256 checksum verified"
                else
                    die "Checksum mismatch!\n  Expected: ${EXPECTED}\n  Actual:   ${ACTUAL}"
                fi
            fi
        else
            warn "No checksum entry found for ${BINARY_FILE}"
        fi
    else
        warn "Checksum file not available — skipping verification"
    fi
}

# ---- Install Binary ----------------------------------------------------------

install_binary() {
    # Determine install location.
    if [ -n "$CLAWDESK_INSTALL" ]; then
        INSTALL_DIR="$CLAWDESK_INSTALL"
    elif [ -w "/usr/local/bin" ]; then
        INSTALL_DIR="/usr/local/bin"
    else
        INSTALL_DIR="${HOME}/.local/bin"
        mkdir -p "$INSTALL_DIR"
    fi

    INSTALL_PATH="${INSTALL_DIR}/${BINARY_NAME}"

    # Check for existing installation — upgrade path.
    if [ -f "$INSTALL_PATH" ]; then
        EXISTING_VERSION=$("$INSTALL_PATH" --version 2>/dev/null | awk '{print $NF}' || echo "unknown")
        info "Upgrading from ${EXISTING_VERSION} to ${VERSION}"
        # Backup current binary for rollback.
        cp "$INSTALL_PATH" "${INSTALL_PATH}.bak" 2>/dev/null || true
    fi

    # Atomic install: copy to temp, then rename (atomic on same filesystem).
    cp "${TMPDIR}/${BINARY_FILE}" "${INSTALL_DIR}/.clawdesk.tmp"
    chmod +x "${INSTALL_DIR}/.clawdesk.tmp"
    mv "${INSTALL_DIR}/.clawdesk.tmp" "$INSTALL_PATH"

    success "Installed to ${BOLD}${INSTALL_PATH}${RESET}"

    # Ensure it's in PATH.
    if ! echo "$PATH" | tr ':' '\n' | grep -q "^${INSTALL_DIR}$"; then
        if [ -z "$CLAWDESK_NO_MODIFY_PATH" ]; then
            add_to_path "$INSTALL_DIR"
        else
            warn "${INSTALL_DIR} is not in PATH — add it manually"
        fi
    fi
}

# ---- PATH Setup --------------------------------------------------------------

add_to_path() {
    local dir="$1"
    local profile=""

    # Detect shell profile.
    if [ -n "$ZSH_VERSION" ] || [ -f "$HOME/.zshrc" ]; then
        profile="$HOME/.zshrc"
    elif [ -n "$BASH_VERSION" ] || [ -f "$HOME/.bashrc" ]; then
        profile="$HOME/.bashrc"
    elif [ -f "$HOME/.profile" ]; then
        profile="$HOME/.profile"
    fi

    if [ -n "$profile" ]; then
        if ! grep -q "clawdesk" "$profile" 2>/dev/null; then
            printf '\n# ClawDesk\nexport PATH="%s:$PATH"\n' "$dir" >> "$profile"
            success "Added ${dir} to PATH in ${profile}"
            info "Run: ${BOLD}source ${profile}${RESET} or open a new terminal"
        fi
    fi

    # Also export for current session.
    export PATH="${dir}:${PATH}"
}

# ---- Shell Completions -------------------------------------------------------

install_completions() {
    if command -v "$BINARY_NAME" > /dev/null 2>&1; then
        if [ -n "$ZSH_VERSION" ] || [ -f "$HOME/.zshrc" ]; then
            local comp_dir="${HOME}/.zfunc"
            mkdir -p "$comp_dir"
            "$BINARY_NAME" completions zsh > "${comp_dir}/_clawdesk" 2>/dev/null && \
                success "Installed zsh completions" || true
        elif [ -n "$BASH_VERSION" ] || [ -f "$HOME/.bashrc" ]; then
            local comp_dir="${HOME}/.local/share/bash-completion/completions"
            mkdir -p "$comp_dir"
            "$BINARY_NAME" completions bash > "${comp_dir}/clawdesk" 2>/dev/null && \
                success "Installed bash completions" || true
        fi

        if [ -d "$HOME/.config/fish" ]; then
            local comp_dir="$HOME/.config/fish/completions"
            mkdir -p "$comp_dir"
            "$BINARY_NAME" completions fish > "${comp_dir}/clawdesk.fish" 2>/dev/null && \
                success "Installed fish completions" || true
        fi
    fi
}

# ---- Daemon Registration -----------------------------------------------------

install_daemon() {
    if [ -n "$CLAWDESK_NO_DAEMON" ]; then
        info "Skipping daemon installation (CLAWDESK_NO_DAEMON set)"
        return
    fi

    info "Installing background daemon..."
    "$BINARY_NAME" daemon install 2>/dev/null && \
        success "Daemon service registered" || \
        warn "Daemon installation skipped (run 'clawdesk daemon install' manually)"

    info "Starting daemon..."
    "$BINARY_NAME" daemon start 2>/dev/null && \
        success "Daemon started" || \
        warn "Daemon start failed (run 'clawdesk daemon start' manually)"
}

# ---- Diagnostics -------------------------------------------------------------

run_doctor() {
    info "Running diagnostics..."
    "$BINARY_NAME" doctor 2>/dev/null || true
}

# ---- Main --------------------------------------------------------------------

main() {
    printf "\n${BOLD}ClawDesk Installer${RESET}\n"
    printf "==================\n\n"

    detect_platform
    resolve_version
    download_binary
    install_binary
    install_completions
    install_daemon

    printf "\n${GREEN}${BOLD}Installation complete!${RESET}\n\n"

    # Check if first-time setup is needed.
    if [ ! -f "$HOME/.clawdesk/config.toml" ] && [ ! -d "$HOME/.clawdesk/data" ]; then
        printf "  Next steps:\n"
        printf "    1. ${BOLD}clawdesk init${RESET}        — Configure providers and channels\n"
        printf "    2. ${BOLD}clawdesk${RESET}             — Start chatting\n"
    else
        printf "  ${GREEN}Ready!${RESET} Run ${BOLD}clawdesk daemon status${RESET} to check the gateway.\n"
    fi
    printf "\n"
}

main "$@"
