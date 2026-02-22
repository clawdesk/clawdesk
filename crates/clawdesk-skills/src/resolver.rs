//! Skill dependency resolution — topological sort via Kahn's algorithm.
//!
//! Given a DAG of skill dependencies, produces a valid activation order.
//! Detects cycles (which would make the dependency graph not a DAG)
//! and missing dependencies.
//!
//! ## Complexity
//!
//! Kahn's algorithm: O(V + E) where V = |skills|, E = |dependency edges|.
//! For typical skill graphs (|V| < 100, |E| < 200), this is sub-microsecond.

use crate::definition::SkillId;
use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::VecDeque;
use tracing::warn;

/// Result of dependency resolution.
#[derive(Debug, Clone)]
pub struct ResolutionResult {
    /// Skills in valid activation order (dependencies before dependents).
    pub activation_order: Vec<SkillId>,
    /// Skills that could not be resolved.
    pub unresolved: Vec<UnresolvedSkill>,
}

/// A skill that could not be resolved.
#[derive(Debug, Clone)]
pub struct UnresolvedSkill {
    pub id: SkillId,
    pub reason: UnresolvedReason,
}

/// Why a skill couldn't be resolved.
#[derive(Debug, Clone)]
pub enum UnresolvedReason {
    /// A required dependency is not in the registry.
    MissingDependency { dependency: SkillId },
    /// Part of a dependency cycle.
    CyclicDependency { cycle: Vec<SkillId> },
}

impl std::fmt::Display for UnresolvedReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingDependency { dependency } => {
                write!(f, "missing dependency: {}", dependency)
            }
            Self::CyclicDependency { cycle } => {
                let ids: Vec<&str> = cycle.iter().map(|id| id.as_str()).collect();
                write!(f, "cyclic dependency among: {}", ids.join(", "))
            }
        }
    }
}

/// Dependency resolver using Kahn's algorithm for topological sorting.
pub struct SkillResolver;

impl SkillResolver {
    /// Resolve activation order for a set of skills and their dependencies.
    ///
    /// # Algorithm (Kahn's)
    ///
    /// 1. Compute in-degree for each node.
    /// 2. Enqueue all nodes with in-degree 0.
    /// 3. While queue non-empty:
    ///    a. Dequeue node u, append to result.
    ///    b. For each edge (u → v): decrement in-degree of v.
    ///       If in-degree of v reaches 0, enqueue v.
    /// 4. If |result| < |nodes|, the remaining nodes form cycles.
    ///
    /// Time: O(V + E). Space: O(V).
    pub fn resolve(
        skills: &[(SkillId, Vec<SkillId>)],
    ) -> ResolutionResult {
        let all_ids: FxHashSet<SkillId> = skills.iter().map(|(id, _)| id.clone()).collect();

        // Check for missing dependencies first.
        let mut unresolved = Vec::new();
        for (id, deps) in skills {
            for dep in deps {
                if !all_ids.contains(dep) {
                    unresolved.push(UnresolvedSkill {
                        id: id.clone(),
                        reason: UnresolvedReason::MissingDependency {
                            dependency: dep.clone(),
                        },
                    });
                }
            }
        }

        if !unresolved.is_empty() {
            return ResolutionResult {
                activation_order: vec![],
                unresolved,
            };
        }

        // Build adjacency list and in-degree map.
        // Edge semantics: if A depends on B, then B → A (B must activate before A).
        let mut adj: FxHashMap<SkillId, Vec<SkillId>> = FxHashMap::default();
        let mut in_degree: FxHashMap<SkillId, usize> = FxHashMap::default();

        for (id, _) in skills {
            adj.entry(id.clone()).or_default();
            in_degree.entry(id.clone()).or_insert(0);
        }

        for (id, deps) in skills {
            for dep in deps {
                adj.entry(dep.clone()).or_default().push(id.clone());
                *in_degree.entry(id.clone()).or_insert(0) += 1;
            }
        }

        // Kahn's algorithm.
        let mut queue: VecDeque<SkillId> = in_degree
            .iter()
            .filter(|(_, &deg)| deg == 0)
            .map(|(id, _)| id.clone())
            .collect();

        let mut activation_order = Vec::with_capacity(skills.len());

        while let Some(node) = queue.pop_front() {
            activation_order.push(node.clone());
            if let Some(neighbors) = adj.get(&node) {
                for neighbor in neighbors {
                    if let Some(deg) = in_degree.get_mut(neighbor) {
                        *deg -= 1;
                        if *deg == 0 {
                            queue.push_back(neighbor.clone());
                        }
                    }
                }
            }
        }

        // Any remaining nodes with in-degree > 0 are part of cycles.
        if activation_order.len() < skills.len() {
            let cycle_nodes: Vec<SkillId> = in_degree
                .iter()
                .filter(|(_, &deg)| deg > 0)
                .map(|(id, _)| id.clone())
                .collect();

            for id in &cycle_nodes {
                warn!(skill = %id, "skill is part of a dependency cycle");
                unresolved.push(UnresolvedSkill {
                    id: id.clone(),
                    reason: UnresolvedReason::CyclicDependency {
                        cycle: cycle_nodes.clone(),
                    },
                });
            }
        }

        ResolutionResult {
            activation_order,
            unresolved,
        }
    }

