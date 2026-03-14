//! # External CLI Agent Orchestration
//!
//! Spawns, monitors, and coordinates external CLI agents (Claude Code,
//! Codex, Gemini CLI, Aider, etc.) as child OS processes with:
//!
//! - **Parallel execution** via `JoinSet` — multiple agents run concurrently
//! - **Blocking detection** — kills agents that produce no output for too long
//! - **Output streaming** — real-time relay to parent agent's context
//! - **Cross-agent communication** — agents can read each other's output
//! - **Rate limiting** — prevents API quota exhaustion across parallel agents
//! - **Workspace isolation** — each agent gets a separate working directory
//!
//! ## Architecture
//!
//! ```text
//! Parent Agent (ClawDesk runner)
//!   │
//!   ├─ spawn("claude", task_a) ──→ [Process A] ──→ output_a.jsonl
//!   │                                                    ↓
//!   ├─ spawn("codex", task_b)  ──→ [Process B] ──→ output_b.jsonl
//!   │                                                    ↓
//!   └─ wait_any / wait_all     ←── output relay ←── system events queue
//! ```
//!
//! ## Known Risks (from real-world deployments)
//!
//! 1. **API rate limits** — parallel agents sharing the same API key hit
//!    rate limits. Mitigation: stagger starts, per-agent rate tokens.
//! 2. **Git conflicts** — multiple agents editing the same repo create
//!    merge conflicts. Mitigation: git worktree isolation.
//! 3. **Context pollution** — agent A's output confuses agent B.
//!    Mitigation: explicit output boundaries, per-agent workspace.
//! 4. **Zombie processes** — agents that hang after parent dies.
//!    Mitigation: process group kill, no-output timeout.
//! 5. **Cost explosion** — parallel agents multiplying token spend.
//!    Mitigation: per-agent budget caps, global cost check.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

// ═══════════════════════════════════════════════════════════════════════════════
// KNOWN CLI AGENTS
// ═══════════════════════════════════════════════════════════════════════════════

/// Registry of known external CLI agents with their invocation patterns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliAgentSpec {
    pub id: String,
    pub name: String,
    pub command: String,
    /// Flag to pass the task/prompt (e.g., "-p" for claude, "--message" for aider).
    pub prompt_flag: String,
    /// Flag to override the model (None = agent picks its own).
    pub model_flag: Option<String>,
    /// Default arguments always passed.
    pub default_args: Vec<String>,
    /// Whether this agent modifies files in the current working directory.
    pub modifies_workspace: bool,
    /// Maximum runtime before forced kill (seconds).
    pub max_runtime_secs: u64,
    /// Kill if no stdout/stderr for this many seconds.
    pub no_output_timeout_secs: u64,
    /// Number of concurrent instances allowed.
    pub max_concurrent: usize,
    /// Estimated cost per invocation (USD) for budget tracking.
    pub estimated_cost_usd: f64,
}

