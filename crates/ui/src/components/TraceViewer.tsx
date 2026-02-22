import { useState, useEffect, useCallback, useMemo } from "react";
import * as api from "../api";
import type { TraceRunInfo, TraceSpanInfo, TraceEntry } from "../types";

// ── Types ─────────────────────────────────────────────────────

type ViewMode = "tree" | "timeline" | "raw";

interface SpanNode extends TraceSpanInfo {
  children: SpanNode[];
  depth: number;
}

// ── Helpers ───────────────────────────────────────────────────

function buildSpanTree(spans: TraceSpanInfo[]): SpanNode[] {
  const byId = new Map<string, SpanNode>();
  const roots: SpanNode[] = [];

  for (const span of spans) {
    byId.set(span.span_id, { ...span, children: [], depth: 0 });
  }

  for (const node of byId.values()) {
    if (node.parent_span_id && byId.has(node.parent_span_id)) {
      const parent = byId.get(node.parent_span_id)!;
      parent.children.push(node);
      node.depth = parent.depth + 1;
    } else {
      roots.push(node);
    }
  }

  const sortChildren = (nodes: SpanNode[]) => {
    nodes.sort((a, b) => a.start_time - b.start_time);
    for (const n of nodes) sortChildren(n.children);
  };
  sortChildren(roots);
  return roots;
}

function formatDuration(us: number | null): string {
  if (us === null || us === undefined) return "—";
  if (us < 1000) return `${us}µs`;
  if (us < 1_000_000) return `${(us / 1000).toFixed(1)}ms`;
  return `${(us / 1_000_000).toFixed(2)}s`;
}

function spanTypeColor(kind: string): string {
  switch (kind.toLowerCase()) {
    case "internal": return "#7c3aed";
    case "server": return "#2563eb";
    case "client": return "#e8612c";
    case "producer": return "#1a8754";
    case "consumer": return "#e8a817";
    default: return "#6b7280";
  }
}

function spanNameIcon(name: string): string {
  const lower = name.toLowerCase();
  if (lower.includes("llm") || lower.includes("chat") || lower.includes("completion")) return "🧠";
  if (lower.includes("tool") || lower.includes("execute")) return "🔧";
  if (lower.includes("plan") || lower.includes("reason")) return "💡";
  if (lower.includes("retrieve") || lower.includes("search")) return "🔍";
  if (lower.includes("embed")) return "📐";
  if (lower.includes("error") || lower.includes("fail")) return "❌";
  return "⚡";
}

// ── Props ─────────────────────────────────────────────────────

export interface TraceViewerProps {
  traceId?: string;
  onClose: () => void;
  pushToast: (text: string) => void;
}

// ── Component ─────────────────────────────────────────────────

