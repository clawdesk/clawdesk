//! A2UI (Agent-to-UI) — JSONL-based declarative UI protocol.
//!
//! Lets agents push structured UI components to a rendering surface (WebView).
//! Each JSONL line is a message with exactly one action key.
//!
//! ## Protocol Version
//! - v1.0: `createSurface`, `surfaceUpdate`, `beginRendering`, `deleteSurface`, `dataModelUpdate`
//!
//! ## Example
//! ```jsonl
//! {"createSurface":{"surfaceId":"main","title":"Results"}}
//! {"surfaceUpdate":{"surfaceId":"main","components":[{"id":"root","component":{"Column":{"children":["text1","chart1"]}}}]}}
//! {"beginRendering":{"surfaceId":"main","root":"root"}}
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

/// Surface identifier (opaque string).
pub type SurfaceId = String;

/// Component identifier within a surface (opaque string).
pub type ComponentId = String;

// ═══════════════════════════════════════════════════════════════
// A2UI Messages
// ═══════════════════════════════════════════════════════════════

/// A single A2UI message — exactly one action per JSONL line.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum A2UIMessage {
    /// Create a new rendering surface.
    CreateSurface(CreateSurface),
    /// Update components on a surface.
    SurfaceUpdate(SurfaceUpdate),
    /// Trigger rendering with a root component.
    BeginRendering(BeginRendering),
    /// Delete a surface.
    DeleteSurface(DeleteSurface),
    /// Update the data model bound to a surface.
    DataModelUpdate(DataModelUpdate),
}

impl A2UIMessage {
    /// Parse a single JSONL line into an A2UI message.
    pub fn parse_line(line: &str) -> Result<Self, A2UIError> {
        let line = line.trim();
        if line.is_empty() {
            return Err(A2UIError::EmptyLine);
        }
        serde_json::from_str(line).map_err(|e| A2UIError::InvalidJson(e.to_string()))
    }

    /// Parse multiple JSONL lines.
    pub fn parse_jsonl(input: &str) -> Result<Vec<Self>, A2UIError> {
        let mut messages = Vec::new();
        for (i, line) in input.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match Self::parse_line(line) {
                Ok(msg) => messages.push(msg),
                Err(e) => {
                    return Err(A2UIError::ParseError {
                        line: i + 1,
                        source: Box::new(e),
                    })
                }
            }
        }
        Ok(messages)
    }

    /// Serialize to a JSONL line.
    pub fn to_jsonl(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

// ═══════════════════════════════════════════════════════════════
// Action payloads
// ═══════════════════════════════════════════════════════════════

/// Create a new rendering surface.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSurface {
    pub surface_id: SurfaceId,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
}

/// Update components on a surface.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SurfaceUpdate {
    pub surface_id: SurfaceId,
    pub components: Vec<ComponentNode>,
}

/// Trigger rendering with a root component.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeginRendering {
    pub surface_id: SurfaceId,
    pub root: ComponentId,
}

/// Delete a surface.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteSurface {
    pub surface_id: SurfaceId,
}

/// Update the data model bound to a surface.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DataModelUpdate {
    pub surface_id: SurfaceId,
    pub data: serde_json::Value,
}

// ═══════════════════════════════════════════════════════════════
// Component tree
// ═══════════════════════════════════════════════════════════════

/// A node in the component tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentNode {
    pub id: ComponentId,
    pub component: A2UIComponent,
}

