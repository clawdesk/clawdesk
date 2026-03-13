import { useState, useCallback, useRef, useEffect, useMemo } from "react";
import * as api from "../api";
import type { DesktopAgent, CreateAgentRequest } from "../types";
import { Icon } from "./Icon";

// ── Types ─────────────────────────────────────────────────────

export type AgentDesignerTab = "editor" | "team" | "observe" | "toml";

export interface SubAgentRef {
  id: string;
  name: string;
  role: string;
}

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
];

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
  { id: "image-gen", label: "Image Generation", icon: "🎨" },
];

const MODEL_OPTIONS = [
  { id: "default", label: "Default (auto-detect)" },
  { id: "claude-sonnet-4-20250514", label: "Claude Sonnet 4" },
  { id: "claude-opus-4-20250514", label: "Claude Opus 4" },
  { id: "gpt-4.1", label: "GPT-4.1" },
  { id: "gpt-4o", label: "GPT-4o" },
  { id: "gemini-2.0-flash", label: "Gemini 2.0 Flash" },
  { id: "o3-mini", label: "o3-mini" },
];

const CHANNEL_OPTIONS = [
  { id: "telegram", label: "Telegram", icon: "📱" },
  { id: "discord", label: "Discord", icon: "🎮" },
  { id: "slack", label: "Slack", icon: "💼" },
  { id: "web", label: "Web Chat", icon: "🌐" },
  { id: "email", label: "Email", icon: "📧" },
];

const ROLE_SUGGESTIONS = [
  "Strategy Lead",
  "Business Analyst",
  "Marketing",
  "Developer",
  "Researcher",
  "QA / Testing",
  "Design",
  "DevOps",
];

// ── Props ─────────────────────────────────────────────────────

export interface AgentDesignerProps {
  /** Existing agent to edit (null = create new) */
  existingAgent?: DesktopAgent | null;
  /** All agents for sub-agent references */
  allAgents: DesktopAgent[];
  onClose: () => void;
  onSaved: (agent: DesktopAgent) => void;
  pushToast: (text: string) => void;
}

// ── Helpers ───────────────────────────────────────────────────

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
  const id = draft.name.toLowerCase().replace(/[^a-z0-9]+/g, "-");
  const lines: string[] = [];
  lines.push("[agent]");
  lines.push(`name = "${id}"`);
  lines.push(`description = "${draft.name}"`);
  lines.push(`version = "1.0.0"`);
  lines.push(`author = "User"`);
  const tags = draft.skills.length > 0 ? draft.skills.map((s) => `"${s}"`).join(", ") : '"general"';
  lines.push(`tags = [${tags}]`);
  lines.push("");
  lines.push("[model]");
  lines.push(`model = "${draft.model || "default"}"`);
  lines.push(`max_tokens = ${draft.tokenBudget}`);
  lines.push("");
  lines.push("[system_prompt]");
  const personaLines = draft.persona.split("\n");
  if (personaLines.length > 1) {
    lines.push(`content = """`);
    for (const l of personaLines) lines.push(l);
    lines.push(`"""`);
  } else {
    lines.push(`content = "${draft.persona.replace(/"/g, '\\"')}"`);
  }
  lines.push("");
  lines.push("[capabilities]");
  // Map UI skill IDs to tool names
  const skillToTools: Record<string, string[]> = {
    "files": ["read_file", "write_file", "list_directory", "search_files"],
    "code-exec": ["execute_command"],
    "web-search": ["web_search"],
    "markdown": [],
    "citations": [],
    "email": [],
    "calendar": [],
    "cron": [],
    "alerts": [],
    "image-gen": [],
  };
  const tools = [...new Set(draft.skills.flatMap((s) => skillToTools[s] || []))];
  lines.push(`tools = [${tools.map((t) => `"${t}"`).join(", ")}]`);
  lines.push("");
  lines.push("[resources]");
  lines.push(`max_tokens_per_hour = 300000`);
  lines.push(`max_tool_iterations = 15`);
  lines.push(`timeout_seconds = 300`);
  if (draft.channels.length > 0) {
    lines.push("");
    for (const ch of draft.channels) {
      lines.push(`[channels.${ch}]`);
      lines.push(`max_message_length = 4000`);
    }
  }
  if (draft.subagents.length > 0) {
    lines.push("");
    lines.push("[orchestration]");
    lines.push(`subagents = [${draft.subagents.map((s) => `"${s.id}"`).join(", ")}]`);
  }
  return lines.join("\n");
}

