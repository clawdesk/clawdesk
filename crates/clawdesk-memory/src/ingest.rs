//! Multi-format memory ingestion pipeline.
//!
//! Accepts raw file uploads and processes them through a staged pipeline:
//! `RawInput → FormatDetector → Parser → Chunker → Deduplicator → BatchPipeline → VectorStore`
//!
//! ## Supported Formats
//! - WhatsApp `.txt` exports (timestamped multi-party chat)
//! - Telegram JSON exports (nested media objects, forwarded messages)
//! - Plain text / Markdown documents
//!
//! ## Chunking Strategy
//! Sliding-window with overlap: window W (default 512 tokens), stride S (default 384).
//! Produces ⌈(N − W) / S⌉ + 1 chunks. Overlap ensures no semantic boundary falls at a chunk edge.
//!
//! ## Deduplication
//! MinHash-based (k=128) with Jaccard threshold τ=0.85 for near-duplicate detection.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use tracing::{debug, info, warn};

// ─── Format Detection ────────────────────────────────────────────────────────

/// Detected input format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputFormat {
    WhatsAppExport,
    TelegramJson,
    PlainText,
    Markdown,
    Unknown,
}

/// Detect the format of raw input bytes.
pub fn detect_format(content: &str, filename: Option<&str>) -> InputFormat {
    if let Some(name) = filename {
        let lower = name.to_lowercase();
        if lower.contains("whatsapp") && lower.ends_with(".txt") {
            return InputFormat::WhatsAppExport;
        }
        if lower.ends_with(".json") {
            if content.contains("\"messages\"") && content.contains("\"type\"") {
                return InputFormat::TelegramJson;
            }
        }
        if lower.ends_with(".md") || lower.ends_with(".markdown") {
            return InputFormat::Markdown;
        }
    }

    // Content-based heuristics
    if content.starts_with('[')
        && (content.contains("AM] ") || content.contains("PM] ") || content.contains(", "))
    {
        // WhatsApp timestamp pattern: [1/15/24, 3:42:17 PM]
        return InputFormat::WhatsAppExport;
    }

    // Looks like a date/time prefix pattern typical of WhatsApp
    let first_line = content.lines().next().unwrap_or("");
    if first_line.starts_with('[')
        && first_line.contains(']')
        && first_line.contains(':')
    {
        return InputFormat::WhatsAppExport;
    }

    if content.trim_start().starts_with('{') && content.contains("\"messages\"") {
        return InputFormat::TelegramJson;
    }

    if content.contains("# ") || content.contains("## ") || content.contains("```") {
        return InputFormat::Markdown;
    }

    InputFormat::PlainText
}

// ─── Parsed Message ──────────────────────────────────────────────────────────

/// A single parsed message from a chat export.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedMessage {
    pub sender: String,
    pub content: String,
    pub timestamp: Option<String>,
    pub is_media: bool,
    pub reply_to: Option<String>,
}

/// Result of parsing an export file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParseResult {
    pub format: InputFormat,
    pub messages: Vec<ParsedMessage>,
    pub metadata: HashMap<String, String>,
    pub errors: Vec<String>,
}

// ─── WhatsApp Parser ─────────────────────────────────────────────────────────

