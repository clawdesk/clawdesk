//! Security & audit types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Audit event categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AuditCategory {
    Authentication,
    ConfigChange,
    ToolExecution,
    FileAccess,
    MessageSend,
    MessageReceive,
    SessionLifecycle,
    PluginLifecycle,
    SecurityAlert,
    AdminAction,
}

/// A structured audit log entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub category: AuditCategory,
    pub action: String,
    pub actor: AuditActor,
    pub target: Option<String>,
    pub detail: serde_json::Value,
    pub outcome: AuditOutcome,
    /// SHA-256 hash of previous entry for tamper evidence.
    pub prev_hash: String,
}

/// Who performed the audited action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AuditActor {
    Agent { id: String },
    User { sender_id: String, channel: String },
    System,
    Plugin { name: String },
    Cron { task_id: String },
}

/// Outcome of an audited action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuditOutcome {
    Success,
    Denied,
    Failed,
    Blocked,
}

/// Scan result for skill/content security analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanResult {
    pub passed: bool,
    pub tier_reached: ScanTier,
    pub findings: Vec<ScanFinding>,
    pub scan_time_ms: u64,
}

/// Tier of the security scan cascade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScanTier {
    /// Fast regex patterns.
    Regex,
    /// AST-based code analysis.
    Ast,
    /// LLM semantic analysis.
    Semantic,
}

/// Individual finding from a security scan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanFinding {
    pub severity: Severity,
    pub rule: String,
    pub description: String,
    pub location: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Severity {
    Info,
    Low,
    Medium,
    High,
    Critical,
}

/// Content safety classification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentClassification {
    pub safe: bool,
    pub categories: Vec<ContentCategory>,
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentCategory {
    pub name: String,
    pub score: f64,
    pub flagged: bool,
}

/// Filesystem access control entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FsAcl {
    pub path: String,
    pub allowed_agents: Vec<String>,
    pub permissions: FsPermissions,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct FsPermissions {
    pub read: bool,
    pub write: bool,
    pub execute: bool,
}
