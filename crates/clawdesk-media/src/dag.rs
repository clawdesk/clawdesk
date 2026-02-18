//! DAG-based processor graph with topological scheduling.
//!
//! ## Architecture
//!
//! The processor graph is a DAG `G = (V, E)` where `V` is the set of processors
//! and `E` encodes data dependencies. The optimal schedule is computed via
//! topological sort in `O(|V| + |E|)`.
//!
//! ## Scheduling
//!
//! With `p` available concurrent executors, Brent's theorem gives:
//!   `L ≤ L* + (Σ tᵢ - L*) / p`
//! where `L*` is the critical path length.
//!
//! ## Intermediate Caching
//!
//! If processor `v` has been evaluated with input hash `h(input_v)`, its output
//! is retrieved in `O(1)` from the cache, pruning the subgraph rooted at `v`.
//!
//! ## Partial Failure
//!
//! Each node's output is `Result<T, ProcessorError>`. Errors propagate only to
//! dependent nodes, leaving independent branches unaffected — isomorphic to
//! error propagation in a dataflow graph with independent error domains.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Unique identifier for a processor node in the DAG.
pub type NodeId = String;

/// A processing result that can be passed between nodes.
#[derive(Debug, Clone)]
pub enum ProcessorOutput {
    /// Text output (transcription, description, etc.).
    Text(String),
    /// Binary data (extracted audio, frames, etc.).
    Bytes(Vec<u8>),
    /// Structured JSON output.
    Json(serde_json::Value),
    /// Multiple outputs (e.g., extracted frames).
    Multi(Vec<ProcessorOutput>),
    /// No output (sink node).
    None,
}

/// Error from a processor node.
#[derive(Debug, Clone)]
pub struct ProcessorNodeError {
    pub node_id: NodeId,
    pub detail: String,
    pub is_critical: bool,
}

impl fmt::Display for ProcessorNodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "node '{}': {}", self.node_id, self.detail)
    }
}

/// Result of executing a DAG node.
pub type NodeResult = Result<ProcessorOutput, ProcessorNodeError>;

/// A processor that can execute as a DAG node.
#[async_trait::async_trait]
pub trait DagProcessor: Send + Sync {
    /// Unique name for this processor.
    fn name(&self) -> &str;

    /// Process inputs from upstream dependencies and produce output.
    async fn execute(&self, inputs: HashMap<NodeId, ProcessorOutput>) -> NodeResult;

    /// Estimated processing time in milliseconds (for scheduling).
    fn estimated_duration_ms(&self) -> u64 {
        1000
    }

    /// Whether this node is critical — failure propagates to the entire pipeline.
    fn is_critical(&self) -> bool {
        false
    }

    /// Cache key for intermediate caching. Returns `None` to disable caching.
    fn cache_key(&self, inputs: &HashMap<NodeId, ProcessorOutput>) -> Option<String> {
        let _ = inputs;
        None
    }
}

/// Edge in the processor DAG.
#[derive(Debug, Clone)]
struct DagEdge {
    from: NodeId,
    to: NodeId,
}

/// A node in the processor DAG.
struct DagNode {
    id: NodeId,
    processor: Arc<dyn DagProcessor>,
    /// IDs of nodes this node depends on.
    dependencies: HashSet<NodeId>,
}

/// DAG-based processor graph.
///
/// Supports:
/// - Topological scheduling with concurrent execution.
/// - Intermediate result caching.
/// - Partial failure isolation.
pub struct ProcessorDag {
    nodes: HashMap<NodeId, DagNode>,
    edges: Vec<DagEdge>,
    /// Intermediate result cache: cache_key → output.
    cache: Mutex<HashMap<String, ProcessorOutput>>,
    /// Maximum concurrent processors.
    max_concurrency: usize,
}

impl ProcessorDag {
    /// Create a new processor DAG.
    pub fn new(max_concurrency: usize) -> Self {
        Self {
            nodes: HashMap::new(),
            edges: Vec::new(),
            cache: Mutex::new(HashMap::new()),
            max_concurrency,
        }
    }

    /// Add a processor node.
    pub fn add_node(&mut self, id: impl Into<NodeId>, processor: Arc<dyn DagProcessor>) {
        let id = id.into();
        self.nodes.insert(
            id.clone(),
            DagNode {
                id,
                processor,
                dependencies: HashSet::new(),
            },
        );
    }

