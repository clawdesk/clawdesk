// ── Typed wrappers around Tauri invoke() ──────────────────
//
// Each function maps 1:1 to a #[tauri::command] in the Rust backend.
// All return types match the Rust Serialize output exactly.
// 136 commands across 13 modules.

import { invoke } from "@tauri-apps/api/core";
import type {
  // Core
  HealthResponse,
  DesktopAgent,
  CreateAgentRequest,
  ImportResult,
  SendMessageResponse,
  SessionSummary,
  ChatMessage,
  SkillDescriptor,
  PipelineDescriptor,
  PipelineNodeDescriptor,
  CostMetrics,
  SecurityStatus,
  TraceEntry,
  TunnelMetricsSnapshot,
  InviteResponse,
  ModelInfo,
  ChannelInfo,
  // Runtime
  DurableRunInfo,
  // Media
  LinkPreviewResult,
  MediaPipelineStatus,
  // Plugins
  PluginSummary,
  // A2A
  AgentCardInfo,
  RegisterAgentCardRequest,
  // Security
  OAuthStartRequest,
  OAuthStartResponse,
  OAuthCallbackRequest,
  OAuthTokenResponse,
  AuthProfileInfo,
  CreateApprovalRequest,
  ApprovalRequestInfo,
  AclRuleRequest,
  AclCheckResult,
  GenerateTokenRequest,
  TokenInfo,
  // Discovery
  PeerInfo,
  PairingStatus,
  // Observability
  ObservabilityStatus,
  ConfigureObservabilityRequest,
  // Infra
  NotificationInfo,
  SendNotificationRequest,
  ClipboardEntryInfo,
  VoiceWakeStatus,
  VoiceWakeConfigRequest,
  IdleStatus,
  // Domain
  ContextGuardStatus,
  PromptManifestInfo,
  ProviderCapabilityInfo,
  RoutingDecisionInfo,
  SkillTrustInfo,
  SkillTriggerInfo,
  // Canvas
  CanvasSummary,
  BlockInfo,
  CreateCanvasRequest,
  AddBlockRequest,
  ConnectBlocksRequest,
  // Memory
  RememberRequest,
  RememberResponse,
  RememberBatchItem,
  RecallRequest,
  MemoryHit,
  MemoryStatsResponse,
  // SochDB
  GraphNodeInfo,
  GraphEdgeInfo,
  TraceRunInfo,
  TraceSpanInfo,
  CacheLookupInfo,
  CheckpointInfo,
  WorkflowRunInfo,
  AtomicWriteInfo,
  AgentRegistryInfo,
  SubgraphInfo,
  TemporalEdgeInfo,
} from "./types";

// ══════════════════════════════════════════════════════════════
// Health (1)
// ══════════════════════════════════════════════════════════════

export async function getHealth(): Promise<HealthResponse> {
  return invoke<HealthResponse>("get_health");
}

// ══════════════════════════════════════════════════════════════
// Agents (3)
// ══════════════════════════════════════════════════════════════

export async function createAgent(req: CreateAgentRequest): Promise<DesktopAgent> {
  return invoke<DesktopAgent>("create_agent", { request: req });
}

export async function listAgents(): Promise<DesktopAgent[]> {
  return invoke<DesktopAgent[]>("list_agents");
}

export async function deleteAgent(agentId: string): Promise<boolean> {
  return invoke<boolean>("delete_agent", { agentId });
}

// ══════════════════════════════════════════════════════════════
// Import (1)
// ══════════════════════════════════════════════════════════════

export async function importOpenClawConfig(configJson: string): Promise<ImportResult> {
  return invoke<ImportResult>("import_openclaw_config", { configJson });
}

// ══════════════════════════════════════════════════════════════
// Chat (3)
// ══════════════════════════════════════════════════════════════

export async function sendMessage(agentId: string, content: string): Promise<SendMessageResponse> {
  return invoke<SendMessageResponse>("send_message", {
    request: { agent_id: agentId, content },
  });
}