/// Parse a WhatsApp `.txt` export.
///
/// Format: `[M/D/YY, H:MM:SS AM/PM] Sender: Message`
/// or: `M/D/YY, H:MM - Sender: Message`
pub fn parse_whatsapp(content: &str) -> ParseResult {
    let mut messages = Vec::new();
    let mut errors = Vec::new();
    let mut current_msg: Option<ParsedMessage> = None;
    let mut participants: HashSet<String> = HashSet::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Try to parse as a new message line
        if let Some(msg) = try_parse_whatsapp_line(line) {
            // Flush previous message
            if let Some(prev) = current_msg.take() {
                participants.insert(prev.sender.clone());
                messages.push(prev);
            }
            current_msg = Some(msg);
        } else if let Some(ref mut msg) = current_msg {
            // Continuation line — append to current message
            msg.content.push('\n');
            msg.content.push_str(line);
        } else {
            // Orphan line before any message
            errors.push(format!("orphan line: {}", &line[..line.len().min(50)]));
        }
    }

    // Flush last message
    if let Some(msg) = current_msg {
        participants.insert(msg.sender.clone());
        messages.push(msg);
    }

    let mut metadata = HashMap::new();
    metadata.insert("participants".to_string(), participants.len().to_string());
    metadata.insert("total_messages".to_string(), messages.len().to_string());

    info!(
        messages = messages.len(),
        participants = participants.len(),
        "parsed WhatsApp export"
    );

    ParseResult {
        format: InputFormat::WhatsAppExport,
        messages,
        metadata,
        errors,
    }
}

fn try_parse_whatsapp_line(line: &str) -> Option<ParsedMessage> {
    // Pattern 1: [M/D/YY, H:MM:SS AM/PM] Sender: Message
    if line.starts_with('[') {
        let bracket_end = line.find(']')?;
        let timestamp = line[1..bracket_end].to_string();
        let rest = line[bracket_end + 1..].trim_start();

        // Split on first ": "
        let colon_pos = rest.find(": ")?;
        let sender = rest[..colon_pos].to_string();
        let content = rest[colon_pos + 2..].to_string();

        let is_media = content.contains("<Media omitted>")
            || content.contains("<attached:")
            || content.contains("image omitted")
            || content.contains("video omitted")
            || content.contains("audio omitted");

        // Skip system messages
        if sender.contains("Messages and calls are end-to-end encrypted")
            || sender.contains("created group")
            || sender.contains("added")
            || sender.contains("changed the")
        {
            return None;
        }

        return Some(ParsedMessage {
            sender,
            content,
            timestamp: Some(timestamp),
            is_media,
            reply_to: None,
        });
    }

    // Pattern 2: M/D/YY, H:MM - Sender: Message
    if let Some(dash_pos) = line.find(" - ") {
        let maybe_date = &line[..dash_pos];
        // Simple check: contains / and : (date and time)
        if maybe_date.contains('/') && maybe_date.contains(':') {
            let rest = &line[dash_pos + 3..];
            if let Some(colon_pos) = rest.find(": ") {
                let sender = rest[..colon_pos].to_string();
                let content = rest[colon_pos + 2..].to_string();
                let is_media = content.contains("<Media omitted>");

                return Some(ParsedMessage {
                    sender,
                    content,
                    timestamp: Some(maybe_date.to_string()),
                    is_media,
                    reply_to: None,
                });
            }
        }
    }

    None
}

// ─── Telegram JSON Parser ────────────────────────────────────────────────────

/// Parse a Telegram JSON export.
///
/// Expected structure: `{ "messages": [{ "from": "...", "text": "...", "date": "..." }] }`
pub fn parse_telegram_json(content: &str) -> ParseResult {
    let mut messages = Vec::new();
    let mut errors = Vec::new();

    let parsed: serde_json::Value = match serde_json::from_str(content) {
        Ok(v) => v,
        Err(e) => {
            return ParseResult {
                format: InputFormat::TelegramJson,
                messages: vec![],
                metadata: HashMap::new(),
                errors: vec![format!("JSON parse error: {e}")],
            };
        }
    };

    let msgs_arr = parsed
        .get("messages")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    for msg_val in &msgs_arr {
        let msg_type = msg_val
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        // Only process actual messages, skip service messages
        if msg_type != "message" {
            continue;
        }

        let sender = msg_val
            .get("from")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown")
            .to_string();

        let timestamp = msg_val
            .get("date")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // Text can be a string or an array of text entities
        let content = extract_telegram_text(msg_val);

        if content.is_empty() {
            continue;
        }

        let is_media = msg_val.get("photo").is_some()
            || msg_val.get("file").is_some()
            || msg_val.get("media_type").is_some();

        let reply_to = msg_val
            .get("reply_to_message_id")
            .and_then(|v| v.as_i64())
            .map(|id| id.to_string());

        messages.push(ParsedMessage {
            sender,
            content,
            timestamp,
            is_media,
            reply_to,
        });
    }

    let mut metadata = HashMap::new();
    metadata.insert("total_messages".to_string(), messages.len().to_string());
    if let Some(name) = parsed.get("name").and_then(|v| v.as_str()) {
        metadata.insert("chat_name".to_string(), name.to_string());
    }

    info!(messages = messages.len(), "parsed Telegram JSON export");

    ParseResult {
        format: InputFormat::TelegramJson,
        messages,
        metadata,
        errors,
    }
}

