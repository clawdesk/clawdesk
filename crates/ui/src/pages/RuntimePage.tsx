import { useState, useEffect, useCallback, useRef, useMemo } from "react";
import { listen } from "@tauri-apps/api/event";
import * as api from "../api";
import { Icon } from "../components/Icon";
import { getActiveProvider } from "../providerConfig";
import type {
  DesktopAgent,
  DurableRunInfo,
  RuntimeStatusInfo,
  DlqEntry,
  CheckpointEntry,
  AgentEventEnvelope,
  SessionSummary,
} from "../types";

// ── Local types ───────────────────────────────────────────────

type MainView = "home" | "chat" | "tasks";
type TaskState = "running" | "blocked" | "retrying" | "done" | "failed" | "pending";
type ExecTab = "context" | "tools" | "events" | "run";
type TaskFilter = "all" | TaskState;

interface AgentTask {
  id: string;
  title: string;
  state: TaskState;
  nextStep: string;
  owner: string;
  progress: number;
  runId: string;
  timeline: TimelineEntry[];
  blockReason?: string;
  checkpoints: CheckpointEntry[];
  durableStatus?: string;
}

interface TimelineEntry {
  time: string;
  text: string;
  type: "info" | "warn" | "error" | "ok";
}

interface ToolCallInfo {
  name: string;
  argsPreview?: string;
  status: "running" | "done" | "error";
  result?: string;
  durationMs?: number;
}

interface ThreadMessage {
  id: string;
  role: "user" | "assistant" | "system";
  text: string;
  thinkingText?: string;
  time: string;
  agent?: string;
  toolCalls?: ToolCallInfo[];
  isStreaming?: boolean;
  tokens?: number;
  cost?: number;
  duration?: number;
  round?: number;
}

interface RuntimeEvent {
  id: string;
  time: string;
  type: string;
  detail: string;
}

// ── Props ─────────────────────────────────────────────────────

export interface RuntimePageProps {
  pushToast: (msg: string) => void;
  agents?: DesktopAgent[];
}

// ── Helpers ───────────────────────────────────────────────────

function mapRunState(state: string): TaskState {
  switch (state) {
    case "running": return "running";
    case "suspended": return "blocked";
    case "failed": return "failed";
    case "completed": return "done";
    default: return "pending";
  }
}

function stateColor(state: TaskState | string): string {
  switch (state) {
    case "running": return "var(--ok)";
    case "blocked": case "retrying": return "var(--warn)";
    case "failed": return "var(--danger)";
    case "done": return "var(--ok)";
    default: return "var(--text-muted)";
  }
}

function stateIcon(state: TaskState | string): string {
  switch (state) {
    case "running": return "play";
    case "blocked": return "pause";
    case "retrying": return "refresh";
    case "failed": return "alert";
    case "done": return "check";
    default: return "clock";
  }
}

function fmtTime(): string {
  return new Date().toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
}

function shortId(id: string, max = 20): string {
  return id.length > max ? id.slice(0, max) + "\u2026" : id;
}

// ── Component ─────────────────────────────────────────────────

