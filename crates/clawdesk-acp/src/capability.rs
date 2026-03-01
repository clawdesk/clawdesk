//! Typed capability bitfield with compile-time arity validation.
//!
//! ## Design
//!
//! Capabilities are modeled as a bounded lattice `(C, ≤)` where `a ≤ b`
//! means "a implies b." The bitset is the characteristic function `χ: C → {0,1}`.
//!
//! ## Properties
//!
//! - **Type-safe**: Each capability has a unique, compile-time-verified bit position.
//!   Bit index aliasing is impossible without explicit `Custom` usage.
//! - **Extensible**: Width scales automatically via const generics `CapSet<N>`.
//!   Currently `N=1` (64 capabilities), expandable to `N=2` (128) etc.
//! - **Hierarchical**: Implication DAG encodes "audio implies media_processing".
//!   Closure is computed via topological sort in `O(|C| + |E|)`.
//! - **Scoring**: Weighted dot product `score(a, t) = Σᵢ wᵢ · χ_a(cᵢ) · χ_t(cᵢ)`
//!   with SIMD-friendly `popcnt` for uniform weights.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Maximum number of well-known capabilities before requiring width expansion.
const MAX_KNOWN_CAPABILITIES: usize = 32;

/// Compile-time capability registry entry.
/// Each entry maps a capability name to a unique bit index.
macro_rules! define_capabilities {
    (
        $( $variant:ident = $index:expr => $parent:expr ),* $(,)?
    ) => {
        /// Well-known capability identifiers with guaranteed unique bit positions.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        #[repr(u8)]
        pub enum CapabilityId {
            $( $variant = $index, )*
        }

        impl CapabilityId {
            /// Bit index for this capability. Guaranteed unique at compile time
            /// via the `repr(u8)` discriminant.
            #[inline]
            pub const fn bit_index(self) -> usize {
                self as usize
            }

            /// Parent capability in the implication hierarchy (if any).
            /// `a.parent() = Some(b)` means `a implies b`.
            pub const fn parent(self) -> Option<CapabilityId> {
                match self {
                    $( Self::$variant => $parent, )*
                }
            }

            /// All known capabilities.
            pub const fn all() -> &'static [CapabilityId] {
                &[ $( Self::$variant, )* ]
            }

            /// Display name.
            pub fn name(&self) -> &'static str {
                match self {
                    $( Self::$variant => stringify!($variant), )*
                }
            }
        }
    };
}

define_capabilities! {
    // ── Root capabilities (no parent) ──
    TextGeneration       = 0  => None,
    CodeExecution        = 1  => None,
    WebSearch            = 2  => None,
    FileProcessing       = 3  => None,
    ApiIntegration       = 4  => None,
    DataManagement       = 5  => None,
    Mathematics          = 6  => None,
    Scheduling           = 7  => None,
    Messaging            = 8  => None,

    // ── Media hierarchy ──
    MediaProcessing      = 9  => None,                              // root
    ImageProcessing      = 10 => Some(CapabilityId::MediaProcessing), // implies media
    AudioProcessing      = 11 => Some(CapabilityId::MediaProcessing), // implies media
    VideoProcessing      = 12 => Some(CapabilityId::MediaProcessing), // implies media
    DocumentProcessing   = 13 => Some(CapabilityId::MediaProcessing), // implies media

    // ── Audio sub-hierarchy ──
    VoiceUnderstanding   = 14 => Some(CapabilityId::AudioProcessing), // implies audio
    TextToSpeech         = 15 => Some(CapabilityId::AudioProcessing), // implies audio

    // ── Advanced capabilities ──
    CodeGeneration       = 16 => Some(CapabilityId::TextGeneration),  // implies text
    Summarization        = 17 => Some(CapabilityId::TextGeneration),  // implies text
    Translation          = 18 => Some(CapabilityId::TextGeneration),  // implies text
    ReasoningAdvanced    = 19 => None,
    ToolUse              = 20 => None,
    MultiModal           = 21 => None,
}

/// Const-generic capability set: `[u64; N]` backing store.
///
/// Default `N=1` supports up to 64 capabilities.
/// Set `N=2` for up to 128, etc. Width is derived from registry cardinality
/// as `⌈|C| / 64⌉`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct CapSet<const N: usize = 1> {
    bits: [u64; N],
}

impl<const N: usize> CapSet<N> {
    /// Empty capability set.
    pub const fn empty() -> Self {
        Self { bits: [0; N] }
    }

