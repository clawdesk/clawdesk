//! Persona Algebra — formal composition of agent personality vectors.
//!
//! Maps Agent Traits into a numeric vector space where composition,
//! coherence, and distance are well-defined operations.
//!
//! ## Algebra
//!
//! Each trait contributes a "personality vector" across dimensions
//! (formality, verbosity, creativity, rigor, empathy, technicality).
//! Composing traits = weighted vector addition with normalization.
//!
//! Coherence = 1 − max-deviation-from-mean across dimensions.
//! Distance = cosine distance between two composed personas.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Number of personality dimensions.
pub const NUM_DIMENSIONS: usize = 6;

/// Named personality dimensions for trait vector space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PersonaDimension {
    /// Low = casual, High = formal
    Formality,
    /// Low = terse, High = elaborate
    Verbosity,
    /// Low = analytical, High = generative
    Creativity,
    /// Low = loose, High = strict evidence-based
    Rigor,
    /// Low = neutral, High = warm/supportive
    Empathy,
    /// Low = general audience, High = specialist
    Technicality,
}

impl PersonaDimension {
    pub const ALL: [PersonaDimension; NUM_DIMENSIONS] = [
        Self::Formality,
        Self::Verbosity,
        Self::Creativity,
        Self::Rigor,
        Self::Empathy,
        Self::Technicality,
    ];

    pub fn index(self) -> usize {
        match self {
            Self::Formality => 0,
            Self::Verbosity => 1,
            Self::Creativity => 2,
            Self::Rigor => 3,
            Self::Empathy => 4,
            Self::Technicality => 5,
        }
    }
}

// ─── Persona Vector ──────────────────────────────────────────────────────────

/// A 6-dimensional persona vector with values ∈ [0, 1].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonaVector {
    pub dims: [f64; NUM_DIMENSIONS],
}

impl PersonaVector {
    /// Neutral persona (all 0.5).
    pub fn neutral() -> Self {
        Self {
            dims: [0.5; NUM_DIMENSIONS],
        }
    }

    /// Create from dimension-value pairs.
    pub fn from_pairs(pairs: &[(PersonaDimension, f64)]) -> Self {
        let mut v = Self::neutral();
        for (dim, val) in pairs {
            v.dims[dim.index()] = val.clamp(0.0, 1.0);
        }
        v
    }

    /// Weighted addition: self + weight * other, clamped to [0, 1].
    pub fn add_weighted(&self, other: &PersonaVector, weight: f64) -> Self {
        let mut result = [0.0; NUM_DIMENSIONS];
        for i in 0..NUM_DIMENSIONS {
            result[i] = (self.dims[i] + weight * other.dims[i]).clamp(0.0, 1.0);
        }
        Self { dims: result }
    }

    /// Normalize to unit length (L2 norm).
    ///
    /// Returns `None` if the vector has near-zero norm (degenerate input),
    /// rather than silently substituting the neutral vector.
    pub fn try_normalize(&self) -> Option<Self> {
        let norm: f64 = self.dims.iter().map(|x| x * x).sum::<f64>().sqrt();
        if norm < 1e-10 {
            return None;
        }
        let mut dims = [0.0; NUM_DIMENSIONS];
        for i in 0..NUM_DIMENSIONS {
            dims[i] = self.dims[i] / norm;
        }
        Some(Self { dims })
    }

    /// Normalize to unit length (L2 norm).
    ///
    /// Falls back to neutral vector on near-zero norm for backward compatibility.
    pub fn normalize(&self) -> Self {
        self.try_normalize().unwrap_or_else(Self::neutral)
    }

    /// Cosine similarity with another vector ∈ [-1, 1].
    pub fn cosine_similarity(&self, other: &PersonaVector) -> f64 {
        let dot: f64 = self
            .dims
            .iter()
            .zip(other.dims.iter())
            .map(|(a, b)| a * b)
            .sum();
        let norm_a: f64 = self.dims.iter().map(|x| x * x).sum::<f64>().sqrt();
        let norm_b: f64 = other.dims.iter().map(|x| x * x).sum::<f64>().sqrt();
        if norm_a < 1e-10 || norm_b < 1e-10 {
            return 0.0;
        }
        dot / (norm_a * norm_b)
    }

    /// Euclidean distance.
    pub fn distance(&self, other: &PersonaVector) -> f64 {
        self.dims
            .iter()
            .zip(other.dims.iter())
            .map(|(a, b)| (a - b) * (a - b))
            .sum::<f64>()
            .sqrt()
    }

    /// Get dimension value.
    pub fn get(&self, dim: PersonaDimension) -> f64 {
        self.dims[dim.index()]
    }

