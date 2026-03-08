// ── Typed wrappers around Tauri invoke() ──────────────────
//
// Each function maps 1:1 to a #[tauri::command] in the Rust backend.
// All return types match the Rust Serialize output exactly.
// 136 commands across 13 modules.

import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import type {
  // Core
  HealthResponse,
  DesktopAgent,
  CreateAgentRequest,
  UpdateAgentRequest,
  ImportResult,
  SendMessageResponse,
  SessionSummary,
  ChatMessage,
  SkillDescriptor,
  PipelineDescriptor,
  PipelineNodeDescriptor,
  PipelineRunResult,
  CostMetrics,
  SecurityStatus,
  TraceEntry,
  TunnelMetricsSnapshot,
  InviteResponse,
  ModelInfo,
  ChannelInfo,
  ChannelTypeSpec,
  // Runtime
  DurableRunInfo,
  DurableRunStatus,
  RuntimeStatusInfo,
  CheckpointEntry,
  DlqEntry,
  // Media
  LinkPreviewResult,
  MediaPipelineStatus,
  // Plugins
  PluginSummary,
  // A2A
  AgentCardInfo,
  RegisterAgentCardRequest,
  TaskSendRequest,
  A2ATaskResponse,
  A2AFullAgentCard,
  // Security
  OAuthStartRequest,
  OAuthStartResponse,
  OAuthCallbackRequest,
  OAuthTokenResponse,
  AuthProfileInfo,
  CreateApprovalRequest,
  ApprovalRequestInfo,
  AclRuleRequest,
  AclCheckResult,
  GenerateTokenRequest,
  TokenInfo,
  // Discovery
  PeerInfo,
  PairingStatus,
  // Observability
  ObservabilityStatus,
  ConfigureObservabilityRequest,
  // Infra
  NotificationInfo,
  SendNotificationRequest,
  ClipboardEntryInfo,
  VoiceWakeStatus,
  VoiceWakeConfigRequest,
  IdleStatus,
  // Domain
  ContextGuardStatus,
  PromptManifestInfo,
  ProviderCapabilityInfo,
  RoutingDecisionInfo,
  SkillTrustInfo,
  SkillTriggerInfo,
  SkillDetail,
  RegisterSkillRequest,
  SkillValidationResult,
  // Canvas
  CanvasSummary,
  BlockInfo,
  CreateCanvasRequest,
  AddBlockRequest,
  ConnectBlocksRequest,
  // Memory
  RememberRequest,
  RememberResponse,
  RememberBatchItem,
  RecallRequest,
  MemoryHit,
  MemoryStatsResponse,
  // SochDB
  GraphNodeInfo,
  GraphEdgeInfo,
  TraceRunInfo,
  TraceSpanInfo,
  CacheLookupInfo,
  CheckpointInfo,
  WorkflowRunInfo,
  AtomicWriteInfo,
  AgentRegistryInfo,
  SubgraphInfo,
  TemporalEdgeInfo,
  // Debug
  StorageSnapshot,
  // Sandbox
  SandboxStatusInfo,
  SandboxExecResult,
  SandboxBackendInfo,
  ResourceLimitsInfo,
  SandboxResourceUsage,
  // MCP
  McpServerInfo,
  McpToolInfo,
  McpToolCallResult,
  McpBundledTemplate,
  McpConnectRequest,
  // Extensions
  IntegrationInfo,
  IntegrationCategoryInfo,
  VaultStatusInfo,
  HealthStatusInfo,
  OAuthFlowInfo,
  IntegrationStatsInfo,
  CredentialRequirementInfo,
  // Migration
  MigrationSourceInfo,
  MigrationReportInfo,
  MigrationRequest,
  ValidateSourceResult,
} from "./types";

// ── Browser-dev mode detection ────────────────────────────
const isBrowserDev = typeof window !== "undefined" && !(window as any).__TAURI_INTERNALS__;

// ── Clear stale mock data when running in Tauri (production) mode ──
if (!isBrowserDev && typeof window !== "undefined") {
  ["clawdesk._mockSkills", "clawdesk._mockChannels", "clawdesk._mockAgents"].forEach((key) => {
    localStorage.removeItem(key);
  });
}

// ── Local pipeline store for browser-dev mode ─────────────
let _localPipelines: PipelineDescriptor[] = [];

// ── Channel type specifications ───────────────────────────
import type { ChannelConfigField } from "./types";

const _channelTypeSpecs: ChannelTypeSpec[] = [
  { id: "WebChat", label: "Web Chat", icon: "💬", blurb: "Built-in web chat interface", docs_url: "", configFields: [], capabilities: ["direct", "media", "threads"] },
  { id: "Internal", label: "Internal", icon: "🏠", blurb: "Internal system messages", docs_url: "", configFields: [], capabilities: ["direct", "group"] },
  {
    id: "Telegram", label: "Telegram", icon: "✈️", blurb: "Telegram Bot API for direct and group chats", docs_url: "https://core.telegram.org/bots",
    configFields: [
      { key: "bot_token", label: "Bot Token", type: "password", placeholder: "123456:ABC-DEF...", help: "Token from @BotFather", required: true },
      { key: "allowed_users", label: "Allowed Users", type: "text", placeholder: "123456789, 987654321 or *", help: "Comma-separated Telegram user IDs, or * for everyone. Empty = deny all" },
      { key: "mention_only", label: "Mention Only", type: "select", options: ["false", "true"], help: "Only respond to @mentions in groups" },
      { key: "webhook_url", label: "Webhook URL", type: "url", placeholder: "https://your-domain.com/webhook/telegram", help: "Public URL for incoming updates (leave blank for polling)" },
    ], capabilities: ["direct", "group", "media", "threads", "reactions"]
  },
  {
    id: "Discord", label: "Discord", icon: "🎮", blurb: "Discord bot for server and DM messaging", docs_url: "https://discord.com/developers/docs",
    configFields: [
      { key: "bot_token", label: "Bot Token", type: "password", placeholder: "MTk....", help: "Discord bot token from Developer Portal", required: true },
      { key: "application_id", label: "Application ID", type: "text", placeholder: "123456789012345678", help: "Discord application ID (from Developer Portal → General Information)", required: true },
      { key: "guild_id", label: "Server (Guild) ID", type: "text", placeholder: "123456789012345678", help: "Restrict to a single server. Leave blank for all servers" },
      { key: "allowed_users", label: "Allowed Users", type: "text", placeholder: "123456789012345678 or *", help: "Comma-separated Discord user IDs, or * for everyone. Empty = deny all" },
      { key: "mention_only", label: "Mention Only", type: "select", options: ["false", "true"], help: "Only respond to @mentions in channels" },
    ], capabilities: ["direct", "group", "media", "threads", "reactions"]
  },
  {
    id: "Slack", label: "Slack", icon: "💼", blurb: "Slack workspace integration via Bot + App tokens", docs_url: "https://api.slack.com/",
    configFields: [
      { key: "bot_token", label: "Bot Token", type: "password", placeholder: "xoxb-...", help: "Bot User OAuth Token from Install App page", required: true },
      { key: "app_token", label: "App Token", type: "password", placeholder: "xapp-...", help: "App-level token for Socket Mode (Settings → Basic Information → App-Level Tokens)", required: true },
      { key: "channel_id", label: "Channel ID", type: "text", placeholder: "C01ABCDEFGH", help: "Restrict to a single channel. Leave blank for all" },
      { key: "allowed_users", label: "Allowed Users", type: "text", placeholder: "U01ABCDEFGH or *", help: "Comma-separated Slack user IDs, or * for everyone. Empty = deny all" },
    ], capabilities: ["direct", "group", "media", "threads", "reactions"]
  },
  {
    id: "WhatsApp", label: "WhatsApp", icon: "📱", blurb: "WhatsApp Business API or Web bridge", docs_url: "https://developers.facebook.com/docs/whatsapp",
    configFields: [
      { key: "phone_number_id", label: "Phone Number ID", type: "text", placeholder: "", help: "WhatsApp Business phone number ID", required: true },
      { key: "access_token", label: "Access Token", type: "password", placeholder: "EAAG...", help: "Meta Graph API access token", required: true },
      { key: "verify_token", label: "Verify Token", type: "text", placeholder: "", help: "Webhook verification token" },
      { key: "app_secret", label: "App Secret", type: "password", placeholder: "", help: "Meta app secret for webhook signature verification" },
      { key: "allowed_numbers", label: "Allowed Numbers", type: "text", placeholder: "+1234567890 or *", help: "Comma-separated E.164 phone numbers, or * for everyone. Empty = deny all" },
    ], capabilities: ["direct", "group", "media"]
  },
  {
    id: "Email", label: "Email", icon: "📧", blurb: "IMAP/SMTP email integration with IDLE push", docs_url: "",
    configFields: [
      { key: "imap_host", label: "IMAP Host", type: "text", placeholder: "imap.gmail.com", help: "IMAP server hostname", required: true },
      { key: "imap_port", label: "IMAP Port", type: "text", placeholder: "993", help: "IMAP TLS port (default: 993)" },
      { key: "smtp_host", label: "SMTP Host", type: "text", placeholder: "smtp.gmail.com", help: "SMTP server hostname", required: true },
      { key: "smtp_port", label: "SMTP Port", type: "text", placeholder: "465", help: "SMTP TLS port (default: 465)" },
      { key: "email", label: "Email Address", type: "text", placeholder: "bot@example.com", help: "Login and from address", required: true },
      { key: "password", label: "Password", type: "password", placeholder: "", help: "Email account password or app password", required: true },
      { key: "imap_folder", label: "IMAP Folder", type: "text", placeholder: "INBOX", help: "Folder to monitor (default: INBOX)" },
      { key: "allowed_senders", label: "Allowed Senders", type: "text", placeholder: "user@example.com, *@company.com or *", help: "Comma-separated emails/domains, or * for everyone. Empty = deny all" },
    ], capabilities: ["direct", "media"]
  },
  {
    id: "IMessage", label: "iMessage", icon: "🍏", blurb: "macOS iMessage via AppleScript bridge", docs_url: "",
    configFields: [
      { key: "allowed_contacts", label: "Allowed Contacts", type: "text", placeholder: "+1234567890, user@example.com", help: "Comma-separated phone/email. Leave blank for everyone (*)" },
      { key: "poll_interval_secs", label: "Poll Interval", type: "text", placeholder: "3", help: "How often to check for new messages (seconds)" },
    ], capabilities: ["direct"]
  },
  {
    id: "Irc", label: "IRC", icon: "💻", blurb: "IRC over TLS with SASL/NickServ auth", docs_url: "https://ircv3.net/",
    configFields: [
      { key: "server", label: "Server", type: "text", placeholder: "irc.libera.chat", help: "IRC server hostname", required: true },
      { key: "nickname", label: "Nickname", type: "text", placeholder: "clawdesk-bot", help: "Bot nickname", required: true },
      { key: "port", label: "Port", type: "text", placeholder: "6697", help: "Server port (default: 6697 TLS)" },
      { key: "channels", label: "Channels", type: "text", placeholder: "#general, #dev", help: "Comma-separated channels to join" },
      { key: "sasl_password", label: "SASL Password", type: "password", placeholder: "", help: "SASL PLAIN authentication password" },
      { key: "nickserv_password", label: "NickServ Password", type: "password", placeholder: "", help: "NickServ IDENTIFY password" },
    ], capabilities: ["direct", "group"]
  },
];

