import { useState, useCallback, useMemo, useEffect } from "react";
import * as api from "../api";
import type { SkillDescriptor, SkillTrustInfo, SkillTriggerInfo } from "../types";
import { Icon } from "./Icon";

// ── Types ─────────────────────────────────────────────────────

interface SkillDraft {
  name: string;
  description: string;
  version: string;
  category: string;
  icon: string;
  instructions: string;
  gating: string;           // newline-separated file patterns
  allowedTools: string;      // space-separated tool whitelist
  envRequires: string;       // comma-separated env vars
  mcpRequires: string;       // comma-separated MCP servers
  tags: string;              // comma-separated tags
}

interface ValidationFinding {
  level: "pass" | "warn" | "error";
  message: string;
}

interface TestTriggerResult {
  input: string;
  matched: boolean;
  relevance: number;
}

type DesignerTab = "edit" | "preview" | "validate" | "test";

// ── Defaults ──────────────────────────────────────────────────

const EMPTY_DRAFT: SkillDraft = {
  name: "",
  description: "",
  version: "1.0.0",
  category: "general",
  icon: "⚡",
  instructions: "",
  gating: "",
  allowedTools: "",
  envRequires: "",
  mcpRequires: "",
  tags: "",
};

const CATEGORIES = [
  "general", "coding", "writing", "research", "automation",
  "data", "devops", "security", "communication", "design",
  "core", "openclaw", "channel", "media", "dev",
];

const ICON_OPTIONS = ["⚡", "🔧", "📝", "🔍", "🤖", "📊", "🛠️", "🔒", "📡", "🎨", "💡", "🧪"];

// ── Props ─────────────────────────────────────────────────────

export interface SkillDesignerProps {
  existingSkill?: SkillDescriptor | null;
  onClose: () => void;
  onSaved: () => void;
  pushToast: (text: string) => void;
}

// ── Component ─────────────────────────────────────────────────

