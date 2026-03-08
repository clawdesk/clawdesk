import { listen } from "@tauri-apps/api/event";
import type {
  CostMetrics,
  SecurityStatus,
  AgentEventEnvelope,
  IncomingMessageEvent,
  ApprovalPendingEvent,
  AskHumanPendingEvent,
  PipelineStepEvent,
} from "../types";

export interface AppEventHandlers {
  onMetricsUpdated?: (metrics: CostMetrics) => void;
  onSecurityChanged?: (security: SecurityStatus) => void;
  onRoutineExecuted?: (payload: unknown) => void;
  onIncomingMessage?: (payload: IncomingMessageEvent) => void;
  onSystemAlert?: (payload: { level?: string; title?: string; message?: string }) => void;
  onAgentEvent?: (payload: AgentEventEnvelope) => void;
  onApprovalPending?: (payload: ApprovalPendingEvent) => void;
  onAskHumanPending?: (payload: AskHumanPendingEvent) => void;
  onPipelineStepStart?: (payload: PipelineStepEvent) => void;
  onPipelineStepEnd?: (payload: PipelineStepEvent) => void;
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
    listen<AskHumanPendingEvent>("ask-human:pending", (event) => handlers.onAskHumanPending?.(event.payload)),
    listen<PipelineStepEvent>("pipeline:step_start", (event) => handlers.onPipelineStepStart?.(event.payload)),
    listen<PipelineStepEvent>("pipeline:step_end", (event) => handlers.onPipelineStepEnd?.(event.payload)),
  ]);

  return () => {
    unlisten.forEach((dispose) => dispose());
  };
}
