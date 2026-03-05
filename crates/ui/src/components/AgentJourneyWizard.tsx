import { useState, useCallback, useRef, useEffect, useMemo } from "react";
import * as api from "../api";
import type { DesktopAgent, CreateAgentRequest, ProviderCapabilityInfo } from "../types";
import { Icon } from "./Icon";
import { AgentTeamCanvas, type TeamNode, type TeamEdge } from "./AgentTeamCanvas";
import { loadProviders } from "../providerConfig";
import { PROVIDER_MODELS } from "../onboarding/OnboardingWizard";

// ── Types ─────────────────────────────────────────────────────

export interface AgentDraft {
  name: string;
  icon: string;
  color: string;
  persona: string;
  skills: string[];
  model: string;
  channels: string[];
  subagents: SubAgentRef[];
  tokenBudget: number;
}

export interface SubAgentRef {
  id: string;
  name: string;
  role: string;
}

/** Git-like version snapshot */
export interface AgentVersion {
  id: string;
  timestamp: number;
  commitMsg: string;
  snapshot: AgentDraft;
  parentId: string | null;
}

// ── Journey Step Type ─────────────────────────────────────────

type JourneyStep = "identity" | "persona" | "capabilities" | "team" | "review";

const STEPS: { key: JourneyStep; label: string; icon: string; desc: string }[] = [
  { key: "identity", label: "Identity", icon: "🎨", desc: "Name, icon & brand" },
  { key: "persona", label: "Persona", icon: "🧠", desc: "Role & behavior" },
  { key: "capabilities", label: "Capabilities", icon: "⚡", desc: "Skills & model" },
  { key: "team", label: "Team", icon: "🔗", desc: "Sub-agents & topology" },
  { key: "review", label: "Review", icon: "✅", desc: "Verify & deploy" },
];

// ── Constants ─────────────────────────────────────────────────

const EMPTY_DRAFT: AgentDraft = {
  name: "",
  icon: "🤖",
  color: "#6366f1",
  persona: "",
  skills: [],
  model: "",
  channels: [],
  subagents: [],
  tokenBudget: 128_000,
};

const ICON_OPTIONS = [
  "🤖", "🧠", "💡", "🔍", "📊", "✍️", "📅", "🛡️",
  "🚀", "🎯", "📈", "🔧", "💬", "📝", "🌐", "⚡",
  "🧪", "📦", "🎭", "🔮", "🦾", "🏗️", "🗂️", "💎",
];

const SKILL_OPTIONS = [
  { id: "web-search", label: "Web Search", icon: "🔍", desc: "Search the internet" },
  { id: "citations", label: "Citations", icon: "📎", desc: "Source referencing" },
  { id: "markdown", label: "Markdown", icon: "📝", desc: "Rich text formatting" },
  { id: "files", label: "File Access", icon: "📁", desc: "Read/write files" },
  { id: "email", label: "Email", icon: "📧", desc: "Send/receive emails" },
  { id: "calendar", label: "Calendar", icon: "📅", desc: "Schedule events" },
  { id: "cron", label: "Scheduled Tasks", icon: "⏰", desc: "Automated routines" },
  { id: "alerts", label: "Alerts", icon: "🔔", desc: "Notifications" },
  { id: "code-exec", label: "Code Execution", icon: "💻", desc: "Run code snippets" },
  { id: "image-gen", label: "Image Generation", icon: "🎨", desc: "Create images" },
];

/** Build a grouped model list from configured providers + auto-detected environment providers */
interface ModelGroup {
  provider: string;
  models: { id: string; label: string }[];
}

function buildModelGroups(
  providerCaps: ProviderCapabilityInfo[],
): ModelGroup[] {
  const groups: ModelGroup[] = [];
  const seen = new Set<string>();

  // 1. User-configured providers (from localStorage) — match with PROVIDER_MODELS for labels
  const configs = loadProviders();
  for (const cfg of configs) {
    const label = cfg.label || cfg.provider;
    const staticModels = PROVIDER_MODELS[cfg.provider] || [];
    const models: { id: string; label: string }[] = [];
    // Always include the configured model first
    if (cfg.model && !seen.has(cfg.model)) {
      seen.add(cfg.model);
      const known = staticModels.find((m) => m.id === cfg.model);
      models.push({ id: cfg.model, label: known?.label || cfg.model });
    }
    // Add remaining known models for this provider type
    for (const m of staticModels) {
      if (!seen.has(m.id)) {
        seen.add(m.id);
        models.push(m);
      }
    }
    if (models.length > 0) groups.push({ provider: label, models });
  }

  // 2. Auto-detected environment providers (from backend capabilities)
  for (const cap of providerCaps) {
    const models: { id: string; label: string }[] = [];
    for (const mid of cap.models) {
      if (!seen.has(mid)) {
        seen.add(mid);
        models.push({ id: mid, label: mid });
      }
    }
    if (models.length > 0) groups.push({ provider: `${cap.provider} (detected)`, models });
  }

  return groups;
}

const CHANNEL_OPTIONS = [
  { id: "telegram", label: "Telegram", icon: "📱" },
  { id: "discord", label: "Discord", icon: "🎮" },
  { id: "slack", label: "Slack", icon: "💼" },
  { id: "web", label: "Web Chat", icon: "🌐" },
  { id: "email", label: "Email", icon: "📧" },
];

