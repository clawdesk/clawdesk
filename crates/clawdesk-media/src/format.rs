//! Trie-based MIME format routing with weighted affinity scoring.
//!
//! ## Architecture
//!
//! A trie over MIME type segments (`type` / `subtype` / `parameter=value`)
//! provides longest-prefix matching in `O(d)` where `d = depth` (typically ≤ 4).
//!
//! Each trie node stores candidate processors with affinity scores.
//! Processor selection: `argmax_p affinity(p, request)` — `O(k)` for `k` candidates.
//!
//! Total routing cost: `O(d + k)` ≈ `O(1)` in practice.
//!
//! ## Fallback
//!
//! - `audio/ogg; codecs=opus` → try `audio/ogg` → try `audio/*` → try `*/*`
//! - Subsumes fallback into a single `O(d)` traversal.
//!
//! ## Affinity Scoring
//!
//! `affinity(p, req) = Σⱼ wⱼ · fⱼ(p, req)`
//! Features: latency estimate, fidelity rating, cache warmth, current load.

use std::collections::HashMap;
use std::fmt;

/// A candidate processor registered at a trie node.
#[derive(Debug, Clone)]
pub struct FormatCandidate {
    /// Processor name.
    pub name: String,
    /// Base fidelity rating [0.0, 1.0] — how well this processor handles the format.
    pub fidelity: f64,
    /// Expected latency in milliseconds.
    pub expected_latency_ms: u64,
    /// Current load factor [0.0, 1.0] — 0 = idle, 1 = at capacity.
    pub load_factor: f64,
    /// Cache warmth [0.0, 1.0] — probability of cache hit for this format.
    pub cache_warmth: f64,
}

/// Affinity weights for processor selection.
#[derive(Debug, Clone)]
pub struct AffinityWeights {
    /// Weight for latency (lower latency = higher score).
    pub latency: f64,
    /// Weight for fidelity (higher fidelity = higher score).
    pub fidelity: f64,
    /// Weight for cache warmth (higher warmth = higher score).
    pub cache_warmth: f64,
    /// Weight for load (lower load = higher score).
    pub load: f64,
}

impl Default for AffinityWeights {
    fn default() -> Self {
        Self {
            latency: 1.0,
            fidelity: 2.0,
            cache_warmth: 0.5,
            load: 1.0,
        }
    }
}

impl AffinityWeights {
    /// Compute affinity score for a candidate.
    ///
    /// `affinity = w_fidelity × fidelity + w_cache × cache_warmth
    ///            - w_latency × normalized_latency - w_load × load_factor`
    pub fn score(&self, candidate: &FormatCandidate) -> f64 {
        let normalized_latency = (candidate.expected_latency_ms as f64 / 10000.0).min(1.0);

        self.fidelity * candidate.fidelity
            + self.cache_warmth * candidate.cache_warmth
            - self.latency * normalized_latency
            - self.load * candidate.load_factor
    }
}

/// A node in the MIME trie.
#[derive(Debug)]
struct TrieNode {
    /// Segment value (e.g., "audio", "ogg", "codecs=opus").
    segment: String,
    /// Candidate processors at this specificity level.
    candidates: Vec<FormatCandidate>,
    /// Children keyed by next segment.
    children: HashMap<String, TrieNode>,
}

impl TrieNode {
    fn new(segment: impl Into<String>) -> Self {
        Self {
            segment: segment.into(),
            candidates: Vec::new(),
            children: HashMap::new(),
        }
    }

    fn insert_child(&mut self, segment: &str) -> &mut TrieNode {
        self.children
            .entry(segment.to_string())
            .or_insert_with(|| TrieNode::new(segment))
    }
}

/// Trie-based MIME format router.
///
/// Lookup: `O(d)` where `d` = MIME hierarchy depth (typically 2-3).
/// Selection: `O(k)` where `k` = candidates at matched node (typically ≤ 4).
pub struct FormatRouter {
    root: TrieNode,
    weights: AffinityWeights,
}

/// Result of a format routing lookup.
#[derive(Debug, Clone)]
pub struct RouteResult {
    /// Best candidate processor name.
    pub processor: String,
    /// Affinity score of the selected processor.
    pub affinity: f64,
    /// Matched MIME pattern (may be a wildcard).
    pub matched_pattern: String,
    /// All candidate processors with scores (for transparency).
    pub all_candidates: Vec<(String, f64)>,
    /// Match specificity level.
    pub specificity: usize,
}

impl FormatRouter {
    /// Create a new format router with default affinity weights.
    pub fn new() -> Self {
        Self {
            root: TrieNode::new("*"),
            weights: AffinityWeights::default(),
        }
    }

    /// Create with custom affinity weights.
    pub fn with_weights(weights: AffinityWeights) -> Self {
        Self {
            root: TrieNode::new("*"),
            weights,
        }
    }

