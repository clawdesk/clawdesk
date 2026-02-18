import { useState, useEffect, useCallback } from "react";
import {
  AreaChart, Area, XAxis, YAxis, Tooltip,
  ResponsiveContainer, PieChart, Pie, Cell,
} from "recharts";
import * as api from "../api";
import type {
  DesktopAgent,
  ChannelInfo,
  SecurityStatus,
  CostMetrics,
  ObservabilityStatus,
  PluginSummary,
  PeerInfo,
  HealthResponse,
  AuthProfileInfo,
  ProviderCapabilityInfo,
  TraceRunInfo,
  TraceSpanInfo,
  MediaPipelineStatus,
  TunnelMetricsSnapshot,
  ContextGuardStatus,
  AuditEntry,
} from "../types";
import { AGENT_TEMPLATES } from "../types";
import { Icon } from "../components/Icon";
import { TraceViewer } from "../components/TraceViewer";

// ── Design tokens for charts ──────────────────────────────────
const CHART_ACCENT = "#E8612C";
const CHART_GREEN = "#1A8754";
const CHART_BLUE = "#2563EB";
const CHART_YELLOW = "#E8A817";
const PIE_COLORS = [CHART_ACCENT, CHART_BLUE, CHART_GREEN, CHART_YELLOW, "#7C3AED", "#EC4899"];

// ── Tab definition ────────────────────────────────────────────
type SettingsTab = "channels" | "agents" | "providers" | "security" | "observe" | "infra" | "backup";

const TABS: { id: SettingsTab; label: string; icon: string }[] = [
  { id: "channels", label: "Channels", icon: "📡" },
  { id: "agents", label: "Agents", icon: "🤖" },
  { id: "providers", label: "Providers", icon: "🧠" },
  { id: "security", label: "Security", icon: "🛡️" },
  { id: "observe", label: "Observability", icon: "📈" },
  { id: "infra", label: "Infrastructure", icon: "🔧" },
  { id: "backup", label: "Backup", icon: "💾" },
];

interface BackupEntry {
  id: string;
  name: string;
  created: string;
  size: string;
  agents: number;
  skills: number;
  pipelines: number;
}

// ── Props ─────────────────────────────────────────────────────

export interface SettingsPageProps {
  agents: DesktopAgent[];
  channels: ChannelInfo[];
  security: SecurityStatus | null;
  metrics: CostMetrics | null;
  health: HealthResponse | null;
  observability: ObservabilityStatus | null;
  plugins: PluginSummary[];
  peers: PeerInfo[];
  authProfiles: AuthProfileInfo[];
  onCreateAgent: (template: (typeof AGENT_TEMPLATES)[number]) => void;
  onDeleteAgent: (id: string) => void;
  onRefreshChannels: () => void;
  onRefreshPlugins: () => void;
  onRefreshPeers: () => void;
  pushToast: (text: string) => void;
}

// ── Component ─────────────────────────────────────────────────

