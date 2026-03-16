//! # Route Dispatcher — Structured agent interaction endpoints.
//!
//! Instead of a monolithic "do browser thing" interface, routes provide
//! structured endpoints for different interaction modes:
//! - `act` — Execute an action (click, type, scroll, etc.)
//! - `snapshot` — Capture current page state
//! - `plan` — Generate an action plan from snapshot diff
//! - `storage` — Read/write browser storage (cookies, localStorage)
//! - `debug` — Debugging utilities (console, network, performance)

use serde::{Deserialize, Serialize};

/// Route identifier for agent requests.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRoute {
    /// Execute a browser action.
    Act,
    /// Capture a page snapshot.
    Snapshot,
    /// Generate an action plan from snapshot diff.
    SnapshotPlan,
    /// Read/write browser storage.
    Storage,
    /// Debugging utilities.
    Debug,
    /// Navigate to a URL.
    Navigate,
    /// Manage tabs.
    Tabs,
}

impl std::fmt::Display for AgentRoute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Act => write!(f, "agent.act"),
            Self::Snapshot => write!(f, "agent.snapshot"),
            Self::SnapshotPlan => write!(f, "agent.snapshot.plan"),
            Self::Storage => write!(f, "agent.storage"),
            Self::Debug => write!(f, "agent.debug"),
            Self::Navigate => write!(f, "agent.navigate"),
            Self::Tabs => write!(f, "agent.tabs"),
        }
    }
}

/// Request payload for a route dispatch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteRequest {
    /// Which route to invoke.
    pub route: AgentRoute,
    /// Profile to operate on.
    pub profile: String,
    /// Tab target (if applicable).
    pub tab_id: Option<String>,
    /// Route-specific parameters.
    pub params: serde_json::Value,
}

/// Response from a route dispatch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteResponse {
    pub route: AgentRoute,
    pub success: bool,
    pub data: serde_json::Value,
    pub duration_ms: u64,
}

impl RouteResponse {
    pub fn ok(route: AgentRoute, data: serde_json::Value, duration_ms: u64) -> Self {
        Self { route, success: true, data, duration_ms }
    }

    pub fn err(route: AgentRoute, error: &str, duration_ms: u64) -> Self {
        Self {
            route,
            success: false,
            data: serde_json::json!({ "error": error }),
            duration_ms,
        }
    }
}

/// Snapshot diff for plan generation.
///
/// Phase 1: Compute minimal DOM diff δ = current ⊖ previous.
/// Phase 2: Generate action plan from δ using element-ref stability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotDiff {
    /// Elements added since last snapshot.
    pub added: Vec<ElementChange>,
    /// Elements removed since last snapshot.
    pub removed: Vec<ElementChange>,
    /// Elements whose properties changed.
    pub changed: Vec<ElementChange>,
    /// Elements that are stable (ref unchanged).
    pub stable_count: usize,
}

/// A single element change in a snapshot diff.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElementChange {
    /// Element reference ID (e.g., "e12").
    pub ref_id: String,
    /// Element tag.
    pub tag: String,
    /// Human-readable description of the change.
    pub description: String,
    /// Previous value (for changed elements).
    pub previous: Option<String>,
    /// Current value.
    pub current: Option<String>,
}

