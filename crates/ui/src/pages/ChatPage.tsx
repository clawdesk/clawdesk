import { useState, useRef, useEffect, useCallback, useMemo } from "react";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import * as api from "../api";
import type {
  DesktopAgent,
  ChatMessage,
  SkillDescriptor,
  AgentEventEnvelope,
  AgentEventPayload,
  SessionSummary,
} from "../types";
import { AGENT_TEMPLATES } from "../types";
import { Icon } from "../components/Icon";
import { VoiceInput } from "../components/VoiceInput";
import { XTerminal } from "../components/XTerminal";
import { PreviewPanel } from "../components/PreviewPanel";
import { AgentFlowSelector } from "../components/AgentFlowSelector";
import { PROVIDER_MODELS } from "../onboarding/OnboardingWizard";
import {
  ProviderConfig,
  loadProviders,
  getActiveProvider,
  getActiveProviderId,
} from "../providerConfig";

// ── Local types ───────────────────────────────────────────────

interface ToolCallInfo {
  id: string;
  name: string;
  argsPreview?: string;
  status: "running" | "done" | "error";
  result?: string;
  durationMs?: number;
}

interface TraceLogEntry {
  ts: number;
  type: string;
  summary: string;
  detail?: string;
  level: "info" | "warn" | "error" | "debug";
  /** Tool name if this entry relates to a specific tool */
  toolName?: string;
}

type SidePanelTab = "trace" | "tasks" | "skills";

interface ActiveTask {
  id: string;
  name: string;
  startedAt: number;
  status: "running" | "done" | "error";
  durationMs?: number;
  detail?: string;
}

interface SkillUsage {
  id: string;
  included: boolean;
  reason?: string;
  tokenCost?: number;
}

interface UploadedContextDocument {
  id: string;
  filename: string;
  chunkCount: number;
}

interface ThreadMessage {
  id: string;
  role: "user" | "assistant" | "system";
  text: string;
  thinkingText?: string;
  time: string;
  agent?: string;
  skills?: string[];
  tokens?: number;
  cost?: number;
  duration?: number;
  toolCalls?: ToolCallInfo[];
  isStreaming?: boolean;
  retryStatus?: string;
  askHuman?: { requestId: string; question: string; options: string[]; urgent: boolean; answered: boolean; response?: string; sentToChannels?: string[]; viaChannel?: string };
}

interface Suggestion {
  icon: string;
  text: string;
}

interface ParsedShellExecArgs {
  command: string;
  background: boolean;
  port: number | null;
}

function inferPortFromCommand(command: string): number | null {
  const explicitPatterns = [
    /(?:--port|--listen|--http-port)\s+(\d{2,5})/i,
    /(?:-p|-P)\s+(\d{2,5})\b/,
    /localhost:(\d{2,5})/i,
    /127\.0\.0\.1:(\d{2,5})/i,
    /python\s+-m\s+http\.server\s+(\d{2,5})/i,
  ];

  for (const pattern of explicitPatterns) {
    const match = command.match(pattern);
    if (match) {
      return Number.parseInt(match[1], 10);
    }
  }

  const lower = command.toLowerCase();
  if (lower.includes("next dev")) return 3000;
  if (lower.includes("vite") || lower.includes("npm run dev") || lower.includes("pnpm dev") || lower.includes("yarn dev")) return 5173;
  if (lower.includes("python -m http.server")) return 8000;
  return null;
}

function parseShellExecArgs(rawArgs?: string): ParsedShellExecArgs | null {
  if (!rawArgs) {
    return null;
  }

  try {
    const parsed = JSON.parse(rawArgs);
    const command = typeof parsed.command === "string" ? parsed.command : "";
    if (!command) {
      return null;
    }
    const background = Boolean(parsed.background);
    return {
      command,
      background,
      port: inferPortFromCommand(command),
    };
  } catch {
    return null;
  }
}

function extractPreviewPorts(text: string): number[] {
  const matches = [...text.matchAll(/(?:localhost|127\.0\.0\.1):(\d{2,5})/gi)];
  return matches
    .map((match) => Number.parseInt(match[1], 10))
    .filter((port) => Number.isFinite(port) && port > 0 && port <= 65535);
}

// ── Time grouping helper (inspired by open-webui) ─────────────

function getTimeRange(dateStr: string): string {
  const d = new Date(dateStr);
  const now = new Date();
  const diffMs = now.getTime() - d.getTime();
  const diffDays = Math.floor(diffMs / 86400000);

  if (diffDays === 0) return "Today";
  if (diffDays === 1) return "Yesterday";
  if (diffDays <= 7) return "Previous 7 days";
  if (diffDays <= 30) return "Previous 30 days";
  // Show month name
  return d.toLocaleString("default", { month: "long", year: "numeric" });
}

export interface ChatPageProps {
  agents: DesktopAgent[];
  skills: SkillDescriptor[];
  selectedAgentId: string | null;
  onSelectAgent: (id: string | null) => void;
  onCreateAgent: (template: (typeof AGENT_TEMPLATES)[number]) => void;
  onAgentEvent?: (event: AgentEventEnvelope) => void;
  /** Open the Agent Journey Wizard for creating/editing */
  onOpenJourney?: (agentId?: string) => void;
  pushToast: (text: string) => void;
  showTerminal: boolean;
  setShowTerminal: (s: boolean) => void;
}

// ── Agentic Components ────────────────────────────────────────

function ThinkingBlock({ text, duration }: { text: string; duration?: number }) {
  const [expanded, setExpanded] = useState(false);
  return (
    <div className={`agentic-thought ${expanded ? "expanded" : ""}`}>
      <button className="thought-header" onClick={() => setExpanded(!expanded)}>
        <Icon name="sparkles" className="thought-icon" />
        <span className="thought-label">Thinking Process</span>
        {duration && <span className="thought-duration">{duration}ms</span>}
        <Icon name="chevron-down" className={`thought-chevron ${expanded ? "rotated" : ""}`} />
      </button>
      {expanded && (
        <div className="thought-content">
          <div className="thought-line-marker" />
          <div className="thought-text">{text}</div>
        </div>
      )}
    </div>
  );
}

function ToolBrick({ icon, name, status, result, onTraceClick }: { icon: string; name: string; status: "running" | "done" | "error"; result?: string; onTraceClick?: () => void }) {
  const [expanded, setExpanded] = useState(false);
  return (
    <div className={`agentic-tool ${status}`}>
      <div className="tool-main">
        <div className={`tool-status-icon ${status}`}>
          {status === "running" ? <Icon name="loader" className="spin" /> : status === "done" ? <Icon name="check" /> : <Icon name="alert-circle" />}
        </div>
        <span className="tool-name" onClick={onTraceClick} style={{ cursor: onTraceClick ? "pointer" : undefined }}>{name}</span>
        {result && (
          <span className="tool-chevron-wrap" onClick={() => setExpanded(!expanded)} style={{ cursor: "pointer", display: "flex" }}>
            <Icon name="chevron-down" className={`tool-chevron ${expanded ? "rotated" : ""}`} />
          </span>
        )}
        {onTraceClick && (
          <button className="tool-trace-btn" onClick={onTraceClick} title="View trace logs">
            <Icon name="terminal" />
          </button>
        )}
      </div>
      {expanded && result && (
        <pre className="tool-result">{result}</pre>
      )}
    </div>
  );
}

/* ── Side Panel (right-side tabbed panel: Trace / Tasks / Skills) ── */

function SidePanel({ tab, onTabChange, traceEntries, traceFilter, onTraceFilterChange, activeTasks, skillUsages, isSending, onCancelRun, onClose }: {
  tab: SidePanelTab;
  onTabChange: (t: SidePanelTab) => void;
  traceEntries: TraceLogEntry[];
  traceFilter: string | null;
  onTraceFilterChange: (f: string | null) => void;
  activeTasks: ActiveTask[];
  skillUsages: SkillUsage[];
  isSending: boolean;
  onCancelRun: () => void;
  onClose: () => void;
}) {
  const runningCount = activeTasks.filter((t) => t.status === "running").length;

  return (
    <div className="trace-panel">
      <div className="trace-panel-header">
        <div className="side-panel-tabs">
          <button className={`side-tab ${tab === "trace" ? "active" : ""}`} onClick={() => onTabChange("trace")}>
            <Icon name="terminal" />
            <span>Trace</span>
          </button>
          <button className={`side-tab ${tab === "tasks" ? "active" : ""}`} onClick={() => onTabChange("tasks")}>
            <Icon name="activity" />
            <span>Tasks</span>
            {runningCount > 0 && <span className="side-tab-badge">{runningCount}</span>}
          </button>
          <button className={`side-tab ${tab === "skills" ? "active" : ""}`} onClick={() => onTabChange("skills")}>
            <Icon name="zap" />
            <span>Skills</span>
          </button>
        </div>
        <button className="trace-collapse-btn" onClick={onClose} title="Collapse panel">
          <Icon name="chevron-right" />
          <span>Close</span>
        </button>
      </div>

      {tab === "trace" && (
        <TraceTab entries={traceEntries} filter={traceFilter} onFilterChange={onTraceFilterChange} />
      )}
      {tab === "tasks" && (
        <TasksTab tasks={activeTasks} isSending={isSending} onCancelRun={onCancelRun} />
      )}
      {tab === "skills" && (
        <SkillsTab skillUsages={skillUsages} />
      )}
    </div>
  );
}

/* ── Trace Tab ── */

function TraceTab({ entries, filter, onFilterChange }: {
  entries: TraceLogEntry[];
  filter: string | null;
  onFilterChange: (f: string | null) => void;
}) {
  const bottomRef = useRef<HTMLDivElement>(null);
  const [autoScroll, setAutoScroll] = useState(true);

  useEffect(() => {
    if (autoScroll && bottomRef.current) {
      bottomRef.current.scrollIntoView({ behavior: "smooth" });
    }
  }, [entries.length, autoScroll]);

  const filtered = filter
    ? entries.filter((e) => e.toolName === filter || e.type === filter)
    : entries;

  const toolNames = useMemo(() => {
    const set = new Set<string>();
    for (const e of entries) {
      if (e.toolName) set.add(e.toolName);
    }
    return [...set];
  }, [entries]);

  return (
    <>
      {toolNames.length > 0 && (
        <div className="trace-panel-filters">
          <button
            className={`trace-chip ${filter === null ? "active" : ""}`}
            onClick={() => onFilterChange(null)}
          >All</button>
          {toolNames.map((tn) => (
            <button
              key={tn}
              className={`trace-chip ${filter === tn ? "active" : ""}`}
              onClick={() => onFilterChange(filter === tn ? null : tn)}
            >{tn}</button>
          ))}
        </div>
      )}

      <div className="trace-panel-body" onScroll={(e) => {
        const el = e.currentTarget;
        const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
        if (atBottom !== autoScroll) setAutoScroll(atBottom);
      }}>
        {filtered.length === 0 ? (
          <div className="trace-empty">
            <Icon name="terminal" />
            <span>Waiting for events…</span>
          </div>
        ) : (
          filtered.map((entry, i) => (
            <div key={i} className={`trace-entry trace-${entry.level}`}>
              <span className="trace-ts">{formatTraceTs(entry.ts)}</span>
              <span className={`trace-type-badge trace-type-${entry.type.toLowerCase()}`}>{entry.type}</span>
              <span className="trace-summary">{entry.summary}</span>
              {entry.detail && <pre className="trace-detail">{entry.detail}</pre>}
            </div>
          ))
        )}
        <div ref={bottomRef} />
      </div>
    </>
  );
}

/* ── Tasks Tab ── */