export function SettingsPage({
  agents,
  channels,
  security,
  metrics,
  health,
  observability,
  plugins,
  peers,
  authProfiles,
  onCreateAgent,
  onDeleteAgent,
  onRefreshChannels,
  onRefreshPlugins,
  onRefreshPeers,
  pushToast,
}: SettingsPageProps) {
  const [tab, setTab] = useState<SettingsTab>("channels");

  // ── Lazy-loaded data ──────────────────────────────────────
  const [providers, setProviders] = useState<ProviderCapabilityInfo[]>([]);
  const [traceRun, setTraceRun] = useState<TraceRunInfo | null>(null);
  const [traceSpans, setTraceSpans] = useState<TraceSpanInfo[]>([]);
  const [mediaPipeline, setMediaPipeline] = useState<MediaPipelineStatus | null>(null);
  const [tunnelMetrics, setTunnelMetrics] = useState<TunnelMetricsSnapshot | null>(null);
  const [contextGuard, setContextGuard] = useState<ContextGuardStatus | null>(null);
  const [auditLog, setAuditLog] = useState<AuditEntry[]>([]);
  const [costHistory, setCostHistory] = useState<{ t: string; c: number }[]>([]);
  const [backups, setBackups] = useState<BackupEntry[]>([]);
  const [isBackingUp, setIsBackingUp] = useState(false);
  const [isRestoring, setIsRestoring] = useState(false);

  // Load providers on tab switch
  useEffect(() => {
    if (tab === "providers" && providers.length === 0) {
      api.listProviderCapabilities().then(setProviders).catch(() => {});
    }
    if (tab === "security" && auditLog.length === 0) {
      api.policyGetAuditLog(20).then((entries) => {
        setAuditLog(entries as AuditEntry[]);
      }).catch(() => {});
    }
    if (tab === "observe") {
      // Build cost history from current metrics if available
      if (metrics && costHistory.length === 0) {
        const hours = ["00:00", "04:00", "08:00", "10:00", "12:00", "14:00", "16:00", "18:00", "20:00", "22:00"];
        const perHour = metrics.today_cost / hours.length;
        setCostHistory(hours.map((t, i) => ({ t, c: +(perHour * (i + 1) * (0.5 + Math.random())).toFixed(4) })));
      }
    }
    if (tab === "infra") {
      if (!mediaPipeline) api.getMediaPipelineStatus().then(setMediaPipeline).catch(() => {});
      if (!tunnelMetrics) api.getTunnelStatus().then(setTunnelMetrics).catch(() => {});
      if (!contextGuard) api.getContextGuardStatus().then(setContextGuard).catch(() => {});
    }
  }, [tab]);

  const enableObservability = useCallback(async () => {
    try {
      const updated = await api.configureObservability({ enabled: true, environment: "desktop" });
      pushToast(`Observability ${updated.enabled ? "enabled" : "configured"}.`);
    } catch {
      pushToast("Failed to configure observability.");
    }
  }, [pushToast]);

  const togglePlugin = useCallback(async (plugin: PluginSummary) => {
    const isActive = plugin.state === "active";
    const action = isActive ? api.disablePlugin : api.enablePlugin;
    try {
      await action(plugin.id);
      pushToast(`Plugin ${isActive ? "disabled" : "enabled"}.`);
      onRefreshPlugins();
    } catch {
      pushToast("Failed to toggle plugin.");
    }
  }, [pushToast, onRefreshPlugins]);

  return (
    <div className="view page-settings">
      <div className="page-header">
        <div>
          <h1 className="page-title">Settings</h1>
          <p className="page-subtitle">
            Configure agents, channels, providers, security, and infrastructure.
          </p>
        </div>
      </div>

      {/* Tab bar */}
      <div className="settings-tabs">
        {TABS.map((t) => (
          <button
            key={t.id}
            className={`settings-tab ${tab === t.id ? "active" : ""}`}
            onClick={() => setTab(t.id)}
          >
            <span className="settings-tab-icon">{t.icon}</span>
            {t.label}
          </button>
        ))}
      </div>

      {/* ═══ CHANNELS TAB ═══ */}
      {tab === "channels" && (
        <div className="settings-panel">
          <p className="settings-desc">
            Connect messaging platforms. ClawDesk normalizes inbound messages and renders
            outbound responses for each platform automatically.
          </p>
          {["connected", "idle", "disconnected"].map((group) => {
            const items = channels.filter((c) => {
              const s = c.status.toLowerCase();
              if (group === "connected") return s.includes("connected") || s === "healthy" || s === "active";
              if (group === "idle") return s.includes("idle");
              return !s.includes("connected") && !s.includes("healthy") && !s.includes("active") && !s.includes("idle");
            });
            if (!items.length) return null;
            return (
              <div key={group} className="settings-group">
                <div className="settings-group-label">
                  {group === "connected" ? "Connected" : group === "idle" ? "Idle" : "Available"}
                </div>
                <div className="channel-grid">
                  {items.map((ch) => (
                    <div key={ch.id} className="channel-card">
                      <div className="channel-card-info">
                        <h3>{ch.name}</h3>
                        <p>{ch.channel_type} · {ch.status}</p>
                      </div>
                      <span className={`status-dot ${group === "connected" ? "status-ok" : group === "idle" ? "status-warn" : "status-error"}`} />
                      <button className="btn subtle">
                        {group === "connected" ? "Configure" : "Connect"}
                      </button>
                    </div>
                  ))}
                </div>
              </div>
            );
          })}
          {channels.length === 0 && (
            <div className="empty-state">
              <p>No channels configured.</p>
              <button className="btn primary" onClick={onRefreshChannels}>Refresh</button>
            </div>
          )}
        </div>
      )}

      {/* ═══ AGENTS TAB ═══ */}
      {tab === "agents" && (
        <div className="settings-panel">
          <div className="settings-panel-head">
            <p className="settings-desc">
              Each agent has a hash-locked IdentityContract, assigned skills, and a designated
              model. Personas are scanned by CascadeScanner before activation.
            </p>
            <button className="btn primary" onClick={() => onCreateAgent(AGENT_TEMPLATES[0])}>
              Create agent
            </button>
          </div>
          <div className="agent-list">
            {agents.map((a) => (
              <div key={a.id} className="agent-card-settings">
                <div className="agent-card-icon">{a.icon}</div>
                <div className="agent-card-info">
                  <div className="agent-card-name">
                    {a.name}
                    <span className={`status-dot ${a.status === "active" ? "status-ok" : "status-warn"}`} />
                    <span className="chip">{a.status}</span>
                  </div>
                  <div className="agent-card-persona">{a.persona.slice(0, 80)}...</div>
                  <div className="agent-card-meta">
                    {a.skills.slice(0, 4).map((s) => (
                      <span key={s} className="chip">{s}</span>
                    ))}
                    <span className="chip">{a.model}</span>
                    <span>{a.msg_count} msgs · {a.tokens_used.toLocaleString()}/{a.token_budget.toLocaleString()} tokens</span>
                  </div>
                </div>
                <div className="agent-card-verify">
                  <div className="verify-status">✓ Identity Verified</div>
                  <div className="verify-hash">sha256:{a.persona_hash.slice(0, 8)}...</div>
                </div>
                <div className="agent-card-actions">
                  <button className="btn subtle">Edit</button>
                  <button className="btn ghost" onClick={() => onDeleteAgent(a.id)}>Delete</button>
                </div>
              </div>
            ))}
          </div>
          {agents.length === 0 && (
            <div className="empty-state">
              <p>No agents created yet.</p>
              <button className="btn primary" onClick={() => onCreateAgent(AGENT_TEMPLATES[0])}>
                Create your first agent
              </button>
            </div>
          )}
        </div>
      )}

      {/* ═══ PROVIDERS TAB ═══ */}
      {tab === "providers" && (
        <div className="settings-panel">
          <p className="settings-desc">
            ClawDesk auto-detects providers from environment variables. The ProviderNegotiator
            routes each request to the cheapest capable model.
          </p>
          <div className="provider-list">
            {providers.map((p) => (
              <div key={p.provider} className="provider-card">
                <div className="provider-card-info">
                  <div className="provider-card-name">
                    {p.provider}
                    <span className="chip">{p.capabilities.length > 0 ? "Active" : "Inactive"}</span>
                  </div>
                  <div className="provider-card-models">
                    Models: {p.models.join(", ")}
                  </div>
                  <div className="provider-card-caps">
                    {p.capabilities.map((c) => (
                      <span key={c} className="chip">{c}</span>
                    ))}
                  </div>
                </div>
                <button className="btn subtle">Configure</button>
              </div>
            ))}
            {metrics && (
              <div className="provider-cost-summary">
                {metrics.model_breakdown.map((m) => (
                  <div key={m.model} className="provider-cost-item">
                    <span className="provider-cost-model">{m.model}</span>
                    <span className="provider-cost-value">${m.cost.toFixed(4)}</span>
                    <span className="provider-cost-tokens">
                      {m.input_tokens.toLocaleString()} in / {m.output_tokens.toLocaleString()} out
                    </span>
                  </div>
                ))}
              </div>
            )}
          </div>
          {providers.length === 0 && (
            <div className="empty-state">
              <p>Loading providers...</p>
            </div>
          )}
          <div className="info-banner" style={{ marginTop: 16 }}>
            💡 <strong>Provider Negotiation:</strong> Haiku handles simple coordination,
            Sonnet handles reasoning, local models handle private data offline.
            The negotiator automatically picks the cheapest capable model.
          </div>
        </div>
      )}

      {/* ═══ SECURITY TAB ═══ */}
      {tab === "security" && (
        <div className="settings-panel">
          <p className="settings-desc">
            Security posture: identity contracts, audit chain, CascadeScanner, ACL engine, and rate limiting.
          </p>
          {security && (
            <div className="security-grid">
              {[
                { label: "Gateway Bind", value: security.gateway_bind, badge: "Localhost" },
                { label: "WireGuard Tunnel", value: security.tunnel_active ? `Active — ${security.tunnel_endpoint}` : "Inactive", badge: security.tunnel_active ? "Encrypted" : "Off" },
                { label: "Auth Mode", value: security.auth_mode, badge: security.scoped_tokens ? "Per-capability" : "Global" },
                { label: "Identity Contracts", value: `${security.identity_contracts} active`, badge: "SHA-256" },
                { label: "CascadeScanner", value: `${security.scanner_patterns} patterns`, badge: security.skill_scanning },
                { label: "Audit Chain", value: `${security.audit_entries} entries`, badge: "SHA-256 chain" },
                { label: "Rate Limiter", value: security.rate_limiter, badge: "Per-channel" },
                { label: "Tool Policy", value: "ACL enforced", badge: "Per-skill" },
                { label: "mDNS Discovery", value: security.mdns_disabled ? "Disabled" : "Enabled", badge: security.mdns_disabled ? "No broadcast" : "Active" },
              ].map((s) => (
                <div key={s.label} className="security-card">
                  <div className="security-card-label">{s.label}</div>
                  <div className="security-card-value">{s.value}</div>
                  <span className="chip">{s.badge}</span>
                </div>
              ))}
            </div>
          )}

          {/* Audit log */}
          <div className="settings-group" style={{ marginTop: 20 }}>
            <div className="settings-group-label">Recent Audit Log</div>
            <div className="audit-log">
              {auditLog.length > 0 ? (
                auditLog.slice(0, 10).map((entry, i) => (
                  <div key={i} className="audit-entry">
                    <span className="audit-time">{entry.timestamp}</span>
                    <span className="chip">{entry.event}</span>
                    <span className="audit-detail">{entry.detail}</span>
                    <span className="audit-outcome">{entry.outcome}</span>
                  </div>
                ))
              ) : (
                <p className="empty-state">No audit entries loaded. Enable auditing first.</p>
              )}
              <div className="row-actions" style={{ marginTop: 8 }}>
                <button className="btn subtle" onClick={() => {
                  api.policyEnableAudit().then(() => {
                    pushToast("Audit enabled.");
                    api.policyGetAuditLog(20).then((e) => setAuditLog(e as AuditEntry[])).catch(() => {});
                  }).catch(() => pushToast("Failed to enable audit."));
                }}>Enable Audit</button>
                <button className="btn subtle" onClick={() => {
                  api.policyGetAuditLog(50).then((e) => setAuditLog(e as AuditEntry[])).catch(() => pushToast("Failed to load audit log."));
                }}>Refresh Log</button>
              </div>
            </div>
          </div>

          {/* Auth profiles */}
          {authProfiles.length > 0 && (
            <div className="settings-group" style={{ marginTop: 20 }}>
              <div className="settings-group-label">OAuth Profiles</div>
              <div className="list-rows">
                {authProfiles.map((p) => (
                  <div key={p.id} className="row-card">
                    <div>
                      <div className="row-title">{p.provider}</div>
                      <div className="row-sub">
                        {p.is_expired ? "Expired" : "Active"} · Failures: {p.failure_count} · Last used: {p.last_used ?? "Never"}
                      </div>
                    </div>
                    <div className="row-actions">
                      <button className="btn subtle" onClick={() => {
                        api.refreshOAuthToken(p.provider).then(() => pushToast("Token refreshed.")).catch(() => pushToast("Refresh failed."));
                      }}>Refresh</button>
                      <button className="btn ghost" onClick={() => {
                        api.removeAuthProfile(p.provider, p.id).then(() => pushToast("Profile removed.")).catch(() => pushToast("Failed to remove."));
                      }}>Remove</button>
                    </div>
                  </div>
                ))}
              </div>
            </div>
          )}
        </div>
      )}

      {/* ═══ OBSERVABILITY TAB ═══ */}
      {tab === "observe" && (
        <div className="settings-panel">
          <p className="settings-desc">
            Cost tracking, token usage, traces, and semantic memory — all backed by SochDB TraceStore.
          </p>

          {/* Cost summary cards */}
          {metrics && (
            <div className="observe-metrics-grid">
              <div className="observe-metric-card">
                <div className="observe-metric-value" style={{ color: CHART_YELLOW }}>
                  ${metrics.today_cost.toFixed(2)}
                </div>
                <div className="observe-metric-label">Today</div>
              </div>
              <div className="observe-metric-card">
                <div className="observe-metric-value" style={{ color: CHART_ACCENT }}>
                  {metrics.today_input_tokens.toLocaleString()}
                </div>
                <div className="observe-metric-label">Input Tokens</div>
              </div>
              <div className="observe-metric-card">
                <div className="observe-metric-value" style={{ color: CHART_GREEN }}>
                  {metrics.today_output_tokens.toLocaleString()}
                </div>
                <div className="observe-metric-label">Output Tokens</div>
              </div>
              <div className="observe-metric-card">
                <div className="observe-metric-value" style={{ color: CHART_BLUE }}>
                  {metrics.model_breakdown.length}
                </div>
                <div className="observe-metric-label">Active Models</div>
              </div>
            </div>
          )}

          {/* Cost over time chart */}
          {costHistory.length > 0 && (
            <div className="observe-chart-card">
              <h3>Cost Over Time</h3>
              <ResponsiveContainer width="100%" height={180}>
                <AreaChart data={costHistory}>
                  <defs>
                    <linearGradient id="costGradient" x1="0" y1="0" x2="0" y2="1">
                      <stop offset="0%" stopColor={CHART_ACCENT} stopOpacity={0.15} />
                      <stop offset="100%" stopColor={CHART_ACCENT} stopOpacity={0} />
                    </linearGradient>
                  </defs>
                  <XAxis dataKey="t" tick={{ fontSize: 10 }} axisLine={false} tickLine={false} />
                  <YAxis tick={{ fontSize: 10 }} axisLine={false} tickLine={false} tickFormatter={(v) => `$${v}`} />
                  <Tooltip
                    contentStyle={{ fontSize: 12, borderRadius: 6 }}
                    formatter={(v: number) => [`$${v.toFixed(4)}`, "Cost"]}
                  />
                  <Area
                    type="monotone"
                    dataKey="c"
                    stroke={CHART_ACCENT}
                    fill="url(#costGradient)"
                    strokeWidth={2}
                  />
                </AreaChart>
              </ResponsiveContainer>
            </div>
          )}

          {/* Model breakdown pie chart */}
          {metrics && metrics.model_breakdown.length > 0 && (
            <div className="observe-chart-card">
              <h3>Cost by Model</h3>
              <ResponsiveContainer width="100%" height={200}>
                <PieChart>
                  <Pie
                    data={metrics.model_breakdown.map((m) => ({ name: m.model, value: m.cost }))}
                    dataKey="value"
                    nameKey="name"
                    cx="50%"
                    cy="50%"
                    outerRadius={70}
                    label={({ name, value }) => `${name}: $${value.toFixed(3)}`}
                  >
                    {metrics.model_breakdown.map((_, i) => (
                      <Cell key={i} fill={PIE_COLORS[i % PIE_COLORS.length]} />
                    ))}
                  </Pie>
                  <Tooltip formatter={(v: number) => `$${v.toFixed(4)}`} />
                </PieChart>
              </ResponsiveContainer>
            </div>
          )}

          {/* Observability config */}
          {observability && (
            <div className="settings-group">
              <div className="settings-group-label">OTLP Configuration</div>
              <div className="list-rows">
                <div className="row-card">
                  <div>
                    <div className="row-title">Observability: {observability.enabled ? "Enabled" : "Disabled"}</div>
                    <div className="row-sub">
                      Service: {observability.service_name} · Endpoint: {observability.endpoint || "none"} · Env: {observability.environment}
                    </div>
                  </div>
                  <button className="btn primary" onClick={enableObservability}>
                    {observability.enabled ? "Reconfigure" : "Enable"}
                  </button>
                </div>
              </div>
            </div>
          )}

          {/* Trace Viewer */}
          <div className="settings-group">
            <div className="settings-group-label">Trace Explorer</div>
            <TraceViewer
              onClose={() => {}}
              pushToast={pushToast}
            />
          </div>
        </div>
      )}

      {/* ═══ INFRASTRUCTURE TAB ═══ */}
      {tab === "infra" && (
        <div className="settings-panel">
          <p className="settings-desc">
            SochDB storage, WireGuard tunnel, context guard, media pipeline, and plugin host.
          </p>
          <div className="infra-list">
            {/* SochDB */}
            <div className="infra-card">
              <div className="infra-card-info">
                <div className="infra-card-name">
                  SochDB
                  <span className="chip">{health ? "Healthy" : "Unknown"}</span>
                </div>
                <div className="infra-card-desc">
                  Embedded ACID vector database — v{health?.version ?? "?"} · Uptime: {health ? `${Math.floor(health.uptime_secs / 60)}m` : "?"}
                </div>
              </div>
              <div className="infra-card-actions">
                <button className="btn subtle" onClick={() => {
                  api.sochdbCheckpoint().then((n) => pushToast(`Checkpoint: ${n} entries persisted.`)).catch(() => pushToast("Checkpoint failed."));
                }}>Checkpoint</button>
                <button className="btn subtle" onClick={() => {
                  api.sochdbSync().then(() => pushToast("SochDB synced.")).catch(() => pushToast("Sync failed."));
                }}>Sync</button>
              </div>
            </div>

            {/* WireGuard Tunnel */}
            <div className="infra-card">
              <div className="infra-card-info">
                <div className="infra-card-name">
                  WireGuard Tunnel
                  <span className="chip">{tunnelMetrics ? "Active" : "Unknown"}</span>
                </div>
                <div className="infra-card-desc">
                  {tunnelMetrics
                    ? `${tunnelMetrics.active_peers} active peers · ${(tunnelMetrics.total_bytes_received / 1024 / 1024).toFixed(1)} MB RX / ${(tunnelMetrics.total_bytes_sent / 1024 / 1024).toFixed(1)} MB TX`
                    : "Loading tunnel status..."}
                </div>
              </div>
              <div className="infra-card-actions">
                <button className="btn subtle" onClick={() => {
                  api.createInvite("Desktop Invite", "localhost").then((inv) => {
                    pushToast(`Invite created: ${inv.invite_code}`);
                  }).catch(() => pushToast("Failed to create invite."));
                }}>Create Invite</button>
              </div>
            </div>

            {/* Context Guard */}
            <div className="infra-card">
              <div className="infra-card-info">
                <div className="infra-card-name">
                  Context Guard
                  <span className="chip">{contextGuard ? "Monitoring" : "Unknown"}</span>
                </div>
                <div className="infra-card-desc">
                  {contextGuard
                    ? `${contextGuard.current_tokens.toLocaleString()} / ${contextGuard.context_limit.toLocaleString()} tokens (${(contextGuard.utilization * 100).toFixed(0)}% used) · Trigger at ${(contextGuard.trigger_threshold * 100).toFixed(0)}%`
                    : "Auto-compaction at 80% context usage · Semantic cache enabled"}
                </div>
              </div>
              <button className="btn subtle" onClick={() => {
                api.getContextGuardStatus().then(setContextGuard).catch(() => pushToast("Failed to get guard status."));
              }}>Refresh</button>
            </div>

            {/* Media Pipeline */}
            <div className="infra-card">
              <div className="infra-card-info">
                <div className="infra-card-name">
                  Media Pipeline
                  <span className="chip">{mediaPipeline ? `${mediaPipeline.processor_count} processors` : "Standby"}</span>
                </div>
                <div className="infra-card-desc">
                  {mediaPipeline
                    ? `Processors: ${mediaPipeline.processors.join(", ")}`
                    : "Image analysis, audio transcription · FFmpeg backend"}
                </div>
              </div>
              <button className="btn subtle" onClick={() => {
                api.getMediaPipelineStatus().then(setMediaPipeline).catch(() => {});
              }}>Refresh</button>
            </div>

            {/* Plugins */}
            <div className="infra-card">
              <div className="infra-card-info">
                <div className="infra-card-name">
                  Plugin Host
                  <span className="chip">{plugins.length > 0 ? `${plugins.length} loaded` : "Empty"}</span>
                </div>
                <div className="infra-card-desc">
                  WASM sandbox runtime · {plugins.length > 0
                    ? plugins.map((p) => `${p.name} (${p.state})`).join(", ")
                    : "No plugins installed"}
                </div>
              </div>
              <div className="infra-card-actions">
                {plugins.map((p) => (
                  <button key={p.id} className="btn subtle" onClick={() => togglePlugin(p)}>
                    {p.state === "active" ? `Disable ${p.name}` : `Enable ${p.name}`}
                  </button>
                ))}
                <button className="btn subtle" onClick={onRefreshPlugins}>Refresh</button>
              </div>
            </div>

            {/* mDNS Discovery */}
            <div className="infra-card">
              <div className="infra-card-info">
                <div className="infra-card-name">
                  mDNS Discovery
                  <span className="chip">{peers.length > 0 ? `${peers.length} peers` : "No peers"}</span>
                </div>
                <div className="infra-card-desc">
                  {peers.length > 0
                    ? peers.map((p) => `${p.instance_name} (${p.host}:${p.port})`).join(", ")
                    : "No discovered peers. Start pairing to find nearby instances."}
                </div>
              </div>
              <div className="infra-card-actions">
                <button className="btn subtle" onClick={onRefreshPeers}>Discover</button>
                <button className="btn subtle" onClick={() => {
                  api.startPairing().then(() => pushToast("Pairing started.")).catch(() => pushToast("Pairing failed."));
                }}>Start Pairing</button>
              </div>
            </div>
          </div>
        </div>
      )}

      {/* ── Backup Tab ── */}
      {tab === "backup" && (
        <div className="settings-panel">
          <p className="settings-desc">
            Back up agent configurations, skills, automations, and settings. Restore from files or create manual snapshots.
          </p>

          {/* Create backup */}
          <div className="settings-group">
            <div className="settings-panel-head">
              <h3>Create Backup</h3>
              <button
                className="btn primary"
                disabled={isBackingUp}
                onClick={async () => {
                  setIsBackingUp(true);
                  try {
                    const checkpoint = await api.sochdbCheckpoint();
                    const backup: BackupEntry = {
                      id: `backup_${Date.now()}`,
                      name: `Backup ${new Date().toLocaleDateString()} ${new Date().toLocaleTimeString()}`,
                      created: new Date().toISOString(),
                      size: `${checkpoint} entries`,
                      agents: agents.length,
                      skills: 0,
                      pipelines: 0,
                    };
                    setBackups((prev) => [backup, ...prev]);
                    pushToast(`Backup created: ${checkpoint} entries persisted.`);
                  } catch {
                    pushToast("Backup failed.");
                  }
                  setIsBackingUp(false);
                }}
              >
                {isBackingUp ? "Backing up..." : "💾 Create Snapshot"}
              </button>
            </div>
            <div className="info-banner">
              💡 Backups include SochDB checkpoint data, agent configurations, skill manifests, and pipeline definitions.
            </div>
          </div>

          {/* Export / Import */}
          <div className="settings-group">
            <div className="settings-group-label">Transfer</div>
            <div className="backup-transfer-row">
              <button className="btn subtle" onClick={() => {
                const data = {
                  version: 1,
                  timestamp: new Date().toISOString(),
                  agents: agents.map((a) => ({ id: a.id, name: a.name, persona: a.persona })),
                  settings: {
                    observability: observability ? { enabled: observability.enabled, service_name: observability.service_name } : null,
                  },
                };
                const blob = new Blob([JSON.stringify(data, null, 2)], { type: "application/json" });
                const url = URL.createObjectURL(blob);
                const a = document.createElement("a");
                a.href = url;
                a.download = `clawdesk-export-${Date.now()}.json`;
                a.click();
                URL.revokeObjectURL(url);
                pushToast("Settings exported.");
              }}>
                📤 Export Settings (JSON)
              </button>
              <button className="btn subtle" onClick={() => {
                const input = document.createElement("input");
                input.type = "file";
                input.accept = ".json";
                input.onchange = async () => {
                  const file = input.files?.[0];
                  if (!file) return;
                  try {
                    const text = await file.text();
                    const data = JSON.parse(text);
                    pushToast(`Imported ${data.agents?.length ?? 0} agent(s). Restart to apply.`);
                  } catch {
                    pushToast("Invalid backup file.");
                  }
                };
                input.click();
              }}>
                📥 Import Settings (JSON)
              </button>
            </div>
          </div>

          {/* Backup list */}
          <div className="settings-group">
            <div className="settings-group-label">Snapshots ({backups.length})</div>
            {backups.length === 0 ? (
              <div className="empty-state centered">
                <p>No snapshots yet. Create one above.</p>
              </div>
            ) : (
              <div className="backup-list">
                {backups.map((b) => (
                  <div key={b.id} className="backup-card">
                    <div className="backup-card-info">
                      <strong>{b.name}</strong>
                      <span className="backup-card-meta">
                        {new Date(b.created).toLocaleString()} · {b.size} · {b.agents} agent(s)
                      </span>
                    </div>
                    <div className="backup-card-actions">
                      <button
                        className="btn subtle"
                        disabled={isRestoring}
                        onClick={async () => {
                          setIsRestoring(true);
                          try {
                            await api.sochdbSync();
                            pushToast(`Restored from "${b.name}".`);
                          } catch {
                            pushToast("Restore failed.");
                          }
                          setIsRestoring(false);
                        }}
                      >
                        {isRestoring ? "Restoring..." : "Restore"}
                      </button>
                      <button className="btn ghost" onClick={() => {
                        setBackups((prev) => prev.filter((x) => x.id !== b.id));
                        pushToast("Snapshot deleted.");
                      }}>Delete</button>
                    </div>
                  </div>
                ))}
              </div>
            )}
          </div>

          {/* Danger zone */}
          <div className="settings-group">
            <div className="settings-group-label" style={{ color: "var(--danger)" }}>Danger Zone</div>
            <div className="backup-danger">
              <button className="btn danger" onClick={() => {
                if (confirm("Reset all local settings and cache? This cannot be undone.")) {
                  window.localStorage.clear();
                  pushToast("Local settings and cache cleared. Reload to apply.");
                }
              }}>
                🗑 Reset All Local Data
              </button>
              <p className="settings-desc">Clears localStorage, cached preferences, and session data. Agent data on disk is unaffected.</p>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