/// Component type — the set of renderable UI primitives.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum A2UIComponent {
    /// Vertical layout container.
    Column(ColumnComponent),
    /// Horizontal layout container.
    Row(RowComponent),
    /// Text display.
    Text(TextComponent),
    /// Markdown block (rendered to HTML).
    Markdown(MarkdownComponent),
    /// Code block with syntax highlighting.
    Code(CodeComponent),
    /// Image display (URL or base64).
    Image(ImageComponent),
    /// Interactive button.
    Button(ButtonComponent),
    /// Text input field.
    Input(InputComponent),
    /// Select dropdown.
    Select(SelectComponent),
    /// Data table.
    Table(TableComponent),
    /// Chart / visualization.
    Chart(ChartComponent),
    /// Progress indicator.
    Progress(ProgressComponent),
    /// Divider / separator.
    Divider(DividerComponent),
    /// Spacer.
    Spacer(SpacerComponent),
    /// Card container with optional header.
    Card(CardComponent),
    /// Raw HTML (sandboxed iframe).
    Html(HtmlComponent),

    // === Expanded Widget Vocabulary (A2UI v2.0) ===

    /// Date picker input.
    DatePicker(DatePickerComponent),
    /// File upload input.
    FileUpload(FileUploadComponent),
    /// Checkbox input.
    Checkbox(CheckboxComponent),
    /// Radio button group.
    RadioGroup(RadioGroupComponent),
    /// Approval buttons (approve/reject pair).
    ApprovalButtons(ApprovalButtonsComponent),
    /// Star rating (1-5).
    StarRating(StarRatingComponent),
    /// Parameter slider.
    Slider(SliderComponent),
    /// KPI metric card.
    Metric(MetricComponent),
    /// Audio player.
    AudioPlayer(AudioPlayerComponent),
    /// Image carousel.
    ImageCarousel(ImageCarouselComponent),
    /// Accordion (collapsible sections).
    Accordion(AccordionComponent),
    /// Tabs container.
    Tabs(TabsComponent),
    /// Badge/tag.
    Badge(BadgeComponent),
    /// Alert/notification banner.
    Alert(AlertComponent),
}

impl A2UIComponent {
    /// Get the component type name.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Column(_) => "Column",
            Self::Row(_) => "Row",
            Self::Text(_) => "Text",
            Self::Markdown(_) => "Markdown",
            Self::Code(_) => "Code",
            Self::Image(_) => "Image",
            Self::Button(_) => "Button",
            Self::Input(_) => "Input",
            Self::Select(_) => "Select",
            Self::Table(_) => "Table",
            Self::Chart(_) => "Chart",
            Self::Progress(_) => "Progress",
            Self::Divider(_) => "Divider",
            Self::Spacer(_) => "Spacer",
            Self::Card(_) => "Card",
            Self::Html(_) => "Html",
            Self::DatePicker(_) => "DatePicker",
            Self::FileUpload(_) => "FileUpload",
            Self::Checkbox(_) => "Checkbox",
            Self::RadioGroup(_) => "RadioGroup",
            Self::ApprovalButtons(_) => "ApprovalButtons",
            Self::StarRating(_) => "StarRating",
            Self::Slider(_) => "Slider",
            Self::Metric(_) => "Metric",
            Self::AudioPlayer(_) => "AudioPlayer",
            Self::ImageCarousel(_) => "ImageCarousel",
            Self::Accordion(_) => "Accordion",
            Self::Tabs(_) => "Tabs",
            Self::Badge(_) => "Badge",
            Self::Alert(_) => "Alert",
        }
    }
}

// ═══════════════════════════════════════════════════════════════
// Component structs
// ═══════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnComponent {
    pub children: Vec<ComponentId>,
    #[serde(default)]
    pub gap: Option<u32>,
    #[serde(default)]
    pub padding: Option<u32>,
    #[serde(default)]
    pub align: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowComponent {
    pub children: Vec<ComponentId>,
    #[serde(default)]
    pub gap: Option<u32>,
    #[serde(default)]
    pub align: Option<String>,
    #[serde(default)]
    pub justify: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextComponent {
    pub text: TextValue,
    #[serde(default)]
    pub usage_hint: Option<String>,
    #[serde(default)]
    pub style: Option<TextStyle>,
}

/// Text value — literal or bound to data model.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TextValue {
    LiteralString(String),
    Binding(String),
}