fn extract_telegram_text(msg: &serde_json::Value) -> String {
    match msg.get("text") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(arr)) => {
            // Telegram rich text: array of strings and objects
            let mut text = String::new();
            for item in arr {
                match item {
                    serde_json::Value::String(s) => text.push_str(s),
                    serde_json::Value::Object(obj) => {
                        if let Some(t) = obj.get("text").and_then(|v| v.as_str()) {
                            text.push_str(t);
                        }
                    }
                    _ => {}
                }
            }
            text
        }
        _ => String::new(),
    }
}

// ─── Document Chunker ────────────────────────────────────────────────────────

/// Configuration for the document chunker.
#[derive(Debug, Clone)]
pub struct ChunkerConfig {
    /// Window size in approximate tokens (default: 512).
    pub window_tokens: usize,
    /// Stride in approximate tokens (default: 384).
    pub stride_tokens: usize,
    /// Characters per token approximation (default: 4).
    pub chars_per_token: usize,
}

impl Default for ChunkerConfig {
    fn default() -> Self {
        Self {
            window_tokens: 512,
            stride_tokens: 384,
            chars_per_token: 4,
        }
    }
}

/// A chunk of a document with source lineage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentChunk {
    /// The text content of this chunk.
    pub content: String,
    /// Source file path or identifier.
    pub source_file: String,
    /// Byte offset in the original document.
    pub byte_offset: usize,
    /// Sequential chunk index (0-based).
    pub chunk_index: usize,
    /// Total number of chunks from this document.
    pub total_chunks: usize,
}

/// Chunk a document using sliding window with overlap.
///
/// Produces ⌈(N − W) / S⌉ + 1 chunks where N = document length,
/// W = window size, S = stride.
pub fn chunk_document(
    content: &str,
    source_file: &str,
    config: &ChunkerConfig,
) -> Vec<DocumentChunk> {
    let window_chars = config.window_tokens * config.chars_per_token;
    let stride_chars = config.stride_tokens * config.chars_per_token;

    if content.len() <= window_chars {
        return vec![DocumentChunk {
            content: content.to_string(),
            source_file: source_file.to_string(),
            byte_offset: 0,
            chunk_index: 0,
            total_chunks: 1,
        }];
    }

    let mut chunks = Vec::new();
    let mut offset = 0;

    while offset < content.len() {
        let end = (offset + window_chars).min(content.len());

        // Snap to a word boundary (don't break mid-word)
        let actual_end = if end < content.len() {
            content[..end]
                .rfind(|c: char| c.is_whitespace())
                .unwrap_or(end)
                .max(offset + 1)
        } else {
            end
        };

        chunks.push(DocumentChunk {
            content: content[offset..actual_end].to_string(),
            source_file: source_file.to_string(),
            byte_offset: offset,
            chunk_index: chunks.len(),
            total_chunks: 0, // filled below
        });

        offset += stride_chars;
        if offset + stride_chars / 2 >= content.len() {
            // Don't create a tiny trailing chunk
            break;
        }
    }

    // If the last chunk didn't reach the end, add a final chunk
    if let Some(last) = chunks.last() {
        let last_end = last.byte_offset + last.content.len();
        if last_end < content.len() {
            let final_start = content.len().saturating_sub(window_chars);
            chunks.push(DocumentChunk {
                content: content[final_start..].to_string(),
                source_file: source_file.to_string(),
                byte_offset: final_start,
                chunk_index: chunks.len(),
                total_chunks: 0,
            });
        }
    }

    let total = chunks.len();
    for chunk in &mut chunks {
        chunk.total_chunks = total;
    }

    debug!(
        source = %source_file,
        total_chunks = total,
        doc_len = content.len(),
        "chunked document"
    );

    chunks
}