/// Compute diff between two snapshots.
///
/// Complexity: O(|DOM| × log|DOM|) via sorted element comparison.
/// Elements are matched by ref_id for O(1) lookup.
pub fn compute_snapshot_diff(
    previous: &serde_json::Value,
    current: &serde_json::Value,
) -> SnapshotDiff {
    // Extract element arrays from snapshots.
    let prev_elements = extract_elements(previous);
    let curr_elements = extract_elements(current);

    let mut prev_map: std::collections::HashMap<&str, &serde_json::Value> =
        std::collections::HashMap::new();
    for elem in &prev_elements {
        if let Some(ref_id) = elem.get("ref").and_then(|r| r.as_str()) {
            prev_map.insert(ref_id, elem);
        }
    }

    let mut added = Vec::new();
    let mut changed = Vec::new();
    let mut stable_count = 0;
    let mut seen_refs: std::collections::HashSet<&str> = std::collections::HashSet::new();

    for elem in &curr_elements {
        if let Some(ref_id) = elem.get("ref").and_then(|r| r.as_str()) {
            seen_refs.insert(ref_id);
            if let Some(prev) = prev_map.get(ref_id) {
                if elem.to_string() != prev.to_string() {
                    changed.push(ElementChange {
                        ref_id: ref_id.to_string(),
                        tag: elem.get("tag").and_then(|t| t.as_str()).unwrap_or("").to_string(),
                        description: "element changed".to_string(),
                        previous: Some(prev.to_string()),
                        current: Some(elem.to_string()),
                    });
                } else {
                    stable_count += 1;
                }
            } else {
                added.push(ElementChange {
                    ref_id: ref_id.to_string(),
                    tag: elem.get("tag").and_then(|t| t.as_str()).unwrap_or("").to_string(),
                    description: "element added".to_string(),
                    previous: None,
                    current: Some(elem.to_string()),
                });
            }
        }
    }

    let removed: Vec<ElementChange> = prev_map
        .iter()
        .filter(|(ref_id, _)| !seen_refs.contains(**ref_id))
        .map(|(ref_id, elem)| ElementChange {
            ref_id: ref_id.to_string(),
            tag: elem.get("tag").and_then(|t| t.as_str()).unwrap_or("").to_string(),
            description: "element removed".to_string(),
            previous: Some(elem.to_string()),
            current: None,
        })
        .collect();

    SnapshotDiff { added, removed, changed, stable_count }
}

fn extract_elements(snapshot: &serde_json::Value) -> Vec<&serde_json::Value> {
    snapshot
        .get("elements")
        .and_then(|e| e.as_array())
        .map(|arr| arr.iter().collect())
        .unwrap_or_default()
}

/// Navigation guard — extends SSRF with redirect chain depth checking.
///
/// Block if redirect chain depth > max_depth (default: 5).
#[derive(Debug, Clone)]
pub struct NavigationGuard {
    pub max_redirect_depth: usize,
}

impl Default for NavigationGuard {
    fn default() -> Self {
        Self { max_redirect_depth: 5 }
    }
}

impl NavigationGuard {
    /// Check if a redirect chain is safe.
    pub fn check_redirect_chain(&self, chain: &[String]) -> NavigationDecision {
        if chain.len() > self.max_redirect_depth {
            return NavigationDecision::Block {
                reason: format!(
                    "redirect chain too deep ({} hops, max {})",
                    chain.len(),
                    self.max_redirect_depth
                ),
            };
        }
        NavigationDecision::Allow
    }
}

/// Navigation decision result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavigationDecision {
    Allow,
    Block { reason: String },
    Redirect { url: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_display() {
        assert_eq!(AgentRoute::Act.to_string(), "agent.act");
        assert_eq!(AgentRoute::SnapshotPlan.to_string(), "agent.snapshot.plan");
    }

    #[test]
    fn navigation_guard_allows_short_chain() {
        let guard = NavigationGuard::default();
        let chain = vec!["https://a.com".into(), "https://b.com".into()];
        assert_eq!(guard.check_redirect_chain(&chain), NavigationDecision::Allow);
    }

    #[test]
    fn navigation_guard_blocks_deep_chain() {
        let guard = NavigationGuard::default();
        let chain: Vec<String> = (0..10).map(|i| format!("https://hop{}.com", i)).collect();
        assert!(matches!(
            guard.check_redirect_chain(&chain),
            NavigationDecision::Block { .. }
        ));
    }

    #[test]
    fn snapshot_diff_empty() {
        let empty = serde_json::json!({ "elements": [] });
        let diff = compute_snapshot_diff(&empty, &empty);
        assert_eq!(diff.added.len(), 0);
        assert_eq!(diff.removed.len(), 0);
        assert_eq!(diff.stable_count, 0);
    }
}
