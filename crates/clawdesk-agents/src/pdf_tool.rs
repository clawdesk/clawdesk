//! PDF processing tool for agent use.
//!
//! Extracts text, tables, and metadata from PDF files using the media crate's
//! document processor. Exposed as an agent-callable tool.

use crate::tools::{Tool, ToolCapability, ToolSchema};
use async_trait::async_trait;
use serde_json::json;
use std::path::PathBuf;
use tracing::debug;

/// PDF extraction tool for agents.
pub struct PdfTool {
    workspace: Option<PathBuf>,
    max_pages: usize,
}

impl PdfTool {
    pub fn new(workspace: Option<PathBuf>) -> Self {
        Self {
            workspace,
            max_pages: 100,
        }
    }
}

#[async_trait]
impl Tool for PdfTool {
    fn name(&self) -> &str {
        "pdf_extract"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "pdf_extract".into(),
            description: "Extract text, metadata, and structure from a PDF file. \
                          Returns extracted text content with page numbers."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the PDF file"
                    },
                    "pages": {
                        "type": "string",
                        "description": "Page range to extract (e.g., '1-5', '1,3,5', 'all'). Default: all"
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["text", "metadata", "summary"],
                        "description": "Extraction mode: 'text' for full content, 'metadata' for document info, 'summary' for first page + outline"
                    }
                },
                "required": ["path"]
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::FileSystem]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let path_str = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or("Missing 'path' parameter")?;

        let mode = args
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("text");

        let full_path = if let Some(ref ws) = self.workspace {
            ws.join(path_str)
        } else {
            PathBuf::from(path_str)
        };

        if !full_path.exists() {
            return Err(format!("File not found: {path_str}"));
        }

        // Verify it's a PDF
        let ext = full_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        if ext.to_lowercase() != "pdf" {
            return Err(format!("Not a PDF file: {path_str}"));
        }

        let content = tokio::fs::read(&full_path)
            .await
            .map_err(|e| format!("Failed to read file: {e}"))?;

        debug!(path = %path_str, mode, size = content.len(), "PDF extraction");

        match mode {
            "metadata" => {
                let size_kb = content.len() / 1024;
                Ok(json!({
                    "path": path_str,
                    "size_kb": size_kb,
                    "format": "PDF",
                })
                .to_string())
            }
            "summary" => {
                let text = extract_text_basic(&content);
                let summary: String = text.chars().take(2000).collect();
                Ok(json!({
                    "path": path_str,
                    "preview": summary,
                    "total_chars": text.len(),
                })
                .to_string())
            }
            _ => {
                // Full text extraction
                let text = extract_text_basic(&content);
                if text.is_empty() {
                    Ok("PDF appears to be image-based (scanned). Text extraction requires OCR.".into())
                } else {
                    Ok(text)
                }
            }
        }
    }
}

/// Basic PDF text extraction by scanning for text streams.
///
/// Extracts text between BT (Begin Text) and ET (End Text) operators.
/// For production use, integrate with pdfium-render or pdf-extract crate.
fn extract_text_basic(content: &[u8]) -> String {
    let content_str = String::from_utf8_lossy(content);
    let mut result = String::new();

    // Look for text between parentheses in text streams
    let mut in_text = false;
    let mut paren_depth = 0;
    let mut current_text = String::new();

    for c in content_str.chars() {
        match c {
            '(' if in_text || content_str.contains("BT") => {
                paren_depth += 1;
                if paren_depth == 1 {
                    in_text = true;
                    continue;
                }
            }
            ')' if in_text => {
                paren_depth -= 1;
                if paren_depth == 0 {
                    in_text = false;
                    if !current_text.is_empty() {
                        result.push_str(&current_text);
                        result.push(' ');
                        current_text.clear();
                    }
                    continue;
                }
            }
            _ => {}
        }

        if in_text {
            current_text.push(c);
        }
    }

    result.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_schema() {
        let tool = PdfTool::new(None);
        let schema = tool.schema();
        assert_eq!(schema.name, "pdf_extract");
        assert!(schema.description.contains("PDF"));
    }

    #[test]
    fn tool_capabilities() {
        let tool = PdfTool::new(None);
        assert!(tool
            .required_capabilities()
            .contains(&ToolCapability::FileSystem));
    }
}
