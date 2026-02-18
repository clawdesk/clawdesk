// ── TypeScript types matching Rust Serialize structs ──────────
// Every type here corresponds 1:1 to a struct in the Rust backend.
// Organized by command module.

// ══════════════════════════════════════════════════════════════
// Core (commands.rs)
// ══════════════════════════════════════════════════════════════

export interface HealthResponse {
  status: string;
  version: string;
  uptime_secs: number;
  agents_active: number;
  skills_loaded: number;
  tunnel_active: boolean;
}

export interface DesktopAgent {
  id: string;
  name: string;
  icon: string;
  color: string;
  persona: string;
  persona_hash: string;
  skills: string[];
  model: string;
  created: string;
  msg_count: number;
  status: string;
  token_budget: number;
  tokens_used: number;
  source: string;
}

export interface CreateAgentRequest {
  name: string;
  icon: string;
  color: string;
  persona: string;
  skills: string[];
  model: string;
  source?: string;
}

export interface ImportResult {
  success: boolean;
  agents: DesktopAgent[];
  warnings: string[];
  error: string | null;
}

export interface ChatMessage {
  id: string;
  role: "user" | "assistant" | "system";
  content: string;
  timestamp: string;
  metadata: ChatMessageMeta | null;
}

export interface ChatMessageMeta {
  skills_activated: string[];
  token_cost: number;
  cost_usd: number;
  model: string;
  duration_ms: number;
  identity_verified: boolean;
  tools_used: ToolUsageSummary[];
  compaction: CompactionInfo | null;
}

export interface ToolUsageSummary {
  name: string;
  success: boolean;
  duration_ms: number;
}

export interface CompactionInfo {
  level: string;
  tokens_before: number;
  tokens_after: number;
}

export interface SendMessageRequest {
  agent_id: string;
  content: string;
}

export interface SendMessageResponse {
  message: ChatMessage;
  trace: TraceEntry[];
}

export interface SessionSummary {
  agent_id: string;
  title: string;
  last_activity: string;
  message_count: number;
  pending_approvals: number;
  routine_generated: boolean;
  has_proof_outputs: boolean;
}

export interface SkillDescriptor {
  id: string;
  name: string;
  description: string;
  category: string;
  estimated_tokens: number;
  state: string;
  verified: boolean;
  icon: string;
}

export interface PipelineDescriptor {
  id: string;
  name: string;
  description: string;
  steps: PipelineNodeDescriptor[];
  edges: [number, number][];
  created: string;
}

export interface PipelineNodeDescriptor {
  label: string;
  node_type: "agent" | "gate" | "input" | "output" | "parallel";
  model: string | null;
  agent_id: string | null;
  x: number;
  y: number;
}

export interface CostMetrics {
  today_cost: number;
  today_input_tokens: number;
  today_output_tokens: number;
  model_breakdown: ModelCostEntry[];
}

export interface ModelCostEntry {
  model: string;
  input_tokens: number;
  output_tokens: number;
  cost: number;
}

export interface SecurityStatus {
  gateway_bind: string;
  tunnel_active: boolean;
  tunnel_endpoint: string;
  auth_mode: string;
  scoped_tokens: boolean;
  identity_contracts: number;
  skill_scanning: string;
  rate_limiter: string;
  mdns_disabled: boolean;
  scanner_patterns: number;
  audit_entries: number;
}

export interface TraceEntry {
  timestamp: string;
  event: string;
  detail: string;
}

export interface ModelInfo {
  id: string;
  name: string;
  cost_per_m_input: string;
  speed: string;
  use_case: string;
  context_window: number;
}

export interface ChannelInfo {
  id: string;
  name: string;
  status: string;
  channel_type: string;
}

export interface InviteResponse {
  invite_code: string;
  qr_text: string;
  expires_at: number;
  label: string;
}

export interface TunnelMetricsSnapshot {
  total_peers: number;
  active_peers: number;
  total_bytes_sent: number;
  total_bytes_received: number;
  total_handshakes: number;
  uptime_secs: number;
  peers: PeerMetricsSnapshot[];
}

export interface PeerMetricsSnapshot {
  peer_id: string;
  bytes_sent: number;
  bytes_received: number;
  rtt_ms: number;
  handshakes: number;
  last_handshake_secs_ago: number;
}

// ══════════════════════════════════════════════════════════════
// Runtime (commands_runtime.rs)
// ══════════════════════════════════════════════════════════════

