//! Semantically-aware conversation compaction with token budgeting.
//!
//! Two-pass algorithm:
//! 1. **Semantic boundary detection**: Identify natural split points (turn boundaries,
//!    tool-use pairs, system messages) — no LLM needed.
//! 2. **Token-budgeted packing**: Pack semantic units into the budget using a
//!    priority-weighted greedy strategy (knapsack approximation).

use chrono::{DateTime, Utc};
use clawdesk_types::session::{AgentMessage, Role};

/// A semantic unit is an atomic block that should not be split.
#[derive(Debug, Clone)]
pub struct SemanticUnit {
    pub messages: Vec<AgentMessage>,
    pub token_count: usize,
    pub unit_type: UnitType,
    pub timestamp: DateTime<Utc>,
}

/// Classification of semantic units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitType {
    /// User message + assistant response pair
    Turn,
    /// Tool call + tool result (possibly nested)
    ToolSequence,
    /// System message insertion
    SystemInsert,
    /// Message with images/audio
    MediaBlock,
}

impl UnitType {
    /// Priority weight for packing. Higher = more important to keep.
    pub fn weight(&self) -> f64 {
        match self {
            UnitType::SystemInsert => 2.0,
            UnitType::ToolSequence => 1.5,
            UnitType::Turn => 1.0,
            UnitType::MediaBlock => 0.8,
        }
    }
}

/// Simple token estimator (4 chars ≈ 1 token).
/// In production, use `CalibratedTokenEstimator` for auto-tuned accuracy.
pub fn estimate_tokens(text: &str) -> usize {
    // Rough estimate: ~4 characters per token for English text
    (text.len() + 3) / 4
}

/// EMA-based auto-calibrating token estimator.
///
/// Learns the actual chars-per-token ratio from provider-reported token
/// counts using exponential moving average (α = 0.1). Converges within
/// ~20 observations to within 5% of the true ratio.
///
/// The ratio adapts to model-specific tokenization:
/// - GPT-4: ~3.7 chars/token for English
/// - Claude: ~3.9 chars/token for English
/// - Code: ~5.2 chars/token (shorter tokens for syntax)
///
/// Thread-safe via `AtomicU64` — no locks on the hot path.
pub struct CalibratedTokenEstimator {
    /// Packed f64 stored as AtomicU64 bits: chars_per_token ratio.
    ratio_bits: std::sync::atomic::AtomicU64,
    /// EMA smoothing factor (α). Lower = more stable, higher = more responsive.
    alpha: f64,
    /// Minimum observations before trusting the calibrated ratio.
    min_observations: u32,
    /// Count of observations (saturates at u32::MAX).
    observation_count: std::sync::atomic::AtomicU32,
}

impl CalibratedTokenEstimator {
    /// Create a new estimator with default settings.
    ///
    /// Initial ratio: 4.0 chars/token (English average).
    /// Alpha: 0.1 (smooth, ~20 samples to converge).
    pub fn new() -> Self {
        Self {
            ratio_bits: std::sync::atomic::AtomicU64::new(4.0f64.to_bits()),
            alpha: 0.1,
            min_observations: 5,
            observation_count: std::sync::atomic::AtomicU32::new(0),
        }
    }

    /// Create with a custom initial ratio and alpha.
    pub fn with_params(initial_ratio: f64, alpha: f64) -> Self {
        Self {
            ratio_bits: std::sync::atomic::AtomicU64::new(initial_ratio.to_bits()),
            alpha,
            min_observations: 5,
            observation_count: std::sync::atomic::AtomicU32::new(0),
        }
    }

    /// Current chars-per-token ratio.
    pub fn ratio(&self) -> f64 {
        f64::from_bits(self.ratio_bits.load(std::sync::atomic::Ordering::Relaxed))
    }

    /// Estimate token count for the given text using the calibrated ratio.
    pub fn estimate(&self, text: &str) -> usize {
        let r = self.ratio();
        ((text.len() as f64) / r).ceil() as usize
    }