const ROLE_SUGGESTIONS = [
  "Strategy Lead", "Business Analyst", "Marketing",
  "Developer", "Researcher", "QA / Testing",
  "Design", "DevOps",
];

// ── Props ─────────────────────────────────────────────────────

export interface AgentJourneyWizardProps {
  existingAgent?: DesktopAgent | null;
  allAgents: DesktopAgent[];
  onClose: () => void;
  onSaved: (agent: DesktopAgent) => void;
  pushToast: (text: string) => void;
  /** Open directly to a specific step */
  initialStep?: JourneyStep;
}

// ── Validation ────────────────────────────────────────────────

interface ValidationIssue {
  field: string;
  message: string;
  severity: "error" | "warning";
  step: JourneyStep;
}

function validateDraft(draft: AgentDraft): ValidationIssue[] {
  const issues: ValidationIssue[] = [];
  if (!draft.name.trim()) {
    issues.push({ field: "name", message: "Agent name is required", severity: "error", step: "identity" });
  }
  if (!draft.persona.trim()) {
    issues.push({ field: "persona", message: "Persona / system prompt is required", severity: "error", step: "persona" });
  }
  if (draft.persona.length > 0 && draft.persona.length < 20) {
    issues.push({ field: "persona", message: "Persona is very short — add more detail", severity: "warning", step: "persona" });
  }
  if (draft.skills.length === 0) {
    issues.push({ field: "skills", message: "No skills selected", severity: "warning", step: "capabilities" });
  }
  if (draft.tokenBudget < 1000) {
    issues.push({ field: "tokenBudget", message: "Token budget seems very low", severity: "warning", step: "capabilities" });
  }
  return issues;
}

function draftFromAgent(agent: DesktopAgent): AgentDraft {
  return {
    name: agent.name,
    icon: agent.icon,
    color: agent.color,
    persona: agent.persona,
    skills: [...agent.skills],
    model: agent.model,
    channels: [...(agent.channels ?? [])],
    subagents: [],
    tokenBudget: agent.token_budget,
  };
}

function draftToToml(draft: AgentDraft): string {
  const lines: string[] = [];
  lines.push("[agent]");
  lines.push(`id = "${draft.name.toLowerCase().replace(/[^a-z0-9]+/g, "-")}"`);
  lines.push(`display_name = "${draft.name}"`);
  lines.push(`model = "${draft.model || "default"}"`);
  lines.push(`token_budget = ${draft.tokenBudget}`);
  lines.push("");
  lines.push("[agent.persona]");
  lines.push(`role = "${draft.name}"`);
  lines.push(`tone = "professional"`);
  const goalLines = draft.persona.split("\n");
  if (goalLines.length > 1) {
    lines.push(`goal = """`);
    for (const l of goalLines) lines.push(l);
    lines.push(`"""`);
  } else {
    lines.push(`goal = "${draft.persona.replace(/"/g, '\\"')}"`);
  }
  if (draft.skills.length > 0) {
    lines.push("");
    lines.push("[agent.skills]");
    lines.push(`allowed = [${draft.skills.map((s) => `"${s}"`).join(", ")}]`);
  }
  if (draft.channels.length > 0) {
    lines.push("");
    lines.push("[agent.bindings]");
    for (const ch of draft.channels) lines.push(`"${ch}:*:*" = true`);
  }
  if (draft.subagents.length > 0) {
    lines.push("");
    lines.push("[agent.subagents]");
    lines.push(`can_spawn = [${draft.subagents.map((s) => `"${s.id}"`).join(", ")}]`);
    lines.push("shared_memory = true");
  }
  return lines.join("\n");
}

// ── Git-like Version History ──────────────────────────────────

function createVersion(
  draft: AgentDraft,
  msg: string,
  parentId: string | null,
): AgentVersion {
  return {
    id: `v_${Date.now()}_${Math.random().toString(36).slice(2, 6)}`,
    timestamp: Date.now(),
    commitMsg: msg,
    snapshot: { ...draft },
    parentId,
  };
}

function diffDrafts(a: AgentDraft, b: AgentDraft): string[] {
  const changes: string[] = [];
  if (a.name !== b.name) changes.push(`name: "${a.name}" → "${b.name}"`);
  if (a.icon !== b.icon) changes.push(`icon: ${a.icon} → ${b.icon}`);
  if (a.color !== b.color) changes.push(`color: ${a.color} → ${b.color}`);
  if (a.model !== b.model) changes.push(`model: ${a.model || "default"} → ${b.model || "default"}`);
  if (a.persona !== b.persona) {
    const aLen = a.persona.length;
    const bLen = b.persona.length;
    changes.push(`persona: ${aLen} → ${bLen} chars`);
  }
  if (JSON.stringify(a.skills) !== JSON.stringify(b.skills)) {
    const added = b.skills.filter((s) => !a.skills.includes(s));
    const removed = a.skills.filter((s) => !b.skills.includes(s));
    if (added.length) changes.push(`skills +${added.join(", ")}`);
    if (removed.length) changes.push(`skills -${removed.join(", ")}`);
  }
  if (JSON.stringify(a.subagents) !== JSON.stringify(b.subagents)) {
    changes.push(`team: ${a.subagents.length} → ${b.subagents.length} members`);
  }
  if (a.tokenBudget !== b.tokenBudget) {
    changes.push(`budget: ${a.tokenBudget.toLocaleString()} → ${b.tokenBudget.toLocaleString()}`);
  }
  return changes;
}

