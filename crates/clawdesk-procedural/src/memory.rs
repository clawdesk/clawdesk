//! Procedural memory — the top-level store that records episodes,
//! suggests patterns, and manages consolidation.

use crate::inhibition::InhibitionGate;
use crate::pattern::{Action, ActionOutcome, ActionPattern};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{debug, info};

/// Configuration for procedural memory.
#[derive(Debug, Clone)]
pub struct ProceduralConfig {
    /// Maximum number of stored patterns before consolidation.
    pub max_patterns: usize,
    /// Keyword overlap threshold for merging similar patterns.
    pub consolidation_threshold: f64,
    /// Action sequence overlap threshold for merging.
    pub action_merge_threshold: f64,
    /// Maximum context keywords to extract from a query.
    pub max_context_keywords: usize,
    /// Minimum confidence to include in suggestions.
    pub min_suggestion_confidence: f64,
}

impl Default for ProceduralConfig {
    fn default() -> Self {
        Self {
            max_patterns: 500,
            consolidation_threshold: 0.7,
            action_merge_threshold: 0.8,
            max_context_keywords: 20,
            min_suggestion_confidence: 0.3,
        }
    }
}

/// A complete episode being recorded.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodeRecord {
    /// Keywords describing the context (extracted from user message + tool outputs).
    pub context_keywords: Vec<String>,
    /// The sequence of actions taken.
    pub actions: Vec<Action>,
    /// Reward signal from the eval pipeline (0.0–1.0).
    pub reward: f64,
    /// Individual action outcomes (parallel to `actions`).
    pub outcomes: Vec<ActionOutcome>,
}

/// A suggestion from procedural memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatternSuggestion {
    /// The suggested action sequence.
    pub actions: Vec<Action>,
    /// Confidence in this suggestion.
    pub confidence: f64,
    /// How many times this pattern has been used.
    pub frequency: u32,
    /// The source pattern's ID.
    pub pattern_id: String,
}

impl PatternSuggestion {
    /// Format as a hint for system prompt injection.
    pub fn to_prompt_hint(&self) -> String {
        let steps: Vec<String> = self.actions.iter().enumerate().map(|(i, a)| {
            if a.argument_signature.is_empty() {
                format!("  {}. {}", i + 1, a.tool_name)
            } else {
                format!("  {}. {} ({})", i + 1, a.tool_name, a.argument_signature)
            }
        }).collect();
        format!(
            "Procedural hint ({:.0}% confidence, used {} times):\n{}",
            self.confidence * 100.0,
            self.frequency,
            steps.join("\n"),
        )
    }
}

/// The procedural memory store.
pub struct ProceduralMemory {
    config: ProceduralConfig,
    /// All learned patterns.
    patterns: Vec<ActionPattern>,
    /// Contextual inhibition gate.
    pub inhibition: InhibitionGate,
    /// Counter for pattern IDs.
    next_id: u64,
}

impl ProceduralMemory {
    pub fn new(config: ProceduralConfig) -> Self {
        Self {
            config,
            patterns: Vec::new(),
            inhibition: InhibitionGate::new(),
            next_id: 0,
        }
    }

    /// Record a completed episode for learning.
    pub fn record_episode(&mut self, episode: &EpisodeRecord) {
        // 1. Update inhibition gate from individual action outcomes
        for (action, outcome) in episode.actions.iter().zip(episode.outcomes.iter()) {
            match outcome {
                ActionOutcome::Failure => {
                    self.inhibition.record_failure(
                        &action.tool_name,
                        &episode.context_keywords,
                        "tool call failed",
                    );
                }
                ActionOutcome::Success => {
                    self.inhibition.record_success(
                        &action.tool_name,
                        &episode.context_keywords,
                    );
                }
                ActionOutcome::Partial => {} // neutral
            }
        }

        // 2. Try to merge into an existing pattern
        let merged = self.try_merge(episode);
        if merged {
            return;
        }

        // 3. Create a new pattern
        self.next_id += 1;
        let pattern = ActionPattern::from_episode(
            format!("proc_{}", self.next_id),
            episode.context_keywords.clone(),
            episode.actions.clone(),
            episode.reward,
        );
        self.patterns.push(pattern);

        // 4. Consolidate if over capacity
        if self.patterns.len() > self.config.max_patterns {
            self.consolidate();
        }
    }

