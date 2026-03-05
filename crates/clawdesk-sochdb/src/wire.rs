//! Wire format abstraction for serialization (GAP-07).
//!
//! ## Problem
//!
//! All 149 `serde_json::to_vec` / `serde_json::from_slice` call sites use
//! JSON for persistence and LLM context assembly. SochDB's `WireFormat::Soch`
//! (TOON format) promises 58-67% fewer tokens than JSON, which directly reduces
//! LLM API costs and improves context utilization.
//!
//! ## Current status
//!
//! SochDB v0.5.0's `WireFormat` enum has no `encode()` / `decode()` methods —
//! it's a format selector, not a codec. The actual TOON codec is not yet
//! exposed through the public API.
//!
//! ## Solution
//!
//! This module provides a `WireCodec` abstraction that:
//! - Currently delegates to `serde_json` (identical behavior)
//! - Provides a single swap-point for migrating to TOON when the codec lands
//! - Tracks serialization statistics for monitoring format differences
//!
//! ## Migration path
//!
//! When SochDB exposes the TOON codec:
//! 1. Update `encode()` / `decode()` to use it
//! 2. Add format detection to `decode()` for reading legacy JSON blobs
//! 3. New writes use TOON; old reads auto-detect and parse both formats

use serde::{de::DeserializeOwned, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

/// Wire format selection for serialization.
///
/// Mirrors `sochdb::WireFormat` but adds codec functionality.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireCodecFormat {
    /// JSON format (current default, compatible with all existing data).
    Json,
    /// TOON/Soch format (future, 58-67% fewer tokens).
    /// Currently falls back to JSON until SochDB exposes the codec.
    Soch,
}

impl Default for WireCodecFormat {
    fn default() -> Self {
        Self::Json
    }
}

/// Serialization statistics for monitoring.
static JSON_ENCODE_COUNT: AtomicU64 = AtomicU64::new(0);
static JSON_ENCODE_BYTES: AtomicU64 = AtomicU64::new(0);
static JSON_DECODE_COUNT: AtomicU64 = AtomicU64::new(0);

/// Encode a value to bytes using the specified format.
///
/// Currently always uses JSON. When TOON codec is available, this will
/// be the single swap-point for the entire codebase.
pub fn encode<T: Serialize>(value: &T, _format: WireCodecFormat) -> Result<Vec<u8>, String> {
    let bytes = serde_json::to_vec(value)
        .map_err(|e| format!("wire encode: {e}"))?;
    JSON_ENCODE_COUNT.fetch_add(1, Ordering::Relaxed);
    JSON_ENCODE_BYTES.fetch_add(bytes.len() as u64, Ordering::Relaxed);
    Ok(bytes)
}

/// Decode bytes to a value, auto-detecting format.
///
/// Currently always uses JSON. When TOON is available, this will
/// detect the format from the first byte and use the appropriate decoder.
pub fn decode<T: DeserializeOwned>(bytes: &[u8], _format: WireCodecFormat) -> Result<T, String> {
    JSON_DECODE_COUNT.fetch_add(1, Ordering::Relaxed);
    serde_json::from_slice(bytes).map_err(|e| format!("wire decode: {e}"))
}

/// Get serialization statistics for monitoring.
pub fn wire_stats() -> WireStats {
    WireStats {
        json_encode_count: JSON_ENCODE_COUNT.load(Ordering::Relaxed),
        json_encode_bytes: JSON_ENCODE_BYTES.load(Ordering::Relaxed),
        json_decode_count: JSON_DECODE_COUNT.load(Ordering::Relaxed),
    }
}

/// Serialization statistics snapshot.
#[derive(Debug, Clone)]
pub struct WireStats {
    pub json_encode_count: u64,
    pub json_encode_bytes: u64,
    pub json_decode_count: u64,
}

impl WireStats {
    /// Estimated token savings if all JSON bytes were TOON-encoded.
    /// Uses the conservative 58% reduction estimate.
    pub fn estimated_toon_savings_bytes(&self) -> u64 {
        (self.json_encode_bytes as f64 * 0.58) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq)]
    struct TestPayload {
        name: String,
        count: u32,
    }

    #[test]
    fn roundtrip_json() {
        let payload = TestPayload { name: "hello".into(), count: 42 };
        let bytes = encode(&payload, WireCodecFormat::Json).unwrap();
        let decoded: TestPayload = decode(&bytes, WireCodecFormat::Json).unwrap();
        assert_eq!(decoded, payload);
    }

    #[test]
    fn soch_format_falls_back_to_json() {
        let payload = TestPayload { name: "soch".into(), count: 7 };
        let bytes = encode(&payload, WireCodecFormat::Soch).unwrap();
        // Should still be valid JSON in v0.5.0
        let decoded: TestPayload = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(decoded, payload);
    }
}
