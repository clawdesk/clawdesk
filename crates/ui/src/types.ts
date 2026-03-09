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
  /** Whether the storage backend is using durable (on-disk) persistence. */
  storage_healthy: boolean;
  /** Human-readable storage path (for diagnostics). */
  storage_path: string;
}

// ── Durable Runtime ───────────────────────────────────────────
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

// ── A2A Protocol ──────────────────────────────────────────────
export interface TaskSendRequest {
  skill_id?: string;
  input: any;
  target_agent?: string;
  required_capabilities?: string[];
}

export interface A2ATaskResponse {
  task_id: string;
  state: string;
  output?: any;
  error?: string;
  progress: number;
  artifacts: any[];
}

// ── Debug / Storage Diagnostics ───────────────────────────────
export interface DebugEvent {
  ts: number;
  category: string;
  action: string;
  detail: string;
  level: "info" | "warn" | "error";
}

export interface SessionMismatch {
  chat_id: string;
  memory_msg_count: number;
  sochdb_msg_count: number;
}

export interface SessionDetail {
  chat_id: string;
  agent_id: string;
  title: string;
  message_count: number;
  created_at: string;
  updated_at: string;
  in_sochdb: boolean;
  in_memory: boolean;
  serialized_size: number;
}

export interface StorageSnapshot {
  is_ephemeral: boolean;
  storage_path: string;
  memory_session_count: number;
  sochdb_session_count: number;
  memory_only_sessions: string[];
  sochdb_only_sessions: string[];
  message_count_mismatches: SessionMismatch[];
  memory_agent_count: number;
  sochdb_agent_count: number;
  wal_size_bytes: number;
  wal_exists: boolean;
  old_format_session_count: number;
  roundtrip_test: string;
  session_details: SessionDetail[];
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
  /** Channels this agent is assigned to (e.g. ["telegram","discord"]). Empty = any/all. */
  channels?: string[];
  /** If this agent belongs to a team, the shared team identifier. */
  team_id?: string;
  /** Role within the team (e.g. "router", "researcher", "developer"). */
  team_role?: string;
}

export interface CreateAgentRequest {
  name: string;
  icon: string;
  color: string;
  persona: string;
  skills: string[];
  model: string;
  source?: string;
  channels?: string[];
  team_id?: string;
  team_role?: string;
}

export interface UpdateAgentRequest {
  name?: string;
  icon?: string;
  color?: string;
  persona?: string;
  skills?: string[];
  model?: string;
  channels?: string[];
  team_id?: string;
  team_role?: string;
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
  model_override?: string;
  chat_id?: string;
}

export interface SendMessageResponse {
  message: ChatMessage;
  trace: TraceEntry[];
  chat_id: string;
  chat_title?: string;
}

export interface SessionSummary {
  chat_id: string;
  agent_id: string;
  title: string;
  created_at: string;
  last_activity: string;
  message_count: number;
  pending_approvals: number;
  routine_generated: boolean;
  has_proof_outputs: boolean;
  first_message_preview: string | null;
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
  schedule?: string | null;
}

export interface PipelineNodeDescriptor {
  label: string;
  node_type: "agent" | "gate" | "input" | "output" | "parallel";
  model: string | null;
  agent_id: string | null;
  x: number;
  y: number;
  condition?: string | null;
  config?: Record<string, string>;
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
  status: string;          // "active" | "available" | "error" | "disconnected"
  channel_type: string;    // "telegram" | "discord" | "slack" | "whatsapp" etc.
  configured?: boolean;
  config?: Record<string, string>;
  capabilities?: string[];  // "direct" | "group" | "media" | "threads" | "reactions"
  last_error?: string;
  docs_url?: string;
}

export type ChannelConfigField = {
  key: string;
  label: string;
  type: "text" | "password" | "url" | "select" | "toggle";
  placeholder?: string;
  help?: string;
  required?: boolean;
  options?: string[];      // for select type
};