    /// Parse a MIME type into trie segments.
    ///
    /// `audio/ogg; codecs=opus` → `["audio", "ogg", "codecs=opus"]`
    fn parse_mime(mime: &str) -> Vec<String> {
        let mime = mime.trim().to_lowercase();
        let mut segments = Vec::new();

        // Split type/subtype.
        let (type_subtype, params) = mime
            .split_once(';')
            .map(|(ts, p)| (ts.trim(), Some(p.trim())))
            .unwrap_or((mime.as_str(), None));

        if let Some((type_part, subtype)) = type_subtype.split_once('/') {
            segments.push(type_part.to_string());
            if subtype != "*" {
                segments.push(subtype.to_string());
            }
        } else {
            segments.push(type_subtype.to_string());
        }

        // Add parameters as additional segments.
        if let Some(params) = params {
            for param in params.split(';') {
                let p = param.trim();
                if !p.is_empty() {
                    segments.push(p.to_string());
                }
            }
        }

        segments
    }

    /// Register a processor for a MIME pattern.
    ///
    /// Patterns can be specific (`audio/ogg; codecs=opus`) or wildcards (`audio/*`).
    pub fn register(&mut self, mime_pattern: &str, candidate: FormatCandidate) {
        let segments = Self::parse_mime(mime_pattern);
        let mut node = &mut self.root;

        for segment in &segments {
            node = node.insert_child(segment);
        }

        node.candidates.push(candidate);
    }

    /// Route a MIME type to the best processor.
    ///
    /// Uses longest-prefix matching with fallback to less specific nodes.
    /// At each matched node, selects `argmax_p affinity(p, request)`.
    pub fn route(&self, mime_type: &str) -> Option<RouteResult> {
        let segments = Self::parse_mime(mime_type);
        let mut node = &self.root;
        let mut best_candidates: Option<(&[FormatCandidate], String, usize)> = None;

        // Check root-level wildcard candidates.
        if !node.candidates.is_empty() {
            best_candidates = Some((&node.candidates, "*/*".to_string(), 0));
        }

        // Walk down the trie, collecting the most specific match.
        let mut matched_pattern = String::new();
        for (depth, segment) in segments.iter().enumerate() {
            if let Some(child) = node.children.get(segment) {
                if depth == 0 {
                    matched_pattern = segment.clone();
                } else {
                    matched_pattern = format!("{}/{}", matched_pattern, segment);
                }

                if !child.candidates.is_empty() {
                    best_candidates = Some((&child.candidates, matched_pattern.clone(), depth + 1));
                }
                node = child;
            } else {
                // No more specific match — use what we have.
                break;
            }
        }

        // Select best candidate from the matched node.
        let (candidates, pattern, specificity) = best_candidates?;

        let mut scored: Vec<(String, f64)> = candidates
            .iter()
            .map(|c| (c.name.clone(), self.weights.score(c)))
            .collect();

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let (best_name, best_score) = scored.first()?.clone();

        Some(RouteResult {
            processor: best_name,
            affinity: best_score,
            matched_pattern: pattern,
            all_candidates: scored,
            specificity,
        })
    }

    /// Update a processor's dynamic stats (load, cache warmth).
    pub fn update_stats(
        &mut self,
        processor_name: &str,
        load_factor: Option<f64>,
        cache_warmth: Option<f64>,
        latency_ms: Option<u64>,
    ) {
        Self::update_node_stats(&mut self.root, processor_name, load_factor, cache_warmth, latency_ms);
    }

    fn update_node_stats(
        node: &mut TrieNode,
        name: &str,
        load: Option<f64>,
        warmth: Option<f64>,
        latency: Option<u64>,
    ) {
        for candidate in &mut node.candidates {
            if candidate.name == name {
                if let Some(l) = load {
                    candidate.load_factor = l;
                }
                if let Some(w) = warmth {
                    candidate.cache_warmth = w;
                }
                if let Some(ms) = latency {
                    candidate.expected_latency_ms = ms;
                }
            }
        }
        for child in node.children.values_mut() {
            Self::update_node_stats(child, name, load, warmth, latency);
        }
    }

    /// List all registered MIME patterns.
    pub fn registered_patterns(&self) -> Vec<String> {
        let mut patterns = Vec::new();
        Self::collect_patterns(&self.root, "", &mut patterns);
        patterns
    }

    fn collect_patterns(node: &TrieNode, prefix: &str, patterns: &mut Vec<String>) {
        let path = if prefix.is_empty() {
            node.segment.clone()
        } else if node.segment == "*" {
            prefix.to_string()
        } else {
            format!("{}/{}", prefix, node.segment)
        };

        if !node.candidates.is_empty() {
            patterns.push(path.clone());
        }

        for child in node.children.values() {
            Self::collect_patterns(child, &path, patterns);
        }
    }
}