    /// Set dimension value.
    pub fn set(&mut self, dim: PersonaDimension, val: f64) {
        self.dims[dim.index()] = val.clamp(0.0, 1.0);
    }
}

impl Default for PersonaVector {
    fn default() -> Self {
        Self::neutral()
    }
}

// ─── Trait to Vector Mapping ─────────────────────────────────────────────────

/// Mapping from trait IDs to persona vectors.
pub struct TraitVectorMap {
    vectors: HashMap<String, PersonaVector>,
}

impl TraitVectorMap {
    pub fn new() -> Self {
        Self {
            vectors: HashMap::new(),
        }
    }

    pub fn register(&mut self, trait_id: &str, vector: PersonaVector) {
        self.vectors.insert(trait_id.into(), vector);
    }

    pub fn get(&self, trait_id: &str) -> Option<&PersonaVector> {
        self.vectors.get(trait_id)
    }

    /// Compose multiple trait vectors into a single persona.
    ///
    /// Each trait contributes equally by default. Use `compose_weighted`
    /// for custom weights.
    pub fn compose(&self, trait_ids: &[String]) -> PersonaVector {
        let weights: Vec<f64> = vec![1.0; trait_ids.len()];
        self.compose_weighted(trait_ids, &weights)
    }

    /// Compose with explicit per-trait weights.
    ///
    /// Result = Σ(wᵢ × vᵢ) / Σwᵢ, clamped to [0, 1].
    pub fn compose_weighted(&self, trait_ids: &[String], weights: &[f64]) -> PersonaVector {
        let mut result = [0.0; NUM_DIMENSIONS];
        let mut total_weight = 0.0;

        for (id, w) in trait_ids.iter().zip(weights.iter()) {
            if let Some(v) = self.vectors.get(id) {
                for i in 0..NUM_DIMENSIONS {
                    result[i] += w * v.dims[i];
                }
                total_weight += w;
            }
        }

        if total_weight < 1e-10 {
            return PersonaVector::neutral();
        }

        for dim in result.iter_mut() {
            *dim = (*dim / total_weight).clamp(0.0, 1.0);
        }

        PersonaVector { dims: result }
    }

    /// Coherence of a composed persona: 1 − max_{dim} |v[dim] − mean(v)|.
    /// Returns ∈ [0, 1] where 1 = perfectly balanced.
    pub fn coherence(&self, composed: &PersonaVector) -> f64 {
        let mean: f64 = composed.dims.iter().sum::<f64>() / NUM_DIMENSIONS as f64;
        let max_dev = composed
            .dims
            .iter()
            .map(|d| (d - mean).abs())
            .fold(0.0_f64, f64::max);
        1.0 - max_dev
    }
}

impl Default for TraitVectorMap {
    fn default() -> Self {
        Self::new()
    }
}

