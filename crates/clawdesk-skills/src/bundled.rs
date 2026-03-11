//! Bundled core skills — shipped with ClawDesk.
//!
//! These 15 skills provide baseline agent capabilities without any external
//! skill packages. They are always available and form the minimum viable
//! skill set for a useful agent.
//!
//! ## Categories
//! - **Knowledge**: web search, code analysis, file operations
//! - **Communication**: email compose, summarization, translation
//! - **Productivity**: task management, calendar, memory recall
//! - **Creative**: creative writing, image description, math
//! - **System**: diagnostics, system info, help

use crate::definition::{
    ParameterType, Skill, SkillId, SkillManifest, SkillParameter, SkillSource,
    SkillState, SkillTrigger, SkillToolBinding,
};
use crate::registry::SkillRegistry;
use clawdesk_types::estimate_tokens;

/// Load all bundled core skills into a registry.
pub fn load_bundled_skills() -> SkillRegistry {
    let mut registry = SkillRegistry::new();

    for skill in all_bundled_skills() {
        let id = skill.manifest.id.clone();
        registry.register(skill, SkillSource::Builtin);
        let _ = registry.activate(&id);
    }

    // Register design skills as core skills
    for skill in crate::bundled_design::design_skills_as_core() {
        let id = skill.manifest.id.clone();
        registry.register(skill, SkillSource::Builtin);
        let _ = registry.activate(&id);
    }

    // Load 52 embedded legacy skills from binary (.rodata)
    let embed_result = crate::embedded_openclaw::load_embedded_openclaw_skills(&mut registry);
    if !embed_result.errors.is_empty() {
        tracing::warn!(
            errors = ?embed_result.errors,
            "some embedded legacy skills failed to load"
        );
    }

    // Activate all embedded legacy skills that were just registered.
    // `load_embedded_openclaw_skills` calls `registry.register()` which
    // sets state to Loaded — we need Active for the agent runtime.
    let openclaw_ids: Vec<SkillId> = registry
        .list()
        .iter()
        .filter(|info| info.state == SkillState::Loaded)
        .map(|info| info.id.clone())
        .collect();
    for id in &openclaw_ids {
        let _ = registry.activate(id);
    }

    registry
}

/// Return all bundled core skills.
///
/// Note: email-compose, calendar, and tasks are excluded because they
/// conflict with extension-provided skills (gws-gmail, gws-calendar,
/// gws-tasks). The "Configured Extensions" section in the system prompt
/// tells the LLM which services are available. Generic skills would
/// cause the agent to suggest `gcalcli` or generic email advice instead
/// of using the configured extension.
pub fn all_bundled_skills() -> Vec<Skill> {
    vec![
        web_search(),
        code_analysis(),
        file_operations(),
        // email_compose() — removed: conflicts with gws-gmail, himalaya, etc.
        summarization(),
        translation(),
        // task_management() — removed: conflicts with gws-tasks, todoist, etc.
        // calendar_awareness() — removed: conflicts with gws-calendar, etc.
        memory_recall(),
        creative_writing(),
        image_description(),
        math_computation(),
        system_diagnostics(),
        system_info(),
        help_guide(),
    ]
}

fn make_skill(
    namespace: &str,
    name: &str,
    display_name: &str,
    description: &str,
    prompt: &str,
    tools: Vec<SkillToolBinding>,
    params: Vec<SkillParameter>,
    triggers: Vec<SkillTrigger>,
    priority: f64,
) -> Skill {
    Skill {
        manifest: SkillManifest {
            id: SkillId::new(namespace, name),
            display_name: display_name.to_string(),
            description: description.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            author: Some("ClawDesk".to_string()),
            dependencies: vec![],
            required_tools: tools.iter().map(|t| t.tool_name.clone()).collect(),
            parameters: params,
            triggers,
            estimated_tokens: estimate_tokens(prompt),
            priority_weight: priority,
            tags: vec![namespace.to_string()],
            signature: None,
            publisher_key: None,
            content_hash: None,
            schema_version: 1,
        },
        prompt_fragment: prompt.to_string(),
        provided_tools: tools,
        parameter_values: serde_json::Value::Null,
        source_path: None,
    }
}

