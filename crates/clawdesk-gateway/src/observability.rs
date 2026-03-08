//! Observability module — metrics collection, SSE streaming, and embedded dashboard.
//!
//! ## Architecture
//!
//! ```text
//! ┌──────────────┐    record()    ┌────────────────┐   SSE    ┌───────────────┐
//! │  Middleware   │───────────────▶│ MetricsCollector│────────▶│ /admin/events │
//! │  Handlers    │                │ (DashMap-based) │         │  (EventSource)│
//! │  Workers     │                └───────┬────────┘         └───────────────┘
//! └──────────────┘                        │ snapshot()
//!                                         ▼
//!                                  ┌──────────────┐
//!                                  │ GET /metrics  │
//!                                  │ GET /dashboard│
//!                                  └──────────────┘
//! ```
//!
//! ## Metric types
//!
//! - **Counter** — monotonically increasing (e.g., `requests_total`)
//! - **Gauge** — point-in-time value (e.g., `active_sessions`)
//! - **Histogram** — distribution with percentiles (e.g., `request_duration_ms`)

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use tracing::debug;

// ---------------------------------------------------------------------------
// Metric types
// ---------------------------------------------------------------------------

/// A named counter that only goes up.
#[derive(Debug)]
pub struct Counter {
    pub name: String,
    pub description: String,
    value: AtomicU64,
}

impl Counter {
    pub fn new(name: impl Into<String>, desc: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: desc.into(),
            value: AtomicU64::new(0),
        }
    }

    pub fn increment(&self) {
        self.value.fetch_add(1, Ordering::Relaxed);
    }

    pub fn add(&self, n: u64) {
        self.value.fetch_add(n, Ordering::Relaxed);
    }

    pub fn get(&self) -> u64 {
        self.value.load(Ordering::Relaxed)
    }
}

/// A gauge that can go up and down.
#[derive(Debug)]
pub struct Gauge {
    pub name: String,
    pub description: String,
    value: AtomicU64,
}

impl Gauge {
    pub fn new(name: impl Into<String>, desc: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: desc.into(),
            value: AtomicU64::new(0),
        }
    }

    pub fn set(&self, val: u64) {
        self.value.store(val, Ordering::Relaxed);
    }

    pub fn increment(&self) {
        self.value.fetch_add(1, Ordering::Relaxed);
    }

    pub fn decrement(&self) {
        self.value.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn get(&self) -> u64 {
        self.value.load(Ordering::Relaxed)
    }
}

/// A histogram that tracks value distribution with O(1) recording.
///
/// Uses a fixed set of buckets for percentile estimation.
#[derive(Debug)]
pub struct Histogram {
    pub name: String,
    pub description: String,
    /// Fixed bucket boundaries (e.g., [1, 5, 10, 25, 50, 100, 250, 500, 1000, 5000]).
    buckets: Vec<u64>,
    /// Count per bucket (bucket[i] = values ≤ buckets[i]).
    counts: Vec<AtomicU64>,
    /// Overflow bucket (values > last bucket boundary).
    overflow: AtomicU64,
    /// Sum of all recorded values.
    sum: AtomicU64,
    /// Total count of observations.
    total: AtomicU64,
}

impl Histogram {
    pub fn new(name: impl Into<String>, desc: impl Into<String>, buckets: Vec<u64>) -> Self {
        let counts: Vec<AtomicU64> = buckets.iter().map(|_| AtomicU64::new(0)).collect();
        Self {
            name: name.into(),
            description: desc.into(),
            buckets,
            counts,
            overflow: AtomicU64::new(0),
            sum: AtomicU64::new(0),
            total: AtomicU64::new(0),
        }
    }

    /// Default latency histogram (ms): [1, 5, 10, 25, 50, 100, 250, 500, 1000, 5000, 30000].
    pub fn latency(name: impl Into<String>, desc: impl Into<String>) -> Self {
        Self::new(name, desc, vec![1, 5, 10, 25, 50, 100, 250, 500, 1000, 5000, 30000])
    }