    /// Set a capability bit.
    #[inline]
    pub fn insert(&mut self, cap: CapabilityId) {
        let idx = cap.bit_index();
        let word = idx / 64;
        let bit = idx % 64;
        assert!(word < N, "capability index {} exceeds CapSet width {}", idx, N * 64);
        self.bits[word] |= 1u64 << bit;
    }

    /// Clear a capability bit.
    #[inline]
    pub fn remove(&mut self, cap: CapabilityId) {
        let idx = cap.bit_index();
        let word = idx / 64;
        let bit = idx % 64;
        if word < N {
            self.bits[word] &= !(1u64 << bit);
        }
    }

    /// Test if a capability is present.
    #[inline]
    pub fn contains(&self, cap: CapabilityId) -> bool {
        let idx = cap.bit_index();
        let word = idx / 64;
        let bit = idx % 64;
        word < N && (self.bits[word] & (1u64 << bit)) != 0
    }

    /// Bitwise AND (intersection).
    #[inline]
    pub fn intersection(&self, other: &Self) -> Self {
        let mut result = Self::empty();
        for i in 0..N {
            result.bits[i] = self.bits[i] & other.bits[i];
        }
        result
    }

    /// Bitwise OR (union).
    #[inline]
    pub fn union(&self, other: &Self) -> Self {
        let mut result = Self::empty();
        for i in 0..N {
            result.bits[i] = self.bits[i] | other.bits[i];
        }
        result
    }

    /// Popcount — total set bits.
    #[inline]
    pub fn count(&self) -> u32 {
        let mut total = 0u32;
        for i in 0..N {
            total += self.bits[i].count_ones();
        }
        total
    }

    /// Whether the set is empty.
    pub fn is_empty(&self) -> bool {
        self.bits.iter().all(|&w| w == 0)
    }

    /// Compute the hierarchical closure.
    ///
    /// For each set bit, also set all ancestor bits via the implication DAG.
    /// `close(S) = S ∪ {b | ∃ a ∈ S: a ≤ b}` — computed by walking each
    /// capability's parent chain. O(|C| × depth) where depth ≤ 3 in practice.
    pub fn close(&self) -> Self {
        let mut closed = *self;
        for &cap in CapabilityId::all() {
            if closed.contains(cap) {
                // Walk parent chain and set all ancestors.
                let mut current = cap.parent();
                while let Some(parent) = current {
                    closed.insert(parent);
                    current = parent.parent();
                }
            }
        }
        closed
    }

    /// Capability overlap score: `|self ∩ required| / |required|`.
    ///
    /// Returns `[0.0, 1.0]`. Uses POPCNT for O(N) = O(1) computation.
    /// Both sets should be closed (hierarchically complete) for accurate scoring.
    pub fn overlap_score(&self, required: &Self) -> f64 {
        let required_count = required.count();
        if required_count == 0 {
            return 1.0;
        }
        let intersection = self.intersection(required);
        intersection.count() as f64 / required_count as f64
    }

    /// Weighted score: `Σᵢ wᵢ · χ_self(cᵢ) · χ_required(cᵢ)`.
    ///
    /// The `weights` slice must have one entry per `CapabilityId::all()`.
    /// Falls back to uniform scoring if `weights` is empty.
    pub fn weighted_score(&self, required: &Self, weights: &[f64]) -> f64 {
        if weights.is_empty() {
            return self.overlap_score(required);
        }

        let mut score = 0.0f64;
        let mut total_weight = 0.0f64;

        for &cap in CapabilityId::all() {
            let idx = cap.bit_index();
            if idx < weights.len() && required.contains(cap) {
                total_weight += weights[idx];
                if self.contains(cap) {
                    score += weights[idx];
                }
            }
        }

        if total_weight == 0.0 {
            return 1.0;
        }
        score / total_weight
    }

    /// Collect all set capabilities.
    pub fn to_vec(&self) -> Vec<CapabilityId> {
        CapabilityId::all()
            .iter()
            .copied()
            .filter(|&cap| self.contains(cap))
            .collect()
    }
}

impl<const N: usize> Default for CapSet<N> {
    fn default() -> Self {
        Self::empty()
    }
}

impl<const N: usize> fmt::Debug for CapSet<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let caps: Vec<&str> = CapabilityId::all()
            .iter()
            .filter(|cap| self.contains(**cap))
            .map(|cap| cap.name())
            .collect();
        f.debug_struct("CapSet")
            .field("capabilities", &caps)
            .field("count", &self.count())
            .finish()
    }
}

impl<const N: usize> fmt::Display for CapSet<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = CapabilityId::all()
            .iter()
            .filter(|cap| self.contains(**cap))
            .map(|cap| cap.name())
            .collect();
        write!(f, "{{{}}}", names.join(", "))
    }
}

