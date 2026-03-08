//! Persona Algebra Extension — interference matrix, Pareto composition,
//! context-switch stack, and hierarchical override.
//!
//! Extends the base 6D persona algebra with:
//!
//! 1. **Interference Matrix** I ∈ R^{6×6} — models coupled dimension pairs
//!    where increasing one naturally suppresses another:
//!    - (formality, creativity): -0.7
//!    - (rigor, empathy): -0.4
//!    - (technicality, verbosity): -0.3
//!
//! 2. **Pareto Composition** — multi-objective optimization when composing
//!    multiple personas that have conflicting preferences.
//!
//! 3. **Context-Switch Stack** — save/restore persona states for nested
//!    agent delegation.
//!
//! 4. **Hierarchical Override** — parent persona can clamp child dimensions
//!    to enforce organizational policies.

use crate::persona_algebra::{PersonaVector, PersonaDimension, NUM_DIMENSIONS};
use serde::{Deserialize, Serialize};

// ───────────────────────────────────────────────────────────────
// Interference Matrix
// ───────────────────────────────────────────────────────────────

/// 6×6 interference matrix modeling coupling between persona dimensions.
///
/// Entry I[i][j] ∈ [-1, 1] indicates how an increase in dimension i
/// affects dimension j:
/// - Negative: suppression (e.g., high formality suppresses creativity)
/// - Positive: reinforcement
/// - Zero: independent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterferenceMatrix {
    /// 6×6 matrix stored row-major.
    pub matrix: [[f64; NUM_DIMENSIONS]; NUM_DIMENSIONS],
}

impl InterferenceMatrix {
    /// Identity matrix (no interference).
    pub fn identity() -> Self {
        let mut m = [[0.0; NUM_DIMENSIONS]; NUM_DIMENSIONS];
        for i in 0..NUM_DIMENSIONS {
            m[i][i] = 1.0;
        }
        Self { matrix: m }
    }

    /// Default interference matrix with known coupling pairs.
    pub fn default_coupling() -> Self {
        let mut m = Self::identity();

        // (formality, creativity) = -0.7 — formal writing suppresses creative expression.
        m.set_coupling(PersonaDimension::Formality, PersonaDimension::Creativity, -0.7);

        // (rigor, empathy) = -0.4 — strict evidence-based style reduces warmth.
        m.set_coupling(PersonaDimension::Rigor, PersonaDimension::Empathy, -0.4);

        // (technicality, verbosity) = -0.3 — specialist language tends to be terser.
        m.set_coupling(PersonaDimension::Technicality, PersonaDimension::Verbosity, -0.3);

        // (empathy, formality) = -0.2 — warmth reduces formality.
        m.set_coupling(PersonaDimension::Empathy, PersonaDimension::Formality, -0.2);

        m
    }

    /// Set a symmetric coupling between two dimensions.
    pub fn set_coupling(&mut self, a: PersonaDimension, b: PersonaDimension, value: f64) {
        let ai = a.index();
        let bi = b.index();
        self.matrix[ai][bi] = value;
        self.matrix[bi][ai] = value;
    }

    /// Get coupling between two dimensions.
    pub fn coupling(&self, a: PersonaDimension, b: PersonaDimension) -> f64 {
        self.matrix[a.index()][b.index()]
    }

    /// Apply interference to a persona vector.
    ///
    /// For each dimension i, the effective value is:
    /// ```text
    /// v'[i] = clamp(v[i] + Σ_{j≠i} I[i][j] × (v[j] - 0.5) × strength, 0, 1)
    /// ```
    ///
    /// The `(v[j] - 0.5)` term means interference only activates when a
    /// dimension deviates from neutral. `strength` ∈ [0, 1] controls
    /// how strongly coupling is applied.
    pub fn apply(&self, persona: &PersonaVector, strength: f64) -> PersonaVector {
        let mut result = persona.dims;

        for i in 0..NUM_DIMENSIONS {
            let mut adjustment = 0.0;
            for j in 0..NUM_DIMENSIONS {
                if i != j {
                    adjustment += self.matrix[i][j] * (persona.dims[j] - 0.5) * strength;
                }
            }
            result[i] = (result[i] + adjustment).clamp(0.0, 1.0);
        }

        PersonaVector { dims: result }
    }

