//! Agent marketplace — discovery, search, and installation of community agents.
//!
//! Agents are distributed as TOML files (see `clawdesk-agent-config`). The
//! marketplace provides:
//!
//! 1. **Local index**: Scan `agents/` directory for installed agents
//! 2. **Search**: Text and tag-based search over agent metadata
//! 3. **Install/Uninstall**: Copy/remove agent TOML files
//! 4. **Featured**: Curated list of recommended agents
//!
//! ## Search Algorithm
//!
//! Uses trigram-based fuzzy matching for agent names/descriptions,
//! combined with exact tag matching. Results are scored and ranked.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

/// An entry in the marketplace index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplaceEntry {
    /// Agent name (unique identifier).
    pub name: String,
    /// Short description.
    pub description: String,
    /// Agent version.
    pub version: String,
    /// Author name.
    pub author: Option<String>,
    /// Tags for categorization.
    pub tags: Vec<String>,
    /// Required capabilities.
    pub capabilities: Vec<String>,
    /// Average rating (0.0-5.0).
    pub rating: f64,
    /// Download/install count.
    pub install_count: u64,
    /// Whether this agent is featured/curated.
    pub featured: bool,
    /// Source of the entry: "bundled", "community", "local".
    pub source: String,
    /// File path (for installed agents).
    pub file_path: Option<PathBuf>,
}

/// Search query for the marketplace.
#[derive(Debug, Clone, Default)]
pub struct MarketplaceQuery {
    /// Text query for name/description search.
    pub text: Option<String>,
    /// Filter by tags (AND logic).
    pub tags: Vec<String>,
    /// Minimum rating filter.
    pub min_rating: Option<f64>,
    /// Sort field.
    pub sort_by: SortField,
    /// Maximum results.
    pub limit: usize,
}

/// Sort options for marketplace results.
#[derive(Debug, Clone, Copy, Default)]
pub enum SortField {
    #[default]
    Relevance,
    Rating,
    Popularity,
    Name,
}

/// Search result with score.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplaceResult {
    pub entry: MarketplaceEntry,
    /// Search relevance score (higher = better).
    pub score: f64,
}

/// Agent marketplace with local index and search.
pub struct AgentMarketplace {
    agents_dir: PathBuf,
    index: Vec<MarketplaceEntry>,
}

impl AgentMarketplace {
    /// Create a new marketplace pointing to the agents directory.
    pub fn new(agents_dir: PathBuf) -> Self {
        Self {
            agents_dir,
            index: Vec::new(),
        }
    }

    /// Build the index by scanning the agents directory.
    pub fn refresh_index(&mut self) -> Result<usize, String> {
        self.index.clear();

        if !self.agents_dir.exists() {
            return Ok(0);
        }

        let entries = std::fs::read_dir(&self.agents_dir)
            .map_err(|e| format!("failed to read agents directory: {e}"))?;

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            match self.parse_agent_entry(&path) {
                Ok(entry) => {
                    debug!(agent = %entry.name, "indexed agent");
                    self.index.push(entry);
                }
                Err(e) => warn!(path = %path.display(), error = %e, "failed to parse agent"),
            }
        }

