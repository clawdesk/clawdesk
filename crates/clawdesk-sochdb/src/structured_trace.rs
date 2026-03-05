//! Structured trace attributes — extends SochDB's message-based spans with queryable metadata.
//!
//! # Problem
//!
//! SochDB's `TraceStore::end_span()` accepts an `Option<String>` message, which
//! means all metadata (tool names, model IDs, namespaces, error types) gets
//! flattened into a single string. This kills queryability: you can't filter
//! spans by `tool.name == "web_search"` or aggregate by `model`.
//!
//! # Solution
//!
//! Store structured attributes as a separate JSON blob alongside the span's
//! human-readable message. Key path:
//!
//! ```text
//! trace/attrs/{trace_id}/{span_id}  →  JSON { attributes, events }
//! ```
//!
//! The original `end_span(message)` continues to work as the human-readable
//! summary. The structured attributes provide machine-queryable metadata.
//!
//! # Usage
//!
//! ```rust,ignore
//! let ext = StructuredTracing::new(soch_store.clone());
//! ext.set_span_attributes("trace-1", "span-1", &attrs)?;
//! ext.add_span_event("trace-1", "span-1", "tool_call", &event_attrs)?;
//! let data = ext.get_span_attributes("trace-1", "span-1")?;
//! ```

use crate::SochStore;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::debug;

/// A structured trace event (like an OpenTelemetry span event / log).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceEvent {
    /// Event name (e.g., "tool_call", "error", "retry").
    pub name: String,
    /// Microsecond Unix timestamp.
    pub timestamp_us: u64,
    /// Event attributes.
    pub attributes: HashMap<String, serde_json::Value>,
}

/// Structured data for a single span.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SpanAttributes {
    /// Schema version for forward compatibility.
    pub version: u32,
    /// Key-value attributes (e.g., tool.name, model.id, namespace).
    pub attributes: HashMap<String, serde_json::Value>,
    /// Ordered list of events within this span.
    pub events: Vec<TraceEvent>,
}

impl SpanAttributes {
    const CURRENT_VERSION: u32 = 1;

    pub fn new() -> Self {
        Self {
            version: Self::CURRENT_VERSION,
            attributes: HashMap::new(),
            events: Vec::new(),
        }
    }

    /// Set an attribute.
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<serde_json::Value>) {
        self.attributes.insert(key.into(), value.into());
    }

    /// Add an event.
    pub fn add_event(&mut self, name: impl Into<String>, attrs: HashMap<String, serde_json::Value>) {
        self.events.push(TraceEvent {
            name: name.into(),
            timestamp_us: chrono::Utc::now().timestamp_micros() as u64,
            attributes: attrs,
        });
    }
}

/// Extends SochDB's trace store with structured attribute storage.
///
/// Stores structured data alongside the existing message-based spans.
/// Thread-safe via `Arc<SochStore>` (which has its own `op_lock`).
#[derive(Clone)]
pub struct StructuredTracing {
    store: Arc<SochStore>,
}

impl StructuredTracing {
    pub fn new(store: Arc<SochStore>) -> Self {
        Self { store }
    }

    /// Key for span attributes.
    fn attrs_key(trace_id: &str, span_id: &str) -> String {
        format!("trace/attrs/{}/{}", trace_id, span_id)
    }

    /// Key for run-level attributes.
    fn run_attrs_key(trace_id: &str) -> String {
        format!("trace/run_attrs/{}", trace_id)
    }

    /// Store structured attributes for a span.
    ///
    /// Merges with existing attributes if they exist (additive).
    pub fn set_span_attributes(
        &self,
        trace_id: &str,
        span_id: &str,
        attributes: &HashMap<String, serde_json::Value>,
    ) -> Result<(), String> {
        let key = Self::attrs_key(trace_id, span_id);
        let mut data = self.load_or_new(&key)?;
        data.attributes.extend(attributes.clone());
        self.save(&key, &data)
    }

    /// Add a structured event to a span (e.g., tool call, error, etc.).
    pub fn add_span_event(
        &self,
        trace_id: &str,
        span_id: &str,
        event_name: &str,
        attributes: &HashMap<String, serde_json::Value>,
    ) -> Result<(), String> {
        let key = Self::attrs_key(trace_id, span_id);
        let mut data = self.load_or_new(&key)?;
        data.add_event(event_name, attributes.clone());
        self.save(&key, &data)
    }

