//! Multimodal memory — image and audio storage with cross-modal retrieval.
//!
//! Extends ClawDesk's text-only memory to support:
//! - CLIP-based image embeddings in the same vector space as text
//! - Whisper transcription + text embedding for audio
//! - Cross-modal queries: text queries find images, image queries find text
//!
//! ## Storage
//!
//! CLIP: shared 768-d space. 768 floats × 4 bytes = 3KB per embedding.
//! 10K images = ~30MB vector storage.
//!
//! Audio: Whisper transcription → text embedding + pointer to raw audio.
//! ~4KB per audio memory (transcription + embedding + metadata).
//!
//! ## Query
//!
//! HNSW index via SochDB: O(d × log n) per query.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use clawdesk_types::error::MemoryError;

// ─────────────────────────────────────────────────────────────────────────────
// Types
// ─────────────────────────────────────────────────────────────────────────────

/// Content modality for a memory entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Modality {
    /// Plain text.
    Text,
    /// Image (JPEG, PNG, WebP, etc.).
    Image,
    /// Audio (WAV, MP3, FLAC, etc.).
    Audio,
    /// Video (MP4, WebM) — stored as key frames + audio.
    Video,
}

/// A multimodal memory entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultimodalEntry {
    /// Unique ID for this memory.
    pub id: String,
    /// Primary modality.
    pub modality: Modality,
    /// The embedding vector (in shared CLIP/text space).
    pub embedding: Vec<f32>,
    /// Original text content (for text entries).
    pub text: Option<String>,
    /// Transcription (for audio entries, via Whisper).
    pub transcription: Option<String>,
    /// Reference to the raw media file (path or URI).
    pub media_ref: Option<String>,
    /// MIME type of the media.
    pub mime_type: Option<String>,
    /// Alt text or description (for images).
    pub description: Option<String>,
    /// When this entry was created.
    pub created_at: DateTime<Utc>,
    /// Arbitrary metadata.
    pub metadata: serde_json::Value,
}

/// Result of a cross-modal search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossModalResult {
    /// The matched entry.
    pub entry: MultimodalEntry,
    /// Similarity score.
    pub score: f32,
    /// Whether this was a cross-modal match (e.g., text query found image).
    pub cross_modal: bool,
}

/// Configuration for the multimodal memory system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultimodalConfig {
    /// Embedding dimension for CLIP (768) or text (1536).
    pub embedding_dim: usize,
    /// Whether to auto-transcribe audio on ingestion.
    pub auto_transcribe_audio: bool,
    /// Whether to auto-describe images on ingestion (via vision model).
    pub auto_describe_images: bool,
    /// Maximum image size for embedding (bytes).
    pub max_image_bytes: usize,
    /// Maximum audio duration for transcription (seconds).
    pub max_audio_duration_secs: u64,
}

impl Default for MultimodalConfig {
    fn default() -> Self {
        Self {
            embedding_dim: 768,
            auto_transcribe_audio: true,
            auto_describe_images: false,
            max_image_bytes: 20 * 1024 * 1024, // 20MB
            max_audio_duration_secs: 600,       // 10 minutes
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Embedding Provider trait
// ─────────────────────────────────────────────────────────────────────────────

/// Provider for generating multimodal embeddings (CLIP or similar).
#[async_trait]
pub trait MultimodalEmbedder: Send + Sync + 'static {
    /// Embed text into the shared vector space.
    async fn embed_text(&self, text: &str) -> Result<Vec<f32>, MemoryError>;

    /// Embed an image (as bytes) into the shared vector space.
    async fn embed_image(&self, image_bytes: &[u8], mime_type: &str) -> Result<Vec<f32>, MemoryError>;

    /// Embedding dimension.
    fn dimensions(&self) -> usize;
}

/// Audio transcription provider (Whisper API or local).
#[async_trait]
pub trait AudioTranscriber: Send + Sync + 'static {
    /// Transcribe audio bytes to text.
    async fn transcribe(&self, audio_bytes: &[u8], mime_type: &str) -> Result<String, MemoryError>;
}

// ─────────────────────────────────────────────────────────────────────────────
// Multimodal Memory Store
// ─────────────────────────────────────────────────────────────────────────────

/// In-memory multimodal store for cross-modal retrieval.
///
/// In production, this would be backed by SochDB's HNSW index.
/// This implementation provides the API surface and basic brute-force search.
pub struct MultimodalStore {
    config: MultimodalConfig,
    entries: Vec<MultimodalEntry>,
}

impl MultimodalStore {
    pub fn new(config: MultimodalConfig) -> Self {
        Self {
            config,
            entries: Vec::new(),
        }
    }

    /// Insert a pre-embedded entry.
    pub fn insert(&mut self, entry: MultimodalEntry) {
        self.entries.push(entry);
    }

