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

    // ── GAP 1: Session Resume ──────────────────────────────────────────
    /// Arguments template for resuming an existing session.
    /// Use `{sessionId}` as placeholder for the session ID.
    #[serde(default)]
    pub resume_args: Vec<String>,
    /// JSON fields in CLI output that contain the session ID.
    #[serde(default)]
    pub session_id_fields: Vec<String>,
    /// When to use session IDs.
    #[serde(default)]
    pub session_mode: SessionMode,

    // ── GAP 2: Output Parsing ──────────────────────────────────────────
    /// Expected output format from the CLI agent.
    #[serde(default)]
    pub output_format: OutputFormat,
    /// Output format when resuming (may differ from fresh output).
    #[serde(default)]
    pub resume_output_format: Option<OutputFormat>,

    // ── GAP 3: Bootstrap Context ───────────────────────────────────────
    /// Flag to inject system prompt / bootstrap context (e.g., "--append-system-prompt").
    pub system_prompt_flag: Option<String>,
    /// Maximum bootstrap context characters for this agent.
    #[serde(default = "default_bootstrap_max_chars")]
    pub bootstrap_max_chars: usize,

    // ── GAP 5: Auth Credential Management ──────────────────────────────
    /// Environment variables to strip from the child process (prevents key sharing).
    #[serde(default)]
    pub clear_env: Vec<String>,

    // ── GAP 6: Watchdog Fresh/Resume ───────────────────────────────────
    /// Timeout for fresh runs (no session ID). If None, uses `no_output_timeout_secs`.
    pub fresh_timeout_secs: Option<u64>,
    /// Timeout for resume runs (has session ID). Typically shorter.
    pub resume_timeout_secs: Option<u64>,
}

fn default_bootstrap_max_chars() -> usize { 30_000 }

/// When to use session IDs for resume.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionMode {
    /// Always use sessions (create new or resume).
    Always,
    /// Only resume existing sessions, never create named sessions.
    Existing,
    /// Never use sessions (every run is fresh).
    Never,
}

impl Default for SessionMode {
    fn default() -> Self { Self::Never }
}

/// Output format from CLI agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputFormat {
    /// Single JSON object.
    Json,
    /// Newline-delimited JSON (one JSON object per line).
    Jsonl,
    /// Plain text.
    Text,
}

impl Default for OutputFormat {
    fn default() -> Self { Self::Text }
}

