#!/bin/bash
# ═══════════════════════════════════════════════════════════
# ClawDesk End-to-End Usecase Validation Suite
# Tests the workflow templates and agent infrastructure
# ═══════════════════════════════════════════════════════════

set -euo pipefail

CLAWDESK="$(cd "$(dirname "$0")/.." && pwd)"
CLI="$CLAWDESK/target/debug/clawdesk"
PASS=0
FAIL=0
SKIP=0
RESULTS=""

log_pass() { PASS=$((PASS+1)); RESULTS+="  ✅ $1\n"; printf "  ✅ %s\n" "$1"; }
log_fail() { FAIL=$((FAIL+1)); RESULTS+="  ❌ $1: $2\n"; printf "  ❌ %s: %s\n" "$1" "$2"; }
log_skip() { SKIP=$((SKIP+1)); RESULTS+="  ⏭️  $1: $2\n"; printf "  ⏭️  %s: %s\n" "$1" "$2"; }

echo "╔═══════════════════════════════════════════════╗"
echo "║  ClawDesk E2E Validation Suite                ║"
echo "╚═══════════════════════════════════════════════╝"
echo ""

# ── 1. CLI binary exists ──
echo "▸ Infrastructure Tests"
if [ -x "$CLI" ]; then
  log_pass "CLI binary exists"
else
  log_fail "CLI binary" "not found at $CLI"
  echo "Build with: cargo build --package clawdesk-cli"
  exit 1
fi

# ── 2. Agent validate ──
if "$CLI" agent validate 2>&1 | grep -q "0 errors"; then
  count=$("$CLI" agent validate 2>&1 | grep -o '[0-9]* agents' | head -1)
  log_pass "Agent validation ($count)"
else
  log_fail "Agent validation" "has errors"
fi

# ── 3. Agent list ──
agent_count=$("$CLI" agent list 2>&1 | grep "agent(s)" | grep -o '[0-9]*' | head -1 || echo "0")
if [ "$agent_count" -gt 0 ]; then
  log_pass "Agent listing ($agent_count agents)"
else
  log_fail "Agent listing" "no agents found"
fi

