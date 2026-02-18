//! Graph overlay for relationship tracking in agent memory.

use async_trait::async_trait;
use clawdesk_types::error::StorageError;
use serde::{Deserialize, Serialize};

/// A node in the memory graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: String,
    pub properties: serde_json::Value,
}

/// An edge in the memory graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEdge {
    pub from: String,
    pub to: String,
    pub label: String,
    pub properties: serde_json::Value,
}

/// Port: graph overlay for memory relationships.
#[async_trait]
pub trait GraphStore: Send + Sync + 'static {
    /// Add a node to the graph.
    async fn add_node(
        &self,
        id: &str,
        properties: serde_json::Value,
    ) -> Result<(), StorageError>;

    /// Add an edge between two nodes.
    async fn add_edge(
        &self,
        from: &str,
        to: &str,
        label: &str,
        properties: serde_json::Value,
    ) -> Result<(), StorageError>;

    /// Get all neighbors of a node (optionally filtered by edge label).
    async fn get_neighbors(
        &self,
        id: &str,
        label: Option<&str>,
    ) -> Result<Vec<GraphNode>, StorageError>;

    /// Traverse the graph from a starting node up to `depth` hops.
    async fn traverse(
        &self,
        start: &str,
        depth: usize,
    ) -> Result<Vec<GraphNode>, StorageError>;

    /// Delete a node and its edges.
    async fn delete_node(&self, id: &str) -> Result<bool, StorageError>;
}
