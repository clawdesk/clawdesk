export type AppNavKey = "chat" | "ide" | "overview" | "a2a" | "runtime" | "skills" | "automations" | "agents" | "channels" | "files" | "settings" | "logs" | "extensions" | "mcp" | "local-models" | "documents";

export interface AppRoute {
  nav: AppNavKey;
  threadId?: string;
}

const NAV_SET = new Set<AppNavKey>(["chat", "ide", "overview", "a2a", "runtime", "skills", "automations", "agents", "channels", "files", "settings", "logs", "extensions", "mcp", "local-models", "documents"]);

// Legacy nav keys → new mapping
const LEGACY_MAP: Record<string, AppNavKey> = {
  now: "chat",
  ask: "chat",
  routines: "automations",
  accounts: "settings",
  library: "skills",
  channels: "channels",
  sessions: "chat",
  usage: "settings",
  nodes: "settings",
};

function decode(value: string | undefined): string | undefined {
  if (!value) return undefined;
  try {
    return decodeURIComponent(value);
  } catch {
    return value;
  }
}

function normalizeNav(value: string | undefined): AppNavKey {
  if (value && NAV_SET.has(value as AppNavKey)) {
    return value as AppNavKey;
  }
  if (value && LEGACY_MAP[value]) {
    return LEGACY_MAP[value];
  }
  return "chat";
}

export function parseRouteHash(hash: string): AppRoute {
  const raw = hash.startsWith("#") ? hash.slice(1) : hash;
  const parts = raw.split("/").filter(Boolean);
  const nav = normalizeNav(parts[0]);
  const threadId = nav === "chat" ? decode(parts[1]) : undefined;
  return { nav, threadId };
}

export function buildRouteHash(route: AppRoute): string {
  const nav = normalizeNav(route.nav);
  if (nav === "chat" && route.threadId) {
    return `#/chat/${encodeURIComponent(route.threadId)}`;
  }
  return `#/${nav}`;
}