function TasksTab({ tasks, isSending, onCancelRun }: {
  tasks: ActiveTask[];
  isSending: boolean;
  onCancelRun: () => void;
}) {
  const running = tasks.filter((t) => t.status === "running");
  const completed = tasks.filter((t) => t.status !== "running");

  return (
    <div className="tasks-tab-body">
      {/* Global cancel */}
      {isSending && (
        <div className="tasks-global-bar">
          <div className="tasks-global-status">
            <span className="tasks-pulse" />
            <span>Agent is running</span>
          </div>
          <button className="tasks-kill-btn" onClick={onCancelRun}>
            <Icon name="square" />
            <span>Stop All</span>
          </button>
        </div>
      )}

      {/* Running tasks */}
      {running.length > 0 && (
        <div className="tasks-section">
          <div className="tasks-section-label">Running ({running.length})</div>
          {running.map((t) => (
            <div key={t.id} className="task-row task-running">
              <Icon name="loader" className="spin" />
              <span className="task-name">{t.name}</span>
              <span className="task-elapsed">{formatElapsed(t.startedAt)}</span>
            </div>
          ))}
        </div>
      )}

      {/* Completed tasks */}
      {completed.length > 0 && (
        <div className="tasks-section">
          <div className="tasks-section-label">Completed ({completed.length})</div>
          {completed.map((t) => (
            <div key={t.id} className={`task-row task-${t.status}`}>
              {t.status === "done" ? <Icon name="check" /> : <Icon name="alert-circle" />}
              <span className="task-name">{t.name}</span>
              {t.durationMs != null && <span className="task-duration">{t.durationMs}ms</span>}
            </div>
          ))}
        </div>
      )}

      {!isSending && tasks.length === 0 && (
        <div className="trace-empty">
          <Icon name="activity" />
          <span>No tasks running</span>
        </div>
      )}
    </div>
  );
}

function formatElapsed(startedAt: number): string {
  const elapsed = Math.round((Date.now() - startedAt) / 1000);
  if (elapsed < 60) return `${elapsed}s`;
  return `${Math.floor(elapsed / 60)}m ${elapsed % 60}s`;
}

/* ── Skills Tab ── */

function SkillsTab({ skillUsages }: { skillUsages: SkillUsage[] }) {
  const included = skillUsages.filter((s) => s.included);
  const excluded = skillUsages.filter((s) => !s.included);

  return (
    <div className="skills-tab-body">
      {skillUsages.length === 0 ? (
        <div className="trace-empty">
          <Icon name="zap" />
          <span>No skill data for this run</span>
        </div>
      ) : (
        <>
          {included.length > 0 && (
            <div className="tasks-section">
              <div className="tasks-section-label">Included ({included.length})</div>
              {included.map((s) => (
                <div key={s.id} className="skill-row skill-included">
                  <Icon name="check" />
                  <span className="task-name">{s.id}</span>
                  {s.tokenCost != null && <span className="task-duration">{s.tokenCost} tok</span>}
                </div>
              ))}
            </div>
          )}
          {excluded.length > 0 && (
            <div className="tasks-section">
              <div className="tasks-section-label">Excluded ({excluded.length})</div>
              {excluded.map((s) => (
                <div key={s.id} className="skill-row skill-excluded">
                  <Icon name="x" />
                  <span className="task-name">{s.id}</span>
                  {s.reason && <span className="skill-reason">{s.reason}</span>}
                </div>
              ))}
            </div>
          )}
        </>
      )}
    </div>
  );
}

function formatTraceTs(ts: number): string {
  const d = new Date(ts);
  return d.toLocaleTimeString("en-US", { hour12: false, hour: "2-digit", minute: "2-digit", second: "2-digit" }) + "." + String(d.getMilliseconds()).padStart(3, "0");
}

function AskHumanCard({ askHuman, onRespond }: {
  askHuman: { requestId: string; question: string; options: string[]; urgent: boolean; answered: boolean; response?: string; sentToChannels?: string[]; viaChannel?: string };
  onRespond: (requestId: string, response: string) => void;
}) {
  const [freeText, setFreeText] = useState("");
  const channelBadges = askHuman.sentToChannels && askHuman.sentToChannels.length > 0 ? (
    <div className="ask-human-channels">
      <Icon name="globe" />
      <span>Also sent to: {askHuman.sentToChannels.join(", ")}</span>
    </div>
  ) : null;

  if (askHuman.answered) {
    return (
      <div className="ask-human-card answered">
        <div className="ask-human-header">
          <Icon name="check-circle" />
          <span className="ask-human-label">Decision made{askHuman.viaChannel ? ` (via ${askHuman.viaChannel})` : ""}</span>
        </div>
        <div className="ask-human-question">{askHuman.question}</div>
        <div className="ask-human-response">
          <Icon name="user" />
          <span>{askHuman.response}</span>
        </div>
        {channelBadges}
      </div>
    );
  }
  return (
    <div className={`ask-human-card ${askHuman.urgent ? "urgent" : ""}`}>
      <div className="ask-human-header">
        <Icon name="help-circle" />
        <span className="ask-human-label">{askHuman.urgent ? "Decision needed (urgent)" : "Your input needed"}</span>
      </div>
      <div className="ask-human-question">{askHuman.question}</div>
      {channelBadges}
      {askHuman.options.length > 0 && (
        <div className="ask-human-options">
          {askHuman.options.map((opt, i) => (
            <button key={i} className="ask-human-option-btn" onClick={() => onRespond(askHuman.requestId, opt)}>
              {opt}
            </button>
          ))}
        </div>
      )}
      <div className="ask-human-freetext">
        <input
          type="text"
          placeholder="Or type your answer..."
          value={freeText}
          onChange={(e) => setFreeText(e.target.value)}
          onKeyDown={(e) => { if (e.key === "Enter" && freeText.trim()) { onRespond(askHuman.requestId, freeText.trim()); setFreeText(""); } }}
        />
        <button disabled={!freeText.trim()} onClick={() => { if (freeText.trim()) { onRespond(askHuman.requestId, freeText.trim()); setFreeText(""); } }}>
          <Icon name="send" />
        </button>
      </div>
    </div>
  );
}

// ── Per-Chat Model Selector (ChatGPT-style, multi-provider) ──

interface ModelSelection {
  providerId: string;
  provider: string;
  modelId: string;
  apiKey: string;
  baseUrl: string;
}

