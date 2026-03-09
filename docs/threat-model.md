# ClawDesk Threat Model

> **Version**: 1.0  
> **Last Updated**: 2025  
> **Status**: Living document — updated as architecture evolves

## 1. System Overview

ClawDesk is a multi-agent AI desktop application that:
- Executes LLM-powered agents with tool access (shell, filesystem, browser, MCP)
- Communicates with external LLM providers (OpenAI, Anthropic, Google, etc.)
- Runs MCP servers as child processes
- Stores data locally in SochDB (embedded ACID database)
- Optionally exposes a gateway API (HTTP/WebSocket)

### Trust Boundaries

```
┌─────────────────────────────────────────────────────────────┐
│                    User's Machine (TB-1)                     │
│                                                              │
│  ┌──────────┐    ┌───────────┐    ┌───────────────────┐     │
│  │  Tauri UI │◄──►│  Runtime   │◄──►│  SochDB (local)   │    │
│  │  (TB-2)   │    │  (TB-3)   │    │  (encrypted @rest) │    │
│  └──────────┘    └─────┬─────┘    └───────────────────┘     │
│                        │                                     │
│          ┌─────────────┼──────────────┐                     │
│          ▼             ▼              ▼                      │
│   ┌───────────┐ ┌───────────┐ ┌───────────────┐            │
│   │  Sandbox  │ │ MCP Server│ │ Agent Plugins  │            │
│   │  (TB-4)   │ │  (TB-5)   │ │   (TB-6)      │            │
│   └───────────┘ └───────────┘ └───────────────┘            │
│                        │                                     │
└────────────────────────┼─────────────────────────────────────┘
                         │ HTTPS (TB-7)
           ┌─────────────┼──────────────┐
           ▼             ▼              ▼
    ┌───────────┐ ┌───────────┐ ┌───────────────┐
    │ OpenAI API│ │Anthropic  │ │ MCP Remote    │
    │           │ │   API     │ │ Servers       │
    └───────────┘ └───────────┘ └───────────────┘
```

## 2. Assets

| Asset | Sensitivity | Location |
|-------|------------|----------|
| API keys | CRITICAL | credential_vault (encrypted at rest) |
| User conversations | HIGH | SochDB threads table |
| Agent system prompts | MEDIUM | TOML config files |
| User filesystem | HIGH | Accessible via sandbox |
| MCP server credentials | HIGH | Environment variables / vault |
| Embedding vectors | LOW | SochDB vector store |
| Audit log | HIGH | SochDB audit chain |

## 3. Threat Actors

| Actor | Capability | Motivation |
|-------|-----------|------------|
| **Malicious web content** | Inject prompts via fetched pages | Exfiltrate data, execute commands |
| **Compromised MCP server** | Return malicious tool results | Escalate privileges, inject instructions |
| **Malicious plugin** | Execute arbitrary code if sandbox fails | Steal keys, modify files |
| **Network attacker (MITM)** | Intercept API traffic | Steal API keys, modify responses |
| **Local attacker** | Access local files/processes | Read vault, tamper with SochDB |

## 4. Threats (STRIDE)

### 4.1 Spoofing

| ID | Threat | Boundary | Mitigation | Status |
|----|--------|----------|------------|--------|
| S-1 | Attacker spoofs MCP server identity | TB-5 | TLS + server certificate pinning (`cert_pinning`) | ✅ Implemented |
| S-2 | Forged WebSocket connection | TB-7 | JWT token authentication (`tokens`) | ✅ Implemented |
| S-3 | Plugin impersonates core agent | TB-6 | Plugin capability bitmap + ABI version check (`abi`) | ✅ Implemented |

### 4.2 Tampering

| ID | Threat | Boundary | Mitigation | Status |
|----|--------|----------|------------|--------|
| T-1 | Modified agent TOML config | TB-1 | SHA-256 file integrity check (`skill_verify`) | ✅ Implemented |
| T-2 | Tampered audit log entries | TB-3 | Hash-chained audit log (`audit`) | ✅ Implemented |
| T-3 | Modified SochDB data at rest | TB-3 | ACID transactions + WAL checksums | ✅ SochDB native |
| T-4 | MCP response tampering | TB-5 | JSON-RPC ID correlation + TLS | ✅ Implemented |

### 4.3 Repudiation

| ID | Threat | Boundary | Mitigation | Status |
|----|--------|----------|------------|--------|
| R-1 | Agent denies executing command | TB-3 | Activity journal with sequence numbers (`journal`) | ✅ Implemented |
| R-2 | User denies sending message | TB-2 | Message lineage tracking (`lineage`) | ✅ Implemented |

