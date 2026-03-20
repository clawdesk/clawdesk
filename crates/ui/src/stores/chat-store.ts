/**
 * Chat Store — single-atom state for the entire conversation subsystem.
 *
 * Uses Zustand with `subscribeWithSelector` middleware so each consumer
 * component re-renders only when its slice changes — O(1) equality check
 * per selector, not O(N) prop diffing.
 *
 * ## State topology
 *
 * The store is a flat product type with three logical partitions:
 *
 *   State = ThreadSlice × MessageSlice × ModelSlice × ErrorSlice
 *
 * - **ThreadSlice**: thread list + active thread ID. Switching threads
 *   clears messages (they're loaded lazily from the backend).
 * - **MessageSlice**: ordered message array + streaming cursor. The hot
 *   path (`appendStreamChunk`) copies the array once per token — this is
 *   the minimum cost for immutable updates that Zustand can diff.
 * - **ModelSlice**: selected model/provider + per-request override.
 *   `selectEffectiveModel` computes: override ?? selected ?? null.
 *
 * ## Streaming lifecycle
 *
 *   startSending → addMessage(user) → addMessage(placeholder) →
 *   startStreaming(id) → appendStreamChunk* → finishStreaming(meta) →
 *   finishSending
 *
 * Cancellation sets `abortRef.current = true` in the hook layer;
 * the store just resets its streaming flags.
 */

import { create } from "zustand";
import { subscribeWithSelector } from "zustand/middleware";

// ── Types ──────────────────────────────────────────

export interface ThreadMessage {
  id: string;
  role: "user" | "assistant" | "system";
  text: string;
  thinkingText?: string;
  time: string;
  agent?: string;
  model?: string;
  skills?: string[];
  tokens?: number;
  cost?: number;
  duration?: number;
  toolCalls?: ToolCallInfo[];
  isStreaming?: boolean;
  attachments?: MessageAttachment[];
  askHuman?: AskHumanRequest;
}

export interface ToolCallInfo {
  id: string;
  name: string;
  status: "running" | "done" | "error";
  args?: string;
  result?: string;
  durationMs?: number;
}

export interface MessageAttachment {
  data: string;
  mime_type: string;
  filename?: string;
}

export interface AskHumanRequest {
  requestId: string;
  question: string;
  options: string[];
  urgent: boolean;
}

export interface Thread {
  id: string;
  agentId?: string;
  title: string;
  lastActivity: string;
  pendingApprovals: number;
  messageCount: number;
}

// ── Store State ────────────────────────────────────

export interface ChatState {
  // Thread state
  threads: Thread[];
  activeThreadId: string | null;

  // Message state
  messages: ThreadMessage[];
  isStreaming: boolean;
  isSending: boolean;
  streamingMessageId: string | null;

  // Model state
  selectedModel: string | null;
  selectedProvider: string | null;
  modelOverride: string | null;

  // Error state
  lastError: string | null;
}

export interface ChatActions {
  // Thread actions
  setThreads: (threads: Thread[]) => void;
  setActiveThread: (threadId: string | null) => void;
  addThread: (thread: Thread) => void;
  removeThread: (threadId: string) => void;
  updateThread: (threadId: string, updates: Partial<Thread>) => void;

  // Message actions
  setMessages: (messages: ThreadMessage[]) => void;
  addMessage: (message: ThreadMessage) => void;
  updateMessage: (messageId: string, updates: Partial<ThreadMessage>) => void;
  appendStreamChunk: (chunk: string) => void;
  appendThinkingChunk: (chunk: string) => void;

  // Streaming lifecycle
  startStreaming: (messageId: string) => void;
  finishStreaming: (finalMessage?: Partial<ThreadMessage>) => void;
  cancelStreaming: () => void;

  // Sending lifecycle
  startSending: () => void;
  finishSending: () => void;

  // Model actions
  setSelectedModel: (model: string | null) => void;
  setSelectedProvider: (provider: string | null) => void;
  setModelOverride: (override: string | null) => void;

  // Error
  setError: (error: string | null) => void;
  clearError: () => void;

  // Reset
  reset: () => void;
}

export type ChatStore = ChatState & ChatActions;

// ── Initial State ──────────────────────────────────

const initialState: ChatState = {
  threads: [],
  activeThreadId: null,
  messages: [],
  isStreaming: false,
  isSending: false,
  streamingMessageId: null,
  selectedModel: null,
  selectedProvider: null,
  modelOverride: null,
  lastError: null,
};

// ── Store ──────────────────────────────────────────

