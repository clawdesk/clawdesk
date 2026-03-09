# ADR-003: Agent Communication Protocol (ACP)

## Status
Accepted

## Context
Multi-agent collaboration requires a protocol for agents to discover each other's capabilities, negotiate contracts, exchange messages, and maintain liveness. Existing options include raw HTTP/gRPC, A2A (Google's Agent-to-Agent), and custom protocols.

## Decision
Implement ACP (Agent Communication Protocol) as a capability-algebra-based protocol with:
- **Agent cards** for capability advertisement
- **Capability lattice** (meet, join, partial order) for negotiation
- **Contracts** for binding agreements between agents
- **Heartbeats** for liveness detection
- **Delta streams** for progressive result delivery

## Consequences

### Positive
- Capability algebra provides formal guarantees about negotiation outcomes (lattice properties)
- Agent cards enable zero-config discovery of compatible agents
- Heartbeat protocol detects agent failures within configurable timeout
- Delta streams support long-running collaborative tasks

### Negative
- Custom protocol requires custom tooling (no off-the-shelf monitoring)
- More complex than simple HTTP RPC for basic agent-to-agent calls
- Capability lattice operations add ~10μs overhead per negotiation step

### Neutral
- Compatible with A2A protocol via adapter layer in `clawdesk-discovery`

## Alternatives Considered

**Raw gRPC:** Efficient but provides no capability negotiation or contract semantics. Would need to build all coordination logic from scratch.

**A2A (Google):** Closer to our needs but lacks the capability algebra for formal negotiation. ACP extends A2A concepts with algebraic foundations.
