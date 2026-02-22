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

  return (
    <PageLayout title="Dashboard" subtitle="System Overview">
      <div className="dashboard-grid">
        {/* Stats Row */}
        <div className="stats-row">
          <StatCard label="Active Agents" value={activeAgents.toString()} icon="bot" color="var(--brand)" />
          <StatCard label="Platform Cost" value={`$${cost.toFixed(2)}`} icon="brand" color="var(--cyan)" />
          <StatCard label="Active Plugins" value={activePlugins.toString()} icon="zap" color="var(--purple)" />
          <StatCard label="Pending Approvals" value={pendingApprovals.toString()} icon="shield" color="var(--amber)" />
          <StatCard label="A2A Agents" value={a2aAgents.length.toString()} icon="globe" color="var(--green)" />
          <StatCard label="Durable Runs" value={durableRuns.filter(r => r.state === "running").length.toString()} icon="cpu" color="var(--cyan)" />
        </div>

        {/* Main Content Grid */}
        <div className="dashboard-main-grid">
          {/* Left Column: Agents */}
          <div className="panel-card">
            <h3 className="panel-title">
              <Icon name="bot" className="w-4 h-4" /> Active Agents
            </h3>
            <div className="agent-list-compact">
              {agents.slice(0, 5).map(agent => (
                <div key={agent.id} className="agent-row">
                  <div className="agent-icon-sm" style={{ backgroundColor: agent.color }}>
                    {agent.icon}
                  </div>
                  <div className="agent-info">
                    <div className="agent-name">{agent.name}</div>
                    <div className="agent-model">{agent.model}</div>
                  </div>
                  <div className="agent-status">
                    <span className={`status-dot ${agent.status === "active" ? "status-ok" : ""}`} />
                  </div>
                </div>
              ))}
              {agents.length === 0 && (
                <div className="empty-state">No active agents</div>
              )}
            </div>
          </div>

          {/* Right Column: Health & Channels */}
          <div className="flex flex-col gap-4">
            {/* System Health */}
            <div className="panel-card">
              <h3 className="panel-title">
                <Icon name="activity" className="w-4 h-4" /> System Health
              </h3>
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
                />
                <HealthRow
                  label="Runtime"
                  status={runtimeStatus?.durable_runner_available ? "ok" : "warn"}
                  detail={runtimeStatus?.durable_runner_available
                    ? `${durableRuns.filter(r => r.state === "running").length} running`
                    : "Unavailable"}
                />
                <HealthRow
                  label="Pipeline"
                  status={pipelineList.length > 0 ? "ok" : "off"}
                  detail={`${pipelineList.length} pipelines`}
                />
                <HealthRow
                  label="Federation"
                  status={peers.length > 0 ? "ok" : "off"}
                  detail={peers.length > 0 ? `${peers.length} peers` : "No peers"}
                />
              </div>
            </div>

            {/* Channels */}
            <div className="panel-card">
              <h3 className="panel-title">
                <Icon name="globe" className="w-4 h-4" /> Channels
              </h3>
              <div className="channel-list-compact">
                {channels.slice(0, 3).map(c => (
                  <div key={c.id} className="channel-row">
                    <div className={`status-dot-sm ${c.status === "active" ? "bg-green-500" : "bg-gray-400"}`} />
                    <span className="channel-name">{c.name}</span>
                    <span className="channel-type">{c.channel_type}</span>
                  </div>
                ))}
                {channels.length === 0 && (
                  <div className="empty-state">No channels connected</div>
                )}
              </div>
            </div>
          </div>
        </div>

        {/* Recent Activity */}
        <div className="panel-card mt-4">
          <h3 className="panel-title">
            <Icon name="clock" className="w-4 h-4" /> Recent Activity
          </h3>
          <div className="activity-list">
            {sessions.slice(0, 5).map(s => (
              <div key={s.chat_id || s.agent_id + s.last_activity} className="activity-row">
                <div className="status-dot-sm" style={{ background: "var(--brand)" }} />
                <div className="activity-text">
                  Session with <strong>{s.title}</strong>
                </div>
                <div className="activity-id">{new Date(s.last_activity).toLocaleTimeString()}</div>
              </div>
            ))}
            {sessions.length === 0 && (
              <div className="empty-state">No recent activity</div>
            )}
          </div>
        </div>
      </div>
    </PageLayout>
  );
}

// ── Subcomponents ─────────────────────────────────────────────

function StatCard({ label, value, icon, color }: { label: string; value: string; icon: string; color: string }) {
  return (
    <div className="stat-card">
      <div className="stat-header">
        <span className="stat-label">{label}</span>
        <div className="stat-icon-wrap" style={{ backgroundColor: `color-mix(in srgb, ${color} 15%, transparent)`, color: color }}>
          <Icon name={icon} />
        </div>
      </div>
      <div className="stat-value">{value}</div>
    </div>
  );
}

function HealthRow({ label, status, detail }: { label: string; status: "ok" | "warn" | "error" | "off"; detail: string }) {
  const color = status === "ok" ? "var(--green)" : status === "warn" ? "var(--amber)" : status === "error" ? "var(--red)" : "var(--text-tertiary)";
  return (
    <div className="health-row">
      <span className="status-dot-sm" style={{ backgroundColor: color, boxShadow: `0 0 4px ${color}40` }} />
      <span className="health-label">{label}</span>
      <span className="health-detail">{detail}</span>
    </div>
  );
}