    /// Record a single observation.
    pub fn observe(&self, value: u64) {
        self.sum.fetch_add(value, Ordering::Relaxed);
        self.total.fetch_add(1, Ordering::Relaxed);

        let mut placed = false;
        for (i, boundary) in self.buckets.iter().enumerate() {
            if value <= *boundary {
                self.counts[i].fetch_add(1, Ordering::Relaxed);
                placed = true;
                break;
            }
        }
        if !placed {
            self.overflow.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Get a snapshot of the histogram.
    pub fn snapshot(&self) -> HistogramSnapshot {
        let total = self.total.load(Ordering::Relaxed);
        let sum = self.sum.load(Ordering::Relaxed);
        let avg = if total > 0 { sum as f64 / total as f64 } else { 0.0 };

        let bucket_counts: Vec<u64> = self.counts.iter().map(|c| c.load(Ordering::Relaxed)).collect();
        let overflow = self.overflow.load(Ordering::Relaxed);

        // Estimate percentiles from buckets.
        let p50 = self.percentile(&bucket_counts, overflow, total, 0.50);
        let p95 = self.percentile(&bucket_counts, overflow, total, 0.95);
        let p99 = self.percentile(&bucket_counts, overflow, total, 0.99);

        HistogramSnapshot {
            count: total,
            sum,
            avg,
            p50,
            p95,
            p99,
            buckets: self.buckets.iter().copied().zip(bucket_counts.iter().copied()).collect(),
            overflow,
        }
    }

    fn percentile(&self, bucket_counts: &[u64], overflow: u64, total: u64, pct: f64) -> f64 {
        if total == 0 {
            return 0.0;
        }
        let target = (total as f64 * pct).ceil() as u64;
        let mut cumulative = 0u64;
        for (i, count) in bucket_counts.iter().enumerate() {
            cumulative += count;
            if cumulative >= target {
                return self.buckets[i] as f64;
            }
        }
        // In overflow bucket — return last bucket boundary.
        if overflow > 0 {
            return *self.buckets.last().unwrap_or(&0) as f64;
        }
        0.0
    }
}

/// Snapshot of a histogram at a point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistogramSnapshot {
    pub count: u64,
    pub sum: u64,
    pub avg: f64,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    /// (boundary, count) pairs.
    pub buckets: Vec<(u64, u64)>,
    pub overflow: u64,
}

// ---------------------------------------------------------------------------
// Metrics collector
// ---------------------------------------------------------------------------

/// Central metrics collector for the gateway.
///
/// Thread-safe, lock-free recording via atomics.
/// All metrics are keyed by name for dynamic registration.
pub struct MetricsCollector {
    counters: DashMap<String, Arc<Counter>>,
    gauges: DashMap<String, Arc<Gauge>>,
    histograms: DashMap<String, Arc<Histogram>>,
    /// Broadcast channel for SSE subscribers.
    event_tx: broadcast::Sender<MetricEvent>,
    pub created_at: DateTime<Utc>,
}

/// A metric event pushed to SSE subscribers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricEvent {
    pub timestamp: DateTime<Utc>,
    pub event_type: String,
    pub data: serde_json::Value,
}

impl MetricsCollector {
    pub fn new() -> Arc<Self> {
        let (event_tx, _) = broadcast::channel(256);
        let collector = Arc::new(Self {
            counters: DashMap::new(),
            gauges: DashMap::new(),
            histograms: DashMap::new(),
            event_tx,
            created_at: Utc::now(),
        });

        // Register built-in metrics.
        collector.register_builtins();
        collector
    }

