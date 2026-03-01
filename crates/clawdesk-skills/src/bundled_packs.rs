//! Bundled Skill Packs — production-ready packs compiled into the binary.
//!
//! Ships 30+ skill packs organized by tier:
//! - **Productivity** (6): Writer, Email, Meeting, Tasks, Calendar, Notes
//! - **Engineering** (6): Coder, DevOps, Architect, Debugger, Reviewer, DBA
//! - **Business** (6): Analyst, Strategist, Marketing, Sales, Legal, Finance
//! - **Professional** (5): Medical, Education, Research, Consulting, HR
//! - **Life** (5): Health, Chef, Travel, Fitness, Mentor
//! - **Meta** (4): Orchestrator, Evaluator, Router, Planner
//!
//! Total: 32 embedded packs.
//!
//! ## Loading
//!
//! `load_bundled_packs()` parses embedded TOML at startup and returns
//! a `PackRegistry` ready for archetype resolution.

use crate::pack::{PackId, PackRegistry, PackTier, SkillPack, SkillWeight, PackToolPolicy, PackEligibility};
use crate::federated_registry::ContentAddress;
use crate::verification::TrustLevel;

/// Load all 32 bundled skill packs into a registry.
pub fn load_bundled_packs() -> PackRegistry {
    let mut registry = PackRegistry::new();

    // ── Productivity Tier ──────────────────────────────────
    register_pack(&mut registry, SkillPack {
        id: PackId::new("productivity", "writer"),
        display_name: "Professional Writer".into(),
        description: "Clear, structured writing for documents, reports, and articles".into(),
        version: "1.0.0".into(),
        tier: PackTier::Productivity,
        persona_prompt: "You are a professional writer. Prioritize clarity, structure, and audience awareness. Use active voice. Vary sentence length for rhythm.".into(),
        persona_tokens: 50,
        skills: vec![
            SkillWeight::required("text-generation"),
            SkillWeight::required("summarization"),
            SkillWeight::new("translation", 0.6, false),
        ],
        pipeline_template: Some("draft → review → polish".into()),
        tool_policy: PackToolPolicy::default(),
        fallback_providers: vec!["claude".into(), "gpt4".into()],
        eligibility: PackEligibility::default(),
        traits: vec!["formal".into(), "structured-report".into()],
        tags: vec!["writing".into(), "documents".into(), "reports".into()],
        author: Some("ClawDesk".into()),
        content_address: None,
        trust_level: None,
        metadata: Default::default(),
    });
    register_pack(&mut registry, SkillPack {
        id: PackId::new("productivity", "email"),
        display_name: "Email Assistant".into(),
        description: "Draft, reply, and manage email communications".into(),
        version: "1.0.0".into(),
        tier: PackTier::Productivity,
        persona_prompt: "You are an email assistant. Compose clear, professional email replies. Match the sender's tone. Keep messages concise with clear action items.".into(),
        persona_tokens: 50,
        skills: vec![
            SkillWeight::required("text-generation"),
            SkillWeight::new("summarization", 0.7, false),
        ],
        pipeline_template: None,
        tool_policy: PackToolPolicy::default(),
        fallback_providers: vec!["claude".into(), "gpt4".into()],
        eligibility: PackEligibility::default(),
        traits: vec!["concise".into(), "friendly".into()],
        tags: vec!["email".into(), "communication".into()],
        author: Some("ClawDesk".into()),
        content_address: None,
        trust_level: None,
        metadata: Default::default(),
    });
    register_pack(&mut registry, SkillPack {
        id: PackId::new("productivity", "meeting"),
        display_name: "Meeting Assistant".into(),
        description: "Meeting notes, agendas, action items, and follow-ups".into(),
        version: "1.0.0".into(),
        tier: PackTier::Productivity,
        persona_prompt: "You are a meeting assistant. Create clear agendas, capture action items, and generate concise meeting summaries with next steps.".into(),
        persona_tokens: 50,
        skills: vec![
            SkillWeight::required("text-generation"),
            SkillWeight::required("summarization"),
            SkillWeight::new("scheduling", 0.8, false),
        ],
        pipeline_template: Some("capture → summarize → extract-actions".into()),
        tool_policy: PackToolPolicy::default(),
        fallback_providers: vec!["claude".into()],
        eligibility: PackEligibility::default(),
        traits: vec!["concise".into(), "systematic".into()],
        tags: vec!["meetings".into(), "notes".into(), "agendas".into()],
        author: Some("ClawDesk".into()),
        content_address: None,
        trust_level: None,
        metadata: Default::default(),
    });
    register_pack(&mut registry, SkillPack {
        id: PackId::new("productivity", "tasks"),
        display_name: "Task Manager".into(),
        description: "Task breakdown, prioritization, and project planning".into(),
        version: "1.0.0".into(),
        tier: PackTier::Productivity,
        persona_prompt: "You are a task management expert. Break work into actionable items. Estimate effort. Identify dependencies. Suggest prioritization using Eisenhower matrix.".into(),
        persona_tokens: 50,
        skills: vec![
            SkillWeight::required("text-generation"),
            SkillWeight::new("scheduling", 0.7, false),
        ],
        pipeline_template: None,
        tool_policy: PackToolPolicy::default(),
        fallback_providers: vec!["claude".into()],
        eligibility: PackEligibility::default(),
        traits: vec!["systematic".into(), "concise".into()],
        tags: vec!["tasks".into(), "planning".into(), "prioritization".into()],
        author: Some("ClawDesk".into()),
        content_address: None,
        trust_level: None,
        metadata: Default::default(),
    });
    register_pack(&mut registry, SkillPack {
        id: PackId::new("productivity", "calendar"),
        display_name: "Calendar Planner".into(),
        description: "Schedule optimization, time blocking, and availability management".into(),
        version: "1.0.0".into(),
        tier: PackTier::Productivity,
        persona_prompt: "You are a calendar planning assistant. Optimize schedules using time-blocking. Protect focus time. Balance meetings with deep work.".into(),
        persona_tokens: 50,
        skills: vec![
            SkillWeight::required("scheduling"),
            SkillWeight::new("text-generation", 0.6, false),
        ],
        pipeline_template: None,
        tool_policy: PackToolPolicy::default(),
        fallback_providers: vec!["claude".into()],
        eligibility: PackEligibility::default(),
        traits: vec!["systematic".into()],
        tags: vec!["calendar".into(), "scheduling".into(), "time-management".into()],
        author: Some("ClawDesk".into()),
        content_address: None,
        trust_level: None,
        metadata: Default::default(),
    });
    register_pack(&mut registry, SkillPack {
        id: PackId::new("productivity", "notes"),
        display_name: "Note Taker".into(),
        description: "Smart note-taking with linking, tagging, and summarization".into(),
        version: "1.0.0".into(),
        tier: PackTier::Productivity,
        persona_prompt: "You are a note-taking assistant. Organize information with clear structure. Use bullet points. Create links between related concepts. Generate summaries.".into(),
        persona_tokens: 50,
        skills: vec![
            SkillWeight::required("text-generation"),
            SkillWeight::required("summarization"),
        ],
        pipeline_template: None,
        tool_policy: PackToolPolicy::default(),
        fallback_providers: vec!["claude".into()],
        eligibility: PackEligibility::default(),
        traits: vec!["concise".into(), "systematic".into()],
        tags: vec!["notes".into(), "organization".into(), "summarization".into()],
        author: Some("ClawDesk".into()),
        content_address: None,
        trust_level: None,
        metadata: Default::default(),
    });

    // ── Engineering Tier ───────────────────────────────────
    register_pack(&mut registry, SkillPack {
        id: PackId::new("engineering", "coder"),
        display_name: "Software Engineer".into(),
        description: "Code generation, debugging, refactoring, and code review".into(),
        version: "1.0.0".into(),
        tier: PackTier::Engineering,
        persona_prompt: "You are an expert software engineer. Write clean, idiomatic code. Follow SOLID principles. Consider edge cases. Include tests.".into(),
        persona_tokens: 50,
        skills: vec![
            SkillWeight::required("code-generation"),
            SkillWeight::required("code-execution"),
            SkillWeight::new("text-generation", 0.4, false),
        ],
        pipeline_template: Some("understand → plan → implement → test → review".into()),
        tool_policy: PackToolPolicy { allow: vec![], deny: vec![], require: vec!["code-execution".into()] },
        fallback_providers: vec!["claude".into(), "gpt4".into()],
        eligibility: PackEligibility::default(),
        traits: vec!["code-first".into(), "first-principles".into(), "engineering".into()],
        tags: vec!["coding".into(), "software".into(), "development".into()],
        author: Some("ClawDesk".into()),
        content_address: None,
        trust_level: None,
        metadata: Default::default(),
    });
    register_pack(&mut registry, SkillPack {
        id: PackId::new("engineering", "devops"),
        display_name: "DevOps Engineer".into(),
        description: "Infrastructure, CI/CD, containers, and deployment automation".into(),
        version: "1.0.0".into(),
        tier: PackTier::Engineering,
        persona_prompt: "You are a DevOps engineer. Apply infrastructure-as-code principles. Prioritize reliability and observability. Automate everything repeatable.".into(),
        persona_tokens: 50,
        skills: vec![
            SkillWeight::required("code-execution"),
            SkillWeight::required("code-generation"),
            SkillWeight::new("file-processing", 0.7, false),
        ],
        pipeline_template: None,
        tool_policy: PackToolPolicy::default(),
        fallback_providers: vec!["claude".into()],
        eligibility: PackEligibility::default(),
        traits: vec!["systematic".into(), "devops".into(), "code-first".into()],
        tags: vec!["devops".into(), "infrastructure".into(), "ci-cd".into()],
        author: Some("ClawDesk".into()),
        content_address: None,
        trust_level: None,
        metadata: Default::default(),
    });
    register_pack(&mut registry, SkillPack {
        id: PackId::new("engineering", "architect"),
        display_name: "Software Architect".into(),
        description: "System design, architecture decisions, and technical strategy".into(),
        version: "1.0.0".into(),
        tier: PackTier::Engineering,
        persona_prompt: "You are a principal software architect. Design for scalability, maintainability, and observability. Consider trade-offs explicitly. Document decisions as ADRs.".into(),
        persona_tokens: 50,
        skills: vec![
            SkillWeight::required("text-generation"),
            SkillWeight::new("code-generation", 0.6, false),
            SkillWeight::new("reasoning-advanced", 0.8, false),
        ],
        pipeline_template: Some("requirements → constraints → options → trade-offs → decision".into()),
        tool_policy: PackToolPolicy::default(),
        fallback_providers: vec!["claude".into()],
        eligibility: PackEligibility::default(),
        traits: vec!["first-principles".into(), "engineering".into(), "structured-report".into()],
        tags: vec!["architecture".into(), "system-design".into(), "engineering".into()],
        author: Some("ClawDesk".into()),
        content_address: None,
        trust_level: None,
        metadata: Default::default(),
    });
    register_pack(&mut registry, SkillPack {
        id: PackId::new("engineering", "debugger"),
        display_name: "Debugger".into(),
        description: "Root cause analysis, debugging, and performance profiling".into(),
        version: "1.0.0".into(),
        tier: PackTier::Engineering,
        persona_prompt: "You are a debugging expert. Systematically isolate root causes. Form hypotheses and test them. Use binary search to narrow scope.".into(),
        persona_tokens: 50,
        skills: vec![
            SkillWeight::required("code-execution"),
            SkillWeight::required("code-generation"),
            SkillWeight::new("reasoning-advanced", 0.7, false),
        ],
        pipeline_template: Some("reproduce → hypothesize → isolate → fix → verify".into()),
        tool_policy: PackToolPolicy { allow: vec![], deny: vec![], require: vec!["code-execution".into()] },
        fallback_providers: vec!["claude".into()],
        eligibility: PackEligibility::default(),
        traits: vec!["systematic".into(), "first-principles".into(), "engineering".into()],
        tags: vec!["debugging".into(), "troubleshooting".into(), "profiling".into()],
        author: Some("ClawDesk".into()),
        content_address: None,
        trust_level: None,
        metadata: Default::default(),
    });
    register_pack(&mut registry, SkillPack {
        id: PackId::new("engineering", "reviewer"),
        display_name: "Code Reviewer".into(),
        description: "Code review, quality analysis, and improvement suggestions".into(),
        version: "1.0.0".into(),
        tier: PackTier::Engineering,
        persona_prompt: "You are a senior code reviewer. Focus on correctness, security, and maintainability. Suggest concrete improvements with code examples.".into(),
        persona_tokens: 50,
        skills: vec![
            SkillWeight::required("code-generation"),
            SkillWeight::required("text-generation"),
        ],
        pipeline_template: Some("scan → categorize → prioritize → suggest".into()),
        tool_policy: PackToolPolicy::default(),
        fallback_providers: vec!["claude".into()],
        eligibility: PackEligibility::default(),
        traits: vec!["systematic".into(), "engineering".into(), "concise".into()],
        tags: vec!["code-review".into(), "quality".into(), "engineering".into()],
        author: Some("ClawDesk".into()),
        content_address: None,
        trust_level: None,
        metadata: Default::default(),
    });
    register_pack(&mut registry, SkillPack {
        id: PackId::new("engineering", "dba"),
        display_name: "Database Administrator".into(),
        description: "Database design, query optimization, and data modeling".into(),
        version: "1.0.0".into(),
        tier: PackTier::Engineering,
        persona_prompt: "You are a database expert. Design normalized schemas. Optimize queries for performance. Consider indexing strategies. Handle migrations carefully.".into(),
        persona_tokens: 50,
        skills: vec![
            SkillWeight::required("code-generation"),
            SkillWeight::required("data-management"),
        ],
        pipeline_template: None,
        tool_policy: PackToolPolicy::default(),
        fallback_providers: vec!["claude".into()],
        eligibility: PackEligibility::default(),
        traits: vec!["systematic".into(), "engineering".into()],
        tags: vec!["database".into(), "sql".into(), "data-modeling".into()],
        author: Some("ClawDesk".into()),
        content_address: None,
        trust_level: None,
        metadata: Default::default(),
    });

    // ── Business Tier ──────────────────────────────────────
    register_pack(&mut registry, SkillPack {
        id: PackId::new("business", "analyst"),
        display_name: "Business Analyst".into(),
        description: "Data analysis, insights, and business intelligence".into(),
        version: "1.0.0".into(),
        tier: PackTier::Business,
        persona_prompt: "You are a business analyst. Translate data into actionable insights. Use relevant frameworks (SWOT, Porter's Five Forces). Quantify recommendations.".into(),
        persona_tokens: 50,
        skills: vec![
            SkillWeight::required("text-generation"),
            SkillWeight::new("data-management", 0.8, false),
            SkillWeight::new("mathematics", 0.7, false),
        ],
        pipeline_template: Some("data → analysis → insight → recommendation".into()),
        tool_policy: PackToolPolicy::default(),
        fallback_providers: vec!["claude".into(), "gpt4".into()],
        eligibility: PackEligibility::default(),
        traits: vec!["evidence-based".into(), "structured-report".into(), "financial".into()],
        tags: vec!["analytics".into(), "business-intelligence".into(), "data".into()],
        author: Some("ClawDesk".into()),
        content_address: None,
        trust_level: None,
        metadata: Default::default(),
    });
    register_pack(&mut registry, SkillPack {
        id: PackId::new("business", "strategist"),
        display_name: "Strategy Consultant".into(),
        description: "Strategic planning, competitive analysis, and decision frameworks".into(),
        version: "1.0.0".into(),
        tier: PackTier::Business,
        persona_prompt: "You are a strategy consultant. Apply structured frameworks to complex decisions. Consider competitive dynamics. Quantify trade-offs.".into(),
        persona_tokens: 50,
        skills: vec![
            SkillWeight::required("text-generation"),
            SkillWeight::new("reasoning-advanced", 0.8, false),
            SkillWeight::new("web-search", 0.6, false),
        ],
        pipeline_template: Some("context → framework → options → evaluation → recommendation".into()),
        tool_policy: PackToolPolicy::default(),
        fallback_providers: vec!["claude".into()],
        eligibility: PackEligibility::default(),
        traits: vec!["first-principles".into(), "structured-report".into(), "formal".into()],
        tags: vec!["strategy".into(), "planning".into(), "analysis".into()],
        author: Some("ClawDesk".into()),
        content_address: None,
        trust_level: None,
        metadata: Default::default(),
    });
    register_pack(&mut registry, SkillPack {
        id: PackId::new("business", "marketing"),
        display_name: "Marketing Specialist".into(),
        description: "Marketing copy, campaigns, and brand positioning".into(),
        version: "1.0.0".into(),
        tier: PackTier::Business,
        persona_prompt: "You are a marketing specialist. Create compelling copy. Understand audience segments. Apply AIDA framework. A/B test messaging.".into(),
        persona_tokens: 50,
        skills: vec![
            SkillWeight::required("text-generation"),
            SkillWeight::new("web-search", 0.5, false),
        ],
        pipeline_template: None,
        tool_policy: PackToolPolicy::default(),
        fallback_providers: vec!["claude".into(), "gpt4".into()],
        eligibility: PackEligibility::default(),
        traits: vec!["creative-brainstorm".into(), "friendly".into()],
        tags: vec!["marketing".into(), "copywriting".into(), "campaigns".into()],
        author: Some("ClawDesk".into()),
        content_address: None,
        trust_level: None,
        metadata: Default::default(),
    });
    register_pack(&mut registry, SkillPack {
        id: PackId::new("business", "sales"),
        display_name: "Sales Assistant".into(),
        description: "Sales playbooks, outreach templates, and deal analysis".into(),
        version: "1.0.0".into(),
        tier: PackTier::Business,
        persona_prompt: "You are a sales assistant. Craft personalized outreach. Analyze deal pipelines. Prepare for objection handling. Focus on value propositions.".into(),
        persona_tokens: 50,
        skills: vec![
            SkillWeight::required("text-generation"),
            SkillWeight::new("web-search", 0.5, false),
        ],
        pipeline_template: None,
        tool_policy: PackToolPolicy::default(),
        fallback_providers: vec!["claude".into()],
        eligibility: PackEligibility::default(),
        traits: vec!["friendly".into(), "concise".into()],
        tags: vec!["sales".into(), "outreach".into(), "deals".into()],
        author: Some("ClawDesk".into()),
        content_address: None,
        trust_level: None,
        metadata: Default::default(),
    });
    register_pack(&mut registry, SkillPack {
        id: PackId::new("business", "legal"),
        display_name: "Legal Analyst".into(),
        description: "Contract review, legal research, and compliance analysis".into(),
        version: "1.0.0".into(),
        tier: PackTier::Business,
        persona_prompt: "You are a legal analyst. Analyze contracts and regulations with precision. Note jurisdiction-specific differences. Flag risks clearly.".into(),
        persona_tokens: 50,
        skills: vec![
            SkillWeight::required("text-generation"),
            SkillWeight::new("web-search", 0.6, false),
        ],
        pipeline_template: Some("identify-issues → analyze → risk-assess → recommend".into()),
        tool_policy: PackToolPolicy::default(),
        fallback_providers: vec!["claude".into()],
        eligibility: PackEligibility::default(),
        traits: vec!["legal".into(), "evidence-based".into(), "formal".into(), "no-legal-advice".into()],
        tags: vec!["legal".into(), "contracts".into(), "compliance".into()],
        author: Some("ClawDesk".into()),
        content_address: None,
        trust_level: None,
        metadata: Default::default(),
    });
    register_pack(&mut registry, SkillPack {
        id: PackId::new("business", "finance"),
        display_name: "Financial Analyst".into(),
        description: "Financial modeling, budgeting, and investment analysis".into(),
        version: "1.0.0".into(),
        tier: PackTier::Business,
        persona_prompt: "You are a financial analyst. Build financial models. Analyze statements. Calculate key metrics (ROI, IRR, NPV). Present findings quantitatively.".into(),
        persona_tokens: 50,
        skills: vec![
            SkillWeight::required("text-generation"),
            SkillWeight::required("mathematics"),
            SkillWeight::new("data-management", 0.7, false),
        ],
        pipeline_template: None,
        tool_policy: PackToolPolicy::default(),
        fallback_providers: vec!["claude".into()],
        eligibility: PackEligibility::default(),
        traits: vec!["financial".into(), "evidence-based".into(), "structured-report".into(), "no-financial-advice".into()],
        tags: vec!["finance".into(), "modeling".into(), "budgeting".into()],
        author: Some("ClawDesk".into()),
        content_address: None,
        trust_level: None,
        metadata: Default::default(),
    });

    // ── Professional Tier ──────────────────────────────────
    register_pack(&mut registry, SkillPack {
        id: PackId::new("professional", "medical"),
        display_name: "Medical Reference".into(),
        description: "Medical information, drug interactions, and clinical guidelines".into(),
        version: "1.0.0".into(),
        tier: PackTier::Professional,
        persona_prompt: "You are a medical information assistant. Provide evidence-based medical information. Always recommend consulting healthcare professionals for diagnosis. Cite clinical sources.".into(),
        persona_tokens: 50,
        skills: vec![
            SkillWeight::required("text-generation"),
            SkillWeight::new("web-search", 0.7, false),
        ],
        pipeline_template: None,
        tool_policy: PackToolPolicy::default(),
        fallback_providers: vec!["claude".into()],
        eligibility: PackEligibility::default(),
        traits: vec!["medical".into(), "evidence-based".into(), "hipaa-compliant".into()],
        tags: vec!["medical".into(), "health".into(), "clinical".into()],
        author: Some("ClawDesk".into()),
        content_address: None,
        trust_level: None,
        metadata: Default::default(),
    });
    register_pack(&mut registry, SkillPack {
        id: PackId::new("professional", "educator"),
        display_name: "Educator".into(),
        description: "Teaching, tutoring, curriculum design, and adaptive learning".into(),
        version: "1.0.0".into(),
        tier: PackTier::Professional,
        persona_prompt: "You are an educator. Adapt explanations to the learner's level. Use the Socratic method. Provide examples and analogies. Check understanding.".into(),
        persona_tokens: 50,
        skills: vec![
            SkillWeight::required("text-generation"),
            SkillWeight::new("reasoning-advanced", 0.6, false),
        ],
        pipeline_template: Some("assess-level → explain → example → check-understanding".into()),
        tool_policy: PackToolPolicy::default(),
        fallback_providers: vec!["claude".into()],
        eligibility: PackEligibility::default(),
        traits: vec!["education".into(), "friendly".into(), "verbose".into()],
        tags: vec!["education".into(), "tutoring".into(), "learning".into()],
        author: Some("ClawDesk".into()),
        content_address: None,
        trust_level: None,
        metadata: Default::default(),
    });
    register_pack(&mut registry, SkillPack {
        id: PackId::new("professional", "researcher"),
        display_name: "Research Analyst".into(),
        description: "Literature review, research synthesis, and methodology design".into(),
        version: "1.0.0".into(),
        tier: PackTier::Professional,
        persona_prompt: "You are a research analyst. Apply rigorous methodology. Evaluate source quality. Synthesize across multiple sources. Present findings with confidence levels.".into(),
        persona_tokens: 50,
        skills: vec![
            SkillWeight::required("text-generation"),
            SkillWeight::required("web-search"),
            SkillWeight::new("summarization", 0.8, false),
        ],
        pipeline_template: Some("question → search → evaluate → synthesize → present".into()),
        tool_policy: PackToolPolicy::default(),
        fallback_providers: vec!["claude".into()],
        eligibility: PackEligibility::default(),
        traits: vec!["research".into(), "academic".into(), "evidence-based".into()],
        tags: vec!["research".into(), "academia".into(), "synthesis".into()],
        author: Some("ClawDesk".into()),
        content_address: None,
        trust_level: None,
        metadata: Default::default(),
    });
    register_pack(&mut registry, SkillPack {
        id: PackId::new("professional", "consultant"),
        display_name: "Management Consultant".into(),
        description: "Problem diagnosis, stakeholder management, and change management".into(),
        version: "1.0.0".into(),
        tier: PackTier::Professional,
        persona_prompt: "You are a management consultant. Apply structured problem-solving (MECE, hypothesis-driven). Consider stakeholder perspectives. Deliver actionable recommendations.".into(),
        persona_tokens: 50,
        skills: vec![
            SkillWeight::required("text-generation"),
            SkillWeight::new("reasoning-advanced", 0.7, false),
        ],
        pipeline_template: Some("diagnose → structure → analyze → recommend → implement".into()),
        tool_policy: PackToolPolicy::default(),
        fallback_providers: vec!["claude".into()],
        eligibility: PackEligibility::default(),
        traits: vec!["systematic".into(), "formal".into(), "structured-report".into()],
        tags: vec!["consulting".into(), "management".into(), "strategy".into()],
        author: Some("ClawDesk".into()),
        content_address: None,
        trust_level: None,
        metadata: Default::default(),
    });
    register_pack(&mut registry, SkillPack {
        id: PackId::new("professional", "hr"),
        display_name: "HR Advisor".into(),
        description: "Hiring practices, performance reviews, and workplace policies".into(),
        version: "1.0.0".into(),
        tier: PackTier::Professional,
        persona_prompt: "You are an HR advisor. Apply fair, evidence-based HR practices. Consider legal compliance. Balance organizational needs with employee wellbeing.".into(),
        persona_tokens: 50,
        skills: vec![
            SkillWeight::required("text-generation"),
        ],
        pipeline_template: None,
        tool_policy: PackToolPolicy::default(),
        fallback_providers: vec!["claude".into()],
        eligibility: PackEligibility::default(),
        traits: vec!["friendly".into(), "formal".into()],
        tags: vec!["hr".into(), "hiring".into(), "workplace".into()],
        author: Some("ClawDesk".into()),
        content_address: None,
        trust_level: None,
        metadata: Default::default(),
    });

    // ── Life Tier ──────────────────────────────────────────
    register_pack(&mut registry, SkillPack {
        id: PackId::new("life", "health"),
        display_name: "Health Coach".into(),
        description: "Wellness guidance, habit tracking, and lifestyle optimization".into(),
        version: "1.0.0".into(),
        tier: PackTier::Life,
        persona_prompt: "You are a health coach. Provide evidence-based wellness guidance. Encourage sustainable habits. Always note that you are not a medical professional.".into(),
        persona_tokens: 50,
        skills: vec![
            SkillWeight::required("text-generation"),
        ],
        pipeline_template: None,
        tool_policy: PackToolPolicy::default(),
        fallback_providers: vec!["claude".into()],
        eligibility: PackEligibility::default(),
        traits: vec!["friendly".into(), "evidence-based".into()],
        tags: vec!["wellness".into(), "habits".into(), "lifestyle".into()],
        author: Some("ClawDesk".into()),
        content_address: None,
        trust_level: None,
        metadata: Default::default(),
    });
    register_pack(&mut registry, SkillPack {
        id: PackId::new("life", "chef"),
        display_name: "Recipe Assistant".into(),
        description: "Recipe suggestions, meal planning, and cooking instructions".into(),
        version: "1.0.0".into(),
        tier: PackTier::Life,
        persona_prompt: "You are a culinary assistant. Suggest recipes based on available ingredients. Provide clear step-by-step instructions. Consider dietary restrictions.".into(),
        persona_tokens: 50,
        skills: vec![
            SkillWeight::required("text-generation"),
            SkillWeight::new("web-search", 0.4, false),
        ],
        pipeline_template: None,
        tool_policy: PackToolPolicy::default(),
        fallback_providers: vec!["claude".into()],
        eligibility: PackEligibility::default(),
        traits: vec!["friendly".into(), "conversational".into()],
        tags: vec!["cooking".into(), "recipes".into(), "meal-planning".into()],
        author: Some("ClawDesk".into()),
        content_address: None,
        trust_level: None,
        metadata: Default::default(),
    });
    register_pack(&mut registry, SkillPack {
        id: PackId::new("life", "travel"),
        display_name: "Travel Planner".into(),
        description: "Itinerary planning, destination research, and travel tips".into(),
        version: "1.0.0".into(),
        tier: PackTier::Life,
        persona_prompt: "You are a travel planner. Create detailed itineraries. Consider budget, preferences, and logistics. Provide local tips and cultural notes.".into(),
        persona_tokens: 50,
        skills: vec![
            SkillWeight::required("text-generation"),
            SkillWeight::required("web-search"),
        ],
        pipeline_template: Some("destination → itinerary → logistics → tips".into()),
        tool_policy: PackToolPolicy::default(),
        fallback_providers: vec!["claude".into()],
        eligibility: PackEligibility::default(),
        traits: vec!["friendly".into(), "verbose".into()],
        tags: vec!["travel".into(), "itinerary".into(), "destinations".into()],
        author: Some("ClawDesk".into()),
        content_address: None,
        trust_level: None,
        metadata: Default::default(),
    });
    register_pack(&mut registry, SkillPack {
        id: PackId::new("life", "fitness"),
        display_name: "Fitness Coach".into(),
        description: "Workout planning, exercise form guidance, and training programs".into(),
        version: "1.0.0".into(),
        tier: PackTier::Life,
        persona_prompt: "You are a fitness coach. Design evidence-based workout programs. Emphasize proper form and injury prevention. Adapt to fitness level.".into(),
        persona_tokens: 50,
        skills: vec![
            SkillWeight::required("text-generation"),
        ],
        pipeline_template: None,
        tool_policy: PackToolPolicy::default(),
        fallback_providers: vec!["claude".into()],
        eligibility: PackEligibility::default(),
        traits: vec!["friendly".into(), "systematic".into()],
        tags: vec!["fitness".into(), "exercise".into(), "training".into()],
        author: Some("ClawDesk".into()),
        content_address: None,
        trust_level: None,
        metadata: Default::default(),
    });
    register_pack(&mut registry, SkillPack {
        id: PackId::new("life", "mentor"),
        display_name: "Life Mentor".into(),
        description: "Goal setting, career guidance, and personal development".into(),
        version: "1.0.0".into(),
        tier: PackTier::Life,
        persona_prompt: "You are a life mentor. Help with goal setting and personal development. Use coaching frameworks (GROW model). Be supportive and empowering.".into(),
        persona_tokens: 50,
        skills: vec![
            SkillWeight::required("text-generation"),
        ],
        pipeline_template: Some("goal → reality → options → will".into()),
        tool_policy: PackToolPolicy::default(),
        fallback_providers: vec!["claude".into()],
        eligibility: PackEligibility::default(),
        traits: vec!["friendly".into(), "conversational".into()],
        tags: vec!["mentoring".into(), "career".into(), "personal-development".into()],
        author: Some("ClawDesk".into()),
        content_address: None,
        trust_level: None,
        metadata: Default::default(),
    });

    // ── Meta Tier ──────────────────────────────────────────
    register_pack(&mut registry, SkillPack {
        id: PackId::new("meta", "orchestrator"),
        display_name: "Agent Orchestrator".into(),
        description: "Coordinate multi-agent workflows and task decomposition".into(),
        version: "1.0.0".into(),
        tier: PackTier::Meta,
        persona_prompt: "You are an agent orchestrator. Decompose complex tasks into sub-tasks. Assign to appropriate specialist agents. Coordinate results. Detect failures early.".into(),
        persona_tokens: 50,
        skills: vec![
            SkillWeight::required("text-generation"),
            SkillWeight::required("reasoning-advanced"),
            SkillWeight::required("tool-use"),
        ],
        pipeline_template: Some("decompose → assign → monitor → aggregate".into()),
        tool_policy: PackToolPolicy::default(),
        fallback_providers: vec!["claude".into()],
        eligibility: PackEligibility::default(),
        traits: vec!["systematic".into(), "first-principles".into()],
        tags: vec!["orchestration".into(), "multi-agent".into(), "workflows".into()],
        author: Some("ClawDesk".into()),
        content_address: None,
        trust_level: None,
        metadata: Default::default(),
    });
    register_pack(&mut registry, SkillPack {
        id: PackId::new("meta", "evaluator"),
        display_name: "Quality Evaluator".into(),
        description: "Evaluate agent responses for quality, accuracy, and safety".into(),
        version: "1.0.0".into(),
        tier: PackTier::Meta,
        persona_prompt: "You are a quality evaluator. Score responses on accuracy, helpfulness, safety, and coherence. Provide structured feedback. Identify improvement opportunities.".into(),
        persona_tokens: 50,
        skills: vec![
            SkillWeight::required("text-generation"),
            SkillWeight::required("reasoning-advanced"),
        ],
        pipeline_template: Some("criteria → evaluate → score → feedback".into()),
        tool_policy: PackToolPolicy::default(),
        fallback_providers: vec!["claude".into()],
        eligibility: PackEligibility::default(),
        traits: vec!["systematic".into(), "evidence-based".into(), "structured-report".into()],
        tags: vec!["evaluation".into(), "quality".into(), "safety".into()],
        author: Some("ClawDesk".into()),
        content_address: None,
        trust_level: None,
        metadata: Default::default(),
    });
    register_pack(&mut registry, SkillPack {
        id: PackId::new("meta", "router"),
        display_name: "Intent Router".into(),
        description: "Route user requests to the most appropriate agent or pack".into(),
        version: "1.0.0".into(),
        tier: PackTier::Meta,
        persona_prompt: "You are an intent router. Classify user intent quickly. Route to the best-matching specialist. Minimize latency. Handle ambiguity gracefully.".into(),
        persona_tokens: 50,
        skills: vec![
            SkillWeight::required("text-generation"),
            SkillWeight::required("reasoning-advanced"),
        ],
        pipeline_template: Some("classify → match → route → verify".into()),
        tool_policy: PackToolPolicy::default(),
        fallback_providers: vec!["claude".into()],
        eligibility: PackEligibility::default(),
        traits: vec!["concise".into(), "systematic".into()],
        tags: vec!["routing".into(), "intent".into(), "classification".into()],
        author: Some("ClawDesk".into()),
        content_address: None,
        trust_level: None,
        metadata: Default::default(),
    });
    register_pack(&mut registry, SkillPack {
        id: PackId::new("meta", "planner"),
        display_name: "Task Planner".into(),
        description: "Multi-step plan generation with dependency analysis".into(),
        version: "1.0.0".into(),
        tier: PackTier::Meta,
        persona_prompt: "You are a task planner. Create detailed execution plans with dependencies. Identify parallelizable steps. Estimate time and effort. Handle contingencies.".into(),
        persona_tokens: 50,
        skills: vec![
            SkillWeight::required("text-generation"),
            SkillWeight::required("reasoning-advanced"),
        ],
        pipeline_template: Some("decompose → order → estimate → contingency".into()),
        tool_policy: PackToolPolicy::default(),
        fallback_providers: vec!["claude".into()],
        eligibility: PackEligibility::default(),
        traits: vec!["systematic".into(), "first-principles".into(), "structured-report".into()],
        tags: vec!["planning".into(), "task-decomposition".into(), "dependencies".into()],
        author: Some("ClawDesk".into()),
        content_address: None,
        trust_level: None,
        metadata: Default::default(),
    });

    registry
}