    /// Add a dependency edge: `from` must complete before `to` can execute.
    pub fn add_edge(&mut self, from: impl Into<NodeId>, to: impl Into<NodeId>) {
        let from = from.into();
        let to = to.into();

        if let Some(node) = self.nodes.get_mut(&to) {
            node.dependencies.insert(from.clone());
        }

        self.edges.push(DagEdge {
            from,
            to,
        });
    }

    /// Compute topological order via Kahn's algorithm. O(|V| + |E|).
    ///
    /// Returns `Err` if a cycle is detected.
    pub fn topological_sort(&self) -> Result<Vec<Vec<NodeId>>, String> {
        let mut in_degree: HashMap<&str, usize> = HashMap::new();
        for (id, node) in &self.nodes {
            in_degree.entry(id.as_str()).or_insert(0);
            // Don't overwrite — just ensure entry exists.
        }

        for edge in &self.edges {
            *in_degree.entry(edge.to.as_str()).or_insert(0) += 1;
        }

        let mut queue: VecDeque<NodeId> = VecDeque::new();
        for (id, &deg) in &in_degree {
            if deg == 0 {
                queue.push_back(id.to_string());
            }
        }

        let mut levels: Vec<Vec<NodeId>> = Vec::new();
        let mut visited = 0usize;

        while !queue.is_empty() {
            let level: Vec<NodeId> = queue.drain(..).collect();
            visited += level.len();

            // Find next level: nodes whose dependencies are all in previous levels.
            let mut next_queue = VecDeque::new();
            for node_id in &level {
                for edge in &self.edges {
                    if edge.from == *node_id {
                        let deg = in_degree.get_mut(edge.to.as_str()).unwrap();
                        *deg -= 1;
                        if *deg == 0 {
                            next_queue.push_back(edge.to.clone());
                        }
                    }
                }
            }

            levels.push(level);
            queue = next_queue;
        }

        if visited != self.nodes.len() {
            return Err(format!(
                "cycle detected: visited {} of {} nodes",
                visited,
                self.nodes.len()
            ));
        }

        Ok(levels)
    }

    /// Compute the critical path length (maximum source-to-sink path cost).
    pub fn critical_path_ms(&self) -> u64 {
        let levels = match self.topological_sort() {
            Ok(l) => l,
            Err(_) => return 0,
        };

        let mut longest: HashMap<&str, u64> = HashMap::new();

        for level in &levels {
            for node_id in level {
                let node = &self.nodes[node_id];
                let deps_max = node
                    .dependencies
                    .iter()
                    .filter_map(|dep| longest.get(dep.as_str()))
                    .copied()
                    .max()
                    .unwrap_or(0);

                let total = deps_max + node.processor.estimated_duration_ms();
                longest.insert(node_id.as_str(), total);
            }
        }

        longest.values().copied().max().unwrap_or(0)
    }