    /// Check if adding a dependency would create a cycle.
    /// Uses DFS reachability: O(V + E).
    pub fn would_cycle(
        skills: &[(SkillId, Vec<SkillId>)],
        from: &SkillId,
        to: &SkillId,
    ) -> bool {
        // If adding from → to (from depends on to), check if to can reach from.
        // Build forward adjacency: if A depends on B, then B → A.
        let mut adj: FxHashMap<&SkillId, Vec<&SkillId>> = FxHashMap::default();
        for (id, deps) in skills {
            for dep in deps {
                adj.entry(dep).or_default().push(id);
            }
        }

        // DFS from `from` to see if we can reach `to` via existing edges.
        let mut visited: FxHashSet<&SkillId> = FxHashSet::default();
        let mut stack = vec![from];

        while let Some(node) = stack.pop() {
            if node == to {
                return true;
            }
            if visited.insert(node) {
                if let Some(neighbors) = adj.get(node) {
                    stack.extend(neighbors.iter());
                }
            }
        }

        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_dependency_chain() {
        let skills = vec![
            (SkillId::from("c"), vec![SkillId::from("b")]),
            (SkillId::from("b"), vec![SkillId::from("a")]),
            (SkillId::from("a"), vec![]),
        ];

        let result = SkillResolver::resolve(&skills);
        assert!(result.unresolved.is_empty());
        assert_eq!(result.activation_order.len(), 3);
        // a must come before b, b before c
        let pos_a = result.activation_order.iter().position(|x| x.as_str() == "a").unwrap();
        let pos_b = result.activation_order.iter().position(|x| x.as_str() == "b").unwrap();
        let pos_c = result.activation_order.iter().position(|x| x.as_str() == "c").unwrap();
        assert!(pos_a < pos_b);
        assert!(pos_b < pos_c);
    }

    #[test]
    fn cycle_detection() {
        let skills = vec![
            (SkillId::from("a"), vec![SkillId::from("b")]),
            (SkillId::from("b"), vec![SkillId::from("a")]),
        ];

        let result = SkillResolver::resolve(&skills);
        assert!(!result.unresolved.is_empty());
    }

    #[test]
    fn missing_dependency() {
        let skills = vec![
            (SkillId::from("a"), vec![SkillId::from("missing")]),
        ];

        let result = SkillResolver::resolve(&skills);
        assert_eq!(result.unresolved.len(), 1);
        matches!(
            &result.unresolved[0].reason,
            UnresolvedReason::MissingDependency { dependency } if dependency.as_str() == "missing"
        );
    }

    #[test]
    fn diamond_dependency() {
        // d depends on b and c, both depend on a
        let skills = vec![
            (SkillId::from("a"), vec![]),
            (SkillId::from("b"), vec![SkillId::from("a")]),
            (SkillId::from("c"), vec![SkillId::from("a")]),
            (SkillId::from("d"), vec![SkillId::from("b"), SkillId::from("c")]),
        ];

        let result = SkillResolver::resolve(&skills);
        assert!(result.unresolved.is_empty());
        assert_eq!(result.activation_order.len(), 4);
        let pos_a = result.activation_order.iter().position(|x| x.as_str() == "a").unwrap();
        let pos_d = result.activation_order.iter().position(|x| x.as_str() == "d").unwrap();
        assert!(pos_a < pos_d);
    }
}
