//! # Auto-Hierarchy Agent Composer
//!
//! Given a user's task, automatically selects the right agents from the
//! catalog, composes them into a DAG, and wires skills + memory.
//!
//! ## How it works
//!
//! 1. Classify the task into a domain (coding, research, writing, etc.)
//! 2. Select agents from the catalog that match the domain
//! 3. Compose them into a hierarchy:
//!    - Coordinator (top) — decomposes + delegates
//!    - Specialists (middle) — execute subtasks
//!    - Reviewer (bottom) — validates output
//! 4. Wire shared memory so agents can see each other's work
//! 5. Return the DAG for the orchestrator to execute
//!
//! ## Example
//!
//! Task: "Build a todo app with tests"
//! → Coordinator → [Coder (writes code), Test Engineer (writes tests)]
//!   → Reviewer (validates both)
//!
//! Task: "Research AI market trends and write a report"
//! → Coordinator → [Researcher (gathers data), Writer (drafts report)]
//!   → Reviewer (fact-checks)

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A composed agent team — a DAG of agents ready for execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComposedTeam {
    /// Human-readable name for this team.
    pub name: String,
    /// Why this composition was chosen.
    pub rationale: String,
    /// Agents in the team, ordered by execution (coordinator first).
    pub agents: Vec<TeamAgent>,
    /// Edges: agent_id → depends_on agent_ids (DAG structure).
    pub edges: Vec<TeamEdge>,
    /// Shared memory scope for the team.
    pub shared_memory_scope: String,
    /// Estimated total cost (USD).
    pub estimated_cost_usd: f64,
}

/// An agent in the composed team.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamAgent {
    pub id: String,
    /// Which agent template from the catalog.
    pub template_id: String,
    pub name: String,
    pub role: TeamRole,
    /// What this agent should do (specific subtask).
    pub task: String,
    /// Skills to activate for this agent.
    pub skills: Vec<String>,
    /// Whether this agent can run in parallel with siblings.
    pub parallelizable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TeamRole {
    Coordinator,
    Specialist,
    Reviewer,
    Observer,
}

/// A directed edge in the team DAG.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamEdge {
    pub from: String,
    pub to: String,
    pub edge_type: EdgeType,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum EdgeType {
    /// Output of `from` feeds into `to`.
    DelegatesTo,
    /// `from` reviews the output of `to`.
    Reviews,
    /// `from` and `to` share context.
    SharesContext,
}

// ═══════════════════════════════════════════════════════════════════════════════
// TASK CLASSIFICATION
// ═══════════════════════════════════════════════════════════════════════════════

/// Classify a task into domains for agent selection.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TaskDomain {
    Coding,
    Research,
    Writing,
    DataAnalysis,
    Design,
    DevOps,
    Security,
    Testing,
    General,
}

/// Classify a task description into one or more domains.
pub fn classify_task(task: &str) -> Vec<TaskDomain> {
    let lower = task.to_lowercase();
    let mut domains = Vec::new();

    let patterns: &[(&[&str], TaskDomain)] = &[
        (&["build", "code", "implement", "create app", "write code", "fix bug",
          "todo app", "api", "function", "class", "module", "refactor"],
         TaskDomain::Coding),
        (&["research", "find", "analyze market", "study", "compare", "investigate",
          "trend", "survey"],
         TaskDomain::Research),
        (&["write", "draft", "blog", "article", "report", "document", "newsletter",
          "readme", "documentation"],
         TaskDomain::Writing),
        (&["data", "csv", "chart", "statistics", "sql", "dashboard", "metrics",
          "visualization"],
         TaskDomain::DataAnalysis),
        (&["design", "ui", "ux", "wireframe", "mockup", "layout", "css", "style"],
         TaskDomain::Design),
        (&["deploy", "docker", "kubernetes", "ci/cd", "pipeline", "server",
          "infrastructure", "nginx"],
         TaskDomain::DevOps),
        (&["security", "audit", "vulnerability", "penetration", "auth", "encrypt"],
         TaskDomain::Security),
        (&["test", "spec", "coverage", "e2e", "unit test", "integration test"],
         TaskDomain::Testing),
    ];

    for (keywords, domain) in patterns {
        if keywords.iter().any(|kw| lower.contains(kw)) {
            domains.push(domain.clone());
        }
    }

    if domains.is_empty() {
        domains.push(TaskDomain::General);
    }

    domains
}

