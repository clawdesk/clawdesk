//! Information Flow Taint Tracking (IFT).
//!
//! Labels data with provenance tags (`TaintLabel`) and tracks them through
//! the system. `TaintSink` validates that tainted data does not flow into
//! sensitive sinks without explicit declassification.
//!
//! ## Lattice
//!
//! ```text
//! Untainted < UserInput < ToolOutput < ExternalContent < Secrets
//! ```
//!
//! Labels form a join-semilattice: merging two labels produces the higher one.
//! Tainted values carry a set of labels (union on merge).
//!
//! ## Usage
//!
//! ```ignore
//! let value = TaintedValue::new("user input".to_string(), TaintLabel::UserInput);
//! assert!(value.has_label(TaintLabel::UserInput));
//! ```

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;

/// Taint provenance label — ordered by sensitivity (lower = less sensitive).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum TaintLabel {
    /// Clean data with no external provenance.
    Untainted,
    /// Data originating from user input.
    UserInput,
    /// Data returned by tool execution.
    ToolOutput,
    /// Data fetched from external sources (web, API, etc.).
    ExternalContent,
    /// Sensitive data (API keys, credentials, PII).
    Secrets,
}

impl fmt::Display for TaintLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TaintLabel::Untainted => write!(f, "untainted"),
            TaintLabel::UserInput => write!(f, "user_input"),
            TaintLabel::ToolOutput => write!(f, "tool_output"),
            TaintLabel::ExternalContent => write!(f, "external"),
            TaintLabel::Secrets => write!(f, "secrets"),
        }
    }
}

/// A value annotated with taint labels tracking its provenance.
///
/// Labels are stored in a `BTreeSet` for deterministic ordering.
/// The `max_label` is cached for O(1) sensitivity checks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaintedValue<T: Clone> {
    /// The underlying value.
    pub value: T,
    /// Set of taint labels applied to this value.
    pub labels: BTreeSet<TaintLabel>,
}

impl<T: Clone> TaintedValue<T> {
    /// Create a new tainted value with a single label.
    pub fn new(value: T, label: TaintLabel) -> Self {
        let mut labels = BTreeSet::new();
        labels.insert(label);
        Self { value, labels }
    }

    /// Create an untainted value.
    pub fn untainted(value: T) -> Self {
        let mut labels = BTreeSet::new();
        labels.insert(TaintLabel::Untainted);
        Self { value, labels }
    }

    /// Add a taint label.
    pub fn taint(&mut self, label: TaintLabel) {
        self.labels.insert(label);
    }

    /// Check if a specific label is present.
    pub fn has_label(&self, label: TaintLabel) -> bool {
        self.labels.contains(&label)
    }

    /// Get the maximum (most sensitive) label.
    pub fn max_label(&self) -> TaintLabel {
        self.labels.iter().copied().max().unwrap_or(TaintLabel::Untainted)
    }

    /// Check if the value is tainted (has any label above Untainted).
    pub fn is_tainted(&self) -> bool {
        self.max_label() > TaintLabel::Untainted
    }

    /// Merge labels from another tainted value (union of label sets).
    pub fn merge_labels<U: Clone>(&mut self, other: &TaintedValue<U>) {
        self.labels.extend(other.labels.iter().copied());
    }

    /// Declassify: remove a specific label (explicit trust decision).
    pub fn declassify(&mut self, label: TaintLabel) {
        self.labels.remove(&label);
        if self.labels.is_empty() {
            self.labels.insert(TaintLabel::Untainted);
        }
    }

    /// Map the inner value while preserving taint labels.
    pub fn map<U: Clone, F: FnOnce(T) -> U>(self, f: F) -> TaintedValue<U> {
        TaintedValue {
            value: f(self.value),
            labels: self.labels,
        }
    }
}

