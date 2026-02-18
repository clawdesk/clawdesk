import { listen } from "@tauri-apps/api/event";
import type {
  CostMetrics,
  SecurityStatus,
  AgentEventEnvelope,
  IncomingMessageEvent,
  ApprovalPendingEvent,
} from "../types";

export interface AppEventHandlers {
  onMetricsUpdated?: (metrics: CostMetrics) => void;
  onSecurityChanged?: (security: SecurityStatus) => void;
  onRoutineExecuted?: (payload: unknown) => void;
  onIncomingMessage?: (payload: IncomingMessageEvent) => void;
  onSystemAlert?: (payload: { level?: string; title?: string; message?: string }) => void;
  onAgentEvent?: (payload: AgentEventEnvelope) => void;
  onApprovalPending?: (payload: ApprovalPendingEvent) => void;
}

export async function subscribeAppEvents(handlers: AppEventHandlers): Promise<() => void> {
  const unlisten = await Promise.all([
    listen<CostMetrics>("metrics:updated", (event) => handlers.onMetricsUpdated?.(event.payload)),
    listen<SecurityStatus>("security:changed", (event) => handlers.onSecurityChanged?.(event.payload)),
    listen("routine:executed", (event) => handlers.onRoutineExecuted?.(event.payload)),
    listen<IncomingMessageEvent>("incoming:message", (event) => handlers.onIncomingMessage?.(event.payload)),
    listen<{ level?: string; title?: string; message?: string }>("system:alert", (event) => handlers.onSystemAlert?.(event.payload)),
    listen<AgentEventEnvelope>("agent-event", (event) => handlers.onAgentEvent?.(event.payload)),
    listen<ApprovalPendingEvent>("approval:pending", (event) => handlers.onApprovalPending?.(event.payload)),
  ]);

  return () => {
    unlisten.forEach((dispose) => dispose());
  };
}
