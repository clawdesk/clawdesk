# ClawDesk Security Threat Model

## Document Info

| Field          | Value                        |
|---------------|------------------------------|
| Version       | 1.0                          |
| Status        | Active                       |
| Last Updated  | 2025-01-15                   |
| Authors       | ClawDesk Security Team       |

---

## 1. System Overview

ClawDesk is a Tauri 2.0 desktop application providing a multi-provider AI chat client with a skill/extension ecosystem. It runs locally on macOS, Linux, and Windows.

### Architecture Boundaries

```
┌────────────────────────────────────────────────────────────┐
│  TRUST BOUNDARY: Local Machine                             │
│                                                            │
│  ┌──────────────────┐    IPC     ┌──────────────────────┐  │
│  │ Frontend (WebView)│ <──────> │  Rust Backend (Tauri) │  │
│  │                    │          │                        │  │
│  │ • React/TypeScript │          │ • 138+ IPC Commands   │  │
│  │ • Rendered HTML    │          │ • Skill Executor      │  │
│  │ • User input       │          │ • Credential Vault    │  │
│  └──────────────────┘          │ • Sandbox Policy       │  │
│                                  │ • Audit Logger         │  │
│                                  └──────────┬─────────────┘  │
│                                             │                │
│  ┌──────────────────┐    OS     ┌──────────┴─────────────┐  │
│  │  OS Keychain      │ <─────── │  Filesystem / DB       │  │
│  │  (macOS/GNOME/Win)│          │  (conversations.db)    │  │
│  └──────────────────┘          └──────────────────────────┘  │
│                                                              │
└──────────────────────────────┬───────────────────────────────┘
                               │ HTTPS / TLS 1.2+
               ┌───────────────┼───────────────┐
               │               │               │
        ┌──────▼─────┐  ┌─────▼──────┐  ┌─────▼──────┐
        │  Anthropic  │  │   OpenAI   │  │  Google AI │
        │  API        │  │   API      │  │  API       │
        └────────────┘  └────────────┘  └────────────┘
```

---

## 2. Assets

| ID   | Asset                        | Sensitivity | Location            |
|------|------------------------------|-------------|---------------------|
| A1   | API Keys / Credentials       | Critical    | OS Keychain + RAM   |
| A2   | Conversation History         | High        | SQLite DB on disk   |
| A3   | User Preferences / Config    | Medium      | JSON config files   |
| A4   | Skill Source Code             | Medium      | Skill directories   |
| A5   | Skill Execution Output       | Medium      | RAM (transient)     |
| A6   | Audit Logs                   | High        | Hash-chained log    |
| A7   | Ed25519 Signing Keys         | Critical    | OS Keychain         |
| A8   | Session Tokens               | High        | RAM (transient)     |

---

## 3. Threat Actors

| Actor           | Motivation        | Capability                                 |
|-----------------|-------------------|--------------------------------------------|
| Malicious Skill | Credential theft  | Code execution within sandbox              |
| MITM Attacker   | Data interception | Network position between client and API    |
| Local User      | Curiosity/misuse  | Filesystem access on shared machines       |
| Supply Chain    | Backdoor          | Compromised skill package or dependency    |
| Compromised CA  | MITM              | Issue rogue certificates for API domains   |

---

## 4. Threats and Mitigations

### T1: Credential Exfiltration by Malicious Skill

**STRIDE**: Information Disclosure + Elevation of Privilege

| Aspect     | Detail |
|-----------|--------|
| Attack    | A skill's tool execution attempts to read API keys from environment variables, keychain, or config files. |
| Impact    | **Critical** — Full API key compromise, billing abuse. |
| Likelihood | Medium — Skills run arbitrary prompts that can request tool use. |
| Mitigation | **Sandbox Policy Engine** (`sandbox_policy.rs`) enforces isolation levels. Skills cannot access keychain directly. Environment variables are filtered via `env_passthrough` whitelist. Filesystem access confined to workspace via `PathValidator`. |
| Residual Risk | Low — Would require sandbox escape exploit. |

### T2: Man-in-the-Middle on API Connections

**STRIDE**: Tampering + Information Disclosure

| Aspect     | Detail |
|-----------|--------|
| Attack    | Attacker intercepts HTTPS traffic to API providers (e.g., via compromised proxy, DNS hijack, or rogue CA). |
| Impact    | **High** — API keys and conversation contents exposed. |
| Likelihood | Low (requires network position). |
| Mitigation | **Certificate Pinning** (`cert_pinning.rs`) pins SPKI hashes for known API domains. TLS 1.2+ enforced. Mismatch blocks connection in Enforce mode. |
| Residual Risk | Very Low — Attacker would need the actual server private key. |

### T3: Malicious Skill Package (Supply Chain)

**STRIDE**: Tampering + Elevation of Privilege