    /// Detect dimension pairs that conflict in a given persona vector.
    ///
    /// Returns pairs where both dimensions are far from neutral AND the
    /// interference coefficient is negative (they fight each other).
    pub fn detect_conflicts(
        &self,
        persona: &PersonaVector,
        threshold: f64,
    ) -> Vec<(PersonaDimension, PersonaDimension, f64)> {
        let mut conflicts = Vec::new();

        for i in 0..NUM_DIMENSIONS {
            for j in (i + 1)..NUM_DIMENSIONS {
                let coupling = self.matrix[i][j];
                if coupling < -threshold {
                    // Both dimensions are active (far from 0.5).
                    let activity_i = (persona.dims[i] - 0.5).abs();
                    let activity_j = (persona.dims[j] - 0.5).abs();
                    if activity_i > 0.2 && activity_j > 0.2 {
                        conflicts.push((
                            PersonaDimension::ALL[i],
                            PersonaDimension::ALL[j],
                            coupling,
                        ));
                    }
                }
            }
        }

        conflicts
    }
}

impl Default for InterferenceMatrix {
    fn default() -> Self {
        Self::default_coupling()
    }
}

// ───────────────────────────────────────────────────────────────
// Pareto Composition
// ───────────────────────────────────────────────────────────────

/// Pareto-optimal composition of multiple persona vectors.
///
/// When personas have conflicting preferences, instead of simple averaging,
/// finds a Pareto-optimal compromise that minimizes the maximum regret
/// across all input personas.
pub fn pareto_compose(
    personas: &[PersonaVector],
    interference: &InterferenceMatrix,
) -> PersonaVector {
    if personas.is_empty() {
        return PersonaVector::neutral();
    }
    if personas.len() == 1 {
        return interference.apply(&personas[0], 1.0);
    }

    // Mini-max regret: for each dimension, pick the value that minimizes
    // the maximum distance to any input persona's preference.
    let mut result = [0.0; NUM_DIMENSIONS];

    for i in 0..NUM_DIMENSIONS {
        let values: Vec<f64> = personas.iter().map(|p| p.dims[i]).collect();
        let min = values.iter().cloned().fold(f64::MAX, f64::min);
        let max = values.iter().cloned().fold(f64::MIN, f64::max);

        // Mini-max point is the midpoint.
        result[i] = ((min + max) / 2.0).clamp(0.0, 1.0);
    }

    let composed = PersonaVector { dims: result };
    interference.apply(&composed, 0.5) // Apply interference at half strength.
}

// ───────────────────────────────────────────────────────────────
// Context-Switch Stack
// ───────────────────────────────────────────────────────────────

/// Stack of persona states for nested context switching.
///
/// When an agent delegates to a sub-agent, the parent's persona is pushed
/// onto the stack. When the sub-agent completes, the parent's persona is
/// restored. This prevents persona drift during deep delegation chains.
#[derive(Debug, Clone)]
pub struct PersonaStack {
    stack: Vec<PersonaVector>,
    max_depth: usize,
}

impl PersonaStack {
    pub fn new(max_depth: usize) -> Self {
        Self {
            stack: Vec::with_capacity(max_depth),
            max_depth,
        }
    }

    /// Push the current persona onto the stack before delegating.
    pub fn push(&mut self, persona: PersonaVector) -> Result<(), &'static str> {
        if self.stack.len() >= self.max_depth {
            return Err("persona stack overflow: delegation too deep");
        }
        self.stack.push(persona);
        Ok(())
    }

    /// Pop and restore the previous persona after delegation completes.
    pub fn pop(&mut self) -> Option<PersonaVector> {
        self.stack.pop()
    }

    /// Peek at the current (top) persona without removing.
    pub fn current(&self) -> Option<&PersonaVector> {
        self.stack.last()
    }

    /// Current stack depth.
    pub fn depth(&self) -> usize {
        self.stack.len()
    }

    /// Whether the stack is empty.
    pub fn is_empty(&self) -> bool {
        self.stack.is_empty()
    }
}

// ───────────────────────────────────────────────────────────────
// Hierarchical Override
// ───────────────────────────────────────────────────────────────

/// Dimension constraints that a parent can impose on child personas.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DimensionConstraint {
    pub dimension: PersonaDimension,
    /// Minimum value (None = no lower bound).
    pub min: Option<f64>,
    /// Maximum value (None = no upper bound).
    pub max: Option<f64>,
}

/// Hierarchical override: parent-imposed constraints on child persona dimensions.
///
/// For example, an organization might require:
/// - Formality ≥ 0.7 (always professional)
/// - Creativity ≤ 0.5 (don't get too creative in production)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonaOverride {
    pub constraints: Vec<DimensionConstraint>,
    /// Description of this policy (for diagnostics).
    pub policy_name: String,
}

impl PersonaOverride {
    pub fn new(policy_name: impl Into<String>) -> Self {
        Self {
            constraints: Vec::new(),
            policy_name: policy_name.into(),
        }
    }

    /// Add a constraint.
    pub fn constrain(mut self, dim: PersonaDimension, min: Option<f64>, max: Option<f64>) -> Self {
        self.constraints.push(DimensionConstraint {
            dimension: dim,
            min,
            max,
        });
        self
    }

