//! # Life OS Template Registry
//!
//! Pre-built pipeline templates for common Life OS workflows.
//! Each template defines a composable pipeline of agent steps
//! that can be instantiated via the skill system.
//!
//! ## Templates
//!
//! | Template              | Description                                    |
//! |-----------------------|------------------------------------------------|
//! | morning-briefing      | Daily digest of emails, calendar, priorities   |
//! | email-triage          | Classify and prioritize inbox                  |
//! | relationship-monitor  | Contact health alerts via Hawkes decay          |
//! | meeting-actions       | Extract action items from transcripts          |
//! | social-dashboard      | Social metric anomaly report                   |
//! | advisory-council      | Multi-expert consensus synthesis               |
//! | security-review       | Periodic security posture assessment           |
//! | knowledge-base        | Auto-index and surface relevant memories        |
//! | food-journal          | Structured food logging with trigger analysis   |
//! | backup-sync           | Automated backup + git sync orchestration       |

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A pipeline template definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineTemplate {
    /// Unique template identifier
    pub id: String,
    /// Human-readable name
    pub name: String,
    /// Description of what the template does
    pub description: String,
    /// Category for grouping
    pub category: TemplateCategory,
    /// Pipeline steps in execution order
    pub steps: Vec<TemplateStep>,
    /// Required skills
    pub required_skills: Vec<String>,
    /// Default cron schedule (optional)
    pub default_schedule: Option<String>,
    /// Whether this requires user approval before execution
    pub requires_approval: bool,
    /// Template variables that users can customize
    pub variables: Vec<TemplateVariable>,
    /// Version string
    pub version: String,
}

/// Category of pipeline template.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum TemplateCategory {
    Communication,
    Productivity,
    Health,
    Social,
    Security,
    Infrastructure,
    Knowledge,
    Analysis,
}

/// A step within a pipeline template.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateStep {
    /// Step identifier
    pub id: String,
    /// Step type (maps to PipelineStep variants)
    pub step_type: StepType,
    /// Agent/skill to use for this step
    pub agent_or_skill: String,
    /// Prompt template (may contain {{variable}} placeholders)
    pub prompt_template: Option<String>,
    /// Dependencies (step IDs that must complete first)
    pub depends_on: Vec<String>,
    /// Timeout in seconds
    pub timeout_secs: Option<u64>,
}

/// Type of pipeline step.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum StepType {
    Agent,
    Parallel,
    Gate,
    Transform,
    Council,
}

/// A customizable variable in a template.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateVariable {
    /// Variable name (used in {{name}} placeholders)
    pub name: String,
    /// Description of what this variable controls
    pub description: String,
    /// Default value
    pub default: String,
    /// Whether this is required
    pub required: bool,
}

/// Registry of all available pipeline templates.
pub struct TemplateRegistry {
    templates: HashMap<String, PipelineTemplate>,
}

impl TemplateRegistry {
    pub fn new() -> Self {
        Self {
            templates: HashMap::new(),
        }
    }

    /// Create a registry with all built-in Life OS templates.
    pub fn with_builtins() -> Self {
        let mut reg = Self::new();
        for template in builtin_templates() {
            reg.register(template);
        }
        reg
    }

    /// Register a template.
    pub fn register(&mut self, template: PipelineTemplate) {
        self.templates.insert(template.id.clone(), template);
    }

    /// Look up a template by ID.
    pub fn get(&self, id: &str) -> Option<&PipelineTemplate> {
        self.templates.get(id)
    }

    /// List all templates.
    pub fn list(&self) -> Vec<&PipelineTemplate> {
        self.templates.values().collect()
    }

    /// List templates by category.
    pub fn by_category(&self, category: TemplateCategory) -> Vec<&PipelineTemplate> {
        self.templates.values()
            .filter(|t| t.category == category)
            .collect()
    }