    /// Search by embedding vector (cross-modal: works for text or image queries).
    ///
    /// O(n × d) brute-force — in production use SochDB HNSW for O(d × log n).
    pub fn search(
        &self,
        query_embedding: &[f32],
        top_k: usize,
        filter_modality: Option<Modality>,
    ) -> Vec<CrossModalResult> {
        let mut scored: Vec<CrossModalResult> = self
            .entries
            .iter()
            .filter(|e| filter_modality.map_or(true, |m| e.modality == m))
            .map(|e| {
                let score = cosine_similarity(query_embedding, &e.embedding);
                CrossModalResult {
                    entry: e.clone(),
                    score,
                    cross_modal: false, // Set by caller based on query modality
                }
            })
            .collect();

        scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(top_k);
        scored
    }

    /// Cross-modal search: mark results as cross-modal when query and result modalities differ.
    pub fn cross_modal_search(
        &self,
        query_embedding: &[f32],
        query_modality: Modality,
        top_k: usize,
    ) -> Vec<CrossModalResult> {
        let mut results = self.search(query_embedding, top_k, None);
        for result in &mut results {
            result.cross_modal = result.entry.modality != query_modality;
        }
        results
    }

    /// Get entries by modality.
    pub fn entries_by_modality(&self, modality: Modality) -> Vec<&MultimodalEntry> {
        self.entries.iter().filter(|e| e.modality == modality).collect()
    }

    /// Total entry count.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Statistics about the store.
    pub fn stats(&self) -> MultimodalStats {
        let mut text_count = 0usize;
        let mut image_count = 0usize;
        let mut audio_count = 0usize;

        for entry in &self.entries {
            match entry.modality {
                Modality::Text => text_count += 1,
                Modality::Image => image_count += 1,
                Modality::Audio => audio_count += 1,
                Modality::Video => {} // counted separately if needed
            }
        }

        let embedding_bytes = self.entries.len() * self.config.embedding_dim * 4;

        MultimodalStats {
            total_entries: self.entries.len(),
            text_count,
            image_count,
            audio_count,
            embedding_memory_bytes: embedding_bytes,
        }
    }
}

/// Statistics about multimodal memory usage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultimodalStats {
    pub total_entries: usize,
    pub text_count: usize,
    pub image_count: usize,
    pub audio_count: usize,
    pub embedding_memory_bytes: usize,
}

// ─────────────────────────────────────────────────────────────────────────────
// Math
// ─────────────────────────────────────────────────────────────────────────────

/// Cosine similarity between two vectors.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom == 0.0 {
        0.0
    } else {
        dot / denom
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(id: &str, modality: Modality, embedding: Vec<f32>) -> MultimodalEntry {
        MultimodalEntry {
            id: id.to_string(),
            modality,
            embedding,
            text: None,
            transcription: None,
            media_ref: None,
            mime_type: None,
            description: None,
            created_at: Utc::now(),
            metadata: serde_json::json!({}),
        }
    }

    #[test]
    fn cosine_similarity_identical() {
        let v = vec![1.0, 0.0, 0.0];
        assert!((cosine_similarity(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_orthogonal() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        assert!(cosine_similarity(&a, &b).abs() < 1e-6);
    }

    #[test]
    fn cross_modal_search_returns_all_modalities() {
        let mut store = MultimodalStore::new(MultimodalConfig::default());

        // Text entry and image entry with similar embeddings
        store.insert(make_entry("text1", Modality::Text, vec![1.0, 0.5, 0.0]));
        store.insert(make_entry("img1", Modality::Image, vec![0.9, 0.6, 0.1]));
        store.insert(make_entry("audio1", Modality::Audio, vec![0.1, 0.0, 1.0]));

        // Text query should find both text and image
        let query = vec![1.0, 0.5, 0.0];
        let results = store.cross_modal_search(&query, Modality::Text, 3);

        assert_eq!(results.len(), 3);
        // First result should be the text entry (exact match)
        assert_eq!(results[0].entry.id, "text1");
        assert!(!results[0].cross_modal); // same modality

        // Image should be cross-modal
        let img_result = results.iter().find(|r| r.entry.id == "img1").unwrap();
        assert!(img_result.cross_modal);
    }

    #[test]
    fn filter_by_modality() {
        let mut store = MultimodalStore::new(MultimodalConfig::default());
        store.insert(make_entry("text1", Modality::Text, vec![1.0, 0.0]));
        store.insert(make_entry("img1", Modality::Image, vec![0.9, 0.1]));

        let results = store.search(&[1.0, 0.0], 10, Some(Modality::Image));
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.modality, Modality::Image);
    }

    #[test]
    fn stats_tracking() {
        let mut store = MultimodalStore::new(MultimodalConfig {
            embedding_dim: 3,
            ..Default::default()
        });
        store.insert(make_entry("t1", Modality::Text, vec![1.0, 0.0, 0.0]));
        store.insert(make_entry("i1", Modality::Image, vec![0.0, 1.0, 0.0]));
        store.insert(make_entry("a1", Modality::Audio, vec![0.0, 0.0, 1.0]));

        let stats = store.stats();
        assert_eq!(stats.total_entries, 3);
        assert_eq!(stats.text_count, 1);
        assert_eq!(stats.image_count, 1);
        assert_eq!(stats.audio_count, 1);
        assert_eq!(stats.embedding_memory_bytes, 3 * 3 * 4); // 3 entries * 3 dims * 4 bytes
    }
}
