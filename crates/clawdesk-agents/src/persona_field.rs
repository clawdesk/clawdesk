//! Persona Field — a vector-space model for agent personality.
//!
//! ## Why not SOUL.md?
//!
//! OpenClaw dumps a markdown file into the context window. This has three
//! fundamental problems from an information-theoretic perspective:
//!
//! 1. **Entropy waste**: A 2000-token persona document uses 2000 tokens
//!    regardless of whether the current turn needs 2 or 200 of those traits.
//!    Shannon's source coding theorem tells us we can do better.
//!
//! 2. **No composability**: Two markdown files can't be meaningfully combined.
//!    You can concatenate them, but that's not composition — it's collision.
//!    Traits contradict, priorities are ambiguous, structure is lost.
//!
//! 3. **No observability**: When an agent behaves unexpectedly, there's no way
//!    to determine which part of the prose caused it. It's a black box.
//!
//! ## The Persona Field Model
//!
//! Instead of treating personality as a *document*, we model it as a
//! *vector field* — a function from context to behavioral modifiers.
//!
//! ```text
//! Persona: Context → Behavioral Modifiers
//!
//! Where:
//!   Context = (topic, channel, tool, turn_depth, user_mood)
//!   Modifiers = weighted set of active traits
//! ```
//!
//! This is analogous to how a gravitational field works in physics:
//! the field exists everywhere, but the force at any point depends on
//! the local context (mass, distance). The persona field "exists" as a
//! complete description, but only the relevant facets manifest at each turn.
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────┐
//! │ Layer 1: Capture (user describes personality)           │
//! │                                                         │
//! │   "Be direct, use data, don't sugarcoat"               │
//! │     ↓                                                   │
//! │   Parsed into: { directness: 0.9, empathy: 0.3,       │
//! │                   data_driven: 0.95, formality: 0.5 }  │
//! └─────────────────────────────────────────────────────────┘
//!                        ↓
//! ┌─────────────────────────────────────────────────────────┐
//! │ Layer 2: Trait Algebra (composable operations)          │
//! │                                                         │
//! │   base_persona ⊕ channel_override ⊕ user_pref          │
//! │     where ⊕ is lattice join with priority weighting    │
//! └─────────────────────────────────────────────────────────┘
//!                        ↓
//! ┌─────────────────────────────────────────────────────────┐
//! │ Layer 3: Projection (context-adaptive injection)        │
//! │                                                         │
//! │   Current turn about "code review"?                     │
//! │     → Activate: technical_depth=0.9, directness=0.8    │
//! │     → Suppress: humor=0.1, verbosity=0.2               │
//! │     → Inject: ~80 tokens (not 2000)                    │
//! └─────────────────────────────────────────────────────────┘
//! ```

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ═══════════════════════════════════════════════════════════════════════════
// Trait Dimensions — the basis vectors of personality space
// ═══════════════════════════════════════════════════════════════════════════

/// A single personality trait with a value in [0.0, 1.0].
///
/// **Design note (v2)**: Internally we still use f32 for algebra (compose,
/// blend, distance). But the USER-FACING API uses `Intensity` — a coarse
/// 5-level scale. This eliminates false precision (0.90 vs 0.85) while
/// keeping the math clean.
pub type TraitValue = f32;

/// Coarse 5-level intensity — the user-facing representation.
///
/// Maps to f32 for internal math, but the user never sees decimals.
/// This prevents the "decorative precision" problem where 0.87 looks
/// rigorous but has no calibrated meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Intensity {
    /// `--` Strongly low (0.1)
    #[serde(rename = "--")]
    StrongLow,
    /// `-` Low (0.3)
    #[serde(rename = "-")]
    Low,
    /// `·` Neutral / don't care (0.5) — default, not stored
    #[serde(rename = ".")]
    Neutral,
    /// `+` High (0.7)
    #[serde(rename = "+")]
    High,
    /// `++` Strongly high (0.9)
    #[serde(rename = "++")]
    StrongHigh,
}

impl Intensity {
    /// Convert to internal f32 value.
    pub fn to_f32(self) -> f32 {
        match self {
            Self::StrongLow => 0.1,
            Self::Low => 0.3,
            Self::Neutral => 0.5,
            Self::High => 0.7,
            Self::StrongHigh => 0.9,
        }
    }

    /// Quantize an f32 to the nearest intensity level.
    pub fn from_f32(v: f32) -> Self {
        if v < 0.2 { Self::StrongLow }
        else if v < 0.4 { Self::Low }
        else if v < 0.6 { Self::Neutral }
        else if v < 0.8 { Self::High }
        else { Self::StrongHigh }
    }

    /// Whether this is non-neutral (has an opinion).
    pub fn is_opinionated(self) -> bool {
        !matches!(self, Self::Neutral)
    }

    /// Display label for prompt generation.
    pub fn label(self) -> &'static str {
        match self {
            Self::StrongLow => "Strongly avoid",
            Self::Low => "Lean away from",
            Self::Neutral => "No preference",
            Self::High => "Lean toward",
            Self::StrongHigh => "Strongly",
        }
    }
}

/// Trait dimensions that span the personality space.
///
/// **v2 change**: Dimensions are now tagged with a `TraitLane`:
/// - **Style**: Response formatting — directness, verbosity, formality, humor
/// - **Stance**: Task approach — pedagogy, technical depth, decisiveness, analytical
///
/// Style traits are context-projected (may be suppressed).
/// Stance traits project with higher survival probability.
/// Hard rules (PolicyConstraint) live in a separate lane entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraitDimension {
    // ── Style lane (response formatting) ─────────────────────
    /// Formal ↔ Casual
    Formality,
    /// Terse ↔ Verbose
    Verbosity,
    /// Gentle ↔ Direct
    Directness,
    /// Serious ↔ Playful
    Humor,

    // ── Stance lane (task approach) ──────────────────────────
    /// Hard facts ↔ Warm vibes
    Analytical,
    /// Cautious ↔ Bold
    Boldness,
    /// Rigid ↔ Creative
    Creativity,
    /// Solo focus ↔ Collaborative
    Collaboration,
    /// Reactive ↔ Proactive
    Initiative,
    /// Shallow breadth ↔ Deep expertise
    TechnicalDepth,
    /// Skip to answer ↔ Thorough teaching
    Pedagogy,
    /// Hedging ↔ Assertive
    Confidence,
}

