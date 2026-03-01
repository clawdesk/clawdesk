import { useState, useCallback } from "react";
import * as api from "../api";
import type { PipelineDescriptor, PipelineNodeDescriptor, PipelineStepEvent } from "../types";
import { Modal } from "../components/Modal";
import { Icon } from "../components/Icon";
import { AutomationDesigner } from "../components/AutomationDesigner";
import { DagCanvas } from "../components/DagCanvas";

// ── Templates (from sample.js) ────────────────────────────────

const AUTOMATION_TEMPLATES = [
  { id: "a1", icon: "�", title: "Email me a daily summary", desc: "Get a morning recap of what happened yesterday and what's planned today." },
  { id: "a2", icon: "📋", title: "Summarize my unread messages", desc: "Check all channels and give me the highlights." },
  { id: "a3", icon: "💰", title: "Alert me if spending is high", desc: "Send a notification when AI usage costs exceed my budget." },
  { id: "a4", icon: "📊", title: "Weekly activity report", desc: "Create a report of what I accomplished this week." },
  { id: "a5", icon: "🔔", title: "Remind me to follow up", desc: "Check for conversations that need a response and remind me." },
  { id: "a6", icon: "🧹", title: "Clean up old conversations", desc: "Archive conversations that haven't been active in a while." },
  { id: "a7", icon: "📝", title: "Prepare meeting notes", desc: "Draft an agenda and notes template for my upcoming meetings." },
  { id: "a8", icon: "🔍", title: "Monitor a topic", desc: "Watch for mentions of a topic I care about and notify me." },
  { id: "a9", icon: "📈", title: "Track my progress", desc: "Summarize how my tasks and goals are progressing." },
];

