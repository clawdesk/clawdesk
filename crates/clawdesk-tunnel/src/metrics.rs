//! Per-peer and tunnel-wide bandwidth/latency metrics.
//!
//! All counters are lock-free `AtomicU64`. Reads are `Relaxed` for
//! dashboard display; authoritative values use `Acquire` ordering.
//!
//! # Memory budget
//!
//! Each `PeerMetrics` is 128 bytes (cacheline-aligned). For 256 peers:
//! 256 × 128 = 32 KB — fits in L1 cache of modern CPUs.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// ── Per-Peer Metrics ─────────────────────────────────────────

/// Per-peer bandwidth and latency counters.
///
/// All fields are atomically updated — no locks required.
///
/// Layout: hot counters first (rx/tx bytes/packets updated on every
/// packet), cold fields later (handshake counters, last_seen).
#[repr(C, align(64))]
pub struct PeerMetrics {
    // Hot path — updated on every packet
    /// Total bytes received from this peer.
    pub rx_bytes: AtomicU64,
    /// Total bytes transmitted to this peer.
    pub tx_bytes: AtomicU64,
    /// Total packets received from this peer.
    pub rx_packets: AtomicU64,
    /// Total packets transmitted to this peer.
    pub tx_packets: AtomicU64,
    /// Last seen timestamp (Unix nanos).
    pub last_seen_ns: AtomicU64,

    // Cold path — updated on handshakes and errors
    /// Number of successful handshakes.
    pub handshakes: AtomicU64,
    /// Number of failed handshakes.
    pub handshake_failures: AtomicU64,
    /// Number of dropped packets (replay, malformed, etc.).
    pub dropped_packets: AtomicU64,
    /// Last handshake timestamp (Unix nanos).
    pub last_handshake_ns: AtomicU64,
    /// Smoothed RTT estimate in microseconds (EWMA α=0.125).
    pub rtt_us: AtomicU64,
    /// RTT variance in microseconds (EWMA β=0.25).
    pub rtt_var_us: AtomicU64,
    /// Keepalive packets sent.
    pub keepalives_sent: AtomicU64,
}

impl PeerMetrics {
    /// Create zeroed metrics.
    pub fn new() -> Self {
        Self {
            rx_bytes: AtomicU64::new(0),
            tx_bytes: AtomicU64::new(0),
            rx_packets: AtomicU64::new(0),
            tx_packets: AtomicU64::new(0),
            last_seen_ns: AtomicU64::new(0),
            handshakes: AtomicU64::new(0),
            handshake_failures: AtomicU64::new(0),
            dropped_packets: AtomicU64::new(0),
            last_handshake_ns: AtomicU64::new(0),
            rtt_us: AtomicU64::new(0),
            rtt_var_us: AtomicU64::new(0),
            keepalives_sent: AtomicU64::new(0),
        }
    }

    // ── Hot-path recording ──────────────────────────────────

    /// Record an incoming packet.
    #[inline]
    pub fn record_rx(&self, bytes: u64) {
        self.rx_bytes.fetch_add(bytes, Ordering::Relaxed);
        self.rx_packets.fetch_add(1, Ordering::Relaxed);
        self.last_seen_ns.store(now_nanos(), Ordering::Relaxed);
    }

    /// Record an outgoing packet.
    #[inline]
    pub fn record_tx(&self, bytes: u64) {
        self.tx_bytes.fetch_add(bytes, Ordering::Relaxed);
        self.tx_packets.fetch_add(1, Ordering::Relaxed);
    }

    // ── Cold-path recording ─────────────────────────────────

    /// Record a successful handshake.
    pub fn record_handshake(&self) {
        self.handshakes.fetch_add(1, Ordering::Relaxed);
        self.last_handshake_ns.store(now_nanos(), Ordering::Relaxed);
    }