### 4.4 Information Disclosure

| ID | Threat | Boundary | Mitigation | Status |
|----|--------|----------|------------|--------|
| I-1 | API key leakage in logs | TB-3 | SecretRef redaction (`secret_ref`) | ✅ Implemented |
| I-2 | Prompt injection extracts system prompt | TB-3 | Injection scanner (`injection`) | ✅ Implemented |
| I-3 | Sandbox escape reads ~/.ssh | TB-4 | Filesystem capability policy + path confinement | ✅ Implemented |
| I-4 | Memory/embedding data exposure | TB-3 | Per-session encryption keys planned | 🔲 Planned |

### 4.5 Denial of Service

| ID | Threat | Boundary | Mitigation | Status |
|----|--------|----------|------------|--------|
| D-1 | Runaway agent consumes all tokens | TB-3 | Budget enforcement (`cost_tracking`) | ✅ Implemented |
| D-2 | MCP server hangs indefinitely | TB-5 | Timeout + structured concurrency (`scope`) | ✅ Implemented |
| D-3 | Plugin exhausts memory | TB-6 | Wasm fuel metering + memory limits (`wasm`) | ✅ Implemented |
| D-4 | Prompt injection causes infinite loop | TB-3 | Max iterations + SLO monitoring (`slo`) | ✅ Implemented |

### 4.6 Elevation of Privilege

| ID | Threat | Boundary | Mitigation | Status |
|----|--------|----------|------------|--------|
| E-1 | Agent escapes capability policy | TB-3 | Default-deny capability guard (`capabilities`) | ✅ Implemented |
| E-2 | Tool escalates to root | TB-4 | Subprocess sandbox drops privileges | ✅ Implemented |
| E-3 | Plugin loads arbitrary native code | TB-6 | Wasm-only plugin execution + ABI validation | ✅ Implemented |
| E-4 | Indirect injection via tool output | TB-5 | InputSource-aware injection scanning | ✅ Implemented |

## 5. Data Flow Security

### 5.1 API Key Lifecycle

```
User input → SecretRef::resolve_or_vault()
           → credential_vault (AES-256-GCM encrypted)
           → Provider request (TLS 1.3, cert pinned)
           → Redacted in logs/audit
```

### 5.2 Agent Execution Flow

```
User message → InjectionScanner.scan(User)
             → CapabilityGuard.check(tools)
             → Provider.stream() [TLS]
             → Tool execution [Sandbox]
             → InjectionScanner.scan(ToolOutput)
             → Response assembly
             → AuditLogger.log()
```

### 5.3 MCP Tool Invocation

```
Agent requests tool → CapabilityGuard.check(Tool)
                    → MCP client sends JSON-RPC [TLS/stdio]
                    → Response validated (schema + injection scan)
                    → Result passed to agent
```

## 6. Security Controls Summary

| Control | Module | Layer |
|---------|--------|-------|
| Capability-based access control | `capabilities` | Authorization |
| Prompt injection detection | `injection` | Input validation |
| Sandbox isolation (subprocess/docker/wasm) | `sandbox` | Execution |
| Certificate pinning | `cert_pinning` | Transport |
| Credential vault (AES-256-GCM) | `credential_vault` | Storage |
| Hash-chained audit log | `audit` | Monitoring |
| Command policy engine | `command_policy` | Authorization |
| Execution approval gates | `exec_approval` | Human-in-the-loop |
| OAuth 2.0 flows | `oauth` | Authentication |
| SLO monitoring + alerting | `slo` | Monitoring |
| Budget enforcement | `cost_tracking` | Resource control |
| Structured concurrency | `scope` | Resource control |

## 7. Security Testing

| Test Type | Coverage | Location |
|-----------|----------|----------|
| Property-based tests | Capability guard invariants | `security/tests/property_tests.rs` |
| Unit tests | All security modules | Per-module `#[cfg(test)]` |
| Fuzz harnesses | TOML parsing, JSON-RPC | `fuzz/` (planned) |
| Prompt injection corpus | Known attack patterns | `injection` module tests |

## 8. Residual Risks

1. **LLM provider data retention**: User prompts sent to external APIs may be retained per provider policies.
2. **Local attacker with root**: If the attacker has root on the user's machine, all local defenses can be bypassed.
3. **Novel prompt injection techniques**: Pattern-based detection may miss zero-day injection techniques.
4. **Supply chain attacks**: Dependencies may contain vulnerabilities (mitigated by `cargo-audit` in CI).

## 9. Review Schedule

- **Quarterly**: Review threat model against new features
- **On architecture change**: Update trust boundaries
- **On security incident**: Add new threats and mitigations
