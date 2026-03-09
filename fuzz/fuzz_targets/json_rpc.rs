//! Fuzz target: JSON-RPC message parsing.
//!
//! Ensures that arbitrary JSON never panics during deserialization.

#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        // Try to parse as generic JSON
        let _ = serde_json::from_str::<serde_json::Value>(s);

        // Try parsing as JSON-RPC request shape
        #[derive(serde::Deserialize)]
        struct JsonRpcRequest {
            jsonrpc: Option<String>,
            id: Option<serde_json::Value>,
            method: Option<String>,
            params: Option<serde_json::Value>,
        }
        let _ = serde_json::from_str::<JsonRpcRequest>(s);

        // Try parsing as JSON-RPC response shape
        #[derive(serde::Deserialize)]
        struct JsonRpcResponse {
            jsonrpc: Option<String>,
            id: Option<serde_json::Value>,
            result: Option<serde_json::Value>,
            error: Option<serde_json::Value>,
        }
        let _ = serde_json::from_str::<JsonRpcResponse>(s);
    }
});