# ── 4. Ollama check ──
echo ""
echo "▸ Provider Tests"
if curl -s http://localhost:11434/api/tags >/dev/null 2>&1; then
  models=$(curl -s http://localhost:11434/api/tags | python3 -c "import json,sys; d=json.load(sys.stdin); print(len(d.get('models',[])))" 2>/dev/null || echo "0")
  log_pass "Ollama running ($models models)"
else
  log_skip "Ollama" "not running"
fi

# ── 5. API key checks ──
for provider in ANTHROPIC_API_KEY OPENAI_API_KEY GOOGLE_API_KEY AZURE_OPENAI_API_KEY OPENROUTER_API_KEY; do
  if [ -n "${!provider:-}" ]; then
    log_pass "$provider configured"
  else
    log_skip "$provider" "not set"
  fi
done

# ── 6. Local agent run ──
echo ""
echo "▸ Agent Runtime Tests"
if curl -s http://localhost:11434/api/tags >/dev/null 2>&1; then
  response=$(echo "Reply with only the word 'hello'" | timeout 30 "$CLI" agent run --model qwen2.5:0.5b --allow-all-tools --workspace /tmp/clawdesk-e2e 2>&1 | grep "^Agent:" | head -1 || echo "")
  if [ -n "$response" ]; then
    log_pass "Local agent run (Ollama qwen2.5:0.5b)"
  else
    log_fail "Local agent run" "no response"
  fi
else
  log_skip "Local agent run" "Ollama not running"
fi

# ── 7. Agent with tools ──
if curl -s http://localhost:11434/api/tags >/dev/null 2>&1; then
  mkdir -p /tmp/clawdesk-e2e
  response=$(echo "Create a file called hello.txt with the content 'test passed'" | timeout 45 "$CLI" agent run --model qwen2.5:0.5b --allow-all-tools --workspace /tmp/clawdesk-e2e 2>&1 | grep -c "file_write\|shell_exec\|Agent:" || echo "0")
  if [ "$response" -gt 0 ]; then
    log_pass "Agent tool execution"
  else
    log_fail "Agent tool execution" "no tool calls detected"
  fi
else
  log_skip "Agent tool execution" "Ollama not running"
fi

# ── 8. Workspace isolation ──
echo ""
echo "▸ Workspace Tests"
if [ -d "$HOME/.clawdesk/workspace" ]; then
  log_pass "Workspace directory exists"
else
  log_fail "Workspace directory" "missing ~/.clawdesk/workspace"
fi

if [ -d "$HOME/.clawdesk/workspace/projects" ]; then
  project_count=$(ls -1 "$HOME/.clawdesk/workspace/projects" 2>/dev/null | wc -l | tr -d ' ')
  log_pass "Project isolation dir ($project_count projects)"
else
  log_skip "Project isolation" "no projects dir yet"
fi

# ── 9. Agent config structure ──
echo ""
echo "▸ Agent Config Tests"
for agent_dir in "$HOME/.clawdesk/agents"/*/; do
  agent_name=$(basename "$agent_dir")
  if [ -f "$agent_dir/agent.toml" ]; then
    if grep -q 'id = ' "$agent_dir/agent.toml" && grep -q 'display_name = ' "$agent_dir/agent.toml"; then
      log_pass "Agent config: $agent_name (valid schema)"
    else
      log_fail "Agent config: $agent_name" "missing required fields"
    fi
  else
    log_fail "Agent config: $agent_name" "no agent.toml"
  fi
done

# ── 10. Usecase template validation ──
echo ""
echo "▸ Usecase Template Tests (36 from awesome-openclaw-usecases)"
USECASES_DIR="/Users/sushanth/llamabot/awesome-openclaw-usecases/usecases"
if [ -d "$USECASES_DIR" ]; then
  usecase_count=$(ls -1 "$USECASES_DIR"/*.md 2>/dev/null | wc -l | tr -d ' ')
  log_pass "Usecase repo accessible ($usecase_count usecases)"
  
  # Check each usecase has the expected structure
  for md in "$USECASES_DIR"/*.md; do
    name=$(basename "$md" .md)
    if head -1 "$md" | grep -q "^#"; then
      log_pass "Usecase: $name (valid markdown)"
    else
      log_fail "Usecase: $name" "no H1 header"
    fi
  done
else
  log_skip "Usecase repo" "not found at $USECASES_DIR"
fi

# ── 11. Rust crate compilation ──
echo ""
echo "▸ Build Verification"
if cd "$CLAWDESK" && cargo check -p clawdesk-domain 2>&1 | tail -1 | grep -q "Finished"; then
  log_pass "clawdesk-domain compiles"
else
  log_fail "clawdesk-domain" "compilation failed"
fi

if cargo check -p clawdesk-agents 2>&1 | tail -1 | grep -q "Finished"; then
  log_pass "clawdesk-agents compiles"
else
  log_fail "clawdesk-agents" "compilation failed"
fi

# ── 12. Unit tests for new modules ──
echo ""
echo "▸ Unit Tests"
for mod in intent eval_loop coherence; do
  result=$(cargo test -p clawdesk-agents --lib "${mod}::tests" 2>&1 | grep "test result" | head -1 || echo "")
  if echo "$result" | grep -q "0 failed"; then
    passed=$(echo "$result" | grep -o '[0-9]* passed' || echo "")
    log_pass "clawdesk-agents::$mod ($passed)"
  else
    log_fail "clawdesk-agents::$mod" "tests failed"
  fi
done

for mod in prompt_trace proactive_compaction policy_dsl workflow_templates; do
  result=$(cargo test -p clawdesk-domain --lib "${mod}::tests" 2>&1 | grep "test result" | head -1 || echo "")
  if echo "$result" | grep -q "0 failed"; then
    passed=$(echo "$result" | grep -o '[0-9]* passed' || echo "")
    log_pass "clawdesk-domain::$mod ($passed)"
  else
    log_fail "clawdesk-domain::$mod" "tests failed"
  fi
done

# ── 13. Frontend build ──
echo ""
echo "▸ Frontend Build"
if cd "$CLAWDESK/crates/ui" && pnpm build 2>&1 | grep -q "built in"; then
  log_pass "Vite 8 frontend build"
else
  log_fail "Frontend build" "failed"
fi

# ── Summary ──
echo ""
echo "╔═══════════════════════════════════════════════╗"
echo "║  Results                                      ║"
echo "╠═══════════════════════════════════════════════╣"
printf "║  ✅ Passed: %-34s║\n" "$PASS"
printf "║  ❌ Failed: %-34s║\n" "$FAIL"
printf "║  ⏭️  Skipped: %-33s║\n" "$SKIP"
echo "╚═══════════════════════════════════════════════╝"

if [ "$FAIL" -gt 0 ]; then
  exit 1
fi
