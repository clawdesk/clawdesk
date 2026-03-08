//! Canvas + Device tool implementations — 9 tools for agent canvas and device access.
//!
//! Canvas tools (A2UI / WebView surface):
//! 1. `canvas_present`  — Show/position the canvas WebView
//! 2. `canvas_hide`     — Hide the canvas WebView
//! 3. `canvas_navigate` — Navigate canvas to a URL
//! 4. `canvas_eval`     — Execute JavaScript in the canvas
//! 5. `canvas_snapshot` — Screenshot the canvas
//! 6. `a2ui_push`       — Push A2UI JSONL to render components
//! 7. `a2ui_reset`      — Clear the A2UI surface
//!
//! Device tools:
//! 8. `device_info`     — Structured device information
//! 9. `location_get`    — GPS/IP-based location

use crate::tools::{Tool, ToolCapability, ToolSchema};
use async_trait::async_trait;
use clawdesk_canvas::commands::{CanvasCommand, CanvasManager};
use clawdesk_canvas::device::DeviceManager;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::RwLock;

// ═══════════════════════════════════════════════════════════════
// Tool 1: canvas_present — Show/position the canvas
// ═══════════════════════════════════════════════════════════════

pub struct CanvasPresentTool {
    canvas: Arc<CanvasManager>,
    agent_id: String,
}

impl CanvasPresentTool {
    pub fn new(canvas: Arc<CanvasManager>, agent_id: String) -> Self {
        Self { canvas, agent_id }
    }
}

#[async_trait]
impl Tool for CanvasPresentTool {
    fn name(&self) -> &str {
        "canvas_present"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "canvas_present".into(),
            description: "Show the agent canvas WebView window. Optionally specify a URL \
                to load and position/size. If no URL is given, shows the A2UI rendering surface."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "URL to load in the canvas. If omitted, shows A2UI surface."
                    },
                    "x": { "type": "number", "description": "X position on screen" },
                    "y": { "type": "number", "description": "Y position on screen" },
                    "width": { "type": "number", "description": "Window width in pixels" },
                    "height": { "type": "number", "description": "Window height in pixels" }
                },
                "additionalProperties": false
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Browser]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let cmd = CanvasCommand::Present {
            url: args["url"].as_str().map(String::from),
            x: args["x"].as_f64(),
            y: args["y"].as_f64(),
            width: args["width"].as_f64(),
            height: args["height"].as_f64(),
        };
        let result = self.canvas.execute(&self.agent_id, cmd).await;
        serde_json::to_string(&result).map_err(|e| e.to_string())
    }
}

// ═══════════════════════════════════════════════════════════════
// Tool 2: canvas_hide — Hide the canvas
// ═══════════════════════════════════════════════════════════════

pub struct CanvasHideTool {
    canvas: Arc<CanvasManager>,
    agent_id: String,
}

impl CanvasHideTool {
    pub fn new(canvas: Arc<CanvasManager>, agent_id: String) -> Self {
        Self { canvas, agent_id }
    }
}

#[async_trait]
impl Tool for CanvasHideTool {
    fn name(&self) -> &str {
        "canvas_hide"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "canvas_hide".into(),
            description: "Hide the agent canvas WebView window.".into(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Browser]
    }

    async fn execute(&self, _args: serde_json::Value) -> Result<String, String> {
        let result = self.canvas.execute(&self.agent_id, CanvasCommand::Hide).await;
        serde_json::to_string(&result).map_err(|e| e.to_string())
    }
}

// ═══════════════════════════════════════════════════════════════
// Tool 3: canvas_navigate — Navigate canvas to URL
// ═══════════════════════════════════════════════════════════════

pub struct CanvasNavigateTool {
    canvas: Arc<CanvasManager>,
    agent_id: String,
}

impl CanvasNavigateTool {
    pub fn new(canvas: Arc<CanvasManager>, agent_id: String) -> Self {
        Self { canvas, agent_id }
    }
}

#[async_trait]
impl Tool for CanvasNavigateTool {
    fn name(&self) -> &str {
        "canvas_navigate"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "canvas_navigate".into(),
            description: "Navigate the canvas WebView to a specific URL.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "URL to navigate to"
                    }
                },
                "required": ["url"],
                "additionalProperties": false
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Browser, ToolCapability::Network]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let url = args["url"]
            .as_str()
            .ok_or("missing 'url' parameter")?
            .to_string();
        let cmd = CanvasCommand::Navigate { url };
        let result = self.canvas.execute(&self.agent_id, cmd).await;
        serde_json::to_string(&result).map_err(|e| e.to_string())
    }
}

// ═══════════════════════════════════════════════════════════════
// Tool 4: canvas_eval — Execute JavaScript in canvas
// ═══════════════════════════════════════════════════════════════

