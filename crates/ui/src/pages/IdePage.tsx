import { useState, useEffect, useCallback, useRef } from "react";
import * as api from "../api";
import { Icon } from "../components/Icon";
import { XTerminal } from "../components/XTerminal";
import type { DesktopAgent, ChatMessage, SessionSummary, WorkspaceFileEntry, MemoryHit } from "../types";
import { getActiveProvider } from "../providerConfig";

interface IdePageProps {
  agents: DesktopAgent[];
  skills: unknown[];
  selectedAgentId: string | null;
  onSelectAgent: (id: string) => void;
  pushToast: (msg: string) => void;
}

interface OpenTab { path: string; name: string; content: string | null; loading: boolean; }

function langFromPath(p: string): string {
  const ext = p.split(".").pop()?.toLowerCase() ?? "";
  const m: Record<string, string> = {
    ts: "TypeScript", tsx: "TypeScript", js: "JavaScript", jsx: "JavaScript",
    rs: "Rust", py: "Python", go: "Go", java: "Java", json: "JSON",
    toml: "TOML", yaml: "YAML", yml: "YAML", md: "Markdown", html: "HTML",
    css: "CSS", sql: "SQL", sh: "Shell", c: "C", cpp: "C++",
  };
  return m[ext] ?? (ext.toUpperCase() || "Text");
}

type SidebarTab = "files" | "memory";

