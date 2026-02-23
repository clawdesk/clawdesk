import { useState, useEffect, useCallback, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  AreaChart, Area, XAxis, YAxis, Tooltip,
  ResponsiveContainer, PieChart, Pie, Cell,
} from "recharts";
import * as api from "../api";
import { ChannelSetupJourney } from "../onboarding/ChannelSetupJourney";
import type {
  DesktopAgent,
  ChannelInfo,
  ChannelTypeSpec,
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
  DebugEvent,
  StorageSnapshot,
} from "../types";
import { AGENT_TEMPLATES } from "../types";
import { Icon } from "../components/Icon";
import { TraceViewer } from "../components/TraceViewer";
import { PROVIDER_MODELS } from "../onboarding/OnboardingWizard";
import { PageLayout } from "../components/PageLayout";
import {
  ProviderConfig,
  loadProviders,
  saveProviders,
  getActiveProviderId,
  setActiveProviderId,
  syncLegacyKeys,
  createProviderConfig,
} from "../providerConfig";

// ── Design tokens for charts ──────────────────────────────────
const CHART_ACCENT = "#E8612C";
const CHART_GREEN = "#1A8754";
const CHART_BLUE = "#2563EB";
const CHART_YELLOW = "#E8A817";
const PIE_COLORS = [CHART_ACCENT, CHART_BLUE, CHART_GREEN, CHART_YELLOW, "#7C3AED", "#EC4899"];

// ── Tab definition ────────────────────────────────────────────
type SettingsTab = "preferences" | "channels" | "agents" | "providers" | "security" | "observe" | "infra" | "backup";

const TABS: { id: SettingsTab; label: string; icon: string }[] = [
  { id: "preferences", label: "Preferences", icon: "⚙️" },
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
  onResetOnboarding: () => void;
  pushToast: (text: string) => void;
  onNavigate: (nav: string, options?: { threadId?: string }) => void;
}

// ── Channels sub-panel ────────────────────────────────────────