export interface DurableRunInfo {
  run_id: string;
  state: string;
  worker_id: string;
}

export interface DurableRunStatus {
  run_id: string;
  state: string;
  checkpoint_count: number;
  journal_entries: number;
}

// ══════════════════════════════════════════════════════════════
// Media (commands_media.rs)
// ══════════════════════════════════════════════════════════════

export interface LinkPreviewResult {
  url: string;
  title: string | null;
  description: string | null;
  image: string | null;
  site_name: string | null;
}

export interface MediaPipelineStatus {
  processor_count: number;
  processors: string[];
}

// ══════════════════════════════════════════════════════════════
// Plugins (commands_plugin.rs)
// ══════════════════════════════════════════════════════════════

export interface PluginSummary {
  id: string;
  name: string;
  version: string;
  description: string;
  state: string;
}

// ══════════════════════════════════════════════════════════════
// A2A Protocol (commands_a2a.rs)
// ══════════════════════════════════════════════════════════════

export interface AgentCardInfo {
  id: string;
  name: string;
  capabilities: string[];
  active_tasks: number;
  is_healthy: boolean;
}

export interface RegisterAgentCardRequest {
  agent_id: string;
  name?: string;
  description?: string;
  capabilities: string[];
  endpoint?: string;
}

// ══════════════════════════════════════════════════════════════
// Security (commands_security.rs)
// ══════════════════════════════════════════════════════════════

export interface OAuthStartRequest {
  provider: string;
  client_id: string;
  client_secret?: string;
  auth_url: string;
  token_url: string;
  redirect_uri: string;
  scopes: string[];
  use_pkce: boolean;
}

export interface OAuthStartResponse {
  auth_url: string;
  state_param: string;
}

export interface OAuthCallbackRequest {
  code: string;
  state_param: string;
  provider: string;
  client_id: string;
  client_secret?: string;
  auth_url: string;
  token_url: string;
  redirect_uri: string;
  scopes: string[];
  use_pkce: boolean;
}

export interface OAuthTokenResponse {
  access_token_preview: string;
  has_refresh_token: boolean;
  expires_at: string | null;
  scope: string | null;
}

export interface AuthProfileInfo {
  id: string;
  provider: string;
  is_expired: boolean;
  failure_count: number;
  created_at: string;
  last_used: string | null;
}

export interface CreateApprovalRequest {
  tool_name: string;
  command: string;
  risk_level: string;
  context?: string;
}

export interface ApprovalRequestInfo {
  id: string;
  tool_name: string;
  command: string;
  risk: string;
  status: string;
  created_at: string;
  expires_at: string;
}

export interface AclRuleRequest {
  principal_type: string;
  principal_id: string;
  resource_type: string;
  resource_id: string;
  action: string;
  effect: string;
}

export interface AclCheckResult {
  decision: string;
  reason: string | null;
}

export interface GenerateTokenRequest {
  scopes: string[];
  ttl_hours: number;
  peer_id?: string;
}

export interface TokenInfo {
  encoded: string;
  scopes: string[];
  expires_in_secs: number;
  is_peer_bound: boolean;
}

// ══════════════════════════════════════════════════════════════
// Discovery (commands_discovery.rs)
// ══════════════════════════════════════════════════════════════

export interface PeerInfo {
  instance_name: string;
  host: string;
  port: number;
  version: string;
  capabilities: string[];
}

export interface PairingStatus {
  code: string;
  state: string;
  remaining_secs: number;
}

// ══════════════════════════════════════════════════════════════
// Observability (commands_observability.rs)
// ══════════════════════════════════════════════════════════════

export interface ObservabilityStatus {
  enabled: boolean;
  service_name: string;
  endpoint: string;
  environment: string;
  version: string;
  api_key_set: boolean;
  project: string | null;
}

export interface ConfigureObservabilityRequest {
  enabled?: boolean;
  endpoint?: string;
  service_name?: string;
  environment?: string;
  api_key?: string;
  project?: string;
}

// ══════════════════════════════════════════════════════════════
// Infra (commands_infra.rs)
// ══════════════════════════════════════════════════════════════

export interface NotificationInfo {
  id: string;
  title: string;
  body: string;
  priority: string;
  created_at: string;
}

export interface SendNotificationRequest {
  title: string;
  body: string;
  priority?: string;
  group_id?: string;
}

export interface ClipboardEntryInfo {
  id: string;
  content_type: string;
  text: string | null;
  byte_size: number;
  timestamp: string;
}

