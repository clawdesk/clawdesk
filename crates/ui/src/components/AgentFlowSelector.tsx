/**
 * AgentFlowSelector — Paperclip-inspired agent coordination panel.
 *
 * Lets users:
 * 1. Browse available agent flow templates (Claude, Codex, Cursor, etc.)
 * 2. Create configured flows from templates
 * 3. Select which flow to route messages through
 * 4. View active orchestration status
 */

import { useCallback, useEffect, useState } from "react";
import * as api from "../api";
import type {
  AgentAdapterType,
  AgentFlowConfig,
  FlowTemplate,
  OrchestrationEvent,
} from "../types";

// ── Adapter type metadata ─────────────────────────────────────

const ADAPTER_BADGES: Record<AgentAdapterType, { label: string; bg: string }> = {
  claude_local: { label: "Claude", bg: "bg-amber-600" },
  codex_local: { label: "Codex", bg: "bg-emerald-600" },
  cursor: { label: "Cursor", bg: "bg-indigo-600" },
  opencode_local: { label: "OpenCode", bg: "bg-cyan-600" },
  process: { label: "Process", bg: "bg-violet-600" },
  http: { label: "HTTP", bg: "bg-sky-600" },
  a2a_gateway: { label: "A2A", bg: "bg-blue-600" },
};

// ── Props ─────────────────────────────────────────────────────

interface AgentFlowSelectorProps {
  /** Currently selected flow ID (null = none / single-agent mode) */
  selectedFlowId: string | null;
  /** Called when user selects a flow */
  onSelectFlow: (flowId: string | null) => void;
  /** Push a toast notification */
  pushToast: (msg: string) => void;
}

// ── Component ─────────────────────────────────────────────────