fn web_search() -> Skill {
    make_skill(
        "core", "web-search",
        "Web Search",
        "Search the web for current information",
        "You can search the web for current information using the web_search tool. \
         Use this when the user asks about recent events, news, or information \
         that may not be in your training data. Always cite your sources.",
        vec![SkillToolBinding {
            tool_name: "web_search".to_string(),
            description: "Search the web for information".to_string(),
            parameters_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search query" },
                    "num_results": { "type": "integer", "description": "Number of results (1-10)", "default": 5 }
                },
                "required": ["query"]
            }),
        }],
        vec![],
        vec![SkillTrigger::Always],
        10.0,
    )
}

fn code_analysis() -> Skill {
    make_skill(
        "core", "code-analysis",
        "Code Analysis",
        "Analyze, explain, and debug code",
        "You can analyze code in any programming language. When shown code, \
         provide clear explanations, identify bugs, suggest improvements, \
         and follow best practices. Use the code_execute tool to run code \
         snippets when verification is needed.",
        vec![SkillToolBinding {
            tool_name: "code_execute".to_string(),
            description: "Execute a code snippet in a sandboxed environment".to_string(),
            parameters_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "language": { "type": "string", "enum": ["python", "javascript", "rust", "shell"] },
                    "code": { "type": "string", "description": "Code to execute" },
                    "timeout_secs": { "type": "integer", "default": 30 }
                },
                "required": ["language", "code"]
            }),
        }],
        vec![],
        vec![SkillTrigger::Keywords {
            words: vec!["code".into(), "debug".into(), "function".into(), "error".into(), "bug".into()],
            threshold: 0.3,
        }],
        9.0,
    )
}

fn file_operations() -> Skill {
    make_skill(
        "core", "file-ops",
        "File Operations",
        "Read, write, and manage files",
        "You can read and write files on the local filesystem using file \
         operation tools. Always confirm before writing or deleting files. \
         Show diffs for modifications.",
        vec![
            SkillToolBinding {
                tool_name: "file_read".to_string(),
                description: "Read file contents".to_string(),
                parameters_schema: serde_json::json!({
                    "type": "object",
                    "properties": { "path": { "type": "string" } },
                    "required": ["path"]
                }),
            },
            SkillToolBinding {
                tool_name: "file_write".to_string(),
                description: "Write content to a file".to_string(),
                parameters_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "content": { "type": "string" }
                    },
                    "required": ["path", "content"]
                }),
            },
        ],
        vec![],
        vec![SkillTrigger::Keywords {
            words: vec!["file".into(), "read".into(), "write".into(), "save".into()],
            threshold: 0.3,
        }],
        8.0,
    )
}

fn email_compose() -> Skill {
    make_skill(
        "core", "email-compose",
        "Email Compose",
        "Draft professional emails",
        "Help compose clear, professional emails. Ask for the recipient, \
         subject, and key points. Match the requested tone (formal, casual, \
         friendly). Include appropriate salutations and sign-offs.",
        vec![],
        vec![SkillParameter {
            name: "tone".to_string(),
            description: "Default email tone".to_string(),
            param_type: ParameterType::Enum {
                values: vec!["formal".into(), "casual".into(), "friendly".into()],
            },
            required: false,
            default_value: Some(serde_json::json!("professional")),
        }],
        vec![SkillTrigger::Keywords {
            words: vec!["email".into(), "draft".into(), "compose".into(), "write to".into()],
            threshold: 0.4,
        }],
        6.0,
    )
}