    fn register_builtins(&self) {
        // Request counters.
        self.counter("http_requests_total", "Total HTTP requests");
        self.counter("http_requests_error", "HTTP requests that returned 4xx/5xx");
        self.counter("ws_connections_total", "Total WebSocket connections");
        self.counter("agent_invocations_total", "Total agent invocations");
        self.counter("tokens_input_total", "Total input tokens consumed");
        self.counter("tokens_output_total", "Total output tokens generated");
        self.counter("webhook_deliveries_total", "Total outbound webhook deliveries attempted");
        self.counter("webhook_deliveries_failed", "Failed webhook deliveries");

        // Gauges.
        self.gauge("active_connections", "Currently active HTTP/WS connections");
        self.gauge("active_sessions", "Currently active sessions");
        self.gauge("active_agents", "Number of loaded agent definitions");
        self.gauge("pending_webhooks", "Pending webhook deliveries in outbox");
        self.gauge("dlq_size", "Dead-lettered webhook deliveries");
        self.gauge("event_bus_queue_depth", "Current event bus queue depth");

        // Histograms.
        self.histogram_latency("http_request_duration_ms", "HTTP request duration in ms");
        self.histogram_latency("agent_response_time_ms", "Agent response time in ms");
        self.histogram_latency("webhook_delivery_latency_ms", "Webhook delivery latency in ms");
    }

    // --- Registration ---

    pub fn counter(&self, name: &str, desc: &str) -> Arc<Counter> {
        self.counters
            .entry(name.to_string())
            .or_insert_with(|| Arc::new(Counter::new(name, desc)))
            .clone()
    }

    pub fn gauge(&self, name: &str, desc: &str) -> Arc<Gauge> {
        self.gauges
            .entry(name.to_string())
            .or_insert_with(|| Arc::new(Gauge::new(name, desc)))
            .clone()
    }

    pub fn histogram_latency(&self, name: &str, desc: &str) -> Arc<Histogram> {
        self.histograms
            .entry(name.to_string())
            .or_insert_with(|| Arc::new(Histogram::latency(name, desc)))
            .clone()
    }

    // --- Recording ---

    pub fn inc_counter(&self, name: &str) {
        if let Some(c) = self.counters.get(name) {
            c.increment();
        }
    }

    pub fn add_counter(&self, name: &str, n: u64) {
        if let Some(c) = self.counters.get(name) {
            c.add(n);
        }
    }

    pub fn set_gauge(&self, name: &str, val: u64) {
        if let Some(g) = self.gauges.get(name) {
            g.set(val);
        }
    }

    pub fn inc_gauge(&self, name: &str) {
        if let Some(g) = self.gauges.get(name) {
            g.increment();
        }
    }

    pub fn dec_gauge(&self, name: &str) {
        if let Some(g) = self.gauges.get(name) {
            g.decrement();
        }
    }

    pub fn observe(&self, name: &str, value: u64) {
        if let Some(h) = self.histograms.get(name) {
            h.observe(value);
        }
    }

    // --- Snapshot ---

    /// Capture a full metrics snapshot for the dashboard.
    pub fn snapshot(&self) -> MetricsFullSnapshot {
        let counters: Vec<CounterSnapshot> = self.counters.iter().map(|entry| {
            CounterSnapshot {
                name: entry.key().clone(),
                description: entry.value().description.clone(),
                value: entry.value().get(),
            }
        }).collect();

        let gauges: Vec<GaugeSnapshot> = self.gauges.iter().map(|entry| {
            GaugeSnapshot {
                name: entry.key().clone(),
                description: entry.value().description.clone(),
                value: entry.value().get(),
            }
        }).collect();

        let histograms: Vec<NamedHistogramSnapshot> = self.histograms.iter().map(|entry| {
            NamedHistogramSnapshot {
                name: entry.key().clone(),
                description: entry.value().description.clone(),
                snapshot: entry.value().snapshot(),
            }
        }).collect();

        MetricsFullSnapshot {
            timestamp: Utc::now(),
            uptime_secs: (Utc::now() - self.created_at).num_seconds() as u64,
            counters,
            gauges,
            histograms,
        }
    }

    // --- SSE ---

    /// Subscribe to metric events (SSE).
    pub fn subscribe(&self) -> broadcast::Receiver<MetricEvent> {
        self.event_tx.subscribe()
    }

    /// Emit a metric event to all SSE subscribers.
    pub fn emit_event(&self, event_type: impl Into<String>, data: serde_json::Value) {
        let event = MetricEvent {
            timestamp: Utc::now(),
            event_type: event_type.into(),
            data,
        };
        // Best-effort: don't block if no subscribers or buffer full.
        let _ = self.event_tx.send(event);
    }

