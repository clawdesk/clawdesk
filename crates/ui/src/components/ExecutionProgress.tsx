import { useState, useEffect, useRef, useMemo, useCallback } from "react";

// ── Types ─────────────────────────────────────────────────────

export type EventKind =
  | "RoundStart"
  | "Response"
  | "ToolStart"
  | "ToolEnd"
  | "StreamChunk"
  | "ThinkingChunk"
  | "Compaction"
  | "Done"
  | "Error"
  | "PromptAssembled"
  | "IdentityVerified"
  | "ContextGuardAction"
  | "FallbackTriggered";

export interface ExecutionEvent {
  id: string;
  timestamp: number;
  kind: EventKind;
  detail: string;
  meta?: Record<string, unknown>;
}

export interface ExecutionProgressProps {
  /** Whether the agent is currently running */
  isRunning: boolean;
  /** Stream of execution events */
  events: ExecutionEvent[];
  /** Current round number */
  round: number;
  /** Max rounds allowed */
  maxRounds: number;
  /** Agent name for display */
  agentName?: string;
  /** Tokens used so far */
  tokensUsed?: number;
  /** Token budget */
  tokenBudget?: number;
  /** Whether to start collapsed */
  defaultCollapsed?: boolean;
}

// ── Helpers ───────────────────────────────────────────────────

function eventIcon(kind: EventKind): string {
  switch (kind) {
    case "RoundStart": return "🔄";
    case "Response": return "💬";
    case "ToolStart": return "🔧";
    case "ToolEnd": return "✅";
    case "StreamChunk": return "📝";
    case "ThinkingChunk": return "🧠";
    case "Compaction": return "📦";
    case "Done": return "✓";
    case "Error": return "❌";
    case "PromptAssembled": return "📋";
    case "IdentityVerified": return "🔒";
    case "ContextGuardAction": return "🛡️";
    case "FallbackTriggered": return "⚡";
    default: return "•";
  }
}

function eventColor(kind: EventKind): string {
  switch (kind) {
    case "RoundStart": return "#58a6ff";
    case "Response": return "#3fb950";
    case "ToolStart": return "#d29922";
    case "ToolEnd": return "#d29922";
    case "StreamChunk": return "#8b949e";
    case "ThinkingChunk": return "#bc8cff";
    case "Done": return "#3fb950";
    case "Error": return "#f85149";
    case "Compaction": return "#8b949e";
    default: return "#c9d1d9";
  }
}

function formatElapsed(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  if (ms < 60000) return `${(ms / 1000).toFixed(1)}s`;
  return `${Math.floor(ms / 60000)}m ${Math.round((ms % 60000) / 1000)}s`;
}

// ── Component ─────────────────────────────────────────────────

