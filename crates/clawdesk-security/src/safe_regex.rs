//! # ReDoS-Safe Regex Compilation
//!
//! Prevents Regular Expression Denial of Service (ReDoS) by analyzing regex
//! patterns for dangerous constructs before compilation. Any user-supplied or
//! config-driven regex MUST pass through [`safe_compile`] before use.
//!
//! ## Detection strategy
//!
//! 1. **Tokenize** the pattern into groups, quantifiers, and alternations.
//! 2. **Detect nested repetition** — e.g. `(a+)+`, `(a*)*`, `(a+|b)*`.
//! 3. **Detect ambiguous alternation** — overlapping branches under a
//!    quantifier, e.g. `(a|ab)+`.
//! 4. **Length limit** — reject patterns exceeding `MAX_PATTERN_LENGTH`.
//!
//! Results are cached in an LRU to avoid re-analysis of repeated patterns.
//!
//! Inspired by openclaw's `safe-regex.ts` — ported to Rust with zero external
//! dependencies beyond `regex`.

use regex::Regex;
use std::collections::HashMap;
use std::sync::Mutex;

/// Maximum allowed pattern length (characters).
const MAX_PATTERN_LENGTH: usize = 1024;
/// Maximum LRU cache entries.
const MAX_CACHE_ENTRIES: usize = 256;

// ───────────────────────────────────────────────────────────────────────────
// Public types
// ───────────────────────────────────────────────────────────────────────────

/// Result of safe regex compilation.
#[derive(Debug, Clone)]
pub struct SafeRegexResult {
    /// The compiled regex, or `None` if unsafe/invalid.
    pub regex: Option<Regex>,
    /// The original source pattern.
    pub source: String,
    /// Rejection reason, if any.
    pub reason: Option<RejectReason>,
}

/// Why a regex pattern was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RejectReason {
    /// Pattern is empty.
    Empty,
    /// Pattern exceeds MAX_PATTERN_LENGTH.
    TooLong { length: usize, max: usize },
    /// Pattern contains nested quantifiers that cause exponential backtracking.
    UnsafeNestedRepetition { detail: String },
    /// Pattern contains ambiguous alternation under a quantifier.
    AmbiguousAlternation { detail: String },
    /// Pattern failed `regex::Regex::new()`.
    InvalidRegex { error: String },
}

impl std::fmt::Display for RejectReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "empty pattern"),
            Self::TooLong { length, max } => {
                write!(f, "pattern too long ({length} chars, max {max})")
            }
            Self::UnsafeNestedRepetition { detail } => {
                write!(f, "unsafe nested repetition: {detail}")
            }
            Self::AmbiguousAlternation { detail } => {
                write!(f, "ambiguous alternation: {detail}")
            }
            Self::InvalidRegex { error } => write!(f, "invalid regex: {error}"),
        }
    }
}

