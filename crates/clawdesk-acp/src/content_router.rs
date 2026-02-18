//! Aho-Corasick content-based message routing.
//!
//! ## Problem
//!
//! Message routing that examines content for pattern matches (skill keywords,
//! capability markers, domain terms) is O(L × P) with naive string matching
//! where L = message length and P = number of patterns.
//!
//! ## Algorithm
//!
//! Build an Aho-Corasick automaton from routing patterns, achieving
//! O(L + R_matched) routing — linear in message length plus matched results,
//! independent of pattern count.
//!
//! ## Design
//!
//! Each registered pattern maps to a set of target agent IDs. When a message
//! arrives, the automaton scans the content in a single pass, collecting all
//! matched patterns. Agents are then scored by the number of pattern hits
//! and their associated weights. The highest-scoring agent wins.
//!
//! ## Complexity
//!
//! - Build: O(Σ|p_i|) — sum of all pattern lengths.
//! - Query: O(L + R) — L = input length, R = number of matches.
//! - Space: O(Σ|p_i|) — the automaton itself.

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};
use std::collections::HashMap;

/// A routing rule that maps a keyword/pattern to target agents.
#[derive(Debug, Clone)]
pub struct RoutingRule {
    /// The pattern to match (keyword, phrase, or regex fragment).
    pub pattern: String,
    /// Agent IDs that should receive messages matching this pattern.
    pub target_agents: Vec<String>,
    /// Weight of this pattern match for scoring (default: 1.0).
    pub weight: f64,
    /// Optional category for grouping patterns.
    pub category: Option<String>,
}

/// Result of content-based routing.
#[derive(Debug, Clone)]
pub struct ContentRouteResult {
    /// Agent ID → aggregate score from pattern matches.
    pub agent_scores: HashMap<String, f64>,
    /// All patterns that matched.
    pub matched_patterns: Vec<MatchedPattern>,
    /// The winning agent (highest score), if any.
    pub best_agent: Option<String>,
    /// Score of the best agent.
    pub best_score: f64,
}

/// A single pattern match found in the content.
#[derive(Debug, Clone)]
pub struct MatchedPattern {
    /// Pattern index in the original rule list.
    pub rule_index: usize,
    /// The matched pattern text.
    pub pattern: String,
    /// Byte offset where the match starts.
    pub start: usize,
    /// Byte offset where the match ends.
    pub end: usize,
}

/// Content-based router using Aho-Corasick automaton.
///
/// Performs multi-pattern matching in a single pass over the message content.
/// O(L + R) where L = content length and R = number of matches.
pub struct ContentRouter {
    /// Compiled Aho-Corasick automaton.
    automaton: AhoCorasick,
    /// Routing rules (indexed by pattern position in the automaton).
    rules: Vec<RoutingRule>,
    /// Minimum score threshold for routing (agent must exceed this to be selected).
    min_score_threshold: f64,
}

impl ContentRouter {
    /// Build a new content router from routing rules.
    ///
    /// ## Build Complexity
    ///
    /// O(Σ|p_i|) — sum of all pattern lengths.
    pub fn new(rules: Vec<RoutingRule>, min_score_threshold: f64) -> Self {
        let patterns: Vec<&str> = rules.iter().map(|r| r.pattern.as_str()).collect();

        let automaton = AhoCorasickBuilder::new()
            .match_kind(MatchKind::Standard)
            .ascii_case_insensitive(true)
            .build(&patterns)
            .expect("aho-corasick automaton build");

        Self {
            automaton,
            rules,
            min_score_threshold,
        }
    }

    /// Route a message based on its content.
    ///
    /// ## Query Complexity
    ///
    /// O(L + R) — L = content length, R = number of matches.
    pub fn route(&self, content: &str) -> ContentRouteResult {
        let mut agent_scores: HashMap<String, f64> = HashMap::new();
        let mut matched_patterns: Vec<MatchedPattern> = Vec::new();

        for mat in self.automaton.find_iter(content) {
            let rule_idx = mat.pattern().as_usize();
            let rule = &self.rules[rule_idx];

            matched_patterns.push(MatchedPattern {
                rule_index: rule_idx,
                pattern: rule.pattern.clone(),
                start: mat.start(),
                end: mat.end(),
            });

            for agent_id in &rule.target_agents {
                *agent_scores.entry(agent_id.clone()).or_insert(0.0) += rule.weight;
            }
        }

        // Find best agent.
        let (best_agent, best_score) = agent_scores
            .iter()
            .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(k, v)| (Some(k.clone()), *v))
            .unwrap_or((None, 0.0));

        // Apply threshold.
        let best_agent = if best_score >= self.min_score_threshold {
            best_agent
        } else {
            None
        };

        ContentRouteResult {
            agent_scores,
            matched_patterns,
            best_agent,
            best_score,
        }
    }

    /// Route and return only the best agent ID, if any.
    pub fn route_best(&self, content: &str) -> Option<String> {
        self.route(content).best_agent
    }

    /// Get the number of registered patterns.
    pub fn pattern_count(&self) -> usize {
        self.rules.len()
    }

    /// Get all categories used in routing rules.
    pub fn categories(&self) -> Vec<String> {
        let mut cats: Vec<String> = self
            .rules
            .iter()
            .filter_map(|r| r.category.clone())
            .collect();
        cats.sort();
        cats.dedup();
        cats
    }
}