export const useChatStore = create<ChatStore>()(
  subscribeWithSelector((set, get) => ({
    ...initialState,

    // ── Thread actions ─────────────────────────────

    setThreads: (threads) => set({ threads }),

    setActiveThread: (threadId) =>
      set({
        activeThreadId: threadId,
        messages: [], // Clear messages when switching threads
        isStreaming: false,
        streamingMessageId: null,
        lastError: null,
      }),

    addThread: (thread) =>
      set((state) => ({
        threads: [thread, ...state.threads],
      })),

    removeThread: (threadId) =>
      set((state) => ({
        threads: state.threads.filter((t) => t.id !== threadId),
        // Clear active if deleted
        ...(state.activeThreadId === threadId
          ? { activeThreadId: null, messages: [] }
          : {}),
      })),

    updateThread: (threadId, updates) =>
      set((state) => ({
        threads: state.threads.map((t) =>
          t.id === threadId ? { ...t, ...updates } : t
        ),
      })),

    // ── Message actions ─────────────────────────────

    setMessages: (messages) => set({ messages }),

    addMessage: (message) =>
      set((state) => ({
        messages: [...state.messages, message],
      })),

    updateMessage: (messageId, updates) =>
      set((state) => ({
        messages: state.messages.map((m) =>
          m.id === messageId ? { ...m, ...updates } : m
        ),
      })),

    /**
     * Append a text chunk to the currently streaming message.
     *
     * This is the hot path during LLM streaming — called for every token.
     * We mutate in-place for efficiency (React will still re-render because
     * Zustand creates a new top-level reference).
     */
    appendStreamChunk: (chunk) =>
      set((state) => {
        const { streamingMessageId, messages } = state;
        if (!streamingMessageId) return state;

        const msgIdx = messages.findIndex((m) => m.id === streamingMessageId);
        if (msgIdx === -1) return state;

        const updated = [...messages];
        updated[msgIdx] = {
          ...updated[msgIdx],
          text: updated[msgIdx].text + chunk,
        };
        return { messages: updated };
      }),

    appendThinkingChunk: (chunk) =>
      set((state) => {
        const { streamingMessageId, messages } = state;
        if (!streamingMessageId) return state;

        const msgIdx = messages.findIndex((m) => m.id === streamingMessageId);
        if (msgIdx === -1) return state;

        const updated = [...messages];
        updated[msgIdx] = {
          ...updated[msgIdx],
          thinkingText: (updated[msgIdx].thinkingText || "") + chunk,
        };
        return { messages: updated };
      }),

    // ── Streaming lifecycle ────────────────────────

    startStreaming: (messageId) =>
      set({
        isStreaming: true,
        streamingMessageId: messageId,
      }),

    finishStreaming: (finalMessage) =>
      set((state) => {
        const updates: Partial<ChatState> = {
          isStreaming: false,
          streamingMessageId: null,
        };

        if (finalMessage && state.streamingMessageId) {
          const msgIdx = state.messages.findIndex(
            (m) => m.id === state.streamingMessageId
          );
          if (msgIdx !== -1) {
            const updated = [...state.messages];
            updated[msgIdx] = {
              ...updated[msgIdx],
              ...finalMessage,
              isStreaming: false,
            };
            updates.messages = updated;
          }
        }

        return updates;
      }),

    cancelStreaming: () =>
      set({
        isStreaming: false,
        isSending: false,
        streamingMessageId: null,
      }),

    // ── Sending lifecycle ──────────────────────────

    startSending: () => set({ isSending: true, lastError: null }),
    finishSending: () => set({ isSending: false }),

    // ── Model actions ──────────────────────────────

    setSelectedModel: (model) => set({ selectedModel: model }),
    setSelectedProvider: (provider) => set({ selectedProvider: provider }),
    setModelOverride: (override) => set({ modelOverride: override }),

    // ── Error ──────────────────────────────────────

    setError: (error) => set({ lastError: error }),
    clearError: () => set({ lastError: null }),

    // ── Reset ──────────────────────────────────────

    reset: () => set(initialState),
  }))
);

// ── Selectors ──────────────────────────────────────

/** Select the active thread object. */
export const selectActiveThread = (state: ChatStore): Thread | undefined =>
  state.threads.find((t) => t.id === state.activeThreadId);

/** Select whether the chat is busy (sending or streaming). */
export const selectIsBusy = (state: ChatStore): boolean =>
  state.isSending || state.isStreaming;

/** Select the current message being streamed. */
export const selectStreamingMessage = (state: ChatStore): ThreadMessage | undefined =>
  state.streamingMessageId
    ? state.messages.find((m) => m.id === state.streamingMessageId)
    : undefined;

/** Select the effective model (override > selected > null). */
export const selectEffectiveModel = (state: ChatStore): string | null =>
  state.modelOverride ?? state.selectedModel;

/** Count unread/pending items across threads. */
export const selectPendingCount = (state: ChatStore): number =>
  state.threads.reduce((sum, t) => sum + t.pendingApprovals, 0);
