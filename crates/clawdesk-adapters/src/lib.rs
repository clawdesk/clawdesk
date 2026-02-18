//! # clawdesk-adapters
//!
//! External service adapter trait with OAuth2 token lifecycle,
//! rate limiting (token bucket), and circuit breaking (sliding window).
//!
//! ## Architecture
//!
//! - **ServiceAdapter trait**: 3 methods (poll, push, configure)
//! - **OAuthManager**: Token refresh with OnceCell coalescing (no thundering herd)
//! - **TokenBucket**: O(1) rate limiting per adapter
//! - **CircuitBreaker**: Sliding window counter with exponential backoff
//! - **Social module**: Platform-specific metric collectors with EWMA trend detection

pub mod circuit_breaker;
pub mod oauth;
pub mod rate_limit;
pub mod service;
pub mod social;
