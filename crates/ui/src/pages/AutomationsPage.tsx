import { useState, useCallback, useEffect } from "react";
import * as api from "../api";
import type { PipelineDescriptor, PipelineNodeDescriptor, PipelineStepEvent } from "../types";
import { AutomationDesigner } from "../components/AutomationDesigner";
import { DagCanvas } from "../components/DagCanvas";

// ── Templates ─────────────────────────────────────────────────

const JOB_TEMPLATES = [
  { id: "a1", icon: "📧", title: "Email me a daily summary", desc: "Get a morning recap of what happened yesterday and what's planned today." },
  { id: "a2", icon: "📋", title: "Summarize my unread messages", desc: "Check all channels and give me the highlights." },
  { id: "a3", icon: "💰", title: "Alert me if spending is high", desc: "Send a notification when AI usage costs exceed my budget." },
  { id: "a4", icon: "📊", title: "Weekly activity report", desc: "Create a report of what I accomplished this week." },
  { id: "a5", icon: "🔔", title: "Remind me to follow up", desc: "Check for conversations that need a response and remind me." },
  { id: "a6", icon: "🧹", title: "Clean up old conversations", desc: "Archive conversations that haven't been active in a while." },
  { id: "a7", icon: "📝", title: "Prepare meeting notes", desc: "Draft an agenda and notes template for my upcoming meetings." },
  { id: "a8", icon: "🔍", title: "Monitor a topic", desc: "Watch for mentions of a topic I care about and notify me." },
  { id: "a9", icon: "📈", title: "Track my progress", desc: "Summarize how my tasks and goals are progressing." },
];

// ── Schedule helpers ──────────────────────────────────────────

function nextCronDate(cron: string, after: Date): Date | null {
  const parts = cron.trim().split(/\s+/);
  if (parts.length !== 5) return null;
  const parseField = (field: string, min: number, max: number): Set<number> => {
    const vals = new Set<number>();
    for (const part of field.split(",")) {
      const stepMatch = part.match(/^(.+)\/(\d+)$/);
      const step = stepMatch ? parseInt(stepMatch[2]) : 1;
      const range = stepMatch ? stepMatch[1] : part;
      if (range === "*") { for (let i = min; i <= max; i += step) vals.add(i); }
      else if (range.includes("-")) { const [a, b] = range.split("-").map(Number); for (let i = a; i <= b; i += step) vals.add(i); }
      else { vals.add(parseInt(range)); }
    }
    return vals;
  };
  const minutes = parseField(parts[0], 0, 59);
  const hours = parseField(parts[1], 0, 23);
  const doms = parseField(parts[2], 1, 31);
  const months = parseField(parts[3], 1, 12);
  const dows = parseField(parts[4], 0, 6);
  const d = new Date(after);
  d.setSeconds(0, 0);
  d.setMinutes(d.getMinutes() + 1);
  for (let safety = 0; safety < 525960; safety++) {
    const mo = d.getMonth() + 1, dom = d.getDate(), dow = d.getDay(), hr = d.getHours(), mn = d.getMinutes();
    if (months.has(mo) && doms.has(dom) && dows.has(dow) && hours.has(hr) && minutes.has(mn)) return d;
    d.setMinutes(d.getMinutes() + 1);
  }
  return null;
}

/** Human-readable schedule description (no cron jargon). */
function describeSchedule(cron: string): string {
  const parts = cron.trim().split(/\s+/);
  if (parts.length !== 5) return cron;
  const [min, hr, dom, , dow] = parts;
  const dayNames = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

  // Format time
  const h = parseInt(hr); const m = parseInt(min);
  const ampm = h >= 12 ? "PM" : "AM";
  const h12 = h === 0 ? 12 : h > 12 ? h - 12 : h;
  const timeStr = `${h12}:${String(m).padStart(2, "0")} ${ampm}`;

  // Day description
  if (dow === "*" && dom === "*") return `Every day at ${timeStr}`;
  if (dow === "1-5") return `Weekdays at ${timeStr}`;
  if (dow === "0,6") return `Weekends at ${timeStr}`;
  if (dow !== "*") {
    const days: number[] = [];
    for (const seg of dow.split(",")) {
      if (seg.includes("-")) {
        const [a, b] = seg.split("-").map(Number);
        for (let i = a; i <= b; i++) days.push(i);
      } else days.push(parseInt(seg));
    }
    if (days.length === 1) return `Every ${dayNames[days[0]]} at ${timeStr}`;
    if (days.length === 7) return `Every day at ${timeStr}`;
    return `${days.map(d => dayNames[d]?.slice(0, 3)).join(", ")} at ${timeStr}`;
  }
  if (dom !== "*") return `Day ${dom} of each month at ${timeStr}`;
  return `Runs at ${timeStr}`;
}