    /// Get structured attributes for a span.
    pub fn get_span_attributes(
        &self,
        trace_id: &str,
        span_id: &str,
    ) -> Result<Option<SpanAttributes>, String> {
        let key = Self::attrs_key(trace_id, span_id);
        match self.store.get(&key) {
            Ok(Some(bytes)) => {
                let data: SpanAttributes = serde_json::from_slice(&bytes)
                    .map_err(|e| format!("deserialize span attrs: {e}"))?;
                Ok(Some(data))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(format!("get span attrs: {e}")),
        }
    }

    /// Set run-level attributes (session_id, agent_id, etc.).
    pub fn set_run_attributes(
        &self,
        trace_id: &str,
        attributes: &HashMap<String, serde_json::Value>,
    ) -> Result<(), String> {
        let key = Self::run_attrs_key(trace_id);
        let mut data = self.load_or_new(&key)?;
        data.attributes.extend(attributes.clone());
        self.save(&key, &data)
    }

    /// Get run-level attributes.
    pub fn get_run_attributes(
        &self,
        trace_id: &str,
    ) -> Result<Option<SpanAttributes>, String> {
        let key = Self::run_attrs_key(trace_id);
        match self.store.get(&key) {
            Ok(Some(bytes)) => {
                let data: SpanAttributes = serde_json::from_slice(&bytes)
                    .map_err(|e| format!("deserialize run attrs: {e}"))?;
                Ok(Some(data))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(format!("get run attrs: {e}")),
        }
    }

    /// Query spans by attribute filter.
    ///
    /// Scans all span attributes for a trace run and returns those matching
    /// the filter criteria. Useful for finding "all tool_call spans" or
    /// "all spans with error status" in a trace.
    pub fn query_spans_by_attribute(
        &self,
        trace_id: &str,
        filter_key: &str,
        filter_value: &serde_json::Value,
    ) -> Result<Vec<(String, SpanAttributes)>, String> {
        let prefix = format!("trace/attrs/{}/", trace_id);
        let entries = self.store.scan(&prefix)
            .map_err(|e| format!("scan span attrs: {e}"))?;

        let mut matches = Vec::new();
        for (key, bytes) in entries {
            if let Ok(data) = serde_json::from_slice::<SpanAttributes>(&bytes) {
                if data.attributes.get(filter_key) == Some(filter_value) {
                    let span_id = key.strip_prefix(&prefix).unwrap_or(&key).to_string();
                    matches.push((span_id, data));
                }
            }
        }

        debug!(
            trace_id = %trace_id,
            filter_key = %filter_key,
            matches = matches.len(),
            "query_spans_by_attribute"
        );

        Ok(matches)
    }

    /// Delete all structured trace data for a run (called by lifecycle cascade).
    pub fn delete_run_attrs(&self, trace_id: &str) -> Result<usize, String> {
        let mut deleted = 0;

        // Span attributes
        let span_prefix = format!("trace/attrs/{}/", trace_id);
        deleted += self.store.delete_prefix(&span_prefix)
            .map_err(|e| format!("delete span attrs: {e}"))?;

        // Run attributes
        let run_key = Self::run_attrs_key(trace_id);
        let _ = self.store.delete(&run_key);
        deleted += 1;

        Ok(deleted)
    }

    // ── Internal helpers ────────────────────────────────────────────

    fn load_or_new(&self, key: &str) -> Result<SpanAttributes, String> {
        match self.store.get(key) {
            Ok(Some(bytes)) => {
                serde_json::from_slice(&bytes)
                    .map_err(|e| format!("deserialize attrs: {e}"))
            }
            Ok(None) => Ok(SpanAttributes::new()),
            Err(e) => Err(format!("load attrs: {e}")),
        }
    }

    fn save(&self, key: &str, data: &SpanAttributes) -> Result<(), String> {
        let bytes = serde_json::to_vec(data)
            .map_err(|e| format!("serialize attrs: {e}"))?;
        self.store.put(key, &bytes)
            .map_err(|e| format!("save attrs: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_tracing_roundtrip() {
        let store = Arc::new(SochStore::open_ephemeral_quiet().unwrap());
        let st = StructuredTracing::new(store);

        // Set span attributes
        let mut attrs = HashMap::new();
        attrs.insert("tool.name".to_string(), serde_json::json!("web_search"));
        attrs.insert("model.id".to_string(), serde_json::json!("claude-3"));
        st.set_span_attributes("trace-1", "span-1", &attrs).unwrap();

        // Add event
        let mut event_attrs = HashMap::new();
        event_attrs.insert("query".to_string(), serde_json::json!("rust async"));
        st.add_span_event("trace-1", "span-1", "tool_call", &event_attrs).unwrap();

        // Read back
        let data = st.get_span_attributes("trace-1", "span-1").unwrap().unwrap();
        assert_eq!(data.attributes["tool.name"], serde_json::json!("web_search"));
        assert_eq!(data.events.len(), 1);
        assert_eq!(data.events[0].name, "tool_call");
    }

    #[test]
    fn query_by_attribute() {
        let store = Arc::new(SochStore::open_ephemeral_quiet().unwrap());
        let st = StructuredTracing::new(store);

        // Create multiple spans with different tool names
        let mut attrs1 = HashMap::new();
        attrs1.insert("tool.name".to_string(), serde_json::json!("web_search"));
        st.set_span_attributes("trace-1", "span-1", &attrs1).unwrap();

        let mut attrs2 = HashMap::new();
        attrs2.insert("tool.name".to_string(), serde_json::json!("code_exec"));
        st.set_span_attributes("trace-1", "span-2", &attrs2).unwrap();

        let mut attrs3 = HashMap::new();
        attrs3.insert("tool.name".to_string(), serde_json::json!("web_search"));
        st.set_span_attributes("trace-1", "span-3", &attrs3).unwrap();

        // Query for web_search spans
        let matches = st.query_spans_by_attribute(
            "trace-1",
            "tool.name",
            &serde_json::json!("web_search"),
        ).unwrap();
        assert_eq!(matches.len(), 2);
    }
}
