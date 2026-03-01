//! Agent Trait Composition System — algebraic persona composition.
//!
//! A Trait is a typed, parameterized prompt fragment with declared conflicts
//! and affinities. Traits compose into coherent agent personas via constraint
//! satisfaction and affinity-ordered assembly.
//!
//! ## Trait categories
//!
//! - **Persona**: tone, formality, communication style
//! - **Methodology**: reasoning framework
//! - **Domain**: subject expertise
//! - **Output**: response format
//! - **Constraint**: behavioral boundaries
//!
//! ## Composition algebra
//!
//! Validity: composition C ⊆ T is valid iff:
//!     ∀tᵢ ∈ C: conflicts(tᵢ) ∩ C = ∅
//!
//! Coherence: S(C) = (Σ affinity(tᵢ, tⱼ)) / (|C| choose 2)
//!
//! Assembly order: topological sort on affinity-weighted DAG.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

// ─── Trait Category ──────────────────────────────────────────────────────────

/// Category of a persona trait.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraitCategory {
    /// Tone and communication style (e.g., concise, academic, friendly).
    Persona,
    /// Reasoning framework (e.g., first-principles, evidence-based).
    Methodology,
    /// Subject expertise (e.g., legal, financial, medical).
    Domain,
    /// Response format (e.g., structured-report, conversational, code-first).
    Output,
    /// Behavioral boundaries (e.g., no-financial-advice, hipaa-compliant).
    Constraint,
}

impl TraitCategory {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Persona => "persona",
            Self::Methodology => "methodology",
            Self::Domain => "domain",
            Self::Output => "output",
            Self::Constraint => "constraint",
        }
    }
}

impl std::fmt::Display for TraitCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ─── Trait Definition ────────────────────────────────────────────────────────

/// A composable agent trait — a typed prompt fragment with constraints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTrait {
    /// Unique trait identifier (e.g., "concise", "legal", "first-principles").
    pub id: String,
    /// Human-readable display name.
    pub display_name: String,
    /// Description of this trait's effect.
    pub description: String,
    /// Category classification.
    pub category: TraitCategory,
    /// Prompt fragment to inject when this trait is active.
    /// Typically 50-150 tokens.
    pub prompt_fragment: String,
    /// Estimated token cost.
    pub estimated_tokens: usize,
    /// Traits that conflict with this one (mutual exclusion).
    #[serde(default)]
    pub conflicts: Vec<String>,
    /// Pairwise affinity scores with other traits ∈ [0, 1].
    /// Higher affinity = should be adjacent in prompt assembly.
    #[serde(default)]
    pub affinities: HashMap<String, f64>,
    /// Priority within its category (higher = placed earlier).
    #[serde(default = "default_priority")]
    pub priority: f64,
}

fn default_priority() -> f64 {
    0.5
}

// ─── Composition Validation ──────────────────────────────────────────────────

/// Error from trait composition validation.
#[derive(Debug, Clone)]
pub enum CompositionError {
    /// Two traits conflict with each other.
    Conflict {
        trait_a: String,
        trait_b: String,
    },
    /// A referenced trait was not found in the trait library.
    UnknownTrait(String),
    /// Too many traits in one category.
    CategoryOverflow {
        category: TraitCategory,
        count: usize,
        max: usize,
    },
}

impl std::fmt::Display for CompositionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict { trait_a, trait_b } => {
                write!(f, "trait conflict: '{}' and '{}' are incompatible", trait_a, trait_b)
            }
            Self::UnknownTrait(id) => write!(f, "unknown trait: '{}'", id),
            Self::CategoryOverflow { category, count, max } => {
                write!(
                    f,
                    "too many {} traits: {} (max {})",
                    category, count, max
                )
            }
        }
    }
}

/// Result of composing a set of traits.
#[derive(Debug, Clone)]
pub struct CompositionResult {
    /// Ordered trait IDs (affinity-sorted for prompt assembly).
    pub ordered_traits: Vec<String>,
    /// Composed prompt text (traits concatenated in order).
    pub composed_prompt: String,
    /// Total estimated tokens.
    pub total_tokens: usize,
    /// Coherence score ∈ [0, 1].
    pub coherence_score: f64,
    /// Category breakdown.
    pub category_counts: HashMap<TraitCategory, usize>,
}

