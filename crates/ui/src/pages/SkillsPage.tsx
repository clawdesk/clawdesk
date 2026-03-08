import { useState, useEffect, useCallback, useRef } from "react";
import ReactDOM from "react-dom";
import * as api from "../api";
import type { SkillDescriptor, SkillTrustInfo, PeerInfo, AgentCardInfo } from "../types";
import { Icon } from "../components/Icon";
import { SkillDesigner } from "../components/SkillDesigner";
import { PageLayout } from "../components/PageLayout";

// ── Types ─────────────────────────────────────────────────────

type SkillsTab = "local" | "federation";

// ── Props ─────────────────────────────────────────────────────

export interface SkillsPageProps {
  skills: SkillDescriptor[];
  onRefreshSkills: () => void;
  pushToast: (text: string) => void;
  onNavigate: (nav: string, options?: { threadId?: string }) => void;
}

// ── Helper ────────────────────────────────────────────────────

function resolveSkillIcon(icon: string): string {
  const value = (icon || "").trim();
  if (!value) return "⚡";
  const emojiLike = /[\u{1F300}-\u{1FAFF}\u{2600}-\u{27BF}]/u;
  if (emojiLike.test(value)) return value;
  switch (value.toLowerCase()) {
    case "code": return "⚡";
    case "send": return "📤";
    case "zap": return "⚡";
    case "file": return "📄";
    case "globe": return "🌐";
    case "search": return "🔍";
    case "tool": case "tools": return "🛠️";
    default: return value.length <= 2 ? value : "⚡";
  }
}

// ── SkillCard (top-level, NOT nested) ─────────────────────────

interface SkillCardProps {
  skill: SkillDescriptor;
  trust: SkillTrustInfo | undefined;
  onEdit: (skill: SkillDescriptor) => void;
  onInstall: (skill: SkillDescriptor) => void;
  onHover: (skillId: string) => void;
  onTryInChat?: () => void;
}

function SkillCard({ skill, trust, onEdit, onInstall, onHover, onTryInChat }: SkillCardProps) {
  const isInstalled = skill.state === "active" || skill.state === "loaded";

  return (
    <div
      className={`skill-card${isInstalled ? " skill-card-installed" : ""}`}
      onMouseEnter={() => onHover(skill.id)}
    >
      <div className="skill-card-top">
        <div className="skill-card-icon">{resolveSkillIcon(skill.icon)}</div>
        <div className="skill-card-actions">
          {isInstalled ? (
            <>
              {onTryInChat && (
                <button
                  className="btn subtle skill-card-edit-btn"
                  onClick={onTryInChat}
                  title="Try this skill in chat"
                >
                  💬 Try
                </button>
              )}
              <button
                className="btn subtle skill-card-edit-btn"
                onClick={() => onEdit(skill)}
                title="Edit skill"
              >
                ✏️ Edit
              </button>
            </>
          ) : (
            <button
              className="btn icon-only subtle"
              onClick={() => onInstall(skill)}
              title="Install"
            >
              <Icon name="plus" />
            </button>
          )}
        </div>
      </div>
      <div className="skill-card-content">
        <div className="skill-card-title">
          {skill.name}
          {skill.verified && <span className="verified-badge" title="Verified">✓</span>}
        </div>
        <div className="skill-card-desc">{skill.description}</div>
      </div>
      <div className="skill-card-footer">
        <div className="skill-card-meta">
          {isInstalled
            ? <span className="status-dot status-ok" title="Installed" />
            : <span className="status-dot" title="Available" />
          }
          {skill.category}
        </div>
        {trust && (
          <span className={`trust-badge ${trust.verified ? "" : "warn"}`}>
            {trust.trust_level}
          </span>
        )}
      </div>
    </div>
  );
}

// ── Page ─────────────────────────────────────────────────────