const SCHED_DAYS = ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"];

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
  const [showCreate, setShowCreate] = useState(false);
  const [selectedTemplate, setSelectedTemplate] = useState<string | null>(null);
  const [schedDays, setSchedDays] = useState<string[]>(["Mo", "Tu", "We", "Th", "Fr"]);
  const [draftName, setDraftName] = useState("");
  const [draftPrompt, setDraftPrompt] = useState("");
  const [runningId, setRunningId] = useState<string | null>(null);
  const [designerOpen, setDesignerOpen] = useState(false);
  const [editingPipeline, setEditingPipeline] = useState<PipelineDescriptor | null>(null);
  const [dagPreview, setDagPreview] = useState<PipelineDescriptor | null>(null);
  const [executionEvents, setExecutionEvents] = useState<Map<number, PipelineStepEvent>>(new Map());

  const openCreate = useCallback((template?: typeof AUTOMATION_TEMPLATES[number]) => {
    if (template) {
      setDraftName(template.title);
      setDraftPrompt(template.desc);
      setSelectedTemplate(template.id);
    } else {
      setDraftName("");
      setDraftPrompt("");
      setSelectedTemplate(null);
    }
    setShowCreate(true);
  }, []);

  const openDesignerFromTemplate = useCallback((template: typeof AUTOMATION_TEMPLATES[number]) => {
    // Create a preconfigured pipeline descriptor so the designer loads with a prompt step
    const prebuilt: PipelineDescriptor = {
      id: "",
      name: template.title,
      description: template.desc,
      steps: [
        { label: "Input", node_type: "input", model: null, agent_id: null, x: 0, y: 0 },
        { label: template.title, node_type: "agent", model: "sonnet", agent_id: null, x: 200, y: 0 },
        { label: "Output", node_type: "output", model: null, agent_id: null, x: 400, y: 0 },
      ],
      edges: [[0, 1], [1, 2]],
      created: "",
    };
    setEditingPipeline(prebuilt);
    setDesignerOpen(true);
  }, []);

  const createAutomation = useCallback(async () => {
    if (!draftName.trim()) {
      pushToast("Automation name is required.");
      return;
    }
    try {
      const steps: PipelineNodeDescriptor[] = [
        { label: "Input", node_type: "input", model: null, agent_id: null, x: 0, y: 0 },
        { label: draftName, node_type: "agent", model: "sonnet", agent_id: null, x: 200, y: 0 },
        { label: "Output", node_type: "output", model: null, agent_id: null, x: 400, y: 0 },
      ];
      await api.createPipeline(draftName, draftPrompt || draftName, steps, [[0, 1], [1, 2]]);
      pushToast(`Automation "${draftName}" created.`);
      setShowCreate(false);
      onRefreshPipelines();
    } catch {
      pushToast("Failed to create automation.");
    }
  }, [draftName, draftPrompt, pushToast, onRefreshPipelines]);

  const runPipeline = useCallback(async (pipelineId: string) => {
    setRunningId(pipelineId);
    // Mark all steps as started for execution overlay (T8)
    const pip = pipelines.find((p) => p.id === pipelineId);
    if (pip) {
      setDagPreview(pip);
      const evMap = new Map<number, PipelineStepEvent>();
      pip.steps.forEach((_s, i) => {
        evMap.set(i, { pipeline_id: pipelineId, step_index: i, status: "started", timestamp: new Date().toISOString() });
      });
      setExecutionEvents(evMap);
    }
    try {
      const result = await api.runPipeline(pipelineId);
      // Build step-level status for the execution overlay
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
          pushToast(`Pipeline finished — ${failedLabels.join(", ")} failed.`);
        } else if (skippedCount > 0) {
          pushToast(`Pipeline completed (${skippedCount} step${skippedCount > 1 ? "s" : ""} skipped by gate).`);
        } else {
          pushToast("Pipeline completed successfully.");
        }
      } else {
        pushToast("Pipeline run completed.");
      }
    } catch {
      pushToast("Pipeline run failed.");
      // Mark all as failed
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

  return (
    <>
    <PageLayout
      title="Automations"
      subtitle="Set up routines that run on a schedule or with one click."
      actions={
        <button className="btn subtle" style={{ whiteSpace: "nowrap" }} onClick={() => { setEditingPipeline(null); setDesignerOpen(true); }}>
          + New Automation
        </button>
      }
      className="page-automations"
    >

      {/* Quick create from natural language */}
      {agents.length > 0 && (
        <section className="section-card">
          <div className="quick-create-row">
            <input
              className="input quick-create-input"
              placeholder="Describe what you want automated, e.g. 'Send me a daily summary of my messages'"
              value={draftPrompt}
              onChange={(e) => setDraftPrompt(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && draftPrompt.trim()) {
                  setDraftName(draftPrompt.trim().slice(0, 60));
                  const prebuilt: PipelineDescriptor = {
                    id: "",
                    name: draftPrompt.trim().slice(0, 60),
                    description: draftPrompt.trim(),
                    steps: [
                      { label: "Input", node_type: "input", model: null, agent_id: null, x: 0, y: 0 },
                      { label: draftPrompt.trim().slice(0, 40), node_type: "agent", model: "sonnet", agent_id: null, x: 200, y: 0 },
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
            <button
              className="btn primary"
              disabled={!draftPrompt.trim()}
              onClick={() => {
                if (draftPrompt.trim()) {
                  setDraftName(draftPrompt.trim().slice(0, 60));
                  const prebuilt: PipelineDescriptor = {
                    id: "",
                    name: draftPrompt.trim().slice(0, 60),
                    description: draftPrompt.trim(),
                    steps: [
                      { label: "Input", node_type: "input", model: null, agent_id: null, x: 0, y: 0 },
                      { label: draftPrompt.trim().slice(0, 40), node_type: "agent", model: "sonnet", agent_id: null, x: 200, y: 0 },
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
            >
              Create
            </button>
          </div>
        </section>
      )}

      {/* Existing pipelines */}
      {pipelines.length > 0 && (
        <section className="section-card">
          <div className="section-head">
            <h2>Active Pipelines ({pipelines.length})</h2>
            <button className="btn subtle" onClick={onRefreshPipelines}>Refresh</button>
          </div>
          <div className="list-rows">
            {pipelines.map((p) => (
              <div key={p.id} className="row-card" style={{ flexDirection: "column" }}>
                <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", width: "100%" }}>
                  <div>
                    <div className="row-title">{p.name}</div>
                    <div className="row-sub">
                      {p.steps.length} steps · Created: {new Date(p.created).toLocaleDateString([], { month: "short", day: "numeric" }) || p.created}
                    </div>
                    {p.description && <div className="row-sub" style={{ marginTop: 2, color: "var(--text-secondary)" }}>{p.description}</div>}
                  </div>
                  <div className="row-actions">
                    <button
                      className="btn subtle"
                      onClick={() => { setEditingPipeline(p); setDesignerOpen(true); }}
                    >
                      Edit
                    </button>
                    <button
                      className="btn subtle"
                      onClick={() => setDagPreview(dagPreview?.id === p.id ? null : p)}
                      title="Show details"
                    >
                      Details
                    </button>
                    <button
                      className="btn primary"
                      disabled={runningId === p.id}
                      onClick={() => runPipeline(p.id)}
                    >
                      {runningId === p.id ? "Running..." : "Run"}
                    </button>
                  </div>
                </div>
                {/* DAG Preview Inline */}
                {dagPreview?.id === p.id && (
                  <div style={{ marginTop: 12, borderTop: "1px solid var(--border)", paddingTop: 12, width: "100%" }}>
                    <DagCanvas
                      pipeline={p}
                      stepEvents={runningId === p.id ? executionEvents : undefined}
                      width={680}
                      height={Math.max(200, p.steps.length * 60)}
                    />
                  </div>
                )}
              </div>
            ))}
          </div>
        </section>
      )}

      {/* Template grid */}
      {agents.length === 0 ? (
        <section className="section-card">
          <div className="empty-state-action" style={{ padding: 32, textAlign: "center" }}>
            <p style={{ fontSize: 16, marginBottom: 8 }}>Create an agent first to power your automations.</p>
            <p className="settings-desc" style={{ marginBottom: 16 }}>Automations use your agents to run pipeline steps. Set up an agent in Settings, then come back here.</p>
            <button className="btn primary" onClick={() => {
              window.localStorage.setItem("clawdesk._settingsTab", "Agents");
              onNavigate("settings");
            }}>
              Go to Settings → Agents
            </button>
          </div>
        </section>
      ) : (
      <section className="section-card">
        <div className="section-head">
          <h2>Popular templates</h2>
          <p className="section-desc">Click any template to customize and set it up.</p>
        </div>
        <div className="automation-template-grid">
          {AUTOMATION_TEMPLATES.map((a) => (
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

      {/* Create Modal */}
      {showCreate && (
        <Modal
          title="Create automation"
          onClose={() => setShowCreate(false)}
        >
          <div className="modal-stack">
            <div className="info-banner">
              ℹ️ Automations run with your default sandbox settings. Tool calls will fail
              if they require modifying files outside the workspace.
            </div>

            <label className="field-label">
              Name
              <input
                className="input"
                placeholder="Check for sentry issues"
                value={draftName}
                onChange={(e) => setDraftName(e.target.value)}
              />
            </label>

            <label className="field-label">
              Prompt
              <textarea
                className="input"
                placeholder="Describe what this automation should do..."
                rows={3}
                value={draftPrompt}
                onChange={(e) => setDraftPrompt(e.target.value)}
              />
            </label>

            <div>
              <label className="field-label">Schedule</label>
              <div className="schedule-days">
                {SCHED_DAYS.map((d) => {
                  const active = schedDays.includes(d);
                  return (
                    <button
                      key={d}
                      className={`schedule-day ${active ? "active" : ""}`}
                      onClick={() =>
                        setSchedDays((prev) =>
                          active ? prev.filter((x) => x !== d) : [...prev, d]
                        )
                      }
                    >
                      {d}
                    </button>
                  );
                })}
              </div>
            </div>

            <div className="row-actions" style={{ justifyContent: "flex-end" }}>
              <button className="btn ghost" onClick={() => setShowCreate(false)}>Cancel</button>
              <button className="btn primary" onClick={createAutomation}>Create</button>
            </div>
          </div>
        </Modal>
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
