import { useState, useRef, useEffect, useCallback } from "react";
import * as api from "../api";
import type {
  DesktopAgent,
  ChatMessage,
  SkillDescriptor,
  AgentEventEnvelope,
} from "../types";
import { AGENT_TEMPLATES } from "../types";
import { Icon } from "../components/Icon";

// ── Local types ───────────────────────────────────────────────

interface ThreadMessage {
  id: string;
  role: "user" | "assistant" | "system";
  text: string;
  time: string;
  agent?: string;
  skills?: string[];
  tokens?: number;
  cost?: number;
  duration?: number;
}

interface Suggestion {
  icon: string;
  text: string;
}

const SUGGESTIONS: Suggestion[] = [
  { icon: "📡", text: "Check all channel health and report any issues." },
  { icon: "💰", text: "Create a cost report summarizing today's usage." },
  { icon: "🛡️", text: "Run a security audit on all agent personas." },
];

// ── Props ─────────────────────────────────────────────────────

export interface ChatPageProps {
  agents: DesktopAgent[];
  skills: SkillDescriptor[];
  selectedAgentId: string | null;
  onSelectAgent: (id: string | null) => void;
  onCreateAgent: (template: (typeof AGENT_TEMPLATES)[number]) => void;
  onAgentEvent?: (event: AgentEventEnvelope) => void;
  pushToast: (text: string) => void;
}

// ── Component ─────────────────────────────────────────────────