function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!isBrowserDev) return tauriInvoke<T>(cmd, args);

  // In browser-dev mode, mock pipeline operations locally
  if (cmd === "list_pipelines") {
    return Promise.resolve(_localPipelines as unknown as T);
  }
  if (cmd === "create_pipeline") {
    const req = (args as any)?.request;
    const pipeline: PipelineDescriptor = {
      id: `pipe_${Date.now().toString(36)}`,
      name: req?.name ?? "Untitled",
      description: req?.description ?? "",
      steps: req?.steps ?? [],
      edges: req?.edges ?? [],
      created: new Date().toISOString(),
    };
    _localPipelines = [..._localPipelines, pipeline];
    return Promise.resolve(pipeline as unknown as T);
  }
  if (cmd === "run_pipeline") {
    return Promise.resolve({ status: "ok", message: "Pipeline run simulated (browser-dev mode)" } as unknown as T);
  }
  if (cmd === "list_agents") {
    const stored = localStorage.getItem("clawdesk._mockAgents");
    return Promise.resolve((stored ? JSON.parse(stored) : []) as unknown as T);
  }
  if (cmd === "create_agent") {
    const req = (args as any)?.request;
    const agent: DesktopAgent = {
      id: `agent_${Date.now().toString(36)}`,
      name: req?.name ?? "Agent",
      icon: req?.icon ?? "🤖",
      color: req?.color ?? "#f06a30",
      persona: req?.persona ?? req?.system_prompt ?? "",
      persona_hash: "mock",
      skills: req?.skills ?? [],
      model: req?.model ?? "default",
      created: new Date().toISOString(),
      msg_count: 0,
      status: "active",
      token_budget: 128000,
      tokens_used: 0,
      source: "mock",
    };
    const stored = localStorage.getItem("clawdesk._mockAgents");
    const agents = stored ? JSON.parse(stored) : [];
    agents.push(agent);
    localStorage.setItem("clawdesk._mockAgents", JSON.stringify(agents));
    return Promise.resolve(agent as unknown as T);
  }
  if (cmd === "get_health") {
    return Promise.resolve({ status: "connected", version: "0.1.0", uptime_secs: 2280 } as unknown as T);
  }
  if (cmd === "list_skills") {
    const stored = localStorage.getItem("clawdesk._mockSkills");
    if (stored) return Promise.resolve(JSON.parse(stored) as unknown as T);
    const defaults: SkillDescriptor[] = [
      { id: "email-compose", name: "Email Compose", description: "Draft professional emails", category: "communication", estimated_tokens: 49, state: "active", verified: true, icon: "📧" },
      { id: "calendar-awareness", name: "Calendar Awareness", description: "Time and date reasoning, scheduling help", category: "general", estimated_tokens: 45, state: "active", verified: true, icon: "📅" },
      { id: "memory-recall", name: "Memory Recall", description: "Remember and recall information from past conversations", category: "general", estimated_tokens: 51, state: "active", verified: true, icon: "🧠" },
      { id: "system-diagnostics", name: "System Diagnostics", description: "Check system health and configuration", category: "devops", estimated_tokens: 40, state: "active", verified: true, icon: "🔧" },
      { id: "image-description", name: "Image Description", description: "Describe and analyze images", category: "general", estimated_tokens: 50, state: "active", verified: true, icon: "🖼️" },
      { id: "code-analysis", name: "Code Analysis", description: "Analyze code for bugs, patterns, and improvements", category: "coding", estimated_tokens: 68, state: "active", verified: true, icon: "⚡" },
      { id: "web-search", name: "Web Search", description: "Search the web for current information", category: "research", estimated_tokens: 55, state: "active", verified: true, icon: "🔍" },
      { id: "file-manager", name: "File Manager", description: "Read, write, and manage local files", category: "general", estimated_tokens: 42, state: "active", verified: true, icon: "📄" },
      { id: "shell-exec", name: "Shell Execute", description: "Run shell commands and scripts", category: "devops", estimated_tokens: 38, state: "active", verified: true, icon: "🛠️" },
      { id: "data-viz", name: "Data Visualization", description: "Create charts and data visualizations", category: "data", estimated_tokens: 62, state: "active", verified: true, icon: "📊" },
      { id: "translate", name: "Translation", description: "Translate text between languages", category: "writing", estimated_tokens: 35, state: "active", verified: true, icon: "🌐" },
      { id: "summarize", name: "Summarize", description: "Summarize long documents and articles", category: "writing", estimated_tokens: 44, state: "active", verified: true, icon: "📝" },
      { id: "json-tools", name: "JSON Tools", description: "Parse, format, and transform JSON data", category: "data", estimated_tokens: 30, state: "active", verified: true, icon: "🔣" },
      { id: "git-ops", name: "Git Operations", description: "Manage git repositories and version control", category: "devops", estimated_tokens: 56, state: "active", verified: true, icon: "🔀" },
      { id: "api-testing", name: "API Testing", description: "Test and debug HTTP API endpoints", category: "devops", estimated_tokens: 47, state: "active", verified: true, icon: "🧪" },
      // Recommended (not installed)
      { id: "weather", name: "Weather", description: "Get current weather and forecasts for any location", category: "general", estimated_tokens: 28, state: "available", verified: true, icon: "🌤️" },
      { id: "pdf-reader", name: "PDF Reader", description: "Extract text and data from PDF documents", category: "data", estimated_tokens: 54, state: "available", verified: true, icon: "📑" },
      { id: "sql-query", name: "SQL Query", description: "Query and analyze databases with natural language", category: "data", estimated_tokens: 60, state: "available", verified: true, icon: "🗃️" },
    ];
    localStorage.setItem("clawdesk._mockSkills", JSON.stringify(defaults));
    return Promise.resolve(defaults as unknown as T);
  }
  if (cmd === "activate_skill") {
    const skillId = (args as any)?.skillId;
    const stored = localStorage.getItem("clawdesk._mockSkills");
    if (stored) {
      const skills = JSON.parse(stored);
      const idx = skills.findIndex((s: any) => s.id === skillId);
      if (idx >= 0) { skills[idx].state = "active"; localStorage.setItem("clawdesk._mockSkills", JSON.stringify(skills)); }
    }
    return Promise.resolve(true as unknown as T);
  }
  if (cmd === "deactivate_skill") {
    const skillId = (args as any)?.skillId;
    const stored = localStorage.getItem("clawdesk._mockSkills");
    if (stored) {
      const skills = JSON.parse(stored);
      const idx = skills.findIndex((s: any) => s.id === skillId);
      if (idx >= 0) { skills[idx].state = "available"; localStorage.setItem("clawdesk._mockSkills", JSON.stringify(skills)); }
    }
    return Promise.resolve(true as unknown as T);
  }
  if (cmd === "get_skill_detail") {
    const skillId = (args as any)?.skillId;
    const stored = localStorage.getItem("clawdesk._mockSkills");
    const skills = stored ? JSON.parse(stored) : [];
    const s = skills.find((sk: any) => sk.id === skillId);
    return Promise.resolve({
      id: skillId, name: s?.name ?? skillId, description: s?.description ?? "",
      version: "1.0.0", category: s?.category ?? "general",
      instructions: `You are a skill named ${s?.name ?? skillId}. ${s?.description ?? ""}`,
      tags: [s?.category ?? "general"], required_tools: [], estimated_tokens: s?.estimated_tokens ?? 40,
      state: s?.state ?? "active", source: "builtin", author: null,
    } as unknown as T);
  }
  if (cmd === "get_skill_trust_level") {
    const skillId = (args as any)?.skillId;
    return Promise.resolve({
      skill_id: skillId, trust_level: "verified", publisher_key: "clawdesk-core", verified: true, error: null,
    } as unknown as T);
  }
  if (cmd === "evaluate_skill_triggers") {
    const text = ((args as any)?.messageText ?? "").toLowerCase();
    const matches: any[] = [];
    const triggerMap: Record<string, string[]> = {
      "email-compose": ["email", "draft", "compose", "send mail"],
      "calendar-awareness": ["schedule", "calendar", "date", "time", "meeting"],
      "web-search": ["search", "look up", "find online", "google"],
      "code-analysis": ["code", "bug", "analyze", "review"],
      "weather": ["weather", "forecast", "temperature"],
      "translate": ["translate", "language"],
      "summarize": ["summarize", "tldr", "summary"],
    };
    for (const [sid, keywords] of Object.entries(triggerMap)) {
      const match = keywords.some((k) => text.includes(k));
      if (match) matches.push({ skill_id: sid, trigger_type: "keyword", matched: true, relevance: 0.85 + Math.random() * 0.15 });
    }
    return Promise.resolve(matches as unknown as T);
  }
  if (cmd === "register_skill") {
    const req = (args as any)?.request;
    const skill: SkillDescriptor = {
      id: `custom_${Date.now().toString(36)}`, name: req?.name ?? "Custom Skill",
      description: req?.description ?? "", category: req?.category ?? "general",
      estimated_tokens: 50, state: "active", verified: false, icon: "⚡",
    };
    const stored = localStorage.getItem("clawdesk._mockSkills");
    const skills = stored ? JSON.parse(stored) : [];
    skills.push(skill);
    localStorage.setItem("clawdesk._mockSkills", JSON.stringify(skills));
    return Promise.resolve(skill as unknown as T);
  }
  if (cmd === "validate_skill_md") {
    const result = { valid: true, errors: [], warnings: [], estimated_tokens: 100, parsed_name: "mock-skill", parsed_description: "Mock validation" };
    return Promise.resolve(result as unknown as T);
  }
  if (cmd === "list_channels") {
    const stored = localStorage.getItem("clawdesk._mockChannels");
    if (stored) return Promise.resolve(JSON.parse(stored) as unknown as T);
    const defaults: ChannelInfo[] = [
      { id: "web-chat", name: "Web Chat", channel_type: "WebChat", status: "active", configured: true, config: {}, capabilities: ["direct", "media", "threads"], docs_url: "" },
      { id: "internal", name: "Internal", channel_type: "Internal", status: "active", configured: true, config: {}, capabilities: ["direct", "group"], docs_url: "" },
      { id: "telegram", name: "Telegram", channel_type: "Telegram", status: "available", configured: false, config: {}, capabilities: ["direct", "group", "media", "threads", "reactions"], docs_url: "https://core.telegram.org/bots" },
      { id: "discord", name: "Discord", channel_type: "Discord", status: "available", configured: false, config: {}, capabilities: ["direct", "group", "media", "threads", "reactions"], docs_url: "https://discord.com/developers/docs" },
      { id: "slack", name: "Slack", channel_type: "Slack", status: "available", configured: false, config: {}, capabilities: ["direct", "group", "media", "threads", "reactions"], docs_url: "https://api.slack.com/" },
      { id: "whatsapp", name: "WhatsApp", channel_type: "WhatsApp", status: "available", configured: false, config: {}, capabilities: ["direct", "group", "media"], docs_url: "https://developers.facebook.com/docs/whatsapp" },
      { id: "email", name: "Email", channel_type: "Email", status: "available", configured: false, config: {}, capabilities: ["direct", "media"], docs_url: "" },
      { id: "imessage", name: "iMessage", channel_type: "IMessage", status: "available", configured: false, config: {}, capabilities: ["direct"], docs_url: "" },
      { id: "irc", name: "IRC", channel_type: "Irc", status: "available", configured: false, config: {}, capabilities: ["direct", "group"], docs_url: "https://ircv3.net/" },
    ];
    localStorage.setItem("clawdesk._mockChannels", JSON.stringify(defaults));
    return Promise.resolve(defaults as unknown as T);
  }
  if (cmd === "update_channel") {
    const { channelId, config } = (args as any) ?? {};
    const stored = localStorage.getItem("clawdesk._mockChannels");
    if (stored) {
      const channels = JSON.parse(stored);
      const idx = channels.findIndex((c: any) => c.id === channelId);
      if (idx >= 0) {
        channels[idx].config = { ...channels[idx].config, ...config };
        channels[idx].configured = true;
        channels[idx].status = "active";
        localStorage.setItem("clawdesk._mockChannels", JSON.stringify(channels));
      }
    }
    return Promise.resolve(true as unknown as T);
  }
  if (cmd === "disconnect_channel") {
    const { channelId } = (args as any) ?? {};
    const stored = localStorage.getItem("clawdesk._mockChannels");
    if (stored) {
      const channels = JSON.parse(stored);
      const idx = channels.findIndex((c: any) => c.id === channelId);
      if (idx >= 0) {
        channels[idx].status = "available";
        channels[idx].configured = false;
        channels[idx].config = {};
        localStorage.setItem("clawdesk._mockChannels", JSON.stringify(channels));
      }
    }
    return Promise.resolve(true as unknown as T);
  }
  if (cmd === "get_channel_types") {
    return Promise.resolve(_channelTypeSpecs as unknown as T);
  }
  if (cmd === "get_tunnel_status") {
    return Promise.resolve({
      active_peers: 0,
      total_bytes_received: 0,
      total_bytes_sent: 0,
      uptime_secs: 0,
    } as unknown as T);
  }
  if (cmd === "get_media_pipeline_status") {
    return Promise.resolve({
      processor_count: 0,
      processors: [],
      queue_depth: 0,
    } as unknown as T);
  }
  if (cmd === "get_context_guard_status") {
    return Promise.resolve({
      current_tokens: 0,
      context_limit: 128000,
      utilization: 0,
      trigger_threshold: 0.8,
      compaction_count: 0,
      cache_hit_rate: 0,
    } as unknown as T);
  }
  if (cmd === "get_security_status") {
    return Promise.resolve({
      gateway_bind: "127.0.0.1:3579",
      tunnel_active: false,
      tunnel_endpoint: "",
      auth_mode: "local",
      scoped_tokens: false,
      identity_contracts: 0,
      scanner_patterns: 42,
      skill_scanning: "on-install",
      audit_entries: 0,
      rate_limiter: "token-bucket",
      mdns_disabled: true,
    } as unknown as T);
  }
  if (cmd === "get_cost_metrics") {
    return Promise.resolve({
      today_cost: 0,
      today_input_tokens: 0,
      today_output_tokens: 0,
      model_breakdown: [],
    } as unknown as T);
  }
  if (cmd === "get_observability_status") {
    return Promise.resolve({
      enabled: false,
      service_name: "clawdesk",
      endpoint: "",
      environment: "desktop",
    } as unknown as T);
  }
  if (cmd === "list_plugins") {
    return Promise.resolve([] as unknown as T);
  }
  if (cmd === "list_peers") {
    return Promise.resolve([] as unknown as T);
  }
  if (cmd === "list_auth_profiles") {
    return Promise.resolve([] as unknown as T);
  }
  if (cmd === "list_provider_capabilities") {
    const provider = localStorage.getItem("clawdesk.provider") || "Ollama (Local)";
    const model = localStorage.getItem("clawdesk.model") || "claude-sonnet-4-20250514";
    return Promise.resolve([
      { provider, models: [model], capabilities: ["chat", "function_calling", "vision"] },
    ] as unknown as T);
  }
  if (cmd === "sochdb_checkpoint") {
    return Promise.resolve(0 as unknown as T);
  }
  if (cmd === "sochdb_sync" || cmd === "policy_enable_audit") {
    return Promise.resolve({} as T);
  }
  if (cmd === "start_pairing") {
    return Promise.resolve({
      code: `${Math.random().toString(36).slice(2, 6).toUpperCase()}-${Math.random().toString(36).slice(2, 6).toUpperCase()}`,
      state: "waiting",
      remaining_secs: 300,
    } as unknown as T);
  }
  if (cmd === "list_sessions") {
    return Promise.resolve([
      { chat_id: "chat_1", agent_id: "agent_default", title: "Getting started", last_activity: new Date().toISOString(), message_count: 3, pending_approvals: 0, routine_generated: false, has_proof_outputs: false },
      { chat_id: "chat_2", agent_id: "agent_code", title: "Code review session", last_activity: new Date(Date.now() - 3600000).toISOString(), message_count: 12, pending_approvals: 0, routine_generated: true, has_proof_outputs: true },
    ] as unknown as T);
  }
  if (cmd === "policy_get_audit_log") {
    return Promise.resolve([] as unknown as T);
  }
  if (cmd === "get_audit_logs") {
    const subs = ["gateway", "agent", "skill", "channel", "system"];
    const actions = ["Request processed", "Agent initialized", "Skill loaded", "Channel connected", "Config updated", "Health check passed"];
    const limit = (args as any)?.limit ?? 20;
    const entries = Array.from({ length: Math.min(limit, 30) }, (_, i) => ({
      id: `log_${Date.now()}_${i}`,
      timestamp: new Date(Date.now() - i * 5000).toISOString(),
      level: ["info", "info", "info", "warn", "error"][Math.floor(Math.random() * 5)],
      subsystem: subs[Math.floor(Math.random() * subs.length)],
      message: actions[Math.floor(Math.random() * actions.length)],
      category: "General",
      actor: "system",
      outcome: "Success",
    }));
    return Promise.resolve(entries as unknown as T);
  }
  if (cmd === "send_message") {
    const text = (args as any)?.request?.content || "";
    let content = "I'm running in browser-dev mode. Connect a real backend with `cargo tauri dev` for full functionality.";

    if (text.toLowerCase().includes("snake in python")) {
      content = "Here is a simple Python Snake game framework:\n\n```python\n" +
        "import pygame\nimport time\nimport random\n\n".repeat(15) +
        "```\n\nHave fun extending it!\n\n".repeat(5);
    }

    return Promise.resolve({
      message: {
        id: `m_${Date.now()}`,
        role: "assistant",
        content,
        timestamp: new Date().toISOString(),
        metadata: {
          skills_activated: [],
          token_cost: 0,
          cost_usd: 0,
          model: "mock-model",
          duration_ms: 150,
          identity_verified: true,
          tools_used: [],
          compaction: null
        }
      },
      trace: [],
      chat_id: (args as any)?.request?.chat_id || `mock_chat_${Date.now()}`,
      chat_title: "Mock conversation",
    } as unknown as T);
  }
  if (cmd === "cancel_active_run") {
    return Promise.resolve(true as unknown as T);
  }
  if (cmd === "delete_agent") {
    const agentId = (args as any)?.agentId;
    const stored = localStorage.getItem("clawdesk._mockAgents");
    if (stored) {
      const agents = JSON.parse(stored).filter((a: any) => a.id !== agentId);
      localStorage.setItem("clawdesk._mockAgents", JSON.stringify(agents));
    }
    return Promise.resolve(true as unknown as T);
  }
  if (cmd === "get_chat_messages") {
    return Promise.resolve([] as unknown as T);
  }
  if (cmd === "create_chat") {
    const agentId = (args as any)?.agentId || "unknown";
    return Promise.resolve({
      chat_id: `mock_chat_${Date.now()}`,
      agent_id: agentId,
      title: "New chat",
      last_activity: new Date().toISOString(),
      message_count: 0,
      pending_approvals: 0,
      routine_generated: false,
      has_proof_outputs: false,
    } as unknown as T);
  }
  if (cmd === "delete_chat") {
    return Promise.resolve(true as unknown as T);
  }
  if (cmd === "clear_all_chats") {
    return Promise.resolve(0 as unknown as T);
  }
  if (cmd === "update_chat_title") {
    return Promise.resolve(true as unknown as T);
  }

  // Default: resolve with empty/stub value
  console.warn(`[browser-dev] unhandled invoke: ${cmd}`, args);
  return Promise.resolve({} as T);
}

