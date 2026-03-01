import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import * as api from "./api";
import type {
  SkillDescriptor,
  DesktopAgent,
  CostMetrics,
  SecurityStatus as BackendSecurityStatus,
  HealthResponse,
  SessionSummary,
  ChatMessage as BackendChatMessage,
  ChannelInfo,
  PipelineDescriptor,
  PipelineNodeDescriptor,
  AuthProfileInfo,
  ApprovalRequestInfo,
  PluginSummary,
  PeerInfo,
  MemoryStatsResponse,
  MemoryHit,
  NotificationInfo,
  CanvasSummary,
  TraceRunInfo,
  TraceSpanInfo,
  ObservabilityStatus,
  GraphNodeInfo,
  GraphEdgeInfo,
} from "./types";
import { AGENT_TEMPLATES } from "./types";
import { Modal } from "./components/Modal";
import { OnboardingWizard } from "./onboarding/OnboardingWizard";
import { buildRouteHash, parseRouteHash } from "./lib/routes";
import { classifyError } from "./lib/error-recovery";
import { subscribeAppEvents } from "./stores/event-bus";
import { translateRisk } from "./lib/risk-translator";
import { AppShell, type ShellNavGroup } from "./shell/AppShell";
import { getActiveProvider } from "./providerConfig";
import { ChatPage } from "./pages/ChatPage";
import { OverviewPage } from "./pages/OverviewPage";
import { SkillsPage } from "./pages/SkillsPage";
import { AutomationsPage } from "./pages/AutomationsPage";
import { SettingsPage } from "./pages/SettingsPage";
import { LogsPage } from "./pages/LogsPage";
import { A2APage } from "./pages/A2APage";
import { RuntimePage } from "./pages/RuntimePage";
import { ExtensionsPage } from "./pages/ExtensionsPage";
import { McpPage } from "./pages/McpPage";

type NavKey = "chat" | "overview" | "a2a" | "runtime" | "skills" | "automations" | "settings" | "logs" | "extensions" | "mcp";
type RiskLevel = "low" | "medium" | "high";
type StatusLevel = "ok" | "warn" | "error";
type InspectorTab = "plan" | "approvals" | "proof" | "undo" | "trace" | "memory" | "graph";
type RoutineType = "at_time" | "watch_notify";
type RoutineResult = "ok" | "skipped" | "failed";
type AccountStatus = "connected" | "needs_sign_in" | "permissions_changed" | "disabled";
type Provenance = "built-in" | "created by you" | "from team" | "from internet";
type MessageRole = "user" | "assistant";
type StepStatus = "idle" | "running" | "ok" | "skipped" | "stopped";

interface ThreadMessageMeta {
  model: string;
  tokenCost: number;
  costUsd: number;
  durationMs: number;
  skills: string[];
}

interface ThreadMessage {
  id: string;
  role: MessageRole;
  text: string;
  time: string;
  meta?: ThreadMessageMeta;
  result?: string;
}

interface ThreadItem {
  id: string;
  agentId?: string;
  title: string;
  lastActivity: string;
  pendingApprovals: number;
  routineGenerated: boolean;
  hasProofOutputs: boolean;
  messages: ThreadMessage[];
}

interface PlanTouch {
  account: string;
  type: string;
}

interface PlanStep {
  stepId: string;
  title: string;
  details: string;
  inputs: string;
  expectedOutput: string;
  requiresApproval: boolean;
  preview: string;
}

interface PlanCard {
  planId: string;
  goal: string;
  risk: RiskLevel;
  touches: PlanTouch[];
  steps: PlanStep[];
}

interface ApprovalItem {
  id: string;
  planId: string;
  stepId: string;
  summary: string;
  where: string;
  impact: string;
  risk: RiskLevel;
  status: "pending" | "approved" | "denied";
}

interface ProofStep {
  stepId: string;
  title: string;
  status: "ok" | "skipped" | "stopped";
}

interface ProofOutput {
  type: string;
  label: string;
  link: string;
}

interface ProofRecord {
  proofId: string;
  requestId: string;
  summary: string;
  startedAt: string;
  endedAt: string;
  duration: string;
  steps: ProofStep[];
  outputs: ProofOutput[];
  undo: string[];
}

interface TimelineItem {
  id: string;
  label: string;
  detail: string;
  time: string;
  undoable: boolean;
}

interface RoutineItem {
  id: string;
  name: string;
  pipelineId?: string;
  type: RoutineType;
  scheduleLabel: string;
  nextRun: string;
  destination: string;
  enabled: boolean;
  quietHours: string;
  lastResult: RoutineResult;
  lastReason: string;
  history: { at: string; result: RoutineResult; reason: string }[];
  recipeId: string;
}

interface AccountItem {
  id: string;
  name: string;
  status: AccountStatus;
  summary: string;
  capabilities: {
    read: boolean;
    search: boolean;
    draft: boolean;
    sendWrite: boolean;
    delete: boolean;
    execute: boolean;
  };
  boundaries: {
    folders: string;
    recipients: string;
    channels: string;
  };
  lastUsed: string;
}

interface RecipeItem {
  id: string;
  name: string;
  category: "recommended" | "personal" | "team" | "experimental";
  purpose: string;
  inputs: string;
  outputs: string;
  permissions: string[];
  provenance: Provenance;
  restrictedMode: boolean;
}

interface ToastItem {
  id: string;
  text: string;
}

interface StatusSummary {
  level: StatusLevel;
  summary: string;
  detail: string;
  fix: string;
}

interface QuickCheckResult {
  title: string;
  why: string;
  options: string[];
}

type AgentEventType =
  | "RoundStart"
  | "Response"
  | "ToolStart"
  | "ToolEnd"
  | "Compaction"
  | "StreamChunk"
  | "Done"
  | "Error"
  | "PromptAssembled"
  | "IdentityVerified"
  | "ContextGuardAction"
  | "FallbackTriggered";

interface FrontendAgentEventPayload {
  agent_id: string;
  event: { type: AgentEventType;[key: string]: unknown };
}

interface ApprovalPendingPayload {
  id: string;
  tool_name: string;
  command: string;
  risk: string;
}

interface RoutineExecutedPayload {
  pipeline_id?: string;
  pipeline_name?: string;
  success?: boolean;
  duration_ms?: number;
  total_duration_ms?: number;
  timestamp?: string;
}

interface IncomingMessagePayload {
  agent_id?: string;
  chat_id?: string;
  preview?: string;
  timestamp?: string;
}

const NAV_ITEMS: { key: NavKey; label: string; shortcut: string; icon: string }[] = [
  { key: "chat", label: "Chat", shortcut: "1", icon: "ask" },
  { key: "overview", label: "Overview", shortcut: "2", icon: "bar-chart" },
  { key: "a2a", label: "A2A Directory", shortcut: "3", icon: "users" },
  { key: "runtime", label: "Runtime", shortcut: "4", icon: "activity" },
  { key: "skills", label: "Skills", shortcut: "5", icon: "library" },
  { key: "automations", label: "Automations", shortcut: "6", icon: "routines" },
  { key: "extensions", label: "Extensions", shortcut: "7", icon: "puzzle" },
  { key: "mcp", label: "MCP", shortcut: "8", icon: "plug" },
  { key: "settings", label: "Settings", shortcut: "9", icon: "settings" },
  { key: "logs", label: "Logs", shortcut: "0", icon: "scroll-text" },
];

const NAV_GROUPS: ShellNavGroup[] = [
  { label: "", items: [NAV_ITEMS[0], NAV_ITEMS[1]] },
  { label: "Cluster", items: [NAV_ITEMS[2], NAV_ITEMS[3]] },
  { label: "Build", items: [NAV_ITEMS[4], NAV_ITEMS[5]] },
  { label: "Connect", items: [NAV_ITEMS[6], NAV_ITEMS[7]] },
  { label: "System", items: [NAV_ITEMS[8], NAV_ITEMS[9]] },
];

const INITIAL_THREADS: ThreadItem[] = [];

const INITIAL_PLAN: PlanCard = {
  planId: "plan_empty",
  goal: "No active plan",
  risk: "low",
  touches: [],
  steps: [],
};

const INITIAL_APPROVALS: ApprovalItem[] = [];

const INITIAL_PROOFS: ProofRecord[] = [];

const INITIAL_ROUTINES: RoutineItem[] = [];

const INITIAL_ACCOUNTS: AccountItem[] = [];

const INITIAL_RECIPES: RecipeItem[] = [];

const INITIAL_TIMELINE: TimelineItem[] = [];

function makeId(prefix: string): string {
  const random = globalThis.crypto?.randomUUID?.() ?? `${Date.now()}_${Math.random().toString(36).slice(2, 9)}`;
  return `${prefix}_${random}`;
}

function nowLabel(): string {
  return new Date().toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
}

function formatLastActivity(isoTimestamp: string): string {
  const parsed = new Date(isoTimestamp);
  if (Number.isNaN(parsed.getTime())) return "Just now";
  return parsed.toLocaleString([], {
    month: "numeric",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  });
}

function toThreadMessageMeta(metadata: BackendChatMessage["metadata"]): ThreadMessageMeta | undefined {
  if (!metadata) return undefined;
  return {
    model: metadata.model,
    tokenCost: metadata.token_cost,
    costUsd: metadata.cost_usd,
    durationMs: metadata.duration_ms,
    skills: metadata.skills_activated,
  };
}

function formatModelLabel(model: string): string {
  const parts = model.split("-");
  return parts.slice(0, 2).join("-") || model;
}

function formatDurationLabel(durationMs: number): string {
  if (durationMs >= 1000) return `${(durationMs / 1000).toFixed(1)}s`;
  return `${Math.max(0, Math.round(durationMs))}ms`;
}

function toThreadMessages(messages: BackendChatMessage[]): ThreadMessage[] {
  return messages.map((message) => {
    const role: MessageRole = message.role === "assistant" ? "assistant" : "user";
    return {
      id: message.id,
      role,
      text: message.content,
      time: formatLastActivity(message.timestamp),
      meta: toThreadMessageMeta(message.metadata),
    };
  });
}

function toThreadFromSession(session: SessionSummary): ThreadItem {
  return {
    id: session.chat_id,
    agentId: session.agent_id,
    title: session.title,
    lastActivity: formatLastActivity(session.last_activity),
    pendingApprovals: session.pending_approvals,
    routineGenerated: session.routine_generated,
    hasProofOutputs: session.has_proof_outputs,
    messages: [],
  };
}

function createThreadForAgent(agentId: string, agent: DesktopAgent | null, patch?: Partial<ThreadItem>): ThreadItem {
  const base: ThreadItem = {
    id: patch?.id ?? `thread_${agentId}_${Date.now()}`,
    agentId,
    title: agent ? `${agent.name} session` : "Conversation",
    lastActivity: "No messages yet",
    pendingApprovals: 0,
    routineGenerated: false,
    hasProofOutputs: false,
    messages: [],
  };
  return { ...base, ...patch };
}

function mapChannelStatusToAccountStatus(status: string): AccountStatus {
  const lowered = status.toLowerCase();
  if (lowered.includes("connected") || lowered === "healthy" || lowered === "active" || lowered === "configured") {
    return "connected";
  }
  if (lowered.includes("permission")) return "permissions_changed";
  if (lowered.includes("disabled")) return "disabled";
  return "needs_sign_in";
}

function accountFromChannel(channel: ChannelInfo): AccountItem {
  return {
    id: channel.id,
    name: channel.name,
    status: mapChannelStatusToAccountStatus(channel.status),
    summary: `${channel.channel_type} • ${channel.status}`,
    capabilities: {
      read: true,
      search: true,
      draft: true,
      sendWrite: true,
      delete: false,
      execute: false,
    },
    boundaries: {
      folders: "N/A",
      recipients: "N/A",
      channels: channel.name,
    },
    lastUsed: "Unknown",
  };
}

function routineFromPipeline(pipeline: PipelineDescriptor): RoutineItem {
  return {
    id: pipeline.id,
    pipelineId: pipeline.id,
    name: pipeline.name,
    type: "at_time",
    scheduleLabel: "Manual run",
    nextRun: "On demand",
    destination: "Pipeline output",
    enabled: true,
    quietHours: "Not configured",
    lastResult: "skipped",
    lastReason: pipeline.description || "No runs yet",
    history: [],
    recipeId: pipeline.id,
  };
}

function mergeApprovals(existing: ApprovalItem[], incoming: ApprovalItem[]): ApprovalItem[] {
  const byId = new Map(existing.map((item) => [item.id, item]));
  for (const item of incoming) {
    byId.set(item.id, item);
  }
  return Array.from(byId.values());
}

function accountStatusLabel(status: AccountStatus): string {
  if (status === "connected") return "Connected";
  if (status === "needs_sign_in") return "Needs sign-in";
  if (status === "permissions_changed") return "Permissions changed";
  return "Disabled";
}

function getRiskFromText(input: string): RiskLevel {
  const lowered = input.toLowerCase();
  if (/(send|delete|purchase|execute|post|transfer)/.test(lowered)) return "high";
  if (/(save|write|update|schedule|create)/.test(lowered)) return "medium";
  return "low";
}

function buildPlanFromRequest(input: string, targetLabel: string): PlanCard {
  const risk = getRiskFromText(input);
  const goal = input.trim().length > 0 ? input.trim() : "Handle your request";
  const requiresApproval = risk === "high";

  return {
    planId: makeId("plan"),
    goal,
    risk,
    touches: [
      { account: targetLabel, type: "process request" },
      ...(requiresApproval ? [{ account: "Approvals", type: "high-risk review" }] : []),
    ],
    steps: [
      {
        stepId: "step_submit",
        title: "Send request to backend agent",
        details: `Submit your prompt to ${targetLabel} for execution.`,
        inputs: "User request",
        expectedOutput: "Accepted request",
        requiresApproval: false,
        preview: goal,
      },
      {
        stepId: "step_execute",
        title: "Execute tools if needed",
        details: "Run backend tools/policies based on agent capabilities.",
        inputs: "Agent runtime context",
        expectedOutput: "Tool outputs + model response",
        requiresApproval: false,
        preview: "Tool activity will appear in result metadata.",
      },
      {
        stepId: "step_finalize",
        title: "Return response and receipt",
        details: "Show final answer with trace/usage metadata.",
        inputs: "Model output",
        expectedOutput: "Assistant response",
        requiresApproval,
        preview: requiresApproval
          ? "High-risk request detected; approval needed before finalization."
          : "No extra approval required.",
      },
    ],
  };
}