export function ExecutionProgress({
  isRunning,
  events,
  round,
  maxRounds,
  agentName,
  tokensUsed = 0,
  tokenBudget = 128000,
  defaultCollapsed = false,
}: ExecutionProgressProps) {
  const [collapsed, setCollapsed] = useState(defaultCollapsed);
  const [showEvents, setShowEvents] = useState(false);
  const eventEndRef = useRef<HTMLDivElement>(null);
  const startTimeRef = useRef<number>(Date.now());

  // Reset start time when execution begins
  useEffect(() => {
    if (isRunning) startTimeRef.current = Date.now();
  }, [isRunning]);

  // Auto-scroll events
  useEffect(() => {
    if (showEvents) {
      eventEndRef.current?.scrollIntoView({ behavior: "smooth" });
    }
  }, [events.length, showEvents]);

  // Derived stats
  const toolCalls = useMemo(
    () => events.filter((e) => e.kind === "ToolStart").length,
    [events]
  );
  const errorCount = useMemo(
    () => events.filter((e) => e.kind === "Error").length,
    [events]
  );
  const lastEvent = events[events.length - 1];
  const elapsed = Date.now() - startTimeRef.current;
  const tokenPct = tokenBudget > 0 ? Math.min(100, (tokensUsed / tokenBudget) * 100) : 0;
  const roundPct = maxRounds > 0 ? Math.min(100, (round / maxRounds) * 100) : 0;

  // Active tool
  const activeTool = useMemo(() => {
    for (let i = events.length - 1; i >= 0; i--) {
      if (events[i].kind === "ToolStart") return events[i].detail;
      if (events[i].kind === "ToolEnd") return null;
    }
    return null;
  }, [events]);

  // Current phase
  const currentPhase = useMemo((): string => {
    if (!isRunning) return lastEvent?.kind === "Error" ? "Failed" : "Complete";
    if (activeTool) return `Running: ${activeTool}`;
    if (lastEvent?.kind === "ThinkingChunk") return "Thinking...";
    if (lastEvent?.kind === "StreamChunk") return "Generating response...";
    if (lastEvent?.kind === "RoundStart") return `Round ${round}`;
    if (lastEvent?.kind === "PromptAssembled") return "Preparing prompt...";
    if (lastEvent?.kind === "IdentityVerified") return "Identity verified";
    return "Processing...";
  }, [isRunning, activeTool, lastEvent, round]);

  if (collapsed) {
    return (
      <button
        onClick={() => setCollapsed(false)}
        style={{
          display: "flex", alignItems: "center", gap: 8,
          padding: "6px 12px", background: "var(--surface)",
          border: "1px solid var(--border)", borderRadius: 8,
          cursor: "pointer", fontSize: 12, color: "var(--text-secondary)",
          width: "100%", textAlign: "left",
        }}
      >
        {isRunning && (
          <span className="pulse-dot" style={{
            width: 8, height: 8, borderRadius: "50%",
            background: "#3fb950", animation: "pulse 1.5s infinite",
          }} />
        )}
        <span style={{ flex: 1 }}>
          {agentName ? `${agentName}: ` : ""}{currentPhase}
        </span>
        <span style={{ opacity: 0.6 }}>
          R{round}/{maxRounds} · {toolCalls} tools · {formatElapsed(elapsed)}
        </span>
        <span style={{ fontSize: 10 }}>▼</span>
      </button>
    );
  }

  return (
    <div style={{
      background: "var(--surface)", border: "1px solid var(--border)",
      borderRadius: 10, overflow: "hidden",
      fontSize: 12,
    }}>
      {/* ── Header ──────────────────────────── */}
      <div
        onClick={() => setCollapsed(true)}
        style={{
          display: "flex", alignItems: "center", gap: 8,
          padding: "8px 12px", cursor: "pointer",
          borderBottom: "1px solid var(--border)",
          background: isRunning ? "rgba(63, 185, 80, 0.06)" : "transparent",
        }}
      >
        {isRunning && (
          <span style={{
            width: 8, height: 8, borderRadius: "50%",
            background: "#3fb950",
            boxShadow: "0 0 6px rgba(63,185,80,0.5)",
            animation: "pulse 1.5s infinite",
          }} />
        )}
        <span style={{ fontWeight: 600, flex: 1 }}>
          {agentName ? `${agentName} ` : ""}Execution
          {!isRunning && lastEvent?.kind === "Done" && " ✓"}
          {!isRunning && lastEvent?.kind === "Error" && " ✗"}
        </span>
        <span style={{ color: "var(--text-tertiary)" }}>{formatElapsed(elapsed)}</span>
        <span style={{ fontSize: 10, color: "var(--text-tertiary)" }}>▲</span>
      </div>

      {/* ── Progress bars ───────────────────── */}
      <div style={{ padding: "10px 12px", display: "flex", flexDirection: "column", gap: 8 }}>
        {/* Round progress */}
        <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <span style={{ minWidth: 55, color: "var(--text-tertiary)" }}>Rounds</span>
          <div style={{
            flex: 1, height: 6, background: "var(--border)", borderRadius: 3,
            overflow: "hidden",
          }}>
            <div style={{
              width: `${roundPct}%`, height: "100%",
              background: roundPct > 80 ? "#f85149" : "#58a6ff",
              borderRadius: 3, transition: "width 0.3s",
            }} />
          </div>
          <span style={{ minWidth: 50, textAlign: "right", fontFamily: "monospace" }}>
            {round}/{maxRounds}
          </span>
        </div>

        {/* Token progress */}
        <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <span style={{ minWidth: 55, color: "var(--text-tertiary)" }}>Tokens</span>
          <div style={{
            flex: 1, height: 6, background: "var(--border)", borderRadius: 3,
            overflow: "hidden",
          }}>
            <div style={{
              width: `${tokenPct}%`, height: "100%",
              background: tokenPct > 80 ? "#f85149" : tokenPct > 60 ? "#d29922" : "#3fb950",
              borderRadius: 3, transition: "width 0.3s",
            }} />
          </div>
          <span style={{ minWidth: 50, textAlign: "right", fontFamily: "monospace" }}>
            {(tokensUsed / 1000).toFixed(0)}k
          </span>
        </div>

        {/* Current phase */}
        <div style={{
          display: "flex", alignItems: "center", gap: 6,
          padding: "4px 8px", background: "var(--bg)", borderRadius: 6,
        }}>
          {isRunning && activeTool && <span>🔧</span>}
          {isRunning && !activeTool && lastEvent?.kind === "ThinkingChunk" && <span>🧠</span>}
          {isRunning && !activeTool && lastEvent?.kind === "StreamChunk" && <span>📝</span>}
          <span style={{ flex: 1, fontWeight: 500 }}>{currentPhase}</span>
        </div>

        {/* Quick stats */}
        <div style={{
          display: "flex", gap: 12, color: "var(--text-tertiary)",
          padding: "2px 0",
        }}>
          <span>🔧 {toolCalls} tool calls</span>
          {errorCount > 0 && <span style={{ color: "#f85149" }}>❌ {errorCount} errors</span>}
          <span>🔄 {round} rounds</span>
        </div>
      </div>

      {/* ── Event timeline toggle ───────────── */}
      <div style={{ borderTop: "1px solid var(--border)" }}>
        <button
          onClick={() => setShowEvents(!showEvents)}
          style={{
            width: "100%", padding: "6px 12px",
            border: "none", background: "none",
            cursor: "pointer", fontSize: 11,
            color: "var(--text-tertiary)",
            display: "flex", alignItems: "center", justifyContent: "center", gap: 4,
          }}
        >
          {showEvents ? "Hide" : "Show"} event timeline ({events.length} events)
          <span>{showEvents ? "▲" : "▼"}</span>
        </button>

        {showEvents && (
          <div style={{
            maxHeight: 200, overflowY: "auto", padding: "0 12px 8px",
          }}>
            {events.map((ev) => (
              <div key={ev.id} style={{
                display: "flex", gap: 6, padding: "3px 0",
                borderBottom: "1px solid var(--border)",
                alignItems: "center",
              }}>
                <span style={{ fontSize: 11 }}>{eventIcon(ev.kind)}</span>
                <span style={{
                  fontFamily: "monospace", fontSize: 10, minWidth: 55,
                  color: "var(--text-tertiary)",
                }}>
                  {new Date(ev.timestamp).toLocaleTimeString([], {
                    hour: "2-digit", minute: "2-digit", second: "2-digit",
                  })}
                </span>
                <span style={{
                  fontWeight: 600, minWidth: 90,
                  color: eventColor(ev.kind),
                  fontSize: 11,
                }}>
                  {ev.kind}
                </span>
                <span style={{ flex: 1, color: "var(--text-secondary)", fontSize: 11 }}>
                  {ev.detail.length > 80 ? ev.detail.slice(0, 80) + "…" : ev.detail}
                </span>
              </div>
            ))}
            <div ref={eventEndRef} />
          </div>
        )}
      </div>

      {/* Pulse animation */}
      <style>{`
        @keyframes pulse {
          0%, 100% { opacity: 1; }
          50% { opacity: 0.4; }
        }
      `}</style>
    </div>
  );
}

// ── Compact inline progress (for embedding in message bubbles) ──

export interface InlineProgressProps {
  isRunning: boolean;
  currentPhase: string;
  round: number;
  maxRounds: number;
  toolName?: string | null;
  elapsed?: number;
}

export function InlineProgress({
  isRunning,
  currentPhase,
  round,
  maxRounds,
  toolName,
  elapsed = 0,
}: InlineProgressProps) {
  if (!isRunning) return null;

  return (
    <div style={{
      display: "flex", alignItems: "center", gap: 8,
      padding: "6px 10px", fontSize: 11,
      color: "var(--text-tertiary)", background: "var(--surface)",
      borderRadius: 6, marginTop: 4,
    }}>
      <span style={{
        width: 6, height: 6, borderRadius: "50%",
        background: "#3fb950", animation: "pulse 1.5s infinite",
      }} />
      {toolName ? (
        <span>🔧 <strong style={{ color: "var(--text-secondary)" }}>{toolName}</strong></span>
      ) : (
        <span>{currentPhase}</span>
      )}
      <span style={{ marginLeft: "auto" }}>
        R{round}/{maxRounds}
        {elapsed > 0 && ` · ${formatElapsed(elapsed)}`}
      </span>
    </div>
  );
}
