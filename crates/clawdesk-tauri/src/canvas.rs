//! Canvas workspace — structured content blocks for collaborative editing.
//!
//! The canvas provides a block-based workspace where users can organize
//! AI-generated content, code snippets, images, and notes into a spatial layout.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Unique identifier for a canvas block.
pub type BlockId = String;

/// Canvas workspace containing blocks in a spatial layout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Canvas {
    pub id: String,
    pub title: String,
    pub blocks: Vec<Block>,
    pub connections: Vec<Connection>,
    pub created_at: String,
    pub updated_at: String,
}

/// A content block in the canvas.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    pub id: BlockId,
    pub block_type: BlockType,
    pub content: String,
    pub position: Position,
    pub size: Size,
    pub metadata: BlockMetadata,
}

/// Block content type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockType {
    Text,
    Code,
    Markdown,
    Image,
    Chat,
    Note,
    Table,
    Diagram,
}

impl BlockType {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Text => "Text",
            Self::Code => "Code",
            Self::Markdown => "Markdown",
            Self::Image => "Image",
            Self::Chat => "Chat",
            Self::Note => "Note",
            Self::Table => "Table",
            Self::Diagram => "Diagram",
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            Self::Text => "📝",
            Self::Code => "💻",
            Self::Markdown => "📄",
            Self::Image => "🖼️",
            Self::Chat => "💬",
            Self::Note => "📌",
            Self::Table => "📊",
            Self::Diagram => "📐",
        }
    }
}

/// 2D position.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Position {
    pub x: f64,
    pub y: f64,
}

/// Block size.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Size {
    pub width: f64,
    pub height: f64,
}

/// Block metadata.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BlockMetadata {
    pub language: Option<String>,
    pub model: Option<String>,
    pub editable: bool,
    pub pinned: bool,
    pub tags: Vec<String>,
}

/// Connection between two blocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Connection {
    pub from_block: BlockId,
    pub to_block: BlockId,
    pub label: Option<String>,
}

impl Canvas {
    /// Create a new empty canvas.
    pub fn new(title: &str) -> Self {
        Self {
            id: format!("canvas-{}", uuid::Uuid::new_v4()),
            title: title.to_string(),
            blocks: Vec::new(),
            connections: Vec::new(),
            created_at: timestamp_now(),
            updated_at: timestamp_now(),
        }
    }

    /// Add a block.
    pub fn add_block(&mut self, block: Block) {
        self.blocks.push(block);
        self.updated_at = timestamp_now();
    }

    /// Remove a block by ID.
    pub fn remove_block(&mut self, id: &str) -> Option<Block> {
        if let Some(pos) = self.blocks.iter().position(|b| b.id == id) {
            self.connections.retain(|c| c.from_block != id && c.to_block != id);
            self.updated_at = timestamp_now();
            Some(self.blocks.remove(pos))
        } else {
            None
        }
    }

    /// Find block by ID.
    pub fn get_block(&self, id: &str) -> Option<&Block> {
        self.blocks.iter().find(|b| b.id == id)
    }

    /// Find block by ID (mutable).
    pub fn get_block_mut(&mut self, id: &str) -> Option<&mut Block> {
        self.blocks.iter_mut().find(|b| b.id == id)
    }

    /// Connect two blocks.
    pub fn connect(&mut self, from: &str, to: &str, label: Option<String>) {
        self.connections.push(Connection {
            from_block: from.to_string(),
            to_block: to.to_string(),
            label,
        });
        self.updated_at = timestamp_now();
    }

    /// Get all blocks of a specific type.
    pub fn blocks_of_type(&self, block_type: BlockType) -> Vec<&Block> {
        self.blocks.iter().filter(|b| b.block_type == block_type).collect()
    }