export async function getSessionMessages(agentId: string): Promise<ChatMessage[]> {
  return invoke<ChatMessage[]>("get_session_messages", { agentId });
}

export async function listSessions(): Promise<SessionSummary[]> {
  return invoke<SessionSummary[]>("list_sessions");
}

// ══════════════════════════════════════════════════════════════
// Skills (3)
// ══════════════════════════════════════════════════════════════

export async function listSkills(): Promise<SkillDescriptor[]> {
  return invoke<SkillDescriptor[]>("list_skills");
}

export async function activateSkill(skillId: string): Promise<boolean> {
  return invoke<boolean>("activate_skill", { skillId });
}

export async function deactivateSkill(skillId: string): Promise<boolean> {
  return invoke<boolean>("deactivate_skill", { skillId });
}

// ══════════════════════════════════════════════════════════════
// Pipelines (3)
// ══════════════════════════════════════════════════════════════

export async function listPipelines(): Promise<PipelineDescriptor[]> {
  return invoke<PipelineDescriptor[]>("list_pipelines");
}

export async function createPipeline(
  name: string,
  description: string,
  steps: PipelineNodeDescriptor[],
  edges: [number, number][]
): Promise<PipelineDescriptor> {
  return invoke<PipelineDescriptor>("create_pipeline", {
    request: { name, description, steps, edges },
  });
}

export async function runPipeline(pipelineId: string): Promise<unknown> {
  return invoke("run_pipeline", { pipelineId });
}

// ══════════════════════════════════════════════════════════════
// Monitoring (3)
// ══════════════════════════════════════════════════════════════

export async function getMetrics(): Promise<CostMetrics> {
  return invoke<CostMetrics>("get_metrics");
}

export async function getSecurityStatus(): Promise<SecurityStatus> {
  return invoke<SecurityStatus>("get_security_status");
}

export async function getAgentTrace(agentId?: string): Promise<TraceEntry[]> {
  return invoke<TraceEntry[]>("get_agent_trace", { agentId: agentId ?? null });
}

// ══════════════════════════════════════════════════════════════
// Tunnel (2)
// ══════════════════════════════════════════════════════════════

export async function getTunnelStatus(): Promise<TunnelMetricsSnapshot> {
  return invoke<TunnelMetricsSnapshot>("get_tunnel_status");
}

export async function createInvite(
  label: string,
  endpoint: string,
  ttlHours?: number
): Promise<InviteResponse> {
  return invoke<InviteResponse>("create_invite", {
    label, endpoint, ttlHours: ttlHours ?? null,
  });
}

// ══════════════════════════════════════════════════════════════
// Config (3)
// ══════════════════════════════════════════════════════════════

export async function getConfig(): Promise<unknown> {
  return invoke("get_config");
}

export async function listModels(): Promise<ModelInfo[]> {
  return invoke<ModelInfo[]>("list_models");
}

export async function listChannels(): Promise<ChannelInfo[]> {
  return invoke<ChannelInfo[]>("list_channels");
}

// ══════════════════════════════════════════════════════════════
// Runtime — Durable agent runs (4)
// ══════════════════════════════════════════════════════════════

export async function getRuntimeStatus(): Promise<unknown> {
  return invoke("get_runtime_status");
}

export async function cancelDurableRun(runId: string, reason?: string): Promise<boolean> {
  return invoke<boolean>("cancel_durable_run", { runId, reason: reason ?? null });
}

export async function getDurableRunStatus(runId: string): Promise<string> {
  return invoke<string>("get_durable_run_status", { runId });
}

export async function resumeDurableRun(runId: string): Promise<DurableRunInfo> {
  return invoke<DurableRunInfo>("resume_durable_run", { runId });
}

// ══════════════════════════════════════════════════════════════
// Media (2)
// ══════════════════════════════════════════════════════════════

export async function getMediaPipelineStatus(): Promise<MediaPipelineStatus> {
  return invoke<MediaPipelineStatus>("get_media_pipeline_status");
}

export async function getLinkPreview(url: string): Promise<LinkPreviewResult> {
  return invoke<LinkPreviewResult>("get_link_preview", { url });
}

