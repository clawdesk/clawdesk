import { useState, useEffect, useCallback } from "react";
import * as api from "../api";
import { PageLayout } from "../components/PageLayout";
import { Icon } from "../components/Icon";
import type {
  IntegrationInfo,
  IntegrationCategoryInfo,
  VaultStatusInfo,
  HealthStatusInfo,
} from "../types";

// ── Types ─────────────────────────────────────────────────────

type ExtTab = "integrations" | "vault" | "health";

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
            {filtered.map(i => (
              <div
                key={i.name}
                className="flex items-center justify-between p-3 rounded-lg bg-neutral-50 dark:bg-neutral-800/50 border border-neutral-200 dark:border-neutral-700"
              >
                <div className="flex items-center gap-3">
                  <span className="text-2xl">{i.icon}</span>
                  <div>
                    <div className="font-medium text-sm">{i.name}</div>
                    <div className="text-xs text-neutral-500">{i.description}</div>
                    <div className="flex gap-1 mt-1">
                      <span className="text-[10px] px-1.5 py-0.5 rounded bg-neutral-200 dark:bg-neutral-700">{i.category}</span>
                      {i.has_oauth && <span className="text-[10px] px-1.5 py-0.5 rounded bg-green-100 dark:bg-green-900 text-green-700 dark:text-green-300">OAuth</span>}
                      {i.credentials_required.length > 0 && (
                        <span className="text-[10px] px-1.5 py-0.5 rounded bg-yellow-100 dark:bg-yellow-900 text-yellow-700 dark:text-yellow-300">
                          {i.credentials_required.length} credential{i.credentials_required.length > 1 ? "s" : ""}
                        </span>
                      )}
                    </div>
                  </div>
                </div>
                <div className="flex items-center gap-2">
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
            ))}
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
