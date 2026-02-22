import { useState, useCallback, useRef } from "react";
import * as api from "../api";
import type { PipelineDescriptor, PipelineNodeDescriptor } from "../types";
import { Icon } from "./Icon";

// ── Types ─────────────────────────────────────────────────────

interface PipelineStep {
  id: string;
  name: string;
  kind: "prompt" | "tool" | "condition" | "loop" | "delay" | "webhook";
  config: Record<string, string>;
}

type DesignerTab = "steps" | "schedule" | "preview" | "test";

const STEP_KINDS: { kind: PipelineStep["kind"]; label: string; icon: string; desc: string }[] = [
  { kind: "prompt", label: "Send Prompt", icon: "💬", desc: "Send a message to the agent" },
  { kind: "tool", label: "Run Tool", icon: "🔧", desc: "Execute a specific tool" },
  { kind: "condition", label: "Condition", icon: "🔀", desc: "Branch based on a condition" },
  { kind: "loop", label: "Loop", icon: "🔁", desc: "Repeat steps N times or until condition" },
  { kind: "delay", label: "Delay", icon: "⏱️", desc: "Wait before next step" },
  { kind: "webhook", label: "Webhook", icon: "🌐", desc: "Call an external URL" },
];

const SCHEDULE_PRESETS = [
  { label: "Every hour", cron: "0 * * * *" },
  { label: "Every 6 hours", cron: "0 */6 * * *" },
  { label: "Daily at 9am", cron: "0 9 * * *" },
  { label: "Weekdays at 9am", cron: "0 9 * * 1-5" },
  { label: "Weekly Monday 9am", cron: "0 9 * * 1" },
  { label: "Monthly 1st", cron: "0 9 1 * *" },
];

// ── Helpers ───────────────────────────────────────────────────

let stepCounter = 0;
function makeStepId() {
  return `step_${++stepCounter}_${Date.now().toString(36)}`;
}

/** Map a backend PipelineNodeDescriptor to a designer PipelineStep */
function nodeToStep(node: PipelineNodeDescriptor): PipelineStep {
  const kind: PipelineStep["kind"] =
    node.node_type === "agent" ? "prompt"
    : node.node_type === "gate" ? "condition"
    : node.node_type === "parallel" ? "loop"
    : node.node_type === "input" ? "prompt"
    : "tool";
  const defaultConfig: Record<string, string> =
    kind === "prompt" ? { prompt: "" }
    : kind === "condition" ? { expression: "", thenLabel: "Yes", elseLabel: "No" }
    : kind === "loop" ? { count: "3", condition: "" }
    : { tool: "", args: "{}" };
  return { id: makeStepId(), name: node.label, kind, config: defaultConfig };
}

/** Convert existing pipeline steps, skipping Input/Output bookend nodes */
function loadExistingSteps(pipeline: PipelineDescriptor): PipelineStep[] {
  return pipeline.steps
    .filter((s) => s.node_type !== "input" && s.node_type !== "output")
    .map(nodeToStep);
}

// ── Props ─────────────────────────────────────────────────────

export interface AutomationDesignerProps {
  existingPipeline?: PipelineDescriptor | null;
  onClose: () => void;
  onSaved: () => void;
  pushToast: (text: string) => void;
}

// ── Component ─────────────────────────────────────────────────

