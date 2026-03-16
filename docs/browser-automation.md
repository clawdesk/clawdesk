# Browser Automation

ClawDesk provides full browser automation via the `clawdesk-browser` crate using Chrome DevTools Protocol (CDP).

## Architecture

```
Agent Request
    │
    ▼
Route Dispatcher (agent.act / agent.snapshot / agent.snapshot.plan)
    │
    ├── Extension Relay ──→ Real user browser (WebSocket)
    │
    └── BrowserManager ──→ Headless Chrome (CDP)
            │
            ├── Session-Tab Registry (O(1) per-profile tab state)
            ├── DOM Intelligence (element indexing, a11y tree)
            ├── Snapshot Engine (DOM/ARIA/AI modes)
            └── SSRF + Navigation Guard
```

## Extension Relay

The extension relay enables bidirectional communication between a Chrome extension running in the user's browser and the ClawDesk backend. This allows agents to observe and act on the user's **real browsing context** rather than a headless instance.

### Connection State Machine

```
Disconnected → Authenticating → Connected → Stale
     ↑              |                |        |
     └──────────────┴────────────────┴────────┘
```

- **Heartbeat**: Extension pings every 10s. Timeout after 30s marks Stale.
- **Reconnection**: Exponential backoff with jitter: `delay(n) = min(100ms × 2^n + rand(0,50ms), 30s)`
- **Auth**: CSRF token + session HMAC on handshake.

## Route Dispatcher

Instead of a monolithic "do browser thing" interface, agents use structured routes:

| Route | Purpose |
|-------|---------|
| `agent.act` | Execute action (click, type, scroll) |
| `agent.snapshot` | Capture page state (DOM/ARIA/AI modes) |
| `agent.snapshot.plan` | Generate action plan from snapshot diff |
| `agent.storage` | Read/write browser storage |
| `agent.debug` | Console, network, performance |
| `agent.navigate` | Navigate to URL |
| `agent.tabs` | Tab management |

## Session-Tab Registry

Tracks tab state across multiple browser profiles. Uses `DashMap` for O(1) concurrent lookups.

### Tab State Machine

```
NoTab → Active(id) → Switching(from, to) → Active(to)
                   → Loading(id, url)
                   → Closed(id)
```

## Snapshot Modes

| Mode | Output | Use Case |
|------|--------|----------|
| DOM | Element-indexed HTML with `data-ci` attributes | General interaction |
| ARIA | Full accessibility tree from CDP | Screen reader simulation |
| AI | Compact snapshot with `[ref=eN]` identifiers | LLM-optimized |

## Snapshot Diff (Plan Mode)

Phase 1: Compute minimal DOM diff `δ = current ⊖ previous` — O(|DOM| × log|DOM|).
Phase 2: Generate action plan from δ using ref-based element stability.

## Navigation Guard

Extends SSRF protection with redirect chain depth checking:
- Block if redirect chain > 5 hops (configurable)
- Checks: origin profile, extension relay context, user explicit navigation

## Security

- SSRF prevention: URL validation against private networks
- Content wrapping: External content sandboxed
- Purchase detection: Financial transactions require approval
- Console capture: Monitored for security events
