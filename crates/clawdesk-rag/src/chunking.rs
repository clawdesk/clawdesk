//! Text chunking for RAG document ingestion.

use serde::{Deserialize, Serialize};

/// A chunk of text from a document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextChunk {
    pub index: usize,
    pub text: String,
    pub offset: usize,
}

/// Chunking configuration.
#[derive(Debug, Clone)]
pub struct ChunkConfig {
    pub chunk_size: usize,
    pub overlap: usize,
    pub max_chunks: usize,
}

impl Default for ChunkConfig {
    fn default() -> Self {
        Self {
            chunk_size: 800,
            overlap: 200,
            max_chunks: 2000,
        }
    }
}

/// Split text into overlapping chunks with sentence-boundary awareness.
pub fn chunk_text(text: &str, config: &ChunkConfig) -> Vec<TextChunk> {
    let text = text.trim();
    if text.is_empty() {
        return Vec::new();
    }
    if text.len() <= config.chunk_size {
        return vec![TextChunk { index: 0, text: text.to_string(), offset: 0 }];
    }

    let mut chunks = Vec::new();
    let mut start = 0;

    while start < text.len() && chunks.len() < config.max_chunks {
        let end = snap_to_char_boundary(text, (start + config.chunk_size).min(text.len()));
        let actual_end = if end < text.len() {
            find_break_point(text, start, end)
        } else {
            end
        };

        let chunk_text = text[start..actual_end].trim().to_string();
        if !chunk_text.is_empty() {
            chunks.push(TextChunk { index: chunks.len(), text: chunk_text, offset: start });
        }

        let advance = if actual_end > start + config.overlap {
            actual_end - start - config.overlap
        } else {
            (actual_end - start).max(1)
        };
        start += advance;
        start = snap_to_char_boundary(text, start);
    }

    chunks
}

/// Snap a byte offset backward to the nearest char boundary.
fn snap_to_char_boundary(text: &str, pos: usize) -> usize {
    let mut p = pos.min(text.len());
    while p > 0 && !text.is_char_boundary(p) {
        p -= 1;
    }
    p
}

fn find_break_point(text: &str, start: usize, target: usize) -> usize {
    let search_start = snap_to_char_boundary(text, if target > start + 100 { target - 100 } else { start });
    let target = snap_to_char_boundary(text, target);
    let slice = &text[search_start..target];
    let patterns = [". ", ".\n", "! ", "? ", "\n\n"];
    let mut best = None;
    for pat in &patterns {
        if let Some(pos) = slice.rfind(pat) {
            let abs = search_start + pos + pat.len();
            if best.is_none() || abs > best.unwrap() {
                best = Some(abs);
            }
        }
    }
    best.unwrap_or(target)
}
