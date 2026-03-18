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
        // ── Gap-closing skills (v0.1.8) ──────────────────────
        app_store_submission(),
        hipaa_medical_research(),
        ecommerce_operator(),
        duckdb_crm(),
        habit_health_tracker(),
        package_tracker(),
        voice_journal(),
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

// ─── Gap-Closing Skills (v0.1.8) ─────────────────────────────────────────────
//
// These skills close the 7 documented feature gaps vs OpenClaw:
//   1. App Store submission pipeline
//   2. HIPAA medical research agent
//   3. One-person e-commerce operator
//   4. SochDB-backed CRM & contact management
//   5. Habit & health tracking (wearable data)
//   6. Package tracking / delivery dashboard
//   7. Voice journaling (Whisper transcription)

fn app_store_submission() -> Skill {
    make_skill(
        "core", "appstore-submit",
        "App Store Submission",
        "Build, sign, validate, and submit apps to Apple App Store and Google Play",
        "You can manage the full App Store submission pipeline. Use shell_exec \
         to run xcodebuild, xcrun altool, and fastlane commands. Steps: \
         1) Archive the Xcode project (xcodebuild archive). \
         2) Export the IPA (xcodebuild -exportArchive). \
         3) Validate with xcrun altool --validate-app. \
         4) Upload with xcrun altool --upload-app or fastlane deliver. \
         For Google Play, use bundletool for AAB and the Play Console API. \
         Always verify code signing identity and provisioning profiles before build. \
         Store credentials in the system keychain — never log secrets.",
        vec![
            SkillToolBinding {
                tool_name: "shell_exec".to_string(),
                description: "Execute shell commands for build and submission".to_string(),
                parameters_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "Shell command to execute" },
                        "working_dir": { "type": "string", "description": "Working directory" },
                        "timeout_secs": { "type": "integer", "default": 300 }
                    },
                    "required": ["command"]
                }),
            },
            SkillToolBinding {
                tool_name: "file_read".to_string(),
                description: "Read project files and plists".to_string(),
                parameters_schema: serde_json::json!({
                    "type": "object",
                    "properties": { "path": { "type": "string" } },
                    "required": ["path"]
                }),
            },
        ],
        vec![
            SkillParameter {
                name: "platform".to_string(),
                description: "Target platform".to_string(),
                param_type: ParameterType::Enum {
                    values: vec!["ios".into(), "macos".into(), "android".into()],
                },
                required: false,
                default_value: Some(serde_json::json!("ios")),
            },
        ],
        vec![SkillTrigger::Keywords {
            words: vec![
                "app store".into(), "submit".into(), "testflight".into(),
                "xcodebuild".into(), "fastlane".into(), "ipa".into(),
                "google play".into(), "aab".into(),
            ],
            threshold: 0.3,
        }],
        7.0,
    )
}

fn hipaa_medical_research() -> Skill {
    make_skill(
        "core", "hipaa-medical",
        "HIPAA Medical Research",
        "Medical research with HIPAA-compliant data handling and EHR integration",
        "You are a medical research assistant operating under HIPAA Safe Harbor \
         de-identification requirements (45 CFR §164.514). \
         MANDATORY CONSTRAINTS: \
         - Never store PHI (Protected Health Information) in plaintext. \
         - Strip all 18 HIPAA identifiers before persisting any patient data. \
         - Use memory_store with scope='hipaa' for audit-logged persistence. \
         - All EHR queries via http_fetch must use TLS and bearer-token auth. \
         - Log every data access for audit trail (memory_store action='audit'). \
         - Refuse to transmit PHI over unencrypted channels. \
         Supported EHR systems: FHIR R4 endpoints (Epic, Cerner, Allscripts). \
         Use web_search for PubMed/ClinicalTrials.gov literature. \
         Always cite DOI or PMID for clinical references.",
        vec![
            SkillToolBinding {
                tool_name: "memory_store".to_string(),
                description: "HIPAA-scoped persistent storage with audit logging".to_string(),
                parameters_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "action": { "type": "string", "enum": ["store", "retrieve", "delete", "audit"] },
                        "scope": { "type": "string", "enum": ["hipaa", "research", "audit"] },
                        "key": { "type": "string" },
                        "value": { "type": "string" },
                        "ttl_hours": { "type": "integer", "description": "Auto-expire after N hours" }
                    },
                    "required": ["action", "scope"]
                }),
            },
            SkillToolBinding {
                tool_name: "http_fetch".to_string(),
                description: "Fetch data from FHIR R4 EHR endpoints".to_string(),
                parameters_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "url": { "type": "string", "description": "FHIR endpoint URL" },
                        "method": { "type": "string", "enum": ["GET", "POST"], "default": "GET" },
                        "headers": { "type": "object", "description": "HTTP headers (Authorization, Accept)" },
                        "body": { "type": "string" }
                    },
                    "required": ["url"]
                }),
            },
            SkillToolBinding {
                tool_name: "web_search".to_string(),
                description: "Search PubMed, ClinicalTrials.gov, and medical literature".to_string(),
                parameters_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string" },
                        "num_results": { "type": "integer", "default": 10 }
                    },
                    "required": ["query"]
                }),
            },
        ],
        vec![],
        vec![SkillTrigger::Keywords {
            words: vec![
                "hipaa".into(), "ehr".into(), "fhir".into(), "patient".into(),
                "clinical".into(), "medical research".into(), "phi".into(),
                "de-identify".into(),
            ],
            threshold: 0.3,
        }],
        8.0,
    )
}

