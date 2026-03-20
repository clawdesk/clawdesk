//! No-Code Visual Skill Builder — DAG Editor for Canvas
//!
//! Enables non-technical users to create custom automation skills by
//! visually connecting trigger → process → output nodes in a canvas editor.
//! The visual DAG is compiled into a planner-compatible task graph via DTGG.
//!
//! ## Architecture
//!
//! ```text
//! VisualNode (UI drag-drop) → VisualDAG → validate() → compile_to_skill()
//!                                                          ↓
//!                                                     SkillScaffoldInput
//!                                                          ↓
//!                                                     generate_scaffold()
//!                                                          ↓
//!                                                     skill.toml + prompt.md
//! ```
//!
//! ## Validation
//!
//! 1. Acyclicity via DFS: O(V + E)
//! 2. Type compatibility: output_type(u) ⊆ input_type(v) for each edge
//! 3. Reachability: every output reachable from a trigger via BFS
//!
//! ## No Competitor Has This
//!
//! OpenClaw, Claude Code, and NemoClaw all require code to create automations.
//! This is ClawDesk's primary platform differentiator.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};

// ---------------------------------------------------------------------------
// Visual Node Types — Building blocks for the DAG editor
// ---------------------------------------------------------------------------

/// Unique identifier for a node in the visual DAG.
pub type VisualNodeId = String;

/// Categories of visual nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeCategory {
    Trigger,
    Processing,
    Output,
}

/// Data type flowing between nodes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataType {
    Text,
    Json,
    File,
    Image,
    Audio,
    Email,
    Event,
    Any,
}

impl DataType {
    /// Type compatibility check for edge validation.
    /// `self` is the output type, checks if it's compatible with `input_type`.
    pub fn is_compatible_with(&self, input_type: &DataType) -> bool {
        if *input_type == DataType::Any || *self == DataType::Any {
            return true;
        }
        self == input_type
    }
}

/// A visual node in the skill builder canvas.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisualNode {
    /// Unique node identifier
    pub id: VisualNodeId,
    /// Display name shown in the canvas
    pub label: String,
    /// Category: trigger, processing, or output
    pub category: NodeCategory,
    /// Template ID linking to a predefined node type
    pub template_id: String,
    /// Configuration values set by the user
    pub config: HashMap<String, serde_json::Value>,
    /// Output data type
    pub output_type: DataType,
    /// Expected input data type (Any for triggers)
    pub input_type: DataType,
    /// Canvas position for layout
    pub position: NodePosition,
    /// Which agent handles this processing (for Processing nodes)
    pub agent_id: Option<String>,
}

/// Position on the visual canvas.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodePosition {
    pub x: f64,
    pub y: f64,
}

/// An edge connecting two visual nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisualEdge {
    pub from: VisualNodeId,
    pub to: VisualNodeId,
}

// ---------------------------------------------------------------------------
// Predefined Node Templates — Drag-and-drop palette
// ---------------------------------------------------------------------------

/// A predefined node template for the palette.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeTemplate {
    pub id: String,
    pub label: String,
    pub category: NodeCategory,
    pub description: String,
    pub icon: String,
    pub output_type: DataType,
    pub input_type: DataType,
    /// Default agent to use (for processing nodes)
    pub default_agent: Option<String>,
    /// Required configuration fields
    pub config_schema: Vec<ConfigField>,
}

/// Configuration field for a node template.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigField {
    pub name: String,
    pub label: String,
    pub field_type: String, // "text", "number", "select", "cron"
    pub required: bool,
    pub default_value: Option<serde_json::Value>,
    pub options: Option<Vec<String>>, // For select fields
}