pub struct CanvasEvalTool {
    canvas: Arc<CanvasManager>,
    agent_id: String,
}

impl CanvasEvalTool {
    pub fn new(canvas: Arc<CanvasManager>, agent_id: String) -> Self {
        Self { canvas, agent_id }
    }
}

#[async_trait]
impl Tool for CanvasEvalTool {
    fn name(&self) -> &str {
        "canvas_eval"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "canvas_eval".into(),
            description: "Execute JavaScript code in the canvas WebView and return the result. \
                Useful for DOM manipulation, reading page state, or running custom logic."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "javascript": {
                        "type": "string",
                        "description": "JavaScript code to execute in the canvas"
                    },
                    "timeout_ms": {
                        "type": "integer",
                        "description": "Timeout in milliseconds (default 5000)",
                        "default": 5000
                    }
                },
                "required": ["javascript"],
                "additionalProperties": false
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Browser]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let javascript = args["javascript"]
            .as_str()
            .ok_or("missing 'javascript' parameter")?
            .to_string();
        let timeout_ms = args["timeout_ms"].as_u64().unwrap_or(5000);
        let cmd = CanvasCommand::Eval {
            javascript,
            timeout_ms,
        };
        let result = self.canvas.execute(&self.agent_id, cmd).await;
        serde_json::to_string(&result).map_err(|e| e.to_string())
    }
}

// ═══════════════════════════════════════════════════════════════
// Tool 5: canvas_snapshot — Screenshot the canvas
// ═══════════════════════════════════════════════════════════════

pub struct CanvasSnapshotTool {
    canvas: Arc<CanvasManager>,
    agent_id: String,
}

impl CanvasSnapshotTool {
    pub fn new(canvas: Arc<CanvasManager>, agent_id: String) -> Self {
        Self { canvas, agent_id }
    }
}

#[async_trait]
impl Tool for CanvasSnapshotTool {
    fn name(&self) -> &str {
        "canvas_snapshot"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "canvas_snapshot".into(),
            description: "Capture a screenshot of the canvas WebView. Returns base64-encoded \
                image data."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "format": {
                        "type": "string",
                        "enum": ["png", "jpeg", "webp"],
                        "default": "png",
                        "description": "Image format"
                    },
                    "max_width": {
                        "type": "integer",
                        "description": "Maximum width (auto-scale if set)"
                    },
                    "quality": {
                        "type": "number",
                        "description": "JPEG/WebP quality 0.0-1.0"
                    }
                },
                "additionalProperties": false
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Browser]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let format = args["format"]
            .as_str()
            .unwrap_or("png")
            .to_string();
        let max_width = args["max_width"].as_u64().map(|v| v as u32);
        let quality = args["quality"].as_f64();
        let cmd = CanvasCommand::Snapshot {
            format,
            max_width,
            quality,
        };
        let result = self.canvas.execute(&self.agent_id, cmd).await;
        serde_json::to_string(&result).map_err(|e| e.to_string())
    }
}

// ═══════════════════════════════════════════════════════════════
// Tool 6: a2ui_push — Push A2UI JSONL components
// ═══════════════════════════════════════════════════════════════

pub struct A2uiPushTool {
    canvas: Arc<CanvasManager>,
    agent_id: String,
}

impl A2uiPushTool {
    pub fn new(canvas: Arc<CanvasManager>, agent_id: String) -> Self {
        Self { canvas, agent_id }
    }
}

#[async_trait]
impl Tool for A2uiPushTool {
    fn name(&self) -> &str {
        "a2ui_push"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "a2ui_push".into(),
            description: "Push A2UI (Agent-to-UI) JSONL to render declarative UI components \
                in the canvas. Each line is a JSON message: CreateSurface, SurfaceUpdate, \
                BeginRendering, DeleteSurface, or DataModelUpdate. Components include: \
                Column, Row, Text, Markdown, Code, Image, Button, Input, Select, Table, \
                Chart, Progress, Divider, Spacer, Card, Html."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "jsonl": {
                        "type": "string",
                        "description": "JSONL string — one JSON message per line. \
                            Example:\n\
                            {\"type\":\"CreateSurface\",\"surface_id\":\"main\"}\n\
                            {\"type\":\"SurfaceUpdate\",\"surface_id\":\"main\",\"components\":[{\"type\":\"text\",\"content\":\"Hello\"}]}\n\
                            {\"type\":\"BeginRendering\",\"surface_id\":\"main\"}"
                    }
                },
                "required": ["jsonl"],
                "additionalProperties": false
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Browser]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let jsonl = args["jsonl"]
            .as_str()
            .ok_or("missing 'jsonl' parameter")?
            .to_string();
        let cmd = CanvasCommand::A2uiPush { jsonl };
        let result = self.canvas.execute(&self.agent_id, cmd).await;
        serde_json::to_string(&result).map_err(|e| e.to_string())
    }
}