fn ecommerce_operator() -> Skill {
    make_skill(
        "core", "ecommerce-ops",
        "E-Commerce Operator",
        "Autonomous e-commerce, invoicing, inventory, and logistics management",
        "You are a one-person-company operations agent. You autonomously manage: \
         - Product catalog: CRUD via http_fetch to Shopify/WooCommerce/custom APIs. \
         - Invoicing: Generate PDF invoices via shell_exec (weasyprint/wkhtmltopdf). \
         - Inventory: Track stock levels in memory_store, alert on low-stock. \
         - Order fulfillment: Monitor orders, create shipping labels via carrier APIs. \
         - Logistics: Track shipments, update customers on delivery status. \
         - Scheduling: Use cron_schedule for recurring tasks (daily sales report, \
           inventory sync, abandoned cart follow-ups). \
         Store all operational data in memory_store with scope='ecommerce'. \
         Support multi-currency pricing. Generate daily P&L summaries.",
        vec![
            SkillToolBinding {
                tool_name: "http_fetch".to_string(),
                description: "API calls to e-commerce platforms and carrier APIs".to_string(),
                parameters_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "url": { "type": "string" },
                        "method": { "type": "string", "enum": ["GET", "POST", "PUT", "PATCH", "DELETE"] },
                        "headers": { "type": "object" },
                        "body": { "type": "string" }
                    },
                    "required": ["url"]
                }),
            },
            SkillToolBinding {
                tool_name: "shell_exec".to_string(),
                description: "Run invoice generation, report scripts, and CLI tools".to_string(),
                parameters_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string" },
                        "timeout_secs": { "type": "integer", "default": 60 }
                    },
                    "required": ["command"]
                }),
            },
            SkillToolBinding {
                tool_name: "memory_store".to_string(),
                description: "Persist inventory, orders, and operational state".to_string(),
                parameters_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "action": { "type": "string", "enum": ["store", "retrieve", "query", "delete"] },
                        "scope": { "type": "string", "default": "ecommerce" },
                        "key": { "type": "string" },
                        "value": { "type": "string" }
                    },
                    "required": ["action"]
                }),
            },
            SkillToolBinding {
                tool_name: "cron_schedule".to_string(),
                description: "Schedule recurring e-commerce tasks".to_string(),
                parameters_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Task name" },
                        "cron": { "type": "string", "description": "Cron expression" },
                        "action": { "type": "string", "description": "Action to perform" }
                    },
                    "required": ["name", "cron", "action"]
                }),
            },
        ],
        vec![
            SkillParameter {
                name: "platform".to_string(),
                description: "E-commerce platform".to_string(),
                param_type: ParameterType::Enum {
                    values: vec!["shopify".into(), "woocommerce".into(), "custom".into()],
                },
                required: false,
                default_value: Some(serde_json::json!("shopify")),
            },
            SkillParameter {
                name: "currency".to_string(),
                description: "Base currency for pricing".to_string(),
                param_type: ParameterType::String,
                required: false,
                default_value: Some(serde_json::json!("USD")),
            },
        ],
        vec![SkillTrigger::Keywords {
            words: vec![
                "ecommerce".into(), "e-commerce".into(), "shopify".into(),
                "inventory".into(), "invoice".into(), "order".into(),
                "shipping".into(), "product".into(), "catalog".into(),
                "woocommerce".into(),
            ],
            threshold: 0.3,
        }],
        8.0,
    )
}

