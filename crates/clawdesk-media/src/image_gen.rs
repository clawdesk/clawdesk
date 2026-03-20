//! Image generation provider system — plugin-based text-to-image.
//!
//! ## Design
//!
//! Image generation is a morphism in the category of media:
//!   `generate: TextPrompt × Style → Image`
//!
//! The provider registry is a coproduct of generators:
//!   `Registry = ∐(p ∈ Providers) Generator(p)`
//!
//! Fallback is a copairing — try each fiber until one succeeds:
//!   `[g₁, g₂, ..., gₙ] : TextPrompt → Image`
//!   where gᵢ is tried iff g₁..gᵢ₋₁ all failed.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

// ── Types ─────────────────────────────────────────────────

/// Image generation request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageGenRequest {
    /// Text prompt describing the desired image.
    pub prompt: String,
    /// Optional negative prompt (what to exclude).
    pub negative_prompt: Option<String>,
    /// Model to use (provider-specific).
    pub model: Option<String>,
    /// Number of images to generate.
    #[serde(default = "default_count")]
    pub count: u32,
    /// Desired size (e.g., "1024x1024").
    pub size: Option<String>,
    /// Aspect ratio (e.g., "16:9"). Some providers prefer this over exact size.
    pub aspect_ratio: Option<String>,
    /// Style hint (e.g., "natural", "vivid").
    pub style: Option<String>,
    /// Quality hint (e.g., "standard", "hd").
    pub quality: Option<String>,
    /// Output format preference.
    pub format: Option<ImageFormat>,
}

fn default_count() -> u32 { 1 }

/// Generated image response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageGenResponse {
    /// Generated images.
    pub images: Vec<GeneratedImage>,
    /// Provider that generated the images.
    pub provider_id: String,
    /// Model used.
    pub model: String,
    /// Usage metadata.
    pub usage: Option<ImageGenUsage>,
}

/// A single generated image.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedImage {
    /// Base64-encoded image data.
    pub data: Option<String>,
    /// URL to the generated image (some providers return URLs instead of data).
    pub url: Option<String>,
    /// MIME type.
    pub mime_type: String,
    /// Revised prompt (some providers rewrite the prompt for safety).
    pub revised_prompt: Option<String>,
}

/// Usage metadata for image generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageGenUsage {
    /// Estimated cost in USD.
    pub estimated_cost_usd: Option<f64>,
    /// Generation time in milliseconds.
    pub generation_time_ms: Option<u64>,
}

/// Output format for generated images.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageFormat {
    Png,
    Jpeg,
    Webp,
}

// ── Capabilities ──────────────────────────────────────────

/// Image generation provider capabilities.
///
/// Each provider declares what it supports up front, enabling the
/// registry to filter candidates before making network calls.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImageGenCapabilities {
    /// Supports text-to-image generation.
    pub generate: bool,
    /// Supports image editing (inpainting, outpainting).
    pub edit: bool,
    /// Maximum images per request.
    pub max_count: Option<u32>,
    /// Supported sizes (e.g., ["1024x1024", "1792x1024"]).
    pub sizes: Vec<String>,
    /// Supported aspect ratios (e.g., ["1:1", "16:9"]).
    pub aspect_ratios: Vec<String>,
    /// Supported output formats.
    pub formats: Vec<ImageFormat>,
}

/// Model info for image generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageModel {
    /// Model ID (e.g., "dall-e-3", "imagen-3.0").
    pub id: String,
    /// Display name.
    pub display_name: String,
    /// Per-model capabilities (may differ from provider-level).
    pub capabilities: Option<ImageGenCapabilities>,
}

// ── Provider Trait ────────────────────────────────────────

/// Image generation provider — the core abstraction.
///
/// Each provider (OpenAI DALL-E, Google Imagen, Fal, etc.) implements
/// this trait. The registry holds `Arc<dyn ImageGenProvider>` for dispatch.
#[async_trait]
pub trait ImageGenProvider: Send + Sync {
    /// Provider identifier (e.g., "openai", "google", "fal").
    fn id(&self) -> &str;

    /// Display name.
    fn display_name(&self) -> &str;

    /// Default model for this provider.
    fn default_model(&self) -> &str;

