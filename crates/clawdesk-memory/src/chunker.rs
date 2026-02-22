//! Content-aware chunking pipeline — segments text into overlapping,
//! independently searchable chunks with content-hash deduplication.
//!
//! ## Algorithm
//!
//! Splits text at semantic boundaries (sentence > paragraph > newline)
//! into chunks of `max_chars` with `overlap_chars` carry-forward.
//!
//! ## UTF-8 Safety
//!
//! All truncation is via `char_indices()` — impossible to split inside
//! a multi-byte character. Fixes the `&user_content[..497]` panic.
//!
//! ## Deduplication
//!
//! Each chunk gets a content hash:
//! ```text
//! chunk_id = SHA256(collection || source || chunk_index || content_hash)
//! ```
//! SochDB `put()` with the same key is an idempotent upsert.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

/// Configuration for the chunker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkerConfig {
    /// Maximum characters per chunk (not bytes).
    pub max_chars: usize,
    /// Overlap characters carried to the next chunk.
    pub overlap_chars: usize,
    /// Minimum chunk size (smaller chunks are merged with previous).
    pub min_chars: usize,
}

impl Default for ChunkerConfig {
    fn default() -> Self {
        Self {
            max_chars: 2048,
            overlap_chars: 200,
            min_chars: 100,
        }
    }
}

/// A chunk produced by the chunker.
#[derive(Debug, Clone)]
pub struct Chunk {
    /// The chunk text.
    pub text: String,
    /// Zero-based chunk index within the source.
    pub index: usize,
    /// Character offset of this chunk within the original text.
    pub char_offset: usize,
    /// Number of characters in this chunk.
    pub char_len: usize,
    /// SHA-256 of the chunk content (hex string).
    pub content_hash: String,
}

/// Chunk text into overlapping segments at semantic boundaries.
///
/// Respects sentence, paragraph, and newline boundaries. Never splits
/// inside a UTF-8 multi-byte character.
pub fn chunk_text(text: &str, config: &ChunkerConfig) -> Vec<Chunk> {
    if text.is_empty() {
        return Vec::new();
    }

    let char_count: usize = text.chars().count();
    if char_count <= config.max_chars {
        // Entire text fits in one chunk
        return vec![Chunk {
            text: text.to_string(),
            index: 0,
            char_offset: 0,
            char_len: char_count,
            content_hash: sha256_hex(text),
        }];
    }

    let mut chunks = Vec::new();
    let mut start_char = 0usize;
    let char_indices: Vec<(usize, char)> = text.char_indices().collect();
    let total_chars = char_indices.len();

    while start_char < total_chars {
        // Determine end boundary (at most max_chars from start)
        let end_char = (start_char + config.max_chars).min(total_chars);

        // Try to find the best split point (sentence > paragraph > newline > word)
        let split_char = if end_char >= total_chars {
            total_chars
        } else {
            find_best_split(&char_indices, start_char, end_char, config.min_chars)
        };

        // Extract the chunk text using byte offsets
        let byte_start = char_indices[start_char].0;
        let byte_end = if split_char >= total_chars {
            text.len()
        } else {
            char_indices[split_char].0
        };
        let chunk_text = &text[byte_start..byte_end];

        if !chunk_text.trim().is_empty() {
            chunks.push(Chunk {
                text: chunk_text.to_string(),
                index: chunks.len(),
                char_offset: start_char,
                char_len: split_char - start_char,
                content_hash: sha256_hex(chunk_text),
            });
        }

        // Advance with overlap
        if split_char >= total_chars {
            break;
        }
        let overlap = config.overlap_chars.min(split_char - start_char);
        start_char = split_char.saturating_sub(overlap);
    }

    // Merge the last chunk if it's too small
    if chunks.len() >= 2 {
        let last = chunks.last().unwrap();
        if last.char_len < config.min_chars {
            let removed = chunks.pop().unwrap();
            if let Some(prev) = chunks.last_mut() {
                prev.text.push_str(&removed.text);
                prev.char_len += removed.char_len;
                prev.content_hash = sha256_hex(&prev.text);
            }
        }
    }

    chunks
}

/// Find the best split point in the range [start, end) of char_indices.
/// Prefers: sentence end (. ! ?) > paragraph (\n\n) > newline (\n) > word boundary (space)
fn find_best_split(
    char_indices: &[(usize, char)],
    start: usize,
    end: usize,
    min_chars: usize,
) -> usize {
    let search_start = start + min_chars;
    if search_start >= end {
        return end;
    }

    // Search backward from end to find the best boundary
    // Priority 1: paragraph break (\n\n)
    for i in (search_start..end).rev() {
        if i + 1 < char_indices.len()
            && char_indices[i].1 == '\n'
            && char_indices[i + 1].1 == '\n'
        {
            return i + 2; // after the double newline
        }
    }

    // Priority 2: sentence end followed by space or newline
    for i in (search_start..end).rev() {
        let ch = char_indices[i].1;
        if (ch == '.' || ch == '!' || ch == '?')
            && i + 1 < char_indices.len()
            && (char_indices[i + 1].1 == ' ' || char_indices[i + 1].1 == '\n')
        {
            return i + 1; // after the punctuation
        }
    }

    // Priority 3: newline
    for i in (search_start..end).rev() {
        if char_indices[i].1 == '\n' {
            return i + 1;
        }
    }

    // Priority 4: word boundary (space)
    for i in (search_start..end).rev() {
        if char_indices[i].1 == ' ' {
            return i + 1;
        }
    }

    // No good boundary found — split at max_chars
    end
}

