import { useState, useEffect, useCallback } from "react";
import * as api from "../api";
import type { SkillDescriptor, SkillTrustInfo } from "../types";
import { Icon } from "../components/Icon";
import { SkillDesigner } from "../components/SkillDesigner";

// ── Props ─────────────────────────────────────────────────────

export interface SkillsPageProps {
  skills: SkillDescriptor[];
  onRefreshSkills: () => void;
  pushToast: (text: string) => void;
}

// ── Component ─────────────────────────────────────────────────

export function SkillsPage({ skills, onRefreshSkills, pushToast }: SkillsPageProps) {
  const [search, setSearch] = useState("");
  const [trustCache, setTrustCache] = useState<Record<string, SkillTrustInfo>>({});
  const [designerOpen, setDesignerOpen] = useState(false);
  const [editingSkill, setEditingSkill] = useState<SkillDescriptor | null>(null);

  const installed = skills.filter((s) => s.state === "active" || s.state === "loaded");
  const recommended = skills.filter((s) => s.verified && s.state !== "active" && s.state !== "loaded");
  const all = skills.filter((s) => !s.verified && s.state !== "active" && s.state !== "loaded");

  const filtered = search.trim()
    ? skills.filter(
        (s) =>
          s.name.toLowerCase().includes(search.toLowerCase()) ||
          s.description.toLowerCase().includes(search.toLowerCase()) ||
          s.category.toLowerCase().includes(search.toLowerCase())
      )
    : null;

  const toggleSkill = useCallback(
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

  const checkTrust = useCallback(
    async (skillId: string) => {
      if (trustCache[skillId]) return;
      try {
        const info = await api.getSkillTrustLevel(skillId);
        setTrustCache((prev) => ({ ...prev, [skillId]: info }));
      } catch {
        // ignore
      }
    },
    [trustCache]
  );

  const testTriggers = useCallback(
    async (skillId: string) => {
      try {
        const triggers = await api.evaluateSkillTriggers(`Test trigger for ${skillId}`);
        const match = triggers.find((t) => t.skill_id === skillId);
        pushToast(
          match
            ? `Trigger matched: relevance ${(match.relevance * 100).toFixed(0)}%`
            : "No trigger match for test input."
        );
      } catch {
        pushToast("Failed to evaluate triggers.");
      }
    },
    [pushToast]
  );

  function SkillRow({ skill }: { skill: SkillDescriptor }) {
    const isInstalled = skill.state === "active" || skill.state === "loaded";
    const trust = trustCache[skill.id];

    return (
      <div
        className="skill-row"
        onMouseEnter={() => checkTrust(skill.id)}
      >
        <div className="skill-row-icon">{skill.icon || "⚡"}</div>
        <div className="skill-row-info">
          <div className="skill-row-name">
            {skill.name}
            {skill.verified && <span className="chip" title="Verified">✓</span>}
            {trust && (
              <span className={`chip ${trust.verified ? "" : "chip-risk"}`} title={`Trust: ${trust.trust_level}`}>
                {trust.trust_level}
              </span>
            )}
          </div>
          <div className="skill-row-desc">{skill.description}</div>
          <div className="skill-row-meta">
            <span className="chip">{skill.category}</span>
            <span>~{skill.estimated_tokens} tokens</span>
          </div>
        </div>
        <div className="skill-row-actions">
          {isInstalled ? (
            <>
              <button className="btn subtle" onClick={() => testTriggers(skill.id)}>Test</button>
              <button className="btn subtle" onClick={() => { setEditingSkill(skill); setDesignerOpen(true); }}>Edit</button>
              <button className="btn ghost" onClick={() => toggleSkill(skill)}>Uninstall</button>
            </>
          ) : (
            <button className="btn subtle" onClick={() => toggleSkill(skill)}>
              <Icon name="safe-on" /> Install
            </button>
          )}
        </div>
      </div>
    );
  }

  return (
    <div className="view page-skills">
      <div className="page-header">
        <div>
          <h1 className="page-title">Skills</h1>
          <p className="page-subtitle">Give ClawDesk superpowers. Install skills to expand agent capabilities.</p>
        </div>
        <div className="page-header-actions">
          <button className="btn primary" onClick={() => { setEditingSkill(null); setDesignerOpen(true); }}>
            ✏️ Design Skill
          </button>
          <button className="btn subtle" onClick={onRefreshSkills}>
            <Icon name="now" /> Refresh
          </button>
          <input
            className="input page-search"
            placeholder="Search skills..."
            value={search}
            onChange={(e) => setSearch(e.target.value)}
          />
        </div>
      </div>

      {filtered ? (
        <section className="section-card">
          <div className="section-head">
            <h2>Results ({filtered.length})</h2>
          </div>
          <div className="skills-grid">
            {filtered.map((s) => <SkillRow key={s.id} skill={s} />)}
          </div>
        </section>
      ) : (
        <>
          {installed.length > 0 && (
            <section className="section-card">
              <div className="section-head">
                <h2>Installed ({installed.length})</h2>
              </div>
              <div className="skills-grid">
                {installed.map((s) => <SkillRow key={s.id} skill={s} />)}
              </div>
            </section>
          )}

          {recommended.length > 0 && (
            <section className="section-card">
              <div className="section-head">
                <h2>Recommended</h2>
              </div>
              <div className="skills-grid">
                {recommended.map((s) => <SkillRow key={s.id} skill={s} />)}
              </div>
            </section>
          )}

          {all.length > 0 && (
            <section className="section-card">
              <div className="section-head">
                <h2>All Skills</h2>
              </div>
              <div className="skills-grid">
                {all.map((s) => <SkillRow key={s.id} skill={s} />)}
              </div>
            </section>
          )}

          {skills.length === 0 && (
            <div className="empty-state centered">
              <p>No skills loaded yet.</p>
              <button className="btn primary" onClick={onRefreshSkills}>Refresh</button>
            </div>
          )}
        </>
      )}

      {designerOpen && (
        <SkillDesigner
          existingSkill={editingSkill}
          onClose={() => setDesignerOpen(false)}
          onSaved={() => { setDesignerOpen(false); onRefreshSkills(); }}
          pushToast={pushToast}
        />
      )}
    </div>
  );
}