// ══════════════════════════════════════════════════════════════
// Health (1)
// ══════════════════════════════════════════════════════════════

export async function getHealth(): Promise<HealthResponse> {
  return invoke<HealthResponse>("get_health");
}

// ══════════════════════════════════════════════════════════════
// Agents (3)
// ══════════════════════════════════════════════════════════════

export async function createAgent(req: CreateAgentRequest): Promise<DesktopAgent> {
  return invoke<DesktopAgent>("create_agent", { request: req });
}

export async function listAgents(): Promise<DesktopAgent[]> {
  return invoke<DesktopAgent[]>("list_agents");
}

export async function updateAgent(agentId: string, request: UpdateAgentRequest): Promise<DesktopAgent> {
  return invoke<DesktopAgent>("update_agent", { agentId, request });
}

export async function deleteAgent(agentId: string): Promise<boolean> {
  return invoke<boolean>("delete_agent", { agentId });
}

// ══════════════════════════════════════════════════════════════
// Import (1)
// ══════════════════════════════════════════════════════════════

export async function importOpenClawConfig(configJson: string): Promise<ImportResult> {
  return invoke<ImportResult>("import_openclaw_config", { configJson });
}

// ══════════════════════════════════════════════════════════════
// Chat (8)
// ══════════════════════════════════════════════════════════════

export async function sendMessage(agentId: string, content: string, modelOverride?: string, chatId?: string, providerOverride?: string, apiKey?: string, baseUrl?: string): Promise<SendMessageResponse> {
  return invoke<SendMessageResponse>("send_message", {
    request: {
      agent_id: agentId,
      content,
      model_override: modelOverride || null,
      chat_id: chatId || null,
      provider_override: providerOverride || null,
      api_key: apiKey || null,
      base_url: baseUrl || null,
    },
  });
}

export async function cancelActiveRun(chatId?: string): Promise<boolean> {
  return invoke<boolean>("cancel_active_run", { chatId: chatId ?? null });
}

export async function getSessionMessages(agentId: string): Promise<ChatMessage[]> {
  return invoke<ChatMessage[]>("get_session_messages", { agentId });
}

export async function getChatMessages(chatId: string): Promise<ChatMessage[]> {
  return invoke<ChatMessage[]>("get_chat_messages", { chatId });
}

export async function listSessions(): Promise<SessionSummary[]> {
  return invoke<SessionSummary[]>("list_sessions");
}

export async function createChat(agentId: string): Promise<SessionSummary> {
  return invoke<SessionSummary>("create_chat", { agentId });
}

export async function deleteChat(chatId: string): Promise<boolean> {
  return invoke<boolean>("delete_chat", { chatId });
}

/** Delete all chat history from SochDB. Returns the number of sessions deleted. */
export async function clearAllChats(): Promise<number> {
  return invoke<number>("clear_all_chats");
}

export async function updateChatTitle(chatId: string, title: string): Promise<boolean> {
  return invoke<boolean>("update_chat_title", { chatId, title });
}

/** Diagnostic: dump all session metadata from SochDB (no message content). */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export async function debugSessionStorage(): Promise<any[]> {
  return invoke<any[]>("debug_session_storage");
}

// ══════════════════════════════════════════════════════════════
// Skills (3)
// ══════════════════════════════════════════════════════════════

export async function listSkills(): Promise<SkillDescriptor[]> {
  return invoke<SkillDescriptor[]>("list_skills");
}

