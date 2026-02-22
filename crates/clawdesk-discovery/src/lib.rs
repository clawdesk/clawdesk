//! # clawdesk-discovery
//!
//! Network discovery and device pairing for ClawDesk instances.
//!
//! ## Architecture
//! - **mDNS**: Multicast DNS service advertisement (`_clawdesk._tcp.local.`)
//! - **Pairing**: SPAKE2-based password-authenticated key exchange
//! - **Registry**: Discovered peer tracking and health monitoring

pub mod federation;
pub mod mdns;
pub mod pairing;
pub mod registry;

pub use federation::{FederationConfig, FederationEngine, FederatedAgent, CardFetcher};
pub use mdns::{MdnsAdvertiser, MdnsScanner, ServiceInfo};
pub use pairing::{PairingSession, PairingState};
pub use registry::PeerRegistry;