// ═══════════════════════════════════════════════════════════════════════════════
// AGENT SELECTION
// ═══════════════════════════════════════════════════════════════════════════════

/// Maps task domains to the best agent templates from the catalog.
fn select_agents_for_domains(domains: &[TaskDomain]) -> Vec<(&'static str, &'static str, TeamRole)> {
    let mut agents = Vec::new();

    for domain in domains {
        match domain {
            TaskDomain::Coding => {
                agents.push(("coder", "Coder", TeamRole::Specialist));
                agents.push(("reviewer", "Code Reviewer", TeamRole::Reviewer));
            }
            TaskDomain::Research => {
                agents.push(("researcher", "Researcher", TeamRole::Specialist));
            }
            TaskDomain::Writing => {
                agents.push(("creative-writer", "Writer", TeamRole::Specialist));
                agents.push(("reviewer", "Editor", TeamRole::Reviewer));
            }
            TaskDomain::DataAnalysis => {
                agents.push(("analyst", "Data Analyst", TeamRole::Specialist));
            }
            TaskDomain::Design => {
                agents.push(("frontend-developer", "Frontend Dev", TeamRole::Specialist));
            }
            TaskDomain::DevOps => {
                agents.push(("devops", "DevOps Engineer", TeamRole::Specialist));
            }
            TaskDomain::Security => {
                agents.push(("security-auditor", "Security Auditor", TeamRole::Specialist));
            }
            TaskDomain::Testing => {
                agents.push(("test-engineer", "Test Engineer", TeamRole::Specialist));
            }
            TaskDomain::General => {
                agents.push(("general-assistant", "Assistant", TeamRole::Specialist));
            }
        }
    }

    // Deduplicate by template_id
    agents.sort_by(|a, b| a.0.cmp(&b.0));
    agents.dedup_by(|a, b| a.0 == b.0);

    agents
}

// ═══════════════════════════════════════════════════════════════════════════════
// COMPOSITION — Builds the DAG
// ═══════════════════════════════════════════════════════════════════════════════

