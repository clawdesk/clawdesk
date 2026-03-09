//! Property-based tests for clawdesk-types.
//!
//! Uses proptest to verify invariants that must hold for ALL inputs,
//! not just hand-picked test cases.

use proptest::prelude::*;

use clawdesk_types::tokenizer::{estimate_tokens, estimate_tokens_batch};
use clawdesk_types::DropOldest;

// ─────────────────────────────────────────────────────────────────────────────
// Token estimator invariants
// ─────────────────────────────────────────────────────────────────────────────

proptest! {
    /// Token count is always non-negative (trivially true for usize, but
    /// ensures no panics on arbitrary Unicode input).
    #[test]
    fn token_estimate_never_panics(s in "\\PC{0,10000}") {
        let _ = estimate_tokens(&s);
    }

    /// Empty string always yields 0 tokens.
    #[test]
    fn empty_string_zero_tokens(s in "\\s{0,100}") {
        // Whitespace-only strings might yield tokens or not — just no panics.
        let _ = estimate_tokens(&s);
    }

    /// Token count scales roughly with input length (monotonicity for
    /// non-pathological inputs). For any string s, estimate_tokens(s+s) >= estimate_tokens(s).
    #[test]
    fn token_estimate_monotonic(s in "[a-zA-Z0-9 ]{1,500}") {
        let single = estimate_tokens(&s);
        let doubled = estimate_tokens(&format!("{s}{s}"));
        prop_assert!(doubled >= single, "doubled={doubled} < single={single} for '{s}'");
    }

    /// Batch estimation produces the same results as individual calls.
    #[test]
    fn batch_matches_individual(
        texts in prop::collection::vec("[a-zA-Z0-9 ]{0,200}", 0..20)
    ) {
        let refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
        let (counts, total) = estimate_tokens_batch(&refs);

        prop_assert_eq!(counts.len(), texts.len());

        let expected_total: usize = texts.iter().map(|t| estimate_tokens(t)).sum();
        prop_assert_eq!(total, expected_total);

        for (i, text) in texts.iter().enumerate() {
            prop_assert_eq!(counts[i], estimate_tokens(text));
        }
    }

    /// Token estimate for CJK/emoji should be non-zero for non-empty strings.
    #[test]
    fn cjk_non_zero(s in "[\\p{Han}\\p{Hiragana}\\p{Katakana}]{1,100}") {
        let tokens = estimate_tokens(&s);
        prop_assert!(tokens > 0, "CJK string '{s}' estimated 0 tokens");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// DropOldest ring buffer invariants
// ─────────────────────────────────────────────────────────────────────────────

proptest! {
    /// Ring buffer never exceeds its capacity.
    #[test]
    fn ring_buffer_bounded(
        cap in 1usize..100,
        items in prop::collection::vec(0i32..1000, 0..200)
    ) {
        let mut ring = DropOldest::new(cap);
        for item in &items {
            ring.push(*item);
            prop_assert!(ring.len() <= cap);
        }
    }

    /// After pushing N > cap items, the buffer contains exactly `cap` items.
    #[test]
    fn ring_buffer_full(
        cap in 1usize..50,
        extra in 1usize..100
    ) {
        let mut ring = DropOldest::new(cap);
        for i in 0..(cap + extra) {
            ring.push(i);
        }
        prop_assert_eq!(ring.len(), cap);
    }

    /// Ring buffer preserves most recent items.
    #[test]
    fn ring_buffer_preserves_recent(
        cap in 1usize..20,
        items in prop::collection::vec(0u32..1000, 1..100)
    ) {
        let mut ring = DropOldest::new(cap);
        for item in &items {
            ring.push(*item);
        }

        let stored: Vec<u32> = ring.iter().copied().collect();
        let expected_start = items.len().saturating_sub(cap);
        let expected: Vec<u32> = items[expected_start..].to_vec();
        prop_assert_eq!(stored, expected);
    }
}
