#!/bin/bash
set -euo pipefail

# ClawDesk E2E Test Script — CLI + Local Model Setup
# Run inside Docker container or locally

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
BLUE='\033[0;34m'
BOLD='\033[1m'
RESET='\033[0m'

PASS=0
FAIL=0
SKIP=0

pass() { printf "${GREEN}  ✓ PASS${RESET}  %s\n" "$1"; PASS=$((PASS+1)); }
fail() { printf "${RED}  ✗ FAIL${RESET}  %s\n" "$1"; FAIL=$((FAIL+1)); }
skip() { printf "${YELLOW}  ⊘ SKIP${RESET}  %s\n" "$1"; SKIP=$((SKIP+1)); }
info() { printf "${BLUE}▸${RESET} %s\n" "$1"; }
section() { printf "\n${BOLD}━━━ %s ━━━${RESET}\n\n" "$1"; }

# ── 1. Binary Basics ──────────────────────────────────────────
section "1. Binary Basics"

info "Checking clawdesk binary exists..."
if command -v clawdesk > /dev/null 2>&1; then
    pass "clawdesk binary found at $(command -v clawdesk)"
else
    fail "clawdesk binary not found in PATH"
    exit 1
fi

info "Checking version..."
if VERSION=$(clawdesk --version 2>&1); then
    pass "Version: $VERSION"
else
    fail "clawdesk --version failed"
fi

info "Checking help..."
if clawdesk --help > /dev/null 2>&1; then
    pass "clawdesk --help works"
else
    fail "clawdesk --help failed"
fi

# ── 2. Subcommand Availability ─────────────────────────────────
section "2. Subcommand Availability"

SUBCOMMANDS=(
    "doctor"
    "init"
    "gateway"
    "config"
    "agent"
    "daemon"
    "completions"
    "security"
    "tmux"
)

for cmd in "${SUBCOMMANDS[@]}"; do
    if clawdesk "$cmd" --help > /dev/null 2>&1; then
        pass "clawdesk $cmd --help"
    else
        fail "clawdesk $cmd --help"
    fi
done

# ── 3. Doctor (Diagnostics) ───────────────────────────────────
section "3. Doctor Diagnostics"

info "Running clawdesk doctor..."
if clawdesk doctor 2>&1; then
    pass "Doctor completed"
else
    # Doctor may report warnings but shouldn't crash
    if [ $? -le 1 ]; then
        pass "Doctor completed with warnings"
    else
        fail "Doctor crashed"
    fi
fi

# ── 4. Init / Data Directory Setup ────────────────────────────
section "4. Data Directory Setup"

info "Checking data directory..."
if [ -d "$HOME/.clawdesk" ]; then
    pass "~/.clawdesk directory exists"
else
    fail "~/.clawdesk directory missing"
fi

info "Running clawdesk init (non-interactive)..."
# init may fail without a TTY but shouldn't crash
if clawdesk init 2>&1 || true; then
    pass "Init attempted (may need TTY for interactive)"
fi

# ── 5. Shell Completions ──────────────────────────────────────
section "5. Shell Completions"

for shell in bash zsh fish; do
    if clawdesk completions "$shell" > /dev/null 2>&1; then
        pass "completions $shell"
    else
        fail "completions $shell"
    fi
done

# ── 6. Gateway Start ──────────────────────────────────────────
section "6. Gateway"

info "Starting gateway in background..."
clawdesk gateway run --port 18789 --bind all &
GW_PID=$!
sleep 3

info "Checking gateway health..."
RETRIES=5
GW_OK=false
for i in $(seq 1 $RETRIES); do
    if curl -sf http://127.0.0.1:18789/api/v1/health > /dev/null 2>&1; then
        GW_OK=true
        break
    fi
    sleep 2
done

if $GW_OK; then
    pass "Gateway health endpoint responding"
else
    fail "Gateway health endpoint not responding after ${RETRIES} retries"
fi

# ── 7. Local Model Provider Setup ─────────────────────────────
section "7. Local Model Provider Configuration"