    /// Spawn a periodic snapshot emitter that pushes snapshots to SSE.
    pub fn spawn_periodic_emitter(
        self: &Arc<Self>,
        interval: Duration,
        cancel: tokio_util::sync::CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        let collector = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = tokio::time::sleep(interval) => {
                        let snap = collector.snapshot();
                        let data = serde_json::to_value(&snap).unwrap_or_default();
                        collector.emit_event("metrics.snapshot", data);
                    }
                }
            }
            debug!("periodic metrics emitter stopped");
        })
    }
}

impl Default for MetricsCollector {
    fn default() -> Self {
        // Can't return Arc<Self> from Default, return inner.
        let (event_tx, _) = broadcast::channel(256);
        let s = Self {
            counters: DashMap::new(),
            gauges: DashMap::new(),
            histograms: DashMap::new(),
            event_tx,
            created_at: Utc::now(),
        };
        s.register_builtins();
        s
    }
}

// ---------------------------------------------------------------------------
// Snapshot types (serializable)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CounterSnapshot {
    pub name: String,
    pub description: String,
    pub value: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GaugeSnapshot {
    pub name: String,
    pub description: String,
    pub value: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamedHistogramSnapshot {
    pub name: String,
    pub description: String,
    pub snapshot: HistogramSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsFullSnapshot {
    pub timestamp: DateTime<Utc>,
    pub uptime_secs: u64,
    pub counters: Vec<CounterSnapshot>,
    pub gauges: Vec<GaugeSnapshot>,
    pub histograms: Vec<NamedHistogramSnapshot>,
}

// ---------------------------------------------------------------------------
// Axum route handlers
// ---------------------------------------------------------------------------

use axum::extract::State;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Json};
use futures::stream::Stream;
use std::convert::Infallible;
use std::pin::Pin;

use crate::state::GatewayState;

/// `GET /api/v1/admin/observability/metrics` — full metrics snapshot.
pub async fn metrics_full(
    State(state): State<Arc<GatewayState>>,
) -> impl IntoResponse {
    let snapshot = state.metrics.snapshot();
    Json(snapshot)
}

/// `GET /api/v1/admin/observability/events` — SSE stream of metric events.
pub async fn metrics_sse(
    State(state): State<Arc<GatewayState>>,
) -> Sse<Pin<Box<dyn Stream<Item = Result<SseEvent, Infallible>> + Send>>> {
    let mut rx = state.metrics.subscribe();

    let stream = async_stream::stream! {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let data = serde_json::to_string(&event).unwrap_or_default();
                    let sse = SseEvent::default()
                        .event(&event.event_type)
                        .data(data);
                    yield Ok(sse);
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    let sse = SseEvent::default()
                        .event("warning")
                        .data(format!("{{\"lagged\": {n}}}"));
                    yield Ok(sse);
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    Sse::new(Box::pin(stream) as Pin<Box<dyn Stream<Item = Result<SseEvent, Infallible>> + Send>>)
        .keep_alive(KeepAlive::default())
}

/// `GET /api/v1/admin/observability/dashboard` — embedded HTML dashboard.
pub async fn dashboard(
    State(_state): State<Arc<GatewayState>>,
) -> impl IntoResponse {
    Html(DASHBOARD_HTML)
}

// ---------------------------------------------------------------------------
// Embedded HTML dashboard
// ---------------------------------------------------------------------------

const DASHBOARD_HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>ClawDesk — Observability Dashboard</title>
<style>
  :root {
    --bg: #0d1117; --surface: #161b22; --border: #30363d;
    --text: #c9d1d9; --dim: #8b949e; --accent: #58a6ff;
    --green: #3fb950; --red: #f85149; --yellow: #d29922;
  }
  * { margin: 0; padding: 0; box-sizing: border-box; }
  body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif;
    background: var(--bg); color: var(--text); padding: 20px; }
  h1 { font-size: 1.5rem; margin-bottom: 16px; color: var(--accent); }
  h2 { font-size: 1.1rem; margin-bottom: 12px; color: var(--dim); }
  .grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(280px, 1fr)); gap: 16px; margin-bottom: 24px; }
  .card { background: var(--surface); border: 1px solid var(--border); border-radius: 8px; padding: 16px; }
  .card .label { font-size: 0.75rem; color: var(--dim); text-transform: uppercase; letter-spacing: 0.05em; }
  .card .value { font-size: 1.8rem; font-weight: 700; margin: 4px 0; }
  .card .desc { font-size: 0.8rem; color: var(--dim); }
  .status { display: inline-block; width: 8px; height: 8px; border-radius: 50%; margin-right: 6px; }
  .status.ok { background: var(--green); }
  .status.error { background: var(--red); }
  .status.warn { background: var(--yellow); }
  table { width: 100%; border-collapse: collapse; margin-top: 8px; }
  th, td { text-align: left; padding: 6px 10px; border-bottom: 1px solid var(--border); font-size: 0.85rem; }
  th { color: var(--dim); font-weight: 500; }
  .conn-badge { font-size: 0.7rem; padding: 2px 6px; border-radius: 4px; background: var(--border); }
  #log { background: var(--surface); border: 1px solid var(--border); border-radius: 8px;
    padding: 12px; max-height: 300px; overflow-y: auto; font-family: monospace; font-size: 0.8rem; color: var(--dim); }
  #log div { margin: 2px 0; }
  .reconnecting { color: var(--yellow); }
