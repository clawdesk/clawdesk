import { useState, useEffect, useCallback } from "react";
import * as api from "../api";
import { PageLayout } from "../components/PageLayout";
import { Modal } from "../components/Modal";
import { Icon } from "../components/Icon";
import type {
  AgentCardInfo,
  RegisterAgentCardRequest,
  A2ATaskResponse,
  TaskSendRequest,
} from "../types";

// ── Types ─────────────────────────────────────────────────────

type A2ATab = "directory" | "tasks";

const CAPABILITIES = [
  "text_generation",
  "code_execution",
  "web_search",
  "file_processing",
  "image_processing",
  "audio_processing",
  "api_integration",
  "data_management",
  "mathematics",
  "scheduling",
  "messaging",
];

// ── Props ─────────────────────────────────────────────────────

export interface A2APageProps {
  pushToast: (msg: string) => void;
}

// ── Component ─────────────────────────────────────────────────

export function A2APage({ pushToast }: A2APageProps) {
  const [tab, setTab] = useState<A2ATab>("directory");
  const [agents, setAgents] = useState<AgentCardInfo[]>([]);
  const [tasks, setTasks] = useState<A2ATaskResponse[]>([]);
  const [loading, setLoading] = useState(true);
  const [showRegister, setShowRegister] = useState(false);
  const [showSendTask, setShowSendTask] = useState(false);
  const [selfCard, setSelfCard] = useState<any>(null);

  // Registration form
  const [regAgentId, setRegAgentId] = useState("");
  const [regName, setRegName] = useState("");
  const [regDesc, setRegDesc] = useState("");
  const [regEndpoint, setRegEndpoint] = useState("http://localhost:18789");
  const [regCaps, setRegCaps] = useState<string[]>([]);

  // Send task form
  const [taskTarget, setTaskTarget] = useState("");
  const [taskInput, setTaskInput] = useState("");
  const [taskSkillId, setTaskSkillId] = useState("");

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      const [a, t] = await Promise.all([
        api.listA2aAgents().catch(() => [] as AgentCardInfo[]),
        api.listA2ATasks().catch(() => [] as A2ATaskResponse[]),
      ]);
      setAgents(a);
      setTasks(t);
      try {
        const sc = await api.getSelfAgentCard();
        setSelfCard(sc);
      } catch { /* ok */ }
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => { refresh(); }, [refresh]);

  const handleRegister = useCallback(async () => {
    if (!regAgentId.trim()) { pushToast("Agent ID is required"); return; }
    const req: RegisterAgentCardRequest = {
      agent_id: regAgentId.trim(),
      name: regName.trim() || undefined,
      description: regDesc.trim() || undefined,
      capabilities: regCaps,
      endpoint: regEndpoint.trim() || undefined,
    };
    try {
      await api.registerA2aAgent(req);
      pushToast(`Agent "${regAgentId}" registered`);
      setShowRegister(false);
      setRegAgentId(""); setRegName(""); setRegDesc(""); setRegCaps([]);
      refresh();
    } catch (e: any) {
      pushToast(`Registration failed: ${e}`);
    }
  }, [regAgentId, regName, regDesc, regEndpoint, regCaps, pushToast, refresh]);

  const handleDeregister = useCallback(async (agentId: string) => {
    try {
      await api.deregisterA2aAgent(agentId);
      pushToast(`Agent "${agentId}" removed`);
      refresh();
    } catch (e: any) {
      pushToast(`Deregister failed: ${e}`);
    }
  }, [pushToast, refresh]);

  const handleSendTask = useCallback(async () => {
    if (!taskTarget.trim() && agents.length === 0) {
      pushToast("Select a target agent");
      return;
    }
    const target = taskTarget.trim() || "self";
    let parsedInput: any;
    try {
      parsedInput = JSON.parse(taskInput || '{"message": "hello"}');
    } catch {
      parsedInput = { message: taskInput || "hello" };
    }
    const req: TaskSendRequest = {
      input: parsedInput,
      target_agent: target,
      skill_id: taskSkillId.trim() || undefined,
    };
    try {
      await api.sendA2ATask("desktop-user", req);
      pushToast("Task sent");
      setShowSendTask(false);
      setTaskInput(""); setTaskTarget(""); setTaskSkillId("");
      refresh();
    } catch (e: any) {
      pushToast(`Send failed: ${e}`);
    }
  }, [taskTarget, taskInput, taskSkillId, agents, pushToast, refresh]);

  const handleCancelTask = useCallback(async (taskId: string) => {
    try {
      await api.cancelA2ATask(taskId, "Cancelled from UI");
      pushToast("Task cancelled");
      refresh();
    } catch (e: any) {
      pushToast(`Cancel failed: ${e}`);
    }
  }, [pushToast, refresh]);

  const stateColor = (state: string) => {
    switch (state) {
      case "completed": return "var(--green)";
      case "failed": case "cancelled": return "var(--red)";
      case "working": case "running": return "var(--cyan)";
      case "input-required": return "var(--amber)";
      default: return "var(--text-tertiary)";
    }
  };

  return (
    <>
      <PageLayout
        title="A2A Agent Directory"
        subtitle="Manage agent mesh connections, delegate tasks, and monitor capabilities."
        actions={
          <div style={{ display: "flex", gap: 8 }}>
            <button className="btn subtle" onClick={refresh} disabled={loading}>
              {loading ? "Loading\u2026" : "Refresh"}
            </button>
            <button className="btn subtle" onClick={() => setShowRegister(true)}>
              <Icon name="plus" /> Register Agent
            </button>
            <button className="btn primary" onClick={() => { setShowSendTask(true); setTaskTarget(agents[0]?.id ?? "self"); }}>
              <Icon name="send" /> Send Task
            </button>
          </div>
        }
      >
        {/* Tab Bar */}
        <div className="tab-bar" style={{ display: "flex", gap: 4, marginBottom: 16 }}>
          {(["directory", "tasks"] as A2ATab[]).map((t) => (
            <button
              key={t}
              className={`btn ${tab === t ? "primary" : "subtle"}`}
              onClick={() => setTab(t)}
              style={{ textTransform: "capitalize" }}
            >
              {t === "directory" ? `Directory (${agents.length})` : `Tasks (${tasks.length})`}
            </button>
          ))}
        </div>

        {/* ── Directory Tab ────────────────────────────────── */}
        {tab === "directory" && (
          <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
            {/* Self Card */}
            {selfCard && (
              <div className="panel-card" style={{ borderLeft: "3px solid var(--brand)" }}>
                <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
                  <div>
                    <h3 className="panel-title" style={{ margin: 0 }}>
                      <Icon name="bot" className="w-4 h-4" /> {selfCard.name ?? "ClawDesk"} <span style={{ color: "var(--text-tertiary)", fontSize: 12 }}>(self)</span>
                    </h3>
                    <div style={{ color: "var(--text-secondary)", fontSize: 13, marginTop: 4 }}>
                      {selfCard.description ?? "Local desktop agent"}
                    </div>
                  </div>
                  <span className="status-dot status-ok" style={{ width: 10, height: 10 }} />
                </div>
                {selfCard.capabilities && (
                  <div style={{ display: "flex", gap: 4, flexWrap: "wrap", marginTop: 8 }}>
                    {(Array.isArray(selfCard.capabilities) ? selfCard.capabilities : []).map((c: string, i: number) => (
                      <span key={i} className="trust-badge">{String(c)}</span>
                    ))}
                  </div>
                )}
              </div>
            )}

            {/* Agent Cards */}
            {agents.length === 0 && !loading && (
              <div className="empty-state centered" style={{ padding: 40 }}>
                <p>No agents registered in the A2A directory.</p>
                <button className="btn primary" onClick={() => setShowRegister(true)}>Register First Agent</button>
              </div>
            )}

            <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(320px, 1fr))", gap: 12 }}>
              {agents.map((agent) => (
                <div key={agent.id} className="panel-card" style={{ position: "relative" }}>
                  <div style={{ display: "flex", justifyContent: "space-between", alignItems: "flex-start" }}>
                    <div style={{ flex: 1 }}>
                      <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                        <span
                          className="status-dot-sm"
                          style={{
                            backgroundColor: agent.is_healthy ? "var(--green)" : "var(--red)",
                            boxShadow: `0 0 4px ${agent.is_healthy ? "var(--green)" : "var(--red)"}40`,
                          }}
                        />
                        <strong style={{ color: "var(--text-primary)" }}>{agent.name}</strong>
                      </div>
                      <div style={{ color: "var(--text-tertiary)", fontSize: 12, marginTop: 2 }}>
                        ID: {agent.id}
                      </div>
                    </div>
                    <button
                      className="btn icon-only subtle"
                      onClick={() => handleDeregister(agent.id)}
                      title="Remove agent"
                      style={{ color: "var(--red)", opacity: 0.6 }}
                    >
                      <Icon name="close" />
                    </button>
                  </div>

                  <div style={{ display: "flex", gap: 4, flexWrap: "wrap", marginTop: 8 }}>
                    {agent.capabilities.map((c, i) => (
                      <span key={i} className="trust-badge" style={{ fontSize: 11 }}>{c}</span>
                    ))}
                  </div>

                  <div style={{ display: "flex", justifyContent: "space-between", marginTop: 10, color: "var(--text-tertiary)", fontSize: 12 }}>
                    <span>Active tasks: {agent.active_tasks}</span>
                    <span style={{ color: agent.is_healthy ? "var(--green)" : "var(--red)" }}>
                      {agent.is_healthy ? "\u25cf Healthy" : "\u25cf Unhealthy"}
                    </span>
                  </div>
                </div>
              ))}
            </div>
          </div>
        )}

        {/* ── Tasks Tab ────────────────────────────────────── */}
        {tab === "tasks" && (
          <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
            {tasks.length === 0 && !loading && (
              <div className="empty-state centered" style={{ padding: 40 }}>
                <p>No A2A tasks yet.</p>
                <button className="btn primary" onClick={() => setShowSendTask(true)}>Send First Task</button>
              </div>
            )}

            {tasks.map((task) => (
              <div key={task.task_id} className="panel-card">
                <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
                  <div>
                    <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                      <span
                        className="status-dot-sm"
                        style={{ backgroundColor: stateColor(task.state) }}
                      />
                      <strong style={{ color: "var(--text-primary)", fontSize: 14 }}>
                        {task.task_id.substring(0, 12)}\u2026
                      </strong>
                      <span className="trust-badge" style={{ textTransform: "capitalize" }}>
                        {task.state}
                      </span>
                    </div>
                    {task.error && (
                      <div style={{ color: "var(--red)", fontSize: 12, marginTop: 4 }}>
                        Error: {task.error}
                      </div>
                    )}
                  </div>
                  <div style={{ display: "flex", gap: 6 }}>
                    {task.state !== "completed" && task.state !== "cancelled" && task.state !== "failed" && (
                      <button
                        className="btn subtle"
                        style={{ fontSize: 12 }}
                        onClick={() => handleCancelTask(task.task_id)}
                      >
                        Cancel
                      </button>
                    )}
                  </div>
                </div>

                {/* Progress bar */}
                {task.progress > 0 && task.progress < 100 && (
                  <div style={{ marginTop: 8, height: 4, background: "var(--bg-tertiary)", borderRadius: 2, overflow: "hidden" }}>
                    <div
                      style={{
                        width: `${task.progress}%`,
                        height: "100%",
                        background: "var(--brand)",
                        borderRadius: 2,
                        transition: "width 0.3s ease",
                      }}
                    />
                  </div>
                )}

                {/* Output preview */}
                {task.output && (
                  <div style={{ marginTop: 6, padding: 8, background: "var(--bg-tertiary)", borderRadius: 6, fontSize: 12, color: "var(--text-secondary)", fontFamily: "monospace", maxHeight: 80, overflow: "auto" }}>
                    {typeof task.output === "string" ? task.output : JSON.stringify(task.output, null, 2)}
                  </div>
                )}
              </div>
            ))}
          </div>
        )}
      </PageLayout>

      {/* ── Register Agent Modal ───────────────────────────── */}
      {showRegister && (
        <Modal title="Register A2A Agent" onClose={() => setShowRegister(false)}>
          <div className="modal-stack">
            <label className="field-label">
              Agent ID *
              <input className="input" value={regAgentId} onChange={(e) => setRegAgentId(e.target.value)} placeholder="my-agent-01" />
            </label>
            <label className="field-label">
              Name
              <input className="input" value={regName} onChange={(e) => setRegName(e.target.value)} placeholder="My Agent" />
            </label>
            <label className="field-label">
              Description
              <textarea className="input" value={regDesc} onChange={(e) => setRegDesc(e.target.value)} placeholder="What this agent does\u2026" rows={2} />
            </label>
            <label className="field-label">
              Endpoint
              <input className="input" value={regEndpoint} onChange={(e) => setRegEndpoint(e.target.value)} placeholder="http://localhost:18789" />
            </label>
            <div>
              <label className="field-label">Capabilities</label>
              <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
                {CAPABILITIES.map((cap) => {
                  const active = regCaps.includes(cap);
                  return (
                    <button
                      key={cap}
                      className={`btn ${active ? "primary" : "subtle"}`}
                      style={{ fontSize: 11, padding: "2px 8px" }}
                      onClick={() => setRegCaps((prev) => active ? prev.filter((c) => c !== cap) : [...prev, cap])}
                    >
                      {cap.replace(/_/g, " ")}
                    </button>
                  );
                })}
              </div>
            </div>
            <div className="row-actions" style={{ justifyContent: "flex-end" }}>
              <button className="btn ghost" onClick={() => setShowRegister(false)}>Cancel</button>
              <button className="btn primary" onClick={handleRegister}>Register</button>
            </div>
          </div>
        </Modal>
      )}

      {/* ── Send Task Modal ────────────────────────────────── */}
      {showSendTask && (
        <Modal title="Send A2A Task" onClose={() => setShowSendTask(false)}>
          <div className="modal-stack">
            <label className="field-label">
              Target Agent
              <select className="input" value={taskTarget} onChange={(e) => setTaskTarget(e.target.value)}>
                <option value="self">Self (ClawDesk)</option>
                {agents.map((a) => (
                  <option key={a.id} value={a.id}>{a.name} ({a.id})</option>
                ))}
              </select>
            </label>
            <label className="field-label">
              Skill ID (optional)
              <input className="input" value={taskSkillId} onChange={(e) => setTaskSkillId(e.target.value)} placeholder="web-search" />
            </label>
            <label className="field-label">
              Input (JSON or text)
              <textarea
                className="input"
                value={taskInput}
                onChange={(e) => setTaskInput(e.target.value)}
                placeholder={'{"message": "Summarize today\'s news"}'}
                rows={4}
                style={{ fontFamily: "monospace", fontSize: 12 }}
              />
            </label>
            <div className="row-actions" style={{ justifyContent: "flex-end" }}>
              <button className="btn ghost" onClick={() => setShowSendTask(false)}>Cancel</button>
              <button className="btn primary" onClick={handleSendTask}>Send Task</button>
            </div>
          </div>
        </Modal>
      )}
    </>
  );
}
