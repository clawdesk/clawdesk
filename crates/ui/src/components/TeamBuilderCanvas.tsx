import { useState, useCallback, useMemo, useRef, useEffect } from "react";
import * as api from "../api";
import type { DesktopAgent, CreateAgentRequest, ProviderCapabilityInfo } from "../types";
import type { AgentCatalogEntry } from "../api";
import { Icon } from "./Icon";
import { loadProviders } from "../providerConfig";
import { PROVIDER_MODELS } from "../onboarding/OnboardingWizard";

// ── Types ─────────────────────────────────────────────────────

export interface TeamMember {
  id: string;
  name: string;
  icon: string;
  color: string;
  role: string;
  persona: string;
  model: string;
  skills: string[];
  channels: string[];
  tokenBudget: number;
  /** Visual position on canvas */
  x: number;
  y: number;
  /** Which member this delegates from (parent id). null = root/router */
  parentId: string | null;
}

interface TeamTemplate {
  name: string;
  description: string;
  icon: string;
  members: Omit<TeamMember, "x" | "y">[];
}

// ── Skill & channel options ───────────────────────────────────

const SKILL_OPTIONS = [
  { id: "web-search", label: "Web Search", icon: "🔍" },
  { id: "citations", label: "Citations", icon: "📎" },
  { id: "markdown", label: "Markdown", icon: "📝" },
  { id: "files", label: "File Access", icon: "📁" },
  { id: "email", label: "Email", icon: "📧" },
  { id: "calendar", label: "Calendar", icon: "📅" },
  { id: "cron", label: "Scheduled Tasks", icon: "⏰" },
  { id: "alerts", label: "Alerts", icon: "🔔" },
  { id: "code-exec", label: "Code Execution", icon: "💻" },
  { id: "image-gen", label: "Image Gen", icon: "🎨" },
];

const CHANNEL_OPTIONS = [
  { id: "telegram", label: "Telegram", icon: "📱" },
  { id: "discord", label: "Discord", icon: "🎮" },
  { id: "slack", label: "Slack", icon: "💼" },
  { id: "web", label: "Web Chat", icon: "🌐" },
  { id: "email", label: "Email", icon: "📧" },
];

// ── Templates ─────────────────────────────────────────────────

const TEAM_TEMPLATES: TeamTemplate[] = [
  {
    name: "Solo Founder Team",
    description: "Strategy lead + business analyst + marketing + dev agent — the 4-person startup team",
    icon: "🚀",
    members: [
      {
        id: "milo",
        name: "Milo",
        icon: "🎯",
        color: "#6366f1",
        role: "Strategy Lead",
        persona: "You are Milo, the team lead. Confident, big-picture, charismatic.\n\nResponsibilities:\n- Strategic planning and prioritization\n- Coordinating the other agents\n- Weekly goal setting and OKR tracking\n- Synthesizing insights from all agents into actionable decisions",
        model: "",
        skills: ["web-search", "email", "calendar", "cron", "alerts"],
        channels: ["telegram"],
        tokenBudget: 128000,
        parentId: null,
      },
      {
        id: "josh",
        name: "Josh",
        icon: "📊",
        color: "#2563eb",
        role: "Business & Growth",
        persona: "You are Josh, the business analyst. Pragmatic, straight to the point, numbers-driven.\n\nResponsibilities:\n- Pricing strategy and competitive analysis\n- Growth metrics and KPI tracking\n- Revenue modeling and unit economics\n- Customer feedback analysis",
        model: "",
        skills: ["web-search", "citations", "markdown"],
        channels: ["telegram"],
        tokenBudget: 128000,
        parentId: "milo",
      },
      {
        id: "marketing",
        name: "Marketing",
        icon: "✍️",
        color: "#16a34a",
        role: "Content & Research",
        persona: "You are the marketing researcher. Creative, curious, trend-aware.\n\nResponsibilities:\n- Content ideation and drafting\n- Competitor social media monitoring\n- Reddit/HN/X trend tracking for relevant topics\n- SEO keyword research",
        model: "",
        skills: ["web-search", "citations", "markdown", "cron"],
        channels: ["telegram"],
        tokenBudget: 128000,
        parentId: "milo",
      },
      {
        id: "dev",
        name: "Dev Agent",
        icon: "💻",
        color: "#ea580c",
        role: "Engineering",
        persona: "You are the dev agent. Precise, thorough, security-conscious.\n\nResponsibilities:\n- Coding and architecture decisions\n- Code review and quality checks\n- Bug investigation and fixing\n- Technical documentation",
        model: "",
        skills: ["code-exec", "files", "markdown", "cron"],
        channels: ["telegram"],
        tokenBudget: 128000,
        parentId: "milo",
      },
    ],
  },
  {
    name: "Research Squad",
    description: "Router directs queries to specialized researchers",
    icon: "🔬",
    members: [
      {
        id: "router",
        name: "Router",
        icon: "🔀",
        color: "#6366f1",
        role: "Router",
        persona: "You direct conversations to the appropriate specialist. Analyze each query and delegate to the right team member.",
        model: "",
        skills: ["markdown"],
        channels: [],
        tokenBudget: 64000,
        parentId: null,
      },
      {
        id: "researcher",
        name: "Researcher",
        icon: "🔍",
        color: "#2563eb",
        role: "Web Research",
        persona: "Google Search specialist. You find, verify, and synthesize information from the web with proper citations.",
        model: "",
        skills: ["web-search", "citations", "markdown"],
        channels: [],
        tokenBudget: 128000,
        parentId: "router",
      },
      {
        id: "docs-guru",
        name: "Docs_Guru",
        icon: "📚",
        color: "#a855f7",
        role: "Documentation Expert",
        persona: "Documentation expert. You find documentation for software repositories and libraries. Provide code snippets or commands based exactly on the docs.",
        model: "",
        skills: ["web-search", "files", "markdown"],
        channels: [],
        tokenBudget: 128000,
        parentId: "router",
      },
    ],
  },
  {
    name: "Blank Canvas",
    description: "Start from scratch — add agents and connect them",
    icon: "🎨",
    members: [],
  },
];

