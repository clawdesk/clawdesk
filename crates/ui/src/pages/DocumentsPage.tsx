import { useState, useEffect, useCallback, useRef } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import * as api from "../api";
import type { RagDocument, RagSearchResult } from "../api";
import { PageLayout } from "../components/PageLayout";
import { Icon } from "../components/Icon";

interface Props {
  pushToast: (text: string) => void;
}

export function DocumentsPage({ pushToast }: Props) {
  const [documents, setDocuments] = useState<RagDocument[]>([]);
  const [loading, setLoading] = useState(true);
  const [activeTab, setActiveTab] = useState<"documents" | "search">("documents");
  const [filePath, setFilePath] = useState("");
  const [uploading, setUploading] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const [searchResults, setSearchResults] = useState<RagSearchResult[]>([]);
  const [searching, setSearching] = useState(false);
  const [expandedDoc, setExpandedDoc] = useState<string | null>(null);
  const [chunks, setChunks] = useState<string[]>([]);
  const [dragOver, setDragOver] = useState(false);

  const ingestPaths = useCallback(async (paths: string[]) => {
    if (!paths.length) return;
    setUploading(true);
    let successCount = 0;
    for (const path of paths) {
      try {
        const doc = await api.ragIngestDocument(path);
        pushToast(`Ingested "${doc.filename}" (${doc.chunk_count} chunks)`);
        successCount++;
      } catch (e: any) {
        pushToast(`Failed "${path.split(/[\\/]/).pop() ?? path}": ${e}`);
      }
    }
    if (successCount > 0) refresh();
    setUploading(false);
  }, [pushToast, refresh]);

  const refresh = useCallback(async () => {
    try {
      const docs = await api.ragListDocuments();
      setDocuments(docs);
    } catch (e) {
      console.error("Failed to load documents:", e);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => { refresh(); }, [refresh]);

  const handleUpload = async () => {
    if (!filePath.trim()) return;
    setUploading(true);
    try {
      const doc = await api.ragIngestDocument(filePath.trim());
      pushToast(`Ingested "${doc.filename}" — ${doc.chunk_count} chunks`);
      setFilePath("");
      refresh();
    } catch (e: any) {
      pushToast(`Ingest failed: ${e}`);
    } finally {
      setUploading(false);
    }
  };

  const handleFileInput = async (files: FileList | null) => {
    if (!files?.length) return;
    const paths = Array.from(files)
      .map((file) => (file as File & { path?: string }).path)
      .filter((value): value is string => Boolean(value));
    if (!paths.length) {
      pushToast("Dropped files did not include filesystem paths. Use Choose Files for the native picker.");
      return;
    }
    await ingestPaths(paths);
  };

  const openDocumentPicker = useCallback(async () => {
    if (uploading) return;
    try {
      const selected = await open({
        multiple: true,
        directory: false,
        filters: [
          {
            name: "Documents and code",
            extensions: ["pdf", "txt", "md", "csv", "json", "html", "xml", "py", "js", "ts", "rs", "go", "java", "c", "cpp", "h", "rb", "sh"],
          },
        ],
      });
      const paths = Array.isArray(selected) ? selected : selected ? [selected] : [];
      await ingestPaths(paths);
    } catch (e: any) {
      pushToast(`Could not open file picker: ${e}`);
    }
  }, [ingestPaths, pushToast, uploading]);

  const handleDelete = async (docId: string, filename: string) => {
    if (!confirm(`Delete "${filename}"? This cannot be undone.`)) return;
    try {
      await api.ragDeleteDocument(docId);
      pushToast(`Deleted ${filename}`);
      refresh();
    } catch (e: any) {
      pushToast(`Delete failed: ${e}`);
    }
  };

  const handleSearch = async () => {
    if (!searchQuery.trim()) return;
    setSearching(true);
    try {
      const results = await api.ragSearch(searchQuery.trim(), 10);
      setSearchResults(results);
    } catch (e: any) {
      pushToast(`Search failed: ${e}`);
    } finally {
      setSearching(false);
    }
  };

  const handleViewChunks = async (docId: string) => {
    if (expandedDoc === docId) {
      setExpandedDoc(null);
      setChunks([]);
      return;
    }
    try {
      const c = await api.ragGetChunks(docId);
      setChunks(c);
      setExpandedDoc(docId);
    } catch (e: any) {
      pushToast(`Failed to load chunks: ${e}`);
    }
  };

  if (loading) {
    return (
      <div style={{ display: "flex", justifyContent: "center", alignItems: "center", height: "100%", color: "var(--text-secondary)" }}>
        Loading documents...
      </div>
    );
  }

  return (
    <PageLayout
      title="Documents"
      subtitle={`${documents.length} documents ingested for RAG context`}
      onRefresh={refresh}
      loading={loading}
    >
      <div className="documents-page-shell">

      {/* Upload Area — drag+drop + file picker + manual path */}
      <section
        className={`documents-upload-zone ${dragOver ? "drag-over" : ""}`}
        onDragOver={(e) => { e.preventDefault(); setDragOver(true); }}
        onDragLeave={() => setDragOver(false)}
        onDrop={(e) => {
          e.preventDefault();
          setDragOver(false);
          handleFileInput(e.dataTransfer.files);
        }}
      >
        <div className="documents-upload-content">
          <div style={{ fontSize: 28, marginBottom: 8 }}>📄</div>
          <h3 style={{ margin: 0, fontSize: 15, fontWeight: 600, color: "var(--text)" }}>
            {uploading ? "Ingesting…" : "Drop files here or click to browse"}
          </h3>
          <p style={{ margin: "4px 0 12px", fontSize: 12, color: "var(--text-muted)" }}>
            PDF, TXT, Markdown, CSV, and source code files
          </p>
          <button className="btn primary" onClick={() => void openDocumentPicker()} disabled={uploading}>
            <Icon name="upload" /> Choose Files
          </button>
        </div>
        {/* Manual path fallback */}
        <div className="documents-upload-manual">
          <input
            type="text"
            value={filePath}
            onChange={(e) => setFilePath(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && handleUpload()}
            placeholder="Or paste a file path: /path/to/document.pdf"
            className="extensions-input"
            style={{ flex: 1 }}
          />
          <button className="btn subtle" onClick={handleUpload} disabled={uploading || !filePath.trim()}>
            Ingest
          </button>
        </div>
      </section>

      {/* Tabs */}
      <div className="extensions-tabs" style={{ marginBottom: 16 }}>
        {([
          ["documents", `Documents (${documents.length})`],
          ["search", "Semantic Search"],
        ] as [string, string][]).map(([key, label]) => (
          <button
            key={key}
            onClick={() => setActiveTab(key as any)}
            className={`extensions-tab${activeTab === key ? " active" : ""}`}
          >
            {label}
          </button>
        ))}
      </div>

      {/* Documents Tab */}
      {activeTab === "documents" && (
        <div className="list-rows">
          {documents.length === 0 ? (
            <div className="empty-state" style={{ padding: 40 }}>
              <p>No documents uploaded yet.</p>
              <p style={{ fontSize: 12, color: "var(--text-muted)" }}>
                Drop files above or use the file picker to ingest documents for RAG.
              </p>
            </div>
          ) : (
            documents.map((doc) => (
              <div key={doc.id} className="section-card" style={{ padding: 0 }}>
                <div style={{ display: "flex", alignItems: "center", gap: 14, padding: "14px 16px" }}>
                  <span style={{ fontSize: 24, flexShrink: 0 }}>
                    {doc.doc_type === "pdf" ? "📕" : doc.doc_type === "markdown" ? "📘" : doc.doc_type === "csv" ? "📊" : "📄"}
                  </span>
                  <div style={{ flex: 1, minWidth: 0 }}>
                    <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                      <strong style={{ fontSize: 14 }}>{doc.filename}</strong>
                      <span className="chip chip-sm" style={{ background: TYPE_COLORS[doc.doc_type] + "20", color: TYPE_COLORS[doc.doc_type] }}>
                        {doc.doc_type.toUpperCase()}
                      </span>
                    </div>
                    <div style={{ fontSize: 12, color: "var(--text-muted)", marginTop: 2 }}>
                      {fmtSize(doc.size_bytes)} · {doc.word_count.toLocaleString()} words · {doc.chunk_count} chunks · {new Date(doc.created_at).toLocaleDateString()}
                    </div>
                    <div style={{ fontSize: 11, color: "var(--text-soft)", marginTop: 2, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                      {doc.file_path}
                    </div>
                  </div>
                  <div style={{ display: "flex", gap: 6, flexShrink: 0 }}>
                    <button className="btn subtle" onClick={() => handleViewChunks(doc.id)}>
                      {expandedDoc === doc.id ? "Hide Chunks" : `View Chunks (${doc.chunk_count})`}
                    </button>
                    <button className="btn danger" onClick={() => handleDelete(doc.id, doc.filename)} style={{ fontSize: 12, padding: "4px 10px" }}>
                      Delete
                    </button>
                  </div>
                </div>
                {expandedDoc === doc.id && chunks.length > 0 && (
                  <div style={{ borderTop: "1px solid var(--line)", padding: "12px 16px", maxHeight: 300, overflow: "auto", background: "var(--panel)" }}>
                    {chunks.map((c, i) => (
                      <div key={i} style={{ padding: "8px 0", borderBottom: i < chunks.length - 1 ? "1px solid var(--line)" : "none", fontSize: 12, lineHeight: 1.5, color: "var(--text-soft)" }}>
                        <span style={{ fontWeight: 600, color: "var(--text)", marginRight: 8 }}>#{i}</span>
                        {c.slice(0, 300)}{c.length > 300 ? "…" : ""}
                      </div>
                    ))}
                  </div>
                )}
              </div>
            ))
          )}
        </div>
      )}

      {/* Search Tab */}
      {activeTab === "search" && (
        <div>
          <div style={{ display: "flex", gap: 8, marginBottom: 16 }}>
            <input
              type="text"
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && handleSearch()}
              placeholder="Search your documents…"
              className="extensions-input"
              style={{ flex: 1 }}
            />
            <button
              onClick={handleSearch}
              disabled={searching || !searchQuery.trim()}
              className="btn primary"
            >
              {searching ? "Searching…" : "Search"}
            </button>
          </div>

          {searchResults.length > 0 && (
            <div className="list-rows">
              <div style={{ fontSize: 12, color: "var(--text-muted)", marginBottom: 8 }}>
                {searchResults.length} results found
              </div>
              {searchResults.map((r, i) => (
                <div key={i} className="section-card" style={{ padding: 14 }}>
                  <div style={{ display: "flex", justifyContent: "space-between", marginBottom: 6 }}>
                    <span style={{ fontWeight: 600, fontSize: 13, color: "var(--text)" }}>{r.filename}</span>
                    <span className="chip chip-sm">
                      chunk #{r.chunk_index} · {(r.similarity * 100).toFixed(0)}% match
                    </span>
                  </div>
                  <div style={{ fontSize: 12, color: "var(--text-soft)", lineHeight: 1.5 }}>
                    {r.chunk_text.slice(0, 400)}{r.chunk_text.length > 400 ? "…" : ""}
                  </div>
                </div>
              ))}
            </div>
          )}

          {searchResults.length === 0 && searchQuery && !searching && (
            <div className="empty-state" style={{ padding: 40 }}>
              <p>No results. Try different search terms.</p>
            </div>
          )}
        </div>
      )}

      </div>
    </PageLayout>
  );
}

/* ── helpers ─────────────────────────────────────────── */

const TYPE_COLORS: Record<string, string> = {
  pdf: "#ef4444",
  text: "#6b7280",
  markdown: "#3b82f6",
  csv: "#22c55e",
};

function fmtSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}
