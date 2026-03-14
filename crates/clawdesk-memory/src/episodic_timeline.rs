//! # Episodic Timeline — Human-like temporal memory
//!
//! Humans don't recall by vector similarity alone. They recall by:
//!
//! 1. **When** — "yesterday", "last week", "3 days ago"
//! 2. **What topic** — "the stock price discussion", "the debugging session"
//! 3. **Who** — "when I talked to the coder agent"
//! 4. **Emotional salience** — important decisions > casual chat
//!
//! This module adds a timeline index on top of the vector memory,
//! enabling natural temporal queries like:
//!
//! - "What did I ask yesterday?"
//! - "Show me the last 3 days of conversations"
//! - "What questions did I ask about Apple stock?"
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────┐
//! │             Episodic Timeline                │
//! │                                             │
//! │  Day Index:  2026-03-14 → [ep1, ep2, ep3]  │
//! │              2026-03-13 → [ep4, ep5]        │
//! │              2026-03-12 → [ep6, ep7, ep8]   │
//! │                                             │
//! │  Topic Index: "stock" → [ep1, ep4]          │
//! │               "debug" → [ep2, ep6]          │
//! │                                             │
//! │  Agent Index: "coder" → [ep2, ep6]          │
//! │               "researcher" → [ep1, ep4]     │
//! └─────────────────────────────────────────────┘
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// An episode — a single conversational exchange worth remembering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Episode {
    pub id: String,
    /// The user's question/message.
    pub user_content: String,
    /// The agent's response (truncated).
    pub agent_content: String,
    /// When this happened (RFC3339).
    pub timestamp: String,
    /// Which day (YYYY-MM-DD) for day-level indexing.
    pub day: String,
    /// Which agent handled this.
    pub agent_id: String,
    pub agent_name: String,
    /// Which channel (desktop, discord, telegram, cli).
    pub channel: String,
    /// Extracted topics/keywords for topic-based recall.
    pub topics: Vec<String>,
    /// Extracted entities (people, places, organizations).
    pub entities: Vec<ExtractedEntity>,
    /// Importance score (0.0-1.0) — high for decisions, preferences; low for greetings.
    pub importance: f64,
    /// The memory IDs this episode links to in the vector store.
    pub memory_ids: Vec<String>,
}

/// An entity extracted from a conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedEntity {
    pub name: String,
    pub kind: EntityKind,
    /// Where in the text this was found.
    pub context: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntityKind {
    Person,
    Organization,
    Place,
    Product,
    Concept,
    Ticker,
    Url,
}

/// The episodic timeline — indexes episodes by day, topic, and agent.
#[derive(Debug, Default)]
pub struct EpisodicTimeline {
    /// All episodes, keyed by ID.
    episodes: HashMap<String, Episode>,
    /// Day → episode IDs (for "what did I ask yesterday?").
    day_index: HashMap<String, Vec<String>>,
    /// Topic keyword → episode IDs (for "what did I ask about stocks?").
    topic_index: HashMap<String, Vec<String>>,
    /// Agent ID → episode IDs (for "what did I discuss with the coder?").
    agent_index: HashMap<String, Vec<String>>,
    /// Entity name → episode IDs (for "what did I ask about Apple?").
    entity_index: HashMap<String, Vec<String>>,
}

impl EpisodicTimeline {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a new episode in the timeline.
    pub fn record(&mut self, episode: Episode) {
        let id = episode.id.clone();

        // Day index
        self.day_index
            .entry(episode.day.clone())
            .or_default()
            .push(id.clone());

        // Topic index
        for topic in &episode.topics {
            self.topic_index
                .entry(topic.to_lowercase())
                .or_default()
                .push(id.clone());
        }

        // Agent index
        self.agent_index
            .entry(episode.agent_id.clone())
            .or_default()
            .push(id.clone());

        // Entity index
        for entity in &episode.entities {
            self.entity_index
                .entry(entity.name.to_lowercase())
                .or_default()
                .push(id.clone());
        }

        self.episodes.insert(id, episode);
    }

    /// Query episodes for a specific day (e.g., "yesterday").
    pub fn episodes_for_day(&self, day: &str) -> Vec<&Episode> {
        self.day_index
            .get(day)
            .map(|ids| ids.iter().filter_map(|id| self.episodes.get(id)).collect())
            .unwrap_or_default()
    }

    /// Query episodes for a date range.
    pub fn episodes_in_range(&self, from: &str, to: &str) -> Vec<&Episode> {
        self.day_index
            .iter()
            .filter(|(day, _)| day.as_str() >= from && day.as_str() <= to)
            .flat_map(|(_, ids)| ids.iter().filter_map(|id| self.episodes.get(id)))
            .collect()
    }

    /// Query episodes matching a topic keyword.
    pub fn episodes_for_topic(&self, topic: &str) -> Vec<&Episode> {
        self.topic_index
            .get(&topic.to_lowercase())
            .map(|ids| ids.iter().filter_map(|id| self.episodes.get(id)).collect())
            .unwrap_or_default()
    }