    /// Record a failed handshake.
    pub fn record_handshake_failure(&self) {
        self.handshake_failures.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a dropped packet.
    pub fn record_drop(&self) {
        self.dropped_packets.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a keepalive sent.
    pub fn record_keepalive(&self) {
        self.keepalives_sent.fetch_add(1, Ordering::Relaxed);
    }

    /// Update the smoothed RTT using TCP-style EWMA.
    ///
    /// SRTT = (1 - α) * SRTT + α * sample  where α = 1/8
    /// RTTVAR = (1 - β) * RTTVAR + β * |SRTT - sample|  where β = 1/4
    pub fn update_rtt(&self, sample_us: u64) {
        let old_rtt = self.rtt_us.load(Ordering::Relaxed);
        let old_var = self.rtt_var_us.load(Ordering::Relaxed);

        if old_rtt == 0 {
            // First sample: initialize
            self.rtt_us.store(sample_us, Ordering::Relaxed);
            self.rtt_var_us.store(sample_us / 2, Ordering::Relaxed);
        } else {
            // EWMA update
            let diff = if sample_us > old_rtt {
                sample_us - old_rtt
            } else {
                old_rtt - sample_us
            };

            // new_var = (3/4) * old_var + (1/4) * diff
            let new_var = (old_var * 3 + diff) / 4;
            // new_rtt = (7/8) * old_rtt + (1/8) * sample
            let new_rtt = (old_rtt * 7 + sample_us) / 8;

            self.rtt_var_us.store(new_var, Ordering::Relaxed);
            self.rtt_us.store(new_rtt, Ordering::Relaxed);
        }
    }

    // ── Snapshot ─────────────────────────────────────────────

    /// Take a consistent snapshot of all counters.
    pub fn snapshot(&self) -> PeerMetricsSnapshot {
        PeerMetricsSnapshot {
            rx_bytes: self.rx_bytes.load(Ordering::Acquire),
            tx_bytes: self.tx_bytes.load(Ordering::Acquire),
            rx_packets: self.rx_packets.load(Ordering::Acquire),
            tx_packets: self.tx_packets.load(Ordering::Acquire),
            last_seen_ns: self.last_seen_ns.load(Ordering::Acquire),
            handshakes: self.handshakes.load(Ordering::Acquire),
            handshake_failures: self.handshake_failures.load(Ordering::Acquire),
            dropped_packets: self.dropped_packets.load(Ordering::Acquire),
            last_handshake_ns: self.last_handshake_ns.load(Ordering::Acquire),
            rtt_us: self.rtt_us.load(Ordering::Acquire),
            rtt_var_us: self.rtt_var_us.load(Ordering::Acquire),
            keepalives_sent: self.keepalives_sent.load(Ordering::Acquire),
        }
    }

    /// Reset all counters.
    pub fn reset(&self) {
        self.rx_bytes.store(0, Ordering::Relaxed);
        self.tx_bytes.store(0, Ordering::Relaxed);
        self.rx_packets.store(0, Ordering::Relaxed);
        self.tx_packets.store(0, Ordering::Relaxed);
        self.last_seen_ns.store(0, Ordering::Relaxed);
        self.handshakes.store(0, Ordering::Relaxed);
        self.handshake_failures.store(0, Ordering::Relaxed);
        self.dropped_packets.store(0, Ordering::Relaxed);
        self.last_handshake_ns.store(0, Ordering::Relaxed);
        self.rtt_us.store(0, Ordering::Relaxed);
        self.rtt_var_us.store(0, Ordering::Relaxed);
        self.keepalives_sent.store(0, Ordering::Relaxed);
    }
}

impl Default for PeerMetrics {
    fn default() -> Self {
        Self::new()
    }
}

// ── Peer Metrics Snapshot ────────────────────────────────────

/// A serializable snapshot of a peer's metrics at a point in time.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PeerMetricsSnapshot {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_packets: u64,
    pub tx_packets: u64,
    pub last_seen_ns: u64,
    pub handshakes: u64,
    pub handshake_failures: u64,
    pub dropped_packets: u64,
    pub last_handshake_ns: u64,
    pub rtt_us: u64,
    pub rtt_var_us: u64,
    pub keepalives_sent: u64,
}

impl PeerMetricsSnapshot {
    /// Total bytes (rx + tx).
    pub fn total_bytes(&self) -> u64 {
        self.rx_bytes + self.tx_bytes
    }