// ─── MinHash Deduplication ───────────────────────────────────────────────────

/// MinHash signature for near-duplicate detection.
#[derive(Debug, Clone)]
pub struct MinHashSignature {
    pub hashes: Vec<u64>,
}

/// Configuration for MinHash deduplication.
#[derive(Debug, Clone)]
pub struct DeduplicationConfig {
    /// Number of hash permutations (default: 128).
    pub num_permutations: usize,
    /// Jaccard similarity threshold for considering duplicates (default: 0.85).
    pub similarity_threshold: f64,
    /// Shingle size in words (default: 3).
    pub shingle_size: usize,
}

impl Default for DeduplicationConfig {
    fn default() -> Self {
        Self {
            num_permutations: 128,
            similarity_threshold: 0.85,
            shingle_size: 3,
        }
    }
}

/// Compute MinHash signature for a text.
pub fn compute_minhash(text: &str, config: &DeduplicationConfig) -> MinHashSignature {
    let words: Vec<&str> = text.split_whitespace().collect();
    let shingles: HashSet<String> = if words.len() >= config.shingle_size {
        (0..=words.len() - config.shingle_size)
            .map(|i| words[i..i + config.shingle_size].join(" "))
            .collect()
    } else {
        let mut s = HashSet::new();
        s.insert(text.to_string());
        s
    };

    let mut hashes = Vec::with_capacity(config.num_permutations);

    for perm in 0..config.num_permutations {
        let mut min_hash = u64::MAX;
        for shingle in &shingles {
            let h = hash_shingle(shingle, perm as u64);
            min_hash = min_hash.min(h);
        }
        hashes.push(min_hash);
    }

    MinHashSignature { hashes }
}

/// Estimate Jaccard similarity between two MinHash signatures.
pub fn jaccard_similarity(a: &MinHashSignature, b: &MinHashSignature) -> f64 {
    assert_eq!(a.hashes.len(), b.hashes.len());
    let matches = a
        .hashes
        .iter()
        .zip(&b.hashes)
        .filter(|(x, y)| x == y)
        .count();
    matches as f64 / a.hashes.len() as f64
}

/// Deduplicate chunks using MinHash similarity.
/// Returns indices of chunks to keep (non-duplicates).
pub fn deduplicate_chunks(
    chunks: &[DocumentChunk],
    config: &DeduplicationConfig,
) -> Vec<usize> {
    let signatures: Vec<MinHashSignature> = chunks
        .iter()
        .map(|c| compute_minhash(&c.content, config))
        .collect();

    let mut keep = Vec::new();
    let mut is_dup = vec![false; chunks.len()];

    for i in 0..chunks.len() {
        if is_dup[i] {
            continue;
        }
        keep.push(i);

        // Mark near-duplicates
        for j in (i + 1)..chunks.len() {
            if !is_dup[j] {
                let sim = jaccard_similarity(&signatures[i], &signatures[j]);
                if sim >= config.similarity_threshold {
                    is_dup[j] = true;
                    debug!(
                        chunk_i = i,
                        chunk_j = j,
                        similarity = sim,
                        "dedup: marking chunk as duplicate"
                    );
                }
            }
        }
    }

    info!(
        original = chunks.len(),
        kept = keep.len(),
        removed = chunks.len() - keep.len(),
        "deduplication complete"
    );

    keep
}

