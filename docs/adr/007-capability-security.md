# ADR-007: Capability-Based Security Model

## Status
Accepted

## Context
Agents in ClawDesk can execute arbitrary tools, access files, make network requests, and interact with external services. An unconstrained agent is a security liability — especially when running untrusted community agents or processing adversarial user input. The system needs fine-grained access control without centralized ACLs that become bottlenecks.

## Decision
Adopt a capability-based security model where each agent declares its required capabilities in its TOML config:

```toml
[capabilities]
tools = { allow = ["shell", "file_read"], deny = ["file_write"] }
network = { allow = ["api.openai.com", "*.anthropic.com"], deny = ["*"] }
filesystem = { allow = ["/tmp/clawdesk/**"], deny = ["~/.ssh/**"] }
max_concurrent_tools = 5
```

Enforcement is a two-layer system:
1. **Static policy** — Parsed from agent TOML at load time, immutable during execution
2. **Runtime guard** — `CapabilityGuard` checks every tool invocation against the policy before dispatch

The access control matrix `A[agent, capability] → {allow, deny}` uses `HashSet` for O(1) membership checks. Default-deny: if a capability is not explicitly allowed, it is denied.

## Consequences

### Positive
- Principle of least privilege enforced per-agent
- Default-deny prevents privilege escalation from new tools
- O(1) capability checks add negligible runtime overhead
- Capabilities are self-documenting in agent TOML files
- Community agents are sandboxed by their declared capabilities

### Negative
- Capability declarations must be maintained as tools evolve
- Glob pattern matching for filesystem/network adds parsing complexity
- Users may need to adjust capabilities for custom workflows

### Neutral
- Wasm-based tools (Task 10.1) get an additional sandbox layer from Wasmtime's capability model, forming defense-in-depth

## Alternatives Considered

**Role-Based Access Control (RBAC):** Groups agents into roles with shared permissions. Simpler to manage for large fleets but too coarse-grained — each ClawDesk agent has unique tool needs. Rejected.

**Mandatory Access Control (MAC/SELinux-style):** System-level enforcement is robust but requires OS support and is opaque to users. Rejected for portability.

**No enforcement (trust all agents):** Unacceptable for a system that runs community-authored agents and makes network requests on behalf of users.