function ModelSelector({
  currentModel,
  currentProviderId,
  providerConfigs,
  onSelect,
  disabled,
}: {
  currentModel: string;
  currentProviderId: string | null;
  providerConfigs: ProviderConfig[];
  onSelect: (selection: ModelSelection) => void;
  disabled?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  // Close on outside click
  useEffect(() => {
    if (!open) return;
    const handler = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [open]);

  // Find display label for current model
  let shortLabel = currentModel || "Select model";
  for (const cfg of providerConfigs) {
    const models = PROVIDER_MODELS[cfg.provider] || [];
    const found = models.find((m) => m.id === currentModel);
    if (found) {
      shortLabel = found.label.replace(/\s*\(.*?\)\s*$/, "");
      break;
    }
  }
  // Fallback: just show model id
  if (shortLabel === currentModel && currentModel.length > 20) {
    shortLabel = currentModel.slice(0, 20) + "…";
  }

  return (
    <div className="model-selector" ref={ref}>
      <button
        className={`model-selector-trigger ${open ? "open" : ""}`}
        onClick={() => !disabled && setOpen(!open)}
        disabled={disabled}
        title={`Model: ${currentModel}`}
      >
        <Icon name="cpu" />
        <span className="model-selector-label">{shortLabel}</span>
        <Icon name="chevron-down" className={`model-selector-chevron ${open ? "rotated" : ""}`} />
      </button>
      {open && (
        <div className="model-selector-dropdown">
          {providerConfigs.length === 0 && (
            <div className="model-selector-empty">No providers configured. Go to Settings → Preferences.</div>
          )}
          {providerConfigs.map((cfg) => {
            const models = PROVIDER_MODELS[cfg.provider] || [];
            // Always show the configured model even if not in PROVIDER_MODELS list
            const configuredInList = models.some((m) => m.id === cfg.model);
            return (
              <div key={cfg.id} className="model-selector-group">
                <div className="model-selector-header">
                  <span className="model-selector-provider">{cfg.label || cfg.provider}</span>
                </div>
                {/* The specifically configured model (if not in preset list) */}
                {!configuredInList && cfg.model && (
                  <button
                    className={`model-selector-option ${cfg.model === currentModel && cfg.id === currentProviderId ? "active" : ""}`}
                    onClick={() => {
                      onSelect({ providerId: cfg.id, provider: cfg.provider, modelId: cfg.model, apiKey: cfg.apiKey, baseUrl: cfg.baseUrl });
                      setOpen(false);
                    }}
                  >
                    <span className="model-selector-option-label">{cfg.model}</span>
                    {cfg.model === currentModel && cfg.id === currentProviderId && <Icon name="check" />}
                  </button>
                )}
                {/* Preset models for this provider */}
                {models.map((m) => (
                  <button
                    key={m.id}
                    className={`model-selector-option ${m.id === currentModel && cfg.id === currentProviderId ? "active" : ""}`}
                    onClick={() => {
                      onSelect({ providerId: cfg.id, provider: cfg.provider, modelId: m.id, apiKey: cfg.apiKey, baseUrl: cfg.baseUrl });
                      setOpen(false);
                    }}
                  >
                    <span className="model-selector-option-label">{m.label}</span>
                    {m.id === currentModel && cfg.id === currentProviderId && <Icon name="check" />}
                  </button>
                ))}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}

// ── Markdown Renderer ─────────────────────────────────────────
// Lightweight inline parser: code blocks, inline code, bold,
// italic, headers, lists, links.  No external deps — works well
// with streaming (re-renders on each chunk).

interface MarkdownBlock {
  type: "code" | "paragraph" | "heading" | "list" | "hr" | "table";
  content: string;
  lang?: string;
  level?: number;
  ordered?: boolean;
  items?: string[];
  headers?: string[];
  rows?: string[][];
}

function isPipeTableRow(line: string) {
  const trimmed = line.trim();
  return trimmed.includes("|") && /^\|?.+\|.+\|?$/.test(trimmed);
}

function isPipeTableSeparator(line: string) {
  const trimmed = line.trim();
  if (!trimmed.includes("|")) return false;
  return trimmed
    .split("|")
    .map((cell) => cell.trim())
    .filter(Boolean)
    .every((cell) => /^:?-{3,}:?$/.test(cell));
}

function parsePipeTableCells(line: string) {
  return line
    .trim()
    .replace(/^\|/, "")
    .replace(/\|$/, "")
    .split("|")
    .map((cell) => cell.trim());
}

function parseMarkdownBlocks(raw: string): MarkdownBlock[] {
  const blocks: MarkdownBlock[] = [];
  const lines = raw.split("\n");
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    // Fenced code block
    const fenceMatch = line.match(/^```(\w*)/);
    if (fenceMatch) {
      const lang = fenceMatch[1] || "";
      const codeLines: string[] = [];
      i++;
      while (i < lines.length && !lines[i].startsWith("```")) {
        codeLines.push(lines[i]);
        i++;
      }
      blocks.push({ type: "code", content: codeLines.join("\n"), lang });
      i++; // skip closing ```
      continue;
    }
    // Heading
    const headingMatch = line.match(/^(#{1,6})\s+(.+)/);
    if (headingMatch) {
      blocks.push({ type: "heading", content: headingMatch[2], level: headingMatch[1].length });
      i++;
      continue;
    }
    // Horizontal rule
    if (/^(-{3,}|_{3,}|\*{3,})\s*$/.test(line)) {
      blocks.push({ type: "hr", content: "" });
      i++;
      continue;
    }
    // Unordered list
    if (/^[-*+]\s/.test(line)) {
      const items: string[] = [];
      while (i < lines.length && /^[-*+]\s/.test(lines[i])) {
        items.push(lines[i].replace(/^[-*+]\s/, ""));
        i++;
      }
      blocks.push({ type: "list", content: "", items, ordered: false });
      continue;
    }
    // Ordered list
    if (/^\d+\.\s/.test(line)) {
      const items: string[] = [];
      while (i < lines.length && /^\d+\.\s/.test(lines[i])) {
        items.push(lines[i].replace(/^\d+\.\s/, ""));
        i++;
      }
      blocks.push({ type: "list", content: "", items, ordered: true });
      continue;
    }
    // GFM-style pipe table
    if (
      i + 1 < lines.length &&
      isPipeTableRow(line) &&
      isPipeTableSeparator(lines[i + 1])
    ) {
      const headers = parsePipeTableCells(line);
      const rows: string[][] = [];
      i += 2;
      while (i < lines.length && lines[i].trim() !== "" && isPipeTableRow(lines[i])) {
        rows.push(parsePipeTableCells(lines[i]));
        i++;
      }
      blocks.push({ type: "table", content: "", headers, rows });
      continue;
    }
    // Empty line
    if (line.trim() === "") { i++; continue; }
    // Paragraph
    const paraLines: string[] = [];
    while (
      i < lines.length &&
      lines[i].trim() !== "" &&
      !lines[i].match(/^```/) &&
      !lines[i].match(/^#{1,6}\s/) &&
      !lines[i].match(/^[-*+]\s/) &&
      !lines[i].match(/^\d+\.\s/) &&
      !lines[i].match(/^(-{3,}|_{3,}|\*{3,})\s*$/)
    ) {
      paraLines.push(lines[i]);
      i++;
    }
    if (paraLines.length > 0) {
      blocks.push({ type: "paragraph", content: paraLines.join("\n") });
    }
  }
  return blocks;
}

function renderInline(text: string): React.ReactNode[] {
  const parts: React.ReactNode[] = [];
  const regex = /(`[^`]+`|\*\*[^*]+\*\*|\*[^*]+\*|\[[^\]]+\]\([^)]+\))/g;
  let lastIndex = 0;
  let match: RegExpExecArray | null;
  let key = 0;
  while ((match = regex.exec(text)) !== null) {
    if (match.index > lastIndex) parts.push(text.slice(lastIndex, match.index));
    const tok = match[0];
    if (tok.startsWith("`") && tok.endsWith("`")) {
      parts.push(<code key={key++} className="md-inline-code">{tok.slice(1, -1)}</code>);
    } else if (tok.startsWith("**") && tok.endsWith("**")) {
      parts.push(<strong key={key++}>{tok.slice(2, -2)}</strong>);
    } else if (tok.startsWith("*") && tok.endsWith("*")) {
      parts.push(<em key={key++}>{tok.slice(1, -1)}</em>);
    } else if (tok.startsWith("[")) {
      const lm = tok.match(/^\[([^\]]+)\]\(([^)]+)\)$/);
      if (lm) parts.push(<a key={key++} href={lm[2]} target="_blank" rel="noopener noreferrer" className="md-link">{lm[1]}</a>);
      else parts.push(tok);
    } else {
      parts.push(tok);
    }
    lastIndex = match.index + tok.length;
  }
  if (lastIndex < text.length) parts.push(text.slice(lastIndex));
  return parts;
}

function CodeBlock({ code, lang }: { code: string; lang?: string }) {
  const [copied, setCopied] = useState(false);
  const handleCopy = useCallback(() => {
    navigator.clipboard.writeText(code).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    });
  }, [code]);
  return (
    <div className="md-code-block">
      <div className="md-code-header">
        <span className="md-code-lang">{lang || "code"}</span>
        <button className="md-code-copy" onClick={handleCopy}>{copied ? "✓ Copied" : "Copy"}</button>
      </div>
      <pre className="md-code-pre"><code>{code}</code></pre>
    </div>
  );
}

function MarkdownContent({ content, isStreaming }: { content: string; isStreaming?: boolean }) {
  const blocks = useMemo(() => parseMarkdownBlocks(content), [content]);
  if (!content && isStreaming) return null;
  if (!content && !isStreaming) return null; // nothing to render
  return (
    <div className="markdown-content">
      {blocks.map((block, i) => {
        switch (block.type) {
          case "code":
            return <CodeBlock key={i} code={block.content} lang={block.lang} />;
          case "heading": {
            const Tag = `h${Math.min(block.level || 1, 6)}` as keyof JSX.IntrinsicElements;
            return <Tag key={i} className="md-heading">{renderInline(block.content)}</Tag>;
          }
          case "hr":
            return <hr key={i} className="md-hr" />;
          case "list":
            if (block.ordered) {
              return (<ol key={i} className="md-list md-ol">{(block.items || []).map((item, j) => (<li key={j}>{renderInline(item)}</li>))}</ol>);
            }
            return (<ul key={i} className="md-list md-ul">{(block.items || []).map((item, j) => (<li key={j}>{renderInline(item)}</li>))}</ul>);
          case "table":
            return (
              <div key={i} className="md-table-wrap">
                <table className="md-table">
                  <thead>
                    <tr>
                      {(block.headers || []).map((header, j) => (
                        <th key={j}>{renderInline(header)}</th>
                      ))}
                    </tr>
                  </thead>
                  <tbody>
                    {(block.rows || []).map((row, rowIndex) => (
                      <tr key={rowIndex}>
                        {row.map((cell, cellIndex) => (
                          <td key={cellIndex}>{renderInline(cell)}</td>
                        ))}
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            );
          case "paragraph":
          default:
            return <p key={i} className="md-paragraph">{renderInline(block.content)}</p>;
        }
      })}
    </div>
  );
}

// ── Component ─────────────────────────────────────────────────

export function ChatPage({
  agents,
  skills,
  selectedAgentId,
  onSelectAgent,
  onCreateAgent,
  onOpenJourney,
  pushToast,
  showTerminal,
  setShowTerminal,
}: ChatPageProps) {
  const [input, setInput] = useState("");
  const [messages, setMessages] = useState<ThreadMessage[]>([]);
  const [isSending, setIsSending] = useState(false);
  const [lastError, setLastError] = useState<string | null>(null);
  const [showAgentPicker, setShowAgentPicker] = useState(false);
  const [showScrollBtn, setShowScrollBtn] = useState(false);

  // ── Agent Flow coordination state ──
  const [selectedFlowId, setSelectedFlowId] = useState<string | null>(null);
  const [showFlowSelector, setShowFlowSelector] = useState(false);

  // ── Trace panel state ──
  const [traceEntries, setTraceEntries] = useState<TraceLogEntry[]>([]);
  const [traceOpen, setTraceOpen] = useState(false);
  const [traceFilter, setTraceFilter] = useState<string | null>(null);
  const [sidePanelTab, setSidePanelTab] = useState<SidePanelTab>("trace");
  const [activeTasks, setActiveTasks] = useState<ActiveTask[]>([]);
  const [skillUsages, setSkillUsages] = useState<SkillUsage[]>([]);
  const [previewOpen, setPreviewOpen] = useState(false);
  const previewPollersRef = useRef<Map<string, number>>(new Map());
  const previewRegisteredRef = useRef<Set<string>>(new Set());
  const toolIdCounterRef = useRef(0);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const messagesContainerRef = useRef<HTMLDivElement>(null);
  const [isUploadingFiles, setIsUploadingFiles] = useState(false);
  const [uploadedContextDocs, setUploadedContextDocs] = useState<UploadedContextDocument[]>([]);

  const pushTrace = useCallback((type: string, summary: string, opts?: { detail?: string; level?: TraceLogEntry["level"]; toolName?: string }) => {
    setTraceEntries((prev) => [...prev, {
      ts: Date.now(),
      type,
      summary,
      detail: opts?.detail,
      level: opts?.level ?? "info",
      toolName: opts?.toolName,
    }]);
  }, []);



  // Thread history state — multi-chat model
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [showSidebar, setShowSidebar] = useState(false);
  // The active chat ID (null = new thread, not yet created)
  const [activeChatId, setActiveChatId] = useState<string | null>(null);
  const activeChatIdRef = useRef<string | null>(null);
  // Flag: skip the next activeChatId-driven message reload (set when
  // sendMessage adopts a new chat_id to avoid overwriting streaming state)
  const skipNextLoadRef = useRef(false);
  // Generation counter to prevent stale listSessions overwrites
  const sessionsGenRef = useRef(0);
  // Semantic dedup — track finalized message IDs to skip redundant reloads
  // Replaces the fragile 500ms temporal debounce that dropped rapid messages.
  const lastFinalizedMsgIdsRef = useRef<Set<string>>(new Set());

  // Per-chat model override: maps chatId → { providerId, provider, modelId, apiKey, baseUrl }.
  // New chats or chats without an override use the active provider from settings.
  const [chatModelOverrides, setChatModelOverrides] = useState<Record<string, ModelSelection>>({});
  const [providerConfigs] = useState<ProviderConfig[]>(() => loadProviders());
  const activeProvider = getActiveProvider();
  const activeProvIdVal = getActiveProviderId();
  // Resolve the effective model & provider for the current chat
  const chatOverride = activeChatId ? chatModelOverrides[activeChatId] : undefined;
  const effectiveModel = chatOverride?.modelId || activeProvider?.model || "";
  const effectiveProvider = chatOverride?.provider || activeProvider?.provider || "Ollama (Local)";
  const effectiveProviderId = chatOverride?.providerId || activeProvIdVal || "";
  const effectiveApiKey = chatOverride?.apiKey || activeProvider?.apiKey || "";
  const effectiveBaseUrl = chatOverride?.baseUrl || activeProvider?.baseUrl || "";

  const registerPreviewFromPort = useCallback(async (id: string, label: string, port: number) => {
    const key = `${id}:${port}`;
    if (previewRegisteredRef.current.has(key)) {
      return;
    }
    previewRegisteredRef.current.add(key);
    try {
      await api.previewRegister(id, label, port);
      setPreviewOpen(true);
      pushToast(`Preview connected on localhost:${port}.`);
    } catch {
      previewRegisteredRef.current.delete(key);
    }
  }, [pushToast]);

  const trackPreviewCandidate = useCallback((id: string, label: string, port: number) => {
    const pollKey = `${id}:${port}`;
    if (previewPollersRef.current.has(pollKey)) {
      return;
    }

    let attempts = 0;
    const timer = window.setInterval(async () => {
      attempts += 1;
      try {
        const alive = await api.previewCheckPort(port);
        if (alive) {
          window.clearInterval(timer);
          previewPollersRef.current.delete(pollKey);
          await registerPreviewFromPort(id, label, port);
          return;
        }
      } catch {
        // Ignore transient probe errors while the server is still starting.
      }

      if (attempts >= 20) {
        window.clearInterval(timer);
        previewPollersRef.current.delete(pollKey);
      }
    }, 1500);

    previewPollersRef.current.set(pollKey, timer);
  }, [registerPreviewFromPort]);

  useEffect(() => {
    return () => {
      previewPollersRef.current.forEach((timer) => window.clearInterval(timer));
      previewPollersRef.current.clear();
    };
  }, []);

  useEffect(() => {
    const assistantTexts = messages
      .filter((message) => message.role === "assistant" && message.text)
      .map((message) => message.text);
    for (const text of assistantTexts) {
      for (const port of extractPreviewPorts(text)) {
        trackPreviewCandidate(`chat_preview_${port}`, `Generated app :${port}`, port);
      }
    }
  }, [messages, trackPreviewCandidate]);

  const setPerChatModel = useCallback((selection: ModelSelection) => {
    const chatId = activeChatIdRef.current;
    if (chatId) {
      setChatModelOverrides((prev) => ({ ...prev, [chatId]: selection }));
    } else {
      // No active chat yet — update localStorage for next send
      window.localStorage.setItem("clawdesk.provider", selection.provider);
      window.localStorage.setItem("clawdesk.model", selection.modelId);
      window.localStorage.setItem("clawdesk.api_key", selection.apiKey);
      window.localStorage.setItem("clawdesk.base_url", selection.baseUrl);
    }
  }, []);

  const openComposerFilePicker = useCallback(async () => {
    if (isUploadingFiles) {
      return;
    }

    try {
      const selected = await open({
        multiple: true,
        directory: false,
        filters: [
          {
            name: "Documents and code",
            extensions: ["pdf", "txt", "md", "csv", "json", "html", "xml", "py", "js", "ts", "rs", "go", "java", "c", "cpp", "h", "rb", "sh"],
          },
        ],
      });

      const paths = Array.isArray(selected) ? selected : selected ? [selected] : [];
      if (!paths.length) {
        return;
      }

      setIsUploadingFiles(true);
      const uploadedDocs: UploadedContextDocument[] = [];
      for (const filePath of paths) {
        try {
          const doc = await api.ragIngestDocument(filePath);
          uploadedDocs.push({ id: doc.id, filename: doc.filename, chunkCount: doc.chunk_count });
          pushToast(`Ingested \"${doc.filename}\" (${doc.chunk_count} chunks)`);
        } catch (err: any) {
          pushToast(`Upload failed: ${err}`);
        }
      }

      if (uploadedDocs.length) {
        setUploadedContextDocs((prev) => {
          const uploadedIds = new Set(uploadedDocs.map((doc) => doc.id));
          const next = [...uploadedDocs, ...prev.filter((doc) => !uploadedIds.has(doc.id))];
          return next.slice(0, 6);
        });
      }
    } catch (err: any) {
      pushToast(`Could not open file picker: ${err}`);
    } finally {
      setIsUploadingFiles(false);
    }
  }, [isUploadingFiles, pushToast]);

  const buildUploadedDocsContext = useCallback(async () => {
    if (!uploadedContextDocs.length) {
      return "";
    }

    const maxChars = 6000;
    const sections: string[] = [];
    let usedChars = 0;

    for (const doc of uploadedContextDocs) {
      if (usedChars >= maxChars) {
        break;
      }

      try {
        const chunks = await api.ragGetChunks(doc.id);
        if (!chunks.length) {
          continue;
        }

        const excerpt = chunks.join("\n\n").slice(0, maxChars - usedChars);
        if (!excerpt.trim()) {
          continue;
        }

        sections.push(`Document: ${doc.filename}\n${excerpt}`);
        usedChars += excerpt.length;
      } catch (err) {
        console.warn("[CHAT] failed to load uploaded document context", doc.filename, err);
      }
    }

    if (!sections.length) {
      return "";
    }

    return [
      "Attached document context:",
      ...sections,
      "Use the attached document context above to answer the user's question. Treat it as source material, not as a filesystem path to open.",
    ].join("\n\n");
  }, [uploadedContextDocs]);

  const agent = agents.find((a) => a.id === selectedAgentId) ?? agents[0] ?? null;
  const isNew = messages.length === 0;
  const currentSession = activeChatId ? sessions.find((session) => session.chat_id === activeChatId) ?? null : null;
  const heroSuggestions = [
    { icon: "✍️", title: "Write an email", desc: "Draft a professional response" },
    { icon: "📋", title: "Summarize this", desc: "Give me the key points" },
    { icon: "💡", title: "Brainstorm ideas", desc: "Help me think through options" },
    { icon: "📅", title: "Plan my week", desc: "Organize tasks and priorities" },
    { icon: "📝", title: "Write a document", desc: "Article, report, or proposal" },
    { icon: "🔍", title: "Research a topic", desc: "Find and summarize information" },
  ];

  // Auto-select the first agent if none is selected
  useEffect(() => {
    if (!selectedAgentId && agents.length > 0) {
      onSelectAgent(agents[0].id);
    }
  }, [agents, selectedAgentId, onSelectAgent]);

  // (Auto-create removed: agents are created explicitly from Settings > Agents)

  // Load sessions on mount so history sidebar is immediately populated
  useEffect(() => {
    const gen = ++sessionsGenRef.current;
    api.listSessions().then((s) => {
      if (gen === sessionsGenRef.current) setSessions(s);
    }).catch(() => { });

    // Diagnostic: dump SochDB session state to DevTools console on mount
    api.debugSessionStorage().then((dump) => {
      console.log("[DIAGNOSTIC] SochDB session storage dump:", JSON.stringify(dump, null, 2));
      console.log("[DIAGNOSTIC] Total sessions in SochDB:", dump.length);
      dump.forEach((s, i) => {
        console.log(`  [${i}] chat_id=${s.chat_id} agent=${s.agent_id} title="${s.title}" created=${s.created_at} updated=${s.updated_at} msgs=${s.message_count} in_cache=${s.in_lru_cache}`);
      });
    }).catch((e) => console.warn("[DIAGNOSTIC] debug_session_storage failed:", e));

    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Note: Thread history is also refreshed by the selectedAgentId effect below.

  // Auto-scroll on new messages
  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages]);

  // Track scroll position to show/hide scroll-to-bottom
  useEffect(() => {
    const el = messagesContainerRef.current;
    if (!el) return;
    const onScroll = () => {
      const gap = el.scrollHeight - el.scrollTop - el.clientHeight;
      setShowScrollBtn(gap > 120);
    };
    el.addEventListener("scroll", onScroll);
    return () => el.removeEventListener("scroll", onScroll);
  }, [messages.length > 0]);

  // Keep the activeChatIdRef in sync
  useEffect(() => {
    activeChatIdRef.current = activeChatId;
  }, [activeChatId]);

  // Helper: map backend ChatMessage[] → ThreadMessage[]
  // Backend get_chat_messages already filters out intermediate tool msgs.
  // We accept all messages the backend returns — no double-filtering.
  const mapBackendMessages = useCallback((backendMsgs: ChatMessage[]): ThreadMessage[] => {
    return backendMsgs.map((m) => ({
      id: m.id,
      role: m.role as "user" | "assistant" | "system",
      text: m.content,
      time: new Date(m.timestamp).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }),
      tokens: m.metadata?.token_cost,
      cost: m.metadata?.cost_usd,
      duration: m.metadata?.duration_ms,
      skills: m.metadata?.skills_activated,
      agent: m.role === "assistant" ? agent?.name : undefined,
    }));
  }, [agent?.name]);

  // Load messages when activeChatId changes (sidebar click or agent switch)
  useEffect(() => {
    console.log("[EFFECT:activeChatId] fired:", activeChatId, "sendingRef:", sendingRef.current, "skipNext:", skipNextLoadRef.current);
    if (!activeChatId) {
      // Don't clear messages if we're mid-send (the streaming placeholder is there)
      if (!sendingRef.current) setMessages([]);
      return;
    }
    // Skip load if sendMessage just adopted this chat_id
    if (skipNextLoadRef.current) {
      skipNextLoadRef.current = false;
      return;
    }
    // Don't reload from backend while actively sending — it would wipe the streaming placeholder
    if (sendingRef.current) return;
    const chatIdToLoad = activeChatId;
    api.getChatMessages(chatIdToLoad).then((backendMsgs) => {
      // Double-check we're still not sending (async gap)
      if (sendingRef.current) return;
      // Verify we're still on the same chat (another action may have changed it)
      if (activeChatIdRef.current !== chatIdToLoad) {
        console.log("[EFFECT:activeChatId] stale load — chatId changed during async", chatIdToLoad, "→", activeChatIdRef.current);
        return;
      }
      const mapped = mapBackendMessages(backendMsgs);
      // Update semantic dedup ref after backend reload so incoming:message
      // events don't redundantly reload the same messages.
      lastFinalizedMsgIdsRef.current = new Set(backendMsgs.map((m) => m.id));
      setMessages(mapped);
    }).catch(() => { });
    // Include mapBackendMessages in deps so messages are re-mapped
    // when the agent changes (agent?.name flows through the useCallback dep).
    // Previously, agent switch could leave stale agent names in message bubbles.
  }, [activeChatId, mapBackendMessages]);

  // When agent changes, find the most recent chat for this agent or start fresh
  useEffect(() => {
    console.log("[EFFECT:selectedAgentId] fired:", selectedAgentId, "sendingRef:", sendingRef.current);
    if (!selectedAgentId) {
      if (!sendingRef.current) {
        setMessages([]);
        setActiveChatId(null);
      }
      return;
    }
    // Abort in-flight send instead of blocking agent switch.
    // This prevents the UI from freezing for up to 120s on agent change.
    if (sendingRef.current && activeSendAbortRef.current) {
      activeSendAbortRef.current.abort();
      sendingRef.current = false;
      activeSendAbortRef.current = null;
      streamingMsgIdRef.current = null;
      setIsSending(false);
    }
    // Snapshot both the chatId and sessions generation BEFORE the async call.
    const chatIdBeforeAsync = activeChatIdRef.current;
    const gen = ++sessionsGenRef.current;
    api.listSessions().then((allSessions) => {
      if (sendingRef.current) return; // re-check after async
      // Only update sessions if no newer fetch has completed
      if (gen === sessionsGenRef.current) setSessions(allSessions);
      // If activeChatId was changed during the async gap (e.g. by sendMessage
      // adopting a new chat, or sidebar click), do NOT overwrite it.
      if (activeChatIdRef.current !== chatIdBeforeAsync) {
        console.log("[EFFECT:selectedAgentId] chatId changed during async, skipping overwrite:", chatIdBeforeAsync, "→", activeChatIdRef.current);
        return;
      }
      // If we already have a valid chatId for this agent, keep it
      const currentChatId = activeChatIdRef.current;
      if (currentChatId) {
        const currentChatBelongsToAgent = allSessions.some(
          (s) => s.chat_id === currentChatId && s.agent_id === selectedAgentId
        );
        if (currentChatBelongsToAgent) {
          console.log("[EFFECT:selectedAgentId] keeping existing chatId:", currentChatId);
          return;
        }
      }
      const agentChats = allSessions.filter((s) => s.agent_id === selectedAgentId);
      console.log("[EFFECT:selectedAgentId] agentChats for", selectedAgentId, ":", agentChats.length, "total sessions:", allSessions.length);
      if (agentChats.length > 0) {
        agentChats.forEach((s, i) => {
          console.log(`  [${i}] chat_id=${s.chat_id} title="${s.title}" created=${s.created_at} last_activity=${s.last_activity} msgs=${s.message_count}`);
        });
        const latest = agentChats[0];
        console.log("[EFFECT:selectedAgentId] selecting latest chat:", latest.chat_id, latest.title);
        setActiveChatId(latest.chat_id);
      } else {
        setActiveChatId(null);
        setMessages([]);
      }
    }).catch(() => { });
  }, [selectedAgentId]);

  // ── Streaming: ref to track active streaming message ──
  const streamingMsgIdRef = useRef<string | null>(null);
  // Replace boolean sendingRef with generation counter + abort controller.
  // The generation counter provides CAS-like semantics — stale send callbacks
  // are discarded without corrupting current state. Agent switches abort the
  // in-flight send, releasing the UI immediately.
  const sendingRef = useRef(false);
  const sendGenRef = useRef(0);
  const activeSendAbortRef = useRef<AbortController | null>(null);

  // ── Listen for incoming:message to refresh session after backend persists ──
  useEffect(() => {
    let aborted = false;
    let unlisten: (() => void) | null = null;
    listen<{ agent_id: string; chat_id?: string }>("incoming:message", (ev) => {
      if (aborted) return;
      const data = ev.payload;
      console.log("[LISTENER:incoming:message]", data, "sendingRef:", sendingRef.current, "streamingMsgIdRef:", streamingMsgIdRef.current);
      if (!data) return;
      // Refresh sessions list (sidebar) with generation guard
      const gen = ++sessionsGenRef.current;
      api.listSessions().then((s) => {
        if (gen === sessionsGenRef.current) setSessions(s);
      }).catch(() => { });
      // NEVER reload messages while mid-send — it would wipe the streaming placeholder
      if (sendingRef.current || streamingMsgIdRef.current) return;
      // If the event is for the currently-viewed chat, reload messages
      const currentChatId = activeChatIdRef.current;
      if (data.chat_id && data.chat_id === currentChatId) {
        const chatIdToReload = data.chat_id;
        api.getChatMessages(chatIdToReload).then((backendMsgs) => {
          // Re-check: don't overwrite if a send started during the async gap
          if (aborted || sendingRef.current || streamingMsgIdRef.current) return;
          // Verify we're still on the same chat
          if (activeChatIdRef.current !== chatIdToReload) return;
          // Semantic dedup — only update if the backend has messages
          // the frontend hasn't seen yet. Prevents redundant re-renders.
          const backendIds = new Set(backendMsgs.map((m) => m.id));
          const known = lastFinalizedMsgIdsRef.current;
          const hasNew = backendMsgs.some((m) => !known.has(m.id));
          if (!hasNew) return;
          lastFinalizedMsgIdsRef.current = backendIds;
          setMessages(mapBackendMessages(backendMsgs));
        }).catch(() => { });
      }
    }).then((dispose) => {
      if (aborted) { dispose(); return; }
      unlisten = dispose;
    }).catch(() => { });
    return () => { aborted = true; if (unlisten) unlisten(); };
  }, [mapBackendMessages]);

  // ── Streaming: Subscribe to Tauri agent-event for live updates ──
  // Event types match TauriAgentEvent in state.rs

  // Map agent events to trace panel entries
  const pushTraceFromEvent = useCallback((event: AgentEventPayload) => {
    const ev = event as any;
    switch (event.type) {
      case "RoundStart":
        pushTrace("Round", `Round ${ev.round} started`, { level: "info" });
        break;
      case "ToolStart":
        pushTrace("ToolStart", `${ev.name}`, { detail: ev.args, level: "info", toolName: ev.name });
        break;
      case "ToolEnd":
        pushTrace("ToolEnd", `${ev.name} — ${ev.success ? "ok" : "FAILED"} (${ev.duration_ms}ms)`, { level: ev.success ? "info" : "error", toolName: ev.name });
        break;
      case "ToolBlocked":
        pushTrace("Blocked", `${ev.name} blocked: ${ev.reason}`, { level: "warn", toolName: ev.name });
        break;
      case "ToolExecutionResult":
        pushTrace("Result", `${ev.name} [${ev.is_error ? "err" : "ok"}] (${ev.duration_ms}ms)`, { detail: ev.preview, level: ev.is_error ? "error" : "debug", toolName: ev.name });
        break;
      case "PromptAssembled":
        pushTrace("Prompt", `${ev.total_tokens} tokens — ${ev.skills_included?.length || 0} skills, ${ev.memory_fragments} memory fragments (${Math.round((ev.budget_utilization || 0) * 100)}% budget)`, { level: "info" });
        break;
      case "ContextGuardAction":
        pushTrace("Guard", `${ev.action} — ${ev.token_count} tokens (threshold ${Math.round(ev.threshold * 100)}%)`, { level: "warn" });
        break;
      case "Compaction":
        pushTrace("Compact", `${ev.level}: ${ev.tokens_before} → ${ev.tokens_after} tokens`, { level: "info" });
        break;
      case "FallbackTriggered":
        pushTrace("Fallback", `${ev.from_model} → ${ev.to_model}: ${ev.reason}`, { level: "warn" });
        break;
      case "RetryStatus":
        pushTrace("Retry", `${ev.reason} (${ev.attempt}/${ev.max_attempts})`, { level: "warn" });
        break;
      case "SkillDecision":
        pushTrace("Skill", `${ev.skill_id} ${ev.included ? "included" : "excluded"}: ${ev.reason} (${ev.token_cost} tokens, ${ev.budget_remaining} remaining)`, { level: "debug" });
        break;
      case "ThinkingChunk":
        // Don't log every thinking chunk — too noisy
        break;
      case "StreamChunk":
        // Don't log stream chunks
        break;
      case "Response":
        pushTrace("Response", `Content received (finish: ${ev.finish_reason})`, { level: "info" });
        break;
      case "Done":
        pushTrace("Done", `Agent finished — ${ev.total_rounds} round(s)`, { level: "info" });
        break;
      case "Error":
        pushTrace("Error", ev.error, { level: "error" });
        break;
      case "InputRequired":
        pushTrace("AskHuman", ev.question, { level: "warn" });
        break;
      default:
        pushTrace(event.type, JSON.stringify(event).slice(0, 200), { level: "debug" });
    }
  }, [pushTrace]);

  useEffect(() => {
    const agentId = selectedAgentId ?? agent?.id;
    if (!agentId) return;

    let aborted = false;
    let unlisten: (() => void) | null = null;

    listen<AgentEventEnvelope>("agent-event", (ev) => {
      if (aborted) return;
      const data = ev.payload;
      console.log("[STREAM] agent-event received:", JSON.stringify(data));
      if (!data || data.agent_id !== agentId) {
        console.log("[STREAM] agent_id mismatch:", data?.agent_id, "!==", agentId);
        return;
      }
      const msgId = streamingMsgIdRef.current;
      if (!msgId) {
        console.log("[STREAM] no streamingMsgIdRef, discarding event type:", data?.event?.type);
        return;
      }

      const event = data.event;
      console.log("[STREAM] processing event:", event.type, "for msgId:", msgId);

      // ── Push ALL events to trace panel ──
      pushTraceFromEvent(event);

      // ── Update activeTasks / skillUsages for side panel ──
      if (event.type === "ToolStart") {
        const toolName = typeof (event as any).name === "string" ? (event as any).name : "unknown";
        setActiveTasks((prev) => [...prev, { id: `task-${Date.now()}-${toolName}`, name: toolName, startedAt: Date.now(), status: "running" }]);
      } else if (event.type === "ToolEnd") {
        const toolName = typeof (event as any).name === "string" ? (event as any).name : "unknown";
        const dur = typeof (event as any).duration_ms === "number" ? (event as any).duration_ms : undefined;
        const ok = Boolean((event as any).success);
        setActiveTasks((prev) => {
          const idx = prev.findIndex((t) => t.name === toolName && t.status === "running");
          if (idx < 0) return prev;
          const copy = [...prev];
          copy[idx] = { ...copy[idx], status: ok ? "done" : "error", durationMs: dur };
          return copy;
        });
      } else if (event.type === "SkillDecision") {
        const ev = event as any;
        setSkillUsages((prev) => [...prev.filter((s) => s.id !== ev.skill_id), { id: ev.skill_id, included: ev.included, reason: ev.reason, tokenCost: ev.token_cost }]);
      } else if (event.type === "PromptAssembled") {
        // Bulk add included/excluded skills if available
        const ev = event as any;
        if (Array.isArray(ev.skills_included)) {
          setSkillUsages((prev) => {
            const existing = new Set(prev.map((s) => s.id));
            const newSkills = (ev.skills_included as string[]).filter((id) => !existing.has(id)).map((id) => ({ id, included: true }));
            const excl = Array.isArray(ev.skills_excluded) ? (ev.skills_excluded as string[]).filter((id) => !existing.has(id)).map((id) => ({ id, included: false })) : [];
            return [...prev, ...newSkills, ...excl];
          });
        }
      } else if (event.type === "Done") {
        // Mark any remaining running tasks as done
        setActiveTasks((prev) => prev.map((t) => t.status === "running" ? { ...t, status: "done", durationMs: Date.now() - t.startedAt } : t));
      }

      // ── StreamChunk: Append streamed text in real-time ──
      if (event.type === "StreamChunk") {
        const chunkText = typeof event.text === "string" ? event.text : "";
        if (chunkText.length > 0) {
          setMessages((prev) =>
            prev.map((m) =>
              m.id === msgId ? { ...m, text: m.text + chunkText } : m
            )
          );
        }
        return;
      }

      // ── ThinkingChunk: Accumulate reasoning/thinking text (shown in collapsible block) ──
      if (event.type === "ThinkingChunk") {
        const chunkText = typeof event.text === "string" ? event.text : "";
        if (chunkText.length > 0) {
          setMessages((prev) =>
            prev.map((m) =>
              m.id === msgId ? { ...m, thinkingText: (m.thinkingText || "") + chunkText } : m
            )
          );
        }
        return;
      }

      // ── ToolStart: Add tool call brick (name comes from backend as `name`) ──
      if (event.type === "ToolStart") {
        const toolName = typeof event.name === "string" ? event.name : "unknown";
        const argsPreview = typeof event.args === "string" ? event.args : undefined;
        if (toolName === "shell_exec") {
          const parsedArgs = parseShellExecArgs(argsPreview);
          if (parsedArgs?.background && parsedArgs.port) {
            const previewLabel = parsedArgs.command.length > 48
              ? `${parsedArgs.command.slice(0, 48)}...`
              : parsedArgs.command;
            trackPreviewCandidate(`shell_exec_${parsedArgs.port}`, previewLabel, parsedArgs.port);
          }
        }
        const toolId = `tool-${++toolIdCounterRef.current}`;
        setMessages((prev) =>
          prev.map((m) => {
            if (m.id !== msgId) return m;
            const existing = m.toolCalls ?? [];
            return {
              ...m,
              toolCalls: [...existing, { id: toolId, name: toolName, argsPreview, status: "running" as const }],
            };
          })
        );
        return;
      }

      // ── ToolEnd: Update tool call status ──
      if (event.type === "ToolEnd") {
        const toolName = typeof event.name === "string" ? event.name : "unknown";
        const success = Boolean(event.success);
        const durationMs = typeof event.duration_ms === "number" ? event.duration_ms : 0;
        setMessages((prev) =>
          prev.map((m) => {
            if (m.id !== msgId) return m;
            const updated = (m.toolCalls ?? []).map((tc) =>
              tc.name === toolName && tc.status === "running"
                ? { ...tc, status: (success ? "done" : "error") as "done" | "error", durationMs, result: `${success ? "completed" : "failed"} in ${durationMs}ms` }
                : tc
            );
            return { ...m, toolCalls: updated };
          })
        );
        return;
      }

      // ── Response: Backend sends final content (may not arrive if streaming is used) ──
      if (event.type === "Response") {
        // If streaming chunks were empty, populate text from Response event
        setMessages((prev) =>
          prev.map((m) =>
            m.id === msgId && !m.text
              ? { ...m, text: (event as any).content || "" }
              : m
          )
        );
        return;
      }

      // ── Done: Agent finished all rounds — mark streaming complete ──
      if (event.type === "Done") {
        setMessages((prev) =>
          prev.map((m) =>
            m.id === msgId ? { ...m, isStreaming: false, retryStatus: undefined } : m
          )
        );
        // Don't clear streamingMsgIdRef yet — sendMessage finalization needs it
        return;
      }

      // ── RetryStatus: Provider is retrying (content filter, etc.) ──
      if (event.type === "RetryStatus") {
        const attempt = typeof event.attempt === "number" ? event.attempt : 0;
        const maxAttempts = typeof event.max_attempts === "number" ? event.max_attempts : 0;
        const reason = typeof event.reason === "string" ? event.reason : "Retrying…";
        setMessages((prev) =>
          prev.map((m) =>
            m.id === msgId ? { ...m, retryStatus: `${reason} (${attempt}/${maxAttempts})` } : m
          )
        );
        return;
      }

      // ── InputRequired: Agent asks the human for a decision ──
      if (event.type === "InputRequired") {
        const question = typeof (event as any).question === "string" ? (event as any).question : "";
        const options: string[] = Array.isArray((event as any).options) ? (event as any).options : [];
        const urgent = Boolean((event as any).urgent);
        // We don't have the requestId from agent-event; the ask-human:pending event carries it.
        // Stash the question so the pending listener can match it.
        return;
      }

      // ── Error: Agent execution failed ──
      if (event.type === "Error") {
        const errorText = typeof event.error === "string" ? event.error : "Agent execution failed.";
        setMessages((prev) =>
          prev.map((m) =>
            m.id === msgId ? { ...m, text: m.text || errorText, isStreaming: false } : m
          )
        );
        streamingMsgIdRef.current = null;
        // Reset BOTH sending indicators to ensure consistent state.
        // Previously only setIsSending was called, leaving sendingRef.current
        // stuck true until the finally block ran — creating a window where
        // the button appeared enabled but sendMessage would early-return.
        sendingRef.current = false;
        activeSendAbortRef.current = null;
        setIsSending(false);
        return;
      }
    }).then((dispose) => {
      if (aborted) { dispose(); return; }
      unlisten = dispose;
    }).catch(() => {
      // Event subscription unavailable (browser-dev mode)
    });

    // ── ask-human:pending: Tauri event carrying the request UUID ──
    let unlistenAskHuman: (() => void) | null = null;
    listen<{ id: string; question: string; options: string[]; urgent: boolean; sent_to_channels?: string[] }>("ask-human:pending", (ev) => {
      if (aborted) return;
      const { id, question, options, urgent, sent_to_channels } = ev.payload;
      const msgId = streamingMsgIdRef.current;
      if (!msgId) return;
      setMessages((prev) =>
        prev.map((m) =>
          m.id === msgId
            ? { ...m, askHuman: { requestId: id, question, options, urgent, answered: false, sentToChannels: sent_to_channels } }
            : m
        )
      );
    }).then((dispose) => {
      if (aborted) { dispose(); return; }
      unlistenAskHuman = dispose;
    }).catch(() => {});

    // ── ask-human:responded: a channel inbound resolved the ask-human ──
    let unlistenAskHumanResponded: (() => void) | null = null;
    listen<{ id: string; response: string; via_channel: string }>("ask-human:responded", (ev) => {
      if (aborted) return;
      const { id, response, via_channel } = ev.payload;
      setMessages((prev) =>
        prev.map((m) =>
          m.askHuman?.requestId === id
            ? { ...m, askHuman: { ...m.askHuman, answered: true, response, viaChannel: via_channel } }
            : m
        )
      );
    }).then((dispose) => {
      if (aborted) { dispose(); return; }
      unlistenAskHumanResponded = dispose;
    }).catch(() => {});

    return () => {
      aborted = true;
      if (unlisten) unlisten();
      if (unlistenAskHuman) unlistenAskHuman();
      if (unlistenAskHumanResponded) unlistenAskHumanResponded();
    };
  }, [selectedAgentId, agent?.id]);

  // Start a new blank thread — clears UI state.
  // Chat entity is created lazily on first message send.
  const startNewThread = useCallback(() => {
    // Abort any in-flight send before switching threads
    if (activeSendAbortRef.current) activeSendAbortRef.current.abort();
    setMessages([]);
    setInput("");
    setIsSending(false);
    setActiveChatId(null);
    streamingMsgIdRef.current = null;
    sendingRef.current = false;
    activeSendAbortRef.current = null;
  }, []);

  const stopMessage = useCallback(async () => {
    if (!isSending && !sendingRef.current) return;

    if (activeSendAbortRef.current) {
      activeSendAbortRef.current.abort();
    }

    try {
      await api.cancelActiveRun(activeChatIdRef.current ?? undefined);
    } catch {
      // Ignore cancel failures; local UI still stops the spinner.
    }

    const streamMsgId = streamingMsgIdRef.current;
    if (streamMsgId) {
      setMessages((prev) =>
        prev.map((m) =>
          m.id === streamMsgId
            ? { ...m, isStreaming: false, text: m.text || "Stopped by user." }
            : m
        )
      );
    }

    streamingMsgIdRef.current = null;
    sendingRef.current = false;
    activeSendAbortRef.current = null;
    setIsSending(false);
    pushToast("Stopped.");
  }, [isSending, pushToast]);

  // ── Human-in-the-loop: respond to ask_human requests ──
  const handleAskHumanRespond = useCallback(async (requestId: string, response: string) => {
    try {
      await api.respondToAskHuman(requestId, response);
    } catch (err) {
      console.error("[ASK_HUMAN] respond failed:", err);
    }
    // Mark the card as answered in UI
    setMessages((prev) =>
      prev.map((m) =>
        m.askHuman?.requestId === requestId
          ? { ...m, askHuman: { ...m.askHuman, answered: true, response } }
          : m
      )
    );
  }, []);

  // Select a chat from the sidebar
  const selectSession = useCallback((session: SessionSummary) => {
    // Switch to the agent if different
    if (session.agent_id !== selectedAgentId) {
      onSelectAgent(session.agent_id);
    }
    // Set the active chat — useEffect will load messages
    setActiveChatId(session.chat_id);
  }, [onSelectAgent, selectedAgentId]);

  // Delete a chat session
  const deleteSession = useCallback(async (chatId: string, e: React.MouseEvent) => {
    e.stopPropagation();
    try {
      await api.deleteChat(chatId);
      // Remove from local state immediately (optimistic)
      setSessions((prev) => prev.filter((s) => s.chat_id !== chatId));
      // If deleting the active chat, clear messages
      if (activeChatIdRef.current === chatId) {
        setActiveChatId(null);
        setMessages([]);
      }
    } catch (err) {
      console.error("Failed to delete chat:", err);
    }
  }, []);

  const sendMessage = useCallback(async (content: string) => {
    const agentId = selectedAgentId ?? agent?.id;
    const agentName = agent?.name;
    const visibleContent = content.replace(/\[Uploaded: [^\]]+\]\s*/g, "").replace(/\n{3,}/g, "\n\n").trim();
    console.log("[SEND] sendMessage called. agentId:", agentId, "content:", visibleContent?.slice(0, 50), "sendingRef:", sendingRef.current);
    if (!agentId || !visibleContent) {
      console.warn("[SEND] early return: no agentId or empty content");
      if (agentId && !visibleContent) {
        pushToast("Add a message for the uploaded file before sending.");
      }
      return;
    }
    // Prevent double-send while in-flight
    if (sendingRef.current) {
      console.warn("[SEND] early return: sendingRef.current is true (already sending)");
      return;
    }

    console.log("[SEND] starting sendMessage, agentId:", agentId, "content:", visibleContent.slice(0, 50));

    // Clear trace entries for the new message
    setTraceEntries([]);
    setActiveTasks([]);
    setSkillUsages([]);
    toolIdCounterRef.current = 0;

    // Generation counter + AbortController for scoped cancellation
    const gen = ++sendGenRef.current;
    const abortCtrl = new AbortController();
    activeSendAbortRef.current = abortCtrl;

    const userModel = effectiveModel || undefined;
    const userProvider = effectiveProvider || undefined;
    const userApiKey = effectiveApiKey || undefined;
    const userBaseUrl = effectiveBaseUrl || undefined;

    const userMsg: ThreadMessage = {
      id: `u_${Date.now()}`,
      role: "user",
      text: visibleContent,
      time: new Date().toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }),
    };

    // Insert a streaming placeholder for the assistant response
    const streamMsgId = `s_${Date.now()}`;
    const streamingMsg: ThreadMessage = {
      id: streamMsgId,
      role: "assistant",
      text: "",
      time: new Date().toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }),
      agent: agentName,
      toolCalls: [],
      isStreaming: true,
    };

    streamingMsgIdRef.current = streamMsgId;
    sendingRef.current = true;
    setMessages((prev) => [...prev, userMsg, streamingMsg]);
    setIsSending(true);
    setLastError(null);
    console.log("[SEND] refs set, messages queued. streamMsgId:", streamMsgId);

    try {
      // Chat Persistence Fix: Pre-create the chat session before sending the first message.
      // This ensures that if the LLM call fails (e.g., due to an invalid API key),
      // the chat session is still retained in the sidebar and the error is visible,
      // instead of the thread disappearing entirely because activeChatId was null.
      let currentChatId = activeChatIdRef.current;
      if (!currentChatId) {
        console.log("[SEND] No active chat, pre-creating session for agent:", agentId);
        try {
          // Await creating the chat on the backend
          const newSession = await api.createChat(agentId);
          currentChatId = newSession.chat_id;

          // CRITICAL: Update ref immediately so subsequent renders/events use it
          skipNextLoadRef.current = true; // Prevent loadMessages from clearing our optimistic state
          activeChatIdRef.current = currentChatId;
          setActiveChatId(currentChatId);

          // Optimistically add it to the sidebar so the user sees it immediately
          setSessions((prev) => [newSession, ...prev]);
        } catch (createErr) {
          console.error("[SEND] Pre-creation failed:", createErr);
          // Fallback to sending without pre-creation (legacy behavior)
        }
      }

      let requestContent = visibleContent;
      const attachedContext = await buildUploadedDocsContext();
      if (attachedContext) {
        requestContent = `${visibleContent}\n\n---\n\n${attachedContext}`;
      }

      // Pass the resolved currentChatId to the backend
      // Add a 120-second timeout to prevent hanging indefinitely
      const invokePromise = api.sendMessage(agentId, requestContent, userModel, currentChatId ?? undefined, userProvider, userApiKey, userBaseUrl);
      const timeoutPromise = new Promise<never>((_, reject) =>
        setTimeout(() => reject(new Error("Request timed out after 120 seconds. The provider may be slow or unresponsive.")), 120_000)
      );
      const response = await Promise.race([invokePromise, timeoutPromise]);

      // Check if this send was superseded (agent switch, abort)
      if (gen !== sendGenRef.current || abortCtrl.signal.aborted) {
        console.log("[SEND] stale generation or aborted — discarding result");
        return;
      }

      console.log("[SEND] invoke resolved! response:", {
        chat_id: response.chat_id,
        chat_title: response.chat_title,
        msg_id: response.message?.id,
        content_len: response.message?.content?.length,
        has_metadata: !!response.message?.metadata,
      });

      // If the backend returned a new chat_id and we missed pre-creation, adopt it
      if (response.chat_id && response.chat_id !== activeChatIdRef.current) {
        skipNextLoadRef.current = true;
        activeChatIdRef.current = response.chat_id;
        setActiveChatId(response.chat_id);
      }

      // Finalize the streaming message with server-side metadata
      // If streaming placeholder was somehow lost (e.g., history reload), append as new message
      setMessages((prev) => {
        const hasPlaceholder = prev.some((m) => m.id === streamMsgId);
        console.log("[SEND] finalization: hasPlaceholder=", hasPlaceholder, "prev.length=", prev.length, "ids=", prev.map(m => m.id));
        if (hasPlaceholder) {
          return prev.map((m) => {
            if (m.id !== streamMsgId) return m;
            const finalText = m.text || response.message.content || "(No response received — the provider may have filtered the content. Try rephrasing your message.)";
            return {
              ...m,
              id: response.message.id,
              text: finalText,
              tokens: response.message.metadata?.token_cost ?? m.tokens,
              cost: response.message.metadata?.cost_usd ?? m.cost,
              duration: response.message.metadata?.duration_ms,
              skills: response.message.metadata?.skills_activated ?? m.skills,
              isStreaming: false,
              retryStatus: undefined,
            };
          });
        }
        // Fallback: placeholder was lost — re-add user msg + assistant response
        const hasUserMsg = prev.some((m) => m.id === userMsg.id);
        const newMsgs = hasUserMsg ? [...prev] : [...prev, userMsg];
        newMsgs.push({
          id: response.message.id,
          role: "assistant" as const,
          text: response.message.content,
          time: new Date().toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }),
          agent: agentName,
          tokens: response.message.metadata?.token_cost,
          cost: response.message.metadata?.cost_usd,
          duration: response.message.metadata?.duration_ms,
          skills: response.message.metadata?.skills_activated,
          isStreaming: false,
        });
        return newMsgs;
      });

      // ── Optimistic sidebar update ────────────────────────────
      // Immediately show/update the session in the sidebar without
      // waiting for the backend listSessions round-trip.
      if (response.chat_id) {
        setSessions((prev) => {
          const existing = prev.find((s) => s.chat_id === response.chat_id);
          if (existing) {
            // Update existing session in-place
            return prev.map((s) =>
              s.chat_id === response.chat_id
                ? { ...s, title: response.chat_title || s.title, message_count: s.message_count + 2, last_activity: new Date().toISOString() }
                : s
            );
          }
          // Insert new session at the top (fallback if pre-creation missed)
          const newSession: SessionSummary = {
            chat_id: response.chat_id,
            agent_id: agentId,
            title: response.chat_title || visibleContent.slice(0, 60),
            message_count: 2,
            created_at: new Date().toISOString(),
            last_activity: new Date().toISOString(),
            pending_approvals: 0,
            routine_generated: false,
            has_proof_outputs: false,
            first_message_preview: visibleContent.slice(0, 80) || null,
          };
          return [newSession, ...prev];
        });
      }
      // Also refresh from backend (authoritative) with generation guard
      const sessGen = ++sessionsGenRef.current;
      api.listSessions().then((s) => {
        if (sessGen === sessionsGenRef.current) setSessions(s);
      }).catch(() => { });
    } catch (err) {
      console.error("[SEND] invoke REJECTED:", err);
      const errMsg = err instanceof Error ? err.message : String(err || "Failed to get response.");
      setLastError(errMsg);
      setMessages((prev) => {
        const hasPlaceholder = prev.some((m) => m.id === streamMsgId);
        if (hasPlaceholder) {
          return prev.map((m) =>
            m.id === streamMsgId
              ? { ...m, text: m.text || errMsg, isStreaming: false }
              : m
          );
        }
        // Fallback: add error message as new assistant bubble
        return [
          ...prev,
          {
            id: `err_${Date.now()}`,
            role: "assistant" as const,
            text: errMsg,
            time: new Date().toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }),
            agent: agentName,
            isStreaming: false,
          },
        ];
      });
      pushToast(errMsg);
    } finally {
      console.log("[SEND] finally block — clearing refs. gen:", gen, "sendGenRef:", sendGenRef.current);
      streamingMsgIdRef.current = null;
      // Always clear sending state. The gen check was previously used to prevent
      // a superseded send from clearing a newer send's state, but since sendMessage
      // guards against concurrent calls (sendingRef.current check at entry), only the
      // abort path can cause gen mismatch — and abort already clears sendingRef.
      // Unconditionally resetting here prevents sendingRef from ever getting stuck.
      sendingRef.current = false;
      activeSendAbortRef.current = null;
      // Note: skipNextLoadRef is intentionally NOT cleared here.
      // It is a one-shot flag consumed by the activeChatId effect.
      // Clearing it here would race with React's effect scheduling,
      // causing an unnecessary backend reload after new chat creation.
      // Snapshot current message IDs for semantic dedup.
      // This replaces the fragile Date.now() timestamp comparison.
      setMessages((current) => {
        lastFinalizedMsgIdsRef.current = new Set(current.map((m) => m.id));
        return current;
      });
      setIsSending(false);
    }
  }, [selectedAgentId, agent?.id, agent?.name, pushToast, effectiveModel, effectiveProvider, effectiveApiKey, effectiveBaseUrl, buildUploadedDocsContext]);

  return (
    <div className="view chat-page">

      {/* Body: optional sidebar + main chat */}
      <div className={`chat-page-body ${showSidebar ? "with-sidebar" : ""} ${traceOpen ? "with-trace" : ""} ${previewOpen ? "with-preview" : ""}`}>
        {/* Thread history sidebar */}
        {showSidebar && (
          <div className="chat-thread-sidebar">
            <div className="chat-thread-sidebar-head">
              <span className="chat-thread-sidebar-title">History</span>
              <button className="btn subtle" onClick={startNewThread} title="New thread" style={{ fontSize: 13, gap: 4, padding: "4px 8px" }}>
                <Icon name="plus" /> New
              </button>
            </div>
            <div className="chat-thread-sidebar-list">
              {(() => {
                const allSessions = sessions;
                if (allSessions.length === 0) {
                  return <div className="chat-thread-sidebar-empty">No threads yet</div>;
                }
                return allSessions.map((s, idx) => {
                  const timeRange = getTimeRange(s.last_activity);
                  const prevTimeRange = idx > 0 ? getTimeRange(allSessions[idx - 1].last_activity) : null;
                  const showHeader = idx === 0 || timeRange !== prevTimeRange;
                  return (
                    <div key={s.chat_id}>
                      {showHeader && (
                        <div className="chat-thread-sidebar-group-header">{timeRange}</div>
                      )}
                      <div
                        className={`chat-thread-sidebar-item ${activeChatId === s.chat_id ? "active" : ""}`}
                        role="button"
                        tabIndex={0}
                        onClick={() => selectSession(s)}
                        onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") selectSession(s); }}
                      >
                        <div className="chat-thread-sidebar-item-row">
                          <div className="chat-thread-sidebar-item-title">{s.title || "Untitled"}</div>
                          <button
                            className="chat-thread-sidebar-delete"
                            onClick={(e) => deleteSession(s.chat_id, e)}
                            title="Delete thread"
                          >
                            ×
                          </button>
                        </div>
                        <div className="chat-thread-sidebar-item-meta">
                          {s.message_count} msgs · started {new Date(s.created_at).toLocaleDateString([], { month: "short", day: "numeric" })}
                          {s.created_at !== s.last_activity && (
                            <> · last {new Date(s.last_activity).toLocaleDateString([], { month: "short", day: "numeric" })}</>
                          )}
                        </div>
                        {s.first_message_preview && (
                          <div className="chat-thread-sidebar-item-preview" title={s.first_message_preview}>
                            {s.first_message_preview}
                          </div>
                        )}
                      </div>
                    </div>
                  );
                });
              })()}
            </div>
          </div>
        )}

        {/* Main chat column */}
        <div className="chat-page-main">
          <div className="chat-stage-header">
            <div className="chat-stage-header__identity">
              <div className="chat-stage-header__avatar">{agent?.icon ?? "✦"}</div>
              <div className="chat-stage-header__copy">
                <div className="chat-stage-header__eyebrow">
                  {currentSession ? "Conversation" : "New Session"}
                </div>
                <div className="chat-stage-header__title-row">
                  <h2 className="chat-stage-header__title">
                    {currentSession?.title || (agent ? `Chat with ${agent.name}` : "Create an assistant to begin")}
                  </h2>
                  {agent && <span className="chat-stage-pill chat-stage-pill--brand">{agent.name}</span>}
                  {effectiveModel && <span className="chat-stage-pill">{effectiveModel}</span>}
                </div>
                <div className="chat-stage-header__meta">
                  <span>{currentSession ? `${currentSession.message_count} messages` : "Fresh context"}</span>
                  <span>{isSending ? "Agent responding" : "Ready"}</span>
                  <span>{effectiveProvider}</span>
                </div>
              </div>
            </div>

            <div className="chat-stage-header__actions">
              <button className={`chat-stage-action ${showSidebar ? "is-active" : ""}`} onClick={() => setShowSidebar(!showSidebar)} title="Toggle history">
                <Icon name="clock" />
                <span>History</span>
              </button>
              <button className="chat-stage-action" onClick={startNewThread} title="New thread">
                <Icon name="plus" />
                <span>New</span>
              </button>
              <button className={`chat-stage-action ${traceOpen ? "is-active" : ""}`} onClick={() => setTraceOpen(!traceOpen)} title="Toggle trace panel">
                <Icon name="activity" />
                <span>Trace</span>
              </button>
              <button className={`chat-stage-action ${previewOpen ? "is-active" : ""}`} onClick={() => setPreviewOpen(!previewOpen)} title="Toggle preview panel">
                <Icon name="eye" />
                <span>Preview</span>
              </button>
              <button className={`chat-stage-action ${showFlowSelector ? "is-active" : ""}`} onClick={() => setShowFlowSelector(!showFlowSelector)} title="Agent flow coordination">
                <Icon name="network" />
                <span>Flows</span>
              </button>
            </div>
          </div>

          <div className={`chat-page-messages ${isNew ? "is-empty-state" : ""}`} ref={messagesContainerRef}>
            {isNew ? (
              /* Hero empty state */
              <div className="chat-hero">
                <div className="chat-hero-orbit chat-hero-orbit--one" />
                <div className="chat-hero-orbit chat-hero-orbit--two" />
                <div className="chat-hero-content">
                  <div className="chat-hero-icon">✦</div>
                  <div className="chat-hero-kicker">Private workspace chat</div>
                  <h1 className="chat-hero-title">
                    What can I help you with?
                  </h1>
                  <p className="chat-hero-tagline">A cleaner, calmer place to think, build, and ship with your local assistant.</p>
                  <div className="chat-hero-signal-row">
                    <span className="chat-stage-pill chat-stage-pill--brand">{agent ? agent.name : "No agent selected"}</span>
                    <span className="chat-stage-pill">{effectiveProvider}</span>
                    <span className="chat-stage-pill">{effectiveModel || "Choose a model"}</span>
                  </div>
                </div>

                {agents.length === 0 ? (
                  /* No agents at all — prompt user to create one */
                  <div className="chat-hero-empty">
                    <p className="chat-hero-desc">
                      ClawDesk is your local AI workspace. Create an assistant to get started.
                    </p>

                    {/* Journey CTA */}
                    {onOpenJourney && (
                      <button
                        className="chat-hero-journey-btn"
                        onClick={() => onOpenJourney()}
                      >
                        <span className="journey-btn-icon">🎨</span>
                        <div className="journey-btn-text">
                          <span className="journey-btn-title">Design Your Agent</span>
                          <span className="journey-btn-desc">Step-by-step guided creation with team topology</span>
                        </div>
                        <span className="journey-btn-arrow">→</span>
                      </button>
                    )}

                    <div className="chat-hero-divider">
                      <span>or pick a template</span>
                    </div>

                    <div className="template-grid">
                      {AGENT_TEMPLATES.map((t) => (
                        <button
                          key={t.name}
                          className="template-tile minimal"
                          onClick={() => onCreateAgent(t)}
                        >
                          <div className="template-icon">{t.icon}</div>
                          <div className="template-info">
                            <div className="template-name">{t.name}</div>
                            <div className="template-desc">{t.description}</div>
                          </div>
                        </button>
                      ))}
                    </div>
                  </div>
                ) : (
                  /* Agent exists — show switcher + suggestions */
                  <>
                    <div className="chat-hero-actions">
                      <button
                        className="chat-hero-agent-pill"
                        onClick={() => setShowAgentPicker(!showAgentPicker)}
                      >
                        <span className="agent-icon">{agent?.icon ?? "⚡️"}</span>
                        <span className="agent-name">{agent?.name ?? "Select agent"}</span>
                        <span className="agent-chevron"><Icon name="collapse-left" /></span>
                      </button>
                      {onOpenJourney && (
                        <button
                          className="chat-hero-new-agent-btn"
                          onClick={() => onOpenJourney()}
                          title="Create a new agent or team"
                        >
                          <span>+</span> Create Agent
                        </button>
                      )}
                    </div>

                    {showAgentPicker && (
                      <div className="chat-agent-overlay" onClick={() => setShowAgentPicker(false)}>
                        <div className="chat-agent-picker" onClick={(e) => e.stopPropagation()}>
                          {(() => {
                            const soloAgents = agents.filter((a) => !a.team_id);
                            const teamMap = new Map<string, typeof agents>();
                            for (const a of agents) {
                              if (a.team_id) {
                                const list = teamMap.get(a.team_id) || [];
                                list.push(a);
                                teamMap.set(a.team_id, list);
                              }
                            }
                            return (
                              <>
                                {soloAgents.map((a) => (
                                  <button
                                    key={a.id}
                                    className={`chat-agent-option ${selectedAgentId === a.id ? "active" : ""}`}
                                    onClick={() => { onSelectAgent(a.id); setShowAgentPicker(false); }}
                                  >
                                    <span className="chat-agent-option-icon">{a.icon}</span>
                                    <div>
                                      <div className="chat-agent-option-name">{a.name}</div>
                                      <div className="chat-agent-option-meta">{a.model === "default" ? "Ready to use" : a.model}</div>
                                    </div>
                                    {a.status === "active" && <span className="status-dot status-ok" />}
                                  </button>
                                ))}

                                {[...teamMap.entries()].map(([teamId, teamAgents]) => {
                                  const router = teamAgents.find((a) => a.team_role === "router") || teamAgents[0];
                                  const isTeamSelected = teamAgents.some((a) => a.id === selectedAgentId);
                                  return (
                                    <div key={teamId} className="chat-agent-team-group">
                                      <button
                                        className={`chat-agent-option chat-agent-team-header ${isTeamSelected ? "active" : ""}`}
                                        onClick={() => { onSelectAgent(router.id); setShowAgentPicker(false); }}
                                      >
                                        <span className="chat-agent-option-icon">👥</span>
                                        <div>
                                          <div className="chat-agent-option-name">Team: {router.name}</div>
                                          <div className="chat-agent-option-meta">{teamAgents.length} agents · routes to team</div>
                                        </div>
                                      </button>
                                      <div className="chat-agent-team-members">
                                        {teamAgents.map((a) => (
                                          <button
                                            key={a.id}
                                            className={`chat-agent-option chat-agent-team-member ${selectedAgentId === a.id ? "active" : ""}`}
                                            onClick={() => { onSelectAgent(a.id); setShowAgentPicker(false); }}
                                          >
                                            <span className="chat-agent-option-icon">{a.icon}</span>
                                            <div>
                                              <div className="chat-agent-option-name">{a.name}</div>
                                              <div className="chat-agent-option-meta">{a.team_role || "member"}</div>
                                            </div>
                                            {a.status === "active" && <span className="status-dot status-ok" />}
                                          </button>
                                        ))}
                                      </div>
                                    </div>
                                  );
                                })}

                                {onOpenJourney && (
                                  <button
                                    className="chat-agent-option chat-agent-option-new"
                                    onClick={() => { setShowAgentPicker(false); onOpenJourney(); }}
                                  >
                                    <span className="chat-agent-option-icon">✨</span>
                                    <div>
                                      <div className="chat-agent-option-name">Create Agent</div>
                                      <div className="chat-agent-option-meta">Single agent or team</div>
                                    </div>
                                  </button>
                                )}
                              </>
                            );
                          })()}
                        </div>
                      </div>
                    )}

                    {/* Suggestion cards */}
                    <div className="chat-suggestions-grid">
                      {[
                        { icon: "✍️", title: "Write an email", desc: "Draft a professional response" },
                        { icon: "📋", title: "Summarize this", desc: "Give me the key points" },
                        { icon: "💡", title: "Brainstorm ideas", desc: "Help me think through options" },
                        { icon: "📅", title: "Plan my week", desc: "Organize tasks and priorities" },
                        { icon: "📝", title: "Write a document", desc: "Article, report, or proposal" },
                        { icon: "🔍", title: "Research a topic", desc: "Find and summarize information" }
                      ].map((s, i) => (
                        <button
                          key={i}
                          className="chat-suggestion-tile"
                          style={{ animationDelay: `${i * 60}ms` } as React.CSSProperties}
                          onClick={() => {
                            const prompt = s.title + ": " + s.desc;
                            if (agent && !isSending) {
                              sendMessage(prompt);
                            } else {
                              setInput(prompt);
                            }
                          }}
                        >
                          <div className="suggestion-icon">{s.icon}</div>
                          <div className="suggestion-content">
                            <div className="suggestion-title">{s.title}</div>
                            <div className="suggestion-desc">{s.desc}</div>
                          </div>
                        </button>
                      ))}
                    </div>
                  </>
                )}
              </div>
            ) : (
              /* Message list */
              <div className="chat-message-list">
                {lastError && (
                  <div className="chat-error-banner">
                    <span className="chat-error-banner__label">Issue</span>
                    <span className="chat-error-banner__message">{lastError}</span>
                    <button className="chat-error-banner__dismiss" onClick={() => setLastError(null)}>×</button>
                  </div>
                )}
                {messages.map((m) => {
                  const hasAssistantContent =
                    m.role !== "assistant"
                    || Boolean(m.text.trim())
                    || Boolean(m.thinkingText)
                    || Boolean(m.retryStatus)
                    || Boolean(m.askHuman)
                    || Boolean(m.toolCalls && m.toolCalls.length > 0)
                    || Boolean(m.tokens || m.cost || m.duration || (m.skills && m.skills.length > 0));

                  if (!hasAssistantContent) {
                    return null;
                  }

                  return (
                  <div key={m.id} className={`chat-msg-row ${m.role === "user" ? "is-user" : "is-assistant"} ${m.isStreaming ? "streaming" : ""}`}>
                    <div className="chat-msg-header">
                      {m.role === "user" ? (
                        <div className="chat-avatar user-avatar">
                          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5">
                            <path strokeLinecap="round" strokeLinejoin="round" d="M20 21v-2a4 4 0 0 0-4-4H8a4 4 0 0 0-4 4v2" />
                            <circle cx="12" cy="7" r="4" strokeLinecap="round" strokeLinejoin="round" />
                          </svg>
                        </div>
                      ) : (
                        <div className="chat-avatar agent-avatar">
                          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5">
                            <rect x="3" y="11" width="18" height="10" rx="2" strokeLinecap="round" strokeLinejoin="round" />
                            <circle cx="12" cy="5" r="2" strokeLinecap="round" strokeLinejoin="round" />
                            <path d="M12 7v4" strokeLinecap="round" strokeLinejoin="round" />
                            <line x1="8" y1="16" x2="8.01" y2="16" strokeLinecap="round" strokeLinejoin="round" strokeWidth="3" />
                            <line x1="16" y1="16" x2="16.01" y2="16" strokeLinecap="round" strokeLinejoin="round" strokeWidth="3" />
                          </svg>
                        </div>
                      )}
                      <div className="chat-msg-sender-block">
                        <span className={`chat-msg-sender ${m.role === "user" ? "user-sender" : "agent-sender"}`}>
                          {m.role === "user" ? "You" : m.agent || agent?.name || "Assistant"}
                        </span>
                        <span className="chat-msg-time">{m.time}</span>
                      </div>
                    </div>

                    <div className={`chat-msg-card ${m.role === "user" ? "user-card" : "assistant-card"}`}>
                      {m.role === "assistant" && m.toolCalls && m.toolCalls.length > 0 && (
                        <div className="chat-msg-tools">
                          {m.toolCalls.map((tc, i) => (
                            <ToolBrick
                              key={tc.id || `${tc.name}-${i}`}
                              icon={tc.status === "running" ? "loader" : tc.status === "done" ? "check" : "alert-circle"}
                              name={tc.name}
                              status={tc.status}
                              result={tc.result}
                              onTraceClick={() => {
                                setTraceFilter(tc.name);
                                setSidePanelTab("trace");
                                setTraceOpen(true);
                              }}
                            />
                          ))}
                        </div>
                      )}

                      {m.role === "assistant" && m.askHuman && (
                        <AskHumanCard askHuman={m.askHuman} onRespond={handleAskHumanRespond} />
                      )}

                      <div className="chat-msg-content">
                        {m.role === "assistant" && m.thinkingText && (
                          <ThinkingBlock text={m.thinkingText} />
                        )}
                        {m.role === "assistant" && m.retryStatus && !m.text && (
                          <div className="chat-retry-status">
                            <Icon name="loader" className="spin" />
                            <span>{m.retryStatus}</span>
                          </div>
                        )}
                        <MarkdownContent content={m.text} isStreaming={m.isStreaming} />
                      </div>

                      {m.role === "assistant" && (m.tokens || m.cost || m.duration || (m.skills && m.skills.length > 0)) && (
                        <div className="chat-msg-meta">
                          {m.tokens != null && <span className="meta-badge">{m.tokens.toLocaleString()} tokens</span>}
                          {m.cost != null && <span className="meta-badge">${m.cost.toFixed(4)}</span>}
                          {m.duration != null && <span className="meta-badge">{(m.duration / 1000).toFixed(1)}s</span>}
                          {m.skills?.map((s) => (
                            <span key={s} className="meta-badge skill-badge">{s}</span>
                          ))}
                        </div>
                      )}
                    </div>
                  </div>
                );})}
                {isSending && (() => {
                  const streamMsg = messages.find((m) => m.id === streamingMsgIdRef.current);
                  const showThinking = !streamMsg || (streamMsg.isStreaming && !streamMsg.text);
                  if (!showThinking) return null;
                  return (
                    <div className="chat-msg-row chat-thinking-indicator">
                      <div className="chat-msg-card assistant-card chat-msg-card--thinking">
                        <div className="chat-msg-content">
                          <div className="typing-dots">
                            <span />
                            <span />
                            <span />
                          </div>
                        </div>
                      </div>
                    </div>
                  );
                })()}
                <div ref={messagesEndRef} />
              </div>
            )}

            {/* Scroll to bottom */}
            {showScrollBtn && !isNew && (
              <button
                className="chat-scroll-bottom-btn"
                onClick={() => messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' })}
                aria-label="Scroll to bottom"
              >
                ↓
              </button>
            )}
          </div>

          {/* Composer */}
          <div className={`chat-composer-wrap ${isNew ? "is-empty-state" : ""}`}>
            <div className="chat-composer-inner-wrap">
              <div className="chat-composer">
                <textarea
                  className="chat-composer-input"
                  placeholder={agent ? "Type a message..." : "Create an assistant to get started"}
                  value={input}
                  onChange={(e) => setInput(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" && !e.shiftKey) {
                      e.preventDefault();
                      if (input.trim() && agent && !isSending) {
                        // Defensive: if sendingRef is stuck true but isSending is false,
                        // force-clear the ref to recover from inconsistent state
                        if (sendingRef.current) {
                          console.warn("[UI] sendingRef stuck true while isSending=false — force-resetting");
                          sendingRef.current = false;
                        }
                        sendMessage(input.trim());
                        setInput("");
                      }
                    }
                  }}
                  rows={2}
                  disabled={!agent}
                />
                {uploadedContextDocs.length > 0 && (
                  <div className="chat-composer-attachments" aria-label="Uploaded context files">
                    {uploadedContextDocs.map((doc) => (
                      <span key={doc.id} className="chat-composer-chip">
                        <Icon name="paperclip" />
                        {doc.filename}
                        <button
                          className="chat-composer-chip-remove"
                          aria-label={`Remove ${doc.filename}`}
                          title="Remove file"
                          onClick={() => setUploadedContextDocs((prev) => prev.filter((d) => d.id !== doc.id))}
                        >
                          ×
                        </button>
                      </span>
                    ))}
                  </div>
                )}
                <div className="chat-composer-actions">
                  <div className="chat-composer-left">
                    <ModelSelector
                      currentModel={effectiveModel}
                      currentProviderId={effectiveProviderId}
                      providerConfigs={providerConfigs}
                      onSelect={setPerChatModel}
                      disabled={isSending}
                    />
                    <div className="badge-safe" title="Safe Mode is on — your data stays on your device and is never sent to external servers without your permission.">
                      <Icon name="shield" /> Safe Mode
                    </div>
                    {selectedFlowId && (
                      <div className="badge-safe" style={{ background: 'var(--accent)', color: 'white', opacity: 0.85 }} title="Routing through selected agent flow">
                        <Icon name="network" /> Flow
                      </div>
                    )}
                    {isSending && <span className="chat-composer-status">Streaming response</span>}
                    {isUploadingFiles && <span className="chat-composer-status">Uploading files</span>}
                  </div>
                  <div className="chat-composer-right">
                    <button
                      className="chat-upload-btn"
                      disabled={isUploadingFiles}
                      onClick={() => void openComposerFilePicker()}
                      title="Upload files for RAG context"
                    >
                      <Icon name="paperclip" />
                      <span>{isUploadingFiles ? "Uploading..." : "Upload files"}</span>
                    </button>
                    <VoiceInput
                      onTranscription={(text) => {
                        if (text && agent && !isSending) {
                          sendMessage(text);
                        } else {
                          setInput((prev) => (prev ? prev + " " + text : text));
                        }
                      }}
                      disabled={isSending || !agent}
                    />
                    {isSending ? (
                      <button
                        className="chat-send-btn chat-stop-btn active"
                        onClick={stopMessage}
                        title="Stop response"
                      >
                        <Icon name="pause" />
                      </button>
                    ) : (
                      <button
                        className={`chat-send-btn ${input.trim() && agent ? "active" : ""}`}
                        disabled={!agent || !input.trim()}
                        onClick={() => {
                          console.log("[UI] send button clicked. input:", input.trim()?.slice(0, 30), "isSending:", isSending, "sendingRef:", sendingRef.current);
                          if (input.trim() && !isSending) {
                            if (sendingRef.current) {
                              console.warn("[UI] sendingRef stuck true while isSending=false — force-resetting");
                              sendingRef.current = false;
                            }
                            sendMessage(input.trim());
                            setInput("");
                          }
                        }}
                      >
                        <Icon name="send" />
                      </button>
                    )}
                  </div>
                </div>
              </div>
            </div>
          </div>
        </div>{/* end chat-page-main */}

        {/* Side panel (right side - Trace / Tasks / Skills tabs) */}
        {traceOpen && (
          <SidePanel
            tab={sidePanelTab}
            onTabChange={setSidePanelTab}
            traceEntries={traceEntries}
            traceFilter={traceFilter}
            onTraceFilterChange={setTraceFilter}
            activeTasks={activeTasks}
            skillUsages={skillUsages}
            isSending={isSending}
            onCancelRun={async () => {
              await api.cancelActiveRun(activeChatIdRef.current ?? undefined);
            }}
            onClose={() => setTraceOpen(false)}
          />
        )}

        {/* Preview panel (live web app preview) */}
        {previewOpen && (
          <div className="chat-preview-shell">
            <PreviewPanel onClose={() => setPreviewOpen(false)} />
          </div>
        )}

        {/* Agent Flow Selector panel (Paperclip-style coordination) */}
        {showFlowSelector && (
          <div className="chat-preview-shell" style={{ maxWidth: 320, minWidth: 280 }}>
            <div className="modal-head" style={{ padding: "8px 12px" }}>
              <span style={{ fontSize: 13, fontWeight: 600, color: "var(--text)" }}>Agent Flows</span>
              <button
                className="btn ghost"
                style={{ fontSize: 11, padding: "2px 6px" }}
                onClick={() => setShowFlowSelector(false)}
              >
                ✕
              </button>
            </div>
            <AgentFlowSelector
              selectedFlowId={selectedFlowId}
              onSelectFlow={setSelectedFlowId}
              pushToast={pushToast}
            />
          </div>
        )}
      </div>{/* end chat-page-body */}

      {/* Terminal panel */}
      <XTerminal visible={showTerminal} onClose={() => setShowTerminal(false)} />
    </div>
  );
}