    /// Query episodes for a specific agent.
    pub fn episodes_for_agent(&self, agent_id: &str) -> Vec<&Episode> {
        self.agent_index
            .get(agent_id)
            .map(|ids| ids.iter().filter_map(|id| self.episodes.get(id)).collect())
            .unwrap_or_default()
    }

    /// Query episodes mentioning an entity.
    pub fn episodes_for_entity(&self, entity_name: &str) -> Vec<&Episode> {
        self.entity_index
            .get(&entity_name.to_lowercase())
            .map(|ids| ids.iter().filter_map(|id| self.episodes.get(id)).collect())
            .unwrap_or_default()
    }

    /// Get the last N episodes across all days (most recent first).
    pub fn recent(&self, n: usize) -> Vec<&Episode> {
        let mut all: Vec<&Episode> = self.episodes.values().collect();
        all.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        all.truncate(n);
        all
    }

    /// Total episode count.
    pub fn len(&self) -> usize {
        self.episodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.episodes.is_empty()
    }

    /// Get all unique days that have episodes.
    pub fn days(&self) -> Vec<&str> {
        let mut days: Vec<&str> = self.day_index.keys().map(|s| s.as_str()).collect();
        days.sort();
        days
    }

    /// Format a day's episodes as a summary string (for memory_search results).
    pub fn format_day_summary(&self, day: &str) -> String {
        let eps = self.episodes_for_day(day);
        if eps.is_empty() {
            return format!("No conversations recorded for {}", day);
        }

        let mut lines = Vec::new();
        lines.push(format!("Conversations on {} ({} exchanges):", day, eps.len()));
        for (i, ep) in eps.iter().enumerate() {
            let preview = if ep.user_content.len() > 80 {
                format!("{}…", &ep.user_content[..80])
            } else {
                ep.user_content.clone()
            };
            let topics_str = if ep.topics.is_empty() {
                String::new()
            } else {
                format!(" [{}]", ep.topics.join(", "))
            };
            lines.push(format!("  {}. {}{}", i + 1, preview, topics_str));
        }
        lines.join("\n")
    }
}

// ─── Entity Extraction ───────────────────────────────────────────────────────

/// Extract entities from text using pattern matching.
/// This is a lightweight extraction — not NER model-based.
pub fn extract_entities(text: &str) -> Vec<ExtractedEntity> {
    let mut entities = Vec::new();

    // Stock tickers (all-caps, 1-5 letters)
    for word in text.split_whitespace() {
        let clean = word.trim_matches(|c: char| !c.is_alphanumeric());
        if clean.len() >= 2 && clean.len() <= 5 && clean.chars().all(|c| c.is_ascii_uppercase()) {
            // Common non-ticker skip list
            if !matches!(clean, "I" | "A" | "AM" | "PM" | "OK" | "OR" | "AN" | "IS" | "IT" | "IN" | "TO" | "IF" | "AT" | "MY" | "ON" | "NO" | "DO" | "THE" | "AND" | "FOR" | "BUT" | "NOT" | "YOU" | "ALL" | "CAN" | "HER" | "WAS" | "ONE" | "OUR" | "OUT" | "ARE" | "HAS" | "HIS" | "HOW" | "ITS" | "LET" | "MAY" | "NEW" | "NOW" | "OLD" | "SEE" | "WAY" | "WHO" | "ARE" | "DID" | "GET" | "HIM" | "HAD" | "HAS" | "HOW") {
                entities.push(ExtractedEntity {
                    name: clean.to_string(),
                    kind: EntityKind::Ticker,
                    context: text[..text.len().min(50)].to_string(),
                });
            }
        }
    }

    // URLs
    for word in text.split_whitespace() {
        if word.starts_with("http://") || word.starts_with("https://") {
            entities.push(ExtractedEntity {
                name: word.to_string(),
                kind: EntityKind::Url,
                context: String::new(),
            });
        }
    }

    entities
}

/// Extract topic keywords from text.
pub fn extract_topics(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    let mut topics = Vec::new();

    // Known topic patterns
    let patterns: &[(&str, &str)] = &[
        ("stock", "stock"), ("price", "price"), ("bitcoin", "crypto"),
        ("btc", "crypto"), ("eth", "crypto"), ("market", "market"),
        ("code", "code"), ("bug", "debugging"), ("error", "debugging"),
        ("deploy", "deployment"), ("build", "build"), ("test", "testing"),
        ("email", "email"), ("gmail", "email"), ("mail", "email"),
        ("meeting", "meeting"), ("calendar", "calendar"),
        ("research", "research"), ("study", "research"),
        ("design", "design"), ("ui", "design"), ("ux", "design"),
        ("database", "database"), ("sql", "database"),
        ("security", "security"), ("auth", "security"),
        ("api", "api"), ("endpoint", "api"),
        ("docker", "devops"), ("kubernetes", "devops"), ("k8s", "devops"),
    ];

    for (pattern, topic) in patterns {
        if lower.contains(pattern) && !topics.contains(&topic.to_string()) {
            topics.push(topic.to_string());
        }
    }

    topics
}