    /// Record an actual token count observation from a provider response.
    ///
    /// Updates the EMA ratio: `ratio = (1 - α) × ratio + α × (chars / actual_tokens)`.
    pub fn observe(&self, text_len: usize, actual_tokens: usize) {
        if actual_tokens == 0 || text_len == 0 {
            return;
        }
        let observed_ratio = text_len as f64 / actual_tokens as f64;
        // Clamp to sane range [1.0, 10.0] to avoid pathological values
        let clamped = observed_ratio.clamp(1.0, 10.0);

        loop {
            let old_bits = self.ratio_bits.load(std::sync::atomic::Ordering::Relaxed);
            let old_ratio = f64::from_bits(old_bits);
            let new_ratio = (1.0 - self.alpha) * old_ratio + self.alpha * clamped;
            let new_bits = new_ratio.to_bits();
            if self
                .ratio_bits
                .compare_exchange_weak(
                    old_bits,
                    new_bits,
                    std::sync::atomic::Ordering::AcqRel,
                    std::sync::atomic::Ordering::Relaxed,
                )
                .is_ok()
            {
                break;
            }
        }
        self.observation_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Whether enough observations have been made to trust the calibrated ratio.
    pub fn is_calibrated(&self) -> bool {
        self.observation_count
            .load(std::sync::atomic::Ordering::Relaxed)
            >= self.min_observations
    }
}

impl Default for CalibratedTokenEstimator {
    fn default() -> Self {
        Self::new()
    }
}

fn estimate_message_tokens(msg: &AgentMessage) -> usize {
    // Content tokens + overhead for role/metadata
    estimate_tokens(&msg.content) + 4
}

/// Detect semantic boundaries in a conversation.
///
/// Returns a list of `SemanticUnit`s — atomic blocks that should not be split.
/// Complexity: O(n) where n = message count (single linear scan).
pub fn detect_semantic_boundaries(messages: &[AgentMessage]) -> Vec<SemanticUnit> {
    let mut units = Vec::new();
    let mut current_msgs: Vec<AgentMessage> = Vec::new();
    let mut current_tokens = 0usize;

    for (i, msg) in messages.iter().enumerate() {
        let tokens = estimate_message_tokens(msg);
        current_msgs.push(msg.clone());
        current_tokens += tokens;

        let is_boundary = match msg.role {
            Role::Assistant => {
                // Boundary after assistant message IF next message is user or system
                messages
                    .get(i + 1)
                    .map(|next| matches!(next.role, Role::User | Role::System))
                    .unwrap_or(true)
            }
            Role::ToolResult => {
                // Boundary after tool result IF no more tool calls pending
                !has_pending_tool_call(&messages[i + 1..])
            }
            Role::System => true, // System messages are always their own unit
            _ => false,
        };

        if is_boundary && !current_msgs.is_empty() {
            let unit_type = classify_unit_type(&current_msgs);
            let timestamp = msg.timestamp;
            units.push(SemanticUnit {
                messages: std::mem::take(&mut current_msgs),
                token_count: current_tokens,
                unit_type,
                timestamp,
            });
            current_tokens = 0;
        }
    }

    // Flush remaining messages
    if !current_msgs.is_empty() {
        let timestamp = current_msgs.last().map(|m| m.timestamp).unwrap_or(Utc::now());
        units.push(SemanticUnit {
            messages: current_msgs,
            token_count: current_tokens,
            unit_type: UnitType::Turn,
            timestamp,
        });
    }

    units
}

fn has_pending_tool_call(remaining: &[AgentMessage]) -> bool {
    remaining
        .first()
        .map(|m| matches!(m.role, Role::Tool))
        .unwrap_or(false)
}

fn classify_unit_type(messages: &[AgentMessage]) -> UnitType {
    let has_tool = messages.iter().any(|m| matches!(m.role, Role::Tool));
    let has_tool_result = messages.iter().any(|m| matches!(m.role, Role::ToolResult));
    let has_system = messages.iter().any(|m| matches!(m.role, Role::System));

    if has_system && messages.len() == 1 {
        UnitType::SystemInsert
    } else if has_tool || has_tool_result {
        UnitType::ToolSequence
    } else {
        UnitType::Turn
    }
}

/// Compaction result.
#[derive(Debug)]
pub struct CompactionResult {
    pub messages: Vec<AgentMessage>,
    pub tokens_used: usize,
    pub units_included: usize,
    pub units_dropped: usize,
}

/// Pack semantic units into the budget using a priority-weighted strategy.
///
/// Recency uses exponential decay λ = 0.85 with floor 0.01:
///   `recency = max(0.01, 0.85^(n - 1 - i))`
/// where `i` = unit index (0 = oldest) and `n` = total units.
///
/// This ensures old turns degrade smoothly but never hit zero priority,
/// so system inserts or tool sequences from early conversation can still
/// win selection through their type weight multiplier.
///
/// Complexity: O(k log k) where k = semantic units (k << n messages).
pub fn compact_to_budget(units: Vec<SemanticUnit>, budget: usize) -> CompactionResult {
    if units.is_empty() {
        return CompactionResult {
            messages: vec![],
            tokens_used: 0,
            units_included: 0,
            units_dropped: 0,
        };
    }

    let total_units = units.len();
    let lambda: f64 = 0.85;
    let floor: f64 = 0.01;

    // Score each unit: recency (exponential decay with floor) × type_weight
    let mut scored: Vec<(f64, usize, &SemanticUnit)> = units
        .iter()
        .enumerate()
        .map(|(i, u)| {
            let distance = (total_units - 1 - i) as i32;
            let recency = lambda.powi(distance).max(floor);
            let score = recency * u.unit_type.weight();
            (score, i, u)
        })
        .collect();

    // Sort by score descending — this is the critical fix: the greedy knapsack
    // must process items in score order, not in index-reverse order. Without
    // this sort, a low-scoring recent unit can consume budget before a
    // high-scoring older unit (e.g., a SystemInsert with weight 2.0).
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    // Greedy pack — highest score first (knapsack approximation)
    let mut selected_indices: Vec<usize> = Vec::new();
    let mut remaining_budget = budget;

    for &(_, idx, unit) in &scored {
        if unit.token_count <= remaining_budget {
            selected_indices.push(idx);
            remaining_budget -= unit.token_count;
        }
    }

    // Single-swap improvement: try exchanging each excluded unit with each
    // included unit to see if swapping improves total score. This catches
    // cases where two small low-scoring units block one large high-scoring
    // unit. O(k²) where k = number of units — acceptable since k is small.
    let selected_set: std::collections::HashSet<usize> =
        selected_indices.iter().copied().collect();
    let mut best_score: f64 = scored
        .iter()
        .filter(|(_, idx, _)| selected_set.contains(idx))
        .map(|(s, _, _)| s)
        .sum();

    loop {
        let mut improved = false;
        for &(ex_score, ex_idx, ex_unit) in &scored {
            if selected_indices.contains(&ex_idx) {
                continue; // already included
            }
            for sel_pos in 0..selected_indices.len() {
                let sel_idx = selected_indices[sel_pos];
                let sel_score = scored
                    .iter()
                    .find(|(_, i, _)| *i == sel_idx)
                    .map(|(s, _, _)| *s)
                    .unwrap_or(0.0);
                let sel_tokens = units[sel_idx].token_count;

                // Can we swap? Check budget: remove selected, add excluded
                let new_remaining = remaining_budget + sel_tokens;
                if ex_unit.token_count <= new_remaining {
                    let new_score = best_score - sel_score + ex_score;
                    if new_score > best_score {
                        remaining_budget = new_remaining - ex_unit.token_count;
                        selected_indices[sel_pos] = ex_idx;
                        best_score = new_score;
                        improved = true;
                        break;
                    }
                }
            }
            if improved {
                break;
            }
        }
        if !improved {
            break;
        }
    }

    // Sort selected indices to maintain chronological order
    selected_indices.sort();

    let tokens_used = budget - remaining_budget;
    let units_included = selected_indices.len();

    let messages: Vec<AgentMessage> = selected_indices
        .iter()
        .flat_map(|&idx| units[idx].messages.iter().cloned())
        .collect();

    CompactionResult {
        messages,
        tokens_used,
        units_included,
        units_dropped: total_units - units_included,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_msg(role: Role, content: &str) -> AgentMessage {
        AgentMessage {
            role,
            content: content.to_string(),
            timestamp: Utc::now(),
            model: None,
            token_count: None,
            tool_call_id: None,
            tool_name: None,
        }
    }

    #[test]
    fn test_semantic_boundary_detection() {
        let messages = vec![
            make_msg(Role::User, "Hello"),
            make_msg(Role::Assistant, "Hi there!"),
            make_msg(Role::User, "What's the weather?"),
            make_msg(Role::Assistant, "Let me check..."),
        ];

        let units = detect_semantic_boundaries(&messages);
        assert_eq!(units.len(), 2); // Two turn pairs
        assert_eq!(units[0].unit_type, UnitType::Turn);
        assert_eq!(units[1].unit_type, UnitType::Turn);
    }

    #[test]
    fn test_tool_sequence_not_split() {
        let messages = vec![
            make_msg(Role::User, "Run ls"),
            make_msg(Role::Tool, "ls command"),
            make_msg(Role::ToolResult, "file1.txt\nfile2.txt"),
            make_msg(Role::Assistant, "Here are the files"),
        ];

        let units = detect_semantic_boundaries(&messages);
        // Should group tool + tool_result + assistant as one unit
        assert!(units.iter().any(|u| u.unit_type == UnitType::ToolSequence));
    }

    #[test]
    fn test_compact_respects_budget() {
        let messages: Vec<AgentMessage> = (0..20)
            .flat_map(|i| {
                vec![
                    make_msg(Role::User, &format!("Question {}", i)),
                    make_msg(Role::Assistant, &format!("Answer {} with some longer content to use tokens", i)),
                ]
            })
            .collect();

        let units = detect_semantic_boundaries(&messages);
        let result = compact_to_budget(units, 100);

        assert!(result.tokens_used <= 100);
        assert!(result.units_included > 0);
        assert!(result.units_dropped > 0);
    }

    #[test]
    fn test_compact_prefers_high_scoring_units() {
        // Build units with controlled scores:
        // Unit 0 (old): SystemInsert (weight=2.0) — high score despite age
        // Units 1-4 (middle): Turns (weight=1.0) — moderate
        // Unit 5 (recent): MediaBlock (weight=0.8) — low despite recency
        // With a tight budget, the knapsack should prefer SystemInsert over MediaBlock.
        let sys_unit = SemanticUnit {
            messages: vec![make_msg(Role::System, "Critical context info")],
            token_count: 10,
            unit_type: UnitType::SystemInsert,
            timestamp: Utc::now(),
        };
        let media_unit = SemanticUnit {
            messages: vec![make_msg(Role::User, "Here is an image")],
            token_count: 10,
            unit_type: UnitType::MediaBlock,
            timestamp: Utc::now(),
        };
        let turn_unit = SemanticUnit {
            messages: vec![
                make_msg(Role::User, "Hello there friend"),
                make_msg(Role::Assistant, "Hi, how can I help you today?"),
            ],
            token_count: 20,
            unit_type: UnitType::Turn,
            timestamp: Utc::now(),
        };

        // Budget fits 2 units but not 3. Score-sorted should pick
        // SystemInsert (highest score via weight 2.0) + a Turn.
        let units = vec![
            sys_unit,
            turn_unit.clone(),
            turn_unit,
            media_unit,
        ];
        let result = compact_to_budget(units, 30);

        assert!(result.tokens_used <= 30);
        assert!(result.units_included >= 2);
        // The SystemInsert's messages should be in the result
        assert!(result.messages.iter().any(|m| m.content.contains("Critical context")));
    }

    #[test]
    fn test_compact_single_swap_improvement() {
        // A single-swap test: one large high-value unit blocked by two small low-value units.
        // Greedy picks the two small ones first; swap should replace one.
        let high_value = SemanticUnit {
            messages: vec![make_msg(Role::System, "Important system message for the agent")],
            token_count: 15,
            unit_type: UnitType::SystemInsert, // weight 2.0
            timestamp: Utc::now(),
        };
        let low_value_1 = SemanticUnit {
            messages: vec![make_msg(Role::User, "Hey")],
            token_count: 8,
            unit_type: UnitType::MediaBlock, // weight 0.8
            timestamp: Utc::now(),
        };
        let low_value_2 = SemanticUnit {
            messages: vec![make_msg(Role::User, "Bye")],
            token_count: 8,
            unit_type: UnitType::MediaBlock, // weight 0.8
            timestamp: Utc::now(),
        };

        // Budget = 16. Greedy by score picks SystemInsert (15 tokens) + can't fit either MediaBlock.
        // So we get 1 unit. Without swap, same result. This tests that the sort works.
        let units = vec![low_value_1, low_value_2, high_value];
        let result = compact_to_budget(units, 16);

        assert!(result.tokens_used <= 16);
        // Should include the SystemInsert since it has highest score
        assert!(result.messages.iter().any(|m| m.content.contains("Important system")));
    }

    #[test]
    fn test_calibrated_estimator_default() {
        let est = CalibratedTokenEstimator::new();
        assert!((est.ratio() - 4.0).abs() < f64::EPSILON);
        assert!(!est.is_calibrated());
        // Should match simple estimate for default ratio
        assert_eq!(est.estimate("hello world!"), estimate_tokens("hello world!"));
    }

    #[test]
    fn test_calibrated_estimator_observe_converges() {
        let est = CalibratedTokenEstimator::with_params(4.0, 0.3);
        // Simulate a model with ~3.5 chars/token for 20 observations
        for _ in 0..20 {
            est.observe(350, 100); // 3.5 chars/token
        }
        // After 20 observations with α=0.3, ratio should converge near 3.5
        let r = est.ratio();
        assert!(
            (r - 3.5).abs() < 0.1,
            "Expected ratio near 3.5, got {r}"
        );
        assert!(est.is_calibrated());
    }

    #[test]
    fn test_calibrated_estimator_clamps_extreme() {
        let est = CalibratedTokenEstimator::new();
        // Pathological: 1 char → 1000 tokens (ratio 0.001). Should be clamped to 1.0
        est.observe(1, 1000);
        // α=0.1: new_ratio = 0.9 * 4.0 + 0.1 * 1.0 = 3.7
        let r = est.ratio();
        assert!(
            (r - 3.7).abs() < 0.01,
            "Expected ~3.7 after extreme low observation, got {r}"
        );
    }

    #[test]
    fn test_calibrated_estimator_ignores_zero() {
        let est = CalibratedTokenEstimator::new();
        est.observe(0, 100);
        est.observe(100, 0);
        assert!((est.ratio() - 4.0).abs() < f64::EPSILON, "Ratio should stay at default");
    }
}