export async function activateSkill(skillId: string): Promise<boolean> {
  return invoke<boolean>("activate_skill", { skillId });
}

export async function deactivateSkill(skillId: string): Promise<boolean> {
  return invoke<boolean>("deactivate_skill", { skillId });
}

export async function deleteSkill(skillId: string): Promise<boolean> {
  return invoke<boolean>("delete_skill", { skillId });
}

// ══════════════════════════════════════════════════════════════
// Pipelines (3)
// ══════════════════════════════════════════════════════════════

export async function listPipelines(): Promise<PipelineDescriptor[]> {
  return invoke<PipelineDescriptor[]>("list_pipelines");
}

export async function createPipeline(
  name: string,
  description: string,
  steps: PipelineNodeDescriptor[],
  edges: [number, number][],
  schedule?: string | null
): Promise<PipelineDescriptor> {
  return invoke<PipelineDescriptor>("create_pipeline", {
    request: { name, description, steps, edges, schedule: schedule ?? null },
  });
}

export async function updatePipeline(
  pipelineId: string,
  name: string,
  description: string,
  steps: PipelineNodeDescriptor[],
  edges: [number, number][],
  schedule?: string | null
): Promise<PipelineDescriptor> {
  return invoke<PipelineDescriptor>("update_pipeline", {
    pipelineId,
    request: { name, description, steps, edges, schedule: schedule ?? null },
  });
}

export async function deletePipeline(pipelineId: string): Promise<boolean> {
  return invoke<boolean>("delete_pipeline", { pipelineId });
}

export async function runPipeline(pipelineId: string): Promise<PipelineRunResult> {
  return invoke<PipelineRunResult>("run_pipeline", { pipelineId });
}

export async function getPipelineRuns(pipelineId: string): Promise<any[]> {
  return invoke<any[]>("get_pipeline_runs", { pipelineId });
}

// ══════════════════════════════════════════════════════════════
// Cron / Scheduled Tasks
// ══════════════════════════════════════════════════════════════

export async function listCronTasks(): Promise<any[]> {
  return invoke<any[]>("list_cron_tasks");
}

export async function triggerCronTask(taskId: string): Promise<any> {
  return invoke<any>("trigger_cron_task", { taskId });
}

export async function getCronLogs(limit?: number): Promise<any[]> {
  return invoke<any[]>("get_cron_logs", { limit: limit ?? 50 });
}

// ══════════════════════════════════════════════════════════════
// Monitoring (3)
// ══════════════════════════════════════════════════════════════

export async function getMetrics(): Promise<CostMetrics> {
  return invoke<CostMetrics>("get_metrics");
}

export async function getSecurityStatus(): Promise<SecurityStatus> {
  return invoke<SecurityStatus>("get_security_status");
}

export async function getAgentTrace(agentId?: string): Promise<TraceEntry[]> {
  return invoke<TraceEntry[]>("get_agent_trace", { agentId: agentId ?? null });
}

// ══════════════════════════════════════════════════════════════
// Tunnel (2)
// ══════════════════════════════════════════════════════════════

export async function getTunnelStatus(): Promise<TunnelMetricsSnapshot> {
  return invoke<TunnelMetricsSnapshot>("get_tunnel_status");
}

export async function createInvite(
  label: string,
  endpoint: string,
  ttlHours?: number
): Promise<InviteResponse> {
  return invoke<InviteResponse>("create_invite", {
    label, endpoint, ttlHours: ttlHours ?? null,
  });
}

// ══════════════════════════════════════════════════════════════
// Config (3)
// ══════════════════════════════════════════════════════════════

export async function getConfig(): Promise<unknown> {
  return invoke("get_config");
}

export async function listModels(): Promise<ModelInfo[]> {
  return invoke<ModelInfo[]>("list_models");
}

export async function listChannels(): Promise<ChannelInfo[]> {
  return invoke<ChannelInfo[]>("list_channels");
}

export async function updateChannel(channelId: string, config: Record<string, string>): Promise<boolean> {
  return invoke<boolean>("update_channel", { channelId, config });
}

export async function disconnectChannel(channelId: string): Promise<boolean> {
  return invoke<boolean>("disconnect_channel", { channelId });
}

export async function getChannelTypes(): Promise<ChannelTypeSpec[]> {
  try {
    const result = await invoke<ChannelTypeSpec[]>("get_channel_types");
    if (result && result.length > 0) return result;
  } catch {
    // backend unavailable — fall through to static specs
  }
  return _channelTypeSpecs;
}

// ══════════════════════════════════════════════════════════════
// Runtime — Durable agent runs (4)
// ══════════════════════════════════════════════════════════════

export async function getRuntimeStatus(): Promise<RuntimeStatusInfo> {
  return invoke<RuntimeStatusInfo>("get_runtime_status");
}

export async function cancelDurableRun(runId: string, reason?: string): Promise<boolean> {
  return invoke<boolean>("cancel_durable_run", { runId, reason: reason ?? null });
}

export async function getDurableRunStatus(runId: string): Promise<string> {
  return invoke<string>("get_durable_run_status", { runId });
}

export async function resumeDurableRun(runId: string): Promise<DurableRunInfo> {
  return invoke<DurableRunInfo>("resume_durable_run", { runId });
}

// ══════════════════════════════════════════════════════════════
// Media (2)
// ══════════════════════════════════════════════════════════════

export async function getMediaPipelineStatus(): Promise<MediaPipelineStatus> {
  return invoke<MediaPipelineStatus>("get_media_pipeline_status");
}

export async function getLinkPreview(url: string): Promise<LinkPreviewResult> {
  return invoke<LinkPreviewResult>("get_link_preview", { url });
}

// ══════════════════════════════════════════════════════════════
// Plugins (4)
// ══════════════════════════════════════════════════════════════

export async function listPlugins(): Promise<PluginSummary[]> {
  return invoke<PluginSummary[]>("list_plugins");
}

export async function getPluginInfo(pluginId: string): Promise<PluginSummary> {
  return invoke<PluginSummary>("get_plugin_info", { pluginId });
}

export async function enablePlugin(pluginId: string): Promise<boolean> {
  return invoke<boolean>("enable_plugin", { pluginId });
}

export async function disablePlugin(pluginId: string): Promise<boolean> {
  return invoke<boolean>("disable_plugin", { pluginId });
}

// ══════════════════════════════════════════════════════════════
// A2A Protocol (5)
// ══════════════════════════════════════════════════════════════

export async function listA2aAgents(): Promise<AgentCardInfo[]> {
  return invoke<AgentCardInfo[]>("list_a2a_agents");
}

export async function registerA2aAgent(request: RegisterAgentCardRequest): Promise<boolean> {
  return invoke<boolean>("register_a2a_agent", { request });
}

export async function deregisterA2aAgent(agentId: string): Promise<boolean> {
  return invoke<boolean>("deregister_a2a_agent", { agentId });
}

export async function getAgentCard(agentId: string): Promise<A2AFullAgentCard> {
  return invoke<A2AFullAgentCard>("get_agent_card", { agentId });
}

export async function getSelfAgentCard(): Promise<A2AFullAgentCard> {
  return invoke<A2AFullAgentCard>("get_self_agent_card");
}

// ══════════════════════════════════════════════════════════════
// Security — OAuth2, Approvals, ACL, Tokens (14)
// ══════════════════════════════════════════════════════════════

export async function startOAuthFlow(request: OAuthStartRequest): Promise<OAuthStartResponse> {
  return invoke<OAuthStartResponse>("start_oauth_flow", { request });
}

export async function handleOAuthCallback(request: OAuthCallbackRequest): Promise<OAuthTokenResponse> {
  return invoke<OAuthTokenResponse>("handle_oauth_callback", { request });
}

export async function refreshOAuthToken(provider: string): Promise<OAuthTokenResponse> {
  return invoke<OAuthTokenResponse>("refresh_oauth_token", { provider });
}

export async function listAuthProfiles(provider?: string): Promise<AuthProfileInfo[]> {
  return invoke<AuthProfileInfo[]>("list_auth_profiles", { provider: provider ?? null });
}

export async function removeAuthProfile(provider: string, profileId: string): Promise<boolean> {
  return invoke<boolean>("remove_auth_profile", { provider, profileId });
}

export async function createApprovalRequest(request: CreateApprovalRequest): Promise<ApprovalRequestInfo> {
  return invoke<ApprovalRequestInfo>("create_approval_request", { request });
}

export async function approveRequest(requestId: string, approver: string): Promise<boolean> {
  return invoke<boolean>("approve_request", { requestId, approver });
}

export async function denyRequest(requestId: string, approver: string, reason?: string): Promise<boolean> {
  return invoke<boolean>("deny_request", { requestId, approver, reason: reason ?? null });
}

export async function getApprovalStatus(requestId: string): Promise<string> {
  return invoke<string>("get_approval_status", { requestId });
}

export async function addAclRule(request: AclRuleRequest): Promise<boolean> {
  return invoke<boolean>("add_acl_rule", { request });
}

export async function checkPermission(
  principalType: string,
  principalId: string,
  resourceType: string,
  resourceId: string,
  action: string
): Promise<AclCheckResult> {
  return invoke<AclCheckResult>("check_permission", {
    principalType, principalId, resourceType, resourceId, action,
  });
}

export async function revokeAclRules(principalType: string, principalId: string): Promise<boolean> {
  return invoke<boolean>("revoke_acl_rules", { principalType, principalId });
}

export async function generateToken(request: GenerateTokenRequest): Promise<TokenInfo> {
  return invoke<TokenInfo>("generate_token", { request });
}

export async function validateToken(encodedToken: string): Promise<TokenInfo> {
  return invoke<TokenInfo>("validate_token", { encodedToken });
}

// ══════════════════════════════════════════════════════════════
// Discovery (5)
// ══════════════════════════════════════════════════════════════

export async function getMdnsServiceInfo(): Promise<unknown> {
  return invoke("get_mdns_service_info");
}

export async function startPairing(): Promise<PairingStatus> {
  return invoke<PairingStatus>("start_pairing");
}

export async function completePairing(code: string, peerName: string): Promise<boolean> {
  return invoke<boolean>("complete_pairing", { code, peerName });
}

export async function getPairingStatus(): Promise<PairingStatus | null> {
  return invoke<PairingStatus | null>("get_pairing_status");
}

export async function listDiscoveredPeers(): Promise<PeerInfo[]> {
  return invoke<PeerInfo[]>("list_discovered_peers");
}

// ══════════════════════════════════════════════════════════════
// Observability (2)
// ══════════════════════════════════════════════════════════════

export async function getObservabilityConfig(): Promise<ObservabilityStatus> {
  return invoke<ObservabilityStatus>("get_observability_config");
}

export async function configureObservability(request: ConfigureObservabilityRequest): Promise<ObservabilityStatus> {
  return invoke<ObservabilityStatus>("configure_observability", { request });
}

// ══════════════════════════════════════════════════════════════
// Infra — Notifications, Clipboard, Voice, Idle (9)
// ══════════════════════════════════════════════════════════════

export async function sendNotification(request: SendNotificationRequest): Promise<NotificationInfo> {
  return invoke<NotificationInfo>("send_notification", { request });
}

export async function listNotifications(): Promise<NotificationInfo[]> {
  return invoke<NotificationInfo[]>("list_notifications");
}

export async function readClipboard(): Promise<ClipboardEntryInfo | null> {
  return invoke<ClipboardEntryInfo | null>("read_clipboard");
}