/// Get all available node templates for the palette.
pub fn available_templates() -> Vec<NodeTemplate> {
    vec![
        // === Trigger Nodes ===
        NodeTemplate {
            id: "trigger_email".into(),
            label: "Email Received".into(),
            category: NodeCategory::Trigger,
            description: "Triggered when a new email arrives".into(),
            icon: "📧".into(),
            output_type: DataType::Email,
            input_type: DataType::Any,
            default_agent: None,
            config_schema: vec![
                ConfigField {
                    name: "filter".into(), label: "Filter".into(),
                    field_type: "text".into(), required: false,
                    default_value: None, options: None,
                },
            ],
        },
        NodeTemplate {
            id: "trigger_cron".into(),
            label: "Scheduled".into(),
            category: NodeCategory::Trigger,
            description: "Runs on a schedule (cron expression)".into(),
            icon: "⏰".into(),
            output_type: DataType::Event,
            input_type: DataType::Any,
            default_agent: None,
            config_schema: vec![
                ConfigField {
                    name: "schedule".into(), label: "Schedule".into(),
                    field_type: "cron".into(), required: true,
                    default_value: Some(serde_json::json!("0 9 * * *")),
                    options: None,
                },
            ],
        },
        NodeTemplate {
            id: "trigger_webhook".into(),
            label: "Webhook".into(),
            category: NodeCategory::Trigger,
            description: "Triggered by an incoming webhook".into(),
            icon: "🔗".into(),
            output_type: DataType::Json,
            input_type: DataType::Any,
            default_agent: None,
            config_schema: vec![],
        },
        NodeTemplate {
            id: "trigger_keyword".into(),
            label: "Keyword Detected".into(),
            category: NodeCategory::Trigger,
            description: "Triggered when a keyword appears in a channel".into(),
            icon: "🔍".into(),
            output_type: DataType::Text,
            input_type: DataType::Any,
            default_agent: None,
            config_schema: vec![
                ConfigField {
                    name: "keywords".into(), label: "Keywords".into(),
                    field_type: "text".into(), required: true,
                    default_value: None, options: None,
                },
                ConfigField {
                    name: "channel".into(), label: "Channel".into(),
                    field_type: "select".into(), required: false,
                    default_value: None, options: Some(vec!["any".into(), "slack".into(), "discord".into(), "telegram".into()]),
                },
            ],
        },
        NodeTemplate {
            id: "trigger_file_changed".into(),
            label: "File Changed".into(),
            category: NodeCategory::Trigger,
            description: "Triggered when a file is modified".into(),
            icon: "📄".into(),
            output_type: DataType::File,
            input_type: DataType::Any,
            default_agent: None,
            config_schema: vec![
                ConfigField {
                    name: "path".into(), label: "Watch Path".into(),
                    field_type: "text".into(), required: true,
                    default_value: None, options: None,
                },
            ],
        },

        // === Processing Nodes ===
        NodeTemplate {
            id: "process_summarize".into(),
            label: "Summarize Text".into(),
            category: NodeCategory::Processing,
            description: "Summarize input text using AI".into(),
            icon: "📝".into(),
            output_type: DataType::Text,
            input_type: DataType::Text,
            default_agent: Some("summarizer".into()),
            config_schema: vec![
                ConfigField {
                    name: "length".into(), label: "Summary Length".into(),
                    field_type: "select".into(), required: false,
                    default_value: Some(serde_json::json!("medium")),
                    options: Some(vec!["brief".into(), "medium".into(), "detailed".into()]),
                },
            ],
        },
        NodeTemplate {
            id: "process_translate".into(),
            label: "Translate".into(),
            category: NodeCategory::Processing,
            description: "Translate text to another language".into(),
            icon: "🌐".into(),
            output_type: DataType::Text,
            input_type: DataType::Text,
            default_agent: Some("translator".into()),
            config_schema: vec![
                ConfigField {
                    name: "target_language".into(), label: "Target Language".into(),
                    field_type: "select".into(), required: true,
                    default_value: None,
                    options: Some(vec![
                        "English".into(), "Spanish".into(), "French".into(),
                        "German".into(), "Chinese".into(), "Japanese".into(),
                        "Korean".into(), "Arabic".into(), "Hindi".into(),
                        "Portuguese".into(),
                    ]),
                },
            ],
        },
        NodeTemplate {
            id: "process_analyze".into(),
            label: "Analyze Sentiment".into(),
            category: NodeCategory::Processing,
            description: "Analyze sentiment of input text".into(),
            icon: "💡".into(),
            output_type: DataType::Json,
            input_type: DataType::Text,
            default_agent: Some("analyst".into()),
            config_schema: vec![],
        },
        NodeTemplate {
            id: "process_extract".into(),
            label: "Extract Data".into(),
            category: NodeCategory::Processing,
            description: "Extract structured data from text".into(),
            icon: "🔬".into(),
            output_type: DataType::Json,
            input_type: DataType::Text,
            default_agent: Some("data-engineer".into()),
            config_schema: vec![
                ConfigField {
                    name: "schema".into(), label: "Output Schema".into(),
                    field_type: "text".into(), required: false,
                    default_value: None, options: None,
                },
            ],
        },
        NodeTemplate {
            id: "process_generate".into(),
            label: "Generate Response".into(),
            category: NodeCategory::Processing,
            description: "Generate a response using AI".into(),
            icon: "🤖".into(),
            output_type: DataType::Text,
            input_type: DataType::Any,
            default_agent: Some("general-assistant".into()),
            config_schema: vec![
                ConfigField {
                    name: "instructions".into(), label: "Instructions".into(),
                    field_type: "text".into(), required: false,
                    default_value: None, options: None,
                },
            ],
        },
        NodeTemplate {
            id: "process_code".into(),
            label: "Run Code".into(),
            category: NodeCategory::Processing,
            description: "Execute code on the input".into(),
            icon: "💻".into(),
            output_type: DataType::Any,
            input_type: DataType::Any,
            default_agent: Some("coder".into()),
            config_schema: vec![
                ConfigField {
                    name: "language".into(), label: "Language".into(),
                    field_type: "select".into(), required: false,
                    default_value: Some(serde_json::json!("python")),
                    options: Some(vec!["python".into(), "javascript".into(), "rust".into()]),
                },
            ],
        },

        // === Output Nodes ===
        NodeTemplate {
            id: "output_whatsapp".into(),
            label: "Send WhatsApp".into(),
            category: NodeCategory::Output,
            description: "Send message via WhatsApp".into(),
            icon: "💬".into(),
            output_type: DataType::Text,
            input_type: DataType::Text,
            default_agent: None,
            config_schema: vec![
                ConfigField {
                    name: "recipient".into(), label: "Recipient".into(),
                    field_type: "text".into(), required: true,
                    default_value: None, options: None,
                },
            ],
        },
        NodeTemplate {
            id: "output_slack".into(),
            label: "Post to Slack".into(),
            category: NodeCategory::Output,
            description: "Post message to a Slack channel".into(),
            icon: "📢".into(),
            output_type: DataType::Text,
            input_type: DataType::Text,
            default_agent: None,
            config_schema: vec![
                ConfigField {
                    name: "channel".into(), label: "Channel".into(),
                    field_type: "text".into(), required: true,
                    default_value: None, options: None,
                },
            ],
        },
        NodeTemplate {
            id: "output_save_file".into(),
            label: "Save File".into(),
            category: NodeCategory::Output,
            description: "Save output to a file".into(),
            icon: "💾".into(),
            output_type: DataType::File,
            input_type: DataType::Any,
            default_agent: None,
            config_schema: vec![
                ConfigField {
                    name: "path".into(), label: "File Path".into(),
                    field_type: "text".into(), required: true,
                    default_value: None, options: None,
                },
            ],
        },
        NodeTemplate {
            id: "output_email".into(),
            label: "Send Email".into(),
            category: NodeCategory::Output,
            description: "Send an email".into(),
            icon: "📤".into(),
            output_type: DataType::Text,
            input_type: DataType::Text,
            default_agent: Some("email-assistant".into()),
            config_schema: vec![
                ConfigField {
                    name: "to".into(), label: "To".into(),
                    field_type: "text".into(), required: true,
                    default_value: None, options: None,
                },
                ConfigField {
                    name: "subject".into(), label: "Subject".into(),
                    field_type: "text".into(), required: true,
                    default_value: None, options: None,
                },
            ],
        },
        NodeTemplate {
            id: "output_notification".into(),
            label: "Send Notification".into(),
            category: NodeCategory::Output,
            description: "Push notification to desktop/mobile".into(),
            icon: "🔔".into(),
            output_type: DataType::Text,
            input_type: DataType::Any,
            default_agent: None,
            config_schema: vec![
                ConfigField {
                    name: "title".into(), label: "Title".into(),
                    field_type: "text".into(), required: false,
                    default_value: None, options: None,
                },
            ],
        },
    ]
}

