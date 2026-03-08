//! HEFT (Heterogeneous Earliest Finish Time) scheduler.
//!
//! Computes upward rank for each node, then assigns nodes to processors
//! in decreasing rank order, selecting the processor that gives the
//! earliest finish time.
//!
//! Complexity: O(V² · P) where V = number of nodes, P = number of processors.

use crate::dtgg::TaskNode;
use std::collections::HashMap;
use tracing::debug;

/// HEFT scheduler for task graph nodes across heterogeneous agents.
pub struct HeftScheduler;

impl HeftScheduler {
    /// Compute HEFT schedule.
    ///
    /// Returns `(node_id, processor_id, start_time)` triples in execution order.
    pub fn schedule(
        nodes: &[&TaskNode],
        edges: &[(&str, &str, f64)],
        num_processors: usize,
    ) -> Vec<(String, usize, f64)> {
        if nodes.is_empty() || num_processors == 0 {
            return Vec::new();
        }

        // Build adjacency structures.
        let mut successors: HashMap<&str, Vec<(&str, f64)>> = HashMap::new();
        let mut predecessors: HashMap<&str, Vec<(&str, f64)>> = HashMap::new();
        for &(from, to, cost) in edges {
            successors.entry(from).or_default().push((to, cost));
            predecessors.entry(to).or_default().push((from, cost));
        }

        let node_map: HashMap<&str, &TaskNode> =
            nodes.iter().map(|n| (n.id.as_str(), *n)).collect();

        // Phase 1: Compute upward rank (rank_u).
        // rank_u(i) = w̄_i + max_{j ∈ succ(i)} (c̄_{i,j} + rank_u(j))
        let mut rank_u: HashMap<&str, f64> = HashMap::new();

        fn compute_rank<'a>(
            id: &'a str,
            node_map: &HashMap<&str, &TaskNode>,
            successors: &HashMap<&'a str, Vec<(&'a str, f64)>>,
            rank_u: &mut HashMap<&'a str, f64>,
        ) -> f64 {
            if let Some(&r) = rank_u.get(id) {
                return r;
            }
            let w = node_map.get(id).map(|n| n.weight).unwrap_or(1.0);
            let max_succ = successors
                .get(id)
                .map(|succs| {
                    succs
                        .iter()
                        .map(|&(s, comm)| {
                            comm + compute_rank(s, node_map, successors, rank_u)
                        })
                        .fold(0.0f64, f64::max)
                })
                .unwrap_or(0.0);
            let rank = w + max_succ;
            rank_u.insert(id, rank);
            rank
        }

        for node in nodes {
            compute_rank(node.id.as_str(), &node_map, &successors, &mut rank_u);
        }

        // Phase 2: Sort nodes by decreasing rank_u.
        let mut sorted_nodes: Vec<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
        sorted_nodes.sort_by(|a, b| {
            let ra = rank_u.get(a).unwrap_or(&0.0);
            let rb = rank_u.get(b).unwrap_or(&0.0);
            rb.partial_cmp(ra).unwrap_or(std::cmp::Ordering::Equal)
        });

        // Phase 3: Assign each node to the processor that minimises finish time.
        // processor_avail[p] = earliest time processor p is free.
        let mut processor_avail = vec![0.0f64; num_processors];
        // node_finish[id] = (assigned_processor, finish_time).
        let mut node_finish: HashMap<&str, (usize, f64)> = HashMap::new();
        let mut schedule = Vec::with_capacity(nodes.len());

        for &id in &sorted_nodes {
            let w = node_map.get(id).map(|n| n.weight).unwrap_or(1.0);

            // Compute earliest start time = max over predecessors of
            // (pred_finish + comm_cost to this processor).
            let pred_ready: f64 = predecessors
                .get(id)
                .map(|preds| {
                    preds
                        .iter()
                        .map(|&(pred_id, comm)| {
                            let (_, ft) = node_finish.get(pred_id).unwrap_or(&(0, 0.0));
                            ft + comm
                        })
                        .fold(0.0f64, f64::max)
                })
                .unwrap_or(0.0);

            // Find best processor.
            let mut best_proc = 0;
            let mut best_eft = f64::MAX;
            for p in 0..num_processors {
                let est = f64::max(processor_avail[p], pred_ready);
                let eft = est + w;
                if eft < best_eft {
                    best_eft = eft;
                    best_proc = p;
                }
            }

            let start_time = f64::max(processor_avail[best_proc], pred_ready);
            processor_avail[best_proc] = start_time + w;
            node_finish.insert(id, (best_proc, start_time + w));

            schedule.push((id.to_string(), best_proc, start_time));
            debug!(
                node = id,
                processor = best_proc,
                start = start_time,
                finish = start_time + w,
                rank = rank_u.get(id).unwrap_or(&0.0),
                "HEFT scheduled"
            );
        }

        schedule
    }

    /// Compute makespan lower bound: max(critical_path, total_work / num_processors).
    pub fn makespan_lower_bound(
        nodes: &[&TaskNode],
        edges: &[(&str, &str, f64)],
        num_processors: usize,
    ) -> f64 {
        let total_work: f64 = nodes.iter().map(|n| n.weight).sum();
        let work_bound = total_work / num_processors as f64;

        // Critical path = longest path through the DAG.
        let mut successors: HashMap<&str, Vec<(&str, f64)>> = HashMap::new();
        for &(from, to, cost) in edges {
            successors.entry(from).or_default().push((to, cost));
        }
        let node_map: HashMap<&str, f64> = nodes.iter().map(|n| (n.id.as_str(), n.weight)).collect();

        fn longest_path<'a>(
            id: &'a str,
            node_map: &HashMap<&str, f64>,
            successors: &HashMap<&'a str, Vec<(&'a str, f64)>>,
            memo: &mut HashMap<&'a str, f64>,
        ) -> f64 {
            if let Some(&v) = memo.get(id) {
                return v;
            }
            let w = node_map.get(id).copied().unwrap_or(0.0);
            let max_child = successors
                .get(id)
                .map(|s| {
                    s.iter()
                        .map(|&(c, cc)| cc + longest_path(c, node_map, successors, memo))
                        .fold(0.0f64, f64::max)
                })
                .unwrap_or(0.0);
            let val = w + max_child;
            memo.insert(id, val);
            val
        }

        let mut memo = HashMap::new();
        let critical = nodes
            .iter()
            .map(|n| longest_path(n.id.as_str(), &node_map, &successors, &mut memo))
            .fold(0.0f64, f64::max);

        f64::max(critical, work_bound)
    }
}