    /// Total packets (rx + tx).
    pub fn total_packets(&self) -> u64 {
        self.rx_packets + self.tx_packets
    }

    /// RTT in milliseconds (f64 for precision).
    pub fn rtt_ms(&self) -> f64 {
        self.rtt_us as f64 / 1000.0
    }

    /// Time since last seen, or None if never seen.
    pub fn time_since_last_seen(&self) -> Option<Duration> {
        if self.last_seen_ns == 0 {
            return None;
        }
        let now = now_nanos();
        if now > self.last_seen_ns {
            Some(Duration::from_nanos(now - self.last_seen_ns))
        } else {
            Some(Duration::ZERO)
        }
    }

    /// Whether this peer appears idle (no packets for > threshold).
    pub fn is_idle(&self, threshold: Duration) -> bool {
        match self.time_since_last_seen() {
            Some(elapsed) => elapsed > threshold,
            None => true, // Never seen = idle
        }
    }
}

// ── Tunnel-Wide Metrics ──────────────────────────────────────

/// Aggregate metrics across all peers in the tunnel.
#[repr(C, align(64))]
pub struct TunnelMetrics {
    /// Total bytes received across all peers.
    pub total_rx_bytes: AtomicU64,
    /// Total bytes transmitted across all peers.
    pub total_tx_bytes: AtomicU64,
    /// Total packets received across all peers.
    pub total_rx_packets: AtomicU64,
    /// Total packets transmitted across all peers.
    pub total_tx_packets: AtomicU64,
    /// Total dropped packets across all peers.
    pub total_dropped: AtomicU64,
    /// Total successful handshakes.
    pub total_handshakes: AtomicU64,
    /// Active peer count (updated externally).
    pub active_peers: AtomicU64,
    /// Timestamp when the tunnel was started (Unix nanos).
    pub started_at_ns: AtomicU64,
}

impl TunnelMetrics {
    /// Create zeroed tunnel metrics and record start time.
    pub fn new() -> Self {
        Self {
            total_rx_bytes: AtomicU64::new(0),
            total_tx_bytes: AtomicU64::new(0),
            total_rx_packets: AtomicU64::new(0),
            total_tx_packets: AtomicU64::new(0),
            total_dropped: AtomicU64::new(0),
            total_handshakes: AtomicU64::new(0),
            active_peers: AtomicU64::new(0),
            started_at_ns: AtomicU64::new(now_nanos()),
        }
    }

    /// Record an incoming packet globally.
    #[inline]
    pub fn record_rx(&self, bytes: u64) {
        self.total_rx_bytes.fetch_add(bytes, Ordering::Relaxed);
        self.total_rx_packets.fetch_add(1, Ordering::Relaxed);
    }