export async function writeClipboard(text: string): Promise<boolean> {
  return invoke<boolean>("write_clipboard", { text });
}

export async function getClipboardHistory(limit?: number): Promise<ClipboardEntryInfo[]> {
  return invoke<ClipboardEntryInfo[]>("get_clipboard_history", { limit: limit ?? null });
}

export async function configureVoiceWake(request: VoiceWakeConfigRequest): Promise<VoiceWakeStatus> {
  return invoke<VoiceWakeStatus>("configure_voice_wake", { request });
}

export async function getVoiceWakeStatus(): Promise<VoiceWakeStatus> {
  return invoke<VoiceWakeStatus>("get_voice_wake_status");
}

export async function getIdleStatus(): Promise<IdleStatus> {
  return invoke<IdleStatus>("get_idle_status");
}

export async function recordActivity(): Promise<boolean> {
  return invoke<boolean>("record_activity");
}

// ══════════════════════════════════════════════════════════════
// Domain — Context Guard, Prompt, Provider, Skills (6)
// ══════════════════════════════════════════════════════════════

export async function getContextGuardStatus(agentId?: string): Promise<ContextGuardStatus> {
  return invoke<ContextGuardStatus>("get_context_guard_status", { agentId: agentId ?? null });
}

export async function getPromptManifest(agentId: string): Promise<PromptManifestInfo | null> {
  return invoke<PromptManifestInfo | null>("get_prompt_manifest", { agentId });
}

export async function listProviderCapabilities(): Promise<ProviderCapabilityInfo[]> {
  return invoke<ProviderCapabilityInfo[]>("list_provider_capabilities");
}

export async function getProviderRouting(model: string, requiredCaps: string[]): Promise<RoutingDecisionInfo> {
  return invoke<RoutingDecisionInfo>("get_provider_routing", { model, requiredCaps });
}

export async function getSkillTrustLevel(skillId: string): Promise<SkillTrustInfo> {
  return invoke<SkillTrustInfo>("get_skill_trust_level", { skillId });
}

export async function evaluateSkillTriggers(messageText: string): Promise<SkillTriggerInfo[]> {
  return invoke<SkillTriggerInfo[]>("evaluate_skill_triggers", { messageText });
}

export async function getSkillDetail(skillId: string): Promise<SkillDetail> {
  return invoke<SkillDetail>("get_skill_detail", { skillId });
}

export async function registerSkill(request: RegisterSkillRequest): Promise<SkillDescriptor> {
  return invoke<SkillDescriptor>("register_skill", { request });
}

export async function validateSkillMd(skillMdContent: string): Promise<SkillValidationResult> {
  return invoke<SkillValidationResult>("validate_skill_md", { skillMdContent });
}

// ══════════════════════════════════════════════════════════════
// Canvas (7)
// ══════════════════════════════════════════════════════════════

export async function createCanvas(request: CreateCanvasRequest): Promise<CanvasSummary> {
  return invoke<CanvasSummary>("create_canvas", { request });
}

export async function getCanvas(canvasId: string): Promise<unknown> {
  return invoke("get_canvas", { canvasId });
}

export async function listCanvases(): Promise<CanvasSummary[]> {
  return invoke<CanvasSummary[]>("list_canvases");
}

export async function addCanvasBlock(request: AddBlockRequest): Promise<BlockInfo> {
  return invoke<BlockInfo>("add_canvas_block", { request });
}

export async function removeCanvasBlock(canvasId: string, blockId: string): Promise<boolean> {
  return invoke<boolean>("remove_canvas_block", { canvasId, blockId });
}

export async function connectCanvasBlocks(request: ConnectBlocksRequest): Promise<boolean> {
  return invoke<boolean>("connect_canvas_blocks", { request });
}

export async function exportCanvasMarkdown(canvasId: string): Promise<string> {
  return invoke<string>("export_canvas_markdown", { canvasId });
}

// ══════════════════════════════════════════════════════════════
// Memory (5)
// ══════════════════════════════════════════════════════════════

export async function rememberMemory(request: RememberRequest): Promise<RememberResponse> {
  return invoke<RememberResponse>("remember_memory", { request });
}

export async function rememberBatch(items: RememberBatchItem[]): Promise<string[]> {
  return invoke<string[]>("remember_batch", { request: { items } });
}

export async function recallMemories(request: RecallRequest): Promise<MemoryHit[]> {
  return invoke<MemoryHit[]>("recall_memories", { request });
}

export async function forgetMemory(id: string): Promise<boolean> {
  return invoke<boolean>("forget_memory", { request: { id } });
}

export async function getMemoryStats(): Promise<MemoryStatsResponse> {
  return invoke<MemoryStatsResponse>("get_memory_stats");
}

// ══════════════════════════════════════════════════════════════
// SochDB — Semantic Cache (3)
// ══════════════════════════════════════════════════════════════

export async function cacheLookup(
  query: string,
  namespace: string,
  queryEmbedding?: number[]
): Promise<CacheLookupInfo> {
  return invoke<CacheLookupInfo>("cache_lookup", {
    query, namespace, queryEmbedding: queryEmbedding ?? null,
  });
}

export async function cacheStore(
  query: string,
  namespace: string,
  result: string,
  embedding?: number[],
  sourceDocs?: string[],
  ttlSecs?: number
): Promise<string> {
  return invoke<string>("cache_store", {
    query, namespace, result,
    embedding: embedding ?? null,
    sourceDocs: sourceDocs ?? [],
    ttlSecs: ttlSecs ?? null,
  });
}

export async function cacheInvalidateSource(docId: string): Promise<number> {
  return invoke<number>("cache_invalidate_source", { docId });
}

// ══════════════════════════════════════════════════════════════
// SochDB — Trace Store (8)
// ══════════════════════════════════════════════════════════════

export async function traceStartRun(
  name: string,
  resource: Record<string, string>
): Promise<TraceRunInfo> {
  return invoke<TraceRunInfo>("trace_start_run", { name, resource });
}

export async function traceEndRun(traceId: string): Promise<void> {
  return invoke<void>("trace_end_run", { traceId });
}

export async function traceStartSpan(
  traceId: string,
  name: string,
  parentSpanId?: string,
  kind?: string
): Promise<TraceSpanInfo> {
  return invoke<TraceSpanInfo>("trace_start_span", {
    traceId, name,
    parentSpanId: parentSpanId ?? null,
    kind: kind ?? "Internal",
  });
}

export async function traceEndSpan(
  traceId: string,
  spanId: string,
  status: string,
  message?: string
): Promise<void> {
  return invoke<void>("trace_end_span", {
    traceId, spanId, status, message: message ?? null,
  });
}

export async function traceGetSpans(traceId: string): Promise<TraceSpanInfo[]> {
  return invoke<TraceSpanInfo[]>("trace_get_spans", { traceId });
}

export async function traceGetRun(traceId: string): Promise<TraceRunInfo | null> {
  return invoke<TraceRunInfo | null>("trace_get_run", { traceId });
}

export async function traceUpdateMetrics(
  traceId: string,
  tokens: number,
  costMillicents: number
): Promise<void> {
  return invoke<void>("trace_update_metrics", { traceId, tokens, costMillicents });
}

export async function traceLogToolCall(
  traceId: string,
  spanId: string,
  toolName: string,
  args: string,
  result?: string,
  durationUs?: number,
  success?: boolean
): Promise<void> {
  return invoke<void>("trace_log_tool_call", {
    traceId, spanId, toolName, arguments: args,
    result: result ?? null,
    durationUs: durationUs ?? 0,
    success: success ?? true,
  });
}

// ══════════════════════════════════════════════════════════════
// SochDB — Checkpoint Store (6)
// ══════════════════════════════════════════════════════════════

export async function checkpointCreateRun(
  runId: string,
  workflow: string,
  params: Record<string, unknown>
): Promise<WorkflowRunInfo> {
  return invoke<WorkflowRunInfo>("checkpoint_create_run", { runId, workflow, params });
}

export async function checkpointSave(
  runId: string,
  nodeId: string,
  stateJson: string,
  metadata?: Record<string, string>
): Promise<CheckpointInfo> {
  return invoke<CheckpointInfo>("checkpoint_save", {
    runId, nodeId, stateJson, metadata: metadata ?? null,
  });
}

export async function checkpointLoad(runId: string, nodeId: string): Promise<string | null> {
  return invoke<string | null>("checkpoint_load", { runId, nodeId });
}

export async function checkpointList(runId: string): Promise<CheckpointInfo[]> {
  return invoke<CheckpointInfo[]>("checkpoint_list", { runId });
}

export async function checkpointGetRun(runId: string): Promise<WorkflowRunInfo | null> {
  return invoke<WorkflowRunInfo | null>("checkpoint_get_run", { runId });
}

export async function checkpointDeleteRun(runId: string): Promise<boolean> {
  return invoke<boolean>("checkpoint_delete_run", { runId });
}

// ══════════════════════════════════════════════════════════════
// SochDB — Knowledge Graph (8)
// ══════════════════════════════════════════════════════════════

export async function graphAddNode(
  nodeId: string,
  nodeType: string,
  properties?: Record<string, unknown>
): Promise<GraphNodeInfo> {
  return invoke<GraphNodeInfo>("graph_add_node", {
    nodeId, nodeType, properties: properties ?? null,
  });
}

export async function graphGetNode(nodeId: string): Promise<GraphNodeInfo | null> {
  return invoke<GraphNodeInfo | null>("graph_get_node", { nodeId });
}

export async function graphDeleteNode(nodeId: string, cascade: boolean): Promise<boolean> {
  return invoke<boolean>("graph_delete_node", { nodeId, cascade });
}

export async function graphAddEdge(
  fromId: string,
  edgeType: string,
  toId: string,
  properties?: Record<string, unknown>
): Promise<GraphEdgeInfo> {
  return invoke<GraphEdgeInfo>("graph_add_edge", {
    fromId, edgeType, toId, properties: properties ?? null,
  });
}

export async function graphGetEdges(fromId: string, edgeType?: string): Promise<GraphEdgeInfo[]> {
  return invoke<GraphEdgeInfo[]>("graph_get_edges", {
    fromId, edgeType: edgeType ?? null,
  });
}

export async function graphShortestPath(
  fromId: string,
  toId: string,
  maxDepth: number
): Promise<string[] | null> {
  return invoke<string[] | null>("graph_shortest_path", { fromId, toId, maxDepth });
}

export async function graphGetSubgraph(startId: string, maxDepth: number): Promise<SubgraphInfo> {
  return invoke<SubgraphInfo>("graph_get_subgraph", { startId, maxDepth });
}

export async function graphGetNodesByType(nodeType: string, limit: number): Promise<GraphNodeInfo[]> {
  return invoke<GraphNodeInfo[]>("graph_get_nodes_by_type", { nodeType, limit });
}

// ══════════════════════════════════════════════════════════════
// SochDB — Temporal Graph (4)
// ══════════════════════════════════════════════════════════════

export async function temporalAddEdge(
  fromId: string,
  edgeType: string,
  toId: string,
  properties?: Record<string, unknown>
): Promise<TemporalEdgeInfo> {
  return invoke<TemporalEdgeInfo>("temporal_add_edge", {
    fromId, edgeType, toId, properties: properties ?? null,
  });
}

export async function temporalInvalidateEdge(
  fromId: string,
  edgeType: string,
  toId: string
): Promise<boolean> {
  return invoke<boolean>("temporal_invalidate_edge", { fromId, edgeType, toId });
}