fn hash_shingle(shingle: &str, seed: u64) -> u64 {
    // FNV-1a variant with seed mixing
    let mut hash: u64 = 14695981039346656037u64.wrapping_add(seed.wrapping_mul(6364136223846793005));
    for byte in shingle.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(1099511628211);
    }
    hash
}

// ─── Ingestion Pipeline ──────────────────────────────────────────────────────

/// Full ingestion pipeline result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestionResult {
    pub format: InputFormat,
    pub total_messages: usize,
    pub total_chunks: usize,
    pub chunks_after_dedup: usize,
    pub source_file: String,
    pub errors: Vec<String>,
}

/// Run the full ingestion pipeline on raw content.
///
/// Pipeline: detect format → parse → chunk → deduplicate → return chunks ready for embedding.
pub fn ingest(
    content: &str,
    source_file: &str,
    filename: Option<&str>,
    chunker_config: &ChunkerConfig,
    dedup_config: &DeduplicationConfig,
) -> (Vec<DocumentChunk>, IngestionResult) {
    let format = detect_format(content, filename);

    // Parse based on format
    let text_to_chunk = match format {
        InputFormat::WhatsAppExport => {
            let result = parse_whatsapp(content);
            // Convert messages to a single text for chunking
            let mut text = String::new();
            for msg in &result.messages {
                if !msg.is_media {
                    if let Some(ref ts) = msg.timestamp {
                        text.push_str(&format!("[{}] {}: {}\n", ts, msg.sender, msg.content));
                    } else {
                        text.push_str(&format!("{}: {}\n", msg.sender, msg.content));
                    }
                }
            }
            text
        }
        InputFormat::TelegramJson => {
            let result = parse_telegram_json(content);
            let mut text = String::new();
            for msg in &result.messages {
                if let Some(ref ts) = msg.timestamp {
                    text.push_str(&format!("[{}] {}: {}\n", ts, msg.sender, msg.content));
                } else {
                    text.push_str(&format!("{}: {}\n", msg.sender, msg.content));
                }
            }
            text
        }
        _ => content.to_string(),
    };

    // Chunk
    let chunks = chunk_document(&text_to_chunk, source_file, chunker_config);
    let total_chunks = chunks.len();

    // Deduplicate
    let keep_indices = deduplicate_chunks(&chunks, dedup_config);
    let deduped: Vec<DocumentChunk> = keep_indices.into_iter().map(|i| chunks[i].clone()).collect();
    let deduped_count = deduped.len();

    let result = IngestionResult {
        format,
        total_messages: 0,
        total_chunks,
        chunks_after_dedup: deduped_count,
        source_file: source_file.to_string(),
        errors: vec![],
    };

    (deduped, result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_whatsapp_by_content() {
        let content = "[1/15/24, 3:42:17 PM] Alice: Hello there!";
        assert_eq!(detect_format(content, None), InputFormat::WhatsAppExport);
    }

    #[test]
    fn detect_telegram_json() {
        let content = r#"{"messages": [{"type": "message", "from": "Bob", "text": "hi"}]}"#;
        assert_eq!(detect_format(content, Some("export.json")), InputFormat::TelegramJson);
    }

    #[test]
    fn detect_markdown() {
        let content = "# My Notes\n\n## Section 1\n\nSome content here.";
        assert_eq!(detect_format(content, Some("notes.md")), InputFormat::Markdown);
    }

    #[test]
    fn parse_whatsapp_messages() {
        let content = "\
[1/15/24, 3:42:17 PM] Alice: Hello there!
[1/15/24, 3:42:30 PM] Bob: Hi Alice, how are you?
[1/15/24, 3:43:00 PM] Alice: I'm good, thanks!
This is a continuation line.";

        let result = parse_whatsapp(content);
        assert_eq!(result.messages.len(), 3);
        assert_eq!(result.messages[0].sender, "Alice");
        assert_eq!(result.messages[0].content, "Hello there!");
        assert_eq!(result.messages[2].sender, "Alice");
        assert!(result.messages[2].content.contains("continuation line"));
    }

    #[test]
    fn parse_whatsapp_media_detected() {
        let content = "[1/1/24, 12:00:00 PM] User: <Media omitted>";
        let result = parse_whatsapp(content);
        assert_eq!(result.messages.len(), 1);
        assert!(result.messages[0].is_media);
    }

    #[test]
    fn parse_telegram_json_messages() {
        let json = r#"{
            "name": "Test Chat",
            "messages": [
                {"type": "message", "from": "Alice", "text": "Hello", "date": "2024-01-15T15:42:17"},
                {"type": "message", "from": "Bob", "text": "Hi!", "date": "2024-01-15T15:42:30"},
                {"type": "service", "action": "phone_call"}
            ]
        }"#;
        let result = parse_telegram_json(json);
        assert_eq!(result.messages.len(), 2);
        assert_eq!(result.messages[0].sender, "Alice");
        assert_eq!(result.metadata.get("chat_name").unwrap(), "Test Chat");
    }

    #[test]
    fn chunk_small_document() {
        let content = "Hello world. This is a small document.";
        let chunks = chunk_document(content, "test.txt", &ChunkerConfig::default());
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].chunk_index, 0);
        assert_eq!(chunks[0].total_chunks, 1);
    }

    #[test]
    fn chunk_large_document() {
        // Create a document > 512 * 4 = 2048 chars
        let content = "word ".repeat(600); // 3000 chars
        let config = ChunkerConfig {
            window_tokens: 100,
            stride_tokens: 75,
            chars_per_token: 4,
        };
        let chunks = chunk_document(&content, "big.txt", &config);
        assert!(chunks.len() > 1);

        // All chunks carry correct lineage
        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.source_file, "big.txt");
            assert_eq!(chunk.chunk_index, i);
            assert_eq!(chunk.total_chunks, chunks.len());
        }
    }

    #[test]
    fn minhash_identical_texts() {
        let config = DeduplicationConfig::default();
        let sig1 = compute_minhash("hello world foo bar", &config);
        let sig2 = compute_minhash("hello world foo bar", &config);
        assert!((jaccard_similarity(&sig1, &sig2) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn minhash_different_texts() {
        let config = DeduplicationConfig::default();
        let sig1 = compute_minhash("the quick brown fox jumps over the lazy dog", &config);
        let sig2 = compute_minhash("completely different text about something else entirely", &config);
        assert!(jaccard_similarity(&sig1, &sig2) < 0.5);
    }

    #[test]
    fn deduplicate_removes_near_duplicates() {
        let config = DeduplicationConfig {
            similarity_threshold: 0.8,
            ..Default::default()
        };
        let chunks = vec![
            DocumentChunk {
                content: "the quick brown fox jumps over the lazy dog and runs".to_string(),
                source_file: "test.txt".to_string(),
                byte_offset: 0,
                chunk_index: 0,
                total_chunks: 3,
            },
            DocumentChunk {
                content: "the quick brown fox jumps over the lazy dog and runs".to_string(), // exact dup
                source_file: "test.txt".to_string(),
                byte_offset: 100,
                chunk_index: 1,
                total_chunks: 3,
            },
            DocumentChunk {
                content: "completely different content about quantum physics and mathematics".to_string(),
                source_file: "test.txt".to_string(),
                byte_offset: 200,
                chunk_index: 2,
                total_chunks: 3,
            },
        ];

        let kept = deduplicate_chunks(&chunks, &config);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0], 0);
        assert_eq!(kept[1], 2);
    }

    #[test]
    fn full_pipeline_plaintext() {
        let content = "This is a plain text document for ingestion testing.";
        let (chunks, result) = ingest(
            content,
            "doc.txt",
            Some("doc.txt"),
            &ChunkerConfig::default(),
            &DeduplicationConfig::default(),
        );
        assert!(!chunks.is_empty());
        assert_eq!(result.format, InputFormat::PlainText);
        assert!(result.errors.is_empty());
    }
}