    /// Record an outgoing packet globally.
    #[inline]
    pub fn record_tx(&self, bytes: u64) {
        self.total_tx_bytes.fetch_add(bytes, Ordering::Relaxed);
        self.total_tx_packets.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a dropped packet.
    pub fn record_drop(&self) {
        self.total_dropped.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a handshake.
    pub fn record_handshake(&self) {
        self.total_handshakes.fetch_add(1, Ordering::Relaxed);
    }

    /// Set the active peer count.
    pub fn set_active_peers(&self, count: u64) {
        self.active_peers.store(count, Ordering::Relaxed);
    }

    /// Take a snapshot.
    pub fn snapshot(&self) -> TunnelMetricsSnapshot {
        TunnelMetricsSnapshot {
            total_rx_bytes: self.total_rx_bytes.load(Ordering::Acquire),
            total_tx_bytes: self.total_tx_bytes.load(Ordering::Acquire),
            total_rx_packets: self.total_rx_packets.load(Ordering::Acquire),
            total_tx_packets: self.total_tx_packets.load(Ordering::Acquire),
            total_dropped: self.total_dropped.load(Ordering::Acquire),
            total_handshakes: self.total_handshakes.load(Ordering::Acquire),
            active_peers: self.active_peers.load(Ordering::Acquire),
            started_at_ns: self.started_at_ns.load(Ordering::Acquire),
        }
    }

    /// Uptime since tunnel started.
    pub fn uptime(&self) -> Duration {
        let started = self.started_at_ns.load(Ordering::Relaxed);
        let now = now_nanos();
        if now > started {
            Duration::from_nanos(now - started)
        } else {
            Duration::ZERO
        }
    }

    /// Compute bytes/second throughput given a time window (approximate).
    pub fn throughput_bps(&self, window: Duration) -> (f64, f64) {
        let secs = window.as_secs_f64();
        if secs == 0.0 {
            return (0.0, 0.0);
        }
        let rx = self.total_rx_bytes.load(Ordering::Relaxed) as f64 / secs;
        let tx = self.total_tx_bytes.load(Ordering::Relaxed) as f64 / secs;
        (rx, tx)
    }

    /// Reset all counters (keeps start time).
    pub fn reset(&self) {
        self.total_rx_bytes.store(0, Ordering::Relaxed);
        self.total_tx_bytes.store(0, Ordering::Relaxed);
        self.total_rx_packets.store(0, Ordering::Relaxed);
        self.total_tx_packets.store(0, Ordering::Relaxed);
        self.total_dropped.store(0, Ordering::Relaxed);
        self.total_handshakes.store(0, Ordering::Relaxed);
    }
}

impl Default for TunnelMetrics {
    fn default() -> Self {
        Self::new()
    }
}

/// Serializable snapshot of tunnel-wide metrics.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TunnelMetricsSnapshot {
    pub total_rx_bytes: u64,
    pub total_tx_bytes: u64,
    pub total_rx_packets: u64,
    pub total_tx_packets: u64,
    pub total_dropped: u64,
    pub total_handshakes: u64,
    pub active_peers: u64,
    pub started_at_ns: u64,
}

impl TunnelMetricsSnapshot {
    /// Total bandwidth (rx + tx) in bytes.
    pub fn total_bandwidth(&self) -> u64 {
        self.total_rx_bytes + self.total_tx_bytes
    }

    /// Uptime duration.
    pub fn uptime(&self) -> Duration {
        let now = now_nanos();
        if now > self.started_at_ns {
            Duration::from_nanos(now - self.started_at_ns)
        } else {
            Duration::ZERO
        }
    }
}

// ── Helpers ─────────────────────────────────────────────────

/// Current time in nanoseconds since UNIX epoch.
#[inline]
fn now_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_metrics_record_rx_tx() {
        let m = PeerMetrics::new();
        m.record_rx(100);
        m.record_rx(200);
        m.record_tx(50);

        let snap = m.snapshot();
        assert_eq!(snap.rx_bytes, 300);
        assert_eq!(snap.tx_bytes, 50);
        assert_eq!(snap.rx_packets, 2);
        assert_eq!(snap.tx_packets, 1);
        assert!(snap.last_seen_ns > 0);
    }

    #[test]
    fn peer_metrics_handshakes() {
        let m = PeerMetrics::new();
        m.record_handshake();
        m.record_handshake();
        m.record_handshake_failure();

        let snap = m.snapshot();
        assert_eq!(snap.handshakes, 2);
        assert_eq!(snap.handshake_failures, 1);
        assert!(snap.last_handshake_ns > 0);
    }

    #[test]
    fn peer_metrics_rtt_first_sample() {
        let m = PeerMetrics::new();
        m.update_rtt(1000); // 1ms

        let snap = m.snapshot();
        assert_eq!(snap.rtt_us, 1000);
        assert_eq!(snap.rtt_var_us, 500);
    }

    #[test]
    fn peer_metrics_rtt_ewma() {
        let m = PeerMetrics::new();
        m.update_rtt(1000); // init
        m.update_rtt(1200);

        let snap = m.snapshot();
        // new_rtt = (7 * 1000 + 1200) / 8 = 1025
        assert_eq!(snap.rtt_us, 1025);
        // new_var = (3 * 500 + 200) / 4 = 425
        assert_eq!(snap.rtt_var_us, 425);
    }

