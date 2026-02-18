//! Document processing — text extraction from PDF, DOCX, markdown, etc.

use serde::{Deserialize, Serialize};

/// Supported document formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DocumentFormat {
    PlainText,
    Markdown,
    Html,
    Pdf,
    Docx,
    Csv,
    Json,
    Xml,
    Unknown,
}

/// Document extraction result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentContent {
    pub text: String,
    pub format: DocumentFormat,
    pub page_count: Option<usize>,
    pub word_count: usize,
    pub metadata: DocumentMetadata,
}

/// Document metadata.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DocumentMetadata {
    pub title: Option<String>,
    pub author: Option<String>,
    pub created: Option<String>,
    pub modified: Option<String>,
    pub language: Option<String>,
}

/// Detect document format from extension and content.
pub fn detect_format(data: &[u8], filename: Option<&str>) -> DocumentFormat {
    // Check extension
    if let Some(name) = filename {
        let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
        match ext.as_str() {
            "txt" => return DocumentFormat::PlainText,
            "md" | "markdown" => return DocumentFormat::Markdown,
            "html" | "htm" => return DocumentFormat::Html,
            "pdf" => return DocumentFormat::Pdf,
            "docx" => return DocumentFormat::Docx,
            "csv" => return DocumentFormat::Csv,
            "json" => return DocumentFormat::Json,
            "xml" => return DocumentFormat::Xml,
            _ => {}
        }
    }

    // Check magic bytes
    if data.len() >= 5 && &data[..5] == b"%PDF-" {
        return DocumentFormat::Pdf;
    }
    if data.len() >= 4 && &data[..4] == [0x50, 0x4B, 0x03, 0x04] {
        // ZIP magic — could be DOCX
        if filename.map(|f| f.ends_with(".docx")).unwrap_or(false) {
            return DocumentFormat::Docx;
        }
    }

    // Try to detect text-based formats from content  
    if let Ok(text) = std::str::from_utf8(data) {
        if text.trim_start().starts_with('<') {
            if text.contains("<html") || text.contains("<HTML") {
                return DocumentFormat::Html;
            }
            return DocumentFormat::Xml;
        }
        if text.trim_start().starts_with('{') || text.trim_start().starts_with('[') {
            return DocumentFormat::Json;
        }
        if text.contains("# ") || text.contains("## ") || text.contains("```") {
            return DocumentFormat::Markdown;
        }
        return DocumentFormat::PlainText;
    }

    DocumentFormat::Unknown
}

/// Extract plain text from known formats.
/// For complex formats (PDF, DOCX), this provides a basic extraction.
/// Full extraction requires external libraries.
pub fn extract_text(data: &[u8], format: DocumentFormat) -> DocumentContent {
    let text = match format {
        DocumentFormat::PlainText | DocumentFormat::Markdown | DocumentFormat::Csv => {
            String::from_utf8_lossy(data).to_string()
        }
        DocumentFormat::Html => {
            // Basic HTML tag stripping
            strip_html_tags(&String::from_utf8_lossy(data))
        }
        DocumentFormat::Json => {
            // Pretty-print JSON for readability
            if let Ok(val) = serde_json::from_slice::<serde_json::Value>(data) {
                serde_json::to_string_pretty(&val).unwrap_or_else(|_| {
                    String::from_utf8_lossy(data).to_string()
                })
            } else {
                String::from_utf8_lossy(data).to_string()
            }
        }
        DocumentFormat::Xml => {
            String::from_utf8_lossy(data).to_string()
        }
        DocumentFormat::Pdf => {
            // Basic PDF text extraction: scan for text streams.
            // Full extraction requires a dedicated library like `pdf-extract`,
            // but we can extract plain text from many simple PDFs.
            extract_pdf_text(data)
        }
        DocumentFormat::Docx => {
            // DOCX is a ZIP containing XML. Extract text from word/document.xml.
            extract_docx_text(data)
        }
        DocumentFormat::Unknown => {
            String::from_utf8_lossy(data).to_string()
        }
    };

    let word_count = text.split_whitespace().count();

    DocumentContent {
        text,
        format,
        page_count: None,
        word_count,
        metadata: DocumentMetadata::default(),
    }
}

/// Basic HTML tag stripping (for simple documents).
fn strip_html_tags(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut in_tag = false;

    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                result.push(' '); // Replace tags with space
            }
            _ if !in_tag => result.push(ch),
            _ => {}
        }
    }

    // Collapse whitespace
    let collapsed: String = result
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    collapsed
}

/// Extract text from PDF data by scanning for text streams.
///
/// This is a lightweight extraction that finds text between BT/ET operators
/// and Tj/TJ commands. For complex PDFs with encodings, CID fonts, or
/// encrypted content, a full PDF library is needed.
fn extract_pdf_text(data: &[u8]) -> String {
    let text = String::from_utf8_lossy(data);
    let mut result = String::new();

    // Strategy 1: Find text between parentheses in Tj/TJ operators
    let mut i = 0;
    let bytes = text.as_bytes();
    while i < bytes.len() {
        // Look for '(' which starts a PDF text string
        if bytes[i] == b'(' {
            let start = i + 1;
            let mut depth = 1;
            i += 1;
            while i < bytes.len() && depth > 0 {
                match bytes[i] {
                    b'(' => depth += 1,
                    b')' => depth -= 1,
                    b'\\' => {
                        i += 1; // skip escaped char
                    }
                    _ => {}
                }
                i += 1;
            }
            if depth == 0 {
                let end = i - 1;
                if end > start {
                    let segment = &text[start..end];
                    // Filter out non-printable sequences
                    let clean: String = segment
                        .chars()
                        .filter(|c| c.is_ascii_graphic() || *c == ' ')
                        .collect();
                    if !clean.trim().is_empty() {
                        if !result.is_empty() {
                            result.push(' ');
                        }
                        result.push_str(clean.trim());
                    }
                }
            }
        } else {
            i += 1;
        }
    }

    if result.trim().is_empty() {
        format!(
            "[PDF document, {} bytes — contains encoded/image content not extractable without full PDF library]",
            data.len()
        )
    } else {
        result
    }
}