        info!(count = self.index.len(), "marketplace index refreshed");
        Ok(self.index.len())
    }

    /// Search the marketplace index.
    pub fn search(&self, query: &MarketplaceQuery) -> Vec<MarketplaceResult> {
        let limit = if query.limit == 0 { 20 } else { query.limit };

        let mut results: Vec<MarketplaceResult> = self
            .index
            .iter()
            .filter_map(|entry| {
                // Tag filter (AND)
                if !query.tags.is_empty() {
                    let entry_tags_lower: Vec<String> =
                        entry.tags.iter().map(|t| t.to_lowercase()).collect();
                    for tag in &query.tags {
                        if !entry_tags_lower.contains(&tag.to_lowercase()) {
                            return None;
                        }
                    }
                }

                // Rating filter
                if let Some(min) = query.min_rating {
                    if entry.rating < min {
                        return None;
                    }
                }

                // Score based on text match
                let score = if let Some(text) = &query.text {
                    self.text_score(entry, text)
                } else {
                    1.0
                };

                if score <= 0.0 && query.text.is_some() {
                    return None;
                }

                Some(MarketplaceResult {
                    entry: entry.clone(),
                    score,
                })
            })
            .collect();

        // Sort
        match query.sort_by {
            SortField::Relevance => {
                results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
            }
            SortField::Rating => {
                results.sort_by(|a, b| b.entry.rating.partial_cmp(&a.entry.rating).unwrap_or(std::cmp::Ordering::Equal));
            }
            SortField::Popularity => {
                results.sort_by(|a, b| b.entry.install_count.cmp(&a.entry.install_count));
            }
            SortField::Name => {
                results.sort_by(|a, b| a.entry.name.cmp(&b.entry.name));
            }
        }

        results.truncate(limit);
        results
    }

    /// List all indexed agents.
    pub fn list_all(&self) -> &[MarketplaceEntry] {
        &self.index
    }

    /// List featured/curated agents.
    pub fn featured(&self) -> Vec<&MarketplaceEntry> {
        self.index.iter().filter(|e| e.featured).collect()
    }

    /// Install an agent from a TOML string.
    pub fn install(&self, name: &str, toml_content: &str) -> Result<PathBuf, String> {
        // Validate TOML parses
        let _: toml::Value = toml::from_str(toml_content)
            .map_err(|e| format!("invalid TOML: {e}"))?;

        let filename = format!("{name}.toml");
        let path = self.agents_dir.join(&filename);

        if path.exists() {
            return Err(format!("agent '{name}' already installed"));
        }

        std::fs::create_dir_all(&self.agents_dir)
            .map_err(|e| format!("failed to create agents directory: {e}"))?;
        std::fs::write(&path, toml_content)
            .map_err(|e| format!("failed to write agent file: {e}"))?;

        info!(agent = name, path = %path.display(), "agent installed");
        Ok(path)
    }

    /// Uninstall an agent by name.
    pub fn uninstall(&self, name: &str) -> Result<(), String> {
        let path = self.agents_dir.join(format!("{name}.toml"));
        if !path.exists() {
            return Err(format!("agent '{name}' not found"));
        }
        std::fs::remove_file(&path)
            .map_err(|e| format!("failed to remove agent: {e}"))?;
        info!(agent = name, "agent uninstalled");
        Ok(())
    }

    // ── Internal ────────────────────────────────────────────────

    /// Parse an agent TOML file into a marketplace entry.
    fn parse_agent_entry(&self, path: &Path) -> Result<MarketplaceEntry, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("read error: {e}"))?;
        let doc: toml::Value = toml::from_str(&content)
            .map_err(|e| format!("parse error: {e}"))?;

        let agent = doc.get("agent").ok_or("missing [agent] section")?;
        let name = agent
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or("missing agent.name")?
            .to_string();

        let description = agent
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let version = agent
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("1.0.0")
            .to_string();

        let author = agent.get("author").and_then(|v| v.as_str()).map(String::from);

        let tags = agent
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let capabilities = doc
            .get("capabilities")
            .and_then(|c| c.get("tools"))
            .and_then(|t| t.get("allow"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let metadata = doc
            .get("metadata")
            .and_then(|m| m.as_table())
            .map(|t| {
                t.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                    .collect::<HashMap<String, String>>()
            })
            .unwrap_or_default();

        let featured = metadata.get("featured").map_or(false, |v| v == "true");

        Ok(MarketplaceEntry {
            name,
            description,
            version,
            author,
            tags,
            capabilities,
            rating: 0.0,
            install_count: 0,
            featured,
            source: "local".into(),
            file_path: Some(path.to_path_buf()),
        })
    }

    /// Compute text relevance score using case-insensitive substring matching.
    /// Returns 0.0 if no match, higher for better matches.
    fn text_score(&self, entry: &MarketplaceEntry, query: &str) -> f64 {
        let query_lower = query.to_lowercase();
        let name_lower = entry.name.to_lowercase();
        let desc_lower = entry.description.to_lowercase();

        let mut score = 0.0;

        // Exact name match
        if name_lower == query_lower {
            score += 10.0;
        }
        // Name contains query
        else if name_lower.contains(&query_lower) {
            score += 5.0;
        }

        // Description contains query
        if desc_lower.contains(&query_lower) {
            score += 2.0;
        }

        // Tag match
        for tag in &entry.tags {
            if tag.to_lowercase().contains(&query_lower) {
                score += 3.0;
            }
        }

        // Word-level matching for multi-word queries
        for word in query_lower.split_whitespace() {
            if name_lower.contains(word) {
                score += 1.0;
            }
            if desc_lower.contains(word) {
                score += 0.5;
            }
        }

        score
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn sample_entries() -> Vec<MarketplaceEntry> {
        vec![
            MarketplaceEntry {
                name: "coder".into(),
                description: "Expert coding assistant for Rust and Python".into(),
                version: "1.0.0".into(),
                author: Some("ClawDesk".into()),
                tags: vec!["code".into(), "rust".into(), "python".into()],
                capabilities: vec!["shell".into(), "file_read".into()],
                rating: 4.8,
                install_count: 1000,
                featured: true,
                source: "bundled".into(),
                file_path: None,
            },
            MarketplaceEntry {
                name: "researcher".into(),
                description: "Deep web research with citation tracking".into(),
                version: "1.0.0".into(),
                author: Some("ClawDesk".into()),
                tags: vec!["research".into(), "web".into()],
                capabilities: vec!["browser".into(), "web_search".into()],
                rating: 4.5,
                install_count: 500,
                featured: false,
                source: "bundled".into(),
                file_path: None,
            },
        ]
    }

    #[test]
    fn search_by_text() {
        let mut marketplace = AgentMarketplace::new(PathBuf::from("/tmp/agents"));
        marketplace.index = sample_entries();

        let results = marketplace.search(&MarketplaceQuery {
            text: Some("rust".into()),
            ..Default::default()
        });

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.name, "coder");
    }

    #[test]
    fn search_by_tag() {
        let mut marketplace = AgentMarketplace::new(PathBuf::from("/tmp/agents"));
        marketplace.index = sample_entries();

        let results = marketplace.search(&MarketplaceQuery {
            tags: vec!["research".into()],
            ..Default::default()
        });

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.name, "researcher");
    }

    #[test]
    fn search_with_rating_filter() {
        let mut marketplace = AgentMarketplace::new(PathBuf::from("/tmp/agents"));
        marketplace.index = sample_entries();

        let results = marketplace.search(&MarketplaceQuery {
            min_rating: Some(4.6),
            ..Default::default()
        });

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.name, "coder");
    }

    #[test]
    fn featured_agents() {
        let mut marketplace = AgentMarketplace::new(PathBuf::from("/tmp/agents"));
        marketplace.index = sample_entries();

        let featured = marketplace.featured();
        assert_eq!(featured.len(), 1);
        assert_eq!(featured[0].name, "coder");
    }
}