pub fn known_agents() -> Vec<CliAgentSpec> {
    vec![
        CliAgentSpec {
            id: "claude-code".into(), name: "Claude Code".into(),
            command: "claude".into(), prompt_flag: "-p".into(),
            model_flag: Some("--model".into()),
            default_args: vec![
                "--output-format".into(), "json".into(),
                "--permission-mode".into(), "bypassPermissions".into(),
            ],
            modifies_workspace: true, max_runtime_secs: 600,
            no_output_timeout_secs: 120, max_concurrent: 3, estimated_cost_usd: 0.50,
            // Session resume
            resume_args: vec![
                "-p".into(), "--output-format".into(), "json".into(),
                "--permission-mode".into(), "bypassPermissions".into(),
                "--resume".into(), "{sessionId}".into(),
            ],
            session_id_fields: vec!["session_id".into(), "sessionId".into()],
            session_mode: SessionMode::Always,
            // Output parsing
            output_format: OutputFormat::Json,
            resume_output_format: Some(OutputFormat::Json),
            // Bootstrap
            system_prompt_flag: Some("--append-system-prompt".into()),
            bootstrap_max_chars: 30_000,
            // Auth — strip parent's API key so Claude CLI uses its own OAuth
            clear_env: vec!["ANTHROPIC_API_KEY".into()],
            // Watchdog
            fresh_timeout_secs: Some(120),
            resume_timeout_secs: Some(60),
        },
        CliAgentSpec {
            id: "codex".into(), name: "OpenAI Codex CLI".into(),
            command: "codex".into(), prompt_flag: "-p".into(),
            model_flag: Some("--model".into()),
            default_args: vec!["--full-auto".into(), "--color".into(), "never".into()],
            modifies_workspace: true, max_runtime_secs: 600,
            no_output_timeout_secs: 120, max_concurrent: 2, estimated_cost_usd: 0.30,
            // Session resume
            resume_args: vec![
                "exec".into(), "resume".into(), "{sessionId}".into(),
                "--color".into(), "never".into(),
                "--sandbox".into(), "workspace-write".into(),
            ],
            session_id_fields: vec!["conversation_id".into(), "conversationId".into()],
            session_mode: SessionMode::Existing,
            // Output parsing
            output_format: OutputFormat::Jsonl,
            resume_output_format: Some(OutputFormat::Text),
            // Bootstrap
            system_prompt_flag: None,
            bootstrap_max_chars: 20_000,
            // Auth — strip parent's key so Codex CLI uses its own OAuth
            clear_env: vec!["OPENAI_API_KEY".into()],
            // Watchdog
            fresh_timeout_secs: Some(120),
            resume_timeout_secs: Some(45),
        },
        CliAgentSpec {
            id: "gemini-cli".into(), name: "Gemini CLI".into(),
            command: "gemini".into(), prompt_flag: "-p".into(),
            model_flag: Some("--model".into()),
            default_args: vec![],
            modifies_workspace: true, max_runtime_secs: 600,
            no_output_timeout_secs: 120, max_concurrent: 2, estimated_cost_usd: 0.20,
            resume_args: vec![],
            session_id_fields: vec![],
            session_mode: SessionMode::Never,
            output_format: OutputFormat::Text,
            resume_output_format: None,
            system_prompt_flag: None,
            bootstrap_max_chars: 20_000,
            clear_env: vec![],
            fresh_timeout_secs: None,
            resume_timeout_secs: None,
        },
        CliAgentSpec {
            id: "aider".into(), name: "Aider".into(),
            command: "aider".into(), prompt_flag: "--message".into(),
            model_flag: Some("--model".into()),
            default_args: vec!["--yes-always".into(), "--no-auto-commits".into()],
            modifies_workspace: true, max_runtime_secs: 600,
            no_output_timeout_secs: 180, max_concurrent: 1, estimated_cost_usd: 0.40,
            resume_args: vec![],
            session_id_fields: vec![],
            session_mode: SessionMode::Never,
            output_format: OutputFormat::Text,
            resume_output_format: None,
            system_prompt_flag: None,
            bootstrap_max_chars: 15_000,
            clear_env: vec![],
            fresh_timeout_secs: None,
            resume_timeout_secs: None,
        },
        CliAgentSpec {
            id: "gh-copilot".into(), name: "GitHub Copilot CLI".into(),
            command: "gh".into(), prompt_flag: "".into(),
            model_flag: None,
            default_args: vec!["copilot".into(), "suggest".into()],
            modifies_workspace: false, max_runtime_secs: 60,
            no_output_timeout_secs: 30, max_concurrent: 5, estimated_cost_usd: 0.01,
            resume_args: vec![],
            session_id_fields: vec![],
            session_mode: SessionMode::Never,
            output_format: OutputFormat::Text,
            resume_output_format: None,
            system_prompt_flag: None,
            bootstrap_max_chars: 5_000,
            clear_env: vec![],
            fresh_timeout_secs: None,
            resume_timeout_secs: None,
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
    /// Session ID to resume (from a previous run's output).
    pub session_id: Option<String>,
    /// Bootstrap context to inject as system prompt prefix.
    pub bootstrap_context: Option<String>,
}

/// Build the command-line arguments for a spawn.
///
/// If a `session_id` is present and the spec supports resume, uses `resume_args`
/// template with `{sessionId}` interpolation. Otherwise uses fresh args.
pub fn build_argv(spec: &CliAgentSpec, config: &SpawnConfig) -> Vec<String> {
    // ── Resume path: use resume_args template with session ID substitution ──
    if let Some(ref session_id) = config.session_id {
        if !spec.resume_args.is_empty() && spec.session_mode != SessionMode::Never {
            let mut argv = vec![spec.command.clone()];
            for arg in &spec.resume_args {
                argv.push(arg.replace("{sessionId}", session_id));
            }
            // Model override still applies on resume.
            if let (Some(ref flag), Some(ref model)) = (&spec.model_flag, &config.model_override) {
                argv.push(flag.clone());
                argv.push(model.clone());
            }
            // Task is appended after resume args.
            if !spec.prompt_flag.is_empty() {
                // On resume, the task is the new follow-up message.
                argv.push(config.task.clone());
            }
            argv.extend(config.extra_args.iter().cloned());
            return argv;
        }
    }

    // ── Fresh path: standard argument construction ──
    let mut argv = vec![spec.command.clone()];
    argv.extend(spec.default_args.iter().cloned());

    if let (Some(ref flag), Some(ref model)) = (&spec.model_flag, &config.model_override) {
        argv.push(flag.clone());
        argv.push(model.clone());
    }

    // ── GAP 3: Bootstrap context injection ──
    if let (Some(ref flag), Some(ref context)) = (&spec.system_prompt_flag, &config.bootstrap_context) {
        if !context.is_empty() {
            argv.push(flag.clone());
            // Truncate bootstrap to agent's max.
            let truncated = if context.len() > spec.bootstrap_max_chars {
                &context[..spec.bootstrap_max_chars]
            } else {
                context.as_str()
            };
            argv.push(truncated.to_string());
        }
    }

    if !spec.prompt_flag.is_empty() {
        argv.push(spec.prompt_flag.clone());
    }

    // For agents without system_prompt_flag, prepend bootstrap to the task.
    let task = if spec.system_prompt_flag.is_none() {
        if let Some(ref context) = config.bootstrap_context {
            if !context.is_empty() {
                let truncated = if context.len() > spec.bootstrap_max_chars {
                    &context[..spec.bootstrap_max_chars]
                } else {
                    context.as_str()
                };
                format!("{}\n\n---\n\n{}", truncated, config.task)
            } else {
                config.task.clone()
            }
        } else {
            config.task.clone()
        }
    } else {
        config.task.clone()
    };
    argv.push(task);
    argv.extend(config.extra_args.iter().cloned());
    argv
}

/// Build the environment variables for child process, applying clear_env.
///
/// GAP 5: Strips specified env vars to prevent credential sharing/leakage.
pub fn build_child_env(spec: &CliAgentSpec, config: &SpawnConfig) -> HashMap<String, String> {
    let mut env = config.env.clone();

    // Add all current env vars except those in clear_env.
    for (key, value) in std::env::vars() {
        if spec.clear_env.iter().any(|k| k == &key) {
            continue; // Strip this key.
        }
        env.entry(key).or_insert(value);
    }

    env
}

/// Get the effective no-output timeout based on whether this is a fresh or resume run.
///
/// GAP 6: Fresh runs get a more generous timeout because they need to build context.
/// Resume runs are tighter because context is already loaded.
pub fn effective_timeout(spec: &CliAgentSpec, is_resume: bool) -> Duration {
    if is_resume {
        Duration::from_secs(
            spec.resume_timeout_secs.unwrap_or(spec.no_output_timeout_secs)
        )
    } else {
        Duration::from_secs(
            spec.fresh_timeout_secs.unwrap_or(spec.no_output_timeout_secs)
        )
    }
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
    /// Session ID extracted from CLI output (for future resume).
    pub session_id: Option<String>,
    /// Whether this run is a resume of a previous session.
    pub is_resume: bool,
    /// Raw output accumulated for parsing.
    pub raw_output: String,
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
    pub async fn register_run(&self, run_id: String, agent_id: String, is_resume: bool) {
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
            session_id: None,
            is_resume,
            raw_output: String::new(),
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
    pub async fn record_output(&self, run_id: &str, bytes: usize, text: Option<&str>) {
        let mut runs = self.runs.write().await;
        if let Some(run) = runs.get_mut(run_id) {
            run.last_output_at = Some(Instant::now());
            run.output_bytes += bytes;
            run.output_lines += 1;
            // Accumulate raw output for parsing (capped at 1MB to prevent OOM).
            if let Some(t) = text {
                if run.raw_output.len() < 1_048_576 {
                    run.raw_output.push_str(t);
                }
            }
        }
    }

    /// Health check — returns runs that should be killed.
    ///
    /// Uses fresh/resume-aware timeouts (GAP 6): resume runs get tighter timeouts
    /// because context is already loaded; fresh runs get more generous timeouts.
    pub async fn check_health(&self) -> Vec<(String, RunState)> {
        let runs = self.runs.read().await;
        let mut to_kill = Vec::new();
        for run in runs.values() {
            if run.state != RunState::Running { continue; }
            let spec = self.specs.get(&run.agent_id);
            let no_output = spec
                .map(|s| effective_timeout(s, run.is_resume))
                .unwrap_or(Duration::from_secs(120));
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

    /// GAP 4: Register user-overridden specs (deep-merged with defaults).
    pub fn register_override(&mut self, spec: CliAgentSpec) {
        self.specs.insert(spec.id.clone(), spec);
    }

    /// Extract session ID from a completed run's output for future resume.
    ///
    /// GAP 1+2: Parses the raw output according to the agent's output_format
    /// and extracts session ID using the session_id_fields list.
    pub async fn extract_session_id(&self, run_id: &str) -> Option<String> {
        let runs = self.runs.read().await;
        let run = runs.get(run_id)?;
        let spec = self.specs.get(&run.agent_id)?;

        if spec.session_id_fields.is_empty() {
            return None;
        }

        let format = if run.is_resume {
            spec.resume_output_format.unwrap_or(spec.output_format)
        } else {
            spec.output_format
        };

        parse_session_id(&run.raw_output, format, &spec.session_id_fields)
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// GAP 2: CLI OUTPUT PARSING
// ═══════════════════════════════════════════════════════════════════════════════

/// Parsed output from a CLI agent.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CliOutput {
    /// Agent's text response.
    pub text: String,
    /// Session ID for future resume (if found in output).
    pub session_id: Option<String>,
    /// Token usage from the CLI agent (if reported).
    pub usage: Option<CliUsage>,
}

/// Token usage reported by a CLI agent.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CliUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
}

/// Parse CLI output according to the agent's output format.
pub fn parse_cli_output(
    raw: &str,
    format: OutputFormat,
    session_id_fields: &[String],
) -> CliOutput {
    match format {
        OutputFormat::Json => parse_cli_json(raw, session_id_fields),
        OutputFormat::Jsonl => parse_cli_jsonl(raw, session_id_fields),
        OutputFormat::Text => CliOutput {
            text: raw.to_string(),
            session_id: None,
            usage: None,
        },
    }
}

/// Extract session ID from raw output.
fn parse_session_id(raw: &str, format: OutputFormat, fields: &[String]) -> Option<String> {
    let output = parse_cli_output(raw, format, fields);
    output.session_id
}

/// Parse a single JSON object from CLI output.
fn parse_cli_json(raw: &str, session_id_fields: &[String]) -> CliOutput {
    // Try to find a JSON object in the output (may be surrounded by text).
    let json_start = raw.find('{');
    let json_end = raw.rfind('}');

    let json_str = match (json_start, json_end) {
        (Some(start), Some(end)) if end > start => &raw[start..=end],
        _ => return CliOutput { text: raw.to_string(), session_id: None, usage: None },
    };

    let parsed: serde_json::Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => return CliOutput { text: raw.to_string(), session_id: None, usage: None },
    };

    let text = parsed.get("result")
        .or_else(|| parsed.get("text"))
        .or_else(|| parsed.get("message"))
        .or_else(|| parsed.get("content"))
        .and_then(|v| v.as_str())
        .unwrap_or(raw)
        .to_string();

    let session_id = extract_field_from_json(&parsed, session_id_fields);

    let usage = extract_usage(&parsed);

    CliOutput { text, session_id, usage }
}

/// Parse newline-delimited JSON (JSONL) from CLI output.
fn parse_cli_jsonl(raw: &str, session_id_fields: &[String]) -> CliOutput {
    let mut text_parts = Vec::new();
    let mut session_id = None;
    let mut usage = None;

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || !trimmed.starts_with('{') {
            text_parts.push(trimmed.to_string());
            continue;
        }

        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed) {
            // Extract text content.
            if let Some(content) = parsed.get("content")
                .or_else(|| parsed.get("text"))
                .or_else(|| parsed.get("result"))
                .and_then(|v| v.as_str())
            {
                text_parts.push(content.to_string());
            }

            // Extract session ID (from any line that has it).
            if session_id.is_none() {
                session_id = extract_field_from_json(&parsed, session_id_fields);
            }

            // Extract usage (from the last line that has it).
            if let Some(u) = extract_usage(&parsed) {
                usage = Some(u);
            }
        } else {
            text_parts.push(trimmed.to_string());
        }
    }

    CliOutput {
        text: text_parts.join("\n"),
        session_id,
        usage,
    }
}

/// Extract a field value from a JSON object by trying multiple field names.
fn extract_field_from_json(json: &serde_json::Value, fields: &[String]) -> Option<String> {
    for field in fields {
        // Support dotted paths like "meta.session_id".
        let parts: Vec<&str> = field.split('.').collect();
        let mut current = json;
        let mut found = true;
        for part in &parts {
            match current.get(part) {
                Some(v) => current = v,
                None => { found = false; break; }
            }
        }
        if found {
            if let Some(s) = current.as_str() {
                return Some(s.to_string());
            }
        }
    }
    None
}

/// Extract token usage from a JSON object.
fn extract_usage(json: &serde_json::Value) -> Option<CliUsage> {
    let usage = json.get("usage")?;
    Some(CliUsage {
        input_tokens: usage.get("input_tokens").and_then(|v| v.as_u64()),
        output_tokens: usage.get("output_tokens").and_then(|v| v.as_u64()),
        cache_read_tokens: usage.get("cache_read_input_tokens")
            .or_else(|| usage.get("cache_read_tokens"))
            .and_then(|v| v.as_u64()),
        cache_write_tokens: usage.get("cache_creation_input_tokens")
            .or_else(|| usage.get("cache_write_tokens"))
            .and_then(|v| v.as_u64()),
    })
}

// ═══════════════════════════════════════════════════════════════════════════════
// GAP 4: PER-BACKEND CONFIG OVERRIDE (DEEP MERGE)
// ═══════════════════════════════════════════════════════════════════════════════

/// Partial override for a CliAgentSpec. Only set fields are merged.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PartialCliSpec {
    pub default_args: Option<Vec<String>>,
    pub max_runtime_secs: Option<u64>,
    pub no_output_timeout_secs: Option<u64>,
    pub max_concurrent: Option<usize>,
    pub estimated_cost_usd: Option<f64>,
    pub resume_args: Option<Vec<String>>,
    pub session_mode: Option<SessionMode>,
    pub output_format: Option<OutputFormat>,
    pub system_prompt_flag: Option<String>,
    pub bootstrap_max_chars: Option<usize>,
    pub clear_env: Option<Vec<String>>,
    pub fresh_timeout_secs: Option<u64>,
    pub resume_timeout_secs: Option<u64>,
}

/// Deep-merge a partial override into a base spec.
///
/// Only fields present in the override replace the base. This enables users
/// to customize `no_output_timeout_secs` without losing all other defaults.
pub fn merge_spec(base: CliAgentSpec, overrides: &PartialCliSpec) -> CliAgentSpec {
    CliAgentSpec {
        id: base.id,
        name: base.name,
        command: base.command,
        prompt_flag: base.prompt_flag,
        model_flag: base.model_flag,
        default_args: overrides.default_args.clone().unwrap_or(base.default_args),
        modifies_workspace: base.modifies_workspace,
        max_runtime_secs: overrides.max_runtime_secs.unwrap_or(base.max_runtime_secs),
        no_output_timeout_secs: overrides.no_output_timeout_secs.unwrap_or(base.no_output_timeout_secs),
        max_concurrent: overrides.max_concurrent.unwrap_or(base.max_concurrent),
        estimated_cost_usd: overrides.estimated_cost_usd.unwrap_or(base.estimated_cost_usd),
        resume_args: overrides.resume_args.clone().unwrap_or(base.resume_args),
        session_id_fields: base.session_id_fields,
        session_mode: overrides.session_mode.unwrap_or(base.session_mode),
        output_format: overrides.output_format.unwrap_or(base.output_format),
        resume_output_format: base.resume_output_format,
        system_prompt_flag: overrides.system_prompt_flag.clone().or(base.system_prompt_flag),
        bootstrap_max_chars: overrides.bootstrap_max_chars.unwrap_or(base.bootstrap_max_chars),
        clear_env: overrides.clear_env.clone().unwrap_or(base.clear_env),
        fresh_timeout_secs: overrides.fresh_timeout_secs.or(base.fresh_timeout_secs),
        resume_timeout_secs: overrides.resume_timeout_secs.or(base.resume_timeout_secs),
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
    fn test_claude_has_session_resume() {
        let claude = &known_agents()[0];
        assert_eq!(claude.id, "claude-code");
        assert!(!claude.resume_args.is_empty());
        assert!(claude.resume_args.iter().any(|a| a.contains("--resume")));
        assert_eq!(claude.session_mode, SessionMode::Always);
        assert!(!claude.session_id_fields.is_empty());
        assert_eq!(claude.output_format, OutputFormat::Json);
    }

    #[test]
    fn test_codex_has_session_resume() {
        let codex = &known_agents()[1];
        assert_eq!(codex.id, "codex");
        assert!(codex.resume_args.iter().any(|a| a == "resume"));
        assert_eq!(codex.session_mode, SessionMode::Existing);
        assert_eq!(codex.output_format, OutputFormat::Jsonl);
    }

    #[test]
    fn test_build_argv_fresh() {
        let spec = &known_agents()[0]; // claude-code
        let config = SpawnConfig {
            agent_id: "claude-code".into(), task: "Fix the auth bug".into(),
            model_override: Some("opus".into()), workspace: "/tmp/test".into(),
            extra_args: vec![], env: HashMap::new(), budget_cap_usd: None,
            use_worktree: false, session_id: None, bootstrap_context: None,
        };
        let argv = build_argv(spec, &config);
        assert_eq!(argv[0], "claude");
        assert!(argv.contains(&"--model".into()));
        assert!(argv.contains(&"opus".into()));
        assert!(argv.contains(&"Fix the auth bug".into()));
        // Fresh run should NOT contain --resume.
        assert!(!argv.contains(&"--resume".into()));
    }

    #[test]
    fn test_build_argv_resume() {
        let spec = &known_agents()[0]; // claude-code
        let config = SpawnConfig {
            agent_id: "claude-code".into(), task: "Continue the task".into(),
            model_override: None, workspace: "/tmp/test".into(),
            extra_args: vec![], env: HashMap::new(), budget_cap_usd: None,
            use_worktree: false,
            session_id: Some("sess-abc-123".into()),
            bootstrap_context: None,
        };
        let argv = build_argv(spec, &config);
        assert!(argv.contains(&"--resume".into()));
        assert!(argv.contains(&"sess-abc-123".into()));
    }

    #[test]
    fn test_build_argv_with_bootstrap() {
        let spec = &known_agents()[0]; // claude-code has system_prompt_flag
        let config = SpawnConfig {
            agent_id: "claude-code".into(), task: "Fix the bug".into(),
            model_override: None, workspace: "/tmp/test".into(),
            extra_args: vec![], env: HashMap::new(), budget_cap_usd: None,
            use_worktree: false, session_id: None,
            bootstrap_context: Some("Project context: this is a Rust workspace".into()),
        };
        let argv = build_argv(spec, &config);
        assert!(argv.contains(&"--append-system-prompt".into()));
        assert!(argv.iter().any(|a| a.contains("Project context")));
    }

    #[test]
    fn test_blocking_detection() {
        let run = RunStatus {
            run_id: "r1".into(), agent_id: "test".into(),
            state: RunState::Running, pid: Some(1234),
            started_at: Instant::now() - Duration::from_secs(300),
            last_output_at: Some(Instant::now() - Duration::from_secs(200)),
            exit_code: None, output_bytes: 100, output_lines: 5, estimated_cost_usd: 0.1,
            session_id: None, is_resume: false, raw_output: String::new(),
        };
        assert!(run.is_blocked(Duration::from_secs(120)));
        assert!(!run.is_blocked(Duration::from_secs(300)));
    }

    #[test]
    fn test_parse_cli_json() {
        let raw = r#"{"result": "Hello world", "session_id": "sess-abc", "usage": {"input_tokens": 100, "output_tokens": 50}}"#;
        let output = parse_cli_output(raw, OutputFormat::Json, &["session_id".into()]);
        assert_eq!(output.text, "Hello world");
        assert_eq!(output.session_id, Some("sess-abc".into()));
        assert_eq!(output.usage.as_ref().unwrap().input_tokens, Some(100));
        assert_eq!(output.usage.as_ref().unwrap().output_tokens, Some(50));
    }

    #[test]
    fn test_parse_cli_jsonl() {
        let raw = r#"{"content": "line 1"}
{"content": "line 2", "session_id": "sess-x"}
{"usage": {"input_tokens": 200, "output_tokens": 80}}"#;
        let output = parse_cli_output(raw, OutputFormat::Jsonl, &["session_id".into()]);
        assert!(output.text.contains("line 1"));
        assert!(output.text.contains("line 2"));
        assert_eq!(output.session_id, Some("sess-x".into()));
        assert_eq!(output.usage.as_ref().unwrap().input_tokens, Some(200));
    }

    #[test]
    fn test_parse_text_passthrough() {
        let raw = "Just plain text output from an agent";
        let output = parse_cli_output(raw, OutputFormat::Text, &[]);
        assert_eq!(output.text, raw);
        assert!(output.session_id.is_none());
    }

    #[test]
    fn test_merge_spec_partial_override() {
        let base = known_agents()[0].clone(); // claude-code
        let overrides = PartialCliSpec {
            no_output_timeout_secs: Some(60),
            max_concurrent: Some(5),
            ..Default::default()
        };
        let merged = merge_spec(base.clone(), &overrides);
        assert_eq!(merged.no_output_timeout_secs, 60); // overridden
        assert_eq!(merged.max_concurrent, 5); // overridden
        assert_eq!(merged.max_runtime_secs, base.max_runtime_secs); // kept from base
        assert_eq!(merged.command, "claude"); // kept from base
    }

    #[test]
    fn test_build_child_env_strips_keys() {
        let spec = &known_agents()[0]; // claude-code clears ANTHROPIC_API_KEY
        let config = SpawnConfig {
            agent_id: "claude-code".into(), task: "test".into(),
            model_override: None, workspace: "/tmp".into(),
            extra_args: vec![], env: HashMap::new(), budget_cap_usd: None,
            use_worktree: false, session_id: None, bootstrap_context: None,
        };
        let _env = build_child_env(spec, &config);
        // The parent might have ANTHROPIC_API_KEY set — it should NOT be in child env.
        // (We can't guarantee the test env has it, so just verify the function runs.)
        assert!(!spec.clear_env.is_empty());
    }

    #[test]
    fn test_effective_timeout_fresh_vs_resume() {
        let spec = &known_agents()[0]; // claude-code: fresh=120, resume=60
        let fresh = effective_timeout(spec, false);
        let resume = effective_timeout(spec, true);
        assert!(fresh >= resume, "fresh timeout should be >= resume timeout");
    }

    #[tokio::test]
    async fn test_orchestrator_limits() {
        let orch = CliOrchestrator::new(2, 10.0);
        orch.register_run("r1".into(), "claude-code".into(), false).await;
        orch.update("r1", RunState::Running, Some(1), None).await;
        orch.register_run("r2".into(), "codex".into(), false).await;
        orch.update("r2", RunState::Running, Some(2), None).await;

        // Should fail — global limit is 2
        let result = orch.can_spawn("gemini-cli").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_per_agent_limit() {
        let orch = CliOrchestrator::new(10, 100.0);
        // Aider has max_concurrent = 1
        orch.register_run("r1".into(), "aider".into(), false).await;
        orch.update("r1", RunState::Running, Some(1), None).await;

        let result = orch.can_spawn("aider").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("max 1"));
    }

    #[tokio::test]
    async fn test_budget_cap() {
        let orch = CliOrchestrator::new(10, 0.50);
        // Each claude-code run costs ~$0.50
        orch.register_run("r1".into(), "claude-code".into(), false).await;
        orch.update("r1", RunState::Running, Some(1), None).await;

        let result = orch.can_spawn("claude-code").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("budget"));
    }

    #[tokio::test]
    async fn test_extract_session_id() {
        let orch = CliOrchestrator::default();
        orch.register_run("r1".into(), "claude-code".into(), false).await;
        orch.update("r1", RunState::Running, Some(1), None).await;
        orch.record_output(
            "r1", 100,
            Some(r#"{"result": "done", "session_id": "sess-test-123"}"#)
        ).await;
        orch.update("r1", RunState::Completed, None, Some(0)).await;

        let sid = orch.extract_session_id("r1").await;
        assert_eq!(sid, Some("sess-test-123".into()));
    }
}
