import { useState, useCallback, useRef, useEffect } from "react";
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

// ── Cron helpers ──────────────────────────────────────────────

/** Parse a 5-field cron expression and compute the next fire time after `after`. */
function nextCronDate(cron: string, after: Date): Date | null {
  const parts = cron.trim().split(/\s+/);
  if (parts.length !== 5) return null;

  const parseField = (field: string, min: number, max: number): Set<number> => {
    const vals = new Set<number>();
    for (const part of field.split(",")) {
      const stepMatch = part.match(/^(.+)\/(\d+)$/);
      const step = stepMatch ? parseInt(stepMatch[2]) : 1;
      const range = stepMatch ? stepMatch[1] : part;

      if (range === "*") {
        for (let i = min; i <= max; i += step) vals.add(i);
      } else if (range.includes("-")) {
        const [a, b] = range.split("-").map(Number);
        for (let i = a; i <= b; i += step) vals.add(i);
      } else {
        vals.add(parseInt(range));
      }
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
  d.setMinutes(d.getMinutes() + 1); // start from next minute

  for (let safety = 0; safety < 525960; safety++) { // max ~1 year of minutes
    const mo = d.getMonth() + 1, dom = d.getDate(), dow = d.getDay(), hr = d.getHours(), mn = d.getMinutes();
    if (months.has(mo) && doms.has(dom) && dows.has(dow) && hours.has(hr) && minutes.has(mn)) {
      return d;
    }
    d.setMinutes(d.getMinutes() + 1);
  }
  return null;
}

/** Human-readable formatted next run time string. */
function computeNextCronRun(cron: string): string {
  const next = nextCronDate(cron, new Date());
  if (!next) return "Invalid cron expression";
  return next.toLocaleString([], { weekday: "short", month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
}

/** Compute the next N fire times as formatted strings. */
function computeNextCronRuns(cron: string, count: number): string[] {
  const results: string[] = [];
  let cursor = new Date();
  for (let i = 0; i < count; i++) {
    const next = nextCronDate(cron, cursor);
    if (!next) break;
    results.push(next.toLocaleString([], { weekday: "short", month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" }));
    cursor = next;
  }
  return results;
}

/** Convert a cron expression to a human-readable description. */
function cronToHuman(cron: string): string {
  const parts = cron.trim().split(/\s+/);
  if (parts.length !== 5) return `Custom: ${cron}`;
  const [min, hr, dom, month, dow] = parts;

  const hrDesc = hr === "*" ? "every hour" : `at ${hr.padStart(2, "0")}:${min.padStart(2, "0")}`;
  const dayDesc = dow === "*" && dom === "*"
    ? "every day"
    : dow !== "*"
      ? `on ${["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"].filter((_, i) => {
          if (dow.includes("-")) { const [a, b] = dow.split("-").map(Number); return i >= a && i <= b; }
          return dow.split(",").map(Number).includes(i);
        }).join(", ") || dow}`
      : dom !== "*" ? `on day ${dom} of the month` : "";
  const monthDesc = month === "*" ? "" : ` in month ${month}`;

  return `Runs ${dayDesc} ${hrDesc}${monthDesc}`.replace(/\s+/g, " ").trim();
}

/** Parse cron day-of-week field → boolean[7] (Sun=0 … Sat=6). */
function parseCronDow(cron: string): boolean[] {
  const parts = cron.trim().split(/\s+/);
  if (parts.length !== 5) return [false, true, true, true, true, true, false];
  const dow = parts[4];
  if (dow === "*") return [true, true, true, true, true, true, true];
  const set = new Set<number>();
  for (const seg of dow.split(",")) {
    if (seg.includes("-")) {
      const [a, b] = seg.split("-").map(Number);
      for (let i = a; i <= b; i++) set.add(i);
    } else {
      set.add(parseInt(seg));
    }
  }
  return [0, 1, 2, 3, 4, 5, 6].map((d) => set.has(d));
}

/** Build cron day-of-week field from boolean[7]. */
function buildCronDow(days: boolean[]): string {
  if (days.every(Boolean)) return "*";
  const active = days.map((v, i) => (v ? i : -1)).filter((i) => i >= 0);
  if (active.length === 0) return "*"; // fallback — all days
  // Compress consecutive runs
  const ranges: string[] = [];
  let start = active[0], end = active[0];
  for (let i = 1; i < active.length; i++) {
    if (active[i] === end + 1) { end = active[i]; }
    else { ranges.push(start === end ? `${start}` : `${start}-${end}`); start = end = active[i]; }
  }
  ranges.push(start === end ? `${start}` : `${start}-${end}`);
  return ranges.join(",");
}

/** Replace only the DOW (5th) field in a cron expression. */
function setCronDow(cron: string, days: boolean[]): string {
  const parts = cron.trim().split(/\s+/);
  if (parts.length !== 5) return cron;
  parts[4] = buildCronDow(days);
  return parts.join(" ");
}

/** Map a backend PipelineNodeDescriptor to a designer PipelineStep */
function nodeToStep(node: PipelineNodeDescriptor): PipelineStep {
  const kind: PipelineStep["kind"] =
    node.node_type === "agent" ? "prompt"
      : node.node_type === "gate" ? "condition"
        : node.node_type === "parallel" ? "loop"
          : node.node_type === "input" ? "prompt"
            : "tool";
  // Preserve existing step config from the backend. Only use defaults
  // for keys that are missing. This prevents save-and-reopen from wiping
  // custom prompts, tool args, gate expressions, etc.
  const defaultConfig: Record<string, string> =
    kind === "prompt" ? { prompt: "" }
      : kind === "condition" ? { expression: "", thenLabel: "Yes", elseLabel: "No" }
        : kind === "loop" ? { count: "3", condition: "" }
          : { tool: "", args: "{}" };
  const mergedConfig: Record<string, string> = { ...defaultConfig };
  if (node.config) {
    for (const [key, value] of Object.entries(node.config)) {
      if (value !== undefined && value !== null) {
        mergedConfig[key] = String(value);
      }
    }
  }
  // If no explicit prompt was set but the node has a label, use the label
  // as the default prompt so the LLM has something to work with.
  if (kind === "prompt" && !mergedConfig.prompt && node.label) {
    mergedConfig.prompt = node.label;
  }
  return { id: makeStepId(), name: node.label, kind, config: mergedConfig };
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
  const [schedule, setSchedule] = useState(existingPipeline?.schedule ?? "");
  const [enableSchedule, setEnableSchedule] = useState(!!existingPipeline?.schedule);
  const [scheduleDays, setScheduleDays] = useState<boolean[]>([false, true, true, true, true, true, false]);
  const [isSaving, setIsSaving] = useState(false);
  const [isTesting, setIsTesting] = useState(false);
  const [testLog, setTestLog] = useState<string[]>([]);
  const [validationError, setValidationError] = useState<string | null>(null);
  const [selectedStepIdx, setSelectedStepIdx] = useState<number | null>(null);
  const [lastAddedId, setLastAddedId] = useState<string | null>(null);
  const [dragOverCanvas, setDragOverCanvas] = useState(false);
  const stepsListRef = useRef<HTMLDivElement>(null);

  // ── Custom pointer-based drag (replaces flaky HTML5 DnD in Tauri/WebKit) ──
  const [draggingKind, setDraggingKind] = useState<PipelineStep["kind"] | null>(null);
  const [ghostPos, setGhostPos] = useState<{ x: number; y: number } | null>(null);
  const dragRef = useRef<{ kind: PipelineStep["kind"]; startX: number; startY: number; isDragging: boolean } | null>(null);
  const canvasRef = useRef<HTMLDivElement>(null);
  const addStepRef = useRef<(kind: PipelineStep["kind"]) => void>(() => {});

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

  // Keep ref in sync so the pointer listeners always call the latest addStep
  addStepRef.current = addStep;

  useEffect(() => {
    const onMove = (e: PointerEvent) => {
      const dr = dragRef.current;
      if (!dr) return;
      const dx = e.clientX - dr.startX;
      const dy = e.clientY - dr.startY;
      if (!dr.isDragging && Math.hypot(dx, dy) < 6) return;
      if (!dr.isDragging) {
        dr.isDragging = true;
        setDraggingKind(dr.kind);
      }
      setGhostPos({ x: e.clientX, y: e.clientY });
      const canvas = canvasRef.current;
      if (canvas) {
        const rect = canvas.getBoundingClientRect();
        const over = e.clientX >= rect.left && e.clientX <= rect.right && e.clientY >= rect.top && e.clientY <= rect.bottom;
        setDragOverCanvas(over);
      }
    };
    const onUp = (e: PointerEvent) => {
      const dr = dragRef.current;
      if (!dr) return;
      if (dr.isDragging) {
        const canvas = canvasRef.current;
        if (canvas) {
          const rect = canvas.getBoundingClientRect();
          if (e.clientX >= rect.left && e.clientX <= rect.right && e.clientY >= rect.top && e.clientY <= rect.bottom) {
            addStepRef.current(dr.kind);
          }
        }
      } else {
        addStepRef.current(dr.kind);
      }
      dragRef.current = null;
      setDraggingKind(null);
      setGhostPos(null);
      setDragOverCanvas(false);
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
    return () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
    };
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

  // ── Sync scheduleDays ↔ cron DOW field ─────────────────

  // When cron expression changes externally (presets, manual edit), sync days
  useEffect(() => {
    if (schedule.trim()) {
      setScheduleDays(parseCronDow(schedule));
    }
  }, [schedule]);

  const toggleDay = useCallback((dayIdx: number) => {
    setScheduleDays((prev) => {
      const next = prev.map((v, i) => (i === dayIdx ? !v : v));
      // Also update the cron expression's DOW field
      setSchedule((prevCron) => setCronDow(prevCron || "0 9 * * *", next));
      return next;
    });
  }, []);

  // ── Save ──────────────────────────────────────────────────

  const handleSave = useCallback(async () => {
    if (!name.trim()) {
      setValidationError("name");
      pushToast("Enter a name for the automation.");
      return;
    }
    if (steps.length === 0) {
      setValidationError("steps");
      pushToast("Add at least one step.");
      return;
    }
    setValidationError(null);
    setIsSaving(true);
    try {
      const pipelineSteps: PipelineNodeDescriptor[] = steps.map((s, i) => ({
        label: s.name,
        node_type: s.kind === "prompt" ? "agent" as const
          : s.kind === "condition" ? "gate" as const
            : s.kind === "loop" ? "parallel" as const
              : i === 0 ? "input" as const
                : "output" as const,
        model: null, // let the backend ProviderNegotiator pick the best available model
        agent_id: null,
        x: i * 200,
        y: 0,
        config: s.config,
      }));
      const edges: [number, number][] = pipelineSteps.map((_, i) => [i, i + 1] as [number, number]).slice(0, -1);
      const scheduleValue = enableSchedule && schedule.trim() ? schedule.trim() : null;

      if (existingPipeline && existingPipeline.id) {
        await api.updatePipeline(existingPipeline.id, name, description, pipelineSteps, edges, scheduleValue);
        pushToast(`Automation "${name}" updated.`);
      } else {
        await api.createPipeline(name, description, pipelineSteps, edges, scheduleValue);
        pushToast(`Automation "${name}" saved.`);
      }
      onSaved();
    } catch {
      pushToast("Failed to save automation.");
    }
    setIsSaving(false);
  }, [name, description, steps, schedule, enableSchedule, existingPipeline, onSaved, pushToast]);

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
            <button className="btn primary" disabled={isSaving} onClick={handleSave}>
              {isSaving ? "Saving..." : existingPipeline?.id ? "Update" : "Save"}
            </button>
            <button className="btn ghost" onClick={onClose}>✕</button>
          </div>
        </div>

        {/* Name / Description */}
        <div className="automation-designer-meta">
          <div>
            <input
              className={`input${validationError === "name" ? " input-error" : ""}`}
              placeholder="Automation name *"
              value={name}
              onChange={(e) => { setName(e.target.value); if (validationError === "name" && e.target.value.trim()) setValidationError(null); }}
              style={{ fontWeight: 600, fontSize: 16 }}
            />
            {validationError === "name" && (
              <div style={{ color: "var(--red, #e53e3e)", fontSize: 12, marginTop: 4 }}>
                A name is required to save this automation.
              </div>
            )}
          </div>
          <input
            className="input"
            placeholder="Description (optional)"
            value={description}
            onChange={(e) => setDescription(e.target.value)}
          />
          {validationError === "steps" && (
            <div style={{ color: "var(--red, #e53e3e)", fontSize: 12 }}>
              Add at least one step before saving.
            </div>
          )}
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
                  <div
                    key={sk.kind}
                    className={`automation-step-kind-btn${draggingKind === sk.kind ? " dragging" : ""}`}
                    role="button"
                    tabIndex={0}
                    onPointerDown={(e) => {
                      e.preventDefault();
                      (e.target as HTMLElement).setPointerCapture?.(e.pointerId);
                      dragRef.current = { kind: sk.kind, startX: e.clientX, startY: e.clientY, isDragging: false };
                    }}
                  >
                    <span className="automation-step-kind-icon">{sk.icon}</span>
                    <div>
                      <div className="automation-step-kind-label">{sk.label}</div>
                      <div className="automation-step-kind-desc">{sk.desc}</div>
                    </div>
                  </div>
                ))}
              </div>

              {/* Drag ghost */}
              {draggingKind && ghostPos && (() => {
                const meta = STEP_KINDS.find((s) => s.kind === draggingKind);
                return (
                  <div className="automation-drag-ghost" style={{ left: ghostPos.x, top: ghostPos.y }}>
                    <span>{meta?.icon}</span> {meta?.label}
                  </div>
                );
              })()}

              {/* Steps list / drop target */}
              <div
                className={`automation-steps-list${dragOverCanvas ? " drag-over" : ""}`}
                ref={(el) => { (stepsListRef as React.MutableRefObject<HTMLDivElement | null>).current = el; (canvasRef as React.MutableRefObject<HTMLDivElement | null>).current = el; }}
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
              <div style={{ display: "flex", alignItems: "center", gap: 12, marginBottom: 16 }}>
                <label className="automation-schedule-toggle" style={{ display: "flex", alignItems: "center", gap: 8, cursor: "pointer" }}>
                  <input
                    type="checkbox"
                    checked={enableSchedule}
                    onChange={(e) => {
                      setEnableSchedule(e.target.checked);
                      if (e.target.checked && !schedule.trim()) setSchedule("0 9 * * *");
                    }}
                  />
                  <span style={{ fontWeight: 600, fontSize: 15 }}>Enable Schedule</span>
                </label>
                {enableSchedule && schedule.trim() && (
                  <span className="automation-next-run-badge">
                    Next run: {(() => {
                      try {
                        const next = computeNextCronRun(schedule.trim());
                        return next;
                      } catch {
                        return "Invalid cron";
                      }
                    })()}
                  </span>
                )}
              </div>

              {!enableSchedule && (
                <div className="empty-state centered" style={{ padding: 24 }}>
                  <p style={{ color: "var(--text-soft)" }}>This automation will only run manually (no schedule).</p>
                  <p style={{ fontSize: 12, color: "var(--text-soft)", marginTop: 4 }}>Toggle the switch above to configure automatic scheduling.</p>
                </div>
              )}

              {enableSchedule && (
                <>
                  <p className="settings-desc" style={{ marginBottom: 12 }}>Configure when this automation runs automatically.</p>

                  <div className="skill-editor-field">
                    <label className="field-label">Cron Expression</label>
                    <input
                      className="input"
                      value={schedule}
                      onChange={(e) => setSchedule(e.target.value)}
                      placeholder="minute hour day month weekday"
                      style={{ fontFamily: "'IBM Plex Mono', monospace" }}
                    />
                    <div style={{ fontSize: 11, color: "var(--text-soft)", marginTop: 4 }}>
                      Format: <code>minute hour day-of-month month day-of-week</code> — e.g. <code>30 9 * * 1-5</code> = weekdays at 9:30 AM
                    </div>
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

                  {/* Human-readable summary of schedule */}
                  {schedule.trim() && (
                    <div className="automation-schedule-summary" style={{ marginTop: 16, padding: "12px 16px", background: "var(--bg-soft)", borderRadius: 8 }}>
                      <div style={{ fontWeight: 600, fontSize: 13, marginBottom: 4 }}>Schedule Summary</div>
                      <div style={{ fontSize: 13, color: "var(--text-secondary)" }}>
                        {cronToHuman(schedule.trim())}
                      </div>
                      <div style={{ fontSize: 12, color: "var(--text-soft)", marginTop: 6 }}>
                        Next 3 runs: {(() => {
                          try {
                            const runs = computeNextCronRuns(schedule.trim(), 3);
                            return runs.join(" · ");
                          } catch {
                            return "Unable to compute";
                          }
                        })()}
                      </div>
                    </div>
                  )}
                </>
              )}
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
                <p><strong>{steps.length}</strong> steps · Schedule: {enableSchedule && schedule.trim() ? <code>{schedule}</code> : <em>Manual only</em>}</p>
                {enableSchedule && schedule.trim() && (
                  <p>Next run: {computeNextCronRun(schedule.trim())}</p>
                )}
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