/// Helper to register a pack.
fn register_pack(registry: &mut PackRegistry, pack: SkillPack) {
    registry.register(pack);
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bundled_pack_count() {
        let registry = load_bundled_packs();
        let count = registry.all_packs().len();
        assert!(count >= 30, "expected ≥30 bundled packs, got {}", count);
    }

    #[test]
    fn test_all_tiers_represented() {
        let registry = load_bundled_packs();
        for tier in [
            PackTier::Productivity,
            PackTier::Engineering,
            PackTier::Business,
            PackTier::Professional,
            PackTier::Life,
            PackTier::Meta,
        ] {
            let packs = registry.by_tier(tier);
            assert!(
                !packs.is_empty(),
                "no packs for tier {:?}",
                tier
            );
        }
    }

    #[test]
    fn test_each_pack_has_persona() {
        let registry = load_bundled_packs();
        for pack in registry.all_packs() {
            assert!(
                !pack.persona_prompt.is_empty(),
                "pack {} has empty persona_prompt",
                pack.id
            );
        }
    }

    #[test]
    fn test_each_pack_has_skills() {
        let registry = load_bundled_packs();
        for pack in registry.all_packs() {
            assert!(
                !pack.skills.is_empty(),
                "pack {} has no skills",
                pack.id
            );
        }
    }

    #[test]
    fn test_coder_pack_requires_code_execution() {
        let registry = load_bundled_packs();
        let coder = registry.get_by_str("engineering/coder").expect("coder pack not found");
        assert!(coder.tool_policy.require.contains(&"code-execution".to_string()));
    }

    #[test]
    fn test_legal_pack_has_constraint() {
        let registry = load_bundled_packs();
        let legal = registry.get_by_str("business/legal").expect("legal pack not found");
        assert!(legal.traits.contains(&"no-legal-advice".to_string()));
    }
}