export function IdePage({ agents, selectedAgentId, onSelectAgent, pushToast }: IdePageProps) {
  // ── Session ──
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [activeChatId, setActiveChatId] = useState<string | null>(null);
  const chatRef = useRef<string | null>(null);
  const [projectPath, setProjectPath] = useState("");

  // ── Files ──
  const [files, setFiles] = useState<WorkspaceFileEntry[]>([]);
  const [openTabs, setOpenTabs] = useState<OpenTab[]>([]);
  const [activeTabIdx, setActiveTabIdx] = useState(-1);

  // ── Chat ──
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [input, setInput] = useState("");
  const [isSending, setIsSending] = useState(false);
  const sendingRef = useRef(false);
  const [sendStartTime, setSendStartTime] = useState<number | null>(null);
  const [elapsed, setElapsed] = useState(0);

  // ── Memory browser ──
  const [sidebarTab, setSidebarTab] = useState<SidebarTab>("files");
  const [memoryQuery, setMemoryQuery] = useState("");
  const [memoryResults, setMemoryResults] = useState<MemoryHit[]>([]);
  const [memorySearching, setMemorySearching] = useState(false);

  // ── Layout ──
  const [showChat, setShowChat] = useState(true);
  const [showTerminal, setShowTerminal] = useState(false);
  const endRef = useRef<HTMLDivElement>(null);

  const provider = getActiveProvider();
  const agent = selectedAgentId ? agents.find((a) => a.id === selectedAgentId) : null;
  const isAutoMode = !selectedAgentId;
  const tab: OpenTab | null = activeTabIdx >= 0 ? openTabs[activeTabIdx] ?? null : null;
  const effectiveModel = provider?.model || "";
  const effectiveProvider = provider?.provider || "";
  const effectiveApiKey = provider?.apiKey || "";
  const effectiveBaseUrl = provider?.baseUrl || "";

  useEffect(() => { chatRef.current = activeChatId; }, [activeChatId]);
  useEffect(() => { endRef.current?.scrollIntoView({ behavior: "smooth" }); }, [messages]);
  useEffect(() => { api.listSessions().then(setSessions).catch(() => {}); }, []);

  // Elapsed timer when agent is working
  useEffect(() => {
    if (!isSending) { setSendStartTime(null); setElapsed(0); return; }
    if (!sendStartTime) return;
    const iv = setInterval(() => setElapsed(Math.floor((Date.now() - sendStartTime) / 1000)), 200);
    return () => clearInterval(iv);
  }, [isSending, sendStartTime]);

  useEffect(() => {
    if (activeChatId) {
      api.getChatWorkspace(activeChatId).then(setProjectPath).catch(() => {});
      setOpenTabs([]); setActiveTabIdx(-1);
      api.getChatMessages(activeChatId).then(setMessages).catch(() => setMessages([]));
    } else {
      setProjectPath(""); setFiles([]); setMessages([]); setOpenTabs([]); setActiveTabIdx(-1);
    }
  }, [activeChatId]);

  const loadFiles = useCallback(async () => {
    if (!activeChatId) return;
    try { setFiles(await api.listChatProjectFiles(activeChatId)); } catch { setFiles([]); }
  }, [activeChatId]);

  useEffect(() => { if (activeChatId) loadFiles(); }, [activeChatId, loadFiles]);
  useEffect(() => { if (!isSending && activeChatId) { const t = setTimeout(loadFiles, 600); return () => clearTimeout(t); } }, [isSending, activeChatId, loadFiles]);

  // ── Memory search ──
  const searchMemory = useCallback(async (q: string) => {
    if (!q.trim()) return;
    setMemorySearching(true);
    try {
      const results = await api.recallMemories({ query: q.trim(), max_results: 20 });
      setMemoryResults(results);
    } catch { setMemoryResults([]); }
    finally { setMemorySearching(false); }
  }, []);

  // Auto mode: pick the best agent for IDE tasks
  const autoPickAgent = useCallback((content: string): DesktopAgent | undefined => {
    if (agents.length === 0) return undefined;
    const lower = content.toLowerCase();
    const codeKeywords = ["build", "create", "implement", "code", "app", "function", "fix", "refactor", "api", "todo"];
    if (codeKeywords.some((kw) => lower.includes(kw))) {
      return agents.find((a) => a.name.toLowerCase().includes("coder") || a.name.toLowerCase().includes("dev")) || agents[0];
    }
    return agents.find((a) => !a.team_id) || agents[0];
  }, [agents]);

  // ── Tab ops ──
  const openFile = useCallback((entry: WorkspaceFileEntry) => {
    if (!activeChatId || entry.is_dir) return;
    const idx = openTabs.findIndex((t) => t.path === entry.path);
    if (idx >= 0) { setActiveTabIdx(idx); return; }
    const newTab: OpenTab = { path: entry.path, name: entry.name, content: null, loading: true };
    setOpenTabs((prev) => [...prev, newTab]);
    setActiveTabIdx(openTabs.length);
    api.readChatProjectFile(activeChatId, entry.path)
      .then((c) => setOpenTabs((p) => p.map((t) => t.path === entry.path ? { ...t, content: c, loading: false } : t)))
      .catch(() => setOpenTabs((p) => p.map((t) => t.path === entry.path ? { ...t, content: "(unreadable)", loading: false } : t)));
  }, [activeChatId, openTabs]);

  const closeTab = useCallback((i: number) => {
    setOpenTabs((p) => p.filter((_, j) => j !== i));
    setActiveTabIdx((prev) => i < prev ? prev - 1 : i === prev ? Math.min(prev, openTabs.length - 2) : prev);
  }, [openTabs.length]);

  // ── Chat ──
  const send = useCallback(async (content: string) => {
    const resolvedAgent = agent || (isAutoMode ? autoPickAgent(content) : undefined);
    if (!resolvedAgent || sendingRef.current) return;
    sendingRef.current = true; setIsSending(true); setSendStartTime(Date.now());
    try {
      let id = chatRef.current;
      if (!id) { const s = await api.createChat(resolvedAgent.id); id = s.chat_id; chatRef.current = id; setActiveChatId(id); setSessions((p) => [s, ...p]); }
      setMessages((p) => [...p, { id: crypto.randomUUID(), role: "user", content, timestamp: new Date().toISOString(), metadata: null }]);
      const r = await api.sendMessage(resolvedAgent.id, content, effectiveModel || undefined, id ?? undefined, effectiveProvider || undefined, effectiveApiKey || undefined, effectiveBaseUrl || undefined);
      setMessages((p) => [...p, r.message]);
    } catch (e) {
      const errMsg = String(e);
      setMessages((p) => [...p, { id: crypto.randomUUID(), role: "assistant", content: `⚠️ Error: ${errMsg}`, timestamp: new Date().toISOString(), metadata: null }]);
      pushToast(`Agent error: ${errMsg.slice(0, 120)}`);
    } finally { sendingRef.current = false; setIsSending(false); }
  }, [agent, isAutoMode, autoPickAgent, pushToast, effectiveModel, effectiveProvider, effectiveApiKey, effectiveBaseUrl]);

  const newProject = useCallback(async () => {
    const resolvedAgent = agent || agents.find((a) => !a.team_id) || agents[0];
    if (!resolvedAgent) return;
    try { const s = await api.createChat(resolvedAgent.id); setActiveChatId(s.chat_id); setSessions((p) => [s, ...p]); setMessages([]); setFiles([]); setOpenTabs([]); setActiveTabIdx(-1); }
    catch (e) { pushToast(`Failed: ${e}`); }
  }, [agent, agents, pushToast]);

  const lines = tab?.content?.split("\n") ?? [];
  const lang = tab ? langFromPath(tab.path) : "";
  const projName = sessions.find((s) => s.chat_id === activeChatId)?.title || (activeChatId ? `project-${activeChatId.slice(0, 8)}` : "");

  return (
    <div className="ix">
      {/* ═══ Sidebar ═══ */}
      <aside className="ix-sidebar">
        <div className="ix-sb-head">
          {/* Sidebar tabs: Files | Memory */}
          <div className="ix-sb-tabs">
            <button className={`ix-sb-tab ${sidebarTab === "files" ? "act" : ""}`} onClick={() => setSidebarTab("files")}>
              <Icon name="folder" /> Files
            </button>
            <button className={`ix-sb-tab ${sidebarTab === "memory" ? "act" : ""}`} onClick={() => setSidebarTab("memory")}>
              <Icon name="search" /> Memory
            </button>
          </div>
          <div className="ix-sb-btns">
            <button className="ix-ib" onClick={loadFiles} title="Refresh"><Icon name="refresh-cw" /></button>
            <button className="ix-ib" onClick={newProject} title="New project"><Icon name="plus" /></button>
          </div>
        </div>

        {sidebarTab === "files" ? (
          <>
            <div className="ix-sb-proj">
              <select className="ix-sel" value={activeChatId ?? ""} onChange={(e) => setActiveChatId(e.target.value || null)}>
                <option value="">Select project…</option>
                {sessions.map((s) => <option key={s.chat_id} value={s.chat_id}>{s.title || `project-${s.chat_id.slice(0, 8)}`}</option>)}
              </select>
              <select className="ix-sel" style={{ marginTop: 6 }} value={selectedAgentId ?? ""} onChange={(e) => { onSelectAgent(e.target.value); }}>
                <option value="">✦ Auto (best agent per task)</option>
                {agents.map((a) => <option key={a.id} value={a.id}>{a.icon} {a.name}</option>)}
              </select>
            </div>
            <div className="ix-tree">
              {!activeChatId ? <div className="ix-tree-mt">Create or select a project</div>
              : files.length === 0 ? <div className="ix-tree-mt">No files — ask the agent</div>
              : files.map((f) => (
                <button key={f.path} className={`ix-node ${tab?.path === f.path ? "act" : ""} ${f.is_dir ? "dir" : ""}`} onClick={() => f.is_dir ? undefined : openFile(f)}>
                  <Icon name={f.is_dir ? "folder" : "file"} />
                  <span className="ix-node-n">{f.name}</span>
                  {!f.is_dir && <span className="ix-node-s">{f.size < 1024 ? `${f.size}B` : `${(f.size / 1024).toFixed(1)}K`}</span>}
                </button>
              ))}
            </div>
          </>
        ) : (
          /* ═══ Memory Browser ═══ */
          <div className="ix-mem">
            <div className="ix-mem-search">
              <input
                className="ix-mem-input"
                placeholder="Search your memory…"
                value={memoryQuery}
                onChange={(e) => setMemoryQuery(e.target.value)}
                onKeyDown={(e) => { if (e.key === "Enter") searchMemory(memoryQuery); }}
              />
              <button className="ix-ib" onClick={() => searchMemory(memoryQuery)} disabled={memorySearching}>
                <Icon name={memorySearching ? "loader" : "search"} className={memorySearching ? "spin" : ""} />
              </button>
            </div>
            <div className="ix-mem-results">
              {memoryResults.length === 0 && !memorySearching ? (
                <div className="ix-tree-mt">
                  {memoryQuery ? "No memories found" : "Search your past conversations, decisions, and facts"}
                </div>
              ) : (
                memoryResults.map((hit, i) => (
                  <div key={hit.id || i} className="ix-mem-hit">
                    <div className="ix-mem-score">{(hit.score * 100).toFixed(0)}%</div>
                    <div className="ix-mem-body">
                      <div className="ix-mem-text">{hit.content || "(empty)"}</div>
                      <div className="ix-mem-meta">
                        {hit.source && <span>{hit.source}</span>}
                        {hit.timestamp && <span>{new Date(hit.timestamp).toLocaleDateString()}</span>}
                      </div>
                    </div>
                  </div>
                ))
              )}
            </div>
          </div>
        )}
      </aside>

      {/* ═══ Main editor column ═══ */}
      <div className="ix-main">
        <div className="ix-tabs">
          {openTabs.map((t, i) => (
            <div key={t.path} className={`ix-tab ${i === activeTabIdx ? "act" : ""}`} onClick={() => setActiveTabIdx(i)}>
              <Icon name="file" /><span>{t.name}</span>
              <button className="ix-tab-x" onClick={(e) => { e.stopPropagation(); closeTab(i); }}>×</button>
            </div>
          ))}
          {openTabs.length === 0 && <div className="ix-tab-mt">No files open</div>}
          <div className="ix-tab-r">
            <button className={`ix-ib ${showChat ? "on" : ""}`} onClick={() => setShowChat(!showChat)} title="Assistant"><Icon name="ask" /></button>
            <button className={`ix-ib ${showTerminal ? "on" : ""}`} onClick={() => setShowTerminal(!showTerminal)} title="Terminal"><Icon name="terminal" /></button>
          </div>
        </div>

        <div className="ix-editor-wrap">
          {/* Working overlay — shows INSIDE the editor when agent is active */}
          {isSending && !tab && (
            <div className="ix-working">
              <div className="ix-working-spinner" />
              <div className="ix-working-text">
                <h3>{isAutoMode ? "Auto Mode" : (agent?.name || "Agent")} is working…</h3>
                <p className="ix-working-elapsed">{elapsed}s elapsed</p>
                <p className="ix-muted">The agent is generating code. Files will appear in the explorer when ready.</p>
                <div className="ix-working-steps">
                  <div className="ix-working-step done"><Icon name="check" /> Connected to {effectiveProvider || "provider"}</div>
                  <div className="ix-working-step done"><Icon name="check" /> Using {effectiveModel || "model"}</div>
                  <div className="ix-working-step active"><Icon name="loader" className="spin" /> Generating response…</div>
                </div>
              </div>
            </div>
          )}

          <div className="ix-editor">
            {tab ? (
              tab.loading ? <div className="ix-loading"><Icon name="loader" className="spin" /> Loading…</div> :
              <div className="ix-code-scroll">
                <div className="ix-gutter">{lines.map((_, i) => <div key={i} className="ix-ln">{i + 1}</div>)}</div>
                <pre className="ix-code">{tab.content}</pre>
              </div>
            ) : !isSending ? (
              <div className="ix-welcome">
                <div className="ix-welcome-mark">⌘</div>
                <h2>ClawDesk IDE</h2>
                <p>Select a project and ask the agent to write code.</p>
                <p className="ix-muted">Generated files appear in the explorer. Click to open.</p>
              </div>
            ) : null}
          </div>

          {showTerminal && (
            <div className="ix-dock">
              <div className="ix-dock-head">
                <span>Terminal</span>
                <button className="ix-ib" onClick={() => setShowTerminal(false)}><Icon name="close" /></button>
              </div>
              <div className="ix-dock-body"><XTerminal visible onClose={() => setShowTerminal(false)} /></div>
            </div>
          )}
        </div>

        <div className="ix-status">
          <div className="ix-status-l">
            {projName && <span className="ix-si ix-si-proj">{projName}</span>}
            {tab && <span className="ix-si">{tab.name}</span>}
          </div>
          <div className="ix-status-r">
            {tab && <span className="ix-si">Ln {lines.length}</span>}
            {tab && <span className="ix-si">{lang}</span>}
            <span className="ix-si">{provider?.provider ?? "—"}</span>
            <span className="ix-si">{effectiveModel || "no model"}</span>
            <span className="ix-si">{isAutoMode ? "Auto" : (agent?.name || "no agent")}</span>
            <span className={`ix-si ${isSending ? "busy" : ""}`}>
              {isSending ? `Working… ${elapsed}s` : "Ready"}
            </span>
          </div>
        </div>
      </div>

      {/* ═══ Chat dock ═══ */}
      {showChat && (
        <aside className="ix-chat">
          <div className="ix-chat-head">
            <span>{agent?.name || "Assistant"}</span>
            <button className="ix-ib" onClick={() => setShowChat(false)}><Icon name="close" /></button>
          </div>
          <div className="ix-chat-body">
            {messages.length === 0 ? <div className="ix-chat-mt"><p>Describe what to build.</p><p className="ix-muted">Files appear as the agent generates them.</p></div>
            : messages.filter((m) => m.role === "user" || m.role === "assistant").map((m) => (
              <div key={m.id} className={`ix-msg ${m.role}`}>
                <div className="ix-msg-who">{m.role === "user" ? "You" : agent?.name || "Agent"}</div>
                <div className="ix-msg-txt">{m.content}</div>
              </div>
            ))}
            <div ref={endRef} />
          </div>
          <div className="ix-chat-foot">
            <textarea className="ix-chat-ta" placeholder={(agent || isAutoMode) ? "Ask the agent…" : "Select an agent"} value={input}
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey && input.trim() && (agent || isAutoMode) && !isSending) { e.preventDefault(); send(input.trim()); setInput(""); } }}
              disabled={!(agent || isAutoMode) || isSending} rows={2}
            />
            <button className={`ix-send ${input.trim() && (agent || isAutoMode) && !isSending ? "go" : ""}`}
              disabled={!(agent || isAutoMode) || !input.trim() || isSending}
              onClick={() => { if (input.trim() && !isSending) { send(input.trim()); setInput(""); } }}
            ><Icon name="send" /></button>
          </div>
        </aside>
      )}
    </div>
  );
}