    /// Suggest action sequences for a given context.
    pub fn suggest(&self, context_keywords: &[String], max_results: usize) -> Vec<PatternSuggestion> {
        let mut candidates: Vec<(usize, f64)> = self.patterns.iter().enumerate()
            .filter_map(|(i, p)| {
                let overlap = keyword_jaccard(&p.context_keywords, context_keywords);
                if overlap < 0.2 {
                    return None;
                }
                let confidence = p.confidence() * overlap;
                if confidence >= self.config.min_suggestion_confidence {
                    Some((i, confidence))
                } else {
                    None
                }
            })
            .collect();

        // Sort by confidence descending
        candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        candidates.truncate(max_results);

        candidates.into_iter().map(|(i, conf)| {
            let p = &self.patterns[i];
            PatternSuggestion {
                actions: p.action_sequence.clone(),
                confidence: conf,
                frequency: p.frequency,
                pattern_id: p.id.clone(),
            }
        }).collect()
    }

    /// Get inhibited actions for a given context (tools that consistently fail here).
    pub fn inhibited_actions(&self, context_keywords: &[String]) -> Vec<String> {
        self.inhibition.active_inhibitions(context_keywords)
            .into_iter()
            .map(|ia| format!("{}: {} (failed {} times)", ia.tool_name, ia.reason, ia.failure_count))
            .collect()
    }

    /// Consolidate patterns — merge near-duplicates, evict lowest-confidence.
    /// Called during idle time or when over capacity.
    pub fn consolidate(&mut self) {
        let before = self.patterns.len();

        // Pass 1: Merge high-overlap patterns
        let mut merged_indices = std::collections::HashSet::new();
        let len = self.patterns.len();
        for i in 0..len {
            if merged_indices.contains(&i) { continue; }
            for j in (i + 1)..len {
                if merged_indices.contains(&j) { continue; }
                let ctx_overlap = keyword_jaccard(
                    &self.patterns[i].context_keywords,
                    &self.patterns[j].context_keywords,
                );
                let action_overlap = self.patterns[i].action_overlap(&self.patterns[j]);
                if ctx_overlap >= self.config.consolidation_threshold
                    && action_overlap >= self.config.action_merge_threshold
                {
                    // Merge j into i
                    let j_reward = self.patterns[j].reward_ewma;
                    let j_freq = self.patterns[j].frequency;
                    self.patterns[i].update(j_reward);
                    self.patterns[i].frequency += j_freq.saturating_sub(1);
                    merged_indices.insert(j);
                }
            }
        }

        // Remove merged patterns (reverse order to preserve indices)
        let mut indices: Vec<usize> = merged_indices.into_iter().collect();
        indices.sort_unstable_by(|a, b| b.cmp(a));
        for idx in indices {
            self.patterns.swap_remove(idx);
        }

        // Pass 2: Evict lowest-confidence if still over capacity
        if self.patterns.len() > self.config.max_patterns {
            self.patterns.sort_by(|a, b| {
                b.confidence().partial_cmp(&a.confidence()).unwrap_or(std::cmp::Ordering::Equal)
            });
            self.patterns.truncate(self.config.max_patterns);
        }

        // GC inhibition gate too
        self.inhibition.gc();

        let after = self.patterns.len();
        if before != after {
            info!(before, after, "procedural memory consolidated");
        }
    }

    /// Try to merge an episode into an existing pattern.
    fn try_merge(&mut self, episode: &EpisodeRecord) -> bool {
        for pattern in self.patterns.iter_mut() {
            let ctx_overlap = keyword_jaccard(&pattern.context_keywords, &episode.context_keywords);
            if ctx_overlap < self.config.consolidation_threshold {
                continue;
            }

            let episode_pattern = ActionPattern::from_episode(
                "tmp",
                episode.context_keywords.clone(),
                episode.actions.clone(),
                episode.reward,
            );
            let action_overlap = pattern.action_overlap(&episode_pattern);
            if action_overlap >= self.config.action_merge_threshold {
                pattern.update(episode.reward);
                debug!(pattern_id = %pattern.id, reward = episode.reward, "merged episode into existing pattern");
                return true;
            }
        }
        false
    }

    pub fn pattern_count(&self) -> usize {
        self.patterns.len()
    }

    pub fn inhibition_count(&self) -> usize {
        self.inhibition.len()
    }
}

impl Default for ProceduralMemory {
    fn default() -> Self {
        Self::new(ProceduralConfig::default())
    }
}

