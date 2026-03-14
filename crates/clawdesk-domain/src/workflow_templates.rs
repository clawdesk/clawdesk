//! # Workflow Templates — Built-in orchestration workflows for ClawDesk
//!
//! Each template defines a complete workflow with:
//! - Agent configurations (which agents to create)
//! - Pipeline definition (how agents connect)
//! - Cron schedules (when things run)
//! - Required skills/tools
//!
//! These are loaded by the frontend and the CLI's `clawdesk pipeline import`.

use serde::{Deserialize, Serialize};

/// A complete workflow template that can be deployed in one click.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowTemplate {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: TemplateCategory,
    pub icon: String,
    /// Agents that need to be created for this workflow.
    pub agents: Vec<AgentSpec>,
    /// Pipeline connections between agents.
    pub pipeline: Vec<PipelineStep>,
    /// Cron schedules that trigger the workflow automatically.
    pub schedules: Vec<ScheduleSpec>,
    /// Required channel integrations.
    pub required_channels: Vec<String>,
    /// Required skills/tools.
    pub required_skills: Vec<String>,
    /// Estimated cost per day (USD).
    pub estimated_daily_cost_usd: f64,
    /// Difficulty level.
    pub difficulty: Difficulty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TemplateCategory {
    SocialMedia,
    ContentCreation,
    DevOps,
    Productivity,
    Research,
    Business,
    MultiAgent,
    Infrastructure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Difficulty {
    Beginner,
    Intermediate,
    Advanced,
}

/// Specification for an agent to create.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSpec {
    pub role: String,
    pub name: String,
    pub description: String,
    pub model: String,
    pub fallback_models: Vec<String>,
    pub system_prompt: String,
    pub tools: Vec<String>,
    pub icon: String,
}

/// A pipeline step connecting agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineStep {
    pub from: String,
    pub to: String,
    pub trigger: StepTrigger,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepTrigger {
    /// Output of `from` automatically feeds into `to`.
    Automatic,
    /// User reviews `from` output before passing to `to`.
    ManualApproval,
    /// Triggered by a cron schedule.
    Scheduled { cron: String },
}

/// A cron schedule specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleSpec {
    pub name: String,
    pub cron: String,
    pub agent_role: String,
    pub prompt: String,
    pub description: String,
}

