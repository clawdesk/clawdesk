//! Dependency resolver — topological sort for plugin activation ordering.

use clawdesk_types::error::PluginError;
use clawdesk_types::plugin::PluginManifest;
use std::collections::{HashMap, HashSet, VecDeque};

/// Resolves plugin dependencies using Kahn's algorithm (topological sort).
///
/// Returns an activation order where every plugin appears after all its
/// dependencies. Detects cycles and missing dependencies.
pub struct DependencyResolver;

impl DependencyResolver {
    /// Compute activation order for a set of plugin manifests.
    ///
    /// Returns plugin names in dependency-first order.
    pub fn resolve(manifests: &[PluginManifest]) -> Result<Vec<String>, PluginError> {
        let names: HashSet<&str> = manifests.iter().map(|m| m.name.as_str()).collect();
        let mut in_degree: HashMap<&str, usize> = HashMap::new();
        let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();

        // Build adjacency list.
        for m in manifests {
            in_degree.entry(m.name.as_str()).or_insert(0);
            for dep in &m.dependencies {
                if !names.contains(dep.as_str()) {
                    return Err(PluginError::ActivationFailed {
                        name: m.name.clone(),
                        detail: format!("Missing dependency: {dep}"),
                    });
                }
                *in_degree.entry(m.name.as_str()).or_insert(0) += 1;
                dependents
                    .entry(dep.as_str())
                    .or_default()
                    .push(m.name.as_str());
            }
        }

        // Kahn's algorithm.
        let mut queue: VecDeque<&str> = in_degree
            .iter()
            .filter(|(_, &deg)| deg == 0)
            .map(|(&name, _)| name)
            .collect();

        let mut order = Vec::with_capacity(manifests.len());

        while let Some(name) = queue.pop_front() {
            order.push(name.to_string());

            if let Some(deps) = dependents.get(name) {
                for &dep in deps {
                    if let Some(deg) = in_degree.get_mut(dep) {
                        *deg -= 1;
                        if *deg == 0 {
                            queue.push_back(dep);
                        }
                    }
                }
            }
        }

        if order.len() != manifests.len() {
            // Cycle detected — find the cycle members.
            let in_order: HashSet<&str> = order.iter().map(|s| s.as_str()).collect();
            let cycle: Vec<String> = manifests
                .iter()
                .filter(|m| !in_order.contains(m.name.as_str()))
                .map(|m| m.name.clone())
                .collect();
            return Err(PluginError::CircularDependency { cycle });
        }

        Ok(order)
    }

    /// Check if adding a new manifest would create a cycle.
    pub fn would_cycle(
        existing: &[PluginManifest],
        new: &PluginManifest,
    ) -> bool {
        let mut all = existing.to_vec();
        all.push(new.clone());
        Self::resolve(&all).is_err()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clawdesk_types::plugin::PluginCapabilities;

    fn manifest(name: &str, deps: Vec<&str>) -> PluginManifest {
        PluginManifest {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            description: String::new(),
            author: "test".to_string(),
            min_sdk_version: "0.1.0".to_string(),
            dependencies: deps.into_iter().map(String::from).collect(),
            capabilities: PluginCapabilities::default(),
        }
    }

    #[test]
    fn test_no_deps() {
        let manifests = vec![manifest("a", vec![]), manifest("b", vec![])];
        let order = DependencyResolver::resolve(&manifests).unwrap();
        assert_eq!(order.len(), 2);
    }

    #[test]
    fn test_linear_deps() {
        let manifests = vec![
            manifest("a", vec![]),
            manifest("b", vec!["a"]),
            manifest("c", vec!["b"]),
        ];
        let order = DependencyResolver::resolve(&manifests).unwrap();
        let pos_a = order.iter().position(|x| x == "a").unwrap();
        let pos_b = order.iter().position(|x| x == "b").unwrap();
        let pos_c = order.iter().position(|x| x == "c").unwrap();
        assert!(pos_a < pos_b);
        assert!(pos_b < pos_c);
    }

    #[test]
    fn test_circular_dependency() {
        let manifests = vec![
            manifest("a", vec!["b"]),
            manifest("b", vec!["a"]),
        ];
        let err = DependencyResolver::resolve(&manifests).unwrap_err();
        assert!(matches!(err, PluginError::CircularDependency { .. }));
    }

    #[test]
    fn test_missing_dependency() {
        let manifests = vec![manifest("a", vec!["missing"])];
        let err = DependencyResolver::resolve(&manifests).unwrap_err();
        assert!(matches!(err, PluginError::ActivationFailed { .. }));
    }
}
