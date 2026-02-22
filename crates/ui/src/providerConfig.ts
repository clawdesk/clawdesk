/**
 * Multi-provider configuration — shared between SettingsPage and ChatPage.
 *
 * Each configured provider is stored in localStorage as a JSON array
 * under the key `clawdesk.providers`.  One entry is marked as the
 * active default via `clawdesk.active_provider_id`.
 *
 * Legacy single-value keys (`clawdesk.provider`, `clawdesk.model`, etc.)
 * are kept in sync with the active provider for backward compatibility.
 */

export interface ProviderConfig {
  /** Unique id (nanoid-style, generated on creation) */
  id: string;
  /** Provider name — must match a key in PROVIDER_MODELS */
  provider: string;
  /** Selected model id */
  model: string;
  /** API key (empty string for Ollama) */
  apiKey: string;
  /** Base URL / endpoint (optional) */
  baseUrl: string;
  /** GCP project id (Vertex AI only) */
  projectId: string;
  /** GCP location (Vertex AI only) */
  location: string;
  /** Human-readable label (e.g. "Work Azure", "Personal Anthropic") */
  label: string;
}

const STORAGE_KEY = "clawdesk.providers";
const ACTIVE_KEY = "clawdesk.active_provider_id";

/** Generate a short random id */
function nanoid(): string {
  return Math.random().toString(36).slice(2, 10);
}

/** Load all configured providers from localStorage */
export function loadProviders(): ProviderConfig[] {
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (raw) return JSON.parse(raw) as ProviderConfig[];
  } catch { /* ignore parse errors */ }

  // Migration: if no providers array exists, create one from legacy single-value keys
  const legacyProvider = window.localStorage.getItem("clawdesk.provider");
  if (legacyProvider) {
    const migrated: ProviderConfig = {
      id: nanoid(),
      provider: legacyProvider,
      model: window.localStorage.getItem("clawdesk.model") || "",
      apiKey: window.localStorage.getItem("clawdesk.api_key") || "",
      baseUrl: window.localStorage.getItem("clawdesk.base_url") || "",
      projectId: window.localStorage.getItem("clawdesk.project_id") || "",
      location: window.localStorage.getItem("clawdesk.location") || "",
      label: legacyProvider,
    };
    saveProviders([migrated]);
    setActiveProviderId(migrated.id);
    return [migrated];
  }

  return [];
}

/** Persist providers array to localStorage */
export function saveProviders(configs: ProviderConfig[]): void {
  window.localStorage.setItem(STORAGE_KEY, JSON.stringify(configs));
}

/** Get the active provider id */
export function getActiveProviderId(): string | null {
  return window.localStorage.getItem(ACTIVE_KEY);
}

/** Set the active provider id and sync legacy keys */
export function setActiveProviderId(id: string): void {
  window.localStorage.setItem(ACTIVE_KEY, id);
  // Sync legacy keys so ChatPage/sendMessage picks them up
  const configs = loadProviders();
  const active = configs.find((c) => c.id === id);
  if (active) {
    syncLegacyKeys(active);
  }
}

/** Sync legacy localStorage keys from a provider config */
export function syncLegacyKeys(config: ProviderConfig): void {
  window.localStorage.setItem("clawdesk.provider", config.provider);
  window.localStorage.setItem("clawdesk.model", config.model);
  window.localStorage.setItem("clawdesk.api_key", config.apiKey);
  if (config.apiKey) window.localStorage.setItem("clawdesk.api_key.configured", "1");
  window.localStorage.setItem("clawdesk.base_url", config.baseUrl);
  window.localStorage.setItem("clawdesk.project_id", config.projectId);
  window.localStorage.setItem("clawdesk.location", config.location);
}

/** Get the active provider config (or first one, or null) */
export function getActiveProvider(): ProviderConfig | null {
  const configs = loadProviders();
  if (configs.length === 0) return null;
  const activeId = getActiveProviderId();
  return configs.find((c) => c.id === activeId) || configs[0];
}

/** Create a new blank provider config */
export function createProviderConfig(provider: string): ProviderConfig {
  return {
    id: nanoid(),
    provider,
    model: "",
    apiKey: "",
    baseUrl: "",
    projectId: "",
    location: "",
    label: provider,
  };
}

/** Find the provider config that owns a given model id */
export function findProviderForModel(
  configs: ProviderConfig[],
  modelId: string
): ProviderConfig | null {
  // Exact match on configured model
  const exact = configs.find((c) => c.model === modelId);
  if (exact) return exact;
  return null;
}