/// Builder for constructing routing rules incrementally.
pub struct ContentRouterBuilder {
    rules: Vec<RoutingRule>,
    min_score_threshold: f64,
}

impl ContentRouterBuilder {
    pub fn new() -> Self {
        Self {
            rules: Vec::new(),
            min_score_threshold: 0.0,
        }
    }

    /// Set the minimum score threshold for routing.
    pub fn min_score(mut self, threshold: f64) -> Self {
        self.min_score_threshold = threshold;
        self
    }

    /// Add a routing rule.
    pub fn rule(mut self, pattern: &str, agents: &[&str], weight: f64) -> Self {
        self.rules.push(RoutingRule {
            pattern: pattern.to_string(),
            target_agents: agents.iter().map(|s| s.to_string()).collect(),
            weight,
            category: None,
        });
        self
    }

    /// Add a routing rule with a category.
    pub fn categorized_rule(
        mut self,
        pattern: &str,
        agents: &[&str],
        weight: f64,
        category: &str,
    ) -> Self {
        self.rules.push(RoutingRule {
            pattern: pattern.to_string(),
            target_agents: agents.iter().map(|s| s.to_string()).collect(),
            weight,
            category: Some(category.to_string()),
        });
        self
    }

    /// Build the content router.
    pub fn build(self) -> ContentRouter {
        ContentRouter::new(self.rules, self.min_score_threshold)
    }
}

impl Default for ContentRouterBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_router() -> ContentRouter {
        ContentRouterBuilder::new()
            .rule("rust", &["code-agent"], 2.0)
            .rule("python", &["code-agent", "data-agent"], 2.0)
            .rule("summarize", &["nlp-agent"], 3.0)
            .rule("translate", &["nlp-agent", "translate-agent"], 2.5)
            .rule("image", &["vision-agent"], 2.0)
            .rule("transcribe", &["audio-agent"], 3.0)
            .rule("audio", &["audio-agent"], 1.5)
            .build()
    }

    #[test]
    fn single_pattern_match() {
        let router = make_router();
        let result = router.route("Please transcribe this audio file");

        assert_eq!(result.best_agent.as_deref(), Some("audio-agent"));
        // "transcribe" (3.0) + "audio" (1.5) = 4.5
        assert!((result.best_score - 4.5).abs() < 0.01);
        assert_eq!(result.matched_patterns.len(), 2);
    }

    #[test]
    fn multi_agent_scoring() {
        let router = make_router();
        let result = router.route("Write python code to process this image");

        // code-agent: python(2.0) = 2.0
        // data-agent: python(2.0) = 2.0
        // vision-agent: image(2.0) = 2.0
        assert_eq!(result.matched_patterns.len(), 2);
        assert!(result.agent_scores.contains_key("code-agent"));
        assert!(result.agent_scores.contains_key("vision-agent"));
    }

    #[test]
    fn no_match() {
        let router = make_router();
        let result = router.route("What is the weather today?");

        assert!(result.best_agent.is_none());
        assert!(result.matched_patterns.is_empty());
    }

    #[test]
    fn case_insensitive() {
        let router = make_router();
        let result = router.route("SUMMARIZE this document");

        assert_eq!(result.best_agent.as_deref(), Some("nlp-agent"));
    }

    #[test]
    fn threshold_filtering() {
        let router = ContentRouterBuilder::new()
            .min_score(5.0)
            .rule("hello", &["greeter"], 1.0)
            .build();

        let result = router.route("hello world");
        // Score 1.0 < threshold 5.0, so no agent selected.
        assert!(result.best_agent.is_none());
    }

    #[test]
    fn multiple_matches_same_pattern() {
        let router = ContentRouterBuilder::new()
            .rule("code", &["coder"], 1.0)
            .build();

        let result = router.route("code review the code and fix the code");
        // "code" appears 3 times → score = 3.0
        assert_eq!(result.matched_patterns.len(), 3);
        assert!((result.best_score - 3.0).abs() < 0.01);
    }

    #[test]
    fn overlapping_patterns() {
        let router = ContentRouterBuilder::new()
            .rule("rust", &["rustacean"], 1.0)
            .rule("rusty", &["mechanic"], 1.0)
            .build();

        let result = router.route("this is rusty old code");
        // With Standard match kind, "rusty" matches but "rust" may also match.
        assert!(!result.matched_patterns.is_empty());
    }

    #[test]
    fn categories() {
        let router = ContentRouterBuilder::new()
            .categorized_rule("rust", &["coder"], 1.0, "programming")
            .categorized_rule("python", &["coder"], 1.0, "programming")
            .categorized_rule("summarize", &["nlp"], 1.0, "nlp")
            .build();

        let cats = router.categories();
        assert_eq!(cats, vec!["nlp", "programming"]);
    }

    #[test]
    fn pattern_count() {
        let router = make_router();
        assert_eq!(router.pattern_count(), 7);
    }

    #[test]
    fn route_best_shortcut() {
        let router = make_router();
        let best = router.route_best("Please summarize and translate this text");
        // nlp-agent: summarize(3.0) + translate(2.5) = 5.5
        // translate-agent: translate(2.5) = 2.5
        assert_eq!(best.as_deref(), Some("nlp-agent"));
    }
}