function parseTomlToDraft(toml: string): Partial<AgentDraft> {
  const draft: Partial<AgentDraft> = {};

  // Parse standard agent TOML format: [agent], [model], [system_prompt], [capabilities]
  // Also supports the legacy UI format for backwards compatibility
  const nameMatch = toml.match(/^name\s*=\s*"([^"]+)"/m)
    || toml.match(/display_name\s*=\s*"([^"]+)"/);
  if (nameMatch) draft.name = nameMatch[1];

  // [model] section
  const modelMatch = toml.match(/^model\s*=\s*"([^"]+)"/m);
  if (modelMatch) draft.model = modelMatch[1];

  const maxTokensMatch = toml.match(/max_tokens\s*=\s*(\d+)/);
  if (maxTokensMatch) draft.tokenBudget = parseInt(maxTokensMatch[1]);

  // [system_prompt] content
  const promptMulti = toml.match(/content\s*=\s*"""([\s\S]*?)"""/);
  if (promptMulti) {
    draft.persona = promptMulti[1].trim();
  } else {
    const promptSingle = toml.match(/content\s*=\s*"([^"]+)"/);
    if (promptSingle) draft.persona = promptSingle[1];
    // Legacy format fallback
    if (!draft.persona) {
      const goalMulti = toml.match(/goal\s*=\s*"""([\s\S]*?)"""/);
      if (goalMulti) draft.persona = goalMulti[1].trim();
      else {
        const goalSingle = toml.match(/goal\s*=\s*"([^"]+)"/);
        if (goalSingle) draft.persona = goalSingle[1];
      }
    }
  }

  // [capabilities] tools
  const toolsMatch = toml.match(/tools\s*=\s*\[([^\]]+)\]/);
  if (toolsMatch) {
    const rawTools = toolsMatch[1].split(",").map((s) => s.trim().replace(/"/g, "")).filter(Boolean);
    // Map tool names to UI skill IDs
    const toolToSkill: Record<string, string> = {
      read_file: "files", write_file: "files", list_directory: "files",
      search_files: "files", file_read: "files", file_write: "files",
      file_list: "files", grep: "files",
      execute_command: "code-exec", shell_exec: "code-exec",
      web_search: "web-search", http_fetch: "web-search",
    };
    draft.skills = [...new Set(rawTools.map((t) => toolToSkill[t] || t))];
  } else {
    // Legacy format: allowed = [...]
    const allowedMatch = toml.match(/allowed\s*=\s*\[([^\]]+)\]/);
    if (allowedMatch) {
      draft.skills = allowedMatch[1].split(",").map((s) => s.trim().replace(/"/g, "")).filter(Boolean);
    }
  }

  // Subagents / orchestration
  const spawnMatch = toml.match(/subagents\s*=\s*\[([^\]]+)\]/)
    || toml.match(/can_spawn\s*=\s*\[([^\]]+)\]/);
  if (spawnMatch) {
    draft.subagents = spawnMatch[1]
      .split(",")
      .map((s) => s.trim().replace(/"/g, ""))
      .filter(Boolean)
      .map((id) => ({ id, name: id, role: "" }));
  }

  return draft;
}

// ── Validation ────────────────────────────────────────────────

interface ValidationIssue {
  field: string;
  message: string;
  severity: "error" | "warning";
}

function validateDraft(draft: AgentDraft): ValidationIssue[] {
  const issues: ValidationIssue[] = [];
  if (!draft.name.trim()) {
    issues.push({ field: "name", message: "Agent name is required", severity: "error" });
  }
  if (!draft.persona.trim()) {
    issues.push({ field: "persona", message: "Persona / system prompt is required", severity: "error" });
  }
  if (draft.persona.length < 20) {
    issues.push({ field: "persona", message: "Persona is very short — consider adding more detail", severity: "warning" });
  }
  if (draft.skills.length === 0) {
    issues.push({ field: "skills", message: "No skills selected — agent will have limited capabilities", severity: "warning" });
  }
  if (draft.tokenBudget < 1000) {
    issues.push({ field: "tokenBudget", message: "Token budget seems very low", severity: "warning" });
  }
  return issues;
}

// ── Component ─────────────────────────────────────────────────

export function AgentDesigner({
  existingAgent,
  allAgents,
  onClose,
  onSaved,
  pushToast,
}: AgentDesignerProps) {
  const isEditing = !!existingAgent;

  const [tab, setTab] = useState<AgentDesignerTab>("editor");
  const [draft, setDraft] = useState<AgentDraft>(
    existingAgent ? draftFromAgent(existingAgent) : { ...EMPTY_DRAFT }
  );
  const [isSaving, setIsSaving] = useState(false);
  const [validationIssues, setValidationIssues] = useState<ValidationIssue[]>([]);
  const [tomlContent, setTomlContent] = useState("");
  const [showIconPicker, setShowIconPicker] = useState(false);
  const [customSkill, setCustomSkill] = useState("");
  const [customChannel, setCustomChannel] = useState("");
  const fileInputRef = useRef<HTMLInputElement>(null);

  // Live events for observability
  const [liveEvents, setLiveEvents] = useState<AgentLiveEvent[]>([]);
  const [isListening, setIsListening] = useState(false);
  const eventEndRef = useRef<HTMLDivElement>(null);

  // Validate on draft changes
  useEffect(() => {
    setValidationIssues(validateDraft(draft));
  }, [draft]);

  // Generate TOML when switching to that tab
  useEffect(() => {
    if (tab === "toml") {
      setTomlContent(draftToToml(draft));
    }
  }, [tab, draft]);

  // Auto-scroll live events
  useEffect(() => {
    eventEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [liveEvents]);

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

  const addSubAgent = useCallback((agent: DesktopAgent) => {
    setDraft((prev) => {
      if (prev.subagents.some((s) => s.id === agent.id)) return prev;
      return {
        ...prev,
        subagents: [...prev.subagents, { id: agent.id, name: agent.name, role: "" }],
      };
    });
  }, []);

  const removeSubAgent = useCallback((agentId: string) => {
    setDraft((prev) => ({
      ...prev,
      subagents: prev.subagents.filter((s) => s.id !== agentId),
    }));
  }, []);

  const handleImportToml = useCallback(() => {
    fileInputRef.current?.click();
  }, []);

  const handleFileUpload = useCallback((e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (!file) return;
    const reader = new FileReader();
    reader.onload = () => {
      const content = reader.result as string;
      const parsed = parseTomlToDraft(content);
      setDraft((prev) => ({ ...prev, ...parsed }));
      pushToast("Imported agent definition from TOML");
    };
    reader.readAsText(file);
    e.target.value = "";
  }, [pushToast]);

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

  const handleApplyToml = useCallback(() => {
    const parsed = parseTomlToDraft(tomlContent);
    setDraft((prev) => ({ ...prev, ...parsed }));
    setTab("editor");
    pushToast("Applied TOML changes to editor");
  }, [tomlContent, pushToast]);

  // ── Save ──────────────────────────────────────────────────

  const handleSave = useCallback(async () => {
    const issues = validateDraft(draft);
    setValidationIssues(issues);
    if (issues.some((i) => i.severity === "error")) {
      pushToast("Fix validation errors before saving.");
      return;
    }

    setIsSaving(true);
    try {
      if (isEditing && existingAgent) {
        // Update existing agent
        const updated = await api.updateAgent(existingAgent.id, {
          name: draft.name,
          icon: draft.icon,
          color: draft.color,
          persona: draft.persona,
          skills: draft.skills,
          model: draft.model,
          channels: draft.channels,
        });
        onSaved(updated);
        pushToast(`Agent "${draft.name}" updated.`);
      } else {
        // Create new agent
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
        onSaved(agent);
        pushToast(`Agent "${draft.name}" created.`);
      }
      onClose();
    } catch (err: any) {
      pushToast(`Failed to save agent: ${err?.message || err}`);
    } finally {
      setIsSaving(false);
    }
  }, [draft, isEditing, existingAgent, onClose, onSaved, pushToast]);

  // ── Available agents for team (exclude self) ──────────────

  const availableForTeam = useMemo(
    () => allAgents.filter((a) => a.id !== existingAgent?.id),
    [allAgents, existingAgent]
  );

  const hasErrors = validationIssues.some((i) => i.severity === "error");

  // ── Render: Tabs ──────────────────────────────────────────

  const tabs: { id: AgentDesignerTab; label: string; icon: string }[] = [
    { id: "editor", label: "Editor", icon: "✏️" },
    { id: "team", label: "Team", icon: "👥" },
    { id: "observe", label: "Activity", icon: "📊" },
    { id: "toml", label: "TOML", icon: "📄" },
  ];

  return (
    <div className="modal-root" role="dialog" aria-modal="true" aria-label="Agent Designer">
      <button className="modal-backdrop" onClick={onClose} aria-label="Close" />
      <div className="modal-card" style={{ maxWidth: 860, width: "92vw", maxHeight: "88vh" }}>
        {/* ── Header ──────────────────────────────────────── */}
        <div className="modal-head">
          <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
            <span style={{ fontSize: 22 }}>{draft.icon}</span>
            <h2>{isEditing ? `Edit: ${draft.name || "Agent"}` : "New Agent"}</h2>
          </div>
          <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
            {validationIssues.length > 0 && (
              <span style={{
                fontSize: 12,
                color: hasErrors ? "var(--error)" : "var(--warning)",
                display: "flex", alignItems: "center", gap: 4,
              }}>
                {hasErrors ? "⚠️" : "💡"} {validationIssues.length} issue{validationIssues.length !== 1 ? "s" : ""}
              </span>
            )}
            <button className="icon-button" onClick={onClose} aria-label="Close dialog">
              <Icon name="close" />
            </button>
          </div>
        </div>

        {/* ── Tab bar ─────────────────────────────────────── */}
        <div className="designer-tabs" style={{
          display: "flex", gap: 0, borderBottom: "1px solid var(--border)",
          padding: "0 16px",
        }}>
          {tabs.map((t) => (
            <button
              key={t.id}
              className={`designer-tab ${tab === t.id ? "active" : ""}`}
              onClick={() => setTab(t.id)}
              style={{
                padding: "10px 16px",
                border: "none",
                background: "none",
                borderBottom: tab === t.id ? "2px solid var(--brand)" : "2px solid transparent",
                color: tab === t.id ? "var(--brand)" : "var(--text-secondary)",
                fontWeight: tab === t.id ? 600 : 400,
                cursor: "pointer",
                display: "flex", alignItems: "center", gap: 6,
                fontSize: 13,
                transition: "all 0.15s",
              }}
            >
              <span>{t.icon}</span> {t.label}
            </button>
          ))}
        </div>

        {/* ── Body ────────────────────────────────────────── */}
        <div className="modal-body" style={{ padding: 0, overflow: "auto", maxHeight: "calc(88vh - 140px)" }}>

          {/* ═══ EDITOR TAB ═══ */}
          {tab === "editor" && (
            <div style={{ padding: "20px 24px", display: "flex", flexDirection: "column", gap: 20 }}>

              {/* ── Identity row ────────────────── */}
              <div style={{ display: "flex", gap: 16, alignItems: "flex-start" }}>
                {/* Icon picker */}
                <div style={{ position: "relative" }}>
                  <button
                    onClick={() => setShowIconPicker(!showIconPicker)}
                    style={{
                      width: 56, height: 56, fontSize: 28, border: "2px solid var(--border)",
                      borderRadius: 12, background: draft.color + "22", cursor: "pointer",
                      display: "flex", alignItems: "center", justifyContent: "center",
                    }}
                    title="Pick icon"
                  >
                    {draft.icon}
                  </button>
                  {showIconPicker && (
                    <div style={{
                      position: "absolute", top: 62, left: 0, zIndex: 100,
                      background: "var(--surface)", border: "1px solid var(--border)",
                      borderRadius: 8, padding: 8, display: "grid",
                      gridTemplateColumns: "repeat(4, 1fr)", gap: 4,
                      boxShadow: "0 8px 24px rgba(0,0,0,0.2)",
                    }}>
                      {ICON_OPTIONS.map((ico) => (
                        <button
                          key={ico}
                          onClick={() => { updateDraft({ icon: ico }); setShowIconPicker(false); }}
                          style={{
                            width: 36, height: 36, fontSize: 20, border: "none",
                            background: ico === draft.icon ? "var(--accent-bg)" : "transparent",
                            borderRadius: 6, cursor: "pointer",
                          }}
                        >
                          {ico}
                        </button>
                      ))}
                    </div>
                  )}
                </div>

                <div style={{ flex: 1, display: "flex", flexDirection: "column", gap: 8 }}>
                  <input
                    type="text"
                    placeholder="Agent name"
                    value={draft.name}
                    onChange={(e) => updateDraft({ name: e.target.value })}
                    className="input"
                    style={{ fontSize: 16, fontWeight: 600 }}
                  />
                  <div style={{ display: "flex", gap: 8 }}>
                    <input
                      type="color"
                      value={draft.color}
                      onChange={(e) => updateDraft({ color: e.target.value })}
                      style={{ width: 36, height: 32, cursor: "pointer", border: "none", padding: 0 }}
                      title="Accent color"
                    />
                    <select
                      value={draft.model}
                      onChange={(e) => updateDraft({ model: e.target.value })}
                      className="input"
                      style={{ flex: 1 }}
                    >
                      <option value="">Select model...</option>
                      {MODEL_OPTIONS.map((m) => (
                        <option key={m.id} value={m.id}>{m.label}</option>
                      ))}
                    </select>
                  </div>
                </div>
              </div>

              {/* ── Persona ────────────────────── */}
              <div>
                <label style={{ display: "block", fontWeight: 600, marginBottom: 6, fontSize: 13 }}>
                  Persona / System Prompt
                </label>
                <textarea
                  value={draft.persona}
                  onChange={(e) => updateDraft({ persona: e.target.value })}
                  placeholder="Describe what this agent does, its personality, guidelines, and constraints..."
                  className="input"
                  style={{
                    minHeight: 120, resize: "vertical", fontFamily: "inherit",
                    lineHeight: 1.5,
                  }}
                />
                <div style={{ fontSize: 11, color: "var(--text-tertiary)", marginTop: 4 }}>
                  {draft.persona.length} chars · Scanned by CascadeScanner before activation
                </div>
              </div>

              {/* ── Skills ─────────────────────── */}
              <div>
                <label style={{ display: "block", fontWeight: 600, marginBottom: 6, fontSize: 13 }}>
                  Skills
                </label>
                <div style={{ display: "flex", flexWrap: "wrap", gap: 6 }}>
                  {SKILL_OPTIONS.map((s) => (
                    <button
                      key={s.id}
                      onClick={() => toggleSkill(s.id)}
                      className={`chip ${draft.skills.includes(s.id) ? "chip-active" : ""}`}
                      style={{
                        padding: "5px 10px",
                        border: draft.skills.includes(s.id)
                          ? "1px solid var(--brand)"
                          : "1px solid var(--border)",
                        background: draft.skills.includes(s.id)
                          ? "var(--accent-bg)"
                          : "transparent",
                        cursor: "pointer",
                        borderRadius: 6,
                        fontSize: 12,
                        display: "flex", alignItems: "center", gap: 4,
                      }}
                    >
                      <span>{s.icon}</span> {s.label}
                    </button>
                  ))}
                </div>
                {/* Custom skill input */}
                <div style={{ display: "flex", gap: 6, marginTop: 8 }}>
                  <input
                    type="text"
                    placeholder="Custom skill"
                    value={customSkill}
                    onChange={(e) => setCustomSkill(e.target.value)}
                    className="input"
                    style={{ flex: 1, fontSize: 12 }}
                    onKeyDown={(e) => {
                      if (e.key === "Enter" && customSkill.trim()) {
                        toggleSkill(customSkill.trim());
                        setCustomSkill("");
                      }
                    }}
                  />
                  <button
                    className="btn subtle"
                    style={{ fontSize: 12 }}
                    disabled={!customSkill.trim()}
                    onClick={() => {
                      if (customSkill.trim()) {
                        toggleSkill(customSkill.trim());
                        setCustomSkill("");
                      }
                    }}
                  >
                    + Add
                  </button>
                </div>
              </div>

              {/* ── Channels ───────────────────── */}
              <div>
                <label style={{ display: "block", fontWeight: 600, marginBottom: 6, fontSize: 13 }}>
                  Channel Bindings
                  <span style={{ fontWeight: 400, color: "var(--text-tertiary)", marginLeft: 6 }}>
                    (empty = all channels)
                  </span>
                </label>
                <div style={{ display: "flex", flexWrap: "wrap", gap: 6 }}>
                  {CHANNEL_OPTIONS.map((ch) => (
                    <button
                      key={ch.id}
                      onClick={() => toggleChannel(ch.id)}
                      className="chip"
                      style={{
                        padding: "5px 10px",
                        border: draft.channels.includes(ch.id)
                          ? "1px solid var(--brand)"
                          : "1px solid var(--border)",
                        background: draft.channels.includes(ch.id)
                          ? "var(--accent-bg)"
                          : "transparent",
                        cursor: "pointer",
                        borderRadius: 6,
                        fontSize: 12,
                        display: "flex", alignItems: "center", gap: 4,
                      }}
                    >
                      <span>{ch.icon}</span> {ch.label}
                    </button>
                  ))}
                </div>
                <div style={{ display: "flex", gap: 6, marginTop: 8 }}>
                  <input
                    type="text"
                    placeholder="Custom channel"
                    value={customChannel}
                    onChange={(e) => setCustomChannel(e.target.value)}
                    className="input"
                    style={{ flex: 1, fontSize: 12 }}
                    onKeyDown={(e) => {
                      if (e.key === "Enter" && customChannel.trim()) {
                        toggleChannel(customChannel.trim());
                        setCustomChannel("");
                      }
                    }}
                  />
                  <button
                    className="btn subtle"
                    style={{ fontSize: 12 }}
                    disabled={!customChannel.trim()}
                    onClick={() => {
                      if (customChannel.trim()) {
                        toggleChannel(customChannel.trim());
                        setCustomChannel("");
                      }
                    }}
                  >
                    + Add
                  </button>
                </div>
              </div>

              {/* ── Token Budget ────────────────── */}
              <div style={{ display: "flex", gap: 16, alignItems: "center" }}>
                <label style={{ fontWeight: 600, fontSize: 13, whiteSpace: "nowrap" }}>
                  Token Budget
                </label>
                <input
                  type="range"
                  min={1000}
                  max={500000}
                  step={1000}
                  value={draft.tokenBudget}
                  onChange={(e) => updateDraft({ tokenBudget: parseInt(e.target.value) })}
                  style={{ flex: 1 }}
                />
                <span style={{ fontSize: 13, fontFamily: "monospace", minWidth: 70, textAlign: "right" }}>
                  {draft.tokenBudget.toLocaleString()}
                </span>
              </div>

              {/* ── Validation issues ──────────── */}
              {validationIssues.length > 0 && (
                <div style={{
                  background: "var(--surface)", border: "1px solid var(--border)",
                  borderRadius: 8, padding: 12,
                }}>
                  <div style={{ fontWeight: 600, fontSize: 12, marginBottom: 6 }}>Validation</div>
                  {validationIssues.map((issue, i) => (
                    <div key={i} style={{
                      fontSize: 12, padding: "3px 0",
                      color: issue.severity === "error" ? "var(--error)" : "var(--warning)",
                      display: "flex", alignItems: "center", gap: 6,
                    }}>
                      <span>{issue.severity === "error" ? "❌" : "⚠️"}</span>
                      <span><strong>{issue.field}:</strong> {issue.message}</span>
                    </div>
                  ))}
                </div>
              )}
            </div>
          )}

          {/* ═══ TEAM TAB ═══ */}
          {tab === "team" && (
            <div style={{ padding: "20px 24px", display: "flex", flexDirection: "column", gap: 20 }}>
              <div>
                <h3 style={{ margin: "0 0 4px", fontSize: 15 }}>Multi-Agent Team</h3>
                <p style={{ margin: 0, fontSize: 13, color: "var(--text-secondary)" }}>
                  Add sub-agents that this agent can spawn and coordinate with.
                  Sub-agents share team memory and can be delegated tasks.
                </p>
              </div>

              {/* Current sub-agents */}
              {draft.subagents.length > 0 && (
                <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
                  <label style={{ fontWeight: 600, fontSize: 13 }}>Team Members</label>
                  {draft.subagents.map((sub) => (
                    <div key={sub.id} style={{
                      display: "flex", alignItems: "center", gap: 10,
                      padding: "10px 12px", background: "var(--surface)",
                      border: "1px solid var(--border)", borderRadius: 8,
                    }}>
                      <span style={{ fontSize: 20 }}>
                        {allAgents.find((a) => a.id === sub.id)?.icon || "🤖"}
                      </span>
                      <div style={{ flex: 1 }}>
                        <div style={{ fontWeight: 600, fontSize: 13 }}>{sub.name}</div>
                        <input
                          type="text"
                          placeholder="Role (e.g., Business Analyst)"
                          value={sub.role}
                          onChange={(e) => {
                            setDraft((prev) => ({
                              ...prev,
                              subagents: prev.subagents.map((s) =>
                                s.id === sub.id ? { ...s, role: e.target.value } : s
                              ),
                            }));
                          }}
                          className="input"
                          style={{ fontSize: 12, marginTop: 4 }}
                          list="role-suggestions"
                        />
                      </div>
                      <button
                        className="btn ghost"
                        style={{ fontSize: 11 }}
                        onClick={() => removeSubAgent(sub.id)}
                      >
                        Remove
                      </button>
                    </div>
                  ))}
                  <datalist id="role-suggestions">
                    {ROLE_SUGGESTIONS.map((r) => (
                      <option key={r} value={r} />
                    ))}
                  </datalist>
                </div>
              )}

              {/* Add sub-agent from existing agents */}
              {availableForTeam.length > 0 && (
                <div>
                  <label style={{ fontWeight: 600, fontSize: 13, marginBottom: 6, display: "block" }}>
                    Add Existing Agent to Team
                  </label>
                  <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
                    {availableForTeam
                      .filter((a) => !draft.subagents.some((s) => s.id === a.id))
                      .map((agent) => (
                        <div key={agent.id} style={{
                          display: "flex", alignItems: "center", gap: 10,
                          padding: "8px 12px", background: "var(--bg)",
                          border: "1px dashed var(--border)", borderRadius: 8,
                          cursor: "pointer",
                          transition: "border-color 0.15s",
                        }}
                          onClick={() => addSubAgent(agent)}
                        >
                          <span>{agent.icon}</span>
                          <div style={{ flex: 1 }}>
                            <div style={{ fontWeight: 500, fontSize: 13 }}>{agent.name}</div>
                            <div style={{ fontSize: 11, color: "var(--text-tertiary)" }}>
                              {agent.persona.slice(0, 60)}...
                            </div>
                          </div>
                          <span style={{ fontSize: 12, color: "var(--brand)" }}>+ Add</span>
                        </div>
                      ))}
                  </div>
                </div>
              )}

              {availableForTeam.length === 0 && draft.subagents.length === 0 && (
                <div style={{
                  padding: 24, textAlign: "center", color: "var(--text-secondary)",
                  background: "var(--surface)", borderRadius: 8,
                }}>
                  <p>No other agents exist yet. Create more agents first to build a team.</p>
                </div>
              )}

              {/* Team topology visualization */}
              {draft.subagents.length > 0 && (
                <div style={{
                  background: "var(--surface)", border: "1px solid var(--border)",
                  borderRadius: 8, padding: 16,
                }}>
                  <label style={{ fontWeight: 600, fontSize: 13, marginBottom: 8, display: "block" }}>
                    Team Topology
                  </label>
                  <div style={{
                    display: "flex", flexDirection: "column", alignItems: "center", gap: 4,
                    fontFamily: "monospace", fontSize: 12,
                  }}>
                    <div style={{
                      padding: "8px 16px", background: draft.color + "22",
                      border: `2px solid ${draft.color}`, borderRadius: 8,
                      fontWeight: 700,
                    }}>
                      {draft.icon} {draft.name || "Leader"}
                    </div>
                    <div style={{ color: "var(--text-tertiary)" }}>│</div>
                    <div style={{
                      color: "var(--text-tertiary)",
                      letterSpacing: 2,
                    }}>
                      {"─".repeat(Math.max(1, draft.subagents.length * 6))}
                    </div>
                    <div style={{ display: "flex", gap: 12, flexWrap: "wrap", justifyContent: "center" }}>
                      {draft.subagents.map((sub) => {
                        const matched = allAgents.find((a) => a.id === sub.id);
                        return (
                          <div key={sub.id} style={{
                            padding: "6px 12px", background: "var(--bg)",
                            border: "1px solid var(--border)", borderRadius: 6,
                            textAlign: "center",
                          }}>
                            <div>{matched?.icon || "🤖"} {sub.name}</div>
                            {sub.role && (
                              <div style={{ fontSize: 10, color: "var(--text-tertiary)" }}>
                                {sub.role}
                              </div>
                            )}
                          </div>
                        );
                      })}
                    </div>
                  </div>
                </div>
              )}
            </div>
          )}

          {/* ═══ ACTIVITY / OBSERVE TAB ═══ */}
          {tab === "observe" && (
            <AgentActivityPanel
              agentId={existingAgent?.id ?? null}
              agentName={draft.name}
              liveEvents={liveEvents}
              isListening={isListening}
              onToggleListening={() => setIsListening((v) => !v)}
              eventEndRef={eventEndRef}
              pushToast={pushToast}
            />
          )}

          {/* ═══ TOML TAB ═══ */}
          {tab === "toml" && (
            <div style={{ padding: "20px 24px", display: "flex", flexDirection: "column", gap: 16 }}>
              <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
                <div>
                  <h3 style={{ margin: 0, fontSize: 15 }}>Agent TOML Definition</h3>
                  <p style={{ margin: "4px 0 0", fontSize: 12, color: "var(--text-secondary)" }}>
                    Edit the TOML directly, import from file, or export for CLI use.
                  </p>
                </div>
                <div style={{ display: "flex", gap: 6 }}>
                  <button className="btn subtle" style={{ fontSize: 12 }} onClick={handleImportToml}>
                    📥 Import
                  </button>
                  <button className="btn subtle" style={{ fontSize: 12 }} onClick={handleExportToml}>
                    📤 Export
                  </button>
                  <button className="btn primary" style={{ fontSize: 12 }} onClick={handleApplyToml}>
                    Apply to Editor
                  </button>
                </div>
              </div>

              <textarea
                value={tomlContent}
                onChange={(e) => setTomlContent(e.target.value)}
                className="input"
                style={{
                  minHeight: 300, fontFamily: "'SF Mono', 'Fira Code', monospace",
                  fontSize: 12, lineHeight: 1.6, resize: "vertical",
                  background: "var(--bg)", padding: 12,
                }}
                spellCheck={false}
              />

              <div style={{
                fontSize: 11, color: "var(--text-tertiary)", display: "flex", gap: 16,
              }}>
                <span>💡 TOML is compatible with <code>clawdesk agent add --from-toml</code></span>
                <span>Store in <code>~/.clawdesk/agents/&lt;id&gt;/agent.toml</code></span>
              </div>

              <input
                ref={fileInputRef}
                type="file"
                accept=".toml,.txt"
                style={{ display: "none" }}
                onChange={handleFileUpload}
              />
            </div>
          )}
        </div>

        {/* ── Footer ──────────────────────────────────────── */}
        <div style={{
          display: "flex", justifyContent: "space-between", alignItems: "center",
          padding: "12px 24px", borderTop: "1px solid var(--border)",
        }}>
          <div style={{ display: "flex", gap: 8 }}>
            <button className="btn subtle" onClick={handleImportToml} style={{ fontSize: 12 }}>
              📥 Import TOML
            </button>
            <button className="btn subtle" onClick={handleExportToml} style={{ fontSize: 12 }}>
              📤 Export TOML
            </button>
          </div>
          <div style={{ display: "flex", gap: 8 }}>
            <button className="btn" onClick={onClose}>Cancel</button>
            <button
              className="btn primary"
              disabled={isSaving || hasErrors}
              onClick={handleSave}
            >
              {isSaving ? "Saving..." : isEditing ? "Update Agent" : "Create Agent"}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

// ── Live Activity sub-component ─────────────────────────────

interface AgentLiveEvent {
  timestamp: number;
  kind: string;
  detail: string;
  meta?: Record<string, string>;
}

interface AgentActivityPanelProps {
  agentId: string | null;
  agentName: string;
  liveEvents: AgentLiveEvent[];
  isListening: boolean;
  onToggleListening: () => void;
  eventEndRef: React.RefObject<HTMLDivElement>;
  pushToast: (text: string) => void;
}

function AgentActivityPanel({
  agentId,
  agentName,
  liveEvents,
  isListening,
  onToggleListening,
  eventEndRef,
  pushToast,
}: AgentActivityPanelProps) {
  const [traceEntries, setTraceEntries] = useState<any[]>([]);
  const [loadingTrace, setLoadingTrace] = useState(false);

  // Load recent trace/audit entries for this agent
  useEffect(() => {
    if (!agentId) return;
    setLoadingTrace(true);
    api.getAgentTrace()
      .then((entries) => {
        // Filter to entries relevant to this agent
        const filtered = entries.filter(
          (e: any) => e.detail?.includes(agentId) || e.detail?.includes(agentName)
        );
        setTraceEntries(filtered.slice(0, 50));
      })
      .catch(() => setTraceEntries([]))
      .finally(() => setLoadingTrace(false));
  }, [agentId, agentName]);

  return (
    <div style={{ padding: "20px 24px", display: "flex", flexDirection: "column", gap: 16 }}>
      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
        <div>
          <h3 style={{ margin: 0, fontSize: 15 }}>Execution Activity</h3>
          <p style={{ margin: "4px 0 0", fontSize: 12, color: "var(--text-secondary)" }}>
            {agentId
              ? "View recent traces, tool calls, and live execution stream."
              : "Save this agent to see execution activity."}
          </p>
        </div>
        {agentId && (
          <button
            className={`btn ${isListening ? "primary" : "subtle"}`}
            style={{ fontSize: 12 }}
            onClick={onToggleListening}
          >
            {isListening ? "⏸ Pause" : "▶ Live"}
          </button>
        )}
      </div>

      {/* ── Live event stream ─────────────────────── */}
      {isListening && (
        <div style={{
          background: "#0d1117", borderRadius: 8, padding: 12,
          maxHeight: 250, overflowY: "auto", fontFamily: "monospace", fontSize: 11,
        }}>
          {liveEvents.length === 0 && (
            <div style={{ color: "#8b949e", textAlign: "center", padding: 24 }}>
              ⏳ Waiting for agent events...
            </div>
          )}
          {liveEvents.map((ev, i) => (
            <div key={i} style={{
              padding: "3px 0", borderBottom: "1px solid #21262d",
              display: "flex", gap: 8, color: "#c9d1d9",
            }}>
              <span style={{ color: "#8b949e", minWidth: 80 }}>
                {new Date(ev.timestamp).toLocaleTimeString()}
              </span>
              <span style={{
                color: eventKindColor(ev.kind), fontWeight: 600, minWidth: 100,
              }}>
                {ev.kind}
              </span>
              <span style={{ flex: 1 }}>{ev.detail}</span>
            </div>
          ))}
          <div ref={eventEndRef as React.RefObject<HTMLDivElement>} />
        </div>
      )}

      {/* ── Recent trace entries ──────────────────── */}
      {agentId && (
        <div>
          <label style={{ fontWeight: 600, fontSize: 13, marginBottom: 8, display: "block" }}>
            Recent Activity
          </label>
          {loadingTrace ? (
            <div style={{ padding: 16, textAlign: "center", color: "var(--text-secondary)" }}>
              Loading traces...
            </div>
          ) : traceEntries.length === 0 ? (
            <div style={{
              padding: 16, textAlign: "center", color: "var(--text-secondary)",
              background: "var(--surface)", borderRadius: 8,
            }}>
              No activity recorded yet. Start a conversation with this agent.
            </div>
          ) : (
            <div style={{
              display: "flex", flexDirection: "column", gap: 4,
              maxHeight: 300, overflowY: "auto",
            }}>
              {traceEntries.map((entry, i) => (
                <div key={i} style={{
                  display: "flex", gap: 8, padding: "6px 10px",
                  background: "var(--surface)", borderRadius: 6,
                  fontSize: 12, alignItems: "center",
                }}>
                  <span style={{ minWidth: 65, fontSize: 11, color: "var(--text-tertiary)" }}>
                    {entry.timestamp?.slice(11, 19) || ""}
                  </span>
                  <span className="chip" style={{ fontSize: 10 }}>
                    {entry.category || entry.event}
                  </span>
                  <span style={{ flex: 1, color: "var(--text-secondary)" }}>
                    {entry.detail?.slice(0, 100) || entry.event}
                  </span>
                  <span style={{
                    fontSize: 10,
                    color: entry.outcome === "Success" ? "var(--success)" : "var(--error)",
                  }}>
                    {entry.outcome || ""}
                  </span>
                </div>
              ))}
            </div>
          )}
        </div>
      )}

      {/* ── Quick stats ──────────────────────────── */}
      {agentId && (
        <div style={{
          display: "grid", gridTemplateColumns: "repeat(4, 1fr)", gap: 10,
        }}>
          {[
            { label: "Total Events", value: traceEntries.length.toString(), icon: "📊" },
            { label: "Tool Calls", value: traceEntries.filter((e: any) => e.event?.includes("tool")).length.toString(), icon: "🔧" },
            { label: "Errors", value: traceEntries.filter((e: any) => e.outcome === "Failure").length.toString(), icon: "❌" },
            { label: "Live Events", value: liveEvents.length.toString(), icon: "⚡" },
          ].map((stat) => (
            <div key={stat.label} style={{
              background: "var(--surface)", border: "1px solid var(--border)",
              borderRadius: 8, padding: 12, textAlign: "center",
            }}>
              <div style={{ fontSize: 18 }}>{stat.icon}</div>
              <div style={{ fontSize: 18, fontWeight: 700 }}>{stat.value}</div>
              <div style={{ fontSize: 10, color: "var(--text-tertiary)" }}>{stat.label}</div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

function eventKindColor(kind: string): string {
  switch (kind) {
    case "RoundStart": return "#58a6ff";
    case "Response": return "#3fb950";
    case "ToolStart": return "#d29922";
    case "ToolEnd": return "#d29922";
    case "StreamChunk": return "#8b949e";
    case "ThinkingChunk": return "#bc8cff";
    case "Done": return "#3fb950";
    case "Error": return "#f85149";
    default: return "#c9d1d9";
  }
}
