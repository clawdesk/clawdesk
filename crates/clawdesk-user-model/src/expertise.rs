//! Expertise modeling — infers what the user knows.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Coarse expertise level (avoids false precision).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ExpertiseLevel {
    Novice,
    Intermediate,
    Advanced,
    Expert,
}

impl ExpertiseLevel {
    /// How much explanation is appropriate at this level.
    pub fn explanation_depth(&self) -> f64 {
        match self {
            Self::Novice => 1.0,        // explain everything
            Self::Intermediate => 0.6,  // explain non-obvious parts
            Self::Advanced => 0.3,      // explain only subtle points
            Self::Expert => 0.1,        // just give the answer
        }
    }
}

/// A domain of expertise.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Domain {
    Rust,
    Python,
    JavaScript,
    SystemAdmin,
    DevOps,
    Database,
    MachineLearning,
    WebDev,
    Security,
    General,
    Custom(String),
}

/// Per-domain expertise profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpertiseProfile {
    /// Domain → expertise level inferred from messages.
    pub domains: HashMap<Domain, DomainExpertise>,
    /// Total interactions across all domains.
    pub total_interactions: u64,
}

/// Expertise evidence for a single domain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainExpertise {
    pub level: ExpertiseLevel,
    /// Evidence counters: (advanced_signals, basic_signals).
    pub evidence: (u32, u32),
    /// How many messages touched this domain.
    pub interaction_count: u32,
}

impl ExpertiseProfile {
    pub fn new() -> Self {
        Self {
            domains: HashMap::new(),
            total_interactions: 0,
        }
    }

    /// Update expertise from a user message.
    pub fn observe_message(&mut self, text: &str) {
        self.total_interactions += 1;
        let domains_detected = detect_domains(text);
        let signal_level = assess_signal_level(text);

        for domain in domains_detected {
            let entry = self.domains.entry(domain).or_insert_with(|| DomainExpertise {
                level: ExpertiseLevel::Intermediate, // prior
                evidence: (0, 0),
                interaction_count: 0,
            });
            entry.interaction_count += 1;
            match signal_level {
                SignalLevel::Advanced => entry.evidence.0 += 1,
                SignalLevel::Basic => entry.evidence.1 += 1,
                SignalLevel::Neutral => {}
            }
            // Recompute level from evidence ratio
            entry.level = compute_level(entry.evidence.0, entry.evidence.1);
        }
    }

    /// Get expertise level for a domain.
    pub fn level_for(&self, domain: &Domain) -> ExpertiseLevel {
        self.domains.get(domain)
            .map(|d| d.level)
            .unwrap_or(ExpertiseLevel::Intermediate) // neutral prior
    }

    /// Whether the user likely needs an explanation for a topic in a domain.
    pub fn should_explain(&self, domain: &Domain) -> bool {
        self.level_for(domain) <= ExpertiseLevel::Intermediate
    }
}

impl Default for ExpertiseProfile {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy)]
enum SignalLevel { Advanced, Basic, Neutral }

fn assess_signal_level(text: &str) -> SignalLevel {
    let lower = text.to_lowercase();
    // Advanced signals: uses technical jargon, references internals,
    // asks "why" not "how", mentions specific tools/flags/options
    let advanced_markers = [
        "lifetime", "borrow checker", "monomorphization", "vtable",
        "O(n)", "amortized", "eigenvalue", "gradient descent",
        "syscall", "mmap", "io_uring", "epoll", "SIMD",
        "combinatorial", "NP-hard", "type erasure",
        "--release", "--no-verify", "-DNDEBUG",
    ];
    let basic_markers = [
        "how do i", "what is", "can you explain", "help me",
        "i don't understand", "what does this mean",
        "for beginners", "step by step", "tutorial",
    ];

    let advanced_hits = advanced_markers.iter().filter(|m| lower.contains(*m)).count();
    let basic_hits = basic_markers.iter().filter(|m| lower.contains(*m)).count();

    if advanced_hits > basic_hits { SignalLevel::Advanced }
    else if basic_hits > 0 { SignalLevel::Basic }
    else { SignalLevel::Neutral }
}

fn detect_domains(text: &str) -> Vec<Domain> {
    let lower = text.to_lowercase();
    let mut domains = Vec::new();

    let domain_markers: &[(&[&str], Domain)] = &[
        (&["rust", "cargo", "tokio", "async fn", "impl ", "trait ", ".rs", "borrow", "lifetime"], Domain::Rust),
        (&["python", "pip", "pytest", "django", "flask", ".py"], Domain::Python),
        (&["javascript", "typescript", "npm", "node", "react", ".js", ".ts"], Domain::JavaScript),
        (&["docker", "kubernetes", "k8s", "helm", "terraform", "ci/cd", "deploy"], Domain::DevOps),
        (&["sql", "postgres", "mysql", "redis", "database", "query", "index"], Domain::Database),
        (&["ml", "model", "training", "inference", "embedding", "neural", "transformer"], Domain::MachineLearning),
        (&["css", "html", "browser", "frontend", "dom", "api endpoint"], Domain::WebDev),
        (&["ssh", "sudo", "systemctl", "nginx", "linux", "chmod"], Domain::SystemAdmin),
        (&["vulnerability", "cve", "encryption", "tls", "auth", "xss", "injection"], Domain::Security),
    ];

    for (markers, domain) in domain_markers {
        if markers.iter().any(|m| lower.contains(m)) {
            domains.push(domain.clone());
        }
    }

    if domains.is_empty() {
        domains.push(Domain::General);
    }
    domains
}

fn compute_level(advanced: u32, basic: u32) -> ExpertiseLevel {
    let total = advanced + basic;
    if total < 3 { return ExpertiseLevel::Intermediate; } // not enough evidence
    let ratio = advanced as f64 / total as f64;
    if ratio >= 0.75 { ExpertiseLevel::Expert }
    else if ratio >= 0.5 { ExpertiseLevel::Advanced }
    else if ratio >= 0.25 { ExpertiseLevel::Intermediate }
    else { ExpertiseLevel::Novice }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn novice_signal() {
        let mut prof = ExpertiseProfile::new();
        prof.observe_message("How do I write a for loop in Rust? Can you explain step by step?");
        assert!(prof.should_explain(&Domain::Rust));
    }

    #[test]
    fn expert_signal() {
        let mut prof = ExpertiseProfile::new();
        for _ in 0..5 {
            prof.observe_message("The borrow checker rejects this because of lifetime variance in the monomorphization");
        }
        assert_eq!(prof.level_for(&Domain::Rust), ExpertiseLevel::Expert);
        assert!(!prof.should_explain(&Domain::Rust));
    }

    #[test]
    fn domain_detection() {
        let domains = detect_domains("fix this Rust cargo build error in the tokio async fn");
        assert!(domains.contains(&Domain::Rust));
    }
}