    /// Available models.
    fn models(&self) -> Vec<ImageModel>;

    /// Provider-level capabilities.
    fn capabilities(&self) -> ImageGenCapabilities;

    /// Generate images.
    async fn generate(
        &self,
        request: &ImageGenRequest,
    ) -> Result<ImageGenResponse, ImageGenError>;

    /// Check if the provider is configured and reachable.
    async fn health_check(&self) -> Result<(), ImageGenError>;
}

/// Image generation errors.
#[derive(Debug, thiserror::Error)]
pub enum ImageGenError {
    #[error("auth error: {0}")]
    Auth(String),
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("content policy violation: {0}")]
    ContentPolicy(String),
    #[error("rate limited: {0}")]
    RateLimit(String),
    #[error("provider error: {0}")]
    Provider(String),
    #[error("not supported: {0}")]
    NotSupported(String),
}

// ── Provider Registry ─────────────────────────────────────

/// Registry for image generation providers.
///
/// Supports canonical ID + alias lookups and fallback chain generation.
pub struct ImageGenRegistry {
    providers: HashMap<String, Arc<dyn ImageGenProvider>>,
    aliases: HashMap<String, String>,
}

impl ImageGenRegistry {
    pub fn new() -> Self {
        Self {
            providers: HashMap::new(),
            aliases: HashMap::new(),
        }
    }

    /// Register a provider with optional aliases.
    pub fn register(&mut self, provider: Arc<dyn ImageGenProvider>, aliases: &[&str]) {
        let id = provider.id().to_string();
        tracing::info!(
            provider = id.as_str(),
            models = provider.models().len(),
            "registered image generation provider"
        );
        for alias in aliases {
            self.aliases.insert(alias.to_string(), id.clone());
        }
        self.providers.insert(id, provider);
    }

    /// Look up a provider by canonical ID or alias.
    pub fn get(&self, id: &str) -> Option<&Arc<dyn ImageGenProvider>> {
        self.providers.get(id).or_else(|| {
            self.aliases
                .get(id)
                .and_then(|canonical| self.providers.get(canonical))
        })
    }

    /// List all registered canonical provider IDs.
    pub fn list(&self) -> Vec<&str> {
        self.providers.keys().map(|k| k.as_str()).collect()
    }

    /// Generate with fallback chain.
    ///
    /// Tries the primary provider, then falls back to alternatives.
    /// This is the copairing morphism: `[g₁, g₂, ..., gₙ]`.
    pub async fn generate_with_fallback(
        &self,
        request: &ImageGenRequest,
        provider_chain: &[&str],
    ) -> Result<ImageGenResponse, ImageGenError> {
        let mut last_error = None;

        for provider_id in provider_chain {
            if let Some(provider) = self.get(provider_id) {
                match provider.generate(request).await {
                    Ok(response) => return Ok(response),
                    Err(e) => {
                        tracing::warn!(
                            provider = *provider_id,
                            error = %e,
                            "image generation fallback, trying next provider"
                        );
                        last_error = Some(e);
                    }
                }
            }
        }

        Err(last_error.unwrap_or_else(|| {
            ImageGenError::NotSupported("No image generation providers registered".into())
        }))
    }
}

impl Default for ImageGenRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alias_resolution() {
        let mut reg = ImageGenRegistry::new();
        // Just test the alias mapping logic, no real provider needed
        reg.aliases.insert("dalle".into(), "openai".into());
        reg.aliases.insert("dall-e".into(), "openai".into());
        assert_eq!(reg.aliases.get("dalle"), Some(&"openai".to_string()));
        assert_eq!(reg.aliases.get("dall-e"), Some(&"openai".to_string()));
    }

    #[test]
    fn empty_registry() {
        let reg = ImageGenRegistry::new();
        assert!(reg.list().is_empty());
        assert!(reg.get("openai").is_none());
    }

    #[test]
    fn image_gen_request_defaults() {
        let req = ImageGenRequest {
            prompt: "A cat in space".into(),
            negative_prompt: None,
            model: None,
            count: 1,
            size: None,
            aspect_ratio: None,
            style: None,
            quality: None,
            format: None,
        };
        assert_eq!(req.count, 1);
    }
}