export interface VoiceWakeStatus {
  enabled: boolean;
  wake_phrases: string[];
  target_agent: string;
  listening: boolean;
}

export interface VoiceWakeConfigRequest {
  enabled: boolean;
  wake_phrases: string[];
  target_agent?: string;
  silence_timeout_secs?: number;
}

export interface IdleStatus {
  is_idle: boolean;
  idle_duration_secs: number;
}

// ══════════════════════════════════════════════════════════════
// Domain (commands_domain.rs)
// ══════════════════════════════════════════════════════════════

export interface ContextGuardStatus {
  current_tokens: number;
  available_budget: number;
  utilization: number;
  context_limit: number;
  trigger_threshold: number;
}

export interface PromptManifestInfo {
  total_tokens: number;
  budget_total: number;
  budget_utilization: number;
  sections: PromptSectionInfo[];
  skills_included: string[];
  skills_excluded: [string, string][];
  memory_fragments: number;
}

export interface PromptSectionInfo {
  name: string;
  tokens: number;
  included: boolean;
  reason: string;
}

export interface ProviderCapabilityInfo {
  provider: string;
  capabilities: string[];
  models: string[];
}

export interface RoutingDecisionInfo {
  selected_provider: string | null;
  selected_model: string | null;
  reason: string;
}

export interface SkillTrustInfo {
  skill_id: string;
  trust_level: string;
  publisher_key: string | null;
  verified: boolean;
  error: string | null;
}

export interface SkillTriggerInfo {
  skill_id: string;
  trigger_type: string;
  matched: boolean;
  relevance: number;
}

// ══════════════════════════════════════════════════════════════
// Canvas (commands_canvas.rs)
// ══════════════════════════════════════════════════════════════

export interface CanvasSummary {
  id: string;
  title: string;
  block_count: number;
  connection_count: number;
  created_at: string;
  updated_at: string;
}

export interface BlockInfo {
  id: string;
  block_type: string;
  content: string;
  x: number;
  y: number;
  language: string | null;
  editable: boolean;
  pinned: boolean;
  tags: string[];
}

export interface CreateCanvasRequest {
  title: string;
}

export interface AddBlockRequest {
  canvas_id: string;
  block_type: string;
  content: string;
  x: number;
  y: number;
  language?: string;
}

export interface ConnectBlocksRequest {
  canvas_id: string;
  from_block: string;
  to_block: string;
  label?: string;
}

// ══════════════════════════════════════════════════════════════
// Memory (commands_memory.rs)
// ══════════════════════════════════════════════════════════════

export interface RememberRequest {
  content: string;
  source?: string;
  metadata?: Record<string, unknown>;
}

export interface RememberResponse {
  id: string;
  content_length: number;
}

export interface RememberBatchItem {
  content: string;
  source?: string;
  metadata?: Record<string, unknown>;
}

export interface RecallRequest {
  query: string;
  max_results?: number;
}

export interface MemoryHit {
  id: string;
  score: number;
  content: string | null;
  source: string | null;
  timestamp: string | null;
}

export interface MemoryStatsResponse {
  collection_name: string;
  embedding_provider: string;
  search_strategy: string;
  min_relevance: number;
  max_results: number;
}

// ══════════════════════════════════════════════════════════════
// SochDB Advanced (commands_sochdb.rs)
// ══════════════════════════════════════════════════════════════

export interface GraphNodeInfo {
  id: string;
  node_type: string;
  properties: Record<string, unknown>;
}

export interface GraphEdgeInfo {
  from_id: string;
  edge_type: string;
  to_id: string;
  properties: Record<string, unknown>;
}

export interface TraceRunInfo {
  trace_id: string;
  name: string;
  start_time: number;
  end_time: number | null;
  status: string;
  total_tokens: number;
  cost_millicents: number;
}

export interface TraceSpanInfo {
  trace_id: string;
  span_id: string;
  parent_span_id: string | null;
  name: string;
  kind: string;
  start_time: number;
  end_time: number | null;
  duration_us: number | null;
}

export interface CacheLookupInfo {
  hit: boolean;
  match_type: string;
  result: string | null;
  latency_us: number;
}

export interface CheckpointInfo {
  run_id: string;
  node_id: string;
  seq: number;
  timestamp: number;
  state_size: number;
}

export interface WorkflowRunInfo {
  run_id: string;
  workflow: string;
  status: string;
  created_at: number;
  updated_at: number;
  latest_checkpoint_seq: number;
  latest_event_seq: number;
}

