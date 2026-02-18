//! # clawdesk-tunnel
//!
//! Secure remote access for ClawDesk via embedded WireGuard tunnel.
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────┐
//! │                  ClawDesk Binary                     │
//! │                                                      │
//! │  ┌──────────────┐  ┌──────────────┐  ┌────────────┐ │
//! │  │  WireGuard   │  │   Gateway    │  │   Agent    │ │
//! │  │  Tunnel Mgr  │──│   (Axum)     │──│   Runtime  │ │
//! │  │  (userspace) │  │  127.0.0.1   │  │            │ │
//! │  └──────┬───────┘  └──────────────┘  └────────────┘ │
//! │         │ UDP :51820                                 │
//! │         │ (only port exposed)                        │
//! └─────────┼───────────────────────────────────────────┘
//!           │
//!      ─────┴───── Internet (NAT-traversed)
//!           │
//!    ┌──────┴───────┐
//!    │ Remote Client │
//!    │ (phone/laptop)│
//!    │ WireGuard peer│
//!    └──────────────┘
//! ```
//!
//! ## Security Properties
//!
//! - **Single exposed UDP port**: HTTP gateway binds only to loopback
//! - **Cryptographic packet filter**: unauthenticated packets silently dropped
//! - **In-process tunnel**: no external binaries, no root/admin privileges
//! - **NAT traversal**: UDP hole-punching + STUN
//! - **QR-code invite**: no tokens in URLs, no browser history leakage
//!
//! ## Modules
//!
//! - [`wireguard`]: Core tunnel manager (userspace WireGuard engine)
//! - [`peer`]: Peer management, key exchange, invite flow
//! - [`nat`]: NAT traversal via STUN + UDP hole-punching
//! - [`discovery`]: Peer discovery via QR codes and invite links
//! - [`metrics`]: Per-peer bandwidth/latency tracking

pub mod discovery;
pub mod metrics;
pub mod nat;
pub mod peer;
pub mod wireguard;

pub use discovery::PeerInvite;
pub use metrics::{PeerMetrics, TunnelMetrics};
pub use nat::{NatStrategy, NatType};
pub use peer::{PeerConfig, PeerManager, PeerState, PeerStatus};
pub use wireguard::{TunnelConfig, TunnelError, TunnelManager};