// ══════════════════════════════════════════════════════════════
// Plugins (4)
// ══════════════════════════════════════════════════════════════

export async function listPlugins(): Promise<PluginSummary[]> {
  return invoke<PluginSummary[]>("list_plugins");
}

export async function getPluginInfo(pluginId: string): Promise<PluginSummary> {
  return invoke<PluginSummary>("get_plugin_info", { pluginId });
}

export async function enablePlugin(pluginId: string): Promise<boolean> {
  return invoke<boolean>("enable_plugin", { pluginId });
}

export async function disablePlugin(pluginId: string): Promise<boolean> {
  return invoke<boolean>("disable_plugin", { pluginId });
}

// ══════════════════════════════════════════════════════════════
// A2A Protocol (5)
// ══════════════════════════════════════════════════════════════

export async function listA2aAgents(): Promise<AgentCardInfo[]> {
  return invoke<AgentCardInfo[]>("list_a2a_agents");
}

export async function registerA2aAgent(request: RegisterAgentCardRequest): Promise<boolean> {
  return invoke<boolean>("register_a2a_agent", { request });
}

export async function deregisterA2aAgent(agentId: string): Promise<boolean> {
  return invoke<boolean>("deregister_a2a_agent", { agentId });
}

export async function getAgentCard(agentId: string): Promise<unknown> {
  return invoke("get_agent_card", { agentId });
}

export async function getSelfAgentCard(): Promise<unknown> {
  return invoke("get_self_agent_card");
}

// ══════════════════════════════════════════════════════════════
// Security — OAuth2, Approvals, ACL, Tokens (14)
// ══════════════════════════════════════════════════════════════

export async function startOAuthFlow(request: OAuthStartRequest): Promise<OAuthStartResponse> {
  return invoke<OAuthStartResponse>("start_oauth_flow", { request });
}

export async function handleOAuthCallback(request: OAuthCallbackRequest): Promise<OAuthTokenResponse> {
  return invoke<OAuthTokenResponse>("handle_oauth_callback", { request });
}

export async function refreshOAuthToken(provider: string): Promise<OAuthTokenResponse> {
  return invoke<OAuthTokenResponse>("refresh_oauth_token", { provider });
}

export async function listAuthProfiles(provider?: string): Promise<AuthProfileInfo[]> {
  return invoke<AuthProfileInfo[]>("list_auth_profiles", { provider: provider ?? null });
}

export async function removeAuthProfile(provider: string, profileId: string): Promise<boolean> {
  return invoke<boolean>("remove_auth_profile", { provider, profileId });
}

export async function createApprovalRequest(request: CreateApprovalRequest): Promise<ApprovalRequestInfo> {
  return invoke<ApprovalRequestInfo>("create_approval_request", { request });
}

export async function approveRequest(requestId: string, approver: string): Promise<boolean> {
  return invoke<boolean>("approve_request", { requestId, approver });
}

export async function denyRequest(requestId: string, approver: string, reason?: string): Promise<boolean> {
  return invoke<boolean>("deny_request", { requestId, approver, reason: reason ?? null });
}

export async function getApprovalStatus(requestId: string): Promise<string> {
  return invoke<string>("get_approval_status", { requestId });
}

export async function addAclRule(request: AclRuleRequest): Promise<boolean> {
  return invoke<boolean>("add_acl_rule", { request });
}

export async function checkPermission(
  principalType: string,
  principalId: string,
  resourceType: string,
  resourceId: string,
  action: string
): Promise<AclCheckResult> {
  return invoke<AclCheckResult>("check_permission", {
    principalType, principalId, resourceType, resourceId, action,
  });
}

export async function revokeAclRules(principalType: string, principalId: string): Promise<boolean> {
  return invoke<boolean>("revoke_acl_rules", { principalType, principalId });
}

export async function generateToken(request: GenerateTokenRequest): Promise<TokenInfo> {
  return invoke<TokenInfo>("generate_token", { request });
}

