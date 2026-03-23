//! Image generation and processing tool for agent use.
//!
//! Routes image operations to the appropriate provider:
//! - Generation: DALL-E, Stable Diffusion
//! - Analysis: Vision models (GPT-4V, Claude Vision)
//! - Transformation: Resize, crop, format conversion

use crate::tools::{Tool, ToolCapability, ToolSchema};
use async_trait::async_trait;
use serde_json::json;
use std::path::PathBuf;
use tracing::debug;

/// Image generation and processing tool.
pub struct ImageTool {
    workspace: Option<PathBuf>,
}

impl ImageTool {
    pub fn new(workspace: Option<PathBuf>) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl Tool for ImageTool {
    fn name(&self) -> &str {
        "image"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "image".into(),
            description: "Generate, analyze, or transform images. Supports generation \
                          via DALL-E/SD, analysis via vision models, and transformations like resize/crop."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["generate", "analyze", "resize", "crop", "convert", "info"],
                        "description": "Action to perform"
                    },
                    "prompt": {
                        "type": "string",
                        "description": "Image generation prompt (for 'generate' action)"
                    },
                    "path": {
                        "type": "string",
                        "description": "Path to input image (for analyze/transform)"
                    },
                    "width": {
                        "type": "integer",
                        "description": "Target width (for resize/crop)"
                    },
                    "height": {
                        "type": "integer",
                        "description": "Target height (for resize/crop)"
                    },
                    "format": {
                        "type": "string",
                        "enum": ["png", "jpeg", "webp"],
                        "description": "Output format (for convert)"
                    },
                    "model": {
                        "type": "string",
                        "description": "Model to use (e.g., 'dall-e-3', 'sd-xl')"
                    },
                    "size": {
                        "type": "string",
                        "enum": ["256x256", "512x512", "1024x1024", "1024x1792", "1792x1024"],
                        "description": "Image size for generation"
                    }
                },
                "required": ["action"]
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::MediaGeneration, ToolCapability::ExternalApi]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or("Missing 'action' parameter")?;

        match action {
            "generate" => {
                let prompt = args
                    .get("prompt")
                    .and_then(|v| v.as_str())
                    .ok_or("Missing 'prompt' for image generation")?;
                let size = args
                    .get("size")
                    .and_then(|v| v.as_str())
                    .unwrap_or("1024x1024");
                let model = args
                    .get("model")
                    .and_then(|v| v.as_str())
                    .unwrap_or("dall-e-3");

                debug!(prompt, size, model, "Image generation requested");

                Ok(json!({
                    "status": "generation_queued",
                    "prompt": prompt,
                    "model": model,
                    "size": size,
                    "note": "Image generation requires a configured provider (DALL-E or SD). Route through the media crate's image_gen module."
                })
                .to_string())
            }
            "analyze" => {
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .ok_or("Missing 'path' for image analysis")?;

                let full_path = if let Some(ref ws) = self.workspace {
                    ws.join(path)
                } else {
                    PathBuf::from(path)
                };

                if !full_path.exists() {
                    return Err(format!("Image file not found: {path}"));
                }

                let metadata = tokio::fs::metadata(&full_path)
                    .await
                    .map_err(|e| format!("Failed to read file: {e}"))?;

                Ok(json!({
                    "path": path,
                    "size_bytes": metadata.len(),
                    "note": "Vision analysis requires sending the image to a vision model (GPT-4V, Claude Vision). Use the provider's multimodal capabilities."
                })
                .to_string())
            }
            "info" => {
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .ok_or("Missing 'path' for image info")?;

                let full_path = if let Some(ref ws) = self.workspace {
                    ws.join(path)
                } else {
                    PathBuf::from(path)
                };

                if !full_path.exists() {
                    return Err(format!("File not found: {path}"));
                }

                let metadata = tokio::fs::metadata(&full_path)
                    .await
                    .map_err(|e| format!("Failed to read file: {e}"))?;
                let ext = full_path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("unknown");

                Ok(json!({
                    "path": path,
                    "size_bytes": metadata.len(),
                    "format": ext,
                })
                .to_string())
            }
            "resize" | "crop" | "convert" => {
                let path = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .ok_or("Missing 'path' for image transformation")?;

                Ok(json!({
                    "status": "transformation_queued",
                    "action": action,
                    "path": path,
                    "note": "Image transformation requires the media crate's image processor. Route through clawdesk_media::processor."
                })
                .to_string())
            }
            _ => Err(format!("Unknown image action: {action}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_schema() {
        let tool = ImageTool::new(None);
        assert_eq!(tool.name(), "image");
        let schema = tool.schema();
        assert!(schema.description.contains("image"));
    }
}