impl Default for FormatRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for FormatRouter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let patterns = self.registered_patterns();
        write!(f, "FormatRouter({} patterns)", patterns.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_candidate(name: &str, fidelity: f64, latency_ms: u64) -> FormatCandidate {
        FormatCandidate {
            name: name.into(),
            fidelity,
            expected_latency_ms: latency_ms,
            load_factor: 0.0,
            cache_warmth: 0.0,
        }
    }

    #[test]
    fn exact_mime_match() {
        let mut router = FormatRouter::new();
        router.register("audio/mpeg", make_candidate("mp3-processor", 0.9, 100));

        let result = router.route("audio/mpeg").unwrap();
        assert_eq!(result.processor, "mp3-processor");
        assert_eq!(result.specificity, 2);
    }

    #[test]
    fn wildcard_fallback() {
        let mut router = FormatRouter::new();
        // Register only a type-level wildcard.
        router.register("audio/*", make_candidate("generic-audio", 0.7, 200));

        // Specific subtype should fall back to audio/*.
        let result = router.route("audio/ogg").unwrap();
        assert_eq!(result.processor, "generic-audio");
    }

    #[test]
    fn most_specific_wins() {
        let mut router = FormatRouter::new();
        router.register("audio/*", make_candidate("generic-audio", 0.5, 200));
        router.register("audio/mpeg", make_candidate("mp3-specialist", 0.95, 50));

        // audio/mpeg should match the specialist, not the generic.
        let result = router.route("audio/mpeg").unwrap();
        assert_eq!(result.processor, "mp3-specialist");
        assert_eq!(result.specificity, 2); // matched at subtype level
    }

    #[test]
    fn affinity_scoring_picks_best() {
        let mut router = FormatRouter::new();
        router.register("image/png", make_candidate("fast-but-bad", 0.3, 10));
        router.register("image/png", make_candidate("slow-but-good", 0.95, 5000));

        // With default weights (fidelity weight=2.0, latency weight=1.0),
        // slow-but-good should win because fidelity is weighted higher.
        let result = router.route("image/png").unwrap();
        assert_eq!(result.processor, "slow-but-good");
    }

    #[test]
    fn parameter_specificity() {
        let mut router = FormatRouter::new();
        router.register("audio/ogg", make_candidate("ogg-generic", 0.7, 100));
        router.register(
            "audio/ogg; codecs=opus",
            make_candidate("opus-specialist", 0.95, 50),
        );

        // Request with codecs=opus should match the specialist.
        let result = router.route("audio/ogg; codecs=opus").unwrap();
        assert_eq!(result.processor, "opus-specialist");
        assert_eq!(result.specificity, 3); // type + subtype + param

        // Request without params should match generic.
        let result = router.route("audio/ogg").unwrap();
        assert_eq!(result.processor, "ogg-generic");
    }

    #[test]
    fn no_match_returns_none() {
        let router = FormatRouter::new();
        assert!(router.route("application/x-unknown").is_none());
    }

    #[test]
    fn dynamic_stats_update() {
        let mut router = FormatRouter::new();
        router.register("audio/wav", make_candidate("wav-proc", 0.8, 100));
        router.register("audio/wav", make_candidate("wav-alt", 0.75, 80));

        // Initially wav-proc wins (higher fidelity).
        let result = router.route("audio/wav").unwrap();
        assert_eq!(result.processor, "wav-proc");

        // After wav-proc becomes overloaded, wav-alt should win.
        router.update_stats("wav-proc", Some(0.95), None, None);
        let result = router.route("audio/wav").unwrap();
        assert_eq!(result.processor, "wav-alt");
    }

    #[test]
    fn multiple_candidates_all_returned() {
        let mut router = FormatRouter::new();
        router.register("video/mp4", make_candidate("proc-a", 0.8, 100));
        router.register("video/mp4", make_candidate("proc-b", 0.7, 200));
        router.register("video/mp4", make_candidate("proc-c", 0.9, 150));

        let result = router.route("video/mp4").unwrap();
        assert_eq!(result.all_candidates.len(), 3);
        // Best should be proc-c (highest fidelity).
        assert_eq!(result.processor, "proc-c");
    }

    #[test]
    fn mime_parsing() {
        let segments = FormatRouter::parse_mime("audio/ogg; codecs=opus; rate=48000");
        assert_eq!(segments, vec!["audio", "ogg", "codecs=opus", "rate=48000"]);

        let segments = FormatRouter::parse_mime("image/png");
        assert_eq!(segments, vec!["image", "png"]);

        let segments = FormatRouter::parse_mime("audio/*");
        assert_eq!(segments, vec!["audio"]);
    }
}