// ---------------------------------------------------------------------------
// Visual DAG — The complete skill definition
// ---------------------------------------------------------------------------

/// A complete visual DAG representing a skill automation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisualDAG {
    /// Skill name
    pub name: String,
    /// Skill description
    pub description: String,
    /// All nodes in the DAG
    pub nodes: Vec<VisualNode>,
    /// All edges (connections between nodes)
    pub edges: Vec<VisualEdge>,
}

/// Validation error for a visual DAG.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationError {
    pub code: String,
    pub message: String,
    pub node_id: Option<VisualNodeId>,
}

impl VisualDAG {
    /// Validate the DAG structure.
    ///
    /// Checks:
    /// 1. Acyclicity via DFS: O(V + E)
    /// 2. Type compatibility: output_type(u) ⊆ input_type(v) per edge
    /// 3. Reachability: every output reachable from a trigger
    /// 4. At least one trigger and one output
    pub fn validate(&self) -> Vec<ValidationError> {
        let mut errors = Vec::new();

        // Build the graph
        let node_map: HashMap<&str, &VisualNode> = self.nodes.iter()
            .map(|n| (n.id.as_str(), n))
            .collect();

        let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
        for edge in &self.edges {
            adj.entry(edge.from.as_str()).or_default().push(edge.to.as_str());
        }

        // Check 1: At least one trigger and one output
        let triggers: Vec<&VisualNode> = self.nodes.iter()
            .filter(|n| n.category == NodeCategory::Trigger)
            .collect();
        let outputs: Vec<&VisualNode> = self.nodes.iter()
            .filter(|n| n.category == NodeCategory::Output)
            .collect();

        if triggers.is_empty() {
            errors.push(ValidationError {
                code: "no_trigger".into(),
                message: "At least one trigger node is required".into(),
                node_id: None,
            });
        }
        if outputs.is_empty() {
            errors.push(ValidationError {
                code: "no_output".into(),
                message: "At least one output node is required".into(),
                node_id: None,
            });
        }

        // Check 2: Acyclicity via DFS — O(V + E)
        if self.has_cycle(&adj) {
            errors.push(ValidationError {
                code: "cycle_detected".into(),
                message: "The workflow contains a cycle — remove circular connections".into(),
                node_id: None,
            });
        }

        // Check 3: Type compatibility per edge
        for edge in &self.edges {
            if let (Some(from_node), Some(to_node)) = (
                node_map.get(edge.from.as_str()),
                node_map.get(edge.to.as_str()),
            ) {
                if !from_node.output_type.is_compatible_with(&to_node.input_type) {
                    errors.push(ValidationError {
                        code: "type_mismatch".into(),
                        message: format!(
                            "Output type {:?} from '{}' is not compatible with input type {:?} of '{}'",
                            from_node.output_type, from_node.label,
                            to_node.input_type, to_node.label
                        ),
                        node_id: Some(edge.to.clone()),
                    });
                }
            }
        }

        // Check 4: Every output reachable from a trigger — BFS O(V + E)
        let trigger_ids: HashSet<&str> = triggers.iter().map(|t| t.id.as_str()).collect();
        let mut reachable: HashSet<&str> = HashSet::new();
        let mut queue: VecDeque<&str> = trigger_ids.iter().copied().collect();

        while let Some(node_id) = queue.pop_front() {
            if !reachable.insert(node_id) {
                continue;
            }
            if let Some(neighbors) = adj.get(node_id) {
                for &next in neighbors {
                    if !reachable.contains(next) {
                        queue.push_back(next);
                    }
                }
            }
        }

        for output in &outputs {
            if !reachable.contains(output.id.as_str()) {
                errors.push(ValidationError {
                    code: "unreachable_output".into(),
                    message: format!("Output '{}' is not reachable from any trigger", output.label),
                    node_id: Some(output.id.clone()),
                });
            }
        }

        errors
    }