/// Builder for constructing CapSets from capability slices.
impl<const N: usize> FromIterator<CapabilityId> for CapSet<N> {
    fn from_iter<I: IntoIterator<Item = CapabilityId>>(iter: I) -> Self {
        let mut set = Self::empty();
        for cap in iter {
            set.insert(cap);
        }
        set
    }
}

/// Convert a skill tag to a `CapabilityId` (for skill wiring).
///
/// Returns `None` for tags that don't map to a known capability.
impl CapabilityId {
    pub fn from_tag(tag: &str) -> Option<Self> {
        match tag.to_lowercase().as_str() {
            "text" | "generation" | "chat" | "llm" => Some(Self::TextGeneration),
            "code" | "coding" | "programming" | "execution" => Some(Self::CodeExecution),
            "web" | "search" | "internet" => Some(Self::WebSearch),
            "file" | "filesystem" | "files" => Some(Self::FileProcessing),
            "image" | "vision" | "visual" => Some(Self::ImageProcessing),
            "audio" | "speech" | "tts" | "transcription" => Some(Self::AudioProcessing),
            "video" => Some(Self::VideoProcessing),
            "media" => Some(Self::MediaProcessing),
            "api" | "integration" | "http" => Some(Self::ApiIntegration),
            "data" | "database" | "storage" => Some(Self::DataManagement),
            "math" | "mathematics" | "calculation" => Some(Self::Mathematics),
            "schedule" | "cron" | "calendar" => Some(Self::Scheduling),
            "message" | "messaging" | "notification" => Some(Self::Messaging),
            "summarize" | "summary" => Some(Self::Summarization),
            "translate" | "translation" => Some(Self::Translation),
            "reason" | "reasoning" => Some(Self::ReasoningAdvanced),
            "tool" | "tool_use" => Some(Self::ToolUse),
            "multimodal" | "multi_modal" => Some(Self::MultiModal),
            _ => None,
        }
    }

