import { useState, useEffect, useCallback, useMemo } from "react";
import * as api from "../api";
import { PageLayout } from "../components/PageLayout";
import { Icon } from "../components/Icon";
import { Modal } from "../components/Modal";
import type {
  IntegrationInfo,
  IntegrationCategoryInfo,
  VaultStatusInfo,
  HealthStatusInfo,
  ConfigFieldInfo,
} from "../types";

// ── Types ─────────────────────────────────────────────────────

type ExtTab = "integrations" | "vault" | "health";

// ── Helpers ───────────────────────────────────────────────────

/** Group config fields by their `group` label (ungrouped → "General"). */
function groupFields(fields: ConfigFieldInfo[]): Map<string, ConfigFieldInfo[]> {
  const out = new Map<string, ConfigFieldInfo[]>();
  for (const f of fields) {
    const g = f.group ?? "General";
    if (!out.has(g)) out.set(g, []);
    out.get(g)!.push(f);
  }
  return out;
}

// Shared input class names
const INPUT_CLS =
  "extensions-input";
const BADGE_CLS = "extensions-badge";

// ── ConfigFieldInput ──────────────────────────────────────────

function ConfigFieldInput({
  field,
  value,
  onChange,
}: {
  field: ConfigFieldInfo;
  value: string;
  onChange: (v: string) => void;
}) {
  switch (field.field_type) {
    case "boolean":
      return (
        <label className="toggle-label">
          <input
            type="checkbox"
            checked={value === "true"}
            onChange={() => onChange(value === "true" ? "false" : "true")}
            style={{ accentColor: "var(--brand)" }}
          />
          {value === "true" ? "Enabled" : "Disabled"}
        </label>
      );

    case "select":
      return (
        <select
          value={value}
          onChange={e => onChange(e.target.value)}
          className={INPUT_CLS}
        >
          <option value="">— select —</option>
          {field.options.map(o => (
            <option key={o.value} value={o.value}>
              {o.label}
            </option>
          ))}
        </select>
      );

    case "number":
    case "port":
      return (
        <input
          type="number"
          value={value}
          onChange={e => onChange(e.target.value)}
          placeholder={field.placeholder ?? field.default ?? ""}
          min={field.field_type === "port" ? 1 : undefined}
          max={field.field_type === "port" ? 65535 : undefined}
          className={INPUT_CLS}
        />
      );

    case "secret":
      return (
        <input
          type="password"
          value={value}
          onChange={e => onChange(e.target.value)}
          placeholder={field.placeholder ?? "••••••••"}
          className={INPUT_CLS}
        />
      );

    case "url":
      return (
        <input
          type="url"
          value={value}
          onChange={e => onChange(e.target.value)}
          placeholder={field.placeholder ?? field.default ?? "https://"}
          className={INPUT_CLS}
        />
      );

    case "filepath":
      return (
        <input
          type="text"
          value={value}
          onChange={e => onChange(e.target.value)}
          placeholder={field.placeholder ?? field.default ?? "/path/to/file"}
          className={INPUT_CLS}
        />
      );

    default: // "text" fallthrough
      return (
        <input
          type="text"
          value={value}
          onChange={e => onChange(e.target.value)}
          placeholder={field.placeholder ?? field.default ?? ""}
          className={INPUT_CLS}
        />
      );
  }
}

// ── IntegrationConfigDialog ───────────────────────────────────