/// Taint sink — validates data before it flows into sensitive operations.
///
/// Uses Aho-Corasick for efficient multi-pattern matching against known
/// sensitive patterns (API keys, bearer tokens, etc.).
pub struct TaintSink {
    /// Patterns that indicate sensitive content.
    sensitive_patterns: Vec<String>,
    /// Compiled Aho-Corasick automaton for pattern matching.
    automaton: Option<aho_corasick::AhoCorasick>,
    /// Maximum taint level allowed to pass through this sink.
    max_allowed: TaintLabel,
}

impl TaintSink {
    /// Create a new sink that blocks data above the given taint level.
    pub fn new(max_allowed: TaintLabel) -> Self {
        Self {
            sensitive_patterns: Vec::new(),
            automaton: None,
            max_allowed,
        }
    }

    /// Add patterns that indicate sensitive content.
    /// Rebuilds the Aho-Corasick automaton.
    pub fn with_patterns(mut self, patterns: Vec<String>) -> Self {
        if !patterns.is_empty() {
            self.automaton = aho_corasick::AhoCorasick::builder()
                .ascii_case_insensitive(true)
                .build(&patterns)
                .ok();
        }
        self.sensitive_patterns = patterns;
        self
    }

    /// Create a sink with default sensitive patterns for API key detection.
    pub fn api_key_sink() -> Self {
        Self::new(TaintLabel::ToolOutput).with_patterns(vec![
            "sk-".to_string(),
            "Bearer ".to_string(),
            "api_key".to_string(),
            "apikey".to_string(),
            "secret_key".to_string(),
            "ANTHROPIC_API_KEY".to_string(),
            "OPENAI_API_KEY".to_string(),
            "password".to_string(),
            "access_token".to_string(),
        ])
    }

    /// Create a sink for shell command execution.
    ///
    /// Blocks commands that contain embedded API keys, bearer tokens, or
    /// other credentials that could be exfiltrated via `curl`, `wget`, etc.
    /// Uses `TaintLabel::ToolOutput` as the max allowed level so normal
    /// LLM-generated commands pass, but pattern matching catches embedded
    /// secrets regardless of taint level.
    pub fn shell_exec_sink() -> Self {
        Self::new(TaintLabel::ToolOutput).with_patterns(vec![
            "sk-".to_string(),
            "Bearer ".to_string(),
            "api_key=".to_string(),
            "apikey=".to_string(),
            "ANTHROPIC_API_KEY=".to_string(),
            "OPENAI_API_KEY=".to_string(),
            "GOOGLE_API_KEY=".to_string(),
            "AZURE_OPENAI_API_KEY=".to_string(),
            "secret_key=".to_string(),
            "access_token=".to_string(),
            "Authorization:".to_string(),
            "Authorization: Bearer".to_string(),
        ])
    }

    /// Validate a tainted value against this sink.
    ///
    /// Returns `Ok(())` if the value is allowed, or an error describing
    /// why the value was rejected.
    pub fn validate<T: Clone + AsRef<str>>(&self, value: &TaintedValue<T>) -> Result<(), TaintViolation> {
        let max = value.max_label();
        if max > self.max_allowed {
            return Err(TaintViolation {
                label: max,
                max_allowed: self.max_allowed,
                reason: format!(
                    "taint level {:?} exceeds maximum allowed {:?}",
                    max, self.max_allowed
                ),
                pattern_match: None,
            });
        }

        // Check for sensitive patterns in the value content
        if let Some(ref ac) = self.automaton {
            let text = value.value.as_ref();
            if let Some(mat) = ac.find(text) {
                let matched = &text[mat.start()..mat.end()];
                return Err(TaintViolation {
                    label: TaintLabel::Secrets,
                    max_allowed: self.max_allowed,
                    reason: "sensitive pattern detected in value".to_string(),
                    pattern_match: Some(matched.to_string()),
                });
            }
        }

        Ok(())
    }
}

/// Describes a taint policy violation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaintViolation {
    /// The taint label that caused the violation.
    pub label: TaintLabel,
    /// The maximum allowed taint level for the sink.
    pub max_allowed: TaintLabel,
    /// Human-readable reason for the violation.
    pub reason: String,
    /// The sensitive pattern that was matched, if any.
    pub pattern_match: Option<String>,
}

