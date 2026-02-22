//! Trigram search index for the skill store catalog.
//!
//! ## Theory
//!
//! A trigram index decomposes each indexed string into 3-character windows:
//! `"weather"` → `{"wea", "eat", "ath", "the", "her"}`
//!
//! At query time, the query is also decomposed into trigrams and the
//! posting lists are intersected to find candidate documents. This
//! provides O(1) lookups per trigram vs O(n) linear scan.
//!
//! ## Complexity
//!
//! - **Index build**: O(N·L) where N = documents, L = avg document length
//! - **Query**: O(T·P) where T = query trigrams, P = avg posting list length
//! - **Space**: O(N·L) — each trigram stores a u32 document index
//!
//! For typical store catalogs (N < 10,000, L < 200), this is sub-millisecond.

use std::collections::HashMap;

/// A trigram search index for string documents.
pub struct TrigramIndex {
    /// Posting lists: trigram → sorted vec of document indices.
    postings: HashMap<[u8; 3], Vec<u32>>,
    /// Original indexed strings (lowercase).
    documents: Vec<String>,
    /// Total number of trigrams across all documents.
    total_trigrams: usize,
}

impl TrigramIndex {
    /// Build a trigram index from a set of documents.
    ///
    /// Each document is a concatenation of all searchable fields for an entry.
    pub fn build(texts: &[String]) -> Self {
        let mut postings: HashMap<[u8; 3], Vec<u32>> = HashMap::new();
        let mut documents = Vec::with_capacity(texts.len());
        let mut total_trigrams = 0;

        for (idx, text) in texts.iter().enumerate() {
            let lower = text.to_lowercase();
            documents.push(lower.clone());

            let bytes = lower.as_bytes();
            if bytes.len() < 3 {
                // For very short strings, index the whole thing as a "prefix"
                if !bytes.is_empty() {
                    let mut key = [b' '; 3];
                    for (i, &b) in bytes.iter().enumerate().take(3) {
                        key[i] = b;
                    }
                    postings.entry(key).or_default().push(idx as u32);
                    total_trigrams += 1;
                }
                continue;
            }

            let mut seen_trigrams: std::collections::HashSet<[u8; 3]> =
                std::collections::HashSet::new();

            for window in bytes.windows(3) {
                let key: [u8; 3] = [window[0], window[1], window[2]];
                if seen_trigrams.insert(key) {
                    postings.entry(key).or_default().push(idx as u32);
                    total_trigrams += 1;
                }
            }
        }

        // Sort posting lists for efficient intersection
        for list in postings.values_mut() {
            list.sort_unstable();
            list.dedup();
        }

        Self {
            postings,
            documents,
            total_trigrams,
        }
    }

    /// Search the index for documents matching the query.
    ///
    /// Returns document indices sorted by relevance (number of matching trigrams).
    pub fn search(&self, query: &str) -> Vec<u32> {
        let lower = query.to_lowercase();
        let bytes = lower.as_bytes();

        if bytes.len() < 3 {
            // Fallback to substring match for very short queries
            return self
                .documents
                .iter()
                .enumerate()
                .filter(|(_, doc)| doc.contains(&lower))
                .map(|(i, _)| i as u32)
                .collect();
        }

        // Extract query trigrams
        let mut query_trigrams: Vec<[u8; 3]> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for window in bytes.windows(3) {
            let key: [u8; 3] = [window[0], window[1], window[2]];
            if seen.insert(key) {
                query_trigrams.push(key);
            }
        }

        if query_trigrams.is_empty() {
            return vec![];
        }

        // Intersect posting lists, counting matches per document
        let mut scores: HashMap<u32, usize> = HashMap::new();
        let mut min_list_len = usize::MAX;
        let mut shortest_list: Option<&Vec<u32>> = None;

        for trigram in &query_trigrams {
            if let Some(list) = self.postings.get(trigram) {
                if list.len() < min_list_len {
                    min_list_len = list.len();
                    shortest_list = Some(list);
                }
            }
        }

        // Start with candidates from the shortest posting list
        let candidates: Vec<u32> = match shortest_list {
            Some(list) => list.clone(),
            None => return vec![], // No trigram matched at all
        };

        // Score each candidate by counting matching trigrams
        for &doc_id in &candidates {
            let doc = &self.documents[doc_id as usize];
            let doc_bytes = doc.as_bytes();
            let mut count = 0;
            for trigram in &query_trigrams {
                if doc_bytes
                    .windows(3)
                    .any(|w| w[0] == trigram[0] && w[1] == trigram[1] && w[2] == trigram[2])
                {
                    count += 1;
                }
            }
            if count > 0 {
                scores.insert(doc_id, count);
            }
        }

        // Sort by score descending
        let mut results: Vec<(u32, usize)> = scores.into_iter().collect();
        results.sort_by(|a, b| b.1.cmp(&a.1));
        results.into_iter().map(|(id, _)| id).collect()
    }

