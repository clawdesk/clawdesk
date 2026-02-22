//! SochDB graph store implementation.

use async_trait::async_trait;
use clawdesk_storage::graph_store::{GraphEdge, GraphNode, GraphStore};
use clawdesk_types::error::StorageError;

use crate::SochStore;

#[async_trait]
impl GraphStore for SochStore {
    async fn add_node(&self, id: &str, properties: serde_json::Value) -> Result<(), StorageError> {
        let key = format!("graph/nodes/{}", id);
        let bytes =
            serde_json::to_vec(&properties).map_err(|e| StorageError::SerializationFailed {
                detail: e.to_string(),
            })?;
        self.put(&key, &bytes)?;
        Ok(())
    }

    async fn add_edge(
        &self,
        from: &str,
        to: &str,
        label: &str,
        properties: serde_json::Value,
    ) -> Result<(), StorageError> {
        let edge = GraphEdge {
            from: from.to_string(),
            to: to.to_string(),
            label: label.to_string(),
            properties,
        };
        let fwd_key = format!("graph/edges/{}/{}/{}", from, label, to);
        let bytes =
            serde_json::to_vec(&edge).map_err(|e| StorageError::SerializationFailed {
                detail: e.to_string(),
            })?;
        self.put(&fwd_key, &bytes)?;

        // Write reverse index so delete_node can find incoming edges.
        // Value = the forward key so we can delete it during cleanup.
        let rev_key = format!("graph/edges_to/{}/{}/{}", to, label, from);
        self.put(&rev_key, fwd_key.as_bytes())?;

        Ok(())
    }

    async fn get_neighbors(
        &self,
        id: &str,
        label: Option<&str>,
    ) -> Result<Vec<GraphNode>, StorageError> {
        let prefix = match label {
            Some(l) => format!("graph/edges/{}/{}/", id, l),
            None => format!("graph/edges/{}/", id),
        };

        let results = self
            .scan(&prefix)?;

        let mut neighbors = Vec::new();
        for (_key, value) in &results {
            if let Ok(edge) = serde_json::from_slice::<GraphEdge>(value) {
                // Load the neighbor node
                let node_key = format!("graph/nodes/{}", edge.to);
                if let Ok(Some(node_bytes)) = self.get(&node_key) {
                    if let Ok(props) = serde_json::from_slice(&node_bytes) {
                        neighbors.push(GraphNode {
                            id: edge.to,
                            properties: props,
                        });
                    }
                }
            }
        }

        Ok(neighbors)
    }

    async fn traverse(&self, start: &str, depth: usize) -> Result<Vec<GraphNode>, StorageError> {
        let mut visited = std::collections::HashSet::new();
        let mut result = Vec::new();
        let mut queue = vec![start.to_string()];
        let mut current_depth = 0;

        while current_depth < depth && !queue.is_empty() {
            let mut next_queue = Vec::new();
            for node_id in &queue {
                if visited.contains(node_id) {
                    continue;
                }
                visited.insert(node_id.clone());

                let neighbors = self.get_neighbors(node_id, None).await?;
                for neighbor in neighbors {
                    if !visited.contains(&neighbor.id) {
                        next_queue.push(neighbor.id.clone());
                        result.push(neighbor);
                    }
                }
            }
            queue = next_queue;
            current_depth += 1;
        }

        Ok(result)
    }

    async fn delete_node(&self, id: &str) -> Result<bool, StorageError> {
        // Delete all outgoing edges from this node
        let out_prefix = format!("graph/edges/{}/", id);
        let out_edges = self
            .scan(&out_prefix)?;
        for (edge_key, _) in &out_edges {
            self.delete(edge_key)?;
        }

        // Delete all incoming edges (scan the reverse index)
        let in_prefix = format!("graph/edges_to/{}/", id);
        let in_edges = self
            .scan(&in_prefix)?;
        for (rev_key, value) in &in_edges {
            // The reverse index value holds the forward edge key
            let fwd_key = String::from_utf8_lossy(value);
            let _ = self.delete(&fwd_key);
            let _ = self.delete(rev_key);
        }

        // Delete the node itself
        let key = format!("graph/nodes/{}", id);
        self.delete(&key)?;
        Ok(true)
    }
}