/// Automatically compose a team of agents for a given task.
///
/// This is the main entry point. Given a task description, it:
/// 1. Classifies the task into domains
/// 2. Selects appropriate agents
/// 3. Composes them into a coordinator → specialists → reviewer DAG
/// 4. Wires shared memory
pub fn compose_team(task: &str) -> ComposedTeam {
    let domains = classify_task(task);
    let selected = select_agents_for_domains(&domains);

    // Separate specialists and reviewers
    let specialists: Vec<_> = selected.iter()
        .filter(|(_, _, role)| *role == TeamRole::Specialist)
        .collect();
    let reviewers: Vec<_> = selected.iter()
        .filter(|(_, _, role)| *role == TeamRole::Reviewer)
        .collect();

    let mut agents = Vec::new();
    let mut edges = Vec::new();

    // Always add a coordinator at the top
    let coord_id = "coord".to_string();
    agents.push(TeamAgent {
        id: coord_id.clone(),
        template_id: "multi-agent-coordinator".into(),
        name: "Coordinator".into(),
        role: TeamRole::Coordinator,
        task: format!("Decompose and coordinate: {}", truncate(task, 200)),
        skills: vec!["web-search".into()],
        parallelizable: false,
    });

    // Add specialists — each gets a delegation edge from coordinator
    for (i, (template, name, _)) in specialists.iter().enumerate() {
        let agent_id = format!("spec-{}", i);
        agents.push(TeamAgent {
            id: agent_id.clone(),
            template_id: template.to_string(),
            name: name.to_string(),
            role: TeamRole::Specialist,
            task: format!("Execute your part of: {}", truncate(task, 150)),
            skills: vec![],
            parallelizable: specialists.len() > 1,
        });
        edges.push(TeamEdge {
            from: coord_id.clone(),
            to: agent_id,
            edge_type: EdgeType::DelegatesTo,
        });
    }

    // Add reviewers — each reviews all specialists
    for (i, (template, name, _)) in reviewers.iter().enumerate() {
        let reviewer_id = format!("review-{}", i);
        agents.push(TeamAgent {
            id: reviewer_id.clone(),
            template_id: template.to_string(),
            name: name.to_string(),
            role: TeamRole::Reviewer,
            task: "Review and validate the output of the specialists".into(),
            skills: vec![],
            parallelizable: false,
        });
        // Reviewer depends on all specialists
        for (j, _) in specialists.iter().enumerate() {
            edges.push(TeamEdge {
                from: format!("spec-{}", j),
                to: reviewer_id.clone(),
                edge_type: EdgeType::Reviews,
            });
        }
    }

    let domain_names: Vec<&str> = domains.iter().map(|d| match d {
        TaskDomain::Coding => "coding",
        TaskDomain::Research => "research",
        TaskDomain::Writing => "writing",
        TaskDomain::DataAnalysis => "data",
        TaskDomain::Design => "design",
        TaskDomain::DevOps => "devops",
        TaskDomain::Security => "security",
        TaskDomain::Testing => "testing",
        TaskDomain::General => "general",
    }).collect();

    let est_cost = agents.len() as f64 * 0.15; // rough estimate per agent

    ComposedTeam {
        name: format!("{} Team", domain_names.join(" + ")),
        rationale: format!(
            "Task classified as {:?}. Composed {} agents: 1 coordinator, {} specialists, {} reviewers.",
            domain_names, agents.len(), specialists.len(), reviewers.len()
        ),
        agents,
        edges,
        shared_memory_scope: format!("team:{}", uuid_short()),
        estimated_cost_usd: est_cost,
    }
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max { return s; }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) { end -= 1; }
    &s[..end]
}

fn uuid_short() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    format!("{:x}", t.as_millis() & 0xFFFFFFFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_coding() {
        let domains = classify_task("Build a todo app with React");
        assert!(domains.contains(&TaskDomain::Coding));
    }

    #[test]
    fn test_classify_multi_domain() {
        let domains = classify_task("Research AI trends and write a blog post about it");
        assert!(domains.contains(&TaskDomain::Research));
        assert!(domains.contains(&TaskDomain::Writing));
    }

    #[test]
    fn test_compose_coding_team() {
        let team = compose_team("Build a todo app with tests");
        // Should have: coordinator + coder + test-engineer + reviewer
        assert!(team.agents.len() >= 3);
        assert!(team.agents.iter().any(|a| a.role == TeamRole::Coordinator));
        assert!(team.agents.iter().any(|a| a.role == TeamRole::Specialist));
        assert!(!team.edges.is_empty());
    }

    #[test]
    fn test_compose_research_team() {
        let team = compose_team("Research quantum computing market trends");
        assert!(team.agents.iter().any(|a| a.template_id == "researcher"));
    }

    #[test]
    fn test_compose_multi_domain() {
        let team = compose_team("Research competitors and write a comparison blog post");
        assert!(team.agents.len() >= 4); // coord + researcher + writer + reviewer
    }

    #[test]
    fn test_dag_edges_valid() {
        let team = compose_team("Build a secure web app");
        let agent_ids: Vec<&str> = team.agents.iter().map(|a| a.id.as_str()).collect();
        for edge in &team.edges {
            assert!(agent_ids.contains(&edge.from.as_str()), "edge from '{}' not in agents", edge.from);
            assert!(agent_ids.contains(&edge.to.as_str()), "edge to '{}' not in agents", edge.to);
        }
    }
}
