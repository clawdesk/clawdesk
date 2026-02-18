//! Page context — DOM queries, text extraction, content summarization.

use serde::{Deserialize, Serialize};

/// Extracted page content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageContext {
    pub url: String,
    pub title: String,
    pub text: String,
    pub links: Vec<PageLink>,
    pub forms: Vec<PageForm>,
    pub metadata: PageMetadata,
}

/// Link on the page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageLink {
    pub href: String,
    pub text: String,
    pub is_external: bool,
}

/// Form on the page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageForm {
    pub action: String,
    pub method: String,
    pub inputs: Vec<FormInput>,
}

/// Form input.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormInput {
    pub name: String,
    pub input_type: String,
    pub placeholder: Option<String>,
    pub required: bool,
}

/// Page metadata.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PageMetadata {
    pub description: Option<String>,
    pub og_title: Option<String>,
    pub og_description: Option<String>,
    pub canonical_url: Option<String>,
    pub language: Option<String>,
}

impl PageContext {
    /// Create empty context.
    pub fn empty() -> Self {
        Self {
            url: String::new(),
            title: String::new(),
            text: String::new(),
            links: Vec::new(),
            forms: Vec::new(),
            metadata: PageMetadata::default(),
        }
    }

    /// Get a summary suitable for LLM context window.
    pub fn summarize(&self, max_chars: usize) -> String {
        let mut summary = format!("URL: {}\nTitle: {}\n\n", self.url, self.title);

        if let Some(desc) = &self.metadata.description {
            summary.push_str(&format!("Description: {}\n\n", desc));
        }

        // Truncate text to fit
        let remaining = max_chars.saturating_sub(summary.len());
        if self.text.len() > remaining {
            summary.push_str(&self.text[..remaining]);
            summary.push_str("...[truncated]");
        } else {
            summary.push_str(&self.text);
        }

        summary
    }

    /// Count interactive elements.
    pub fn interactive_element_count(&self) -> usize {
        let form_inputs: usize = self.forms.iter().map(|f| f.inputs.len()).sum();
        self.links.len() + form_inputs
    }

    /// Get external links only.
    pub fn external_links(&self) -> Vec<&PageLink> {
        self.links.iter().filter(|l| l.is_external).collect()
    }
}

/// JavaScript to extract page context (inject into browser).
pub fn extraction_js() -> &'static str {
    r#"(() => {
    const links = Array.from(document.querySelectorAll('a[href]')).slice(0, 50).map(a => ({
        href: a.href,
        text: (a.textContent || '').trim().slice(0, 100),
        is_external: a.hostname !== window.location.hostname
    }));

    const forms = Array.from(document.querySelectorAll('form')).slice(0, 10).map(f => ({
        action: f.action || '',
        method: (f.method || 'get').toUpperCase(),
        inputs: Array.from(f.querySelectorAll('input, textarea, select')).slice(0, 20).map(i => ({
            name: i.name || '',
            input_type: i.type || 'text',
            placeholder: i.placeholder || null,
            required: i.required || false
        }))
    }));

    const meta = {};
    const desc = document.querySelector('meta[name="description"]');
    if (desc) meta.description = desc.content;
    const ogTitle = document.querySelector('meta[property="og:title"]');
    if (ogTitle) meta.og_title = ogTitle.content;
    const ogDesc = document.querySelector('meta[property="og:description"]');
    if (ogDesc) meta.og_description = ogDesc.content;
    const canonical = document.querySelector('link[rel="canonical"]');
    if (canonical) meta.canonical_url = canonical.href;
    meta.language = document.documentElement.lang || null;

    return {
        url: window.location.href,
        title: document.title,
        text: (document.body?.innerText || '').slice(0, 50000),
        links,
        forms,
        metadata: meta
    };
})()"#
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_context() {
        let ctx = PageContext::empty();
        assert!(ctx.url.is_empty());
        assert_eq!(ctx.interactive_element_count(), 0);
    }

    #[test]
    fn summarize_truncation() {
        let mut ctx = PageContext::empty();
        ctx.url = "https://example.com".to_string();
        ctx.title = "Example".to_string();
        ctx.text = "A".repeat(1000);

        let summary = ctx.summarize(200);
        assert!(summary.len() <= 220); // Some overhead for headers
        assert!(summary.contains("[truncated]"));
    }

    #[test]
    fn external_links_filter() {
        let ctx = PageContext {
            url: "https://example.com".into(),
            title: "Test".into(),
            text: String::new(),
            links: vec![
                PageLink {
                    href: "https://example.com/page".into(),
                    text: "Internal".into(),
                    is_external: false,
                },
                PageLink {
                    href: "https://other.com".into(),
                    text: "External".into(),
                    is_external: true,
                },
            ],
            forms: Vec::new(),
            metadata: PageMetadata::default(),
        };

        assert_eq!(ctx.external_links().len(), 1);
        assert_eq!(ctx.interactive_element_count(), 2);
    }

    #[test]
    fn extraction_js_valid() {
        let js = extraction_js();
        assert!(js.contains("document.querySelectorAll"));
        assert!(js.contains("window.location.href"));
    }
}