export function TraceViewer({ traceId: initialTraceId, onClose, pushToast }: TraceViewerProps) {
  // Recent traces list
  const [recentTraces, setRecentTraces] = useState<TraceEntry[]>([]);
  const [loadingList, setLoadingList] = useState(false);
  const [resolvingTraceId, setResolvingTraceId] = useState<string | null>(null);

  // Selected trace detail
  const [activeTraceId, setActiveTraceId] = useState<string | null>(initialTraceId ?? null);
  const [viewMode, setViewMode] = useState<ViewMode>("tree");
  const [run, setRun] = useState<TraceRunInfo | null>(null);
  const [spans, setSpans] = useState<TraceSpanInfo[]>([]);
  const [selectedSpanId, setSelectedSpanId] = useState<string | null>(null);
  const [expandedSpans, setExpandedSpans] = useState<Set<string>>(new Set());
  const [loadingDetail, setLoadingDetail] = useState(false);

  // ── Load recent traces ────────────────────────────────────

  useEffect(() => {
    setLoadingList(true);
    api.getAgentTrace()
      .then((entries) => setRecentTraces(entries))
      .catch(() => {
        // silently handle — may not have traces yet
        setRecentTraces([]);
      })
      .finally(() => setLoadingList(false));
  }, []);

  // ── Load trace detail ─────────────────────────────────────

  useEffect(() => {
    if (!activeTraceId) {
      setRun(null);
      setSpans([]);
      return;
    }
    setLoadingDetail(true);
    Promise.all([
      api.traceGetRun(activeTraceId),
      api.traceGetSpans(activeTraceId),
    ]).then(([runInfo, spanInfos]) => {
      setRun(runInfo);
      setSpans(spanInfos);
      const roots = spanInfos.filter((s) => !s.parent_span_id);
      setExpandedSpans(new Set(roots.map((s) => s.span_id)));
    }).catch(() => {
      pushToast("Failed to load trace data.");
      setRun(null);
      setSpans([]);
    }).finally(() => {
      setLoadingDetail(false);
    });
  }, [activeTraceId, pushToast]);

  // ── Tree ──────────────────────────────────────────────────

  const tree = useMemo(() => buildSpanTree(spans), [spans]);

  const toggleExpand = useCallback((spanId: string) => {
    setExpandedSpans((prev) => {
      const next = new Set(prev);
      if (next.has(spanId)) next.delete(spanId);
      else next.add(spanId);
      return next;
    });
  }, []);

  const expandAll = useCallback(() => {
    setExpandedSpans(new Set(spans.map((s) => s.span_id)));
  }, [spans]);

  const collapseAll = useCallback(() => {
    setExpandedSpans(new Set());
  }, []);

  // ── Timeline calculations ─────────────────────────────────

  const timeRange = useMemo(() => {
    if (spans.length === 0) return { start: 0, end: 1 };
    const start = Math.min(...spans.map((s) => s.start_time));
    const end = Math.max(...spans.map((s) => (s.end_time ?? s.start_time) + (s.duration_us ?? 0)));
    return { start, end: Math.max(end, start + 1) };
  }, [spans]);

  const selectedSpan = useMemo(
    () => spans.find((s) => s.span_id === selectedSpanId) ?? null,
    [spans, selectedSpanId]
  );

  const extractTraceIdCandidates = useCallback((entry: TraceEntry): string[] => {
    const fromFields = [entry.detail, entry.event]
      .map((v) => (v || "").trim())
      .filter(Boolean);

    const joined = `${entry.event} ${entry.detail}`;
    const uuidMatches = joined.match(/[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}/gi) ?? [];
    const tracePrefixed = joined.match(/(?:trace(?:[_:-]|\s+id(?:=|:)?\s*))[A-Za-z0-9_-]+/gi) ?? [];
    const normalizedPrefixed = tracePrefixed
      .map((raw) => {
        const parts = raw.split(/[_:-]|\s+id(?:=|:)?\s*/i);
        return parts[parts.length - 1]?.trim() ?? "";
      })
      .filter(Boolean);

    return Array.from(new Set([...fromFields, ...uuidMatches, ...normalizedPrefixed]));
  }, []);

  const openTraceFromEntry = useCallback(async (entry: TraceEntry) => {
    const candidates = extractTraceIdCandidates(entry);
    if (candidates.length === 0) {
      pushToast("No trace ID found in this log entry.");
      return;
    }

    setResolvingTraceId(entry.timestamp + entry.event + entry.detail);
    try {
      for (const candidate of candidates) {
        try {
          const runInfo = await api.traceGetRun(candidate);
          if (runInfo) {
            setActiveTraceId(candidate);
            return;
          }

          const spanInfos = await api.traceGetSpans(candidate);
          if (spanInfos.length > 0) {
            setActiveTraceId(candidate);
            return;
          }
        } catch {
          // try next candidate
        }
      }
      pushToast("This entry does not map to a stored trace run.");
    } finally {
      setResolvingTraceId(null);
    }
  }, [extractTraceIdCandidates, pushToast]);

  // ── Render helpers ────────────────────────────────────────

  function renderTreeNode(node: SpanNode): JSX.Element {
    const isExpanded = expandedSpans.has(node.span_id);
    const hasChildren = node.children.length > 0;
    const isSelected = selectedSpanId === node.span_id;

    return (
      <div key={node.span_id} className="trace-tree-node">
        <div
          className={`trace-tree-row${isSelected ? " selected" : ""}`}
          style={{ paddingLeft: node.depth * 24 + 8 }}
          onClick={() => setSelectedSpanId(node.span_id)}
        >
          {hasChildren ? (
            <button
              className="trace-tree-toggle"
              onClick={(e) => { e.stopPropagation(); toggleExpand(node.span_id); }}
            >
              {isExpanded ? "▾" : "▸"}
            </button>
          ) : (
            <span className="trace-tree-toggle-spacer" />
          )}
          <span className="trace-tree-icon">{spanNameIcon(node.name)}</span>
          <span className="trace-tree-name">{node.name}</span>
          <span
            className="trace-tree-kind"
            style={{ background: spanTypeColor(node.kind), color: "#fff" }}
          >
            {node.kind}
          </span>
          <span className="trace-tree-duration">
            {formatDuration(node.duration_us)}
          </span>
        </div>
        {isExpanded && hasChildren && (
          <div className="trace-tree-children">
            {node.children.map((child) => renderTreeNode(child))}
          </div>
        )}
      </div>
    );
  }

  function renderTimeline(): JSX.Element {
    const totalDuration = timeRange.end - timeRange.start;
    const flatSpans = spans.slice().sort((a, b) => a.start_time - b.start_time);

    return (
      <div className="trace-timeline">
        <div className="trace-timeline-ruler">
          {[0, 0.25, 0.5, 0.75, 1].map((pct) => (
            <span key={pct} className="trace-timeline-tick" style={{ left: `${pct * 100}%` }}>
              {formatDuration(Math.round(pct * totalDuration))}
            </span>
          ))}
        </div>
        <div className="trace-timeline-bars">
          {flatSpans.map((span) => {
            const left = ((span.start_time - timeRange.start) / totalDuration) * 100;
            const width = Math.max(((span.duration_us ?? 0) / totalDuration) * 100, 0.5);
            const isSelected = selectedSpanId === span.span_id;

            return (
              <div
                key={span.span_id}
                className={`trace-timeline-row${isSelected ? " selected" : ""}`}
                onClick={() => setSelectedSpanId(span.span_id)}
              >
                <span className="trace-timeline-label">{span.name}</span>
                <div className="trace-timeline-track">
                  <div
                    className="trace-timeline-bar"
                    style={{
                      left: `${left}%`,
                      width: `${width}%`,
                      background: spanTypeColor(span.kind),
                    }}
                    title={`${span.name}: ${formatDuration(span.duration_us)}`}
                  />
                </div>
              </div>
            );
          })}
        </div>
      </div>
    );
  }

  function renderSpanInspector(): JSX.Element | null {
    if (!selectedSpan) {
      return (
        <div className="trace-inspector-empty">
          <p>Select a span to view details.</p>
        </div>
      );
    }

    return (
      <div className="trace-inspector">
        <div className="trace-inspector-header">
          <h3>{spanNameIcon(selectedSpan.name)} {selectedSpan.name}</h3>
          <button className="btn ghost" onClick={() => setSelectedSpanId(null)}>✕</button>
        </div>

        <div className="trace-inspector-grid">
          <div className="trace-inspector-item">
            <span className="trace-inspector-label">Span ID</span>
            <span className="trace-inspector-value mono">{selectedSpan.span_id}</span>
          </div>
          <div className="trace-inspector-item">
            <span className="trace-inspector-label">Trace ID</span>
            <span className="trace-inspector-value mono">{selectedSpan.trace_id}</span>
          </div>
          {selectedSpan.parent_span_id && (
            <div className="trace-inspector-item">
              <span className="trace-inspector-label">Parent</span>
              <button
                className="trace-inspector-value mono trace-inspector-link"
                onClick={() => setSelectedSpanId(selectedSpan.parent_span_id)}
              >
                {selectedSpan.parent_span_id}
              </button>
            </div>
          )}
          <div className="trace-inspector-item">
            <span className="trace-inspector-label">Kind</span>
            <span
              className="trace-inspector-value"
              style={{ color: spanTypeColor(selectedSpan.kind) }}
            >
              {selectedSpan.kind}
            </span>
          </div>
          <div className="trace-inspector-item">
            <span className="trace-inspector-label">Duration</span>
            <span className="trace-inspector-value">
              {formatDuration(selectedSpan.duration_us)}
            </span>
          </div>
          <div className="trace-inspector-item">
            <span className="trace-inspector-label">Start Time</span>
            <span className="trace-inspector-value mono">
              {new Date(selectedSpan.start_time / 1000).toISOString()}
            </span>
          </div>
          {selectedSpan.end_time && (
            <div className="trace-inspector-item">
              <span className="trace-inspector-label">End Time</span>
              <span className="trace-inspector-value mono">
                {new Date(selectedSpan.end_time / 1000).toISOString()}
              </span>
            </div>
          )}
        </div>

        <div className="trace-inspector-section">
          <h4>Raw JSON</h4>
          <pre className="trace-inspector-raw">
            {JSON.stringify(selectedSpan, null, 2)}
          </pre>
        </div>
      </div>
    );
  }

  // ── Render: trace list (no active trace) ──────────────────

  if (!activeTraceId) {
    return (
      <div className="trace-explorer">
        <div className="trace-explorer-header">
          <h3>Recent Traces</h3>
          <button
            className="btn subtle"
            onClick={() => {
              setLoadingList(true);
              api.getAgentTrace()
                .then((entries) => setRecentTraces(entries))
                .catch(() => setRecentTraces([]))
                .finally(() => setLoadingList(false));
            }}
          >
            Refresh
          </button>
        </div>

        {loadingList && (
          <div className="trace-explorer-loading">Loading traces...</div>
        )}

        {!loadingList && recentTraces.length === 0 && (
          <div className="trace-explorer-empty">
            <p>No traces recorded yet.</p>
            <p className="trace-explorer-hint">
              Traces appear here as agents process requests. Start a conversation to see traces.
            </p>
          </div>
        )}

        {!loadingList && recentTraces.length > 0 && (
          <div className="trace-explorer-list">
            {recentTraces.map((entry, i) => (
              <div
                key={i}
                className="trace-explorer-row"
                onClick={() => { void openTraceFromEntry(entry); }}
              >
                <div className="trace-explorer-row-icon">
                  {spanNameIcon(entry.event)}
                </div>
                <div className="trace-explorer-row-info">
                  <div className="trace-explorer-row-event">{entry.event}</div>
                  <div className="trace-explorer-row-detail">{entry.detail}</div>
                </div>
                <div className="trace-explorer-row-time">
                  {resolvingTraceId === entry.timestamp + entry.event + entry.detail ? "Resolving…" : entry.timestamp}
                </div>
              </div>
            ))}
          </div>
        )}
      </div>
    );
  }

  // ── Render: loading detail ────────────────────────────────

  if (loadingDetail) {
    return (
      <div className="trace-explorer">
        <div className="trace-explorer-header">
          <button className="btn ghost" onClick={() => setActiveTraceId(null)}>← Back</button>
          <h3>Trace Detail</h3>
        </div>
        <div className="trace-explorer-loading">Loading trace detail...</div>
      </div>
    );
  }

  // ── Render: trace detail view ─────────────────────────────

  return (
    <div className="trace-explorer">
      {/* Header with back button */}
      <div className="trace-detail-header">
        <button className="btn ghost" onClick={() => { setActiveTraceId(null); setRun(null); setSpans([]); }}>
          ← Back
        </button>
        <div className="trace-detail-title">
          <h3>🔍 {run?.name || "Trace"}</h3>
          {run && (
            <span className="trace-viewer-subtitle">
              {run.total_tokens} tokens · ${(run.cost_millicents / 100).toFixed(4)}
            </span>
          )}
        </div>
      </div>

      {/* View mode tabs */}
      <div className="trace-detail-tabs">
        {(["tree", "timeline", "raw"] as ViewMode[]).map((mode) => (
          <button
            key={mode}
            className={`trace-detail-tab${viewMode === mode ? " active" : ""}`}
            onClick={() => setViewMode(mode)}
          >
            {mode === "tree" ? "🌳 Tree" : mode === "timeline" ? "📊 Timeline" : "📝 Raw"}
          </button>
        ))}
        <div style={{ flex: 1 }} />
        {viewMode === "tree" && (
          <>
            <button className="btn ghost" style={{ fontSize: 12 }} onClick={expandAll}>Expand All</button>
            <button className="btn ghost" style={{ fontSize: 12 }} onClick={collapseAll}>Collapse All</button>
          </>
        )}
      </div>

      {/* Run summary */}
      {run && (
        <div className="trace-run-summary">
          <div className="trace-run-stat">
            <span className="trace-run-stat-value">{spans.length}</span>
            <span className="trace-run-stat-label">Spans</span>
          </div>
          <div className="trace-run-stat">
            <span className="trace-run-stat-value">{run.total_tokens.toLocaleString()}</span>
            <span className="trace-run-stat-label">Tokens</span>
          </div>
          <div className="trace-run-stat">
            <span className="trace-run-stat-value">${(run.cost_millicents / 100).toFixed(4)}</span>
            <span className="trace-run-stat-label">Cost</span>
          </div>
          <div className="trace-run-stat">
            <span className={`trace-run-stat-value ${run.status === "completed" ? "status-ok" : run.status === "error" ? "status-error" : ""}`}>
              {run.status}
            </span>
            <span className="trace-run-stat-label">Status</span>
          </div>
        </div>
      )}

      {/* Body = view + inspector */}
      <div className="trace-viewer-body">
        <div className="trace-viewer-main">
          {viewMode === "tree" && (
            <div className="trace-tree">
              {tree.length === 0 ? (
                <div className="empty-state centered">
                  <p>No spans found for this trace.</p>
                </div>
              ) : (
                tree.map((root) => renderTreeNode(root))
              )}
            </div>
          )}

          {viewMode === "timeline" && renderTimeline()}

          {viewMode === "raw" && (
            <pre className="trace-raw-json">
              {JSON.stringify({ run, spans }, null, 2)}
            </pre>
          )}
        </div>

        {/* Inspector panel */}
        <div className="trace-viewer-inspector">
          {renderSpanInspector()}
        </div>
      </div>
    </div>
  );
}
