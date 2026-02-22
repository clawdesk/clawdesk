# Troubleshooting

## Quick Diagnostics

### ClawDesk Doctor

Run the built-in diagnostic command:

```bash
# From the clawdesk directory
cargo run --bin clawdesk -- doctor
```

This checks:
- Rust toolchain version
- Node.js / pnpm availability
- Database integrity
- Provider connectivity
- Disk space for data directory
- Port 18789 availability

### Log Locations

| Platform | Path |
|----------|------|
| macOS | `~/Library/Application Support/com.clawdesk.app/logs/` |
| Linux | `~/.local/share/com.clawdesk.app/logs/` |
| Windows | `%APPDATA%\com.clawdesk.app\logs\` |

Set `CLAWDESK_LOG=debug` to increase verbosity:

```bash
CLAWDESK_LOG=debug cargo tauri dev
```

Fine-grained filtering:

```bash
# Only agent and memory subsystems at debug level
CLAWDESK_LOG="clawdesk_agents=debug,clawdesk_memory=debug,warn"
```

---

## Common Issues

### Build Issues

#### "error: failed to run custom build command for `tauri`"

**Cause**: Missing system dependencies for Tauri.

**Fix** (macOS):
```bash
xcode-select --install
```

**Fix** (Ubuntu/Debian):
```bash
sudo apt install libwebkit2gtk-4.1-dev \
  build-essential curl wget file \
  libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev
