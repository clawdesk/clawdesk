import { useState, useMemo } from "react";
import type { DesktopAgent } from "../types";
import { AGENT_TEMPLATES } from "../types";
import { AgentDesigner } from "../components/AgentDesigner";
import { TeamBuilderCanvas } from "../components/TeamBuilderCanvas";
import { PageLayout } from "../components/PageLayout";

export interface AgentsPageProps {
  agents: DesktopAgent[];
  onCreateAgent: (template: (typeof AGENT_TEMPLATES)[number]) => void;
  onDeleteAgent: (id: string) => void;
  /** Called when an agent is created or updated — updates the agent list in parent state */
  onAgentSaved: (agent: DesktopAgent) => void;
  pushToast: (text: string) => void;
  onNavigate: (nav: string, options?: { threadId?: string }) => void;
}

type CreateMode = "pick" | "single" | "team";

export function AgentsPage({
  agents,
  onCreateAgent,
  onDeleteAgent,
  onAgentSaved,
  pushToast,
  onNavigate,
}: AgentsPageProps) {
  const [createMode, setCreateMode] = useState<CreateMode | null>(null);
  const [editingAgent, setEditingAgent] = useState<DesktopAgent | null>(null);
  const [collapsedTeams, setCollapsedTeams] = useState<Set<string>>(new Set());

  const toggleTeam = (teamId: string) =>
    setCollapsedTeams((prev) => {
      const next = new Set(prev);
      next.has(teamId) ? next.delete(teamId) : next.add(teamId);
      return next;
    });

  const openCreate = () => setCreateMode("pick");

  const closeCreate = () => {
    setCreateMode(null);
    setEditingAgent(null);
  };

  return (
    <PageLayout
      title="Agents"
      subtitle="Manage your AI agents — each with a hash-locked identity, assigned skills, and a designated model."
      className="page-agents"
      actions={
        <button className="btn primary" onClick={openCreate}>
          + Create Agent
        </button>
      }
    >
      <div className="agents-page-content">
        {(() => {
          const soloAgents = agents.filter((a) => !a.team_id);
          const teamMap = new Map<string, DesktopAgent[]>();
          for (const a of agents) {
            if (a.team_id) {
              const list = teamMap.get(a.team_id) || [];
              list.push(a);
              teamMap.set(a.team_id, list);
            }
          }

          const renderAgentCard = (a: DesktopAgent, compact?: boolean) => (
            <div key={a.id} className={`agent-card-settings ${compact ? "agent-card-compact" : ""}`}>
              <div className="agent-card-icon">{a.icon}</div>
              <div className="agent-card-info">
                <div className="agent-card-name">
                  {a.name}
                  {a.team_role && <span className="chip chip-role">{a.team_role}</span>}
                  <span className={`status-dot ${a.status === "active" ? "status-ok" : "status-warn"}`} />
                  <span className="chip">{a.status}</span>
                </div>
                <div className="agent-card-persona">{a.persona.slice(0, 80)}...</div>
                <div className="agent-card-meta">
                  {a.skills.slice(0, 4).map((s) => (
                    <span key={s} className="chip">{s}</span>
                  ))}
                  <span className="chip">{a.model}</span>
                  {(a.channels ?? []).length > 0 ? (
                    (a.channels ?? []).map((ch) => (
                      <span key={ch} className="chip" style={{ background: "var(--accent-bg)", color: "var(--brand)" }}>{ch}</span>
                    ))
                  ) : (
                    <span className="chip" style={{ opacity: 0.5 }}>all channels</span>
                  )}
                  <span>{a.msg_count} msgs · {(a.tokens_used ?? 0).toLocaleString()}/{(a.token_budget ?? 0).toLocaleString()} tokens</span>
                </div>
              </div>
              <div className="agent-card-verify">
                <div className="verify-status">✓ Identity Verified</div>
                <div className="verify-hash">sha256:{a.persona_hash.slice(0, 8)}...</div>
              </div>
              <div className="agent-card-actions">
                <button className="btn subtle" onClick={() => onNavigate("chat")}>Chat →</button>
                <button className="btn subtle" onClick={() => {
                  setEditingAgent(a);
                  setCreateMode("single");
                }}>Edit</button>
                <button className="btn ghost" onClick={() => onDeleteAgent(a.id)}>Delete</button>
              </div>
            </div>
          );

          return (
            <>
              {/* Solo agents */}
              {soloAgents.length > 0 && (
                <div className="agent-list">
                  {soloAgents.map((a) => renderAgentCard(a))}
                </div>
              )}

              {/* Teams */}
              {[...teamMap.entries()].map(([teamId, teamAgents]) => {
                const router = teamAgents.find((a) => a.team_role === "router") || teamAgents[0];
                const collapsed = collapsedTeams.has(teamId);
                return (
                  <div key={teamId} className={`agent-team-group ${collapsed ? "agent-team-collapsed" : ""}`}>
                    <div className="agent-team-header" onClick={() => toggleTeam(teamId)} style={{ cursor: "pointer" }}>
                      <div className="agent-team-header-left">
                        <span className={`agent-team-chevron ${collapsed ? "" : "agent-team-chevron-open"}`}>▶</span>
                        <span className="agent-team-icon">👥</span>
                        <div>
                          <div className="agent-team-name">Team: {router.name}</div>
                          <div className="agent-team-meta">{teamAgents.length} agents · {teamAgents.filter((a) => a.status === "ready").length} ready</div>
                        </div>
                      </div>
                      <div className="agent-team-header-actions">
                        <button className="btn subtle" onClick={(e) => {
                          e.stopPropagation();
                          onNavigate("chat");
                        }}>Chat with Team →</button>
                      </div>
                    </div>
                    <div className={`agent-team-members ${collapsed ? "agent-team-members-collapsed" : ""}`}>
                      {teamAgents.map((a) => renderAgentCard(a, true))}
                    </div>
                  </div>
                );
              })}

              {agents.length === 0 && (
                <div className="empty-state-action" style={{ padding: 24, textAlign: "center" }}>
                  <p style={{ marginBottom: 12 }}>No agents created yet. Create one to start chatting.</p>
                  <button className="btn primary" onClick={openCreate}>
                    Create your first agent
                  </button>
                </div>
              )}
            </>
          );
        })()}
      </div>

      {/* ── Step 1: Pick single or team ─────────────────────── */}
      {createMode === "pick" && (
        <div className="agent-pick-backdrop" onClick={closeCreate}>
          <div className="agent-pick-modal" onClick={(e) => e.stopPropagation()}>
            <div className="agent-pick-header">
              <h2>Create Agent</h2>
              <p>How would you like to get started?</p>
            </div>

            <div className="agent-pick-options">
              <button
                className="agent-pick-card"
                onClick={() => setCreateMode("single")}
              >
                <div className="agent-pick-card-icon">🤖</div>
                <div className="agent-pick-card-body">
                  <h3>Single Agent</h3>
                  <p>Create one agent with a specific role, persona, skills, and model. Best for focused tasks.</p>
                </div>
                <span className="agent-pick-arrow">→</span>
              </button>

              <button
                className="agent-pick-card"
                onClick={() => setCreateMode("team")}
              >
                <div className="agent-pick-card-icon">👥</div>
                <div className="agent-pick-card-body">
                  <h3>Agent Team</h3>
                  <p>Build a team of agents that work together — with roles, delegation, and shared memory. Start from a template or blank canvas.</p>
                </div>
                <span className="agent-pick-arrow">→</span>
              </button>
            </div>
          </div>
        </div>
      )}

      {/* ── Step 2a: Single agent wizard ────────────────────── */}
      {createMode === "single" && (
        <AgentDesigner
          existingAgent={editingAgent}
          allAgents={agents}
          onClose={closeCreate}
          onSaved={(agent) => {
            onAgentSaved(agent);
            closeCreate();
          }}
          pushToast={pushToast}
        />
      )}

      {/* ── Step 2b: Team canvas ────────────────────────────── */}
      {createMode === "team" && (
        <TeamBuilderCanvas
          allAgents={agents}
          onClose={closeCreate}
          onTeamCreated={() => {
            // Team builder creates agents via api.createAgent directly;
            // refresh the agent list from the backend
            onCreateAgent(AGENT_TEMPLATES[0]);
            closeCreate();
          }}
          pushToast={pushToast}
        />
      )}
    </PageLayout>
  );
}