/// Which lane a trait belongs to — determines projection survival rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraitLane {
    /// Response formatting. Context-projected with normal thresholds.
    Style,
    /// Task approach. Projects with higher survival (lower threshold).
    Stance,
}

impl TraitDimension {
    /// All dimensions for iteration.
    pub const ALL: [TraitDimension; 12] = [
        Self::Formality, Self::Verbosity, Self::Directness, Self::Humor,
        Self::Analytical, Self::Boldness, Self::Creativity, Self::Collaboration,
        Self::Initiative, Self::TechnicalDepth, Self::Pedagogy, Self::Confidence,
    ];

    /// Which lane this trait belongs to.
    pub fn lane(&self) -> TraitLane {
        match self {
            Self::Formality | Self::Verbosity | Self::Directness | Self::Humor => TraitLane::Style,
            _ => TraitLane::Stance,
        }
    }

    /// Human-readable description of the trait at high/low extremes.
    ///
    /// **v2 fix**: Descriptions stay faithful to the dimension name.
    /// No semantic leaps — "confidence" describes confidence, not "opinionated."
    /// Each description is a direct behavioral instruction the LLM can follow.
    pub fn describe(&self, value: TraitValue) -> &'static str {
        match (self, value > 0.5) {
            (Self::Formality, true) => "use formal, professional tone",
            (Self::Formality, false) => "use casual, conversational tone",
            (Self::Verbosity, true) => "give detailed, thorough responses",
            (Self::Verbosity, false) => "keep responses concise and brief",
            (Self::Directness, true) => "be direct — state conclusions first",
            (Self::Directness, false) => "be diplomatic — soften critical feedback",
            (Self::Analytical, true) => "ground claims in data and evidence",
            (Self::Analytical, false) => "lead with empathy and understanding",
            (Self::Boldness, true) => "commit to recommendations decisively",
            (Self::Boldness, false) => "present multiple options cautiously",
            (Self::Creativity, true) => "suggest unconventional approaches",
            (Self::Creativity, false) => "stick to proven, standard methods",
            (Self::Collaboration, true) => "frame work as collaborative effort",
            (Self::Collaboration, false) => "operate autonomously, report results",
            (Self::Initiative, true) => "proactively suggest next steps",
            (Self::Initiative, false) => "wait for explicit instructions",
            (Self::TechnicalDepth, true) => "go deep — implementation details, edge cases",
            (Self::TechnicalDepth, false) => "stay high-level — skip implementation details",
            (Self::Humor, true) => "use occasional dry wit where appropriate",
            (Self::Humor, false) => "maintain serious, all-business tone",
            (Self::Pedagogy, true) => "explain reasoning step by step",
            (Self::Pedagogy, false) => "skip explanations — give the answer directly",
            (Self::Confidence, true) => "state views with confidence, don't hedge",
            (Self::Confidence, false) => "acknowledge uncertainty, present alternatives",
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Persona Vector — a point in personality space
// ═══════════════════════════════════════════════════════════════════════════

/// A persona vector — a point in the 12-dimensional personality space.
///
/// Mathematically: P ∈ [0,1]^12
///
/// Only non-default (non-0.5) dimensions are stored. This is a sparse
/// representation — most personas only specify 3-6 traits. The rest
/// default to 0.5 (neutral), meaning "I don't care about this axis."
///
/// This sparsity is what makes projection efficient: we only inject
/// instructions for dimensions where the persona has an opinion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonaVector {
    /// Sparse trait values. Missing dimensions default to 0.5 (neutral).
    traits: BTreeMap<TraitDimension, TraitValue>,
    /// Free-form behavioral rules that don't map to trait dimensions.
    /// These are short, imperative sentences (e.g., "Never apologize").
    /// Max 10 rules, each max 200 chars.
    custom_rules: Vec<String>,
    /// The user's original natural language description (for display/editing).
    /// This is the "source of truth" — trait values are derived from it.
    source_description: Option<String>,
}

impl Default for PersonaVector {
    fn default() -> Self {
        Self {
            traits: BTreeMap::new(),
            custom_rules: Vec::new(),
            source_description: None,
        }
    }
}

impl PersonaVector {
    /// Create a persona with explicit trait values.
    pub fn from_traits(traits: Vec<(TraitDimension, TraitValue)>) -> Self {
        let mut map = BTreeMap::new();
        for (dim, val) in traits {
            map.insert(dim, val.clamp(0.0, 1.0));
        }
        Self {
            traits: map,
            custom_rules: Vec::new(),
            source_description: None,
        }
    }

