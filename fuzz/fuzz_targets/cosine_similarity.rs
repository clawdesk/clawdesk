//! Fuzz target: SIMD cosine similarity.
//!
//! Ensures that cosine_similarity never panics or returns NaN
//! for arbitrary f32 inputs.

#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Interpret bytes as pairs of f32 vectors
    if data.len() < 8 || data.len() % 4 != 0 {
        return;
    }

    let floats: Vec<f32> = data
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect();

    if floats.len() < 2 {
        return;
    }

    let mid = floats.len() / 2;
    let a = &floats[..mid];
    let b = &floats[mid..mid * 2]; // ensure equal length

    let sim = clawdesk_simd::cosine_similarity(a, b);

    // Result must be finite or 0.0 (for zero vectors)
    assert!(
        sim.is_finite() || sim == 0.0,
        "got non-finite result: {sim}"
    );

    // If both vectors are finite and non-zero, result should be in [-1, 1]
    let all_finite = a.iter().chain(b.iter()).all(|f| f.is_finite());
    if all_finite && sim.is_finite() {
        assert!(
            sim >= -1.0 - 1e-4 && sim <= 1.0 + 1e-4,
            "cosine similarity out of range: {sim}"
        );
    }
});