    #[test]
    fn peer_metrics_reset() {
        let m = PeerMetrics::new();
        m.record_rx(100);
        m.record_tx(50);
        m.record_handshake();
        m.reset();

        let snap = m.snapshot();
        assert_eq!(snap.rx_bytes, 0);
        assert_eq!(snap.tx_bytes, 0);
        assert_eq!(snap.handshakes, 0);
    }

    #[test]
    fn peer_snapshot_totals() {
        let snap = PeerMetricsSnapshot {
            rx_bytes: 1000,
            tx_bytes: 500,
            rx_packets: 10,
            tx_packets: 5,
            last_seen_ns: 0,
            handshakes: 0,
            handshake_failures: 0,
            dropped_packets: 0,
            last_handshake_ns: 0,
            rtt_us: 2000,
            rtt_var_us: 0,
            keepalives_sent: 0,
        };

        assert_eq!(snap.total_bytes(), 1500);
        assert_eq!(snap.total_packets(), 15);
        assert!((snap.rtt_ms() - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn peer_snapshot_idle_detection() {
        let snap = PeerMetricsSnapshot {
            rx_bytes: 0,
            tx_bytes: 0,
            rx_packets: 0,
            tx_packets: 0,
            last_seen_ns: 0, // never seen
            handshakes: 0,
            handshake_failures: 0,
            dropped_packets: 0,
            last_handshake_ns: 0,
            rtt_us: 0,
            rtt_var_us: 0,
            keepalives_sent: 0,
        };

        assert!(snap.is_idle(Duration::from_secs(60)));
        assert!(snap.time_since_last_seen().is_none());
    }

    #[test]
    fn tunnel_metrics_basic() {
        let tm = TunnelMetrics::new();
        tm.record_rx(500);
        tm.record_tx(300);
        tm.record_drop();
        tm.record_handshake();
        tm.set_active_peers(3);

        let snap = tm.snapshot();
        assert_eq!(snap.total_rx_bytes, 500);
        assert_eq!(snap.total_tx_bytes, 300);
        assert_eq!(snap.total_rx_packets, 1);
        assert_eq!(snap.total_tx_packets, 1);
        assert_eq!(snap.total_dropped, 1);
        assert_eq!(snap.total_handshakes, 1);
        assert_eq!(snap.active_peers, 3);
        assert_eq!(snap.total_bandwidth(), 800);
    }

    #[test]
    fn tunnel_metrics_uptime() {
        let tm = TunnelMetrics::new();
        let uptime = tm.uptime();
        // Should be very small since we just created it
        assert!(uptime.as_millis() < 100);
    }

    #[test]
    fn tunnel_metrics_reset() {
        let tm = TunnelMetrics::new();
        tm.record_rx(1000);
        tm.record_tx(500);
        tm.reset();

        let snap = tm.snapshot();
        assert_eq!(snap.total_rx_bytes, 0);
        assert_eq!(snap.total_tx_bytes, 0);
        // Start time should be preserved
        assert!(snap.started_at_ns > 0);
    }

    #[test]
    fn tunnel_throughput() {
        let tm = TunnelMetrics::new();
        // Simulate 1 MB sent in 1 second
        tm.total_rx_bytes.store(1_000_000, Ordering::Relaxed);
        tm.total_tx_bytes.store(500_000, Ordering::Relaxed);

        let (rx_bps, tx_bps) = tm.throughput_bps(Duration::from_secs(10));
        assert!((rx_bps - 100_000.0).abs() < 1.0);
        assert!((tx_bps - 50_000.0).abs() < 1.0);
    }

    #[test]
    fn concurrent_metrics() {
        use std::sync::Arc;

        let m = Arc::new(PeerMetrics::new());
        let mut handles = Vec::new();

        for _ in 0..4 {
            let m = Arc::clone(&m);
            handles.push(std::thread::spawn(move || {
                for _ in 0..1000 {
                    m.record_rx(1);
                    m.record_tx(1);
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        let snap = m.snapshot();
        assert_eq!(snap.rx_bytes, 4000);
        assert_eq!(snap.tx_bytes, 4000);
        assert_eq!(snap.rx_packets, 4000);
        assert_eq!(snap.tx_packets, 4000);
    }
}
