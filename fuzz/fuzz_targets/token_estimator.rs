//! Fuzz target: Token estimation on arbitrary Unicode.
//!
//! The token estimator must never panic on any input.

#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let estimate = clawdesk_types::estimate_tokens(s);
        // Sanity: estimate must be non-negative (always true for usize)
        assert!(estimate > 0 || s.is_empty() || s.chars().all(|c| c.is_whitespace()));
    }
});