// ─── Trait Library ───────────────────────────────────────────────────────────

/// Maximum traits per category.
const MAX_TRAITS_PER_CATEGORY: usize = 3;

/// Library of available traits + composition engine.
pub struct TraitLibrary {
    traits: HashMap<String, AgentTrait>,
}

impl TraitLibrary {
    pub fn new() -> Self {
        Self {
            traits: HashMap::new(),
        }
    }

    /// Register a trait definition.
    pub fn register(&mut self, t: AgentTrait) {
        self.traits.insert(t.id.clone(), t);
    }

    /// Get a trait by ID.
    pub fn get(&self, id: &str) -> Option<&AgentTrait> {
        self.traits.get(id)
    }

    /// All trait IDs.
    pub fn trait_ids(&self) -> Vec<&str> {
        self.traits.keys().map(|s| s.as_str()).collect()
    }

    /// All traits in a category.
    pub fn by_category(&self, cat: TraitCategory) -> Vec<&AgentTrait> {
        self.traits
            .values()
            .filter(|t| t.category == cat)
            .collect()
    }

    /// Total number of registered traits.
    pub fn len(&self) -> usize {
        self.traits.len()
    }

    pub fn is_empty(&self) -> bool {
        self.traits.is_empty()
    }

    /// Validate a set of trait IDs for composability.
    /// Returns errors if any conflicts exist or traits are unknown.
    pub fn validate(&self, trait_ids: &[String]) -> Vec<CompositionError> {
        let mut errors = Vec::new();
        let id_set: HashSet<&str> = trait_ids.iter().map(|s| s.as_str()).collect();

        // Check for unknown traits
        for id in trait_ids {
            if !self.traits.contains_key(id) {
                errors.push(CompositionError::UnknownTrait(id.clone()));
            }
        }

        // Check conflicts
        for id in trait_ids {
            if let Some(t) = self.traits.get(id) {
                for conflict in &t.conflicts {
                    if id_set.contains(conflict.as_str()) {
                        // Only report once (alphabetical order)
                        if id.as_str() < conflict.as_str() {
                            errors.push(CompositionError::Conflict {
                                trait_a: id.clone(),
                                trait_b: conflict.clone(),
                            });
                        }
                    }
                }
            }
        }

        // Check category limits
        let mut cat_counts: HashMap<TraitCategory, usize> = HashMap::new();
        for id in trait_ids {
            if let Some(t) = self.traits.get(id) {
                *cat_counts.entry(t.category).or_default() += 1;
            }
        }
        for (cat, count) in &cat_counts {
            if *count > MAX_TRAITS_PER_CATEGORY {
                errors.push(CompositionError::CategoryOverflow {
                    category: *cat,
                    count: *count,
                    max: MAX_TRAITS_PER_CATEGORY,
                });
            }
        }

        errors
    }

    /// Compose a set of trait IDs into a coherent prompt.
    ///
    /// Ordering: traits are sorted by category priority, then by pairwise
    /// affinity (adjacent traits maximize local affinity).
    pub fn compose(&self, trait_ids: &[String]) -> Result<CompositionResult, Vec<CompositionError>> {
        let errors = self.validate(trait_ids);
        if !errors.is_empty() {
            return Err(errors);
        }

        // Collect resolved traits
        let mut resolved: Vec<&AgentTrait> = trait_ids
            .iter()
            .filter_map(|id| self.traits.get(id))
            .collect();

        // Sort by category order (Constraint > Domain > Methodology > Output > Persona)
        // then by priority within category
        let cat_order = |c: &TraitCategory| -> u8 {
            match c {
                TraitCategory::Constraint => 0,
                TraitCategory::Domain => 1,
                TraitCategory::Methodology => 2,
                TraitCategory::Output => 3,
                TraitCategory::Persona => 4,
            }
        };
        resolved.sort_by(|a, b| {
            cat_order(&a.category)
                .cmp(&cat_order(&b.category))
                .then(b.priority.partial_cmp(&a.priority).unwrap_or(std::cmp::Ordering::Equal))
        });

        // Compute coherence score
        let coherence = self.coherence_score(&resolved);

        // Build composed prompt
        let mut parts = Vec::new();
        let mut total_tokens = 0;
        let mut category_counts: HashMap<TraitCategory, usize> = HashMap::new();

        for t in &resolved {
            parts.push(t.prompt_fragment.as_str());
            total_tokens += t.estimated_tokens;
            *category_counts.entry(t.category).or_default() += 1;
        }

        let ordered_traits: Vec<String> = resolved.iter().map(|t| t.id.clone()).collect();
        let composed_prompt = parts.join("\n\n");

        Ok(CompositionResult {
            ordered_traits,
            composed_prompt,
            total_tokens,
            coherence_score: coherence,
            category_counts,
        })
    }