/// Build the standard trait-to-vector mapping for built-in traits.
pub fn builtin_trait_vectors() -> TraitVectorMap {
    let mut m = TraitVectorMap::new();

    // Persona traits
    m.register(
        "concise",
        PersonaVector::from_pairs(&[
            (PersonaDimension::Verbosity, 0.1),
            (PersonaDimension::Formality, 0.6),
        ]),
    );
    m.register(
        "verbose",
        PersonaVector::from_pairs(&[
            (PersonaDimension::Verbosity, 0.95),
            (PersonaDimension::Formality, 0.6),
        ]),
    );
    m.register(
        "friendly",
        PersonaVector::from_pairs(&[
            (PersonaDimension::Empathy, 0.9),
            (PersonaDimension::Formality, 0.2),
        ]),
    );
    m.register(
        "formal",
        PersonaVector::from_pairs(&[
            (PersonaDimension::Formality, 0.95),
            (PersonaDimension::Empathy, 0.3),
        ]),
    );
    m.register(
        "academic",
        PersonaVector::from_pairs(&[
            (PersonaDimension::Formality, 0.9),
            (PersonaDimension::Rigor, 0.9),
            (PersonaDimension::Verbosity, 0.8),
            (PersonaDimension::Technicality, 0.8),
        ]),
    );

    // Methodology traits
    m.register(
        "first-principles",
        PersonaVector::from_pairs(&[
            (PersonaDimension::Rigor, 0.85),
            (PersonaDimension::Creativity, 0.6),
            (PersonaDimension::Technicality, 0.7),
        ]),
    );
    m.register(
        "evidence-based",
        PersonaVector::from_pairs(&[
            (PersonaDimension::Rigor, 0.95),
            (PersonaDimension::Creativity, 0.2),
        ]),
    );
    m.register(
        "creative-brainstorm",
        PersonaVector::from_pairs(&[
            (PersonaDimension::Creativity, 0.95),
            (PersonaDimension::Rigor, 0.2),
        ]),
    );
    m.register(
        "systematic",
        PersonaVector::from_pairs(&[
            (PersonaDimension::Rigor, 0.8),
            (PersonaDimension::Formality, 0.7),
        ]),
    );

    // Domain traits
    m.register(
        "engineering",
        PersonaVector::from_pairs(&[
            (PersonaDimension::Technicality, 0.9),
            (PersonaDimension::Rigor, 0.7),
        ]),
    );
    m.register(
        "legal",
        PersonaVector::from_pairs(&[
            (PersonaDimension::Formality, 0.95),
            (PersonaDimension::Rigor, 0.9),
            (PersonaDimension::Technicality, 0.8),
        ]),
    );
    m.register(
        "medical",
        PersonaVector::from_pairs(&[
            (PersonaDimension::Rigor, 0.95),
            (PersonaDimension::Empathy, 0.7),
            (PersonaDimension::Technicality, 0.85),
        ]),
    );
    m.register(
        "financial",
        PersonaVector::from_pairs(&[
            (PersonaDimension::Formality, 0.8),
            (PersonaDimension::Rigor, 0.85),
            (PersonaDimension::Technicality, 0.75),
        ]),
    );
    m.register(
        "data-science",
        PersonaVector::from_pairs(&[
            (PersonaDimension::Technicality, 0.9),
            (PersonaDimension::Rigor, 0.85),
        ]),
    );
    m.register(
        "education",
        PersonaVector::from_pairs(&[
            (PersonaDimension::Empathy, 0.85),
            (PersonaDimension::Verbosity, 0.7),
            (PersonaDimension::Formality, 0.4),
        ]),
    );

    // Output traits
    m.register(
        "structured-report",
        PersonaVector::from_pairs(&[
            (PersonaDimension::Formality, 0.7),
            (PersonaDimension::Verbosity, 0.7),
        ]),
    );
    m.register(
        "conversational",
        PersonaVector::from_pairs(&[
            (PersonaDimension::Formality, 0.2),
            (PersonaDimension::Empathy, 0.7),
            (PersonaDimension::Verbosity, 0.5),
        ]),
    );
    m.register(
        "code-first",
        PersonaVector::from_pairs(&[
            (PersonaDimension::Technicality, 0.9),
            (PersonaDimension::Verbosity, 0.2),
        ]),
    );

    m
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_neutral_persona() {
        let v = PersonaVector::neutral();
        for d in &v.dims {
            assert!((*d - 0.5).abs() < 1e-10);
        }
    }

    #[test]
    fn test_cosine_similarity_identical() {
        let v = PersonaVector::from_pairs(&[
            (PersonaDimension::Formality, 0.8),
            (PersonaDimension::Rigor, 0.9),
        ]);
        let sim = v.cosine_similarity(&v);
        assert!((sim - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_compose_weighted() {
        let m = builtin_trait_vectors();
        let traits = vec!["concise".into(), "engineering".into()];
        let weights = vec![1.0, 2.0];
        let composed = m.compose_weighted(&traits, &weights);
        // Engineering is double-weighted, so technicality should be high
        let tech = composed.get(PersonaDimension::Technicality);
        assert!(tech > 0.7, "technicality = {} (expected > 0.7)", tech);
    }

    #[test]
    fn test_coherence_balanced() {
        let m = TraitVectorMap::new();
        let balanced = PersonaVector {
            dims: [0.5; NUM_DIMENSIONS],
        };
        let c = m.coherence(&balanced);
        assert!((c - 1.0).abs() < 1e-6, "balanced coherence = {}", c);
    }

    #[test]
    fn test_coherence_unbalanced() {
        let m = TraitVectorMap::new();
        let skewed = PersonaVector {
            dims: [1.0, 0.0, 0.5, 0.5, 0.5, 0.5],
        };
        let c = m.coherence(&skewed);
        assert!(c < 0.7, "skewed coherence = {} (expected < 0.7)", c);
    }

    #[test]
    fn test_distance() {
        let a = PersonaVector::from_pairs(&[(PersonaDimension::Formality, 0.0)]);
        let b = PersonaVector::from_pairs(&[(PersonaDimension::Formality, 1.0)]);
        let d = a.distance(&b);
        assert!(d > 0.9, "distance = {} (expected > 0.9)", d);
    }

    #[test]
    fn test_builtin_vectors() {
        let m = builtin_trait_vectors();
        assert!(m.get("concise").is_some());
        assert!(m.get("engineering").is_some());
        assert!(m.get("nonexistent").is_none());
    }
}