    /// Create a persona from a natural language description.
    ///
    /// **v2 change**: Uses synonym groups for robust normalization.
    /// "Direct," "blunt," "terse," "no fluff," "executive summary style"
    /// all map to the same trait at the same intensity.
    ///
    /// This is intentionally NOT an LLM call — deterministic, instant, offline.
    pub fn from_description(description: &str) -> Self {
        let mut traits = BTreeMap::new();
        let lower = description.to_ascii_lowercase();

        // Synonym groups: each group maps to (dimension, intensity).
        // The FIRST matching group for a dimension wins.
        // Groups are ordered high→low so "not direct" doesn't override "direct".
        static SYNONYM_MAP: &[(&[&str], TraitDimension, Intensity)] = &[
            // ── Formality ──
            (&["formal", "professional", "corporate", "polished", "business-like"],
             TraitDimension::Formality, Intensity::StrongHigh),
            (&["casual", "chill", "relaxed", "friendly", "informal", "conversational tone"],
             TraitDimension::Formality, Intensity::Low),

            // ── Verbosity ──
            (&["concise", "brief", "short", "terse", "minimal", "to the point",
               "to-the-point", "no fluff", "executive summary", "bottom line", "tldr"],
             TraitDimension::Verbosity, Intensity::StrongLow),
            (&["detailed", "thorough", "verbose", "explain everything", "comprehensive",
               "leave nothing out", "full detail"],
             TraitDimension::Verbosity, Intensity::StrongHigh),

            // ── Directness ──
            (&["direct", "blunt", "honest", "no sugarcoat", "don't sugarcoat",
               "unfiltered", "straight", "no fluff", "cut the bs", "no bs",
               "tell it like it is", "no beating around"],
             TraitDimension::Directness, Intensity::StrongHigh),
            (&["diplomatic", "gentle", "sensitive", "tactful", "careful",
               "soften", "considerate"],
             TraitDimension::Directness, Intensity::Low),

            // ── Humor ──
            (&["funny", "humor", "wit", "playful", "lighthearted", "jokes", "sarcastic"],
             TraitDimension::Humor, Intensity::High),
            (&["serious", "no jokes", "professional only", "no humor", "all business",
               "no nonsense"],
             TraitDimension::Humor, Intensity::StrongLow),

            // ── Analytical ──
            (&["data", "evidence", "analytical", "quantitative", "numbers",
               "metrics", "systematic", "data-driven", "empirical", "rigorous"],
             TraitDimension::Analytical, Intensity::StrongHigh),
            (&["empathetic", "emotional", "intuitive", "warm", "caring", "feeling"],
             TraitDimension::Analytical, Intensity::Low),

            // ── Boldness ──
            (&["bold", "decisive", "assertive", "opinionated", "strong opinion",
               "take a stand", "don't sit on the fence"],
             TraitDimension::Boldness, Intensity::StrongHigh),
            (&["cautious", "conservative", "safe", "careful", "measured", "risk-averse"],
             TraitDimension::Boldness, Intensity::Low),

            // ── Creativity ──
            (&["creative", "inventive", "unconventional", "think outside",
               "out of the box", "innovative", "lateral thinking"],
             TraitDimension::Creativity, Intensity::StrongHigh),
            (&["methodical", "by the book", "structured", "standard",
               "conventional", "proven approach"],
             TraitDimension::Creativity, Intensity::Low),

            // ── Initiative ──
            (&["proactive", "initiative", "anticipate", "suggest", "recommend",
               "take charge", "go ahead", "just do it"],
             TraitDimension::Initiative, Intensity::StrongHigh),
            (&["wait for instruction", "only when asked", "don't assume",
               "ask first", "confirm before"],
             TraitDimension::Initiative, Intensity::Low),

            // ── Technical depth ──
            (&["deep dive", "deep-dive", "expert", "advanced", "in-depth",
               "thorough analysis", "low-level", "implementation detail"],
             TraitDimension::TechnicalDepth, Intensity::StrongHigh),
            (&["overview", "high-level", "summary", "big picture", "eli5",
               "layman", "non-technical"],
             TraitDimension::TechnicalDepth, Intensity::StrongLow),

            // ── Pedagogy ──
            (&["teach", "explain", "step by step", "step-by-step", "why",
               "educational", "mentor", "walk me through"],
             TraitDimension::Pedagogy, Intensity::StrongHigh),
            (&["skip explanation", "just answer", "no explanation",
               "just the code", "just do it", "no preamble"],
             TraitDimension::Pedagogy, Intensity::StrongLow),

            // ── Confidence ──
            (&["confident", "assertive", "opinionated", "take a stand",
               "don't hedge", "commit to an answer", "pick one"],
             TraitDimension::Confidence, Intensity::StrongHigh),
            (&["hedge", "consider alternatives", "present options",
               "uncertain", "on the other hand"],
             TraitDimension::Confidence, Intensity::Low),
        ];

        // Apply synonym groups — first match per dimension wins
        let mut seen_dims = std::collections::HashSet::new();
        for &(synonyms, dim, intensity) in SYNONYM_MAP {
            if seen_dims.contains(&dim) {
                continue;
            }
            if synonyms.iter().any(|s| lower.contains(s)) {
                traits.insert(dim, intensity.to_f32());
                seen_dims.insert(dim);
            }
        }

        // ── Extract hard rules (PolicyConstraints — separate lane) ────
        let custom_rules: Vec<String> = description
            .split(|c: char| c == '.' || c == '\n')
            .map(|s| s.trim().to_string())
            .filter(|s| {
                let sl = s.to_ascii_lowercase();
                (sl.starts_with("never")
                    || sl.starts_with("always")
                    || sl.starts_with("don't")
                    || sl.starts_with("do not")
                    || sl.starts_with("avoid")
                    || sl.starts_with("prefer")
                    || sl.starts_with("use ")
                    || sl.starts_with("when "))
                    && s.len() > 10
                    && s.len() <= 200
            })
            .take(10)
            .collect();

        Self {
            traits,
            custom_rules,
            source_description: Some(description.to_string()),
        }
    }

    /// Get a trait value (returns 0.5/neutral if not set).
    pub fn get(&self, dim: TraitDimension) -> TraitValue {
        self.traits.get(&dim).copied().unwrap_or(0.5)
    }

    /// Set a trait value.
    pub fn set(&mut self, dim: TraitDimension, value: TraitValue) {
        self.traits.insert(dim, value.clamp(0.0, 1.0));
    }

    /// Number of non-neutral traits.
    pub fn active_dimensions(&self) -> usize {
        self.traits.len()
    }

    /// Add a custom behavioral rule.
    pub fn add_rule(&mut self, rule: impl Into<String>) {
        let rule = rule.into();
        if self.custom_rules.len() < 10 && rule.len() <= 200 {
            self.custom_rules.push(rule);
        }
    }

    // ── Algebra operations ───────────────────────────────────────────────

    /// Compose two personas: self ⊕ override.
    ///
    /// The override's explicitly-set traits take precedence.
    /// Self's traits are used as defaults. Custom rules are merged (deduped).
    ///
    /// This is a lattice join: override ⊕ base = override where set, base elsewhere.
    pub fn compose(&self, overlay: &PersonaVector) -> PersonaVector {
        let mut merged = self.traits.clone();
        for (&dim, &val) in &overlay.traits {
            merged.insert(dim, val);
        }

        let mut rules = self.custom_rules.clone();
        for rule in &overlay.custom_rules {
            if !rules.contains(rule) {
                rules.push(rule.clone());
            }
        }
        rules.truncate(10);

        PersonaVector {
            traits: merged,
            custom_rules: rules,
            source_description: overlay
                .source_description
                .clone()
                .or_else(|| self.source_description.clone()),
        }
    }

    /// Blend two personas with a weight: result = (1-α)·self + α·other.
    ///
    /// Linear interpolation in trait space. Useful for gradual persona
    /// transitions (e.g., becoming more formal over the course of a conversation).
    pub fn blend(&self, other: &PersonaVector, alpha: f32) -> PersonaVector {
        let alpha = alpha.clamp(0.0, 1.0);
        let mut blended = BTreeMap::new();

        // Union of all dimensions from both vectors
        for &dim in TraitDimension::ALL.iter() {
            let a = self.get(dim);
            let b = other.get(dim);
            let val = (1.0 - alpha) * a + alpha * b;
            // Only store if it deviates from neutral
            if (val - 0.5).abs() > 0.05 {
                blended.insert(dim, val);
            }
        }

        PersonaVector {
            traits: blended,
            custom_rules: self.custom_rules.clone(), // Keep self's rules on blend
            source_description: None,
        }
    }

