import { useState, useEffect, useCallback } from "react";
import * as api from "../api";
import { PageLayout } from "../components/PageLayout";
import { Icon } from "../components/Icon";
import type { WorkspaceFileEntry, MemoryHit, MemoryStatsResponse } from "../types";

interface FilesPageProps {
  pushToast: (msg: string) => void;
}

type PageTab = "workspace" | "memory";

export function FilesPage({ pushToast }: FilesPageProps) {
  const [pageTab, setPageTab] = useState<PageTab>("workspace");

  // ── Workspace state ──
  const [workspaceRoot, setWorkspaceRoot] = useState("");
  const [pathStack, setPathStack] = useState<string[]>([""]);
  const [entries, setEntries] = useState<WorkspaceFileEntry[]>([]);
  const [selectedFile, setSelectedFile] = useState<string | null>(null);
  const [fileContent, setFileContent] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  // ── Memory state ──
  const [memQuery, setMemQuery] = useState("");
  const [memResults, setMemResults] = useState<MemoryHit[]>([]);
  const [memSearching, setMemSearching] = useState(false);
  const [memStats, setMemStats] = useState<MemoryStatsResponse | null>(null);
  const [selectedMemory, setSelectedMemory] = useState<MemoryHit | null>(null);

  const currentPath = pathStack[pathStack.length - 1];

  useEffect(() => {
    api.getWorkspaceRoot().then(setWorkspaceRoot).catch(() => {});
  }, []);

  const loadDir = useCallback(async (relPath: string) => {
    setLoading(true);
    try {
      const list = await api.listWorkspaceFiles(relPath || undefined);
      setEntries(list);
    } catch (err) {
      pushToast("Failed to list files");
      setEntries([]);
    } finally {
      setLoading(false);
    }
  }, [pushToast]);

  useEffect(() => {
    loadDir(currentPath);
  }, [currentPath, loadDir]);

  const openEntry = useCallback((entry: WorkspaceFileEntry) => {
    if (entry.is_dir) {
      setPathStack((prev) => [...prev, entry.path]);
      setSelectedFile(null);
      setFileContent(null);
    } else {
      setSelectedFile(entry.path);
      setFileContent(null);
      api.readWorkspaceFile(entry.path).then(setFileContent).catch(() => {
        pushToast("Cannot read file");
        setFileContent("(binary or unreadable)");
      });
    }
  }, [pushToast]);

  const goBack = useCallback(() => {
    if (pathStack.length > 1) {
      setPathStack((prev) => prev.slice(0, -1));
      setSelectedFile(null);
      setFileContent(null);
    }
  }, [pathStack]);

  const goRoot = useCallback(() => {
    setPathStack([""]);
    setSelectedFile(null);
    setFileContent(null);
  }, []);

  const formatSize = (bytes: number) => {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1048576) return `${(bytes / 1024).toFixed(1)} KB`;
    return `${(bytes / 1048576).toFixed(1)} MB`;
  };

  const breadcrumbs = currentPath
    ? currentPath.split("/").filter(Boolean)
    : [];
  const currentFolderName = breadcrumbs[breadcrumbs.length - 1] ?? "workspace";

  // ── Memory functions ──
  useEffect(() => {
    if (pageTab === "memory" && !memStats) {
      api.getMemoryStats().then(setMemStats).catch(() => {});
    }
  }, [pageTab, memStats]);

  const searchMem = useCallback(async (q: string) => {
    if (!q.trim()) return;
    setMemSearching(true);
    try {
      const results = await api.recallMemories({ query: q.trim(), max_results: 30 });
      setMemResults(results);
    } catch { setMemResults([]); pushToast("Memory search failed"); }
    finally { setMemSearching(false); }
  }, [pushToast]);

  const forgetMem = useCallback(async (id: string) => {
    try {
      await api.forgetMemory(id);
      setMemResults((prev) => prev.filter((m) => m.id !== id));
      pushToast("Memory forgotten");
    } catch { pushToast("Failed to forget memory"); }
  }, [pushToast]);

  return (
    <PageLayout
      title={pageTab === "workspace" ? "Files" : "Memory"}
      subtitle={pageTab === "workspace" ? (workspaceRoot || "Workspace") : "Search your agent's memory — past conversations, decisions, and facts"}
      actions={
        <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
          <div className="mode-toggle" style={{ margin: 0 }}>
            <button className={`mode-toggle-btn ${pageTab === "workspace" ? "active" : ""}`} onClick={() => setPageTab("workspace")}>
              <Icon name="folder" /> Workspace
            </button>
            <button className={`mode-toggle-btn ${pageTab === "memory" ? "active" : ""}`} onClick={() => setPageTab("memory")}>
              <Icon name="search" /> Memory
            </button>
          </div>
          {pageTab === "workspace" && (
            <button className="btn btn-sm" onClick={() => loadDir(currentPath)}>
              <Icon name="refresh-cw" /> Refresh
            </button>
          )}
        </div>
      }
    >
      {pageTab === "memory" ? (
        /* ═══ Memory Browser ═══ */
        <div className="files-page">
          <section className="files-hero">
            <div className="files-hero__intro">
              <span className="files-hero__eyebrow">Agent Memory</span>
              <h2>Search through everything your agents remember — conversations, decisions, preferences, and facts.</h2>
              <p>{memResults.length > 0 ? `${memResults.length} memories found` : "Type a query to search your memory"}</p>
            </div>
            <div className="files-hero__stats">
              <FileHeroStat label="Strategy" value={memStats?.search_strategy || "hybrid"} meta="Vector + BM25 fusion" />
              <FileHeroStat label="Collection" value={memStats?.collection_name || "memories"} meta={memStats?.embedding_provider || "—"} />
              <FileHeroStat label="Results" value={memResults.length.toString()} meta={memSearching ? "Searching…" : "Matching memories"} />
            </div>
          </section>

          {/* Search bar */}
          <div className="files-breadcrumbs" style={{ display: "flex", gap: 8, alignItems: "center" }}>
            <input
              style={{ flex: 1, padding: "8px 12px", borderRadius: 8, border: "1px solid var(--line)", background: "var(--bg)", color: "var(--text)", fontSize: 14, outline: "none" }}
              placeholder="Search your memory… (e.g., 'What did I ask about stocks?' or 'my preferences')"
              value={memQuery}
              onChange={(e) => setMemQuery(e.target.value)}
              onKeyDown={(e) => { if (e.key === "Enter") searchMem(memQuery); }}
            />
            <button className="btn btn-sm" onClick={() => searchMem(memQuery)} disabled={memSearching || !memQuery.trim()}>
              {memSearching ? <><Icon name="loader" className="spin" /> Searching</> : <><Icon name="search" /> Search</>}
            </button>
          </div>

          <div className="files-layout">
            {/* Memory list */}
            <div className="files-list-panel">
              <div className="files-list-panel__head">
                <span>Memories</span>
                <strong>{memResults.length} results</strong>
              </div>
              {memResults.length === 0 ? (
                <div className="files-empty" style={{ padding: 24, textAlign: "center" }}>
                  {memQuery ? "No memories match your query" : "Search to browse your memory"}
                </div>
              ) : (
                memResults.map((hit) => (
                  <div
                    key={hit.id}
                    className={`files-entry ${selectedMemory?.id === hit.id ? "selected" : ""}`}
                    onClick={() => setSelectedMemory(hit)}
                    style={{ flexDirection: "column", alignItems: "flex-start", gap: 4 }}
                  >
                    <div style={{ display: "flex", width: "100%", justifyContent: "space-between", alignItems: "center" }}>
                      <span style={{ fontWeight: 600, fontSize: 12 }}>{(hit.score * 100).toFixed(0)}% match</span>
                      <span className="files-entry-size">{hit.timestamp ? new Date(hit.timestamp).toLocaleDateString() : "—"}</span>
                    </div>
                    <span className="files-entry-name" style={{ fontSize: 12, opacity: 0.8, display: "-webkit-box", WebkitLineClamp: 2, WebkitBoxOrient: "vertical" as const, overflow: "hidden" }}>
                      {hit.content?.slice(0, 120) || "(empty)"}
                    </span>
                  </div>
                ))
              )}
            </div>

            {/* Memory detail panel */}
            <div className="files-preview-panel">
              {selectedMemory ? (
                <>
                  <div className="files-preview-header" style={{ justifyContent: "space-between" }}>
                    <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                      <Icon name="search" />
                      <span>Memory Detail</span>
                    </div>
                    <button className="btn btn-sm" style={{ background: "var(--error-bg, #fee)", color: "var(--error, #c00)", border: "1px solid var(--error, #c00)" }}
                      onClick={() => { if (confirm("Forget this memory permanently?")) forgetMem(selectedMemory.id); setSelectedMemory(null); }}
                    >
                      Forget
                    </button>
                  </div>
                  <div style={{ padding: 16 }}>
                    <div style={{ display: "flex", gap: 16, marginBottom: 12, fontSize: 12, color: "var(--text-soft)" }}>
                      <span><strong>Score:</strong> {(selectedMemory.score * 100).toFixed(1)}%</span>
                      <span><strong>Source:</strong> {selectedMemory.source || "conversation"}</span>
                      <span><strong>Date:</strong> {selectedMemory.timestamp ? new Date(selectedMemory.timestamp).toLocaleString() : "unknown"}</span>
                    </div>
                    <pre className="files-preview-content" style={{ whiteSpace: "pre-wrap", wordBreak: "break-word" }}>
                      {selectedMemory.content || "(empty memory)"}
                    </pre>
                  </div>
                </>
              ) : (
                <div className="files-preview-empty">
                  <Icon name="search" />
                  <span>Select a memory to view details</span>
                  <p style={{ fontSize: 12, color: "var(--text-soft)", marginTop: 8 }}>
                    Search for past conversations, decisions, preferences, or any facts your agents have stored.
                  </p>
                </div>
              )}
            </div>
          </div>
        </div>
      ) : (
        /* ═══ Workspace File Browser (existing) ═══ */
      <div className="files-page">
        <section className="files-hero">
          <div className="files-hero__intro">
            <span className="files-hero__eyebrow">Workspace explorer</span>
            <h2>Browse folders, inspect file contents, and stay oriented inside the current workspace.</h2>
            <p>{entries.length} items in {currentFolderName}, with {selectedFile ? "a file preview open" : "no file selected yet"}.</p>
          </div>
          <div className="files-hero__stats">
            <FileHeroStat label="Current folder" value={currentFolderName} meta={currentPath || "/"} />
            <FileHeroStat label="Entries" value={entries.length.toString()} meta={loading ? "Refreshing directory" : "Visible in the current view"} />
            <FileHeroStat label="Preview" value={selectedFile ? "Open" : "Idle"} meta={selectedFile ?? "Select a file to inspect"} />
          </div>
        </section>

        {/* Breadcrumbs */}
        <div className="files-breadcrumbs">
          <button className="files-crumb" onClick={goRoot}>
            <Icon name="search" />
          </button>
          {breadcrumbs.map((crumb, i) => {
            const crumbPath = breadcrumbs.slice(0, i + 1).join("/");
            return (
              <span key={crumbPath}>
                <span className="files-crumb-sep">/</span>
                <button
                  className="files-crumb"
                  onClick={() => {
                    const idx = pathStack.indexOf(crumbPath);
                    if (idx >= 0) setPathStack((prev) => prev.slice(0, idx + 1));
                    else setPathStack((prev) => [...prev, crumbPath]);
                    setSelectedFile(null);
                    setFileContent(null);
                  }}
                >
                  {crumb}
                </button>
              </span>
            );
          })}
        </div>

        <div className="files-layout">
          {/* File list panel */}
          <div className="files-list-panel">
            <div className="files-list-panel__head">
              <span>Explorer</span>
              <strong>{currentFolderName}</strong>
            </div>
            {pathStack.length > 1 && (
              <div className="files-entry files-back" onClick={goBack}>
                <Icon name="collapse-left" />
                <span>..</span>
              </div>
            )}
            {loading ? (
              <div className="files-loading">
                <Icon name="loader" className="spin" /> Loading...
              </div>
            ) : entries.length === 0 ? (
              <div className="files-empty">Empty directory</div>
            ) : (
              entries.map((entry) => (
                <div
                  key={entry.path}
                  className={`files-entry ${selectedFile === entry.path ? "selected" : ""} ${entry.is_dir ? "is-dir" : ""}`}
                  onClick={() => openEntry(entry)}
                >
                  <Icon name={entry.is_dir ? "folder" : "file"} />
                  <span className="files-entry-name">{entry.name}</span>
                  {!entry.is_dir && (
                    <span className="files-entry-size">{formatSize(entry.size)}</span>
                  )}
                </div>
              ))
            )}
          </div>

          {/* File preview panel */}
          <div className="files-preview-panel">
            {selectedFile ? (
              <>
                <div className="files-preview-header">
                  <Icon name="search" />
                  <span>{selectedFile}</span>
                </div>
                <pre className="files-preview-content">
                  {fileContent === null ? "Loading..." : fileContent}
                </pre>
              </>
            ) : (
              <div className="files-preview-empty">
                <Icon name="search" />
                <span>Select a file to preview</span>
              </div>
            )}
          </div>
        </div>
      </div>
      )}
    </PageLayout>
  );
}

function FileHeroStat({ label, value, meta }: { label: string; value: string; meta: string }) {
  return (
    <div className="files-hero-stat">
      <span>{label}</span>
      <strong>{value}</strong>
      <small>{meta}</small>
    </div>
  );
}