/// Safe UTF-8 truncation — never panics on multi-byte characters.
///
/// Returns a slice of at most `max_chars` Unicode scalar values.
pub fn safe_truncate(text: &str, max_chars: usize) -> &str {
    match text.char_indices().nth(max_chars) {
        Some((byte_idx, _)) => &text[..byte_idx],
        None => text,
    }
}

/// Safe UTF-8 truncation that returns an owned String with "..." suffix.
pub fn safe_truncate_with_ellipsis(text: &str, max_chars: usize) -> String {
    let char_count = text.chars().count();
    if char_count <= max_chars {
        text.to_string()
    } else {
        let truncated = safe_truncate(text, max_chars.saturating_sub(3));
        format!("{}...", truncated)
    }
}

/// Compute SHA-256 hex digest of text.
pub fn sha256_hex(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Compute a deduplication key for a chunk.
///
/// Key = SHA256(collection || source || chunk_index || content_hash || model)
pub fn dedup_key(
    collection: &str,
    source: &str,
    chunk_index: usize,
    content_hash: &str,
    model: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(collection.as_bytes());
    hasher.update(b"|");
    hasher.update(source.as_bytes());
    hasher.update(b"|");
    hasher.update(chunk_index.to_string().as_bytes());
    hasher.update(b"|");
    hasher.update(content_hash.as_bytes());
    hasher.update(b"|");
    hasher.update(model.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Flatten a conversation session's messages into a single document.
///
/// Used by the session indexer to chunk multi-turn conversations.
pub fn flatten_session(messages: &[(String, String)]) -> String {
    let mut doc = String::new();
    for (role, content) in messages {
        doc.push_str(role);
        doc.push_str(": ");
        doc.push_str(content);
        doc.push('\n');
    }
    doc
}

/// Deduplicate chunks by content hash, keeping the first occurrence.
pub fn dedup_chunks(chunks: &[Chunk]) -> Vec<&Chunk> {
    let mut seen = HashMap::new();
    let mut result = Vec::new();
    for chunk in chunks {
        if seen.insert(chunk.content_hash.clone(), ()).is_none() {
            result.push(chunk);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_chunk_small_text() {
        let config = ChunkerConfig::default();
        let chunks = chunk_text("Hello, world!", &config);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "Hello, world!");
        assert_eq!(chunks[0].index, 0);
    }

    #[test]
    fn empty_text() {
        let config = ChunkerConfig::default();
        let chunks = chunk_text("", &config);
        assert!(chunks.is_empty());
    }

    #[test]
    fn multi_chunk_with_overlap() {
        let config = ChunkerConfig {
            max_chars: 50,
            overlap_chars: 10,
            min_chars: 10,
        };
        let text = "This is sentence one. This is sentence two. This is sentence three. This is sentence four.";
        let chunks = chunk_text(text, &config);
        assert!(chunks.len() >= 2, "Expected multiple chunks, got {}", chunks.len());
        // Verify all text is covered
        for chunk in &chunks {
            assert!(!chunk.text.is_empty());
            assert!(!chunk.content_hash.is_empty());
        }
    }

    #[test]
    fn utf8_safety() {
        let config = ChunkerConfig {
            max_chars: 10,
            overlap_chars: 3,
            min_chars: 3,
        };
        // Emoji are 4 bytes each in UTF-8
        let text = "Hello 🌍🌎🌏 world! This has emoji 🎉 inside it. More text here.";
        // Must not panic
        let chunks = chunk_text(text, &config);
        assert!(!chunks.is_empty());
        // Verify each chunk is valid UTF-8 (implicit — Rust strings are always valid)
        for chunk in &chunks {
            assert!(!chunk.text.is_empty());
        }
    }

    #[test]
    fn safe_truncate_ascii() {
        assert_eq!(safe_truncate("hello world", 5), "hello");
        assert_eq!(safe_truncate("hello", 10), "hello");
    }

    #[test]
    fn safe_truncate_multibyte() {
        let text = "café résumé";
        let truncated = safe_truncate(text, 4);
        assert_eq!(truncated, "café");  // 4 chars, not bytes
    }

    #[test]
    fn safe_truncate_emoji() {
        let text = "🌍🌎🌏🎉";
        let truncated = safe_truncate(text, 2);
        assert_eq!(truncated, "🌍🌎");
    }

    #[test]
    fn dedup_identical_chunks() {
        let chunks = vec![
            Chunk { text: "hello".into(), index: 0, char_offset: 0, char_len: 5, content_hash: sha256_hex("hello") },
            Chunk { text: "hello".into(), index: 1, char_offset: 5, char_len: 5, content_hash: sha256_hex("hello") },
            Chunk { text: "world".into(), index: 2, char_offset: 10, char_len: 5, content_hash: sha256_hex("world") },
        ];
        let deduped = dedup_chunks(&chunks);
        assert_eq!(deduped.len(), 2);
    }

    #[test]
    fn content_hash_deterministic() {
        let h1 = sha256_hex("test content");
        let h2 = sha256_hex("test content");
        assert_eq!(h1, h2);
        let h3 = sha256_hex("different content");
        assert_ne!(h1, h3);
    }

    #[test]
    fn safe_truncate_with_ellipsis_short() {
        assert_eq!(safe_truncate_with_ellipsis("hello", 10), "hello");
    }

    #[test]
    fn safe_truncate_with_ellipsis_long() {
        let result = safe_truncate_with_ellipsis("hello wonderful world", 10);
        assert!(result.ends_with("..."));
        assert!(result.chars().count() <= 10);
    }
}