    /// Euclidean distance between two persona vectors.
    ///
    /// Useful for measuring how "different" two personas are.
    /// Range: [0, √12] ≈ [0, 3.46]
    pub fn distance(&self, other: &PersonaVector) -> f32 {
        let mut sum_sq = 0.0f32;
        for &dim in TraitDimension::ALL.iter() {
            let diff = self.get(dim) - other.get(dim);
            sum_sq += diff * diff;
        }
        sum_sq.sqrt()
    }

    // ── Projection: context → active traits ──────────────────────────────

    /// Project the persona onto a specific context with explicit activation function.
    ///
    /// **v2 changes**:
    /// 1. Three-lane projection: Style traits projected normally; Stance traits
    ///    project with higher survival; Hard rules ALWAYS survive.
    /// 2. Explicit activation: `activation = base_strength × context_relevance`
    /// 3. Top-k budgeted selection (max 6 traits to keep prompt tight).
    /// 4. `channel_fit` parameter allows channel-specific dampening.
    ///
    /// The activation function is transparent and auditable — you can see
    /// exactly why a trait was included or excluded.
    pub fn project(&self, topic_hints: &[&str]) -> ProjectedPersona {
        self.project_with_fit(topic_hints, 1.0)
    }

    /// Project with an explicit channel fit multiplier.
    ///
    /// `channel_fit` in [0.0, 1.0]: how well this channel suits rich persona.
    /// Telegram (short messages) → 0.6, Desktop (full UI) → 1.0, IRC → 0.4.
    ///
    /// **v2 fix**: Important negatives survive projection.
    /// Low pedagogy + "explain" context → emits "skip explanations — give the answer directly"
    /// This prevents the threshold from hiding behaviorally crucial low values.
    pub fn project_with_fit(&self, topic_hints: &[&str], channel_fit: f32) -> ProjectedPersona {
        let relevance = compute_relevance(topic_hints);
        let channel_fit = channel_fit.clamp(0.1, 1.0);

        let mut candidates: Vec<(TraitDimension, TraitValue, f32)> = Vec::new();
        for &dim in TraitDimension::ALL.iter() {
            let trait_val = self.get(dim);
            let base_strength = (trait_val - 0.5).abs() * 2.0; // normalize to [0, 1]
            let context_relevance = relevance.get(&dim).copied().unwrap_or(0.2);

            // Explicit activation function (auditable):
            // activation = base_strength × context_relevance × channel_fit
            let activation = base_strength * context_relevance * channel_fit;

            // Lane-dependent survival threshold:
            // Style traits: activation > 0.25 (can be suppressed)
            // Stance traits: activation > 0.15 (survive more easily)
            let threshold = match dim.lane() {
                TraitLane::Style => 0.25,
                TraitLane::Stance => 0.15,
            };

            if activation > threshold {
                candidates.push((dim, trait_val, activation));
            }
        }

        // Sort by activation score (highest first)
        candidates.sort_by(|a, b| {
            b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal)
        });

        // Budget: max 6 traits (keeps prompt under ~80 tokens)
        candidates.truncate(6);

        ProjectedPersona {
            active_traits: candidates.iter().map(|(d, v, _)| (*d, *v)).collect(),
            // Hard rules ALWAYS survive — they're policy constraints, not soft style
            custom_rules: self.custom_rules.clone(),
        }
    }

    /// Generate the prompt fragment for this persona.
    ///
    /// Full injection (no projection) — used when context is unknown
    /// or for the initial system prompt.
    pub fn to_prompt_fragment(&self) -> String {
        let projected = ProjectedPersona {
            active_traits: self.traits.iter().map(|(&d, &v)| (d, v)).collect(),
            custom_rules: self.custom_rules.clone(),
        };
        projected.to_prompt_fragment()
    }
}

/// A projected persona — the subset of traits active for a specific turn.
#[derive(Debug, Clone)]
pub struct ProjectedPersona {
    /// Active trait dimensions and their values.
    pub active_traits: Vec<(TraitDimension, TraitValue)>,
    /// Custom behavioral rules (always included).
    pub custom_rules: Vec<String>,
}

impl ProjectedPersona {
    /// Generate a compact prompt fragment (~50-100 tokens).
    ///
    /// **v2 change**: Three-section output:
    /// 1. `[Style]` — formatting preferences (projected, may be absent)
    /// 2. `[Stance]` — task approach preferences (projected, higher survival)
    /// 3. `[Rules]` — hard constraints (ALWAYS present if defined)
    ///
    /// This separation lets the LLM distinguish "I should try to be concise"
    /// (style, soft) from "Never apologize" (rule, hard).
    pub fn to_prompt_fragment(&self) -> String {
        if self.active_traits.is_empty() && self.custom_rules.is_empty() {
            return String::new();
        }

        let mut lines = Vec::with_capacity(self.active_traits.len() + self.custom_rules.len() + 4);

        // Partition active traits into style and stance
        let style_traits: Vec<_> = self.active_traits.iter()
            .filter(|(d, _)| d.lane() == TraitLane::Style)
            .collect();
        let stance_traits: Vec<_> = self.active_traits.iter()
            .filter(|(d, _)| d.lane() == TraitLane::Stance)
            .collect();

        if !style_traits.is_empty() {
            lines.push("[Style]".to_string());
            for &&(dim, val) in &style_traits {
                let intensity = Intensity::from_f32(val);
                let description = dim.describe(val);
                lines.push(format!("- {}: {description}", intensity.label()));
            }
        }

        if !stance_traits.is_empty() {
            lines.push("[Stance]".to_string());
            for &&(dim, val) in &stance_traits {
                let intensity = Intensity::from_f32(val);
                let description = dim.describe(val);
                lines.push(format!("- {}: {description}", intensity.label()));
            }
        }

        if !self.custom_rules.is_empty() {
            lines.push("[Rules]".to_string());
            for rule in &self.custom_rules {
                lines.push(format!("- {rule}"));
            }
        }

        lines.join("\n")
    }