export async function validateToken(encodedToken: string): Promise<TokenInfo> {
  return invoke<TokenInfo>("validate_token", { encodedToken });
}

// ══════════════════════════════════════════════════════════════
// Discovery (5)
// ══════════════════════════════════════════════════════════════

export async function getMdnsServiceInfo(): Promise<unknown> {
  return invoke("get_mdns_service_info");
}

export async function startPairing(): Promise<PairingStatus> {
  return invoke<PairingStatus>("start_pairing");
}

export async function completePairing(code: string, peerName: string): Promise<boolean> {
  return invoke<boolean>("complete_pairing", { code, peerName });
}

export async function getPairingStatus(): Promise<PairingStatus | null> {
  return invoke<PairingStatus | null>("get_pairing_status");
}

export async function listDiscoveredPeers(): Promise<PeerInfo[]> {
  return invoke<PeerInfo[]>("list_discovered_peers");
}

// ══════════════════════════════════════════════════════════════
// Observability (2)
// ══════════════════════════════════════════════════════════════

export async function getObservabilityConfig(): Promise<ObservabilityStatus> {
  return invoke<ObservabilityStatus>("get_observability_config");
}

export async function configureObservability(request: ConfigureObservabilityRequest): Promise<ObservabilityStatus> {
  return invoke<ObservabilityStatus>("configure_observability", { request });
}

// ══════════════════════════════════════════════════════════════
// Infra — Notifications, Clipboard, Voice, Idle (9)
// ══════════════════════════════════════════════════════════════

export async function sendNotification(request: SendNotificationRequest): Promise<NotificationInfo> {
  return invoke<NotificationInfo>("send_notification", { request });
}

export async function listNotifications(): Promise<NotificationInfo[]> {
  return invoke<NotificationInfo[]>("list_notifications");
}

export async function readClipboard(): Promise<ClipboardEntryInfo | null> {
  return invoke<ClipboardEntryInfo | null>("read_clipboard");
}

export async function writeClipboard(text: string): Promise<boolean> {
  return invoke<boolean>("write_clipboard", { text });
}

export async function getClipboardHistory(limit?: number): Promise<ClipboardEntryInfo[]> {
  return invoke<ClipboardEntryInfo[]>("get_clipboard_history", { limit: limit ?? null });
}

export async function configureVoiceWake(request: VoiceWakeConfigRequest): Promise<VoiceWakeStatus> {
  return invoke<VoiceWakeStatus>("configure_voice_wake", { request });
}

export async function getVoiceWakeStatus(): Promise<VoiceWakeStatus> {
  return invoke<VoiceWakeStatus>("get_voice_wake_status");
}

export async function getIdleStatus(): Promise<IdleStatus> {
  return invoke<IdleStatus>("get_idle_status");
}

export async function recordActivity(): Promise<boolean> {
  return invoke<boolean>("record_activity");
}

// ══════════════════════════════════════════════════════════════
// Domain — Context Guard, Prompt, Provider, Skills (6)
// ══════════════════════════════════════════════════════════════

export async function getContextGuardStatus(agentId?: string): Promise<ContextGuardStatus> {
  return invoke<ContextGuardStatus>("get_context_guard_status", { agentId: agentId ?? null });
}

export async function getPromptManifest(agentId: string): Promise<PromptManifestInfo | null> {
  return invoke<PromptManifestInfo | null>("get_prompt_manifest", { agentId });
}

export async function listProviderCapabilities(): Promise<ProviderCapabilityInfo[]> {
  return invoke<ProviderCapabilityInfo[]>("list_provider_capabilities");
}

export async function getProviderRouting(model: string, requiredCaps: string[]): Promise<RoutingDecisionInfo> {
  return invoke<RoutingDecisionInfo>("get_provider_routing", { model, requiredCaps });
}

export async function getSkillTrustLevel(skillId: string): Promise<SkillTrustInfo> {
  return invoke<SkillTrustInfo>("get_skill_trust_level", { skillId });
}

export async function evaluateSkillTriggers(messageText: string): Promise<SkillTriggerInfo[]> {
  return invoke<SkillTriggerInfo[]>("evaluate_skill_triggers", { messageText });
}