/// Extract text from DOCX data (ZIP archive containing XML).
///
/// DOCX files are ZIP archives. The main content is in word/document.xml.
/// We extract text from `<w:t>` elements.
fn extract_docx_text(data: &[u8]) -> String {
    // Check ZIP magic bytes
    if data.len() < 4 || data[..4] != [0x50, 0x4B, 0x03, 0x04] {
        return format!(
            "[DOCX document, {} bytes — invalid ZIP format]",
            data.len()
        );
    }

    // Simple ZIP parsing: find word/document.xml entry
    // ZIP local file headers start with PK\x03\x04
    let mut offset = 0;
    let mut document_xml: Option<Vec<u8>> = None;

    while offset + 30 <= data.len() {
        if data[offset..offset + 4] != [0x50, 0x4B, 0x03, 0x04] {
            break;
        }

        let compressed_size =
            u32::from_le_bytes([data[offset + 18], data[offset + 19], data[offset + 20], data[offset + 21]]) as usize;
        let uncompressed_size =
            u32::from_le_bytes([data[offset + 22], data[offset + 23], data[offset + 24], data[offset + 25]]) as usize;
        let name_len =
            u16::from_le_bytes([data[offset + 26], data[offset + 27]]) as usize;
        let extra_len =
            u16::from_le_bytes([data[offset + 28], data[offset + 29]]) as usize;

        let name_start = offset + 30;
        let name_end = name_start + name_len;
        if name_end > data.len() {
            break;
        }

        let name = String::from_utf8_lossy(&data[name_start..name_end]);
        let data_start = name_end + extra_len;
        let data_end = data_start + compressed_size;

        if name == "word/document.xml" && data_end <= data.len() {
            // For stored (uncompressed) entries, compressed_size == uncompressed_size
            if compressed_size == uncompressed_size && compressed_size > 0 {
                document_xml = Some(data[data_start..data_end].to_vec());
            }
            break;
        }

        offset = data_end;
    }

    match document_xml {
        Some(xml_data) => {
            let xml_str = String::from_utf8_lossy(&xml_data);
            // Extract text from <w:t> and <w:t xml:space="preserve"> elements
            let mut result = String::new();
            let mut in_wt = false;

            let mut chars = xml_str.chars().peekable();
            while let Some(ch) = chars.next() {
                if ch == '<' {
                    let mut tag = String::new();
                    for tc in chars.by_ref() {
                        if tc == '>' {
                            break;
                        }
                        tag.push(tc);
                    }
                    if tag.starts_with("w:t") && !tag.starts_with("w:tab") {
                        in_wt = true;
                    } else if tag == "/w:t" {
                        in_wt = false;
                    } else if tag == "/w:p" || tag.starts_with("w:br") {
                        result.push('\n');
                    }
                } else if in_wt {
                    result.push(ch);
                }
            }

            if result.trim().is_empty() {
                // Fallback: strip all XML tags
                strip_html_tags(&xml_str)
            } else {
                result
            }
        }
        None => {
            format!(
                "[DOCX document, {} bytes — word/document.xml not found or compressed]",
                data.len()
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_pdf_magic() {
        assert_eq!(detect_format(b"%PDF-1.7", None), DocumentFormat::Pdf);
    }

    #[test]
    fn detect_markdown_content() {
        let md = b"# Hello\n\nThis is **bold** text\n\n```rust\nfn main() {}\n```";
        assert_eq!(detect_format(md, None), DocumentFormat::Markdown);
    }

    #[test]
    fn detect_by_extension() {
        assert_eq!(detect_format(&[], Some("doc.csv")), DocumentFormat::Csv);
        assert_eq!(detect_format(&[], Some("page.html")), DocumentFormat::Html);
    }

    #[test]
    fn extract_plaintext() {
        let content = extract_text(b"Hello world", DocumentFormat::PlainText);
        assert_eq!(content.text, "Hello world");
        assert_eq!(content.word_count, 2);
    }

    #[test]
    fn strip_html() {
        let html = "<html><body><h1>Title</h1><p>Content here</p></body></html>";
        let text = strip_html_tags(html);
        assert!(text.contains("Title"));
        assert!(text.contains("Content here"));
        assert!(!text.contains("<h1>"));
    }

    #[test]
    fn extract_pdf_basic_text() {
        // Simulate a trivial PDF with text operators
        let fake_pdf = b"%PDF-1.4\nBT (Hello World) Tj ET\n%%EOF";
        let content = extract_text(fake_pdf, DocumentFormat::Pdf);
        assert!(
            content.text.contains("Hello World"),
            "Expected 'Hello World' in: {}",
            content.text,
        );
    }

    #[test]
    fn extract_pdf_empty_content() {
        // A PDF with no extractable text
        let binary_pdf = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\n%%EOF";
        let content = extract_text(binary_pdf, DocumentFormat::Pdf);
        // Should produce a fallback message
        assert!(!content.text.is_empty());
    }
}