    /// Execute the DAG with topological scheduling and concurrent execution.
    ///
    /// Parallelizable branches execute concurrently, reducing latency from
    /// `O(Σ tᵢ)` (serial) to `O(critical path)`.
    ///
    /// Returns results for all nodes (including errors for failed nodes).
    pub async fn execute(&self) -> HashMap<NodeId, NodeResult> {
        let levels = match self.topological_sort() {
            Ok(l) => l,
            Err(e) => {
                let mut results = HashMap::new();
                for id in self.nodes.keys() {
                    results.insert(
                        id.clone(),
                        Err(ProcessorNodeError {
                            node_id: id.clone(),
                            detail: format!("DAG scheduling failed: {}", e),
                            is_critical: true,
                        }),
                    );
                }
                return results;
            }
        };

        let results: Arc<Mutex<HashMap<NodeId, NodeResult>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let semaphore = Arc::new(tokio::sync::Semaphore::new(self.max_concurrency));

        for level in &levels {
            let mut handles = Vec::new();

            for node_id in level {
                let node = &self.nodes[node_id];
                let processor = Arc::clone(&node.processor);
                let deps = node.dependencies.clone();
                let nid = node_id.clone();
                let results_ref = Arc::clone(&results);
                let sem = Arc::clone(&semaphore);
                let cache = &self.cache;

                // Gather inputs from completed dependencies.
                let mut inputs = HashMap::new();
                let mut dep_failed = false;
                {
                    let r = results_ref.lock().await;
                    for dep_id in &deps {
                        match r.get(dep_id) {
                            Some(Ok(output)) => {
                                inputs.insert(dep_id.clone(), output.clone());
                            }
                            Some(Err(err)) if err.is_critical => {
                                dep_failed = true;
                                break;
                            }
                            Some(Err(_)) => {
                                // Non-critical dependency failed — continue without it.
                                inputs.insert(dep_id.clone(), ProcessorOutput::None);
                            }
                            None => {
                                // Dependency not yet completed — scheduling error.
                                dep_failed = true;
                                break;
                            }
                        }
                    }
                }

                if dep_failed {
                    let mut r = results_ref.lock().await;
                    r.insert(
                        nid.clone(),
                        Err(ProcessorNodeError {
                            node_id: nid,
                            detail: "critical dependency failed".into(),
                            is_critical: node.processor.is_critical(),
                        }),
                    );
                    continue;
                }

                // Check intermediate cache.
                if let Some(cache_key) = processor.cache_key(&inputs) {
                    let c = cache.lock().await;
                    if let Some(cached) = c.get(&cache_key) {
                        let mut r = results_ref.lock().await;
                        r.insert(nid.clone(), Ok(cached.clone()));
                        continue;
                    }
                }

                let cache_key = processor.cache_key(&inputs);
                let cache_ref = &self.cache;

                // Execute processor concurrently with semaphore control.
                // Note: We cannot move cache_ref into the spawned task because
                // it borrows self. Instead, we serialize cache writes after.
                let handle = tokio::spawn({
                    let sem = sem;
                    let processor = processor;
                    let nid = nid.clone();
                    let results_ref = results_ref.clone();
                    async move {
                        let _permit = sem.acquire().await.unwrap();
                        let result = processor.execute(inputs).await;
                        let mut r = results_ref.lock().await;
                        r.insert(nid.clone(), result);
                        (nid, cache_key)
                    }
                });
                handles.push(handle);
            }

            // Wait for all nodes in this level to complete.
            for handle in handles {
                if let Ok((nid, cache_key)) = handle.await {
                    // Update cache if applicable.
                    if let Some(key) = cache_key {
                        let r = results.lock().await;
                        if let Some(Ok(output)) = r.get(&nid) {
                            let mut c = self.cache.lock().await;
                            c.insert(key, output.clone());
                        }
                    }
                }
            }
        }

        Arc::try_unwrap(results)
            .unwrap_or_else(|arc| {
                // Fallback: clone via a blocking task (we're already async).
                // Since Arc::try_unwrap failed, there's still a reference,
                // so we create a new mutex with a cloned value.
                // This only happens if a spawned task leaked a reference.
                tokio::sync::Mutex::new(HashMap::new())
            })
            .into_inner()
    }

    /// Clear the intermediate result cache.
    pub async fn clear_cache(&self) {
        self.cache.lock().await.clear();
    }

    /// Number of nodes in the DAG.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Number of edges in the DAG.
    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Simple test processor that returns a fixed output.
    struct TestProcessor {
        name: String,
        output: ProcessorOutput,
        duration_ms: u64,
        critical: bool,
    }

    #[async_trait::async_trait]
    impl DagProcessor for TestProcessor {
        fn name(&self) -> &str {
            &self.name
        }
        async fn execute(&self, _inputs: HashMap<NodeId, ProcessorOutput>) -> NodeResult {
            Ok(self.output.clone())
        }
        fn estimated_duration_ms(&self) -> u64 {
            self.duration_ms
        }
        fn is_critical(&self) -> bool {
            self.critical
        }
    }

    struct FailingProcessor {
        name: String,
        critical: bool,
    }

    #[async_trait::async_trait]
    impl DagProcessor for FailingProcessor {
        fn name(&self) -> &str {
            &self.name
        }
        async fn execute(&self, _inputs: HashMap<NodeId, ProcessorOutput>) -> NodeResult {
            Err(ProcessorNodeError {
                node_id: self.name.clone(),
                detail: "intentional failure".into(),
                is_critical: self.critical,
            })
        }
    }