pub fn known_agents() -> Vec<CliAgentSpec> {
    vec![
        CliAgentSpec {
            id: "claude-code".into(), name: "Claude Code".into(),
            command: "claude".into(), prompt_flag: "-p".into(),
            model_flag: Some("--model".into()),
            default_args: vec!["--allowedTools".into(), "Edit,Bash,Read,Write,MultiEdit".into()],
            modifies_workspace: true, max_runtime_secs: 600,
            no_output_timeout_secs: 120, max_concurrent: 3, estimated_cost_usd: 0.50,
        },
        CliAgentSpec {
            id: "codex".into(), name: "OpenAI Codex CLI".into(),
            command: "codex".into(), prompt_flag: "-p".into(),
            model_flag: Some("--model".into()),
            default_args: vec!["--full-auto".into()],
            modifies_workspace: true, max_runtime_secs: 600,
            no_output_timeout_secs: 120, max_concurrent: 2, estimated_cost_usd: 0.30,
        },
        CliAgentSpec {
            id: "gemini-cli".into(), name: "Gemini CLI".into(),
            command: "gemini".into(), prompt_flag: "-p".into(),
            model_flag: Some("--model".into()),
            default_args: vec![],
            modifies_workspace: true, max_runtime_secs: 600,
            no_output_timeout_secs: 120, max_concurrent: 2, estimated_cost_usd: 0.20,
        },
        CliAgentSpec {
            id: "aider".into(), name: "Aider".into(),
            command: "aider".into(), prompt_flag: "--message".into(),
            model_flag: Some("--model".into()),
            default_args: vec!["--yes-always".into(), "--no-auto-commits".into()],
            modifies_workspace: true, max_runtime_secs: 600,
            no_output_timeout_secs: 180, max_concurrent: 1, estimated_cost_usd: 0.40,
        },
        CliAgentSpec {
            id: "gh-copilot".into(), name: "GitHub Copilot CLI".into(),
            command: "gh".into(), prompt_flag: "".into(),
            model_flag: None,
            default_args: vec!["copilot".into(), "suggest".into()],
            modifies_workspace: false, max_runtime_secs: 60,
            no_output_timeout_secs: 30, max_concurrent: 5, estimated_cost_usd: 0.01,
        },
    ]
}

// ═══════════════════════════════════════════════════════════════════════════════
// SPAWN CONFIGURATION
// ═══════════════════════════════════════════════════════════════════════════════

/// Full configuration for spawning a CLI agent process.
#[derive(Debug, Clone)]
pub struct SpawnConfig {
    pub agent_id: String,
    pub task: String,
    pub model_override: Option<String>,
    pub workspace: String,
    pub extra_args: Vec<String>,
    pub env: HashMap<String, String>,
    /// Budget cap for this specific run (USD). Kill if exceeded.
    pub budget_cap_usd: Option<f64>,
    /// Whether to use git worktree isolation.
    pub use_worktree: bool,
}

/// Build the command-line arguments for a spawn.
pub fn build_argv(spec: &CliAgentSpec, config: &SpawnConfig) -> Vec<String> {
    let mut argv = vec![spec.command.clone()];
    argv.extend(spec.default_args.iter().cloned());

    if let (Some(ref flag), Some(ref model)) = (&spec.model_flag, &config.model_override) {
        argv.push(flag.clone());
        argv.push(model.clone());
    }

    if !spec.prompt_flag.is_empty() {
        argv.push(spec.prompt_flag.clone());
    }
    argv.push(config.task.clone());
    argv.extend(config.extra_args.iter().cloned());
    argv
}

// ═══════════════════════════════════════════════════════════════════════════════
// RUN TRACKING
// ═══════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone)]
pub struct RunStatus {
    pub run_id: String,
    pub agent_id: String,
    pub state: RunState,
    pub pid: Option<u32>,
    pub started_at: Instant,
    pub last_output_at: Option<Instant>,
    pub exit_code: Option<i32>,
    pub output_bytes: usize,
    pub output_lines: usize,
    pub estimated_cost_usd: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunState {
    Starting,
    Running,
    Completed,
    Failed,
    TimedOut,
    Killed,
    BlockingDetected,
    BudgetExceeded,
}

impl RunStatus {
    /// Check if this run should be killed due to no output.
    pub fn is_blocked(&self, no_output_timeout: Duration) -> bool {
        if self.state != RunState::Running { return false; }
        match self.last_output_at {
            Some(last) => last.elapsed() > no_output_timeout,
            None => self.started_at.elapsed() > no_output_timeout,
        }
    }