// ══════════════════════════════════════════════════════════════
// Canvas (7)
// ══════════════════════════════════════════════════════════════

export async function createCanvas(request: CreateCanvasRequest): Promise<CanvasSummary> {
  return invoke<CanvasSummary>("create_canvas", { request });
}

export async function getCanvas(canvasId: string): Promise<unknown> {
  return invoke("get_canvas", { canvasId });
}

export async function listCanvases(): Promise<CanvasSummary[]> {
  return invoke<CanvasSummary[]>("list_canvases");
}

export async function addCanvasBlock(request: AddBlockRequest): Promise<BlockInfo> {
  return invoke<BlockInfo>("add_canvas_block", { request });
}

export async function removeCanvasBlock(canvasId: string, blockId: string): Promise<boolean> {
  return invoke<boolean>("remove_canvas_block", { canvasId, blockId });
}

export async function connectCanvasBlocks(request: ConnectBlocksRequest): Promise<boolean> {
  return invoke<boolean>("connect_canvas_blocks", { request });
}

export async function exportCanvasMarkdown(canvasId: string): Promise<string> {
  return invoke<string>("export_canvas_markdown", { canvasId });
}

// ══════════════════════════════════════════════════════════════
// Memory (5)
// ══════════════════════════════════════════════════════════════

export async function rememberMemory(request: RememberRequest): Promise<RememberResponse> {
  return invoke<RememberResponse>("remember_memory", { request });
}

export async function rememberBatch(items: RememberBatchItem[]): Promise<string[]> {
  return invoke<string[]>("remember_batch", { request: { items } });
}

export async function recallMemories(request: RecallRequest): Promise<MemoryHit[]> {
  return invoke<MemoryHit[]>("recall_memories", { request });
}

export async function forgetMemory(id: string): Promise<boolean> {
  return invoke<boolean>("forget_memory", { request: { id } });
}

export async function getMemoryStats(): Promise<MemoryStatsResponse> {
  return invoke<MemoryStatsResponse>("get_memory_stats");
}

// ══════════════════════════════════════════════════════════════
// SochDB — Semantic Cache (3)
// ══════════════════════════════════════════════════════════════

export async function cacheLookup(
  query: string,
  namespace: string,
  queryEmbedding?: number[]
): Promise<CacheLookupInfo> {
  return invoke<CacheLookupInfo>("cache_lookup", {
    query, namespace, queryEmbedding: queryEmbedding ?? null,
  });
}

export async function cacheStore(
  query: string,
  namespace: string,
  result: string,
  embedding?: number[],
  sourceDocs?: string[],
  ttlSecs?: number
): Promise<string> {
  return invoke<string>("cache_store", {
    query, namespace, result,
    embedding: embedding ?? null,
    sourceDocs: sourceDocs ?? [],
    ttlSecs: ttlSecs ?? null,
  });
}

export async function cacheInvalidateSource(docId: string): Promise<number> {
  return invoke<number>("cache_invalidate_source", { docId });
}

// ══════════════════════════════════════════════════════════════
// SochDB — Trace Store (8)
// ══════════════════════════════════════════════════════════════

export async function traceStartRun(
  name: string,
  resource: Record<string, string>
): Promise<TraceRunInfo> {
  return invoke<TraceRunInfo>("trace_start_run", { name, resource });
}

export async function traceEndRun(traceId: string): Promise<void> {
  return invoke<void>("trace_end_run", { traceId });
}

export async function traceStartSpan(
  traceId: string,
  name: string,
  parentSpanId?: string,
  kind?: string
): Promise<TraceSpanInfo> {
  return invoke<TraceSpanInfo>("trace_start_span", {
    traceId, name,
    parentSpanId: parentSpanId ?? null,
    kind: kind ?? "Internal",
  });
}

export async function traceEndSpan(
  traceId: string,
  spanId: string,
  status: string,
  message?: string
): Promise<void> {
  return invoke<void>("trace_end_span", {
    traceId, spanId, status, message: message ?? null,
  });
}