</style>
</head>
<body>
<h1>🔭 ClawDesk Observability Dashboard</h1>
<div id="connection-status" style="margin-bottom:12px">
  <span class="status" id="conn-dot"></span>
  <span id="conn-text">Connecting…</span>
</div>

<h2>Counters</h2>
<div class="grid" id="counters"></div>

<h2>Gauges</h2>
<div class="grid" id="gauges"></div>

<h2>Latency Histograms</h2>
<div class="grid" id="histograms"></div>

<h2>Event Log</h2>
<div id="log"></div>

<script>
const API = window.location.origin;
let es = null;
let reconnectTimer = null;

function formatNumber(n) {
  if (n >= 1e6) return (n / 1e6).toFixed(1) + 'M';
  if (n >= 1e3) return (n / 1e3).toFixed(1) + 'K';
  return String(n);
}

function setConnection(state) {
  const dot = document.getElementById('conn-dot');
  const text = document.getElementById('conn-text');
  dot.className = 'status ' + (state === 'ok' ? 'ok' : state === 'error' ? 'error' : 'warn');
  text.textContent = state === 'ok' ? 'Connected (SSE)' : state === 'error' ? 'Disconnected' : 'Reconnecting…';
}

function renderCounters(counters) {
  const el = document.getElementById('counters');
  el.innerHTML = counters.map(c => `
    <div class="card">
      <div class="label">${c.name}</div>
      <div class="value">${formatNumber(c.value)}</div>
      <div class="desc">${c.description}</div>
    </div>
  `).join('');
}

function renderGauges(gauges) {
  const el = document.getElementById('gauges');
  el.innerHTML = gauges.map(g => `
    <div class="card">
      <div class="label">${g.name}</div>
      <div class="value">${formatNumber(g.value)}</div>
      <div class="desc">${g.description}</div>
    </div>
  `).join('');
}

function renderHistograms(histograms) {
  const el = document.getElementById('histograms');
  el.innerHTML = histograms.map(h => {
    const s = h.snapshot;
    return `
    <div class="card">
      <div class="label">${h.name}</div>
      <table>
        <tr><th>count</th><td>${formatNumber(s.count)}</td></tr>
        <tr><th>avg</th><td>${s.avg.toFixed(1)} ms</td></tr>
        <tr><th>p50</th><td>${s.p50.toFixed(0)} ms</td></tr>
        <tr><th>p95</th><td>${s.p95.toFixed(0)} ms</td></tr>
        <tr><th>p99</th><td>${s.p99.toFixed(0)} ms</td></tr>
      </table>
      <div class="desc">${h.description}</div>
    </div>`;
  }).join('');
}