function IntegrationConfigDialog({
  integration,
  pushToast,
  onSaved,
  onClose,
}: {
  integration: IntegrationInfo;
  pushToast: (msg: string) => void;
  onSaved: () => void;
  onClose: () => void;
}) {
  const [draft, setDraft] = useState<Record<string, string>>({});
  const [credInputs, setCredInputs] = useState<Record<string, string>>({});
  const [credStatuses, setCredStatuses] = useState<Record<string, boolean>>({});
  const [saving, setSaving] = useState(false);
  const [testing, setTesting] = useState(false);
  const [validationErrors, setValidationErrors] = useState<string[]>([]);
  const [loadingPanel, setLoadingPanel] = useState(true);
  const [testResult, setTestResult] = useState<HealthStatusInfo | null>(null);
  const [saveMessage, setSaveMessage] = useState<string | null>(null);
  const [hasPersistedSetup, setHasPersistedSetup] = useState(false);
  const [vaultUnlocked, setVaultUnlocked] = useState(false);

  // Hydrate draft + credential statuses on mount
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const [cfg, creds] = await Promise.all([
          api.getExtensionConfig(integration.name).catch(() => ({} as Record<string, string>)),
          api.checkExtensionCredentials(integration.name).catch(() => ({} as Record<string, boolean>)),
        ]);
        const vault = await api.vaultStatus().catch(() => null);
        if (cancelled) return;
        // Merge config_values (from listing) with fresh fetch
        const merged: Record<string, string> = {};
        for (const f of integration.config_fields) {
          merged[f.key] = cfg[f.key] ?? integration.config_values[f.key] ?? "";
        }
        setDraft(merged);
        setCredStatuses(creds);
        setVaultUnlocked(Boolean(vault?.unlocked));
        setHasPersistedSetup(
          Object.values(cfg).some((value) => value.trim() !== "") || Object.values(creds).some(Boolean)
        );
      } finally {
        if (!cancelled) setLoadingPanel(false);
      }
    })();
    return () => { cancelled = true; };
  }, [integration.name, integration.config_fields, integration.config_values]);

  const grouped = useMemo(() => groupFields(integration.config_fields), [integration.config_fields]);

  const healthTone = (state?: string) => {
    if (state === "Healthy") return "success";
    if (state === "Degraded") return "warning";
    if (state === "Unhealthy") return "danger";
    return "neutral";
  };

  const handleSave = async () => {
    setSaving(true);
    setValidationErrors([]);
    setSaveMessage(null);
    try {
      const credentialEntries = integration.credentials_required
        .map((cred) => ({ cred, value: (credInputs[cred.name] ?? "").trim() }))
        .filter((entry) => entry.value.length > 0);

      // Save config values (non-empty only)
      const toSave: Record<string, string> = {};
      for (const [k, v] of Object.entries(draft)) {
        const trimmed = v.trim();
        if (trimmed !== "") toSave[k] = trimmed;
      }
      for (const { cred, value } of credentialEntries) {
        toSave[cred.env_var ?? cred.name] = value;
      }
      await api.saveExtensionConfig(integration.name, toSave);

      let storedInVault = 0;
      let savedToConfigOnly = 0;
      if (vaultUnlocked) {
        for (const { cred, value } of credentialEntries) {
          try {
            await api.storeExtensionCredential(integration.name, cred.name, value);
            storedInVault += 1;
          } catch {
            savedToConfigOnly += 1;
          }
        }
      } else {
        savedToConfigOnly = credentialEntries.length;
      }

      // Validate
      const missing = await api.validateExtensionConfig(integration.name).catch(() => []);
      const credentialWarning = savedToConfigOnly > 0
        ? ` ${savedToConfigOnly} credential${savedToConfigOnly > 1 ? "s were" : " was"} saved in integration config because the vault is locked or unavailable.`
        : "";
      if (missing.length > 0) {
        setValidationErrors(missing);
        setSaveMessage(`Saved, but ${missing.length} required field${missing.length > 1 ? "s are" : " is"} still missing.${credentialWarning}`);
        pushToast(`Saved, but ${missing.length} required field(s) still missing`);
      } else {
        const vaultNote = storedInVault > 0
          ? ` ${storedInVault} credential${storedInVault > 1 ? "s" : ""} also stored in the vault.`
          : credentialWarning;
        setSaveMessage(`${hasPersistedSetup ? "Configuration updated" : "Configuration saved"} for ${integration.name}.${vaultNote}`);
        pushToast(`Configuration saved for ${integration.name}`);
      }

      // Refresh credential statuses
      const creds = await api.checkExtensionCredentials(integration.name).catch(() => ({} as Record<string, boolean>));
      setCredStatuses(creds);
      setCredInputs({});
      setHasPersistedSetup(true);
      onSaved();
    } catch (e: any) {
      setSaveMessage(null);
      pushToast(`Save failed: ${e}`);
    } finally {
      setSaving(false);
    }
  };

  const handleTest = async () => {
    setTesting(true);
    setTestResult(null);
    try {
      const result = await api.checkIntegrationHealth(integration.name);
      setTestResult(result);
      pushToast(`${integration.name}: ${result.state}${result.latency_ms != null ? ` (${result.latency_ms}ms)` : ""}`);
      onSaved();
    } catch (e: any) {
      pushToast(`Test failed: ${e}`);
    } finally {
      setTesting(false);
    }
  };

  if (loadingPanel) {
    return (
      <Modal title={`${integration.name} configuration`} onClose={onClose}>
        <div className="extensions-config-loading">Loading configuration…</div>
      </Modal>
    );
  }

  const hasConfigFields = integration.config_fields.length > 0;
  const hasCredentials = integration.credentials_required.length > 0;

  return (
    <Modal title={`${integration.name} configuration`} onClose={onClose}>
      <div className="extensions-config-dialog">
        <section className="extensions-config-hero">
          <div className="extensions-config-hero__main">
            <div className="extensions-config-hero__title">
              <span className="extensions-config-hero__icon">{integration.icon}</span>
              <div>
                <h3>{integration.name}</h3>
                <p>{integration.description}</p>
              </div>
            </div>
            <div className="extensions-config-hero__chips">
              <span className={`${BADGE_CLS} extensions-badge--neutral`}>{integration.category}</span>
              <span className={`${BADGE_CLS} extensions-badge--transport`}>{integration.transport_type}</span>
              {integration.has_oauth ? <span className={`${BADGE_CLS} extensions-badge--success`}>OAuth</span> : null}
            </div>
          </div>
          <div className="extensions-config-hero__side">
            <div className={`extensions-health-pill extensions-health-pill--${healthTone(testResult?.state)}`}>
              {testResult?.state ?? "Not tested"}
            </div>
            {testResult?.latency_ms != null ? <div className="extensions-health-meta">{testResult.latency_ms}ms latency</div> : <div className="extensions-health-meta">Testing checks the saved integration health endpoint.</div>}
          </div>
        </section>

        {saveMessage ? <div className="extensions-config-banner extensions-config-banner--success">{saveMessage}</div> : null}
        {validationErrors.length > 0 ? <div className="extensions-config-banner extensions-config-banner--warning">{validationErrors.length} required field{validationErrors.length > 1 ? "s are" : " is"} still missing.</div> : null}

        {hasConfigFields && (
          <div className="extensions-config-groups">
            {[...grouped.entries()].map(([group, fields]) => (
              <fieldset key={group} className="extensions-config-group">
                <legend>{group}</legend>
                <div className="extensions-config-group__grid">
                  {fields.map(f => (
                    <div key={f.key} className="extensions-config-field">
                      <label className="extensions-config-field__label">
                        {f.label}
                        {f.required && <span>*</span>}
                      </label>
                      {f.description ? <div className="extensions-config-field__help">{f.description}</div> : null}
                      <ConfigFieldInput
                        field={f}
                        value={draft[f.key] ?? ""}
                        onChange={v => setDraft(prev => ({ ...prev, [f.key]: v }))}
                      />
                      {validationErrors.includes(f.key) ? <div className="extensions-config-field__error">This field is required.</div> : null}
                    </div>
                  ))}
                </div>
              </fieldset>
            ))}
          </div>
        )}

        {hasCredentials && (
          <fieldset className="extensions-config-group">
            <legend>Credentials</legend>
            {!vaultUnlocked ? (
              <div className="extensions-config-field__help">
                Credential Vault is locked. Entered secrets will still save into the integration configuration so the agent can use them, but unlock the vault if you want encrypted at-rest storage.
              </div>
            ) : null}
            <div className="extensions-credentials-grid">
              {integration.credentials_required.map(cred => {
                const ok = credStatuses[cred.name] === true;
                return (
                  <div key={cred.name} className="extensions-credential-card">
                    <div className="extensions-credential-card__head">
                      <div>
                        <div className="extensions-credential-card__title">
                          <span className={`extensions-credential-dot ${ok ? "ok" : "missing"}`} />
                          {cred.name}
                          {cred.required ? <span className="extensions-required-mark">*</span> : null}
                        </div>
                        {cred.description ? <div className="extensions-config-field__help">{cred.description}</div> : null}
                        {cred.env_var ? <div className="extensions-credential-card__env">env: {cred.env_var}</div> : null}
                      </div>
                      <span className={`${BADGE_CLS} ${ok ? "extensions-badge--success" : "extensions-badge--neutral"}`}>{ok ? "Configured" : "Missing"}</span>
                    </div>
                    <input
                      type="password"
                      value={credInputs[cred.name] ?? ""}
                      onChange={e => setCredInputs(prev => ({ ...prev, [cred.name]: e.target.value }))}
                      placeholder={ok ? `Replace ${cred.name}` : `Enter ${cred.name}`}
                      className={INPUT_CLS}
                    />
                  </div>
                );
              })}
            </div>
          </fieldset>
        )}

        {!hasConfigFields && !hasCredentials ? (
          <div className="extensions-config-empty">This integration requires no additional configuration. You can enable it directly or run a health test.</div>
        ) : null}

        <div className="extensions-config-actions">
          <div className="extensions-config-actions__left">
            {integration.has_oauth ? (
              <button onClick={() => void api.startExtensionOAuth(integration.name).then(flow => { window.open(flow.auth_url, "_blank"); pushToast(`Opening OAuth flow for ${integration.name}…`); }).catch((e: any) => pushToast(`OAuth error: ${e}`))} className="btn subtle">
                <Icon name="link" /> OAuth
              </button>
            ) : null}
            <button onClick={handleTest} disabled={testing} className="btn subtle">
              <Icon name="activity" /> {testing ? "Testing…" : "Test Connection"}
            </button>
          </div>
          <div className="extensions-config-actions__right">
            <button onClick={onClose} className="btn ghost">Close</button>
            <button onClick={handleSave} disabled={saving} className="btn primary">
              <Icon name="check" /> {saving ? "Saving…" : hasPersistedSetup ? "Update Configuration" : "Save Configuration"}
            </button>
          </div>
        </div>
      </div>
    </Modal>
  );
}