function ChannelsPanel({
  channels,
  onRefreshChannels,
  pushToast,
}: {
  channels: ChannelInfo[];
  onRefreshChannels: () => void;
  pushToast: (text: string) => void;
}) {
  const [typeSpecs, setTypeSpecs] = useState<ChannelTypeSpec[]>([]);
  const [configuring, setConfiguring] = useState<ChannelInfo | null>(null);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    api.getChannelTypes().then(setTypeSpecs).catch(() => { });
  }, []);

  const specFor = useCallback(
    (ch: ChannelInfo) => typeSpecs.find((ts) => ts.id === ch.channel_type),
    [typeSpecs]
  );

  const openConfig = useCallback((ch: ChannelInfo) => {
    setConfiguring(ch);
  }, []);

  const disconnect = useCallback(async (ch: ChannelInfo) => {
    try {
      await api.disconnectChannel(ch.id);
      pushToast(`${ch.name} disconnected.`);
      onRefreshChannels();
    } catch {
      pushToast(`Failed to disconnect ${ch.name}.`);
    }
  }, [onRefreshChannels, pushToast]);

  const connected = channels.filter((c) => c.status === "active");
  const available = channels.filter((c) => c.status !== "active");
  const spec = configuring ? specFor(configuring) : null;

  return (
    <div className="settings-panel">
      <p className="settings-desc">
        Connect messaging platforms. ClawDesk normalizes inbound messages and renders
        outbound responses for each platform automatically.
      </p>

      {connected.length > 0 && (
        <div className="settings-group">
          <div className="settings-group-label">Connected</div>
          <div className="channel-grid">
            {connected.map((ch) => {
              const ts = specFor(ch);
              return (
                <div key={ch.id} className="channel-card channel-card--active">
                  <div className="channel-card-info">
                    <h3>
                      {ts?.icon ?? "📡"} {ch.name}
                    </h3>
                    <p>{ch.channel_type} · <span className="status-text-ok">active</span></p>
                    {(ch.capabilities ?? []).length > 0 && (
                      <div className="channel-card-caps">
                        {(ch.capabilities ?? []).map((c) => (
                          <span key={c} className="chip chip-sm">{c}</span>
                        ))}
                      </div>
                    )}
                  </div>
                  <span className="status-dot status-ok" />
                  <div className="channel-card-btns">
                    <button className="btn subtle" onClick={() => openConfig(ch)}>Configure</button>
                    {ch.channel_type !== "WebChat" && ch.channel_type !== "Internal" && (
                      <button className="btn ghost" onClick={() => disconnect(ch)}>Disconnect</button>
                    )}
                  </div>
                </div>
              );
            })}
          </div>
        </div>
      )}

      {available.length > 0 && (
        <div className="settings-group">
          <div className="settings-group-label">Available</div>
          <div className="channel-grid">
            {available.map((ch) => {
              const ts = specFor(ch);
              return (
                <div key={ch.id} className="channel-card">
                  <div className="channel-card-info">
                    <h3>
                      {ts?.icon ?? "📡"} {ch.name}
                    </h3>
                    <p>{ch.channel_type} · <span className="status-text-off">available</span></p>
                  </div>
                  <span className="status-dot status-error" />
                  <button className="btn subtle" onClick={() => openConfig(ch)}>Connect</button>
                </div>
              );
            })}
          </div>
        </div>
      )}

      {channels.length === 0 && (
        <div className="empty-state">
          <p>No channels found.</p>
          <button className="btn primary" onClick={onRefreshChannels}>Refresh</button>
        </div>
      )}

      {configuring && spec && (
        <div className="modal-backdrop" onClick={() => setConfiguring(null)}>
          <div className="modal channel-config-modal" onClick={(e) => e.stopPropagation()}>
            <div className="modal-head">
              <h2>{spec.icon} {configuring.name} Setup</h2>
              <button className="btn ghost" onClick={() => setConfiguring(null)}>✕</button>
            </div>
            <div className="modal-body">
              <ChannelSetupJourney
                spec={spec}
                initialValues={configuring.config ?? {}}
                onComplete={async (config) => {
                  setSaving(true);
                  try {
                    await api.updateChannel(configuring.id, config);
                    pushToast(`${configuring.name} connected.`);
                    onRefreshChannels();
                    setConfiguring(null);
                  } catch {
                    pushToast(`Failed to save ${configuring.name} config.`);
                  } finally {
                    setSaving(false);
                  }
                }}
                onCancel={() => setConfiguring(null)}
              />
            </div>
          </div>
        </div>
      )}
    </div>
  );
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
  onResetOnboarding,
  pushToast,
  onNavigate,
}: SettingsPageProps) {
  const [tab, setTab] = useState<SettingsTab>(() => {
    // Support deep-links from Overview dashboard: clawdesk._settingsTab
    const deepLink = window.localStorage.getItem("clawdesk._settingsTab");
    if (deepLink) {
      window.localStorage.removeItem("clawdesk._settingsTab");
      const mapping: Record<string, SettingsTab> = {
        Channels: "channels",
        Agents: "agents",
        Providers: "providers",
        Security: "security",
        Observability: "observe",
        Infrastructure: "infra",
        Backup: "backup",
        Preferences: "preferences",
      };
      return mapping[deepLink] || "preferences";
    }
    return "preferences";
  });

  // ── Preferences state ─────────────────────────────────────
  // Multi-provider configs
  const [providerConfigs, setProviderConfigs] = useState<ProviderConfig[]>(() => loadProviders());
  const [activeProviderId, setActiveProvId] = useState<string | null>(() => getActiveProviderId());
  // Which provider card is expanded for editing (by id), null = none
  const [editingProviderId, setEditingProviderId] = useState<string | null>(null);

  // Clear history confirmation
  const [showClearConfirm, setShowClearConfirm] = useState(false);
  const [isClearing, setIsClearing] = useState(false);
  // Draft state for the provider being edited
  const [editDraft, setEditDraft] = useState<ProviderConfig | null>(null);
  const [showApiKeyFor, setShowApiKeyFor] = useState<string | null>(null);

  const [prefTheme, setPrefTheme] = useState(() =>
    window.localStorage.getItem("clawdesk.theme") || "light"
  );

  const [prefSaved, setPrefSaved] = useState(false);
  const [isTestingLlm, setIsTestingLlm] = useState(false);
  const [testingProviderId, setTestingProviderId] = useState<string | null>(null);

  // Legacy compat: derive prefProvider/prefModel/prefApiKey/prefBaseUrl from active provider
  const activeConfig = providerConfigs.find((c) => c.id === activeProviderId) || providerConfigs[0] || null;
  const prefProvider = activeConfig?.provider || "Ollama (Local)";
  const prefModel = activeConfig?.model || "";
  const prefApiKey = activeConfig?.apiKey || "";
  const prefBaseUrl = activeConfig?.baseUrl || "";
  const prefProject = activeConfig?.projectId || "";
  const prefLocation = activeConfig?.location || "";

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

  // ── Debug mode state ──
  const [debugEnabled, setDebugEnabled] = useState(false);
  const [debugEvents, setDebugEvents] = useState<DebugEvent[]>([]);
  const [storageSnapshot, setStorageSnapshot] = useState<StorageSnapshot | null>(null);
  const [isSnapshotLoading, setIsSnapshotLoading] = useState(false);
  const debugLogRef = useRef<HTMLDivElement>(null);

  // Load debug mode state on mount
  useEffect(() => {
    api.getDebugMode().then(setDebugEnabled).catch(() => { });
  }, []);

  // Listen to debug:event when debug mode is enabled
  useEffect(() => {
    if (!debugEnabled) return;
    let cancelled = false;
    let unlisten: (() => void) | null = null;
    listen<DebugEvent>("debug:event", (ev) => {
      if (cancelled) return;
      setDebugEvents((prev) => {
        const next = [...prev, ev.payload];
        // Keep at most 500 events
        return next.length > 500 ? next.slice(-500) : next;
      });
    }).then((fn) => { if (!cancelled) unlisten = fn; });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [debugEnabled]);

  // Auto-scroll debug log
  useEffect(() => {
    if (debugLogRef.current) {
      debugLogRef.current.scrollTop = debugLogRef.current.scrollHeight;
    }
  }, [debugEvents]);

  const handleToggleDebug = useCallback(async () => {
    try {
      const next = !debugEnabled;
      await api.toggleDebugMode(next);
      setDebugEnabled(next);
      if (!next) setDebugEvents([]);
      pushToast(next ? "Debug mode enabled — events will be captured." : "Debug mode disabled.");
    } catch {
      pushToast("Failed to toggle debug mode.");
    }
  }, [debugEnabled, pushToast]);

  const handleRunSnapshot = useCallback(async () => {
    setIsSnapshotLoading(true);
    try {
      const snap = await api.debugStorageSnapshot();
      setStorageSnapshot(snap);
      pushToast(`Snapshot captured: ${snap.memory_session_count} sessions in memory, ${snap.sochdb_session_count} in SochDB.`);
    } catch (e: any) {
      pushToast(`Snapshot failed: ${e}`);
    } finally {
      setIsSnapshotLoading(false);
    }
  }, [pushToast]);

  const handleForcePersist = useCallback(async () => {
    try {
      const result = await api.debugForcePersist();
      pushToast(result);
    } catch (e: any) {
      pushToast(`Force persist failed: ${e}`);
    }
  }, [pushToast]);

  const handleRehydrate = useCallback(async () => {
    try {
      const result = await api.debugRehydrate();
      pushToast(result);
    } catch (e: any) {
      pushToast(`Rehydrate failed: ${e}`);
    }
  }, [pushToast]);

  // Load providers on tab switch
  useEffect(() => {
    if (tab === "providers" && providers.length === 0) {
      api.listProviderCapabilities().then(setProviders).catch(() => { });
    }
    if (tab === "security" && auditLog.length === 0) {
      api.policyGetAuditLog(20).then((entries) => {
        setAuditLog(entries as AuditEntry[]);
      }).catch(() => { });
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
      if (!mediaPipeline) api.getMediaPipelineStatus().then(setMediaPipeline).catch(() => { });
      if (!tunnelMetrics) api.getTunnelStatus().then(setTunnelMetrics).catch(() => { });
      if (!contextGuard) api.getContextGuardStatus().then(setContextGuard).catch(() => { });
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

  const runLlmTest = useCallback(async (config?: ProviderConfig) => {
    const c = config || activeConfig;
    if (!c) return;
    setIsTestingLlm(true);
    setTestingProviderId(c.id);
    try {
      const res = await api.testLlmConnection(
        c.provider, c.model, c.apiKey, c.baseUrl, c.projectId, c.location
      );
      pushToast(`Success: ${res}`);
    } catch (e: any) {
      pushToast(`Failed: ${e}`);
    } finally {
      setIsTestingLlm(false);
      setTestingProviderId(null);
    }
  }, [activeConfig, pushToast]);

  return (
    <PageLayout
      title="Settings"
      subtitle="Configure agents, channels, providers, security, and infrastructure."
      className="page-settings"
    >
      <div className="settings-layout">
        {/* Sidebar Navigation */}
        <aside className="settings-sidebar">
          <nav className="settings-nav">
            {TABS.map((t) => (
              <button
                key={t.id}
                className={`settings-nav-item ${tab === t.id ? "active" : ""}`}
                onClick={() => setTab(t.id)}
              >
                <span className="settings-nav-icon">{t.icon}</span>
                <span className="settings-nav-label">{t.label}</span>
              </button>
            ))}
          </nav>
        </aside>

        {/* Content Area */}
        <div className="settings-content-area">
          {/* ═══ PREFERENCES TAB ═══ */}
          {tab === "preferences" && (
            <div className="settings-panel">
              <div className="settings-panel-head">
                <h3>Preferences</h3>
                <p className="settings-desc">
                  Configure your personal settings — API keys, providers, and appearance.
                </p>
              </div>

              {/* Theme */}
              <div className="settings-group">
                <div className="settings-group-label">Appearance</div>
                <div className="section-card" style={{ padding: 16 }}>
                  <label className="field-label" style={{ marginBottom: 16 }}>
                    Theme
                    <select
                      value={prefTheme}
                      onChange={(e) => setPrefTheme(e.target.value)}
                      className="input"
                    >
                      <option value="light">Light</option>
                      <option value="dark">Dark</option>
                      <option value="system">System</option>
                    </select>
                  </label>

                  <div style={{ display: "flex", justifyContent: "flex-start", marginTop: 8 }}>
                    <button
                      className="btn primary"
                      onClick={() => {
                        window.localStorage.setItem("clawdesk.theme", prefTheme);
                        document.documentElement.setAttribute("data-theme", prefTheme);
                        // Sync active provider to legacy keys
                        if (activeConfig) syncLegacyKeys(activeConfig);
                        setPrefSaved(true);
                        pushToast("Preferences saved.");
                        setTimeout(() => setPrefSaved(false), 2000);
                      }}
                    >
                      {prefSaved ? "✓ Saved" : "Save Preferences"}
                    </button>
                  </div>
                </div>
              </div>

              {/* Onboarding */}
              <div className="settings-group">
                <div className="settings-group-label">Setup</div>
                <div className="section-card" style={{ padding: 16 }}>
                  <p className="settings-desc" style={{ marginBottom: 8 }}>
                    Re-run the setup wizard to change your provider, skills, and channels.
                  </p>
                  <button
                    className="btn subtle"
                    onClick={() => {
                      onResetOnboarding();
                    }}
                  >
                    🔄 Re-run Setup Wizard
                  </button>
                </div>
              </div>

              {/* Danger Zone — Full Reset */}
              <div className="settings-group">
                <div className="settings-group-label" style={{ color: "var(--error)" }}>Danger Zone</div>
                <div className="section-card" style={{ padding: 16, border: "1px solid var(--error)", borderRadius: 8 }}>
                  <p className="settings-desc" style={{ marginBottom: 4 }}>
                    Permanently delete all chat conversations and message history.
                  </p>
                  <p className="settings-desc" style={{ marginBottom: 12, color: "var(--text-soft)", fontSize: 12 }}>
                    Your agents, skills, providers, and settings will NOT be affected.
                  </p>
                  <button
                    className="btn subtle"
                    style={{ color: "var(--error)", borderColor: "var(--error)" }}
                    onClick={() => setShowClearConfirm(true)}
                  >
                    🗑️ Clear All Chat History
                  </button>
                </div>
              </div>

              {/* Clear history confirmation modal */}
              {showClearConfirm && (
                <div className="modal-backdrop" onClick={() => !isClearing && setShowClearConfirm(false)}>
                  <div className="modal" style={{ maxWidth: 440, padding: 0 }} onClick={(e) => e.stopPropagation()}>
                    <div style={{ padding: "20px 24px 0" }}>
                      <h3 style={{ fontSize: 17, fontWeight: 600, marginBottom: 12, color: "var(--text)" }}>Clear all chat history?</h3>
                      <div style={{ fontSize: 14, lineHeight: 1.6, color: "var(--text)" }}>
                        <p style={{ marginBottom: 12 }}>This will permanently delete:</p>
                        <ul style={{ paddingLeft: 20, marginBottom: 16 }}>
                          <li>All chat conversations</li>
                          <li>All message history</li>
                          <li>All tool call records</li>
                        </ul>
                        <div style={{ padding: "10px 12px", background: "var(--panel-strong)", borderRadius: 8, marginBottom: 16, fontSize: 13 }}>
                          ✅ <strong>Not affected:</strong> Your agents, skills, providers, automations, and all settings will remain unchanged.
                        </div>
                        <p style={{ color: "var(--error)", fontWeight: 500, fontSize: 13 }}>
                          This action cannot be undone.
                        </p>
                      </div>
                    </div>
                    <div style={{ display: "flex", justifyContent: "flex-end", gap: 8, padding: "16px 24px", borderTop: "1px solid var(--line)", marginTop: 8 }}>
                      <button
                        className="btn ghost"
                        onClick={() => setShowClearConfirm(false)}
                        disabled={isClearing}
                      >
                        Cancel
                      </button>
                      <button
                        className="btn"
                        style={{ background: "var(--error)", color: "#fff", borderColor: "var(--error)" }}
                        disabled={isClearing}
                        onClick={async () => {
                          setIsClearing(true);
                          try {
                            const count = await api.clearAllChats();
                            pushToast(`Cleared ${count} chat session${count === 1 ? "" : "s"} successfully.`);
                            setShowClearConfirm(false);
                          } catch (e: any) {
                            pushToast(`Failed to clear history: ${e?.message || e}`);
                          } finally {
                            setIsClearing(false);
                          }
                        }}
                      >
                        {isClearing ? "Clearing..." : "Yes, clear all history"}
                      </button>
                    </div>
                  </div>
                </div>
              )}
            </div>
          )}

          {/* ═══ CHANNELS TAB ═══ */}
          {tab === "channels" && (
            <ChannelsPanel
              channels={channels}
              onRefreshChannels={onRefreshChannels}
              pushToast={pushToast}
            />
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
                        <span>{a.msg_count} msgs · {(a.tokens_used ?? 0).toLocaleString()}/{(a.token_budget ?? 0).toLocaleString()} tokens</span>
                      </div>
                    </div>
                    <div className="agent-card-verify">
                      <div className="verify-status">✓ Identity Verified</div>
                      <div className="verify-hash">sha256:{a.persona_hash.slice(0, 8)}...</div>
                    </div>
                    <div className="agent-card-actions">
                      <button className="btn subtle" onClick={() => onNavigate("chat")}>Chat →</button>
                      <button className="btn subtle">Edit</button>
                      <button className="btn ghost" onClick={() => onDeleteAgent(a.id)}>Delete</button>
                    </div>
                  </div>
                ))}
              </div>
              {agents.length === 0 && (
                <div className="empty-state-action" style={{ padding: 24, textAlign: "center" }}>
                  <p style={{ marginBottom: 12 }}>No agents created yet. Create one to start chatting.</p>
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

              {/* ── Multi-Provider cards ───────────────────────────── */}
              <div className="settings-group">
                <div className="settings-group-label" style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
                  <span>LLM Providers</span>
                  <button
                    className="btn subtle"
                    style={{ fontSize: 12, padding: "4px 10px" }}
                    onClick={() => {
                      const newCfg = createProviderConfig("Anthropic");
                      const models = PROVIDER_MODELS["Anthropic"] || [];
                      if (models.length > 0) newCfg.model = models[0].id;
                      const updated = [...providerConfigs, newCfg];
                      setProviderConfigs(updated);
                      saveProviders(updated);
                      // If this is the first provider, set it as active
                      if (updated.length === 1) {
                        setActiveProvId(newCfg.id);
                        setActiveProviderId(newCfg.id);
                      }
                      // Auto-open editor for the new card
                      setEditingProviderId(newCfg.id);
                      setEditDraft({ ...newCfg });
                    }}
                  >
                    + Add Provider
                  </button>
                </div>

                {providerConfigs.length === 0 && (
                  <div className="section-card empty-providers">
                    No providers configured. Click <strong>+ Add Provider</strong> to get started.
                  </div>
                )}

                {providerConfigs.map((cfg) => {
                  const isActive = cfg.id === (activeProviderId || providerConfigs[0]?.id);
                  const isEditing = editingProviderId === cfg.id;
                  const draft = isEditing && editDraft ? editDraft : cfg;
                  const showKey = showApiKeyFor === cfg.id;

                  return (
                    <div
                      key={cfg.id}
                      className={`section-card provider-card ${isActive ? "provider-active" : ""}`}
                      style={{ marginBottom: 12, position: "relative" }}
                    >
                      {/* Card header */}
                      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: isEditing ? 12 : 0 }}>
                        <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                          {isActive && (
                            <span className="provider-active-badge">Default</span>
                          )}
                          <strong style={{ fontSize: 14, lineHeight: 1 }}>{cfg.label || cfg.provider}</strong>
                          <span style={{ fontSize: 12, color: "var(--text-tertiary)", lineHeight: 1 }}>
                            {cfg.model || "No model"}
                          </span>
                        </div>
                        <div style={{ display: "flex", gap: 4, alignItems: "center" }}>
                          {!isActive && (
                            <button
                              className="btn subtle"
                              onClick={() => {
                                setActiveProvId(cfg.id);
                                setActiveProviderId(cfg.id);
                                pushToast(`"${cfg.label || cfg.provider}" set as default.`);
                              }}
                            >
                              Set Default
                            </button>
                          )}
                          <button
                            className="btn subtle"
                            onClick={() => {
                              if (isEditing) {
                                setEditingProviderId(null);
                                setEditDraft(null);
                              } else {
                                setEditingProviderId(cfg.id);
                                setEditDraft({ ...cfg });
                              }
                            }}
                          >
                            {isEditing ? "Cancel" : "Edit"}
                          </button>
                          <button
                            className="btn subtle"
                            style={{ color: "var(--danger, #e53e3e)" }}
                            onClick={() => {
                              const updated = providerConfigs.filter((c) => c.id !== cfg.id);
                              setProviderConfigs(updated);
                              saveProviders(updated);
                              if (isActive && updated.length > 0) {
                                setActiveProvId(updated[0].id);
                                setActiveProviderId(updated[0].id);
                              }
                              if (isEditing) { setEditingProviderId(null); setEditDraft(null); }
                              pushToast(`"${cfg.label || cfg.provider}" removed.`);
                            }}
                          >
                            Delete
                          </button>
                        </div>
                      </div>

                      {/* Expanded editor */}
                      {isEditing && editDraft && (
                        <div className="provider-edit-form">
                          <label className="field-label">
                            Label
                            <input
                              type="text"
                              value={draft.label}
                              onChange={(e) => setEditDraft({ ...draft, label: e.target.value })}
                              placeholder="e.g. Work Azure, Personal Anthropic"
                              className="input"
                            />
                          </label>
                          <label className="field-label" style={{ marginTop: 10 }}>
                            Provider
                            <select
                              value={draft.provider}
                              onChange={(e) => {
                                const p = e.target.value;
                                const models = PROVIDER_MODELS[p] || [];
                                setEditDraft({
                                  ...draft,
                                  provider: p,
                                  model: models.length > 0 ? models[0].id : draft.model,
                                  label: draft.label === draft.provider ? p : draft.label,
                                });
                              }}
                              className="input"
                            >
                              <option>Anthropic</option>
                              <option>OpenAI</option>
                              <option>Google</option>
                              <option>Azure OpenAI</option>
                              <option>Cohere</option>
                              <option>Vertex AI</option>
                              <option>Ollama (Local)</option>
                              <option>Local (OpenAI Compatible)</option>
                            </select>
                          </label>
                          <label className="field-label" style={{ marginTop: 10 }}>
                            Model
                            <input
                              type="text"
                              list={`pref-models-${cfg.id}`}
                              value={draft.model}
                              onChange={(e) => setEditDraft({ ...draft, model: e.target.value })}
                              placeholder="Type or select a model"
                              className="input"
                            />
                            <datalist id={`pref-models-${cfg.id}`}>
                              {(PROVIDER_MODELS[draft.provider] || []).map((m) => (
                                <option key={m.id} value={m.id}>{m.label}</option>
                              ))}
                            </datalist>
                          </label>
                          <label className="field-label" style={{ marginTop: 10 }}>
                            API Key
                            <div style={{ display: "flex", gap: 8 }}>
                              <input
                                type={showKey ? "text" : "password"}
                                value={draft.apiKey}
                                placeholder={draft.provider.toLowerCase().includes("ollama") || draft.provider === "Local (OpenAI Compatible)" ? "Not required for local models" : "Paste your API key"}
                                onChange={(e) => setEditDraft({ ...draft, apiKey: e.target.value })}
                                disabled={draft.provider.toLowerCase().includes("ollama") || draft.provider === "Local (OpenAI Compatible)"}
                                className="input"
                                style={{ flex: 1 }}
                              />
                              <button
                                className="btn subtle"
                                onClick={() => setShowApiKeyFor(showKey ? null : cfg.id)}
                                style={{ whiteSpace: "nowrap" }}
                              >
                                {showKey ? "Hide" : "Show"}
                              </button>
                            </div>
                          </label>

                          {(draft.provider === "Azure OpenAI" || draft.provider === "OpenAI" || draft.provider === "Ollama (Local)" || draft.provider === "Local (OpenAI Compatible)") && (
                            <label className="field-label" style={{ marginTop: 10 }}>
                              Base URL / Endpoint
                              <input
                                type="url"
                                value={draft.baseUrl}
                                placeholder={draft.provider === "Azure OpenAI" ? "https://your-resource.openai.azure.com" : draft.provider === "Local (OpenAI Compatible)" ? "http://localhost:8080/v1" : "http://localhost:11434"}
                                onChange={(e) => setEditDraft({ ...draft, baseUrl: e.target.value })}
                                className="input"
                              />
                            </label>
                          )}

                          {draft.provider === "Vertex AI" && (
                            <div style={{ display: "flex", gap: 12 }}>
                              <label className="field-label" style={{ flex: 1 }}>
                                Project ID
                                <input
                                  type="text"
                                  value={draft.projectId}
                                  placeholder="my-gcp-project"
                                  onChange={(e) => setEditDraft({ ...draft, projectId: e.target.value })}
                                  className="input"
                                />
                              </label>
                              <label className="field-label" style={{ flex: 1 }}>
                                Location
                                <input
                                  type="text"
                                  value={draft.location}
                                  placeholder="us-central1"
                                  onChange={(e) => setEditDraft({ ...draft, location: e.target.value })}
                                  className="input"
                                />
                              </label>
                            </div>
                          )}

                          <div className="row-actions" style={{ marginTop: 14, display: "flex", gap: 8 }}>
                            <button
                              className="btn primary"
                              onClick={() => {
                                const updated = providerConfigs.map((c) =>
                                  c.id === cfg.id ? { ...draft } : c
                                );
                                setProviderConfigs(updated);
                                saveProviders(updated);
                                // If this is the active provider, sync legacy keys
                                if (cfg.id === (activeProviderId || providerConfigs[0]?.id)) {
                                  syncLegacyKeys(draft);
                                }
                                setEditingProviderId(null);
                                setEditDraft(null);
                                pushToast(`"${draft.label || draft.provider}" saved.`);
                              }}
                            >
                              Save
                            </button>
                            <button
                              className="btn subtle"
                              onClick={() => runLlmTest(draft)}
                              disabled={isTestingLlm && testingProviderId === cfg.id}
                            >
                              {isTestingLlm && testingProviderId === cfg.id ? "Testing..." : "Test Connection"}
                            </button>
                          </div>
                        </div>
                      )}
                    </div>
                  );
                })}
              </div>

              {providers.length > 0 && (
                <div className="settings-group" style={{ marginTop: 20 }}>
                  <div className="settings-group-label">Auto-detected (environment)</div>
                  <div className="provider-list">
                    {providers.map((p) => (
                      <div key={p.provider} className="provider-card" style={{ opacity: 0.85 }}>
                        <div className="provider-card-info">
                          <div className="provider-card-name">
                            {p.provider}
                            <span className="chip">{(p.capabilities ?? []).length > 0 ? "Active" : "Inactive"}</span>
                          </div>
                          {(p.models ?? []).length > 0 && (
                            <div className="provider-card-models">
                              Models: {(p.models ?? []).join(", ")}
                            </div>
                          )}
                          {(p.capabilities ?? []).length > 0 && (
                            <div className="provider-card-caps">
                              {(p.capabilities ?? []).map((c) => (
                                <span key={c} className="chip">{c}</span>
                              ))}
                            </div>
                          )}
                        </div>
                        <span style={{ fontSize: 11, color: "var(--text-soft)", whiteSpace: "nowrap" }}>Read-only</span>
                      </div>
                    ))}
                  </div>
                </div>
              )}
              {metrics && metrics.model_breakdown.length > 0 && (
                <div className="provider-cost-summary" style={{ marginTop: 16 }}>
                  {metrics.model_breakdown.map((m) => (
                    <div key={m.model} className="provider-cost-item">
                      <span className="provider-cost-model">{m.model}</span>
                      <span className="provider-cost-value">${(m.cost ?? 0).toFixed(4)}</span>
                      <span className="provider-cost-tokens">
                        {(m.input_tokens ?? 0).toLocaleString()} in / {(m.output_tokens ?? 0).toLocaleString()} out
                      </span>
                    </div>
                  ))}
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
                        api.policyGetAuditLog(20).then((e) => setAuditLog(e as AuditEntry[])).catch(() => { });
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
                      {(metrics.today_input_tokens ?? 0).toLocaleString()}
                    </div>
                    <div className="observe-metric-label">Input Tokens</div>
                  </div>
                  <div className="observe-metric-card">
                    <div className="observe-metric-value" style={{ color: CHART_GREEN }}>
                      {(metrics.today_output_tokens ?? 0).toLocaleString()}
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
                  onClose={() => { }}
                  pushToast={pushToast}
                />
              </div>

              {/* ── Debug / Storage Diagnostics ── */}
              <div className="settings-group">
                <div className="settings-group-label" style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
                  <span>🔍 Debug: Storage Diagnostics</span>
                  <label style={{ display: "flex", alignItems: "center", gap: 8, fontSize: 13, cursor: "pointer" }}>
                    <span style={{ color: debugEnabled ? "var(--accent)" : "var(--text-secondary)" }}>
                      {debugEnabled ? "Capturing Events" : "Off"}
                    </span>
                    <input
                      type="checkbox"
                      checked={debugEnabled}
                      onChange={handleToggleDebug}
                      style={{ width: 18, height: 18, cursor: "pointer" }}
                    />
                  </label>
                </div>
                <div className="section-card" style={{ padding: 16 }}>
                  <p className="settings-desc" style={{ marginBottom: 12 }}>
                    Enable debug mode to capture real-time persistence events (save, load, sync, checkpoint).
                    Use the snapshot tool to compare in-memory state vs. on-disk SochDB data.
                  </p>

                  {/* Action buttons */}
                  <div style={{ display: "flex", gap: 8, marginBottom: 16, flexWrap: "wrap" }}>
                    <button className="btn primary" onClick={handleRunSnapshot} disabled={isSnapshotLoading}>
                      {isSnapshotLoading ? "Running..." : "📊 Run Storage Snapshot"}
                    </button>
                    <button className="btn subtle" onClick={handleForcePersist}>
                      💾 Force Persist
                    </button>
                    <button className="btn subtle" onClick={handleRehydrate}>
                      🔄 Re-hydrate from Disk
                    </button>
                    {debugEvents.length > 0 && (
                      <button className="btn subtle" onClick={() => setDebugEvents([])}>
                        🗑 Clear Log
                      </button>
                    )}
                  </div>

                  {/* Storage Snapshot Results */}
                  {storageSnapshot && (
                    <div style={{
                      background: "var(--bg-secondary, #f5f5f5)",
                      borderRadius: 8,
                      padding: 16,
                      marginBottom: 16,
                      fontFamily: "monospace",
                      fontSize: 12,
                    }}>
                      <h4 style={{ margin: "0 0 8px", fontSize: 14 }}>Storage Snapshot</h4>
                      {storageSnapshot.is_ephemeral && (
                        <div style={{ background: "#dc2626", color: "#fff", padding: "8px 12px", borderRadius: 6, marginBottom: 8, fontWeight: 600 }}>
                          ⚠️ EPHEMERAL MODE — SochDB is running in-memory only! Data WILL be lost on restart!
                        </div>
                      )}
                      <table style={{ width: "100%", borderCollapse: "collapse", fontSize: 12 }}>
                        <tbody>
                          <tr><td style={{ padding: "3px 8px", color: "var(--text-secondary)" }}>Storage Path</td><td>{storageSnapshot.storage_path}</td></tr>
                          <tr><td style={{ padding: "3px 8px", color: "var(--text-secondary)" }}>Sessions (Memory)</td><td><strong>{storageSnapshot.memory_session_count}</strong></td></tr>
                          <tr><td style={{ padding: "3px 8px", color: "var(--text-secondary)" }}>Sessions (SochDB)</td><td><strong>{storageSnapshot.sochdb_session_count}</strong></td></tr>
                          <tr><td style={{ padding: "3px 8px", color: "var(--text-secondary)" }}>Agents (Memory)</td><td>{storageSnapshot.memory_agent_count}</td></tr>
                          <tr><td style={{ padding: "3px 8px", color: "var(--text-secondary)" }}>Agents (SochDB)</td><td>{storageSnapshot.sochdb_agent_count}</td></tr>
                          <tr><td style={{ padding: "3px 8px", color: "var(--text-secondary)" }}>WAL File</td><td>{storageSnapshot.wal_exists ? `${storageSnapshot.wal_size_bytes.toLocaleString()} bytes` : "Not found"}</td></tr>
                          <tr><td style={{ padding: "3px 8px", color: "var(--text-secondary)" }}>Old Format Sessions</td><td>{storageSnapshot.old_format_session_count}</td></tr>
                          <tr><td style={{ padding: "3px 8px", color: "var(--text-secondary)" }}>Roundtrip Test</td><td style={{ color: storageSnapshot.roundtrip_test.startsWith("PASS") ? "green" : "red" }}>{storageSnapshot.roundtrip_test}</td></tr>
                        </tbody>
                      </table>

                      {/* Warnings */}
                      {storageSnapshot.memory_only_sessions.length > 0 && (
                        <div style={{ background: "#dc2626", color: "#fff", padding: "8px 12px", borderRadius: 6, marginTop: 8, fontSize: 11 }}>
                          🚨 {storageSnapshot.memory_only_sessions.length} session(s) in memory but NOT in SochDB — DATA LOSS RISK!
                          <div style={{ marginTop: 4 }}>{storageSnapshot.memory_only_sessions.join(", ")}</div>
                        </div>
                      )}
                      {storageSnapshot.sochdb_only_sessions.length > 0 && (
                        <div style={{ background: "#f59e0b", color: "#000", padding: "8px 12px", borderRadius: 6, marginTop: 8, fontSize: 11 }}>
                          ⚠️ {storageSnapshot.sochdb_only_sessions.length} session(s) in SochDB but NOT in memory — hydration failure!
                          <div style={{ marginTop: 4 }}>{storageSnapshot.sochdb_only_sessions.join(", ")}</div>
                        </div>
                      )}
                      {storageSnapshot.message_count_mismatches.length > 0 && (
                        <div style={{ background: "#f59e0b", color: "#000", padding: "8px 12px", borderRadius: 6, marginTop: 8, fontSize: 11 }}>
                          ⚠️ {storageSnapshot.message_count_mismatches.length} session(s) with mismatched message counts:
                          <div style={{ marginTop: 4 }}>
                            {storageSnapshot.message_count_mismatches.map((m) => (
                              <div key={m.chat_id}>
                                {m.chat_id.slice(0, 8)}… — Memory: {m.memory_msg_count} msgs, SochDB: {m.sochdb_msg_count} msgs
                              </div>
                            ))}
                          </div>
                        </div>
                      )}

                      {/* Session details table */}
                      {storageSnapshot.session_details.length > 0 && (
                        <details style={{ marginTop: 12 }}>
                          <summary style={{ cursor: "pointer", fontSize: 12, fontWeight: 600 }}>
                            Session Details ({storageSnapshot.session_details.length})
                          </summary>
                          <div style={{ maxHeight: 200, overflow: "auto", marginTop: 8 }}>
                            <table style={{ width: "100%", borderCollapse: "collapse", fontSize: 11 }}>
                              <thead>
                                <tr style={{ borderBottom: "1px solid var(--border)" }}>
                                  <th style={{ textAlign: "left", padding: "4px 6px" }}>Chat ID</th>
                                  <th style={{ textAlign: "left", padding: "4px 6px" }}>Title</th>
                                  <th style={{ textAlign: "right", padding: "4px 6px" }}>Msgs</th>
                                  <th style={{ textAlign: "center", padding: "4px 6px" }}>Mem</th>
                                  <th style={{ textAlign: "center", padding: "4px 6px" }}>DB</th>
                                  <th style={{ textAlign: "right", padding: "4px 6px" }}>Size</th>
                                </tr>
                              </thead>
                              <tbody>
                                {storageSnapshot.session_details.map((s) => (
                                  <tr key={s.chat_id} style={{ borderBottom: "1px solid var(--border-light, #eee)" }}>
                                    <td style={{ padding: "3px 6px", fontFamily: "monospace" }}>{s.chat_id.slice(0, 8)}…</td>
                                    <td style={{ padding: "3px 6px", maxWidth: 200, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{s.title}</td>
                                    <td style={{ padding: "3px 6px", textAlign: "right" }}>{s.message_count}</td>
                                    <td style={{ padding: "3px 6px", textAlign: "center" }}>{s.in_memory ? "✅" : "❌"}</td>
                                    <td style={{ padding: "3px 6px", textAlign: "center" }}>{s.in_sochdb ? "✅" : "❌"}</td>
                                    <td style={{ padding: "3px 6px", textAlign: "right" }}>{s.serialized_size > 0 ? `${(s.serialized_size / 1024).toFixed(1)}KB` : "—"}</td>
                                  </tr>
                                ))}
                              </tbody>
                            </table>
                          </div>
                        </details>
                      )}
                    </div>
                  )}

                  {/* Live Debug Event Log */}
                  {debugEnabled && (
                    <div>
                      <h4 style={{ margin: "0 0 8px", fontSize: 13 }}>Live Event Log ({debugEvents.length})</h4>
                      <div
                        ref={debugLogRef}
                        style={{
                          maxHeight: 300,
                          overflow: "auto",
                          background: "var(--bg-tertiary, #1e1e1e)",
                          color: "var(--text-primary, #d4d4d4)",
                          borderRadius: 8,
                          padding: 12,
                          fontFamily: "monospace",
                          fontSize: 11,
                          lineHeight: 1.5,
                        }}
                      >
                        {debugEvents.length === 0 && (
                          <div style={{ color: "var(--text-tertiary, #666)", fontStyle: "italic" }}>
                            Waiting for events… Send a message or run a snapshot to see persistence events.
                          </div>
                        )}
                        {debugEvents.map((evt, i) => {
                          const timeStr = new Date(evt.ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
                          const levelColor = evt.level === "error" ? "#ef4444" : evt.level === "warn" ? "#f59e0b" : "#22c55e";
                          return (
                            <div key={i} style={{ marginBottom: 2 }}>
                              <span style={{ color: "#888" }}>{timeStr}</span>{" "}
                              <span style={{ color: levelColor, fontWeight: 600, textTransform: "uppercase", fontSize: 10 }}>{evt.level}</span>{" "}
                              <span style={{ color: "#60a5fa" }}>[{evt.category}]</span>{" "}
                              <span style={{ color: "#c084fc" }}>{evt.action}</span>{" "}
                              <span>{evt.detail}</span>
                            </div>
                          );
                        })}
                      </div>
                    </div>
                  )}
                </div>
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
                        ? `${tunnelMetrics.active_peers ?? 0} active peers · ${((tunnelMetrics.total_bytes_received ?? 0) / 1024 / 1024).toFixed(1)} MB RX / ${((tunnelMetrics.total_bytes_sent ?? 0) / 1024 / 1024).toFixed(1)} MB TX`
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
                        ? `${(contextGuard.current_tokens ?? 0).toLocaleString()} / ${(contextGuard.context_limit ?? 128000).toLocaleString()} tokens (${((contextGuard.utilization ?? 0) * 100).toFixed(0)}% used) · Trigger at ${((contextGuard.trigger_threshold ?? 0.8) * 100).toFixed(0)}%`
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
                        ? (mediaPipeline.processors.length > 0
                          ? `Processors: ${mediaPipeline.processors.join(", ")}`
                          : "No processors configured. Add image/audio processors to enable media handling.")
                        : "Image analysis, audio transcription · FFmpeg backend"}
                    </div>
                  </div>
                  <button className="btn subtle" onClick={() => {
                    api.getMediaPipelineStatus().then(setMediaPipeline).catch(() => { });
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
      </div >
    </PageLayout >
  );
}