    /// Apply the override to a persona vector, clamping dimensions.
    pub fn apply(&self, persona: &PersonaVector) -> PersonaVector {
        let mut result = persona.dims;

        for c in &self.constraints {
            let i = c.dimension.index();
            if let Some(min) = c.min {
                result[i] = result[i].max(min);
            }
            if let Some(max) = c.max {
                result[i] = result[i].min(max);
            }
        }

        PersonaVector { dims: result }
    }

    /// Check which dimensions of a persona violate the constraints.
    pub fn violations(&self, persona: &PersonaVector) -> Vec<(PersonaDimension, f64, String)> {
        let mut violations = Vec::new();

        for c in &self.constraints {
            let i = c.dimension.index();
            let val = persona.dims[i];

            if let Some(min) = c.min {
                if val < min {
                    violations.push((
                        c.dimension,
                        val,
                        format!("{:?} = {:.2} < min {:.2}", c.dimension, val, min),
                    ));
                }
            }
            if let Some(max) = c.max {
                if val > max {
                    violations.push((
                        c.dimension,
                        val,
                        format!("{:?} = {:.2} > max {:.2}", c.dimension, val, max),
                    ));
                }
            }
        }

        violations
    }
}

// ───────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_interference_formality_creativity() {
        let im = InterferenceMatrix::default_coupling();

        // High formality (0.9) should suppress creativity.
        let persona = PersonaVector::from_pairs(&[
            (PersonaDimension::Formality, 0.9),
            (PersonaDimension::Creativity, 0.8),
        ]);

        let adjusted = im.apply(&persona, 1.0);
        assert!(
            adjusted.get(PersonaDimension::Creativity) < persona.get(PersonaDimension::Creativity),
            "high formality should suppress creativity"
        );
    }

    #[test]
    fn test_conflict_detection() {
        let im = InterferenceMatrix::default_coupling();

        let persona = PersonaVector::from_pairs(&[
            (PersonaDimension::Formality, 0.9),
            (PersonaDimension::Creativity, 0.9),
        ]);

        let conflicts = im.detect_conflicts(&persona, 0.3);
        assert!(!conflicts.is_empty(), "should detect formality-creativity conflict");
    }

    #[test]
    fn test_pareto_composition() {
        let im = InterferenceMatrix::default_coupling();

        let formal = PersonaVector::from_pairs(&[
            (PersonaDimension::Formality, 0.9),
            (PersonaDimension::Creativity, 0.2),
        ]);
        let creative = PersonaVector::from_pairs(&[
            (PersonaDimension::Formality, 0.2),
            (PersonaDimension::Creativity, 0.9),
        ]);

        let composed = pareto_compose(&[formal, creative], &im);
        // Should be a compromise between 0.2 and 0.9.
        let formality = composed.get(PersonaDimension::Formality);
        assert!(formality > 0.3 && formality < 0.7, "should be a compromise");
    }

    #[test]
    fn test_persona_stack() {
        let mut stack = PersonaStack::new(3);

        let parent = PersonaVector::from_pairs(&[(PersonaDimension::Formality, 0.9)]);
        let child = PersonaVector::from_pairs(&[(PersonaDimension::Formality, 0.3)]);

        stack.push(parent.clone()).unwrap();
        stack.push(child).unwrap();

        assert_eq!(stack.depth(), 2);

        let restored = stack.pop().unwrap();
        assert!((restored.get(PersonaDimension::Formality) - 0.3).abs() < 0.01);

        let original = stack.pop().unwrap();
        assert!((original.get(PersonaDimension::Formality) - 0.9).abs() < 0.01);
    }

    #[test]
    fn test_hierarchical_override() {
        let policy = PersonaOverride::new("corporate")
            .constrain(PersonaDimension::Formality, Some(0.7), None)
            .constrain(PersonaDimension::Creativity, None, Some(0.5));

        let casual = PersonaVector::from_pairs(&[
            (PersonaDimension::Formality, 0.2),
            (PersonaDimension::Creativity, 0.9),
        ]);

        let overridden = policy.apply(&casual);
        assert!(overridden.get(PersonaDimension::Formality) >= 0.7);
        assert!(overridden.get(PersonaDimension::Creativity) <= 0.5);

        let violations = policy.violations(&casual);
        assert_eq!(violations.len(), 2);
    }

    #[test]
    fn test_stack_overflow() {
        let mut stack = PersonaStack::new(2);
        stack.push(PersonaVector::neutral()).unwrap();
        stack.push(PersonaVector::neutral()).unwrap();
        assert!(stack.push(PersonaVector::neutral()).is_err());
    }
}