export function AgentFlowSelector({
  selectedFlowId,
  onSelectFlow,
  pushToast,
}: AgentFlowSelectorProps) {
  const [flows, setFlows] = useState<AgentFlowConfig[]>([]);
  const [templates, setTemplates] = useState<FlowTemplate[]>([]);
  const [showTemplates, setShowTemplates] = useState(false);
  const [showConfig, setShowConfig] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  // ── Load flows + templates ──
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const [f, t] = await Promise.all([
          api.listAgentFlows(),
          api.listFlowTemplates(),
        ]);
        if (!cancelled) {
          setFlows(f);
          setTemplates(t);
        }
      } catch {
        // Backend may not have these commands yet — start empty
        if (!cancelled) {
          setFlows([]);
          setTemplates(getBuiltinTemplates());
        }
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => { cancelled = true; };
  }, []);

  // ── Create flow from template ──
  const createFromTemplate = useCallback(async (tpl: FlowTemplate) => {
    try {
      const newFlow = await api.createAgentFlow({
        name: tpl.name,
        adapter_type: tpl.adapter_type as AgentAdapterType,
        description: tpl.description,
        model: tpl.default_config.model || "",
        role: "general",
        adapter_config: tpl.default_config,
        heartbeat_interval_sec: 0,
        max_concurrent_runs: 1,
        cwd: undefined,
        icon: tpl.icon,
        color: tpl.color,
        active: true,
      });
      setFlows((prev) => [...prev, newFlow]);
      setShowTemplates(false);
      onSelectFlow(newFlow.id);
      pushToast(`Created "${newFlow.name}" flow`);
    } catch (err) {
      pushToast(`Failed to create flow: ${err}`);
    }
  }, [onSelectFlow, pushToast]);

  // ── Delete flow ──
  const deleteFlow = useCallback(async (flowId: string) => {
    try {
      await api.deleteAgentFlow(flowId);
      setFlows((prev) => prev.filter((f) => f.id !== flowId));
      if (selectedFlowId === flowId) onSelectFlow(null);
      pushToast("Flow deleted");
    } catch (err) {
      pushToast(`Failed to delete: ${err}`);
    }
  }, [selectedFlowId, onSelectFlow, pushToast]);

  // ── Toggle flow active ──
  const toggleActive = useCallback(async (flowId: string, active: boolean) => {
    try {
      const updated = await api.updateAgentFlow(flowId, { active });
      setFlows((prev) => prev.map((f) => (f.id === flowId ? updated : f)));
    } catch (err) {
      pushToast(`Failed to update: ${err}`);
    }
  }, [pushToast]);

  if (loading) {
    return (
      <div className="p-3 text-sm text-[var(--text-muted)]">
        Loading agent flows…
      </div>
    );
  }

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 8, padding: 8 }}>
      {/* ── Header ── */}
      <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", padding: "0 4px" }}>
        <span style={{ fontSize: 11, fontWeight: 600, textTransform: "uppercase", letterSpacing: "0.05em", color: "var(--text-muted)" }}>
          Agent Flows
        </span>
        <button
          className="btn subtle"
          style={{ fontSize: 11, padding: "3px 10px" }}
          onClick={() => setShowTemplates(!showTemplates)}
        >
          {showTemplates ? "Cancel" : "+ Add"}
        </button>
      </div>

      {/* ── "None" / single-agent mode ── */}
      <button
        className={selectedFlowId === null ? "btn primary" : "btn subtle"}
        style={{ display: "flex", alignItems: "center", gap: 8, padding: "8px 12px", textAlign: "left", width: "100%", borderRadius: 8 }}
        onClick={() => onSelectFlow(null)}
      >
        <span style={{ fontSize: 16 }}>💬</span>
        <span style={{ flex: 1, textAlign: "left" }}>Direct Chat</span>
        <span style={{ fontSize: 10, color: "var(--text-muted)" }}>Single agent</span>
      </button>

      {/* ── Template picker ── */}
      {showTemplates && (
        <div className="section-card" style={{ padding: 8 }}>
          <p style={{ fontSize: 11, color: "var(--text-muted)", marginBottom: 8, padding: "0 4px" }}>
            Choose an agent type to add:
          </p>
          <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
            {templates.map((tpl) => {
              const badge = ADAPTER_BADGES[tpl.adapter_type as AgentAdapterType];
              return (
                <button
                  key={tpl.id}
                  className="btn subtle"
                  style={{ display: "flex", alignItems: "center", gap: 8, padding: "8px 12px", textAlign: "left", width: "100%", borderRadius: 8 }}
                  onClick={() => createFromTemplate(tpl)}
                >
                  <span style={{ fontSize: 18 }}>{tpl.icon}</span>
                  <div style={{ flex: 1, minWidth: 0 }}>
                    <div style={{ display: "flex", alignItems: "center", gap: 6 }}>
                      <span style={{ fontSize: 13, fontWeight: 500, color: "var(--text)" }}>{tpl.name}</span>
                      {badge && (
                        <span className="chip chip-sm" style={{ background: tpl.color || "var(--brand)", color: "#fff" }}>
                          {badge.label}
                        </span>
                      )}
                    </div>
                    <p style={{ fontSize: 11, color: "var(--text-muted)", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", margin: 0 }}>{tpl.description}</p>
                  </div>
                </button>
              );
            })}
          </div>
        </div>
      )}

      {/* ── Active flows ── */}
      {flows.map((flow) => {
        const isSelected = selectedFlowId === flow.id;
        const badge = ADAPTER_BADGES[flow.adapter_type as AgentAdapterType];

        return (
          <div
            key={flow.id}
            className="section-card"
            style={{
              padding: 0,
              border: isSelected ? "1px solid var(--brand)" : "1px solid var(--line)",
              background: isSelected ? "var(--brand-soft)" : "transparent",
            }}
          >
            <button
              style={{ display: "flex", alignItems: "center", gap: 8, padding: "8px 12px", textAlign: "left", width: "100%", background: "none", border: "none", cursor: "pointer", color: "var(--text)" }}
              onClick={() => onSelectFlow(isSelected ? null : flow.id)}
            >
              <span style={{ fontSize: 18 }}>{flow.icon}</span>
              <div style={{ flex: 1, minWidth: 0 }}>
                <div style={{ display: "flex", alignItems: "center", gap: 6 }}>
                  <span style={{ fontSize: 13, fontWeight: 500 }}>{flow.name}</span>
                  {badge && (
                    <span className="chip chip-sm" style={{ background: flow.color || "var(--brand)", color: "#fff" }}>
                      {badge.label}
                    </span>
                  )}
                  {!flow.active && (
                    <span className="chip chip-sm" style={{ background: "#6b7280", color: "#fff" }}>
                      paused
                    </span>
                  )}
                </div>
                <p style={{ fontSize: 11, color: "var(--text-muted)", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", margin: 0 }}>
                  {flow.model || flow.adapter_type} · {flow.role}
                </p>
              </div>
              <div
                style={{ width: 8, height: 8, borderRadius: "50%", flexShrink: 0, backgroundColor: flow.active ? "#10b981" : "#6b7280" }}
              />
            </button>

            {/* ── Expanded config for selected flow ── */}
            {isSelected && (
              <div style={{ padding: "0 12px 8px", display: "flex", alignItems: "center", gap: 4 }}>
                <button
                  className="btn subtle"
                  style={{ fontSize: 10, padding: "2px 8px" }}
                  onClick={(e) => { e.stopPropagation(); setShowConfig(showConfig === flow.id ? null : flow.id); }}
                >
                  Config
                </button>
                <button
                  className="btn subtle"
                  style={{ fontSize: 10, padding: "2px 8px" }}
                  onClick={(e) => { e.stopPropagation(); toggleActive(flow.id, !flow.active); }}
                >
                  {flow.active ? "Pause" : "Resume"}
                </button>
                <button
                  className="btn danger"
                  style={{ fontSize: 10, padding: "2px 8px", marginLeft: "auto" }}
                  onClick={(e) => { e.stopPropagation(); deleteFlow(flow.id); }}
                >
                  Remove
                </button>
              </div>
            )}

            {/* ── Config panel ── */}
            {showConfig === flow.id && (
              <div style={{ padding: "0 12px 8px", fontSize: 11, color: "var(--text-muted)" }}>
                <div style={{ display: "grid", gridTemplateColumns: "auto 1fr", gap: "2px 12px" }}>
                  <span style={{ fontWeight: 500 }}>Adapter:</span>
                  <span>{flow.adapter_type}</span>
                  <span style={{ fontWeight: 500 }}>Model:</span>
                  <span>{flow.model || "—"}</span>
                  <span style={{ fontWeight: 500 }}>Role:</span>
                  <span>{flow.role}</span>
                  <span style={{ fontWeight: 500 }}>Heartbeat:</span>
                  <span>{flow.heartbeat_interval_sec > 0 ? `${flow.heartbeat_interval_sec}s` : "Manual"}</span>
                  <span style={{ fontWeight: 500 }}>Max concurrent:</span>
                  <span>{flow.max_concurrent_runs}</span>
                  {flow.cwd && (
                    <>
                      <span style={{ fontWeight: 500 }}>CWD:</span>
                      <span style={{ overflow: "hidden", textOverflow: "ellipsis" }}>{flow.cwd}</span>
                    </>
                  )}
                  {Object.entries(flow.adapter_config).map(([k, v]) => (
                    <div key={k} style={{ display: "contents" }}>
                      <span style={{ fontWeight: 500 }}>{k}:</span>
                      <span style={{ overflow: "hidden", textOverflow: "ellipsis" }}>{v}</span>
                    </div>
                  ))}
                </div>
              </div>
            )}
          </div>
        );
      })}

      {/* ── Empty state ── */}
      {flows.length === 0 && !showTemplates && (
        <div style={{ textAlign: "center", padding: 16, fontSize: 12, color: "var(--text-muted)" }}>
          <p>No agent flows configured.</p>
          <p style={{ marginTop: 4 }}>Click <strong>+ Add</strong> to set up Claude, Codex, Cursor, or other agents.</p>
        </div>
      )}
    </div>
  );
}

// ── Fallback templates (used when backend is unavailable) ──────

function getBuiltinTemplates(): FlowTemplate[] {
  return [
    {
      id: "tpl_claude",
      name: "Claude Code Agent",
      description: "Anthropic Claude — best for complex reasoning, code generation, and multi-step tasks",
      adapter_type: "claude_local",
      icon: "🧠",
      color: "#D97706",
      default_config: { command: "claude", model: "claude-sonnet-4-20250514" },
    },
    {
      id: "tpl_codex",
      name: "Codex Agent",
      description: "OpenAI Codex — optimized for code editing, refactoring, and test writing",
      adapter_type: "codex_local",
      icon: "⚡",
      color: "#10B981",
      default_config: { command: "codex", model: "o4-mini" },
    },
    {
      id: "tpl_cursor",
      name: "Cursor Agent",
      description: "Cursor — IDE-integrated agent for contextual code edits and navigation",
      adapter_type: "cursor",
      icon: "🎯",
      color: "#6366F1",
      default_config: { command: "cursor" },
    },
    {
      id: "tpl_process",
      name: "Shell Process Agent",
      description: "Generic shell command — run any CLI tool as an agent (aider, continue, etc.)",
      adapter_type: "process",
      icon: "🔧",
      color: "#8B5CF6",
      default_config: {},
    },
    {
      id: "tpl_a2a",
      name: "A2A Gateway Agent",
      description: "Remote agent via Agent-to-Agent protocol — connect to any A2A-compatible endpoint",
      adapter_type: "a2a_gateway",
      icon: "🌐",
      color: "#0EA5E9",
      default_config: { endpoint: "http://localhost:8080" },
    },
  ];
}
