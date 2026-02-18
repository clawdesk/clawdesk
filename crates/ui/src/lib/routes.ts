export type AppNavKey = "now" | "ask" | "routines" | "accounts" | "library";

export interface AppRoute {
  nav: AppNavKey;
  threadId?: string;
}

const NAV_SET = new Set<AppNavKey>(["now", "ask", "routines", "accounts", "library"]);

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
  return "now";
}

export function parseRouteHash(hash: string): AppRoute {
  const raw = hash.startsWith("#") ? hash.slice(1) : hash;
  const parts = raw.split("/").filter(Boolean);
  const nav = normalizeNav(parts[0]);
  const threadId = nav === "ask" ? decode(parts[1]) : undefined;
  return { nav, threadId };
}

export function buildRouteHash(route: AppRoute): string {
  const nav = normalizeNav(route.nav);
  if (nav === "ask" && route.threadId) {
    return `#/ask/${encodeURIComponent(route.threadId)}`;
  }
  return `#/${nav}`;
}