export async function temporalEdgesAt(
  fromId: string,
  edgeType: string | null,
  atTime: number
): Promise<TemporalEdgeInfo[]> {
  return invoke<TemporalEdgeInfo[]>("temporal_edges_at", {
    fromId, edgeType, atTime,
  });
}

export async function temporalEdgeHistory(
  fromId: string,
  edgeType: string,
  toId: string
): Promise<TemporalEdgeInfo[]> {
  return invoke<TemporalEdgeInfo[]>("temporal_edge_history", { fromId, edgeType, toId });
}

// ══════════════════════════════════════════════════════════════
// SochDB — Policy Engine (3)
// ══════════════════════════════════════════════════════════════

export async function policyEnableAudit(): Promise<void> {
  return invoke<void>("policy_enable_audit");
}

export async function policyGetAuditLog(limit: number): Promise<unknown[]> {
  return invoke<unknown[]>("policy_get_audit_log", { limit });
}

export interface LogEntry {
  id: string;
  timestamp: string;
  level: string;
  subsystem: string;
  message: string;
  category: string;
  actor: string;
  outcome: string;
}

export async function getAuditLogs(limit: number): Promise<LogEntry[]> {
  return invoke<LogEntry[]>("get_audit_logs", { limit });
}

export async function policyAddRateLimit(
  operation: string,
  maxPerMinute: number,
  scope: string
): Promise<void> {
  return invoke<void>("policy_add_rate_limit", { operation, maxPerMinute, scope });
}

// ══════════════════════════════════════════════════════════════
// SochDB — Atomic Memory (2)
// ══════════════════════════════════════════════════════════════

export async function atomicMemoryWrite(
  memoryId: string,
  blobs: [string, string][],
  graphNodes: [string, string, string][],
  graphEdges: [string, string, string, string][]
): Promise<AtomicWriteInfo> {
  return invoke<AtomicWriteInfo>("atomic_memory_write", {
    memoryId, blobs, graphNodes, graphEdges,
  });
}

export async function atomicMemoryRecover(): Promise<unknown> {
  return invoke("atomic_memory_recover");
}

// ══════════════════════════════════════════════════════════════
// SochDB — Agent Registry (4)
// ══════════════════════════════════════════════════════════════

export async function registryRegisterAgent(
  agentId: string,
  capabilities: string[]
): Promise<AgentRegistryInfo> {
  return invoke<AgentRegistryInfo>("registry_register_agent", { agentId, capabilities });
}

export async function registryListAgents(): Promise<AgentRegistryInfo[]> {
  return invoke<AgentRegistryInfo[]>("registry_list_agents");
}

export async function registryFindCapable(
  required: string[],
  exclude: string[]
): Promise<AgentRegistryInfo[]> {
  return invoke<AgentRegistryInfo[]>("registry_find_capable", { required, exclude });
}

export async function registryUnregisterAgent(agentId: string): Promise<boolean> {
  return invoke<boolean>("registry_unregister_agent", { agentId });
}

// ══════════════════════════════════════════════════════════════
// SochDB — Core operations (2)
// ══════════════════════════════════════════════════════════════

export async function sochdbCheckpoint(): Promise<number> {
  return invoke<number>("sochdb_checkpoint");
}

export async function sochdbSync(): Promise<void> {
  return invoke<void>("sochdb_sync");
}

// ══════════════════════════════════════════════════════════════
// SochDB — Storage Health + Lifecycle + Session Indexes
// ══════════════════════════════════════════════════════════════

export interface StoreHealth {
  name: string;
  status: "healthy" | "ephemeral" | "failed";
  path: string | null;
  key_count: number | null;
  detail: string;
}

export interface StorageHealthResponse {
  overall: "healthy" | "ephemeral" | "failed";
  stores: StoreHealth[];
  any_ephemeral: boolean;
  recommendations: string[];
  checked_at: string;
}

export interface LifecycleReport {
  entity_id: string;
  entity_type: string;
  total_deleted: number;
  stores_touched: number;
  warnings: string[];
  duration_us: number;
}

/** Deep storage health check across all subsystems. */
export async function storageHealth(): Promise<StorageHealthResponse> {
  return invoke<StorageHealthResponse>("storage_health");
}

/** Cascade-delete a session and all related data. */
export async function lifecycleDeleteSession(sessionId: string, agentId?: string): Promise<LifecycleReport> {
  return invoke<LifecycleReport>("lifecycle_delete_session", { session_id: sessionId, agent_id: agentId ?? null });
}

/** Cascade-delete a thread and all related data. */
export async function lifecycleDeleteThread(threadId: string): Promise<LifecycleReport> {
  return invoke<LifecycleReport>("lifecycle_delete_thread", { thread_id: threadId });
}

/** Cascade-delete an agent and ALL related data. */
export async function lifecycleDeleteAgent(agentId: string): Promise<LifecycleReport> {
  return invoke<LifecycleReport>("lifecycle_delete_agent", { agent_id: agentId });
}

/** List sessions ordered by last activity. */
export async function sessionsByActivity(limit?: number): Promise<string[]> {
  return invoke<string[]>("sessions_by_activity", { limit: limit ?? null });
}

/** List sessions filtered by channel. */
export async function sessionsByChannel(channel: string, limit?: number): Promise<string[]> {
  return invoke<string[]>("sessions_by_channel", { channel, limit: limit ?? null });
}

/** List sessions filtered by agent. */
export async function sessionsByAgent(agentId: string, limit?: number): Promise<string[]> {
  return invoke<string[]>("sessions_by_agent", { agent_id: agentId, limit: limit ?? null });
}

/** Rebuild all session indexes from primary data. */
export async function sessionsRebuildIndexes(): Promise<number> {
  return invoke<number>("sessions_rebuild_indexes");
}

// ══════════════════════════════════════════════════════════════
// Terminal — Shell command execution
// ══════════════════════════════════════════════════════════════

export interface RunCommandResponse {
  stdout: string;
  stderr: string;
  exit_code: number | null;
  success: boolean;
}

export async function runShellCommand(
  command: string,
  cwd?: string,
): Promise<RunCommandResponse> {
  if (isBrowserDev) {
    throw new Error(
      "Shell commands require the Tauri desktop runtime. " +
      "Run the app with `cargo tauri dev` instead of standalone `pnpm dev`."
    );
  }
  return invoke<RunCommandResponse>("run_shell_command", {
    request: { command, cwd },
  });
}

// ══════════════════════════════════════════════════════════════
// Providers — Testing Connection
// ══════════════════════════════════════════════════════════════

export async function testLlmConnection(
  provider: string,
  model: string,
  apiKey?: string,
  baseUrl?: string,
  project?: string,
  location?: string,
): Promise<string> {
  if (isBrowserDev) {
    return Promise.resolve("Hello World (Browser Dev Mock)");
  }
  return invoke<string>("test_llm_connection", {
    provider,
    model,
    apiKey: apiKey || null,
    baseUrl: baseUrl || null,
    project: project || null,
    location: location || null,
  });
}

// ══════════════════════════════════════════════════════════════
// Debug / Storage Diagnostics (5)
// ══════════════════════════════════════════════════════════════

export async function toggleDebugMode(enabled: boolean): Promise<boolean> {
  if (isBrowserDev) return enabled;
  return invoke<boolean>("toggle_debug_mode", { enabled });
}

export async function getDebugMode(): Promise<boolean> {
  if (isBrowserDev) return false;
  return invoke<boolean>("get_debug_mode");
}

export async function debugStorageSnapshot(): Promise<StorageSnapshot> {
  return invoke<StorageSnapshot>("debug_storage_snapshot");
}

export async function debugForcePersist(): Promise<string> {
  if (isBrowserDev) return "Browser dev mode — no backend";
  return invoke<string>("debug_force_persist");
}

export async function debugRehydrate(): Promise<string> {
  if (isBrowserDev) return "Browser dev mode — no backend";
  return invoke<string>("debug_rehydrate");
}

// ══════════════════════════════════════════════════════════════
// Durable Runtime
// ══════════════════════════════════════════════════════════════

export async function listDurableRuns(): Promise<DurableRunInfo[]> {
  return invoke<DurableRunInfo[]>("list_durable_runs");
}

export async function listCheckpoints(runId: string): Promise<CheckpointEntry[]> {
  return invoke<CheckpointEntry[]>("list_checkpoints", { runId });
}

export async function getDlq(): Promise<DlqEntry[]> {
  return invoke<DlqEntry[]>("get_dlq");
}

// ══════════════════════════════════════════════════════════════
// A2A Protocol Tasks
// ══════════════════════════════════════════════════════════════

export async function sendA2ATask(
  requesterId: string,
  req: TaskSendRequest
): Promise<A2ATaskResponse> {
  return invoke<A2ATaskResponse>("send_a2a_task", { requesterId, req });
}

export async function getA2ATask(taskId: string): Promise<A2ATaskResponse> {
  return invoke<A2ATaskResponse>("get_a2a_task", { taskId });
}

export async function listA2ATasks(): Promise<A2ATaskResponse[]> {
  return invoke<A2ATaskResponse[]>("list_a2a_tasks");
}

export async function cancelA2ATask(taskId: string, reason?: string): Promise<A2ATaskResponse> {
  return invoke<A2ATaskResponse>("cancel_a2a_task", { taskId, reason: reason ?? null });
}

export async function provideA2ATaskInput(taskId: string, input: any): Promise<A2ATaskResponse> {
  return invoke<A2ATaskResponse>("provide_a2a_task_input", { taskId, input });
}

// Voice Input — Local Whisper STT (8)

export interface RecordingResponse {
  success: boolean;
  state: string;
  sample_rate?: number;
  error?: string;
}

export interface TranscribeResult {
  text: string;
  language?: string;
  duration_ms: number;
  segments: { text: string; start_ms: number; end_ms: number }[];
}

export interface WhisperModelStatus {
  model: string;
  downloaded: boolean;
  path?: string;
  size_bytes?: number;
}

export interface VoiceInputStatusResult {
  engine_ready: boolean;
  model: string;
  model_downloaded: boolean;
  models_dir: string;
}

export async function startVoiceRecording(): Promise<RecordingResponse> {
  return invoke<RecordingResponse>("start_voice_recording");
}

export async function stopVoiceRecording(): Promise<TranscribeResult> {
  return invoke<TranscribeResult>("stop_voice_recording");
}

export async function cancelVoiceRecording(): Promise<RecordingResponse> {
  return invoke<RecordingResponse>("cancel_voice_recording");
}

export async function transcribeAudio(audioBase64: string): Promise<TranscribeResult> {
  return invoke<TranscribeResult>("transcribe_audio", { audioBase64 });
}

export async function getWhisperModels(): Promise<WhisperModelStatus[]> {
  return invoke<WhisperModelStatus[]>("get_whisper_models");
}

export async function downloadWhisperModel(model: string): Promise<WhisperModelStatus> {
  return invoke<WhisperModelStatus>("download_whisper_model", { model });
}

export async function deleteWhisperModel(model: string): Promise<boolean> {
  return invoke<boolean>("delete_whisper_model", { model });
}

export async function getVoiceInputStatus(): Promise<VoiceInputStatusResult> {
  return invoke<VoiceInputStatusResult>("get_voice_input_status");
}

// ══════════════════════════════════════════════════════════════
// Sandbox — Multi-modal Code Execution Isolation (5)
// ══════════════════════════════════════════════════════════════