// ── Props ─────────────────────────────────────────────────────

export interface ExtensionsPageProps {
  pushToast: (msg: string) => void;
}

// ── Component ─────────────────────────────────────────────────

export function ExtensionsPage({ pushToast }: ExtensionsPageProps) {
  const [tab, setTab] = useState<ExtTab>("integrations");
  const [integrations, setIntegrations] = useState<IntegrationInfo[]>([]);
  const [categories, setCategories] = useState<IntegrationCategoryInfo[]>([]);
  const [vaultStatus, setVaultStatus] = useState<VaultStatusInfo | null>(null);
  const [healthStatuses, setHealthStatuses] = useState<HealthStatusInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [filterCategory, setFilterCategory] = useState<string>("all");
  const [vaultPassword, setVaultPassword] = useState("");
  const [credentialName, setCredentialName] = useState("");
  const [credentialValue, setCredentialValue] = useState("");
  const [credentialNames, setCredentialNames] = useState<string[]>([]);
  const [selectedIntegration, setSelectedIntegration] = useState<IntegrationInfo | null>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      const [intg, cats, vs, hs] = await Promise.all([
        api.listIntegrations().catch(() => [] as IntegrationInfo[]),
        api.listIntegrationCategories().catch(() => [] as IntegrationCategoryInfo[]),
        api.vaultStatus().catch(() => null),
        api.getAllHealthStatuses().catch(() => [] as HealthStatusInfo[]),
      ]);
      setIntegrations(intg);
      setCategories(cats);
      setVaultStatus(vs);
      setHealthStatuses(hs);
      if (vs?.unlocked) {
        const names = await api.vaultListCredentials().catch(() => []);
        setCredentialNames(names);
      }
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => { refresh(); }, [refresh]);

  const handleToggle = async (name: string, enabled: boolean) => {
    try {
      if (enabled) {
        await api.disableIntegration(name);
        pushToast(`Disabled ${name}`);
      } else {
        await api.enableIntegration(name);
        pushToast(`Enabled ${name}`);
      }
      refresh();
    } catch (e: any) {
      pushToast(`Error: ${e}`);
    }
  };

  const handleVaultUnlock = async () => {
    if (!vaultPassword) return;
    try {
      if (!vaultStatus?.exists) {
        await api.vaultInitialize(vaultPassword);
        pushToast("Vault created and unlocked");
      } else {
        await api.vaultUnlock(vaultPassword);
        pushToast("Vault unlocked");
      }
      setVaultPassword("");
      refresh();
    } catch (e: any) {
      pushToast(`Vault error: ${e}`);
    }
  };

  const handleVaultLock = async () => {
    try {
      await api.vaultLock();
      pushToast("Vault locked");
      setCredentialNames([]);
      refresh();
    } catch (e: any) {
      pushToast(`Error: ${e}`);
    }
  };

  const handleStoreCredential = async () => {
    if (!credentialName || !credentialValue) return;
    try {
      await api.vaultStoreCredential(credentialName, credentialValue);
      pushToast(`Stored credential: ${credentialName}`);
      setCredentialName("");
      setCredentialValue("");
      const names = await api.vaultListCredentials().catch(() => []);
      setCredentialNames(names);
    } catch (e: any) {
      pushToast(`Error: ${e}`);
    }
  };

  const handleDeleteCredential = async (name: string) => {
    try {
      await api.vaultDeleteCredential(name);
      pushToast(`Deleted credential: ${name}`);
      const names = await api.vaultListCredentials().catch(() => []);
      setCredentialNames(names);
    } catch (e: any) {
      pushToast(`Error: ${e}`);
    }
  };

  const handleCheckHealth = async (name: string) => {
    try {
      const result = await api.checkIntegrationHealth(name);
      pushToast(`${name}: ${result.state} (${result.latency_ms ?? "?"}ms)`);
      refresh();
    } catch (e: any) {
      pushToast(`Health check failed: ${e}`);
    }
  };

  const handleOAuth = async (name: string) => {
    try {
      const flow = await api.startExtensionOAuth(name);
      window.open(flow.auth_url, "_blank");
      pushToast(`Opening OAuth flow for ${name}…`);
    } catch (e: any) {
      pushToast(`OAuth error: ${e}`);
    }
  };

  const filtered = filterCategory === "all"
    ? integrations
    : integrations.filter(i => i.category === filterCategory);

  const enabledCount = integrations.filter((integration) => integration.enabled).length;
  const oauthCount = integrations.filter((integration) => integration.has_oauth).length;
  const healthMap = useMemo(() => {
    const map = new Map<string, HealthStatusInfo>();
    for (const status of healthStatuses) map.set(status.name, status);
    return map;
  }, [healthStatuses]);

  const TABS: { key: ExtTab; label: string }[] = [
    { key: "integrations", label: "Integrations" },
    { key: "vault", label: "Credential Vault" },
    { key: "health", label: "Health Monitor" },
  ];

  return (
    <PageLayout
      title="Extensions"
      subtitle={`${integrations.length} integrations • ${categories.length} categories`}
      onRefresh={refresh}
      loading={loading}
    >
      <div className="extensions-page-shell">
      <section className="extensions-hero">
        <div className="extensions-hero__intro">
          <span className="extensions-hero__eyebrow">Integration registry</span>
          <h2>Connect external systems, configure credentials in context, and verify they are actually working.</h2>
          <p>{enabledCount} enabled, {oauthCount} OAuth-capable, and {categories.length} categories available across the extension catalog.</p>
        </div>
        <div className="extensions-hero__stats">
          <div className="extensions-hero-stat"><span>Enabled</span><strong>{enabledCount}</strong><small>Active integrations</small></div>
          <div className="extensions-hero-stat"><span>OAuth</span><strong>{oauthCount}</strong><small>Browser-based auth flows</small></div>
          <div className="extensions-hero-stat"><span>Categories</span><strong>{categories.length}</strong><small>Integration domains</small></div>
        </div>
      </section>

      <div className="extensions-tabs">
        {TABS.map(t => (
          <button
            key={t.key}
            onClick={() => setTab(t.key)}
            className={`extensions-tab${tab === t.key ? " active" : ""}`}
          >
            {t.label}
          </button>
        ))}
      </div>

      {tab === "integrations" && (
        <div className="extensions-pane">
          <div className="extensions-filter-row">
            <button
              onClick={() => setFilterCategory("all")}
              className={`extensions-filter-chip${filterCategory === "all" ? " active" : ""}`}
            >
              All ({integrations.length})
            </button>
            {categories.map(c => (
              <button
                key={c.name}
                onClick={() => setFilterCategory(c.name)}
                className={`extensions-filter-chip${filterCategory === c.name ? " active" : ""}`}
              >
                {c.name} ({c.count})
              </button>
            ))}
          </div>

          <div className="extensions-list">
            {filtered.map(i => {
              const configurable = i.config_fields.length > 0 || i.credentials_required.length > 0;
              const health = healthMap.get(i.name);
              return (
                <div
                  key={i.name}
                  className="extensions-card"
                  role="button"
                  tabIndex={0}
                  onClick={() => setSelectedIntegration(i)}
                  onKeyDown={(event) => {
                    if (event.key === "Enter" || event.key === " ") {
                      event.preventDefault();
                      setSelectedIntegration(i);
                    }
                  }}
                >
                  <div className="extensions-card__main">
                    <div className="extensions-card__identity">
                      <span className="extensions-card__icon">{i.icon}</span>
                      <div className="extensions-card__text">
                        <div className="extensions-card__title-row">
                          <strong>{i.name}</strong>
                          <span className={`extensions-card__health extensions-card__health--${formatHealthTone(health?.state)}`}>{health?.state ?? "Unknown"}</span>
                        </div>
                        <p>{i.description}</p>
                        <div className="extensions-card__chips">
                          <span className={`${BADGE_CLS} extensions-badge--neutral`}>{i.category}</span>
                          <span className={`${BADGE_CLS} extensions-badge--transport`}>{i.transport_type}</span>
                          {i.has_oauth && <span className={`${BADGE_CLS} extensions-badge--success`}>OAuth</span>}
                          {i.credentials_required.length > 0 && (
                            <span className={`${BADGE_CLS} extensions-badge--warning`}>
                              {i.credentials_required.length} cred{i.credentials_required.length > 1 ? "s" : ""}
                            </span>
                          )}
                          {i.config_fields.length > 0 && (
                            <span className={`${BADGE_CLS} extensions-badge--accent`}>
                              {i.config_fields.length} setting{i.config_fields.length > 1 ? "s" : ""}
                            </span>
                          )}
                          {!configurable && <span className={`${BADGE_CLS} extensions-badge--neutral`}>No setup</span>}
                        </div>
                      </div>
                    </div>
                    <div className="extensions-card__actions" onClick={e => e.stopPropagation()}>
                      <button
                        onClick={() => setSelectedIntegration(i)}
                        className="btn subtle"
                      >
                        {configurable ? "Configure" : "Open"}
                      </button>
                      {i.has_oauth && (
                        <button
                          onClick={() => handleOAuth(i.name)}
                          className="btn subtle"
                        >
                          OAuth
                        </button>
                      )}
                      <button
                        onClick={() => handleToggle(i.name, i.enabled)}
                        className={`extensions-toggle${i.enabled ? " enabled" : ""}`}
                      >
                        {i.enabled ? "Enabled" : "Disabled"}
                      </button>
                    </div>
                  </div>
                </div>
              );
            })}
            {filtered.length === 0 && (
              <div className="extensions-empty">No integrations found</div>
            )}
          </div>
        </div>
      )}

      {tab === "vault" && (
        <div className="extensions-pane">
          <div className="extensions-surface">
            <div className="extensions-surface__head">
              <div>
                <h3>Credential Vault</h3>
                <p>Securely store integration secrets and manage reusable credentials.</p>
              </div>
              <span className={`extensions-status-chip ${
                vaultStatus?.unlocked
                  ? "success"
                  : "warning"
              }`}>
                {vaultStatus?.unlocked ? "Unlocked" : vaultStatus?.exists ? "Locked" : "Not Created"}
              </span>
            </div>
            {vaultStatus?.unlocked && (
              <div className="extensions-surface__meta">
                {vaultStatus.credential_count} credential{vaultStatus.credential_count !== 1 ? "s" : ""} stored
              </div>
            )}

            {!vaultStatus?.unlocked && (
              <div className="extensions-inline-form">
                <input
                  type="password"
                  value={vaultPassword}
                  onChange={e => setVaultPassword(e.target.value)}
                  placeholder={vaultStatus?.exists ? "Enter master password" : "Create master password"}
                  className={INPUT_CLS}
                  onKeyDown={e => e.key === "Enter" && handleVaultUnlock()}
                />
                <button
                  onClick={handleVaultUnlock}
                  className="btn primary"
                >
                  {vaultStatus?.exists ? "Unlock" : "Create"}
                </button>
              </div>
            )}

            {vaultStatus?.unlocked && (
              <button
                onClick={handleVaultLock}
                className="btn subtle"
              >
                Lock Vault
              </button>
            )}
          </div>

          {vaultStatus?.unlocked && (
            <div className="extensions-surface">
              <div className="extensions-surface__head">
                <div>
                  <h3>Add Credential</h3>
                  <p>Store a reusable secret in the vault.</p>
                </div>
              </div>
              <div className="extensions-inline-form">
                <input
                  type="text"
                  value={credentialName}
                  onChange={e => setCredentialName(e.target.value)}
                  placeholder="Name (e.g. github_token)"
                  className={INPUT_CLS}
                />
                <input
                  type="password"
                  value={credentialValue}
                  onChange={e => setCredentialValue(e.target.value)}
                  placeholder="Secret value"
                  className={INPUT_CLS}
                />
                <button
                  onClick={handleStoreCredential}
                  className="btn primary"
                >
                  Store
                </button>
              </div>
            </div>
          )}

          {vaultStatus?.unlocked && credentialNames.length > 0 && (
            <div className="extensions-surface">
              <div className="extensions-surface__head">
                <div>
                  <h3>Stored Credentials</h3>
                  <p>Vault entries currently available to extensions.</p>
                </div>
              </div>
              <div className="extensions-credential-list">
                {credentialNames.map(name => (
                  <div key={name} className="extensions-credential-row">
                    <span className="extensions-credential-row__name">{name}</span>
                    <button
                      onClick={() => handleDeleteCredential(name)}
                      className="btn ghost"
                    >
                      Delete
                    </button>
                  </div>
                ))}
              </div>
            </div>
          )}
        </div>
      )}

      {tab === "health" && (
        <div className="extensions-pane">
          {healthStatuses.length === 0 && (
            <div className="extensions-empty">No health checks registered yet</div>
          )}
          {healthStatuses.map(h => (
            <div
              key={h.name}
              className="extensions-health-row"
            >
              <div>
                <div className="extensions-health-row__title">{h.name}</div>
                <div className="extensions-health-row__meta">
                  {h.state}
                  {h.latency_ms != null && ` • ${h.latency_ms}ms`}
                  {h.consecutive_failures > 0 && ` • ${h.consecutive_failures} failures`}
                </div>
                {h.last_check && (
                  <div className="extensions-health-row__submeta">
                    Last check: {new Date(h.last_check).toLocaleString()}
                  </div>
                )}
              </div>
              <div className="extensions-health-row__actions">
                <span className={`extensions-status-chip ${formatHealthTone(h.state)}`}>
                  {h.state}
                </span>
                <button
                  onClick={() => handleCheckHealth(h.name)}
                  className="btn subtle"
                >
                  Check
                </button>
              </div>
            </div>
          ))}
        </div>
      )}
      </div>

      {selectedIntegration ? (
        <IntegrationConfigDialog
          integration={selectedIntegration}
          pushToast={pushToast}
          onSaved={refresh}
          onClose={() => setSelectedIntegration(null)}
        />
      ) : null}
    </PageLayout>
  );
}

function formatHealthTone(state?: string) {
  if (state === "Healthy") return "success";
  if (state === "Degraded") return "warning";
  if (state === "Unhealthy") return "danger";
  return "neutral";
}