// ═══════════════════════════════════════════════════════════════
// Tool 7: a2ui_reset — Clear A2UI surface
// ═══════════════════════════════════════════════════════════════

pub struct A2uiResetTool {
    canvas: Arc<CanvasManager>,
    agent_id: String,
}

impl A2uiResetTool {
    pub fn new(canvas: Arc<CanvasManager>, agent_id: String) -> Self {
        Self { canvas, agent_id }
    }
}

#[async_trait]
impl Tool for A2uiResetTool {
    fn name(&self) -> &str {
        "a2ui_reset"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "a2ui_reset".into(),
            description: "Clear (reset) the A2UI rendering surface. Optionally specify \
                a surface_id to reset only that surface, or omit to clear all surfaces."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "surface_id": {
                        "type": "string",
                        "description": "Surface to reset (optional — clears all if omitted)"
                    }
                },
                "additionalProperties": false
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Browser]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let surface_id = args["surface_id"].as_str().map(String::from);
        let cmd = CanvasCommand::A2uiReset { surface_id };
        let result = self.canvas.execute(&self.agent_id, cmd).await;
        serde_json::to_string(&result).map_err(|e| e.to_string())
    }
}

// ═══════════════════════════════════════════════════════════════
// Tool 8: device_info — Structured device information
// ═══════════════════════════════════════════════════════════════

pub struct DeviceInfoTool {
    device: Arc<RwLock<DeviceManager>>,
}

impl DeviceInfoTool {
    pub fn new(device: Arc<RwLock<DeviceManager>>) -> Self {
        Self { device }
    }
}

#[async_trait]
impl Tool for DeviceInfoTool {
    fn name(&self) -> &str {
        "device_info"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "device_info".into(),
            description: "Get structured information about the device: OS, version, CPU, \
                memory, architecture, and available capabilities."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "include_status": {
                        "type": "boolean",
                        "description": "Include dynamic status (battery, uptime, memory usage)",
                        "default": false
                    }
                },
                "additionalProperties": false
            }),
        }
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let device = self.device.read().await;
        let include_status = args["include_status"].as_bool().unwrap_or(false);

        let mut result = json!({
            "info": device.device_info(),
            "capabilities": device.capabilities(),
        });

        if include_status {
            result["status"] = serde_json::to_value(device.device_status())
                .map_err(|e| e.to_string())?;
        }

        serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
    }
}

// ═══════════════════════════════════════════════════════════════
// Tool 9: location_get — GPS/IP-based location
// ═══════════════════════════════════════════════════════════════

pub struct LocationGetTool {
    device: Arc<RwLock<DeviceManager>>,
}

impl LocationGetTool {
    pub fn new(device: Arc<RwLock<DeviceManager>>) -> Self {
        Self { device }
    }
}

#[async_trait]
impl Tool for LocationGetTool {
    fn name(&self) -> &str {
        "location_get"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "location_get".into(),
            description: "Get the current device location (GPS or IP-based fallback). \
                Returns latitude, longitude, accuracy, and source."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Network]
    }

    async fn execute(&self, _args: serde_json::Value) -> Result<String, String> {
        let device = self.device.read().await;
        match device.get_location().await {
            Ok(loc) => serde_json::to_string_pretty(&loc).map_err(|e| e.to_string()),
            Err(e) => Err(format!("location error: {e}")),
        }
    }
}

// ═══════════════════════════════════════════════════════════════
// Tool factory — create all canvas + device tools for an agent
// ═══════════════════════════════════════════════════════════════

/// Create all canvas + device tools for an agent.
pub fn create_canvas_tools(
    canvas: Arc<CanvasManager>,
    device: Arc<RwLock<DeviceManager>>,
    agent_id: String,
) -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(CanvasPresentTool::new(canvas.clone(), agent_id.clone())),
        Box::new(CanvasHideTool::new(canvas.clone(), agent_id.clone())),
        Box::new(CanvasNavigateTool::new(canvas.clone(), agent_id.clone())),
        Box::new(CanvasEvalTool::new(canvas.clone(), agent_id.clone())),
        Box::new(CanvasSnapshotTool::new(canvas.clone(), agent_id.clone())),
        Box::new(A2uiPushTool::new(canvas.clone(), agent_id.clone())),
        Box::new(A2uiResetTool::new(canvas, agent_id)),
        Box::new(DeviceInfoTool::new(device.clone())),
        Box::new(LocationGetTool::new(device)),
    ]
}