fn duckdb_crm() -> Skill {
    make_skill(
        "core", "sochdb-crm",
        "SochDB CRM",
        "CRM and contact management with SochDB-backed natural-language queries",
        "You are a CRM agent backed by SochDB (the embedded ACID vector database). \
         Manage contacts, companies, deals, and interactions. \
         CAPABILITIES: \
         - Add/update/search contacts with full-text + vector similarity search. \
         - Track deal pipelines: stages, values, expected close dates. \
         - Log interactions (calls, emails, meetings) with timestamps. \
         - Natural-language queries: 'show me all contacts at Acme who haven't \
           been contacted in 30 days' → translated to SochDB filter + temporal decay. \
         - Relationship graph: model connections between contacts and companies. \
         - Use memory_store with scope='crm' for all persistence. \
         All data lives in SochDB — no external database required. \
         Leverage BM25+vector hybrid search for contact/deal lookups. \
         Support CSV/JSON import/export via shell_exec.",
        vec![
            SkillToolBinding {
                tool_name: "contacts_add".to_string(),
                description: "Add or update a contact in the CRM".to_string(),
                parameters_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "email": { "type": "string" },
                        "phone": { "type": "string" },
                        "company": { "type": "string" },
                        "role": { "type": "string" },
                        "tags": { "type": "array", "items": { "type": "string" } },
                        "notes": { "type": "string" }
                    },
                    "required": ["name"]
                }),
            },
            SkillToolBinding {
                tool_name: "contacts_search".to_string(),
                description: "Search contacts using natural language or filters".to_string(),
                parameters_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Natural language or keyword search" },
                        "company": { "type": "string" },
                        "tag": { "type": "string" },
                        "last_contact_days": { "type": "integer", "description": "Filter by days since last contact" },
                        "top_k": { "type": "integer", "default": 20 }
                    },
                    "required": ["query"]
                }),
            },
            SkillToolBinding {
                tool_name: "memory_store".to_string(),
                description: "Persist CRM data (deals, interactions, pipelines) in SochDB".to_string(),
                parameters_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "action": { "type": "string", "enum": ["store", "retrieve", "query", "delete"] },
                        "scope": { "type": "string", "default": "crm" },
                        "key": { "type": "string" },
                        "value": { "type": "string" }
                    },
                    "required": ["action", "scope"]
                }),
            },
            SkillToolBinding {
                tool_name: "shell_exec".to_string(),
                description: "CSV/JSON import/export and report generation".to_string(),
                parameters_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string" },
                        "timeout_secs": { "type": "integer", "default": 30 }
                    },
                    "required": ["command"]
                }),
            },
        ],
        vec![],
        vec![SkillTrigger::Keywords {
            words: vec![
                "crm".into(), "contact".into(), "deal".into(), "pipeline".into(),
                "lead".into(), "prospect".into(), "customer".into(),
                "interaction".into(), "follow up".into(),
            ],
            threshold: 0.3,
        }],
        7.0,
    )
}

fn habit_health_tracker() -> Skill {
    make_skill(
        "core", "habit-health",
        "Habit & Health Tracker",
        "Track habits, fitness data, streaks, and wearable integrations",
        "You are a habit and health tracking agent. \
         CAPABILITIES: \
         - Habit tracking: define habits, log completions, compute streaks. \
         - Fitness data: ingest WHOOP, Fitbit, Apple Health, Garmin data \
           via http_fetch to their REST APIs (OAuth2 bearer tokens). \
         - Sleep analysis: track duration, quality, HRV, resting heart rate. \
         - Streak tracking: current streak, longest streak, streak recovery. \
         - Visualizations: generate charts via canvas_eval (JS in WebView). \
         - Reminders: use cron_schedule for daily habit check-ins. \
         Store all data in memory_store with scope='health'. \
         Present weekly/monthly summaries with trend analysis. \
         Never provide medical diagnoses — always recommend consulting professionals.",
        vec![
            SkillToolBinding {
                tool_name: "memory_store".to_string(),
                description: "Persist habit logs, streaks, and health metrics".to_string(),
                parameters_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "action": { "type": "string", "enum": ["store", "retrieve", "query", "delete"] },
                        "scope": { "type": "string", "default": "health" },
                        "key": { "type": "string" },
                        "value": { "type": "string" }
                    },
                    "required": ["action", "scope"]
                }),
            },
            SkillToolBinding {
                tool_name: "http_fetch".to_string(),
                description: "Fetch wearable data from WHOOP, Fitbit, Garmin APIs".to_string(),
                parameters_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "url": { "type": "string" },
                        "method": { "type": "string", "default": "GET" },
                        "headers": { "type": "object" }
                    },
                    "required": ["url"]
                }),
            },
            SkillToolBinding {
                tool_name: "cron_schedule".to_string(),
                description: "Schedule daily habit reminders and weekly reports".to_string(),
                parameters_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "cron": { "type": "string" },
                        "action": { "type": "string" }
                    },
                    "required": ["name", "cron", "action"]
                }),
            },
            SkillToolBinding {
                tool_name: "canvas_eval".to_string(),
                description: "Render health charts and streak visualizations".to_string(),
                parameters_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "js_code": { "type": "string", "description": "JavaScript to render in WebView" }
                    },
                    "required": ["js_code"]
                }),
            },
        ],
        vec![
            SkillParameter {
                name: "wearable".to_string(),
                description: "Connected wearable platform".to_string(),
                param_type: ParameterType::Enum {
                    values: vec!["whoop".into(), "fitbit".into(), "garmin".into(), "apple_health".into(), "none".into()],
                },
                required: false,
                default_value: Some(serde_json::json!("none")),
            },
        ],
        vec![SkillTrigger::Keywords {
            words: vec![
                "habit".into(), "streak".into(), "fitness".into(), "sleep".into(),
                "workout".into(), "whoop".into(), "health track".into(),
                "heart rate".into(), "hrv".into(),
            ],
            threshold: 0.3,
        }],
        7.0,
    )
}