function addLog(msg) {
  const el = document.getElementById('log');
  const div = document.createElement('div');
  div.textContent = new Date().toISOString().slice(11,19) + ' ' + msg;
  el.appendChild(div);
  if (el.children.length > 200) el.removeChild(el.firstChild);
  el.scrollTop = el.scrollHeight;
}

function connect() {
  if (es) { es.close(); }
  es = new EventSource(API + '/api/v1/admin/observability/events');

  es.onopen = () => { setConnection('ok'); addLog('SSE connected'); };
  es.onerror = () => {
    setConnection('error');
    addLog('SSE disconnected — retrying in 5s');
    es.close();
    reconnectTimer = setTimeout(connect, 5000);
  };

  es.addEventListener('metrics.snapshot', (e) => {
    try {
      const snap = JSON.parse(e.data);
      renderCounters(snap.counters || []);
      renderGauges(snap.gauges || []);
      renderHistograms(snap.histograms || []);
    } catch (err) { addLog('parse error: ' + err); }
  });

  es.addEventListener('warning', (e) => { addLog('⚠ ' + e.data); });
  es.addEventListener('agent.invocation', (e) => { addLog('🤖 ' + e.data); });
  es.addEventListener('webhook.delivery', (e) => { addLog('🔗 ' + e.data); });
}

// Initial fetch for immediate display
fetch(API + '/api/v1/admin/observability/metrics')
  .then(r => r.json())
  .then(snap => {
    renderCounters(snap.counters || []);
    renderGauges(snap.gauges || []);
    renderHistograms(snap.histograms || []);
  })
  .catch(e => addLog('initial fetch failed: ' + e));

connect();
</script>
</body>
</html>"##;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_increment() {
        let c = Counter::new("test", "test counter");
        assert_eq!(c.get(), 0);
        c.increment();
        c.increment();
        assert_eq!(c.get(), 2);
        c.add(5);
        assert_eq!(c.get(), 7);
    }

    #[test]
    fn gauge_up_down() {
        let g = Gauge::new("test", "test gauge");
        g.set(10);
        assert_eq!(g.get(), 10);
        g.increment();
        assert_eq!(g.get(), 11);
        g.decrement();
        assert_eq!(g.get(), 10);
    }

    #[test]
    fn histogram_percentiles() {
        let h = Histogram::latency("latency", "test latency");
        // Record 100 values: 1..=100.
        for v in 1..=100 {
            h.observe(v);
        }
        let snap = h.snapshot();
        assert_eq!(snap.count, 100);
        assert_eq!(snap.sum, 5050);
        assert!(snap.p50 > 0.0);
        assert!(snap.p95 > snap.p50);
        assert!(snap.p99 >= snap.p95);
    }

    #[test]
    fn collector_records_metrics() {
        let collector = MetricsCollector::new();
        collector.inc_counter("http_requests_total");
        collector.inc_counter("http_requests_total");
        collector.set_gauge("active_sessions", 5);
        collector.observe("http_request_duration_ms", 42);

        let snap = collector.snapshot();
        let req_counter = snap.counters.iter().find(|c| c.name == "http_requests_total").unwrap();
        assert_eq!(req_counter.value, 2);

        let sessions = snap.gauges.iter().find(|g| g.name == "active_sessions").unwrap();
        assert_eq!(sessions.value, 5);

        let latency = snap.histograms.iter().find(|h| h.name == "http_request_duration_ms").unwrap();
        assert_eq!(latency.snapshot.count, 1);
    }

    #[test]
    fn collector_sse_subscribe() {
        let collector = MetricsCollector::new();
        let mut rx = collector.subscribe();

        collector.emit_event("test.event", serde_json::json!({"hello": "world"}));

        // Should receive the event.
        let event = rx.try_recv().unwrap();
        assert_eq!(event.event_type, "test.event");
    }

    #[test]
    fn snapshot_serializes() {
        let collector = MetricsCollector::new();
        collector.inc_counter("http_requests_total");
        let snap = collector.snapshot();
        let json = serde_json::to_string(&snap).unwrap();
        assert!(json.contains("http_requests_total"));
    }
}