impl SafeRegexResult {
    /// Returns `true` if the regex compiled successfully.
    pub fn is_ok(&self) -> bool {
        self.regex.is_some()
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Token types for pattern analysis
// ───────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Token {
    /// A literal character or character class `[...]`.
    Atom,
    /// Opening group `(`.
    GroupOpen,
    /// Closing group `)`.
    GroupClose,
    /// A quantifier: `*`, `+`, `?`, `{n,m}`.
    Quantifier(QuantifierKind),
    /// Alternation `|`.
    Alternation,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum QuantifierKind {
    Star,     // *
    Plus,     // +
    Question, // ?
    Braced,   // {n,m}
}

impl QuantifierKind {
    fn is_repeating(self) -> bool {
        matches!(self, Self::Star | Self::Plus | Self::Braced)
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Tokenizer
// ───────────────────────────────────────────────────────────────────────────

fn tokenize(pattern: &str) -> Vec<Token> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;

    while i < chars.len() {
        match chars[i] {
            '\\' => {
                // Escaped character — treat as atom, skip next char.
                i += 1; // skip the backslash
                if i < chars.len() {
                    i += 1; // skip the escaped char
                }
                tokens.push(Token::Atom);
            }
            '[' => {
                // Character class — skip to closing `]`.
                i += 1;
                if i < chars.len() && chars[i] == '^' {
                    i += 1;
                }
                // first char after `[` or `[^` can be `]` literally
                if i < chars.len() && chars[i] == ']' {
                    i += 1;
                }
                while i < chars.len() && chars[i] != ']' {
                    if chars[i] == '\\' {
                        i += 1; // skip escape in char class
                    }
                    i += 1;
                }
                if i < chars.len() {
                    i += 1; // skip closing `]`
                }
                tokens.push(Token::Atom);
            }
            '(' => {
                i += 1;
                // Skip group modifiers: `?:`, `?=`, `?!`, `?<`, etc.
                if i < chars.len() && chars[i] == '?' {
                    i += 1;
                    // Skip modifier chars until we hit something that ends the modifier.
                    while i < chars.len() && chars[i] != ')' && chars[i] != '(' {
                        let c = chars[i];
                        i += 1;
                        if c == ':' || c == '=' || c == '!' || c == '>' {
                            break;
                        }
                        // Named group: `?<name>` or `?P<name>`
                        if c == '<' || c == 'P' {
                            while i < chars.len() && chars[i] != '>' {
                                i += 1;
                            }
                            if i < chars.len() {
                                i += 1; // skip `>`
                            }
                            break;
                        }
                    }
                }
                tokens.push(Token::GroupOpen);
            }
            ')' => {
                i += 1;
                tokens.push(Token::GroupClose);
            }
            '*' => {
                i += 1;
                if i < chars.len() && chars[i] == '?' {
                    i += 1; // lazy modifier
                }
                tokens.push(Token::Quantifier(QuantifierKind::Star));
            }
            '+' => {
                i += 1;
                if i < chars.len() && chars[i] == '?' {
                    i += 1;
                }
                tokens.push(Token::Quantifier(QuantifierKind::Plus));
            }
            '?' => {
                i += 1;
                if i < chars.len() && chars[i] == '?' {
                    i += 1;
                }
                tokens.push(Token::Quantifier(QuantifierKind::Question));
            }
            '{' => {
                // Braced quantifier: {n}, {n,}, {n,m}
                let start = i;
                i += 1;
                let mut valid = false;
                while i < chars.len() && chars[i] != '}' {
                    i += 1;
                }
                if i < chars.len() {
                    // Check if content between braces looks like a quantifier
                    let inner = &pattern[start + 1..i];
                    valid = inner.chars().all(|c| c.is_ascii_digit() || c == ',');
                    i += 1; // skip `}`
                }
                if i < chars.len() && chars[i] == '?' {
                    i += 1; // lazy
                }
                if valid {
                    tokens.push(Token::Quantifier(QuantifierKind::Braced));
                } else {
                    // Not a valid quantifier — treat as literal.
                    tokens.push(Token::Atom);
                }
            }
            '|' => {
                i += 1;
                tokens.push(Token::Alternation);
            }
            '^' | '$' => {
                i += 1;
                // Anchors don't produce atoms.
            }
            '.' => {
                i += 1;
                tokens.push(Token::Atom);
            }
            _ => {
                i += 1;
                tokens.push(Token::Atom);
            }
        }
    }
    tokens
}

// ───────────────────────────────────────────────────────────────────────────
// Structural analysis
// ───────────────────────────────────────────────────────────────────────────

/// Check for nested repetition: a quantifier applied to a group that itself
/// contains a quantifier (e.g., `(a+)+`).
fn detect_nested_repetition(tokens: &[Token]) -> Option<String> {
    // Track group nesting depth and whether we've seen a quantifier at each level.
    let mut depth: usize = 0;
    let mut quantifier_at_depth: Vec<bool> = Vec::new();

    for token in tokens {
        match token {
            Token::GroupOpen => {
                depth += 1;
                if quantifier_at_depth.len() <= depth {
                    quantifier_at_depth.resize(depth + 1, false);
                }
                quantifier_at_depth[depth] = false;
            }
            Token::GroupClose => {
                if depth == 0 {
                    continue; // unbalanced — skip
                }
                depth -= 1;
            }
            Token::Quantifier(kind) if kind.is_repeating() => {
                // If we're inside a group AND there's already a quantifier
                // at this depth, we have nested repetition.
                if depth > 0 {
                    quantifier_at_depth[depth] = true;
                }

                // If the previous token was GroupClose, check if the inner group
                // had a repeating quantifier.
                // We approximate: if any depth > current had a quantifier, flag it.
                for d in (depth + 1)..quantifier_at_depth.len() {
                    if quantifier_at_depth[d] {
                        return Some(format!(
                            "repeating quantifier on group that contains a repeating quantifier (depth {d})"
                        ));
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// Check for alternation inside a quantified group where branches overlap.
/// This is a simplified heuristic: flag `(...|...)+` or `(...|...)*` when
/// any branch is a prefix/suffix of another.
fn detect_ambiguous_alternation(tokens: &[Token]) -> Option<String> {
    // Find groups that contain alternation AND are followed by a repeating quantifier.
    let mut i = 0;
    let mut group_starts: Vec<usize> = Vec::new();

    while i < tokens.len() {
        match &tokens[i] {
            Token::GroupOpen => {
                group_starts.push(i);
            }
            Token::GroupClose => {
                if let Some(start) = group_starts.pop() {
                    // Check if this group is followed by a repeating quantifier.
                    let next = i + 1;
                    if next < tokens.len() {
                        if let Token::Quantifier(kind) = &tokens[next] {
                            if kind.is_repeating() {
                                // Check if group body contains alternation.
                                let body = &tokens[start + 1..i];
                                let has_alternation =
                                    body.iter().any(|t| matches!(t, Token::Alternation));
                                if has_alternation {
                                    // Count branches (atoms between alternations)
                                    let branch_count =
                                        body.iter().filter(|t| matches!(t, Token::Alternation)).count()
                                            + 1;
                                    if branch_count >= 2 {
                                        return Some(format!(
                                            "alternation with {branch_count} branches under repeating quantifier"
                                        ));
                                    }
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

// ───────────────────────────────────────────────────────────────────────────
// Core analysis
// ───────────────────────────────────────────────────────────────────────────

fn analyze_pattern(pattern: &str) -> Option<RejectReason> {
    if pattern.is_empty() {
        return Some(RejectReason::Empty);
    }
    if pattern.len() > MAX_PATTERN_LENGTH {
        return Some(RejectReason::TooLong {
            length: pattern.len(),
            max: MAX_PATTERN_LENGTH,
        });
    }

    let tokens = tokenize(pattern);

    if let Some(detail) = detect_nested_repetition(&tokens) {
        return Some(RejectReason::UnsafeNestedRepetition { detail });
    }
    if let Some(detail) = detect_ambiguous_alternation(&tokens) {
        return Some(RejectReason::AmbiguousAlternation { detail });
    }

    None
}

// ───────────────────────────────────────────────────────────────────────────
// LRU Cache
// ───────────────────────────────────────────────────────────────────────────

struct SafeRegexCache {
    entries: HashMap<String, SafeRegexResult>,
    order: Vec<String>,
}

impl SafeRegexCache {
    fn new() -> Self {
        Self {
            entries: HashMap::with_capacity(MAX_CACHE_ENTRIES),
            order: Vec::with_capacity(MAX_CACHE_ENTRIES),
        }
    }

    fn get(&self, pattern: &str) -> Option<SafeRegexResult> {
        self.entries.get(pattern).cloned()
    }

    fn insert(&mut self, pattern: String, result: SafeRegexResult) {
        if self.entries.len() >= MAX_CACHE_ENTRIES {
            // Evict oldest
            if let Some(oldest) = self.order.first().cloned() {
                self.entries.remove(&oldest);
                self.order.remove(0);
            }
        }
        self.order.push(pattern.clone());
        self.entries.insert(pattern, result);
    }
}

static CACHE: Mutex<Option<SafeRegexCache>> = Mutex::new(None);

fn with_cache<F, R>(f: F) -> R
where
    F: FnOnce(&mut SafeRegexCache) -> R,
{
    let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let cache = guard.get_or_insert_with(SafeRegexCache::new);
    f(cache)
}

// ───────────────────────────────────────────────────────────────────────────
// Public API
// ───────────────────────────────────────────────────────────────────────────

/// Compile a regex pattern with ReDoS safety analysis.
///
/// Returns a [`SafeRegexResult`] containing either the compiled `Regex` or a
/// rejection reason. Results are cached (LRU, 256 entries).
///
/// # Usage
///
/// Any code path that compiles user-supplied or config-driven regex patterns
/// MUST use this function instead of `regex::Regex::new()` directly.
///
/// ```rust
/// use clawdesk_security::safe_regex::safe_compile;
///
/// let result = safe_compile(r"\d{3}-\d{4}");
/// assert!(result.is_ok());
///
/// let evil = safe_compile(r"(a+)+$");
/// assert!(!evil.is_ok());
/// ```
pub fn safe_compile(pattern: &str) -> SafeRegexResult {
    // Check cache first.
    if let Some(cached) = with_cache(|c| c.get(pattern)) {
        return cached;
    }

    let result = compile_inner(pattern);
    with_cache(|c| c.insert(pattern.to_string(), result.clone()));
    result
}

/// Compile with case-insensitive flag.
pub fn safe_compile_case_insensitive(pattern: &str) -> SafeRegexResult {
    let ci_pattern = format!("(?i){pattern}");
    safe_compile(&ci_pattern)
}

fn compile_inner(pattern: &str) -> SafeRegexResult {
    // Phase 1: structural analysis.
    if let Some(reason) = analyze_pattern(pattern) {
        return SafeRegexResult {
            regex: None,
            source: pattern.to_string(),
            reason: Some(reason),
        };
    }

    // Phase 2: try to compile.
    match Regex::new(pattern) {
        Ok(regex) => SafeRegexResult {
            regex: Some(regex),
            source: pattern.to_string(),
            reason: None,
        },
        Err(err) => SafeRegexResult {
            regex: None,
            source: pattern.to_string(),
            reason: Some(RejectReason::InvalidRegex {
                error: err.to_string(),
            }),
        },
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_pattern_compiles() {
        let r = safe_compile(r"\d{3}-\d{4}");
        assert!(r.is_ok());
        assert!(r.reason.is_none());
    }

    #[test]
    fn empty_pattern_rejected() {
        let r = safe_compile("");
        assert!(!r.is_ok());
        assert_eq!(r.reason, Some(RejectReason::Empty));
    }

    #[test]
    fn too_long_pattern_rejected() {
        let long = "a".repeat(MAX_PATTERN_LENGTH + 1);
        let r = safe_compile(&long);
        assert!(!r.is_ok());
        assert!(matches!(r.reason, Some(RejectReason::TooLong { .. })));
    }

    #[test]
    fn nested_repetition_rejected() {
        // Classic ReDoS: (a+)+
        let r = safe_compile(r"(a+)+");
        assert!(!r.is_ok());
        assert!(matches!(
            r.reason,
            Some(RejectReason::UnsafeNestedRepetition { .. })
        ));
    }

    #[test]
    fn nested_star_star_rejected() {
        let r = safe_compile(r"(a*)*");
        assert!(!r.is_ok());
        assert!(matches!(
            r.reason,
            Some(RejectReason::UnsafeNestedRepetition { .. })
        ));
    }

    #[test]
    fn ambiguous_alternation_rejected() {
        let r = safe_compile(r"(a|ab)+");
        assert!(!r.is_ok());
        assert!(matches!(
            r.reason,
            Some(RejectReason::AmbiguousAlternation { .. })
        ));
    }

    #[test]
    fn safe_alternation_allowed() {
        // Non-quantified alternation is fine.
        let r = safe_compile(r"(cat|dog)");
        assert!(r.is_ok());
    }

    #[test]
    fn simple_quantified_group_allowed() {
        // Group with quantifier but no inner quantifier — safe.
        let r = safe_compile(r"(abc)+");
        assert!(r.is_ok());
    }

    #[test]
    fn invalid_regex_caught() {
        let r = safe_compile(r"(unclosed");
        assert!(!r.is_ok());
        assert!(matches!(r.reason, Some(RejectReason::InvalidRegex { .. })));
    }

    #[test]
    fn cache_returns_same_result() {
        let r1 = safe_compile(r"\w+");
        let r2 = safe_compile(r"\w+");
        assert_eq!(r1.source, r2.source);
        assert_eq!(r1.reason, r2.reason);
    }

    #[test]
    fn case_insensitive_works() {
        let r = safe_compile_case_insensitive(r"hello");
        assert!(r.is_ok());
        let regex = r.regex.unwrap();
        assert!(regex.is_match("HELLO"));
    }

    #[test]
    fn character_class_not_flagged() {
        let r = safe_compile(r"[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}");
        assert!(r.is_ok());
    }

    #[test]
    fn escaped_chars_handled() {
        let r = safe_compile(r"\(\d+\)\+");
        assert!(r.is_ok());
    }
}