    /// Check for cycles via DFS — O(V + E).
    fn has_cycle(&self, adj: &HashMap<&str, Vec<&str>>) -> bool {
        let mut visited: HashSet<&str> = HashSet::new();
        let mut in_stack: HashSet<&str> = HashSet::new();

        for node in &self.nodes {
            if !visited.contains(node.id.as_str()) {
                if self.dfs_cycle(node.id.as_str(), adj, &mut visited, &mut in_stack) {
                    return true;
                }
            }
        }
        false
    }

    fn dfs_cycle<'a>(
        &self,
        node: &'a str,
        adj: &HashMap<&str, Vec<&'a str>>,
        visited: &mut HashSet<&'a str>,
        in_stack: &mut HashSet<&'a str>,
    ) -> bool {
        visited.insert(node);
        in_stack.insert(node);

        if let Some(neighbors) = adj.get(node) {
            for &next in neighbors {
                if !visited.contains(next) {
                    if self.dfs_cycle(next, adj, visited, in_stack) {
                        return true;
                    }
                } else if in_stack.contains(next) {
                    return true;
                }
            }
        }

        in_stack.remove(node);
        false
    }

    /// Compile the visual DAG into a SkillScaffoldInput.
    ///
    /// This converts the visual representation into the format expected
    /// by the scaffold generator, producing a complete skill.toml + prompt.md.
    pub fn compile_to_scaffold_input(&self) -> Result<SkillBuilderOutput, Vec<ValidationError>> {
        let errors = self.validate();
        if !errors.is_empty() {
            return Err(errors);
        }

        // Collect all tools needed
        let tools: Vec<String> = self.nodes.iter()
            .filter_map(|n| match n.category {
                NodeCategory::Trigger => Some(format!("trigger_{}", n.template_id)),
                NodeCategory::Output => Some(format!("output_{}", n.template_id)),
                _ => None,
            })
            .collect();

        // Collect agent IDs used
        let agents: Vec<String> = self.nodes.iter()
            .filter_map(|n| n.agent_id.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();

        // Compute required capabilities
        let capabilities = self.compute_required_capabilities();

        // Build the trigger description
        let trigger_nodes: Vec<&VisualNode> = self.nodes.iter()
            .filter(|n| n.category == NodeCategory::Trigger)
            .collect();
        let trigger_desc: Vec<String> = trigger_nodes.iter()
            .map(|t| t.label.clone())
            .collect();

        // Build processing pipeline description
        let proc_nodes: Vec<&VisualNode> = self.nodes.iter()
            .filter(|n| n.category == NodeCategory::Processing)
            .collect();
        let proc_desc: Vec<String> = proc_nodes.iter()
            .map(|p| p.label.clone())
            .collect();

        // Build output description
        let output_nodes: Vec<&VisualNode> = self.nodes.iter()
            .filter(|n| n.category == NodeCategory::Output)
            .collect();
        let output_desc: Vec<String> = output_nodes.iter()
            .map(|o| o.label.clone())
            .collect();

        // Generate prompt template
        let prompt = format!(
            "# {}\n\n{}\n\n## Triggers\n{}\n\n## Processing\n{}\n\n## Outputs\n{}\n",
            self.name,
            self.description,
            trigger_desc.iter().map(|t| format!("- {}", t)).collect::<Vec<_>>().join("\n"),
            proc_desc.iter().map(|p| format!("- {}", p)).collect::<Vec<_>>().join("\n"),
            output_desc.iter().map(|o| format!("- {}", o)).collect::<Vec<_>>().join("\n"),
        );

        // Generate skill ID from name
        let skill_id = self.name
            .to_lowercase()
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '-' })
            .collect::<String>()
            .trim_matches('-')
            .to_string();

        Ok(SkillBuilderOutput {
            skill_id,
            name: self.name.clone(),
            description: self.description.clone(),
            tools,
            agents,
            capabilities,
            trigger_descriptions: trigger_desc,
            processing_descriptions: proc_desc,
            output_descriptions: output_desc,
            prompt_template: prompt,
            dag_json: serde_json::to_string(self).unwrap_or_default(),
            node_count: self.nodes.len(),
            edge_count: self.edges.len(),
        })
    }

    /// Compute required capabilities based on node types.
    fn compute_required_capabilities(&self) -> Vec<String> {
        let mut caps = HashSet::new();
        for node in &self.nodes {
            match node.template_id.as_str() {
                t if t.contains("email") => { caps.insert("network"); caps.insert("channel_send"); }
                t if t.contains("webhook") => { caps.insert("network"); caps.insert("network_listen"); }
                t if t.contains("file") => { caps.insert("file_read"); caps.insert("file_write"); }
                t if t.contains("slack") | t.contains("whatsapp") | t.contains("discord") => {
                    caps.insert("network"); caps.insert("channel_send");
                }
                t if t.contains("code") => { caps.insert("shell_exec"); caps.insert("process_spawn"); }
                t if t.contains("cron") => { caps.insert("cron"); }
                _ => { caps.insert("tool_invoke"); }
            }
        }
        // Always need memory access for context
        caps.insert("memory_read");
        caps.into_iter().map(String::from).collect()
    }
}

