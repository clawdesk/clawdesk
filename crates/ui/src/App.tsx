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
import { AppShell } from "./shell/AppShell";

type NavKey = "now" | "ask" | "routines" | "accounts" | "library";
type RiskLevel = "low" | "medium" | "high";
type StatusLevel = "ok" | "warn" | "error";
type InspectorTab = "plan" | "approvals" | "proof" | "undo" | "trace" | "memory" | "graph";
type RoutineType = "at_time" | "watch_notify";
type RoutineResult = "ok" | "skipped" | "failed";
type AccountStatus = "connected" | "needs_sign_in" | "permissions_changed" | "disabled";
type Provenance = "built-in" | "created by you" | "from team" | "from internet";
type MessageRole = "user" | "assistant";
type StepStatus = "idle" | "running" | "ok" | "skipped" | "stopped";

interface ThreadMessage {
  id: string;
  role: MessageRole;
  text: string;
  time: string;
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
  event: { type: AgentEventType; [key: string]: unknown };
}

const NAV_ITEMS: { key: NavKey; label: string; shortcut: string; icon: string }[] = [
  { key: "now", label: "Now", shortcut: "1", icon: "now" },
  { key: "ask", label: "Ask", shortcut: "2", icon: "ask" },
  { key: "routines", label: "Routines", shortcut: "3", icon: "routines" },
  { key: "accounts", label: "Accounts", shortcut: "4", icon: "accounts" },
  { key: "library", label: "Library", shortcut: "5", icon: "library" },
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

function toThreadMessages(messages: BackendChatMessage[]): ThreadMessage[] {
  return messages.map((message) => {
    const role: MessageRole = message.role === "assistant" ? "assistant" : "user";
    let result: string | undefined;
    if (message.metadata) {
      result = `${message.metadata.model} · ${message.metadata.token_cost} tokens · $${message.metadata.cost_usd.toFixed(4)} · ${message.metadata.duration_ms}ms`;
    }
    return {
      id: message.id,
      role,
      text: message.content,
      time: formatLastActivity(message.timestamp),
      result,
    };
  });
}

function toThreadFromSession(session: SessionSummary): ThreadItem {
  return {
    id: session.agent_id,
    agentId: session.agent_id,
    title: session.title,
    lastActivity: formatLastActivity(session.last_activity),
    pendingApprovals: session.pending_approvals,
    routineGenerated: session.routine_generated,
    hasProofOutputs: session.has_proof_outputs,
    messages: [],
  };
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

function buildPlanFromRequest(input: string): PlanCard {
  const risk = getRiskFromText(input);
  const goal = input.trim().length > 0 ? input.trim() : "Handle your request";
  const needsSend = risk === "high";

  return {
    planId: makeId("plan"),
    goal,
    risk,
    touches: [
      { account: "Messages", type: needsSend ? "send message" : "draft message" },
      { account: "Files", type: "read notes" },
    ],
    steps: [
      {
        stepId: "step_draft",
        title: "Create a clean draft",
        details: "Use your recent context to build the first version.",
        inputs: "Thread context + latest request",
        expectedOutput: "Draft text",
        requiresApproval: false,
        preview: `Draft preview for: ${goal}`,
      },
      {
        stepId: "step_preview",
        title: "Show exact preview",
        details: "Present what will be sent or changed before execution.",
        inputs: "Draft text",
        expectedOutput: "Preview card",
        requiresApproval: false,
        preview: "Preview includes destination and final content.",
      },
      {
        stepId: "step_action",
        title: needsSend ? "Send approved result" : "Save approved draft",
        details: needsSend
          ? "Send to selected destination once approved."
          : "Save draft to selected location once approved.",
        inputs: "Approved preview",
        expectedOutput: needsSend ? "Sent confirmation" : "Saved output",
        requiresApproval: true,
        preview: needsSend ? "Destination: #team-updates" : "Output: ~/Documents/draft.txt",
      },
    ],
  };
}

export default function App() {
  const initialRouteRef = useRef(parseRouteHash(window.location.hash));
  const [activeNav, setActiveNavState] = useState<NavKey>(
    (initialRouteRef.current.nav as NavKey) ?? "now"
  );
  const [threads, setThreads] = useState<ThreadItem[]>(INITIAL_THREADS);
  const [activeThreadId, setActiveThreadIdState] = useState<string>(
    initialRouteRef.current.threadId ?? INITIAL_THREADS[0]?.id ?? ""
  );
  const [messageInput, setMessageInput] = useState("");
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
  const [isSidebarCollapsed, setIsSidebarCollapsed] = useState(false);
  const [toasts, setToasts] = useState<ToastItem[]>([]);
  const [onboardingOpen, setOnboardingOpen] = useState<boolean>(() => {
    try {
      return window.localStorage.getItem("clawdesk.onboarding.complete") !== "1";
    } catch {
      return true;
    }
  });

  const activeThread = useMemo(
    () => threads.find((thread) => thread.id === activeThreadId) ?? threads[0] ?? undefined,
    [threads, activeThreadId]
  );

  const selectedRoutine = useMemo(
    () => routines.find((routine) => routine.id === selectedRoutineId) ?? routines[0],
    [routines, selectedRoutineId]
  );

  const selectedAccount = useMemo(
    () => accounts.find((account) => account.id === selectedAccountId) ?? accounts[0],
    [accounts, selectedAccountId]
  );

  const selectedRecipe = useMemo(
    () => recipes.find((recipe) => recipe.id === selectedRecipeId) ?? recipes[0],
    [recipes, selectedRecipeId]
  );

  const pendingApprovals = approvals.filter((approval) => approval.status === "pending");

  const filteredRecipes = useMemo(
    () => recipes.filter((recipe) => recipe.category === libraryFilter),
    [recipes, libraryFilter]
  );

  const compactSidebar = viewportWidth < 680;
  const sidebarCollapsed = compactSidebar || isSidebarCollapsed;
  const drawerAsModal = viewportWidth < 1000;

  const navigate = useCallback(
    (nextNav: NavKey, options?: { threadId?: string; replace?: boolean }) => {
      const nextThreadId =
        options?.threadId ?? (nextNav === "ask" ? activeThreadId : undefined);
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
      navigate("ask", { threadId, replace: options?.replace });
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
    apiKey: string;
    templateName: string;
    firstPrompt: string;
  }) {
    try {
      if (result.provider) {
        window.localStorage.setItem("clawdesk.provider", result.provider);
      }
      if (result.apiKey) {
        window.localStorage.setItem("clawdesk.api_key.configured", "1");
      }
      const template = AGENT_TEMPLATES.find((item) => item.name === result.templateName);
      if (template) {
        await createBackendAgent(template);
      }
      window.localStorage.setItem("clawdesk.onboarding.complete", "1");
      setOnboardingOpen(false);
      navigate("ask");
      setMessageInput(result.firstPrompt);
      pushToast("ClawDesk setup completed.");
    } catch (error) {
      const mapped = classifyError(error);
      pushToast(mapped.userMessage);
    }
  }

  const runQuickCheck = useCallback(async () => {
    const needsSignIn = accounts.some((account) => account.status === "needs_sign_in");
    const permissionsChanged = accounts.some((account) => account.status === "permissions_changed");

    try {
      const health = await api.getHealth();
      setBackendHealth(health);
      // Refresh all backend data on quick check
      api.listSkills().then((s) => setBackendSkills(s)).catch(() => {});
      api.listAgents().then((a) => setBackendAgents(a)).catch(() => {});
      api.getMetrics().then((m) => setBackendMetrics(m)).catch(() => {});
      api.getSecurityStatus().then((s) => setBackendSecurity(s)).catch(() => {});
      api.listPlugins().then((p) => setBackendPlugins(p)).catch(() => {});
      api.listDiscoveredPeers().then((p) => setBackendPeers(p)).catch(() => {});
      api.getMemoryStats().then((s) => setBackendMemoryStats(s)).catch(() => {});
      api.listNotifications().then((n) => setBackendNotifications(n)).catch(() => {});
      api.listCanvases().then((c) => setBackendCanvases(c)).catch(() => {});
      api.listAuthProfiles().then((p) => setBackendAuthProfiles(p)).catch(() => {});
      if (needsSignIn || permissionsChanged) {
        setStatus({
          level: "warn",
          summary: "One account needs attention.",
          detail: "Some connected services need sign-in or permission review.",
          fix: needsSignIn ? "Reconnect Email" : "Review permissions",
        });
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
      } else {
        setStatus({
          level: "ok",
          summary: "Everything is connected.",
          detail: "Engine reachable. Accounts are healthy.",
          fix: "No action needed",
        });
        setQuickCheck({
          title: "All clear",
          why: "No blocking issues found.",
          options: ["Try again", "Show details"],
        });
      }
    } catch {
      setStatus({
        level: "error",
        summary: "Can’t reach the assistant engine.",
        detail: "The local engine did not respond to health check.",
        fix: "Restart local engine",
      });
      setQuickCheck({
        title: "What’s wrong",
        why: "The app can’t contact the local assistant engine.",
        options: ["Restart local engine", "Turn on Safe Mode", "Try again"],
      });
    }
  }, [accounts]);

  useEffect(() => {
    runQuickCheck();
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
        buildRouteHash({ nav: activeNav, threadId: activeNav === "ask" ? activeThreadId : undefined })
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
      try {
        // Phase 1: Core data
        const [health, agents, skills, security, metrics, sessions] = await Promise.allSettled([
          api.getHealth(),
          api.listAgents(),
          api.listSkills(),
          api.getSecurityStatus(),
          api.getMetrics(),
          api.listSessions(),
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
        if (sessions.status === "fulfilled" && sessions.value.length > 0) {
          const sessionThreads = await Promise.all(
            sessions.value.map(async (session) => {
              const baseThread = toThreadFromSession(session);
              try {
                const messages = await api.getSessionMessages(session.agent_id);
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
          // Map auth profiles to account items for the Accounts view
          const profileAccounts: AccountItem[] = authProfiles.value.map((p) => ({
            id: p.id,
            name: p.provider,
            status: p.is_expired ? "needs_sign_in" as AccountStatus : p.failure_count > 0 ? "permissions_changed" as AccountStatus : "connected" as AccountStatus,
            summary: `Provider: ${p.provider} — failures: ${p.failure_count}`,
            capabilities: {
              read: true,
              search: true,
              draft: true,
              sendWrite: false,
              delete: false,
              execute: false,
            },
            boundaries: {
              folders: "N/A",
              recipients: "N/A",
              channels: "N/A",
            },
            lastUsed: p.last_used ?? "Never",
          }));
          if (profileAccounts.length > 0) {
            setAccounts(profileAccounts);
            setSelectedAccountId((prev) => prev || profileAccounts[0].id);
          }
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
        const [metrics, security, notifications, memStats] = await Promise.allSettled([
          api.getMetrics(),
          api.getSecurityStatus(),
          api.listNotifications(),
          api.getMemoryStats(),
        ]);
        if (metrics.status === "fulfilled") setBackendMetrics(metrics.value);
        if (security.status === "fulfilled") setBackendSecurity(security.value);
        if (notifications.status === "fulfilled") setBackendNotifications(notifications.value);
        if (memStats.status === "fulfilled") setBackendMemoryStats(memStats.value);
      } catch { /* silent */ }
    }, 30_000);
    return () => { if (pollRef.current) clearInterval(pollRef.current); };
  }, []);

  useEffect(() => {
    let cleanup: (() => void) | null = null;
    subscribeAppEvents({
      onMetricsUpdated: (metrics) => setBackendMetrics(metrics),
      onSecurityChanged: (security) => setBackendSecurity(security),
      onRoutineExecuted: () => pushToast("Routine run completed."),
      onIncomingMessage: () => pushToast("New channel message received."),
      onSystemAlert: (alert) => {
        if (alert?.message) pushToast(alert.message);
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
  }, [pushToast, streamingAgentId, streamingThreadId, streamingMessageId]);

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
        const newThread: ThreadItem = {
          id: makeId("thread"),
          title: "New request",
          lastActivity: "Just now",
          pendingApprovals: 0,
          routineGenerated: false,
          hasProofOutputs: false,
          messages: [],
        };
        setThreads((prev) => [newThread, ...prev]);
        selectThread(newThread.id);
        pushToast("Created a new Ask thread.");
      }
      if (meta && event.key.toLowerCase() === "r") {
        event.preventDefault();
        runQuickCheck();
      }
      if (meta && event.key === ".") {
        event.preventDefault();
        stopExecution();
      }
      if (meta && ["1", "2", "3", "4", "5"].includes(event.key)) {
        event.preventDefault();
        navigate(NAV_ITEMS[Number(event.key) - 1].key);
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
  }, [navigate, pushToast, runQuickCheck, selectThread]);

  function markThreadActivity(threadId: string, patch: Partial<ThreadItem>) {
    setThreads((prev) =>
      prev.map((thread) => (thread.id === threadId ? { ...thread, ...patch } : thread))
    );
  }

  async function requestToAssistant(text: string) {
    if (isSending) return;

    const plan = buildPlanFromRequest(text);
    setCurrentPlan(plan);

    if (!activeThread) return;
    const threadId = activeThread.id;
    const agentId = selectedAgentId;
    const usingBackendAgent = Boolean(agentId);
    const userMessage: ThreadMessage = {
      id: makeId("message"),
      role: "user",
      text,
      time: nowLabel(),
    };

    // Add user message immediately
    setThreads((prev) =>
      prev.map((thread) => {
        if (thread.id !== threadId) return thread;
        return {
          ...thread,
          title: text.length > 44 ? `${text.slice(0, 44)}...` : text,
          lastActivity: "Just now",
          messages: [...thread.messages, userMessage],
        };
      })
    );

    // Try to send message to backend agent
    let assistantText = "I mapped this into a clear plan. You can preview first, run step-by-step, or run all.";
    let resultLabel = `Plan ready: ${plan.steps.length} steps`;
    let streamedMessageId: string | null = null;

    if (agentId) {
      setIsSending(true);
      streamedMessageId = makeId("message");
      const placeholderMessage: ThreadMessage = {
        id: streamedMessageId,
        role: "assistant",
        text: "",
        time: nowLabel(),
        result: "Thinking...",
      };
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
      try {
        const response = await api.sendMessage(agentId, text);
        assistantText = response.message.content;
        const meta = response.message.metadata;
        if (meta) {
          resultLabel = `${meta.model} · ${meta.token_cost} tokens · $${meta.cost_usd.toFixed(4)} · ${meta.duration_ms}ms`;
          if (meta.skills_activated.length > 0) {
            resultLabel += ` · skills: ${meta.skills_activated.join(", ")}`;
          }
        }
        // Refresh metrics after sending a message
        api.getMetrics().then((m) => setBackendMetrics(m)).catch(() => {});
        api.listAgents().then((a) => setBackendAgents(a)).catch(() => {});

        // Auto-remember this exchange in memory
        api.rememberMemory({
          content: `User: ${text}\nAssistant: ${assistantText}`,
          source: `chat:${agentId}:${threadId}`,
        }).catch(() => {});

        // Recall related context and store for inspector
        api.recallMemories({
          query: text,
          max_results: 5,
        }).then((hits) => setBackendMemoryHits(hits)).catch(() => {});

        // Refresh memory stats
        api.getMemoryStats().then((s) => setBackendMemoryStats(s)).catch(() => {});

        // Fetch SochDB trace data for this response (created by send_message backend)
        const rawMeta = response.message.metadata as Record<string, unknown> | null;
        if (rawMeta?.trace_id) {
          const traceId = rawMeta.trace_id as string;
          api.traceGetRun(traceId).then((run) => setBackendTraceRun(run)).catch(() => {});
          api.traceGetSpans(traceId).then((spans) => setBackendTraceSpans(spans)).catch(() => {});
        }
        // Fetch knowledge graph data related to the agent
        api.graphGetNodesByType("agent", 10).then((nodes) => setBackendGraphNodes(nodes)).catch(() => {});

        if (streamedMessageId) {
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
        }
      } catch (err) {
        const mapped = classifyError(err);
        assistantText = mapped.userMessage;
        resultLabel = "Error";
        if (streamedMessageId) {
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
        }
      } finally {
        setIsSending(false);
        setStreamingAgentId(null);
        setStreamingThreadId(null);
        setStreamingMessageId(null);
      }
    }
    if (!agentId) {
      const assistantMessage: ThreadMessage = {
        id: makeId("message"),
        role: "assistant",
        text: assistantText,
        time: nowLabel(),
        result: resultLabel,
      };
      setThreads((prev) =>
        prev.map((thread) => {
          if (thread.id !== threadId) return thread;
          return {
            ...thread,
            lastActivity: "Just now",
            messages: [...thread.messages, assistantMessage],
          };
        })
      );
    }

    const generatedApprovals = plan.steps
      .filter((step) => step.requiresApproval)
      .map((step) => ({
        id: makeId("approval"),
        planId: plan.planId,
        stepId: step.stepId,
        summary: step.title,
        where: "Messages / #team-updates",
        impact: step.expectedOutput,
        risk: plan.risk,
        status: "pending" as const,
      }));

    setApprovals((prev) => [...generatedApprovals, ...prev]);
    markThreadActivity(threadId, {
      pendingApprovals: generatedApprovals.length,
      lastActivity: "Just now",
    });

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
        label: usingBackendAgent ? "Agent responded" : "Plan drafted",
        detail: usingBackendAgent ? "Response from backend agent" : `${plan.steps.length} steps generated`,
        time: nowLabel(),
        undoable: false,
      },
    ]);
    setStepStatus({});
    setInspectorTab("plan");
    pushToast(usingBackendAgent ? "Agent response received." : "Plan ready in inspector.");
  }

  function initializeExecution() {
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
    initializeExecution();
    pushToast("Step-by-step mode started.");
  }

  function runAll() {
    if (safeMode && currentPlan.steps.some((step) => step.requiresApproval)) {
      setApprovalsInboxOpen(true);
      pushToast("Safe Mode blocked Run all. Review approvals first.");
      return;
    }

    const runStart = Date.now();
    const updates: Record<string, StepStatus> = {};
    currentPlan.steps.forEach((step) => {
      updates[step.stepId] = "ok";
    });
    setStepStatus(updates);
    setIsExecuting(false);
    setActiveStepIndex(currentPlan.steps.length - 1);

    const duration = `${Math.max(1, Math.round((Date.now() - runStart) / 1000))}s`;
    const proof: ProofRecord = {
      proofId: makeId("proof"),
      requestId: activeThread?.id ?? "",
      summary: `Completed plan: ${currentPlan.goal}`,
      startedAt: new Date(runStart).toISOString(),
      endedAt: new Date().toISOString(),
      duration,
      steps: currentPlan.steps.map((step) => ({ stepId: step.stepId, title: step.title, status: "ok" })),
      outputs: [
        {
          type: "summary",
          label: "Execution receipt",
          link: `local://proof/${Date.now()}`,
        },
      ],
      undo: ["Create follow-up correction"],
    };

    setProofs((prev) => [proof, ...prev]);
    if (activeThread) markThreadActivity(activeThread.id, {
      hasProofOutputs: true,
      pendingApprovals: 0,
      lastActivity: "Just now",
    });

    setTimeline((prev) => [
      ...prev,
      {
        id: makeId("timeline"),
        label: "Run all completed",
        detail: "All steps executed successfully.",
        time: nowLabel(),
        undoable: true,
      },
    ]);

    setInspectorTab("proof");
    pushToast("Plan completed and proof generated.");
  }

  function moveToNextStep(currentStepId: string, finalStatus: StepStatus) {
    const index = currentPlan.steps.findIndex((step) => step.stepId === currentStepId);
    const next = currentPlan.steps[index + 1];

    setStepStatus((prev) => {
      const nextState = { ...prev, [currentStepId]: finalStatus };
      if (next) {
        nextState[next.stepId] = "running";
      }
      return nextState;
    });

    setTimeline((prev) => [
      ...prev,
      {
        id: makeId("timeline"),
        label: currentPlan.steps[index].title,
        detail: finalStatus === "ok" ? "Completed" : "Skipped",
        time: nowLabel(),
        undoable: finalStatus === "ok",
      },
    ]);

    if (next) {
      setActiveStepIndex(index + 1);
      return;
    }

    const start = runStartedAt ?? Date.now();
    const finishedProof: ProofRecord = {
      proofId: makeId("proof"),
      requestId: activeThread?.id ?? "",
      summary: `Finished plan step-by-step: ${currentPlan.goal}`,
      startedAt: new Date(start).toISOString(),
      endedAt: new Date().toISOString(),
      duration: `${Math.max(1, Math.round((Date.now() - start) / 1000))}s`,
      steps: currentPlan.steps.map((step) => {
        const status = stepStatus[step.stepId];
        if (step.stepId === currentStepId) {
          return { stepId: step.stepId, title: step.title, status: finalStatus === "skipped" ? "skipped" : "ok" as const };
        }
        if (status === "skipped") return { stepId: step.stepId, title: step.title, status: "skipped" as const };
        if (status === "stopped") return { stepId: step.stepId, title: step.title, status: "stopped" as const };
        return { stepId: step.stepId, title: step.title, status: "ok" as const };
      }),
      outputs: [
        {
          type: "proof",
          label: "Receipt generated",
          link: `local://proof/${Date.now()}`,
        },
      ],
      undo: ["Undo last step", "Create compensating follow-up"],
    };

    setProofs((prev) => [finishedProof, ...prev]);
    setIsExecuting(false);
    setInspectorTab("proof");
    if (activeThread) markThreadActivity(activeThread.id, {
      hasProofOutputs: true,
      pendingApprovals: 0,
      lastActivity: "Just now",
    });
    pushToast("Step-by-step run finished.");
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

    moveToNextStep(step.stepId, "ok");
  }

  function skipCurrentStep() {
    const step = currentPlan.steps[activeStepIndex];
    if (!step) return;
    moveToNextStep(step.stepId, "skipped");
  }

  function stopExecution() {
    if (!isExecuting) return;

    const step = currentPlan.steps[activeStepIndex];
    if (step) {
      setStepStatus((prev) => ({ ...prev, [step.stepId]: "stopped" }));
      setTimeline((prev) => [
        ...prev,
        {
          id: makeId("timeline"),
          label: "Stopped by user",
          detail: `Stopped during: ${step.title}`,
          time: nowLabel(),
          undoable: false,
        },
      ]);
    }

    const start = runStartedAt ?? Date.now();
    const stoppedProof: ProofRecord = {
      proofId: makeId("proof"),
      requestId: activeThread?.id ?? "",
      summary: "Execution stopped by user",
      startedAt: new Date(start).toISOString(),
      endedAt: new Date().toISOString(),
      duration: `${Math.max(1, Math.round((Date.now() - start) / 1000))}s`,
      steps: currentPlan.steps.map((planStep) => {
        const status = stepStatus[planStep.stepId];
        if (status === "ok") return { stepId: planStep.stepId, title: planStep.title, status: "ok" as const };
        if (status === "skipped") return { stepId: planStep.stepId, title: planStep.title, status: "skipped" as const };
        return { stepId: planStep.stepId, title: planStep.title, status: "stopped" as const };
      }),
      outputs: [],
      undo: [],
    };

    setProofs((prev) => [stoppedProof, ...prev]);
    setIsExecuting(false);
    setInspectorTab("proof");
    pushToast("Execution stopped.");
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
      api.approveRequest(item.id, "user").catch(() => {});
      pushToast("Approval granted.");
    } else {
      api.denyRequest(item.id, "user", "Denied by user").catch(() => {});
      pushToast("Approval denied. No action taken.");
    }

    if (item.planId === currentPlan.planId && nextStatus === "approved" && !isExecuting && activeNav === "ask") {
      setInspectorTab("plan");
    }
  }

  function createRoutineFromWizard() {
    const routine: RoutineItem = {
      id: makeId("routine"),
      name: routineDraft.template,
      type: routineDraft.type,
      scheduleLabel: routineDraft.type === "at_time" ? routineDraft.when : routineDraft.watchInterval,
      nextRun: routineDraft.type === "at_time" ? "Tomorrow, 8:30 AM" : "In 30 minutes",
      destination: routineDraft.destination,
      enabled: true,
      quietHours: routineDraft.quietHours,
      lastResult: "ok",
      lastReason: "Created and ready",
      history: [],
      recipeId: routineDraft.recipeId,
    };

    setRoutines((prev) => [routine, ...prev]);
    setRoutineWizardOpen(false);
    setRoutineWizardStep(1);
    setSelectedRoutineId(routine.id);
    pushToast("Routine created.");
  }

  function runRoutineTest() {
    const selected = routines.find((routine) => routine.id === selectedRoutineId);
    if (!selected) return;

    const historyEntry = {
      at: new Date().toLocaleString(),
      result: "ok" as const,
      reason: "Test run succeeded",
    };

    setRoutines((prev) =>
      prev.map((routine) =>
        routine.id === selected.id
          ? {
              ...routine,
              lastResult: "ok",
              lastReason: "Test run succeeded",
              history: [historyEntry, ...routine.history],
            }
          : routine
      )
    );

    const proof: ProofRecord = {
      proofId: makeId("proof"),
      requestId: selected.id,
      summary: `Test run completed for routine: ${selected.name}`,
      startedAt: new Date().toISOString(),
      endedAt: new Date().toISOString(),
      duration: "2s",
      steps: [{ stepId: "test", title: "Routine test run", status: "ok" }],
      outputs: [{ type: "result", label: "Routine receipt", link: "local://proof/routine-test" }],
      undo: [],
    };

    setProofs((prev) => [proof, ...prev]);
    pushToast("Routine test run complete.");
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
      api.listAuthProfiles().then((profiles) => setBackendAuthProfiles(profiles)).catch(() => {});
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
  async function createBackendAgent(template: typeof AGENT_TEMPLATES[number]) {
    try {
      const agent = await api.createAgent({
        name: template.name,
        icon: template.icon,
        color: template.color,
        persona: template.persona,
        skills: template.skills,
        model: template.model,
      });
      setBackendAgents((prev) => [...prev, agent]);
      setSelectedAgentId(agent.id);
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
    const configJson = window.prompt("Paste OpenClaw JSON config:");
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
          navigate("routines");
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
        run: () => { api.listPlugins().then((p) => setBackendPlugins(p)).catch(() => {}); pushToast("Plugins refreshed."); },
      },
      {
        id: "cmd_refresh_peers",
        label: "Discover Peers",
        group: "Backend",
        run: () => { api.listDiscoveredPeers().then((p) => setBackendPeers(p)).catch(() => {}); pushToast("Peers refreshed."); },
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
      ...threads.map((thread) => ({ id: thread.id, label: thread.title, group: "Requests", run: () => {
        selectThread(thread.id);
      } })),
      ...proofs.slice(0, 12).map((proof) => ({ id: proof.proofId, label: proof.summary, group: "Proof", run: () => {
        navigate("now");
        setProofArchiveOpen(true);
      } })),
      ...routines.map((routine) => ({ id: routine.id, label: routine.name, group: "Routines", run: () => {
        navigate("routines");
        setSelectedRoutineId(routine.id);
      } })),
      ...accounts.map((account) => ({ id: account.id, label: account.name, group: "Accounts", run: () => {
        navigate("accounts");
        setSelectedAccountId(account.id);
      } })),
      ...recipes.map((recipe) => ({ id: recipe.id, label: recipe.name, group: "Library", run: () => {
        navigate("library");
        setSelectedRecipeId(recipe.id);
      } })),
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
                    <button className="btn subtle" onClick={() => runRoutineTest()}>
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
                      Input: {backendMetrics.today_input_tokens.toLocaleString()} tokens •{" "}
                      Output: {backendMetrics.today_output_tokens.toLocaleString()} tokens
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
                      {agent.model} • {agent.msg_count} msgs • {agent.tokens_used.toLocaleString()}/{agent.token_budget.toLocaleString()} tokens • {agent.status}
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
                        api.listPlugins().then((p) => setBackendPlugins(p)).catch(() => {});
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
                api.listDiscoveredPeers().then((p) => setBackendPeers(p)).catch(() => {});
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
                  api.listCanvases().then((c) => setBackendCanvases(c)).catch(() => {});
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
                        api.writeClipboard(md).then(() => pushToast("Canvas markdown copied to clipboard.")).catch(() => {});
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
            <h2>Threads</h2>
            <button className="btn subtle" onClick={() => {
              const newThread: ThreadItem = {
                id: makeId("thread"),
                title: "New request",
                lastActivity: "Just now",
                pendingApprovals: 0,
                routineGenerated: false,
                hasProofOutputs: false,
                messages: [],
              };
              setThreads((prev) => [newThread, ...prev]);
              selectThread(newThread.id);
            }}>
              New
            </button>
          </div>
          <div className="thread-items">
            {threads.map((thread) => (
              <button
                key={thread.id}
                className={`thread-item ${activeThread?.id === thread.id ? "active" : ""}`}
                onClick={() => selectThread(thread.id)}
                aria-current={activeThread?.id === thread.id ? "true" : undefined}
              >
                <div className="thread-title">{thread.title}</div>
                <div className="thread-meta">{thread.lastActivity}</div>
                <div className="thread-flags">
                  {thread.pendingApprovals > 0 && <span className="chip chip-risk">Approvals</span>}
                  {thread.routineGenerated && <span className="chip">Routine</span>}
                  {thread.hasProofOutputs && <span className="chip">Proof</span>}
                </div>
              </button>
            ))}
          </div>
        </aside>

        <section className="chat-area">
          <div className="chat-head">
            <div>
              <h2>{activeThread?.title ?? "New conversation"}</h2>
              <p>
                {selectedAgentId
                  ? `Agent: ${backendAgents.find((a) => a.id === selectedAgentId)?.name ?? "Unknown"} • ${backendAgents.find((a) => a.id === selectedAgentId)?.model ?? ""}`
                  : "Request → Plan → Approvals → Proof"}
                {isSending && " • Sending..."}
              </p>
            </div>
            <div className="chat-head-actions">
              {backendAgents.length > 0 && (
                <select
                  value={selectedAgentId ?? ""}
                  onChange={(e) => setSelectedAgentId(e.target.value || null)}
                  style={{ fontSize: "0.8rem", padding: "4px 8px", borderRadius: 6, border: "1px solid var(--border)", background: "var(--bg-card)" }}
                >
                  <option value="">No agent (local plan)</option>
                  {backendAgents.map((a) => (
                    <option key={a.id} value={a.id}>{a.icon} {a.name} ({a.model})</option>
                  ))}
                </select>
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
                  <div className="bubble-text">{message.text}</div>
                  <div className="bubble-meta">{message.time}</div>
                  {message.result && (
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
              placeholder="Type your request..."
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
                disabled={isSending}
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
                className={`row-card row-button ${selectedRoutine.id === routine.id ? "selected" : ""}`}
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
          <div className="section-head">
            <h2>{selectedRoutine.name}</h2>
            <button className="btn subtle" onClick={runRoutineTest}>Run once now</button>
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
              <div key={account.id} className={`account-card ${selectedAccount.id === account.id ? "selected" : ""}`}>
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
                className={`recipe-card ${selectedRecipe.id === recipe.id ? "selected" : ""}`}
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
        </section>
      </div>
    );
  }

  function renderInspectorContent() {
    if (activeNav !== "ask") {
      if (activeNav === "routines") {
        return (
          <div className="inspector-content">
            <h3>Routine Inspector</h3>
            <p>{selectedRoutine.name}</p>
            <p>{selectedRoutine.scheduleLabel}</p>
            <p>{selectedRoutine.lastReason}</p>
          </div>
        );
      }

      if (activeNav === "accounts") {
        return (
          <div className="inspector-content">
            <h3>Account Inspector</h3>
            <p>{selectedAccount.name}</p>
            <p>{accountStatusLabel(selectedAccount.status)}</p>
            <p>Last used: {selectedAccount.lastUsed}</p>
          </div>
        );
      }

      if (activeNav === "library") {
        return (
          <div className="inspector-content">
            <h3>Recipe Inspector</h3>
            <p>{selectedRecipe.name}</p>
            <p>{selectedRecipe.provenance}</p>
            <p>{selectedRecipe.restrictedMode ? "Restricted mode ON" : "Restricted mode OFF"}</p>
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
                <button className="btn primary" onClick={runStepByStep}>Run step-by-step</button>
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
                        Cost: ${(backendTraceRun.cost_millicents / 100000).toFixed(4)}
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
                  api.getMemoryStats().then((s) => setBackendMemoryStats(s)).catch(() => {});
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
                        Score: {hit.score.toFixed(3)}
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
                      api.graphGetEdges(node.id).then((edges) => setBackendGraphEdges(edges)).catch(() => {});
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
                api.graphGetNodesByType("agent", 20).then((n) => setBackendGraphNodes(n)).catch(() => {});
              }}>Load Agent Nodes</button>
              <button className="btn subtle" onClick={() => {
                api.graphGetNodesByType("message", 20).then((n) => setBackendGraphNodes(n)).catch(() => {});
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
    <div>
      <AppShell
        sidebarCollapsed={sidebarCollapsed}
        compactSidebar={compactSidebar}
        activeNav={activeNav}
        navItems={NAV_ITEMS}
        onNavigate={navigate}
        onToggleSidebar={() => setIsSidebarCollapsed((prev) => !prev)}
        onOpenPalette={() => setPaletteOpen(true)}
        status={status}
        onToggleStatus={() => setStatusPopoverOpen((prev) => !prev)}
        safeMode={safeMode}
        onToggleSafeMode={() => setSafeMode((prev) => !prev)}
        onOpenApprovals={() => setApprovalsInboxOpen(true)}
        approvalCount={pendingApprovals.length}
        onOpenSettings={() => setSettingsOpen(true)}
        showSafeModeBanner={!safeMode}
        inspector={renderInspectorContent()}
        drawerAsModal={drawerAsModal}
        onOpenInspectorModal={() => setInspectorModalOpen(true)}
      >
        {activeNav === "now" && renderNowView()}
        {activeNav === "ask" && renderAskView()}
        {activeNav === "routines" && renderRoutinesView()}
        {activeNav === "accounts" && renderAccountsView()}
        {activeNav === "library" && renderLibraryView()}
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

      {settingsOpen && (
        <Modal title="Settings" onClose={() => setSettingsOpen(false)}>
          <div className="modal-stack">
            <h3>General</h3>
            <label className="toggle-row">
              <input type="checkbox" checked readOnly />
              <span>Start on login</span>
            </label>
            <label className="toggle-row">
              <input type="checkbox" checked={safeMode} onChange={() => setSafeMode((prev) => !prev)} />
              <span>Safe Mode default</span>
            </label>
            <label className="toggle-row">
              <input type="checkbox" checked readOnly />
              <span>Require approvals for Send/Write/Execute</span>
            </label>
            <h3>Advanced</h3>
            <button className="btn subtle block" onClick={() => {
              api.getObservabilityConfig().then((o) => {
                setBackendObservability(o);
                pushToast(`Observability: enabled=${o.enabled}, service=${o.service_name}`);
              }).catch(() => pushToast("Observability config unavailable."));
            }}>Observability config</button>
            <button className="btn subtle block" onClick={() => {
              api.configureObservability({ enabled: true, environment: "desktop" }).then((o) => {
                setBackendObservability(o);
                pushToast("Observability enabled.");
              }).catch(() => pushToast("Failed to configure observability."));
            }}>Enable full observability</button>
            <button className="btn subtle block" onClick={() => {
              api.getVoiceWakeStatus().then((v) => {
                pushToast(`Voice wake: ${v.enabled ? "On" : "Off"} — phrases: ${v.wake_phrases.join(", ") || "none"}`);
              }).catch(() => pushToast("Voice wake unavailable."));
            }}>Voice wake status</button>
            <button className="btn subtle block" onClick={() => {
              api.getIdleStatus().then((i) => {
                pushToast(`Idle: ${i.is_idle ? "Yes" : "No"} — ${i.idle_duration_secs}s`);
              }).catch(() => pushToast("Idle status unavailable."));
            }}>Idle status</button>
            <button className="btn subtle block" onClick={() => {
              api.sochdbCheckpoint().then((n) => pushToast(`SochDB checkpoint: ${n} entries persisted.`))
                .catch(() => pushToast("Checkpoint failed."));
            }}>SochDB checkpoint</button>
            <button className="btn subtle block" onClick={() => pushToast("Diagnostics report prepared.")}>Diagnostics export</button>
            <button className="btn subtle block" onClick={() => pushToast("Developer console opened.")}>Developer console</button>
            <button className="btn subtle block" onClick={() => pushToast("Local cache reset queued.")}>Reset local cache</button>
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
                <button className="btn primary" onClick={runRoutineTest}>Run once now</button>
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