export async function traceGetSpans(traceId: string): Promise<TraceSpanInfo[]> {
  return invoke<TraceSpanInfo[]>("trace_get_spans", { traceId });
}

export async function traceGetRun(traceId: string): Promise<TraceRunInfo | null> {
  return invoke<TraceRunInfo | null>("trace_get_run", { traceId });
}

export async function traceUpdateMetrics(
  traceId: string,
  tokens: number,
  costMillicents: number
): Promise<void> {
  return invoke<void>("trace_update_metrics", { traceId, tokens, costMillicents });
}

export async function traceLogToolCall(
  traceId: string,
  spanId: string,
  toolName: string,
  args: string,
  result?: string,
  durationUs?: number,
  success?: boolean
): Promise<void> {
  return invoke<void>("trace_log_tool_call", {
    traceId, spanId, toolName, arguments: args,
    result: result ?? null,
    durationUs: durationUs ?? 0,
    success: success ?? true,
  });
}

// ══════════════════════════════════════════════════════════════
// SochDB — Checkpoint Store (6)
// ══════════════════════════════════════════════════════════════

export async function checkpointCreateRun(
  runId: string,
  workflow: string,
  params: Record<string, unknown>
): Promise<WorkflowRunInfo> {
  return invoke<WorkflowRunInfo>("checkpoint_create_run", { runId, workflow, params });
}

export async function checkpointSave(
  runId: string,
  nodeId: string,
  stateJson: string,
  metadata?: Record<string, string>
): Promise<CheckpointInfo> {
  return invoke<CheckpointInfo>("checkpoint_save", {
    runId, nodeId, stateJson, metadata: metadata ?? null,
  });
}

export async function checkpointLoad(runId: string, nodeId: string): Promise<string | null> {
  return invoke<string | null>("checkpoint_load", { runId, nodeId });
}

export async function checkpointList(runId: string): Promise<CheckpointInfo[]> {
  return invoke<CheckpointInfo[]>("checkpoint_list", { runId });
}

export async function checkpointGetRun(runId: string): Promise<WorkflowRunInfo | null> {
  return invoke<WorkflowRunInfo | null>("checkpoint_get_run", { runId });
}

export async function checkpointDeleteRun(runId: string): Promise<boolean> {
  return invoke<boolean>("checkpoint_delete_run", { runId });
}

// ══════════════════════════════════════════════════════════════
// SochDB — Knowledge Graph (8)
// ══════════════════════════════════════════════════════════════

export async function graphAddNode(
  nodeId: string,
  nodeType: string,
  properties?: Record<string, unknown>
): Promise<GraphNodeInfo> {
  return invoke<GraphNodeInfo>("graph_add_node", {
    nodeId, nodeType, properties: properties ?? null,
  });
}

export async function graphGetNode(nodeId: string): Promise<GraphNodeInfo | null> {
  return invoke<GraphNodeInfo | null>("graph_get_node", { nodeId });
}

export async function graphDeleteNode(nodeId: string, cascade: boolean): Promise<boolean> {
  return invoke<boolean>("graph_delete_node", { nodeId, cascade });
}

export async function graphAddEdge(
  fromId: string,
  edgeType: string,
  toId: string,
  properties?: Record<string, unknown>
): Promise<GraphEdgeInfo> {
  return invoke<GraphEdgeInfo>("graph_add_edge", {
    fromId, edgeType, toId, properties: properties ?? null,
  });
}

export async function graphGetEdges(fromId: string, edgeType?: string): Promise<GraphEdgeInfo[]> {
  return invoke<GraphEdgeInfo[]>("graph_get_edges", {
    fromId, edgeType: edgeType ?? null,
  });
}

export async function graphShortestPath(
  fromId: string,
  toId: string,
  maxDepth: number
): Promise<string[] | null> {
  return invoke<string[] | null>("graph_shortest_path", { fromId, toId, maxDepth });
}