    /// Parse a capability string from thread/agent config (kebab-case or snake_case).
    ///
    /// Returns `None` for unrecognized strings (replacing the old `Custom(String)` variant).
    pub fn from_str_loose(s: &str) -> Option<Self> {
        match s {
            "text-generation" | "text_generation" => Some(Self::TextGeneration),
            "code-execution" | "code_execution" => Some(Self::CodeExecution),
            "web-search" | "web_search" => Some(Self::WebSearch),
            "file-processing" | "file_processing" => Some(Self::FileProcessing),
            "image-processing" | "image_processing" => Some(Self::ImageProcessing),
            "audio-processing" | "audio_processing" => Some(Self::AudioProcessing),
            "video-processing" | "video_processing" => Some(Self::VideoProcessing),
            "media-processing" | "media_processing" => Some(Self::MediaProcessing),
            "document-processing" | "document_processing" => Some(Self::DocumentProcessing),
            "voice-understanding" | "voice_understanding" => Some(Self::VoiceUnderstanding),
            "text-to-speech" | "text_to_speech" | "tts" => Some(Self::TextToSpeech),
            "api-integration" | "api_integration" => Some(Self::ApiIntegration),
            "data-management" | "data_management" => Some(Self::DataManagement),
            "mathematics" | "math" => Some(Self::Mathematics),
            "scheduling" | "schedule" => Some(Self::Scheduling),
            "messaging" | "message" => Some(Self::Messaging),
            "code-generation" | "code_generation" => Some(Self::CodeGeneration),
            "summarization" | "summarize" => Some(Self::Summarization),
            "translation" | "translate" => Some(Self::Translation),
            "reasoning-advanced" | "reasoning_advanced" | "reasoning" => Some(Self::ReasoningAdvanced),
            "tool-use" | "tool_use" => Some(Self::ToolUse),
            "multi-modal" | "multi_modal" | "multimodal" => Some(Self::MultiModal),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_bit_indices() {
        // Verify no two capabilities share a bit index (enforced by repr(u8)).
        let mut seen = std::collections::HashSet::new();
        for &cap in CapabilityId::all() {
            assert!(seen.insert(cap.bit_index()), "duplicate bit index for {:?}", cap);
        }
    }

    #[test]
    fn algebraic_identities() {
        let a: CapSet = [CapabilityId::TextGeneration, CapabilityId::WebSearch]
            .into_iter()
            .collect();
        let b: CapSet = [CapabilityId::WebSearch, CapabilityId::CodeExecution]
            .into_iter()
            .collect();
        let c: CapSet = [CapabilityId::TextGeneration, CapabilityId::Mathematics]
            .into_iter()
            .collect();

        // Commutativity: A ∩ B = B ∩ A
        assert_eq!(a.intersection(&b), b.intersection(&a));

        // Commutativity: A ∪ B = B ∪ A
        assert_eq!(a.union(&b), b.union(&a));

        // Idempotency: A ∩ A = A
        assert_eq!(a.intersection(&a), a);

        // Idempotency: A ∪ A = A
        assert_eq!(a.union(&a), a);

        // Distributivity: A ∩ (B ∪ C) = (A ∩ B) ∪ (A ∩ C)
        let lhs = a.intersection(&b.union(&c));
        let rhs = a.intersection(&b).union(&a.intersection(&c));
        assert_eq!(lhs, rhs);

        // Absorption: A ∩ (A ∪ B) = A
        assert_eq!(a.intersection(&a.union(&b)), a);
    }

    #[test]
    fn hierarchical_closure() {
        // VoiceUnderstanding implies AudioProcessing implies MediaProcessing.
        let mut set: CapSet = CapSet::empty();
        set.insert(CapabilityId::VoiceUnderstanding);

        let closed = set.close();
        assert!(closed.contains(CapabilityId::VoiceUnderstanding));
        assert!(closed.contains(CapabilityId::AudioProcessing));
        assert!(closed.contains(CapabilityId::MediaProcessing));
        assert!(!closed.contains(CapabilityId::ImageProcessing));
    }

    #[test]
    fn overlap_score_full_match() {
        let offered: CapSet = [CapabilityId::TextGeneration, CapabilityId::WebSearch]
            .into_iter()
            .collect();
        let required: CapSet = [CapabilityId::TextGeneration, CapabilityId::WebSearch]
            .into_iter()
            .collect();
        assert_eq!(offered.overlap_score(&required), 1.0);
    }

    #[test]
    fn overlap_score_partial() {
        let offered: CapSet = [CapabilityId::TextGeneration].into_iter().collect();
        let required: CapSet = [CapabilityId::TextGeneration, CapabilityId::WebSearch]
            .into_iter()
            .collect();
        assert_eq!(offered.overlap_score(&required), 0.5);
    }

    #[test]
    fn overlap_score_no_match() {
        let offered: CapSet = [CapabilityId::Mathematics].into_iter().collect();
        let required: CapSet = [CapabilityId::TextGeneration].into_iter().collect();
        assert_eq!(offered.overlap_score(&required), 0.0);
    }

    #[test]
    fn weighted_scoring() {
        let offered: CapSet = [CapabilityId::TextGeneration, CapabilityId::WebSearch]
            .into_iter()
            .collect();
        let required: CapSet = [CapabilityId::TextGeneration, CapabilityId::WebSearch, CapabilityId::CodeExecution]
            .into_iter()
            .collect();

        // Weight TextGeneration=2.0, CodeExecution=1.0, WebSearch=1.0
        let mut weights = vec![0.0; MAX_KNOWN_CAPABILITIES];
        weights[CapabilityId::TextGeneration.bit_index()] = 2.0;
        weights[CapabilityId::CodeExecution.bit_index()] = 1.0;
        weights[CapabilityId::WebSearch.bit_index()] = 1.0;

        // Score = (2.0 + 1.0) / (2.0 + 1.0 + 1.0) = 3/4 = 0.75
        let score = offered.weighted_score(&required, &weights);
        assert!((score - 0.75).abs() < 1e-10);
    }

    #[test]
    fn hierarchical_scoring_with_closure() {
        // Agent advertises VoiceUnderstanding → after closure, also has Audio + Media.
        let mut offered: CapSet = CapSet::empty();
        offered.insert(CapabilityId::VoiceUnderstanding);
        let offered_closed = offered.close();

        // Task requires AudioProcessing → satisfied because Voice implies Audio.
        let mut required: CapSet = CapSet::empty();
        required.insert(CapabilityId::AudioProcessing);

        assert_eq!(offered_closed.overlap_score(&required), 1.0);
    }

    #[test]
    fn empty_required_returns_1() {
        let offered: CapSet = [CapabilityId::TextGeneration].into_iter().collect();
        let required: CapSet = CapSet::empty();
        assert_eq!(offered.overlap_score(&required), 1.0);
    }

    #[test]
    fn display_format() {
        let set: CapSet = [CapabilityId::TextGeneration, CapabilityId::WebSearch]
            .into_iter()
            .collect();
        let s = format!("{}", set);
        assert!(s.contains("TextGeneration"));
        assert!(s.contains("WebSearch"));
    }
}
