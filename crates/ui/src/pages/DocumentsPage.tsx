import { useState, useEffect, useCallback } from "react";
import * as api from "../api";
import type { RagDocument, RagSearchResult } from "../api";

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
    <div style={{ height: "100%", overflow: "auto", padding: "24px 32px" }}>
      {/* Header */}
      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 24 }}>
        <div>
          <h1 style={{ fontSize: 22, fontWeight: 700, margin: 0, color: "var(--text-primary)" }}>
            Documents
          </h1>
          <p style={{ margin: "4px 0 0", fontSize: 13, color: "var(--text-secondary)" }}>
            Upload documents for RAG — PDF, Text, Markdown, CSV
          </p>
        </div>
        <button onClick={refresh} style={btnStyle("secondary")}>Refresh</button>
      </div>

      {/* Upload Bar */}
      <div style={{ ...cardStyle, marginBottom: 20, display: "flex", gap: 8, alignItems: "center" }}>
        <input
          type="text"
          value={filePath}
          onChange={(e) => setFilePath(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && handleUpload()}
          placeholder="/path/to/document.pdf"
          style={{ ...inputStyle, flex: 1 }}
        />
        <button
          onClick={handleUpload}
          disabled={uploading || !filePath.trim()}
          style={btnStyle(uploading || !filePath.trim() ? "disabled" : "primary")}
        >
          {uploading ? "Ingesting..." : "Upload"}
        </button>
        <span style={{ fontSize: 11, color: "var(--text-tertiary)", whiteSpace: "nowrap" }}>
          Supported: PDF, TXT, MD, CSV
        </span>
      </div>

      {/* Tabs */}
      <div style={{ display: "flex", gap: 0, marginBottom: 16, borderBottom: "1px solid var(--border)" }}>
        {([
          ["documents", `Documents (${documents.length})`],
          ["search", "Search"],
        ] as [string, string][]).map(([key, label]) => (
          <button
            key={key}
            onClick={() => setActiveTab(key as any)}
            style={{
              padding: "8px 16px",
              fontSize: 13,
              fontWeight: 500,
              border: "none",
              background: "none",
              cursor: "pointer",
              color: activeTab === key ? "var(--accent)" : "var(--text-secondary)",
              borderBottom: activeTab === key ? "2px solid var(--accent)" : "2px solid transparent",
            }}
          >
            {label}
          </button>
        ))}
      </div>

      {/* Documents Tab */}
      {activeTab === "documents" && (
        <div>
          {documents.length === 0 ? (
            <div style={{ textAlign: "center", padding: 40, color: "var(--text-secondary)" }}>
              No documents uploaded yet. Use the upload bar above to ingest a file.
            </div>
          ) : (
            <table style={{ width: "100%", borderCollapse: "collapse", fontSize: 13 }}>
              <thead>
                <tr style={{ borderBottom: "1px solid var(--border)", textAlign: "left" }}>
                  <th style={thStyle}>File</th>
                  <th style={thStyle}>Type</th>
                  <th style={thStyle}>Size</th>
                  <th style={thStyle}>Words</th>
                  <th style={thStyle}>Chunks</th>
                  <th style={thStyle}>Date</th>
                  <th style={{ ...thStyle, textAlign: "right" }}>Actions</th>
                </tr>
              </thead>
              <tbody>
                {documents.map((doc) => (
                  <>
                    <tr key={doc.id} style={{ borderBottom: "1px solid var(--border)" }}>
                      <td style={tdStyle}>
                        <span style={{ fontWeight: 500 }}>{doc.filename}</span>
                        <div style={{ fontSize: 11, color: "var(--text-tertiary)", maxWidth: 300, overflow: "hidden", textOverflow: "ellipsis" }}>
                          {doc.file_path}
                        </div>
                      </td>
                      <td style={tdStyle}>
                        <span style={{ ...typeBadge, background: TYPE_COLORS[doc.doc_type] + "20", color: TYPE_COLORS[doc.doc_type] }}>
                          {doc.doc_type.toUpperCase()}
                        </span>
                      </td>
                      <td style={tdStyle}>{fmtSize(doc.size_bytes)}</td>
                      <td style={tdStyle}>{doc.word_count.toLocaleString()}</td>
                      <td style={tdStyle}>{doc.chunk_count}</td>
                      <td style={tdStyle}>{new Date(doc.created_at).toLocaleDateString()}</td>
                      <td style={{ ...tdStyle, textAlign: "right" }}>
                        <div style={{ display: "flex", gap: 6, justifyContent: "flex-end" }}>
                          <button onClick={() => handleViewChunks(doc.id)} style={btnStyle("secondary")}>
                            {expandedDoc === doc.id ? "Hide" : "Chunks"}
                          </button>
                          <button onClick={() => handleDelete(doc.id, doc.filename)} style={btnStyle("danger")}>
                            Delete
                          </button>
                        </div>
                      </td>
                    </tr>
                    {expandedDoc === doc.id && (
                      <tr key={doc.id + "-chunks"}>
                        <td colSpan={7} style={{ padding: "8px 12px", background: "var(--surface-alt, #f9fafb)" }}>
                          <div style={{ maxHeight: 300, overflow: "auto", fontSize: 12 }}>
                            {chunks.map((c, i) => (
                              <div key={i} style={{ padding: "6px 0", borderBottom: "1px solid var(--border)" }}>
                                <span style={{ color: "var(--text-tertiary)", fontWeight: 600, marginRight: 8 }}>
                                  #{i}
                                </span>
                                {c.slice(0, 200)}{c.length > 200 ? "..." : ""}
                              </div>
                            ))}
                          </div>
                        </td>
                      </tr>
                    )}
                  </>
                ))}
              </tbody>
            </table>
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
              placeholder="Search your documents..."
              style={{ ...inputStyle, flex: 1 }}
            />
            <button
              onClick={handleSearch}
              disabled={searching || !searchQuery.trim()}
              style={btnStyle(searching || !searchQuery.trim() ? "disabled" : "primary")}
            >
              {searching ? "Searching..." : "Search"}
            </button>
          </div>

          {searchResults.length > 0 && (
            <div>
              <div style={{ fontSize: 12, color: "var(--text-secondary)", marginBottom: 12 }}>
                {searchResults.length} results found
              </div>
              {searchResults.map((r, i) => (
                <div key={i} style={{ ...cardStyle, marginBottom: 8 }}>
                  <div style={{ display: "flex", justifyContent: "space-between", marginBottom: 6 }}>
                    <span style={{ fontWeight: 500, fontSize: 13 }}>{r.filename}</span>
                    <span style={{ fontSize: 11, color: "var(--text-tertiary)" }}>
                      chunk #{r.chunk_index} • {(r.similarity * 100).toFixed(0)}% match
                    </span>
                  </div>
                  <div style={{ fontSize: 12, color: "var(--text-secondary)", lineHeight: 1.5 }}>
                    {r.chunk_text.slice(0, 300)}{r.chunk_text.length > 300 ? "..." : ""}
                  </div>
                </div>
              ))}
            </div>
          )}

          {searchResults.length === 0 && searchQuery && !searching && (
            <div style={{ textAlign: "center", padding: 40, color: "var(--text-secondary)" }}>
              No results. Try different search terms.
            </div>
          )}
        </div>
      )}
    </div>
  );
}

/* ── helpers & styles ─────────────────────────────────────────── */

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

const cardStyle: React.CSSProperties = {
  background: "var(--surface)",
  border: "1px solid var(--border)",
  borderRadius: 8,
  padding: 16,
};

const thStyle: React.CSSProperties = {
  padding: "10px 12px",
  fontSize: 12,
  fontWeight: 600,
  color: "var(--text-secondary)",
  textTransform: "uppercase",
  letterSpacing: 0.5,
};

const tdStyle: React.CSSProperties = {
  padding: "10px 12px",
};

const inputStyle: React.CSSProperties = {
  padding: "8px 12px",
  fontSize: 13,
  border: "1px solid var(--border)",
  borderRadius: 6,
  background: "var(--surface)",
  color: "var(--text-primary)",
  outline: "none",
};

const typeBadge: React.CSSProperties = {
  fontSize: 10,
  fontWeight: 600,
  padding: "2px 6px",
  borderRadius: 4,
};

function btnStyle(variant: "primary" | "secondary" | "danger" | "disabled"): React.CSSProperties {
  const base: React.CSSProperties = {
    padding: "6px 14px",
    fontSize: 12,
    fontWeight: 500,
    border: "none",
    borderRadius: 6,
    cursor: variant === "disabled" ? "not-allowed" : "pointer",
    transition: "opacity 0.15s",
  };
  switch (variant) {
    case "primary":
      return { ...base, background: "var(--accent)", color: "#fff" };
    case "secondary":
      return { ...base, background: "transparent", border: "1px solid var(--border)", color: "var(--text-secondary)" };
    case "danger":
      return { ...base, background: "#ef444420", color: "#ef4444" };
    case "disabled":
      return { ...base, background: "var(--border)", color: "var(--text-tertiary)", opacity: 0.6 };
  }
}
