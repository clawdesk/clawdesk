# Security & Safety

ClawDesk implements defense-in-depth security with zero-trust principles. Every message is scanned, every tool call can be gated, every action is audited, and all data stays local.

## Security Architecture

```
┌─────────────────────────────────────────────────────┐
│                  Security Layers                     │
│                                                      │
│  Layer 1: Content Scanning (CascadeScanner)          │
│  ┌─────────────────────────────────────────────┐    │
│  │ Aho-Corasick fast pass → Regex deep pass    │    │
│  │ Detects: secrets, PII, prompt injection     │    │
│  └─────────────────────────────────────────────┘    │
│                                                      │
│  Layer 2: Access Control (AclManager)                │
│  ┌─────────────────────────────────────────────┐    │
│  │ Principal-based permissions per resource     │    │
│  └─────────────────────────────────────────────┘    │
│                                                      │
│  Layer 3: Identity Verification (IdentityContract)   │
│  ┌─────────────────────────────────────────────┐    │
│  │ Hash-locked persona — tamper detection       │    │
│  └─────────────────────────────────────────────┘    │
│                                                      │
│  Layer 4: Execution Approval (ExecApprovalManager)   │
│  ┌─────────────────────────────────────────────┐    │
│  │ Human-in-the-loop for sensitive operations   │    │
│  └─────────────────────────────────────────────┘    │
│                                                      │
│  Layer 5: Audit Trail (AuditLogger)                  │
│  ┌─────────────────────────────────────────────┐    │
│  │ Hash-chained (SHA-256) tamper-evident log    │    │
│  └─────────────────────────────────────────────┘    │
│                                                      │
│  Layer 6: Authentication (OAuth2 + Scoped Tokens)    │
│  ┌─────────────────────────────────────────────┐    │
│  │ OAuth2/PKCE + capability-separated tokens    │    │
│  └─────────────────────────────────────────────┘    │
│                                                      │
│  Layer 7: Network Security (Tunnel + Cert Pinning)   │
│  ┌─────────────────────────────────────────────┐    │
│  │ WireGuard P2P + TLS pinning + zero-trust    │    │
│  └─────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────┘
```

## Content Scanning

### CascadeScanner

Every message (inbound and outbound) passes through `CascadeScanner`:

```
Input text
    │
    ▼
┌─ Tier 1: Aho-Corasick Fast Pass ──────────────────┐
│  Multi-pattern automaton — O(n) regardless of      │
│  pattern count. Checks for:                        │
│  • API key prefixes (sk-ant-, sk-, AKIA, etc.)     │
│  • Secret patterns (password=, token=, etc.)       │
│  • Known prompt injection markers                  │
│  If no match → pass immediately (fast path)        │
└──────────────────────┬─────────────────────────────┘
                       │ match found
                       ▼
┌─ Tier 2: Regex Deep Pass ─────────────────────────┐
│  Compiled regex patterns for:                      │
│  • Email addresses                                 │
│  • Credit card numbers                             │
│  • SSN patterns                                    │
│  • JWT tokens                                      │
│  • Private keys (PEM format)                       │
│  • Custom patterns (configurable)                  │
└──────────────────────┬─────────────────────────────┘
                       │
                       ▼
                  ScanResult
                  • passed: bool
                  • findings: Vec<Finding>
                  • severity: Low/Medium/High/Critical
```

### Configuration

```rust
pub struct CascadeScannerConfig {
    pub enabled: bool,
    pub scan_user_messages: bool,
    pub scan_agent_responses: bool,
    pub scan_tool_results: bool,
    pub custom_patterns: Vec<CustomPattern>,
    pub severity_threshold: Severity,
}
```

### Scan Points

| Location | What's Scanned | Action on Detection |
|----------|---------------|---------------------|
| User message input | Inbound content | Flag or reject message |
| Agent response | Outbound content | Redact or flag |
| Tool results | Tool output | Truncate or flag |
| Memory storage | Content before persist | Strip detected secrets |

## Audit Logging

### Hash-Chained Audit Log

`AuditLogger` creates tamper-evident audit trails using SHA-256 hash chains:

```
Entry N:
  category: MessageSend
  action: "user_message"
  actor: User { sender_id: "desktop", channel: "tauri" }
  agent_id: "agent-123"
  data: { content_length: 150, scan_passed: true }
  outcome: Success
  timestamp: 2026-02-21T10:30:00Z
  prev_hash: SHA256(Entry N-1)  ← chain integrity
  entry_hash: SHA256(this entry + prev_hash)
```

Any modification to a past entry breaks the hash chain, making tampering detectable.

### Audit Categories