fn package_tracker() -> Skill {
    make_skill(
        "core", "package-track",
        "Package Tracker",
        "Monitor shipments, track deliveries, and alert on status changes",
        "You are a package tracking and delivery dashboard agent. \
         CAPABILITIES: \
         - Track packages across carriers: USPS, UPS, FedEx, DHL, SF Express. \
         - Auto-detect carrier from tracking number format. \
         - Poll carrier APIs via http_fetch for status updates. \
         - Store tracking state in memory_store with scope='shipping'. \
         - Alert on status changes: shipped, in-transit, out-for-delivery, delivered, exception. \
         - Schedule periodic polling via cron_schedule (every 2 hours). \
         - Dashboard view: render delivery timeline via canvas_eval. \
         - Summary: show all active shipments with ETA and current status. \
         Use web_search as fallback for carriers without API access.",
        vec![
            SkillToolBinding {
                tool_name: "http_fetch".to_string(),
                description: "Fetch tracking data from carrier APIs".to_string(),
                parameters_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "url": { "type": "string" },
                        "method": { "type": "string", "default": "GET" },
                        "headers": { "type": "object" }
                    },
                    "required": ["url"]
                }),
            },
            SkillToolBinding {
                tool_name: "memory_store".to_string(),
                description: "Persist tracking numbers and delivery states".to_string(),
                parameters_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "action": { "type": "string", "enum": ["store", "retrieve", "query", "delete"] },
                        "scope": { "type": "string", "default": "shipping" },
                        "key": { "type": "string" },
                        "value": { "type": "string" }
                    },
                    "required": ["action", "scope"]
                }),
            },
            SkillToolBinding {
                tool_name: "cron_schedule".to_string(),
                description: "Schedule periodic tracking status polls".to_string(),
                parameters_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "cron": { "type": "string" },
                        "action": { "type": "string" }
                    },
                    "required": ["name", "cron", "action"]
                }),
            },
            SkillToolBinding {
                tool_name: "web_search".to_string(),
                description: "Fallback carrier tracking via web search".to_string(),
                parameters_schema: serde_json::json!({
                    "type": "object",
                    "properties": { "query": { "type": "string" } },
                    "required": ["query"]
                }),
            },
            SkillToolBinding {
                tool_name: "canvas_eval".to_string(),
                description: "Render delivery timeline dashboard".to_string(),
                parameters_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "js_code": { "type": "string" }
                    },
                    "required": ["js_code"]
                }),
            },
        ],
        vec![],
        vec![SkillTrigger::Keywords {
            words: vec![
                "package".into(), "tracking".into(), "shipment".into(),
                "delivery".into(), "usps".into(), "ups".into(), "fedex".into(),
                "dhl".into(), "tracking number".into(),
            ],
            threshold: 0.3,
        }],
        6.0,
    )
}

