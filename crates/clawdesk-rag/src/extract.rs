//! Document text extraction for various file formats.

use std::path::Path;
use tracing::info;

/// Supported document types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocType {
    Pdf,
    Text,
    Markdown,
    Csv,
}

impl DocType {
    /// Detect document type from file extension.
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_lowercase();
        match ext.as_str() {
            "pdf" => Some(Self::Pdf),
            "txt" | "log" => Some(Self::Text),
            "md" | "markdown" => Some(Self::Markdown),
            "csv" => Some(Self::Csv),
            _ => None,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Pdf => "PDF",
            Self::Text => "Text",
            Self::Markdown => "Markdown",
            Self::Csv => "CSV",
        }
    }
}

/// Extract plain text from a document file.
pub fn extract_text(path: &Path) -> Result<String, String> {
    let doc_type = DocType::from_path(path)
        .ok_or_else(|| format!("Unsupported file type: {}", path.display()))?;

    info!(path = %path.display(), doc_type = ?doc_type, "extracting text");

    match doc_type {
        DocType::Pdf => extract_pdf(path),
        DocType::Text | DocType::Markdown | DocType::Csv => extract_plaintext(path),
    }
}

fn extract_pdf(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("Failed to read PDF: {}", e))?;
    pdf_extract::extract_text_from_mem(&bytes)
        .map_err(|e| format!("Failed to extract PDF text: {}", e))
}

fn extract_plaintext(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("Failed to read file: {}", e))
}