// ── Component ─────────────────────────────────────────────────

export function AgentJourneyWizard({
  existingAgent,
  allAgents,
  onClose,
  onSaved,
  pushToast,
  initialStep = "identity",
}: AgentJourneyWizardProps) {
  const isEditing = !!existingAgent;

  // ── State ─────────────────────────────────────────────
  const [step, setStep] = useState<JourneyStep>(initialStep);
  const [draft, setDraft] = useState<AgentDraft>(
    existingAgent ? draftFromAgent(existingAgent) : { ...EMPTY_DRAFT },
  );
  const [isSaving, setIsSaving] = useState(false);
  const [showIconPicker, setShowIconPicker] = useState(false);
  const [showAddTeamPicker, setShowAddTeamPicker] = useState(false);
  const [customSkill, setCustomSkill] = useState("");

  // ── Dynamic model list from providers ─────────────────
  const [providerCaps, setProviderCaps] = useState<ProviderCapabilityInfo[]>([]);
  useEffect(() => {
    api.listProviderCapabilities().then(setProviderCaps).catch(() => {});
  }, []);
  const modelGroups = useMemo(() => buildModelGroups(providerCaps), [providerCaps]);

  // Git-like versioning
  const [versions, setVersions] = useState<AgentVersion[]>(() => {
    const initial = existingAgent ? draftFromAgent(existingAgent) : { ...EMPTY_DRAFT };
    return [createVersion(initial, isEditing ? "Loaded from agent" : "Initial draft", null)];
  });
  const [showVersionHistory, setShowVersionHistory] = useState(false);

  const validationIssues = useMemo(() => validateDraft(draft), [draft]);
  const stepIssues = useMemo(
    () => validationIssues.filter((i) => i.step === step),
    [validationIssues, step],
  );
  const hasErrors = validationIssues.some((i) => i.severity === "error");

  const currentStepIdx = STEPS.findIndex((s) => s.key === step);

  // ── Draft updates ─────────────────────────────────────

  const updateDraft = useCallback((updates: Partial<AgentDraft>) => {
    setDraft((prev) => ({ ...prev, ...updates }));
  }, []);

  const toggleSkill = useCallback((skillId: string) => {
    setDraft((prev) => ({
      ...prev,
      skills: prev.skills.includes(skillId)
        ? prev.skills.filter((s) => s !== skillId)
        : [...prev.skills, skillId],
    }));
  }, []);

  const toggleChannel = useCallback((channelId: string) => {
    setDraft((prev) => ({
      ...prev,
      channels: prev.channels.includes(channelId)
        ? prev.channels.filter((c) => c !== channelId)
        : [...prev.channels, channelId],
    }));
  }, []);

  // ── Team (sub-agent) management ───────────────────────

  const addSubAgent = useCallback((agent: DesktopAgent) => {
    setDraft((prev) => {
      if (prev.subagents.some((s) => s.id === agent.id)) return prev;
      return {
        ...prev,
        subagents: [...prev.subagents, { id: agent.id, name: agent.name, role: "" }],
      };
    });
    setShowAddTeamPicker(false);
  }, []);

  const removeSubAgent = useCallback((agentId: string) => {
    setDraft((prev) => ({
      ...prev,
      subagents: prev.subagents.filter((s) => s.id !== agentId),
    }));
  }, []);

  const availableForTeam = useMemo(
    () => allAgents.filter(
      (a) => a.id !== existingAgent?.id && !draft.subagents.some((s) => s.id === a.id),
    ),
    [allAgents, existingAgent, draft.subagents],
  );

  // ── Version control ───────────────────────────────────

  const commitVersion = useCallback((msg: string) => {
    setVersions((prev) => {
      const parentId = prev.length > 0 ? prev[prev.length - 1].id : null;
      return [...prev, createVersion(draft, msg, parentId)];
    });
  }, [draft]);

  const revertToVersion = useCallback((version: AgentVersion) => {
    setDraft({ ...version.snapshot });
    pushToast(`Reverted to: ${version.commitMsg}`);
  }, [pushToast]);

  // ── Navigation ────────────────────────────────────────

  const goNext = useCallback(() => {
    // Auto-commit on step transitions
    const lastVersion = versions[versions.length - 1];
    const changes = diffDrafts(lastVersion.snapshot, draft);
    if (changes.length > 0) {
      commitVersion(`Completed ${STEPS[currentStepIdx].label} step`);
    }

    if (currentStepIdx < STEPS.length - 1) {
      setStep(STEPS[currentStepIdx + 1].key);
    }
  }, [currentStepIdx, versions, draft, commitVersion]);

  const goPrev = useCallback(() => {
    if (currentStepIdx > 0) {
      setStep(STEPS[currentStepIdx - 1].key);
    }
  }, [currentStepIdx]);

  // ── Save ──────────────────────────────────────────────

  const handleSave = useCallback(async () => {
    const issues = validateDraft(draft);
    if (issues.some((i) => i.severity === "error")) {
      pushToast("Fix validation errors before saving.");
      return;
    }

    setIsSaving(true);
    try {
      if (isEditing && existingAgent) {
        const updated = await api.updateAgent(existingAgent.id, {
          name: draft.name,
          icon: draft.icon,
          color: draft.color,
          persona: draft.persona,
          skills: draft.skills,
          model: draft.model,
          channels: draft.channels,
        });
        commitVersion("Deployed update");
        onSaved(updated);
        pushToast(`Agent "${draft.name}" updated.`);
      } else {
        const req: CreateAgentRequest = {
          name: draft.name,
          icon: draft.icon,
          color: draft.color,
          persona: draft.persona,
          skills: draft.skills,
          model: draft.model || "default",
          channels: draft.channels,
        };
        const agent = await api.createAgent(req);
        commitVersion("Created & deployed");
        onSaved(agent);
        pushToast(`Agent "${draft.name}" created & registered.`);
      }
      onClose();
    } catch (err: any) {
      pushToast(`Failed: ${err?.message || err}`);
    } finally {
      setIsSaving(false);
    }
  }, [draft, isEditing, existingAgent, onClose, onSaved, pushToast, commitVersion]);

  // ── Export TOML ───────────────────────────────────────

  const handleExportToml = useCallback(() => {
    const toml = draftToToml(draft);
    const blob = new Blob([toml], { type: "text/plain" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = `${draft.name.toLowerCase().replace(/[^a-z0-9]+/g, "-") || "agent"}.toml`;
    a.click();
    URL.revokeObjectURL(url);
    pushToast("Exported agent TOML");
  }, [draft, pushToast]);

  // ── Team canvas data ──────────────────────────────────

  const teamNodes: TeamNode[] = useMemo(() => {
    const nodes: TeamNode[] = [];
    // Leader (current agent)
    if (draft.name) {
      nodes.push({
        id: existingAgent?.id || "__self__",
        label: draft.name,
        icon: draft.icon,
        color: draft.color,
        role: "Leader",
        kind: "leader",
      });
    }
    // Sub-agents
    for (const sub of draft.subagents) {
      const matched = allAgents.find((a) => a.id === sub.id);
      nodes.push({
        id: sub.id,
        label: sub.name,
        icon: matched?.icon || "🤖",
        color: matched?.color || "#6b7280",
        role: sub.role || "Member",
        kind: "member",
      });
    }
    return nodes;
  }, [draft, allAgents, existingAgent]);

  const teamEdges: TeamEdge[] = useMemo(() => {
    const selfId = existingAgent?.id || "__self__";
    return draft.subagents.map((sub) => ({
      from: selfId,
      to: sub.id,
      label: sub.role ? `delegates: ${sub.role}` : "spawns",
    }));
  }, [draft.subagents, existingAgent]);

  // ── Custom model dropdown state ────────────────────
  const [showModelDropdown, setShowModelDropdown] = useState(false);
  const [modelSearch, setModelSearch] = useState("");
  const modelDropdownRef = useRef<HTMLDivElement>(null);

  const selectedModelLabel = useMemo(() => {
    if (!draft.model) return null;
    if (draft.model === "default") return "Default (auto-detect)";
    for (const g of modelGroups) {
      const m = g.models.find((m) => m.id === draft.model);
      if (m) return m.label;
    }
    return draft.model;
  }, [draft.model, modelGroups]);

  const filteredModelGroups = useMemo(() => {
    if (!modelSearch.trim()) return modelGroups;
    const q = modelSearch.toLowerCase();
    return modelGroups
      .map((g) => ({
        ...g,
        models: g.models.filter(
          (m) => m.label.toLowerCase().includes(q) || m.id.toLowerCase().includes(q),
        ),
      }))
      .filter((g) => g.models.length > 0);
  }, [modelGroups, modelSearch]);

  // Close model dropdown on outside click
  useEffect(() => {
    if (!showModelDropdown) return;
    const handler = (e: MouseEvent) => {
      if (modelDropdownRef.current && !modelDropdownRef.current.contains(e.target as Node)) {
        setShowModelDropdown(false);
        setModelSearch("");
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [showModelDropdown]);

  // ── Render ────────────────────────────────────────────

  return (
    <div className="modal-root" role="dialog" aria-modal="true" aria-label="Agent Journey">
      <button className="modal-backdrop" onClick={onClose} aria-label="Close" />
      <div className="jw-card">
        {/* ── Left: Step Rail ─────────────────────────── */}
        <nav className="jw-rail">
          <div className="jw-rail-brand">
            <div className="jw-rail-avatar" style={{ background: `linear-gradient(135deg, ${draft.color}, ${draft.color}88)` }}>
              <span>{draft.icon}</span>
            </div>
            <div className="jw-rail-title">
              {isEditing ? "Edit Agent" : "New Agent"}
            </div>
          </div>

          <div className="jw-rail-steps">
            {STEPS.map((s, idx) => {
              const isActive = s.key === step;
              const isCompleted = idx < currentStepIdx;
              const errCount = validationIssues.filter((i) => i.step === s.key && i.severity === "error").length;
              return (
                <button
                  key={s.key}
                  className={`jw-step ${isActive ? "jw-step--active" : ""} ${isCompleted ? "jw-step--done" : ""}`}
                  onClick={() => setStep(s.key)}
                >
                  <div className="jw-step-num">
                    {isCompleted ? "✓" : idx + 1}
                  </div>
                  <div className="jw-step-info">
                    <span className="jw-step-name">{s.label}</span>
                    <span className="jw-step-sub">{s.desc}</span>
                  </div>
                  {errCount > 0 && <span className="jw-step-err">{errCount}</span>}
                </button>
              );
            })}
          </div>

          <button
            className="jw-rail-versions"
            onClick={() => setShowVersionHistory(!showVersionHistory)}
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"><circle cx="12" cy="12" r="3"/><path d="M12 3v6m0 6v6M3 12h6m6 0h6"/></svg>
            {versions.length} version{versions.length !== 1 ? "s" : ""}
          </button>
        </nav>

        {/* ── Right: Content Panel ────────────────────── */}
        <div className="jw-main">
          {/* Step header */}
          <header className="jw-header">
            <div className="jw-header-left">
              <div className="jw-header-icon">{STEPS[currentStepIdx].icon}</div>
              <div>
                <h2 className="jw-header-title">{STEPS[currentStepIdx].label}</h2>
                <p className="jw-header-sub">{STEPS[currentStepIdx].desc}</p>
              </div>
            </div>
            <button className="jw-close" onClick={onClose} aria-label="Close">
              <Icon name="close" />
            </button>
          </header>

          {/* Step body */}
          <div className="jw-body">

            {/* ═══ STEP 1: IDENTITY ═══ */}
            {step === "identity" && (
              <div className="jw-section">
                {/* Hero icon + name side by side */}
                <div className="jw-id-hero">
                  <div className="jw-id-icon-wrap">
                    <button
                      className="jw-id-icon-btn"
                      onClick={() => setShowIconPicker(!showIconPicker)}
                      style={{ "--agent-color": draft.color } as React.CSSProperties}
                    >
                      <span className="jw-id-icon-emoji">{draft.icon}</span>
                      <div className="jw-id-icon-overlay">Change</div>
                    </button>
                    {showIconPicker && (
                      <div className="jw-id-icon-popover">
                        <div className="jw-id-icon-popover-title">Choose Icon</div>
                        <div className="jw-id-icon-grid">
                          {ICON_OPTIONS.map((ico) => (
                            <button
                              key={ico}
                              className={`jw-id-icon-opt ${ico === draft.icon ? "jw-id-icon-opt--sel" : ""}`}
                              onClick={() => { updateDraft({ icon: ico }); setShowIconPicker(false); }}
                            >
                              {ico}
                            </button>
                          ))}
                        </div>
                      </div>
                    )}
                  </div>

                  <div className="jw-id-name-wrap">
                    <label className="jw-label">Agent Name</label>
                    <input
                      type="text"
                      className="jw-input jw-input--hero"
                      placeholder="Research Assistant, Strategy Lead..."
                      value={draft.name}
                      onChange={(e) => updateDraft({ name: e.target.value })}
                      autoFocus
                    />
                  </div>
                </div>

                {/* Model + Color in glass cards */}
                <div className="jw-id-row">
                  <div className="jw-glass-card jw-id-model-card">
                    <label className="jw-label">Model</label>
                    <div className="jw-model-select" ref={modelDropdownRef}>
                      <button
                        className="jw-model-trigger"
                        onClick={() => setShowModelDropdown(!showModelDropdown)}
                      >
                        <span className="jw-model-trigger-text">
                          {selectedModelLabel || "Choose a model..."}
                        </span>
                        <svg className={`jw-model-chevron ${showModelDropdown ? "jw-model-chevron--open" : ""}`} width="12" height="12" viewBox="0 0 12 12" fill="none">
                          <path d="M3 4.5L6 7.5L9 4.5" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round"/>
                        </svg>
                      </button>
                      {showModelDropdown && (
                        <div className="jw-model-dropdown">
                          <div className="jw-model-search-wrap">
                            <input
                              className="jw-model-search"
                              placeholder="Search models..."
                              value={modelSearch}
                              onChange={(e) => setModelSearch(e.target.value)}
                              autoFocus
                            />
                          </div>
                          <div className="jw-model-list">
                            <button
                              className={`jw-model-opt ${draft.model === "default" ? "jw-model-opt--sel" : ""}`}
                              onClick={() => { updateDraft({ model: "default" }); setShowModelDropdown(false); setModelSearch(""); }}
                            >
                              <span className="jw-model-opt-icon">✨</span>
                              <span className="jw-model-opt-name">Default (auto-detect)</span>
                            </button>
                            {filteredModelGroups.map((g) => (
                              <div key={g.provider} className="jw-model-group">
                                <div className="jw-model-group-label">{g.provider}</div>
                                {g.models.map((m) => (
                                  <button
                                    key={m.id}
                                    className={`jw-model-opt ${draft.model === m.id ? "jw-model-opt--sel" : ""}`}
                                    onClick={() => { updateDraft({ model: m.id }); setShowModelDropdown(false); setModelSearch(""); }}
                                  >
                                    <span className="jw-model-opt-name">{m.label}</span>
                                  </button>
                                ))}
                              </div>
                            ))}
                            {filteredModelGroups.length === 0 && !modelSearch && (
                              <div className="jw-model-empty">No providers configured yet</div>
                            )}
                            {filteredModelGroups.length === 0 && modelSearch && (
                              <div className="jw-model-empty">No models match "{modelSearch}"</div>
                            )}
                          </div>
                        </div>
                      )}
                    </div>
                  </div>

                  <div className="jw-glass-card jw-id-color-card">
                    <label className="jw-label">Brand Color</label>
                    <div className="jw-color-pick">
                      <input
                        type="color"
                        value={draft.color}
                        onChange={(e) => updateDraft({ color: e.target.value })}
                        className="jw-color-input"
                      />
                      <span className="jw-color-hex">{draft.color}</span>
                    </div>
                  </div>
                </div>

                {/* Live preview */}
                <div className="jw-id-preview">
                  <div className="jw-id-preview-label">Preview</div>
                  <div className="jw-id-preview-agent" style={{ "--agent-color": draft.color } as React.CSSProperties}>
                    <div className="jw-id-preview-avatar">
                      <span>{draft.icon}</span>
                    </div>
                    <div className="jw-id-preview-info">
                      <span className="jw-id-preview-name">{draft.name || "Your Agent"}</span>
                      <span className="jw-id-preview-model">{selectedModelLabel || "Default model"}</span>
                    </div>
                    <div className="jw-id-preview-status">
                      <span className="jw-id-preview-dot" />
                      Ready
                    </div>
                  </div>
                </div>
              </div>
            )}

            {/* ═══ STEP 2: PERSONA ═══ */}
            {step === "persona" && (
              <div className="jw-section">
                <div className="jw-glass-card">
                  <label className="jw-label">System Prompt</label>
                  <p className="jw-hint">
                    Describe this agent's personality, role, and constraints. More detail = better performance.
                  </p>
                  <div className="jw-persona-wrap">
                    <textarea
                      className="jw-persona-textarea"
                      value={draft.persona}
                      onChange={(e) => updateDraft({ persona: e.target.value })}
                      placeholder="You are a thorough research assistant. You search the web, read papers, extract key findings, and cite your sources..."
                    />
                    <div className="jw-persona-bar">
                      <span className="jw-persona-count">{draft.persona.length} chars</span>
                      <span className={`jw-persona-quality ${draft.persona.length >= 100 ? "jw-persona-quality--good" : draft.persona.length >= 20 ? "jw-persona-quality--ok" : "jw-persona-quality--low"}`}>
                        {draft.persona.length < 20 ? "Too short" : draft.persona.length < 100 ? "Add more detail" : "Good length"}
                      </span>
                    </div>
                  </div>
                </div>

                <div className="jw-field">
                  <label className="jw-label">Quick Templates</label>
                  <div className="jw-templates">
                    {[
                      { label: "🔬 Researcher", text: "You are a thorough research assistant. Search the web, read papers, extract key findings, and cite your sources. Always provide structured summaries." },
                      { label: "✍️ Writer", text: "You are a professional writer. Create engaging content — articles, reports, emails, documentation. Maintain consistent tone and structure." },
                      { label: "📊 Analyst", text: "You are a data analyst. Break down complex data, find patterns, create visualizations, and deliver actionable insights. Be precise and evidence-based." },
                      { label: "🎯 Strategist", text: "You are a strategic advisor. Evaluate options, weigh trade-offs, consider long-term implications, and recommend clear courses of action." },
                    ].map((tmpl) => (
                      <button
                        key={tmpl.label}
                        className="jw-template-chip"
                        onClick={() => updateDraft({ persona: tmpl.text })}
                      >
                        {tmpl.label}
                      </button>
                    ))}
                  </div>
                </div>
              </div>
            )}

            {/* ═══ STEP 3: CAPABILITIES ═══ */}
            {step === "capabilities" && (
              <div className="jw-section">
                <div className="jw-field">
                  <label className="jw-label">Skills</label>
                  <p className="jw-hint">Select the capabilities this agent should have.</p>
                  <div className="jw-skills-grid">
                    {SKILL_OPTIONS.map((s) => {
                      const sel = draft.skills.includes(s.id);
                      return (
                        <button
                          key={s.id}
                          className={`jw-skill ${sel ? "jw-skill--on" : ""}`}
                          onClick={() => toggleSkill(s.id)}
                        >
                          <span className="jw-skill-icon">{s.icon}</span>
                          <span className="jw-skill-name">{s.label}</span>
                          <span className="jw-skill-desc">{s.desc}</span>
                          {sel && <span className="jw-skill-check">✓</span>}
                        </button>
                      );
                    })}
                  </div>
                  <div className="jw-custom-skill">
                    <input
                      type="text"
                      className="jw-input"
                      placeholder="Add custom skill..."
                      value={customSkill}
                      onChange={(e) => setCustomSkill(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter" && customSkill.trim()) {
                          toggleSkill(customSkill.trim());
                          setCustomSkill("");
                        }
                      }}
                    />
                    <button
                      className="jw-add-btn"
                      disabled={!customSkill.trim()}
                      onClick={() => { toggleSkill(customSkill.trim()); setCustomSkill(""); }}
                    >
                      Add
                    </button>
                  </div>
                </div>

                <div className="jw-field">
                  <label className="jw-label">Channels</label>
                  <p className="jw-hint">Which channels can this agent respond on? Leave empty for all.</p>
                  <div className="jw-channels">
                    {CHANNEL_OPTIONS.map((ch) => (
                      <button
                        key={ch.id}
                        className={`jw-channel ${draft.channels.includes(ch.id) ? "jw-channel--on" : ""}`}
                        onClick={() => toggleChannel(ch.id)}
                      >
                        <span>{ch.icon}</span> {ch.label}
                      </button>
                    ))}
                  </div>
                </div>

                <div className="jw-glass-card">
                  <label className="jw-label">Token Budget</label>
                  <div className="jw-budget">
                    <input
                      type="range"
                      min={1000}
                      max={500000}
                      step={1000}
                      value={draft.tokenBudget}
                      onChange={(e) => updateDraft({ tokenBudget: parseInt(e.target.value) })}
                      className="jw-budget-slider"
                    />
                    <span className="jw-budget-val">{draft.tokenBudget.toLocaleString()}</span>
                  </div>
                </div>
              </div>
            )}

            {/* ═══ STEP 4: TEAM ═══ */}
            {step === "team" && (
              <div className="jw-section">
                <div className="jw-field">
                  <label className="jw-label">Agent Team Topology</label>
                  <p className="jw-hint">
                    Build a multi-agent team. The current agent is the leader who delegates to sub-agents.
                  </p>
                </div>

                <div className="jw-team-canvas-wrap">
                  <AgentTeamCanvas
                    nodes={teamNodes}
                    edges={teamEdges}
                    width={580}
                    height={300}
                    onNodeClick={(node) => {
                      if (node.kind === "member") {
                        pushToast(`${node.label}: ${node.role || "No role assigned"}`);
                      }
                    }}
                    onAddNode={() => setShowAddTeamPicker(true)}
                    onRemoveNode={(id) => removeSubAgent(id)}
                  />
                </div>

                {draft.subagents.length > 0 && (
                  <div className="jw-field">
                    <label className="jw-label">Team Roles</label>
                    <div className="jw-team-roles">
                      {draft.subagents.map((sub) => {
                        const matched = allAgents.find((a) => a.id === sub.id);
                        return (
                          <div key={sub.id} className="jw-team-row">
                            <span className="jw-team-row-icon">{matched?.icon || "🤖"}</span>
                            <span className="jw-team-row-name">{sub.name}</span>
                            <input
                              type="text"
                              className="jw-input"
                              placeholder="Role (e.g., Researcher)"
                              value={sub.role}
                              onChange={(e) => {
                                setDraft((prev) => ({
                                  ...prev,
                                  subagents: prev.subagents.map((s) =>
                                    s.id === sub.id ? { ...s, role: e.target.value } : s,
                                  ),
                                }));
                              }}
                              list="role-suggestions"
                            />
                          </div>
                        );
                      })}
                      <datalist id="role-suggestions">
                        {ROLE_SUGGESTIONS.map((r) => <option key={r} value={r} />)}
                      </datalist>
                    </div>
                  </div>
                )}

                {showAddTeamPicker && (
                  <div className="jw-overlay" onClick={() => setShowAddTeamPicker(false)}>
                    <div className="jw-picker" onClick={(e) => e.stopPropagation()}>
                      <div className="jw-picker-title">Add Team Member</div>
                      {availableForTeam.length === 0 ? (
                        <p className="jw-picker-empty">No other agents available. Create more agents first.</p>
                      ) : (
                        availableForTeam.map((agent) => (
                          <button
                            key={agent.id}
                            className="jw-picker-opt"
                            onClick={() => addSubAgent(agent)}
                          >
                            <span className="jw-picker-opt-icon">{agent.icon}</span>
                            <div className="jw-picker-opt-info">
                              <span className="jw-picker-opt-name">{agent.name}</span>
                              <span className="jw-picker-opt-desc">{agent.persona.slice(0, 60)}...</span>
                            </div>
                            <span className="jw-picker-opt-add">+ Add</span>
                          </button>
                        ))
                      )}
                    </div>
                  </div>
                )}
              </div>
            )}

            {/* ═══ STEP 5: REVIEW ═══ */}
            {step === "review" && (
              <div className="jw-section">
                {/* Hero summary */}
                <div className="jw-review-hero" style={{ "--agent-color": draft.color } as React.CSSProperties}>
                  <div className="jw-review-avatar">
                    <span>{draft.icon}</span>
                  </div>
                  <div className="jw-review-hero-info">
                    <h3 className="jw-review-name">{draft.name || "Unnamed Agent"}</h3>
                    <span className="jw-review-meta">
                      {selectedModelLabel || "Default"} · {draft.skills.length} skills · {draft.subagents.length} members
                    </span>
                  </div>
                </div>

                {/* Sections */}
                <div className="jw-review-grid">
                  <div className="jw-glass-card">
                    <div className="jw-review-section-label">Persona</div>
                    <p className="jw-review-text">
                      {draft.persona.length > 200 ? draft.persona.slice(0, 200) + "..." : draft.persona || "(not set)"}
                    </p>
                  </div>

                  <div className="jw-glass-card">
                    <div className="jw-review-section-label">Skills</div>
                    <div className="jw-review-chips">
                      {draft.skills.length > 0
                        ? draft.skills.map((s) => {
                            const matched = SKILL_OPTIONS.find((so) => so.id === s);
                            return (
                              <span key={s} className="jw-chip">{matched?.icon || "⚡"} {matched?.label || s}</span>
                            );
                          })
                        : <span className="jw-review-empty">None selected</span>}
                    </div>
                  </div>

                  {draft.channels.length > 0 && (
                    <div className="jw-glass-card">
                      <div className="jw-review-section-label">Channels</div>
                      <div className="jw-review-chips">
                        {draft.channels.map((ch) => {
                          const matched = CHANNEL_OPTIONS.find((co) => co.id === ch);
                          return <span key={ch} className="jw-chip">{matched?.icon || "📡"} {matched?.label || ch}</span>;
                        })}
                      </div>
                    </div>
                  )}

                  {draft.subagents.length > 0 && (
                    <div className="jw-glass-card">
                      <div className="jw-review-section-label">Team</div>
                      <div className="jw-review-chips">
                        {draft.subagents.map((sub) => {
                          const matched = allAgents.find((a) => a.id === sub.id);
                          return <span key={sub.id} className="jw-chip">{matched?.icon || "🤖"} {sub.name}</span>;
                        })}
                      </div>
                    </div>
                  )}

                  <div className="jw-glass-card">
                    <div className="jw-review-section-label">Token Budget</div>
                    <span className="jw-review-mono">{draft.tokenBudget.toLocaleString()}</span>
                  </div>
                </div>

                {validationIssues.length > 0 && (
                  <div className="jw-issues">
                    {validationIssues.map((issue, i) => (
                      <div key={i} className={`jw-issue jw-issue--${issue.severity}`}>
                        <span>{issue.severity === "error" ? "●" : "○"}</span>
                        <span className="jw-issue-text"><strong>{issue.field}:</strong> {issue.message}</span>
                        <button className="jw-issue-fix" onClick={() => setStep(issue.step)}>Fix →</button>
                      </div>
                    ))}
                  </div>
                )}

                <button className="jw-export-btn" onClick={handleExportToml}>
                  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"><path d="M21 15v4a2 2 0 01-2 2H5a2 2 0 01-2-2v-4"/><polyline points="7 10 12 15 17 10"/><line x1="12" y1="15" x2="12" y2="3"/></svg>
                  Export TOML
                </button>
              </div>
            )}
          </div>

          {/* ── Version History Panel ─────────────────── */}
          {showVersionHistory && (
            <div className="jw-ver-panel">
              <div className="jw-ver-panel-head">
                <strong>Version History</strong>
                <button className="jw-close" onClick={() => setShowVersionHistory(false)}>
                  <Icon name="close" />
                </button>
              </div>
              <div className="jw-ver-list">
                {[...versions].reverse().map((v, idx) => {
                  const isLatest = idx === 0;
                  const prev = versions.find((pv) => pv.id === v.parentId);
                  const changes = prev ? diffDrafts(prev.snapshot, v.snapshot) : [];
                  return (
                    <div key={v.id} className={`jw-ver-entry ${isLatest ? "jw-ver-entry--head" : ""}`}>
                      <div className="jw-ver-dot" />
                      <div className="jw-ver-info">
                        <div className="jw-ver-msg">
                          {v.commitMsg}
                          {isLatest && <span className="jw-ver-tag">HEAD</span>}
                        </div>
                        <div className="jw-ver-time">{new Date(v.timestamp).toLocaleTimeString()}</div>
                        {changes.length > 0 && (
                          <div className="jw-ver-diff">
                            {changes.map((c, ci) => <div key={ci}>{c}</div>)}
                          </div>
                        )}
                      </div>
                      {!isLatest && (
                        <button className="jw-ver-revert" onClick={() => revertToVersion(v)}>Revert</button>
                      )}
                    </div>
                  );
                })}
              </div>
            </div>
          )}

          {/* ── Footer ────────────────────────────────── */}
          <footer className="jw-footer">
            <button className="jw-footer-btn jw-footer-btn--back" onClick={goPrev} disabled={currentStepIdx === 0}>
              ← Back
            </button>
            <div className="jw-footer-progress">
              {STEPS.map((s, idx) => (
                <button
                  key={s.key}
                  className={`jw-progress-dot ${idx === currentStepIdx ? "jw-progress-dot--cur" : ""} ${idx < currentStepIdx ? "jw-progress-dot--done" : ""}`}
                  onClick={() => setStep(s.key)}
                  aria-label={s.label}
                />
              ))}
            </div>
            {currentStepIdx < STEPS.length - 1 ? (
              <button className="jw-footer-btn jw-footer-btn--next" onClick={goNext}>
                Next →
              </button>
            ) : (
              <button
                className="jw-footer-btn jw-footer-btn--deploy"
                onClick={handleSave}
                disabled={isSaving || hasErrors}
              >
                {isSaving ? "Deploying..." : isEditing ? "Update & Deploy" : "Create & Deploy"}
              </button>
            )}
          </footer>
        </div>
      </div>
    </div>
  );
}