    /// Estimated token count (~1.3 tokens per word in English).
    pub fn estimated_tokens(&self) -> usize {
        let text = self.to_prompt_fragment();
        // Conservative estimate: ~4 chars per token
        text.len() / 4 + 1
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Temporal Smoothing — prevents style jitter across turns
// ═══════════════════════════════════════════════════════════════════════════

/// EWMA smoother for projected trait vectors.
///
/// Without smoothing, adjacent turns about "code" and "explaining code"
/// could swing wildly between "suppress humor" and "activate pedagogy."
/// The smoother blends the current projection with a moving average,
/// providing hysteresis that prevents jarring style transitions.
///
/// ```text
/// smoothed_n = α × current + (1 - α) × smoothed_{n-1}
/// ```
///
/// α = 0.4 means ~40% new signal, ~60% momentum. Settles in ~3 turns.
#[derive(Debug, Clone)]
pub struct PersonaSmoother {
    /// Previous smoothed trait values.
    prev: BTreeMap<TraitDimension, f32>,
    /// Smoothing factor (0 < α ≤ 1). Higher = more responsive, lower = more stable.
    alpha: f32,
    /// Number of observations (for cold-start: first turn uses raw values).
    observations: u32,
}

impl PersonaSmoother {
    /// Create with default smoothing (α = 0.4).
    pub fn new() -> Self {
        Self {
            prev: BTreeMap::new(),
            alpha: 0.4,
            observations: 0,
        }
    }

    /// Create with custom smoothing factor.
    pub fn with_alpha(alpha: f32) -> Self {
        Self {
            prev: BTreeMap::new(),
            alpha: alpha.clamp(0.05, 1.0),
            observations: 0,
        }
    }

    /// Smooth a projected persona against the running average.
    ///
    /// Returns a new ProjectedPersona with smoothed trait values.
    /// Hard rules are passed through unchanged (they don't smooth).
    pub fn smooth(&mut self, projected: &ProjectedPersona) -> ProjectedPersona {
        self.observations += 1;

        // Cold start: first observation uses raw values
        if self.observations == 1 {
            for &(dim, val) in &projected.active_traits {
                self.prev.insert(dim, val);
            }
            return projected.clone();
        }

        let mut smoothed_traits = Vec::new();
        for &(dim, current_val) in &projected.active_traits {
            let prev_val = self.prev.get(&dim).copied().unwrap_or(0.5);
            let smoothed = self.alpha * current_val + (1.0 - self.alpha) * prev_val;
            self.prev.insert(dim, smoothed);

            // Only include if still non-neutral after smoothing
            if (smoothed - 0.5).abs() > 0.1 {
                smoothed_traits.push((dim, smoothed));
            }
        }

        // Decay traits that were active previously but aren't in current projection
        // (they fade toward neutral rather than vanishing instantly)
        for (&dim, prev_val) in self.prev.iter_mut() {
            if !projected.active_traits.iter().any(|(d, _)| *d == dim) {
                let decayed = self.alpha * 0.5 + (1.0 - self.alpha) * *prev_val;
                *prev_val = decayed;
                // Don't add decayed traits unless still meaningfully non-neutral
            }
        }

        ProjectedPersona {
            active_traits: smoothed_traits,
            custom_rules: projected.custom_rules.clone(),
        }
    }

    /// Reset the smoother (e.g., on session start or context switch).
    pub fn reset(&mut self) {
        self.prev.clear();
        self.observations = 0;
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Context Relevance — which traits matter for which topics
// ═══════════════════════════════════════════════════════════════════════════

/// Compute trait relevance scores for a set of topic hints.
///
/// **This is a fixed lookup table** — not an embedding, classifier, or LLM call.
/// Each topic category maps to a predetermined set of trait relevance weights.
/// The table is hand-authored and should be evaluated against a consistency
/// test suite (see tests at the bottom of this file).
///
/// ## Projection method: `FIXED_LOOKUP_TABLE`
///
/// ```text
/// Topic Category     Activated Traits (relevance weight)
/// ─────────────────  ──────────────────────────────────────
/// code/technical     TechnicalDepth(0.95) Directness(0.8) Confidence(0.7) Verbosity(0.6)
/// data/analysis      Analytical(0.95) TechnicalDepth(0.8) Verbosity(0.7)
/// creative/writing   Creativity(0.95) Humor(0.7) Boldness(0.6)
/// support/emotional  Pedagogy(0.9) Directness(0.5) Humor(0.4)
/// planning/strategy  Initiative(0.85) Boldness(0.75) Collaboration(0.7)
/// teaching/learning  Pedagogy(0.95) Verbosity(0.8) TechnicalDepth(0.7)
/// ```
///
/// This design is intentionally simple. If you need richer topic→trait
/// mapping, replace this function with embedding similarity or a classifier —
/// but measure the latency cost first. The current implementation is O(k×m)
/// where k=topic_count, m=synonym_count per category — typically < 100μs.
fn compute_relevance(hints: &[&str]) -> BTreeMap<TraitDimension, f32> {
    let mut relevance = BTreeMap::new();

    // Initialize all to low baseline
    for &dim in TraitDimension::ALL.iter() {
        relevance.insert(dim, 0.2);
    }

    for &hint in hints {
        let lower = hint.to_ascii_lowercase();

        // Code / technical
        if contains_any(&lower, &["code", "debug", "review", "refactor", "implement", "test", "rust", "python", "typescript"]) {
            boost(&mut relevance, TraitDimension::TechnicalDepth, 0.95);
            boost(&mut relevance, TraitDimension::Directness, 0.8);
            boost(&mut relevance, TraitDimension::Confidence, 0.7);
            boost(&mut relevance, TraitDimension::Verbosity, 0.6);
        }

        // Data / analysis
        if contains_any(&lower, &["data", "analysis", "statistics", "chart", "metric", "dashboard", "csv"]) {
            boost(&mut relevance, TraitDimension::Analytical, 0.95);
            boost(&mut relevance, TraitDimension::TechnicalDepth, 0.8);
            boost(&mut relevance, TraitDimension::Verbosity, 0.7);
        }

        // Creative / writing
        if contains_any(&lower, &["write", "creative", "story", "poem", "essay", "blog", "copy", "slogan"]) {
            boost(&mut relevance, TraitDimension::Creativity, 0.95);
            boost(&mut relevance, TraitDimension::Humor, 0.7);
            boost(&mut relevance, TraitDimension::Boldness, 0.6);
        }

        // Support / emotional
        if contains_any(&lower, &["help", "support", "frustrated", "confused", "stuck", "problem", "issue"]) {
            boost(&mut relevance, TraitDimension::Pedagogy, 0.9);
            boost(&mut relevance, TraitDimension::Directness, 0.5); // Lower — be gentler
            boost(&mut relevance, TraitDimension::Humor, 0.4);
        }

        // Planning / strategy
        if contains_any(&lower, &["plan", "strategy", "roadmap", "architecture", "design", "approach"]) {
            boost(&mut relevance, TraitDimension::Initiative, 0.85);
            boost(&mut relevance, TraitDimension::Boldness, 0.75);
            boost(&mut relevance, TraitDimension::Collaboration, 0.7);
        }

        // Teaching / learning
        if contains_any(&lower, &["explain", "learn", "understand", "how does", "what is", "tutorial", "teach"]) {
            boost(&mut relevance, TraitDimension::Pedagogy, 0.95);
            boost(&mut relevance, TraitDimension::Verbosity, 0.8);
            boost(&mut relevance, TraitDimension::TechnicalDepth, 0.7);
        }
    }

    relevance
}

/// Boost a relevance score (keeps maximum).
fn boost(map: &mut BTreeMap<TraitDimension, f32>, dim: TraitDimension, score: f32) {
    let entry = map.entry(dim).or_insert(0.0);
    if score > *entry {
        *entry = score;
    }
}

/// Check if text contains any of the given needles (substring match).
fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| text.contains(n))
}

// ═══════════════════════════════════════════════════════════════════════════
// Persona Presets — common archetypes as starting points
// ═══════════════════════════════════════════════════════════════════════════

impl PersonaVector {
    /// The "principal engineer" archetype — deep, direct, opinionated.
    pub fn principal_engineer() -> Self {
        Self::from_traits(vec![
            (TraitDimension::TechnicalDepth, 0.95),
            (TraitDimension::Directness, 0.85),
            (TraitDimension::Confidence, 0.9),
            (TraitDimension::Verbosity, 0.3),
            (TraitDimension::Initiative, 0.8),
            (TraitDimension::Formality, 0.4),
        ])
    }

    /// The "friendly mentor" archetype — patient, encouraging, thorough.
    pub fn mentor() -> Self {
        Self::from_traits(vec![
            (TraitDimension::Pedagogy, 0.95),
            (TraitDimension::Verbosity, 0.75),
            (TraitDimension::Humor, 0.6),
            (TraitDimension::Directness, 0.4),
            (TraitDimension::Confidence, 0.7),
            (TraitDimension::Initiative, 0.7),
        ])
    }

    /// The "executive briefer" archetype — concise, decisive, high-level.
    pub fn executive() -> Self {
        Self::from_traits(vec![
            (TraitDimension::Verbosity, 0.1),
            (TraitDimension::Directness, 0.95),
            (TraitDimension::Confidence, 0.95),
            (TraitDimension::Formality, 0.8),
            (TraitDimension::TechnicalDepth, 0.2),
            (TraitDimension::Boldness, 0.9),
        ])
    }

    /// The "creative partner" archetype — inventive, playful, collaborative.
    pub fn creative() -> Self {
        Self::from_traits(vec![
            (TraitDimension::Creativity, 0.95),
            (TraitDimension::Humor, 0.7),
            (TraitDimension::Boldness, 0.8),
            (TraitDimension::Collaboration, 0.85),
            (TraitDimension::Formality, 0.15),
            (TraitDimension::Initiative, 0.8),
        ])
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Trait extraction from TOML agent config (backward compatibility)
// ═══════════════════════════════════════════════════════════════════════════

impl PersonaVector {
    /// Convert from the existing TOML `[traits]` format.
    ///
    /// Maps ClawDesk's string-based traits to vector coordinates.
    /// This preserves backward compatibility with all 202 existing agents.
    pub fn from_toml_traits(
        persona: &[String],
        methodology: &[String],
        output: &[String],
        constraints: &[String],
    ) -> Self {
        let mut traits = BTreeMap::new();

        for p in persona {
            match p.to_ascii_lowercase().as_str() {
                "concise" => { traits.insert(TraitDimension::Verbosity, 0.15); }
                "verbose" | "detailed" => { traits.insert(TraitDimension::Verbosity, 0.85); }
                "formal" => { traits.insert(TraitDimension::Formality, 0.85); }
                "casual" | "friendly" => { traits.insert(TraitDimension::Formality, 0.2); }
                "direct" => { traits.insert(TraitDimension::Directness, 0.85); }
                "diplomatic" => { traits.insert(TraitDimension::Directness, 0.2); }
                "playful" => { traits.insert(TraitDimension::Humor, 0.7); }
                _ => {}
            }
        }

        for m in methodology {
            match m.to_ascii_lowercase().as_str() {
                "evidence-based" | "data-driven" => { traits.insert(TraitDimension::Analytical, 0.9); }
                "systematic" | "methodical" => { traits.insert(TraitDimension::Creativity, 0.2); }
                "creative" | "innovative" => { traits.insert(TraitDimension::Creativity, 0.85); }
                _ => {}
            }
        }

        for o in output {
            match o.to_ascii_lowercase().as_str() {
                "structured-report" => {
                    traits.insert(TraitDimension::Formality, 0.7);
                    traits.insert(TraitDimension::Verbosity, 0.7);
                }
                "conversational" => {
                    traits.insert(TraitDimension::Formality, 0.2);
                }
                _ => {}
            }
        }

        let custom_rules: Vec<String> = constraints
            .iter()
            .map(|c| format!("Constraint: {c}"))
            .take(10)
            .collect();

        Self {
            traits,
            custom_rules,
            source_description: None,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_description_extracts_traits() {
        let p = PersonaVector::from_description(
            "Be direct, data-driven, concise. Don't sugarcoat. Never apologize for being thorough.",
        );
        assert!(p.get(TraitDimension::Directness) > 0.7);
        assert!(p.get(TraitDimension::Analytical) > 0.7);
        assert!(p.get(TraitDimension::Verbosity) < 0.3);
        // Custom rule extracted
        assert!(p.custom_rules.iter().any(|r| r.contains("Never apologize")));
    }

    #[test]
    fn neutral_traits_not_stored() {
        let p = PersonaVector::from_description("Be direct.");
        // Only directness should be set, everything else is neutral
        assert!(p.active_dimensions() <= 2); // direct might also trigger confidence
    }

    #[test]
    fn compose_override_takes_precedence() {
        let base = PersonaVector::from_traits(vec![
            (TraitDimension::Formality, 0.8),
            (TraitDimension::Humor, 0.3),
        ]);
        let overlay = PersonaVector::from_traits(vec![
            (TraitDimension::Formality, 0.2), // Override
        ]);
        let merged = base.compose(&overlay);
        assert!((merged.get(TraitDimension::Formality) - 0.2).abs() < 0.01);
        assert!((merged.get(TraitDimension::Humor) - 0.3).abs() < 0.01); // Preserved
    }

    #[test]
    fn blend_interpolates() {
        let a = PersonaVector::from_traits(vec![(TraitDimension::Formality, 0.0)]);
        let b = PersonaVector::from_traits(vec![(TraitDimension::Formality, 1.0)]);
        let mid = a.blend(&b, 0.5);
        assert!((mid.get(TraitDimension::Formality) - 0.5).abs() < 0.01);
    }

    #[test]
    fn distance_same_is_zero() {
        let p = PersonaVector::principal_engineer();
        assert!(p.distance(&p) < 0.001);
    }

    #[test]
    fn distance_opposite_is_large() {
        let a = PersonaVector::from_traits(vec![(TraitDimension::Formality, 0.0)]);
        let b = PersonaVector::from_traits(vec![(TraitDimension::Formality, 1.0)]);
        assert!(a.distance(&b) > 0.9);
    }

    #[test]
    fn projection_filters_irrelevant_traits() {
        let p = PersonaVector::from_traits(vec![
            (TraitDimension::TechnicalDepth, 0.95),
            (TraitDimension::Humor, 0.8),
            (TraitDimension::Analytical, 0.9),
        ]);
        let projected = p.project(&["code review"]);
        let dims: Vec<_> = projected.active_traits.iter().map(|(d, _)| *d).collect();
        // TechnicalDepth should be activated (relevant to code)
        assert!(dims.contains(&TraitDimension::TechnicalDepth));
        // Humor is less relevant to code review — may or may not appear
    }

    #[test]
    fn projection_token_efficiency() {
        let full = PersonaVector::principal_engineer();
        let full_tokens = full.to_prompt_fragment().len() / 4;

        let projected = full.project(&["debug"]);
        let proj_tokens = projected.estimated_tokens();

        // Projected should use significantly fewer tokens
        assert!(proj_tokens < full_tokens);
    }

    #[test]
    fn prompt_fragment_not_empty_for_non_neutral() {
        let p = PersonaVector::from_description("Be direct and concise.");
        let fragment = p.to_prompt_fragment();
        assert!(!fragment.is_empty());
        // v2: three-section format
        assert!(fragment.contains("[Style]") || fragment.contains("[Stance]") || fragment.contains("[Rules]"));
    }

    #[test]
    fn toml_backward_compat() {
        let p = PersonaVector::from_toml_traits(
            &["concise".into(), "formal".into()],
            &["evidence-based".into()],
            &["structured-report".into()],
            &["no-financial-advice".into()],
        );
        assert!(p.get(TraitDimension::Verbosity) < 0.3);
        assert!(p.get(TraitDimension::Formality) > 0.7);
        assert!(p.get(TraitDimension::Analytical) > 0.7);
        assert!(p.custom_rules.iter().any(|r| r.contains("no-financial-advice")));
    }

    #[test]
    fn preset_principal_engineer() {
        let p = PersonaVector::principal_engineer();
        assert!(p.get(TraitDimension::TechnicalDepth) > 0.9);
        assert!(p.get(TraitDimension::Directness) > 0.8);
        assert!(p.get(TraitDimension::Verbosity) < 0.4);
    }

    #[test]
    fn presets_are_distinct() {
        let pe = PersonaVector::principal_engineer();
        let mentor = PersonaVector::mentor();
        let exec = PersonaVector::executive();
        let creative = PersonaVector::creative();

        // All should be meaningfully different
        assert!(pe.distance(&mentor) > 0.5);
        assert!(pe.distance(&exec) > 0.3);
        assert!(mentor.distance(&creative) > 0.5);
    }

    #[test]
    fn custom_rules_capped() {
        let mut p = PersonaVector::default();
        for i in 0..20 {
            p.add_rule(format!("Rule {i}"));
        }
        assert_eq!(p.custom_rules.len(), 10); // Capped at 10
    }

    // ═══════════════════════════════════════════════════════════════════════
    // v2 tests — three-lane, coarse values, smoothing, synonym groups
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn intensity_quantization_roundtrip() {
        // Coarse values should quantize cleanly
        assert_eq!(Intensity::from_f32(0.1), Intensity::StrongLow);
        assert_eq!(Intensity::from_f32(0.3), Intensity::Low);
        assert_eq!(Intensity::from_f32(0.5), Intensity::Neutral);
        assert_eq!(Intensity::from_f32(0.7), Intensity::High);
        assert_eq!(Intensity::from_f32(0.9), Intensity::StrongHigh);
    }

    #[test]
    fn trait_lane_classification() {
        // Style traits
        assert_eq!(TraitDimension::Formality.lane(), TraitLane::Style);
        assert_eq!(TraitDimension::Verbosity.lane(), TraitLane::Style);
        assert_eq!(TraitDimension::Directness.lane(), TraitLane::Style);
        assert_eq!(TraitDimension::Humor.lane(), TraitLane::Style);
        // Stance traits
        assert_eq!(TraitDimension::Analytical.lane(), TraitLane::Stance);
        assert_eq!(TraitDimension::TechnicalDepth.lane(), TraitLane::Stance);
        assert_eq!(TraitDimension::Pedagogy.lane(), TraitLane::Stance);
    }

    #[test]
    fn synonym_normalization_across_paraphrases() {
        // These should all produce similar directness values:
        let p1 = PersonaVector::from_description("Be direct.");
        let p2 = PersonaVector::from_description("Be blunt.");
        let p3 = PersonaVector::from_description("No sugarcoat.");
        let p4 = PersonaVector::from_description("Tell it like it is.");
        let p5 = PersonaVector::from_description("No bs, cut the bs.");

        // All should map to high directness (StrongHigh = 0.9)
        for p in [&p1, &p2, &p3, &p4, &p5] {
            assert!(p.get(TraitDimension::Directness) > 0.7,
                "Failed for: {:?}", p.source_description);
        }
    }

    #[test]
    fn executive_summary_synonym() {
        // "executive summary style" and "concise" should both reduce verbosity
        let p1 = PersonaVector::from_description("executive summary style");
        let p2 = PersonaVector::from_description("concise");
        let p3 = PersonaVector::from_description("to the point, no fluff");

        for p in [&p1, &p2, &p3] {
            assert!(p.get(TraitDimension::Verbosity) < 0.3,
                "Failed to detect low verbosity in: {:?}", p.source_description);
        }
    }

    #[test]
    fn hard_rules_survive_projection() {
        let mut p = PersonaVector::from_description(
            "Be formal. Never apologize for being thorough."
        );
        p.add_rule("Always cite sources");

        // Project onto a context that suppresses many traits
        let projected = p.project(&["quick question"]);

        // Hard rules MUST survive regardless of projection
        assert!(projected.custom_rules.iter().any(|r| r.contains("Never apologize")));
        assert!(projected.custom_rules.iter().any(|r| r.contains("Always cite")));
    }

    #[test]
    fn three_section_prompt_format() {
        let p = PersonaVector::from_traits(vec![
            (TraitDimension::Directness, 0.9),   // Style
            (TraitDimension::Analytical, 0.9),    // Stance
        ]);
        let mut pv = p;
        pv.add_rule("Never apologize");

        let fragment = pv.to_prompt_fragment();
        assert!(fragment.contains("[Style]"), "Missing Style section");
        assert!(fragment.contains("[Stance]"), "Missing Stance section");
        assert!(fragment.contains("[Rules]"), "Missing Rules section");
        assert!(fragment.contains("Never apologize"), "Rule not in output");
    }

    #[test]
    fn channel_fit_dampens_projection() {
        let p = PersonaVector::principal_engineer();

        // Desktop (full fit)
        let desktop = p.project_with_fit(&["code review"], 1.0);
        // IRC (low fit — tiny messages)
        let irc = p.project_with_fit(&["code review"], 0.3);

        // IRC should have fewer active traits (more suppressed)
        assert!(irc.active_traits.len() <= desktop.active_traits.len());
    }

    #[test]
    fn stance_traits_survive_better_than_style() {
        // Create a persona with one style trait and one stance trait at same intensity
        let p = PersonaVector::from_traits(vec![
            (TraitDimension::Humor, 0.8),          // Style
            (TraitDimension::TechnicalDepth, 0.8),  // Stance
        ]);

        // Project onto a weakly relevant context
        let projected = p.project_with_fit(&["general chat"], 0.5);
        let dims: Vec<_> = projected.active_traits.iter().map(|(d, _)| *d).collect();

        // Stance traits have lower threshold (0.15 vs 0.25), so TechnicalDepth
        // should survive more often than Humor at the same base_strength
        if dims.len() == 1 {
            assert!(dims.contains(&TraitDimension::TechnicalDepth));
        }
    }

    #[test]
    fn smoother_prevents_jitter() {
        let mut smoother = PersonaSmoother::new();

        // Turn 1: code review → high tech_depth
        let p = PersonaVector::principal_engineer();
        let turn1 = p.project(&["code review"]);
        let smoothed1 = smoother.smooth(&turn1);

        // Turn 2: same context → values should be stable
        let turn2 = p.project(&["code review"]);
        let smoothed2 = smoother.smooth(&turn2);

        // Values should converge (not jitter)
        for &(dim, val1) in &smoothed1.active_traits {
            if let Some(&(_, val2)) = smoothed2.active_traits.iter().find(|(d, _)| *d == dim) {
                let drift = (val1 - val2).abs();
                assert!(drift < 0.3, "Style jitter too high for {:?}: {drift}", dim);
            }
        }
    }

    #[test]
    fn smoother_cold_start_passthrough() {
        let mut smoother = PersonaSmoother::new();
        let p = PersonaVector::from_description("Be direct and concise.");
        let projected = p.project(&["debug"]);
        let smoothed = smoother.smooth(&projected);

        // First observation should pass through unchanged
        assert_eq!(smoothed.active_traits.len(), projected.active_traits.len());
    }

    #[test]
    fn smoother_reset_clears_state() {
        let mut smoother = PersonaSmoother::new();
        let p = PersonaVector::principal_engineer();

        smoother.smooth(&p.project(&["code"]));
        smoother.smooth(&p.project(&["data"]));

        smoother.reset();
        assert_eq!(smoother.observations, 0);
    }

    #[test]
    fn negative_trait_survives_when_relevant() {
        // User explicitly set low pedagogy ("skip explanations")
        let p = PersonaVector::from_traits(vec![
            (TraitDimension::Pedagogy, 0.1),       // Strongly low
            (TraitDimension::TechnicalDepth, 0.9),  // Strongly high
        ]);

        // Context: someone asking to learn → Pedagogy is highly relevant
        let projected = p.project(&["explain how this works"]);
        let dims: Vec<_> = projected.active_traits.iter().map(|(d, _)| *d).collect();

        // Low pedagogy SHOULD survive because it's behaviorally important
        // when the context is about teaching/explaining.
        // The activation function treats |0.1 - 0.5| = 0.4 as strong,
        // and "explain" → Pedagogy relevance = 0.95.
        // activation = 0.8 × 0.95 × 1.0 = 0.76, well above threshold.
        assert!(
            dims.contains(&TraitDimension::Pedagogy),
            "Low pedagogy should survive in teaching context"
        );

        // And the description should be a negative instruction
        let fragment = projected.to_prompt_fragment();
        assert!(
            fragment.contains("skip explanations") || fragment.contains("answer directly"),
            "Low pedagogy should emit negative instruction, got: {fragment}"
        );
    }

    #[test]
    fn describe_no_semantic_leaps() {
        // Confidence should say "confident" not "opinionated"
        let desc = TraitDimension::Confidence.describe(0.9);
        assert!(desc.contains("confidence") || desc.contains("hedge"),
            "Confidence description should not leap to 'opinionated': {desc}");

        // Directness should say "direct" not "unfiltered"
        let desc = TraitDimension::Directness.describe(0.9);
        assert!(desc.contains("direct"),
            "Directness description should contain 'direct': {desc}");
    }
}