/// Keyword Jaccard similarity.
fn keyword_jaccard(a: &[String], b: &[String]) -> f64 {
    use std::collections::HashSet;
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let sa: HashSet<&str> = a.iter().map(|s| s.as_str()).collect();
    let sb: HashSet<&str> = b.iter().map(|s| s.as_str()).collect();
    let intersection = sa.intersection(&sb).count();
    let union = sa.union(&sb).count();
    if union == 0 { 0.0 } else { intersection as f64 / union as f64 }
}

/// Extract context keywords from a user message (cheap bag-of-words).
pub fn extract_keywords(text: &str) -> Vec<String> {
    // Split on whitespace and punctuation, lowercase, dedup, skip stopwords
    let stopwords = [
        "the", "a", "an", "is", "are", "was", "were", "be", "been", "being",
        "have", "has", "had", "do", "does", "did", "will", "would", "could",
        "should", "may", "might", "can", "shall", "to", "of", "in", "for",
        "on", "with", "at", "by", "from", "as", "into", "through", "during",
        "before", "after", "above", "below", "between", "and", "but", "or",
        "not", "no", "nor", "so", "yet", "both", "either", "neither", "each",
        "this", "that", "these", "those", "it", "its", "i", "me", "my", "you",
        "your", "he", "she", "we", "they", "them", "his", "her", "our", "their",
        "what", "which", "who", "whom", "how", "when", "where", "why",
        "if", "then", "else", "than", "just", "also", "very", "too",
    ];

    let mut keywords: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .filter(|w| w.len() >= 2)
        .map(|w| w.to_lowercase())
        .filter(|w| !stopwords.contains(&w.as_str()))
        .collect();
    keywords.sort_unstable();
    keywords.dedup();
    keywords.truncate(20);
    keywords
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_and_suggest() {
        let mut mem = ProceduralMemory::default();
        let episode = EpisodeRecord {
            context_keywords: vec!["rust".into(), "build".into(), "error".into()],
            actions: vec![
                Action::new("read_file", "Cargo.toml"),
                Action::new("execute_command", "cargo build"),
                Action::new("read_file", "src/lib.rs"),
            ],
            reward: 0.9,
            outcomes: vec![ActionOutcome::Success, ActionOutcome::Success, ActionOutcome::Success],
        };
        mem.record_episode(&episode);

        let suggestions = mem.suggest(&["rust".into(), "build".into()], 5);
        assert!(!suggestions.is_empty());
        assert!(suggestions[0].confidence > 0.0);
    }

    #[test]
    fn similar_episodes_merge() {
        let mut mem = ProceduralMemory::default();
        let base = EpisodeRecord {
            context_keywords: vec!["python".into(), "test".into()],
            actions: vec![
                Action::new("execute_command", "pytest"),
                Action::new("read_file", "test_output.txt"),
            ],
            reward: 0.85,
            outcomes: vec![ActionOutcome::Success, ActionOutcome::Success],
        };

        mem.record_episode(&base);
        mem.record_episode(&base); // identical → should merge

        assert_eq!(mem.pattern_count(), 1, "identical episodes should merge");
    }

    #[test]
    fn inhibition_from_failures() {
        let mut mem = ProceduralMemory::default();
        let episode = EpisodeRecord {
            context_keywords: vec!["deploy".into(), "production".into()],
            actions: vec![
                Action::new("execute_command", "rm -rf /tmp/cache"),
            ],
            reward: 0.0,
            outcomes: vec![ActionOutcome::Failure],
        };
        mem.record_episode(&episode);

        let inhibited = mem.inhibited_actions(&["deploy".into(), "production".into()]);
        assert!(!inhibited.is_empty());
    }

    #[test]
    fn extract_keywords_works() {
        let kw = extract_keywords("Fix the Rust build error in src/main.rs");
        assert!(kw.contains(&"rust".to_string()));
        assert!(kw.contains(&"build".to_string()));
        assert!(kw.contains(&"error".to_string()));
        assert!(!kw.contains(&"the".to_string()));
    }

    #[test]
    fn prompt_hint_formatting() {
        let suggestion = PatternSuggestion {
            actions: vec![
                Action::new("read_file", "Cargo.toml"),
                Action::new("execute_command", "cargo test"),
            ],
            confidence: 0.85,
            frequency: 12,
            pattern_id: "proc_1".into(),
        };
        let hint = suggestion.to_prompt_hint();
        assert!(hint.contains("85%"));
        assert!(hint.contains("12 times"));
        assert!(hint.contains("read_file"));
    }
}