/// Generate a daily digest of what was discussed.
pub fn daily_digest(timeline: &EpisodicTimeline, day: &str) -> String {
    let eps = timeline.episodes_for_day(day);
    if eps.is_empty() {
        return format!("Nothing recorded for {}.", day);
    }

    let mut topics: Vec<String> = eps.iter()
        .flat_map(|e| e.topics.iter().cloned())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    topics.sort();

    let agents: Vec<String> = eps.iter()
        .map(|e| e.agent_name.clone())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    let high_importance: Vec<&&Episode> = eps.iter()
        .filter(|e| e.importance > 0.7)
        .collect();

    let mut digest = format!("📅 {} — {} conversations", day, eps.len());
    if !topics.is_empty() {
        digest.push_str(&format!("\n📌 Topics: {}", topics.join(", ")));
    }
    if !agents.is_empty() {
        digest.push_str(&format!("\n🤖 Agents: {}", agents.join(", ")));
    }
    if !high_importance.is_empty() {
        digest.push_str(&format!("\n⭐ {} important exchanges", high_importance.len()));
        for ep in high_importance {
            let preview = if ep.user_content.len() > 60 {
                format!("{}…", &ep.user_content[..60])
            } else {
                ep.user_content.clone()
            };
            digest.push_str(&format!("\n  • {}", preview));
        }
    }

    digest
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_episode(id: &str, day: &str, user: &str, topics: &[&str]) -> Episode {
        Episode {
            id: id.into(),
            user_content: user.into(),
            agent_content: "response".into(),
            timestamp: format!("{}T12:00:00Z", day),
            day: day.into(),
            agent_id: "coder".into(),
            agent_name: "Coder".into(),
            channel: "desktop".into(),
            topics: topics.iter().map(|s| s.to_string()).collect(),
            entities: vec![],
            importance: 0.5,
            memory_ids: vec![],
        }
    }

    #[test]
    fn test_day_query() {
        let mut tl = EpisodicTimeline::new();
        tl.record(make_episode("e1", "2026-03-13", "apple stock price", &["stock"]));
        tl.record(make_episode("e2", "2026-03-13", "bitcoin price", &["crypto"]));
        tl.record(make_episode("e3", "2026-03-14", "fix the auth bug", &["debugging"]));

        let yesterday = tl.episodes_for_day("2026-03-13");
        assert_eq!(yesterday.len(), 2);
        let today = tl.episodes_for_day("2026-03-14");
        assert_eq!(today.len(), 1);
    }

    #[test]
    fn test_topic_query() {
        let mut tl = EpisodicTimeline::new();
        tl.record(make_episode("e1", "2026-03-13", "AAPL stock", &["stock"]));
        tl.record(make_episode("e2", "2026-03-14", "MSFT stock", &["stock"]));
        tl.record(make_episode("e3", "2026-03-14", "fix bug", &["debugging"]));

        let stock_eps = tl.episodes_for_topic("stock");
        assert_eq!(stock_eps.len(), 2);
    }

    #[test]
    fn test_recent() {
        let mut tl = EpisodicTimeline::new();
        tl.record(make_episode("e1", "2026-03-11", "old question", &[]));
        tl.record(make_episode("e2", "2026-03-12", "mid question", &[]));
        tl.record(make_episode("e3", "2026-03-13", "new question", &[]));

        let recent = tl.recent(2);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].day, "2026-03-13");
    }

    #[test]
    fn test_entity_extraction() {
        let entities = extract_entities("Check AAPL and MSFT stock prices on https://finance.yahoo.com");
        assert!(entities.iter().any(|e| e.name == "AAPL" && e.kind == EntityKind::Ticker));
        assert!(entities.iter().any(|e| e.name == "MSFT" && e.kind == EntityKind::Ticker));
        assert!(entities.iter().any(|e| e.kind == EntityKind::Url));
    }

    #[test]
    fn test_topic_extraction() {
        let topics = extract_topics("Check the stock price and fix the auth bug in the API");
        assert!(topics.contains(&"stock".to_string()));
        assert!(topics.contains(&"debugging".to_string()));
        assert!(topics.contains(&"security".to_string()));
        assert!(topics.contains(&"api".to_string()));
    }

    #[test]
    fn test_day_summary() {
        let mut tl = EpisodicTimeline::new();
        tl.record(make_episode("e1", "2026-03-13", "apple stock price", &["stock"]));
        tl.record(make_episode("e2", "2026-03-13", "who is gandhi", &["research"]));

        let summary = tl.format_day_summary("2026-03-13");
        assert!(summary.contains("2 exchanges"));
        assert!(summary.contains("apple stock"));
        assert!(summary.contains("gandhi"));
    }
}