| Category | Events |
|----------|--------|
| `MessageSend` | User messages, agent responses |
| `MessageReceive` | Inbound channel messages |
| `ToolExecution` | Tool calls with args and results |
| `AgentStart` / `AgentEnd` | Agent run lifecycle |
| `ConfigChange` | Settings modifications |
| `SecurityEvent` | Scan findings, access violations |
| `AuthEvent` | Login, token refresh, profile rotation |
| `SkillChange` | Skill activation, deactivation |
| `PluginEvent` | Plugin enable, disable |

### Audit Actors

```rust
pub enum AuditActor {
    User { sender_id: String, channel: String },
    Agent { agent_id: String },
    System { component: String },
    Plugin { plugin_name: String },
}
```

## Access Control

### ACL Manager

`AclManager` provides resource-level access control:

```rust
// Add a permission rule
acl.add_rule("user:alice", "agent:private-bot", Permission::ReadWrite)?;

// Check permission before action
let allowed = acl.check_permission("user:alice", "agent:private-bot", Permission::Execute)?;

// Revoke access
acl.revoke("user:alice", "agent:private-bot")?;
```

### Permissions

| Permission | Allows |
|------------|--------|
| `Read` | View agent, read messages |
| `Write` | Send messages, modify config |
| `Execute` | Run agent, trigger tools |
| `Admin` | Full control, delete, manage ACL |
| `ReadWrite` | Read + Write combined |

### Allowlist Manager

`AllowlistManager` controls which senders can interact with agents:
- **Per-channel allowlists** — Only approved senders on each channel
- **Global allowlist** — Applies to all channels
- **Wildcard support** — Pattern-based allowlisting

## Identity Verification

### IdentityContract

`IdentityContract` verifies that agent personas haven't been tampered with:

```rust
let contract = IdentityContract::new(agent_persona);
// SHA-256 hash of the persona text
let hash = contract.hash();
let version = contract.version();

// On each run, verify the persona matches
let verified = contract.verify(current_persona);
// → IdentityVerified { hash_match: true, version: 1 }
```

This detects:
- System prompt modification by prompt injection
- Persona drift from unauthorized edits
- Version tracking for persona evolution

## Execution Approval

### Human-in-the-Loop Gate

`ExecApprovalManager` requires human approval for sensitive operations:

```
Tool call: shell_exec("rm -rf /tmp/data")
    │
    ▼
ApprovalGate.request_approval("shell_exec", args)
    │
    ▼
┌─ UI Prompt ──────────────────────────────┐
│  🔒 Approval Required                    │
│                                          │
│  Tool: shell_exec                        │
│  Args: rm -rf /tmp/data                  │
│                                          │
│  [Approve]  [Deny]  [Always Allow]       │
└──────────────────────────────────────────┘
    │
    ▼
Approved → Execute tool
Denied → Return error to agent
```

### Approval Commands

| Command | Description |
|---------|-------------|
| `create_approval_request` | Create a pending approval |
| `approve_request` | Approve with optional modifications |
| `deny_request` | Deny with reason |
| `get_approval_status` | Check request status |

### Command Policy Engine

`CommandPolicyEngine` assesses risk levels for commands:

| Risk Level | Examples | Policy |
|------------|----------|--------|
| `Safe` | Read-only operations | Auto-approve |
| `Low` | File reads, web fetches | Log only |
| `Medium` | File writes, API calls | Require approval if configured |
| `High` | Shell execution, system commands | Always require approval |
| `Critical` | Destructive operations, admin actions | Require approval + audit |

## Authentication

### OAuth2 + PKCE

`OAuthFlowManager` manages OAuth2 flows for external service authentication:

```
User clicks "Connect Slack"
    │
    ▼
Generate PKCE challenge (code_verifier + code_challenge)
    │
    ▼
Open browser: authorization_url + code_challenge
    │
    ▼
User authorizes → redirect with auth code
    │
    ▼
Exchange code + code_verifier → access_token + refresh_token
    │
    ▼
Store securely in AuthProfileManager
```

### Auth Profiles

`AuthProfileManager` manages multiple authentication profiles:

```rust
pub struct AuthProfile {
    pub provider: String,
    pub profile_id: String,
    pub token_set: TokenSet,
    pub created_at: DateTime<Utc>,
    pub last_used: DateTime<Utc>,
    pub failure_count: u32,
    pub cooldown_until: Option<DateTime<Utc>>,
}
```

Features:
- Automatic token refresh before expiry
- Failure tracking with cooldown periods
- Profile rotation on auth errors (coordinated with `ProfileRotator`)

### OAuth Commands