impl TextValue {
    pub fn literal(s: impl Into<String>) -> Self {
        Self::LiteralString(s.into())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextStyle {
    #[serde(default)]
    pub font_size: Option<u32>,
    #[serde(default)]
    pub font_weight: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub text_align: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkdownComponent {
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeComponent {
    pub code: String,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub show_line_numbers: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageComponent {
    pub src: String,
    #[serde(default)]
    pub alt: Option<String>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ButtonComponent {
    pub label: String,
    pub action_id: String,
    #[serde(default)]
    pub variant: Option<String>,
    #[serde(default)]
    pub disabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputComponent {
    pub placeholder: Option<String>,
    pub binding: String,
    #[serde(default)]
    pub input_type: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectComponent {
    pub options: Vec<SelectOption>,
    pub binding: String,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectOption {
    pub value: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableComponent {
    pub columns: Vec<TableColumn>,
    pub rows: Vec<Vec<serde_json::Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableColumn {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub width: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChartComponent {
    pub chart_type: String,
    pub data: serde_json::Value,
    #[serde(default)]
    pub options: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressComponent {
    pub value: f64,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DividerComponent {
    #[serde(default)]
    pub margin: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpacerComponent {
    #[serde(default)]
    pub height: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CardComponent {
    pub children: Vec<ComponentId>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub subtitle: Option<String>,
    #[serde(default)]
    pub padding: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HtmlComponent {
    pub html: String,
    #[serde(default)]
    pub sandbox: bool,
}

// ═══════════════════════════════════════════════════════════════
// Expanded Widget Vocabulary (A2UI v2.0)
// ═══════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DatePickerComponent {
    pub binding: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub min_date: Option<String>,
    #[serde(default)]
    pub max_date: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileUploadComponent {
    pub binding: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub accept: Option<String>,
    #[serde(default)]
    pub multiple: bool,
    #[serde(default)]
    pub max_size_mb: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckboxComponent {
    pub label: String,
    pub binding: String,
    #[serde(default)]
    pub checked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RadioGroupComponent {
    pub options: Vec<SelectOption>,
    pub binding: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub layout: Option<String>, // "horizontal" or "vertical"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalButtonsComponent {
    pub approve_label: String,
    pub reject_label: String,
    pub approve_action_id: String,
    pub reject_action_id: String,
    #[serde(default)]
    pub context: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StarRatingComponent {
    pub binding: String,
    #[serde(default)]
    pub max_stars: Option<u32>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub current: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SliderComponent {
    pub binding: String,
    pub min: f64,
    pub max: f64,
    #[serde(default)]
    pub step: Option<f64>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub current: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricComponent {
    pub label: String,
    pub value: String,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub trend: Option<String>,        // "up", "down", "neutral"
    #[serde(default)]
    pub trend_value: Option<String>,   // "+12%", "-3%"
    #[serde(default)]
    pub color: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioPlayerComponent {
    pub src: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub autoplay: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageCarouselComponent {
    pub images: Vec<CarouselImage>,
    #[serde(default)]
    pub auto_advance_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CarouselImage {
    pub src: String,
    #[serde(default)]
    pub alt: Option<String>,
    #[serde(default)]
    pub caption: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccordionComponent {
    pub sections: Vec<AccordionSection>,
    #[serde(default)]
    pub allow_multiple: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccordionSection {
    pub title: String,
    pub children: Vec<ComponentId>,
    #[serde(default)]
    pub expanded: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TabsComponent {
    pub tabs: Vec<TabItem>,
    #[serde(default)]
    pub active_tab: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TabItem {
    pub id: String,
    pub label: String,
    pub children: Vec<ComponentId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BadgeComponent {
    pub text: String,
    #[serde(default)]
    pub variant: Option<String>,  // "success", "warning", "error", "info", "neutral"
    #[serde(default)]
    pub icon: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlertComponent {
    pub message: String,
    #[serde(default)]
    pub severity: Option<String>,     // "info", "success", "warning", "error"
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub dismissible: bool,
    #[serde(default)]
    pub action: Option<ButtonComponent>,
}

// ═══════════════════════════════════════════════════════════════
// A2UI Action (agent tool interface)
// ═══════════════════════════════════════════════════════════════

/// High-level actions agents can dispatch.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum A2UIAction {
    /// Push JSONL to the A2UI surface.
    Push { jsonl: String },
    /// Reset (clear) the A2UI surface.
    Reset,
}

// ═══════════════════════════════════════════════════════════════
// Component tree builder
// ═══════════════════════════════════════════════════════════════

/// In-memory component tree for a surface.
#[derive(Debug, Clone, Default)]
pub struct ComponentTree {
    pub nodes: HashMap<ComponentId, A2UIComponent>,
    pub root: Option<ComponentId>,
    pub data_model: serde_json::Value,
}

impl ComponentTree {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply a surface update (merges components).
    pub fn apply_update(&mut self, update: &SurfaceUpdate) {
        for node in &update.components {
            self.nodes.insert(node.id.clone(), node.component.clone());
        }
    }

    /// Set the root component and mark tree as ready.
    pub fn set_root(&mut self, root: ComponentId) {
        self.root = Some(root);
    }

    /// Update the data model.
    pub fn update_data(&mut self, data: serde_json::Value) {
        self.data_model = data;
    }

    /// Clear all state.
    pub fn reset(&mut self) {
        self.nodes.clear();
        self.root = None;
        self.data_model = serde_json::Value::Null;
    }

    /// Number of components.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the tree is empty.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Serialize current state to JSON for the renderer.
    pub fn to_renderer_json(&self) -> serde_json::Value {
        let nodes: Vec<serde_json::Value> = self
            .nodes
            .iter()
            .map(|(id, comp)| {
                serde_json::json!({
                    "id": id,
                    "component": comp,
                })
            })
            .collect();

        serde_json::json!({
            "root": self.root,
            "components": nodes,
            "data": self.data_model,
        })
    }
}

// ═══════════════════════════════════════════════════════════════
// Errors
// ═══════════════════════════════════════════════════════════════

/// A2UI protocol errors.
#[derive(Debug, thiserror::Error)]
pub enum A2UIError {
    #[error("empty JSONL line")]
    EmptyLine,
    #[error("invalid JSON: {0}")]
    InvalidJson(String),
    #[error("parse error at line {line}: {source}")]
    ParseError {
        line: usize,
        source: Box<A2UIError>,
    },
    #[error("unknown surface: {0}")]
    UnknownSurface(SurfaceId),
    #[error("surface already exists: {0}")]
    SurfaceAlreadyExists(SurfaceId),
}

impl fmt::Display for A2UIComponent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.type_name())
    }
}

// ═══════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_create_surface() {
        let line = r#"{"createSurface":{"surfaceId":"main","title":"Test"}}"#;
        let msg = A2UIMessage::parse_line(line).unwrap();
        match msg {
            A2UIMessage::CreateSurface(cs) => {
                assert_eq!(cs.surface_id, "main");
                assert_eq!(cs.title, "Test");
            }
            _ => panic!("expected CreateSurface"),
        }
    }

    #[test]
    fn parse_surface_update() {
        let line = r#"{"surfaceUpdate":{"surfaceId":"main","components":[{"id":"t1","component":{"Text":{"text":{"literalString":"Hello"},"usageHint":"body"}}}]}}"#;
        let msg = A2UIMessage::parse_line(line).unwrap();
        match msg {
            A2UIMessage::SurfaceUpdate(su) => {
                assert_eq!(su.surface_id, "main");
                assert_eq!(su.components.len(), 1);
                assert_eq!(su.components[0].id, "t1");
            }
            _ => panic!("expected SurfaceUpdate"),
        }
    }

    #[test]
    fn parse_jsonl_multiline() {
        let input = r#"
{"createSurface":{"surfaceId":"main","title":"Test"}}
{"surfaceUpdate":{"surfaceId":"main","components":[]}}
{"beginRendering":{"surfaceId":"main","root":"root"}}
"#;
        let msgs = A2UIMessage::parse_jsonl(input).unwrap();
        assert_eq!(msgs.len(), 3);
    }

    #[test]
    fn component_tree_lifecycle() {
        let mut tree = ComponentTree::new();
        assert!(tree.is_empty());

        let update = SurfaceUpdate {
            surface_id: "main".into(),
            components: vec![
                ComponentNode {
                    id: "root".into(),
                    component: A2UIComponent::Column(ColumnComponent {
                        children: vec!["text1".into()],
                        gap: Some(8),
                        padding: None,
                        align: None,
                    }),
                },
                ComponentNode {
                    id: "text1".into(),
                    component: A2UIComponent::Text(TextComponent {
                        text: TextValue::literal("Hello, World!"),
                        usage_hint: Some("body".into()),
                        style: None,
                    }),
                },
            ],
        };

        tree.apply_update(&update);
        assert_eq!(tree.len(), 2);

        tree.set_root("root".into());
        let json = tree.to_renderer_json();
        assert_eq!(json["root"], "root");

        tree.reset();
        assert!(tree.is_empty());
    }

    #[test]
    fn roundtrip_serialization() {
        let msg = A2UIMessage::CreateSurface(CreateSurface {
            surface_id: "panel".into(),
            title: "My Panel".into(),
            width: Some(400),
            height: Some(600),
        });
        let jsonl = msg.to_jsonl();
        let parsed = A2UIMessage::parse_line(&jsonl).unwrap();
        match parsed {
            A2UIMessage::CreateSurface(cs) => {
                assert_eq!(cs.surface_id, "panel");
                assert_eq!(cs.width, Some(400));
            }
            _ => panic!("wrong variant"),
        }
    }
}
