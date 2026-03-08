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

  const healthyAgents = agents.filter((agent) => agent.is_healthy).length;
  const runningTasks = tasks.filter((task) => !["completed", "cancelled", "failed"].includes(task.state)).length;
  const totalCapabilities = new Set(agents.flatMap((agent) => agent.capabilities)).size;

  return (
    <>
      <PageLayout
        title="A2A Agent Directory"
        subtitle="Manage agent mesh connections, delegate tasks, and monitor capabilities."
        actions={
          <div className="a2a-page-actions">
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
        <div className="a2a-page-shell">
          <section className="a2a-hero">
            <div className="a2a-hero__intro">
              <span className="a2a-hero__eyebrow">Mesh overview</span>
              <h2>Keep your agent network visible, healthy, and ready to take work.</h2>
              <p>
                {healthyAgents}/{agents.length || 0} remote agents are healthy, {runningTasks} tasks are in flight, and {totalCapabilities} capabilities are exposed across the mesh.
              </p>
            </div>
            <div className="a2a-hero__stats">
              <HeroMetric label="Registered agents" value={agents.length.toString()} meta={`${healthyAgents} healthy`} accent="brand" />
              <HeroMetric label="Live tasks" value={runningTasks.toString()} meta={`${tasks.length} total tracked`} accent="cyan" />
              <HeroMetric label="Capabilities" value={totalCapabilities.toString()} meta="Distinct abilities across remote cards" accent="green" />
            </div>
          </section>

          <div className="a2a-tabs" role="tablist" aria-label="A2A views">
            {(["directory", "tasks"] as A2ATab[]).map((t) => (
              <button
                key={t}
                className={`a2a-tab${tab === t ? " active" : ""}`}
                onClick={() => setTab(t)}
                role="tab"
                aria-selected={tab === t}
              >
                <span>{t === "directory" ? "Directory" : "Tasks"}</span>
                <strong>{t === "directory" ? agents.length : tasks.length}</strong>
              </button>
            ))}
          </div>

          {tab === "directory" && (
            <div className="a2a-directory-view">
              {selfCard && (
                <section className="a2a-self-card">
                  <div className="a2a-self-card__header">
                    <div>
                      <span className="a2a-section-label">Local node</span>
                      <h3>
                        <Icon name="bot" className="w-4 h-4" /> {selfCard.name ?? "ClawDesk"} <span>(self)</span>
                      </h3>
                      <p>{selfCard.description ?? "Local desktop agent"}</p>
                    </div>
                    <div className="a2a-health-pill ok">Online</div>
                  </div>
                  {selfCard.capabilities && (
                    <div className="a2a-capability-row">
                      {(Array.isArray(selfCard.capabilities) ? selfCard.capabilities : []).map((c: string, i: number) => (
                        <span key={i} className="a2a-capability-chip">{formatCapability(c)}</span>
                      ))}
                    </div>
                  )}
                </section>
              )}

              <section className="a2a-section-head">
                <div>
                  <span className="a2a-section-label">Directory</span>
                  <h3>Registered agent cards</h3>
                  <p>Browse remote nodes, inspect exposed capabilities, and prune stale registrations.</p>
                </div>
                <div className="a2a-section-meta">{agents.length} agents</div>
              </section>

              {agents.length === 0 && !loading && (
                <div className="a2a-empty-state">
                  <h3>No agents registered yet</h3>
                  <p>Register the first remote node to start sharing skills and delegated work across the mesh.</p>
                  <button className="btn primary" onClick={() => setShowRegister(true)}>Register First Agent</button>
                </div>
              )}

              <div className="a2a-agent-grid">
                {agents.map((agent) => (
                  <article key={agent.id} className="a2a-agent-card">
                    <div className="a2a-agent-card__header">
                      <div className="a2a-agent-card__title">
                        <span
                          className="status-dot-sm"
                          style={{
                            backgroundColor: agent.is_healthy ? "var(--green)" : "var(--red)",
                            boxShadow: `0 0 4px ${agent.is_healthy ? "var(--green)" : "var(--red)"}40`,
                          }}
                        />
                        <div>
                          <h4>{agent.name}</h4>
                          <p>{agent.id}</p>
                        </div>
                      </div>
                      <button
                        className="btn icon-only subtle a2a-agent-card__remove"
                        onClick={() => handleDeregister(agent.id)}
                        title="Remove agent"
                      >
                        <Icon name="close" />
                      </button>
                    </div>

                    <div className="a2a-capability-row">
                      {agent.capabilities.map((c, i) => (
                        <span key={i} className="a2a-capability-chip">{formatCapability(c)}</span>
                      ))}
                    </div>

                    <div className="a2a-agent-card__footer">
                      <span>Active tasks: {agent.active_tasks}</span>
                      <span className={`a2a-health-pill ${agent.is_healthy ? "ok" : "error"}`}>
                        {agent.is_healthy ? "Healthy" : "Unhealthy"}
                      </span>
                    </div>
                  </article>
                ))}
              </div>
            </div>
          )}

          {tab === "tasks" && (
            <div className="a2a-task-view">
              <section className="a2a-section-head">
                <div>
                  <span className="a2a-section-label">Task timeline</span>
                  <h3>Delegated work across the mesh</h3>
                  <p>Track active work, progress updates, failure states, and partial output from remote tasks.</p>
                </div>
                <div className="a2a-section-meta">{tasks.length} tasks</div>
              </section>

            {tasks.length === 0 && !loading && (
              <div className="a2a-empty-state">
                <h3>No A2A tasks yet</h3>
                <p>Send a task to a local or remote agent to populate the task timeline and progress stream.</p>
                <button className="btn primary" onClick={() => setShowSendTask(true)}>Send First Task</button>
              </div>
            )}

              <div className="a2a-task-list">
                {tasks.map((task) => (
                  <article key={task.task_id} className="a2a-task-card">
                    <div className="a2a-task-card__header">
                      <div>
                        <div className="a2a-task-card__title">
                          <span
                            className="status-dot-sm"
                            style={{ backgroundColor: stateColor(task.state) }}
                          />
                          <strong>{task.task_id.substring(0, 12)}\u2026</strong>
                          <span className={`a2a-task-state a2a-task-state--${task.state}`}>{formatTaskState(task.state)}</span>
                        </div>
                        {task.error ? <div className="a2a-task-error">Error: {task.error}</div> : null}
                      </div>
                      <div className="a2a-task-card__actions">
                    {task.state !== "completed" && task.state !== "cancelled" && task.state !== "failed" && (
                      <button
                        className="btn subtle"
                        onClick={() => handleCancelTask(task.task_id)}
                      >
                        Cancel
                      </button>
                    )}
                      </div>
                    </div>

                    <div className="a2a-task-meta-row">
                      <span>Progress {typeof task.progress === "number" ? `${task.progress}%` : "Unknown"}</span>
                      <span>State: {formatTaskState(task.state)}</span>
                    </div>

                    {task.progress > 0 && task.progress < 100 && (
                      <div className="a2a-progress-bar">
                        <div
                          className="a2a-progress-bar__fill"
                          style={{ width: `${task.progress}%` }}
                        />
                      </div>
                    )}

                    {task.output && (
                      <div className="a2a-task-output">
                        {typeof task.output === "string" ? task.output : JSON.stringify(task.output, null, 2)}
                      </div>
                    )}
                  </article>
                ))}
              </div>
            </div>
          )}
        </div>
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
              <div className="a2a-modal-cap-grid">
                {CAPABILITIES.map((cap) => {
                  const active = regCaps.includes(cap);
                  return (
                    <button
                      key={cap}
                      className={`btn ${active ? "primary" : "subtle"}`}
                      style={{ fontSize: 11, padding: "4px 10px" }}
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

function HeroMetric({ label, value, meta, accent }: { label: string; value: string; meta: string; accent: "brand" | "cyan" | "green" }) {
  return (
    <div className={`a2a-hero-metric a2a-hero-metric--${accent}`}>
      <span>{label}</span>
      <strong>{value}</strong>
      <small>{meta}</small>
    </div>
  );
}

function formatCapability(value: string) {
  return value.replace(/_/g, " ").replace(/\b\w/g, (char) => char.toUpperCase());
}

function formatTaskState(value: string) {
  return value.replace(/-/g, " ").replace(/\b\w/g, (char) => char.toUpperCase());
}