export interface AtomicWriteInfo {
  memory_id: string;
  ops_applied: number;
  status: string;
}

export interface AgentRegistryInfo {
  agent_id: string;
  capabilities: string[];
  status: string;
}

export interface SubgraphInfo {
  nodes: GraphNodeInfo[];
  edges: GraphEdgeInfo[];
}

export interface TemporalEdgeInfo {
  from_id: string;
  edge_type: string;
  to_id: string;
  valid_start: number;
  valid_end: number | null;
  properties: Record<string, unknown>;
  version: number;
}

// ══════════════════════════════════════════════════════════════
// Agent Templates
// ══════════════════════════════════════════════════════════════

export interface AgentTemplate {
  name: string;
  icon: string;
  color: string;
  persona: string;
  skills: string[];
  model: string;
  description: string;
}

export const AGENT_TEMPLATES: AgentTemplate[] = [
  {
    name: "Research Assistant",
    icon: "🔬",
    color: "#6366f1",
    persona:
      "You are a thorough research assistant. Search the web, read papers, extract key findings, and cite your sources. Always provide structured summaries.",
    skills: ["web-search", "citations", "markdown"],
    model: "sonnet",
    description: "Deep research with citations and structured output",
  },
  {
    name: "Code Builder",
    icon: "⚡",
    color: "#10b981",
    persona:
      "You are a senior software engineer. Write clean, tested code. Use best practices, proper error handling, and clear documentation. Run tests.",
    skills: ["code-exec", "files", "git"],
    model: "sonnet",
    description: "Write, test, and ship production code",
  },
  {
    name: "Content Writer",
    icon: "📝",
    color: "#f59e0b",
    persona:
      "You are a professional writer. Create engaging content — articles, reports, documentation. Maintain consistent tone and structure.",
    skills: ["web-search", "markdown", "files"],
    model: "opus",
    description: "Professional writing with research backing",
  },
  {
    name: "Daily Coordinator",
    icon: "🎯",
    color: "#ec4899",
    persona:
      "You coordinate daily tasks. Check emails, summarize updates, schedule reminders. Be concise and actionable.",
    skills: ["email", "calendar", "cron", "alerts"],
    model: "haiku",
    description: "Lightweight coordinator for daily workflows (cheapest)",
  },
];

// ══════════════════════════════════════════════════════════════
// Additional types from integration map (real.js)
// ══════════════════════════════════════════════════════════════

export interface AuditEntry {
  timestamp: string;
  category: string;
  event: string;
  actor: string;
  detail: string;
  outcome: string;
}

export interface ToolCallEntry {
  tool_name: string;
  args_preview: string;
  result_preview: string;
}

export interface RuntimeStatus {
  active_runs: number;
  completed_runs: number;
  failed_runs: number;
}

export interface CacheEntry {
  key: string;
  value: string;
  source?: string;
  ttl_secs?: number;
}

export interface CreatePipelineRequest {
  name: string;
  description: string;
  steps: PipelineNodeDescriptor[];
  edges: [number, number][];
}

export interface PipelineRunResult {
  pipeline_id: string;
  pipeline_name: string;
  success: boolean;
  steps: PipelineStepResult[];
  total_duration_ms: number;
}

export interface PipelineStepResult {
  step_index: number;
  label: string;
  node_type: string;
  success: boolean;
  duration_ms: number;
  output_preview?: string;
  error?: string;
}

// ── Typed event payloads ───────────────────────────────────

export type AgentEventPayload =
  | { type: "StreamChunk"; text: string }
  | { type: "ToolStart"; tool: string; args_preview: string }
  | { type: "ToolEnd"; tool: string; success: boolean; duration_ms: number }
  | { type: "RoundComplete"; round: number; input_tokens: number; output_tokens: number }
  | { type: "Finished"; total_tokens: number; cost_usd: number }
  | { type: "Error"; message: string };

export interface AgentEventEnvelope {
  agent_id: string;
  event: AgentEventPayload;
}

export interface IncomingMessageEvent {
  channel: string;
  sender: string;
  content: string;
  timestamp: string;
}

export interface ApprovalPendingEvent {
  request_id: string;
  agent_id: string;
  action: string;
  detail: string;
}

// ── View Identifiers ─────────────────────────────────────────

export type ViewId =
  | "home"
  | "agents"
  | "skills"
  | "flows"
  | "chat"
  | "monitor";