// ── Model group builder (shared with AgentJourneyWizard) ──────

interface ModelGroup {
  provider: string;
  models: { id: string; label: string }[];
}

function buildModelGroups(providerCaps: ProviderCapabilityInfo[]): ModelGroup[] {
  const groups: ModelGroup[] = [];
  const seen = new Set<string>();
  const configs = loadProviders();
  for (const cfg of configs) {
    const label = cfg.label || cfg.provider;
    const staticModels = PROVIDER_MODELS[cfg.provider] || [];
    const models: { id: string; label: string }[] = [];
    if (cfg.model && !seen.has(cfg.model)) {
      seen.add(cfg.model);
      const known = staticModels.find((m) => m.id === cfg.model);
      models.push({ id: cfg.model, label: known?.label || cfg.model });
    }
    for (const m of staticModels) {
      if (!seen.has(m.id)) {
        seen.add(m.id);
        models.push(m);
      }
    }
    if (models.length > 0) groups.push({ provider: label, models });
  }
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

// ── Layout helper ─────────────────────────────────────────────

function autoLayout(members: TeamMember[], canvasW: number): TeamMember[] {
  if (members.length === 0) return members;

  // Build a stable parent/child map, then lay nodes out in reading order
  // so teams feel more like a sequence than a wide DAG canvas.
  const indexById = new Map(members.map((member, index) => [member.id, index]));
  const roots = members.filter((m) => !m.parentId);
  const childMap = new Map<string, TeamMember[]>();
  for (const m of members) {
    if (m.parentId) {
      const kids = childMap.get(m.parentId) || [];
      kids.push(m);
      childMap.set(m.parentId, kids);
    }
  }
  for (const children of childMap.values()) {
    children.sort((a, b) => (indexById.get(a.id) ?? 0) - (indexById.get(b.id) ?? 0));
  }

  const NODE_W = 220;
  const START_X = Math.max(40, Math.min(120, Math.round((canvasW - NODE_W) / 4)));
  const COLUMN_GAP = 92;
  const ROW_GAP = 42;
  const NODE_H = 100;
  const result: TeamMember[] = [];
  const visited = new Set<string>();
  let row = 0;

  function layoutNode(node: TeamMember, depth: number) {
    if (visited.has(node.id)) return;
    visited.add(node.id);

    const x = START_X + depth * (NODE_W + COLUMN_GAP);
    const y = 40 + row * (NODE_H + ROW_GAP);
    result.push({ ...node, x, y });
    row += 1;

    const children = childMap.get(node.id) || [];
    for (const child of children) {
      layoutNode(child, depth + 1);
    }
  }

  const orderedRoots = [...roots].sort(
    (a, b) => (indexById.get(a.id) ?? 0) - (indexById.get(b.id) ?? 0),
  );
  for (const root of orderedRoots) {
    layoutNode(root, 0);
  }

  // Also find orphans (broken parent links) and place them at root depth.
  const orphans = members.filter((m) => m.parentId && !members.some((p) => p.id === m.parentId));
  const orderedOrphans = [...orphans].sort(
    (a, b) => (indexById.get(a.id) ?? 0) - (indexById.get(b.id) ?? 0),
  );
  for (const orphan of orderedOrphans) {
    layoutNode(orphan, 0);
  }

  for (const member of members) {
    if (!visited.has(member.id)) {
      layoutNode(member, 0);
    }
  }

  return result;
}

// ── Props ─────────────────────────────────────────────────────

export interface TeamBuilderCanvasProps {
  allAgents: DesktopAgent[];
  onClose: () => void;
  onTeamCreated: () => void;
  pushToast: (text: string) => void;
}

type BuilderMode = "auto" | "custom";

// ── Component ─────────────────────────────────────────────────

export function TeamBuilderCanvas({
  allAgents,
  onClose,
  onTeamCreated,
  pushToast,
}: TeamBuilderCanvasProps) {
  const [phase, setPhase] = useState<"template" | "canvas">("template");
  const [builderMode, setBuilderMode] = useState<BuilderMode>("auto");
  const [members, setMembers] = useState<TeamMember[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [linkingFromId, setLinkingFromId] = useState<string | null>(null);
  const [teamName, setTeamName] = useState("My Team");
  const [isDeploying, setIsDeploying] = useState(false);
  const [catalog, setCatalog] = useState<AgentCatalogEntry[]>([]);
  const [catalogFilter, setCatalogFilter] = useState("");
  const canvasRef = useRef<HTMLDivElement>(null);
  const backdropPressStartedRef = useRef(false);
  const dragStateRef = useRef<{
    memberId: string;
    offsetX: number;
    offsetY: number;
    pointerId: number;
  } | null>(null);

  // Dynamic model list
  const [providerCaps, setProviderCaps] = useState<ProviderCapabilityInfo[]>([]);
  useEffect(() => {
    api.listProviderCapabilities().then(setProviderCaps).catch(() => {});
    api.listAgentCatalog().then(setCatalog).catch(() => {});
  }, []);
  const modelGroups = useMemo(() => buildModelGroups(providerCaps), [providerCaps]);

  const filteredCatalog = useMemo(() => {
    if (!catalogFilter) return catalog;
    const q = catalogFilter.toLowerCase();
    return catalog.filter(
      (c) => c.name.toLowerCase().includes(q) || c.description.toLowerCase().includes(q) || c.tags.some((t) => t.includes(q))
    );
  }, [catalog, catalogFilter]);

  const availableCatalog = useMemo(
    () => filteredCatalog.filter((entry) => !members.some((member) => member.name === entry.name)),
    [filteredCatalog, members],
  );

  const availableAgents = useMemo(
    () => allAgents.filter((agent) => !members.some((member) => member.name === agent.name)),
    [allAgents, members],
  );

  const CANVAS_W = 700;
  const CANVAS_H = 520;

  const selected = useMemo(
    () => members.find((m) => m.id === selectedId) ?? null,
    [members, selectedId],
  );

  const insertionParentId = selected?.id ?? null;

  const canvasExtent = useMemo(() => {
    const padding = 120;
    const width = Math.max(
      CANVAS_W,
      ...members.map((member) => member.x + 220 + padding),
    );
    const height = Math.max(
      CANVAS_H,
      ...members.map((member) => member.y + 100 + padding),
    );
    return { width, height };
  }, [members]);

  const handleBackdropMouseDown = useCallback((event: React.MouseEvent<HTMLDivElement>) => {
    backdropPressStartedRef.current = event.target === event.currentTarget;
  }, []);

  const handleBackdropClick = useCallback((event: React.MouseEvent<HTMLDivElement>) => {
    const shouldClose = backdropPressStartedRef.current && event.target === event.currentTarget;
    backdropPressStartedRef.current = false;
    if (shouldClose) {
      onClose();
    }
  }, [onClose]);

  // ── Template selection ──────────────────────────────────

  const applyTemplate = useCallback((template: TeamTemplate) => {
    if (template.members.length === 0) {
      setMembers([]);
      setTeamName("My Team");
    } else {
      const ms: TeamMember[] = template.members.map((m) => ({
        ...m,
        x: 0,
        y: 0,
      }));
      setMembers(autoLayout(ms, CANVAS_W));
      setTeamName(template.name);
    }
    setSelectedId(null);
    setPhase("canvas");
  }, [CANVAS_W]);

  // ── Member CRUD ─────────────────────────────────────────

  const updateMember = useCallback((id: string, updates: Partial<TeamMember>) => {
    setMembers((prev) => prev.map((m) => m.id === id ? { ...m, ...updates } : m));
  }, []);

  const canAssignParent = useCallback((childId: string, parentId: string | null) => {
    if (parentId === null) return true;
    if (childId === parentId) return false;

    let cursor = members.find((member) => member.id === parentId) ?? null;
    while (cursor) {
      if (cursor.parentId === childId) return false;
      cursor = cursor.parentId
        ? members.find((member) => member.id === cursor.parentId) ?? null
        : null;
    }

    return true;
  }, [members]);

  const connectMembers = useCallback((parentId: string, childId: string) => {
    if (!canAssignParent(childId, parentId)) {
      pushToast("That connection would create a loop.");
      setLinkingFromId(null);
      return;
    }

    setMembers((prev) => autoLayout(
      prev.map((member) => member.id === childId ? { ...member, parentId } : member),
      CANVAS_W,
    ));
    setSelectedId(childId);
    setLinkingFromId(null);
  }, [CANVAS_W, canAssignParent, pushToast]);

  const addMember = useCallback((parentId: string | null, seed?: Partial<TeamMember>) => {
    const newId = `agent_${Date.now()}_${Math.random().toString(36).slice(2, 5)}`;
    const newMember: TeamMember = {
      id: newId,
      name: seed?.name ?? "New Agent",
      icon: seed?.icon ?? "🤖",
      color: seed?.color ?? "#6b7280",
      role: seed?.role ?? "",
      persona: seed?.persona ?? "",
      model: seed?.model ?? "",
      skills: seed?.skills ? [...seed.skills] : [],
      channels: seed?.channels ? [...seed.channels] : [],
      tokenBudget: seed?.tokenBudget ?? 128000,
      x: 0,
      y: 0,
      parentId,
    };
    setMembers((prev) => autoLayout([...prev, newMember], CANVAS_W));
    setSelectedId(newId);
  }, [CANVAS_W]);

  const addRouter = useCallback(() => {
    addMember(null, {
      name: "Router",
      icon: "🔀",
      color: "#6366f1",
      role: "Router",
      persona: "Direct conversations to the appropriate specialist.",
      tokenBudget: 64000,
    });
  }, [addMember]);

  const removeMember = useCallback((id: string) => {
    setMembers((prev) => {
      // Remove the member and all descendants
      const toRemove = new Set<string>();
      const queue = [id];
      while (queue.length > 0) {
        const curr = queue.shift()!;
        toRemove.add(curr);
        for (const m of prev) {
          if (m.parentId === curr) queue.push(m.id);
        }
      }
      return autoLayout(prev.filter((m) => !toRemove.has(m.id)), CANVAS_W);
    });
    if (selectedId === id) setSelectedId(null);
    if (linkingFromId === id) setLinkingFromId(null);
  }, [selectedId, linkingFromId, CANVAS_W]);

  const importExistingAgent = useCallback((agent: DesktopAgent, parentId: string | null) => {
    const newMember: TeamMember = {
      id: `agent_${Date.now()}_${agent.id}`,
      name: agent.name,
      icon: agent.icon,
      color: agent.color,
      role: "",
      persona: agent.persona,
      model: agent.model,
      skills: [...agent.skills],
      channels: [...(agent.channels ?? [])],
      tokenBudget: agent.token_budget,
      x: 0,
      y: 0,
      parentId,
    };
    setMembers((prev) => autoLayout([...prev, newMember], CANVAS_W));
  }, [CANVAS_W]);

  const importFromCatalog = useCallback((entry: AgentCatalogEntry, parentId: string | null) => {
    // Map TOML tool names to UI skill IDs
    const skillMap: Record<string, string> = {
      read_file: "files", file_read: "files", write_file: "files", file_write: "files",
      list_directory: "files", file_list: "files", search_files: "files", grep: "files",
      execute_command: "code-exec", shell_exec: "code-exec",
      web_search: "web-search", http_fetch: "web-search",
    };
    const skills = [...new Set(entry.tools.map((t) => skillMap[t] || t).filter(Boolean))];

    const iconMap: Record<string, string> = {
      engineering: "💻", research: "🔍", writing: "✍️", security: "🛡️",
      devops: "🔧", data: "📊", design: "🎨", orchestration: "🎯",
    };

    const newMember: TeamMember = {
      id: `cat_${Date.now()}_${entry.id}`,
      name: entry.name,
      icon: iconMap[entry.category] || "🤖",
      color: "#6366f1",
      role: entry.description,
      persona: entry.system_prompt,
      model: entry.model,
      skills,
      channels: [],
      tokenBudget: entry.max_tokens,
      x: 0,
      y: 0,
      parentId,
    };
    setMembers((prev) => autoLayout([...prev, newMember], CANVAS_W));
  }, [CANVAS_W]);

  const updateMemberPosition = useCallback((memberId: string, x: number, y: number) => {
    setMembers((prev) => prev.map((member) => (
      member.id === memberId
        ? { ...member, x: Math.max(20, Math.round(x)), y: Math.max(20, Math.round(y)) }
        : member
    )));
  }, []);

  useEffect(() => {
    const handlePointerMove = (event: PointerEvent) => {
      const dragState = dragStateRef.current;
      const canvas = canvasRef.current;
      if (!dragState || !canvas || event.pointerId !== dragState.pointerId) return;

      const rect = canvas.getBoundingClientRect();
      const nextX = event.clientX - rect.left + canvas.scrollLeft - dragState.offsetX;
      const nextY = event.clientY - rect.top + canvas.scrollTop - dragState.offsetY;
      updateMemberPosition(dragState.memberId, nextX, nextY);
    };

    const handlePointerUp = (event: PointerEvent) => {
      if (dragStateRef.current?.pointerId === event.pointerId) {
        dragStateRef.current = null;
      }
    };

    window.addEventListener("pointermove", handlePointerMove);
    window.addEventListener("pointerup", handlePointerUp);
    window.addEventListener("pointercancel", handlePointerUp);

    return () => {
      window.removeEventListener("pointermove", handlePointerMove);
      window.removeEventListener("pointerup", handlePointerUp);
      window.removeEventListener("pointercancel", handlePointerUp);
    };
  }, [updateMemberPosition]);

  // ── Deploy all agents ───────────────────────────────────

  const handleDeploy = useCallback(async () => {
    const errors: string[] = [];
    for (const m of members) {
      if (!m.name.trim()) errors.push("All agents need a name");
      if (!m.persona.trim()) errors.push(`${m.name}: persona is required`);
    }
    if (errors.length > 0) {
      pushToast(errors[0]);
      return;
    }

    // Generate a shared team ID so all agents in this team are grouped
    const teamId = `team-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 6)}`;

    setIsDeploying(true);
    try {
      let created = 0;
      let updated = 0;
      for (const m of members) {
        const isRouter = m.parentId === null;
        const role = isRouter ? "router" : m.role || "member";

        // Check if an agent with the same name already exists (avoid duplicates)
        const existing = allAgents.find(
          (a) => a.name.toLowerCase() === m.name.trim().toLowerCase() && !a.team_id,
        );

        if (existing) {
          // Update existing agent with team membership
          await api.updateAgent(existing.id, {
            icon: m.icon,
            color: m.color,
            persona: m.persona,
            skills: m.skills,
            model: m.model || "default",
            channels: m.channels,
            team_id: teamId,
            team_role: role,
          });
          updated++;
        } else {
          const req: CreateAgentRequest = {
            name: m.name,
            icon: m.icon,
            color: m.color,
            persona: m.persona,
            skills: m.skills,
            model: m.model || "default",
            channels: m.channels,
            team_id: teamId,
            team_role: role,
          };
          await api.createAgent(req);
          created++;
        }
      }
      const parts = [];
      if (created > 0) parts.push(`${created} created`);
      if (updated > 0) parts.push(`${updated} updated`);
      pushToast(`Team "${teamName}" deployed — ${parts.join(", ")}!`);
      onTeamCreated();
      onClose();
    } catch (err: any) {
      pushToast(`Deploy failed: ${err?.message || err}`);
    } finally {
      setIsDeploying(false);
    }
  }, [members, allAgents, teamName, pushToast, onTeamCreated, onClose]);

  // ── Edge drawing helpers ────────────────────────────────

  const edges = useMemo(() => {
    const result: { from: TeamMember; to: TeamMember }[] = [];
    for (const m of members) {
      if (m.parentId) {
        const parent = members.find((p) => p.id === m.parentId);
        if (parent) result.push({ from: parent, to: m });
      }
    }
    return result;
  }, [members]);

  // ── Template selection phase ────────────────────────────

  if (phase === "template") {
    return (
      <div className="tb-backdrop" onMouseDown={handleBackdropMouseDown} onClick={handleBackdropClick}>
        <div className="tb-modal tb-template-modal" onClick={(e) => e.stopPropagation()}>
          <header className="tb-header">
            <div className="tb-header-icon">🔗</div>
            <div>
              <h2 className="tb-title">Build Agent Team</h2>
              <p className="tb-subtitle">Choose a starting template or start from scratch</p>
            </div>
            <button type="button" className="tb-close" onClick={onClose}>
              <Icon name="close" />
            </button>
          </header>

          <div className="tb-templates">
            {TEAM_TEMPLATES.map((t) => (
              <button
                type="button"
                key={t.name}
                className="tb-template-card"
                onClick={() => applyTemplate(t)}
              >
                <span className="tb-template-icon">{t.icon}</span>
                <div className="tb-template-info">
                  <strong>{t.name}</strong>
                  <span>{t.description}</span>
                </div>
                <span className="tb-template-count">
                  {t.members.length === 0 ? "Empty" : `${t.members.length} agents`}
                </span>
              </button>
            ))}
          </div>
        </div>
      </div>
    );
  }

  // ── Canvas phase ────────────────────────────────────────

  const NODE_W = 220;
  const NODE_H = 100;

  return (
    <div className="tb-backdrop" onMouseDown={handleBackdropMouseDown} onClick={handleBackdropClick}>
      <div className="tb-modal tb-canvas-modal" onClick={(e) => e.stopPropagation()}>
        {/* ── Top bar ─────────────────────────────────── */}
        <header className="tb-header">
          <div className="tb-header-icon">🔗</div>
          <div className="tb-header-title-group">
            <input
              className="tb-team-name-input"
              value={teamName}
              onChange={(e) => setTeamName(e.target.value)}
              placeholder="Team name..."
            />
            <span className="tb-member-count">{members.length} agents</span>
          </div>
          <div className="tb-header-actions">
            <button type="button" className="tb-btn tb-btn--subtle" onClick={() => setPhase("template")}>
              ← Templates
            </button>
            <button
              type="button"
              className="tb-btn tb-btn--primary"
              onClick={handleDeploy}
              disabled={isDeploying || members.length === 0}
            >
              {isDeploying ? "Deploying..." : `Deploy ${members.length} Agents`}
            </button>
            <button type="button" className="tb-close" onClick={onClose}>
              <Icon name="close" />
            </button>
          </div>
        </header>

        <div className="tb-split">
          <aside className="tb-left-panel">
            <div className="tb-mode-switch" role="tablist" aria-label="Builder mode">
              <button
                type="button"
                className={`tb-mode-btn ${builderMode === "auto" ? "tb-mode-btn--active" : ""}`}
                onClick={() => setBuilderMode("auto")}
              >
                Auto
              </button>
              <button
                type="button"
                className={`tb-mode-btn ${builderMode === "custom" ? "tb-mode-btn--active" : ""}`}
                onClick={() => setBuilderMode("custom")}
              >
                Custom
              </button>
            </div>

            {builderMode === "auto" ? (
              <div className="tb-sidebar-section">
                <div className="tb-sidebar-copy">
                  <strong>Available agents</strong>
                  <span>Start from real agents and catalog entries. New additions attach to the selected node, or become a root when nothing is selected. Use the link handle on each card to connect lineage.</span>
                </div>
                <input
                  type="text"
                  className="tb-detail-input"
                  placeholder="Search catalog..."
                  value={catalogFilter}
                  onChange={(e) => setCatalogFilter(e.target.value)}
                />

                <div className="tb-sidebar-group">
                  <div className="tb-sidebar-group-head">
                    <span>Catalog</span>
                    <span>{availableCatalog.length}</span>
                  </div>
                  <div className="tb-sidebar-list tb-sidebar-list--stack">
                    {availableCatalog.length === 0 ? (
                      <span className="tb-sidebar-empty">No catalog agents match the current filter.</span>
                    ) : (
                      availableCatalog.slice(0, 12).map((entry) => (
                        <button
                          type="button"
                          key={entry.id}
                          className="tb-palette-btn"
                          onClick={() => importFromCatalog(entry, insertionParentId)}
                          title={entry.description}
                        >
                          <span className="tb-palette-btn__title">{entry.name}</span>
                          <span className="tb-palette-btn__meta">{entry.category} · {entry.model || "default"}</span>
                        </button>
                      ))
                    )}
                  </div>
                </div>

                <div className="tb-sidebar-group">
                  <div className="tb-sidebar-group-head">
                    <span>Existing agents</span>
                    <span>{availableAgents.length}</span>
                  </div>
                  <div className="tb-sidebar-list tb-sidebar-list--stack">
                    {availableAgents.length === 0 ? (
                      <span className="tb-sidebar-empty">All existing agents are already in this team.</span>
                    ) : (
                      availableAgents.slice(0, 8).map((agent) => (
                        <button
                          type="button"
                          key={agent.id}
                          className="tb-palette-btn"
                          onClick={() => importExistingAgent(agent, insertionParentId)}
                        >
                          <span className="tb-palette-btn__title">{agent.icon} {agent.name}</span>
                          <span className="tb-palette-btn__meta">{agent.model || "default"}</span>
                        </button>
                      ))
                    )}
                  </div>
                </div>
                {linkingFromId && (
                  <div className="tb-sidebar-note">
                    Connecting from {members.find((member) => member.id === linkingFromId)?.name || "selected agent"}. Click another card or its top dot to complete the link.
                  </div>
                )}
                {linkingFromId && (
                  <button type="button" className="tb-btn tb-btn--subtle" onClick={() => setLinkingFromId(null)}>
                    Cancel Link
                  </button>
                )}
              </div>
            ) : (
              <div className="tb-sidebar-section">
                <div className="tb-sidebar-copy">
                  <strong>Custom builder</strong>
                  <span>Freeform mode for hand-built teams, routing trees, and custom agent instructions.</span>
                </div>
                <div className="tb-sidebar-actions">
                  <button type="button" className="tb-btn tb-btn--primary" onClick={addRouter}>
                    + Add Router
                  </button>
                  <button type="button" className="tb-btn tb-btn--subtle" onClick={() => addMember(insertionParentId)}>
                    + Add Agent
                  </button>
                  {linkingFromId && (
                    <button type="button" className="tb-btn tb-btn--subtle" onClick={() => setLinkingFromId(null)}>
                      Cancel Link
                    </button>
                  )}
                </div>
                <div className="tb-sidebar-group">
                  <div className="tb-sidebar-group-head">
                    <span>Lineage</span>
                    <span>{members.length}</span>
                  </div>
                  <div className="tb-sidebar-tree">
                    {members.length === 0 ? (
                      <span className="tb-sidebar-empty">Create a router or agent, then use the link button on a card to connect it to another agent.</span>
                    ) : (
                      members.map((member) => {
                        const parent = members.find((candidate) => candidate.id === member.parentId);
                        return (
                          <button
                            type="button"
                            key={member.id}
                            className={`tb-lineage-item ${selected?.id === member.id ? "tb-lineage-item--active" : ""}`}
                            onClick={() => setSelectedId(member.id)}
                          >
                            <span className="tb-lineage-item__name">{member.icon} {member.name}</span>
                            <span className="tb-lineage-item__meta">{parent ? `from ${parent.name}` : "root"}</span>
                          </button>
                        );
                      })
                    )}
                  </div>
                </div>
                {linkingFromId && (
                  <div className="tb-sidebar-note">
                    Connecting from {members.find((member) => member.id === linkingFromId)?.name || "selected agent"}. Click another card or its top dot to complete the link.
                  </div>
                )}
                <div className="tb-sidebar-note">
                  {selected
                    ? `New agents will be added under ${selected.name}. Use “Delegates from” on the right to reconnect lineage.`
                    : "Select a node to add a child, or add a new root agent."}
                </div>
              </div>
            )}
          </aside>

          {/* ── Canvas ────────────────────────────────── */}
          <div
            className="tb-canvas-area"
            ref={canvasRef}
            data-no-drag
            onPointerDown={(event) => {
              if (event.target === event.currentTarget) {
                setSelectedId(null);
              }
            }}
          >
            <svg className="tb-svg-layer" width={canvasExtent.width} height={canvasExtent.height}>
              <defs>
                <marker
                  id="tb-arrow"
                  viewBox="0 0 10 7"
                  refX="10"
                  refY="3.5"
                  markerWidth="7"
                  markerHeight="5"
                  orient="auto"
                >
                  <polygon points="0 0, 10 3.5, 0 7" fill="var(--text-soft)" opacity="0.4" />
                </marker>
              </defs>
              {edges.map((edge, i) => {
                const sx = edge.from.x + NODE_W / 2;
                const sy = edge.from.y + NODE_H;
                const tx = edge.to.x + NODE_W / 2;
                const ty = edge.to.y;
                const midY = (sy + ty) / 2;
                return (
                  <path
                    key={i}
                    d={`M ${sx} ${sy} C ${sx} ${midY}, ${tx} ${midY}, ${tx} ${ty}`}
                    fill="none"
                    stroke="var(--text-soft)"
                    strokeWidth="1.5"
                    strokeDasharray="6 4"
                    opacity="0.35"
                    markerEnd="url(#tb-arrow)"
                  />
                );
              })}
            </svg>

            {/* HTML nodes overlay */}
            <div
              className="tb-nodes-layer"
              style={{ width: canvasExtent.width, height: canvasExtent.height }}
            >
              {members.map((m) => {
                const isRoot = !m.parentId;
                const isSelected = m.id === selectedId;
                const isLinkSource = m.id === linkingFromId;
                const canReceiveLink = linkingFromId !== null
                  && linkingFromId !== m.id
                  && canAssignParent(m.id, linkingFromId);
                return (
                  <div
                    key={m.id}
                    className={`tb-node ${isRoot ? "tb-node--root" : ""} ${isSelected ? "tb-node--selected" : ""} ${isLinkSource ? "tb-node--linking" : ""} ${canReceiveLink ? "tb-node--link-target" : ""}`}
                    style={{
                      left: m.x,
                      top: m.y,
                      width: NODE_W,
                      borderColor: isSelected ? m.color : undefined,
                      "--node-color": m.color,
                    } as React.CSSProperties}
                    data-no-drag
                    onPointerDown={(event) => {
                      if ((event.target as HTMLElement | null)?.closest("button")) return;
                      event.stopPropagation();

                      if (linkingFromId && linkingFromId !== m.id) {
                        connectMembers(linkingFromId, m.id);
                        return;
                      }

                      setSelectedId(m.id);
                      dragStateRef.current = {
                        memberId: m.id,
                        offsetX: event.clientX - event.currentTarget.getBoundingClientRect().left,
                        offsetY: event.clientY - event.currentTarget.getBoundingClientRect().top,
                        pointerId: event.pointerId,
                      };
                    }}
                  >
                    <button
                      type="button"
                      className={`tb-node-port tb-node-port--top ${canReceiveLink ? "tb-node-port--ready" : ""}`}
                      onClick={(e) => {
                        e.preventDefault();
                        e.stopPropagation();
                        if (linkingFromId && linkingFromId !== m.id) {
                          connectMembers(linkingFromId, m.id);
                        }
                      }}
                      title={canReceiveLink ? "Connect here" : "Incoming connection"}
                    />

                    <div className="tb-node-header">
                      <span className="tb-node-icon-badge" style={{ background: `${m.color}20`, color: m.color }}>
                        {isRoot ? "✦" : "✦"} {m.name}
                      </span>
                      {isRoot && <span className="tb-node-root-tag">Router</span>}
                    </div>
                    <p className="tb-node-desc">
                      {m.role || m.persona.slice(0, 50) || "No description."}
                    </p>
                    <div className="tb-node-tools">
                      {m.skills.slice(0, 3).map((s) => {
                        const sk = SKILL_OPTIONS.find((so) => so.id === s);
                        return (
                          <span key={s} className="tb-tool-badge" title={sk?.label || s}>
                            {sk?.icon || "⚡"}
                          </span>
                        );
                      })}
                      {m.skills.length > 3 && (
                        <span className="tb-tool-badge tb-tool-more">+{m.skills.length - 3}</span>
                      )}
                    </div>

                    {/* Add child button (on hover) */}
                    <button
                      type="button"
                      className="tb-node-add-child"
                      style={{ display: builderMode === "custom" ? undefined : "none" }}
                      onClick={(e) => {
                        e.stopPropagation();
                        addMember(m.id);
                      }}
                      title="Add sub-agent"
                    >
                      +
                    </button>

                    <button
                      type="button"
                      className={`tb-node-port tb-node-port--bottom ${isLinkSource ? "tb-node-port--active" : ""}`}
                      onClick={(e) => {
                        e.preventDefault();
                        e.stopPropagation();
                        setSelectedId(m.id);
                        setLinkingFromId((current) => current === m.id ? null : m.id);
                      }}
                      title={isLinkSource ? "Cancel connection" : "Start connection from this agent"}
                    >
                      <Icon name="link" />
                    </button>
                  </div>
                );
              })}

              {/* Add root node button */}
              {members.length === 0 && (
                <div className="tb-empty-canvas">
                  <span className="tb-empty-icon">🔗</span>
                  <p>
                    {builderMode === "auto"
                      ? "Pick an existing or catalog agent from the left panel to start the team."
                      : "Start building your agent team."}
                  </p>
                  <div style={{ display: "flex", gap: 8, flexWrap: "wrap", justifyContent: "center" }}>
                    {builderMode === "custom" ? (
                      <>
                        <button type="button" className="tb-btn tb-btn--primary" onClick={addRouter}>
                          + Add Router
                        </button>
                        <button type="button" className="tb-btn tb-btn--subtle" onClick={() => addMember(null)}>
                          + Custom Agent
                        </button>
                      </>
                    ) : null}
                  </div>
                </div>
              )}
            </div>

            {/* Floating + button at bottom */}
            {members.length > 0 && builderMode === "custom" && (
              <button
                type="button"
                className="tb-canvas-add-btn"
                onClick={() => addMember(members[0]?.id ?? null)}
                title="Add agent to team"
              >
                + Add Agent
              </button>
            )}
          </div>

          {/* ── Detail Panel ──────────────────────────── */}
          {selected ? (
            <aside className="tb-detail-panel">
              <div className="tb-detail-header">
                <div className="tb-detail-avatar" style={{ background: `${selected.color}20` }}>
                  <span>{selected.icon}</span>
                </div>
                <input
                  className="tb-detail-name-input"
                  value={selected.name}
                  onChange={(e) => updateMember(selected.id, { name: e.target.value })}
                  placeholder="Agent name"
                />
                {members.length > 1 && (
                  <button
                    type="button"
                    className="tb-detail-delete"
                    onPointerDown={(e) => {
                      e.preventDefault();
                      e.stopPropagation();
                    }}
                    onClick={(e) => {
                      e.preventDefault();
                      e.stopPropagation();
                      removeMember(selected.id);
                    }}
                    title="Remove agent"
                  >
                    <Icon name="trash" />
                  </button>
                )}
              </div>

              {/* Role */}
              <div className="tb-detail-field">
                <label className="tb-detail-label">Role</label>
                <input
                  className="tb-detail-input"
                  value={selected.role}
                  onChange={(e) => updateMember(selected.id, { role: e.target.value })}
                  placeholder="e.g., Strategy Lead, Researcher"
                />
              </div>

              {/* Instructions / Persona */}
              <div className="tb-detail-field">
                <label className="tb-detail-label">
                  Instructions <span className="tb-detail-hint">ⓘ</span>
                </label>
                <textarea
                  className="tb-detail-textarea"
                  value={selected.persona}
                  onChange={(e) => updateMember(selected.id, { persona: e.target.value })}
                  placeholder="Describe this agent's behavior, responsibilities, and personality..."
                  rows={5}
                />
                <span className="tb-detail-charcount">{selected.persona.length} / 8k</span>
              </div>

              {/* Model */}
              <div className="tb-detail-field">
                <label className="tb-detail-label">
                  Model <span className="tb-detail-hint">ⓘ</span>
                </label>
                <select
                  className="tb-detail-select"
                  value={selected.model}
                  onChange={(e) => updateMember(selected.id, { model: e.target.value })}
                >
                  <option value="">Default (auto-detect)</option>
                  {modelGroups.map((g) => (
                    <optgroup key={g.provider} label={g.provider}>
                      {g.models.map((m) => (
                        <option key={m.id} value={m.id}>{m.label}</option>
                      ))}
                    </optgroup>
                  ))}
                </select>
              </div>

              {/* Tools / Skills */}
              <div className="tb-detail-field">
                <label className="tb-detail-label">
                  Tools <span className="tb-detail-sublabel">Enable agent to complete tasks</span>
                </label>
                <div className="tb-tools-grid">
                  {SKILL_OPTIONS.map((sk) => {
                    const isOn = selected.skills.includes(sk.id);
                    return (
                      <button
                        type="button"
                        key={sk.id}
                        className={`tb-tool-toggle ${isOn ? "tb-tool-toggle--on" : ""}`}
                        onClick={() => {
                          const newSkills = isOn
                            ? selected.skills.filter((s) => s !== sk.id)
                            : [...selected.skills, sk.id];
                          updateMember(selected.id, { skills: newSkills });
                        }}
                      >
                        <span>{sk.icon}</span> {sk.label}
                      </button>
                    );
                  })}
                </div>
              </div>

              {/* Channels */}
              <div className="tb-detail-field">
                <label className="tb-detail-label">Channels</label>
                <div className="tb-tools-grid">
                  {CHANNEL_OPTIONS.map((ch) => {
                    const isOn = selected.channels.includes(ch.id);
                    return (
                      <button
                        type="button"
                        key={ch.id}
                        className={`tb-tool-toggle ${isOn ? "tb-tool-toggle--on" : ""}`}
                        onClick={() => {
                          const newCh = isOn
                            ? selected.channels.filter((c) => c !== ch.id)
                            : [...selected.channels, ch.id];
                          updateMember(selected.id, { channels: newCh });
                        }}
                      >
                        <span>{ch.icon}</span> {ch.label}
                      </button>
                    );
                  })}
                </div>
              </div>

              {/* Token Budget */}
              <div className="tb-detail-field">
                <label className="tb-detail-label">Token Budget</label>
                <div className="tb-budget-row">
                  <input
                    type="range"
                    min={1000}
                    max={500000}
                    step={1000}
                    value={selected.tokenBudget}
                    onChange={(e) => updateMember(selected.id, { tokenBudget: parseInt(e.target.value) })}
                    className="tb-budget-slider"
                  />
                  <span className="tb-budget-val">{selected.tokenBudget.toLocaleString()}</span>
                </div>
              </div>

              {/* Icon & Color */}
              <div className="tb-detail-field tb-detail-row">
                <div style={{ flex: 1 }}>
                  <label className="tb-detail-label">Icon</label>
                  <div className="tb-icon-grid">
                    {["🤖", "🧠", "💡", "🔍", "📊", "✍️", "📅", "🛡️", "🚀", "🎯", "📈", "🔧", "💬", "💻", "🌐", "⚡", "🔀", "📚"].map(
                      (ic) => (
                        <button
                          type="button"
                          key={ic}
                          className={`tb-icon-btn ${selected.icon === ic ? "tb-icon-btn--on" : ""}`}
                          onClick={() => updateMember(selected.id, { icon: ic })}
                        >
                          {ic}
                        </button>
                      ),
                    )}
                  </div>
                </div>
                <div>
                  <label className="tb-detail-label">Color</label>
                  <input
                    type="color"
                    className="tb-color-input"
                    value={selected.color}
                    onChange={(e) => updateMember(selected.id, { color: e.target.value })}
                  />
                </div>
              </div>

              {/* Parent / delegation */}
              <div className="tb-detail-field">
                <label className="tb-detail-label">Delegates from</label>
                <select
                  className="tb-detail-select"
                  value={selected.parentId ?? ""}
                  onChange={(e) => {
                    const newParent = e.target.value || null;
                    updateMember(selected.id, { parentId: newParent });
                    setMembers((prev) => autoLayout([...prev], CANVAS_W));
                  }}
                >
                  <option value="">None (root agent)</option>
                  {members
                    .filter((m) => m.id !== selected.id)
                    .map((m) => (
                      <option key={m.id} value={m.id}>{m.icon} {m.name}</option>
                    ))}
                </select>
              </div>

            </aside>
          ) : (
            <aside className="tb-detail-panel tb-detail-panel--empty">
              <div className="tb-detail-empty">
                <span className="tb-detail-empty-icon">👆</span>
                <p>Select an agent on the canvas to edit its properties</p>
              </div>
            </aside>
          )}
        </div>
      </div>
    </div>
  );
}
