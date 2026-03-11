# Developer Notes — Recent Changes

## Session Summary (March 10, 2026)

### Gateway Pipeline Parity

The HTTP gateway was missing critical pipeline stages that the Tauri desktop
app had, causing gateway agents to behave as generic chatbots.

**Fixed:**
- System prompt: was hardcoded `"You are a helpful assistant."` → now loaded from agent registry
- Tools: was empty `ToolRegistry::new()` → now registers all builtin tools
- Skills: was 0 skills → now loads 93 bundled skills (52 original + 41 GWS)
- SkillProvider: was missing → now wired with `OrchestratorSkillProvider`
- Prompt pipeline: was raw persona string → now uses full `PromptBuilder` knapsack
- Hook manager: was missing → now wired for plugin lifecycle dispatch
- WebSocket: was `system_prompt: None` → now resolved from agent registry

### Token Tracking

Token tracking is implemented and working:
- `state.record_usage()` called after every LLM response (commands.rs:2498)
- `emit_metrics_updated()` event fired after every message (commands.rs:2843)
- `get_metrics()` Tauri command returns `CostMetrics`: today_cost, input/output tokens, model breakdown
- Frontend event: `metrics:updated` with full payload

If the Observability page shows `$0.00` after sending messages, the frontend
needs to subscribe to the `metrics:updated` event (not just fetch on mount).

### Logs

Audit logging infrastructure is complete:
- `AuditLogger` initialized at startup (state.rs:2254)
- `get_audit_logs` and `get_execution_logs` commands registered (lib.rs:207-208)
- Audit events written during: message send, agent create, security alert, session lifecycle
- If logs show "0 entries", check that audit events have been triggered (send a message first)

### Channel Registration

All 17 channels are now registered in the factory:
- **Previously registered** (9): Discord, Telegram, Slack, WhatsApp, Email, iMessage, IRC, WebChat, Internal
- **Newly registered** (4): Signal, Matrix, MS Teams, Mastodon
- Plus: Markdown, Webhook adapters
- All show in Tauri UI when env vars are set

### Extension OAuth

Google integrations unified to use shared `GOOGLE_CLIENT_ID`:
- Previously: `GMAIL_CLIENT_ID`, `GDRIVE_CLIENT_ID`, `GCAL_CLIENT_ID`, `GWS_CLIENT_ID` (4 different variables!)
- Now: single `GOOGLE_CLIENT_ID` for all Google services
- OAuth template resolution fixed — `${KEY}` patterns resolved from saved config before building auth URL
- Clear error message when client ID not configured

### Google Workspace (gws)

- 41 GWS skills bundled via `include_dir!` (embedded in binary at compile time)
- **IMPORTANT**: After adding/changing skills in `openclaw-skills/`, you MUST
  rebuild the binary. `include_dir!` bakes files into the `.rodata` section
  at compile time. A running app won't see new skills until restarted:
  ```bash
  # Restart Tauri dev:
  ./run-tauri.sh
  # Or just rebuild skills crate:
  cargo build -p clawdesk-skills
  ```
- After rebuild: Skills page should show 93+ skills (73 original + ~20 GWS that parse correctly)
- gws binary built from source via `scripts/build-gws.sh` or downloaded via `scripts/download-gws.sh`
- Tauri sidecar: `binaries/gws-{target-triple}` alongside `llama-server`
- GitHub Actions: gws sidecar built in all 3 release workflows (mac, linux, windows)
- `run-tauri.sh` auto-builds gws sidecar if not present
- Original Apache-2.0 license preserved (GWS-LICENSE in binaries/)

### Files Modified

```
crates/clawdesk-gateway/src/routes.rs          — Full prompt pipeline, skills, hooks
crates/clawdesk-gateway/src/state.rs           — Hook manager, skill reload
crates/clawdesk-gateway/src/ws.rs              — System prompt from registry
crates/clawdesk-agents/src/runner.rs           — DEFAULT_SYSTEM_PROMPT
crates/clawdesk-types/src/session.rs           — DEFAULT_SYSTEM_PROMPT constant
crates/clawdesk-tauri/src/commands.rs          — Empty persona fallback, dynamic_spawn fix
crates/clawdesk-tauri/src/enriched_backend.rs  — DEFAULT_SYSTEM_PROMPT
crates/clawdesk-tauri/src/commands_extensions.rs — OAuth template resolution
crates/clawdesk-tauri/src/state.rs             — Signal/Matrix/Teams/Mastodon env vars
crates/clawdesk-tauri/tauri.conf.json          — gws sidecar in externalBin
crates/clawdesk-channels/src/factory.rs        — Signal/Matrix/Teams/Mastodon registration
crates/clawdesk-extensions/src/registry.rs     — Unified GOOGLE_CLIENT_ID, google-workspace
crates/clawdesk-cli/src/main.rs                — Bundled skills + builtin tools
crates/clawdesk-skills/openclaw-skills/gws-*   — 41 GWS skills
agents/general-assistant.toml                  — Enhanced prompt + tools
scripts/build-gws.sh                           — Build gws from source
scripts/download-gws.sh                        — Build/download gws sidecar
run-tauri.sh                                   — Auto-build gws sidecar
.github/workflows/release-mac.yml              — gws sidecar step
.github/workflows/release-linux.yml            — gws sidecar step
.github/workflows/release-windows.yml          — gws sidecar step
.github/workflows/release.yml                  — gws binary step
docs/google-workspace.md                       — GWS usage guide
docs/dev-notes.md                              — This file
docs/diagrams/*.svg                            — Architecture flow diagrams
CHANGELOG.md                                   — All changes documented
```