| Aspect     | Detail |
|-----------|--------|
| Attack    | Attacker publishes a backdoored skill to the store, or compromises a legitimate skill update. |
| Impact    | **High** — Code execution with sandbox permissions. |
| Likelihood | Medium — Open store ecosystem invites untrusted publishers. |
| Mitigation | **Trust Chain Signing** (`verification.rs`) — Ed25519 signatures, content-hash integrity via SHA-256, publisher key pinning. **Federated Registry** (`federated_registry.rs`) — CAS dedup prevents tampered re-uploads. **Tiered trust** — Unsigned < SelfSigned < CommunityTrusted < OfficialVerified. |
| Residual Risk | Low — Depends on publisher key management. |

### T4: Prompt Injection via Skill Prompts

**STRIDE**: Tampering

| Aspect     | Detail |
|-----------|--------|
| Attack    | A skill's prompt fragment contains instructions that override the system prompt or exfiltrate data. |
| Impact    | **Medium** — Confused model behavior, potential data leakage in responses. |
| Likelihood | Medium — Inherent in LLM prompt concatenation. |
| Mitigation | Skills are assessed for prompt injection risk during verification. Skill prompts are sandboxed within clear delimiters. Content scanner (`scanner.rs`) can flag suspicious patterns. |
| Residual Risk | Medium — Prompt injection is an open research problem. |

### T5: Local Data Theft

**STRIDE**: Information Disclosure

| Aspect     | Detail |
|-----------|--------|
| Attack    | Another user on a shared machine reads conversation database or config files. |
| Impact    | **High** — Conversation privacy compromised. |
| Likelihood | Low (requires local access). |
| Mitigation | Credential vault uses OS keychain (user-session scoped). SQLite DB has user-only permissions. Config files respect platform conventions (XDG, Application Support). |
| Residual Risk | Low — Root/admin can still access. |

### T6: Audit Log Tampering

**STRIDE**: Tampering + Repudiation

| Aspect     | Detail |
|-----------|--------|
| Attack    | Attacker modifies audit logs to hide malicious activity. |
| Impact    | **Medium** — Forensic trail destroyed. |
| Likelihood | Low (requires filesystem access). |
| Mitigation | **Hash-chained audit log** (`audit.rs`) — each entry references the SHA-256 of the previous entry. Tampering breaks the chain and is detectable via `verify_chain()`. |
| Residual Risk | Low — Attacker could truncate (delete) but not silently modify. |

### T7: Auto-Update Hijack

**STRIDE**: Tampering + Elevation of Privilege

| Aspect     | Detail |
|-----------|--------|
| Attack    | Attacker compromises the update server or performs MITM to serve a malicious update binary. |
| Impact    | **Critical** — Full application compromise. |
| Likelihood | Low. |
| Mitigation | **Update manifest is Ed25519-signed** (`updater.rs`). Update binaries verified by SHA-256 checksum. Certificate pinning on `releases.clawdesk.app`. |
| Residual Risk | Very Low — Would require compromise of signing key. |

### T8: Deep Link Injection

**STRIDE**: Tampering

| Aspect     | Detail |
|-----------|--------|
| Attack    | Malicious website triggers `clawdesk://` URI with crafted parameters to install a malicious skill or navigate to a phishing settings page. |
| Impact    | **Medium** — Unwanted skill installation, UI confusion. |
| Likelihood | Medium (deep links triggered from browsers). |
| Mitigation | **Deep link handler** (`deep_link.rs`) — URI length bounded (2048 chars), IDs validated against allowed character set, path traversal blocked. Skill installation from deep links requires user confirmation in the UI. |
| Residual Risk | Low — User must still confirm actions. |

---

## 5. Security Controls Summary

| Control                    | Module                     | Status     |
|---------------------------|----------------------------|------------|
| OS Keychain Integration    | `credential_vault.rs`      | ✅ Active  |
| Secret Reference System    | `secret_ref.rs`            | ✅ Active  |
| Certificate Pinning        | `cert_pinning.rs`          | ✅ Active  |
| Sandbox Policy Engine      | `sandbox_policy.rs`        | ✅ Active  |
| Network ACL                | `sandbox_policy.rs` (D4)   | ✅ Active  |
| Path Validation            | `sandbox_policy.rs` (D4)   | ✅ Active  |
| Rate Limiting              | `sandbox_policy.rs` (D4)   | ✅ Active  |
| Hash-Chained Audit Log     | `audit.rs`                 | ✅ Active  |
| Ed25519 Skill Signing      | `verification.rs`          | ✅ Active  |
| Content Scanner            | `scanner.rs`               | ✅ Active  |
| ACL / RBAC                 | `acl.rs`                   | ✅ Active  |
| Update Signature Verify    | `updater.rs`               | ✅ Active  |
| Deep Link Sanitization     | `deep_link.rs`             | ✅ Active  |

---

## 6. Accepted Risks

1. **Prompt injection** remains partially mitigated. No technical solution can fully prevent LLM prompt injection. We rely on skill vetting and content scanning as defense-in-depth.

2. **Root/admin local access** can bypass all local controls. This is accepted as it is outside ClawDesk's threat model (if the OS is compromised, all bets are off).

3. **Placeholder SPKI pins** in `cert_pinning.rs` must be replaced with actual pins before production deployment.

---

## 7. Review Schedule

- **Quarterly**: Review threat model against new features.
- **On release**: Verify all controls are properly tested.
- **On incident**: Update threat model with new attack vectors.
