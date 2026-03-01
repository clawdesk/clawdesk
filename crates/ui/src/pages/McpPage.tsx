import { useState, useEffect, useCallback } from "react";
import * as api from "../api";
import { PageLayout } from "../components/PageLayout";
import { Icon } from "../components/Icon";
import type {
  McpServerInfo,
  McpToolInfo,
  McpBundledTemplate,
} from "../types";

// ── Types ─────────────────────────────────────────────────────

type McpTab = "servers" | "tools" | "templates";

// ── Props ─────────────────────────────────────────────────────

export interface McpPageProps {
  pushToast: (msg: string) => void;
}

// ── Component ─────────────────────────────────────────────────

export function McpPage({ pushToast }: McpPageProps) {
  const [tab, setTab] = useState<McpTab>("servers");
  const [servers, setServers] = useState<McpServerInfo[]>([]);
  const [tools, setTools] = useState<McpToolInfo[]>([]);
  const [templates, setTemplates] = useState<McpBundledTemplate[]>([]);
  const [categories, setCategories] = useState<string[]>([]);
  const [loading, setLoading] = useState(true);

  // Connect form
  const [connectName, setConnectName] = useState("");
  const [connectTransport, setConnectTransport] = useState<"stdio" | "sse">("stdio");
  const [connectCommand, setConnectCommand] = useState("");
  const [connectArgs, setConnectArgs] = useState("");
  const [connectUrl, setConnectUrl] = useState("");
  const [showConnect, setShowConnect] = useState(false);

  // Tool call form
  const [callServer, setCallServer] = useState("");
  const [callTool, setCallTool] = useState("");
  const [callArgs, setCallArgs] = useState("{}");
  const [callResult, setCallResult] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      const [s, t, tmpl, cats] = await Promise.all([
        api.listMcpServers().catch(() => [] as McpServerInfo[]),
        api.listMcpTools().catch(() => [] as McpToolInfo[]),
        api.listMcpTemplates().catch(() => [] as McpBundledTemplate[]),
        api.listMcpCategories().catch(() => [] as string[]),
      ]);
      setServers(s);
      setTools(t);
      setTemplates(tmpl);
      setCategories(cats);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => { refresh(); }, [refresh]);

  const handleConnect = async () => {
    if (!connectName) return;
    try {
      await api.connectMcpServer({
        name: connectName,
        transport: connectTransport,
        command: connectTransport === "stdio" ? connectCommand || undefined : undefined,
        args: connectTransport === "stdio" && connectArgs ? connectArgs.split(" ") : undefined,
        url: connectTransport === "sse" ? connectUrl || undefined : undefined,
      });
      pushToast(`Connected to ${connectName}`);
      setConnectName("");
      setConnectCommand("");
      setConnectArgs("");
      setConnectUrl("");
      setShowConnect(false);
      refresh();
    } catch (e: any) {
      pushToast(`Connection error: ${e}`);
    }
  };

  const handleDisconnect = async (name: string) => {
    try {
      await api.disconnectMcpServer(name);
      pushToast(`Disconnected ${name}`);
      refresh();
    } catch (e: any) {
      pushToast(`Error: ${e}`);
    }
  };

  const handleInstallTemplate = async (name: string) => {
    try {
      await api.installMcpTemplate(name);
      pushToast(`Installed template: ${name}`);
      refresh();
    } catch (e: any) {
      pushToast(`Install error: ${e}`);
    }
  };

  const handleCallTool = async () => {
    if (!callServer || !callTool) return;
    try {
      const args = JSON.parse(callArgs);
      const result = await api.callMcpTool(callServer, callTool, args);
      const text = result.content
        .map(c => c.text ?? c.data ?? "")
        .join("\n");
      setCallResult(result.is_error ? `ERROR: ${text}` : text);
    } catch (e: any) {
      setCallResult(`Error: ${e}`);
    }
  };

  const TABS: { key: McpTab; label: string }[] = [
    { key: "servers", label: `Servers (${servers.length})` },
    { key: "tools", label: `Tools (${tools.length})` },
    { key: "templates", label: `Templates (${templates.length})` },
  ];

  return (
    <PageLayout
      title="MCP"
      subtitle={`${servers.length} server${servers.length !== 1 ? "s" : ""} connected • ${tools.length} tools available`}
      onRefresh={refresh}
      loading={loading}
    >
      {/* ── Tab bar ────────────────────────────────────── */}
      <div className="flex gap-1 mb-4 border-b border-neutral-200 dark:border-neutral-700">
        {TABS.map(t => (
          <button
            key={t.key}
            onClick={() => setTab(t.key)}
            className={`px-3 py-2 text-sm font-medium border-b-2 transition-colors ${
              tab === t.key
                ? "border-blue-500 text-blue-600 dark:text-blue-400"
                : "border-transparent text-neutral-500 hover:text-neutral-700 dark:hover:text-neutral-300"
            }`}
          >
            {t.label}
          </button>
        ))}
      </div>

      {/* ── Servers Tab ────────────────────────────────── */}
      {tab === "servers" && (
        <div className="space-y-4">
          <div className="flex justify-between items-center">
            <div className="text-sm text-neutral-500">Connected MCP servers</div>
            <button
              onClick={() => setShowConnect(!showConnect)}
              className="text-xs px-3 py-1 rounded bg-blue-500 text-white hover:bg-blue-600"
            >
              {showConnect ? "Cancel" : "+ Connect Server"}
            </button>
          </div>

          {/* Connect form */}
          {showConnect && (
            <div className="p-4 rounded-lg bg-neutral-50 dark:bg-neutral-800/50 border border-neutral-200 dark:border-neutral-700 space-y-3">
              <div className="grid grid-cols-2 gap-2">
                <input
                  type="text"
                  value={connectName}
                  onChange={e => setConnectName(e.target.value)}
                  placeholder="Server name"
                  className="px-3 py-1.5 text-sm rounded border border-neutral-300 dark:border-neutral-600 bg-white dark:bg-neutral-900"
                />
                <select
                  value={connectTransport}
                  onChange={e => setConnectTransport(e.target.value as "stdio" | "sse")}
                  className="px-3 py-1.5 text-sm rounded border border-neutral-300 dark:border-neutral-600 bg-white dark:bg-neutral-900"
                >
                  <option value="stdio">Stdio</option>
                  <option value="sse">SSE</option>
                </select>
              </div>
              {connectTransport === "stdio" && (
                <div className="grid grid-cols-2 gap-2">
                  <input
                    type="text"
                    value={connectCommand}
                    onChange={e => setConnectCommand(e.target.value)}
                    placeholder="Command (e.g. npx)"
                    className="px-3 py-1.5 text-sm rounded border border-neutral-300 dark:border-neutral-600 bg-white dark:bg-neutral-900"
                  />
                  <input
                    type="text"
                    value={connectArgs}
                    onChange={e => setConnectArgs(e.target.value)}
                    placeholder="Args (space-separated)"
                    className="px-3 py-1.5 text-sm rounded border border-neutral-300 dark:border-neutral-600 bg-white dark:bg-neutral-900"
                  />
                </div>
              )}
              {connectTransport === "sse" && (
                <input
                  type="url"
                  value={connectUrl}
                  onChange={e => setConnectUrl(e.target.value)}
                  placeholder="SSE URL"
                  className="w-full px-3 py-1.5 text-sm rounded border border-neutral-300 dark:border-neutral-600 bg-white dark:bg-neutral-900"
                />
              )}
              <button
                onClick={handleConnect}
                className="px-4 py-1.5 text-sm rounded bg-green-500 text-white hover:bg-green-600"
              >
                Connect
              </button>
            </div>
          )}

          {/* Server list */}
          {servers.map(s => (
            <div
              key={s.name}
              className="flex items-center justify-between p-3 rounded-lg bg-neutral-50 dark:bg-neutral-800/50 border border-neutral-200 dark:border-neutral-700"
            >
              <div>
                <div className="font-medium text-sm">{s.name}</div>
                <div className="text-xs text-neutral-500">
                  {s.transport} • {s.tool_count} tool{s.tool_count !== 1 ? "s" : ""}
                </div>
              </div>
              <div className="flex items-center gap-2">
                <span className={`w-2 h-2 rounded-full ${s.connected ? "bg-green-500" : "bg-red-500"}`} />
                <button
                  onClick={() => handleDisconnect(s.name)}
                  className="text-xs px-2 py-1 rounded text-red-500 hover:bg-red-50 dark:hover:bg-red-900/20"
                >
                  Disconnect
                </button>
              </div>
            </div>
          ))}
          {servers.length === 0 && !showConnect && (
            <div className="text-center text-neutral-500 py-8">
              No MCP servers connected. Use templates or connect manually.
            </div>
          )}
        </div>
      )}

      {/* ── Tools Tab ──────────────────────────────────── */}
      {tab === "tools" && (
        <div className="space-y-4">
          {/* Tool list */}
          <div className="grid gap-2">
            {tools.map((t, i) => (
              <div
                key={`${t.server}-${t.name}-${i}`}
                className="p-3 rounded-lg bg-neutral-50 dark:bg-neutral-800/50 border border-neutral-200 dark:border-neutral-700 cursor-pointer hover:border-blue-400"
                onClick={() => {
                  setCallServer(t.server);
                  setCallTool(t.name);
                }}
              >
                <div className="flex items-center gap-2">
                  <span className="font-mono text-sm font-medium">{t.name}</span>
                  <span className="text-[10px] px-1.5 py-0.5 rounded bg-neutral-200 dark:bg-neutral-700">{t.server}</span>
                </div>
                <div className="text-xs text-neutral-500 mt-1">{t.description}</div>
              </div>
            ))}
          </div>
          {tools.length === 0 && (
            <div className="text-center text-neutral-500 py-8">No tools available. Connect an MCP server first.</div>
          )}

          {/* Tool call form */}
          {callServer && callTool && (
            <div className="p-4 rounded-lg bg-blue-50 dark:bg-blue-900/20 border border-blue-200 dark:border-blue-800 space-y-3">
              <div className="font-medium text-sm">
                Call: <span className="font-mono">{callServer}/{callTool}</span>
              </div>
              <textarea
                value={callArgs}
                onChange={e => setCallArgs(e.target.value)}
                placeholder='{"key": "value"}'
                rows={3}
                className="w-full px-3 py-1.5 text-sm font-mono rounded border border-neutral-300 dark:border-neutral-600 bg-white dark:bg-neutral-900"
              />
              <div className="flex gap-2">
                <button
                  onClick={handleCallTool}
                  className="px-3 py-1.5 text-sm rounded bg-blue-500 text-white hover:bg-blue-600"
                >
                  Execute
                </button>
                <button
                  onClick={() => { setCallServer(""); setCallTool(""); setCallResult(null); }}
                  className="px-3 py-1.5 text-sm rounded bg-neutral-200 dark:bg-neutral-700"
                >
                  Clear
                </button>
              </div>
              {callResult !== null && (
                <pre className="p-3 text-xs font-mono bg-neutral-900 text-green-400 rounded overflow-x-auto max-h-48">
                  {callResult}
                </pre>
              )}
            </div>
          )}
        </div>
      )}

      {/* ── Templates Tab ──────────────────────────────── */}
      {tab === "templates" && (
        <div className="space-y-3">
          <div className="text-sm text-neutral-500 mb-2">
            Pre-configured MCP server templates — install with one click
          </div>
          {templates.map(t => (
            <div
              key={t.name}
              className="flex items-center justify-between p-3 rounded-lg bg-neutral-50 dark:bg-neutral-800/50 border border-neutral-200 dark:border-neutral-700"
            >
              <div>
                <div className="font-medium text-sm">{t.name}</div>
                <div className="text-xs text-neutral-500">{t.description}</div>
                <span className="text-[10px] px-1.5 py-0.5 rounded bg-neutral-200 dark:bg-neutral-700 mt-1 inline-block">
                  {t.category}
                </span>
              </div>
              <button
                onClick={() => handleInstallTemplate(t.name)}
                className="text-xs px-3 py-1 rounded bg-green-500 text-white hover:bg-green-600"
              >
                Install
              </button>
            </div>
          ))}
          {templates.length === 0 && (
            <div className="text-center text-neutral-500 py-8">No templates available</div>
          )}
        </div>
      )}
    </PageLayout>
  );
}
