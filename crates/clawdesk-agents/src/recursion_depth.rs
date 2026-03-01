//! Inter-Agent Recursion Depth Tracking.
//!
//! Prevents runaway recursive agent calls (A spawns B spawns A spawns B...) by
//! tracking the call depth via `tokio::task_local!`. Each agent invocation
//! increments the depth counter; if it exceeds `MAX_AGENT_CALL_DEPTH`, the
//! call is rejected.
//!
//! ## Usage
//!
//! ```ignore
//! use clawdesk_agents::recursion_depth::{with_incremented_depth, current_depth, MAX_AGENT_CALL_DEPTH};
//!
//! // At the point of spawning a sub-agent:
//! let depth = current_depth();
//! if depth >= MAX_AGENT_CALL_DEPTH {
//!     return Err("max agent recursion depth exceeded");
//! }
//! with_incremented_depth(async {
//!     sub_agent.run(messages, prompt).await
//! }).await
//! ```

use std::future::Future;

/// Maximum allowed agent call depth. Exceeding this returns an error.
///
/// Depth 5 allows reasonable fan-out (A→B→C→D→E) while preventing
/// infinite recursion between agents.
pub const MAX_AGENT_CALL_DEPTH: u32 = 5;

tokio::task_local! {
    /// Current agent call depth (0 at the top level).
    static AGENT_CALL_DEPTH: u32;
}

/// Get the current agent call depth. Returns 0 if not inside an agent context.
pub fn current_depth() -> u32 {
    AGENT_CALL_DEPTH.try_with(|d| *d).unwrap_or(0)
}

/// Execute a future with the agent call depth incremented by 1.
///
/// This should be called when spawning a sub-agent or delegating to
/// another agent. The depth is automatically decremented when the
/// future completes (via task_local scoping).
pub async fn with_incremented_depth<F, T>(f: F) -> T
where
    F: Future<Output = T>,
{
    let new_depth = current_depth() + 1;
    AGENT_CALL_DEPTH.scope(new_depth, f).await
}

/// Check if spawning another agent would exceed the maximum depth.
///
/// Returns `Ok(current_depth)` if another level is allowed, or
/// `Err(reason)` if the maximum would be exceeded.
pub fn check_depth() -> Result<u32, RecursionDepthError> {
    let depth = current_depth();
    if depth >= MAX_AGENT_CALL_DEPTH {
        Err(RecursionDepthError {
            current_depth: depth,
            max_depth: MAX_AGENT_CALL_DEPTH,
        })
    } else {
        Ok(depth)
    }
}

/// Error returned when agent recursion depth would be exceeded.
#[derive(Debug, Clone)]
pub struct RecursionDepthError {
    pub current_depth: u32,
    pub max_depth: u32,
}

impl std::fmt::Display for RecursionDepthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "agent recursion depth {} exceeds maximum {}",
            self.current_depth, self.max_depth
        )
    }
}

impl std::error::Error for RecursionDepthError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_initial_depth_is_zero() {
        assert_eq!(current_depth(), 0);
    }

    #[tokio::test]
    async fn test_incremented_depth() {
        assert_eq!(current_depth(), 0);
        with_incremented_depth(async {
            assert_eq!(current_depth(), 1);
            with_incremented_depth(async {
                assert_eq!(current_depth(), 2);
            })
            .await;
            assert_eq!(current_depth(), 1);
        })
        .await;
        assert_eq!(current_depth(), 0);
    }

    #[tokio::test]
    async fn test_check_depth_ok() {
        assert!(check_depth().is_ok());
        let depth = check_depth().unwrap();
        assert_eq!(depth, 0);
    }

    #[tokio::test]
    async fn test_check_depth_exceeded() {
        // Nest to MAX_AGENT_CALL_DEPTH
        async fn nest(depth: u32) {
            if depth >= MAX_AGENT_CALL_DEPTH {
                assert!(check_depth().is_err());
                let err = check_depth().unwrap_err();
                assert_eq!(err.current_depth, MAX_AGENT_CALL_DEPTH);
                return;
            }
            with_incremented_depth(async move {
                Box::pin(nest(depth + 1)).await;
            })
            .await;
        }
        nest(0).await;
    }

    #[tokio::test]
    async fn test_depth_restores_after_scope() {
        with_incremented_depth(async {
            assert_eq!(current_depth(), 1);
        })
        .await;
        // Back to 0 after scope exits
        assert_eq!(current_depth(), 0);
    }
}