export function RuntimePage({ pushToast, agents: agentsProp }: RuntimePageProps) {
  const [view, setView] = useState<MainView>("home");

  // ── Runtime backend data (all real) ──
  const [runs, setRuns] = useState<DurableRunInfo[]>([]);
  const [dlq, setDlq] = useState<DlqEntry[]>([]);
  const [runtimeStatus, setRuntimeStatus] = useState<RuntimeStatusInfo | null>(null);
  const [loading, setLoading] = useState(true);
  const [agents, setAgents] = useState<DesktopAgent[]>(agentsProp ?? []);
  const [sessions, setSessions] = useState<SessionSummary[]>([]);

  // ── Task layer (derived from durable runs) ──
  const [tasks, setTasks] = useState<AgentTask[]>([]);
  const [selectedTaskId, setSelectedTaskId] = useState<string | null>(null);
  const [taskFilter, setTaskFilter] = useState<TaskFilter>("all");
  const [taskSearch, setTaskSearch] = useState("");

  // ── Chat: real agent-event streaming ──
  const [selectedAgentId, setSelectedAgentId] = useState<string | null>(null);
  const [activeChatId, setActiveChatId] = useState<string | null>(null);
  const activeChatIdRef = useRef<string | null>(null);
  const [messages, setMessages] = useState<ThreadMessage[]>([]);
  const [chatInput, setChatInput] = useState("");
  const [isSending, setIsSending] = useState(false);
  const sendingRef = useRef(false);
  const streamingMsgIdRef = useRef<string | null>(null);
  const sendGenRef = useRef(0);
  const activeSendAbortRef = useRef<AbortController | null>(null);
  const chatEndRef = useRef<HTMLDivElement>(null);

  // ── Execution Sidebar ──
  const [execTab, setExecTab] = useState<ExecTab>("context");
  const [runtimeEvents, setRuntimeEvents] = useState<RuntimeEvent[]>([]);
  const [currentRound, setCurrentRound] = useState(0);
  const [promptInfo, setPromptInfo] = useState<{ totalTokens: number; skills: string[]; memFragments: number; budget: number } | null>(null);

  // ── Auto-refresh ──
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);

  // Keep activeChatIdRef synced
  useEffect(() => { activeChatIdRef.current = activeChatId; }, [activeChatId]);

  // Scroll to bottom on new messages
  useEffect(() => { chatEndRef.current?.scrollIntoView({ behavior: "smooth" }); }, [messages]);

  // Sync agents prop
  useEffect(() => { if (agentsProp?.length) setAgents(agentsProp); }, [agentsProp]);

  // ── Core refresh: fetch all runtime + agent data ──
  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      const [r, d, s, a, sess] = await Promise.all([
        api.listDurableRuns().catch(() => [] as DurableRunInfo[]),
        api.getDlq().catch(() => [] as DlqEntry[]),
        api.getRuntimeStatus().catch(() => null),
        agentsProp?.length ? Promise.resolve(agentsProp) : api.listAgents().catch(() => [] as DesktopAgent[]),
        api.listSessions().catch(() => [] as SessionSummary[]),
      ]);
      setRuns(r);
      setDlq(d);
      setRuntimeStatus(s);
      if (!agentsProp?.length) setAgents(a);
      setSessions(sess);

      // Build tasks from durable runs with real checkpoint data
      const taskPromises = r.map(async (run, i) => {
        let checkpoints: CheckpointEntry[] = [];
        let durableStatus = "";
        try {
          checkpoints = await api.listCheckpoints(run.run_id);
        } catch { /* checkpoint store may not have data */ }
        try {
          durableStatus = await api.getDurableRunStatus(run.run_id);
        } catch { /* status may not be available */ }

        const tl: TimelineEntry[] = [
          { time: fmtTime(), text: `State: ${run.state}`, type: run.state === "failed" ? "error" : run.state === "suspended" ? "warn" : "info" },
        ];
        if (checkpoints.length > 0) {
          tl.push({ time: checkpoints[0].created_at, text: `Checkpoint at step ${checkpoints[0].step_index}`, type: "ok" });
        }

        const dlqMatch = d.filter(e => e.run_id === run.run_id);
        if (dlqMatch.length > 0) {
          tl.push({ time: dlqMatch[0].failed_at, text: `DLQ: ${dlqMatch[0].error}`, type: "error" });
        }

        return {
          id: `R-${100 + i}`,
          title: run.run_id.length > 30 ? run.run_id.substring(0, 30) + "\u2026" : run.run_id,
          state: mapRunState(run.state),
          nextStep: run.state === "running" ? "Executing\u2026"
            : run.state === "suspended" ? "Awaiting input"
            : run.state === "failed" ? "Needs retry"
            : run.state === "completed" ? "Done" : "\u2014",
          owner: run.worker_id || "unassigned",
          progress: run.state === "running" ? 50 : run.state === "completed" ? 100 : run.state === "failed" ? 0 : 25,
          runId: run.run_id,
          timeline: tl,
          blockReason: run.state === "suspended" ? "Awaiting human input" : undefined,
          checkpoints,
          durableStatus,
        } as AgentTask;
      });
      const resolvedTasks = await Promise.all(taskPromises);
      setTasks(resolvedTasks);

      // Auto-select first agent if none selected
      if (!selectedAgentId && a.length > 0) {
        setSelectedAgentId(a[0].id);
      }
    } finally {
      setLoading(false);
    }
  }, [agentsProp, selectedAgentId]);

  // Initial load + polling
  useEffect(() => {
    refresh();
    pollRef.current = setInterval(refresh, 8000);
    return () => { if (pollRef.current) clearInterval(pollRef.current); };
  }, [refresh]);

  // ── Agent-event streaming (real, same pattern as ChatPage) ──
  useEffect(() => {
    if (!selectedAgentId) return;
    let aborted = false;
    let unlisten: (() => void) | null = null;

    listen<AgentEventEnvelope>("agent-event", (ev) => {
      if (aborted) return;
      const data = ev.payload;
      if (!data || data.agent_id !== selectedAgentId) return;

      const msgId = streamingMsgIdRef.current;
      const event = data.event;

      // Record to runtime events log
      setRuntimeEvents(prev => [...prev.slice(-200), {
        id: `ev-${Date.now()}-${Math.random().toString(36).slice(2, 6)}`,
        time: fmtTime(),
        type: event.type,
        detail: event.type === "StreamChunk" ? `+${(event as any).text?.length ?? 0} chars`
          : event.type === "ToolStart" ? `${(event as any).name}`
          : event.type === "ToolEnd" ? `${(event as any).name} (${(event as any).success ? "ok" : "err"})`
          : event.type === "RoundStart" ? `Round ${(event as any).round}`
          : event.type === "Error" ? `${(event as any).error}`
          : event.type === "Done" ? `${(event as any).total_rounds} rounds`
          : event.type === "PromptAssembled" ? `${(event as any).total_tokens} tokens`
          : event.type,
      }]);

      if (!msgId) return;

      // StreamChunk
      if (event.type === "StreamChunk") {
        const text = typeof (event as any).text === "string" ? (event as any).text : "";
        if (text.length > 0) {
          setMessages(prev => prev.map(m => m.id === msgId ? { ...m, text: m.text + text } : m));
        }
        return;
      }

      // ThinkingChunk
      if (event.type === "ThinkingChunk") {
        const text = typeof (event as any).text === "string" ? (event as any).text : "";
        if (text.length > 0) {
          setMessages(prev => prev.map(m => m.id === msgId ? { ...m, thinkingText: (m.thinkingText || "") + text } : m));
        }
        return;
      }

      // ToolStart
      if (event.type === "ToolStart") {
        const name = typeof (event as any).name === "string" ? (event as any).name : "unknown";
        const args = typeof (event as any).args === "string" ? (event as any).args : undefined;
        setMessages(prev => prev.map(m => {
          if (m.id !== msgId) return m;
          return { ...m, toolCalls: [...(m.toolCalls ?? []), { name, argsPreview: args, status: "running" as const }] };
        }));
        return;
      }

      // ToolEnd
      if (event.type === "ToolEnd") {
        const name = typeof (event as any).name === "string" ? (event as any).name : "unknown";
        const success = Boolean((event as any).success);
        const dur = typeof (event as any).duration_ms === "number" ? (event as any).duration_ms : 0;
        setMessages(prev => prev.map(m => {
          if (m.id !== msgId) return m;
          const updated = (m.toolCalls ?? []).map(tc =>
            tc.name === name && tc.status === "running"
              ? { ...tc, status: (success ? "done" : "error") as "done" | "error", durationMs: dur, result: `${success ? "ok" : "failed"} ${dur}ms` }
              : tc
          );
          return { ...m, toolCalls: updated };
        }));
        return;
      }

      // RoundStart
      if (event.type === "RoundStart") {
        setCurrentRound((event as any).round ?? 0);
        return;
      }

      // PromptAssembled
      if (event.type === "PromptAssembled") {
        const p = event as any;
        setPromptInfo({
          totalTokens: p.total_tokens ?? 0,
          skills: p.skills_included ?? [],
          memFragments: p.memory_fragments ?? 0,
          budget: p.budget_utilization ?? 0,
        });
        return;
      }

      // Response (fallback if no streaming chunks)
      if (event.type === "Response") {
        setMessages(prev => prev.map(m =>
          m.id === msgId && !m.text ? { ...m, text: (event as any).content || "" } : m
        ));
        return;
      }

      // Done
      if (event.type === "Done") {
        setMessages(prev => prev.map(m =>
          m.id === msgId ? { ...m, isStreaming: false } : m
        ));
        return;
      }

      // Error
      if (event.type === "Error") {
        const errorText = typeof (event as any).error === "string" ? (event as any).error : "Agent execution failed.";
        setMessages(prev => prev.map(m =>
          m.id === msgId ? { ...m, text: m.text || errorText, isStreaming: false } : m
        ));
        streamingMsgIdRef.current = null;
        sendingRef.current = false;
        activeSendAbortRef.current = null;
        setIsSending(false);
        return;
      }
    }).then((dispose) => {
      if (aborted) { dispose(); return; }
      unlisten = dispose;
    }).catch(() => { /* event sub unavailable */ });

    return () => { aborted = true; if (unlisten) unlisten(); };
  }, [selectedAgentId]);

  // ── Send message (real, with streaming) ──
  const handleSendChat = useCallback(async () => {
    if (!chatInput.trim() || !selectedAgentId) return;
    if (sendingRef.current) return;

    const content = chatInput.trim();
    const agentId = selectedAgentId;
    const agentObj = agents.find(a => a.id === agentId);
    const gen = ++sendGenRef.current;
    const abortCtrl = new AbortController();
    activeSendAbortRef.current = abortCtrl;

    const userMsg: ThreadMessage = {
      id: `u_${Date.now()}`,
      role: "user",
      text: content,
      time: fmtTime(),
    };

    const streamMsgId = `s_${Date.now()}`;
    const streamingMsg: ThreadMessage = {
      id: streamMsgId,
      role: "assistant",
      text: "",
      time: fmtTime(),
      agent: agentObj?.name ?? agentId,
      toolCalls: [],
      isStreaming: true,
    };

    streamingMsgIdRef.current = streamMsgId;
    sendingRef.current = true;
    setMessages(prev => [...prev, userMsg, streamingMsg]);
    setIsSending(true);
    setChatInput("");
    setCurrentRound(0);
    setPromptInfo(null);

    try {
      // Pre-create chat session if needed
      let chatId = activeChatIdRef.current;
      if (!chatId) {
        try {
          const newSession = await api.createChat(agentId);
          chatId = newSession.chat_id;
          activeChatIdRef.current = chatId;
          setActiveChatId(chatId);
          setSessions(prev => [newSession, ...prev]);
        } catch (e) {
          console.error("[RT-SEND] Pre-creation failed:", e);
        }
      }

      // Send with timeout
      const invokePromise = api.sendMessage(agentId, content, undefined, chatId ?? undefined);
      const timeoutPromise = new Promise<never>((_, reject) =>
        setTimeout(() => reject(new Error("Request timed out (300s)")), 300000)
      );
      const response = await Promise.race([invokePromise, timeoutPromise]);

      if (gen !== sendGenRef.current || abortCtrl.signal.aborted) return;

      // Adopt chat_id from response
      if (response.chat_id && response.chat_id !== activeChatIdRef.current) {
        activeChatIdRef.current = response.chat_id;
        setActiveChatId(response.chat_id);
      }

      // Finalize streaming message with server metadata
      setMessages(prev => {
        const has = prev.some(m => m.id === streamMsgId);
        if (has) {
          return prev.map(m => {
            if (m.id !== streamMsgId) return m;
            return {
              ...m,
              id: response.message.id,
              text: m.text || response.message.content,
              tokens: response.message.metadata?.token_cost ?? m.tokens,
              cost: response.message.metadata?.cost_usd ?? m.cost,
              duration: response.message.metadata?.duration_ms,
              isStreaming: false,
            };
          });
        }
        // Fallback: placeholder lost
        return [...prev, {
          id: response.message.id,
          role: "assistant" as const,
          text: response.message.content,
          time: fmtTime(),
          agent: agentObj?.name,
          tokens: response.message.metadata?.token_cost,
          cost: response.message.metadata?.cost_usd,
          duration: response.message.metadata?.duration_ms,
          isStreaming: false,
        }];
      });

      // Optimistic sidebar update
      if (response.chat_id) {
        setSessions(prev => {
          const existing = prev.find(s => s.chat_id === response.chat_id);
          if (existing) {
            return prev.map(s => s.chat_id === response.chat_id
              ? { ...s, title: response.chat_title || s.title, message_count: s.message_count + 2, last_activity: new Date().toISOString() }
              : s
            );
          }
          return [{ chat_id: response.chat_id, agent_id: agentId, title: response.chat_title || content.slice(0, 60), message_count: 2, created_at: new Date().toISOString(), last_activity: new Date().toISOString(), pending_approvals: 0, routine_generated: false, has_proof_outputs: false, first_message_preview: content.slice(0, 80) || null } as SessionSummary, ...prev];
        });
      }
    } catch (err) {
      const errMsg = err instanceof Error ? err.message : String(err || "Send failed");
      setMessages(prev => {
        const has = prev.some(m => m.id === streamMsgId);
        if (has) {
          return prev.map(m => m.id === streamMsgId ? { ...m, text: m.text || `Error: ${errMsg}`, isStreaming: false } : m);
        }
        return [...prev, { id: `err_${Date.now()}`, role: "system" as const, text: `Error: ${errMsg}`, time: fmtTime(), isStreaming: false }];
      });
      pushToast(`Send failed: ${errMsg}`);
    } finally {
      streamingMsgIdRef.current = null;
      sendingRef.current = false;
      activeSendAbortRef.current = null;
      setIsSending(false);
    }
  }, [chatInput, selectedAgentId, agents, pushToast]);

  // ── Stop in-flight message ──
  const stopMessage = useCallback(async () => {
    if (!isSending && !sendingRef.current) return;
    if (activeSendAbortRef.current) activeSendAbortRef.current.abort();
    try { await api.cancelActiveRun(activeChatIdRef.current ?? undefined); } catch { /* ignore */ }
    const mid = streamingMsgIdRef.current;
    if (mid) {
      setMessages(prev => prev.map(m => m.id === mid ? { ...m, isStreaming: false, text: m.text || "Stopped." } : m));
    }
    streamingMsgIdRef.current = null;
    sendingRef.current = false;
    activeSendAbortRef.current = null;
    setIsSending(false);
    pushToast("Generation stopped.");
  }, [isSending, pushToast]);

  // ── New thread ──
  const startNewThread = useCallback(() => {
    if (activeSendAbortRef.current) activeSendAbortRef.current.abort();
    setMessages([]);
    setChatInput("");
    setIsSending(false);
    setActiveChatId(null);
    streamingMsgIdRef.current = null;
    sendingRef.current = false;
    activeSendAbortRef.current = null;
    setRuntimeEvents([]);
    setCurrentRound(0);
    setPromptInfo(null);
  }, []);

  // ── Load session messages ──
  const selectSession = useCallback(async (sess: SessionSummary) => {
    if (sendingRef.current) return;
    setActiveChatId(sess.chat_id);
    if (sess.agent_id && sess.agent_id !== selectedAgentId) setSelectedAgentId(sess.agent_id);
    try {
      const msgs = await api.getChatMessages(sess.chat_id);
      setMessages(msgs.map(m => ({
        id: m.id,
        role: m.role as "user" | "assistant" | "system",
        text: m.content,
        time: m.created_at ? new Date(m.created_at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }) : "",
        tokens: m.metadata?.token_cost,
        cost: m.metadata?.cost_usd,
        duration: m.metadata?.duration_ms,
        isStreaming: false,
      })));
    } catch {
      pushToast("Failed to load messages");
    }
  }, [selectedAgentId, pushToast]);

  // ── Task actions (real backend) ──
  const handleTaskAction = useCallback(async (taskId: string, action: string) => {
    const task = tasks.find(t => t.id === taskId);
    if (!task?.runId) return;
    try {
      if (action === "cancel") {
        await api.cancelDurableRun(task.runId, "Cancelled from Runtime UI");
        pushToast(`Run ${shortId(task.runId)} cancelled`);
      } else if (action === "resume" || action === "approve") {
        await api.resumeDurableRun(task.runId);
        pushToast(`Run ${shortId(task.runId)} resumed`);
      }
      refresh();
    } catch (e: any) {
      pushToast(`Action failed: ${e}`);
    }
  }, [tasks, pushToast, refresh]);

  // ── Derived data ──
  const selectedAgent = agents.find(a => a.id === selectedAgentId);
  const activeProvider = getActiveProvider();

  // Effective model: agent's model if explicitly set, else default provider model
  const effectiveModel = useMemo(() => {
    const agentModel = selectedAgent?.model || "";
    if (agentModel && agentModel !== "default" && agentModel !== "auto" && agentModel !== "") {
      return agentModel;
    }
    return activeProvider?.model || "default";
  }, [selectedAgent, activeProvider]);

  // Effective provider label for display
  const effectiveProviderLabel = activeProvider?.provider || "";

  const filteredTasks = useMemo(() => {
    let list = tasks;
    if (taskFilter !== "all") list = list.filter(t => t.state === taskFilter);
    if (taskSearch.trim()) {
      const q = taskSearch.toLowerCase();
      list = list.filter(t => t.title.toLowerCase().includes(q) || t.id.toLowerCase().includes(q) || t.runId.toLowerCase().includes(q));
    }
    return list;
  }, [tasks, taskFilter, taskSearch]);

  const selectedTask = tasks.find(t => t.id === selectedTaskId) ?? null;
  const nowRunning = tasks.filter(t => t.state === "running");
  const needsAttention = tasks.filter(t => t.state === "blocked" || t.state === "retrying");
  const recentDone = tasks.filter(t => t.state === "done").slice(0, 5);

  // Sessions for current agent in the sidebar
  const agentSessions = useMemo(() =>
    sessions.filter(s => !selectedAgentId || s.agent_id === selectedAgentId).slice(0, 30),
    [sessions, selectedAgentId]
  );

  // Current tool calls from latest streaming message
  const activeToolCalls = useMemo(() => {
    const streaming = messages.find(m => m.isStreaming);
    return streaming?.toolCalls ?? [];
  }, [messages]);

  // ── Render ──
  return (
    <div className="rt-root">
      {/* ── Top bar ── */}
      <header className="rt-topbar">
        <div className="rt-topbar-left">
          {(["home", "chat", "tasks"] as MainView[]).map(v => (
            <button key={v} className={`rt-tab ${view === v ? "active" : ""}`} onClick={() => setView(v)}>
              <Icon name={v === "home" ? "dashboard" : v === "chat" ? "chat" : "layers"} />
              {v === "home" ? "Home" : v === "chat" ? "Chat" : "Tasks"}
              {v === "tasks" && runs.length > 0 && (
                <span style={{ marginLeft: 4, fontSize: 10, background: "var(--bg-2)", borderRadius: 8, padding: "1px 5px" }}>{runs.length}</span>
              )}
            </button>
          ))}
        </div>
        {view === "chat" && (
          <div className="rt-topbar-selectors">
            <span className="rt-sel">
              Agent
              <select
                value={selectedAgentId ?? ""}
                onChange={e => { setSelectedAgentId(e.target.value || null); startNewThread(); }}
                style={{ minWidth: 120 }}
              >
                {agents.length === 0 && <option value="">No agents</option>}
                {agents.map(a => <option key={a.id} value={a.id}>{a.name}</option>)}
              </select>
            </span>
            {selectedAgent && (
              <span className="rt-sel" style={{ color: "var(--text-muted)" }}>
                Model: {effectiveModel}{effectiveProviderLabel ? ` (${effectiveProviderLabel})` : ""}
              </span>
            )}
            {isSending && (
              <span className="rt-sel" style={{ color: "var(--ok)", fontWeight: 600 }}>
                <Icon name="loader" /> Streaming\u2026
              </span>
            )}
          </div>
        )}
        <div className="rt-topbar-right">
          {runtimeStatus && (
            <span style={{ fontSize: 11, color: runtimeStatus.durable_runner_available ? "var(--ok)" : "var(--danger)", marginRight: 8 }}>
              {runtimeStatus.durable_runner_available ? "\u25CF Runtime Online" : "\u25CB Runtime Offline"}
            </span>
          )}
          <button className="btn subtle" onClick={refresh} disabled={loading} style={{ fontSize: 12 }}>
            <Icon name="refresh" /> {loading ? "\u2026" : "Refresh"}
          </button>
        </div>
      </header>

      {/* ═══════════════ HOME ═══════════════ */}
      {view === "home" && (
        <div className="rt-home">
          {/* Runtime health banner */}
          {runtimeStatus && (
            <div style={{
              display: "flex", gap: 16, padding: "10px 16px", marginBottom: 12,
              background: "var(--bg-1)", borderRadius: 8, fontSize: 12, color: "var(--text-muted)", alignItems: "center",
            }}>
              <span style={{ color: runtimeStatus.durable_runner_available ? "var(--ok)" : "var(--danger)", fontWeight: 600 }}>
                {runtimeStatus.durable_runner_available ? "\u2713 Runner Available" : "\u2717 Runner Unavailable"}
              </span>
              <span>Worker: {runtimeStatus.worker_id || "\u2014"}</span>
              <span>Store: {runtimeStatus.checkpoint_store || "\u2014"}</span>
              <span>Journal: {runtimeStatus.journal || "\u2014"}</span>
              <span>Leases: {runtimeStatus.lease_manager || "\u2014"}</span>
            </div>
          )}

          <div className="rt-home-grid">
            {/* Now Running */}
            <section className="rt-card">
              <h3 className="rt-card-title"><Icon name="play" /> Active Runs ({nowRunning.length})</h3>
              {nowRunning.length === 0 && <p className="rt-empty-sm">No active runs</p>}
              {nowRunning.map(t => (
                <div key={t.id} className="rt-mini" onClick={() => { setSelectedTaskId(t.id); setView("tasks"); }}>
                  <div className="rt-mini-top">
                    <span className="rt-mini-name">{t.title}</span>
                    <span className="rt-mini-pct">{t.progress}%</span>
                  </div>
                  <div className="rt-bar"><div className="rt-bar-fill" style={{ width: `${t.progress}%` }} /></div>
                  <div className="rt-mini-meta">
                    <span>Worker: {t.owner}</span>
                    {t.checkpoints.length > 0 && <span style={{ marginLeft: 8 }}>{t.checkpoints.length} checkpoint(s)</span>}
                  </div>
                  <div className="rt-mini-actions">
                    <button className="btn ghost" onClick={e => { e.stopPropagation(); setSelectedTaskId(t.id); setView("tasks"); }}>Details</button>
                    <button className="btn ghost" style={{ color: "var(--danger)" }} onClick={e => { e.stopPropagation(); handleTaskAction(t.id, "cancel"); }}>Cancel</button>
                  </div>
                </div>
              ))}
            </section>

            {/* Needs Attention */}
            <section className="rt-card">
              <h3 className="rt-card-title"><Icon name="alert" /> Needs Attention ({needsAttention.length})</h3>
              {needsAttention.length === 0 && <p className="rt-empty-sm">All clear</p>}
              {needsAttention.map(t => (
                <div key={t.id} className="rt-mini attention" onClick={() => { setSelectedTaskId(t.id); setView("tasks"); }}>
                  <div className="rt-mini-top"><Icon name="alert" /> <span className="rt-mini-name">{t.title}</span></div>
                  {t.blockReason && <div className="rt-mini-meta" style={{ color: "var(--warn)" }}>{t.blockReason}</div>}
                  <div className="rt-mini-actions">
                    <button className="btn subtle" onClick={e => { e.stopPropagation(); handleTaskAction(t.id, "resume"); }}>Resume</button>
                    <button className="btn ghost" onClick={e => { e.stopPropagation(); setSelectedTaskId(t.id); setView("tasks"); }}>Review</button>
                  </div>
                </div>
              ))}
            </section>

            {/* Recent Results */}
            <section className="rt-card">
              <h3 className="rt-card-title"><Icon name="check" /> Completed ({recentDone.length})</h3>
              {recentDone.length === 0 && <p className="rt-empty-sm">No completed runs yet</p>}
              {recentDone.map(t => (
                <div key={t.id} className="rt-mini done" onClick={() => { setSelectedTaskId(t.id); setView("tasks"); }}>
                  <div className="rt-mini-top"><Icon name="check" /> <span className="rt-mini-name">{t.title}</span></div>
                  {t.checkpoints.length > 0 && (
                    <div className="rt-mini-meta">{t.checkpoints.length} checkpoint(s)</div>
                  )}
                </div>
              ))}
            </section>

            {/* System Stats */}
            <section className="rt-card">
              <h3 className="rt-card-title"><Icon name="layers" /> System</h3>
              <div className="rt-stats">
                <div className="rt-stat"><span className="rt-stat-val">{tasks.filter(t => t.state === "running").length}</span><span className="rt-stat-lbl">Running</span></div>
                <div className="rt-stat"><span className="rt-stat-val">{tasks.filter(t => t.state === "done").length}</span><span className="rt-stat-lbl">Completed</span></div>
                <div className="rt-stat"><span className="rt-stat-val" style={{ color: "var(--danger)" }}>{tasks.filter(t => t.state === "failed").length}</span><span className="rt-stat-lbl">Failed</span></div>
                <div className="rt-stat"><span className="rt-stat-val" style={{ color: "var(--warn)" }}>{dlq.length}</span><span className="rt-stat-lbl">DLQ</span></div>
              </div>
              {agents.length > 0 && (
                <div style={{ marginTop: 12, fontSize: 12, color: "var(--text-muted)" }}>
                  {agents.length} agent(s) available &middot; {sessions.length} session(s)
                </div>
              )}
            </section>
          </div>

          {/* DLQ details if any */}
          {dlq.length > 0 && (
            <section style={{ marginTop: 16, padding: "12px 16px", background: "color-mix(in srgb, var(--danger) 8%, var(--bg-1))", borderRadius: 8, border: "1px solid color-mix(in srgb, var(--danger) 20%, transparent)" }}>
              <h3 style={{ fontSize: 13, fontWeight: 600, color: "var(--danger)", marginBottom: 8 }}>
                <Icon name="alert" /> Dead Letter Queue ({dlq.length})
              </h3>
              {dlq.slice(0, 5).map(entry => (
                <div key={entry.id} style={{ fontSize: 12, padding: "6px 0", borderBottom: "1px solid var(--border)", display: "flex", gap: 12, alignItems: "baseline" }}>
                  <span style={{ fontFamily: "var(--font-mono)", color: "var(--text-muted)" }}>{shortId(entry.run_id, 16)}</span>
                  <span style={{ flex: 1, color: "var(--danger)" }}>{entry.error}</span>
                  <span style={{ color: "var(--text-muted)" }}>retries: {entry.retry_count}</span>
                  <span style={{ color: "var(--text-muted)" }}>{entry.failed_at}</span>
                </div>
              ))}
              {dlq.length > 5 && <p style={{ fontSize: 11, color: "var(--text-muted)", marginTop: 6 }}>+ {dlq.length - 5} more entries</p>}
            </section>
          )}
        </div>
      )}

      {/* ═══════════════ CHAT (Real Streaming) ═══════════════ */}
      {view === "chat" && (
        <div className="rt-chat">
          {/* Sessions sidebar */}
          <aside className="rt-threads">
            <div style={{ padding: "8px 10px", borderBottom: "1px solid var(--border)" }}>
              <button className="btn primary" onClick={startNewThread} style={{ width: "100%", fontSize: 12 }}>
                <Icon name="plus" /> New Thread
              </button>
            </div>
            <div className="rt-threads-list">
              {agentSessions.length === 0 && (
                <div style={{ padding: "16px 10px", fontSize: 12, color: "var(--text-muted)", textAlign: "center" }}>
                  No sessions yet. Start chatting!
                </div>
              )}
              {agentSessions.map(s => (
                <div
                  key={s.chat_id}
                  className={`rt-thread ${activeChatId === s.chat_id ? "active" : ""}`}
                  onClick={() => selectSession(s)}
                >
                  <span className="rt-thread-name">{s.title || "Untitled"}</span>
                  <span className="rt-thread-ch">{s.message_count} msgs</span>
                </div>
              ))}
            </div>
          </aside>

          {/* Conversation (real streaming) */}
          <main className="rt-convo">
            <div className="rt-convo-msgs">
              {messages.length === 0 && (
                <div className="rt-convo-empty">
                  <Icon name="chat" />
                  {selectedAgent ? (
                    <>
                      <p style={{ fontWeight: 600 }}>Chat with {selectedAgent.name}</p>
                      <p style={{ fontSize: 12, color: "var(--text-muted)" }}>Model: {effectiveModel}{effectiveProviderLabel ? ` via ${effectiveProviderLabel}` : ""} &middot; {selectedAgent.skills.length} skills</p>
                      <p style={{ fontSize: 12, color: "var(--text-muted)", marginTop: 4 }}>Messages stream in real-time with tool calls, thinking, and full agent-event integration.</p>
                    </>
                  ) : (
                    <p>Select an agent above to start chatting.</p>
                  )}
                </div>
              )}
              {messages.map(m => (
                <div key={m.id} className={`rt-msg ${m.role}`}>
                  <div className="rt-msg-head">
                    <span className="rt-msg-role">
                      {m.role === "user" ? "You" : m.role === "assistant" ? (m.agent || "Agent") : "System"}
                    </span>
                    <span className="rt-msg-time">
                      {m.time}
                      {m.tokens != null && <span style={{ marginLeft: 8, opacity: 0.6 }}>{m.tokens} tok</span>}
                      {m.duration != null && <span style={{ marginLeft: 6, opacity: 0.6 }}>{m.duration}ms</span>}
                      {m.cost != null && m.cost > 0 && <span style={{ marginLeft: 6, opacity: 0.6 }}>${m.cost.toFixed(4)}</span>}
                    </span>
                  </div>
                  {/* Thinking block */}
                  {m.thinkingText && (
                    <details className="rt-thinking" style={{ margin: "4px 0 6px", fontSize: 12 }}>
                      <summary style={{ cursor: "pointer", color: "var(--text-muted)", userSelect: "none" }}>
                        <Icon name="sparkles" /> Thinking ({m.thinkingText.length} chars)
                      </summary>
                      <pre style={{ whiteSpace: "pre-wrap", padding: "8px", margin: "4px 0", background: "var(--bg-0)", borderRadius: 6, fontSize: 11, maxHeight: 200, overflow: "auto", color: "var(--text-muted)" }}>
                        {m.thinkingText}
                      </pre>
                    </details>
                  )}
                  {/* Tool calls */}
                  {m.toolCalls && m.toolCalls.length > 0 && (
                    <div style={{ display: "flex", flexDirection: "column", gap: 4, margin: "4px 0 6px" }}>
                      {m.toolCalls.map((tc, i) => (
                        <div key={i} style={{
                          display: "flex", alignItems: "center", gap: 8, padding: "4px 8px",
                          background: "var(--bg-0)", borderRadius: 6, fontSize: 12,
                          borderLeft: `3px solid ${tc.status === "running" ? "var(--warn)" : tc.status === "done" ? "var(--ok)" : "var(--danger)"}`,
                        }}>
                          <span style={{ fontWeight: 600 }}>{tc.name}</span>
                          {tc.status === "running" && <Icon name="loader" />}
                          {tc.status === "done" && <Icon name="check" />}
                          {tc.status === "error" && <Icon name="alert" />}
                          {tc.durationMs != null && <span style={{ color: "var(--text-muted)" }}>{tc.durationMs}ms</span>}
                          {tc.argsPreview && <span style={{ color: "var(--text-muted)", fontSize: 11, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", maxWidth: 200 }}>{tc.argsPreview}</span>}
                        </div>
                      ))}
                    </div>
                  )}
                  {/* Message text */}
                  <div className="rt-msg-text">
                    {m.text}
                    {m.isStreaming && !m.text && <span className="rt-streaming-cursor" />}
                  </div>
                </div>
              ))}
              <div ref={chatEndRef} />
            </div>
            <div className="rt-input-bar">
              <input
                className="rt-input"
                placeholder={selectedAgentId ? "Type a message\u2026" : "Select an agent first"}
                value={chatInput}
                onChange={e => setChatInput(e.target.value)}
                onKeyDown={e => { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); handleSendChat(); } }}
                disabled={!selectedAgentId || isSending}
              />
              {isSending ? (
                <button className="btn ghost" onClick={stopMessage} style={{ color: "var(--danger)" }}>
                  <Icon name="pause" /> Stop
                </button>
              ) : (
                <button className="btn primary" onClick={handleSendChat} disabled={!chatInput.trim() || !selectedAgentId}>
                  <Icon name="send" /> Send
                </button>
              )}
            </div>
          </main>

          {/* Execution context pane (real data) */}
          <aside className="rt-exec">
            <div className="rt-exec-tabs">
              {(["context", "tools", "events", "run"] as ExecTab[]).map(t => (
                <button key={t} className={`rt-exec-tab ${execTab === t ? "active" : ""}`} onClick={() => setExecTab(t)}>
                  {t === "context" ? "Ctx" : t === "tools" ? "Tools" : t === "events" ? "Ev" : "Run"}
                </button>
              ))}
            </div>
            <div className="rt-exec-body">
              {/* Context tab: prompt/agent info */}
              {execTab === "context" && (
                <>
                  <div className="rt-exec-field"><span className="rt-exec-lbl">Agent</span><span>{selectedAgent?.name || "\u2014"}</span></div>
                  <div className="rt-exec-field"><span className="rt-exec-lbl">Model</span><span>{effectiveModel || "\u2014"}</span></div>
                  <div className="rt-exec-field"><span className="rt-exec-lbl">Round</span><span>{currentRound || "\u2014"}</span></div>
                  {promptInfo && (
                    <>
                      <div className="rt-exec-field"><span className="rt-exec-lbl">Tokens</span><span>{promptInfo.totalTokens.toLocaleString()}</span></div>
                      <div className="rt-exec-field"><span className="rt-exec-lbl">Budget</span><span>{(promptInfo.budget * 100).toFixed(0)}%</span></div>
                      <div className="rt-exec-field"><span className="rt-exec-lbl">Memory</span><span>{promptInfo.memFragments} fragments</span></div>
                      {promptInfo.skills.length > 0 && (
                        <div style={{ marginTop: 8 }}>
                          <span className="rt-exec-lbl" style={{ display: "block", marginBottom: 4 }}>Active Skills</span>
                          <div style={{ display: "flex", flexWrap: "wrap", gap: 4 }}>
                            {promptInfo.skills.map((s, i) => (
                              <span key={i} style={{ fontSize: 10, background: "var(--bg-0)", borderRadius: 4, padding: "2px 6px" }}>{s}</span>
                            ))}
                          </div>
                        </div>
                      )}
                    </>
                  )}
                  {!promptInfo && !isSending && <p className="rt-empty-sm">Send a message to see context info.</p>}
                  {isSending && !promptInfo && <p className="rt-empty-sm">Waiting for prompt assembly\u2026</p>}
                </>
              )}

              {/* Tools tab: active tool calls */}
              {execTab === "tools" && (
                <>
                  {activeToolCalls.length === 0 && (
                    <p className="rt-empty-sm">No active tool calls.</p>
                  )}
                  {activeToolCalls.map((tc, i) => (
                    <div key={i} style={{
                      padding: "8px", marginBottom: 6, background: "var(--bg-0)", borderRadius: 6, fontSize: 12,
                      borderLeft: `3px solid ${tc.status === "running" ? "var(--warn)" : tc.status === "done" ? "var(--ok)" : "var(--danger)"}`,
                    }}>
                      <div style={{ fontWeight: 600, marginBottom: 2 }}>
                        {tc.status === "running" && <Icon name="loader" />} {tc.name}
                      </div>
                      {tc.argsPreview && (
                        <pre style={{ fontSize: 10, color: "var(--text-muted)", whiteSpace: "pre-wrap", margin: "2px 0" }}>{tc.argsPreview.slice(0, 200)}</pre>
                      )}
                      {tc.result && <div style={{ fontSize: 11, color: tc.status === "done" ? "var(--ok)" : "var(--danger)" }}>{tc.result}</div>}
                    </div>
                  ))}
                  {/* Show all tools from all messages */}
                  {activeToolCalls.length === 0 && messages.some(m => m.toolCalls?.length) && (
                    <>
                      <p style={{ fontSize: 11, color: "var(--text-muted)", padding: "4px 0" }}>Previous tool calls:</p>
                      {messages.flatMap(m => m.toolCalls ?? []).slice(-10).map((tc, i) => (
                        <div key={i} style={{ fontSize: 11, padding: "3px 0", display: "flex", gap: 6, alignItems: "center", color: "var(--text-muted)" }}>
                          {tc.status === "done" ? <Icon name="check" /> : <Icon name="alert" />}
                          <span>{tc.name}</span>
                          {tc.durationMs != null && <span>{tc.durationMs}ms</span>}
                        </div>
                      ))}
                    </>
                  )}
                </>
              )}

              {/* Events tab: real agent-event log */}
              {execTab === "events" && (
                <>
                  {runtimeEvents.length === 0 && <p className="rt-empty-sm">No events recorded yet.</p>}
                  <div style={{ maxHeight: "calc(100% - 20px)", overflow: "auto" }}>
                    {runtimeEvents.slice(-50).reverse().map(ev => (
                      <div key={ev.id} style={{ fontSize: 11, padding: "3px 0", borderBottom: "1px solid var(--border)", display: "flex", gap: 6 }}>
                        <span style={{ color: "var(--text-muted)", minWidth: 56, fontFamily: "var(--font-mono)" }}>{ev.time}</span>
                        <span style={{
                          fontWeight: 600, minWidth: 80,
                          color: ev.type === "Error" ? "var(--danger)" : ev.type === "Done" ? "var(--ok)" : ev.type.includes("Tool") ? "var(--warn)" : "var(--text)",
                        }}>{ev.type}</span>
                        <span style={{ color: "var(--text-muted)", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{ev.detail}</span>
                      </div>
                    ))}
                  </div>
                </>
              )}

              {/* Run tab: durable run info */}
              {execTab === "run" && (
                <>
                  <div className="rt-exec-field"><span className="rt-exec-lbl">Chat ID</span><span style={{ fontFamily: "var(--font-mono)", fontSize: 10 }}>{activeChatId || "\u2014"}</span></div>
                  <div className="rt-exec-field"><span className="rt-exec-lbl">Sending</span><span>{isSending ? "Yes" : "No"}</span></div>
                  <div className="rt-exec-field"><span className="rt-exec-lbl">Messages</span><span>{messages.length}</span></div>
                  <div className="rt-exec-field"><span className="rt-exec-lbl">Events</span><span>{runtimeEvents.length}</span></div>
                  <div style={{ marginTop: 12 }}>
                    <span className="rt-exec-lbl" style={{ display: "block", marginBottom: 4 }}>Durable Runs</span>
                    {runs.length === 0 && <p className="rt-empty-sm">No durable runs active.</p>}
                    {runs.slice(0, 5).map(r => (
                      <div key={r.run_id} style={{ fontSize: 11, padding: "4px 0", borderBottom: "1px solid var(--border)" }}>
                        <div style={{ fontFamily: "var(--font-mono)", fontSize: 10 }}>{shortId(r.run_id)}</div>
                        <div style={{ display: "flex", gap: 8, marginTop: 2 }}>
                          <span style={{ color: stateColor(mapRunState(r.state)) }}>{r.state}</span>
                          <span style={{ color: "var(--text-muted)" }}>{r.worker_id || "unassigned"}</span>
                        </div>
                      </div>
                    ))}
                  </div>
                </>
              )}
            </div>
            <div className="rt-exec-actions">
              {isSending && <button className="btn subtle" style={{ color: "var(--danger)" }} onClick={stopMessage}>Stop</button>}
              <button className="btn ghost" onClick={startNewThread}>Clear</button>
              <button className="btn ghost" onClick={() => setRuntimeEvents([])}>Clear Events</button>
            </div>
          </aside>
        </div>
      )}

      {/* ═══════════════ TASKS ═══════════════ */}
      {view === "tasks" && (
        <div className="rt-tasks">
          {/* Toolbar */}
          <div className="rt-tasks-toolbar">
            <div className="rt-tasks-filters">
              <select className="rt-filter-sel" value={taskFilter} onChange={e => setTaskFilter(e.target.value as TaskFilter)}>
                <option value="all">All ({tasks.length})</option>
                <option value="running">Running ({tasks.filter(t => t.state === "running").length})</option>
                <option value="blocked">Blocked ({tasks.filter(t => t.state === "blocked").length})</option>
                <option value="retrying">Retrying ({tasks.filter(t => t.state === "retrying").length})</option>
                <option value="done">Done ({tasks.filter(t => t.state === "done").length})</option>
                <option value="failed">Failed ({tasks.filter(t => t.state === "failed").length})</option>
                <option value="pending">Pending ({tasks.filter(t => t.state === "pending").length})</option>
              </select>
              <div className="rt-tasks-search">
                <Icon name="search" />
                <input placeholder="Search runs\u2026" value={taskSearch} onChange={e => setTaskSearch(e.target.value)} />
              </div>
            </div>
            <div style={{ display: "flex", gap: 6 }}>
              <button className="btn subtle" onClick={refresh} style={{ fontSize: 12 }}><Icon name="refresh" /> Refresh</button>
            </div>
          </div>

          {/* Table + detail split */}
          <div className="rt-tasks-split">
            <div className="rt-tasks-table-wrap">
              <table className="rt-tasks-table">
                <thead>
                  <tr>
                    <th style={{ width: 64 }}>ID</th>
                    <th>Run ID</th>
                    <th style={{ width: 90 }}>State</th>
                    <th style={{ width: 80 }}>Worker</th>
                    <th style={{ width: 60 }}>Chkpts</th>
                  </tr>
                </thead>
                <tbody>
                  {filteredTasks.length === 0 && (
                    <tr><td colSpan={5} className="rt-tasks-empty-row">
                      {loading ? "Loading\u2026" : "No durable runs found."}
                    </td></tr>
                  )}
                  {filteredTasks.map(t => (
                    <tr key={t.id} className={`rt-task-row ${selectedTaskId === t.id ? "selected" : ""}`} onClick={() => setSelectedTaskId(t.id)}>
                      <td className="rt-task-id">{t.id}</td>
                      <td style={{ fontFamily: "var(--font-mono)", fontSize: 11 }}>{shortId(t.runId, 28)}</td>
                      <td>
                        <span className="rt-state-badge" style={{ color: stateColor(t.state), background: `color-mix(in srgb, ${stateColor(t.state)} 12%, transparent)` }}>
                          <Icon name={stateIcon(t.state)} /> {t.state}
                        </span>
                      </td>
                      <td className="rt-task-owner">{t.owner}</td>
                      <td style={{ textAlign: "center" }}>{t.checkpoints.length}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>

            {/* Detail panel */}
            {selectedTask && (
              <div className="rt-detail">
                <div className="rt-detail-head">
                  <span className="rt-detail-id">{selectedTask.id}</span>
                  <span className="rt-detail-title">{selectedTask.title}</span>
                </div>
                <div className="rt-detail-body">
                  <div className="rt-detail-section">
                    <h4>Run Details</h4>
                    <div className="rt-detail-field"><b>Run ID:</b> <code style={{ fontSize: 10, background: "var(--bg-0)", padding: "1px 4px", borderRadius: 3 }}>{selectedTask.runId}</code></div>
                    <div className="rt-detail-field"><b>State:</b> <span style={{ color: stateColor(selectedTask.state), fontWeight: 600 }}>{selectedTask.state}</span></div>
                    <div className="rt-detail-field"><b>Worker:</b> {selectedTask.owner}</div>
                    {selectedTask.durableStatus && (
                      <div className="rt-detail-field"><b>Status:</b> <span style={{ fontSize: 11, color: "var(--text-muted)" }}>{selectedTask.durableStatus.slice(0, 200)}</span></div>
                    )}
                  </div>

                  <div className="rt-detail-section">
                    <h4>Timeline ({selectedTask.timeline.length})</h4>
                    {selectedTask.timeline.map((e, i) => (
                      <div key={i} className="rt-tl-entry">
                        <span className="rt-tl-time">{e.time}</span>
                        <span className="rt-tl-text" style={{ color: e.type === "error" ? "var(--danger)" : e.type === "warn" ? "var(--warn)" : e.type === "ok" ? "var(--ok)" : "var(--text)" }}>{e.text}</span>
                      </div>
                    ))}
                  </div>

                  {selectedTask.checkpoints.length > 0 && (
                    <div className="rt-detail-section">
                      <h4>Checkpoints ({selectedTask.checkpoints.length})</h4>
                      {selectedTask.checkpoints.map((cp, i) => (
                        <div key={i} style={{ fontSize: 11, padding: "4px 0", borderBottom: "1px solid var(--border)" }}>
                          <div style={{ display: "flex", gap: 8 }}>
                            <span style={{ fontWeight: 600 }}>Step {cp.step_index}</span>
                            <span style={{ color: "var(--text-muted)" }}>{cp.created_at}</span>
                          </div>
                          {cp.state_snapshot && (
                            <pre style={{ fontSize: 10, color: "var(--text-muted)", whiteSpace: "pre-wrap", maxHeight: 60, overflow: "auto", margin: "2px 0" }}>
                              {cp.state_snapshot.slice(0, 200)}{cp.state_snapshot.length > 200 ? "\u2026" : ""}
                            </pre>
                          )}
                        </div>
                      ))}
                    </div>
                  )}

                  {selectedTask.blockReason && (
                    <div className="rt-detail-section">
                      <h4>Block Info</h4>
                      <div className="rt-detail-field" style={{ color: "var(--warn)" }}>{selectedTask.blockReason}</div>
                    </div>
                  )}
                </div>
                <div className="rt-detail-actions">
                  {(selectedTask.state === "blocked" || selectedTask.state === "failed") && (
                    <button className="btn primary" onClick={() => handleTaskAction(selectedTask.id, "resume")}><Icon name="play" /> Resume</button>
                  )}
                  {selectedTask.state === "running" && (
                    <button className="btn ghost" style={{ color: "var(--danger)" }} onClick={() => handleTaskAction(selectedTask.id, "cancel")}>
                      <Icon name="pause" /> Cancel
                    </button>
                  )}
                  {selectedTask.state === "blocked" && (
                    <button className="btn subtle" onClick={() => handleTaskAction(selectedTask.id, "approve")}>Approve</button>
                  )}
                </div>
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
