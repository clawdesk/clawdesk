import { useState, useEffect, useCallback, useMemo, Fragment } from "react";
import * as api from "../api";
import { PageLayout } from "../components/PageLayout";
import { Icon } from "../components/Icon";
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
  "w-full px-3 py-1.5 text-sm rounded border border-neutral-300 dark:border-neutral-600 bg-white dark:bg-neutral-900 focus:outline-none focus:ring-1 focus:ring-blue-500 dark:focus:ring-blue-400 placeholder:text-neutral-400";
const BADGE_CLS = "text-[10px] px-1.5 py-0.5 rounded";

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
        <button
          type="button"
          onClick={() => onChange(value === "true" ? "false" : "true")}
          className={`relative inline-flex h-5 w-9 shrink-0 rounded-full border-2 border-transparent transition-colors ${
            value === "true" ? "bg-blue-500" : "bg-neutral-300 dark:bg-neutral-600"
          }`}
        >
          <span
            className={`pointer-events-none inline-block h-4 w-4 rounded-full bg-white shadow transform transition-transform ${
              value === "true" ? "translate-x-4" : "translate-x-0"
            }`}
          />
        </button>
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

// ── IntegrationConfigPanel ────────────────────────────────────

function IntegrationConfigPanel({
  integration,
  pushToast,
  onSaved,
}: {
  integration: IntegrationInfo;
  pushToast: (msg: string) => void;
  onSaved: () => void;
}) {
  const [draft, setDraft] = useState<Record<string, string>>({});
  const [credInputs, setCredInputs] = useState<Record<string, string>>({});
  const [credStatuses, setCredStatuses] = useState<Record<string, boolean>>({});
  const [saving, setSaving] = useState(false);
  const [validationErrors, setValidationErrors] = useState<string[]>([]);
  const [loadingPanel, setLoadingPanel] = useState(true);

  // Hydrate draft + credential statuses on mount
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const [cfg, creds] = await Promise.all([
          api.getExtensionConfig(integration.name).catch(() => ({} as Record<string, string>)),
          api.checkExtensionCredentials(integration.name).catch(() => ({} as Record<string, boolean>)),
        ]);
        if (cancelled) return;
        // Merge config_values (from listing) with fresh fetch
        const merged: Record<string, string> = {};
        for (const f of integration.config_fields) {
          merged[f.key] = cfg[f.key] ?? integration.config_values[f.key] ?? "";
        }
        setDraft(merged);
        setCredStatuses(creds);
      } finally {
        if (!cancelled) setLoadingPanel(false);
      }
    })();
    return () => { cancelled = true; };
  }, [integration.name, integration.config_fields, integration.config_values]);

  const grouped = useMemo(() => groupFields(integration.config_fields), [integration.config_fields]);

  const handleSave = async () => {
    setSaving(true);
    setValidationErrors([]);
    try {
      // Save config values (non-empty only)
      const toSave: Record<string, string> = {};
      for (const [k, v] of Object.entries(draft)) {
        if (v !== "") toSave[k] = v;
      }
      await api.saveExtensionConfig(integration.name, toSave);

      // Store any filled credential inputs into the vault
      for (const [credName, credVal] of Object.entries(credInputs)) {
        if (credVal) {
          await api.storeExtensionCredential(integration.name, credName, credVal);
        }
      }

      // Validate
      const missing = await api.validateExtensionConfig(integration.name).catch(() => []);
      if (missing.length > 0) {
        setValidationErrors(missing);
        pushToast(`Saved, but ${missing.length} required field(s) still missing`);
      } else {
        pushToast(`Configuration saved for ${integration.name}`);
      }

      // Refresh credential statuses
      const creds = await api.checkExtensionCredentials(integration.name).catch(() => ({} as Record<string, boolean>));
      setCredStatuses(creds);
      setCredInputs({});
      onSaved();
    } catch (e: any) {
      pushToast(`Save failed: ${e}`);
    } finally {
      setSaving(false);
    }
  };

  if (loadingPanel) {
    return (
      <div className="px-4 py-6 text-sm text-neutral-500 text-center">Loading configuration…</div>
    );
  }

  const hasConfigFields = integration.config_fields.length > 0;
  const hasCredentials = integration.credentials_required.length > 0;

  return (
    <div className="px-4 pb-4 space-y-5 border-t border-neutral-200 dark:border-neutral-700 bg-neutral-100/50 dark:bg-neutral-900/30">
      {/* ── Transport badge ─────────────────────── */}
      <div className="flex items-center gap-2 pt-3">
        <span className="text-[10px] font-medium text-neutral-400 uppercase tracking-wide">Transport</span>
        <span className={`${BADGE_CLS} bg-indigo-100 dark:bg-indigo-900 text-indigo-700 dark:text-indigo-300`}>
          {integration.transport_type}
        </span>
      </div>

      {/* ── Config field groups ─────────────────── */}
      {hasConfigFields && (
        <div className="space-y-4">
          {[...grouped.entries()].map(([group, fields]) => (
            <fieldset key={group} className="space-y-3">
              <legend className="text-xs font-semibold text-neutral-500 dark:text-neutral-400 uppercase tracking-wide">
                {group}
              </legend>
              {fields.map(f => (
                <div key={f.key} className="space-y-1">
                  <label className="flex items-center gap-1 text-xs font-medium text-neutral-700 dark:text-neutral-300">
                    {f.label}
                    {f.required && <span className="text-red-500">*</span>}
                  </label>
                  {f.description && (
                    <div className="text-[11px] text-neutral-400 mb-1">{f.description}</div>
                  )}
                  <ConfigFieldInput
                    field={f}
                    value={draft[f.key] ?? ""}
                    onChange={v => setDraft(prev => ({ ...prev, [f.key]: v }))}
                  />
                  {validationErrors.includes(f.key) && (
                    <div className="text-[11px] text-red-500">This field is required</div>
                  )}
                </div>
              ))}
            </fieldset>
          ))}
        </div>
      )}

      {/* ── Credential requirements ────────────── */}
      {hasCredentials && (
        <fieldset className="space-y-3">
          <legend className="text-xs font-semibold text-neutral-500 dark:text-neutral-400 uppercase tracking-wide">
            Credentials
          </legend>
          {integration.credentials_required.map(cred => {
            const ok = credStatuses[cred.name] === true;
            return (
              <div key={cred.name} className="space-y-1">
                <label className="flex items-center gap-2 text-xs font-medium text-neutral-700 dark:text-neutral-300">
                  <span className={`inline-block w-2 h-2 rounded-full ${ok ? "bg-green-500" : "bg-red-400"}`} />
                  {cred.name}
                  {cred.required && <span className="text-red-500">*</span>}
                  {ok && <span className="text-[10px] text-green-600 dark:text-green-400">(configured)</span>}
                </label>
                {cred.description && (
                  <div className="text-[11px] text-neutral-400">{cred.description}</div>
                )}
                {cred.env_var && (
                  <div className="text-[10px] text-neutral-400 font-mono">env: {cred.env_var}</div>
                )}
                {!ok && (
                  <input
                    type="password"
                    value={credInputs[cred.name] ?? ""}
                    onChange={e => setCredInputs(prev => ({ ...prev, [cred.name]: e.target.value }))}
                    placeholder={`Enter ${cred.name}`}
                    className={INPUT_CLS}
                  />
                )}
              </div>
            );
          })}
        </fieldset>
      )}

      {/* ── No config message ──────────────────── */}
      {!hasConfigFields && !hasCredentials && (
        <div className="text-xs text-neutral-500 pt-2">
          This integration requires no additional configuration. Just enable it.
        </div>
      )}

      {/* ── Actions ────────────────────────────── */}
      {(hasConfigFields || hasCredentials) && (
        <div className="flex items-center gap-3 pt-1">
          <button
            onClick={handleSave}
            disabled={saving}
            className="px-4 py-1.5 text-sm rounded font-medium bg-blue-500 text-white hover:bg-blue-600 disabled:opacity-50 disabled:cursor-not-allowed"
          >
            {saving ? "Saving…" : "Save Configuration"}
          </button>
          {validationErrors.length > 0 && (
            <span className="text-xs text-amber-600 dark:text-amber-400">
              {validationErrors.length} required field{validationErrors.length > 1 ? "s" : ""} missing
            </span>
          )}
        </div>
      )}
    </div>
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
  // Which integration card (by name) is expanded to show config.
  const [expanded, setExpanded] = useState<string | null>(null);

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
      {/* ── Tab bar ────────────────────────────────────── */}
      <div className="flex gap-1 mb-4 border-b border-neutral-200 dark:border-neutral-700">
        {TABS.map(t => (
          <button
            key={t.key}
            onClick={() => setTab(t.key)}
            className={`px-3 py-2 text-sm font-medium border-b-2 transition-colors ${
              tab === t.key
                ? "border-blue-500 text-blue-600 dark:text-blue-400"
                : "border-transparent text-neutral-500 hover:text-neutral-700 dark:hover:text-neutral-300"
            }`}
          >
            {t.label}
          </button>
        ))}
      </div>

      {/* ── Integrations Tab ───────────────────────────── */}
      {tab === "integrations" && (
        <div className="space-y-4">
          {/* Category filter */}
          <div className="flex gap-2 flex-wrap">
            <button
              onClick={() => setFilterCategory("all")}
              className={`px-2 py-1 text-xs rounded ${filterCategory === "all" ? "bg-blue-100 text-blue-700 dark:bg-blue-900 dark:text-blue-300" : "bg-neutral-100 dark:bg-neutral-800"}`}
            >
              All ({integrations.length})
            </button>
            {categories.map(c => (
              <button
                key={c.name}
                onClick={() => setFilterCategory(c.name)}
                className={`px-2 py-1 text-xs rounded ${filterCategory === c.name ? "bg-blue-100 text-blue-700 dark:bg-blue-900 dark:text-blue-300" : "bg-neutral-100 dark:bg-neutral-800"}`}
              >
                {c.name} ({c.count})
              </button>
            ))}
          </div>

          {/* Integration list */}
          <div className="grid gap-3">
            {filtered.map(i => {
              const isExpanded = expanded === i.name;
              const configurable = i.config_fields.length > 0 || i.credentials_required.length > 0;
              return (
                <div
                  key={i.name}
                  className={`rounded-lg border transition-colors ${
                    isExpanded
                      ? "border-blue-300 dark:border-blue-700 bg-white dark:bg-neutral-800"
                      : "border-neutral-200 dark:border-neutral-700 bg-neutral-50 dark:bg-neutral-800/50"
                  }`}
                >
                  {/* Card header — always visible */}
                  <div
                    className="flex items-center justify-between p-3 cursor-pointer select-none"
                    onClick={() => setExpanded(isExpanded ? null : i.name)}
                  >
                    <div className="flex items-center gap-3 min-w-0">
                      <span className="text-2xl shrink-0">{i.icon}</span>
                      <div className="min-w-0">
                        <div className="font-medium text-sm flex items-center gap-2">
                          {i.name}
                          {configurable && (
                            <svg
                              className={`w-3.5 h-3.5 text-neutral-400 transition-transform ${isExpanded ? "rotate-180" : ""}`}
                              fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}
                            >
                              <path strokeLinecap="round" strokeLinejoin="round" d="M19 9l-7 7-7-7" />
                            </svg>
                          )}
                        </div>
                        <div className="text-xs text-neutral-500 truncate">{i.description}</div>
                        <div className="flex gap-1 mt-1 flex-wrap">
                          <span className={`${BADGE_CLS} bg-neutral-200 dark:bg-neutral-700`}>{i.category}</span>
                          <span className={`${BADGE_CLS} bg-neutral-200 dark:bg-neutral-700 font-mono`}>{i.transport_type}</span>
                          {i.has_oauth && <span className={`${BADGE_CLS} bg-green-100 dark:bg-green-900 text-green-700 dark:text-green-300`}>OAuth</span>}
                          {i.credentials_required.length > 0 && (
                            <span className={`${BADGE_CLS} bg-yellow-100 dark:bg-yellow-900 text-yellow-700 dark:text-yellow-300`}>
                              {i.credentials_required.length} cred{i.credentials_required.length > 1 ? "s" : ""}
                            </span>
                          )}
                          {i.config_fields.length > 0 && (
                            <span className={`${BADGE_CLS} bg-purple-100 dark:bg-purple-900 text-purple-700 dark:text-purple-300`}>
                              {i.config_fields.length} setting{i.config_fields.length > 1 ? "s" : ""}
                            </span>
                          )}
                        </div>
                      </div>
                    </div>
                    <div className="flex items-center gap-2 shrink-0" onClick={e => e.stopPropagation()}>
                      {i.has_oauth && (
                        <button
                          onClick={() => handleOAuth(i.name)}
                          className="text-xs px-2 py-1 rounded bg-blue-500 text-white hover:bg-blue-600"
                        >
                          OAuth
                        </button>
                      )}
                      <button
                        onClick={() => handleToggle(i.name, i.enabled)}
                        className={`text-xs px-3 py-1 rounded font-medium ${
                          i.enabled
                            ? "bg-green-100 text-green-700 dark:bg-green-900 dark:text-green-300"
                            : "bg-neutral-200 text-neutral-600 dark:bg-neutral-700 dark:text-neutral-400"
                        }`}
                      >
                        {i.enabled ? "Enabled" : "Disabled"}
                      </button>
                    </div>
                  </div>

                  {/* Expanded config panel */}
                  {isExpanded && (
                    <IntegrationConfigPanel
                      integration={i}
                      pushToast={pushToast}
                      onSaved={refresh}
                    />
                  )}
                </div>
              );
            })}
            {filtered.length === 0 && (
              <div className="text-center text-neutral-500 py-8">No integrations found</div>
            )}
          </div>
        </div>
      )}

      {/* ── Vault Tab ──────────────────────────────────── */}
      {tab === "vault" && (
        <div className="space-y-4">
          {/* Vault status */}
          <div className="p-4 rounded-lg bg-neutral-50 dark:bg-neutral-800/50 border border-neutral-200 dark:border-neutral-700">
            <div className="flex items-center justify-between mb-3">
              <div className="font-medium">Credential Vault</div>
              <span className={`text-xs px-2 py-1 rounded font-medium ${
                vaultStatus?.unlocked
                  ? "bg-green-100 text-green-700 dark:bg-green-900 dark:text-green-300"
                  : "bg-yellow-100 text-yellow-700 dark:bg-yellow-900 dark:text-yellow-300"
              }`}>
                {vaultStatus?.unlocked ? "Unlocked" : vaultStatus?.exists ? "Locked" : "Not Created"}
              </span>
            </div>
            {vaultStatus?.unlocked && (
              <div className="text-xs text-neutral-500 mb-3">
                {vaultStatus.credential_count} credential{vaultStatus.credential_count !== 1 ? "s" : ""} stored
              </div>
            )}

            {!vaultStatus?.unlocked && (
              <div className="flex gap-2">
                <input
                  type="password"
                  value={vaultPassword}
                  onChange={e => setVaultPassword(e.target.value)}
                  placeholder={vaultStatus?.exists ? "Enter master password" : "Create master password"}
                  className="flex-1 px-3 py-1.5 text-sm rounded border border-neutral-300 dark:border-neutral-600 bg-white dark:bg-neutral-900"
                  onKeyDown={e => e.key === "Enter" && handleVaultUnlock()}
                />
                <button
                  onClick={handleVaultUnlock}
                  className="px-3 py-1.5 text-sm rounded bg-blue-500 text-white hover:bg-blue-600"
                >
                  {vaultStatus?.exists ? "Unlock" : "Create"}
                </button>
              </div>
            )}

            {vaultStatus?.unlocked && (
              <button
                onClick={handleVaultLock}
                className="text-xs px-3 py-1 rounded bg-neutral-200 dark:bg-neutral-700 hover:bg-neutral-300 dark:hover:bg-neutral-600"
              >
                Lock Vault
              </button>
            )}
          </div>

          {/* Store credential */}
          {vaultStatus?.unlocked && (
            <div className="p-4 rounded-lg bg-neutral-50 dark:bg-neutral-800/50 border border-neutral-200 dark:border-neutral-700">
              <div className="font-medium text-sm mb-3">Add Credential</div>
              <div className="flex gap-2 mb-2">
                <input
                  type="text"
                  value={credentialName}
                  onChange={e => setCredentialName(e.target.value)}
                  placeholder="Name (e.g. github_token)"
                  className="flex-1 px-3 py-1.5 text-sm rounded border border-neutral-300 dark:border-neutral-600 bg-white dark:bg-neutral-900"
                />
                <input
                  type="password"
                  value={credentialValue}
                  onChange={e => setCredentialValue(e.target.value)}
                  placeholder="Secret value"
                  className="flex-1 px-3 py-1.5 text-sm rounded border border-neutral-300 dark:border-neutral-600 bg-white dark:bg-neutral-900"
                />
                <button
                  onClick={handleStoreCredential}
                  className="px-3 py-1.5 text-sm rounded bg-green-500 text-white hover:bg-green-600"
                >
                  Store
                </button>
              </div>
            </div>
          )}

          {/* Credential list */}
          {vaultStatus?.unlocked && credentialNames.length > 0 && (
            <div className="p-4 rounded-lg bg-neutral-50 dark:bg-neutral-800/50 border border-neutral-200 dark:border-neutral-700">
              <div className="font-medium text-sm mb-3">Stored Credentials</div>
              <div className="space-y-2">
                {credentialNames.map(name => (
                  <div key={name} className="flex items-center justify-between text-sm">
                    <span className="font-mono text-xs">{name}</span>
                    <button
                      onClick={() => handleDeleteCredential(name)}
                      className="text-xs text-red-500 hover:text-red-700"
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

      {/* ── Health Tab ─────────────────────────────────── */}
      {tab === "health" && (
        <div className="space-y-3">
          {healthStatuses.length === 0 && (
            <div className="text-center text-neutral-500 py-8">No health checks registered yet</div>
          )}
          {healthStatuses.map(h => (
            <div
              key={h.name}
              className="flex items-center justify-between p-3 rounded-lg bg-neutral-50 dark:bg-neutral-800/50 border border-neutral-200 dark:border-neutral-700"
            >
              <div>
                <div className="font-medium text-sm">{h.name}</div>
                <div className="text-xs text-neutral-500">
                  {h.state}
                  {h.latency_ms != null && ` • ${h.latency_ms}ms`}
                  {h.consecutive_failures > 0 && ` • ${h.consecutive_failures} failures`}
                </div>
                {h.last_check && (
                  <div className="text-[10px] text-neutral-400">
                    Last check: {new Date(h.last_check).toLocaleString()}
                  </div>
                )}
              </div>
              <div className="flex items-center gap-2">
                <span className={`w-2 h-2 rounded-full ${
                  h.state === "Healthy" ? "bg-green-500" :
                  h.state === "Degraded" ? "bg-yellow-500" :
                  h.state === "Unhealthy" ? "bg-red-500" :
                  "bg-neutral-400"
                }`} />
                <button
                  onClick={() => handleCheckHealth(h.name)}
                  className="text-xs px-2 py-1 rounded bg-neutral-200 dark:bg-neutral-700 hover:bg-neutral-300 dark:hover:bg-neutral-600"
                >
                  Check
                </button>
              </div>
            </div>
          ))}
        </div>
      )}
    </PageLayout>
  );
}