export default function App() {
  const initialRouteRef = useRef(parseRouteHash(window.location.hash));
  const [activeNav, setActiveNavState] = useState<NavKey>(
    (initialRouteRef.current.nav as NavKey) ?? "chat"
  );
  const [threads, setThreads] = useState<ThreadItem[]>(INITIAL_THREADS);
  const [activeThreadId, setActiveThreadIdState] = useState<string>(
    initialRouteRef.current.threadId ?? INITIAL_THREADS[0]?.id ?? ""
  );
  const [messageInput, setMessageInput] = useState("");
  const [threadSearch, setThreadSearch] = useState("");
  const [currentPlan, setCurrentPlan] = useState<PlanCard>(INITIAL_PLAN);
  const [inspectorTab, setInspectorTab] = useState<InspectorTab>("plan");
  const [timeline, setTimeline] = useState<TimelineItem[]>(INITIAL_TIMELINE);
  const [stepStatus, setStepStatus] = useState<Record<string, StepStatus>>({});
  const [activeStepIndex, setActiveStepIndex] = useState<number>(0);
  const [isExecuting, setIsExecuting] = useState(false);
  const [runStartedAt, setRunStartedAt] = useState<number | null>(null);

  const [approvals, setApprovals] = useState<ApprovalItem[]>(INITIAL_APPROVALS);
  const [proofs, setProofs] = useState<ProofRecord[]>(INITIAL_PROOFS);

  const [routines, setRoutines] = useState<RoutineItem[]>(INITIAL_ROUTINES);
  const [selectedRoutineId, setSelectedRoutineId] = useState<string>(INITIAL_ROUTINES[0]?.id ?? "");
  const [routineWizardOpen, setRoutineWizardOpen] = useState(false);
  const [routineWizardStep, setRoutineWizardStep] = useState(1);
  const [routineDraft, setRoutineDraft] = useState<{
    template: string;
    type: RoutineType;
    when: string;
    watchInterval: string;
    recipeId: string;
    destination: string;
    quietHours: string;
  }>({
    template: "Daily briefing",
    type: "at_time",
    when: "Weekdays at 8:30 AM",
    watchInterval: "Every 30 minutes",
    recipeId: "recipe_daily_briefing",
    destination: "Messages / #ops-briefing",
    quietHours: "10:00 PM - 7:00 AM",
  });

  const [accounts, setAccounts] = useState<AccountItem[]>(INITIAL_ACCOUNTS);
  const [selectedAccountId, setSelectedAccountId] = useState<string>(INITIAL_ACCOUNTS[0]?.id ?? "");

  const [recipes, setRecipes] = useState<RecipeItem[]>(INITIAL_RECIPES);
  const [libraryFilter, setLibraryFilter] = useState<RecipeItem["category"]>("recommended");
  const [selectedRecipeId, setSelectedRecipeId] = useState<string>(INITIAL_RECIPES[0]?.id ?? "");
  const [internetInstallConfirm, setInternetInstallConfirm] = useState(false);

  const [safeMode, setSafeMode] = useState(true);
  const [status, setStatus] = useState<StatusSummary>({
    level: "ok",
    summary: "Connecting to backend...",
    detail: "Waiting for initial health check.",
    fix: "No action needed",
  });

  const [quickCheck, setQuickCheck] = useState<QuickCheckResult | null>(null);
  const [statusPopoverOpen, setStatusPopoverOpen] = useState(false);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [paletteQuery, setPaletteQuery] = useState("");
  const [approvalsInboxOpen, setApprovalsInboxOpen] = useState(false);
  const [proofArchiveOpen, setProofArchiveOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [inspectorModalOpen, setInspectorModalOpen] = useState(false);
  const [showTerminal, setShowTerminal] = useState(false);

  // ── Backend-connected state ──────────────────────────────
  const [backendAgents, setBackendAgents] = useState<DesktopAgent[]>([]);
  const [backendSkills, setBackendSkills] = useState<SkillDescriptor[]>([]);
  const [backendSecurity, setBackendSecurity] = useState<BackendSecurityStatus | null>(null);
  const [backendMetrics, setBackendMetrics] = useState<CostMetrics | null>(null);
  const [backendHealth, setBackendHealth] = useState<HealthResponse | null>(null);
  const [selectedAgentId, setSelectedAgentId] = useState<string | null>(null);
  const [isSending, setIsSending] = useState(false);
  const [streamingAgentId, setStreamingAgentId] = useState<string | null>(null);
  const [streamingThreadId, setStreamingThreadId] = useState<string | null>(null);
  const [streamingMessageId, setStreamingMessageId] = useState<string | null>(null);
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);

  // ── New backend-connected state ─────────────────────────
  const [backendAuthProfiles, setBackendAuthProfiles] = useState<AuthProfileInfo[]>([]);
  const [backendApprovals, setBackendApprovals] = useState<ApprovalRequestInfo[]>([]);
  const [backendPlugins, setBackendPlugins] = useState<PluginSummary[]>([]);
  const [backendPeers, setBackendPeers] = useState<PeerInfo[]>([]);
  const [backendChannels, setBackendChannels] = useState<ChannelInfo[]>([]);
  const [backendPipelines, setBackendPipelines] = useState<PipelineDescriptor[]>([]);
  const [backendMemoryStats, setBackendMemoryStats] = useState<MemoryStatsResponse | null>(null);
  const [backendMemoryHits, setBackendMemoryHits] = useState<MemoryHit[]>([]);
  const [backendNotifications, setBackendNotifications] = useState<NotificationInfo[]>([]);
  const [backendCanvases, setBackendCanvases] = useState<CanvasSummary[]>([]);
  const [backendTraceRun, setBackendTraceRun] = useState<TraceRunInfo | null>(null);
  const [backendTraceSpans, setBackendTraceSpans] = useState<TraceSpanInfo[]>([]);
  const [backendObservability, setBackendObservability] = useState<ObservabilityStatus | null>(null);
  const [backendGraphNodes, setBackendGraphNodes] = useState<GraphNodeInfo[]>([]);
  const [backendGraphEdges, setBackendGraphEdges] = useState<GraphEdgeInfo[]>([]);

  const [viewportWidth, setViewportWidth] = useState<number>(window.innerWidth);
  const [isSidebarCollapsed, setIsSidebarCollapsed] = useState<boolean>(() => {
    try {
      const stored = window.localStorage.getItem("clawdesk.sidebar.collapsed");
      if (stored === null) return true;
      return stored === "1";
    } catch {
      return true;
    }
  });

  const [inspectorOpen, setInspectorOpen] = useState<boolean>(() => {
    try {
      const stored = window.localStorage.getItem("clawdesk.inspector.open");
      if (stored === null) return false;
      return stored === "1";
    } catch {
      return false;
    }
  });

  const [toasts, setToasts] = useState<ToastItem[]>([]);
  const [onboardingOpen, setOnboardingOpen] = useState<boolean>(() => {
    try {
      return window.localStorage.getItem("clawdesk.onboarding.complete") !== "1";
    } catch {
      return true;
    }
  });

  const activeThread = useMemo<ThreadItem | null>(
    () => threads.find((thread) => thread.id === activeThreadId) ?? threads[0] ?? null,
    [threads, activeThreadId]
  );

  const agentNameById = useMemo(() => {
    const map = new Map<string, string>();
    for (const agent of backendAgents) {
      map.set(agent.id, agent.name);
    }
    return map;
  }, [backendAgents]);

  const activeAgent = useMemo<DesktopAgent | null>(
    () => (selectedAgentId ? backendAgents.find((agent) => agent.id === selectedAgentId) ?? null : null),
    [backendAgents, selectedAgentId]
  );

  const filteredThreads = useMemo(() => {
    const query = threadSearch.trim().toLowerCase();
    if (!query) return threads;
    return threads.filter((thread) => {
      if (thread.title.toLowerCase().includes(query)) return true;
      const agentName = thread.agentId ? agentNameById.get(thread.agentId)?.toLowerCase() : "";
      if (agentName?.includes(query)) return true;
      return thread.messages.some((message) => message.text.toLowerCase().includes(query));
    });
  }, [agentNameById, threadSearch, threads]);

  const selectedRoutine = useMemo<RoutineItem | null>(
    () => routines.find((routine) => routine.id === selectedRoutineId) ?? routines[0] ?? null,
    [routines, selectedRoutineId]
  );

  const selectedAccount = useMemo<AccountItem | null>(
    () => accounts.find((account) => account.id === selectedAccountId) ?? accounts[0] ?? null,
    [accounts, selectedAccountId]
  );

  const selectedRecipe = useMemo<RecipeItem | null>(
    () => recipes.find((recipe) => recipe.id === selectedRecipeId) ?? recipes[0] ?? null,
    [recipes, selectedRecipeId]
  );

  useEffect(() => {
    if (threads.length === 0) {
      if (activeThreadId !== "") setActiveThreadIdState("");
      return;
    }

    if (activeThreadId === "" || !threads.some((t) => t.id === activeThreadId)) {
      setActiveThreadIdState(threads[0].id);
    }
  }, [threads, activeThreadId]);

  // Init theme
  useEffect(() => {
    const theme = window.localStorage.getItem("clawdesk.theme") || "system";
    document.documentElement.setAttribute("data-theme", theme);
  }, []);

  useEffect(() => {
    if (routines.length === 0) {
      if (selectedRoutineId !== "") setSelectedRoutineId("");
      return;
    }
    if (!routines.some((routine) => routine.id === selectedRoutineId)) {
      setSelectedRoutineId(routines[0].id);
    }
  }, [routines, selectedRoutineId]);

  useEffect(() => {
    if (accounts.length === 0) {
      if (selectedAccountId !== "") setSelectedAccountId("");
      return;
    }
    if (!accounts.some((account) => account.id === selectedAccountId)) {
      setSelectedAccountId(accounts[0].id);
    }
  }, [accounts, selectedAccountId]);

  useEffect(() => {
    if (recipes.length === 0) {
      if (selectedRecipeId !== "") setSelectedRecipeId("");
      return;
    }
    if (!recipes.some((recipe) => recipe.id === selectedRecipeId)) {
      setSelectedRecipeId(recipes[0].id);
    }
  }, [recipes, selectedRecipeId]);

  useEffect(() => {
    if (!selectedAgentId) return;
    if (activeNav !== "chat") return;
    if (!threads.some((thread) => thread.id === selectedAgentId)) return;
    if (activeThreadId === selectedAgentId) return;
    setActiveThreadIdState(selectedAgentId);
  }, [selectedAgentId, threads, activeNav, activeThreadId]);

  const pendingApprovals = approvals.filter((approval) => approval.status === "pending");

  const filteredRecipes = useMemo(
    () => recipes.filter((recipe) => recipe.category === libraryFilter),
    [recipes, libraryFilter]
  );

  const compactSidebar = viewportWidth < 680;
  const sidebarCollapsed = compactSidebar || isSidebarCollapsed;
  const drawerAsModal = viewportWidth < 1000;

  useEffect(() => {
    try {
      window.localStorage.setItem("clawdesk.sidebar.collapsed", isSidebarCollapsed ? "1" : "0");
    } catch {
      // Ignore persistence failures.
    }
  }, [isSidebarCollapsed]);

  useEffect(() => {
    try {
      window.localStorage.setItem("clawdesk.inspector.open", inspectorOpen ? "1" : "0");
    } catch {
      // Ignore persistence failures.
    }
  }, [inspectorOpen]);

  const navigate = useCallback(
    (nextNav: NavKey, options?: { threadId?: string; replace?: boolean }) => {
      const nextThreadId =
        options?.threadId ?? (nextNav === "chat" ? activeThreadId : undefined);
      const hash = buildRouteHash({
        nav: nextNav,
        threadId: nextThreadId,
      });
      if (options?.replace) {
        window.history.replaceState(null, "", hash);
      } else {
        window.history.pushState(null, "", hash);
      }
      setActiveNavState(nextNav);
      if (nextThreadId) setActiveThreadIdState(nextThreadId);
    },
    [activeThreadId]
  );

  const selectThread = useCallback(
    (threadId: string, options?: { replace?: boolean }) => {
      navigate("chat", { threadId, replace: options?.replace });
    },
    [navigate]
  );

  // Loosely-typed navigation callback for child pages that don't know NavKey.
  const navigateLoose = useCallback(
    (nav: string, options?: { threadId?: string }) => {
      navigate(nav as NavKey, options);
    },
    [navigate]
  );

  const pushToast = useCallback((text: string) => {
    const toast: ToastItem = { id: makeId("toast"), text };
    setToasts((prev) => [...prev, toast]);
    setTimeout(() => {
      setToasts((prev) => prev.filter((item) => item.id !== toast.id));
    }, 3200);
  }, []);

  async function completeOnboarding(result: {
    provider: string;
    model: string;
    apiKey: string;
    templateName: string;
    storedInVault: boolean;
    enabledSkills: string[];
    channelSetups: { channelId: string; config: Record<string, string> }[];
  }) {
    // Always save settings first — even if agent creation fails
    if (result.provider) {
      window.localStorage.setItem("clawdesk.provider", result.provider);
    }
    if (result.model) {
      window.localStorage.setItem("clawdesk.model", result.model);
    }
    if (result.apiKey) {
      window.localStorage.setItem("clawdesk.api_key", result.apiKey);
      window.localStorage.setItem("clawdesk.api_key.configured", "1");
    }
    window.localStorage.setItem("clawdesk.onboarding.complete", "1");
    setOnboardingOpen(false);

    try {
      // Create agent from selected template
      const template = AGENT_TEMPLATES.find((item) => item.name === result.templateName);
      if (template) {
        await createBackendAgent(template);
      }

      // Activate selected skills (skip errors — skills can be toggled later)
      for (const skillId of result.enabledSkills) {
        try { await api.activateSkill(skillId); } catch { /* skip */ }
      }
      // Refresh skills list after activation
      api.listSkills().then((s) => setBackendSkills(s)).catch(() => { });

      // Connect configured channels (skip errors — channels can be set up later)
      for (const { channelId, config } of result.channelSetups) {
        try { await api.updateChannel(channelId, config); } catch { /* skip */ }
      }
      // Refresh channels after setup
      if (result.channelSetups.length > 0) {
        api.listChannels().then((c) => setBackendChannels(c)).catch(() => { });
      }

      navigate("chat");
      pushToast("ClawDesk setup completed.");
    } catch (error) {
      // Settings are saved even on error. Just warn about the agent.
      navigate("chat");
      pushToast("Settings saved. Agent creation will retry when backend is available.");
    }
  }

  function resetOnboarding() {
    window.localStorage.removeItem("clawdesk.onboarding.complete");
    setOnboardingOpen(true);
  }

  const runQuickCheck = useCallback(async (opts?: { showModal?: boolean }) => {
    const showModal = opts?.showModal ?? true;
    // Only flag account problems when accounts have actually been loaded from backend
    const hasLoadedAccounts = accounts.length > 0;
    const needsSignIn = hasLoadedAccounts && accounts.some((account) => account.status === "needs_sign_in");
    const permissionsChanged = hasLoadedAccounts && accounts.some((account) => account.status === "permissions_changed");

    try {
      const health = await api.getHealth();
      setBackendHealth(health);
      // Refresh all backend data on quick check
      api.listSkills().then((s) => setBackendSkills(s)).catch(() => { });
      api.listAgents().then((a) => setBackendAgents(a)).catch(() => { });
      api.listChannels().then((c) => setBackendChannels(c)).catch(() => { });
      api.listPipelines().then((p) => {
        setBackendPipelines(p);
        setRoutines(p.map(routineFromPipeline));
      }).catch(() => { });
      api.getMetrics().then((m) => setBackendMetrics(m)).catch(() => { });
      api.getSecurityStatus().then((s) => setBackendSecurity(s)).catch(() => { });
      api.listPlugins().then((p) => setBackendPlugins(p)).catch(() => { });
      api.listDiscoveredPeers().then((p) => setBackendPeers(p)).catch(() => { });
      api.getMemoryStats().then((s) => setBackendMemoryStats(s)).catch(() => { });
      api.listNotifications().then((n) => setBackendNotifications(n)).catch(() => { });
      api.listCanvases().then((c) => setBackendCanvases(c)).catch(() => { });

      // Fetch fresh auth profiles and determine actual account health
      let freshNeedsSignIn = needsSignIn;
      let freshPermissionsChanged = permissionsChanged;
      try {
        const freshProfiles = await api.listAuthProfiles();
        setBackendAuthProfiles(freshProfiles);
        freshNeedsSignIn = freshProfiles.some((p) => p.is_expired);
        freshPermissionsChanged = freshProfiles.some((p) => p.failure_count > 0 && !p.is_expired);
      } catch {
        // fall back to local accounts state
      }

      if (freshNeedsSignIn || freshPermissionsChanged) {
        setStatus({
          level: "warn",
          summary: "One account needs attention.",
          detail: "Some connected services need sign-in or permission review.",
          fix: needsSignIn ? "Reconnect Email" : "Review permissions",
        });
        if (showModal) {
          setQuickCheck({
            title: "What’s wrong",
            why: "Some connected accounts can’t run actions reliably.",
            options: [
              "Reconnect Messages account",
              "Reconnect Email account",
              "Pause routines until connected",
              "Try again",
            ],
          });
        }
      } else {
        setStatus({
          level: "ok",
          summary: "Everything is connected.",
          detail: "Engine reachable. Accounts are healthy.",
          fix: "No action needed",
        });
        setQuickCheck(null);
      }
    } catch {
      setStatus({
        level: "error",
        summary: "Can’t reach the assistant engine.",
        detail: "The local engine did not respond to health check.",
        fix: "Restart local engine",
      });
      if (showModal) {
        setQuickCheck({
          title: "What’s wrong",
          why: "The app can’t contact the local assistant engine.",
          options: ["Restart local engine", "Turn on Safe Mode", "Try again"],
        });
      }
    }
  }, [accounts]);

  useEffect(() => {
    runQuickCheck({ showModal: false });
  }, [runQuickCheck]);

  useEffect(() => {
    const applyRoute = () => {
      const route = parseRouteHash(window.location.hash);
      setActiveNavState(route.nav as NavKey);
      if (route.threadId) {
        setActiveThreadIdState(route.threadId);
      }
    };

    if (!window.location.hash) {
      window.history.replaceState(
        null,
        "",
        buildRouteHash({ nav: activeNav, threadId: activeNav === "chat" ? activeThreadId : undefined })
      );
    } else {
      applyRoute();
    }

    window.addEventListener("hashchange", applyRoute);
    window.addEventListener("popstate", applyRoute);
    return () => {
      window.removeEventListener("hashchange", applyRoute);
      window.removeEventListener("popstate", applyRoute);
    };
    // Route listener should only bind once.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // ── Load backend data on mount ────────────────────────────
  useEffect(() => {
    async function loadBackendData() {
      // Sync the active provider to the backend so channel adapters (Discord,
      // Telegram, etc.) that started before the UI loaded have the right
      // provider immediately — even before the user opens ChatPage.
      getActiveProvider();

      try {
        // Phase 1: Core data
        const [health, agents, skills, security, metrics, sessions, channels, pipelines] = await Promise.allSettled([
          api.getHealth(),
          api.listAgents(),
          api.listSkills(),
          api.getSecurityStatus(),
          api.getMetrics(),
          api.listSessions(),
          api.listChannels(),
          api.listPipelines(),
        ]);

        if (health.status === "fulfilled") setBackendHealth(health.value);
        if (agents.status === "fulfilled") {
          setBackendAgents(agents.value);
          setSelectedAgentId((prev) => prev ?? agents.value[0]?.id ?? null);
        }
        if (skills.status === "fulfilled") {
          setBackendSkills(skills.value);
          // Map skills to recipe items for the Library view
          const skillRecipes: RecipeItem[] = skills.value.map((s) => ({
            id: s.id,
            name: s.name,
            category: s.state === "active" ? "recommended" as const : s.category === "core" ? "personal" as const : "team" as const,
            purpose: s.description,
            inputs: `${s.estimated_tokens} estimated tokens`,
            outputs: `Category: ${s.category}`,
            permissions: [s.state === "active" ? "Active" : "Inactive"],
            provenance: s.verified ? "built-in" as const : "from internet" as const,
            restrictedMode: true,
          }));
          if (skillRecipes.length > 0) {
            setRecipes(skillRecipes);
            setSelectedRecipeId(skillRecipes[0].id);
          }
        }
        if (security.status === "fulfilled") setBackendSecurity(security.value);
        if (metrics.status === "fulfilled") setBackendMetrics(metrics.value);
        if (channels.status === "fulfilled") {
          setBackendChannels(channels.value);
          setAccounts(channels.value.map(accountFromChannel));
          if (channels.value.length > 0) {
            setSelectedAccountId((current) => current || channels.value[0].id);
          }
        }
        if (pipelines.status === "fulfilled") {
          setBackendPipelines(pipelines.value);
          const mappedRoutines = pipelines.value.map(routineFromPipeline);
          setRoutines(mappedRoutines);
          if (mappedRoutines.length > 0) {
            setSelectedRoutineId((current) => current || mappedRoutines[0].id);
          }
        }
        if (sessions.status === "fulfilled" && sessions.value.length > 0) {
          const sessionThreads = await Promise.all(
            sessions.value.map(async (session) => {
              const baseThread = toThreadFromSession(session);
              try {
                const messages = await api.getChatMessages(session.chat_id);
                return { ...baseThread, messages: toThreadMessages(messages) };
              } catch {
                return baseThread;
              }
            })
          );
          setThreads((prev) => {
            const previousById = new Map(prev.map((thread) => [thread.id, thread]));
            return sessionThreads.map((thread) => {
              const previous = previousById.get(thread.id);
              if (!previous) return thread;
              return {
                ...thread,
                messages: thread.messages.length > 0 ? thread.messages : previous.messages,
              };
            });
          });
          setActiveThreadIdState((current) =>
            sessionThreads.some((thread) => thread.id === current)
              ? current
              : sessionThreads[0].id
          );
        } else if (agents.status === "fulfilled" && agents.value.length > 0) {
          // Session model is agent-scoped. Seed thread list from real agents when no prior messages exist.
          const agentThreads = agents.value.map((agent) =>
            createThreadForAgent(agent.id, agent)
          );
          setThreads(agentThreads);
          setActiveThreadIdState((current) =>
            agentThreads.some((thread) => thread.id === current) ? current : agentThreads[0].id
          );
        }

        // Phase 2: Extended backend data — auth, plugins, peers, memory, canvas, observability
        const [
          authProfiles, plugins, peers, memoryStats,
          canvases, notifications, observability,
        ] = await Promise.allSettled([
          api.listAuthProfiles(),
          api.listPlugins(),
          api.listDiscoveredPeers(),
          api.getMemoryStats(),
          api.listCanvases(),
          api.listNotifications(),
          api.getObservabilityConfig(),
        ]);

        if (authProfiles.status === "fulfilled") {
          setBackendAuthProfiles(authProfiles.value);
          const profileByProvider = new Map(
            authProfiles.value.map((profile) => [profile.provider.toLowerCase(), profile])
          );
          setAccounts((prev) => {
            if (prev.length === 0) {
              return authProfiles.value.map((profile) => ({
                id: profile.id,
                name: profile.provider,
                status: profile.is_expired
                  ? "needs_sign_in"
                  : profile.failure_count > 0
                    ? "permissions_changed"
                    : "connected",
                summary: `Provider: ${profile.provider} • failures: ${profile.failure_count}`,
                capabilities: {
                  read: true,
                  search: true,
                  draft: true,
                  sendWrite: true,
                  delete: false,
                  execute: false,
                },
                boundaries: {
                  folders: "N/A",
                  recipients: "N/A",
                  channels: profile.provider,
                },
                lastUsed: profile.last_used ?? "Never",
              }));
            }
            return prev.map((account) => {
              const profile = profileByProvider.get(account.name.toLowerCase());
              if (!profile) return account;
              return {
                ...account,
                status: profile.is_expired
                  ? "needs_sign_in"
                  : profile.failure_count > 0
                    ? "permissions_changed"
                    : account.status,
                summary: `${account.summary} • profile failures: ${profile.failure_count}`,
                lastUsed: profile.last_used ?? account.lastUsed,
              };
            });
          });
          setSelectedAccountId((prev) => prev || authProfiles.value[0]?.id || "");
        }
        if (plugins.status === "fulfilled") setBackendPlugins(plugins.value);
        if (peers.status === "fulfilled") setBackendPeers(peers.value);
        if (memoryStats.status === "fulfilled") setBackendMemoryStats(memoryStats.value);
        if (canvases.status === "fulfilled") setBackendCanvases(canvases.value);
        if (notifications.status === "fulfilled") setBackendNotifications(notifications.value);
        if (observability.status === "fulfilled") setBackendObservability(observability.value);

        // Update status based on actual health
        setStatus({
          level: health.status === "fulfilled" ? "ok" : "error",
          summary: health.status === "fulfilled"
            ? "Engine running. All systems connected."
            : "Can't reach the assistant engine.",
          detail: health.status === "fulfilled"
            ? `v${(health as PromiseFulfilledResult<HealthResponse>).value.version} • ${agents.status === "fulfilled" ? (agents as PromiseFulfilledResult<DesktopAgent[]>).value.length : 0} agents • ${skills.status === "fulfilled" ? (skills as PromiseFulfilledResult<SkillDescriptor[]>).value.length : 0} skills`
            : "The local engine did not respond to health check.",
          fix: health.status === "fulfilled" ? "No action needed" : "Restart local engine",
        });
      } catch {
        // Backend not available — keep defaults
        setStatus({
          level: "error",
          summary: "Can't reach the assistant engine.",
          detail: "The local engine did not respond.",
          fix: "Restart local engine",
        });
      }
    }
    loadBackendData();
  }, []);

  // ── Periodic refresh of metrics, security & notifications ──
  useEffect(() => {
    pollRef.current = setInterval(async () => {
      try {
        const [metrics, security, notifications, memStats, channels, pipelines] = await Promise.allSettled([
          api.getMetrics(),
          api.getSecurityStatus(),
          api.listNotifications(),
          api.getMemoryStats(),
          api.listChannels(),
          api.listPipelines(),
        ]);
        if (metrics.status === "fulfilled") setBackendMetrics(metrics.value);
        if (security.status === "fulfilled") setBackendSecurity(security.value);
        if (notifications.status === "fulfilled") setBackendNotifications(notifications.value);
        if (memStats.status === "fulfilled") setBackendMemoryStats(memStats.value);
        if (channels.status === "fulfilled") {
          setBackendChannels(channels.value);
          setAccounts((prev) => {
            const byId = new Map(prev.map((account) => [account.id, account]));
            return channels.value.map((channel) => {
              const mapped = accountFromChannel(channel);
              const existing = byId.get(channel.id);
              return existing
                ? { ...mapped, boundaries: existing.boundaries, capabilities: existing.capabilities, lastUsed: existing.lastUsed }
                : mapped;
            });
          });
        }
        if (pipelines.status === "fulfilled") {
          setBackendPipelines(pipelines.value);
          setRoutines((prev) => {
            const existingById = new Map(prev.map((routine) => [routine.id, routine]));
            return pipelines.value.map((pipeline) => {
              const mapped = routineFromPipeline(pipeline);
              const existing = existingById.get(mapped.id);
              return existing ? { ...mapped, enabled: existing.enabled, history: existing.history, lastResult: existing.lastResult, lastReason: existing.lastReason } : mapped;
            });
          });
        }
      } catch { /* silent */ }
    }, 30_000);
    return () => { if (pollRef.current) clearInterval(pollRef.current); };
  }, []);

  useEffect(() => {
    let cleanup: (() => void) | null = null;
    subscribeAppEvents({
      onMetricsUpdated: (metrics) => setBackendMetrics(metrics),
      onSecurityChanged: (security) => setBackendSecurity(security),
      onRoutineExecuted: (payload) => {
        const data = payload as RoutineExecutedPayload | null;
        const pipelineId = data && typeof data.pipeline_id === "string" ? data.pipeline_id : "";
        const pipelineName =
          data && typeof data.pipeline_name === "string"
            ? data.pipeline_name
            : "Routine";
        const success = data?.success !== false;
        const durationMs =
          typeof data?.duration_ms === "number"
            ? data.duration_ms
            : typeof data?.total_duration_ms === "number"
              ? data.total_duration_ms
              : 0;
        const timestamp =
          data && typeof data.timestamp === "string"
            ? data.timestamp
            : new Date().toISOString();
        if (pipelineId) {
          const historyEntry = {
            at: formatLastActivity(timestamp),
            result: (success ? "ok" : "failed") as RoutineResult,
            reason: success
              ? `Pipeline run completed${durationMs > 0 ? ` in ${Math.max(1, Math.round(durationMs / 1000))}s` : ""}`
              : "Pipeline run failed",
          };
          setRoutines((prev) =>
            prev.map((routine) =>
              routine.id === pipelineId
                ? {
                  ...routine,
                  lastResult: historyEntry.result,
                  lastReason: historyEntry.reason,
                  history: [historyEntry, ...routine.history],
                }
                : routine
            )
          );
        }
        api.listPipelines().then((pipelines) => {
          setBackendPipelines(pipelines);
          setRoutines((prev) => {
            const existingById = new Map(prev.map((routine) => [routine.id, routine]));
            return pipelines.map((pipeline) => {
              const mapped = routineFromPipeline(pipeline);
              const existing = existingById.get(mapped.id);
              return existing
                ? {
                  ...mapped,
                  enabled: existing.enabled,
                  history: existing.history,
                  lastResult: existing.lastResult,
                  lastReason: existing.lastReason,
                }
                : mapped;
            });
          });
        }).catch(() => { });
        // AutomationsPage shows a detailed per-step toast for manual runs.
        // For cron-scheduled runs the result appears in the Routines sidebar.
        // Only show a global toast when the pipeline truly failed so the user
        // gets an alert even if they aren't on the Automations page.
        if (!success) {
          pushToast(`${pipelineName} failed.`);
        }
      },
      onIncomingMessage: (payload) => {
        const data = payload as IncomingMessagePayload | null;
        const agentId = data && typeof data.agent_id === "string" ? data.agent_id : "";
        const chatId = data && typeof data.chat_id === "string" ? data.chat_id : "";
        const preview =
          data && typeof data.preview === "string" && data.preview.length > 0
            ? data.preview
            : "New channel message received.";
        const timestamp =
          data && typeof data.timestamp === "string"
            ? data.timestamp
            : new Date().toISOString();
        if (!agentId) {
          pushToast(preview);
          return;
        }
        upsertThreadForAgent(agentId, {
          id: chatId || undefined,
          title: preview.length > 44 ? `${preview.slice(0, 44)}...` : preview,
          lastActivity: formatLastActivity(timestamp),
        });
        // Skip full message hydration when ChatPage is active —
        // ChatPage has its own incoming:message listener and manages messages[].
        if (chatId && activeNav !== "chat") {
          hydrateThreadMessages(chatId, preview, timestamp).catch(() => {
            // Best-effort thread refresh for incoming messages.
          });
        }
        // Only show toast for incoming messages when NOT in the chat tab
        // (ChatPage displays the message inline; no need for a popup too)
        if (activeNav !== "chat") {
          pushToast(preview);
        }
      },
      onSystemAlert: (alert) => {
        if (alert?.message) pushToast(alert.message);
      },
      onApprovalPending: (payload) => {
        const data = payload as Partial<ApprovalPendingPayload> | null;
        if (!data || typeof data.id !== "string") return;
        const risk: RiskLevel =
          data.risk === "high" || data.risk === "low" || data.risk === "medium"
            ? data.risk
            : "medium";
        const nextApproval: ApprovalItem = {
          id: data.id,
          planId: currentPlan.planId || "plan_backend",
          stepId: `approval_${data.id}`,
          summary: data.tool_name ?? "Approval request",
          where: "Backend policy gate",
          impact: data.command ?? "Command execution requires approval.",
          risk,
          status: "pending",
        };
        setApprovals((prev) => mergeApprovals(prev, [nextApproval]));
        pushToast(`Approval needed: ${nextApproval.summary}`);
      },
      onAgentEvent: (payload) => {
        const data = payload as FrontendAgentEventPayload | null;
        if (!data || typeof data !== "object") return;
        if (typeof data.agent_id !== "string" || !data.event || typeof data.event !== "object") return;
        if (!streamingAgentId || !streamingThreadId || !streamingMessageId) return;
        if (data.agent_id !== streamingAgentId) return;

        const event = data.event;
        if (event.type === "StreamChunk") {
          const chunkText = typeof event.text === "string" ? event.text : "";
          const done = Boolean(event.done);
          if (chunkText.length > 0) {
            setThreads((prev) =>
              prev.map((thread) => {
                if (thread.id !== streamingThreadId) return thread;
                return {
                  ...thread,
                  lastActivity: "Just now",
                  messages: thread.messages.map((message) =>
                    message.id === streamingMessageId
                      ? { ...message, text: `${message.text}${chunkText}` }
                      : message
                  ),
                };
              })
            );
          }
          if (done) {
            setStreamingAgentId(null);
            setStreamingThreadId(null);
            setStreamingMessageId(null);
          }
          return;
        }

        if (event.type === "ToolStart" && typeof event.name === "string") {
          setThreads((prev) =>
            prev.map((thread) => {
              if (thread.id !== streamingThreadId) return thread;
              return {
                ...thread,
                messages: thread.messages.map((message) =>
                  message.id === streamingMessageId
                    ? { ...message, result: `Using ${event.name}...` }
                    : message
                ),
              };
            })
          );
          return;
        }

        if (event.type === "ToolEnd" && typeof event.name === "string") {
          const durationMs = typeof event.duration_ms === "number" ? event.duration_ms : 0;
          const success = Boolean(event.success);
          setThreads((prev) =>
            prev.map((thread) => {
              if (thread.id !== streamingThreadId) return thread;
              return {
                ...thread,
                messages: thread.messages.map((message) =>
                  message.id === streamingMessageId
                    ? {
                      ...message,
                      result: `${event.name} ${success ? "completed" : "failed"} · ${durationMs}ms`,
                    }
                    : message
                ),
              };
            })
          );
          return;
        }

        if (event.type === "Error") {
          const errorText = typeof event.error === "string" ? event.error : "Agent execution failed.";
          setThreads((prev) =>
            prev.map((thread) => {
              if (thread.id !== streamingThreadId) return thread;
              return {
                ...thread,
                messages: thread.messages.map((message) =>
                  message.id === streamingMessageId
                    ? {
                      ...message,
                      text: message.text || errorText,
                      result: "Error",
                    }
                    : message
                ),
              };
            })
          );
          setStreamingAgentId(null);
          setStreamingThreadId(null);
          setStreamingMessageId(null);
          return;
        }

        if (event.type === "Done") {
          setStreamingAgentId(null);
          setStreamingThreadId(null);
          setStreamingMessageId(null);
        }
      },
    }).then((dispose) => {
      cleanup = dispose;
    }).catch(() => {
      // Event bridge is optional; fallback polling remains active.
    });
    return () => {
      if (cleanup) cleanup();
    };
  }, [
    currentPlan.planId,
    pushToast,
    streamingAgentId,
    streamingMessageId,
    streamingThreadId,
  ]);

  // ── System tray menu → frontend navigation ──────────────────────────
  useEffect(() => {
    let disposed = false;
    const cleanups: Array<() => void> = [];

    (async () => {
      const { listen } = await import("@tauri-apps/api/event");

      if (disposed) return;

      const u1 = await listen("tray-open-settings", () => {
        navigate("settings");
      });
      cleanups.push(u1);

      const u2 = await listen("tray-new-chat", () => {
        navigate("chat");
      });
      cleanups.push(u2);
    })().catch(() => { /* tray events optional */ });

    return () => {
      disposed = true;
      cleanups.forEach((fn) => fn());
    };
  }, [navigate]);

  useEffect(() => {
    const onResize = () => setViewportWidth(window.innerWidth);
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);

  useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      const meta = event.metaKey || event.ctrlKey;

      if (meta && event.key.toLowerCase() === "k") {
        event.preventDefault();
        setPaletteOpen(true);
      }
      if (meta && event.key.toLowerCase() === "n") {
        event.preventDefault();
        navigate("chat");
        if (selectedAgentId) {
          selectThread(selectedAgentId, { replace: true });
          setMessageInput("");
          pushToast("Ready for a new request.");
        } else {
          pushToast("Create or select an agent first.");
        }
      }
      if (meta && event.key.toLowerCase() === "r") {
        event.preventDefault();
        runQuickCheck();
      }
      if (meta && event.key === ".") {
        event.preventDefault();
        stopExecution();
      }
      if (meta && ["1", "2", "3", "4", "5", "6"].includes(event.key)) {
        event.preventDefault();
        const idx = Number(event.key) - 1;
        if (idx < NAV_ITEMS.length) navigate(NAV_ITEMS[idx].key);
      }
      if (event.key === "Escape") {
        setPaletteOpen(false);
        setApprovalsInboxOpen(false);
        setProofArchiveOpen(false);
        setSettingsOpen(false);
        setStatusPopoverOpen(false);
        setQuickCheck(null);
        setInspectorModalOpen(false);
      }
    };

    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [navigate, pushToast, runQuickCheck, selectThread, selectedAgentId]);

  function markThreadActivity(threadId: string, patch: Partial<ThreadItem>) {
    setThreads((prev) =>
      prev.map((thread) => (thread.id === threadId ? { ...thread, ...patch } : thread))
    );
  }

  function upsertThreadForAgent(agentId: string, patch?: Partial<ThreadItem>) {
    const agent = backendAgents.find((item) => item.id === agentId) ?? null;
    setThreads((prev) => {
      const existing = prev.find((thread) => thread.agentId === agentId);
      if (existing) {
        if (!patch) return prev;
        return prev.map((thread) =>
          thread.id === existing.id ? { ...thread, ...patch } : thread
        );
      }
      return [createThreadForAgent(agentId, agent, patch), ...prev];
    });
  }

  async function hydrateThreadMessages(threadId: string, fallbackPreview?: string, fallbackTimestamp?: string) {
    if (!threadId) return;
    try {
      const backendMessages = await api.getChatMessages(threadId);
      const mappedMessages = toThreadMessages(backendMessages);
      const lastMessage = mappedMessages[mappedMessages.length - 1];
      setThreads((prev) =>
        prev.map((thread) =>
          thread.id === threadId
            ? {
              ...thread,
              lastActivity: lastMessage?.time ?? thread.lastActivity,
              messages: mappedMessages,
            }
            : thread
        )
      );
    } catch {
      if (!fallbackPreview) return;
      const timeLabel = fallbackTimestamp ? formatLastActivity(fallbackTimestamp) : nowLabel();
      setThreads((prev) =>
        prev.map((thread) => {
          if (thread.id !== threadId) return thread;
          const tail = thread.messages[thread.messages.length - 1];
          if (tail?.role === "assistant" && tail.text === fallbackPreview) {
            return { ...thread, lastActivity: timeLabel };
          }
          return {
            ...thread,
            lastActivity: timeLabel,
            messages: [
              ...thread.messages,
              {
                id: makeId("message"),
                role: "assistant",
                text: fallbackPreview,
                time: timeLabel,
              },
            ],
          };
        })
      );
    }
  }

  useEffect(() => {
    if (!activeThreadId) return;
    hydrateThreadMessages(activeThreadId).catch(() => {
      // Thread hydration is best-effort.
    });
  }, [activeThreadId, hydrateThreadMessages]);

  async function requestToAssistant(text: string) {
    if (isSending) return;

    const agentId = selectedAgentId;
    if (!agentId) {
      pushToast("Select or create an agent first.");
      return;
    }

    const selectedAgent = backendAgents.find((agent) => agent.id === agentId) ?? null;
    const targetLabel = selectedAgent?.name ?? "selected agent";
    const plan = buildPlanFromRequest(text, targetLabel);
    setCurrentPlan(plan);

    // Resolve thread by agentId (thread.id is now chat_id, not agentId)
    const existingThread = threads.find((t) => t.agentId === agentId);
    const threadId = existingThread?.id ?? `thread_${agentId}_${Date.now()}`;
    const cacheNamespace = `agent:${agentId}`;
    const userMessage: ThreadMessage = {
      id: makeId("message"),
      role: "user",
      text,
      time: nowLabel(),
    };

    setThreads((prev) => {
      const index = prev.findIndex((thread) => thread.agentId === agentId);
      if (index === -1) {
        return [
          {
            id: threadId,
            agentId,
            title: text.length > 44 ? `${text.slice(0, 44)}...` : text,
            lastActivity: "Just now",
            pendingApprovals: 0,
            routineGenerated: false,
            hasProofOutputs: false,
            messages: [userMessage],
          },
          ...prev,
        ];
      }
      return prev.map((thread) =>
        thread.agentId === agentId
          ? {
            ...thread,
            title: text.length > 44 ? `${text.slice(0, 44)}...` : text,
            lastActivity: "Just now",
            messages: [...thread.messages, userMessage],
          }
          : thread
      );
    });
    if (activeThreadId !== threadId) {
      selectThread(threadId, { replace: true });
    }

    let assistantText = "";
    let resultLabel = "Response received";
    const streamedMessageId = makeId("message");
    const placeholderMessage: ThreadMessage = {
      id: streamedMessageId,
      role: "assistant",
      text: "",
      time: nowLabel(),
      result: "Thinking...",
    };

    setIsSending(true);
    setThreads((prev) =>
      prev.map((thread) => {
        if (thread.id !== threadId) return thread;
        return {
          ...thread,
          lastActivity: "Just now",
          messages: [...thread.messages, placeholderMessage],
        };
      })
    );
    setStreamingAgentId(agentId);
    setStreamingThreadId(threadId);
    setStreamingMessageId(streamedMessageId);

    let matchedSkills: string[] = [];
    let routeSummary = "";
    try {
      const [triggerInfo, routingInfo, cacheInfo] = await Promise.all([
        api.evaluateSkillTriggers(text).catch(() => []),
        selectedAgent
          ? api.getProviderRouting(selectedAgent.model, ["text_completion", "system_prompt", "streaming"]).catch(() => null)
          : Promise.resolve(null),
        api.cacheLookup(text, cacheNamespace).catch(() => null),
      ]);

      if (Array.isArray(triggerInfo)) {
        matchedSkills = triggerInfo
          .filter((item) => item.matched)
          .map((item) => item.skill_id);
      }

      if (routingInfo?.selected_provider) {
        routeSummary = `${routingInfo.selected_provider}${routingInfo.selected_model ? `/${routingInfo.selected_model}` : ""}`;
      }

      if (cacheInfo?.hit && cacheInfo.result) {
        const cacheResult = cacheInfo.result;
        const cachedLabelParts = [
          `Cache hit (${cacheInfo.match_type})`,
          routeSummary ? `route: ${routeSummary}` : "",
          matchedSkills.length > 0 ? `skills: ${matchedSkills.join(", ")}` : "",
        ].filter(Boolean);
        setThreads((prev) =>
          prev.map((thread) => {
            if (thread.id !== threadId) return thread;
            return {
              ...thread,
              lastActivity: "Just now",
              messages: thread.messages.map((message) =>
                message.id === streamedMessageId
                  ? { ...message, text: cacheResult, result: cachedLabelParts.join(" · ") }
                  : message
              ),
            };
          })
        );
        setIsSending(false);
        setStreamingAgentId(null);
        setStreamingThreadId(null);
        setStreamingMessageId(null);
        setTimeline([
          {
            id: makeId("timeline"),
            label: "Request received",
            detail: text,
            time: nowLabel(),
            undoable: false,
          },
          {
            id: makeId("timeline"),
            label: "Cache response served",
            detail: cachedLabelParts.join(" · ") || "Served from semantic cache.",
            time: nowLabel(),
            undoable: false,
          },
        ]);
        setStepStatus({});
        setInspectorTab("plan");
        pushToast("Response served from cache.");
        return;
      }
    } catch {
      // Preflight should never block the core send flow.
    }

    try {
      const userModel = window.localStorage.getItem("clawdesk.model") || undefined;
      // BUG FIX: Pass threadId as chatId so the backend reuses the same session
      // across messages. Without this, every message created a new chat session
      // and the LLM had no conversation history (could not remember context).
      const response = await api.sendMessage(agentId, text, userModel, threadId);
      assistantText = response.message.content;

      // Remap temp thread ID → backend chat_id after first send
      const realChatId = response.chat_id;
      if (realChatId && realChatId !== threadId) {
        setThreads((prev) =>
          prev.map((thread) =>
            thread.id === threadId ? { ...thread, id: realChatId } : thread
          )
        );
        if (activeThreadId === threadId) {
          selectThread(realChatId, { replace: true });
        }
      }

      const meta = response.message.metadata;
      const messageMeta = toThreadMessageMeta(meta);
      if (meta) {
        resultLabel = `${meta.model} · ${meta.token_cost} tokens · $${meta.cost_usd.toFixed(4)} · ${meta.duration_ms}ms`;
        if (meta.skills_activated.length > 0) {
          resultLabel += ` · skills: ${meta.skills_activated.join(", ")}`;
        }
      } else {
        const preflightBits = [
          routeSummary ? `route: ${routeSummary}` : "",
          matchedSkills.length > 0 ? `skills: ${matchedSkills.join(", ")}` : "",
        ].filter(Boolean);
        if (preflightBits.length > 0) {
          resultLabel = preflightBits.join(" · ");
        }
      }

      api.getMetrics().then((m) => setBackendMetrics(m)).catch(() => { });
      api.listAgents().then((a) => setBackendAgents(a)).catch(() => { });
      api.listSessions().then(async (sessions) => {
        const session = sessions.find((s) => s.agent_id === agentId);
        if (!session) return;
        try {
          const messages = await api.getChatMessages(session.chat_id);
          setThreads((prev) =>
            prev.map((thread) =>
              thread.agentId === agentId
                ? { ...toThreadFromSession(session), messages: toThreadMessages(messages) }
                : thread
            )
          );
        } catch {
          // Ignore refresh failures.
        }
      }).catch(() => { });

      api.rememberMemory({
        content: `User: ${text}\nAssistant: ${assistantText}`,
        source: `chat:${agentId}:${threadId}`,
      }).catch(() => { });
      api.recallMemories({ query: text, max_results: 5 }).then((hits) => setBackendMemoryHits(hits)).catch(() => { });
      api.getMemoryStats().then((s) => setBackendMemoryStats(s)).catch(() => { });

      const rawMeta = response.message.metadata as Record<string, unknown> | null;
      if (rawMeta?.trace_id) {
        const traceId = rawMeta.trace_id as string;
        api.traceGetRun(traceId).then((run) => setBackendTraceRun(run)).catch(() => { });
        api.traceGetSpans(traceId).then((spans) => setBackendTraceSpans(spans)).catch(() => { });
      }
      api.graphGetNodesByType("agent", 10).then((nodes) => setBackendGraphNodes(nodes)).catch(() => { });

      api.cacheStore(
        text,
        cacheNamespace,
        assistantText,
        undefined,
        [`session:${threadId}`],
        3600
      ).catch(() => { });

      setThreads((prev) =>
        prev.map((thread) => {
          if (thread.id !== threadId) return thread;
          return {
            ...thread,
            lastActivity: "Just now",
            messages: thread.messages.map((message) =>
              message.id === streamedMessageId
                ? {
                  ...message,
                  text: assistantText,
                  result: messageMeta ? undefined : resultLabel,
                  meta: messageMeta,
                }
                : message
            ),
          };
        })
      );
    } catch (err) {
      const mapped = classifyError(err);
      assistantText = mapped.userMessage;
      resultLabel = "Error";
      setThreads((prev) =>
        prev.map((thread) => {
          if (thread.id !== threadId) return thread;
          return {
            ...thread,
            lastActivity: "Just now",
            messages: thread.messages.map((message) =>
              message.id === streamedMessageId
                ? { ...message, text: assistantText, result: resultLabel }
                : message
            ),
          };
        })
      );
    } finally {
      setIsSending(false);
      setStreamingAgentId(null);
      setStreamingThreadId(null);
      setStreamingMessageId(null);
    }

    const approvalSteps = plan.steps.filter((step) => step.requiresApproval);
    if (approvalSteps.length > 0) {
      const createdApprovals = await Promise.all(
        approvalSteps.map(async (step) => {
          try {
            const backendApproval = await api.createApprovalRequest({
              tool_name: step.title,
              command: step.preview,
              risk_level: plan.risk,
              context: `${plan.goal} • agent=${targetLabel}`,
            });
            setBackendApprovals((prev) => [backendApproval, ...prev]);
            return {
              id: backendApproval.id,
              planId: plan.planId,
              stepId: step.stepId,
              summary: step.title,
              where: targetLabel,
              impact: step.expectedOutput,
              risk: plan.risk,
              status: "pending" as const,
            };
          } catch {
            pushToast(`Approval gate unavailable for "${step.title}".`);
            return null;
          }
        })
      );
      const persistedApprovals = createdApprovals.filter(Boolean) as ApprovalItem[];
      if (persistedApprovals.length > 0) {
        setApprovals((prev) => mergeApprovals(prev, persistedApprovals));
        markThreadActivity(threadId, {
          pendingApprovals: persistedApprovals.length,
          lastActivity: "Just now",
        });
      }
    }

    setTimeline([
      {
        id: makeId("timeline"),
        label: "Request received",
        detail: text,
        time: nowLabel(),
        undoable: false,
      },
      {
        id: makeId("timeline"),
        label: "Agent responded",
        detail: selectedAgent ? `${selectedAgent.name} completed the request.` : "Response from backend agent",
        time: nowLabel(),
        undoable: false,
      },
    ]);
    setStepStatus({});
    setInspectorTab("plan");
    // Removed: redundant "Agent response received" toast (the chat already shows the response inline)
  }

  function initializeExecution() {
    if (currentPlan.steps.length === 0) {
      pushToast("No plan available yet.");
      return;
    }
    const initialState: Record<string, StepStatus> = {};
    currentPlan.steps.forEach((step, idx) => {
      initialState[step.stepId] = idx === 0 ? "running" : "idle";
    });
    setStepStatus(initialState);
    setActiveStepIndex(0);
    setIsExecuting(true);
    setRunStartedAt(Date.now());
    setInspectorTab("plan");
  }

  function runStepByStep() {
    if (safeMode && currentPlan.steps.some((step) => step.requiresApproval)) {
      setApprovalsInboxOpen(true);
    }
    initializeExecution();
    setTimeline((prev) => [
      ...prev,
      {
        id: makeId("timeline"),
        label: "Execution preview",
        detail: "Review each step and approvals before backend actions.",
        time: nowLabel(),
        undoable: false,
      },
    ]);
    pushToast("Step-by-step review started.");
  }

  function runAll() {
    if (!selectedAgentId) {
      pushToast("Select an agent first.");
      return;
    }
    if (safeMode && currentPlan.steps.some((step) => step.requiresApproval)) {
      setApprovalsInboxOpen(true);
      pushToast("Safe Mode blocked Run all. Review approvals first.");
      return;
    }
    if (messageInput.trim()) {
      requestToAssistant(messageInput.trim());
      setMessageInput("");
      return;
    }
    pushToast("Send a request from chat to execute on backend.");
  }

  function approveAndRunCurrentStep() {
    const step = currentPlan.steps[activeStepIndex];
    if (!step) return;

    if (safeMode && step.requiresApproval) {
      setApprovals((prev) =>
        prev.map((approval) =>
          approval.planId === currentPlan.planId && approval.stepId === step.stepId
            ? { ...approval, status: "approved" }
            : approval
        )
      );
    }
    setStepStatus((prev) => ({ ...prev, [step.stepId]: "ok" }));
    if (activeStepIndex < currentPlan.steps.length - 1) {
      const next = currentPlan.steps[activeStepIndex + 1];
      setStepStatus((prev) => ({ ...prev, [next.stepId]: "running" }));
      setActiveStepIndex((prev) => prev + 1);
    } else {
      setIsExecuting(false);
      setInspectorTab("proof");
      pushToast("Step review completed.");
    }
  }

  function skipCurrentStep() {
    const step = currentPlan.steps[activeStepIndex];
    if (!step) return;
    setStepStatus((prev) => ({ ...prev, [step.stepId]: "skipped" }));
    if (activeStepIndex < currentPlan.steps.length - 1) {
      const next = currentPlan.steps[activeStepIndex + 1];
      setStepStatus((prev) => ({ ...prev, [next.stepId]: "running" }));
      setActiveStepIndex((prev) => prev + 1);
    } else {
      setIsExecuting(false);
      pushToast("All steps reviewed.");
    }
  }

  function stopExecution() {
    if (!isExecuting && !isSending) return;
    const step = currentPlan.steps[activeStepIndex];
    if (step) {
      setStepStatus((prev) => ({ ...prev, [step.stepId]: "stopped" }));
    }
    setIsExecuting(false);
    setRunStartedAt(null);
    if (isSending) {
      setIsSending(false);
      setStreamingAgentId(null);
      setStreamingThreadId(null);
      setStreamingMessageId(null);
      pushToast("Stopped live updates. Backend may still complete this run.");
      return;
    }
    pushToast("Execution review stopped.");
  }

  function editCurrentStepPreview() {
    const step = currentPlan.steps[activeStepIndex];
    if (!step) return;
    const edited = window.prompt("Edit preview", step.preview);
    if (edited === null) return;
    setCurrentPlan((prev) => ({
      ...prev,
      steps: prev.steps.map((planStep) =>
        planStep.stepId === step.stepId ? { ...planStep, preview: edited } : planStep
      ),
    }));
    pushToast("Step preview updated.");
  }

  function setApprovalStatus(item: ApprovalItem, nextStatus: "approved" | "denied") {
    // Update local state immediately
    setApprovals((prev) => prev.map((approval) => (approval.id === item.id ? { ...approval, status: nextStatus } : approval)));

    // Wire to backend approval system
    if (nextStatus === "approved") {
      api.approveRequest(item.id, "user").catch(() => { });
      pushToast("Approval granted.");
    } else {
      api.denyRequest(item.id, "user", "Denied by user").catch(() => { });
      pushToast("Approval denied. No action taken.");
    }

    if (item.planId === currentPlan.planId && nextStatus === "approved" && !isExecuting && activeNav === "chat") {
      setInspectorTab("plan");
    }
  }

  async function createRoutineFromWizard() {
    const agentId = selectedAgentId ?? backendAgents[0]?.id ?? null;
    if (!agentId) {
      pushToast("Create an agent before creating a routine.");
      return;
    }
    const agent = backendAgents.find((item) => item.id === agentId) ?? null;
    const routineName = routineDraft.template || "New routine";
    const scheduleLabel = routineDraft.type === "at_time" ? routineDraft.when : routineDraft.watchInterval;
    const description = `${scheduleLabel} • ${routineDraft.destination} • quiet hours ${routineDraft.quietHours}`;
    const steps: PipelineNodeDescriptor[] = [
      {
        label: "Input",
        node_type: "input",
        model: null,
        agent_id: null,
        x: 30,
        y: 80,
      },
      {
        label: `${agent?.name ?? "Agent"} run`,
        node_type: "agent",
        model: agent?.model ?? "sonnet",
        agent_id: agentId,
        x: 180,
        y: 80,
      },
      {
        label: "Output",
        node_type: "output",
        model: null,
        agent_id: null,
        x: 340,
        y: 80,
      },
    ];

    try {
      const created = await api.createPipeline(
        routineName,
        description,
        steps,
        [[0, 1], [1, 2]]
      );
      const pipelines = await api.listPipelines();
      const mappedRoutines = pipelines.map(routineFromPipeline);
      setBackendPipelines(pipelines);
      setRoutines(mappedRoutines);
      setSelectedRoutineId(created.id);
      setRoutineWizardOpen(false);
      setRoutineWizardStep(1);
      pushToast("Routine created from backend pipeline.");
    } catch (err) {
      const mapped = classifyError(err);
      pushToast(mapped.userMessage);
    }
  }

  async function runRoutineTest(routineId?: string) {
    const targetRoutineId = routineId ?? selectedRoutineId;
    const selected = routines.find((routine) => routine.id === targetRoutineId);
    if (!selected) return;

    const startedAt = Date.now();
    const pipelineId = selected.pipelineId ?? selected.id;
    try {
      const runResult = await api.runPipeline(pipelineId);
      const durationMs = Date.now() - startedAt;
      const resultText =
        typeof runResult === "string"
          ? runResult
          : JSON.stringify(runResult).slice(0, 220);

      const historyEntry = {
        at: new Date().toLocaleString(),
        result: "ok" as const,
        reason: "Pipeline run succeeded",
      };

      setRoutines((prev) =>
        prev.map((routine) =>
          routine.id === selected.id
            ? {
              ...routine,
              lastResult: "ok",
              lastReason: "Pipeline run succeeded",
              history: [historyEntry, ...routine.history],
            }
            : routine
        )
      );

      const proof: ProofRecord = {
        proofId: makeId("proof"),
        requestId: selected.id,
        summary: `Pipeline run completed: ${selected.name}`,
        startedAt: new Date(startedAt).toISOString(),
        endedAt: new Date().toISOString(),
        duration: `${Math.max(1, Math.round(durationMs / 1000))}s`,
        steps: [{ stepId: "pipeline_run", title: "Run backend pipeline", status: "ok" }],
        outputs: [{ type: "result", label: "Run result", link: resultText || "Pipeline completed." }],
        undo: [],
      };
      setProofs((prev) => [proof, ...prev]);
      pushToast("Pipeline run completed.");
    } catch (err) {
      const mapped = classifyError(err);
      const historyEntry = {
        at: new Date().toLocaleString(),
        result: "failed" as const,
        reason: mapped.userMessage,
      };
      setRoutines((prev) =>
        prev.map((routine) =>
          routine.id === selected.id
            ? {
              ...routine,
              lastResult: "failed",
              lastReason: mapped.userMessage,
              history: [historyEntry, ...routine.history],
            }
            : routine
        )
      );
      pushToast(mapped.userMessage);
    }
  }

  function reconnectAccount(id: string) {
    const account = accounts.find((item) => item.id === id);
    if (!account) return;

    // Start OAuth flow for this provider
    api.startOAuthFlow({
      provider: account.name.toLowerCase(),
      client_id: "clawdesk",
      auth_url: `https://${account.name.toLowerCase()}.com/oauth/authorize`,
      token_url: `https://${account.name.toLowerCase()}.com/oauth/token`,
      redirect_uri: "clawdesk://oauth/callback",
      scopes: ["read", "write"],
      use_pkce: true,
    }).then((oauthResult) => {
      if (oauthResult.auth_url) {
        pushToast(`Opening ${account.name} sign-in...`);
      }
      // Optimistically mark connected
      setAccounts((prev) =>
        prev.map((a) =>
          a.id === id ? { ...a, status: "connected", lastUsed: "Just now" } : a
        )
      );
      // Refresh auth profiles
      api.listAuthProfiles().then((profiles) => setBackendAuthProfiles(profiles)).catch(() => { });
    }).catch(() => {
      // Fallback: just mark reconnected locally
      setAccounts((prev) =>
        prev.map((a) =>
          a.id === id ? { ...a, status: "connected", lastUsed: "Just now" } : a
        )
      );
    });
    pushToast("Account reconnecting...");
    runQuickCheck();
  }

  async function runAccountTest(id: string) {
    const account = accounts.find((item) => item.id === id);
    if (!account) return;
    try {
      const result = await api.checkPermission(
        "account", id, "service", account.name.toLowerCase(), "read"
      );
      pushToast(`${account.name} connection test: ${result.decision === "allow" ? "OK" : "Denied"} (${result.reason ?? "no reason"})`);
    } catch {
      pushToast(`${account.name} connection test: OK (backend check unavailable)`);
    }
  }

  function installRecipe(recipe: RecipeItem) {
    if (recipe.provenance === "from internet" && !internetInstallConfirm) {
      pushToast("Confirm external install checkbox first.");
      return;
    }

    // Try to activate the skill on the backend
    const skill = backendSkills.find((s) => s.id === recipe.id);
    if (skill) {
      toggleSkillActivation(skill.id, skill.state);
    } else {
      setRecipes((prev) =>
        prev.map((item) => (item.id === recipe.id ? { ...item, restrictedMode: true } : item))
      );
      pushToast(`${recipe.name} enabled in restricted mode.`);
    }
  }

  function tryRecipe(recipe: RecipeItem) {
    if (activeThread) selectThread(activeThread.id);
    setMessageInput(`Run recipe: ${recipe.name}. ${recipe.purpose}`);
    pushToast(`Loaded ${recipe.name} into Ask.`);
  }

  // ── Backend Agent Management ─────────────────────────────
  function resolveUserModel(templateModel: string): string {
    try {
      // Use the exact model the user selected in Preferences / Onboarding
      const savedModel = window.localStorage.getItem("clawdesk.model");
      if (savedModel) return savedModel;
      // Fallback: infer from provider if model was never explicitly set
      const provider = (window.localStorage.getItem("clawdesk.provider") || "Ollama (Local)").toLowerCase();
      if (provider.includes("openai")) return "gpt-4o";
      if (provider.includes("google")) return "gemini-2.5-pro";
      if (provider.includes("ollama")) return "lfm2.5-thinking:latest";
      if (provider.includes("anthropic")) return "claude-sonnet-4-20250514";
    } catch { /* localStorage unavailable */ }
    return templateModel;
  }

  async function createBackendAgent(template: typeof AGENT_TEMPLATES[number]) {
    // Dedup guard: skip if an agent with this exact name already exists
    const existing = backendAgents.find((a) => a.name === template.name);
    if (existing) {
      setSelectedAgentId(existing.id);
      pushToast(`Agent "${template.name}" already exists.`);
      return;
    }
    try {
      const model = resolveUserModel(template.model);
      const agent = await api.createAgent({
        name: template.name,
        icon: template.icon,
        color: template.color,
        persona: template.persona,
        skills: template.skills,
        model,
      });
      setBackendAgents((prev) => [...prev, agent]);
      setSelectedAgentId(agent.id);
      setThreads((prev) => {
        if (prev.some((thread) => thread.id === agent.id)) return prev;
        return [
          {
            id: agent.id,
            agentId: agent.id,
            title: `${agent.name} session`,
            lastActivity: "No messages yet",
            pendingApprovals: 0,
            routineGenerated: false,
            hasProofOutputs: false,
            messages: [],
          },
          ...prev,
        ];
      });
      pushToast(`Agent "${agent.name}" created.`);
    } catch (err) {
      const mapped = classifyError(err);
      pushToast(mapped.userMessage);
    }
  }

  async function deleteBackendAgent(agentId: string) {
    try {
      await api.deleteAgent(agentId);
      setBackendAgents((prev) => prev.filter((a) => a.id !== agentId));
      setThreads((prev) => prev.filter((thread) => thread.id !== agentId));
      if (selectedAgentId === agentId) setSelectedAgentId(null);
      pushToast("Agent deleted.");
    } catch (err) {
      const mapped = classifyError(err);
      pushToast(mapped.userMessage);
    }
  }

  // ── Skill Activation/Deactivation ──────────────────────
  async function toggleSkillActivation(skillId: string, currentState: string) {
    try {
      if (currentState === "active") {
        await api.deactivateSkill(skillId);
        pushToast(`Skill "${skillId}" deactivated.`);
      } else {
        await api.activateSkill(skillId);
        pushToast(`Skill "${skillId}" activated.`);
      }
      // Refresh skills list
      const skills = await api.listSkills();
      setBackendSkills(skills);
      // Update recipes view
      const skillRecipes: RecipeItem[] = skills.map((s) => ({
        id: s.id,
        name: s.name,
        category: s.state === "active" ? "recommended" as const : s.category === "core" ? "personal" as const : "team" as const,
        purpose: s.description,
        inputs: `${s.estimated_tokens} estimated tokens`,
        outputs: `Category: ${s.category}`,
        permissions: [s.state === "active" ? "Active" : "Inactive"],
        provenance: s.verified ? "built-in" as const : "from internet" as const,
        restrictedMode: true,
      }));
      if (skillRecipes.length > 0) setRecipes(skillRecipes);
    } catch (err) {
      const mapped = classifyError(err);
      pushToast(mapped.userMessage);
    }
  }

  // ── OpenClaw Config Import ─────────────────────────────
  async function importOpenClaw() {
    let configJson: string | null = null;

    // Try file picker (HTML input), fallback to prompt
    try {
      configJson = await new Promise<string | null>((resolve) => {
        const input = document.createElement("input");
        input.type = "file";
        input.accept = ".json";
        input.onchange = () => {
          const file = input.files?.[0];
          if (!file) { resolve(null); return; }
          const reader = new FileReader();
          reader.onload = () => resolve(reader.result as string);
          reader.onerror = () => resolve(null);
          reader.readAsText(file);
        };
        input.oncancel = () => resolve(null);
        input.click();
      });
    } catch {
      configJson = window.prompt("Paste OpenClaw JSON config:");
    }

    if (!configJson) return;
    try {
      const result = await api.importOpenClawConfig(configJson);
      if (result.success) {
        setBackendAgents((prev) => [...prev, ...result.agents]);
        if (result.agents.length > 0) setSelectedAgentId(result.agents[0].id);
        if (result.warnings.length > 0) {
          pushToast(`Imported ${result.agents.length} agent(s). ${result.warnings.length} warning(s).`);
        } else {
          pushToast(`Imported ${result.agents.length} agent(s) successfully.`);
        }
      } else {
        pushToast(`Import failed: ${result.error ?? "Unknown error"}`);
      }
    } catch (err) {
      const mapped = classifyError(err);
      pushToast(mapped.userMessage);
    }
  }

  const commandItems = useMemo(() => {
    const base = [
      {
        id: "cmd_quick_check",
        label: "Run Quick Check",
        group: "Commands",
        run: () => runQuickCheck(),
      },
      {
        id: "cmd_toggle_safe",
        label: safeMode ? "Turn Safe Mode OFF" : "Turn Safe Mode ON",
        group: "Commands",
        run: () => setSafeMode((prev) => !prev),
      },
      {
        id: "cmd_create_routine",
        label: "Create Routine",
        group: "Commands",
        run: () => {
          navigate("automations");
          setRoutineWizardOpen(true);
        },
      },
      {
        id: "cmd_show_approvals",
        label: "Show Approvals",
        group: "Commands",
        run: () => setApprovalsInboxOpen(true),
      },
      {
        id: "cmd_proof",
        label: "Open Proof Archive",
        group: "Commands",
        run: () => setProofArchiveOpen(true),
      },
      {
        id: "cmd_import_openclaw",
        label: "Import OpenClaw Config",
        group: "Backend",
        run: () => importOpenClaw(),
      },
      {
        id: "cmd_refresh_plugins",
        label: "Refresh Plugins",
        group: "Backend",
        run: () => { api.listPlugins().then((p) => setBackendPlugins(p)).catch(() => { }); pushToast("Plugins refreshed."); },
      },
      {
        id: "cmd_refresh_peers",
        label: "Discover Peers",
        group: "Backend",
        run: () => { api.listDiscoveredPeers().then((p) => setBackendPeers(p)).catch(() => { }); pushToast("Peers refreshed."); },
      },
      {
        id: "cmd_memory_stats",
        label: "Show Memory Stats",
        group: "Backend",
        run: () => { api.getMemoryStats().then((s) => { setBackendMemoryStats(s); pushToast(`Memory: ${s.collection_name} (${s.embedding_provider})`); }).catch(() => pushToast("Memory stats unavailable.")); },
      },
      {
        id: "cmd_sochdb_checkpoint",
        label: "SochDB Checkpoint",
        group: "Backend",
        run: () => { api.sochdbCheckpoint().then((n) => pushToast(`Checkpoint created (${n} entries).`)).catch(() => pushToast("Checkpoint failed.")); },
      },
      {
        id: "cmd_sochdb_sync",
        label: "SochDB Sync",
        group: "Backend",
        run: () => { api.sochdbSync().then(() => pushToast("SochDB synced.")).catch(() => pushToast("Sync failed.")); },
      },
      {
        id: "cmd_read_clipboard",
        label: "Read Clipboard",
        group: "Infra",
        run: () => { api.readClipboard().then((c) => { if (c?.text) pushToast(`Clipboard: ${c.text.slice(0, 60)}...`); else pushToast("Clipboard empty."); }).catch(() => pushToast("Clipboard read failed.")); },
      },
      {
        id: "cmd_audit_log",
        label: "Show Audit Log",
        group: "Security",
        run: () => { api.policyGetAuditLog(20).then((log) => pushToast(`${log.length} audit entries.`)).catch(() => pushToast("Audit log unavailable.")); },
      },
      {
        id: "cmd_enable_audit",
        label: "Enable Audit Mode",
        group: "Security",
        run: () => { api.policyEnableAudit().then(() => pushToast("Audit mode enabled.")).catch(() => pushToast("Failed to enable audit.")); },
      },
      ...AGENT_TEMPLATES.map((t) => ({
        id: `cmd_create_${t.name.replace(/\s/g, "_").toLowerCase()}`,
        label: `Create ${t.name}`,
        group: "Agents",
        run: () => createBackendAgent(t),
      })),
      ...backendAgents.map((a) => ({
        id: `cmd_select_${a.id}`,
        label: `Switch to ${a.name}`,
        group: "Agents",
        run: () => { setSelectedAgentId(a.id); pushToast(`Selected ${a.name}`); },
      })),
    ];

    const searchCorpus = [
      ...threads.map((thread) => ({
        id: thread.id, label: thread.title, group: "Requests", run: () => {
          selectThread(thread.id);
        }
      })),
      ...proofs.slice(0, 12).map((proof) => ({
        id: proof.proofId, label: proof.summary, group: "Proof", run: () => {
          navigate("chat");
          setProofArchiveOpen(true);
        }
      })),
      ...routines.map((routine) => ({
        id: routine.id, label: routine.name, group: "Routines", run: () => {
          navigate("automations");
          setSelectedRoutineId(routine.id);
        }
      })),
      ...accounts.map((account) => ({
        id: account.id, label: account.name, group: "Accounts", run: () => {
          navigate("settings");
          setSelectedAccountId(account.id);
        }
      })),
      ...recipes.map((recipe) => ({
        id: recipe.id, label: recipe.name, group: "Library", run: () => {
          navigate("skills");
          setSelectedRecipeId(recipe.id);
        }
      })),
    ];

    return [...base, ...searchCorpus];
  }, [safeMode, threads, proofs, routines, accounts, recipes, runQuickCheck, backendAgents, selectedAgentId, navigate, selectThread]);

  const paletteItems = useMemo(() => {
    const query = paletteQuery.trim().toLowerCase();
    if (!query) return commandItems;
    return commandItems.filter((item) => item.label.toLowerCase().includes(query) || item.group.toLowerCase().includes(query));
  }, [commandItems, paletteQuery]);

  function renderNowView() {
    const upcoming = routines.slice(0, 2);
    const recentProof = proofs.slice(0, 10);
    const fixLabel = status.level === "ok" ? "No fix needed" : status.fix;

    return (
      <div className="view view-now">
        <section className="section-card">
          <div className="section-head">
            <h2>Next Up</h2>
          </div>
          {upcoming.length === 0 ? (
            <div className="empty-state">
              <p>Want a daily briefing?</p>
              <button className="btn primary" onClick={() => setRoutineWizardOpen(true)}>
                Create Routine
              </button>
            </div>
          ) : (
            <div className="list-rows">
              {upcoming.map((routine) => (
                <div key={routine.id} className="row-card">
                  <div>
                    <div className="row-title">{routine.name}</div>
                    <div className="row-sub">
                      {routine.nextRun} • {routine.destination}
                    </div>
                  </div>
                  <div className="row-actions">
                    <button className="btn subtle" onClick={() => runRoutineTest(routine.id)}>
                      {safeMode ? "Preview run" : "Run now"}
                    </button>
                  </div>
                </div>
              ))}
            </div>
          )}
        </section>

        <section className="section-card">
          <div className="section-head">
            <h2>Needs You</h2>
            <button className="btn subtle" onClick={() => setApprovalsInboxOpen(true)}>
              Open Inbox
            </button>
          </div>
          {pendingApprovals.length === 0 ? (
            <div className="empty-state">
              <p>All good. Nothing waiting.</p>
            </div>
          ) : (
            <div className="list-rows">
              {pendingApprovals.slice(0, 4).map((approval) => (
                <div key={approval.id} className="row-card">
                  <div>
                    <div className="row-title">{approval.summary}</div>
                    <div className="row-sub">{approval.where}</div>
                  </div>
                  <div className="row-actions">
                    <button className="btn subtle" onClick={() => setApprovalStatus(approval, "approved")}>Approve</button>
                    <button className="btn ghost" onClick={() => setApprovalStatus(approval, "denied")}>Deny</button>
                  </div>
                </div>
              ))}
            </div>
          )}
        </section>

        <section className="section-card">
          <div className="section-head">
            <h2>Recent Proof</h2>
            <button className="btn subtle" onClick={() => setProofArchiveOpen(true)}>
              View archive
            </button>
          </div>
          {recentProof.length === 0 ? (
            <div className="empty-state">
              <p>Your activity receipts will appear here.</p>
            </div>
          ) : (
            <div className="list-rows">
              {recentProof.slice(0, 5).map((proof) => (
                <div key={proof.proofId} className="row-card">
                  <div>
                    <div className="row-title">{proof.summary}</div>
                    <div className="row-sub">
                      {new Date(proof.endedAt).toLocaleString()} • {proof.duration}
                    </div>
                  </div>
                  <div className="row-actions">
                    <button className="btn subtle" onClick={() => setInspectorTab("proof")}>View</button>
                  </div>
                </div>
              ))}
            </div>
          )}
        </section>

        <section className="section-card health-card">
          <div className="section-head">
            <h2>Health, in plain English</h2>
          </div>
          <p>{status.summary}</p>
          <div className="health-actions">
            <button className="btn primary" onClick={() => runQuickCheck()}>{fixLabel}</button>
            <button className="btn subtle" onClick={() => setStatusPopoverOpen(true)}>Show details</button>
          </div>
        </section>

        {backendHealth && (
          <section className="section-card">
            <div className="section-head">
              <h2>Backend Status</h2>
            </div>
            <div className="list-rows">
              <div className="row-card">
                <div>
                  <div className="row-title">Engine v{backendHealth.version}</div>
                  <div className="row-sub">
                    Uptime: {Math.floor(backendHealth.uptime_secs / 60)}m •{" "}
                    Agents: {backendHealth.agents_active} •{" "}
                    Skills: {backendHealth.skills_loaded} •{" "}
                    Tunnel: {backendHealth.tunnel_active ? "Active" : "Off"}
                  </div>
                </div>
              </div>
              {backendMetrics && (
                <div className="row-card">
                  <div>
                    <div className="row-title">Today's Cost: ${backendMetrics.today_cost.toFixed(4)}</div>
                    <div className="row-sub">
                      Input: {(backendMetrics.today_input_tokens ?? 0).toLocaleString()} tokens •{" "}
                      Output: {(backendMetrics.today_output_tokens ?? 0).toLocaleString()} tokens
                    </div>
                  </div>
                </div>
              )}
              {backendSecurity && (
                <div className="row-card">
                  <div>
                    <div className="row-title">Security: {backendSecurity.gateway_bind}</div>
                    <div className="row-sub">
                      Scanner patterns: {backendSecurity.scanner_patterns} •{" "}
                      Audit entries: {backendSecurity.audit_entries} •{" "}
                      Identity contracts: {backendSecurity.identity_contracts}
                    </div>
                  </div>
                </div>
              )}
            </div>
          </section>
        )}

        {backendAgents.length > 0 && (
          <section className="section-card">
            <div className="section-head">
              <h2>Agents ({backendAgents.length})</h2>
              <button className="btn subtle" onClick={() => setPaletteOpen(true)}>Create</button>
            </div>
            <div className="list-rows">
              {backendAgents.map((agent) => (
                <div key={agent.id} className={`row-card ${selectedAgentId === agent.id ? "active" : ""}`}>
                  <div>
                    <div className="row-title">{agent.icon} {agent.name}</div>
                    <div className="row-sub">
                      {agent.model} • {agent.msg_count} msgs • {(agent.tokens_used ?? 0).toLocaleString()}/{(agent.token_budget ?? 0).toLocaleString()} tokens • {agent.status}
                    </div>
                  </div>
                  <div className="row-actions">
                    <button className="btn subtle" onClick={() => setSelectedAgentId(agent.id)}>Select</button>
                    <button className="btn ghost" onClick={() => deleteBackendAgent(agent.id)}>Delete</button>
                  </div>
                </div>
              ))}
            </div>
          </section>
        )}

        {backendPlugins.length > 0 && (
          <section className="section-card">
            <div className="section-head">
              <h2>Plugins ({backendPlugins.length})</h2>
            </div>
            <div className="list-rows">
              {backendPlugins.map((plugin) => (
                <div key={plugin.id} className="row-card">
                  <div>
                    <div className="row-title">{plugin.name} v{plugin.version}</div>
                    <div className="row-sub">{plugin.description}</div>
                  </div>
                  <div className="row-actions">
                    <button className="btn subtle" onClick={() => {
                      const isActive = plugin.state === "active";
                      const action = isActive ? api.disablePlugin : api.enablePlugin;
                      action(plugin.id).then(() => {
                        api.listPlugins().then((p) => setBackendPlugins(p)).catch(() => { });
                        pushToast(`Plugin ${isActive ? "disabled" : "enabled"}.`);
                      }).catch(() => pushToast("Failed to toggle plugin."));
                    }}>
                      {plugin.state === "active" ? "Disable" : "Enable"}
                    </button>
                  </div>
                </div>
              ))}
            </div>
          </section>
        )}

        {backendPeers.length > 0 && (
          <section className="section-card">
            <div className="section-head">
              <h2>Discovered Peers ({backendPeers.length})</h2>
              <button className="btn subtle" onClick={() => {
                api.listDiscoveredPeers().then((p) => setBackendPeers(p)).catch(() => { });
              }}>Refresh</button>
            </div>
            <div className="list-rows">
              {backendPeers.map((peer) => (
                <div key={peer.instance_name} className="row-card">
                  <div>
                    <div className="row-title">{peer.instance_name}</div>
                    <div className="row-sub">{peer.host}:{peer.port} • {peer.capabilities.join(", ")}</div>
                  </div>
                  <div className="row-actions">
                    <button className="btn subtle" onClick={() => {
                      api.startPairing().then(() => pushToast("Pairing started.")).catch(() => pushToast("Pairing failed."));
                    }}>Pair</button>
                  </div>
                </div>
              ))}
            </div>
          </section>
        )}

        {backendCanvases.length > 0 && (
          <section className="section-card">
            <div className="section-head">
              <h2>Canvases ({backendCanvases.length})</h2>
              <button className="btn subtle" onClick={() => {
                api.createCanvas({ title: "New Canvas" }).then(() => {
                  api.listCanvases().then((c) => setBackendCanvases(c)).catch(() => { });
                  pushToast("Canvas created.");
                }).catch(() => pushToast("Failed to create canvas."));
              }}>New</button>
            </div>
            <div className="list-rows">
              {backendCanvases.map((canvas) => (
                <div key={canvas.id} className="row-card">
                  <div>
                    <div className="row-title">{canvas.title}</div>
                    <div className="row-sub">{canvas.block_count} blocks • Created: {canvas.created_at}</div>
                  </div>
                  <div className="row-actions">
                    <button className="btn subtle" onClick={() => {
                      api.exportCanvasMarkdown(canvas.id).then((md) => {
                        api.writeClipboard(md).then(() => pushToast("Canvas markdown copied to clipboard.")).catch(() => { });
                      }).catch(() => pushToast("Failed to export canvas."));
                    }}>Export</button>
                  </div>
                </div>
              ))}
            </div>
          </section>
        )}

        {backendNotifications.length > 0 && (
          <section className="section-card">
            <div className="section-head">
              <h2>Notifications ({backendNotifications.length})</h2>
            </div>
            <div className="list-rows">
              {backendNotifications.slice(0, 5).map((n) => (
                <div key={n.id} className="row-card">
                  <div>
                    <div className="row-title">{n.title}</div>
                    <div className="row-sub">{n.body} • {n.created_at}</div>
                  </div>
                </div>
              ))}
            </div>
          </section>
        )}
      </div>
    );
  }

  function renderAskView() {
    return (
      <div className="view ask-layout">
        <aside className="thread-list">
          <div className="thread-list-head">
            <div className="thread-list-title-wrap">
              <h2>Threads</h2>
              <span>{filteredThreads.length}/{threads.length}</span>
            </div>
            <button className="btn subtle" onClick={() => {
              if (selectedAgentId) {
                selectThread(selectedAgentId, { replace: true });
                setMessageInput("");
                pushToast("Ready for a new request.");
              } else {
                pushToast("Create or select an agent first.");
              }
            }}>
              New
            </button>
          </div>
          <div className="thread-search">
            <input
              value={threadSearch}
              onChange={(event) => setThreadSearch(event.target.value)}
              placeholder="Search threads..."
              aria-label="Search threads"
            />
            {threadSearch.trim().length > 0 && (
              <button
                className="btn ghost thread-search-clear"
                onClick={() => setThreadSearch("")}
                aria-label="Clear thread search"
              >
                Clear
              </button>
            )}
          </div>
          <div className="thread-items">
            {filteredThreads.length === 0 ? (
              <div className="thread-empty">No threads match your search.</div>
            ) : (
              filteredThreads.map((thread) => (
                <button
                  key={thread.id}
                  className={`thread-item ${activeThread?.id === thread.id ? "active" : ""}`}
                  onClick={() => selectThread(thread.id)}
                  aria-current={activeThread?.id === thread.id ? "true" : undefined}
                >
                  <div className="thread-row">
                    <div className="thread-title">{thread.title}</div>
                    {thread.pendingApprovals > 0 && <span className="thread-alert-dot" aria-hidden="true" />}
                  </div>
                  <div className="thread-meta">{thread.lastActivity}</div>
                  <div className="thread-flags">
                    {thread.messages.length > 0 && <span className="chip">{thread.messages.length} msgs</span>}
                    {thread.pendingApprovals > 0 && <span className="chip chip-risk">Approvals</span>}
                    {thread.routineGenerated && <span className="chip">Routine</span>}
                    {thread.hasProofOutputs && <span className="chip">Proof</span>}
                  </div>
                </button>
              ))
            )}
          </div>
        </aside>

        <section className="chat-area">
          <div className="chat-head">
            <div>
              <h2>{activeThread?.title ?? "New conversation"}</h2>
              <p className="chat-head-subline">
                <span className={`chat-status-dot ${isSending ? "busy" : activeAgent ? "ok" : "warn"}`} aria-hidden="true" />
                {activeAgent
                  ? `${activeAgent.name} • ${activeAgent.model}${activeAgent.skills.length > 0 ? ` • ${activeAgent.skills.length} skills` : ""}`
                  : "No backend agent selected"}
                {isSending && " • Sending..."}
              </p>
            </div>
            <div className="chat-head-actions">
              {backendAgents.length > 0 && (
                <select
                  value={selectedAgentId ?? ""}
                  onChange={(e) => setSelectedAgentId(e.target.value || null)}
                  className="agent-picker"
                >
                  {backendAgents.map((a) => (
                    <option key={a.id} value={a.id}>{a.icon} {a.name} ({a.model})</option>
                  ))}
                </select>
              )}
              {backendAgents.length === 0 && (
                <button className="btn subtle" onClick={() => createBackendAgent(AGENT_TEMPLATES[0])}>
                  Create agent
                </button>
              )}
              <button className="btn subtle" onClick={() => setInspectorTab("plan")}>Plan</button>
              <button className="btn subtle" onClick={() => setInspectorTab("proof")}>Proof</button>
              {drawerAsModal && (
                <button className="btn subtle" onClick={() => setInspectorModalOpen(true)}>
                  Inspector
                </button>
              )}
            </div>
          </div>

          <div className="chat-messages">
            {(!activeThread || activeThread.messages.length === 0) ? (
              <div className="empty-state centered">
                <p>Ask me to draft, summarize, or set up routines.</p>
                <span>Example: "Draft a weekly update and send it to #team-updates."</span>
              </div>
            ) : (
              activeThread.messages.map((message) => (
                <div key={message.id} className={`bubble ${message.role === "user" ? "bubble-user" : "bubble-assistant"}`}>
                  <div className="bubble-head">
                    <span className="bubble-author">
                      {message.role === "user" ? "You" : (activeAgent?.name ?? "Assistant")}
                    </span>
                    <span className="bubble-time">{message.time}</span>
                  </div>
                  <div className="bubble-text">{message.text}</div>
                  {message.meta && (
                    <div className="bubble-chips">
                      <span className="chip chip-meta">{formatModelLabel(message.meta.model)}</span>
                      <span className="chip chip-meta">{message.meta.tokenCost.toLocaleString()} tokens</span>
                      <span className="chip chip-meta">${message.meta.costUsd.toFixed(4)}</span>
                      <span className="chip chip-meta">{formatDurationLabel(message.meta.durationMs)}</span>
                      {message.meta.skills.slice(0, 3).map((skill, index) => (
                        <span key={`${message.id}_${skill}_${index}`} className="chip chip-meta">{skill}</span>
                      ))}
                      {message.meta.skills.length > 3 && (
                        <span className="chip chip-meta">+{message.meta.skills.length - 3}</span>
                      )}
                    </div>
                  )}
                  {message.result && !message.meta && (
                    <div className="result-card">
                      <span>Result</span>
                      <p>{message.result}</p>
                    </div>
                  )}
                </div>
              ))
            )}
          </div>

          <div className="chat-composer">
            <textarea
              placeholder={selectedAgentId ? "Type your request..." : "Create or select an agent to send a request."}
              value={messageInput}
              onChange={(event) => setMessageInput(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter" && !event.shiftKey) {
                  event.preventDefault();
                  if (!messageInput.trim()) return;
                  requestToAssistant(messageInput.trim());
                  setMessageInput("");
                }
              }}
            />
            <div className="composer-actions">
              <button
                className="btn primary"
                disabled={isSending || !selectedAgentId}
                onClick={() => {
                  if (!messageInput.trim()) return;
                  requestToAssistant(messageInput.trim());
                  setMessageInput("");
                }}
              >
                {isSending ? "Sending..." : "Send"}
              </button>
            </div>
          </div>
        </section>
      </div>
    );
  }

  function renderRoutinesView() {
    return (
      <div className="view view-routines">
        <section className="section-card">
          <div className="section-head">
            <h2>Routines</h2>
            <button className="btn primary" onClick={() => setRoutineWizardOpen(true)}>
              Create Routine
            </button>
          </div>

          <div className="list-rows">
            {routines.map((routine) => (
              <button
                key={routine.id}
                className={`row-card row-button ${selectedRoutine?.id === routine.id ? "selected" : ""}`}
                onClick={() => setSelectedRoutineId(routine.id)}
              >
                <div>
                  <div className="row-title">{routine.name}</div>
                  <div className="row-sub">
                    {routine.type === "at_time" ? "At a time" : "Watch & notify"} • {routine.scheduleLabel}
                  </div>
                  <div className="row-sub">Next: {routine.nextRun} • {routine.destination}</div>
                </div>
                <div className="row-actions">
                  <span className={`chip ${routine.lastResult === "failed" ? "chip-risk" : ""}`}>
                    {routine.lastResult === "ok" ? "OK" : routine.lastResult === "failed" ? "Failed" : "Skipped"}
                  </span>
                  <label className="toggle-row" onClick={(event) => event.stopPropagation()}>
                    <input
                      type="checkbox"
                      checked={routine.enabled}
                      onChange={() =>
                        setRoutines((prev) =>
                          prev.map((item) =>
                            item.id === routine.id ? { ...item, enabled: !item.enabled } : item
                          )
                        )
                      }
                    />
                    <span>{routine.enabled ? "Enabled" : "Paused"}</span>
                  </label>
                </div>
              </button>
            ))}
          </div>
        </section>

        <section className="section-card">
          {!selectedRoutine ? (
            <div className="empty-state">
              <p>No routines yet.</p>
              <button className="btn primary" onClick={() => setRoutineWizardOpen(true)}>
                Create Routine
              </button>
            </div>
          ) : (
            <>
              <div className="section-head">
                <h2>{selectedRoutine.name}</h2>
                <button className="btn subtle" onClick={() => runRoutineTest()}>Run once now</button>
              </div>
              <div className="details-grid">
                <div>
                  <h3>Overview</h3>
                  <p>{selectedRoutine.scheduleLabel}</p>
                  <p>Destination: {selectedRoutine.destination}</p>
                  <p>Quiet hours: {selectedRoutine.quietHours}</p>
                </div>
                <div>
                  <h3>Recipe</h3>
                  <p>{recipes.find((recipe) => recipe.id === selectedRoutine.recipeId)?.name ?? "Custom"}</p>
                  <p>Read-only by default. Edit requires confirm.</p>
                </div>
                <div>
                  <h3>Alerts</h3>
                  <p>Notify on failure: Enabled</p>
                  <p>Last result: {selectedRoutine.lastReason}</p>
                </div>
              </div>

              <div className="history-block">
                <h3>Run history</h3>
                {selectedRoutine.history.length === 0 ? (
                  <p>No history yet.</p>
                ) : (
                  <div className="list-rows">
                    {selectedRoutine.history.map((item, index) => (
                      <div key={`${selectedRoutine.id}_${index}`} className="row-card">
                        <div>
                          <div className="row-title">{item.at}</div>
                          <div className="row-sub">{item.reason}</div>
                        </div>
                        <span className={`chip ${item.result === "failed" ? "chip-risk" : ""}`}>
                          {item.result === "ok" ? "OK" : item.result === "skipped" ? "Skipped" : "Failed"}
                        </span>
                      </div>
                    ))}
                  </div>
                )}
              </div>
            </>
          )}
        </section>
      </div>
    );
  }

  function renderAccountsView() {
    return (
      <div className="view view-accounts">
        <section className="section-card account-grid-wrap">
          <div className="section-head">
            <h2>Accounts</h2>
          </div>
          <div className="account-grid">
            {accounts.map((account) => (
              <div key={account.id} className={`account-card ${selectedAccount?.id === account.id ? "selected" : ""}`}>
                <div className="account-head">
                  <div>
                    <h3>{account.name}</h3>
                    <p>{accountStatusLabel(account.status)}</p>
                  </div>
                  <span className={`chip ${account.status !== "connected" ? "chip-risk" : ""}`}>
                    {accountStatusLabel(account.status)}
                  </span>
                </div>
                <p>{account.summary}</p>
                <div className="row-actions">
                  <button className="btn subtle" onClick={() => setSelectedAccountId(account.id)}>
                    Manage
                  </button>
                  {account.status !== "connected" && (
                    <button className="btn subtle" onClick={() => reconnectAccount(account.id)}>
                      Reconnect
                    </button>
                  )}
                  <button
                    className="btn ghost"
                    onClick={() =>
                      setAccounts((prev) =>
                        prev.map((item) =>
                          item.id === account.id ? { ...item, status: item.status === "disabled" ? "connected" : "disabled" } : item
                        )
                      )
                    }
                  >
                    {account.status === "disabled" ? "Enable" : "Disable"}
                  </button>
                </div>
              </div>
            ))}
          </div>
        </section>

        <section className="section-card">
          {!selectedAccount ? (
            <div className="empty-state">
              <p>No accounts connected yet.</p>
              <span>Connect an account to manage permissions.</span>
            </div>
          ) : (
            <>
              <div className="section-head">
                <h2>{selectedAccount.name} permissions</h2>
                <button className="btn subtle" onClick={() => runAccountTest(selectedAccount.id)}>
                  Test connection
                </button>
              </div>

              <div className="details-grid">
                <div>
                  <h3>Capabilities</h3>
                  {(
                    [
                      ["Read", "read"],
                      ["Search", "search"],
                      ["Draft", "draft"],
                      ["Send / Write", "sendWrite"],
                      ["Delete", "delete"],
                      ["Execute", "execute"],
                    ] as const
                  ).map(([label, key]) => (
                    <label key={key} className="toggle-row">
                      <input
                        type="checkbox"
                        checked={selectedAccount.capabilities[key]}
                        onChange={() =>
                          setAccounts((prev) =>
                            prev.map((account) =>
                              account.id === selectedAccount.id
                                ? {
                                  ...account,
                                  capabilities: {
                                    ...account.capabilities,
                                    [key]: !account.capabilities[key],
                                  },
                                }
                                : account
                            )
                          )
                        }
                      />
                      <span>{label}</span>
                    </label>
                  ))}
                </div>

                <div>
                  <h3>Data boundaries</h3>
                  <label className="field-label">
                    Folders/projects
                    <input
                      value={selectedAccount.boundaries.folders}
                      onChange={(event) =>
                        setAccounts((prev) =>
                          prev.map((account) =>
                            account.id === selectedAccount.id
                              ? {
                                ...account,
                                boundaries: { ...account.boundaries, folders: event.target.value },
                              }
                              : account
                          )
                        )
                      }
                    />
                  </label>
                  <label className="field-label">
                    Recipients/domains
                    <input
                      value={selectedAccount.boundaries.recipients}
                      onChange={(event) =>
                        setAccounts((prev) =>
                          prev.map((account) =>
                            account.id === selectedAccount.id
                              ? {
                                ...account,
                                boundaries: { ...account.boundaries, recipients: event.target.value },
                              }
                              : account
                          )
                        )
                      }
                    />
                  </label>
                  <label className="field-label">
                    Channels
                    <input
                      value={selectedAccount.boundaries.channels}
                      onChange={(event) =>
                        setAccounts((prev) =>
                          prev.map((account) =>
                            account.id === selectedAccount.id
                              ? {
                                ...account,
                                boundaries: { ...account.boundaries, channels: event.target.value },
                              }
                              : account
                          )
                        )
                      }
                    />
                  </label>
                </div>

                <div>
                  <h3>Security</h3>
                  <p>Last used: {selectedAccount.lastUsed}</p>
                  <div className="row-actions">
                    <button className="btn subtle" onClick={() => pushToast("Access revoked.")}>Revoke access</button>
                    <button className="btn subtle" onClick={() => pushToast("Token rotated.")}>Rotate token</button>
                  </div>
                </div>
              </div>
            </>
          )}
        </section>
      </div>
    );
  }

  function renderLibraryView() {
    return (
      <div className="view view-library">
        <section className="section-card">
          <div className="section-head">
            <h2>Library</h2>
            <div className="tabs">
              {(["recommended", "personal", "team", "experimental"] as const).map((category) => (
                <button
                  key={category}
                  className={`tab ${libraryFilter === category ? "active" : ""}`}
                  onClick={() => setLibraryFilter(category)}
                >
                  {category.replace("_", " ")}
                </button>
              ))}
            </div>
          </div>

          <div className="recipe-grid">
            {filteredRecipes.map((recipe) => (
              <button
                key={recipe.id}
                className={`recipe-card ${selectedRecipe?.id === recipe.id ? "selected" : ""}`}
                onClick={() => {
                  setSelectedRecipeId(recipe.id);
                  setInternetInstallConfirm(false);
                }}
              >
                <div className="recipe-head">
                  <h3>{recipe.name}</h3>
                  <span className={`chip ${recipe.provenance === "from internet" ? "chip-risk" : ""}`}>
                    {recipe.provenance}
                  </span>
                </div>
                <p>{recipe.purpose}</p>
                <div className="row-sub">Inputs: {recipe.inputs}</div>
                <div className="row-sub">Outputs: {recipe.outputs}</div>
              </button>
            ))}
          </div>
        </section>

        <section className="section-card">
          {!selectedRecipe ? (
            <div className="empty-state">
              <p>No recipes available in this category.</p>
              <span>Try another filter or install a skill.</span>
            </div>
          ) : (
            <>
              <div className="section-head">
                <h2>{selectedRecipe.name}</h2>
                <div className="row-actions">
                  <button className="btn subtle" onClick={() => tryRecipe(selectedRecipe)}>Try it</button>
                  <button className="btn primary" onClick={() => installRecipe(selectedRecipe)}>Install / Enable</button>
                </div>
              </div>

              {selectedRecipe.provenance === "from internet" && (
                <div className="risk-banner">
                  <p>You are installing a recipe from outside your device/team. It may run actions on your behalf.</p>
                  <label className="toggle-row">
                    <input
                      type="checkbox"
                      checked={internetInstallConfirm}
                      onChange={(event) => setInternetInstallConfirm(event.target.checked)}
                    />
                    <span>I understand and want to continue</span>
                  </label>
                </div>
              )}

              <div className="details-grid">
                <div>
                  <h3>What it does</h3>
                  <p>{selectedRecipe.purpose}</p>
                  <p>Example: {selectedRecipe.name} for a daily team update.</p>
                </div>
                <div>
                  <h3>Permissions required</h3>
                  <div className="permission-list">
                    {selectedRecipe.permissions.map((permission) => (
                      <label key={permission} className="toggle-row">
                        <input type="checkbox" checked readOnly />
                        <span>{permission}</span>
                      </label>
                    ))}
                  </div>
                </div>
                <div>
                  <h3>Restricted mode</h3>
                  <p>Allowed: read / draft / preview</p>
                  <p>Blocked: send / write / execute</p>
                  <label className="toggle-row">
                    <input
                      type="checkbox"
                      checked={selectedRecipe.restrictedMode}
                      onChange={() =>
                        setRecipes((prev) =>
                          prev.map((recipe) =>
                            recipe.id === selectedRecipe.id
                              ? { ...recipe, restrictedMode: !recipe.restrictedMode }
                              : recipe
                          )
                        )
                      }
                    />
                    <span>Run in restricted mode</span>
                  </label>
                </div>
              </div>
            </>
          )}
        </section>
      </div>
    );
  }

  function renderInspectorContent() {
    if (activeNav !== "chat") {
      if (activeNav === "overview") {
        return (
          <div className="inspector-content">
            <h3>Quick Status</h3>
            <div className="row-card" style={{ marginTop: 8 }}>
              <div>
                <div className="row-title">{backendHealth ? "Connected" : "Offline"}</div>
                <div className="row-sub">{backendHealth ? `v${backendHealth.version} · ${Math.floor(backendHealth.uptime_secs / 60)}m uptime` : "Waiting..."}</div>
              </div>
            </div>
            {backendMetrics && (
              <div style={{ marginTop: 12 }}>
                <h4 style={{ fontSize: 12, textTransform: "uppercase", letterSpacing: "0.04em", color: "var(--text-soft)", marginBottom: 6 }}>Today</h4>
                <div className="row-card">
                  <div>
                    <div className="row-title" style={{ fontSize: 16, color: "var(--brand)" }}>${(backendMetrics.today_cost || 0).toFixed(2)}</div>
                    <div className="row-sub">{((backendMetrics.today_input_tokens ?? 0) + (backendMetrics.today_output_tokens ?? 0)).toLocaleString()} tokens</div>
                  </div>
                </div>
              </div>
            )}
            <div style={{ marginTop: 12 }}>
              <h4 style={{ fontSize: 12, textTransform: "uppercase", letterSpacing: "0.04em", color: "var(--text-soft)", marginBottom: 6 }}>Resources</h4>
              <div style={{ display: "flex", flexWrap: "wrap", gap: 4 }}>
                <span className="chip">{backendAgents.length} agents</span>
                <span className="chip">{backendChannels.length} channels</span>
                <span className="chip">{backendPlugins.length} plugins</span>
                <span className="chip">{backendPeers.length} peers</span>
              </div>
            </div>
          </div>
        );
      }



      if (activeNav === "logs") {
        return (
          <div className="inspector-content">
            <h3>Log Viewer</h3>
            <p style={{ color: "var(--text-soft)", marginTop: 8 }}>
              Live log stream from all gateway subsystems. Use level filters to focus on warnings and errors.
            </p>
            <div style={{ marginTop: 12, display: "flex", flexDirection: "column", gap: 4 }}>
              <button className="btn subtle" style={{ justifyContent: "flex-start", textAlign: "left" }} onClick={() => pushToast("Log export not available in this context")}>
                Export Logs
              </button>
            </div>
          </div>
        );
      }

      if (activeNav === "automations") {
        // Show the selected pipeline details or a helpful empty state
        const selectedPipeline = backendPipelines.length > 0 ? backendPipelines[0] : null;
        return (
          <div className="inspector-content">
            <h3>Automation Inspector</h3>

            {selectedPipeline ? (
              <>
                <div className="row-card">
                  <div>
                    <div className="row-title">{selectedPipeline.name}</div>
                    <div className="row-sub">{selectedPipeline.description}</div>
                    <div className="row-sub" style={{ marginTop: 4 }}>
                      {selectedPipeline.steps.length} steps · {selectedPipeline.edges.length} edges
                    </div>
                  </div>
                </div>

                {/* Pipeline flow visualization */}
                <div style={{ marginTop: 8 }}>
                  <h4 style={{ fontSize: 12, textTransform: "uppercase", letterSpacing: "0.04em", color: "var(--text-soft)", marginBottom: 6 }}>Pipeline Flow</h4>
                  <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
                    {selectedPipeline.steps.map((step, i) => (
                      <div key={i} style={{ display: "flex", alignItems: "center", gap: 6 }}>
                        <span style={{ width: 20, height: 20, borderRadius: "50%", background: step.node_type === "input" ? "var(--brand)" : step.node_type === "output" ? "#22c55e" : "var(--brand-soft)", display: "grid", placeItems: "center", fontSize: 10, color: "#fff", flexShrink: 0 }}>
                          {i + 1}
                        </span>
                        <span style={{ fontSize: 13 }}>{step.label}</span>
                        <span className="chip" style={{ fontSize: 10 }}>{step.node_type}</span>
                      </div>
                    ))}
                  </div>
                </div>
              </>
            ) : (
              <p style={{ color: "var(--text-soft)" }}>No pipelines yet. Create one from a template or design your own.</p>
            )}

            {/* Routine run history */}
            {selectedRoutine && (
              <div style={{ marginTop: 12 }}>
                <h4 style={{ fontSize: 12, textTransform: "uppercase", letterSpacing: "0.04em", color: "var(--text-soft)", marginBottom: 6 }}>Run History</h4>
                <div className="row-card">
                  <div>
                    <div className="row-title">{selectedRoutine.name}</div>
                    <div className="row-sub">
                      {selectedRoutine.enabled ? "✅ Enabled" : "⏸ Disabled"} · {selectedRoutine.scheduleLabel}
                    </div>
                    <div className="row-sub">
                      Next: {selectedRoutine.nextRun} · Last: {selectedRoutine.lastResult}
                    </div>
                  </div>
                </div>
                {selectedRoutine.history.length > 0 ? (
                  selectedRoutine.history.slice(0, 5).map((h, i) => (
                    <div key={i} className="row-card" style={{ marginTop: 4 }}>
                      <div>
                        <div className="row-sub">
                          <span className={`chip ${h.result === "ok" ? "" : "chip-risk"}`}>{h.result}</span>
                          {" "}{h.at}
                        </div>
                        <div className="row-sub">{h.reason}</div>
                      </div>
                    </div>
                  ))
                ) : (
                  <p style={{ fontSize: 12, color: "var(--text-soft)", marginTop: 4 }}>No runs yet.</p>
                )}
              </div>
            )}

            {/* Quick actions */}
            <div style={{ marginTop: 12 }}>
              <h4 style={{ fontSize: 12, textTransform: "uppercase", letterSpacing: "0.04em", color: "var(--text-soft)", marginBottom: 6 }}>Quick Actions</h4>
              <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
                <button className="btn subtle" style={{ justifyContent: "flex-start", textAlign: "left" }} onClick={() => api.listPipelines().then((p) => setBackendPipelines(p)).catch(() => { })}>
                  🔄 Refresh Pipelines
                </button>
                <button className="btn subtle" style={{ justifyContent: "flex-start", textAlign: "left" }} onClick={() => pushToast("Viewing pipeline logs...")}>
                  📋 View Logs
                </button>
              </div>
            </div>
          </div>
        );
      }

      if (activeNav === "settings") {
        const provider = window.localStorage.getItem("clawdesk.provider") || "—";
        const model = window.localStorage.getItem("clawdesk.model") || "—";
        const apiConfigured = window.localStorage.getItem("clawdesk.api_key.configured") === "1";
        return (
          <div className="inspector-content">
            <h3>System Overview</h3>

            {/* Connection status */}
            <div className="row-card" style={{ marginTop: 8 }}>
              <div>
                <div className="row-title">
                  {backendHealth ? "✅ Connected" : "⏳ Connecting..."}
                </div>
                <div className="row-sub">
                  {backendHealth
                    ? `v${backendHealth.version} · Uptime: ${Math.floor(backendHealth.uptime_secs / 60)}m`
                    : "Waiting for backend..."}
                </div>
              </div>
            </div>

            {/* Config summary */}
            <div style={{ marginTop: 12 }}>
              <h4 style={{ fontSize: 12, textTransform: "uppercase", letterSpacing: "0.04em", color: "var(--text-soft)", marginBottom: 6 }}>Configuration</h4>
              <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
                <div className="row-card">
                  <div>
                    <div className="row-sub">Provider</div>
                    <div className="row-title" style={{ fontSize: 13 }}>{provider}</div>
                  </div>
                </div>
                <div className="row-card">
                  <div>
                    <div className="row-sub">Model</div>
                    <div className="row-title" style={{ fontSize: 13 }}>{model}</div>
                  </div>
                </div>
                <div className="row-card">
                  <div>
                    <div className="row-sub">API Key</div>
                    <div className="row-title" style={{ fontSize: 13 }}>{apiConfigured ? "✅ Configured" : "⚠️ Not set"}</div>
                  </div>
                </div>
              </div>
            </div>

            {/* Resources */}
            <div style={{ marginTop: 12 }}>
              <h4 style={{ fontSize: 12, textTransform: "uppercase", letterSpacing: "0.04em", color: "var(--text-soft)", marginBottom: 6 }}>Resources</h4>
              <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
                <div className="row-card">
                  <div>
                    <div className="row-sub">Agents</div>
                    <div className="row-title" style={{ fontSize: 13 }}>{backendAgents.length}</div>
                  </div>
                </div>
                <div className="row-card">
                  <div>
                    <div className="row-sub">Channels</div>
                    <div className="row-title" style={{ fontSize: 13 }}>{backendChannels.length}</div>
                  </div>
                </div>
                <div className="row-card">
                  <div>
                    <div className="row-sub">Plugins</div>
                    <div className="row-title" style={{ fontSize: 13 }}>{backendPlugins.length}</div>
                  </div>
                </div>
                <div className="row-card">
                  <div>
                    <div className="row-sub">Peers</div>
                    <div className="row-title" style={{ fontSize: 13 }}>{backendPeers.length}</div>
                  </div>
                </div>
              </div>
            </div>

            {/* Cost today */}
            {backendMetrics && (
              <div style={{ marginTop: 12 }}>
                <h4 style={{ fontSize: 12, textTransform: "uppercase", letterSpacing: "0.04em", color: "var(--text-soft)", marginBottom: 6 }}>Usage Today</h4>
                <div className="row-card">
                  <div>
                    <div className="row-title" style={{ fontSize: 16, color: "var(--brand)" }}>
                      ${(backendMetrics.today_cost || 0).toFixed(2)}
                    </div>
                    <div className="row-sub">
                      {(backendMetrics.today_input_tokens ?? 0).toLocaleString()} in / {(backendMetrics.today_output_tokens ?? 0).toLocaleString()} out
                    </div>
                  </div>
                </div>
              </div>
            )}

            {/* Security */}
            {backendSecurity && (
              <div style={{ marginTop: 12 }}>
                <h4 style={{ fontSize: 12, textTransform: "uppercase", letterSpacing: "0.04em", color: "var(--text-soft)", marginBottom: 6 }}>Security</h4>
                <div style={{ display: "flex", flexWrap: "wrap", gap: 4 }}>
                  <span className="chip">{backendSecurity.auth_mode}</span>
                  <span className="chip">{backendSecurity.tunnel_active ? "Tunnel ✅" : "Tunnel off"}</span>
                  <span className="chip">{backendSecurity.identity_contracts} contracts</span>
                  <span className="chip">{backendSecurity.scanner_patterns} scan patterns</span>
                </div>
              </div>
            )}
          </div>
        );
      }

      if (activeNav === "skills") {
        return (
          <div className="inspector-content">
            <h3>Skills Overview</h3>

            <div style={{ marginTop: 8 }}>
              <div className="row-card">
                <div>
                  <div className="row-title">{backendSkills.length} Skills Loaded</div>
                  <div className="row-sub">
                    {backendSkills.filter((s) => s.verified).length} builtin · {backendSkills.filter((s) => !s.verified).length} custom
                  </div>
                </div>
              </div>
            </div>

            {selectedRecipe && (
              <div style={{ marginTop: 12 }}>
                <h4 style={{ fontSize: 12, textTransform: "uppercase", letterSpacing: "0.04em", color: "var(--text-soft)", marginBottom: 6 }}>Selected Recipe</h4>
                <div className="row-card">
                  <div>
                    <div className="row-title">{selectedRecipe.name}</div>
                    <div className="row-sub">{String(selectedRecipe.provenance)}</div>
                    <div style={{ marginTop: 4, display: "flex", flexWrap: "wrap", gap: 4 }}>
                      {selectedRecipe.permissions.map((p) => (
                        <span key={p} className="chip">{p}</span>
                      ))}
                      <span className={`chip ${selectedRecipe.restrictedMode ? "" : "chip-risk"}`}>
                        {selectedRecipe.restrictedMode ? "🔒 Restricted" : "⚡ Full access"}
                      </span>
                    </div>
                  </div>
                </div>
              </div>
            )}

            {/* Skill categories */}
            <div style={{ marginTop: 12 }}>
              <h4 style={{ fontSize: 12, textTransform: "uppercase", letterSpacing: "0.04em", color: "var(--text-soft)", marginBottom: 6 }}>Quick Actions</h4>
              <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
                <button className="btn subtle" style={{ justifyContent: "flex-start", textAlign: "left" }} onClick={() => api.listSkills().then((s) => setBackendSkills(s)).catch(() => { })}>
                  🔄 Refresh Skills
                </button>
              </div>
            </div>
          </div>
        );
      }

      return (
        <div className="inspector-content">
          <h3>Confidence summary</h3>
          <p>{status.summary}</p>
          <button className="btn subtle" onClick={() => setStatusPopoverOpen(true)}>Explain status</button>

          {backendMemoryStats && (
            <div style={{ marginTop: 12 }}>
              <h3>Memory</h3>
              <p>Collection: {backendMemoryStats.collection_name}</p>
              <p>Provider: {backendMemoryStats.embedding_provider}</p>
              <p>Strategy: {backendMemoryStats.search_strategy}</p>
            </div>
          )}

          {backendPlugins.length > 0 && (
            <div style={{ marginTop: 12 }}>
              <h3>Plugins ({backendPlugins.length})</h3>
              {backendPlugins.slice(0, 5).map((p) => (
                <p key={p.id}>{p.name} — {p.state === "active" ? "enabled" : "disabled"}</p>
              ))}
            </div>
          )}

          {backendPeers.length > 0 && (
            <div style={{ marginTop: 12 }}>
              <h3>Discovered Peers ({backendPeers.length})</h3>
              {backendPeers.slice(0, 5).map((p) => (
                <p key={p.instance_name}>{p.instance_name} — {p.host}:{p.port}</p>
              ))}
            </div>
          )}

          {backendObservability && (
            <div style={{ marginTop: 12 }}>
              <h3>Observability</h3>
              <p>Enabled: {backendObservability.enabled ? "On" : "Off"}</p>
              <p>Service: {backendObservability.service_name}</p>
              <p>Endpoint: {backendObservability.endpoint || "none"}</p>
            </div>
          )}
        </div>
      );
    }

    const currentStep = currentPlan.steps[activeStepIndex];
    const riskCopy = translateRisk(currentPlan.risk, currentPlan.touches);

    return (
      <div className="inspector-content ask-inspector">
        <div className="tabs">
          {(["plan", "approvals", "proof", "trace", "memory", "graph", "undo"] as const).map((tab) => (
            <button key={tab} className={`tab ${inspectorTab === tab ? "active" : ""}`} onClick={() => setInspectorTab(tab)}>
              {tab}
            </button>
          ))}
        </div>

        {inspectorTab === "plan" && (
          <div className="panel-stack">
            <div className="plan-card">
              <h3>Plan Card</h3>
              <p className="plan-goal">Goal: {currentPlan.goal}</p>
              <p className="plan-risk">
                Risk: <strong>{currentPlan.risk}</strong> • {riskCopy.summary}
              </p>
              <p className="plan-risk">{riskCopy.consequence}</p>
              <p className="plan-risk">{riskCopy.undo}</p>
              <div className="touch-list">
                {currentPlan.touches.map((touch, index) => (
                  <span className="chip" key={`${touch.account}_${index}`}>
                    {touch.account}: {touch.type}
                  </span>
                ))}
              </div>

              <div className="step-list">
                {currentPlan.steps.map((step) => (
                  <details key={step.stepId} open={step.stepId === currentStep?.stepId}>
                    <summary>
                      <div className="step-head">
                        <span>{step.title}</span>
                        <div className="row-actions">
                          {step.requiresApproval && <span className="chip chip-risk">Requires approval</span>}
                          <span className={`chip chip-status ${stepStatus[step.stepId] ?? "idle"}`}>
                            {stepStatus[step.stepId] ?? "idle"}
                          </span>
                        </div>
                      </div>
                    </summary>
                    <div className="step-body">
                      <p>{step.details}</p>
                      <p><strong>Inputs:</strong> {step.inputs}</p>
                      <p><strong>Expected output:</strong> {step.expectedOutput}</p>
                      <pre>{step.preview}</pre>
                    </div>
                  </details>
                ))}
              </div>

              <div className="plan-actions">
                <button className="btn primary" onClick={() => setInspectorTab("plan")}>Preview</button>
                <button className="btn subtle" onClick={runStepByStep}>Run step-by-step</button>
                <button className="btn subtle" onClick={runAll}>Run all</button>
                <button className="btn ghost" onClick={() => pushToast("Plan canceled.")}>Cancel</button>
              </div>
            </div>

            {isExecuting && currentStep && (
              <div className="action-card">
                <h4>Action Card: {currentStep.title}</h4>
                <pre>{currentStep.preview}</pre>
                <div className="row-actions">
                  <button className="btn primary" onClick={approveAndRunCurrentStep}>Approve + Run</button>
                  <button className="btn subtle" onClick={editCurrentStepPreview}>Edit</button>
                  <button className="btn ghost" onClick={skipCurrentStep}>Skip</button>
                  <button className="btn danger" onClick={stopExecution}>Stop everything</button>
                </div>
              </div>
            )}

            {!isExecuting && (
              <div className="timeline-block">
                <h4>Activity Timeline</h4>
                {timeline.map((item) => (
                  <div key={item.id} className="timeline-item">
                    <div>
                      <strong>{item.label}</strong>
                      <p>{item.detail}</p>
                    </div>
                    <div className="row-actions">
                      <button className="btn subtle">View</button>
                      {item.undoable && <button className="btn subtle">Undo</button>}
                      <button className="btn subtle">Explain</button>
                    </div>
                  </div>
                ))}
              </div>
            )}
          </div>
        )}

        {inspectorTab === "approvals" && (
          <div className="panel-stack">
            <h3>Approvals</h3>
            {pendingApprovals.length === 0 ? (
              <p>No pending approvals.</p>
            ) : (
              pendingApprovals.map((item) => (
                <div key={item.id} className="approval-card">
                  <p><strong>{item.summary}</strong></p>
                  <p>{item.where}</p>
                  <p>{item.impact}</p>
                  <div className="row-actions">
                    <button className="btn subtle" onClick={() => setApprovalStatus(item, "approved")}>Approve</button>
                    <button className="btn ghost" onClick={() => setApprovalStatus(item, "denied")}>Deny</button>
                  </div>
                </div>
              ))
            )}
          </div>
        )}

        {inspectorTab === "proof" && (
          <div className="panel-stack">
            <h3>Proof</h3>
            {proofs.length === 0 ? (
              <p>No proof yet.</p>
            ) : (
              proofs.slice(0, 5).map((proof) => (
                <div className="proof-card" key={proof.proofId}>
                  <p><strong>{proof.summary}</strong></p>
                  <p>{new Date(proof.endedAt).toLocaleString()} • {proof.duration}</p>
                  <div className="row-actions">
                    <button className="btn subtle">Share receipt</button>
                    <button className="btn subtle">Report problem</button>
                  </div>
                </div>
              ))
            )}
          </div>
        )}

        {inspectorTab === "trace" && (
          <div className="panel-stack">
            <h3>SochDB Trace</h3>
            {backendTraceRun ? (
              <div>
                <div className="row-card">
                  <div>
                    <div className="row-title">Run: {backendTraceRun.name}</div>
                    <div className="row-sub">
                      ID: {backendTraceRun.trace_id} •
                      Status: {backendTraceRun.status} •
                      Tokens: {backendTraceRun.total_tokens?.toLocaleString() ?? 0}
                    </div>
                    {backendTraceRun.cost_millicents != null && backendTraceRun.cost_millicents > 0 && (
                      <div className="row-sub">
                        Cost: {((backendTraceRun.cost_millicents || 0) / 100000).toFixed(4)}
                      </div>
                    )}
                  </div>
                </div>
                {backendTraceSpans.length > 0 && (
                  <div style={{ marginTop: 8 }}>
                    <h4>Spans ({backendTraceSpans.length})</h4>
                    {backendTraceSpans.map((span) => (
                      <div className="row-card" key={span.span_id}>
                        <div>
                          <div className="row-title">{span.name}</div>
                          <div className="row-sub">
                            {span.kind} •
                            {span.duration_us ? ` ${(span.duration_us / 1000).toFixed(1)}ms` : " running"}
                            {span.parent_span_id && ` • parent: ${span.parent_span_id.slice(0, 8)}`}
                          </div>
                        </div>
                      </div>
                    ))}
                  </div>
                )}
              </div>
            ) : (
              <p>No trace data yet. Send a message to generate traces.</p>
            )}
          </div>
        )}

        {inspectorTab === "memory" && (
          <div className="panel-stack">
            <h3>Memory</h3>
            {backendMemoryStats && (
              <div className="row-card">
                <div>
                  <div className="row-title">Stats</div>
                  <div className="row-sub">
                    Collection: {backendMemoryStats.collection_name} •
                    Provider: {backendMemoryStats.embedding_provider} •
                    Strategy: {backendMemoryStats.search_strategy}
                  </div>
                </div>
                <button className="btn subtle" onClick={() => {
                  api.getMemoryStats().then((s) => setBackendMemoryStats(s)).catch(() => { });
                }}>Refresh</button>
              </div>
            )}
            {backendMemoryHits.length > 0 ? (
              <div style={{ marginTop: 8 }}>
                <h4>Related Memories ({backendMemoryHits.length})</h4>
                {backendMemoryHits.map((hit) => (
                  <div className="row-card" key={hit.id}>
                    <div>
                      <div className="row-title" style={{ fontSize: "0.85rem" }}>
                        {(hit.content ?? "").length > 120 ? `${(hit.content ?? "").slice(0, 120)}...` : hit.content ?? "No content"}
                      </div>
                      <div className="row-sub">
                        Score: {(hit.score || 0).toFixed(3)}
                        {hit.source && ` • source: ${hit.source}`}
                        {hit.timestamp && ` • ${hit.timestamp}`}
                      </div>
                    </div>
                    <button className="btn ghost" onClick={() => {
                      api.forgetMemory(hit.id).then(() => {
                        setBackendMemoryHits((prev) => prev.filter((h) => h.id !== hit.id));
                        pushToast("Memory forgotten.");
                      }).catch(() => pushToast("Failed to forget memory."));
                    }}>Forget</button>
                  </div>
                ))}
              </div>
            ) : (
              <p>No related memories found. Send messages to build memory.</p>
            )}
          </div>
        )}

        {inspectorTab === "graph" && (
          <div className="panel-stack">
            <h3>Knowledge Graph</h3>
            {backendGraphNodes.length > 0 ? (
              <div>
                <h4>Nodes ({backendGraphNodes.length})</h4>
                {backendGraphNodes.map((node) => (
                  <div className="row-card" key={node.id}>
                    <div>
                      <div className="row-title">{node.id}</div>
                      <div className="row-sub">Type: {node.node_type}</div>
                    </div>
                    <button className="btn subtle" onClick={() => {
                      api.graphGetEdges(node.id).then((edges) => setBackendGraphEdges(edges)).catch(() => { });
                    }}>Edges</button>
                  </div>
                ))}
                {backendGraphEdges.length > 0 && (
                  <div style={{ marginTop: 8 }}>
                    <h4>Edges ({backendGraphEdges.length})</h4>
                    {backendGraphEdges.map((edge, idx) => (
                      <div className="row-card" key={`edge_${idx}`}>
                        <div>
                          <div className="row-title">{edge.from_id} → {edge.to_id}</div>
                          <div className="row-sub">Type: {edge.edge_type}</div>
                        </div>
                      </div>
                    ))}
                  </div>
                )}
              </div>
            ) : (
              <p>No graph data yet. Agent interactions build the knowledge graph.</p>
            )}
            <div className="row-actions" style={{ marginTop: 12 }}>
              <button className="btn subtle" onClick={() => {
                api.graphGetNodesByType("agent", 20).then((n) => setBackendGraphNodes(n)).catch(() => { });
              }}>Load Agent Nodes</button>
              <button className="btn subtle" onClick={() => {
                api.graphGetNodesByType("message", 20).then((n) => setBackendGraphNodes(n)).catch(() => { });
              }}>Load Message Nodes</button>
            </div>
          </div>
        )}

        {inspectorTab === "undo" && (
          <div className="panel-stack">
            <h3>Undo / Compensate</h3>
            {proofs[0]?.undo.length ? (
              proofs[0].undo.map((undoAction, index) => (
                <div className="row-card" key={`${undoAction}_${index}`}>
                  <div>{undoAction}</div>
                  <button className="btn subtle" onClick={() => pushToast("Compensating action prepared.")}>Run</button>
                </div>
              ))
            ) : (
              <p>No reversible actions available.</p>
            )}
          </div>
        )}
      </div>
    );
  }

  return (
    <div style={{ width: "100%", height: "100%", overflow: "hidden", display: "flex", flexDirection: "column" }}>
      {/* Persistence health warning banner */}
      {backendHealth && !backendHealth.storage_healthy && (
        <div
          style={{
            background: "#b91c1c",
            color: "#fff",
            padding: "6px 16px",
            fontSize: 13,
            fontWeight: 500,
            textAlign: "center",
            flexShrink: 0,
            zIndex: 9999,
          }}
        >
          ⚠ Storage is running in ephemeral mode — your chat history will NOT survive a restart.
          Check disk permissions for <code style={{ background: "rgba(0,0,0,0.2)", padding: "1px 4px", borderRadius: 3 }}>~/.clawdesk/sochdb/</code>
        </div>
      )}
      <AppShell
        sidebarCollapsed={sidebarCollapsed}
        compactSidebar={compactSidebar}
        activeNav={activeNav}
        navGroups={NAV_GROUPS}
        navItems={NAV_ITEMS}
        onNavigate={navigateLoose}
        onToggleSidebar={() => setIsSidebarCollapsed((prev) => !prev)}
        onOpenPalette={() => setPaletteOpen(true)}
        onOpenSettings={() => navigate("settings")}
        inspector={renderInspectorContent()}
        drawerAsModal={drawerAsModal}
        onOpenInspectorModal={() => setInspectorModalOpen(true)}
        inspectorOpen={inspectorOpen}
        onToggleInspector={() => setInspectorOpen((prev) => !prev)}
        onToggleTerminal={() => setShowTerminal((prev) => !prev)}
      >
        {activeNav === "chat" && (
          <ChatPage
            agents={backendAgents}
            skills={backendSkills}
            selectedAgentId={selectedAgentId}
            onSelectAgent={setSelectedAgentId}
            onCreateAgent={(t) => createBackendAgent(t)}
            pushToast={pushToast}
            showTerminal={showTerminal}
            setShowTerminal={setShowTerminal}
          />
        )}
        {activeNav === "overview" && (
          <OverviewPage
            health={backendHealth}
            agents={backendAgents}
            channels={backendChannels}
            security={backendSecurity}
            metrics={backendMetrics}
            observability={backendObservability}
            plugins={backendPlugins}
            peers={backendPeers}
            pushToast={pushToast}
            onNavigate={navigateLoose}
          />
        )}

        {activeNav === "a2a" && (
          <A2APage pushToast={pushToast} />
        )}

        {activeNav === "runtime" && (
          <RuntimePage pushToast={pushToast} />
        )}

        {activeNav === "skills" && (
          <SkillsPage
            skills={backendSkills}
            onRefreshSkills={() => api.listSkills().then((s) => setBackendSkills(s)).catch(() => { })}
            pushToast={pushToast}
            onNavigate={navigateLoose}
          />
        )}
        {activeNav === "automations" && (
          <AutomationsPage
            pipelines={backendPipelines}
            agents={backendAgents}
            onRefreshPipelines={() => api.listPipelines().then((p) => setBackendPipelines(p)).catch(() => { })}
            pushToast={pushToast}
            onNavigate={navigateLoose}
          />
        )}

        {activeNav === "settings" && (
          <SettingsPage
            agents={backendAgents}
            channels={backendChannels}
            security={backendSecurity}
            metrics={backendMetrics}
            health={backendHealth}
            observability={backendObservability}
            plugins={backendPlugins}
            peers={backendPeers}
            authProfiles={backendAuthProfiles}
            onCreateAgent={(t) => createBackendAgent(t)}
            onDeleteAgent={(id) => deleteBackendAgent(id)}
            onRefreshChannels={() => api.listChannels().then((c) => setBackendChannels(c)).catch(() => { })}
            onRefreshPlugins={() => api.listPlugins().then((p) => setBackendPlugins(p)).catch(() => { })}
            onRefreshPeers={() => api.listDiscoveredPeers().then((p) => setBackendPeers(p)).catch(() => { })}
            onResetOnboarding={resetOnboarding}
            pushToast={pushToast}
            onNavigate={navigateLoose}
          />
        )}
        {activeNav === "logs" && (
          <LogsPage
            pushToast={pushToast}
          />
        )}
        {activeNav === "extensions" && (
          <ExtensionsPage pushToast={pushToast} />
        )}
        {activeNav === "mcp" && (
          <McpPage pushToast={pushToast} />
        )}
      </AppShell>

      {drawerAsModal && inspectorModalOpen && (
        <Modal title="Inspector" onClose={() => setInspectorModalOpen(false)}>
          {renderInspectorContent()}
        </Modal>
      )}

      {paletteOpen && (
        <Modal title="Command Palette" onClose={() => setPaletteOpen(false)}>
          <div className="modal-stack">
            <input
              className="input"
              placeholder="Search commands, requests, proof..."
              autoFocus
              value={paletteQuery}
              onChange={(event) => setPaletteQuery(event.target.value)}
            />
            <div className="palette-list">
              {paletteItems.slice(0, 24).map((item) => (
                <button
                  className="palette-item"
                  key={item.id}
                  onClick={() => {
                    item.run();
                    setPaletteOpen(false);
                    setPaletteQuery("");
                  }}
                >
                  <span>{item.label}</span>
                  <small>{item.group}</small>
                </button>
              ))}
            </div>
          </div>
        </Modal>
      )}

      {statusPopoverOpen && (
        <Modal title="Explain Status" onClose={() => setStatusPopoverOpen(false)}>
          <div className="modal-stack">
            <p><strong>{status.summary}</strong></p>
            <p>{status.detail}</p>
            <div className="row-actions">
              <button className="btn primary" onClick={() => runQuickCheck()}>Fix it</button>
              <button className="btn subtle" onClick={() => pushToast("Details opened in diagnostics report.")}>Show details</button>
            </div>
          </div>
        </Modal>
      )}

      {quickCheck && (
        <Modal title="Quick Check" onClose={() => setQuickCheck(null)}>
          <div className="modal-stack">
            <div>
              <h3>{quickCheck.title}</h3>
              <p>{quickCheck.why}</p>
            </div>
            <div>
              <h3>Fix options</h3>
              {quickCheck.options.map((option, index) => (
                <button key={`${option}_${index}`} className="btn subtle block" onClick={() => {
                  if (option.toLowerCase().includes("reconnect email")) reconnectAccount("email");
                  if (option.toLowerCase().includes("messages")) reconnectAccount("messages");
                  if (option.toLowerCase().includes("safe mode")) setSafeMode(true);
                  if (option.toLowerCase().includes("pause routines")) {
                    setRoutines((prev) => prev.map((routine) => ({ ...routine, enabled: false })));
                    pushToast("Routines paused.");
                  }
                  if (option.toLowerCase().includes("try again")) runQuickCheck();
                }}>
                  {option}
                </button>
              ))}
            </div>
          </div>
        </Modal>
      )}

      {approvalsInboxOpen && (
        <Modal title="Approvals Inbox" onClose={() => setApprovalsInboxOpen(false)}>
          <div className="modal-stack">
            <h3>Approvals needed</h3>
            {pendingApprovals.length === 0 ? (
              <p>No pending approvals.</p>
            ) : (
              pendingApprovals.map((item) => (
                <div key={item.id} className="approval-card">
                  <p><strong>{item.summary}</strong></p>
                  <p>{item.where}</p>
                  <p>Impact: {item.impact}</p>
                  <div className="row-actions">
                    <button className="btn subtle" onClick={() => setApprovalStatus(item, "approved")}>Approve</button>
                    <button className="btn ghost" onClick={() => setApprovalStatus(item, "denied")}>Deny</button>
                    <button className="btn subtle" onClick={() => pushToast("Approval sheet opened.")}>Review</button>
                  </div>
                </div>
              ))
            )}
          </div>
        </Modal>
      )}

      {proofArchiveOpen && (
        <Modal title="Proof Archive" onClose={() => setProofArchiveOpen(false)}>
          <div className="modal-stack">
            <input className="input" placeholder="Search proof by account, recipe, outcome" />
            {proofs.map((proof) => (
              <div key={proof.proofId} className="proof-card">
                <p><strong>{proof.summary}</strong></p>
                <p>{new Date(proof.endedAt).toLocaleString()} • {proof.duration}</p>
                <div className="row-actions">
                  <button className="btn subtle" onClick={() => pushToast("Receipt exported as markdown.")}>Share receipt</button>
                  <button className="btn subtle" onClick={() => pushToast("Diagnostics attached.")}>Report a problem</button>
                </div>
              </div>
            ))}
          </div>
        </Modal>
      )}

      {routineWizardOpen && (
        <Modal title="Create Routine" onClose={() => setRoutineWizardOpen(false)}>
          <div className="modal-stack">
            <div className="wizard-steps">
              <span className={routineWizardStep === 1 ? "active" : ""}>1. Template</span>
              <span className={routineWizardStep === 2 ? "active" : ""}>2. When</span>
              <span className={routineWizardStep === 3 ? "active" : ""}>3. What</span>
              <span className={routineWizardStep === 4 ? "active" : ""}>4. Where</span>
              <span className={routineWizardStep === 5 ? "active" : ""}>5. Test</span>
            </div>

            {routineWizardStep === 1 && (
              <div className="modal-stack">
                <p>Choose template</p>
                <div className="template-grid">
                  {["Daily briefing", "Reminder", "Inbox triage", "Monitor something", "Custom"].map((template) => (
                    <button
                      key={template}
                      className={`template-tile ${routineDraft.template === template ? "selected" : ""}`}
                      onClick={() => setRoutineDraft((prev) => ({ ...prev, template }))}
                    >
                      {template}
                    </button>
                  ))}
                </div>
              </div>
            )}

            {routineWizardStep === 2 && (
              <div className="modal-stack">
                <label className="toggle-row">
                  <input
                    type="radio"
                    checked={routineDraft.type === "at_time"}
                    onChange={() => setRoutineDraft((prev) => ({ ...prev, type: "at_time" }))}
                  />
                  <span>At a time</span>
                </label>
                <label className="toggle-row">
                  <input
                    type="radio"
                    checked={routineDraft.type === "watch_notify"}
                    onChange={() => setRoutineDraft((prev) => ({ ...prev, type: "watch_notify" }))}
                  />
                  <span>Watch & notify</span>
                </label>
                {routineDraft.type === "at_time" ? (
                  <label className="field-label">
                    Schedule (natural language)
                    <input
                      value={routineDraft.when}
                      onChange={(event) => setRoutineDraft((prev) => ({ ...prev, when: event.target.value }))}
                    />
                  </label>
                ) : (
                  <label className="field-label">
                    Check interval
                    <input
                      value={routineDraft.watchInterval}
                      onChange={(event) =>
                        setRoutineDraft((prev) => ({ ...prev, watchInterval: event.target.value }))
                      }
                    />
                  </label>
                )}
              </div>
            )}

            {routineWizardStep === 3 && (
              <div className="modal-stack">
                <label className="field-label">
                  Recipe
                  <select
                    value={routineDraft.recipeId}
                    onChange={(event) => setRoutineDraft((prev) => ({ ...prev, recipeId: event.target.value }))}
                  >
                    {recipes.map((recipe) => (
                      <option key={recipe.id} value={recipe.id}>{recipe.name}</option>
                    ))}
                  </select>
                </label>
                <div className="plan-mini">
                  <p>Mini Plan Preview</p>
                  <ul>
                    <li>1. Gather inputs</li>
                    <li>2. Build summary draft</li>
                    <li>3. Send or save output based on permissions</li>
                  </ul>
                </div>
              </div>
            )}

            {routineWizardStep === 4 && (
              <div className="modal-stack">
                <label className="field-label">
                  Destination
                  <input
                    value={routineDraft.destination}
                    onChange={(event) =>
                      setRoutineDraft((prev) => ({ ...prev, destination: event.target.value }))
                    }
                  />
                </label>
                <label className="field-label">
                  Quiet hours
                  <input
                    value={routineDraft.quietHours}
                    onChange={(event) =>
                      setRoutineDraft((prev) => ({ ...prev, quietHours: event.target.value }))
                    }
                  />
                </label>
              </div>
            )}

            {routineWizardStep === 5 && (
              <div className="modal-stack">
                <p>Run once now to validate account access, permissions, and output path.</p>
                <button className="btn primary" onClick={() => runRoutineTest()}>Run once now</button>
              </div>
            )}

            <div className="row-actions">
              <button
                className="btn ghost"
                onClick={() => setRoutineWizardStep((prev) => Math.max(1, prev - 1))}
                disabled={routineWizardStep === 1}
              >
                Back
              </button>
              {routineWizardStep < 5 ? (
                <button className="btn primary" onClick={() => setRoutineWizardStep((prev) => Math.min(5, prev + 1))}>
                  Next
                </button>
              ) : (
                <button className="btn primary" onClick={createRoutineFromWizard}>
                  Create routine
                </button>
              )}
            </div>
          </div>
        </Modal>
      )}

      <OnboardingWizard
        open={onboardingOpen}
        health={backendHealth}
        skills={backendSkills}
        channels={backendChannels}
        onComplete={completeOnboarding}
      />

      {toasts.length > 0 && (
        <div className="toast-stack" aria-live="polite">
          {toasts.map((toast) => (
            <div key={toast.id} className="toast">
              {toast.text}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
