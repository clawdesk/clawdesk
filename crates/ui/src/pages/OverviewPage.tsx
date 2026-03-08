import { useEffect, useState, useCallback } from "react";
import * as api from "../api";
import { PageLayout } from "../components/PageLayout";
import { Icon } from "../components/Icon";
import type {
  HealthResponse,
  CostMetrics,
  SecurityStatus,
  ObservabilityStatus,
  PluginSummary,
  PeerInfo,
  ChannelInfo,
  DesktopAgent,
  SessionSummary,
  RuntimeStatusInfo,
  DurableRunInfo,
  AgentCardInfo,
  PipelineDescriptor,
} from "../types";

interface OverviewPageProps {
  health: HealthResponse | null;
  agents: DesktopAgent[];
  channels: ChannelInfo[];
  security: SecurityStatus | null;
  metrics: CostMetrics | null;
  observability: ObservabilityStatus | null;
  plugins: PluginSummary[];
  peers: PeerInfo[];
  pushToast: (msg: string) => void;
  onNavigate: (nav: string, options?: { threadId?: string }) => void;
}

export function OverviewPage({
  health,
  agents,
  channels,
  security,
  metrics,
  observability,
  plugins,
  peers,
  pushToast,
  onNavigate,
}: OverviewPageProps) {
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [refreshing, setRefreshing] = useState(false);

  // Subsystem health for dashboard (T9)
  const [runtimeStatus, setRuntimeStatus] = useState<RuntimeStatusInfo | null>(null);
  const [durableRuns, setDurableRuns] = useState<DurableRunInfo[]>([]);
  const [a2aAgents, setA2aAgents] = useState<AgentCardInfo[]>([]);
  const [pipelineList, setPipelineList] = useState<PipelineDescriptor[]>([]);

  useEffect(() => {
    api.listSessions().then(setSessions).catch(() => { });
    // Fetch subsystem data for health ring
    api.getRuntimeStatus().then(setRuntimeStatus).catch(() => { });
    api.listDurableRuns().then(setDurableRuns).catch(() => { });
    api.listA2aAgents().then(setA2aAgents).catch(() => { });
    api.listPipelines().then(setPipelineList).catch(() => { });
  }, []);

  const handleRefresh = useCallback(async () => {
    setRefreshing(true);
    try {
      const [s, rt, runs, agents, pips] = await Promise.all([
        api.listSessions().catch(() => [] as SessionSummary[]),
        api.getRuntimeStatus().catch(() => null),
        api.listDurableRuns().catch(() => [] as DurableRunInfo[]),
        api.listA2aAgents().catch(() => [] as AgentCardInfo[]),
        api.listPipelines().catch(() => [] as PipelineDescriptor[]),
      ]);
      setSessions(s);
      setRuntimeStatus(rt);
      setDurableRuns(runs);
      setA2aAgents(agents);
      setPipelineList(pips);
      pushToast("Dashboard refreshed");
    } catch {
      pushToast("Failed to refresh");
    } finally {
      setRefreshing(false);
    }
  }, [pushToast]);

  // Derived Stats
  const activeAgents = agents.filter(a => a.status === "active" || a.status === "idle").length;
  const cost = metrics?.today_cost ?? 0;
  const pendingApprovals = 0; // Placeholder until approvals API is ready
  const activePlugins = plugins.filter(p => p.state === "active").length;
  const connectedChannels = channels.filter(c => c.status === "active").length;
  const runningDurableRuns = durableRuns.filter(r => r.state === "running").length;
  const healthySystems = [
    health?.status === "ok",
    health ? health.skills_loaded > 0 : false,
    !!health?.tunnel_active,
    a2aAgents.length > 0,
    !!runtimeStatus?.durable_runner_available,
    pipelineList.length > 0,
    peers.length > 0,
  ].filter(Boolean).length;
  const latestSession = sessions[0] ?? null;

  return (
    <PageLayout title="Dashboard" subtitle="System Overview">
      <div className="dashboard-grid">
        <section className="dashboard-hero">
          <div className="dashboard-hero__content">
            <div className="dashboard-kicker">Operations overview</div>
            <div className="dashboard-hero__headline">
              <div>
                <h2>Operational status across agents, runtime, and channels.</h2>
                <p>
                  {healthySystems}/7 core systems are healthy, {activeAgents} agents are online, and {runningDurableRuns} durable runs are currently active.
                </p>
              </div>
              <button className="dashboard-refresh-btn" onClick={handleRefresh} disabled={refreshing}>
                <Icon name="refresh" className={refreshing ? "spin" : ""} />
                <span>{refreshing ? "Refreshing" : "Refresh overview"}</span>
              </button>
            </div>
            <div className="dashboard-hero__rail">
              <div className="dashboard-hero-chip">
                <span className="dashboard-hero-chip__label">Latest session</span>
                <strong>{latestSession?.title ?? "No recent session"}</strong>
                <span>{latestSession ? formatRelativeTime(latestSession.last_activity) : "Start a chat to create activity"}</span>
              </div>
              <div className="dashboard-hero-chip">
                <span className="dashboard-hero-chip__label">Channel coverage</span>
                <strong>{connectedChannels}/{channels.length || 0} active</strong>
                <span>{channels.length ? "Communication surfaces connected" : "No channels configured yet"}</span>
              </div>
              <div className="dashboard-hero-chip dashboard-hero-chip--accent">
                <span className="dashboard-hero-chip__label">Spend today</span>
                <strong>${cost.toFixed(2)}</strong>
                <span>{activePlugins} plugins active across the workspace</span>
              </div>
            </div>
          </div>
          <div className="dashboard-hero__aside">
            <div className="dashboard-hero-card">
              <div className="dashboard-hero-card__label">Command focus</div>
              <div className="dashboard-hero-card__value">{health?.status === "ok" ? "Stable" : "Needs attention"}</div>
              <p>Prioritize runtime, skills, and federation health before scaling agent throughput.</p>
            </div>
            <div className="dashboard-hero-card dashboard-hero-card--compact">
              <div>
                <span className="dashboard-hero-card__label">Network</span>
                <strong>{peers.length} peers</strong>
              </div>
              <div>
                <span className="dashboard-hero-card__label">Pipelines</span>
                <strong>{pipelineList.length}</strong>
              </div>
              <div>
                <span className="dashboard-hero-card__label">Approvals</span>
                <strong>{pendingApprovals}</strong>
              </div>
            </div>
          </div>
        </section>

        <div className="quick-actions-row">
          <button className="quick-action-card" onClick={() => onNavigate("chat")}>
            <span className="quick-action-card__icon"><Icon name="ask" className="w-5 h-5" /></span>
            <span className="quick-action-card__body">
              <strong>New Chat</strong>
              <small>Start a fresh thread with your active team.</small>
            </span>
          </button>
          <button className="quick-action-card" onClick={() => onNavigate("automations")}>
            <span className="quick-action-card__icon"><Icon name="routines" className="w-5 h-5" /></span>
            <span className="quick-action-card__body">
              <strong>New Automation</strong>
              <small>Design a pipeline or durable run.</small>
            </span>
          </button>
          <button className="quick-action-card" onClick={() => onNavigate("skills")}>
            <span className="quick-action-card__icon"><Icon name="library" className="w-5 h-5" /></span>
            <span className="quick-action-card__body">
              <strong>Browse Skills</strong>
              <small>Inspect loaded capabilities and tools.</small>
            </span>
          </button>
          <button className="quick-action-card" onClick={() => {
            window.localStorage.setItem("clawdesk._settingsTab", "Providers");
            onNavigate("settings");
          }}>
            <span className="quick-action-card__icon"><Icon name="zap" className="w-5 h-5" /></span>
            <span className="quick-action-card__body">
              <strong>Add Provider</strong>
              <small>Expand model routing and inference options.</small>
            </span>
          </button>
        </div>

        <div className="stats-row">
          <button className="stat-card stat-card-clickable" onClick={() => {
            window.localStorage.setItem("clawdesk._settingsTab", "Agents");
            onNavigate("settings");
          }}>
            <div className="stat-header">
              <span className="stat-label">Active Agents</span>
              <div className="stat-icon-wrap" style={{ backgroundColor: `color-mix(in srgb, var(--brand) 15%, transparent)`, color: "var(--brand)" }}>
                <Icon name="bot" />
              </div>
            </div>
            <div className="stat-value">{activeAgents}</div>
            <div className="stat-meta">{agents.length} configured across workspace</div>
          </button>
          <StatCard label="Platform Cost" value={`$${cost.toFixed(2)}`} icon="brand" color="var(--cyan)" meta="Estimated spend for the current day" />
          <button className="stat-card stat-card-clickable" onClick={() => onNavigate("skills")}>
            <div className="stat-header">
              <span className="stat-label">Active Plugins</span>
              <div className="stat-icon-wrap" style={{ backgroundColor: `color-mix(in srgb, var(--purple) 15%, transparent)`, color: "var(--purple)" }}>
                <Icon name="zap" />
              </div>
            </div>
            <div className="stat-value">{activePlugins}</div>
            <div className="stat-meta">{plugins.length} discovered integrations</div>
          </button>
          <StatCard label="Pending Approvals" value={pendingApprovals.toString()} icon="shield" color="var(--amber)" meta="Approval queue is clear" />
          <button className="stat-card stat-card-clickable" onClick={() => onNavigate("a2a")}>
            <div className="stat-header">
              <span className="stat-label">A2A Agents</span>
              <div className="stat-icon-wrap" style={{ backgroundColor: `color-mix(in srgb, var(--green) 15%, transparent)`, color: "var(--green)" }}>
                <Icon name="globe" />
              </div>
            </div>
            <div className="stat-value">{a2aAgents.length}</div>
            <div className="stat-meta">Federated agents available for orchestration</div>
          </button>
          <div className="stat-card">
            <div className="stat-header">
              <span className="stat-label">Durable Runs</span>
              <div className="stat-icon-wrap" style={{ backgroundColor: `color-mix(in srgb, var(--cyan) 15%, transparent)`, color: "var(--cyan)" }}>
                <Icon name="cpu" />
              </div>
            </div>
            <div className="stat-value">{runningDurableRuns}</div>
            <div className="stat-meta">{durableRuns.length} tracked run records</div>
          </div>
        </div>

        <div className="dashboard-main-grid">
          <div className="panel-card panel-card--feature">
            <div className="panel-title-row">
              <div>
                <div className="panel-eyebrow">Agent workspace</div>
                <h3 className="panel-title">
                  <Icon name="bot" className="w-4 h-4" /> Active Agents
                </h3>
              </div>
              <button className="btn-link" onClick={() => {
                window.localStorage.setItem("clawdesk._settingsTab", "Agents");
                onNavigate("settings");
              }}>Manage →</button>
            </div>
            <div className="agent-list-compact">
              {agents.slice(0, 5).map(agent => (
                <button key={agent.id} className="agent-row agent-row-clickable" onClick={() => onNavigate("chat")}>
                  <div className="agent-icon-sm" style={{ backgroundColor: agent.color }}>
                    {agent.icon}
                  </div>
                  <div className="agent-info">
                    <div className="agent-name">{agent.name}</div>
                    <div className="agent-model">{agent.model}</div>
                  </div>
                  <div className="agent-status-pill">
                    <span className={`status-dot ${agent.status === "active" ? "status-ok" : ""}`} />
                    <span>{agent.status}</span>
                  </div>
                </button>
              ))}
              {agents.length === 0 && (
                <div className="empty-state-action">
                  <p>No agents yet</p>
                  <button className="btn subtle" onClick={() => {
                    window.localStorage.setItem("clawdesk._settingsTab", "Agents");
                    onNavigate("settings");
                  }}>Create your first agent →</button>
                </div>
              )}
            </div>
          </div>

          <div className="dashboard-side-column">
            <div className="panel-card">
              <div className="panel-title-row panel-title-row--stacked">
                <div>
                  <div className="panel-eyebrow">Reliability</div>
                  <h3 className="panel-title">
                    <Icon name="activity" className="w-4 h-4" /> System Health
                  </h3>
                </div>
                <div className="status-summary-pill">{healthySystems}/7 healthy</div>
              </div>
              <div className="health-list">
                <HealthRow
                  label="Gateway"
                  status={health?.status === "ok" ? "ok" : "warn"}
                  detail={health ? `Up ${Math.floor(health.uptime_secs / 60)}m` : "Unknown"}
                />
                <HealthRow
                  label="Skills"
                  status={health && health.skills_loaded > 0 ? "ok" : "warn"}
                  detail={health ? `${health.skills_loaded} loaded` : "Unknown"}
                  onClick={() => onNavigate("skills")}
                />
                <HealthRow
                  label="Tunnel"
                  status={health?.tunnel_active ? "ok" : "warn"}
                  detail={health?.tunnel_active ? "Active" : "Inactive"}
                />
                <HealthRow
                  label="A2A Protocol"
                  status={a2aAgents.length > 0 ? "ok" : "off"}
                  detail={a2aAgents.length > 0 ? `${a2aAgents.length} agents` : "No agents"}
                  onClick={() => onNavigate("a2a")}
                />
                <HealthRow
                  label="Pipeline"
                  status={pipelineList.length > 0 ? "ok" : "off"}
                  detail={`${pipelineList.length} pipelines`}
                  onClick={() => onNavigate("automations")}
                />
                <HealthRow
                  label="Federation"
                  status={peers.length > 0 ? "ok" : "off"}
                  detail={peers.length > 0 ? `${peers.length} peers` : "No peers"}
                />
              </div>
            </div>

            <div className="panel-card">
              <div className="panel-title-row">
                <div>
                  <div className="panel-eyebrow">Reach</div>
                  <h3 className="panel-title">
                    <Icon name="globe" className="w-4 h-4" /> Channels
                  </h3>
                </div>
                <button className="btn-link" onClick={() => {
                  window.localStorage.setItem("clawdesk._settingsTab", "Channels");
                  onNavigate("settings");
                }}>Configure →</button>
              </div>
              <div className="channel-list-compact">
                {channels.slice(0, 3).map(c => (
                  <div key={c.id} className="channel-row">
                    <div className={`status-dot-sm ${c.status === "active" ? "bg-green-500" : "bg-gray-400"}`} />
                    <span className="channel-name">{c.name}</span>
                    <span className="channel-type">{c.channel_type}</span>
                  </div>
                ))}
                {channels.length === 0 && (
                  <div className="empty-state-action">
                    <p>No channels connected</p>
                    <button className="btn-link" onClick={() => {
                      window.localStorage.setItem("clawdesk._settingsTab", "Channels");
                      onNavigate("settings");
                    }}>Connect a channel →</button>
                  </div>
                )}
              </div>
            </div>
          </div>
        </div>

        <div className="panel-card panel-card--activity">
          <div className="panel-title-row">
            <div>
              <div className="panel-eyebrow">Recent movement</div>
              <h3 className="panel-title">
                <Icon name="clock" className="w-4 h-4" /> Recent Activity
              </h3>
            </div>
            <button className="btn-link" onClick={() => onNavigate("chat")}>View all →</button>
          </div>
          <div className="activity-list">
            {sessions.slice(0, 5).map(s => (
              <button key={s.chat_id || s.agent_id + s.last_activity} className="activity-row activity-row-clickable" onClick={() => {
                if (s.chat_id) onNavigate("chat", { threadId: s.chat_id });
                else onNavigate("chat");
              }}>
                <div className="status-dot-sm" style={{ background: "var(--brand)" }} />
                <div className="activity-text">
                  <strong>{s.title}</strong>
                  <span>{s.agent_id ? `Agent ${s.agent_id}` : "Direct workspace chat"}</span>
                </div>
                <div className="activity-id">{formatRelativeTime(s.last_activity)}</div>
              </button>
            ))}
            {sessions.length === 0 && (
              <div className="empty-state-action">
                <p>No recent activity</p>
                <button className="btn subtle" onClick={() => onNavigate("chat")}>Start your first chat →</button>
              </div>
            )}
          </div>
        </div>
      </div>
    </PageLayout>
  );
}