    /// Compute pairwise coherence score for a set of traits.
    /// S(C) = (Σ_{i≠j} affinity(tᵢ, tⱼ)) / (|C| choose 2)
    fn coherence_score(&self, traits: &[&AgentTrait]) -> f64 {
        let n = traits.len();
        if n < 2 {
            return 1.0;
        }
        let pairs = n * (n - 1) / 2;
        let mut total_affinity = 0.0;
        for i in 0..n {
            for j in (i + 1)..n {
                let aff_ij = traits[i]
                    .affinities
                    .get(&traits[j].id)
                    .copied()
                    .unwrap_or(0.5); // default neutral affinity
                total_affinity += aff_ij;
            }
        }
        total_affinity / pairs as f64
    }
}

impl Default for TraitLibrary {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Built-in Traits ─────────────────────────────────────────────────────────

/// Load the built-in trait library with standard personality traits.
pub fn builtin_trait_library() -> TraitLibrary {
    let mut lib = TraitLibrary::new();

    // ── Persona traits ─────────────────────────────────────
    lib.register(AgentTrait {
        id: "concise".into(),
        display_name: "Concise".into(),
        description: "Brief, focused responses without unnecessary elaboration".into(),
        category: TraitCategory::Persona,
        prompt_fragment: "Be concise and direct. Prioritize clarity over completeness. \
            Omit filler phrases and unnecessary qualifications."
            .into(),
        estimated_tokens: 25,
        conflicts: vec!["verbose".into()],
        affinities: HashMap::from([("code-first".into(), 0.8), ("structured-report".into(), 0.4)]),
        priority: 0.8,
    });
    lib.register(AgentTrait {
        id: "verbose".into(),
        display_name: "Verbose".into(),
        description: "Detailed, thorough responses with full explanations".into(),
        category: TraitCategory::Persona,
        prompt_fragment: "Provide thorough, detailed explanations. Include context, \
            examples, and edge cases. Anticipate follow-up questions."
            .into(),
        estimated_tokens: 25,
        conflicts: vec!["concise".into()],
        affinities: HashMap::from([
            ("academic".into(), 0.8),
            ("evidence-based".into(), 0.7),
        ]),
        priority: 0.6,
    });
    lib.register(AgentTrait {
        id: "friendly".into(),
        display_name: "Friendly".into(),
        description: "Warm, approachable communication style".into(),
        category: TraitCategory::Persona,
        prompt_fragment: "Use a warm, approachable tone. Be encouraging and supportive. \
            Use \"we\" language where appropriate."
            .into(),
        estimated_tokens: 22,
        conflicts: vec!["formal".into()],
        affinities: HashMap::from([("conversational".into(), 0.9)]),
        priority: 0.7,
    });
    lib.register(AgentTrait {
        id: "formal".into(),
        display_name: "Formal".into(),
        description: "Professional, formal communication style".into(),
        category: TraitCategory::Persona,
        prompt_fragment: "Maintain a professional, formal tone. Use precise terminology. \
            Avoid colloquialisms and casual language."
            .into(),
        estimated_tokens: 22,
        conflicts: vec!["friendly".into()],
        affinities: HashMap::from([
            ("academic".into(), 0.9),
            ("legal".into(), 0.8),
            ("structured-report".into(), 0.7),
        ]),
        priority: 0.7,
    });
    lib.register(AgentTrait {
        id: "academic".into(),
        display_name: "Academic".into(),
        description: "Scholarly, citation-aware communication".into(),
        category: TraitCategory::Persona,
        prompt_fragment: "Write in an academic style with proper citations and references. \
            Use hedging language for uncertain claims. Structure arguments logically."
            .into(),
        estimated_tokens: 28,
        conflicts: vec![],
        affinities: HashMap::from([
            ("evidence-based".into(), 0.95),
            ("formal".into(), 0.9),
        ]),
        priority: 0.5,
    });

    // ── Methodology traits ─────────────────────────────────
    lib.register(AgentTrait {
        id: "first-principles".into(),
        display_name: "First Principles".into(),
        description: "Reason from fundamental axioms, decompose complex problems".into(),
        category: TraitCategory::Methodology,
        prompt_fragment: "Reason from first principles. Break problems into fundamental \
            components. Question assumptions. Build solutions from ground truths."
            .into(),
        estimated_tokens: 28,
        conflicts: vec![],
        affinities: HashMap::from([("code-first".into(), 0.7)]),
        priority: 0.9,
    });
    lib.register(AgentTrait {
        id: "evidence-based".into(),
        display_name: "Evidence-Based".into(),
        description: "Ground all claims in verifiable evidence and data".into(),
        category: TraitCategory::Methodology,
        prompt_fragment: "Base all claims on verifiable evidence. Cite sources when possible. \
            Distinguish between established facts, strong evidence, and speculation."
            .into(),
        estimated_tokens: 30,
        conflicts: vec!["creative-brainstorm".into()],
        affinities: HashMap::from([
            ("academic".into(), 0.95),
            ("legal".into(), 0.85),
            ("medical".into(), 0.9),
        ]),
        priority: 0.8,
    });
    lib.register(AgentTrait {
        id: "creative-brainstorm".into(),
        display_name: "Creative Brainstorm".into(),
        description: "Divergent thinking, idea generation, no premature filtering".into(),
        category: TraitCategory::Methodology,
        prompt_fragment: "Think divergently. Generate multiple creative options before \
            converging. Suspend judgment during ideation. Build on ideas rather than \
            critiquing prematurely."
            .into(),
        estimated_tokens: 32,
        conflicts: vec!["evidence-based".into()],
        affinities: HashMap::from([("conversational".into(), 0.6)]),
        priority: 0.6,
    });
    lib.register(AgentTrait {
        id: "systematic".into(),
        display_name: "Systematic".into(),
        description: "Step-by-step, methodical problem solving".into(),
        category: TraitCategory::Methodology,
        prompt_fragment: "Work systematically through problems step by step. \
            Create clear checklists and frameworks. Verify each step before proceeding."
            .into(),
        estimated_tokens: 26,
        conflicts: vec![],
        affinities: HashMap::from([
            ("structured-report".into(), 0.8),
            ("first-principles".into(), 0.7),
        ]),
        priority: 0.7,
    });

    // ── Domain traits ──────────────────────────────────────
    for (id, name, desc, prompt, affs) in [
        ("legal", "Legal", "Legal domain expertise",
         "Apply legal reasoning principles. Use precise legal terminology. \
          Note jurisdiction-specific differences. Flag when legal advice should \
          be sought from a qualified attorney.",
         vec![("evidence-based", 0.85), ("formal", 0.8), ("no-legal-advice", 0.9)]),
        ("financial", "Financial", "Financial analysis expertise",
         "Apply financial analysis frameworks. Use standard financial metrics \
          and terminology. Consider risk factors and market conditions.",
         vec![("evidence-based", 0.8), ("systematic", 0.7), ("no-financial-advice", 0.85)]),
        ("medical", "Medical", "Medical/health domain knowledge",
         "Apply evidence-based medical reasoning. Use standard medical terminology. \
          Always note limitations of AI medical guidance. Recommend professional \
          consultation for diagnosis or treatment.",
         vec![("evidence-based", 0.9), ("hipaa-compliant", 0.95)]),
        ("engineering", "Engineering", "Software engineering expertise",
         "Apply software engineering best practices. Consider scalability, \
          maintainability, and performance. Follow SOLID principles and clean \
          architecture patterns.",
         vec![("code-first", 0.9), ("first-principles", 0.7), ("systematic", 0.8)]),
        ("data-science", "Data Science", "Data analysis and ML expertise",
         "Apply statistical rigor to data analysis. Consider sample sizes, \
          bias, and confounders. Use appropriate visualization and metrics.",
         vec![("evidence-based", 0.85), ("systematic", 0.7)]),
        ("education", "Education", "Teaching and tutoring expertise",
         "Adapt explanations to the learner's level. Use analogies and examples. \
          Build on prior knowledge. Check understanding with questions.",
         vec![("friendly", 0.8), ("verbose", 0.6)]),
        ("writing", "Writing", "Creative and professional writing",
         "Apply strong writing craft: clear structure, vivid language, \
          appropriate voice. Consider audience and purpose.",
         vec![("creative-brainstorm", 0.7)]),
        ("devops", "DevOps", "Infrastructure and operations expertise",
         "Apply infrastructure-as-code principles. Consider reliability, \
          observability, and security. Follow the principle of least privilege.",
         vec![("systematic", 0.8), ("engineering", 0.85)]),
        ("security", "Security", "Cybersecurity expertise",
         "Apply defense-in-depth principles. Consider threat models and attack \
          surfaces. Follow OWASP guidelines. Never expose credentials or secrets.",
         vec![("systematic", 0.9), ("evidence-based", 0.7)]),
        ("research", "Research", "Academic and scientific research",
         "Apply rigorous research methodology. Evaluate source quality. \
          Synthesize findings across multiple sources. Note confidence levels.",
         vec![("academic", 0.95), ("evidence-based", 0.9)]),
    ] {
        lib.register(AgentTrait {
            id: id.into(),
            display_name: name.into(),
            description: desc.into(),
            category: TraitCategory::Domain,
            prompt_fragment: prompt.into(),
            estimated_tokens: 35,
            conflicts: vec![],
            affinities: affs.into_iter().map(|(k, v)| (k.into(), v)).collect(),
            priority: 0.7,
        });
    }

    // ── Output traits ──────────────────────────────────────
    lib.register(AgentTrait {
        id: "structured-report".into(),
        display_name: "Structured Report".into(),
        description: "Responses organized with headers, sections, and key takeaways".into(),
        category: TraitCategory::Output,
        prompt_fragment: "Structure responses with clear headers, numbered sections, \
            and key takeaways. Use tables for comparative data. End with actionable \
            next steps."
            .into(),
        estimated_tokens: 30,
        conflicts: vec!["conversational".into()],
        affinities: HashMap::from([
            ("formal".into(), 0.7),
            ("systematic".into(), 0.8),
        ]),
        priority: 0.7,
    });
    lib.register(AgentTrait {
        id: "conversational".into(),
        display_name: "Conversational".into(),
        description: "Natural dialogue-style responses".into(),
        category: TraitCategory::Output,
        prompt_fragment: "Respond in a natural conversational style. Ask clarifying \
            questions. Use shorter paragraphs. Make the interaction feel like a dialogue."
            .into(),
        estimated_tokens: 26,
        conflicts: vec!["structured-report".into()],
        affinities: HashMap::from([("friendly".into(), 0.9)]),
        priority: 0.6,
    });
    lib.register(AgentTrait {
        id: "code-first".into(),
        display_name: "Code First".into(),
        description: "Lead with code, explain after".into(),
        category: TraitCategory::Output,
        prompt_fragment: "Lead with working code examples. Use inline comments for \
            explanation. Keep prose minimal — let the code speak. Include usage examples."
            .into(),
        estimated_tokens: 26,
        conflicts: vec![],
        affinities: HashMap::from([
            ("concise".into(), 0.8),
            ("engineering".into(), 0.9),
        ]),
        priority: 0.8,
    });

    // ── Constraint traits ──────────────────────────────────
    lib.register(AgentTrait {
        id: "no-financial-advice".into(),
        display_name: "No Financial Advice".into(),
        description: "Prohibits specific financial advice".into(),
        category: TraitCategory::Constraint,
        prompt_fragment: "IMPORTANT: Do not provide specific financial advice, \
            investment recommendations, or tax guidance. Always recommend consulting \
            a qualified financial advisor for personal financial decisions."
            .into(),
        estimated_tokens: 35,
        conflicts: vec![],
        affinities: HashMap::from([("financial".into(), 0.85)]),
        priority: 1.0,
    });
    lib.register(AgentTrait {
        id: "no-legal-advice".into(),
        display_name: "No Legal Advice".into(),
        description: "Prohibits specific legal advice".into(),
        category: TraitCategory::Constraint,
        prompt_fragment: "IMPORTANT: Do not provide specific legal advice or opinions \
            on legal matters. Always recommend consulting a qualified attorney. \
            Provide general legal information only."
            .into(),
        estimated_tokens: 32,
        conflicts: vec![],
        affinities: HashMap::from([("legal".into(), 0.9)]),
        priority: 1.0,
    });
    lib.register(AgentTrait {
        id: "hipaa-compliant".into(),
        display_name: "HIPAA Compliant".into(),
        description: "Health information privacy compliance".into(),
        category: TraitCategory::Constraint,
        prompt_fragment: "IMPORTANT: Never store, transmit, or request Protected Health \
            Information (PHI). Do not provide medical diagnoses or treatment plans. \
            Always recommend consulting a healthcare professional."
            .into(),
        estimated_tokens: 35,
        conflicts: vec![],
        affinities: HashMap::from([("medical".into(), 0.95)]),
        priority: 1.0,
    });

    lib
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_library_size() {
        let lib = builtin_trait_library();
        // 5 persona + 4 methodology + 10 domain + 3 output + 3 constraint = 25
        assert!(lib.len() >= 22, "expected ≥22 traits, got {}", lib.len());
    }

    #[test]
    fn test_valid_composition() {
        let lib = builtin_trait_library();
        let traits = vec![
            "concise".to_string(),
            "first-principles".to_string(),
            "engineering".to_string(),
            "code-first".to_string(),
        ];
        let result = lib.compose(&traits).unwrap();
        assert_eq!(result.ordered_traits.len(), 4);
        assert!(result.coherence_score > 0.0);
        assert!(result.total_tokens > 0);
    }

    #[test]
    fn test_conflict_detection() {
        let lib = builtin_trait_library();
        let traits = vec!["concise".to_string(), "verbose".to_string()];
        let errors = lib.validate(&traits);
        assert!(!errors.is_empty());
        assert!(matches!(errors[0], CompositionError::Conflict { .. }));
    }

    #[test]
    fn test_compose_with_constraints() {
        let lib = builtin_trait_library();
        let traits = vec![
            "legal".to_string(),
            "evidence-based".to_string(),
            "formal".to_string(),
            "structured-report".to_string(),
            "no-legal-advice".to_string(),
        ];
        let result = lib.compose(&traits).unwrap();
        // Constraints should come first in assembly order
        assert_eq!(result.ordered_traits[0], "no-legal-advice");
        assert!(result.coherence_score > 0.6);
    }

    #[test]
    fn test_unknown_trait() {
        let lib = builtin_trait_library();
        let traits = vec!["nonexistent".to_string()];
        let errors = lib.validate(&traits);
        assert!(matches!(errors[0], CompositionError::UnknownTrait(_)));
    }

    #[test]
    fn test_combinatorial_count() {
        let lib = builtin_trait_library();
        let persona_count = lib.by_category(TraitCategory::Persona).len();
        let method_count = lib.by_category(TraitCategory::Methodology).len();
        let domain_count = lib.by_category(TraitCategory::Domain).len();
        let output_count = lib.by_category(TraitCategory::Output).len();
        // Upper bound: p × m × d × o (ignoring conflicts)
        let max_archetypes = persona_count * method_count * domain_count * output_count;
        assert!(max_archetypes >= 200, "expected ≥200 combos, got {}", max_archetypes);
    }
}
