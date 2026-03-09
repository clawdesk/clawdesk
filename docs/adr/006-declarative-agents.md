# ADR-006: Declarative Agent Configuration

## Status
Accepted

## Context
ClawDesk supports multiple specialized agents (coder, researcher, debugger, etc.). Each agent has a unique combination of: model routing, system prompt, personality traits, tool permissions, resource limits, and channel-specific overrides. Hard-coding these in Rust requires recompilation for every persona change.

## Decision
Define agents declaratively via TOML files in the `agents/` directory. Each file specifies the full agent configuration as a product type:

```
AgentConfig = AgentIdentity × ModelConfig × SystemPromptConfig × TraitConfig
             × CapabilityConfig × ResourceConfig × Map<Channel, ChannelOverride>
             × BootstrapConfig × MetadataConfig
```

The `clawdesk-agent-config` crate provides:
1. **Schema** — Rust types mapping 1:1 to TOML structure
2. **Registry** — `DashMap<AgentId, Arc<AgentConfig>>` for O(1) concurrent access
3. **Loader** — Directory scanner with validation
4. **Watcher** — `notify`-based hot-reload (Create/Modify → upsert, Remove → delete)

## Consequences

### Positive
- Add/modify agents without recompiling — hot-reload on file change
- Non-engineers can author agents by copying a template
- Marketplace distribution as single `.toml` files
- Validation at load time catches errors early
- DashMap provides lock-free concurrent reads

### Negative
- TOML parsing adds startup latency (~1ms per agent file)
- Schema must be versioned to handle format evolution
- File watching adds a background task and OS-specific complexity

### Neutral
- 39 bundled agents provide broad coverage; community can extend

## Alternatives Considered

**Rust enums (compile-time agents):** Type-safe but requires recompilation. Rejected for flexibility.

**YAML/JSON configuration:** YAML is error-prone (indentation); JSON lacks comments. TOML chosen for readability and Rust ecosystem support.

**Database-stored agents:** Would work for server mode but complicates desktop/CLI deployment. Files chosen as the universal denominator.