// ── Subcomponents ─────────────────────────────────────────────

function StatCard({ label, value, icon, color, meta }: { label: string; value: string; icon: string; color: string; meta?: string }) {
  return (
    <div className="stat-card">
      <div className="stat-header">
        <span className="stat-label">{label}</span>
        <div className="stat-icon-wrap" style={{ backgroundColor: `color-mix(in srgb, ${color} 15%, transparent)`, color: color }}>
          <Icon name={icon} />
        </div>
      </div>
      <div className="stat-value">{value}</div>
      {meta ? <div className="stat-meta">{meta}</div> : null}
    </div>
  );
}

function HealthRow({ label, status, detail, onClick }: { label: string; status: "ok" | "warn" | "error" | "off"; detail: string; onClick?: () => void }) {
  const color = status === "ok" ? "var(--green)" : status === "warn" ? "var(--amber)" : status === "error" ? "var(--red)" : "var(--text-tertiary)";
  const Tag = onClick ? "button" : "div";
  return (
    <Tag className={`health-row${onClick ? " health-row-clickable" : ""}`} onClick={onClick}>
      <span className="status-dot-sm" style={{ backgroundColor: color, boxShadow: `0 0 4px ${color}40` }} />
      <span className="health-label">{label}</span>
      <span className={`health-status health-status--${status}`}>{statusLabel(status)}</span>
      <span className="health-detail">{detail}</span>
      {onClick && <span className="health-arrow">→</span>}
    </Tag>
  );
}

function formatRelativeTime(value: string) {
  const time = new Date(value).getTime();
  if (!Number.isFinite(time)) return "Unknown";
  const deltaMinutes = Math.max(0, Math.round((Date.now() - time) / 60000));
  if (deltaMinutes < 1) return "Just now";
  if (deltaMinutes < 60) return `${deltaMinutes}m ago`;
  const deltaHours = Math.round(deltaMinutes / 60);
  if (deltaHours < 24) return `${deltaHours}h ago`;
  const deltaDays = Math.round(deltaHours / 24);
  return `${deltaDays}d ago`;
}

function statusLabel(status: "ok" | "warn" | "error" | "off") {
  if (status === "ok") return "Healthy";
  if (status === "warn") return "Monitor";
  if (status === "error") return "Issue";
  return "Offline";
}