export type ChannelTypeSpec = {
  id: string;
  label: string;
  icon: string;
  blurb: string;
  docs_url: string;
  configFields: ChannelConfigField[];
  capabilities: string[];
};

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

export interface SkillDetail {
  id: string;
  name: string;
  description: string;
  version: string;
  category: string;
  instructions: string;
  tags: string[];
  required_tools: string[];
  estimated_tokens: number;
  state: string;
  source: string;
  author: string | null;
}

export interface RegisterSkillRequest {
  name: string;
  description: string;
  version: string;
  category: string;
  instructions: string;
  tags: string[];
  allowed_tools: string[];
  /** When editing an existing skill, pass its original ID to update in-place. */
  existing_id?: string;
}

export interface SkillValidationResult {
  valid: boolean;
  errors: string[];
  warnings: string[];
  estimated_tokens: number;
  parsed_name: string | null;
  parsed_description: string | null;
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
  // ── General ──
  {
    name: "Research Assistant",
    icon: "🔍",
    color: "#6366f1",
    persona: "You are a thorough research assistant. Search the web, read papers, extract key findings, and cite your sources. Always provide structured summaries.",
    skills: ["web-search", "citations", "markdown"],
    model: "default",
    description: "Find information and summarize it clearly",
  },
  {
    name: "Writing Helper",
    icon: "✍️",
    color: "#10b981",
    persona: "You are a professional writer. Create engaging content — articles, reports, emails, documentation. Maintain consistent tone and structure.",
    skills: ["web-search", "markdown", "files"],
    model: "default",
    description: "Draft emails, documents, and creative content",
  },
  {
    name: "Daily Planner",
    icon: "📅",
    color: "#f59e0b",
    persona: "You help plan and organize daily tasks. Summarize what's important, set priorities, suggest time blocks, and keep track of to-dos.",
    skills: ["email", "calendar", "cron", "alerts"],
    model: "default",
    description: "Organize your day, tasks, and priorities",
  },
  {
    name: "Problem Solver",
    icon: "💡",
    color: "#ec4899",
    persona: "You help think through problems step by step. Break down complex questions, brainstorm options, weigh pros and cons, and suggest solutions.",
    skills: ["web-search", "markdown"],
    model: "default",
    description: "Think through decisions and brainstorm ideas",
  },
  // ── Engineering ──
  {
    name: "Senior Developer",
    icon: "👨‍💻",
    color: "#10b981",
    persona: "You are a premium implementation specialist mastering multiple frameworks, advanced CSS, and modern web technologies. Write production-quality code with clean architecture.",
    skills: ["code-exec", "files", "web-search"],
    model: "default",
    description: "Full-stack code implementation and architecture",
  },
  {
    name: "Frontend Developer",
    icon: "🖥️",
    color: "#06b6d4",
    persona: "You are an expert frontend developer specializing in modern web technologies, React/Vue/Angular frameworks, UI implementation, and performance optimization.",
    skills: ["code-exec", "files", "web-search"],
    model: "default",
    description: "Modern web UI development and frameworks",
  },
  {
    name: "Backend Architect",
    icon: "🏗️",
    color: "#3b82f6",
    persona: "You are a senior backend architect specializing in scalable system design, database architecture, API development, and distributed systems.",
    skills: ["code-exec", "files", "web-search"],
    model: "default",
    description: "Scalable backend systems and API design",
  },
  {
    name: "AI Engineer",
    icon: "🤖",
    color: "#3b82f6",
    persona: "You are an expert AI/ML engineer specializing in machine learning model development, deployment, and integration into production systems.",
    skills: ["code-exec", "files", "web-search"],
    model: "default",
    description: "ML models, data pipelines, and AI integration",
  },
  {
    name: "DevOps Automator",
    icon: "⚙️",
    color: "#f97316",
    persona: "You are an expert DevOps engineer specializing in infrastructure automation, CI/CD pipeline development, and cloud platform management.",
    skills: ["code-exec", "files", "web-search"],
    model: "default",
    description: "CI/CD, infrastructure, and cloud automation",
  },
  {
    name: "Security Engineer",
    icon: "🔒",
    color: "#ef4444",
    persona: "You are an expert application security engineer specializing in threat modeling, vulnerability assessment, and secure code review.",
    skills: ["code-exec", "files", "web-search"],
    model: "default",
    description: "Security audits, threat modeling, and hardening",
  },
  {
    name: "Mobile App Builder",
    icon: "📱",
    color: "#8b5cf6",
    persona: "You are a specialized mobile application developer with expertise in native iOS/Android development and cross-platform frameworks.",
    skills: ["code-exec", "files", "web-search"],
    model: "default",
    description: "iOS, Android, and cross-platform mobile apps",
  },
  {
    name: "Rapid Prototyper",
    icon: "🚀",
    color: "#10b981",
    persona: "You are specialized in ultra-fast proof-of-concept development and MVP creation using efficient tools and frameworks.",
    skills: ["code-exec", "files", "web-search"],
    model: "default",
    description: "Fast prototyping and MVP development",
  },
  // ── Design ──
  {
    name: "UX Architect",
    icon: "📐",
    color: "#8b5cf6",
    persona: "You are a technical architecture and UX specialist who creates solid foundations for developers. You bridge the gap between project specifications and implementation.",
    skills: ["web-search", "markdown", "files"],
    model: "default",
    description: "CSS systems, layout frameworks, and UX structure",
  },
  {
    name: "UI Designer",
    icon: "🎨",
    color: "#8b5cf6",
    persona: "You are an expert UI designer specializing in visual design systems, component libraries, and pixel-perfect interfaces.",
    skills: ["web-search", "markdown", "files"],
    model: "default",
    description: "Visual design systems and component libraries",
  },
  {
    name: "UX Researcher",
    icon: "🔬",
    color: "#10b981",
    persona: "You are an expert user experience researcher specializing in user behavior analysis, usability testing, and data-driven design decisions.",
    skills: ["web-search", "markdown"],
    model: "default",
    description: "User research, usability testing, and insights",
  },
  {
    name: "Brand Guardian",
    icon: "🛡️",
    color: "#3b82f6",
    persona: "You are an expert brand strategist and guardian specializing in brand identity development and consistency maintenance.",
    skills: ["web-search", "markdown"],
    model: "default",
    description: "Brand identity, consistency, and guidelines",
  },
  {
    name: "Visual Storyteller",
    icon: "🎬",
    color: "#8b5cf6",
    persona: "You are an expert visual communication specialist focused on creating compelling visual narratives and multimedia content.",
    skills: ["web-search", "markdown"],
    model: "default",
    description: "Visual narratives and multimedia content",
  },
  // ── Marketing ──
  {
    name: "Content Creator",
    icon: "📝",
    color: "#14b8a6",
    persona: "You are an expert content strategist and creator for multi-platform campaigns. Develop editorial calendars, create compelling copy, and optimize content for engagement.",
    skills: ["web-search", "markdown", "files"],
    model: "default",
    description: "Multi-platform content strategy and creation",
  },
  {
    name: "Growth Hacker",
    icon: "📈",
    color: "#10b981",
    persona: "You are an expert growth strategist specializing in rapid user acquisition through data-driven experimentation.",
    skills: ["web-search", "markdown"],
    model: "default",
    description: "Data-driven growth and user acquisition",
  },
  {
    name: "Social Media Strategist",
    icon: "📣",
    color: "#3b82f6",
    persona: "You are an expert social media strategist for LinkedIn, Twitter, and professional platforms. Create cross-platform content strategies.",
    skills: ["web-search", "markdown"],
    model: "default",
    description: "Cross-platform social media strategy",
  },
  // ── Product ──
  {
    name: "Feedback Synthesizer",
    icon: "📊",
    color: "#3b82f6",
    persona: "You are an expert in collecting, analyzing, and synthesizing user feedback from multiple channels to extract actionable insights.",
    skills: ["web-search", "markdown"],
    model: "default",
    description: "User feedback analysis and actionable insights",
  },
  {
    name: "Sprint Prioritizer",
    icon: "🎯",
    color: "#10b981",
    persona: "You are an expert product manager specializing in agile sprint planning, feature prioritization, and resource allocation.",
    skills: ["web-search", "markdown"],
    model: "default",
    description: "Sprint planning and feature prioritization",
  },
  {
    name: "Trend Researcher",
    icon: "🔮",
    color: "#8b5cf6",
    persona: "You are an expert market intelligence analyst specializing in identifying emerging trends and competitive analysis.",
    skills: ["web-search", "markdown"],
    model: "default",
    description: "Market trends and competitive intelligence",
  },
  // ── Project Management ──
  {
    name: "Senior Project Manager",
    icon: "📋",
    color: "#3b82f6",
    persona: "You convert specs to tasks and remember previous projects. Focused on realistic scope and actionable project plans.",
    skills: ["web-search", "markdown", "cron"],
    model: "default",
    description: "Task planning, timelines, and project tracking",
  },
  {
    name: "Studio Producer",
    icon: "🎬",
    color: "#eab308",
    persona: "You are a senior strategic leader specializing in high-level creative and technical project orchestration and resource management.",
    skills: ["web-search", "markdown", "cron"],
    model: "default",
    description: "Creative project orchestration and leadership",
  },
  // ── Testing & QA ──
  {
    name: "API Tester",
    icon: "🧪",
    color: "#8b5cf6",
    persona: "You are an expert API testing specialist focused on comprehensive API validation, performance testing, and quality assurance.",
    skills: ["code-exec", "web-search", "markdown"],
    model: "default",
    description: "API validation, performance, and quality",
  },
  {
    name: "Accessibility Auditor",
    icon: "♿",
    color: "#0077B6",
    persona: "You are an expert accessibility specialist who audits interfaces against WCAG standards and tests with assistive technologies.",
    skills: ["web-search", "markdown", "code-exec"],
    model: "default",
    description: "WCAG compliance and accessibility testing",
  },
  {
    name: "Performance Benchmarker",
    icon: "⚡",
    color: "#f97316",
    persona: "You are an expert performance testing and optimization specialist focused on measuring, analyzing, and improving system performance.",
    skills: ["code-exec", "web-search", "markdown"],
    model: "default",
    description: "Performance testing and optimization",
  },
  // ── Support ──
  {
    name: "Support Responder",
    icon: "💬",
    color: "#3b82f6",
    persona: "You are an expert customer support specialist delivering exceptional customer service, issue resolution, and user guidance.",
    skills: ["web-search", "markdown"],
    model: "default",
    description: "Customer support and issue resolution",
  },
  {
    name: "Legal Compliance Checker",
    icon: "⚖️",
    color: "#ef4444",
    persona: "You are an expert legal and compliance specialist ensuring business operations, data handling, and content creation meet regulatory requirements.",
    skills: ["web-search", "markdown"],
    model: "default",
    description: "Legal compliance and regulatory checks",
  },
  {
    name: "Finance Tracker",
    icon: "💰",
    color: "#10b981",
    persona: "You are an expert financial analyst and controller specializing in financial planning, budget management, and business analytics.",
    skills: ["web-search", "markdown"],
    model: "default",
    description: "Financial planning and budget analysis",
  },
  // ── Specialized ──
  {
    name: "Agents Orchestrator",
    icon: "🎭",
    color: "#06b6d4",
    persona: "You are an autonomous pipeline manager that orchestrates the entire development workflow. You are the leader of this process.",
    skills: ["web-search", "code-exec", "files", "markdown"],
    model: "default",
    description: "Orchestrate multi-agent development workflows",
  },
  {
    name: "Data Analytics Reporter",
    icon: "📉",
    color: "#6366f1",
    persona: "You are an expert data analyst transforming raw data into actionable business insights. Create dashboards and performance reports.",
    skills: ["code-exec", "web-search", "markdown"],
    model: "default",
    description: "Data analysis, dashboards, and reporting",
  },
  {
    name: "Executive Summary Generator",
    icon: "📄",
    color: "#8b5cf6",
    persona: "You are a consultant-grade AI specialist trained to think and communicate like a senior strategy consultant. Produce clear, actionable executive briefings.",
    skills: ["web-search", "markdown"],
    model: "default",
    description: "Strategy consulting and executive briefings",
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

// Typed shape returned by get_runtime_status backend command
export interface RuntimeStatusInfo {
  durable_runner_available: boolean;
  worker_id: string;
  checkpoint_store: string;
  journal: string;
  lease_manager: string;
}

// Checkpoint record from list_checkpoints
export interface CheckpointEntry {
  run_id: string;
  step_index: number;
  state_snapshot: string;
  created_at: string;
}

// Dead Letter Queue entry from get_dlq
export interface DlqEntry {
  id: string;
  run_id: string;
  error: string;
  failed_at: string;
  retry_count: number;
  payload: string;
}

// Pipeline execution step event for live overlay
export interface PipelineStepEvent {
  pipeline_id: string;
  step_index: number;
  status: "started" | "completed" | "failed";
  timestamp: string;
  output_preview?: string;
  error?: string;
}

// A2A full agent card (detailed shape from get_agent_card / get_self_agent_card)
export interface A2AFullAgentCard {
  id: string;
  name: string;
  description: string;
  url: string;
  capabilities: string[];
  version: string;
  provider: string;
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
// These MUST match TauriAgentEvent in state.rs (serde tag = "type")

export type AgentEventPayload =
  | { type: "StreamChunk"; text: string; done: boolean }
  | { type: "ThinkingChunk"; text: string }
  | { type: "ToolStart"; name: string; args: string }
  | { type: "ToolEnd"; name: string; success: boolean; duration_ms: number }
  | { type: "RoundStart"; round: number }
  | { type: "Response"; content: string; finish_reason: string }
  | { type: "Done"; total_rounds: number }
  | { type: "Error"; error: string }
  | { type: "Compaction"; level: string; tokens_before: number; tokens_after: number }
  | { type: "PromptAssembled"; total_tokens: number; skills_included: string[]; skills_excluded: string[]; memory_fragments: number; budget_utilization: number }
  | { type: "IdentityVerified"; hash_match: boolean; version: number }
  | { type: "ContextGuardAction"; action: string; token_count: number; threshold: number }
  | { type: "FallbackTriggered"; from_model: string; to_model: string; reason: string }
  | { type: "RetryStatus"; attempt: number; max_attempts: number; reason: string }
  | { type: "ToolBlocked"; name: string; reason: string }
  | { type: "ToolExecutionResult"; name: string; tool_call_id: string; is_error: boolean; preview: string; duration_ms: number }
  | { type: "SkillDecision"; skill_id: string; included: boolean; reason: string; token_cost: number; budget_remaining: number }
  | { type: "InputRequired"; question: string; options: string[]; urgent: boolean };

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

export interface AskHumanPendingEvent {
  id: string;
  question: string;
  options: string[];
  urgent: boolean;
  sent_to_channels?: string[];
}

/** Emitted when a channel inbound message resolves a pending ask-human. */
export interface AskHumanRespondedEvent {
  id: string;
  response: string;
  via_channel: string;
}

/** Entry returned by list_workspace_files Tauri command. */
export interface WorkspaceFileEntry {
  name: string;
  path: string;
  is_dir: boolean;
  size: number;
  modified: string;
}

// ── View Identifiers ─────────────────────────────────────────

export type ViewId =
  | "home"
  | "agents"
  | "skills"
  | "flows"
  | "chat"
  | "monitor";

// ══════════════════════════════════════════════════════════════
// Sandbox (commands_sandbox.rs)
// ══════════════════════════════════════════════════════════════

export interface SandboxStatusInfo {
  available: boolean;
  max_isolation: string;
  available_levels: string[];
  default_limits: ResourceLimitsInfo;
}

export interface ResourceLimitsInfo {
  cpu_time_secs: number;
  wall_time_secs: number;
  memory_bytes: number;
  max_fds: number;
  max_output_bytes: number;
  max_processes: number;
}

export interface SandboxExecResult {
  exit_code: number;
  stdout: string;
  stderr: string;
  duration_ms: number;
  resource_usage: SandboxResourceUsage;
}

export interface SandboxResourceUsage {
  cpu_time_ms: number;
  wall_time_ms: number;
  peak_memory_bytes: number;
  output_bytes: number;
}

export interface SandboxBackendInfo {
  name: string;
  isolation_level: string;
  available: boolean;
}

// ══════════════════════════════════════════════════════════════
// MCP — Model Context Protocol (commands_mcp.rs)
// ══════════════════════════════════════════════════════════════

export interface McpServerInfo {
  name: string;
  transport: string;
  connected: boolean;
  tool_count: number;
}

export interface McpToolInfo {
  name: string;
  description: string;
  input_schema: any;
  server: string;
}

export interface McpToolCallResult {
  content: McpContentItem[];
  is_error: boolean;
}

export interface McpContentItem {
  content_type: string;
  text?: string;
  data?: string;
  mime_type?: string;
}

export interface McpBundledTemplate {
  name: string;
  category: string;
  description: string;
}

export interface McpConnectRequest {
  name: string;
  transport: string;
  command?: string;
  args?: string[];
  url?: string;
  env?: Record<string, string>;
}

// ══════════════════════════════════════════════════════════════
// Extensions (commands_extensions.rs)
// ══════════════════════════════════════════════════════════════

export interface IntegrationInfo {
  name: string;
  description: string;
  category: string;
  icon: string;
  enabled: boolean;
  credentials_required: CredentialRequirementInfo[];
  has_oauth: boolean;
  health_check_url?: string;
  /** Per-extension configuration schema (typed fields). */
  config_fields: ConfigFieldInfo[];
  /** Current user-configured values (non-secret only). */
  config_values: Record<string, string>;
  /** Transport type: "stdio" | "sse" | "api" */
  transport_type: string;
}

export interface CredentialRequirementInfo {
  name: string;
  description: string;
  env_var?: string;
  required: boolean;
}

export interface ConfigFieldInfo {
  key: string;
  label: string;
  description: string;
  field_type: string; // "text" | "number" | "boolean" | "secret" | "select" | "url" | "filepath" | "port"
  default?: string;
  required: boolean;
  placeholder?: string;
  validation?: string;
  options: ConfigFieldOptionInfo[];
  group?: string;
}

export interface ConfigFieldOptionInfo {
  label: string;
  value: string;
}

export interface IntegrationCategoryInfo {
  name: string;
  count: number;
}

export interface VaultStatusInfo {
  exists: boolean;
  unlocked: boolean;
  credential_count: number;
}

export interface HealthStatusInfo {
  name: string;
  state: string;
  last_check?: string;
  last_success?: string;
  consecutive_failures: number;
  latency_ms?: number;
}

export interface OAuthFlowInfo {
  auth_url: string;
  state: string;
}

export interface IntegrationStatsInfo {
  total: number;
  enabled: number;
  disabled: number;
}

// ══════════════════════════════════════════════════════════════
// Migration (commands_migrate.rs)
// ══════════════════════════════════════════════════════════════

export interface MigrationSourceInfo {
  name: string;
  label: string;
  supported_items: string[];
}

export interface MigrationReportInfo {
  source: string;
  source_path: string;
  dry_run: boolean;
  success: boolean;
  summary: MigrationSummaryInfo;
  items: MigrationItemInfo[];
  warnings: string[];
  errors: string[];
}

export interface MigrationSummaryInfo {
  total: number;
  migrated: number;
  skipped: number;
  failed: number;
  dry_run: number;
}

export interface MigrationItemInfo {
  category: string;
  source_name: string;
  dest_path: string;
  status: string;
  note: string;
}

export interface MigrationRequest {
  source: string;
  source_path: string;
  dest_path?: string;
  dry_run: boolean;
  overwrite: boolean;
  include?: string[];
}

export interface ValidateSourceResult {
  valid: boolean;
  source: string;
  found_items: string[];
  error?: string;
}

// ══════════════════════════════════════════════════════════════
// Agent Coordination / Orchestration
// ══════════════════════════════════════════════════════════════

/** Adapter types for agent coordination. */
export type AgentAdapterType =
  | "claude_local"
  | "codex_local"
  | "cursor"
  | "opencode_local"
  | "process"
  | "http"
  | "a2a_gateway";

/** An agent flow defines how a coordinated agent runs. */
export interface AgentFlowConfig {
  id: string;
  name: string;
  adapter_type: AgentAdapterType;
  description: string;
  /** Provider + model used (e.g. "anthropic:claude-sonnet-4") */
  model: string;
  /** Role in the team ("router", "researcher", "developer", "reviewer") */
  role: string;
  /** Adapter-specific configuration */
  adapter_config: Record<string, string>;
  /** Heartbeat interval in seconds (0 = manual only) */
  heartbeat_interval_sec: number;
  /** Maximum concurrent runs for this agent */
  max_concurrent_runs: number;
  /** Working directory for process-based adapters */
  cwd?: string;
  /** Icon identifier */
  icon: string;
  /** Color hex */
  color: string;
  /** Whether this flow is currently active */
  active: boolean;
}

/** Orchestration task to be dispatched to an agent flow. */
export interface OrchestrationTask {
  id: string;
  title: string;
  description: string;
  /** Agent flow ID this task is assigned to */
  assigned_flow_id: string | null;
  /** Parent task ID for delegation hierarchy */
  parent_task_id: string | null;
  status: "todo" | "in_progress" | "blocked" | "done" | "failed";
  priority: "low" | "medium" | "high" | "urgent";
  created_at: string;
  updated_at: string;
  output?: string;
}

/** Event emitted during orchestration execution. */
export interface OrchestrationEvent {
  type:
    | "dag_created"
    | "task_started"
    | "task_completed"
    | "task_failed"
    | "agent_spawned"
    | "agent_heartbeat"
    | "delegation"
    | "escalated"
    | "finished";
  task_id?: string;
  flow_id?: string;
  detail: string;
  timestamp: string;
}

/** Result of running an orchestration. */
export interface OrchestrationResultFrontend {
  status: string;
  outputs: Record<string, unknown>;
  duration_ms: number;
  rewrite_count: number;
  total_nodes: number;
  completed_nodes: number;
  failed_nodes: number;
}

/** Capability info from the backend. */
export interface CapabilityInfo {
  action: string;
  skill_count: number;
  skills: string[];
}

/** Input to start an orchestration run. */
export interface OrchestrationRunRequest {
  /** Natural language goal or structured task plan */
  goal: string;
  /** Specific flow IDs to use (empty = auto-select) */
  flow_ids: string[];
  /** Strategy: "single" sends to one agent, "fan_out" parallelizes, "pipeline" chains */
  strategy: "single" | "fan_out" | "pipeline" | "auto";
}

/** Predefined flow templates for quick setup. */
export interface FlowTemplate {
  id: string;
  name: string;
  description: string;
  adapter_type: AgentAdapterType;
  icon: string;
  color: string;
  default_config: Record<string, string>;
}