/// Get all built-in workflow templates.
pub fn builtin_templates() -> Vec<WorkflowTemplate> {
    vec![
        // ═══ Content Factory ═══
        WorkflowTemplate {
            id: "content-factory".into(),
            name: "Multi-Agent Content Factory".into(),
            description: "Research → Write → Design pipeline. Agents work in dedicated channels, producing content overnight.".into(),
            category: TemplateCategory::ContentCreation,
            icon: "🏭".into(),
            agents: vec![
                AgentSpec {
                    role: "research".into(),
                    name: "Research Agent".into(),
                    description: "Scans trending stories, competitor content, and social media".into(),
                    model: "gemini:gemini-2.5-pro".into(),
                    fallback_models: vec!["openai:gpt-4o".into()],
                    system_prompt: "You are a research agent. Every morning, research top trending stories, competitor content, and what's performing well on social media in the user's niche. Post the top 5 content opportunities with sources.".into(),
                    tools: vec!["web_search".into(), "file_write".into()],
                    icon: "🔍".into(),
                },
                AgentSpec {
                    role: "writer".into(),
                    name: "Writing Agent".into(),
                    description: "Takes research output and writes full scripts/drafts".into(),
                    model: "anthropic:claude-sonnet-4-20250514".into(),
                    fallback_models: vec!["openai:gpt-4o".into()],
                    system_prompt: "You are a content writing agent. Take the research report and write a full draft — script, thread, or newsletter. Match the creator's voice and style.".into(),
                    tools: vec!["file_read".into(), "file_write".into()],
                    icon: "✍️".into(),
                },
                AgentSpec {
                    role: "designer".into(),
                    name: "Design Agent".into(),
                    description: "Generates thumbnails and cover images".into(),
                    model: "openai:gpt-4o".into(),
                    fallback_models: vec![],
                    system_prompt: "You are a design agent. Create thumbnail descriptions and cover image briefs for the content.".into(),
                    tools: vec!["file_read".into(), "file_write".into()],
                    icon: "🎨".into(),
                },
            ],
            pipeline: vec![
                PipelineStep { from: "research".into(), to: "writer".into(), trigger: StepTrigger::Automatic, description: "Research feeds into writing".into() },
                PipelineStep { from: "writer".into(), to: "designer".into(), trigger: StepTrigger::Automatic, description: "Written content feeds into design".into() },
            ],
            schedules: vec![
                ScheduleSpec { name: "Morning research".into(), cron: "0 8 * * *".into(), agent_role: "research".into(), prompt: "Research today's top 5 content opportunities in AI and startups".into(), description: "Daily at 8 AM".into() },
            ],
            required_channels: vec!["discord".into()],
            required_skills: vec!["web_search".into()],
            estimated_daily_cost_usd: 0.50,
            difficulty: Difficulty::Intermediate,
        },

        // ═══ Solo Founder Team ═══
        WorkflowTemplate {
            id: "solo-founder-team".into(),
            name: "Solo Founder AI Team".into(),
            description: "Strategy lead, business analyst, marketing researcher, and dev agent — all coordinated through shared memory.".into(),
            category: TemplateCategory::MultiAgent,
            icon: "👥".into(),
            agents: vec![
                AgentSpec {
                    role: "strategy_lead".into(),
                    name: "Milo (Strategy Lead)".into(),
                    description: "Big-picture planning, OKR tracking, team coordination".into(),
                    model: "anthropic:claude-sonnet-4-20250514".into(),
                    fallback_models: vec!["openai:o1".into()],
                    system_prompt: "You are Milo, the strategy lead. Confident, big-picture, charismatic. You coordinate the team, set weekly goals, and synthesize insights from all agents.".into(),
                    tools: vec!["web_search".into(), "file_read".into(), "file_write".into()],
                    icon: "🎯".into(),
                },
                AgentSpec {
                    role: "business_analyst".into(),
                    name: "Josh (Business Analyst)".into(),
                    description: "Numbers-driven: pricing, metrics, competitive analysis".into(),
                    model: "anthropic:claude-sonnet-4-20250514".into(),
                    fallback_models: vec!["openai:gpt-4o".into()],
                    system_prompt: "You are Josh, the business analyst. Pragmatic, numbers-driven. You track KPIs, analyze competitors, model revenue, and surface customer feedback.".into(),
                    tools: vec!["web_search".into(), "file_read".into(), "file_write".into()],
                    icon: "📊".into(),
                },
                AgentSpec {
                    role: "marketing".into(),
                    name: "Marketing Agent".into(),
                    description: "Content ideation, trend tracking, SEO research".into(),
                    model: "gemini:gemini-2.5-pro".into(),
                    fallback_models: vec!["openai:gpt-4o".into()],
                    system_prompt: "You are the marketing researcher. Creative, curious, trend-aware. You track trends, ideate content, monitor competitors on social media, and do SEO research.".into(),
                    tools: vec!["web_search".into(), "file_read".into(), "file_write".into()],
                    icon: "📣".into(),
                },
                AgentSpec {
                    role: "dev".into(),
                    name: "Dev Agent".into(),
                    description: "Code, architecture, reviews, debugging".into(),
                    model: "anthropic:claude-sonnet-4-20250514".into(),
                    fallback_models: vec!["openai:gpt-4o".into()],
                    system_prompt: "You are the dev agent. Precise, thorough, security-conscious. You write code, review PRs, investigate bugs, and maintain technical documentation.".into(),
                    tools: vec!["file_read".into(), "file_write".into(), "shell_exec".into(), "grep".into()],
                    icon: "💻".into(),
                },
            ],
            pipeline: vec![
                PipelineStep { from: "strategy_lead".into(), to: "business_analyst".into(), trigger: StepTrigger::Automatic, description: "Strategy delegates analysis tasks".into() },
                PipelineStep { from: "strategy_lead".into(), to: "marketing".into(), trigger: StepTrigger::Automatic, description: "Strategy delegates marketing tasks".into() },
                PipelineStep { from: "strategy_lead".into(), to: "dev".into(), trigger: StepTrigger::Automatic, description: "Strategy delegates dev tasks".into() },
            ],
            schedules: vec![
                ScheduleSpec { name: "Morning standup".into(), cron: "0 8 * * 1-5".into(), agent_role: "strategy_lead".into(), prompt: "Review overnight agent activity and post a morning standup summary with progress toward weekly goals".into(), description: "Weekdays at 8 AM".into() },
                ScheduleSpec { name: "Metrics check".into(), cron: "0 9 * * 1-5".into(), agent_role: "business_analyst".into(), prompt: "Pull and summarize key business metrics for today".into(), description: "Weekdays at 9 AM".into() },
                ScheduleSpec { name: "Trend scan".into(), cron: "0 10 * * *".into(), agent_role: "marketing".into(), prompt: "Surface 3 content ideas based on trending topics in AI and startups".into(), description: "Daily at 10 AM".into() },
                ScheduleSpec { name: "Nightly code review".into(), cron: "0 23 * * *".into(), agent_role: "dev".into(), prompt: "Review the codebase for gaps, outdated dependencies, and potential improvements. Draft a summary report.".into(), description: "Daily at 11 PM".into() },
            ],
            required_channels: vec!["telegram".into()],
            required_skills: vec!["web_search".into()],
            estimated_daily_cost_usd: 1.50,
            difficulty: Difficulty::Advanced,
        },

        // ═══ Morning Brief ═══
        WorkflowTemplate {
            id: "morning-brief".into(),
            name: "Custom Morning Brief".into(),
            description: "Wake up to a personalized briefing: news, calendar, tasks, weather, all delivered as a text message.".into(),
            category: TemplateCategory::Productivity,
            icon: "☕".into(),
            agents: vec![
                AgentSpec {
                    role: "briefer".into(),
                    name: "Morning Brief Agent".into(),
                    description: "Compiles and delivers your personalized morning briefing".into(),
                    model: "anthropic:claude-sonnet-4-20250514".into(),
                    fallback_models: vec!["openai:gpt-4o".into()],
                    system_prompt: "You deliver a morning briefing every day. Include: top news headlines relevant to the user's interests, today's calendar events, pending tasks from yesterday, weather for the user's location, and one proactive recommendation.".into(),
                    tools: vec!["web_search".into(), "file_read".into()],
                    icon: "☕".into(),
                },
            ],
            pipeline: vec![],
            schedules: vec![
                ScheduleSpec { name: "Morning brief".into(), cron: "30 7 * * *".into(), agent_role: "briefer".into(), prompt: "Compile the morning briefing: top AI/tech news, any calendar events, pending tasks, and a recommendation for today.".into(), description: "Daily at 7:30 AM".into() },
            ],
            required_channels: vec![],
            required_skills: vec!["web_search".into()],
            estimated_daily_cost_usd: 0.10,
            difficulty: Difficulty::Beginner,
        },

        // ═══ Daily Reddit Digest ═══
        WorkflowTemplate {
            id: "reddit-digest".into(),
            name: "Daily Reddit Digest".into(),
            description: "Summarize top posts from your favorite subreddits, delivered every morning.".into(),
            category: TemplateCategory::SocialMedia,
            icon: "📰".into(),
            agents: vec![
                AgentSpec {
                    role: "reddit_scanner".into(),
                    name: "Reddit Scanner".into(),
                    description: "Scans subreddits for top posts and summarizes them".into(),
                    model: "gemini:gemini-2.5-pro".into(),
                    fallback_models: vec!["openai:gpt-4o".into()],
                    system_prompt: "You scan Reddit for the top performing posts in the user's configured subreddits. Summarize each post (title, score, key discussion points) and rank by relevance.".into(),
                    tools: vec!["web_search".into(), "file_write".into()],
                    icon: "📰".into(),
                },
            ],
            pipeline: vec![],
            schedules: vec![
                ScheduleSpec { name: "Reddit scan".into(), cron: "0 8 * * *".into(), agent_role: "reddit_scanner".into(), prompt: "Scan r/MachineLearning, r/LocalLLaMA, r/artificial, r/programming for today's top posts. Summarize the top 10 with discussion highlights.".into(), description: "Daily at 8 AM".into() },
            ],
            required_channels: vec![],
            required_skills: vec!["web_search".into()],
            estimated_daily_cost_usd: 0.05,
            difficulty: Difficulty::Beginner,
        },

        // ═══ Self-Healing Server ═══
        WorkflowTemplate {
            id: "self-healing-server".into(),
            name: "Self-Healing Home Server".into(),
            description: "Always-on infrastructure agent with SSH, automated cron, and self-healing capabilities.".into(),
            category: TemplateCategory::Infrastructure,
            icon: "🖥️".into(),
            agents: vec![
                AgentSpec {
                    role: "infra".into(),
                    name: "Infrastructure Agent".into(),
                    description: "Monitors and heals your home server infrastructure".into(),
                    model: "anthropic:claude-sonnet-4-20250514".into(),
                    fallback_models: vec!["openai:gpt-4o".into()],
                    system_prompt: "You are an infrastructure agent with SSH access. Monitor services, check disk usage, verify certificates, and fix issues automatically. Log all actions. Escalate to the user if a fix requires destructive changes.".into(),
                    tools: vec!["shell_exec".into(), "file_read".into(), "file_write".into()],
                    icon: "🖥️".into(),
                },
            ],
            pipeline: vec![],
            schedules: vec![
                ScheduleSpec { name: "Health check".into(), cron: "*/15 * * * *".into(), agent_role: "infra".into(), prompt: "Run health checks: verify all critical services are running, check disk usage, check certificate expiry dates. Fix any issues found.".into(), description: "Every 15 minutes".into() },
                ScheduleSpec { name: "Nightly cleanup".into(), cron: "0 3 * * *".into(), agent_role: "infra".into(), prompt: "Run nightly maintenance: rotate logs, clean Docker images, verify backups, update system packages if safe.".into(), description: "Daily at 3 AM".into() },
            ],
            required_channels: vec!["telegram".into()],
            required_skills: vec![],
            estimated_daily_cost_usd: 0.30,
            difficulty: Difficulty::Advanced,
        },

        // ═══ SEO + Growth Engine ═══
        WorkflowTemplate {
            id: "seo-growth".into(),
            name: "SEO & Growth Engine".into(),
            description: "Research agent finds keywords, writer produces blog posts, social agent distributes across platforms.".into(),
            category: TemplateCategory::Business,
            icon: "📈".into(),
            agents: vec![
                AgentSpec {
                    role: "seo_researcher".into(),
                    name: "SEO Researcher".into(),
                    description: "Identifies keywords and competitor content gaps".into(),
                    model: "gemini:gemini-2.5-pro".into(),
                    fallback_models: vec!["openai:gpt-4o".into()],
                    system_prompt: "You are an SEO research agent. Find the exact questions target customers are searching. Identify competitor content gaps. Produce keyword briefs with search volume estimates.".into(),
                    tools: vec!["web_search".into(), "file_write".into()],
                    icon: "🔍".into(),
                },
                AgentSpec {
                    role: "blog_writer".into(),
                    name: "Blog Writer".into(),
                    description: "Writes SEO-optimized long-form blog posts".into(),
                    model: "anthropic:claude-sonnet-4-20250514".into(),
                    fallback_models: vec!["openai:gpt-4o".into()],
                    system_prompt: "You write SEO-optimized blog posts. Use the keyword brief from the research agent. Write engaging, informative content that ranks. Include headers, meta descriptions, and internal link suggestions.".into(),
                    tools: vec!["file_read".into(), "file_write".into()],
                    icon: "✍️".into(),
                },
                AgentSpec {
                    role: "social_distributor".into(),
                    name: "Social Distributor".into(),
                    description: "Repurposes blog content into social media posts".into(),
                    model: "openai:gpt-4o".into(),
                    fallback_models: vec![],
                    system_prompt: "You repurpose blog content into social media posts. Create 3-5 Twitter/X threads, 1 LinkedIn post, and 1 newsletter excerpt from each blog post. Maintain the brand voice.".into(),
                    tools: vec!["file_read".into(), "file_write".into()],
                    icon: "📱".into(),
                },
            ],
            pipeline: vec![
                PipelineStep { from: "seo_researcher".into(), to: "blog_writer".into(), trigger: StepTrigger::Automatic, description: "Keyword research feeds into writing".into() },
                PipelineStep { from: "blog_writer".into(), to: "social_distributor".into(), trigger: StepTrigger::ManualApproval, description: "Review blog post before social distribution".into() },
            ],
            schedules: vec![
                ScheduleSpec { name: "Daily keyword research".into(), cron: "0 6 * * 1-5".into(), agent_role: "seo_researcher".into(), prompt: "Find 3 new keyword opportunities in AI tooling. Produce a brief with search volume and competition level.".into(), description: "Weekdays at 6 AM".into() },
            ],
            required_channels: vec![],
            required_skills: vec!["web_search".into()],
            estimated_daily_cost_usd: 0.80,
            difficulty: Difficulty::Intermediate,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_templates_valid() {
        let templates = builtin_templates();
        assert!(templates.len() >= 6);
        for t in &templates {
            assert!(!t.id.is_empty());
            assert!(!t.name.is_empty());
            assert!(!t.agents.is_empty());
            // Every pipeline step references a valid agent role.
            let roles: Vec<&str> = t.agents.iter().map(|a| a.role.as_str()).collect();
            for step in &t.pipeline {
                assert!(roles.contains(&step.from.as_str()), "pipeline 'from' role '{}' not in agents", step.from);
                assert!(roles.contains(&step.to.as_str()), "pipeline 'to' role '{}' not in agents", step.to);
            }
            // Every schedule references a valid agent role.
            for sched in &t.schedules {
                assert!(roles.contains(&sched.agent_role.as_str()), "schedule role '{}' not in agents", sched.agent_role);
            }
        }
    }
}
