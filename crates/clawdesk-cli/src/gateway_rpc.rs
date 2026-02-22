//! Gateway-RPC skill operations client.
//!
//! Provides a typed client for skill operations that tunnels through the
//! running gateway process via JSON-RPC over HTTP. This ensures a single
//! source of truth for skill state (the gateway's registries) and enables
//! real-time UI updates via WebSocket event broadcast.
//!
//! ## Protocol
//!
//! ```text
//! CLI ─── JSON-RPC POST ──→ Gateway /api/v1/skills/rpc
//!                            │
//!                            ├── dispatch by "method" field
//!                            ├── execute against live registries
//!                            └── broadcast SkillEvent on bus
//! ```
//!
//! ## Dispatch
//!
//! O(1) per RPC call via enum variant matching.

use serde::{Deserialize, Serialize};

/// RPC method identifiers for skill operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillRpcMethod {
    /// List all skills — returns `Vec<SkillStatusEntry>`.
    List,
    /// Get detailed skill info — returns `SkillDetailResponse`.
    Info,
    /// Search the store catalog — returns `StoreSearchResult`.
    Search,
    /// Install a skill — returns stream of `InstallProgress` or final `InstallResult`.
    Install,
    /// Uninstall a skill — returns `UninstallResult`.
    Uninstall,
    /// Update a skill or all skills — returns `UpdateResult`.
    Update,
    /// Check eligibility for all skills — returns `EligibilityReport`.
    Check,
    /// Sync the store catalog from remote — returns `SyncResult`.
    Sync,
    /// Audit installed skills — returns audit report.
    Audit,
    /// Publish a skill to the store — returns publish result.
    Publish,
}

/// A JSON-RPC request envelope for skill operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillRpcRequest {
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

/// A JSON-RPC response envelope for skill operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillRpcResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

/// RPC error descriptor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
}

impl SkillRpcResponse {
    /// Create a success response.
    pub fn ok(result: serde_json::Value) -> Self {
        Self {
            result: Some(result),
            error: None,
        }
    }

    /// Create an error response.
    pub fn err(code: i32, message: impl Into<String>) -> Self {
        Self {
            result: None,
            error: Some(RpcError {
                code,
                message: message.into(),
            }),
        }
    }
}

/// Typed RPC client that sends skill operations to the gateway.
///
/// All operations go through a single endpoint (`/api/v1/skills/rpc`),
/// ensuring the gateway is the sole source of truth.
pub struct GatewayRpcClient {
    base_url: String,
    client: reqwest::Client,
}

impl GatewayRpcClient {
    /// Create a new RPC client pointed at a gateway.
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            client: reqwest::Client::new(),
        }
    }

    /// Send an RPC call and return the response.
    pub async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<SkillRpcResponse, String> {
        let url = format!("{}/api/v1/skills/rpc", self.base_url);
        let req = SkillRpcRequest {
            method: method.to_string(),
            params,
        };

        let resp = self
            .client
            .post(&url)
            .json(&req)
            .send()
            .await
            .map_err(|e| format!("RPC call failed: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("RPC error ({status}): {body}"));
        }

        resp.json::<SkillRpcResponse>()
            .await
            .map_err(|e| format!("Failed to parse RPC response: {e}"))
    }

    /// Convenience: list all skills.
    pub async fn list(&self) -> Result<serde_json::Value, String> {
        let resp = self.call("list", serde_json::json!({})).await?;
        resp.result.ok_or_else(|| {
            resp.error
                .map(|e| e.message)
                .unwrap_or_else(|| "unknown error".into())
        })
    }

    /// Convenience: get skill info.
    pub async fn info(&self, name: &str) -> Result<serde_json::Value, String> {
        let resp = self
            .call("info", serde_json::json!({ "name": name }))
            .await?;
        resp.result.ok_or_else(|| {
            resp.error
                .map(|e| e.message)
                .unwrap_or_else(|| "unknown error".into())
        })
    }

    /// Convenience: search the store.
    pub async fn search(
        &self,
        query: &str,
        category: Option<&str>,
        verified_only: bool,
    ) -> Result<serde_json::Value, String> {
        let resp = self
            .call(
                "search",
                serde_json::json!({
                    "query": query,
                    "category": category,
                    "verified_only": verified_only,
                }),
            )
            .await?;
        resp.result.ok_or_else(|| {
            resp.error
                .map(|e| e.message)
                .unwrap_or_else(|| "unknown error".into())
        })
    }

    /// Convenience: install a skill.
    pub async fn install(
        &self,
        skill_ref: &str,
        force: bool,
    ) -> Result<serde_json::Value, String> {
        let resp = self
            .call(
                "install",
                serde_json::json!({
                    "ref": skill_ref,
                    "force": force,
                }),
            )
            .await?;
        resp.result.ok_or_else(|| {
            resp.error
                .map(|e| e.message)
                .unwrap_or_else(|| "unknown error".into())
        })
    }

    /// Convenience: uninstall a skill.
    pub async fn uninstall(&self, id: &str) -> Result<serde_json::Value, String> {
        let resp = self
            .call("uninstall", serde_json::json!({ "id": id }))
            .await?;
        resp.result.ok_or_else(|| {
            resp.error
                .map(|e| e.message)
                .unwrap_or_else(|| "unknown error".into())
        })
    }

    /// Convenience: check eligibility.
    pub async fn check(&self) -> Result<serde_json::Value, String> {
        let resp = self.call("check", serde_json::json!({})).await?;
        resp.result.ok_or_else(|| {
            resp.error
                .map(|e| e.message)
                .unwrap_or_else(|| "unknown error".into())
        })
    }

    /// Convenience: sync store catalog.
    pub async fn sync(&self) -> Result<serde_json::Value, String> {
        let resp = self.call("sync", serde_json::json!({})).await?;
        resp.result.ok_or_else(|| {
            resp.error
                .map(|e| e.message)
                .unwrap_or_else(|| "unknown error".into())
        })
    }
}