/// Output of compiling a visual DAG into a skill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillBuilderOutput {
    pub skill_id: String,
    pub name: String,
    pub description: String,
    pub tools: Vec<String>,
    pub agents: Vec<String>,
    pub capabilities: Vec<String>,
    pub trigger_descriptions: Vec<String>,
    pub processing_descriptions: Vec<String>,
    pub output_descriptions: Vec<String>,
    pub prompt_template: String,
    /// The serialized DAG JSON (stored in skill metadata for re-editing)
    pub dag_json: String,
    pub node_count: usize,
    pub edge_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_dag() -> VisualDAG {
        VisualDAG {
            name: "Email Summarizer".into(),
            description: "Summarizes incoming emails and sends to Slack".into(),
            nodes: vec![
                VisualNode {
                    id: "t1".into(), label: "Email Received".into(),
                    category: NodeCategory::Trigger, template_id: "trigger_email".into(),
                    config: HashMap::new(), output_type: DataType::Email,
                    input_type: DataType::Any, position: NodePosition { x: 0.0, y: 0.0 },
                    agent_id: None,
                },
                VisualNode {
                    id: "p1".into(), label: "Summarize".into(),
                    category: NodeCategory::Processing, template_id: "process_summarize".into(),
                    config: HashMap::new(), output_type: DataType::Text,
                    input_type: DataType::Text, position: NodePosition { x: 200.0, y: 0.0 },
                    agent_id: Some("summarizer".into()),
                },
                VisualNode {
                    id: "o1".into(), label: "Post to Slack".into(),
                    category: NodeCategory::Output, template_id: "output_slack".into(),
                    config: HashMap::new(), output_type: DataType::Text,
                    input_type: DataType::Text, position: NodePosition { x: 400.0, y: 0.0 },
                    agent_id: None,
                },
            ],
            edges: vec![
                VisualEdge { from: "t1".into(), to: "p1".into() },
                VisualEdge { from: "p1".into(), to: "o1".into() },
            ],
        }
    }

    #[test]
    fn valid_dag_passes_validation() {
        let dag = make_dag();
        let errors = dag.validate();
        assert!(errors.is_empty(), "Expected no errors, got: {:?}", errors);
    }

    #[test]
    fn dag_without_trigger_fails() {
        let mut dag = make_dag();
        dag.nodes.retain(|n| n.category != NodeCategory::Trigger);
        dag.edges.retain(|e| e.from != "t1");
        let errors = dag.validate();
        assert!(errors.iter().any(|e| e.code == "no_trigger"));
    }

    #[test]
    fn dag_with_cycle_fails() {
        let mut dag = make_dag();
        dag.edges.push(VisualEdge { from: "o1".into(), to: "t1".into() });
        let errors = dag.validate();
        assert!(errors.iter().any(|e| e.code == "cycle_detected"));
    }

    #[test]
    fn type_mismatch_detected() {
        let mut dag = make_dag();
        // Change summarize output to Image, which is incompatible with Slack's Text input
        dag.nodes[1].output_type = DataType::Image;
        let errors = dag.validate();
        assert!(errors.iter().any(|e| e.code == "type_mismatch"));
    }

    #[test]
    fn unreachable_output_detected() {
        let mut dag = make_dag();
        // Add an output with no incoming edge
        dag.nodes.push(VisualNode {
            id: "o2".into(), label: "Orphan".into(),
            category: NodeCategory::Output, template_id: "output_notification".into(),
            config: HashMap::new(), output_type: DataType::Text,
            input_type: DataType::Any, position: NodePosition { x: 400.0, y: 100.0 },
            agent_id: None,
        });
        let errors = dag.validate();
        assert!(errors.iter().any(|e| e.code == "unreachable_output"));
    }

    #[test]
    fn compiles_to_scaffold() {
        let dag = make_dag();
        let output = dag.compile_to_scaffold_input().unwrap();
        assert_eq!(output.name, "Email Summarizer");
        assert!(!output.tools.is_empty());
        assert!(output.agents.contains(&"summarizer".to_string()));
        assert!(!output.prompt_template.is_empty());
    }

    #[test]
    fn computes_capabilities() {
        let dag = make_dag();
        let output = dag.compile_to_scaffold_input().unwrap();
        assert!(output.capabilities.contains(&"network".to_string()));
        assert!(output.capabilities.contains(&"channel_send".to_string()));
    }

    #[test]
    fn available_templates_cover_all_categories() {
        let templates = available_templates();
        assert!(templates.iter().any(|t| t.category == NodeCategory::Trigger));
        assert!(templates.iter().any(|t| t.category == NodeCategory::Processing));
        assert!(templates.iter().any(|t| t.category == NodeCategory::Output));
    }
}