export function SkillsPage({ skills, onRefreshSkills, pushToast, onNavigate }: SkillsPageProps) {
  const [search, setSearch] = useState("");
  const [trustCache, setTrustCache] = useState<Record<string, SkillTrustInfo>>({});
  const [designerOpen, setDesignerOpen] = useState(false);
  const [editingSkill, setEditingSkill] = useState<SkillDescriptor | null>(null);
  const requestedTrust = useRef(new Set<string>());
  const [activeTab, setActiveTab] = useState<SkillsTab>("local");

  // Federation state
  const [fedPeers, setFedPeers] = useState<PeerInfo[]>([]);
  const [fedAgents, setFedAgents] = useState<AgentCardInfo[]>([]);
  const [fedLoading, setFedLoading] = useState(false);

  useEffect(() => {
    if (activeTab === "federation" && fedPeers.length === 0 && !fedLoading) {
      setFedLoading(true);
      Promise.all([
        api.listDiscoveredPeers().catch(() => [] as PeerInfo[]),
        api.listA2aAgents().catch(() => [] as AgentCardInfo[]),
      ]).then(([peers, agents]) => {
        setFedPeers(peers);
        setFedAgents(agents);
      }).finally(() => setFedLoading(false));
    }
  }, [activeTab, fedPeers.length, fedLoading]);

  const installed = skills.filter((s) => s.state === "active" || s.state === "loaded");
  const recommended = skills.filter((s) => s.verified && s.state !== "active" && s.state !== "loaded");
  const all = skills.filter((s) => !s.verified && s.state !== "active" && s.state !== "loaded");
  const verifiedCount = skills.filter((s) => s.verified).length;
  const categoryCount = new Set(skills.map((s) => s.category)).size;
  const federatedCapabilityCount = new Set(fedAgents.flatMap((agent) => agent.capabilities)).size;

  const filtered = search.trim()
    ? skills.filter(
      (s) =>
        s.name.toLowerCase().includes(search.toLowerCase()) ||
        s.description.toLowerCase().includes(search.toLowerCase()) ||
        s.category.toLowerCase().includes(search.toLowerCase())
    )
    : null;

  const handleInstall = useCallback(
    async (skill: SkillDescriptor) => {
      try {
        if (skill.state === "active" || skill.state === "loaded") {
          await api.deactivateSkill(skill.id);
          pushToast(`Skill "${skill.name}" deactivated.`);
        } else {
          await api.activateSkill(skill.id);
          pushToast(`Skill "${skill.name}" activated.`);
        }
        onRefreshSkills();
      } catch {
        pushToast(`Failed to toggle "${skill.name}".`);
      }
    },
    [onRefreshSkills, pushToast]
  );

  const handleEdit = useCallback((skill: SkillDescriptor) => {
    setEditingSkill(skill);
    setDesignerOpen(true);
  }, []);

  const handleHover = useCallback(
    async (skillId: string) => {
      if (requestedTrust.current.has(skillId)) return;
      requestedTrust.current.add(skillId);
      try {
        const info = await api.getSkillTrustLevel(skillId);
        setTrustCache((prev) => ({ ...prev, [skillId]: info }));
      } catch {
        // ignore
      }
    },
    []
  );

  const openNew = () => {
    setEditingSkill(null);
    setDesignerOpen(true);
  };

  const renderCard = (s: SkillDescriptor) => (
    <SkillCard
      key={s.id}
      skill={s}
      trust={trustCache[s.id]}
      onEdit={handleEdit}
      onInstall={handleInstall}
      onHover={handleHover}
      onTryInChat={(s.state === "active" || s.state === "loaded") ? () => onNavigate("chat") : undefined}
    />
  );

  return (
    <>
      <PageLayout
        title="Skills"
        subtitle="Give ClawDesk superpowers. Install skills to expand capabilities."
        actions={
          <>
            <input
              className="input page-search"
              placeholder="Search skills..."
              value={search}
              onChange={(e) => setSearch(e.target.value)}
            />
            <button className="btn subtle" style={{ whiteSpace: "nowrap" }} onClick={openNew}>
              <Icon name="plus" /> New Skill
            </button>
          </>
        }
        className="page-skills"
      >
        <div className="skills-page-shell">
          <section className="skills-hero">
            <div className="skills-hero__intro">
              <span className="skills-hero__eyebrow">Capability library</span>
              <h2>Install, audit, and route the skills that give your agents leverage.</h2>
              <p>{installed.length} installed, {verifiedCount} verified, and {categoryCount} categories available across your local workspace.</p>
            </div>
            <div className="skills-hero__stats">
              <HeroPill label="Installed" value={installed.length.toString()} meta="Active in the current workspace" />
              <HeroPill label="Verified" value={verifiedCount.toString()} meta="Trusted or signed capabilities" />
              <HeroPill label="Categories" value={categoryCount.toString()} meta="Coverage across tool domains" />
            </div>
          </section>

          <div className="skills-tabs" role="tablist" aria-label="Skills sources">
            <button className={`skills-tab${activeTab === "local" ? " active" : ""}`} onClick={() => setActiveTab("local")} role="tab" aria-selected={activeTab === "local"}>
              <span>Local</span>
              <strong>{skills.length}</strong>
            </button>
            <button className={`skills-tab${activeTab === "federation" ? " active" : ""}`} onClick={() => setActiveTab("federation")} role="tab" aria-selected={activeTab === "federation"}>
              <span>Federation</span>
              <strong>{fedPeers.length + fedAgents.length}</strong>
            </button>
          </div>

        {activeTab === "local" && (
        <div className="skills-container skills-container--modern">
          {filtered ? (
            <section className="skills-section">
              <div className="skills-section-head">
                <div>
                  <span className="skills-section-kicker">Search</span>
                  <h2 className="skills-section-title">Results ({filtered.length})</h2>
                </div>
              </div>
              <div className="skills-grid">
                {filtered.map(renderCard)}
              </div>
            </section>
          ) : (
            <>
              {installed.length > 0 && (
                <section className="skills-section">
                  <div className="skills-section-head">
                    <div>
                      <span className="skills-section-kicker">Installed</span>
                      <h2 className="skills-section-title">Local skills in rotation</h2>
                    </div>
                    <div className="skills-section-badge">{installed.length} active</div>
                  </div>
                  <div className="skills-grid">
                    {installed.map(renderCard)}
                  </div>
                </section>
              )}

              <section className="skills-section">
                <div className="skills-section-head">
                  <div>
                    <span className="skills-section-kicker">Explore</span>
                    <h2 className="skills-section-title">Recommended and available skills</h2>
                  </div>
                  <div className="skills-section-badge">{recommended.length + all.length} discoverable</div>
                </div>
                <div className="skills-grid">
                  {recommended.length > 0 ? (
                    recommended.map(renderCard)
                  ) : (
                    <div className="empty-message">No recommendations available.</div>
                  )}
                  {all.map(renderCard)}
                </div>
              </section>

              {skills.length === 0 && (
                <div className="empty-state centered">
                  <p>No skills loaded yet.</p>
                  <button className="btn primary" onClick={onRefreshSkills}>Refresh</button>
                </div>
              )}
            </>
          )}
        </div>
        )}

        {/* ── Federation Tab ───────────────────────────────── */}
        {activeTab === "federation" && (
          <div className="skills-container skills-container--modern">
            <section className="skills-federation-hero">
              <div>
                <span className="skills-section-kicker">Federation</span>
                <h2 className="skills-section-title">Remote peers, agent cards, and shared capability graph</h2>
                <p className="skills-federation-hero__copy">{fedPeers.length} peers discovered, {fedAgents.length} remote agents registered, and {federatedCapabilityCount} federated capabilities visible.</p>
              </div>
            </section>

            {fedLoading && (
              <div className="empty-state centered" style={{ padding: 40 }}>
                <p>Discovering peers and federated skills…</p>
              </div>
            )}

            {!fedLoading && (
              <>
                {/* Discovered Peers */}
                <section className="skills-section">
                  <div className="skills-section-head">
                    <div>
                      <span className="skills-section-kicker">Network</span>
                      <h2 className="skills-section-title">Discovered peers ({fedPeers.length})</h2>
                    </div>
                  </div>
                  {fedPeers.length === 0 ? (
                    <div className="empty-message">No peers discovered on local network.</div>
                  ) : (
                    <div className="skills-federation-grid">
                      {fedPeers.map((peer, i) => (
                        <div key={i} className="skills-federation-card">
                          <div className="skills-federation-card__head">
                            <Icon name="globe" className="w-4 h-4" />
                            <strong>{peer.instance_name}</strong>
                          </div>
                          <div className="skills-federation-card__meta">
                            {peer.host}:{peer.port} · v{peer.version}
                          </div>
                          {peer.capabilities.length > 0 && (
                            <div className="skills-federation-card__chips">
                              {peer.capabilities.map((c, ci) => (
                                <span key={ci} className="trust-badge" style={{ fontSize: 10 }}>{formatSkillLabel(c)}</span>
                              ))}
                            </div>
                          )}
                        </div>
                      ))}
                    </div>
                  )}
                </section>

                {/* Federated Agents & Skills */}
                <section className="skills-section">
                  <div className="skills-section-head">
                    <div>
                      <span className="skills-section-kicker">Agents</span>
                      <h2 className="skills-section-title">Federated agents ({fedAgents.length})</h2>
                    </div>
                  </div>
                  {fedAgents.length === 0 ? (
                    <div className="empty-message">No remote agents registered. Register agents in A2A Directory.</div>
                  ) : (
                    <div className="skills-federation-grid">
                      {fedAgents.map((agent) => (
                        <div key={agent.id} className="skills-federation-card">
                          <div className="skills-federation-card__head">
                            <span
                              className="status-dot-sm"
                              style={{ backgroundColor: agent.is_healthy ? "var(--green)" : "var(--red)" }}
                            />
                            <strong>{agent.name}</strong>
                          </div>
                          <div className="skills-federation-card__meta">
                            ID: {agent.id} · {agent.active_tasks} active tasks
                          </div>
                          {agent.capabilities.length > 0 && (
                            <div className="skills-federation-card__chips">
                              {agent.capabilities.map((c, ci) => (
                                <span key={ci} className="trust-badge" style={{ fontSize: 10 }}>{formatSkillLabel(c)}</span>
                              ))}
                            </div>
                          )}
                        </div>
                      ))}
                    </div>
                  )}
                </section>

                {/* Skills across peers */}
                <section className="skills-section">
                  <div className="skills-section-head">
                    <div>
                      <span className="skills-section-kicker">Map</span>
                      <h2 className="skills-section-title">Skill dependency graph</h2>
                    </div>
                  </div>
                  <div className="skills-graph-card">
                    <div className="skills-graph-list">
                      {skills.filter((s) => s.state === "active" || s.state === "loaded").map((skill) => (
                        <div key={skill.id} className="skills-graph-item">
                          <span className="skills-graph-item__icon">{resolveSkillIcon(skill.icon)}</span>
                          <div>
                            <div className="skills-graph-item__title">{skill.name}</div>
                            <div className="skills-graph-item__meta">{skill.category}</div>
                          </div>
                          {skill.verified && <span className="verified-badge" title="Verified">✓</span>}
                        </div>
                      ))}
                      {skills.filter((s) => s.state === "active" || s.state === "loaded").length === 0 && (
                        <div className="empty-message">No active skills to graph.</div>
                      )}
                    </div>
                  </div>
                </section>
              </>
            )}
          </div>
        )}
        </div>
      </PageLayout>

      {designerOpen &&
        ReactDOM.createPortal(
          <SkillDesigner
            existingSkill={editingSkill}
            onClose={() => setDesignerOpen(false)}
            onSaved={() => { setDesignerOpen(false); onRefreshSkills(); }}
            pushToast={pushToast}
          />,
          document.body
        )
      }
    </>
  );
}

function HeroPill({ label, value, meta }: { label: string; value: string; meta: string }) {
  return (
    <div className="skills-hero-pill">
      <span>{label}</span>
      <strong>{value}</strong>
      <small>{meta}</small>
    </div>
  );
}

function formatSkillLabel(value: string) {
  return value.replace(/_/g, " ").replace(/\b\w/g, (char) => char.toUpperCase());
}
