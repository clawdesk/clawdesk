/**
 * Streaming hooks — composable send/stream/cancel lifecycle.
 *
 * Separates the transport concern (IPC → Tauri → Rust backend) from
 * the presentation concern (message list rendering). Any component
 * that needs to send messages — ChatPage, IDE panel, agent designer —
 * can import `useStreaming()` without duplicating the state machine.
 *
 * The hook is a pure controller: it reads/writes the Zustand chat store
 * but owns no state itself (except the abort ref). This makes it safe
 * to mount in multiple components simultaneously — they all share the
 * same store atom.
 */

import { useCallback, useRef } from "react";
import { useChatStore } from "../stores/chat-store";
import type { ThreadMessage } from "../stores/chat-store";

// ── Types ──────────────────────────────────────────

interface SendOptions {
  content: string;
  agentId: string;
  chatId?: string;
  modelOverride?: string;
  providerOverride?: string;
  apiKey?: string;
  baseUrl?: string;
  attachments?: { data: string; mime_type: string; filename?: string }[];
}

interface UseStreamingReturn {
  /** Send a message and handle the streaming response. */
  send: (options: SendOptions) => Promise<void>;
  /** Cancel the current streaming operation. */
  cancel: () => void;
  /** Whether a message is currently being sent or streamed. */
  isBusy: boolean;
}

// ── Hook ───────────────────────────────────────────

/**
 * Manages the send → stream → complete lifecycle.
 *
 * Extracts the streaming state machine from ChatPage into a
 * reusable hook that works with the Zustand chat store.
 *
 * ## Lifecycle
 *
 * 1. User calls `send({ content, agentId, ... })`
 * 2. User message added to store immediately
 * 3. Placeholder assistant message added (empty, isStreaming: true)
 * 4. Tauri `send_message` invoked → returns response
 * 5. Response text set on the assistant message
 * 6. Streaming finalized with metadata (tokens, cost, duration)
 *
 * For true SSE streaming (when backend supports it), the hook
 * listens for `agent-event` Tauri events and appends chunks
 * via `appendStreamChunk()`.
 */
export function useStreaming(): UseStreamingReturn {
  const {
    addMessage,
    startSending,
    finishSending,
    startStreaming,
    finishStreaming,
    cancelStreaming,
    setError,
    isSending,
    isStreaming,
  } = useChatStore();

  const abortRef = useRef(false);

  const send = useCallback(
    async (options: SendOptions) => {
      abortRef.current = false;
      startSending();

      const userMsgId = `user-${Date.now()}`;
      const assistantMsgId = `assistant-${Date.now()}`;

      try {
        // 1. Add user message immediately (optimistic)
        const userMessage: ThreadMessage = {
          id: userMsgId,
          role: "user",
          text: options.content,
          time: new Date().toISOString(),
          attachments: options.attachments?.map((a) => ({
            data: a.data,
            mime_type: a.mime_type,
            filename: a.filename,
          })),
        };
        addMessage(userMessage);

        // 2. Add placeholder assistant message
        const placeholderMessage: ThreadMessage = {
          id: assistantMsgId,
          role: "assistant",
          text: "",
          time: new Date().toISOString(),
          isStreaming: true,
        };
        addMessage(placeholderMessage);
        startStreaming(assistantMsgId);

        // 3. Import and call the API
        // Dynamic import avoids circular dependency with api.ts
        const api = await import("../api");
        const response = await api.sendMessage(
          options.agentId,
          options.content,
          options.modelOverride,
          options.chatId,
          options.providerOverride,
          options.apiKey,
          options.baseUrl,
          options.attachments
        );

        if (abortRef.current) return;

        // 4. Finalize with response data
        const msg = response.message;
        const meta = msg.metadata;
        finishStreaming({
          text: msg.content,
          model: meta?.model,
          tokens: meta ? meta.input_tokens + meta.output_tokens : undefined,
          cost: meta?.cost_usd,
          duration: meta?.duration_ms,
          skills: meta?.skills_activated,
          isStreaming: false,
        });
      } catch (err) {
        const errorMsg =
          err instanceof Error ? err.message : "Unknown error occurred";
        setError(errorMsg);
        finishStreaming({ text: `Error: ${errorMsg}`, isStreaming: false });
      } finally {
        finishSending();
      }
    },
    [addMessage, startSending, finishSending, startStreaming, finishStreaming, setError]
  );

  const cancel = useCallback(() => {
    abortRef.current = true;
    cancelStreaming();
  }, [cancelStreaming]);

  return {
    send,
    cancel,
    isBusy: isSending || isStreaming,
  };
}

// ── Thread Management Hook ─────────────────────────

interface UseThreadsReturn {
  threads: ReturnType<typeof useChatStore.getState>["threads"];
  activeThreadId: string | null;
  switchThread: (threadId: string) => void;
  createThread: (title: string, agentId?: string) => string;
  deleteThread: (threadId: string) => void;
  renameThread: (threadId: string, title: string) => void;
}

/**
 * Thread list management — CRUD over the thread collection.
 *
 * Thin wrapper around the Zustand store's thread actions.
 * Keeps the store as the single source of truth while exposing
 * a hook-friendly API with stable callback references.
 */
export function useThreads(): UseThreadsReturn {
  const threads = useChatStore((s) => s.threads);
  const activeThreadId = useChatStore((s) => s.activeThreadId);
  const { setActiveThread, addThread, removeThread, updateThread } =
    useChatStore();

  const switchThread = useCallback(
    (threadId: string) => {
      setActiveThread(threadId);
    },
    [setActiveThread]
  );

  const createThread = useCallback(
    (title: string, agentId?: string): string => {
      const id = `thread-${Date.now()}-${Math.random().toString(36).slice(2, 6)}`;
      addThread({
        id,
        agentId,
        title,
        lastActivity: new Date().toISOString(),
        pendingApprovals: 0,
        messageCount: 0,
      });
      setActiveThread(id);
      return id;
    },
    [addThread, setActiveThread]
  );

  const deleteThread = useCallback(
    (threadId: string) => {
      removeThread(threadId);
    },
    [removeThread]
  );

  const renameThread = useCallback(
    (threadId: string, title: string) => {
      updateThread(threadId, { title });
    },
    [updateThread]
  );

  return {
    threads,
    activeThreadId,
    switchThread,
    createThread,
    deleteThread,
    renameThread,
  };
}