fn voice_journal() -> Skill {
    make_skill(
        "core", "voice-journal",
        "Voice Journal",
        "Record voice, transcribe with Whisper, and format daily journal entries",
        "You are a voice journaling agent. \
         CAPABILITIES: \
         - Record audio via the clawdesk-voice crate (microphone capture). \
         - Transcribe using Whisper (local via whisper.cpp or remote via OpenAI API). \
         - Format transcriptions into structured journal entries with: \
           date, mood tags, key topics, action items, and gratitude notes. \
         - Store journal entries in memory_store with scope='journal'. \
         - Search past entries using natural language (BM25+vector hybrid). \
         - Generate weekly/monthly reflections from journal corpus. \
         - Export to Markdown files via file_write. \
         WORKFLOW: \
         1) User says 'journal' → start recording. \
         2) On stop → send audio to Whisper for transcription. \
         3) Parse transcript → extract structure → persist. \
         4) Confirm entry with formatted preview.",
        vec![
            SkillToolBinding {
                tool_name: "voice_record".to_string(),
                description: "Start/stop voice recording via microphone".to_string(),
                parameters_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "action": { "type": "string", "enum": ["start", "stop", "status"] },
                        "max_duration_secs": { "type": "integer", "default": 300 }
                    },
                    "required": ["action"]
                }),
            },
            SkillToolBinding {
                tool_name: "whisper_transcribe".to_string(),
                description: "Transcribe audio using Whisper (local or API)".to_string(),
                parameters_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "audio_path": { "type": "string", "description": "Path to audio file" },
                        "language": { "type": "string", "default": "en" },
                        "model": { "type": "string", "enum": ["tiny", "base", "small", "medium", "large"], "default": "base" }
                    },
                    "required": ["audio_path"]
                }),
            },
            SkillToolBinding {
                tool_name: "memory_store".to_string(),
                description: "Persist and search journal entries".to_string(),
                parameters_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "action": { "type": "string", "enum": ["store", "retrieve", "query", "delete"] },
                        "scope": { "type": "string", "default": "journal" },
                        "key": { "type": "string" },
                        "value": { "type": "string" }
                    },
                    "required": ["action", "scope"]
                }),
            },
            SkillToolBinding {
                tool_name: "file_write".to_string(),
                description: "Export journal entries to Markdown".to_string(),
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
        vec![
            SkillParameter {
                name: "whisper_backend".to_string(),
                description: "Whisper transcription backend".to_string(),
                param_type: ParameterType::Enum {
                    values: vec!["local".into(), "openai-api".into()],
                },
                required: false,
                default_value: Some(serde_json::json!("local")),
            },
        ],
        vec![SkillTrigger::Keywords {
            words: vec![
                "journal".into(), "voice journal".into(), "record".into(),
                "transcribe".into(), "diary".into(), "daily entry".into(),
                "whisper".into(),
            ],
            threshold: 0.3,
        }],
        7.0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_skills_count() {
        let skills = all_bundled_skills();
        assert_eq!(skills.len(), 19, "expected 19 bundled skills (12 original + 7 gap-closing)");
    }

    #[test]
    fn all_skills_have_unique_ids() {
        let skills = all_bundled_skills();
        let mut ids: Vec<String> = skills.iter().map(|s| s.manifest.id.0.clone()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 19, "skill IDs must be unique");
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
        // 19 core + 6 design + 92 embedded legacy skills (gog removed)
        assert!(
            registry.len() >= 100,
            "expected 100+ skills in registry, got {}",
            registry.len()
        );
    }

    #[test]
    fn gap_closing_skills_present() {
        let skills = all_bundled_skills();
        let ids: Vec<String> = skills.iter().map(|s| s.manifest.id.0.clone()).collect();
        assert!(ids.contains(&"core/appstore-submit".to_string()));
        assert!(ids.contains(&"core/hipaa-medical".to_string()));
        assert!(ids.contains(&"core/ecommerce-ops".to_string()));
        assert!(ids.contains(&"core/sochdb-crm".to_string()));
        assert!(ids.contains(&"core/habit-health".to_string()));
        assert!(ids.contains(&"core/package-track".to_string()));
        assert!(ids.contains(&"core/voice-journal".to_string()));
    }

    #[test]
    fn hipaa_skill_has_audit_tool() {
        let skill = hipaa_medical_research();
        assert!(
            skill.provided_tools.iter().any(|t| t.tool_name == "memory_store"),
            "HIPAA skill must have memory_store for audit logging"
        );
    }

    #[test]
    fn crm_uses_sochdb_not_duckdb() {
        let skill = duckdb_crm();
        assert!(
            skill.manifest.display_name.contains("SochDB"),
            "CRM skill must use SochDB, not DuckDB"
        );
        assert!(
            skill.prompt_fragment.contains("SochDB"),
            "CRM prompt must reference SochDB"
        );
    }
}
