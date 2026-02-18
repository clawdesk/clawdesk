import { useState, useCallback } from "react";
import * as api from "../api";
import type { PipelineDescriptor, PipelineNodeDescriptor } from "../types";
import { Modal } from "../components/Modal";
import { Icon } from "../components/Icon";
import { AutomationDesigner } from "../components/AutomationDesigner";

// ── Templates (from sample.js) ────────────────────────────────

const AUTOMATION_TEMPLATES = [
  { id: "a1", icon: "🐛", title: "Scan recent messages for issues", desc: "Check all channels for unresolved questions (last 24h) and flag for agents." },
  { id: "a2", icon: "📊", title: "Daily cost & usage report", desc: "Summarize today's token usage, model breakdown, and cache hit rate." },
  { id: "a3", icon: "🔒", title: "Security audit sweep", desc: "Run CascadeScanner on all agent personas and verify identity contracts." },
  { id: "a4", icon: "🧠", title: "Memory compaction", desc: "Compact stale memory fragments in SochDB and rebuild HNSW indexes." },
  { id: "a5", icon: "📡", title: "Channel health check", desc: "Ping all connected channels and report latency, errors, and rate limits." },
  { id: "a6", icon: "🎯", title: "Agent performance summary", desc: "Summarize agent delegation patterns, A2A task success rates, and costs." },
  { id: "a7", icon: "🔄", title: "Standup summary", desc: "Summarize yesterday's agent activity across all channels for standup." },
  { id: "a8", icon: "🎮", title: "Build a classic game", desc: "Create a small browser game from a connected repo." },
  { id: "a9", icon: "📈", title: "Suggest skill upgrades", desc: "From recent agent traces, suggest new skills to install or promote." },
  { id: "a10", icon: "🧹", title: "Stale session cleanup", desc: "Archive sessions with no activity for 7+ days." },
  { id: "a11", icon: "⚠️", title: "Rate limit monitor", desc: "Check rate limiter stats across channels and alert on near-limits." },
  { id: "a12", icon: "🪙", title: "Cost alert", desc: "Alert if daily spend exceeds threshold across all providers." },
];

const SCHED_DAYS = ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"];

// ── Props ─────────────────────────────────────────────────────

export interface AutomationsPageProps {
  pipelines: PipelineDescriptor[];
  onRefreshPipelines: () => void;
  pushToast: (text: string) => void;
}

// ── Component ─────────────────────────────────────────────────

export function AutomationsPage({
  pipelines,
  onRefreshPipelines,
  pushToast,
}: AutomationsPageProps) {
  const [showCreate, setShowCreate] = useState(false);
  const [selectedTemplate, setSelectedTemplate] = useState<string | null>(null);
  const [schedDays, setSchedDays] = useState<string[]>(["Mo", "Tu", "We", "Th", "Fr"]);
  const [draftName, setDraftName] = useState("");
  const [draftPrompt, setDraftPrompt] = useState("");
  const [runningId, setRunningId] = useState<string | null>(null);
  const [designerOpen, setDesignerOpen] = useState(false);
  const [editingPipeline, setEditingPipeline] = useState<PipelineDescriptor | null>(null);

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
    try {
      await api.runPipeline(pipelineId);
      pushToast("Pipeline run completed.");
    } catch {
      pushToast("Pipeline run failed.");
    } finally {
      setRunningId(null);
    }
  }, [pushToast]);

  return (
    <div className="view page-automations">
      <div className="page-header">
        <div>
          <h1 className="page-title">
            Automations <span className="chip chip-accent">Beta</span>
          </h1>
          <p className="page-subtitle">
            Automate work by setting up scheduled threads and pipelines.
          </p>
        </div>
        <div className="row-actions">
          <button className="btn accent" onClick={() => { setEditingPipeline(null); setDesignerOpen(true); }}>
            🛠️ Design Automation
          </button>
          <button className="btn primary" onClick={() => openCreate()}>
            <Icon name="safe-on" /> New automation
          </button>
        </div>
      </div>

      {/* Existing pipelines */}
      {pipelines.length > 0 && (
        <section className="section-card">
          <div className="section-head">
            <h2>Active Pipelines ({pipelines.length})</h2>
            <button className="btn subtle" onClick={onRefreshPipelines}>Refresh</button>
          </div>
          <div className="list-rows">
            {pipelines.map((p) => (
              <div key={p.id} className="row-card">
                <div>
                  <div className="row-title">{p.name}</div>
                  <div className="row-sub">
                    {p.steps.length} steps · {p.edges.length} edges · Created: {p.created}
                  </div>
                  {p.description && <div className="row-sub">{p.description}</div>}
                </div>
                <div className="row-actions">
                  <button
                    className="btn subtle"
                    onClick={() => { setEditingPipeline(p); setDesignerOpen(true); }}
                  >
                    Edit
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
            ))}
          </div>
        </section>
      )}

      {/* Template grid */}
      <section className="section-card">
        <div className="section-head">
          <h2>Start with a template</h2>
        </div>
        <div className="automation-template-grid">
          {AUTOMATION_TEMPLATES.map((a) => (
            <button
              key={a.id}
              className="automation-template-card"
              onClick={() => openCreate(a)}
            >
              <span className="automation-template-icon">{a.icon}</span>
              <div className="automation-template-desc">{a.desc}</div>
            </button>
          ))}
        </div>
      </section>

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

      {designerOpen && (
        <AutomationDesigner
          existingPipeline={editingPipeline}
          onClose={() => setDesignerOpen(false)}
          onSaved={() => { setDesignerOpen(false); onRefreshPipelines(); }}
          pushToast={pushToast}
        />
      )}
    </div>
  );
}