export async function graphGetSubgraph(startId: string, maxDepth: number): Promise<SubgraphInfo> {
  return invoke<SubgraphInfo>("graph_get_subgraph", { startId, maxDepth });
}

export async function graphGetNodesByType(nodeType: string, limit: number): Promise<GraphNodeInfo[]> {
  return invoke<GraphNodeInfo[]>("graph_get_nodes_by_type", { nodeType, limit });
}

// ══════════════════════════════════════════════════════════════
// SochDB — Temporal Graph (4)
// ══════════════════════════════════════════════════════════════

export async function temporalAddEdge(
  fromId: string,
  edgeType: string,
  toId: string,
  properties?: Record<string, unknown>
): Promise<TemporalEdgeInfo> {
  return invoke<TemporalEdgeInfo>("temporal_add_edge", {
    fromId, edgeType, toId, properties: properties ?? null,
  });
}

export async function temporalInvalidateEdge(
  fromId: string,
  edgeType: string,
  toId: string
): Promise<boolean> {
  return invoke<boolean>("temporal_invalidate_edge", { fromId, edgeType, toId });
}

export async function temporalEdgesAt(
  fromId: string,
  edgeType: string | null,
  atTime: number
): Promise<TemporalEdgeInfo[]> {
  return invoke<TemporalEdgeInfo[]>("temporal_edges_at", {
    fromId, edgeType, atTime,
  });
}

export async function temporalEdgeHistory(
  fromId: string,
  edgeType: string,
  toId: string
): Promise<TemporalEdgeInfo[]> {
  return invoke<TemporalEdgeInfo[]>("temporal_edge_history", { fromId, edgeType, toId });
}

// ══════════════════════════════════════════════════════════════
// SochDB — Policy Engine (3)
// ══════════════════════════════════════════════════════════════

export async function policyEnableAudit(): Promise<void> {
  return invoke<void>("policy_enable_audit");
}

export async function policyGetAuditLog(limit: number): Promise<unknown[]> {
  return invoke<unknown[]>("policy_get_audit_log", { limit });
}

export async function policyAddRateLimit(
  operation: string,
  maxPerMinute: number,
  scope: string
): Promise<void> {
  return invoke<void>("policy_add_rate_limit", { operation, maxPerMinute, scope });
}

// ══════════════════════════════════════════════════════════════
// SochDB — Atomic Memory (2)
// ══════════════════════════════════════════════════════════════

export async function atomicMemoryWrite(
  memoryId: string,
  blobs: [string, string][],
  graphNodes: [string, string, string][],
  graphEdges: [string, string, string, string][]
): Promise<AtomicWriteInfo> {
  return invoke<AtomicWriteInfo>("atomic_memory_write", {
    memoryId, blobs, graphNodes, graphEdges,
  });
}

export async function atomicMemoryRecover(): Promise<unknown> {
  return invoke("atomic_memory_recover");
}

// ══════════════════════════════════════════════════════════════
// SochDB — Agent Registry (4)
// ══════════════════════════════════════════════════════════════

export async function registryRegisterAgent(
  agentId: string,
  capabilities: string[]
): Promise<AgentRegistryInfo> {
  return invoke<AgentRegistryInfo>("registry_register_agent", { agentId, capabilities });
}

export async function registryListAgents(): Promise<AgentRegistryInfo[]> {
  return invoke<AgentRegistryInfo[]>("registry_list_agents");
}

export async function registryFindCapable(
  required: string[],
  exclude: string[]
): Promise<AgentRegistryInfo[]> {
  return invoke<AgentRegistryInfo[]>("registry_find_capable", { required, exclude });
}

export async function registryUnregisterAgent(agentId: string): Promise<boolean> {
  return invoke<boolean>("registry_unregister_agent", { agentId });
}

// ══════════════════════════════════════════════════════════════
// SochDB — Core operations (2)
// ══════════════════════════════════════════════════════════════

export async function sochdbCheckpoint(): Promise<number> {
  return invoke<number>("sochdb_checkpoint");
}

export async function sochdbSync(): Promise<void> {
  return invoke<void>("sochdb_sync");
}
