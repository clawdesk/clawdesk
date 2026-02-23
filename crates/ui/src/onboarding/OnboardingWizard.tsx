import { useCallback, useEffect, useMemo, useState } from "react";
import type { HealthResponse, SkillDescriptor, ChannelInfo, ChannelTypeSpec } from "../types";
import { AGENT_TEMPLATES } from "../types";
import { Modal } from "../components/Modal";
import { ChannelSetupJourney } from "./ChannelSetupJourney";
import * as api from "../api";

// Provider → available models mapping
const PROVIDER_MODELS: Record<string, { id: string; label: string }[]> = {
  Anthropic: [
    { id: "claude-sonnet-4-20250514", label: "Claude Sonnet 4 (Recommended)" },
    { id: "claude-opus-4-20250514", label: "Claude Opus 4" },
    { id: "claude-haiku-3-20250307", label: "Claude Haiku 3.5 (Fast)" },
  ],
  OpenAI: [
    { id: "gpt-4o", label: "GPT-4o (Recommended)" },
    { id: "gpt-4o-mini", label: "GPT-4o Mini (Fast)" },
    { id: "o3", label: "o3 (Reasoning)" },
  ],
  Google: [
    { id: "gemini-2.5-pro", label: "Gemini 2.5 Pro (Recommended)" },
    { id: "gemini-2.5-flash", label: "Gemini 2.5 Flash (Fast)" },
  ],
  "Azure OpenAI": [
    { id: "gpt-4o", label: "GPT-4o (Azure)" },
    { id: "gpt-4o-mini", label: "GPT-4o Mini (Azure)" },
  ],
  "Cohere": [
    { id: "command-r-plus-08-2024", label: "Command R+ (08-2024)" },
    { id: "command-r-08-2024", label: "Command R (08-2024)" },
  ],
  "Vertex AI": [
    { id: "gemini-2.5-pro", label: "Gemini 2.5 Pro (Vertex)" },
    { id: "gemini-2.5-flash", label: "Gemini 2.5 Flash (Vertex)" },
  ],
  "Ollama (Local)": [
    { id: "lfm2.5-thinking:latest", label: "LFM 2.5 Thinking (Recommended)" },
    { id: "llama3", label: "Llama 3" },
    { id: "mistral", label: "Mistral" },
    { id: "codellama", label: "Code Llama" },
    { id: "deepseek-r1", label: "DeepSeek R1" },
  ],
  "Local (OpenAI Compatible)": [
    { id: "default", label: "Default Model" },
  ],
};

export { PROVIDER_MODELS };

export interface OnboardingResult {
  provider: string;
  model: string;
  apiKey: string;
  templateName: string;
  /** Whether the key was stored in the OS vault. */
  storedInVault: boolean;
  /** Skill IDs the user toggled on during onboarding. */
  enabledSkills: string[];
  /** Channel configs the user set up during onboarding. */
  channelSetups: { channelId: string; config: Record<string, string> }[];
}

/** Validate that an API key has the expected format for a provider. */
function validateApiKeyFormat(provider: string, key: string): { valid: boolean; hint: string } {
  const trimmed = key.trim();
  if (!trimmed) return { valid: false, hint: "" };

  switch (provider) {
    case "Anthropic":
      if (trimmed.startsWith("sk-ant-")) return { valid: true, hint: "✓ Valid Anthropic key format" };
      return { valid: false, hint: "Anthropic keys start with sk-ant-" };
    case "OpenAI":
      if (trimmed.startsWith("sk-")) return { valid: true, hint: "✓ Valid OpenAI key format" };
      return { valid: false, hint: "OpenAI keys start with sk-" };
    case "Google":
      if (trimmed.startsWith("AIza")) return { valid: true, hint: "✓ Valid Google AI key format" };
      return { valid: false, hint: "Google AI keys start with AIza" };
    case "Azure OpenAI":
      if (/^[a-f0-9]{32}$/.test(trimmed)) return { valid: true, hint: "✓ Valid Azure key format" };
      return { valid: false, hint: "Azure keys are 32-char hex strings" };
    case "Cohere":
      if (trimmed.length >= 20) return { valid: true, hint: "✓ Key format accepted" };
      return { valid: false, hint: "Key seems too short" };
    default:
      return { valid: trimmed.length >= 10, hint: trimmed.length >= 10 ? "✓ Key accepted" : "Key seems too short" };
  }
}

// ── Category display helpers ──────────────────────────────────