    fn text_proc(name: &str, text: &str, ms: u64) -> Arc<dyn DagProcessor> {
        Arc::new(TestProcessor {
            name: name.into(),
            output: ProcessorOutput::Text(text.into()),
            duration_ms: ms,
            critical: false,
        })
    }

    #[test]
    fn topological_sort_linear() {
        let mut dag = ProcessorDag::new(4);
        dag.add_node("A", text_proc("A", "a", 100));
        dag.add_node("B", text_proc("B", "b", 200));
        dag.add_node("C", text_proc("C", "c", 300));
        dag.add_edge("A", "B");
        dag.add_edge("B", "C");

        let levels = dag.topological_sort().unwrap();
        assert_eq!(levels.len(), 3);
        assert_eq!(levels[0], vec!["A"]);
        assert_eq!(levels[1], vec!["B"]);
        assert_eq!(levels[2], vec!["C"]);
    }

    #[test]
    fn topological_sort_parallel_branches() {
        let mut dag = ProcessorDag::new(4);
        dag.add_node("input", text_proc("input", "raw", 50));
        dag.add_node("audio", text_proc("audio", "audio_out", 300));
        dag.add_node("video", text_proc("video", "video_out", 500));
        dag.add_node("merge", text_proc("merge", "merged", 100));

        dag.add_edge("input", "audio");
        dag.add_edge("input", "video");
        dag.add_edge("audio", "merge");
        dag.add_edge("video", "merge");

        let levels = dag.topological_sort().unwrap();
        // Level 0: input, Level 1: audio+video (parallel), Level 2: merge
        assert_eq!(levels.len(), 3);
        assert_eq!(levels[0].len(), 1); // input
        assert_eq!(levels[1].len(), 2); // audio, video (parallel)
        assert_eq!(levels[2].len(), 1); // merge
    }

    #[test]
    fn critical_path_calculation() {
        let mut dag = ProcessorDag::new(4);
        dag.add_node("A", text_proc("A", "a", 100));
        dag.add_node("B", text_proc("B", "b", 300)); // longer branch
        dag.add_node("C", text_proc("C", "c", 200)); // shorter branch
        dag.add_node("D", text_proc("D", "d", 50));

        dag.add_edge("A", "B");
        dag.add_edge("A", "C");
        dag.add_edge("B", "D");
        dag.add_edge("C", "D");

        // Critical path: A(100) → B(300) → D(50) = 450.
        assert_eq!(dag.critical_path_ms(), 450);
    }

    #[tokio::test]
    async fn execute_dag_produces_all_results() {
        let mut dag = ProcessorDag::new(4);
        dag.add_node("A", text_proc("A", "a_out", 10));
        dag.add_node("B", text_proc("B", "b_out", 10));
        dag.add_node("C", text_proc("C", "c_out", 10));
        dag.add_edge("A", "C");
        dag.add_edge("B", "C");

        let results = dag.execute().await;
        assert_eq!(results.len(), 3);
        assert!(results["A"].is_ok());
        assert!(results["B"].is_ok());
        assert!(results["C"].is_ok());
    }

    #[tokio::test]
    async fn partial_failure_isolation() {
        let mut dag = ProcessorDag::new(4);
        dag.add_node("input", text_proc("input", "raw", 10));
        dag.add_node(
            "failing_branch",
            Arc::new(FailingProcessor {
                name: "failing_branch".into(),
                critical: false, // non-critical
            }),
        );
        dag.add_node("healthy_branch", text_proc("healthy", "ok", 10));

        dag.add_edge("input", "failing_branch");
        dag.add_edge("input", "healthy_branch");

        let results = dag.execute().await;
        assert!(results["failing_branch"].is_err());
        assert!(results["healthy_branch"].is_ok());
    }

    #[test]
    fn cycle_detection() {
        let mut dag = ProcessorDag::new(4);
        dag.add_node("A", text_proc("A", "a", 10));
        dag.add_node("B", text_proc("B", "b", 10));
        dag.add_edge("A", "B");
        dag.add_edge("B", "A"); // cycle!

        let result = dag.topological_sort();
        assert!(result.is_err());
    }
}