fn summarization() -> Skill {
    make_skill(
        "core", "summarize",
        "Summarization",
        "Summarize text, documents, and conversations",
        "Provide concise, accurate summaries. Identify key points, main \
         arguments, and conclusions. Offer bullet-point format for quick \
         scanning and paragraph format for detailed understanding. \
         Preserve important nuances and caveats.",
        vec![],
        vec![],
        vec![SkillTrigger::Keywords {
            words: vec!["summarize".into(), "summary".into(), "tldr".into(), "key points".into()],
            threshold: 0.4,
        }],
        7.0,
    )
}

fn translation() -> Skill {
    make_skill(
        "core", "translate",
        "Translation",
        "Translate text between languages",
        "Translate text accurately between languages. Preserve meaning, \
         tone, and cultural context. Flag idiomatic expressions that don't \
         translate directly. Support 50+ languages.",
        vec![],
        vec![SkillParameter {
            name: "target_language".to_string(),
            description: "Default target language for translation".to_string(),
            param_type: ParameterType::String,
            required: false,
            default_value: Some(serde_json::json!("English")),
        }],
        vec![SkillTrigger::Keywords {
            words: vec!["translate".into(), "translation".into(), "in english".into(), "to spanish".into()],
            threshold: 0.5,
        }],
        6.0,
    )
}

fn task_management() -> Skill {
    make_skill(
        "core", "tasks",
        "Task Management",
        "Create, track, and manage tasks and to-do lists",
        "Help organize tasks and to-do lists. Support creating, updating, \
         and completing tasks. Provide priority suggestions, deadline tracking, \
         and progress summaries. Use the task_store tool for persistence.",
        vec![SkillToolBinding {
            tool_name: "task_store".to_string(),
            description: "Store and retrieve tasks".to_string(),
            parameters_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["create", "list", "complete", "delete"] },
                    "task": { "type": "string" },
                    "priority": { "type": "string", "enum": ["high", "medium", "low"] }
                },
                "required": ["action"]
            }),
        }],
        vec![],
        vec![SkillTrigger::Keywords {
            words: vec!["task".into(), "todo".into(), "remind".into(), "deadline".into()],
            threshold: 0.3,
        }],
        7.0,
    )
}

fn calendar_awareness() -> Skill {
    make_skill(
        "core", "calendar",
        "Calendar Awareness",
        "Time and date reasoning, scheduling help",
        "Help with time and date calculations, scheduling, and timezone \
         conversions. Understand relative time references ('next Tuesday', \
         'in 3 hours'). Suggest optimal meeting times.",
        vec![],
        vec![],
        vec![SkillTrigger::Keywords {
            words: vec!["schedule".into(), "when".into(), "meeting".into(), "calendar".into(), "time".into()],
            threshold: 0.3,
        }],
        5.0,
    )
}

fn memory_recall() -> Skill {
    make_skill(
        "core", "memory",
        "Memory Recall",
        "Remember and recall information from past conversations",
        "You have access to conversation memory. Use the memory_search tool \
         to recall information from previous conversations. This helps maintain \
         context across sessions. Reference relevant past discussions when helpful.",
        vec![SkillToolBinding {
            tool_name: "memory_search".to_string(),
            description: "Search conversation memory".to_string(),
            parameters_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "What to search for in memory" },
                    "top_k": { "type": "integer", "default": 5 }
                },
                "required": ["query"]
            }),
        }],
        vec![],
        vec![SkillTrigger::Always],
        8.0,
    )
}

fn creative_writing() -> Skill {
    make_skill(
        "core", "creative",
        "Creative Writing",
        "Generate creative content — stories, poems, scripts",
        "Help with creative writing tasks. Generate stories, poems, scripts, \
         slogans, and other creative content. Match the requested style, tone, \
         and format. Offer multiple variations when appropriate.",
        vec![],
        vec![],
        vec![SkillTrigger::Keywords {
            words: vec!["write".into(), "story".into(), "poem".into(), "creative".into()],
            threshold: 0.4,
        }],
        5.0,
    )
}

