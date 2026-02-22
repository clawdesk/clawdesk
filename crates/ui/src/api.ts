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
    id: "IMessage", label: "iMessage", icon: "💬", blurb: "Apple iMessage bridge via command-line tool", docs_url: "",
    configFields: [
      { key: "cli_path", label: "CLI Path", type: "text", placeholder: "/usr/local/bin/imessage-bridge", help: "Path to the iMessage CLI bridge binary", required: true },
      { key: "db_path", label: "Database Path", type: "text", placeholder: "~/Library/Messages/chat.db", help: "Path to the iMessage database" },
    ], capabilities: ["direct", "group", "media", "reactions"]
  },
  {
    id: "Telegram", label: "Telegram", icon: "✈️", blurb: "Telegram Bot API for direct and group chats", docs_url: "https://core.telegram.org/bots",
    configFields: [
      { key: "bot_token", label: "Bot Token", type: "password", placeholder: "123456:ABC-DEF...", help: "Token from @BotFather", required: true },
      { key: "webhook_url", label: "Webhook URL", type: "url", placeholder: "https://your-domain.com/webhook/telegram", help: "Public URL for incoming updates (leave blank for polling)" },
      { key: "mode", label: "Mode", type: "select", options: ["polling", "webhook"], help: "How to receive updates" },
    ], capabilities: ["direct", "group", "media", "threads", "reactions"]
  },
  {
    id: "Discord", label: "Discord", icon: "🎮", blurb: "Discord bot for server and DM messaging", docs_url: "https://discord.com/developers/docs",
    configFields: [
      { key: "bot_token", label: "Bot Token", type: "password", placeholder: "MTk....", help: "Discord bot token from Developer Portal", required: true },
      { key: "application_id", label: "Application ID", type: "text", placeholder: "123456789012345678", help: "Discord application ID" },
    ], capabilities: ["direct", "group", "media", "threads", "reactions"]
  },
  {
    id: "Slack", label: "Slack", icon: "💼", blurb: "Slack workspace integration via Bot + App tokens", docs_url: "https://api.slack.com/",
    configFields: [
      { key: "bot_token", label: "Bot Token", type: "password", placeholder: "xoxb-...", help: "Slack bot user OAuth token", required: true },
      { key: "app_token", label: "App Token", type: "password", placeholder: "xapp-...", help: "Slack app-level token for Socket Mode", required: true },
      { key: "signing_secret", label: "Signing Secret", type: "password", placeholder: "", help: "Slack request signing secret" },
    ], capabilities: ["direct", "group", "media", "threads", "reactions"]
  },
  {
    id: "WhatsApp", label: "WhatsApp", icon: "📱", blurb: "WhatsApp Business API or Web bridge", docs_url: "https://developers.facebook.com/docs/whatsapp",
    configFields: [
      { key: "phone_number_id", label: "Phone Number ID", type: "text", placeholder: "", help: "WhatsApp Business phone number ID", required: true },
      { key: "access_token", label: "Access Token", type: "password", placeholder: "EAAG...", help: "Meta Graph API access token", required: true },
      { key: "verify_token", label: "Verify Token", type: "text", placeholder: "", help: "Webhook verification token" },
    ], capabilities: ["direct", "group", "media"]
  },
  {
    id: "Signal", label: "Signal", icon: "🔒", blurb: "Signal messenger via signal-cli REST API", docs_url: "https://signal.org/",
    configFields: [
      { key: "base_url", label: "API Base URL", type: "url", placeholder: "http://localhost:8080", help: "Signal CLI REST API endpoint", required: true },
      { key: "phone_number", label: "Phone Number", type: "text", placeholder: "+1234567890", help: "Registered Signal phone number", required: true },
    ], capabilities: ["direct", "group", "media", "reactions"]
  },
  {
    id: "Matrix", label: "Matrix", icon: "🟩", blurb: "Matrix/Element homeserver integration", docs_url: "https://matrix.org/",
    configFields: [
      { key: "homeserver", label: "Homeserver URL", type: "url", placeholder: "https://matrix.org", help: "Matrix homeserver base URL", required: true },
      { key: "access_token", label: "Access Token", type: "password", placeholder: "", help: "Matrix access token", required: true },
      { key: "user_id", label: "User ID", type: "text", placeholder: "@bot:matrix.org", help: "Matrix user ID for the bot" },
    ], capabilities: ["direct", "group", "media", "threads", "reactions"]
  },
  {
    id: "Email", label: "Email", icon: "📧", blurb: "IMAP/SMTP email integration", docs_url: "",
    configFields: [
      { key: "imap_host", label: "IMAP Host", type: "text", placeholder: "imap.gmail.com", help: "IMAP server hostname", required: true },
      { key: "smtp_host", label: "SMTP Host", type: "text", placeholder: "smtp.gmail.com", help: "SMTP server hostname", required: true },
      { key: "email", label: "Email Address", type: "text", placeholder: "bot@example.com", required: true },
      { key: "password", label: "Password", type: "password", placeholder: "", help: "Email account password or app password", required: true },
    ], capabilities: ["direct", "media"]
  },
  {
    id: "MsTeams", label: "MS Teams", icon: "🟦", blurb: "Microsoft Teams bot via Azure Bot Service", docs_url: "https://learn.microsoft.com/en-us/microsoftteams/",
    configFields: [
      { key: "app_id", label: "App ID", type: "text", placeholder: "", help: "Azure Bot app registration ID", required: true },
      { key: "app_secret", label: "App Secret", type: "password", placeholder: "", help: "Azure Bot app secret", required: true },
      { key: "tenant_id", label: "Tenant ID", type: "text", placeholder: "", help: "Azure AD tenant ID" },
    ], capabilities: ["direct", "group", "media", "threads", "reactions"]
  },
  {
    id: "GoogleChat", label: "Google Chat", icon: "💚", blurb: "Google Workspace Chat via service account", docs_url: "https://developers.google.com/chat",
    configFields: [
      { key: "credentials_json", label: "Service Account JSON", type: "password", placeholder: "", help: "Google service account credentials JSON", required: true },
      { key: "webhook_url", label: "Webhook URL", type: "url", placeholder: "", help: "Google Chat webhook URL" },
    ], capabilities: ["direct", "group", "threads"]
  },
  {
    id: "Nostr", label: "Nostr", icon: "🟣", blurb: "Nostr protocol relay messaging", docs_url: "https://nostr.com/",
    configFields: [
      { key: "private_key", label: "Private Key (nsec)", type: "password", placeholder: "nsec1...", help: "Nostr private key", required: true },
      { key: "relays", label: "Relays", type: "text", placeholder: "wss://relay.damus.io,wss://nos.lol", help: "Comma-separated relay URLs" },
    ], capabilities: ["direct", "group"]
  },
  {
    id: "Irc", label: "IRC", icon: "📟", blurb: "IRC server connection", docs_url: "",
    configFields: [
      { key: "server", label: "Server", type: "text", placeholder: "irc.libera.chat", help: "IRC server hostname", required: true },
      { key: "port", label: "Port", type: "text", placeholder: "6697", help: "IRC server port" },
      { key: "nick", label: "Nickname", type: "text", placeholder: "clawdesk-bot", help: "Bot nickname", required: true },
      { key: "channels", label: "Channels", type: "text", placeholder: "#general,#dev", help: "Comma-separated channels to join" },
      { key: "password", label: "Password", type: "password", placeholder: "", help: "NickServ/server password" },
    ], capabilities: ["direct", "group"]
  },
  {
    id: "Mattermost", label: "Mattermost", icon: "🔵", blurb: "Self-hosted Mattermost server integration", docs_url: "https://developers.mattermost.com/",
    configFields: [
      { key: "url", label: "Server URL", type: "url", placeholder: "https://mattermost.example.com", help: "Mattermost server base URL", required: true },
      { key: "bot_token", label: "Bot Token", type: "password", placeholder: "", help: "Mattermost bot access token", required: true },
    ], capabilities: ["direct", "group", "media", "threads", "reactions"]
  },
  {
    id: "Line", label: "LINE", icon: "🟢", blurb: "LINE Messaging API", docs_url: "https://developers.line.biz/",
    configFields: [
      { key: "channel_access_token", label: "Channel Access Token", type: "password", placeholder: "", help: "LINE channel access token", required: true },
      { key: "channel_secret", label: "Channel Secret", type: "password", placeholder: "", help: "LINE channel secret", required: true },
    ], capabilities: ["direct", "group", "media"]
  },
  {
    id: "Feishu", label: "Feishu/Lark", icon: "🪶", blurb: "Feishu (Lark) enterprise messaging", docs_url: "https://open.feishu.cn/",
    configFields: [
      { key: "app_id", label: "App ID", type: "text", placeholder: "", help: "Feishu app ID", required: true },
      { key: "app_secret", label: "App Secret", type: "password", placeholder: "", help: "Feishu app secret", required: true },
    ], capabilities: ["direct", "group", "media", "threads"]
  },
  {
    id: "Twitch", label: "Twitch", icon: "🟪", blurb: "Twitch IRC chat integration", docs_url: "https://dev.twitch.tv/",
    configFields: [
      { key: "oauth_token", label: "OAuth Token", type: "password", placeholder: "oauth:...", help: "Twitch OAuth token", required: true },
      { key: "channel", label: "Channel", type: "text", placeholder: "your_channel", help: "Twitch channel to join", required: true },
      { key: "bot_name", label: "Bot Username", type: "text", placeholder: "clawdesk_bot" },
    ], capabilities: ["group"]
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
      { id: "imessage", name: "iMessage", channel_type: "IMessage", status: "active", configured: true, config: { cli_path: "/usr/local/bin/imessage-bridge", db_path: "~/Library/Messages/chat.db" }, capabilities: ["direct", "group", "media", "reactions"], docs_url: "" },
      { id: "telegram", name: "Telegram", channel_type: "Telegram", status: "available", configured: false, config: {}, capabilities: ["direct", "group", "media", "threads", "reactions"], docs_url: "https://core.telegram.org/bots" },
      { id: "discord", name: "Discord", channel_type: "Discord", status: "available", configured: false, config: {}, capabilities: ["direct", "group", "media", "threads", "reactions"], docs_url: "https://discord.com/developers/docs" },
      { id: "slack", name: "Slack", channel_type: "Slack", status: "available", configured: false, config: {}, capabilities: ["direct", "group", "media", "threads", "reactions"], docs_url: "https://api.slack.com/" },
      { id: "whatsapp", name: "WhatsApp", channel_type: "WhatsApp", status: "available", configured: false, config: {}, capabilities: ["direct", "group", "media"], docs_url: "https://developers.facebook.com/docs/whatsapp" },
      { id: "signal", name: "Signal", channel_type: "Signal", status: "available", configured: false, config: {}, capabilities: ["direct", "group", "media", "reactions"], docs_url: "https://signal.org/" },
      { id: "matrix", name: "Matrix", channel_type: "Matrix", status: "available", configured: false, config: {}, capabilities: ["direct", "group", "media", "threads", "reactions"], docs_url: "https://matrix.org/" },
      { id: "email", name: "Email", channel_type: "Email", status: "available", configured: false, config: {}, capabilities: ["direct", "media"], docs_url: "" },
      { id: "msteams", name: "MS Teams", channel_type: "MsTeams", status: "available", configured: false, config: {}, capabilities: ["direct", "group", "media", "threads", "reactions"], docs_url: "https://learn.microsoft.com/en-us/microsoftteams/" },
      { id: "googlechat", name: "Google Chat", channel_type: "GoogleChat", status: "available", configured: false, config: {}, capabilities: ["direct", "group", "threads"], docs_url: "https://developers.google.com/chat" },
      { id: "nostr", name: "Nostr", channel_type: "Nostr", status: "available", configured: false, config: {}, capabilities: ["direct", "group"], docs_url: "https://nostr.com/" },
      { id: "irc", name: "IRC", channel_type: "Irc", status: "available", configured: false, config: {}, capabilities: ["direct", "group"], docs_url: "" },
      { id: "mattermost", name: "Mattermost", channel_type: "Mattermost", status: "available", configured: false, config: {}, capabilities: ["direct", "group", "media", "threads", "reactions"], docs_url: "https://developers.mattermost.com/" },
      { id: "line", name: "LINE", channel_type: "Line", status: "available", configured: false, config: {}, capabilities: ["direct", "group", "media"], docs_url: "https://developers.line.biz/" },
      { id: "feishu", name: "Feishu/Lark", channel_type: "Feishu", status: "available", configured: false, config: {}, capabilities: ["direct", "group", "media", "threads"], docs_url: "https://open.feishu.cn/" },
      { id: "twitch", name: "Twitch", channel_type: "Twitch", status: "available", configured: false, config: {}, capabilities: ["group"], docs_url: "https://dev.twitch.tv/" },
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
  edges: [number, number][]
): Promise<PipelineDescriptor> {
  return invoke<PipelineDescriptor>("create_pipeline", {
    request: { name, description, steps, edges },
  });
}

export async function runPipeline(pipelineId: string): Promise<PipelineRunResult> {
  return invoke<PipelineRunResult>("run_pipeline", { pipelineId });
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