export async function getSandboxStatus(): Promise<SandboxStatusInfo> {
  if (isBrowserDev) {
    return {
      available: true,
      max_isolation: "ProcessIsolation",
      available_levels: ["None", "PathScope", "ProcessIsolation"],
      default_limits: {
        cpu_time_secs: 30,
        wall_time_secs: 60,
        memory_bytes: 536870912,
        max_fds: 256,
        max_output_bytes: 10485760,
        max_processes: 10,
      },
    };
  }
  return invoke<SandboxStatusInfo>("get_sandbox_status");
}

export async function listSandboxBackends(): Promise<SandboxBackendInfo[]> {
  if (isBrowserDev) {
    return [
      { name: "WorkspaceSandbox", isolation_level: "PathScope", available: true },
      { name: "SubprocessSandbox", isolation_level: "ProcessIsolation", available: true },
    ];
  }
  return invoke<SandboxBackendInfo[]>("list_sandbox_backends");
}

export async function executeSandboxed(
  command: string,
  isolationLevel?: string,
  limits?: ResourceLimitsInfo,
): Promise<SandboxExecResult> {
  if (isBrowserDev) {
    return {
      exit_code: 0,
      stdout: `[mock] ${command}`,
      stderr: "",
      duration_ms: 42,
      resource_usage: { cpu_time_ms: 30, wall_time_ms: 42, peak_memory_bytes: 1024000, output_bytes: 100 },
    };
  }
  return invoke<SandboxExecResult>("execute_sandboxed", {
    command,
    isolationLevel,
    limits,
  });
}

export async function getSandboxResourceLimits(): Promise<ResourceLimitsInfo> {
  if (isBrowserDev) {
    return {
      cpu_time_secs: 30,
      wall_time_secs: 60,
      memory_bytes: 536870912,
      max_fds: 256,
      max_output_bytes: 10485760,
      max_processes: 10,
    };
  }
  return invoke<ResourceLimitsInfo>("get_sandbox_resource_limits");
}

export async function cleanupSandboxes(): Promise<boolean> {
  if (isBrowserDev) return true;
  return invoke<boolean>("cleanup_sandboxes");
}

// ══════════════════════════════════════════════════════════════
// MCP — Model Context Protocol (10)
// ══════════════════════════════════════════════════════════════

export async function listMcpServers(): Promise<McpServerInfo[]> {
  if (isBrowserDev) {
    return [
      { name: "sqlite", transport: "stdio", connected: true, tool_count: 4 },
      { name: "filesystem", transport: "stdio", connected: true, tool_count: 6 },
    ];
  }
  return invoke<McpServerInfo[]>("list_mcp_servers");
}

export async function connectMcpServer(request: McpConnectRequest): Promise<McpServerInfo> {
  if (isBrowserDev) {
    return { name: request.name, transport: request.transport, connected: true, tool_count: 0 };
  }
  return invoke<McpServerInfo>("connect_mcp_server", { request });
}

export async function disconnectMcpServer(serverName: string): Promise<boolean> {
  if (isBrowserDev) return true;
  return invoke<boolean>("disconnect_mcp_server", { serverName });
}

export async function listMcpTools(serverName?: string): Promise<McpToolInfo[]> {
  if (isBrowserDev) {
    return [
      { name: "read_query", description: "Execute a SELECT query", input_schema: {}, server: "sqlite" },
      { name: "write_query", description: "Execute INSERT/UPDATE/DELETE", input_schema: {}, server: "sqlite" },
      { name: "list_tables", description: "List database tables", input_schema: {}, server: "sqlite" },
      { name: "read_file", description: "Read a file", input_schema: {}, server: "filesystem" },
      { name: "write_file", description: "Write to a file", input_schema: {}, server: "filesystem" },
      { name: "list_directory", description: "List directory contents", input_schema: {}, server: "filesystem" },
    ];
  }
  return invoke<McpToolInfo[]>("list_mcp_tools", { serverName });
}

export async function callMcpTool(
  serverName: string,
  toolName: string,
  arguments_: any,
): Promise<McpToolCallResult> {
  if (isBrowserDev) {
    return {
      content: [{ content_type: "text", text: `[mock] Tool ${toolName} executed` }],
      is_error: false,
    };
  }
  return invoke<McpToolCallResult>("call_mcp_tool", { serverName, toolName, arguments: arguments_ });
}

export async function getMcpServerStatus(serverName: string): Promise<McpServerInfo> {
  if (isBrowserDev) {
    return { name: serverName, transport: "stdio", connected: true, tool_count: 0 };
  }
  return invoke<McpServerInfo>("get_mcp_server_status", { serverName });
}

export async function listMcpTemplates(): Promise<McpBundledTemplate[]> {
  if (isBrowserDev) {
    return [
      { name: "sqlite", category: "database", description: "SQLite database access via MCP" },
      { name: "filesystem", category: "system", description: "File system operations" },
      { name: "github", category: "devtools", description: "GitHub API integration" },
      { name: "brave-search", category: "search", description: "Brave Search API" },
      { name: "puppeteer", category: "browser", description: "Browser automation" },
    ];
  }
  return invoke<McpBundledTemplate[]>("list_mcp_templates");
}

export async function listMcpCategories(): Promise<string[]> {
  if (isBrowserDev) return ["database", "system", "devtools", "search", "browser"];
  return invoke<string[]>("list_mcp_categories");
}

export async function installMcpTemplate(
  templateName: string,
  envOverrides?: Record<string, string>,
): Promise<McpServerInfo> {
  if (isBrowserDev) {
    return { name: templateName, transport: "stdio", connected: true, tool_count: 0 };
  }
  return invoke<McpServerInfo>("install_mcp_template", { templateName, envOverrides });
}

export async function disconnectAllMcp(): Promise<boolean> {
  if (isBrowserDev) return true;
  return invoke<boolean>("disconnect_all_mcp");
}

// ══════════════════════════════════════════════════════════════
// Extensions — Integration Registry + Config + Vault + Health (24)
// ══════════════════════════════════════════════════════════════

export async function listIntegrations(): Promise<IntegrationInfo[]> {
  if (isBrowserDev) {
    return [
      { name: "github", description: "GitHub API", category: "DevTools", icon: "🐙", enabled: true, credentials_required: [{ name: "token", description: "Personal access token", env_var: "GITHUB_TOKEN", required: true }], has_oauth: true, health_check_url: "https://api.github.com", config_fields: [{ key: "GITHUB_API_URL", label: "API URL", description: "GitHub API endpoint", field_type: "url", default: "https://api.github.com", required: false, placeholder: "https://api.github.com", options: [], group: "Connection" }], config_values: {}, transport_type: "stdio" },
      { name: "slack", description: "Slack workspace", category: "Communication", icon: "💼", enabled: false, credentials_required: [{ name: "bot_token", description: "Bot token", env_var: "SLACK_BOT_TOKEN", required: true }], has_oauth: true, health_check_url: "https://slack.com/api/api.test", config_fields: [], config_values: {}, transport_type: "stdio" },
      { name: "jira", description: "Atlassian Jira", category: "Productivity", icon: "📋", enabled: false, credentials_required: [{ name: "api_token", description: "API token", env_var: "JIRA_API_TOKEN", required: true }], has_oauth: false, health_check_url: undefined, config_fields: [{ key: "JIRA_BASE_URL", label: "Jira URL", description: "Your Jira instance URL", field_type: "url", required: true, placeholder: "https://your-company.atlassian.net", options: [], group: "Connection" }], config_values: {}, transport_type: "api" },
    ];
  }
  return invoke<IntegrationInfo[]>("list_integrations");
}

export async function getIntegrationDetail(name: string): Promise<IntegrationInfo> {
  if (isBrowserDev) {
    return { name, description: `${name} integration`, category: "DevTools", icon: "🔌", enabled: false, credentials_required: [], has_oauth: false, health_check_url: undefined, config_fields: [], config_values: {}, transport_type: "api" };
  }
  return invoke<IntegrationInfo>("get_integration_detail", { name });
}

export async function listIntegrationCategories(): Promise<IntegrationCategoryInfo[]> {
  if (isBrowserDev) {
    return [
      { name: "DevTools", count: 5 },
      { name: "Productivity", count: 4 },
      { name: "Communication", count: 3 },
      { name: "Data", count: 4 },
      { name: "Cloud", count: 3 },
      { name: "Search", count: 2 },
    ];
  }
  return invoke<IntegrationCategoryInfo[]>("list_integration_categories");
}

export async function enableIntegration(name: string): Promise<boolean> {
  if (isBrowserDev) return true;
  return invoke<boolean>("enable_integration", { name });
}

export async function disableIntegration(name: string): Promise<boolean> {
  if (isBrowserDev) return true;
  return invoke<boolean>("disable_integration", { name });
}

export async function getIntegrationStats(): Promise<IntegrationStatsInfo> {
  if (isBrowserDev) return { total: 25, enabled: 3, disabled: 22 };
  return invoke<IntegrationStatsInfo>("get_integration_stats");
}

// ── Extension Configuration ──────────────────────────────────

export async function getExtensionConfig(name: string): Promise<Record<string, string>> {
  if (isBrowserDev) return {};
  return invoke<Record<string, string>>("get_extension_config", { name });
}

export async function saveExtensionConfig(name: string, values: Record<string, string>): Promise<boolean> {
  if (isBrowserDev) return true;
  return invoke<boolean>("save_extension_config", { name, values });
}

export async function validateExtensionConfig(name: string): Promise<string[]> {
  if (isBrowserDev) return [];
  return invoke<string[]>("validate_extension_config", { name });
}

export async function storeExtensionCredential(integrationName: string, credentialName: string, value: string): Promise<boolean> {
  if (isBrowserDev) return true;
  return invoke<boolean>("store_extension_credential", { integrationName, credentialName, value });
}

export async function checkExtensionCredentials(name: string): Promise<Record<string, boolean>> {
  if (isBrowserDev) return {};
  return invoke<Record<string, boolean>>("check_extension_credentials", { name });
}

export async function vaultStatus(): Promise<VaultStatusInfo> {
  if (isBrowserDev) return { exists: true, unlocked: false, credential_count: 0 };
  return invoke<VaultStatusInfo>("vault_status");
}

export async function vaultInitialize(password: string): Promise<boolean> {
  if (isBrowserDev) return true;
  return invoke<boolean>("vault_initialize", { password });
}

export async function vaultUnlock(password: string): Promise<boolean> {
  if (isBrowserDev) return true;
  return invoke<boolean>("vault_unlock", { password });
}

export async function vaultLock(): Promise<boolean> {
  if (isBrowserDev) return true;
  return invoke<boolean>("vault_lock");
}

export async function vaultStoreCredential(name: string, value: string): Promise<boolean> {
  if (isBrowserDev) return true;
  return invoke<boolean>("vault_store_credential", { name, value });
}

export async function vaultGetCredential(name: string): Promise<string | null> {
  if (isBrowserDev) return null;
  return invoke<string | null>("vault_get_credential", { name });
}

export async function vaultDeleteCredential(name: string): Promise<boolean> {
  if (isBrowserDev) return true;
  return invoke<boolean>("vault_delete_credential", { name });
}

export async function vaultListCredentials(): Promise<string[]> {
  if (isBrowserDev) return ["github_token", "slack_bot_token"];
  return invoke<string[]>("vault_list_credentials");
}