    /// Instantiate a template with variable substitutions.
    pub fn instantiate(
        &self,
        template_id: &str,
        variables: &HashMap<String, String>,
    ) -> Option<PipelineTemplate> {
        let template = self.get(template_id)?;
        let mut instance = template.clone();

        // Substitute variables in prompt templates
        for step in &mut instance.steps {
            if let Some(ref mut prompt) = step.prompt_template {
                for (key, value) in variables {
                    *prompt = prompt.replace(&format!("{{{{{}}}}}", key), value);
                }
                // Apply defaults for unset variables
                for var in &template.variables {
                    *prompt = prompt.replace(
                        &format!("{{{{{}}}}}", var.name),
                        &var.default,
                    );
                }
            }
        }

        Some(instance)
    }

    /// Number of registered templates.
    pub fn count(&self) -> usize {
        self.templates.len()
    }
}

impl Default for TemplateRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Generate all built-in Life OS pipeline templates.
fn builtin_templates() -> Vec<PipelineTemplate> {
    vec![
        PipelineTemplate {
            id: "morning-briefing".into(),
            name: "Morning Briefing".into(),
            description: "Daily digest of emails, calendar events, priorities, and relationship alerts".into(),
            category: TemplateCategory::Productivity,
            steps: vec![
                TemplateStep {
                    id: "fetch-emails".into(),
                    step_type: StepType::Parallel,
                    agent_or_skill: "email-reader".into(),
                    prompt_template: Some("Summarize unread emails from the last {{hours}} hours, prioritizing by urgency.".into()),
                    depends_on: vec![],
                    timeout_secs: Some(30),
                },
                TemplateStep {
                    id: "fetch-calendar".into(),
                    step_type: StepType::Parallel,
                    agent_or_skill: "calendar-reader".into(),
                    prompt_template: Some("List today's calendar events and prep notes.".into()),
                    depends_on: vec![],
                    timeout_secs: Some(15),
                },
                TemplateStep {
                    id: "check-contacts".into(),
                    step_type: StepType::Parallel,
                    agent_or_skill: "relationship-monitor".into(),
                    prompt_template: Some("Report contacts with health score below {{contact_threshold}}.".into()),
                    depends_on: vec![],
                    timeout_secs: Some(10),
                },
                TemplateStep {
                    id: "synthesize".into(),
                    step_type: StepType::Agent,
                    agent_or_skill: "briefing-composer".into(),
                    prompt_template: Some("Compose a concise morning briefing from the gathered data. Style: {{style}}.".into()),
                    depends_on: vec!["fetch-emails".into(), "fetch-calendar".into(), "check-contacts".into()],
                    timeout_secs: Some(30),
                },
            ],
            required_skills: vec!["email".into(), "calendar".into(), "contacts".into()],
            default_schedule: Some("0 7 * * *".into()),
            requires_approval: false,
            variables: vec![
                TemplateVariable { name: "hours".into(), description: "Hours of email history to scan".into(), default: "12".into(), required: false },
                TemplateVariable { name: "contact_threshold".into(), description: "Contact health score alert threshold".into(), default: "0.3".into(), required: false },
                TemplateVariable { name: "style".into(), description: "Briefing style (concise/detailed/bullet)".into(), default: "concise".into(), required: false },
            ],
            version: "1.0.0".into(),
        },
        PipelineTemplate {
            id: "email-triage".into(),
            name: "Email Triage".into(),
            description: "Classify and prioritize inbox, draft responses for routine items".into(),
            category: TemplateCategory::Communication,
            steps: vec![
                TemplateStep {
                    id: "classify".into(),
                    step_type: StepType::Agent,
                    agent_or_skill: "email-classifier".into(),
                    prompt_template: Some("Classify each email as: urgent, action-needed, informational, or spam.".into()),
                    depends_on: vec![],
                    timeout_secs: Some(60),
                },
                TemplateStep {
                    id: "approve".into(),
                    step_type: StepType::Gate,
                    agent_or_skill: "approval-gate".into(),
                    prompt_template: Some("Review classification results before proceeding with drafts.".into()),
                    depends_on: vec!["classify".into()],
                    timeout_secs: Some(300),
                },
                TemplateStep {
                    id: "draft".into(),
                    step_type: StepType::Agent,
                    agent_or_skill: "email-drafter".into(),
                    prompt_template: Some("Draft responses for action-needed emails matching user's tone.".into()),
                    depends_on: vec!["approve".into()],
                    timeout_secs: Some(120),
                },
            ],
            required_skills: vec!["email".into()],
            default_schedule: Some("0 */2 * * *".into()),
            requires_approval: true,
            variables: vec![],
            version: "1.0.0".into(),
        },
        PipelineTemplate {
            id: "relationship-monitor".into(),
            name: "Relationship Monitor".into(),
            description: "Track contact health via Hawkes decay, alert on fading relationships".into(),
            category: TemplateCategory::Social,
            steps: vec![
                TemplateStep {
                    id: "scan".into(),
                    step_type: StepType::Agent,
                    agent_or_skill: "contact-scanner".into(),
                    prompt_template: Some("Scan contacts with health score below {{threshold}} and no interaction in {{days}} days.".into()),
                    depends_on: vec![],
                    timeout_secs: Some(15),
                },
                TemplateStep {
                    id: "suggest".into(),
                    step_type: StepType::Agent,
                    agent_or_skill: "outreach-advisor".into(),
                    prompt_template: Some("Suggest personalized outreach for each flagged contact based on past interactions.".into()),
                    depends_on: vec!["scan".into()],
                    timeout_secs: Some(30),
                },
            ],
            required_skills: vec!["contacts".into()],
            default_schedule: Some("0 9 * * 1".into()),
            requires_approval: false,
            variables: vec![
                TemplateVariable { name: "threshold".into(), description: "Health score threshold".into(), default: "0.3".into(), required: false },
                TemplateVariable { name: "days".into(), description: "Days since last interaction".into(), default: "30".into(), required: false },
            ],
            version: "1.0.0".into(),
        },
        PipelineTemplate {
            id: "meeting-actions".into(),
            name: "Meeting Action Items".into(),
            description: "Extract action items, decisions, and follow-ups from meeting transcripts".into(),
            category: TemplateCategory::Productivity,
            steps: vec![
                TemplateStep {
                    id: "parse-transcript".into(),
                    step_type: StepType::Agent,
                    agent_or_skill: "transcript-parser".into(),
                    prompt_template: Some("Parse the meeting transcript and extract: decisions made, action items with owners, open questions, and follow-up dates.".into()),
                    depends_on: vec![],
                    timeout_secs: Some(60),
                },
                TemplateStep {
                    id: "create-tasks".into(),
                    step_type: StepType::Agent,
                    agent_or_skill: "task-creator".into(),
                    prompt_template: Some("Create structured tasks from extracted action items.".into()),
                    depends_on: vec!["parse-transcript".into()],
                    timeout_secs: Some(30),
                },
            ],
            required_skills: vec!["transcription".into()],
            default_schedule: None,
            requires_approval: false,
            variables: vec![],
            version: "1.0.0".into(),
        },
        PipelineTemplate {
            id: "social-dashboard".into(),
            name: "Social Dashboard".into(),
            description: "Social metric anomaly report with EWMA trend analysis".into(),
            category: TemplateCategory::Social,
            steps: vec![
                TemplateStep {
                    id: "collect".into(),
                    step_type: StepType::Parallel,
                    agent_or_skill: "social-collector".into(),
                    prompt_template: Some("Collect metrics from configured social platforms.".into()),
                    depends_on: vec![],
                    timeout_secs: Some(60),
                },
                TemplateStep {
                    id: "analyze".into(),
                    step_type: StepType::Agent,
                    agent_or_skill: "anomaly-analyzer".into(),
                    prompt_template: Some("Analyze social metrics for anomalies (z > {{z_threshold}}) and trends.".into()),
                    depends_on: vec!["collect".into()],
                    timeout_secs: Some(30),
                },
            ],
            required_skills: vec!["social".into()],
            default_schedule: Some("0 18 * * *".into()),
            requires_approval: false,
            variables: vec![
                TemplateVariable { name: "z_threshold".into(), description: "Z-score threshold for anomaly alerts".into(), default: "2.0".into(), required: false },
            ],
            version: "1.0.0".into(),
        },
        PipelineTemplate {
            id: "advisory-council".into(),
            name: "Advisory Council".into(),
            description: "Multi-expert consensus synthesis via Dempster-Shafer theory".into(),
            category: TemplateCategory::Analysis,
            steps: vec![
                TemplateStep {
                    id: "expert-1".into(),
                    step_type: StepType::Parallel,
                    agent_or_skill: "expert-strategic".into(),
                    prompt_template: Some("Analyze the question from a strategic perspective: {{question}}".into()),
                    depends_on: vec![],
                    timeout_secs: Some(60),
                },
                TemplateStep {
                    id: "expert-2".into(),
                    step_type: StepType::Parallel,
                    agent_or_skill: "expert-technical".into(),
                    prompt_template: Some("Analyze the question from a technical perspective: {{question}}".into()),
                    depends_on: vec![],
                    timeout_secs: Some(60),
                },
                TemplateStep {
                    id: "expert-3".into(),
                    step_type: StepType::Parallel,
                    agent_or_skill: "expert-ethical".into(),
                    prompt_template: Some("Analyze the question from an ethical perspective: {{question}}".into()),
                    depends_on: vec![],
                    timeout_secs: Some(60),
                },
                TemplateStep {
                    id: "synthesize".into(),
                    step_type: StepType::Council,
                    agent_or_skill: "council-synthesizer".into(),
                    prompt_template: Some("Synthesize expert perspectives using Dempster-Shafer combination. Highlight conflicts above {{conflict_threshold}}.".into()),
                    depends_on: vec!["expert-1".into(), "expert-2".into(), "expert-3".into()],
                    timeout_secs: Some(90),
                },
            ],
            required_skills: vec![],
            default_schedule: None,
            requires_approval: false,
            variables: vec![
                TemplateVariable { name: "question".into(), description: "The question for the council".into(), default: "".into(), required: true },
                TemplateVariable { name: "conflict_threshold".into(), description: "Conflict threshold for flagging disagreements".into(), default: "0.3".into(), required: false },
            ],
            version: "1.0.0".into(),
        },
        PipelineTemplate {
            id: "security-review".into(),
            name: "Security Review".into(),
            description: "Periodic security posture assessment".into(),
            category: TemplateCategory::Security,
            steps: vec![
                TemplateStep {
                    id: "audit".into(),
                    step_type: StepType::Agent,
                    agent_or_skill: "security-auditor".into(),
                    prompt_template: Some("Review security configuration: TLS status, token expiration, rate limit health, recent auth failures.".into()),
                    depends_on: vec![],
                    timeout_secs: Some(30),
                },
                TemplateStep {
                    id: "report".into(),
                    step_type: StepType::Agent,
                    agent_or_skill: "security-reporter".into(),
                    prompt_template: Some("Generate a security posture report with risk ratings and recommended actions.".into()),
                    depends_on: vec!["audit".into()],
                    timeout_secs: Some(30),
                },
            ],
            required_skills: vec!["security".into()],
            default_schedule: Some("0 3 * * 0".into()),
            requires_approval: false,
            variables: vec![],
            version: "1.0.0".into(),
        },
        PipelineTemplate {
            id: "knowledge-base".into(),
            name: "Knowledge Base Indexer".into(),
            description: "Auto-index conversations, documents, and notes into searchable knowledge".into(),
            category: TemplateCategory::Knowledge,
            steps: vec![
                TemplateStep {
                    id: "extract".into(),
                    step_type: StepType::Agent,
                    agent_or_skill: "knowledge-extractor".into(),
                    prompt_template: Some("Extract key facts, relationships, and insights from recent conversations.".into()),
                    depends_on: vec![],
                    timeout_secs: Some(60),
                },
                TemplateStep {
                    id: "index".into(),
                    step_type: StepType::Agent,
                    agent_or_skill: "knowledge-indexer".into(),
                    prompt_template: Some("Store extracted knowledge with embeddings for semantic retrieval.".into()),
                    depends_on: vec!["extract".into()],
                    timeout_secs: Some(30),
                },
            ],
            required_skills: vec!["memory".into()],
            default_schedule: Some("0 2 * * *".into()),
            requires_approval: false,
            variables: vec![],
            version: "1.0.0".into(),
        },
        PipelineTemplate {
            id: "food-journal".into(),
            name: "Food Journal".into(),
            description: "Structured food logging with photo analysis and trigger identification".into(),
            category: TemplateCategory::Health,
            steps: vec![
                TemplateStep {
                    id: "log".into(),
                    step_type: StepType::Agent,
                    agent_or_skill: "food-logger".into(),
                    prompt_template: Some("Parse food entry: extract items, estimate calories and macros. If photo attached, use vision.".into()),
                    depends_on: vec![],
                    timeout_secs: Some(30),
                },
                TemplateStep {
                    id: "analyze".into(),
                    step_type: StepType::Agent,
                    agent_or_skill: "trigger-analyzer".into(),
                    prompt_template: Some("Run case-crossover analysis on last {{days}} days for configured triggers.".into()),
                    depends_on: vec!["log".into()],
                    timeout_secs: Some(30),
                },
            ],
            required_skills: vec!["journal".into()],
            default_schedule: None,
            requires_approval: false,
            variables: vec![
                TemplateVariable { name: "days".into(), description: "Days of history for trigger analysis".into(), default: "30".into(), required: false },
            ],
            version: "1.0.0".into(),
        },
        PipelineTemplate {
            id: "backup-sync".into(),
            name: "Backup & Sync".into(),
            description: "Automated encrypted backup + git sync orchestration".into(),
            category: TemplateCategory::Infrastructure,
            steps: vec![
                TemplateStep {
                    id: "backup".into(),
                    step_type: StepType::Agent,
                    agent_or_skill: "backup-executor".into(),
                    prompt_template: Some("Execute incremental backup, apply GFS retention, verify checksums.".into()),
                    depends_on: vec![],
                    timeout_secs: Some(300),
                },
                TemplateStep {
                    id: "sync".into(),
                    step_type: StepType::Agent,
                    agent_or_skill: "git-sync-executor".into(),
                    prompt_template: Some("Commit pending changes and push to remote.".into()),
                    depends_on: vec!["backup".into()],
                    timeout_secs: Some(60),
                },
            ],
            required_skills: vec![],
            default_schedule: Some("0 1 * * *".into()),
            requires_approval: false,
            variables: vec![],
            version: "1.0.0".into(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_templates() {
        let reg = TemplateRegistry::with_builtins();
        assert_eq!(reg.count(), 10);

        // Check all expected IDs exist
        let expected = vec![
            "morning-briefing", "email-triage", "relationship-monitor",
            "meeting-actions", "social-dashboard", "advisory-council",
            "security-review", "knowledge-base", "food-journal", "backup-sync",
        ];
        for id in expected {
            assert!(reg.get(id).is_some(), "Missing template: {}", id);
        }
    }

    #[test]
    fn test_template_instantiation() {
        let reg = TemplateRegistry::with_builtins();
        let mut vars = HashMap::new();
        vars.insert("hours".into(), "24".into());

        let instance = reg.instantiate("morning-briefing", &vars).unwrap();
        let fetch_step = &instance.steps[0];
        assert!(fetch_step.prompt_template.as_ref().unwrap().contains("24"));
    }

    #[test]
    fn test_by_category() {
        let reg = TemplateRegistry::with_builtins();
        let social = reg.by_category(TemplateCategory::Social);
        assert_eq!(social.len(), 2); // relationship-monitor + social-dashboard
    }
}