    /// Number of indexed documents.
    pub fn document_count(&self) -> usize {
        self.documents.len()
    }

    /// Number of unique trigrams in the index.
    pub fn trigram_count(&self) -> usize {
        self.postings.len()
    }

    /// Total trigrams across all documents (before dedup within posting lists).
    pub fn total_trigrams(&self) -> usize {
        self.total_trigrams
    }
}

/// Build a searchable text string from a store entry's fields.
pub fn entry_to_search_text(
    display_name: &str,
    description: &str,
    author: &str,
    tags: &[String],
) -> String {
    let mut text = format!("{} {} {}", display_name, description, author);
    for tag in tags {
        text.push(' ');
        text.push_str(tag);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_and_search() {
        let docs = vec![
            "weather forecast skill".to_string(),
            "code review automation".to_string(),
            "data analysis pipeline".to_string(),
            "weather alerts and notifications".to_string(),
        ];

        let index = TrigramIndex::build(&docs);
        assert_eq!(index.document_count(), 4);
        assert!(index.trigram_count() > 0);

        let results = index.search("weather");
        assert!(results.len() >= 2);
        assert!(results.contains(&0)); // "weather forecast skill"
        assert!(results.contains(&3)); // "weather alerts and notifications"
    }

    #[test]
    fn search_partial_match() {
        let docs = vec![
            "typescript compiler".to_string(),
            "type checking tool".to_string(),
            "python linter".to_string(),
        ];

        let index = TrigramIndex::build(&docs);
        let results = index.search("type");
        assert!(results.contains(&0)); // "typescript compiler"
        assert!(results.contains(&1)); // "type checking tool"
    }

    #[test]
    fn search_no_match() {
        let docs = vec!["alpha beta gamma".to_string()];
        let index = TrigramIndex::build(&docs);
        let results = index.search("xyz");
        assert!(results.is_empty());
    }

    #[test]
    fn search_short_query() {
        let docs = vec![
            "ai assistant".to_string(),
            "big data tool".to_string(),
        ];
        let index = TrigramIndex::build(&docs);
        let results = index.search("ai");
        assert!(results.contains(&0)); // substring match
    }

    #[test]
    fn case_insensitive() {
        let docs = vec!["WebSearch Tool".to_string()];
        let index = TrigramIndex::build(&docs);
        let results = index.search("websearch");
        assert!(!results.is_empty());
    }

    #[test]
    fn entry_to_search_text_combines_fields() {
        let text = entry_to_search_text(
            "Weather",
            "Get weather forecasts",
            "ClawDesk",
            &["weather".into(), "forecast".into()],
        );
        assert!(text.contains("Weather"));
        assert!(text.contains("forecasts"));
        assert!(text.contains("ClawDesk"));
        assert!(text.contains("forecast"));
    }

    #[test]
    fn empty_index() {
        let index = TrigramIndex::build(&[]);
        assert_eq!(index.document_count(), 0);
        assert_eq!(index.trigram_count(), 0);
        assert!(index.search("anything").is_empty());
    }
}