export async function getAllHealthStatuses(): Promise<HealthStatusInfo[]> {
  if (isBrowserDev) {
    return [
      { name: "github", state: "Healthy", last_check: new Date().toISOString(), last_success: new Date().toISOString(), consecutive_failures: 0, latency_ms: 120 },
      { name: "slack", state: "Unknown", consecutive_failures: 0 },
    ];
  }
  return invoke<HealthStatusInfo[]>("get_all_health_statuses");
}

export async function getIntegrationHealth(name: string): Promise<HealthStatusInfo> {
  if (isBrowserDev) {
    return { name, state: "Unknown", consecutive_failures: 0 };
  }
  return invoke<HealthStatusInfo>("get_integration_health", { name });
}

export async function checkIntegrationHealth(name: string): Promise<HealthStatusInfo> {
  if (isBrowserDev) {
    return { name, state: "Healthy", last_check: new Date().toISOString(), last_success: new Date().toISOString(), consecutive_failures: 0, latency_ms: 150 };
  }
  return invoke<HealthStatusInfo>("check_integration_health", { name });
}

export async function startExtensionOAuth(integrationName: string): Promise<OAuthFlowInfo> {
  if (isBrowserDev) {
    return { auth_url: "https://example.com/oauth/authorize?mock=true", state: "mock_state_123" };
  }
  return invoke<OAuthFlowInfo>("start_extension_oauth", { integrationName });
}

export async function completeExtensionOAuth(
  integrationName: string,
  code: string,
  stateParam: string,
): Promise<boolean> {
  if (isBrowserDev) return true;
  return invoke<boolean>("complete_extension_oauth", { integrationName, code, stateParam });
}

// ══════════════════════════════════════════════════════════════
// Migration — Import from Other AI Apps (4)
// ══════════════════════════════════════════════════════════════

export async function listMigrationSources(): Promise<MigrationSourceInfo[]> {
  if (isBrowserDev) {
    return [
      { name: "openclaw", label: "OpenClaw", supported_items: ["Agents", "Sessions", "Skills", "Channels", "Credentials", "Config"] },
      { name: "claude_desktop", label: "Claude Desktop", supported_items: ["Agents", "Sessions", "Skills", "Channels", "Credentials", "Config"] },
    ];
  }
  return invoke<MigrationSourceInfo[]>("list_migration_sources");
}

export async function validateMigrationSource(
  source: string,
  sourcePath: string,
): Promise<ValidateSourceResult> {
  if (isBrowserDev) {
    return { valid: true, source, found_items: ["Agents", "Skills", "Config"], error: undefined };
  }
  return invoke<ValidateSourceResult>("validate_migration_source", { source, sourcePath });
}

export async function runMigration(request: MigrationRequest): Promise<MigrationReportInfo> {
  if (isBrowserDev) {
    return {
      source: request.source,
      source_path: request.source_path,
      dry_run: request.dry_run,
      success: true,
      summary: { total: 5, migrated: 4, skipped: 1, failed: 0, dry_run: request.dry_run ? 5 : 0 },
      items: [
        { category: "Agents", source_name: "default-agent.yaml", dest_path: "agents/default-agent.toml", status: "Migrated", note: "" },
        { category: "Skills", source_name: "web-search.md", dest_path: "skills/web-search.md", status: "Migrated", note: "" },
      ],
      warnings: [],
      errors: [],
    };
  }
  return invoke<MigrationReportInfo>("run_migration", { request });
}

export async function previewMigration(source: string, sourcePath: string): Promise<MigrationReportInfo> {
  if (isBrowserDev) {
    return runMigration({ source, source_path: sourcePath, dry_run: true, overwrite: false });
  }
  return invoke<MigrationReportInfo>("preview_migration", { source, sourcePath });
}

// ── ask_human response ────────────────────────────────────────
export async function respondToAskHuman(requestId: string, response: string): Promise<boolean> {
  return invoke<boolean>("respond_to_ask_human", { requestId, response });
}

// ── Workspace file browser ────────────────────────────────────
import type { WorkspaceFileEntry } from "./types";

export async function listWorkspaceFiles(relativePath?: string): Promise<WorkspaceFileEntry[]> {
  if (isBrowserDev) {
    return [
      { name: "src/", path: "src", is_dir: true, size: 0, modified: new Date().toISOString() },
      { name: "package.json", path: "package.json", is_dir: false, size: 1024, modified: new Date().toISOString() },
    ];
  }
  return invoke<WorkspaceFileEntry[]>("list_workspace_files", { relativePath: relativePath ?? null });
}

export async function readWorkspaceFile(relativePath: string): Promise<string> {
  if (isBrowserDev) return "// browser-dev mock file content";
  return invoke<string>("read_workspace_file", { relativePath });
}

export async function getWorkspaceRoot(): Promise<string> {
  if (isBrowserDev) return "/mock/workspace";
  return invoke<string>("get_workspace_root");
}

// ── Local Models: built-in LLM management ─────────────────────

export interface SystemSpecs {
  total_ram_gb: number;
  available_ram_gb: number;
  total_cpu_cores: number;
  cpu_name: string;
  has_gpu: boolean;
  gpu_vram_gb: number | null;
  total_gpu_vram_gb: number | null;
  gpu_name: string | null;
  gpu_count: number;
  unified_memory: boolean;
  backend: string;
  gpus: { name: string; vram_gb: number; index: number }[];
}

export interface ModelFit {
  model: {
    name: string;
    provider: string;
    parameter_count: string;
    parameters_raw: number;
    context_length: number;
    use_case: string;
    gguf_repo: string;
    gguf_filename_pattern: string;
  };
  fit_level: "perfect" | "good" | "marginal" | "too_tight";
  run_mode: "gpu" | "cpu_offload" | "cpu_only";
  memory_required_gb: number;
  memory_available_gb: number;
  utilization_pct: number;
  best_quant: string;
  estimated_tps: number;
  score: number;
  use_case: string;
  gguf_download_url: string;
  installed: boolean;
}

export interface RunningModel {
  name: string;
  state: "stopped" | "starting" | "ready" | "stopping" | "failed";
  port: number;
  model_path: string;
  pid: number | null;
}

export interface DownloadedModel {
  name: string;
  path: string;
  size_gb: number;
}

export interface LocalModelsStatus {
  system: SystemSpecs;
  llama_server_available: boolean;
  models_dir: string;
  downloaded_models: DownloadedModel[];
  running_models: RunningModel[];
  recommended_models: ModelFit[];
}

export async function localModelsStatus(): Promise<LocalModelsStatus> {
  if (isBrowserDev) {
    return {
      system: { total_ram_gb: 32, available_ram_gb: 24, total_cpu_cores: 10, cpu_name: "Apple M2 Max", has_gpu: true, gpu_vram_gb: 32, total_gpu_vram_gb: 32, gpu_name: "Apple M2 Max", gpu_count: 1, unified_memory: true, backend: "metal", gpus: [{ name: "Apple M2 Max", vram_gb: 32, index: 0 }] },
      llama_server_available: false,
      models_dir: "~/.clawdesk/models",
      downloaded_models: [],
      running_models: [],
      recommended_models: [],
    };
  }
  return invoke<LocalModelsStatus>("local_models_status");
}

export async function localModelsSystemInfo(): Promise<SystemSpecs> {
  if (isBrowserDev) return { total_ram_gb: 32, available_ram_gb: 24, total_cpu_cores: 10, cpu_name: "Apple M2 Max", has_gpu: true, gpu_vram_gb: 32, total_gpu_vram_gb: 32, gpu_name: "Apple M2 Max", gpu_count: 1, unified_memory: true, backend: "metal", gpus: [] };
  return invoke<SystemSpecs>("local_models_system_info");
}

export async function localModelsRecommend(): Promise<ModelFit[]> {
  if (isBrowserDev) return [];
  return invoke<ModelFit[]>("local_models_recommend");
}

export async function localModelsStart(modelName: string): Promise<number> {
  return invoke<number>("local_models_start", { request: { model_name: modelName } });
}

export async function localModelsStop(modelName: string): Promise<void> {
  return invoke<void>("local_models_stop", { request: { model_name: modelName } });
}

export async function localModelsRunning(): Promise<RunningModel[]> {
  if (isBrowserDev) return [];
  return invoke<RunningModel[]>("local_models_running");
}

export async function localModelsDownload(modelName: string, downloadUrl: string): Promise<void> {
  return invoke<void>("local_models_download", { request: { model_name: modelName, download_url: downloadUrl } });
}

export async function localModelsDelete(modelName: string): Promise<void> {
  return invoke<void>("local_models_delete", { request: { model_name: modelName } });
}

export async function localModelsSetServerPath(path: string): Promise<void> {
  return invoke<void>("local_models_set_server_path", { request: { path } });
}

export async function localModelsScanDirectory(directory: string): Promise<ScannedModel[]> {
  return invoke<ScannedModel[]>("local_models_scan_directory", { request: { directory } });
}

export async function localModelsImport(sourcePath: string): Promise<string> {
  return invoke<string>("local_models_import", { request: { source_path: sourcePath } });
}

export async function localModelsSetTtl(ttlSecs: number): Promise<void> {
  return invoke<void>("local_models_set_ttl", { request: { ttl_secs: ttlSecs } });
}

export interface ScannedModel {
  name: string;
  path: string;
  size_gb: number;
  already_imported: boolean;
}

// ── RAG: Document ingestion & retrieval ─────────────────────────

export interface RagDocument {
  id: string;
  filename: string;
  file_path: string;
  doc_type: "pdf" | "text" | "markdown" | "csv";
  size_bytes: number;
  word_count: number;
  chunk_count: number;
  created_at: string;
}

export interface RagSearchResult {
  doc_id: string;
  filename: string;
  chunk_index: number;
  chunk_text: string;
  similarity: number;
}

export async function ragIngestDocument(filePath: string): Promise<RagDocument> {
  return invoke<RagDocument>("rag_ingest_document", { request: { file_path: filePath } });
}

export async function ragListDocuments(): Promise<RagDocument[]> {
  if (isBrowserDev) return [];
  return invoke<RagDocument[]>("rag_list_documents");
}

export async function ragDeleteDocument(docId: string): Promise<void> {
  return invoke<void>("rag_delete_document", { request: { doc_id: docId } });
}

export async function ragSearch(query: string, topK?: number): Promise<RagSearchResult[]> {
  return invoke<RagSearchResult[]>("rag_search", { request: { query, top_k: topK } });
}

export async function ragGetChunks(docId: string): Promise<string[]> {
  return invoke<string[]>("rag_get_chunks", { request: { doc_id: docId } });
}

export async function ragBuildContext(query: string, topK?: number, maxChars?: number): Promise<string> {
  return invoke<string>("rag_build_context", { request: { query, top_k: topK, max_chars: maxChars } });
}

// ── Preview: live web app preview ───────────────────────────

export interface PreviewService {
  id: string;
  label: string;
  url: string;
  port: number;
  created_at: string;
}

export async function previewRegister(id: string, label: string, port: number): Promise<PreviewService> {
  return invoke<PreviewService>("preview_register", { request: { id, label, port } });
}

export async function previewList(): Promise<PreviewService[]> {
  if (isBrowserDev) return [];
  return invoke<PreviewService[]>("preview_list");
}

export async function previewRemove(id: string): Promise<void> {
  return invoke<void>("preview_remove", { request: { id } });
}

export async function previewCheckPort(port: number): Promise<boolean> {
  return invoke<boolean>("preview_check_port", { request: { port } });
}
