import { useEffect, useRef, useState, useCallback } from "react";
import * as api from "../api";
import { PageLayout } from "../components/PageLayout";
import { Icon } from "../components/Icon";

interface LogEntry {
  id: string;
  timestamp: string;
  level: "debug" | "info" | "warn" | "error";
  subsystem: string;
  message: string;
}

interface LogsPageProps {
  pushToast: (msg: string) => void;
}

export function LogsPage({ pushToast }: LogsPageProps) {
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const [filter, setFilter] = useState("");
  const [levels, setLevels] = useState<Record<LogEntry["level"], boolean>>({
    debug: true,
    info: true,
    warn: true,
    error: true,
  });
  const [autoFollow, setAutoFollow] = useState(true);
  const [loading, setLoading] = useState(true);
  const streamRef = useRef<HTMLDivElement>(null);

  const fetchLogs = useCallback(async () => {
    try {
      const entries = await api.getAuditLogs(200);
      const mapped: LogEntry[] = entries.map((e) => ({
        id: e.id,
        timestamp: e.timestamp,
        level: (e.level === "debug" || e.level === "info" || e.level === "warn" || e.level === "error"
          ? e.level
          : "info") as LogEntry["level"],
        subsystem: e.subsystem,
        message: e.message,
      }));
      setLogs(mapped);
    } catch {
      // Silently handle — may not have entries yet
    } finally {
      setLoading(false);
    }
  }, []);

  // Initial load
  useEffect(() => {
    fetchLogs();
  }, [fetchLogs]);

  // Poll for new entries every 5s
  useEffect(() => {
    const interval = setInterval(fetchLogs, 5000);
    return () => clearInterval(interval);
  }, [fetchLogs]);

  useEffect(() => {
    if (autoFollow && streamRef.current) {
      streamRef.current.scrollTop = 0;
    }
  }, [logs, autoFollow]);

  const toggleLevel = (level: LogEntry["level"]) => {
    setLevels((prev) => ({ ...prev, [level]: !prev[level] }));
  };

  const filtered = logs.filter((l) => {
    if (!levels[l.level]) return false;
    if (filter && !l.message.toLowerCase().includes(filter.toLowerCase()) && !l.subsystem.toLowerCase().includes(filter.toLowerCase())) return false;
    return true;
  });

  const formatTime = (iso: string) => {
    const d = new Date(iso);
    return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
  };

  const handleExport = () => {
    const text = filtered.map((l) => `[${l.timestamp}] [${l.level.toUpperCase()}] [${l.subsystem}] ${l.message}`).join("\n");
    const blob = new Blob([text], { type: "text/plain" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = `clawdesk-logs-${new Date().toISOString().slice(0, 10)}.txt`;
    a.click();
    URL.revokeObjectURL(url);
    pushToast("Logs exported");
  };

  const levelCounts = {
    debug: logs.filter((l) => l.level === "debug").length,
    info: logs.filter((l) => l.level === "info").length,
    warn: logs.filter((l) => l.level === "warn").length,
    error: logs.filter((l) => l.level === "error").length,
  };

  return (
    <PageLayout
      title="Logs"
      subtitle="Live log stream from all gateway subsystems."
      actions={
        <div style={{ display: "flex", gap: 8 }}>
          <button className="btn subtle" onClick={handleExport}>Export</button>
          <button className="btn subtle" onClick={() => { setLogs([]); pushToast("Logs cleared"); }}>Clear</button>
        </div>
      }
    >
      {/* Toolbar */}
      <div style={{ display: "flex", gap: 12, alignItems: "center", flexWrap: "wrap", marginBottom: 12 }}>
        <div style={{ display: "flex", alignItems: "center", gap: 6, flex: 1, minWidth: 200 }}>
          <Icon name="search" />
          <input
            className="input"
            type="text"
            placeholder="Filter logs…"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            style={{ flex: 1, maxWidth: 300 }}
          />
        </div>

        <label style={{ display: "flex", alignItems: "center", gap: 6, cursor: "pointer", fontSize: 13, color: "var(--text-secondary)" }}>
          Auto-follow
          <input
            type="checkbox"
            checked={autoFollow}
            onChange={(e) => setAutoFollow(e.target.checked)}
            style={{ accentColor: "var(--brand)" }}
          />
        </label>
      </div>

      {/* Level filter chips */}
      <div style={{ display: "flex", gap: 6, marginBottom: 16 }}>
        {(["debug", "info", "warn", "error"] as const).map((level) => (
          <button
            key={level}
            className={`chip log-chip ${level} ${levels[level] ? "active" : ""}`}
            onClick={() => toggleLevel(level)}
            style={{ cursor: "pointer", border: "none", padding: "4px 12px", borderRadius: 12, fontSize: 12, fontWeight: 600 }}
          >
            {level.toUpperCase()}
            {levelCounts[level] > 0 && (
              <span style={{ marginLeft: 4, opacity: 0.7 }}>({levelCounts[level]})</span>
            )}
          </button>
        ))}
      </div>

      {/* Log stream */}
      <div className="log-stream" ref={streamRef} style={{ maxHeight: "calc(100vh - 280px)", minHeight: 300 }}>
        {filtered.length === 0 ? (
          <div className="empty-state" style={{ padding: 40 }}>
            {loading ? (
              <p>Loading logs…</p>
            ) : logs.length === 0 ? (
              <>
                <Icon name="check" />
                <p>No log entries yet. Logs appear as you interact with ClawDesk.</p>
              </>
            ) : (
              <p>No logs match the current filters.</p>
            )}
          </div>
        ) : (
          filtered.map((l) => (
            <div key={l.id} className={`log-row log-${l.level}`}>
              <span className="log-time">{formatTime(l.timestamp)}</span>
              <span className={`log-level ${l.level}`}>{l.level.toUpperCase().padEnd(5)}</span>
              <span className="log-subsystem">{l.subsystem}</span>
              <span className="log-message">{l.message}</span>
            </div>
          ))
        )}
      </div>

      <div style={{ marginTop: 8, fontSize: 12, color: "var(--text-tertiary)" }}>
        {filtered.length} entries shown · {logs.length} total
      </div>
    </PageLayout>
  );
}
