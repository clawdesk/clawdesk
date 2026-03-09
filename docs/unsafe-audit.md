# Unsafe Code Audit Report

Date: 2025-01-XX  
Scope: All `unsafe` blocks in `crates/*/src/**/*.rs`

## Summary

| Crate | Count | Category | Status |
|-------|-------|----------|--------|
| clawdesk-simd | 7 | SIMD intrinsics (AVX2) | ✅ Justified — guarded by `is_x86_feature_detected!` |
| clawdesk-acp/streaming | 4 | SPSC ring buffer (UnsafeCell) | ✅ Justified — single-producer single-consumer invariant upheld by atomic head/tail |
| clawdesk-acp/streaming | 2 | `unsafe impl Send/Sync` for SpscRing | ✅ Justified — atomics enforce thread-safe access |
| clawdesk-adapters/circuit_breaker | 2 | `unsafe impl Send/Sync` for AtomicBucket, CircuitBreaker | ✅ Justified — interior mutability via atomics only |
| clawdesk-agents | 3 | `libc::kill` for process probing/termination | ✅ Justified — POSIX process management, PID validated |
| clawdesk-daemon/pid | 1 | `libc::kill(pid, 0)` for liveness check | ✅ Justified — standard PID file guard |
| clawdesk-infra/task_scope | 3 | `Pin::map_unchecked_mut`, `unsafe impl Send` | ⚠️ Review — unwind safety wrappers, correctness depends on Future pinning |
| clawdesk-media | 2 | `unsafe impl Send/Sync` for WhisperContextHolder | ✅ Justified — FFI handle not accessed concurrently |
| clawdesk-media/recorder | 1 | `unsafe impl Send` for AudioRecorder | ✅ Justified — platform audio handle moved between threads |
| clawdesk-tauri/pty_session | 5 | `libc::fork`, `libc::setsid`, `libc::dup2`, `libc::execvp`, PTY setup | ✅ Justified — standard Unix PTY pseudoterminal creation |

## Audit Findings

### No Critical Issues Found

All `unsafe` blocks fall into well-known patterns:

1. **SIMD intrinsics** (clawdesk-simd): Guarded by runtime CPU feature detection (`is_x86_feature_detected!("avx2")`). Falls back to safe scalar implementation when AVX2 unavailable. Slice length equality checked before entry.

2. **Lock-free data structures** (clawdesk-acp): `UnsafeCell` access in SPSC ring buffer follows established single-producer/single-consumer pattern with acquire/release ordering on atomic head/tail indices. `Send + Sync` impls are sound because the ring is designed for cross-thread use.

3. **Process management** (`libc::kill`): All PIDs sourced from known process spawns or PID files. Signal 0 (existence check) and SIGTERM (graceful stop) only.

4. **PTY/fork** (clawdesk-tauri): Standard `forkpty` pattern for terminal emulation. Post-fork child calls `execvp` immediately; parent tracks child PID.

5. **Unwind safety wrappers** (clawdesk-infra): `Pin::map_unchecked_mut` used to implement `FutureUnwindSafe` wrapper. The wrapper ensures the inner future is not moved once pinned.

### Recommendations

- [ ] Add `// SAFETY:` comments to all `unsafe` blocks that lack them (clawdesk-simd functions are well-documented; others could use brief safety annotations)
- [ ] Consider `#![deny(unsafe_op_in_unsafe_fn)]` in clawdesk-simd for maximum rigor
- [ ] The `libc::kill` calls in 3 crates duplicate the same PID-liveness pattern — consider extracting to a shared `process::is_alive(pid)` function in clawdesk-infra
