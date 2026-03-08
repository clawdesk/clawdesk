import { useState, useEffect, useCallback } from "react";
import * as api from "../api";
import { PageLayout } from "../components/PageLayout";
import { Icon } from "../components/Icon";
import type { WorkspaceFileEntry } from "../types";

interface FilesPageProps {
  pushToast: (msg: string) => void;
}

export function FilesPage({ pushToast }: FilesPageProps) {
  const [workspaceRoot, setWorkspaceRoot] = useState("");
  const [pathStack, setPathStack] = useState<string[]>([""]);
  const [entries, setEntries] = useState<WorkspaceFileEntry[]>([]);
  const [selectedFile, setSelectedFile] = useState<string | null>(null);
  const [fileContent, setFileContent] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

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

  return (
    <PageLayout
      title="Files"
      subtitle={workspaceRoot || "Workspace"}
      actions={
        <button className="btn btn-sm" onClick={() => loadDir(currentPath)}>
          <Icon name="refresh-cw" /> Refresh
        </button>
      }
    >
      <div className="files-page">
        {/* Breadcrumbs */}
        <div className="files-breadcrumbs">
          <button className="files-crumb" onClick={goRoot}>
            <Icon name="home" />
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
            {pathStack.length > 1 && (
              <div className="files-entry files-back" onClick={goBack}>
                <Icon name="arrow-left" />
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
                  <Icon name="file" />
                  <span>{selectedFile}</span>
                </div>
                <pre className="files-preview-content">
                  {fileContent === null ? "Loading..." : fileContent}
                </pre>
              </>
            ) : (
              <div className="files-preview-empty">
                <Icon name="file-text" />
                <span>Select a file to preview</span>
              </div>
            )}
          </div>
        </div>
      </div>
    </PageLayout>
  );
}