```

#### "pnpm: command not found"

Install pnpm:
```bash
npm install -g pnpm
```

Or via Homebrew:
```bash
brew install pnpm
```

#### "error[E0433]: failed to resolve: use of undeclared crate"

The workspace has many crates with feature flags. Ensure you build from the workspace root:
```bash
cd clawdesk
cargo build --workspace
```

### Provider Connection Issues

#### "Provider connection failed" / "Unauthorized"

1. **Verify the API key** is correctly set:
   ```bash
   # Test in terminal
   echo $ANTHROPIC_API_KEY  # Should print your key
   ```

2. **Check key validity** — API keys expire or get revoked. Try:
   ```bash
   curl -H "x-api-key: $ANTHROPIC_API_KEY" \
        -H "content-type: application/json" \
        -H "anthropic-version: 2023-06-01" \
        https://api.anthropic.com/v1/models
   ```

3. **Check proxy settings** — If behind a corporate proxy:
   ```bash
   export HTTPS_PROXY=http://proxy.example.com:8080
   ```

#### "Model not found"

- Verify the model name matches the provider's naming convention
- Run `list_models` to see available models
- Some models require specific API plan access (e.g., GPT-4o requires a paid plan)

#### "429 Too Many Requests" / Rate Limited

- **Reduce concurrency**: Lower `max_tool_rounds` in agent config
- **Add delay**: The built-in retry logic uses exponential backoff
- **Check provider dashboard**: Review your rate limit tier
- **Use failover**: Configure a secondary provider

#### Ollama: "connection refused"

Ensure Ollama is running:
```bash
ollama serve
# Or check if it's already running:
curl http://localhost:11434/api/version
```

Pull the model first:
```bash
ollama pull llama3.1
```

### Memory System

#### "Memory search returns no results"

1. **Check embedding provider** — A configured embedding API key is required:
   ```
   OPENAI_API_KEY=... (for text-embedding-3-small)
   ```

2. **Verify memories exist**:
   - Use the `get_memory_stats` IPC command
   - Check if the memory database exists at the data directory

3. **Rebuild the index** — If the BM25 or vector index is corrupted:
   - Delete the memory database file
   - Memories will be re-indexed on next startup

#### "Embedding provider unavailable"

The tiered provider falls back automatically:
1. OpenAI → 2. Ollama → 3. Local (reduced quality)

Check that at least one is configured. For fully local operation:
```bash
ollama pull nomic-embed-text
```

### Database Issues

#### "Database locked" / "SQLITE_BUSY"

SochDB uses WAL mode, which supports concurrent reads. If you get lock errors:

1. **Only one ClawDesk instance** should run at a time
2. **Check for zombie processes**:
   ```bash
   ps aux | grep clawdesk
   kill -9 <pid>
   ```
3. **Remove stale lock files**:
   ```bash
   rm ~/Library/Application\ Support/com.clawdesk.app/data/*.lock
   ```

#### "Database corrupted"

1. Stop ClawDesk
2. Back up the data directory
3. Run integrity check:
   ```bash
   cargo run --bin clawdesk -- db-check
   ```
4. If unrecoverable, delete the database file — it will be recreated on next start (you lose stored data)

### Gateway Issues

#### "Port 18789 already in use"

```bash
# Find what's using the port
lsof -i :18789

# Kill it or use a different port
CLAWDESK_GW_PORT=18790 cargo run
```

#### "CORS error" in browser

If accessing the gateway from a browser (not the Tauri app), ensure the origin is allowed:

```bash
CLAWDESK_GW_CORS_ORIGINS="http://localhost:3000" cargo run
```

#### WebSocket disconnects

- Increase the timeout: `CLAWDESK_GW_WS_TIMEOUT=120`
- Check for proxy/firewall interference
- The client should implement reconnection logic

### Channel Issues

#### Slack: "invalid_auth"

1. Recreate the Slack app token
2. Ensure the token has the correct scopes: `chat:write`, `channels:read`, `channels:history`
3. Reinstall the app to your workspace

#### Discord: "Bot token invalid"

1. Regenerate the token in the Discord Developer Portal
2. Ensure the bot has the Message Content intent enabled

#### Email: "Authentication failed"

For Gmail, use an App Password (not your regular password):
1. Enable 2FA on your Google account
2. Generate an App Password at https://myaccount.google.com/apppasswords
3. Set `CLAWDESK_EMAIL_PASSWORD` to the app password

### Skill Issues

#### "Skill failed to load"

1. Validate the manifest:
   ```bash
   cargo run --bin clawdesk -- skill validate /path/to/manifest.toml
   ```

2. Common manifest errors:
   - Missing required fields (`id`, `display_name`, `description`)
   - Invalid TOML syntax
   - Missing `prompt.md` file
   - `token_cost` exceeds agent's budget

#### "Skill not activating"

- Check trigger keywords match the user's message
- Verify the skill is activated for the agent
- Check that token budget allows the skill's `token_cost`
- Look at logs for selection pipeline decisions

### Plugin Issues

#### "Plugin hook not firing"

1. Verify the plugin is loaded:
   ```
   list_plugins IPC command
   ```
2. Check the hook phase is correct
3. Ensure the manifest declares the correct hook phase
4. Check plugin logs for errors during initialization

#### "Plugin sandbox error"

Sandboxed plugins have restricted access:
- **No file system access** outside the workspace
- **No network access** unless explicitly granted
- **Memory limit**: Default 64 MB
- **CPU time limit**: Default 5 seconds per invocation

### Performance Issues

#### High memory usage

1. **Check context window sizes** — Large `tokenBudget` values consume more memory
2. **Reduce active agents** — Each agent maintains state
3. **Clear old sessions** — Delete unused chat sessions
4. **Limit memory entries** — Prune old memories

#### Slow responses

1. **Check the model** — Larger models (Claude Opus, GPT-4) are slower
2. **Reduce tool rounds** — Set `max_tool_rounds: 3` or lower
3. **Disable unnecessary skills** — Each injected skill uses tokens
4. **Check network latency** — Use `curl -w "%{time_total}\n" -o /dev/null` to test
5. **Enable semantic cache** — Duplicate queries hit the cache instead of the API

#### High CPU during startup

Normal — the system initializes embeddings, loads indexes, and warms caches. Should settle within 10-30 seconds.

### Tunnel Issues

#### "Tunnel handshake failed"

1. Ensure both peers are running ClawDesk
2. Check that the pairing code was entered correctly
3. Verify no firewall blocks UDP on the configured port
4. Try refreshing the pairing code

#### "Tunnel connection unstable"

- The WireGuard tunnel uses UDP — some corporate networks block it
- Try switching to the fallback TCP mode if available
- Check MTU issues: reduce tunnel MTU to 1280

---

## Debugging Techniques

### Enable Tracing

```bash
# Enable OpenTelemetry tracing to stdout
CLAWDESK_OTEL_ENDPOINT=stdout cargo run
```

### Inspect the Agent Pipeline

Enable trace-level logging for the agent runner:
```bash
CLAWDESK_LOG="clawdesk_agents::runner=trace" cargo run
```

This logs every stage:
1. Skill selection and injection
2. Memory retrieval scores
3. Prompt assembly (with token counts)
4. Provider request/response
5. Tool execution details
6. Context compaction decisions

### Database Inspection

```bash
# List all tables
cargo run --bin clawdesk -- db-tables

# Dump a table
cargo run --bin clawdesk -- db-dump agents
```

### Network Debugging

```bash
# Watch gateway requests
CLAWDESK_LOG="tower_http=debug" cargo run

# Capture provider API calls
CLAWDESK_LOG="clawdesk_providers=trace" cargo run
```

### Frontend Debugging

1. Open DevTools: `Cmd+Opt+I` (macOS) / `Ctrl+Shift+I` (Linux/Windows)
2. Check the Console for IPC errors
3. Monitor Network tab for WebSocket frames
4. React DevTools extension works inside the Tauri WebView

---

## Error Reference

| Error Code | Meaning | Resolution |
|------------|---------|------------|
| `E001` | Provider authentication failed | Check API key |
| `E002` | Model not available | Verify model name |
| `E003` | Context window exceeded | Reduce prompt size or enable compaction |
| `E004` | Tool execution failed | Check tool permissions |
| `E005` | Rate limit exceeded | Wait or configure failover |
| `E006` | Database write failed | Check disk space and permissions |
| `E007` | Memory index error | Rebuild memory index |
| `E008` | Skill validation failed | Fix manifest.toml |
| `E009` | Plugin load error | Check plugin binary/manifest |
| `E010` | Channel authentication failed | Refresh channel credentials |
| `E011` | Tunnel handshake failed | Re-pair devices |
| `E012` | Security scan blocked | Content violated safety policy |

---

## Getting Help

1. **Check logs first** — Most issues are diagnosed from log output
2. **Search existing issues** — Check the GitHub issue tracker
3. **Include diagnostics** — When filing an issue, include:
   - ClawDesk version (`cargo run -- --version`)
   - OS and version
   - Relevant log output (with `CLAWDESK_LOG=debug`)
   - Steps to reproduce
   - Expected vs actual behavior