// ───────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_heft_linear_chain() {
        let a = TaskNode::new("a", "A").with_weight(3.0);
        let b = TaskNode::new("b", "B").with_weight(2.0);
        let c = TaskNode::new("c", "C").with_weight(1.0);
        let nodes = vec![&a, &b, &c];
        let edges = vec![("a", "b", 0.0), ("b", "c", 0.0)];

        let sched = HeftScheduler::schedule(&nodes, &edges, 2);
        assert_eq!(sched.len(), 3);
        // a must come first
        assert_eq!(sched[0].0, "a");
    }

    #[test]
    fn test_heft_parallel_tasks() {
        let a = TaskNode::new("a", "Start").with_weight(1.0);
        let b = TaskNode::new("b", "Branch1").with_weight(3.0);
        let c = TaskNode::new("c", "Branch2").with_weight(3.0);
        let d = TaskNode::new("d", "Merge").with_weight(1.0);
        let nodes = vec![&a, &b, &c, &d];
        let edges = vec![
            ("a", "b", 0.0),
            ("a", "c", 0.0),
            ("b", "d", 0.0),
            ("c", "d", 0.0),
        ];

        let sched = HeftScheduler::schedule(&nodes, &edges, 2);
        assert_eq!(sched.len(), 4);
        // With 2 processors, b and c should be on different processors
        let b_proc = sched.iter().find(|s| s.0 == "b").unwrap().1;
        let c_proc = sched.iter().find(|s| s.0 == "c").unwrap().1;
        assert_ne!(b_proc, c_proc);
    }

    #[test]
    fn test_makespan_bound() {
        let a = TaskNode::new("a", "A").with_weight(4.0);
        let b = TaskNode::new("b", "B").with_weight(4.0);
        let nodes = vec![&a, &b];
        let edges: Vec<(&str, &str, f64)> = vec![];

        let bound = HeftScheduler::makespan_lower_bound(&nodes, &edges, 2);
        assert!((bound - 4.0).abs() < f64::EPSILON); // total_work / 2 = 4
    }
}