export function SkillDesigner({
  existingSkill,
  onClose,
  onSaved,
  pushToast,
}: SkillDesignerProps) {
  const isEditing = !!existingSkill;

  // Build dynamic categories list — include existence skill's category if not in defaults
  const categories = useMemo(() => {
    if (existingSkill && !CATEGORIES.includes(existingSkill.category)) {
      return [existingSkill.category, ...CATEGORIES];
    }
    return CATEGORIES;
  }, [existingSkill]);

  const [tab, setTab] = useState<DesignerTab>("edit");
  const [draft, setDraft] = useState<SkillDraft>(() => {
    if (existingSkill) {
      return {
        name: existingSkill.name,
        description: existingSkill.description,
        version: "1.0.0",
        category: existingSkill.category,
        icon: existingSkill.icon || "⚡",
        instructions: "",
        gating: "",
        allowedTools: "",
        envRequires: "",
        mcpRequires: "",
        tags: "",
      };
    }
    return { ...EMPTY_DRAFT };
  });
  const [findings, setFindings] = useState<ValidationFinding[]>([]);
  const [testResults, setTestResults] = useState<TestTriggerResult[]>([]);
  const [testInput, setTestInput] = useState("");
  const [isSaving, setIsSaving] = useState(false);
  const [isValidating, setIsValidating] = useState(false);
  const [isTesting, setIsTesting] = useState(false);
  const [isLoading, setIsLoading] = useState(false);

  // Load full skill detail (instructions, tags, tools) from backend when editing
  useEffect(() => {
    if (!existingSkill) return;
    setIsLoading(true);
    api.getSkillDetail(existingSkill.id).then((detail) => {
      setDraft((prev) => ({
        ...prev,
        description: detail.description || prev.description,
        version: detail.version || prev.version,
        instructions: detail.instructions || "",
        tags: detail.tags.join(", "),
        allowedTools: detail.required_tools.join(" "),
      }));
    }).catch(() => {
      // Backend detail unavailable — keep existing draft
    }).finally(() => setIsLoading(false));
  }, [existingSkill]);

  const patch = useCallback(
    (key: keyof SkillDraft, value: string) =>
      setDraft((prev) => ({ ...prev, [key]: value })),
    []
  );

  // ── SKILL.md generation ───────────────────────────────────

  const skillMd = useMemo(() => {
    const lines: string[] = ["---"];
    lines.push(`name: ${draft.name || "untitled-skill"}`);
    lines.push(`description: ${JSON.stringify(draft.description)}`);
    lines.push(`version: ${draft.version}`);
    lines.push(`category: ${draft.category}`);
    if (draft.tags.trim()) {
      lines.push(`tags: [${draft.tags.split(",").map((t) => t.trim()).filter(Boolean).join(", ")}]`);
    }
    if (draft.envRequires.trim()) {
      lines.push("requires:");
      lines.push(`  env: [${draft.envRequires.split(",").map((e) => e.trim()).filter(Boolean).join(", ")}]`);
    }
    if (draft.mcpRequires.trim()) {
      if (!draft.envRequires.trim()) lines.push("requires:");
      lines.push(`  mcp: [${draft.mcpRequires.split(",").map((m) => m.trim()).filter(Boolean).join(", ")}]`);
    }
    if (draft.gating.trim()) {
      lines.push("gating:");
      draft.gating.split("\n").filter(Boolean).forEach((pattern) => {
        lines.push(`  - file_pattern: "${pattern.trim()}"`);
      });
    }
    if (draft.allowedTools.trim()) {
      lines.push(`allowed-tools: ${draft.allowedTools.trim()}`);
    }
    lines.push("---");
    lines.push("");
    lines.push(draft.instructions || "# Instructions\n\nDescribe what the skill does here.");
    return lines.join("\n");
  }, [draft]);

  // ── Validation ────────────────────────────────────────────

  const runValidation = useCallback(async () => {
    setIsValidating(true);
    const results: ValidationFinding[] = [];

    // Name validation (Agent Skills spec: 1-64 chars, lowercase alphanum + hyphens)
    if (!draft.name.trim()) {
      results.push({ level: "error", message: "Skill name is required." });
    } else if (!/^[a-z0-9][a-z0-9-]{0,63}$/.test(draft.name)) {
      results.push({ level: "error", message: "Name must be 1-64 chars, lowercase alphanumeric + hyphens, no leading hyphen." });
    } else {
      results.push({ level: "pass", message: "Name format valid." });
    }

    // Description
    if (!draft.description.trim()) {
      results.push({ level: "error", message: "Description is required." });
    } else if (draft.description.length > 1024) {
      results.push({ level: "warn", message: `Description is ${draft.description.length} chars. Recommend ≤ 1024.` });
    } else {
      results.push({ level: "pass", message: "Description valid." });
    }

    // Version
    if (!/^\d+\.\d+\.\d+$/.test(draft.version)) {
      results.push({ level: "error", message: "Version must be semver (e.g. 1.0.0)." });
    } else {
      results.push({ level: "pass", message: "Version valid." });
    }

    // Instructions (progressive disclosure)
    if (!draft.instructions.trim()) {
      results.push({ level: "warn", message: "Instructions are empty. Add instructions for the agent." });
    } else if (draft.instructions.length < 20) {
      results.push({ level: "warn", message: "Instructions very short. Consider adding more detail." });
    } else {
      results.push({ level: "pass", message: "Instructions present." });
    }

    // Suspicious content detection (prompt injection patterns)
    const suspiciousPatterns = [
      /ignore previous instructions/i,
      /forget everything/i,
      /you are now/i,
      /\beval\s*\(/,
      /rm\s+-rf/,
      /\bexec\s*\(/,
      /system\s*\(/,
    ];
    const allContent = `${draft.instructions} ${draft.description}`;
    const suspicious = suspiciousPatterns.filter((p) => p.test(allContent));
    if (suspicious.length > 0) {
      results.push({ level: "error", message: `Suspicious content detected: potential prompt injection pattern (${suspicious.length} match${suspicious.length > 1 ? "es" : ""}).` });
    } else {
      results.push({ level: "pass", message: "No prompt injection patterns detected." });
    }

    // Allowed tools check
    if (draft.allowedTools.trim()) {
      const tools = draft.allowedTools.split(/\s+/).filter(Boolean);
      if (tools.length > 50) {
        results.push({ level: "warn", message: `${tools.length} allowed tools. Consider restricting to minimize attack surface.` });
      } else {
        results.push({ level: "pass", message: `${tools.length} allowed tool(s) defined.` });
      }
    } else {
      results.push({ level: "warn", message: "No allowed-tools defined. Skill will use agent defaults." });
    }

    // Token estimation (client-side)
    const estimatedTokens = Math.ceil(skillMd.length / 4);
    if (estimatedTokens > 8000) {
      results.push({ level: "warn", message: `Estimated ${estimatedTokens} tokens. Large skills increase context cost.` });
    } else {
      results.push({ level: "pass", message: `Estimated ${estimatedTokens} tokens.` });
    }

    // Backend validation — run SKILL.md through parse_skill_md + adapt_skill pipeline
    try {
      const backendResult = await api.validateSkillMd(skillMd);
      if (backendResult.valid) {
        results.push({ level: "pass", message: `Backend adapter pipeline: OK (${backendResult.estimated_tokens} tokens).` });
      }
      for (const err of backendResult.errors) {
        results.push({ level: "error", message: `Backend: ${err}` });
      }
      for (const warn of backendResult.warnings) {
        results.push({ level: "warn", message: `Backend: ${warn}` });
      }
    } catch {
      results.push({ level: "warn", message: "Backend validation unavailable." });
    }

    // Trust check for existing skills
    if (existingSkill) {
      try {
        const trust = await api.getSkillTrustLevel(existingSkill.id);
        results.push({
          level: trust.verified ? "pass" : "warn",
          message: `Trust level: ${trust.trust_level}${trust.verified ? " (verified)" : ""}`,
        });
      } catch {
        // no backend validation available
      }
    }

    setFindings(results);
    setIsValidating(false);
  }, [draft, skillMd, existingSkill]);

  // ── Trigger testing ───────────────────────────────────────

  const runTriggerTest = useCallback(async () => {
    if (!testInput.trim()) return;
    setIsTesting(true);
    try {
      const triggers = await api.evaluateSkillTriggers(testInput);
      const match = triggers.find((t) => t.skill_id === existingSkill?.id);
      setTestResults((prev) => [
        {
          input: testInput,
          matched: !!match,
          relevance: match?.relevance ?? 0,
        },
        ...prev,
      ]);
      setTestInput("");
    } catch {
      pushToast("Trigger evaluation unavailable.");
    }
    setIsTesting(false);
  }, [testInput, existingSkill, pushToast]);

  // ── Save ──────────────────────────────────────────────────

  const handleSave = useCallback(async () => {
    setIsSaving(true);
    try {
      await api.registerSkill({
        name: draft.name,
        description: draft.description,
        version: draft.version,
        category: draft.category,
        instructions: draft.instructions,
        tags: draft.tags.split(",").map((t) => t.trim()).filter(Boolean),
        allowed_tools: draft.allowedTools.split(/\s+/).filter(Boolean),
        existing_id: existingSkill?.id ?? undefined,
      });
      pushToast(existingSkill ? `Skill "${draft.name}" updated and redeployed.` : `Skill "${draft.name}" created and activated.`);
      onSaved();
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      pushToast(`Failed to save skill: ${msg}`);
    }
    setIsSaving(false);
  }, [draft, existingSkill, onSaved, pushToast]);

  // ── Render ────────────────────────────────────────────────

  const errorCount = findings.filter((f) => f.level === "error").length;
  const warnCount = findings.filter((f) => f.level === "warn").length;
  const passCount = findings.filter((f) => f.level === "pass").length;

  return (
    <div className="skill-designer-overlay">
      <div className="skill-designer">
        {/* Header */}
        <div className="skill-designer-header">
          <div className="skill-designer-header-left">
            <span className="skill-designer-icon">{draft.icon}</span>
            <h2>{existingSkill ? "Edit Skill" : "Design New Skill"}</h2>
          </div>
          <div className="skill-designer-header-right">
            <button className="btn primary" disabled={isSaving || !draft.name.trim()} onClick={handleSave}>
              {isSaving ? "Saving..." : isEditing ? "Save & Redeploy" : "Save"}
            </button>
            <button className="btn ghost" onClick={onClose}>✕</button>
          </div>
        </div>

        {/* Tabs */}
        <div className="skill-designer-tabs">
          {(["edit", "preview", "validate", "test"] as DesignerTab[]).map((t) => (
            <button
              key={t}
              className={`skill-designer-tab${tab === t ? " active" : ""}`}
              onClick={() => {
                setTab(t);
                if (t === "validate" && findings.length === 0) runValidation();
              }}
            >
              {t === "edit" && "✏️ Editor"}
              {t === "preview" && "👁 Preview"}
              {t === "validate" && `🛡️ Validate${findings.length > 0 ? ` (${errorCount}E/${warnCount}W)` : ""}`}
              {t === "test" && "🧪 Test"}
            </button>
          ))}
        </div>

        {/* Content */}
        <div className="skill-designer-body">
          {isLoading && (
            <div style={{ textAlign: "center", padding: 20, color: "#888" }}>Loading skill details...</div>
          )}
          {/* ── Editor Tab ── */}
          {tab === "edit" && !isLoading && (
            <div className="skill-editor-form">
              <div className="skill-editor-row">
                <div className="skill-editor-field" style={{ flex: 1 }}>
                  <label className="field-label">Skill Name</label>
                  <input
                    className="input"
                    placeholder="my-awesome-skill"
                    value={draft.name}
                    readOnly={isEditing}
                    onChange={(e) => isEditing ? undefined : patch("name", e.target.value.toLowerCase().replace(/[^a-z0-9-]/g, "-"))}
                  />
                  <span className="field-hint">{isEditing ? "Read-only when editing" : " "}</span>
                </div>
                <div className="skill-editor-field" style={{ width: 130 }}>
                  <label className="field-label">Version</label>
                  <input className="input" value={draft.version} onChange={(e) => patch("version", e.target.value)} />
                  <span className="field-hint"> </span>
                </div>
                <div className="skill-editor-field" style={{ width: 90 }}>
                  <label className="field-label">Icon</label>
                  <select className="input" value={draft.icon} onChange={(e) => patch("icon", e.target.value)}>
                    {ICON_OPTIONS.map((ic) => <option key={ic} value={ic}>{ic}</option>)}
                  </select>
                  <span className="field-hint"> </span>
                </div>
              </div>

              <div className="skill-editor-field">
                <label className="field-label">Description</label>
                <input
                  className="input"
                  placeholder="What does this skill do?"
                  value={draft.description}
                  onChange={(e) => patch("description", e.target.value)}
                />
              </div>

              <div className="skill-editor-row">
                <div className="skill-editor-field" style={{ flex: 1 }}>
                  <label className="field-label">Category</label>
                  <select className="input" value={draft.category} onChange={(e) => patch("category", e.target.value)}>
                    {categories.map((c) => <option key={c} value={c}>{c}</option>)}
                  </select>
                </div>
                <div className="skill-editor-field" style={{ flex: 1 }}>
                  <label className="field-label">Tags</label>
                  <input
                    className="input"
                    placeholder="frontend, react (comma-separated)"
                    value={draft.tags}
                    onChange={(e) => patch("tags", e.target.value)}
                  />
                </div>
              </div>

              <div className="skill-editor-field">
                <label className="field-label">Instructions</label>
                <textarea
                  className="input skill-editor-textarea"
                  placeholder="# What this skill does&#10;&#10;Describe the skill's behavior, constraints, and examples..."
                  value={draft.instructions}
                  onChange={(e) => patch("instructions", e.target.value)}
                  rows={10}
                />
                <span className="field-hint">markdown body</span>
              </div>

              <details className="skill-editor-advanced">
                <summary>Advanced Settings</summary>
                <div className="skill-editor-advanced-body">
                  <div className="skill-editor-field">
                    <label className="field-label">Gating Patterns <span className="field-hint">one per line — file patterns that activate this skill</span></label>
                    <textarea
                      className="input"
                      placeholder="*.tsx&#10;src/components/**"
                      value={draft.gating}
                      onChange={(e) => patch("gating", e.target.value)}
                      rows={3}
                    />
                  </div>
                  <div className="skill-editor-field">
                    <label className="field-label">Allowed Tools <span className="field-hint">space-separated whitelist</span></label>
                    <input
                      className="input"
                      placeholder="read_file write_file run_command"
                      value={draft.allowedTools}
                      onChange={(e) => patch("allowedTools", e.target.value)}
                    />
                  </div>
                  <div className="skill-editor-row">
                    <div className="skill-editor-field" style={{ flex: 1 }}>
                      <label className="field-label">Environment Requires <span className="field-hint">comma-separated</span></label>
                      <input
                        className="input"
                        placeholder="API_KEY, DATABASE_URL"
                        value={draft.envRequires}
                        onChange={(e) => patch("envRequires", e.target.value)}
                      />
                    </div>
                    <div className="skill-editor-field" style={{ flex: 1 }}>
                      <label className="field-label">MCP Requires <span className="field-hint">comma-separated</span></label>
                      <input
                        className="input"
                        placeholder="sentry-mcp, github-mcp"
                        value={draft.mcpRequires}
                        onChange={(e) => patch("mcpRequires", e.target.value)}
                      />
                    </div>
                  </div>
                </div>
              </details>
            </div>
          )}

          {/* ── Preview Tab ── */}
          {tab === "preview" && (
            <div className="skill-preview">
              <div className="skill-preview-header">
                <h3>Generated SKILL.md</h3>
                <button
                  className="btn subtle"
                  onClick={() => {
                    navigator.clipboard.writeText(skillMd).then(
                      () => pushToast("SKILL.md copied to clipboard."),
                      () => pushToast("Copy failed.")
                    );
                  }}
                >
                  📋 Copy
                </button>
              </div>
              <pre className="skill-preview-code">{skillMd}</pre>
              <div className="skill-preview-stats">
                <span>~{Math.ceil(skillMd.length / 4)} tokens</span>
                <span>{skillMd.split("\n").length} lines</span>
                <span>{skillMd.length} chars</span>
              </div>
            </div>
          )}

          {/* ── Validate Tab ── */}
          {tab === "validate" && (
            <div className="skill-validate">
              <div className="skill-validate-header">
                <h3>Validation Report</h3>
                <button className="btn subtle" onClick={runValidation} disabled={isValidating}>
                  {isValidating ? "Checking..." : "🔄 Re-validate"}
                </button>
              </div>
              {findings.length > 0 && (
                <div className="skill-validate-summary">
                  <span className="validate-badge validate-pass">{passCount} pass</span>
                  <span className="validate-badge validate-warn">{warnCount} warn</span>
                  <span className="validate-badge validate-error">{errorCount} error</span>
                </div>
              )}
              <div className="skill-validate-findings">
                {findings.map((f, i) => (
                  <div key={i} className={`validate-finding validate-finding-${f.level}`}>
                    <span className="validate-finding-icon">
                      {f.level === "pass" ? "✅" : f.level === "warn" ? "⚠️" : "❌"}
                    </span>
                    <span>{f.message}</span>
                  </div>
                ))}
                {findings.length === 0 && (
                  <div className="empty-state centered">
                    <p>Click Re-validate to check the skill.</p>
                  </div>
                )}
              </div>
            </div>
          )}

          {/* ── Test Tab ── */}
          {tab === "test" && (
            <div className="skill-test">
              <div className="skill-test-header">
                <h3>Trigger Testing</h3>
                <p className="settings-desc">
                  Test whether user messages would trigger this skill's gating rules.
                </p>
              </div>
              <div className="skill-test-input-row">
                <input
                  className="input"
                  style={{ flex: 1 }}
                  placeholder="Enter a test message..."
                  value={testInput}
                  onChange={(e) => setTestInput(e.target.value)}
                  onKeyDown={(e) => e.key === "Enter" && runTriggerTest()}
                />
                <button className="btn primary" disabled={isTesting || !testInput.trim()} onClick={runTriggerTest}>
                  {isTesting ? "Testing..." : "Test"}
                </button>
              </div>
              <div className="skill-test-results">
                {testResults.map((r, i) => (
                  <div key={i} className={`skill-test-result ${r.matched ? "matched" : "no-match"}`}>
                    <span className="skill-test-result-icon">{r.matched ? "✅" : "❌"}</span>
                    <span className="skill-test-result-input">"{r.input}"</span>
                    <span className="skill-test-result-score">
                      {r.matched ? `${(r.relevance * 100).toFixed(0)}% relevance` : "No match"}
                    </span>
                  </div>
                ))}
                {testResults.length === 0 && (
                  <div className="empty-state centered">
                    <p>No tests run yet. Enter a message above to test trigger matching.</p>
                  </div>
                )}
              </div>
              {!existingSkill && (
                <div className="info-banner">
                  💡 Save and install the skill first to test triggers against the backend.
                </div>
              )}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