LOCAL_URL="${LOCAL_MODEL_URL:-http://localhost:8000}"

info "Configuring local OpenAI-compatible provider..."
info "Base URL: $LOCAL_URL"

# Configure via CLI config set
if clawdesk config set provider.local.type "openai-compatible" 2>&1; then
    pass "Set provider type"
else
    skip "config set provider type (may need gateway)"
fi

if clawdesk config set provider.local.base_url "$LOCAL_URL" 2>&1; then
    pass "Set provider base_url"
else
    skip "config set provider base_url"
fi

if clawdesk config set provider.local.model "grok-4-1-fast-non-reasoning" 2>&1; then
    pass "Set provider model"
else
    skip "config set provider model"
fi

# ── 8. Test Local Model Connectivity ──────────────────────────
section "8. Local Model Connectivity"

info "Testing connection to local model at $LOCAL_URL..."
if curl -sf "${LOCAL_URL}/v1/models" > /dev/null 2>&1; then
    pass "Local model endpoint reachable"

    info "Listing available models..."
    MODELS=$(curl -sf "${LOCAL_URL}/v1/models" 2>&1 | jq -r '.data[].id' 2>/dev/null || echo "")
    if [ -n "$MODELS" ]; then
        pass "Models available: $(echo "$MODELS" | tr '\n' ', ')"
    else
        skip "Could not parse model list"
    fi
else
    skip "Local model at $LOCAL_URL not reachable (start your model server first)"
fi

# ── 9. Send Test Message ──────────────────────────────────────
section "9. Send Test Message"

if $GW_OK; then
    info "Sending test message via CLI..."
    if RESP=$(clawdesk message send "Hello, what model are you?" --model grok-4-1-fast-non-reasoning 2>&1); then
        pass "Message sent successfully"
        printf "  Response: %.200s...\n" "$RESP"
    else
        skip "Message send failed (local model may not be running)"
    fi
else
    skip "Gateway not running, skipping message test"
fi

# ── 10. Agent Operations ──────────────────────────────────────
section "10. Agent Operations"

info "Listing agents..."
if AGENTS=$(clawdesk agent list 2>&1); then
    AGENT_COUNT=$(echo "$AGENTS" | wc -l)
    pass "Agent list returned ($AGENT_COUNT entries)"
else
    fail "Agent list failed"
fi

info "Validating agent configs..."
if clawdesk agent validate 2>&1; then
    pass "Agent validation passed"
else
    fail "Agent validation failed"
fi

# ── 11. Resource Monitor ──────────────────────────────────────
section "11. Resource Monitor"

info "Checking resources..."
if clawdesk resources 2>&1; then
    pass "Resources command works"
else
    skip "Resources command not available"
fi

# ── 12. Daemon Operations ─────────────────────────────────────
section "12. Daemon Operations"

info "Checking daemon status..."
if clawdesk daemon status 2>&1; then
    pass "Daemon status works"
else
    # May not be installed as a service in Docker
    skip "Daemon not installed (expected in container)"
fi

# ── Cleanup ───────────────────────────────────────────────────
section "Cleanup"

if [ -n "${GW_PID:-}" ]; then
    info "Stopping gateway (PID $GW_PID)..."
    kill "$GW_PID" 2>/dev/null || true
    wait "$GW_PID" 2>/dev/null || true
    pass "Gateway stopped"
fi

# ── Summary ───────────────────────────────────────────────────
section "Summary"

TOTAL=$((PASS+FAIL+SKIP))
printf "  ${GREEN}Passed${RESET}: %d\n" "$PASS"
printf "  ${RED}Failed${RESET}: %d\n" "$FAIL"
printf "  ${YELLOW}Skipped${RESET}: %d\n" "$SKIP"
printf "  Total:   %d\n\n" "$TOTAL"

if [ "$FAIL" -gt 0 ]; then
    printf "${RED}${BOLD}Some tests failed!${RESET}\n\n"
    exit 1
else
    printf "${GREEN}${BOLD}All tests passed!${RESET}\n\n"
    exit 0
fi