    /// Export canvas as Markdown.
    pub fn to_markdown(&self) -> String {
        let mut md = format!("# {}\n\n", self.title);
        for block in &self.blocks {
            match block.block_type {
                BlockType::Code => {
                    let lang = block.metadata.language.as_deref().unwrap_or("");
                    md.push_str(&format!("```{}\n{}\n```\n\n", lang, block.content));
                }
                BlockType::Image => {
                    md.push_str(&format!("![{}]({})\n\n", block.id, block.content));
                }
                _ => {
                    md.push_str(&format!("{}\n\n", block.content));
                }
            }
        }
        md
    }

    /// Total number of blocks.
    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }
}

impl Block {
    /// Create a new block.
    pub fn new(block_type: BlockType, content: &str, x: f64, y: f64) -> Self {
        Self {
            id: format!("block-{}", uuid::Uuid::new_v4()),
            block_type,
            content: content.to_string(),
            position: Position { x, y },
            size: Size {
                width: 300.0,
                height: 200.0,
            },
            metadata: BlockMetadata {
                editable: true,
                ..Default::default()
            },
        }
    }

    /// Create a code block with language.
    pub fn code(content: &str, language: &str, x: f64, y: f64) -> Self {
        let mut block = Self::new(BlockType::Code, content, x, y);
        block.metadata.language = Some(language.to_string());
        block
    }
}

/// Simple timestamp.
fn timestamp_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{}", secs)
}

/// Generate a proper UUID v4 identifier.
fn uuid_stub() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_canvas() {
        let c = Canvas::new("Test Canvas");
        assert_eq!(c.title, "Test Canvas");
        assert_eq!(c.block_count(), 0);
    }

    #[test]
    fn add_and_remove_blocks() {
        let mut c = Canvas::new("Test");
        let block = Block::new(BlockType::Text, "Hello", 0.0, 0.0);
        let id = block.id.clone();
        c.add_block(block);
        assert_eq!(c.block_count(), 1);

        c.remove_block(&id);
        assert_eq!(c.block_count(), 0);
    }

    #[test]
    fn code_block_with_language() {
        let block = Block::code("fn main() {}", "rust", 10.0, 20.0);
        assert_eq!(block.block_type, BlockType::Code);
        assert_eq!(block.metadata.language.as_deref(), Some("rust"));
    }

    #[test]
    fn canvas_connections() {
        let mut c = Canvas::new("Test");
        c.add_block(Block::new(BlockType::Text, "A", 0.0, 0.0));
        c.add_block(Block::new(BlockType::Text, "B", 100.0, 0.0));
        let id_a = c.blocks[0].id.clone();
        let id_b = c.blocks[1].id.clone();

        c.connect(&id_a, &id_b, Some("flow".into()));
        assert_eq!(c.connections.len(), 1);

        // Removing a block removes its connections
        c.remove_block(&id_a);
        assert_eq!(c.connections.len(), 0);
    }

    #[test]
    fn to_markdown() {
        let mut c = Canvas::new("Export Test");
        c.add_block(Block::new(BlockType::Text, "Some text content", 0.0, 0.0));
        c.add_block(Block::code("let x = 1;", "rust", 0.0, 100.0));

        let md = c.to_markdown();
        assert!(md.contains("# Export Test"));
        assert!(md.contains("Some text content"));
        assert!(md.contains("```rust"));
    }

    #[test]
    fn blocks_of_type_filter() {
        let mut c = Canvas::new("Filter Test");
        c.add_block(Block::new(BlockType::Text, "A", 0.0, 0.0));
        c.add_block(Block::code("B", "js", 0.0, 100.0));
        c.add_block(Block::new(BlockType::Text, "C", 0.0, 200.0));

        assert_eq!(c.blocks_of_type(BlockType::Text).len(), 2);
        assert_eq!(c.blocks_of_type(BlockType::Code).len(), 1);
    }

    #[test]
    fn block_type_labels() {
        assert_eq!(BlockType::Code.label(), "Code");
        assert_eq!(BlockType::Text.label(), "Text");
    }
}
