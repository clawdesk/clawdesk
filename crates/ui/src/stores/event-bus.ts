import { listen } from "@tauri-apps/api/event";
import type { CostMetrics, SecurityStatus } from "../types";

export interface AppEventHandlers {
  onMetricsUpdated?: (metrics: CostMetrics) => void;
  onSecurityChanged?: (security: SecurityStatus) => void;
  onRoutineExecuted?: (payload: unknown) => void;
  onIncomingMessage?: (payload: unknown) => void;
  onSystemAlert?: (payload: { level?: string; title?: string; message?: string }) => void;
  onAgentEvent?: (payload: unknown) => void;
}

export async function subscribeAppEvents(handlers: AppEventHandlers): Promise<() => void> {
  const unlisten = await Promise.all([
    listen<CostMetrics>("metrics:updated", (event) => handlers.onMetricsUpdated?.(event.payload)),
    listen<SecurityStatus>("security:changed", (event) => handlers.onSecurityChanged?.(event.payload)),
    listen("routine:executed", (event) => handlers.onRoutineExecuted?.(event.payload)),
    listen("incoming:message", (event) => handlers.onIncomingMessage?.(event.payload)),
    listen<{ level?: string; title?: string; message?: string }>("system:alert", (event) => handlers.onSystemAlert?.(event.payload)),
    listen("agent-event", (event) => handlers.onAgentEvent?.(event.payload)),
  ]);

  return () => {
    unlisten.forEach((dispose) => dispose());
  };
}