    /// Check if this run has exceeded its maximum runtime.
    pub fn is_timed_out(&self, max_runtime: Duration) -> bool {
        self.state == RunState::Running && self.started_at.elapsed() > max_runtime
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// ORCHESTRATOR — manages parallel CLI agent execution
// ═══════════════════════════════════════════════════════════════════════════════

/// Manages concurrent CLI agent processes with health monitoring.
pub struct CliOrchestrator {
    runs: Arc<RwLock<HashMap<String, RunStatus>>>,
    specs: HashMap<String, CliAgentSpec>,
    max_global_concurrent: usize,
    global_budget_cap_usd: f64,
}

impl CliOrchestrator {
    pub fn new(max_global_concurrent: usize, global_budget_cap_usd: f64) -> Self {
        let specs: HashMap<String, CliAgentSpec> = known_agents()
            .into_iter()
            .map(|s| (s.id.clone(), s))
            .collect();
        Self {
            runs: Arc::new(RwLock::new(HashMap::new())),
            specs,
            max_global_concurrent,
            global_budget_cap_usd,
        }
    }

    /// Check if we can spawn another agent (global + per-agent limits).
    pub async fn can_spawn(&self, agent_id: &str) -> Result<(), String> {
        let runs = self.runs.read().await;
        let active_count = runs.values()
            .filter(|r| matches!(r.state, RunState::Running | RunState::Starting))
            .count();
        if active_count >= self.max_global_concurrent {
            return Err(format!("Global limit of {} concurrent agents reached", self.max_global_concurrent));
        }

        if let Some(spec) = self.specs.get(agent_id) {
            let agent_active = runs.values()
                .filter(|r| r.agent_id == agent_id && matches!(r.state, RunState::Running | RunState::Starting))
                .count();
            if agent_active >= spec.max_concurrent {
                return Err(format!("{} already has {} instances running (max {})", spec.name, agent_active, spec.max_concurrent));
            }
        }

        // Global budget check
        let total_cost: f64 = runs.values().map(|r| r.estimated_cost_usd).sum();
        if total_cost >= self.global_budget_cap_usd {
            return Err(format!("Global budget cap ${:.2} exceeded (spent: ${:.2})", self.global_budget_cap_usd, total_cost));
        }

        Ok(())
    }

    /// Register a new run.
    pub async fn register_run(&self, run_id: String, agent_id: String) {
        let spec = self.specs.get(&agent_id);
        let mut runs = self.runs.write().await;
        runs.insert(run_id.clone(), RunStatus {
            run_id,
            agent_id,
            state: RunState::Starting,
            pid: None,
            started_at: Instant::now(),
            last_output_at: None,
            exit_code: None,
            output_bytes: 0,
            output_lines: 0,
            estimated_cost_usd: spec.map(|s| s.estimated_cost_usd).unwrap_or(0.0),
        });
    }

    /// Update run state.
    pub async fn update(&self, run_id: &str, state: RunState, pid: Option<u32>, exit_code: Option<i32>) {
        let mut runs = self.runs.write().await;
        if let Some(run) = runs.get_mut(run_id) {
            run.state = state;
            if let Some(p) = pid { run.pid = Some(p); }
            if let Some(c) = exit_code { run.exit_code = Some(c); }
        }
    }

    /// Record output from a running agent.
    pub async fn record_output(&self, run_id: &str, bytes: usize) {
        let mut runs = self.runs.write().await;
        if let Some(run) = runs.get_mut(run_id) {
            run.last_output_at = Some(Instant::now());
            run.output_bytes += bytes;
            run.output_lines += 1;
        }
    }

    /// Health check — returns runs that should be killed.
    pub async fn check_health(&self) -> Vec<(String, RunState)> {
        let runs = self.runs.read().await;
        let mut to_kill = Vec::new();
        for run in runs.values() {
            if run.state != RunState::Running { continue; }
            let spec = self.specs.get(&run.agent_id);
            let no_output = spec.map(|s| Duration::from_secs(s.no_output_timeout_secs)).unwrap_or(Duration::from_secs(120));
            let max_rt = spec.map(|s| Duration::from_secs(s.max_runtime_secs)).unwrap_or(Duration::from_secs(600));

            if run.is_blocked(no_output) {
                to_kill.push((run.run_id.clone(), RunState::BlockingDetected));
            } else if run.is_timed_out(max_rt) {
                to_kill.push((run.run_id.clone(), RunState::TimedOut));
            }
        }
        to_kill
    }

    /// List all active runs.
    pub async fn active_runs(&self) -> Vec<RunStatus> {
        let runs = self.runs.read().await;
        runs.values()
            .filter(|r| matches!(r.state, RunState::Running | RunState::Starting))
            .cloned()
            .collect()
    }

    /// Get a specific run's status.
    pub async fn get_run(&self, run_id: &str) -> Option<RunStatus> {
        let runs = self.runs.read().await;
        runs.get(run_id).cloned()
    }

    /// Clean up completed runs and return them.
    pub async fn drain_completed(&self) -> Vec<RunStatus> {
        let mut runs = self.runs.write().await;
        let completed_ids: Vec<String> = runs.iter()
            .filter(|(_, r)| !matches!(r.state, RunState::Running | RunState::Starting))
            .map(|(id, _)| id.clone())
            .collect();
        completed_ids.iter().filter_map(|id| runs.remove(id)).collect()
    }

    /// Get the spec for a known agent.
    pub fn get_spec(&self, agent_id: &str) -> Option<&CliAgentSpec> {
        self.specs.get(agent_id)
    }
}

impl Default for CliOrchestrator {
    fn default() -> Self {
        Self::new(8, 50.0) // 8 concurrent, $50 budget cap
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_known_agents_valid() {
        let agents = known_agents();
        assert!(agents.len() >= 5);
        for a in &agents {
            assert!(!a.command.is_empty());
            assert!(a.max_runtime_secs > 0);
            assert!(a.no_output_timeout_secs > 0);
        }
    }

    #[test]
    fn test_build_argv() {
        let spec = &known_agents()[0]; // claude-code
        let config = SpawnConfig {
            agent_id: "claude-code".into(), task: "Fix the auth bug".into(),
            model_override: Some("opus".into()), workspace: "/tmp/test".into(),
            extra_args: vec![], env: HashMap::new(), budget_cap_usd: None, use_worktree: false,
        };
        let argv = build_argv(spec, &config);
        assert_eq!(argv[0], "claude");
        assert!(argv.contains(&"--model".into()));
        assert!(argv.contains(&"opus".into()));
        assert!(argv.contains(&"Fix the auth bug".into()));
    }

    #[test]
    fn test_blocking_detection() {
        let run = RunStatus {
            run_id: "r1".into(), agent_id: "test".into(),
            state: RunState::Running, pid: Some(1234),
            started_at: Instant::now() - Duration::from_secs(300),
            last_output_at: Some(Instant::now() - Duration::from_secs(200)),
            exit_code: None, output_bytes: 100, output_lines: 5, estimated_cost_usd: 0.1,
        };
        assert!(run.is_blocked(Duration::from_secs(120)));
        assert!(!run.is_blocked(Duration::from_secs(300)));
    }

    #[tokio::test]
    async fn test_orchestrator_limits() {
        let orch = CliOrchestrator::new(2, 10.0);
        orch.register_run("r1".into(), "claude-code".into()).await;
        orch.update("r1", RunState::Running, Some(1), None).await;
        orch.register_run("r2".into(), "codex".into()).await;
        orch.update("r2", RunState::Running, Some(2), None).await;

        // Should fail — global limit is 2
        let result = orch.can_spawn("gemini-cli").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_per_agent_limit() {
        let orch = CliOrchestrator::new(10, 100.0);
        // Aider has max_concurrent = 1
        orch.register_run("r1".into(), "aider".into()).await;
        orch.update("r1", RunState::Running, Some(1), None).await;

        let result = orch.can_spawn("aider").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("max 1"));
    }

    #[tokio::test]
    async fn test_budget_cap() {
        let orch = CliOrchestrator::new(10, 0.50);
        // Each claude-code run costs ~$0.50
        orch.register_run("r1".into(), "claude-code".into()).await;
        orch.update("r1", RunState::Running, Some(1), None).await;

        let result = orch.can_spawn("claude-code").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("budget"));
    }
}