const SKILL_CATEGORY_ICONS: Record<string, string> = {
  code: "⚡", research: "🔬", writing: "📝", productivity: "🎯",
  communication: "💬", data: "📊", media: "🎨", system: "🔧",
};

const POPULAR_CHANNELS = ["Telegram", "Discord", "Slack", "WhatsApp", "Email", "IMessage", "Irc"];

export function OnboardingWizard({
  open,
  health,
  skills: externalSkills,
  channels: externalChannels,
  onComplete,
}: {
  open: boolean;
  health: HealthResponse | null;
  skills: SkillDescriptor[];
  channels: ChannelInfo[];
  onComplete: (result: OnboardingResult) => void;
}) {
  // ── Step 1: Provider ──────────────────────────────────────
  const [step, setStep] = useState(1);
  const [provider, setProvider] = useState("Ollama (Local)");
  const [model, setModel] = useState(PROVIDER_MODELS["Ollama (Local)"][0].id);
  const [apiKey, setApiKey] = useState("");
  const [templateName, setTemplateName] = useState(AGENT_TEMPLATES[0].name);
  const [storeInVault, setStoreInVault] = useState(true);
  const [validating, setValidating] = useState(false);
  const [validationResult, setValidationResult] = useState<{ ok: boolean; message: string } | null>(null);

  // ── Step 2: Skills ────────────────────────────────────────
  const [enabledSkills, setEnabledSkills] = useState<Set<string>>(new Set());
  const [skillSearch, setSkillSearch] = useState("");

  // ── Step 3: Channels ──────────────────────────────────────
  const [channelSetups, setChannelSetups] = useState<Map<string, Record<string, string>>>(new Map());
  const [configuringChannel, setConfiguringChannel] = useState<ChannelInfo | null>(null);
  const [typeSpecs, setTypeSpecs] = useState<ChannelTypeSpec[]>([]);

  // Load channel type specs on mount
  useEffect(() => {
    api.getChannelTypes().then(setTypeSpecs).catch(() => { });
  }, []);

  // Pre-select verified/builtin skills on first load
  useEffect(() => {
    if (externalSkills.length > 0 && enabledSkills.size === 0) {
      const builtins = new Set(externalSkills.filter((s) => s.verified).map((s) => s.id));
      setEnabledSkills(builtins);
    }
  }, [externalSkills]); // eslint-disable-line react-hooks/exhaustive-deps

  const localProvider = provider.toLowerCase().includes("ollama");

  const keyValidation = useMemo(() => {
    if (localProvider || !apiKey.trim()) return null;
    return validateApiKeyFormat(provider, apiKey);
  }, [provider, apiKey, localProvider]);

  const handleTestConnection = useCallback(async () => {
    if (!apiKey.trim() || localProvider) return;
    setValidating(true);
    setValidationResult(null);
    try {
      // @ts-ignore — Tauri IPC
      await window.__TAURI__?.invoke("test_llm_connection", {
        provider: provider.toLowerCase().replace(/\s+/g, "_"),
        apiKey: apiKey.trim(),
        model,
      });
      setValidationResult({ ok: true, message: "✓ Connection successful" });
    } catch (e: unknown) {
      const message = e instanceof Error ? e.message : String(e);
      setValidationResult({ ok: false, message: `✗ ${message}` });
    } finally {
      setValidating(false);
    }
  }, [apiKey, provider, model, localProvider]);

  const toggleSkill = useCallback((skillId: string) => {
    setEnabledSkills((prev) => {
      const next = new Set(prev);
      if (next.has(skillId)) next.delete(skillId);
      else next.add(skillId);
      return next;
    });
  }, []);

  const filteredSkills = useMemo(() => {
    const query = skillSearch.trim().toLowerCase();
    if (!query) return externalSkills;
    return externalSkills.filter(
      (s) => s.name.toLowerCase().includes(query) ||
        s.description.toLowerCase().includes(query) ||
        s.category.toLowerCase().includes(query)
    );
  }, [externalSkills, skillSearch]);

  // Group skills by category
  const skillsByCategory = useMemo(() => {
    const map = new Map<string, SkillDescriptor[]>();
    for (const s of filteredSkills) {
      const cat = s.category || "other";
      if (!map.has(cat)) map.set(cat, []);
      map.get(cat)!.push(s);
    }
    return map;
  }, [filteredSkills]);

  // Available channels (exclude already-active ones like WebChat/Internal)
  const availableChannels = useMemo(() => {
    return externalChannels.filter(
      (ch) => ch.channel_type !== "WebChat" && ch.channel_type !== "Internal"
    );
  }, [externalChannels]);

  const specFor = useCallback(
    (ch: ChannelInfo) => typeSpecs.find((ts) => ts.id === ch.channel_type),
    [typeSpecs]
  );

  // Sort channels: popular ones first
  const sortedChannels = useMemo(() => {
    return [...availableChannels].sort((a, b) => {
      const aPopular = POPULAR_CHANNELS.indexOf(a.channel_type);
      const bPopular = POPULAR_CHANNELS.indexOf(b.channel_type);
      if (aPopular !== -1 && bPopular !== -1) return aPopular - bPopular;
      if (aPopular !== -1) return -1;
      if (bPopular !== -1) return 1;
      return a.name.localeCompare(b.name);
    });
  }, [availableChannels]);

  const openChannelConfig = useCallback((ch: ChannelInfo) => {
    setConfiguringChannel(ch);
  }, []);

  const removeChannelSetup = useCallback((channelId: string) => {
    setChannelSetups((prev) => {
      const next = new Map(prev);
      next.delete(channelId);
      return next;
    });
  }, []);

  // Reset state when wizard opens fresh
  useEffect(() => {
    if (open) {
      setStep(1);
      setValidationResult(null);
      setConfiguringChannel(null);
    }
  }, [open]);

  if (!open) return null;

  const canAdvanceFromProvider = localProvider || apiKey.trim().length > 0;
  const configuredChannelCount = channelSetups.size;
  const spec = configuringChannel ? specFor(configuringChannel) : null;

  return (
    <Modal title="Welcome to ClawDesk" onClose={() => undefined}>
      <div className="modal-stack onboarding">
        {/* ── Step indicators ── */}
        <div className="wizard-steps onboarding-steps">
          <span className={`${step === 1 ? "active" : ""} ${step > 1 ? "done" : ""}`}>
            1. Provider
          </span>
          <span className={`${step === 2 ? "active" : ""} ${step > 2 ? "done" : ""}`}>
            2. Skills
          </span>
          <span className={step === 3 ? "active" : ""}>
            3. Channels
          </span>
        </div>

        {/* ═══════════════════════════════════════════════════════
            STEP 1 — Provider Setup (required)
            ═══════════════════════════════════════════════════════ */}
        {step === 1 && (
          <section className="section-card onboarding-step">
            <div className="onboarding-brand">
              <img src="/logo.svg" alt="ClawDesk logo" className="onboarding-logo" />
              <div>
                <h3>Set up your AI provider</h3>
                <p>Connect to a cloud provider or use a local model with Ollama.</p>
              </div>
            </div>

            <label className="field-label">
              Provider
              <select value={provider} onChange={(event) => {
                const p = event.target.value;
                setProvider(p);
                const models = PROVIDER_MODELS[p] || [];
                if (models.length > 0) setModel(models[0].id);
                setApiKey("");
                setValidationResult(null);
              }}>
                <option>Anthropic</option>
                <option>OpenAI</option>
                <option>Google</option>
                <option>Azure OpenAI</option>
                <option>Cohere</option>
                <option>Vertex AI</option>
                <option>Ollama (Local)</option>
              </select>
            </label>

            <label className="field-label">
              Model
              <input
                type="text"
                list="onboarding-models"
                value={model}
                onChange={(event) => setModel(event.target.value)}
                placeholder="Type or select a model"
              />
              <datalist id="onboarding-models">
                {(PROVIDER_MODELS[provider] || []).map((m) => (
                  <option key={m.id} value={m.id}>{m.label}</option>
                ))}
              </datalist>
            </label>

            <label className="field-label">
              API key
              <input
                type="password"
                value={apiKey}
                placeholder={localProvider ? "Not required for local Ollama" : "Paste your API key"}
                onChange={(event) => setApiKey(event.target.value)}
                disabled={localProvider}
              />
            </label>

            {localProvider ? (
              <p className="onboarding-hint">
                ⓘ Local mode — make sure <code>ollama serve</code> is running on this machine.
              </p>
            ) : (
              <>
                {keyValidation && (
                  <p className={keyValidation.valid ? "text-success" : "text-warning"}>
                    {keyValidation.hint}
                  </p>
                )}
                <div className="row-actions" style={{ gap: "0.5rem", marginTop: "0.5rem" }}>
                  <button
                    className="btn ghost"
                    onClick={handleTestConnection}
                    disabled={validating || !apiKey.trim() || (keyValidation !== null && !keyValidation.valid)}
                  >
                    {validating ? "Testing..." : "Test Connection"}
                  </button>
                  {validationResult && (
                    <span className={validationResult.ok ? "text-success" : "text-error"}>
                      {validationResult.message}
                    </span>
                  )}
                </div>
                <label className="field-label" style={{ marginTop: "0.75rem" }}>
                  <input
                    type="checkbox"
                    checked={storeInVault}
                    onChange={(event) => setStoreInVault(event.target.checked)}
                  />{" "}
                  Store securely in OS keychain
                  <span className="row-sub" style={{ display: "block", marginTop: "0.25rem" }}>
                    Uses macOS Keychain, Windows Credential Manager, or Linux Secret Service
                  </span>
                </label>
              </>
            )}

            {/* Assistant template picker */}
            <div style={{ marginTop: "1rem" }}>
              <div className="field-label" style={{ marginBottom: "0.5rem" }}>Default assistant style</div>
              <div className="template-grid">
                {AGENT_TEMPLATES.map((template) => (
                  <button
                    key={template.name}
                    className={`template-tile ${templateName === template.name ? "selected" : ""}`}
                    onClick={() => setTemplateName(template.name)}
                  >
                    <div className="row-title">{template.icon} {template.name}</div>
                    <div className="row-sub">{template.description}</div>
                  </button>
                ))}
              </div>
            </div>

            {health && (
              <div className="onboarding-engine-status">
                <span className="status-dot status-ok" /> Engine connected (v{health.version})
              </div>
            )}
          </section>
        )}

        {/* ═══════════════════════════════════════════════════════
            STEP 2 — Skills (optional)
            ═══════════════════════════════════════════════════════ */}
        {step === 2 && (
          <section className="section-card onboarding-step">
            <div className="onboarding-step-header">
              <div>
                <h3>Choose your skills</h3>
                <p>Skills give your agent capabilities. Built-in skills are pre-selected. Toggle any you'd like.</p>
              </div>
              <span className="chip">{enabledSkills.size} selected</span>
            </div>

            <div className="onboarding-skill-search">
              <input
                type="text"
                placeholder="Search skills..."
                value={skillSearch}
                onChange={(e) => setSkillSearch(e.target.value)}
              />
              <div className="onboarding-skill-actions">
                <button
                  className="btn ghost"
                  onClick={() => setEnabledSkills(new Set(externalSkills.map((s) => s.id)))}
                >
                  Select all
                </button>
                <button
                  className="btn ghost"
                  onClick={() => setEnabledSkills(new Set())}
                >
                  Clear all
                </button>
              </div>
            </div>

            <div className="onboarding-skill-list">
              {externalSkills.length === 0 ? (
                <p className="onboarding-hint">No skills available yet. Skills will load once the engine is ready.</p>
              ) : filteredSkills.length === 0 ? (
                <p className="onboarding-hint">No skills match "{skillSearch}"</p>
              ) : (
                Array.from(skillsByCategory.entries()).map(([category, skills]) => (
                  <div key={category} className="onboarding-skill-category">
                    <div className="onboarding-skill-category-label">
                      {SKILL_CATEGORY_ICONS[category] ?? "📦"} {category}
                    </div>
                    <div className="onboarding-skill-grid">
                      {skills.map((skill) => (
                        <button
                          key={skill.id}
                          className={`onboarding-skill-card ${enabledSkills.has(skill.id) ? "selected" : ""}`}
                          onClick={() => toggleSkill(skill.id)}
                        >
                          <div className="onboarding-skill-card-header">
                            <span className="onboarding-skill-icon">{skill.icon || "📦"}</span>
                            <span className="onboarding-skill-name">{skill.name}</span>
                            {skill.verified && <span className="chip chip-sm" title="Built-in">✓</span>}
                          </div>
                          <div className="onboarding-skill-desc">{skill.description}</div>
                          <div className="onboarding-skill-toggle">
                            <span className={`toggle-indicator ${enabledSkills.has(skill.id) ? "on" : "off"}`} />
                          </div>
                        </button>
                      ))}
                    </div>
                  </div>
                ))
              )}
            </div>

            <p className="onboarding-hint">
              ⓘ You can change skills anytime from the <strong>Skills</strong> page.
            </p>
          </section>
        )}

        {/* ═══════════════════════════════════════════════════════
            STEP 3 — Channel Setup (optional)
            ═══════════════════════════════════════════════════════ */}
        {step === 3 && !configuringChannel && (
          <section className="section-card onboarding-step">
            <div className="onboarding-step-header">
              <div>
                <h3>Connect channels</h3>
                <p>Let your agent respond on messaging platforms. This is optional — you can set up channels later in Settings.</p>
              </div>
              {configuredChannelCount > 0 && (
                <span className="chip">{configuredChannelCount} configured</span>
              )}
            </div>

            {/* Channels the user has already configured in this session */}
            {configuredChannelCount > 0 && (
              <div className="onboarding-channel-configured">
                <div className="settings-group-label">Ready to connect</div>
                <div className="onboarding-channel-grid">
                  {Array.from(channelSetups.entries()).map(([chId]) => {
                    const ch = availableChannels.find((c) => c.id === chId);
                    const ts = ch ? specFor(ch) : null;
                    if (!ch) return null;
                    return (
                      <div key={chId} className="onboarding-channel-card configured">
                        <span className="onboarding-channel-icon">{ts?.icon ?? "📡"}</span>
                        <span className="onboarding-channel-name">{ch.name}</span>
                        <div className="onboarding-channel-card-actions">
                          <button className="btn ghost" onClick={() => openChannelConfig(ch)}>Edit</button>
                          <button className="btn ghost" onClick={() => removeChannelSetup(chId)}>Remove</button>
                        </div>
                      </div>
                    );
                  })}
                </div>
              </div>
            )}

            <div className="onboarding-channel-grid">
              {sortedChannels
                .filter((ch) => !channelSetups.has(ch.id))
                .map((ch) => {
                  const ts = specFor(ch);
                  const isPopular = POPULAR_CHANNELS.includes(ch.channel_type);
                  return (
                    <button
                      key={ch.id}
                      className={`onboarding-channel-card ${isPopular ? "popular" : ""}`}
                      onClick={() => openChannelConfig(ch)}
                    >
                      <span className="onboarding-channel-icon">{ts?.icon ?? "📡"}</span>
                      <div className="onboarding-channel-info">
                        <span className="onboarding-channel-name">{ch.name}</span>
                        <span className="onboarding-channel-blurb">{ts?.blurb ?? ch.channel_type}</span>
                      </div>
                      {isPopular && <span className="chip chip-sm">Popular</span>}
                    </button>
                  );
                })}
            </div>

            <p className="onboarding-hint">
              ⓘ You can configure channels anytime from <strong>Settings → Channels</strong>.
            </p>
          </section>
        )}

        {/* ── Channel setup journey sub-view ── */}
        {step === 3 && configuringChannel && spec && (
          <ChannelSetupJourney
            spec={spec}
            initialValues={channelSetups.get(configuringChannel.id) ?? configuringChannel.config ?? {}}
            onComplete={(config) => {
              setChannelSetups((prev) => {
                const next = new Map(prev);
                next.set(configuringChannel.id, config);
                return next;
              });
              setConfiguringChannel(null);
            }}
            onCancel={() => setConfiguringChannel(null)}
          />
        )}

        {/* ── Navigation buttons ── */}
        <div className="row-actions onboarding-actions">
          {step > 1 && !configuringChannel && (
            <button
              className="btn ghost"
              onClick={() => setStep((value) => Math.max(1, value - 1))}
            >
              Back
            </button>
          )}
          <div style={{ flex: 1 }} />
          {step < 3 ? (
            <button
              className="btn primary"
              onClick={() => setStep((value) => Math.min(3, value + 1))}
              disabled={step === 1 && !canAdvanceFromProvider}
            >
              {step === 2 ? (enabledSkills.size > 0 ? "Continue" : "Skip — set up later") : "Continue"}
            </button>
          ) : !configuringChannel ? (
            <button
              className="btn primary"
              onClick={() =>
                onComplete({
                  provider,
                  model,
                  apiKey,
                  templateName,
                  storedInVault: storeInVault && !localProvider,
                  enabledSkills: Array.from(enabledSkills),
                  channelSetups: Array.from(channelSetups.entries()).map(([channelId, config]) => ({
                    channelId,
                    config,
                  })),
                })
              }
            >
              {configuredChannelCount > 0
                ? "Finish setup"
                : "Skip channels — Start using ClawDesk"}
            </button>
          ) : null}
        </div>
      </div>
    </Modal>
  );
}
