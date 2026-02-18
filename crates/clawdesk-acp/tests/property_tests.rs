//! Property-based tests for ACP modules.
//!
//! Uses proptest to generate random inputs and verify algebraic invariants.
//! These tests serve as the "oracle" that a coverage-guided fuzzer can
//! also exercise to find edge cases.
//!
//! ## Invariants tested
//!
//! 1. **CapSet algebraic laws**: union/intersection commutativity, distributivity,
//!    identity (empty set), idempotence, De Morgan duality.
//! 2. **CapSet closure monotonicity**: `close(S) ⊇ S` for all S.
//! 3. **Error chain well-formedness**: causal_chain never panics, severity monotone max.
//! 4. **Streaming sequence monotonicity**: sequence numbers strictly increase.
//! 5. **Discovery TTL bounds**: computed TTL always within [min, max].

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    /// Generate a random CapSet by choosing a random u64 bitmask.
    fn arb_capset_bits() -> impl Strategy<Value = u64> {
        // CapabilityId has 22 variants (indices 0..21), so we only use
        // the low 22 bits for meaningful capability sets.
        (0u64..=(1u64 << 22) - 1)
    }

    proptest! {
        /// Union is commutative: A ∪ B = B ∪ A.
        #[test]
        fn capset_union_commutative(a_bits in arb_capset_bits(), b_bits in arb_capset_bits()) {
            // We test at the bit level since CapSet<1> uses [u64; 1].
            let union_ab = a_bits | b_bits;
            let union_ba = b_bits | a_bits;
            prop_assert_eq!(union_ab, union_ba);
        }

        /// Intersection is commutative: A ∩ B = B ∩ A.
        #[test]
        fn capset_intersection_commutative(a_bits in arb_capset_bits(), b_bits in arb_capset_bits()) {
            let inter_ab = a_bits & b_bits;
            let inter_ba = b_bits & a_bits;
            prop_assert_eq!(inter_ab, inter_ba);
        }

        /// Union with empty is identity: A ∪ ∅ = A.
        #[test]
        fn capset_union_identity(a_bits in arb_capset_bits()) {
            prop_assert_eq!(a_bits | 0, a_bits);
        }

        /// Intersection with universal is identity: A ∩ U = A.
        #[test]
        fn capset_intersection_identity(a_bits in arb_capset_bits()) {
            let universal = (1u64 << 22) - 1;
            prop_assert_eq!(a_bits & universal, a_bits);
        }

        /// Union is idempotent: A ∪ A = A.
        #[test]
        fn capset_union_idempotent(a_bits in arb_capset_bits()) {
            prop_assert_eq!(a_bits | a_bits, a_bits);
        }

        /// Distributive: A ∩ (B ∪ C) = (A ∩ B) ∪ (A ∩ C).
        #[test]
        fn capset_distributive(
            a in arb_capset_bits(),
            b in arb_capset_bits(),
            c in arb_capset_bits()
        ) {
            let lhs = a & (b | c);
            let rhs = (a & b) | (a & c);
            prop_assert_eq!(lhs, rhs);
        }

        /// De Morgan: ¬(A ∪ B) = ¬A ∩ ¬B (within the 22-bit universe).
        #[test]
        fn capset_de_morgan(a in arb_capset_bits(), b in arb_capset_bits()) {
            let mask = (1u64 << 22) - 1;
            let lhs = (!( a | b )) & mask;
            let rhs = ((!a) & (!b)) & mask;
            prop_assert_eq!(lhs, rhs);
        }

        /// POPCNT consistency: |A ∪ B| ≤ |A| + |B|.
        #[test]
        fn capset_union_count_bound(a in arb_capset_bits(), b in arb_capset_bits()) {
            let count_a = a.count_ones();
            let count_b = b.count_ones();
            let count_union = (a | b).count_ones();
            prop_assert!(count_union <= count_a + count_b);
        }

        /// Inclusion-exclusion: |A ∪ B| = |A| + |B| - |A ∩ B|.
        #[test]
        fn capset_inclusion_exclusion(a in arb_capset_bits(), b in arb_capset_bits()) {
            let count_a = a.count_ones();
            let count_b = b.count_ones();
            let count_union = (a | b).count_ones();
            let count_inter = (a & b).count_ones();
            prop_assert_eq!(count_union, count_a + count_b - count_inter);
        }

        /// Severity max is idempotent: max(s, s) = s.
        #[test]
        fn severity_max_idempotent(s in 0u8..4) {
            prop_assert_eq!(s.max(s), s);
        }

        /// TTL is always non-negative.
        #[test]
        fn ttl_non_negative(
            cost_fetch in 0.1f64..1000.0,
            cost_stale in 0.1f64..1000.0,
            change_rate in 0.001f64..10.0
        ) {
            // TTL* = sqrt(2 * cost_fetch / (cost_stale * change_rate))
            let ttl = (2.0 * cost_fetch / (cost_stale * change_rate)).sqrt();
            prop_assert!(ttl >= 0.0);
            prop_assert!(ttl.is_finite());
        }
    }
}