| Command | Description |
|---------|-------------|
| `start_oauth_flow` | Begin OAuth2 authorization |
| `oauth_callback` | Handle OAuth2 redirect callback |
| `refresh_oauth_token` | Manually refresh a token |
| `list_auth_profiles` | List all authentication profiles |
| `remove_auth_profile` | Delete an auth profile |

### Scoped Tokens

`ScopedToken` provides capability-separated tokens:

| Scope | Allows |
|-------|--------|
| `Chat` | Send/receive messages |
| `Admin` | Configuration changes |
| `Tools` | Tool execution |
| `Read` | Read-only access |

```rust
// Generate a scoped token
let token = ScopedToken::generate(&server_secret, TokenScope::Chat, Duration::hours(24))?;

// Validate — returns scope or error
let scope = ScopedToken::validate(&token, &server_secret)?;
```

## Credential Management

### Secret References

`SecretRef` ensures secrets are never stored in plaintext:

```rust
// Instead of storing "sk-ant-abc123", store a reference
let secret_ref = SecretRef::new("anthropic-key");

// Resolve at runtime — fetches from secure vault
let value = secret_ref.resolve(&vault)?;
```

### SecretDetector

`SecretDetector` identifies secrets in text:
- API key patterns (provider-specific prefixes)
- Token patterns (JWT, Bearer)
- Connection strings
- Private keys (PEM)

### Credential Vault

`CredentialVault` provides secure storage:
- Encrypted at rest
- Memory-protected (mlock where available)
- Access-logged

## Network Security

### WireGuard Tunnel

`clawdesk-tunnel` provides P2P encrypted networking:

```
Device A ──── WireGuard ──── Device B
   │                           │
   │  Userspace WireGuard      │
   │  No root privileges       │
   │  Single UDP port          │
   │  Cryptographic filter     │
   │  NAT traversal (STUN)     │
   │  QR-code invite pairing   │
   │                           │
   └───────────────────────────┘
```

Without a tunnel, the gateway only binds to `127.0.0.1` (localhost).

### TLS Certificate Pinning

`CertPinning` prevents MITM attacks:

```rust
pub enum PinningMode {
    None,              // No pinning
    PublicKey(Vec<u8>), // Pin to specific public key
    Certificate(Vec<u8>), // Pin to specific certificate
    CertificateAuthority(Vec<u8>), // Pin to CA
}
```

### Device Pairing

`clawdesk-discovery` provides secure device pairing:

1. **mDNS Advertisement** — Broadcast `_clawdesk._tcp.local.` on local network
2. **SPAKE2 Key Exchange** — Password-authenticated key exchange (no plaintext passwords)
3. **Peer Registration** — Paired devices registered in `PeerRegistry`
4. **Tunnel Establishment** — WireGuard tunnel set up between paired devices

## Sandbox Enforcement

### Plugin Sandbox

`PluginSandbox` isolates plugin execution:

| Resource | Enforcement |
|----------|-------------|
| Memory | Configurable limit per plugin |
| CPU | Per-execution timeout |
| Network | Can be restricted to specific endpoints |
| Filesystem | Confined to plugin directory |
| System calls | Filtered (no exec, no socket creation) |

### Workspace Confinement

`WorkspaceGuard` confines file operations:
- All paths canonicalized (prevent symlink escape)
- Operations outside workspace rejected
- Traversal attempts (`../`) blocked
- Audit-logged violations

### Sandbox Gate

`SandboxGate` in the agent runner provides tool-level sandboxing:
- Each tool can declare its sandbox requirements
- Gate enforces requirements before execution
- Violations are logged and rejected

## Security Commands

| Command | Description |
|---------|-------------|
| `get_security_status` | Overall security posture |
| `add_acl_rule` | Add an ACL permission |
| `check_permission` | Check if a principal has a permission |
| `revoke_acl` | Revoke a permission |
| `generate_scoped_token` | Create a capability-scoped token |
| `validate_scoped_token` | Validate and decode a token |
| `get_audit_logs` | Retrieve audit log entries |

## Security Best Practices

1. **Always enable content scanning** — Default is on; don't disable in production
2. **Use scoped tokens** — Never use admin tokens for chat-only access
3. **Set up allowlists** — Restrict which senders can interact with agents
4. **Review audit logs** — Regularly check for anomalous activity
5. **Configure approval gates** — Require approval for destructive tools
6. **Use workspace confinement** — Set `workspace_path` to limit file access
7. **Enable TLS pinning** — For external API connections
8. **Keep profiles rotated** — Multiple API key profiles prevent single-point-of-failure
9. **Use the tunnel** — Don't expose the gateway to the public internet directly
10. **Review skill trust levels** — Only run verified or builtin skills on sensitive data