export function ChatPage({
  agents,
  skills,
  selectedAgentId,
  onSelectAgent,
  onCreateAgent,
  pushToast,
}: ChatPageProps) {
  const [input, setInput] = useState("");
  const [messages, setMessages] = useState<ThreadMessage[]>([]);
  const [isSending, setIsSending] = useState(false);
  const [showTerminal, setShowTerminal] = useState(false);
  const [showAgentPicker, setShowAgentPicker] = useState(false);
  const messagesEndRef = useRef<HTMLDivElement>(null);

  const agent = agents.find((a) => a.id === selectedAgentId) ?? agents[0] ?? null;
  const isNew = messages.length === 0;

  // Auto-scroll on new messages
  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages]);

  // Load session messages when agent changes
  useEffect(() => {
    if (!selectedAgentId) {
      setMessages([]);
      return;
    }
    api.getSessionMessages(selectedAgentId).then((backendMsgs) => {
      setMessages(
        backendMsgs.map((m) => ({
          id: m.id,
          role: m.role,
          text: m.content,
          time: new Date(m.timestamp).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }),
          tokens: m.metadata?.token_cost,
          cost: m.metadata?.cost_usd,
          duration: m.metadata?.duration_ms,
          skills: m.metadata?.skills_activated,
          agent: m.role === "assistant" ? agent?.name : undefined,
        }))
      );
    }).catch(() => {});
  }, [selectedAgentId]);

  const sendMessage = useCallback(async (content: string) => {
    if (!selectedAgentId || !content.trim()) return;

    const userMsg: ThreadMessage = {
      id: `u_${Date.now()}`,
      role: "user",
      text: content,
      time: new Date().toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }),
    };
    setMessages((prev) => [...prev, userMsg]);
    setIsSending(true);

    try {
      const response = await api.sendMessage(selectedAgentId, content);
      const assistantMsg: ThreadMessage = {
        id: response.message.id,
        role: "assistant",
        text: response.message.content,
        time: new Date(response.message.timestamp).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }),
        agent: agent?.name,
        tokens: response.message.metadata?.token_cost,
        cost: response.message.metadata?.cost_usd,
        duration: response.message.metadata?.duration_ms,
        skills: response.message.metadata?.skills_activated,
      };
      setMessages((prev) => [...prev, assistantMsg]);
    } catch (err) {
      pushToast("Failed to send message.");
    } finally {
      setIsSending(false);
    }
  }, [selectedAgentId, agent, pushToast]);

  return (
    <div className="view chat-page">
      {/* Top bar */}
      <div className="chat-page-topbar">
        <span className="chat-page-title">
          {isNew ? "New thread" : `${agent?.icon ?? "🤖"} ${agent?.name ?? "Chat"}`}
        </span>
        <div className="chat-page-topbar-actions">
          <button className="btn subtle" onClick={() => setShowTerminal(!showTerminal)}>
            <Icon name="settings" /> Terminal ⌘J
          </button>
        </div>
      </div>

      {/* Main chat area */}
      <div className="chat-page-body">
        <div className="chat-page-messages">
          {isNew ? (
            /* Hero empty state */
            <div className="chat-hero">
              <div className="chat-hero-logo">🐾</div>
              <h1 className="chat-hero-title">Let's build</h1>
              <div className="chat-hero-agent">
                <span>{agent?.icon ?? "🎯"} {agent?.name ?? "Select agent"}</span>
                <button
                  className="btn ghost"
                  onClick={() => setShowAgentPicker(!showAgentPicker)}
                  aria-label="Switch agent"
                >
                  <Icon name="collapse-left" />
                </button>
              </div>

              {showAgentPicker && (
                <div className="chat-agent-picker">
                  {agents.map((a) => (
                    <button
                      key={a.id}
                      className={`chat-agent-option ${selectedAgentId === a.id ? "active" : ""}`}
                      onClick={() => { onSelectAgent(a.id); setShowAgentPicker(false); }}
                    >
                      <span className="chat-agent-option-icon">{a.icon}</span>
                      <div>
                        <div className="chat-agent-option-name">{a.name}</div>
                        <div className="chat-agent-option-meta">{a.model} · {a.skills.length} skills</div>
                      </div>
                      <span className={`status-dot ${a.status === "active" ? "status-ok" : "status-warn"}`} />
                    </button>
                  ))}
                  {agents.length === 0 && (
                    <button className="btn primary" onClick={() => onCreateAgent(AGENT_TEMPLATES[0])}>
                      Create first agent
                    </button>
                  )}
                </div>
              )}

              {/* Suggestion cards */}
              <div className="chat-suggestions">
                {SUGGESTIONS.map((s, i) => (
                  <button
                    key={i}
                    className="chat-suggestion-card"
                    onClick={() => setInput(s.text)}
                  >
                    <span className="chat-suggestion-icon">{s.icon}</span>
                    <span>{s.text}</span>
                  </button>
                ))}
              </div>
            </div>
          ) : (
            /* Message list */
            <div className="chat-message-list">
              {messages.map((m) => (
                <div key={m.id} className={`chat-bubble ${m.role === "user" ? "chat-bubble-user" : m.role === "system" ? "chat-bubble-system" : "chat-bubble-assistant"}`}>
                  {m.role === "system" ? (
                    <div className="chat-bubble-system-text">{m.text}</div>
                  ) : m.role === "user" ? (
                    <>
                      <div className="chat-bubble-sender">You</div>
                      <div className="chat-bubble-content">{m.text}</div>
                    </>
                  ) : (
                    <>
                      <div className="chat-bubble-header">
                        <span className="chat-bubble-agent">{agent?.icon ?? "🤖"} {m.agent}</span>
                        {m.skills?.map((s) => (
                          <span key={s} className="chip">{s}</span>
                        ))}
                      </div>
                      <div className="chat-bubble-content">{m.text}</div>
                      {(m.tokens || m.cost || m.duration) && (
                        <div className="chat-bubble-footer">
                          {m.tokens != null && <span>🪙 {m.tokens.toLocaleString()} tokens</span>}
                          {m.cost != null && <span>💰 ${m.cost.toFixed(4)}</span>}
                          {m.duration != null && <span>⏱ {m.duration}ms</span>}
                        </div>
                      )}
                    </>
                  )}
                </div>
              ))}
              <div ref={messagesEndRef} />
            </div>
          )}
        </div>

        {/* Composer */}
        <div className="chat-composer-wrap">
          <div className="chat-composer">
            <textarea
              placeholder={selectedAgentId ? "Ask ClawDesk anything, @ to add files, / for commands" : "Create or select an agent first."}
              value={input}
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  if (input.trim()) {
                    sendMessage(input.trim());
                    setInput("");
                  }
                }
              }}
              rows={2}
              disabled={!selectedAgentId}
            />
            <div className="chat-composer-actions">
              <div className="chat-composer-left">
                {agents.length > 0 && (
                  <select
                    value={selectedAgentId ?? ""}
                    onChange={(e) => onSelectAgent(e.target.value || null)}
                    className="chat-model-select"
                  >
                    {agents.map((a) => (
                      <option key={a.id} value={a.id}>{a.icon} {a.name} ({a.model})</option>
                    ))}
                  </select>
                )}
              </div>
              <button
                className="btn primary chat-send-btn"
                disabled={isSending || !selectedAgentId || !input.trim()}
                onClick={() => {
                  if (input.trim()) {
                    sendMessage(input.trim());
                    setInput("");
                  }
                }}
              >
                {isSending ? "Sending..." : "Send"}
              </button>
            </div>
          </div>
        </div>
      </div>

      {/* Terminal panel */}
      {showTerminal && (
        <div className="chat-terminal">
          <div className="chat-terminal-header">
            <span>Terminal <span className="chat-terminal-shell">/bin/zsh</span></span>
            <button className="btn ghost" onClick={() => setShowTerminal(false)}>
              <Icon name="close" />
            </button>
          </div>
          <div className="chat-terminal-body">
            <span className="chat-terminal-prompt">$</span> <span className="chat-terminal-cursor">▊</span>
          </div>
        </div>
      )}
    </div>
  );
}