/** Relative time until next run. */
function timeUntilNextRun(cron: string): string {
  const next = nextCronDate(cron, new Date());
  if (!next) return "";
  const diffMs = next.getTime() - Date.now();
  const diffMin = Math.floor(diffMs / 60000);
  if (diffMin < 1) return "less than a minute";
  if (diffMin < 60) return `in ${diffMin} min`;
  const diffHr = Math.floor(diffMin / 60);
  if (diffHr < 24) return `in ${diffHr}h ${diffMin % 60}m`;
  const diffDay = Math.floor(diffHr / 24);
  return `in ${diffDay}d ${diffHr % 24}h`;
}

function formatNextRunFull(cron: string): string {
  const next = nextCronDate(cron, new Date());
  if (!next) return "—";
  return next.toLocaleString([], { weekday: "short", month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
}

// ── Props ─────────────────────────────────────────────────────

export interface AutomationsPageProps {
  pipelines: PipelineDescriptor[];
  agents: { id: string; name: string; icon: string }[];
  onRefreshPipelines: () => void;
  pushToast: (text: string) => void;
  onNavigate: (nav: string, options?: { threadId?: string }) => void;
}

// ── Component ─────────────────────────────────────────────────

import { PageLayout } from "../components/PageLayout";

export function AutomationsPage({
  pipelines,
  agents,
  onRefreshPipelines,
  pushToast,
  onNavigate,
}: AutomationsPageProps) {
  const [draftPrompt, setDraftPrompt] = useState("");
  const [runningId, setRunningId] = useState<string | null>(null);
  const [deletingId, setDeletingId] = useState<string | null>(null);
  const [designerOpen, setDesignerOpen] = useState(false);
  const [editingPipeline, setEditingPipeline] = useState<PipelineDescriptor | null>(null);
  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [executionEvents, setExecutionEvents] = useState<Map<number, PipelineStepEvent>>(new Map());
  const [cronTasks, setCronTasks] = useState<any[]>([]);
  const [cronLogs, setCronLogs] = useState<any[]>([]);

  // Fetch cron data
  useEffect(() => {
    api.listCronTasks().then(setCronTasks).catch(() => {});
    api.getCronLogs(30).then(setCronLogs).catch(() => {});
  }, [pipelines]);

  const openDesignerFromTemplate = useCallback((template: typeof JOB_TEMPLATES[number]) => {
    const prebuilt: PipelineDescriptor = {
      id: "",
      name: template.title,
      description: template.desc,
      steps: [
        { label: "Input", node_type: "input", model: null, agent_id: null, x: 0, y: 0 },
        { label: template.title, node_type: "agent", model: null, agent_id: null, x: 200, y: 0 },
        { label: "Output", node_type: "output", model: null, agent_id: null, x: 400, y: 0 },
      ],
      edges: [[0, 1], [1, 2]],
      created: "",
    };
    setEditingPipeline(prebuilt);
    setDesignerOpen(true);
  }, []);

  const runPipeline = useCallback(async (pipelineId: string) => {
    setRunningId(pipelineId);
    const pip = pipelines.find((p) => p.id === pipelineId);
    if (pip) {
      const evMap = new Map<number, PipelineStepEvent>();
      pip.steps.forEach((_s, i) => {
        evMap.set(i, { pipeline_id: pipelineId, step_index: i, status: "started", timestamp: new Date().toISOString() });
      });
      setExecutionEvents(evMap);
    }
    try {
      const result = await api.runPipeline(pipelineId);
      if (result && result.steps) {
        const evMap = new Map<number, PipelineStepEvent>();
        let failedLabels: string[] = [];
        let skippedCount = 0;
        result.steps.forEach((s: any) => {
          const isSkipped = s.skipped === true;
          const isFailed = !s.success && !isSkipped;
          evMap.set(s.step_index, {
            pipeline_id: pipelineId,
            step_index: s.step_index,
            status: isSkipped ? "completed" : s.success ? "completed" : "failed",
            timestamp: new Date().toISOString(),
            output_preview: s.output_preview ?? s.output,
            error: s.error,
          });
          if (isFailed) failedLabels.push(s.label ?? `Step ${s.step_index}`);
          if (isSkipped) skippedCount++;
        });
        setExecutionEvents(evMap);
        if (failedLabels.length > 0) {
          pushToast(`Job finished — ${failedLabels.join(", ")} failed.`);
        } else if (skippedCount > 0) {
          pushToast(`Job completed (${skippedCount} step${skippedCount > 1 ? "s" : ""} skipped).`);
        } else {
          pushToast("Job completed successfully!");
        }
      } else {
        pushToast("Job completed.");
      }
      api.getCronLogs(30).then(setCronLogs).catch(() => {});
    } catch {
      pushToast("Job failed to run.");
      if (pip) {
        const evMap = new Map<number, PipelineStepEvent>();
        pip.steps.forEach((_s, i) => {
          evMap.set(i, { pipeline_id: pipelineId, step_index: i, status: "failed", timestamp: new Date().toISOString(), error: "Run failed" });
        });
        setExecutionEvents(evMap);
      }
    } finally {
      setRunningId(null);
    }
  }, [pushToast, pipelines]);

  const handleDeletePipeline = useCallback(async (pipelineId: string, pipelineName: string) => {
    setDeletingId(pipelineId);
    try {
      await api.deletePipeline(pipelineId);
      pushToast(`"${pipelineName}" deleted.`);
      onRefreshPipelines();
    } catch {
      pushToast("Failed to delete.");
    } finally {
      setDeletingId(null);
    }
  }, [pushToast, onRefreshPipelines]);

  const getCronStatus = (pipelineId: string): { active: boolean; label: string; className: string } => {
    const task = cronTasks.find((t) => t.id === `pipeline:${pipelineId}`);
    if (task) return task.enabled
      ? { active: true, label: "Active", className: "sj-status active" }
      : { active: false, label: "Paused", className: "sj-status paused" };
    return { active: false, label: "Registered", className: "sj-status pending" };
  };

  const getLogsForPipeline = (pipelineId: string) => {
    return cronLogs.filter((l) => l.task_id === `pipeline:${pipelineId}`);
  };

  return (
    <>
    <PageLayout
      title="Scheduled Jobs"
      subtitle="Create routines that run automatically or with one click."
      actions={
        <button className="btn primary" style={{ whiteSpace: "nowrap" }} onClick={() => { setEditingPipeline(null); setDesignerOpen(true); }}>
          + New Job
        </button>
      }
      className="page-automations"
    >

      {/* Quick create */}
      {agents.length > 0 && (
        <section className="section-card" style={{ padding: "12px 16px" }}>
          <div className="quick-create-row">
            <span style={{ fontSize: 18 }}>✨</span>
            <input
              className="input quick-create-input"
              placeholder="What would you like to automate? Describe it here and press Enter..."
              value={draftPrompt}
              onChange={(e) => setDraftPrompt(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && draftPrompt.trim()) {
                  const prebuilt: PipelineDescriptor = {
                    id: "",
                    name: draftPrompt.trim().slice(0, 60),
                    description: draftPrompt.trim(),
                    steps: [
                      { label: "Input", node_type: "input", model: null, agent_id: null, x: 0, y: 0 },
                      { label: draftPrompt.trim().slice(0, 40), node_type: "agent", model: null, agent_id: null, x: 200, y: 0 },
                      { label: "Output", node_type: "output", model: null, agent_id: null, x: 400, y: 0 },
                    ],
                    edges: [[0, 1], [1, 2]],
                    created: "",
                  };
                  setEditingPipeline(prebuilt);
                  setDesignerOpen(true);
                  setDraftPrompt("");
                }
              }}
            />
          </div>
        </section>
      )}

      {/* ── Unified Job List ── */}
      {pipelines.length > 0 && (
        <section className="section-card">
          <div className="section-head">
            <h2>Your Jobs ({pipelines.length})</h2>
            <button className="btn subtle" onClick={() => {
              onRefreshPipelines();
              api.listCronTasks().then(setCronTasks).catch(() => {});
              api.getCronLogs(30).then(setCronLogs).catch(() => {});
            }}>Refresh</button>
          </div>

          <div className="sj-list">
            {pipelines.map((p) => {
              const hasSchedule = !!p.schedule;
              const cronStatus = hasSchedule ? getCronStatus(p.id) : null;
              const pipelineLogs = getLogsForPipeline(p.id);
              const isExpanded = expandedId === p.id;

              return (
                <div key={p.id} className={`sj-card${isExpanded ? " expanded" : ""}`}>
                  {/* Main row */}
                  <div className="sj-card-main">
                    <div className="sj-card-icon">
                      {hasSchedule ? "🕐" : "⚡"}
                    </div>
                    <div className="sj-card-info">
                      <div className="sj-card-name">{p.name}</div>
                      {p.description && (
                        <div className="sj-card-desc">{p.description}</div>
                      )}
                      <div className="sj-card-meta">
                        {hasSchedule ? (
                          <>
                            <span className="sj-schedule-text">
                              {describeSchedule(p.schedule!)}
                            </span>
                            <span className="sj-separator">·</span>
                            <span className="sj-next-run">
                              Next: {formatNextRunFull(p.schedule!)}
                              <span className="sj-countdown">({timeUntilNextRun(p.schedule!)})</span>
                            </span>
                          </>
                        ) : (
                          <span className="sj-manual-label">Manual — run on demand</span>
                        )}
                        <span className="sj-separator">·</span>
                        <span className="sj-steps-count">{p.steps.length} steps</span>
                      </div>
                    </div>
                    <div className="sj-card-right">
                      {cronStatus && (
                        <span className={cronStatus.className}>
                          {cronStatus.active ? "●" : "○"} {cronStatus.label}
                        </span>
                      )}
                      <div className="sj-card-actions">
                        <button
                          className="btn primary sj-run-btn"
                          disabled={runningId === p.id}
                          onClick={() => runPipeline(p.id)}
                          title="Run now"
                        >
                          {runningId === p.id ? "Running..." : "▶ Run"}
                        </button>
                        <button
                          className="btn subtle"
                          onClick={() => { setEditingPipeline(p); setDesignerOpen(true); }}
                          title="Edit job"
                        >
                          Edit
                        </button>
                        <button
                          className="btn subtle"
                          onClick={() => setExpandedId(isExpanded ? null : p.id)}
                          title={isExpanded ? "Collapse" : "View details & logs"}
                        >
                          {isExpanded ? "▲" : "▼"}
                        </button>
                        <button
                          className="btn ghost sj-delete-btn"
                          disabled={deletingId === p.id}
                          onClick={() => handleDeletePipeline(p.id, p.name)}
                          title="Delete job"
                        >
                          {deletingId === p.id ? "..." : "🗑"}
                        </button>
                      </div>
                    </div>
                  </div>

                  {/* Expanded details panel */}
                  {isExpanded && (
                    <div className="sj-card-expanded">
                      <div className="sj-detail-section">
                        <h4>Pipeline Steps</h4>
                        <DagCanvas
                          pipeline={p}
                          stepEvents={runningId === p.id ? executionEvents : undefined}
                          width={660}
                          height={Math.max(180, p.steps.length * 55)}
                        />
                      </div>

                      <div className="sj-detail-section">
                        <h4>Run History</h4>
                        {pipelineLogs.length > 0 ? (
                          <div className="sj-logs">
                            {pipelineLogs.slice(0, 8).map((log, i) => {
                              const isSuccess = (log.status || "").toLowerCase().includes("success");
                              const isFailed = (log.status || "").toLowerCase().includes("fail");
                              return (
                                <div key={log.run_id || i} className="sj-log-row">
                                  <span className={`sj-log-icon ${isSuccess ? "success" : isFailed ? "failed" : "other"}`}>
                                    {isSuccess ? "✓" : isFailed ? "✗" : "⟳"}
                                  </span>
                                  <span className="sj-log-label">
                                    {isSuccess ? "Completed" : isFailed ? "Failed" : log.status || "Running"}
                                  </span>
                                  <span className="sj-log-time">
                                    {log.started_at
                                      ? new Date(log.started_at).toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" })
                                      : "—"}
                                  </span>
                                  {log.error && (
                                    <span className="sj-log-error" title={log.error}>
                                      {log.error.slice(0, 80)}
                                    </span>
                                  )}
                                </div>
                              );
                            })}
                          </div>
                        ) : (
                          <div className="sj-no-logs">No runs yet. Click ▶ Run to execute this job.</div>
                        )}
                      </div>

                      {hasSchedule && (
                        <div className="sj-detail-section">
                          <h4>Schedule Details</h4>
                          <div className="sj-schedule-detail">
                            <div><strong>Cron:</strong> <code>{p.schedule}</code></div>
                            <div><strong>Runs:</strong> {describeSchedule(p.schedule!)}</div>
                            <div style={{ marginTop: 6 }}><strong>Next 3 runs:</strong></div>
                            <div className="sj-next-runs">
                              {(() => {
                                const runs: string[] = [];
                                let cursor = new Date();
                                for (let i = 0; i < 3; i++) {
                                  const next = nextCronDate(p.schedule!, cursor);
                                  if (!next) break;
                                  runs.push(next.toLocaleString([], { weekday: "short", month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" }));
                                  cursor = next;
                                }
                                return runs.map((r, i) => <div key={i} className="sj-next-run-item">{i + 1}. {r}</div>);
                              })()}
                            </div>
                          </div>
                        </div>
                      )}
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        </section>
      )}

      {/* No agents */}
      {agents.length === 0 ? (
        <section className="section-card">
          <div className="empty-state-action" style={{ padding: 32, textAlign: "center" }}>
            <p style={{ fontSize: 16, marginBottom: 8 }}>Set up a provider first to power your jobs.</p>
            <p className="settings-desc" style={{ marginBottom: 16 }}>Scheduled jobs use AI to run each step. Configure your LLM in Settings, then come back here.</p>
            <button className="btn primary" onClick={() => onNavigate("settings")}>
              Go to Settings
            </button>
          </div>
        </section>
      ) : pipelines.length === 0 && (
        <section className="section-card">
          <div className="empty-state-action" style={{ padding: 32, textAlign: "center" }}>
            <div style={{ fontSize: 36, marginBottom: 12 }}>🕐</div>
            <p style={{ fontSize: 16, marginBottom: 8 }}>No scheduled jobs yet</p>
            <p className="settings-desc" style={{ marginBottom: 16 }}>Create a job to run tasks on a schedule or with one click.</p>
            <button className="btn primary" onClick={() => { setEditingPipeline(null); setDesignerOpen(true); }}>
              + Create Your First Job
            </button>
          </div>
        </section>
      )}

      {/* Template grid */}
      {agents.length > 0 && (
        <section className="section-card">
          <div className="section-head">
            <h2>Quick Start Templates</h2>
            <p className="section-desc">Pick a template to get started quickly.</p>
          </div>
          <div className="automation-template-grid">
            {JOB_TEMPLATES.map((a) => (
              <button
                key={a.id}
                className="automation-template-card"
                onClick={() => openDesignerFromTemplate(a)}
              >
                <span className="automation-template-icon">{a.icon}</span>
                <div className="automation-template-body">
                  <div className="automation-template-title">{a.title}</div>
                  <div className="automation-template-desc">{a.desc}</div>
                </div>
              </button>
            ))}
          </div>
        </section>
      )}

    </PageLayout>

    {designerOpen && (
      <AutomationDesigner
        existingPipeline={editingPipeline}
        onClose={() => setDesignerOpen(false)}
        onSaved={() => { setDesignerOpen(false); onRefreshPipelines(); }}
        pushToast={pushToast}
      />
    )}
    </>
  );
}