impl fmt::Display for TaintViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "taint violation: {}", self.reason)?;
        if let Some(ref pat) = self.pattern_match {
            write!(f, " (matched: {})", pat)?;
        }
        Ok(())
    }
}

impl std::error::Error for TaintViolation {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_taint_label_ordering() {
        assert!(TaintLabel::Untainted < TaintLabel::UserInput);
        assert!(TaintLabel::UserInput < TaintLabel::ToolOutput);
        assert!(TaintLabel::ToolOutput < TaintLabel::ExternalContent);
        assert!(TaintLabel::ExternalContent < TaintLabel::Secrets);
    }

    #[test]
    fn test_tainted_value_basic() {
        let v = TaintedValue::new("hello".to_string(), TaintLabel::UserInput);
        assert!(v.has_label(TaintLabel::UserInput));
        assert!(!v.has_label(TaintLabel::Secrets));
        assert!(v.is_tainted());
        assert_eq!(v.max_label(), TaintLabel::UserInput);
    }

    #[test]
    fn test_untainted_value() {
        let v = TaintedValue::untainted("clean".to_string());
        assert!(!v.is_tainted());
        assert_eq!(v.max_label(), TaintLabel::Untainted);
    }

    #[test]
    fn test_merge_labels() {
        let mut a = TaintedValue::new("data".to_string(), TaintLabel::UserInput);
        let b = TaintedValue::new("external".to_string(), TaintLabel::ExternalContent);
        a.merge_labels(&b);
        assert!(a.has_label(TaintLabel::UserInput));
        assert!(a.has_label(TaintLabel::ExternalContent));
        assert_eq!(a.max_label(), TaintLabel::ExternalContent);
    }

    #[test]
    fn test_declassify() {
        let mut v = TaintedValue::new("data".to_string(), TaintLabel::Secrets);
        v.taint(TaintLabel::UserInput);
        v.declassify(TaintLabel::Secrets);
        assert!(!v.has_label(TaintLabel::Secrets));
        assert!(v.has_label(TaintLabel::UserInput));
        assert_eq!(v.max_label(), TaintLabel::UserInput);
    }

    #[test]
    fn test_map_preserves_labels() {
        let v = TaintedValue::new("42".to_string(), TaintLabel::ToolOutput);
        let mapped = v.map(|s| s.len());
        assert_eq!(mapped.value, 2);
        assert!(mapped.has_label(TaintLabel::ToolOutput));
    }

    #[test]
    fn test_sink_taint_level_check() {
        let sink = TaintSink::new(TaintLabel::UserInput);
        let ok = TaintedValue::new("safe".to_string(), TaintLabel::UserInput);
        let bad = TaintedValue::new("tainted".to_string(), TaintLabel::ExternalContent);

        assert!(sink.validate(&ok).is_ok());
        assert!(sink.validate(&bad).is_err());
    }

    #[test]
    fn test_sink_pattern_detection() {
        let sink = TaintSink::api_key_sink();
        let value = TaintedValue::new(
            "my key is sk-abc123xyz".to_string(),
            TaintLabel::ToolOutput,
        );
        let result = sink.validate(&value);
        assert!(result.is_err());
        let violation = result.unwrap_err();
        assert_eq!(violation.pattern_match.as_deref(), Some("sk-"));
    }

    #[test]
    fn test_sink_no_pattern_match() {
        let sink = TaintSink::api_key_sink();
        let value = TaintedValue::new(
            "just normal text output".to_string(),
            TaintLabel::ToolOutput,
        );
        assert!(sink.validate(&value).is_ok());
    }

    #[test]
    fn test_declassify_to_empty_becomes_untainted() {
        let mut v = TaintedValue::new("data".to_string(), TaintLabel::UserInput);
        v.declassify(TaintLabel::UserInput);
        assert_eq!(v.max_label(), TaintLabel::Untainted);
        assert!(!v.is_tainted());
    }
}