fn image_description() -> Skill {
    make_skill(
        "core", "image-describe",
        "Image Description",
        "Describe and analyze images",
        "When shown images, provide detailed descriptions covering: \
         subject matter, composition, colors, mood, text/labels, \
         and relevant context. Support accessibility use cases \
         with alt-text generation.",
        vec![],
        vec![],
        vec![SkillTrigger::Keywords {
            words: vec!["image".into(), "picture".into(), "photo".into(), "describe".into(), "what is this".into()],
            threshold: 0.3,
        }],
        6.0,
    )
}

fn math_computation() -> Skill {
    make_skill(
        "core", "math",
        "Math & Computation",
        "Mathematical calculations and reasoning",
        "Help with mathematical problems: arithmetic, algebra, calculus, \
         statistics, and more. Show step-by-step solutions. Use the \
         code_execute tool for complex computations that benefit from \
         precise calculation.",
        vec![],
        vec![],
        vec![SkillTrigger::Keywords {
            words: vec!["calculate".into(), "math".into(), "equation".into(), "formula".into(), "solve".into()],
            threshold: 0.3,
        }],
        6.0,
    )
}

fn system_diagnostics() -> Skill {
    make_skill(
        "core", "diagnostics",
        "System Diagnostics",
        "Check system health and configuration",
        "Run system diagnostics when asked. Check provider connectivity, \
         channel status, database health, and resource usage. Report \
         issues clearly with suggested fixes.",
        vec![SkillToolBinding {
            tool_name: "system_check".to_string(),
            description: "Run system health checks".to_string(),
            parameters_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "component": { "type": "string", "enum": ["all", "providers", "channels", "database", "network"] }
                },
                "required": ["component"]
            }),
        }],
        vec![],
        vec![SkillTrigger::Command {
            command: "diagnostics".to_string(),
        }],
        4.0,
    )
}

fn system_info() -> Skill {
    make_skill(
        "core", "sysinfo",
        "System Info",
        "Provide system information and configuration",
        "When asked about the system, provide information about the current \
         ClawDesk configuration: version, active providers, channels, \
         loaded skills, and resource usage.",
        vec![],
        vec![],
        vec![SkillTrigger::Keywords {
            words: vec!["system".into(), "version".into(), "status".into(), "config".into()],
            threshold: 0.4,
        }],
        4.0,
    )
}

fn help_guide() -> Skill {
    make_skill(
        "core", "help",
        "Help Guide",
        "Guide users through ClawDesk features",
        "Help users understand and use ClawDesk effectively. Explain \
         available commands, features, and capabilities. Provide \
         examples and step-by-step instructions.",
        vec![],
        vec![],
        vec![SkillTrigger::Keywords {
            words: vec!["help".into(), "how to".into(), "what can you".into(), "guide".into()],
            threshold: 0.3,
        }],
        5.0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_skills_count() {
        let skills = all_bundled_skills();
        assert_eq!(skills.len(), 12, "expected 12 bundled skills (email/calendar/tasks removed)");
    }

    #[test]
    fn all_skills_have_unique_ids() {
        let skills = all_bundled_skills();
        let mut ids: Vec<String> = skills.iter().map(|s| s.manifest.id.0.clone()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 12, "skill IDs must be unique");
    }

    #[test]
    fn all_skills_have_prompts() {
        for skill in all_bundled_skills() {
            assert!(
                !skill.prompt_fragment.is_empty(),
                "skill {} has empty prompt",
                skill.manifest.id
            );
        }
    }

    #[test]
    fn bundled_registry_load() {
        let registry = load_bundled_skills();
        // 12 core + 6 design + 92 embedded legacy skills (gog removed)
        assert!(
            registry.len() >= 100,
            "expected 100+ skills in registry, got {}",
            registry.len()
        );
    }
}