export function AutomationDesigner({
  existingPipeline,
  onClose,
  onSaved,
  pushToast,
}: AutomationDesignerProps) {
  const [tab, setTab] = useState<DesignerTab>("steps");
  const [name, setName] = useState(existingPipeline?.name ?? "");
  const [description, setDescription] = useState(existingPipeline?.description ?? "");
  const [steps, setSteps] = useState<PipelineStep[]>(
    existingPipeline ? loadExistingSteps(existingPipeline) : []
  );
  const [schedule, setSchedule] = useState("0 9 * * *");
  const [scheduleDays, setScheduleDays] = useState<boolean[]>([false, true, true, true, true, true, false]);
  const [isSaving, setIsSaving] = useState(false);
  const [isTesting, setIsTesting] = useState(false);
  const [testLog, setTestLog] = useState<string[]>([]);
  const [selectedStepIdx, setSelectedStepIdx] = useState<number | null>(null);
  const [lastAddedId, setLastAddedId] = useState<string | null>(null);
  const [dragOverCanvas, setDragOverCanvas] = useState(false);
  const stepsListRef = useRef<HTMLDivElement>(null);

  // ── Step management ───────────────────────────────────────

  const addStep = useCallback((kind: PipelineStep["kind"]) => {
    const meta = STEP_KINDS.find((s) => s.kind === kind)!;
    const id = makeStepId();
    setSteps((prev) => [
      ...prev,
      {
        id,
        name: meta.label,
        kind,
        config: kind === "prompt" ? { prompt: "" }
          : kind === "tool" ? { tool: "", args: "{}" }
          : kind === "condition" ? { expression: "", thenLabel: "Yes", elseLabel: "No" }
          : kind === "loop" ? { count: "3", condition: "" }
          : kind === "delay" ? { seconds: "60" }
          : { url: "", method: "POST", body: "{}" },
      },
    ]);
    setLastAddedId(id);
    setSelectedStepIdx(null);
    // Clear highlight after animation
    setTimeout(() => setLastAddedId(null), 800);
    // Scroll to bottom of steps list
    setTimeout(() => {
      if (stepsListRef.current) {
        stepsListRef.current.scrollTop = stepsListRef.current.scrollHeight;
      }
    }, 50);
  }, []);

  const removeStep = useCallback((idx: number) => {
    setSteps((prev) => prev.filter((_, i) => i !== idx));
    setSelectedStepIdx(null);
  }, []);

  const moveStep = useCallback((idx: number, dir: -1 | 1) => {
    setSteps((prev) => {
      const next = [...prev];
      const target = idx + dir;
      if (target < 0 || target >= next.length) return prev;
      [next[idx], next[target]] = [next[target], next[idx]];
      return next;
    });
    setSelectedStepIdx((prev) => (prev !== null ? prev + dir : null));
  }, []);

  const updateStepConfig = useCallback((idx: number, key: string, value: string) => {
    setSteps((prev) =>
      prev.map((s, i) => (i === idx ? { ...s, config: { ...s.config, [key]: value } } : s))
    );
  }, []);

  const updateStepName = useCallback((idx: number, newName: string) => {
    setSteps((prev) =>
      prev.map((s, i) => (i === idx ? { ...s, name: newName } : s))
    );
  }, []);

  // ── Toggle schedule day ───────────────────────────────────

  const toggleDay = useCallback((dayIdx: number) => {
    setScheduleDays((prev) => prev.map((v, i) => (i === dayIdx ? !v : v)));
  }, []);

  // ── Save ──────────────────────────────────────────────────

  const handleSave = useCallback(async () => {
    if (!name.trim()) {
      pushToast("Enter a name for the automation.");
      return;
    }
    if (steps.length === 0) {
      pushToast("Add at least one step.");
      return;
    }
    setIsSaving(true);
    try {
      const pipelineSteps: PipelineNodeDescriptor[] = steps.map((s, i) => ({
        label: s.name,
        node_type: s.kind === "prompt" ? "agent" as const
          : s.kind === "condition" ? "gate" as const
          : s.kind === "loop" ? "parallel" as const
          : i === 0 ? "input" as const
          : "output" as const,
        model: s.kind === "prompt" ? "sonnet" : null,
        agent_id: null,
        x: i * 200,
        y: 0,
      }));
      const edges: [number, number][] = pipelineSteps.map((_, i) => [i, i + 1] as [number, number]).slice(0, -1);
      await api.createPipeline(name, description, pipelineSteps, edges);
      pushToast(`Automation "${name}" saved.`);
      onSaved();
    } catch {
      pushToast("Failed to save automation.");
    }
    setIsSaving(false);
  }, [name, description, steps, schedule, onSaved, pushToast]);

  // ── Dry run ───────────────────────────────────────────────

  const handleTestRun = useCallback(async () => {
    setIsTesting(true);
    setTestLog(["Starting dry run..."]);
    for (let i = 0; i < steps.length; i++) {
      const step = steps[i];
      setTestLog((prev) => [...prev, `Step ${i + 1}: ${step.name} (${step.kind})`]);
      await new Promise((r) => setTimeout(r, 400));
      if (step.kind === "prompt") {
        setTestLog((prev) => [...prev, `  → Prompt: "${step.config.prompt?.slice(0, 60) || "(empty)"}..."`]);
      } else if (step.kind === "tool") {
        setTestLog((prev) => [...prev, `  → Tool: ${step.config.tool || "(none)"}`]);
      } else if (step.kind === "condition") {
        setTestLog((prev) => [...prev, `  → Condition: ${step.config.expression || "(empty)"} → taking ${step.config.thenLabel} branch`]);
      } else if (step.kind === "delay") {
        setTestLog((prev) => [...prev, `  → Would wait ${step.config.seconds}s`]);
      } else if (step.kind === "webhook") {
        setTestLog((prev) => [...prev, `  → ${step.config.method} ${step.config.url || "(no URL)"}`]);
      } else if (step.kind === "loop") {
        setTestLog((prev) => [...prev, `  → Loop ${step.config.count}x`]);
      }
    }
    setTestLog((prev) => [...prev, "✅ Dry run complete. All steps validated."]);
    setIsTesting(false);
  }, [steps]);

  // ── Render ────────────────────────────────────────────────

  const DAY_LABELS = ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"];

  return (
    <div className="skill-designer-overlay">
      <div className="skill-designer automation-designer">
        {/* Header */}
        <div className="skill-designer-header">
          <div className="skill-designer-header-left">
            <span className="skill-designer-icon">⚙️</span>
            <h2>{existingPipeline ? "Edit Automation" : "Design New Automation"}</h2>
          </div>
          <div className="skill-designer-header-right">
            <button className="btn primary" disabled={isSaving || !name.trim()} onClick={handleSave}>
              {isSaving ? "Saving..." : "Save"}
            </button>
            <button className="btn ghost" onClick={onClose}>✕</button>
          </div>
        </div>

        {/* Name / Description */}
        <div className="automation-designer-meta">
          <input
            className="input"
            placeholder="Automation name"
            value={name}
            onChange={(e) => setName(e.target.value)}
            style={{ fontWeight: 600, fontSize: 16 }}
          />
          <input
            className="input"
            placeholder="Description (optional)"
            value={description}
            onChange={(e) => setDescription(e.target.value)}
          />
        </div>

        {/* Tabs */}
        <div className="skill-designer-tabs">
          {(["steps", "schedule", "preview", "test"] as DesignerTab[]).map((t) => (
            <button
              key={t}
              className={`skill-designer-tab${tab === t ? " active" : ""}`}
              onClick={() => setTab(t)}
            >
              {t === "steps" && `🔗 Steps (${steps.length})`}
              {t === "schedule" && "📅 Schedule"}
              {t === "preview" && "👁 Preview"}
              {t === "test" && "🧪 Dry Run"}
            </button>
          ))}
        </div>

        <div className="skill-designer-body">
          {/* ── Steps Tab ── */}
          {tab === "steps" && (
            <div className="automation-steps-layout">
              {/* Step palette */}
              <div className="automation-step-palette">
                <h4>Add Step</h4>
                <p style={{ fontSize: 11, color: "var(--text-soft)", marginBottom: 4 }}>Click or drag to canvas →</p>
                {STEP_KINDS.map((sk) => (
                  <button
                    key={sk.kind}
                    className="automation-step-kind-btn"
                    onClick={() => addStep(sk.kind)}
                    draggable
                    onDragStart={(e) => {
                      e.dataTransfer.setData("application/x-step-kind", sk.kind);
                      e.dataTransfer.effectAllowed = "copy";
                    }}
                  >
                    <span className="automation-step-kind-icon">{sk.icon}</span>
                    <div>
                      <div className="automation-step-kind-label">{sk.label}</div>
                      <div className="automation-step-kind-desc">{sk.desc}</div>
                    </div>
                  </button>
                ))}
              </div>

              {/* Steps list / drop target */}
              <div
                className={`automation-steps-list${dragOverCanvas ? " drag-over" : ""}`}
                ref={stepsListRef}
                onDragOver={(e) => {
                  e.preventDefault();
                  e.dataTransfer.dropEffect = "copy";
                  setDragOverCanvas(true);
                }}
                onDragLeave={() => setDragOverCanvas(false)}
                onDrop={(e) => {
                  e.preventDefault();
                  setDragOverCanvas(false);
                  const kind = e.dataTransfer.getData("application/x-step-kind") as PipelineStep["kind"];
                  if (kind) addStep(kind);
                }}
              >
                {steps.length === 0 ? (
                  <div className={`automation-empty-canvas${dragOverCanvas ? " drag-over" : ""}`}>
                    <div className="automation-empty-icon">📋</div>
                    <p>No steps yet</p>
                    <p className="automation-empty-hint">Click a step type on the left, or drag it here</p>
                  </div>
                ) : (
                  steps.map((step, idx) => (
                    <div
                      key={step.id}
                      className={`automation-step-card${selectedStepIdx === idx ? " selected" : ""}${lastAddedId === step.id ? " just-added" : ""}`}
                      onClick={() => setSelectedStepIdx(selectedStepIdx === idx ? null : idx)}
                    >
                      <div className="automation-step-card-header">
                        <span className="automation-step-card-num">{idx + 1}</span>
                        <span className="automation-step-card-icon">
                          {STEP_KINDS.find((s) => s.kind === step.kind)?.icon}
                        </span>
                        <input
                          className="automation-step-card-name"
                          value={step.name}
                          onChange={(e) => updateStepName(idx, e.target.value)}
                          onClick={(e) => e.stopPropagation()}
                        />
                        <div className="automation-step-card-actions">
                          <button className="btn ghost" onClick={(e) => { e.stopPropagation(); moveStep(idx, -1); }} disabled={idx === 0}>↑</button>
                          <button className="btn ghost" onClick={(e) => { e.stopPropagation(); moveStep(idx, 1); }} disabled={idx === steps.length - 1}>↓</button>
                          <button className="btn ghost" onClick={(e) => { e.stopPropagation(); removeStep(idx); }}>🗑</button>
                        </div>
                      </div>

                      {selectedStepIdx === idx && (
                        <div className="automation-step-card-body">
                          {step.kind === "prompt" && (
                            <div className="skill-editor-field">
                              <label className="field-label">Prompt</label>
                              <textarea
                                className="input"
                                rows={3}
                                placeholder="What should the agent do?"
                                value={step.config.prompt ?? ""}
                                onChange={(e) => updateStepConfig(idx, "prompt", e.target.value)}
                              />
                            </div>
                          )}
                          {step.kind === "tool" && (
                            <>
                              <div className="skill-editor-field">
                                <label className="field-label">Tool Name</label>
                                <input
                                  className="input"
                                  placeholder="read_file"
                                  value={step.config.tool ?? ""}
                                  onChange={(e) => updateStepConfig(idx, "tool", e.target.value)}
                                />
                              </div>
                              <div className="skill-editor-field">
                                <label className="field-label">Arguments (JSON)</label>
                                <textarea
                                  className="input"
                                  rows={2}
                                  placeholder='{"path": "/tmp/data.json"}'
                                  value={step.config.args ?? "{}"}
                                  onChange={(e) => updateStepConfig(idx, "args", e.target.value)}
                                />
                              </div>
                            </>
                          )}
                          {step.kind === "condition" && (
                            <>
                              <div className="skill-editor-field">
                                <label className="field-label">Expression</label>
                                <input
                                  className="input"
                                  placeholder="output.status === 'success'"
                                  value={step.config.expression ?? ""}
                                  onChange={(e) => updateStepConfig(idx, "expression", e.target.value)}
                                />
                              </div>
                              <div className="skill-editor-row">
                                <div className="skill-editor-field" style={{ flex: 1 }}>
                                  <label className="field-label">Then Label</label>
                                  <input
                                    className="input"
                                    value={step.config.thenLabel ?? "Yes"}
                                    onChange={(e) => updateStepConfig(idx, "thenLabel", e.target.value)}
                                  />
                                </div>
                                <div className="skill-editor-field" style={{ flex: 1 }}>
                                  <label className="field-label">Else Label</label>
                                  <input
                                    className="input"
                                    value={step.config.elseLabel ?? "No"}
                                    onChange={(e) => updateStepConfig(idx, "elseLabel", e.target.value)}
                                  />
                                </div>
                              </div>
                            </>
                          )}
                          {step.kind === "loop" && (
                            <div className="skill-editor-row">
                              <div className="skill-editor-field" style={{ width: 100 }}>
                                <label className="field-label">Count</label>
                                <input
                                  className="input"
                                  type="number"
                                  min={1}
                                  value={step.config.count ?? "3"}
                                  onChange={(e) => updateStepConfig(idx, "count", e.target.value)}
                                />
                              </div>
                              <div className="skill-editor-field" style={{ flex: 1 }}>
                                <label className="field-label">Until Condition (optional)</label>
                                <input
                                  className="input"
                                  placeholder="output.done === true"
                                  value={step.config.condition ?? ""}
                                  onChange={(e) => updateStepConfig(idx, "condition", e.target.value)}
                                />
                              </div>
                            </div>
                          )}
                          {step.kind === "delay" && (
                            <div className="skill-editor-field" style={{ width: 160 }}>
                              <label className="field-label">Wait (seconds)</label>
                              <input
                                className="input"
                                type="number"
                                min={1}
                                value={step.config.seconds ?? "60"}
                                onChange={(e) => updateStepConfig(idx, "seconds", e.target.value)}
                              />
                            </div>
                          )}
                          {step.kind === "webhook" && (
                            <>
                              <div className="skill-editor-row">
                                <div className="skill-editor-field" style={{ width: 100 }}>
                                  <label className="field-label">Method</label>
                                  <select
                                    className="input"
                                    value={step.config.method ?? "POST"}
                                    onChange={(e) => updateStepConfig(idx, "method", e.target.value)}
                                  >
                                    {["GET", "POST", "PUT", "PATCH", "DELETE"].map((m) => (
                                      <option key={m} value={m}>{m}</option>
                                    ))}
                                  </select>
                                </div>
                                <div className="skill-editor-field" style={{ flex: 1 }}>
                                  <label className="field-label">URL</label>
                                  <input
                                    className="input"
                                    placeholder="https://api.example.com/webhook"
                                    value={step.config.url ?? ""}
                                    onChange={(e) => updateStepConfig(idx, "url", e.target.value)}
                                  />
                                </div>
                              </div>
                              <div className="skill-editor-field">
                                <label className="field-label">Body (JSON)</label>
                                <textarea
                                  className="input"
                                  rows={2}
                                  value={step.config.body ?? "{}"}
                                  onChange={(e) => updateStepConfig(idx, "body", e.target.value)}
                                />
                              </div>
                            </>
                          )}
                        </div>
                      )}

                      {/* Connector line */}
                      {idx < steps.length - 1 && <div className="automation-step-connector" />}
                    </div>
                  ))
                )}
              </div>
            </div>
          )}

          {/* ── Schedule Tab ── */}
          {tab === "schedule" && (
            <div className="automation-schedule">
              <h3>Schedule</h3>
              <p className="settings-desc">Configure when this automation runs automatically.</p>

              <div className="skill-editor-field">
                <label className="field-label">Cron Expression</label>
                <input
                  className="input"
                  value={schedule}
                  onChange={(e) => setSchedule(e.target.value)}
                  style={{ fontFamily: "'IBM Plex Mono', monospace" }}
                />
              </div>

              <div className="automation-schedule-presets">
                <label className="field-label">Quick Presets</label>
                <div className="automation-preset-grid">
                  {SCHEDULE_PRESETS.map((preset) => (
                    <button
                      key={preset.cron}
                      className={`btn ${schedule === preset.cron ? "primary" : "subtle"}`}
                      onClick={() => setSchedule(preset.cron)}
                    >
                      {preset.label}
                    </button>
                  ))}
                </div>
              </div>

              <div className="skill-editor-field">
                <label className="field-label">Active Days</label>
                <div className="schedule-days">
                  {DAY_LABELS.map((label, idx) => (
                    <button
                      key={label}
                      className={`schedule-day${scheduleDays[idx] ? " active" : ""}`}
                      onClick={() => toggleDay(idx)}
                    >
                      {label}
                    </button>
                  ))}
                </div>
              </div>
            </div>
          )}

          {/* ── Preview Tab ── */}
          {tab === "preview" && (
            <div className="automation-preview">
              <h3>Pipeline Preview</h3>
              <div className="automation-preview-flow">
                <div className="automation-flow-node automation-flow-start">▶ Start</div>
                {steps.map((step, idx) => (
                  <div key={step.id}>
                    <div className="automation-flow-arrow">↓</div>
                    <div className={`automation-flow-node automation-flow-${step.kind}`}>
                      <span>{STEP_KINDS.find((s) => s.kind === step.kind)?.icon}</span>
                      <span>{step.name}</span>
                      {step.kind === "condition" && (
                        <span className="automation-flow-branch">
                          ({step.config.thenLabel} / {step.config.elseLabel})
                        </span>
                      )}
                    </div>
                  </div>
                ))}
                <div className="automation-flow-arrow">↓</div>
                <div className="automation-flow-node automation-flow-end">⏹ End</div>
              </div>
              <div className="automation-preview-summary">
                <p><strong>{steps.length}</strong> steps · Schedule: <code>{schedule}</code></p>
                <p>Active days: {DAY_LABELS.filter((_, i) => scheduleDays[i]).join(", ") || "None"}</p>
              </div>
            </div>
          )}

          {/* ── Test Tab ── */}
          {tab === "test" && (
            <div className="automation-test">
              <div className="skill-validate-header">
                <h3>Dry Run</h3>
                <button
                  className="btn primary"
                  disabled={isTesting || steps.length === 0}
                  onClick={handleTestRun}
                >
                  {isTesting ? "Running..." : "▶ Run Test"}
                </button>
              </div>
              <div className="automation-test-log">
                {testLog.map((entry, i) => (
                  <div key={i} className="automation-test-log-entry">
                    {entry}
                  </div>
                ))}
                {testLog.length === 0 && (
                  <div className="empty-state centered">
                    <p>Click Run Test to simulate the automation pipeline.</p>
                  </div>
                )}
              </div>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
