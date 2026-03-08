import { useState, useEffect, useCallback, useRef } from "react";
import { listen } from "@tauri-apps/api/event";
import * as api from "../api";
import type {
  LocalModelsStatus,
  ModelFit,
  RunningModel,
  DownloadedModel,
  SystemSpecs,
} from "../api";

/* ────────────────────────── helpers ──────────────────────────── */

const FIT_COLORS: Record<string, string> = {
  perfect: "#22c55e",
  good: "#3b82f6",
  marginal: "#f59e0b",
  too_tight: "#ef4444",
};

const FIT_LABELS: Record<string, string> = {
  perfect: "Perfect",
  good: "Good",
  marginal: "Marginal",
  too_tight: "Too Tight",
};

const STATE_COLORS: Record<string, string> = {
  ready: "#22c55e",
  starting: "#f59e0b",
  stopping: "#f59e0b",
  stopped: "#6b7280",
  failed: "#ef4444",
};

function fmtGb(n: number): string {
  return n < 1 ? `${(n * 1024).toFixed(0)} MB` : `${n.toFixed(1)} GB`;
}

function fmtTps(n: number): string {
  return `${n.toFixed(1)} tok/s`;
}

/* ────────────────────────── types ────────────────────────────── */

interface DownloadProgress {
  model_name: string;
  percent: number;
  downloaded_bytes: number;
  total_bytes: number;
}

interface Props {
  pushToast: (text: string) => void;
}

/* ────────────────────────── component ───────────────────────── */

export function LocalModelsPage({ pushToast }: Props) {
  const [status, setStatus] = useState<LocalModelsStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [activeTab, setActiveTab] = useState<
    "recommended" | "downloaded" | "running"
  >("recommended");
  const [downloads, setDownloads] = useState<Record<string, DownloadProgress>>(
    {}
  );
  const [filter, setFilter] = useState<"all" | "runnable" | "perfect">("all");
  const [serverPath, setServerPath] = useState("");
  const [showSettings, setShowSettings] = useState(false);
  const [importPath, setImportPath] = useState("");
  const [ttlSecs, setTtlSecs] = useState(0);

  /* ── load status ─────────────────────────────────────────── */

  const refresh = useCallback(async () => {
    try {
      const s = await api.localModelsStatus();
      setStatus(s);
    } catch (e) {
      console.error("Failed to load local models status:", e);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  /* ── listen for download progress ────────────────────────── */

  useEffect(() => {
    const unlisten = listen<any>("local-model-download", (event) => {
      const data = event.payload;
      if (data.type === "progress") {
        setDownloads((prev) => ({
          ...prev,
          [data.model_name]: {
            model_name: data.model_name,
            percent: data.percent,
            downloaded_bytes: data.downloaded_bytes,
            total_bytes: data.total_bytes,
          },
        }));
      } else if (data.type === "done") {
        setDownloads((prev) => {
          const next = { ...prev };
          delete next[data.model_name];
          return next;
        });
        pushToast(`Downloaded ${data.model_name}`);
        refresh();
      } else if (data.type === "error") {
        setDownloads((prev) => {
          const next = { ...prev };
          delete next[data.model_name];
          return next;
        });
        pushToast(`Download failed: ${data.message}`);
      }
    });

    return () => {
      unlisten.then((f) => f());
    };
  }, [pushToast, refresh]);

  /* ── actions ─────────────────────────────────────────────── */

  const handleDownload = async (model: ModelFit) => {
    try {
      await api.localModelsDownload(model.model.name, model.gguf_download_url);
      pushToast(`Downloading ${model.model.name}...`);
    } catch (e: any) {
      pushToast(`Failed: ${e}`);
    }
  };

  const handleStart = async (name: string) => {
    try {
      const port = await api.localModelsStart(name);
      pushToast(`${name} started on port ${port}`);
      refresh();
    } catch (e: any) {
      pushToast(`Failed to start: ${e}`);
    }
  };

  const handleStop = async (name: string) => {
    try {
      await api.localModelsStop(name);
      pushToast(`${name} stopped`);
      refresh();
    } catch (e: any) {
      pushToast(`Failed to stop: ${e}`);
    }
  };

  const handleDelete = async (name: string) => {
    if (!confirm(`Delete model "${name}"? This cannot be undone.`)) return;
    try {
      await api.localModelsDelete(name);
      pushToast(`Deleted ${name}`);
      refresh();
    } catch (e: any) {
      pushToast(`Failed to delete: ${e}`);
    }
  };

  const handleSetServerPath = async () => {
    try {
      await api.localModelsSetServerPath(serverPath);
      pushToast("llama-server path updated");
      refresh();
    } catch (e: any) {
      pushToast(`Failed: ${e}`);
    }
  };

  const handleImport = async () => {
    if (!importPath.trim()) return;
    try {
      const name = await api.localModelsImport(importPath.trim());
      pushToast(`Imported ${name}`);
      setImportPath("");
      refresh();
    } catch (e: any) {
      pushToast(`Import failed: ${e}`);
    }
  };

  const handleSetTtl = async () => {
    try {
      await api.localModelsSetTtl(ttlSecs);
      pushToast(ttlSecs === 0 ? "Auto-unload disabled" : `Auto-unload set to ${ttlSecs}s`);
    } catch (e: any) {
      pushToast(`Failed: ${e}`);
    }
  };

  /* ── filter models ───────────────────────────────────────── */

  const filteredModels = (status?.recommended_models ?? []).filter((m) => {
    if (filter === "all") return true;
    if (filter === "runnable") return m.fit_level !== "too_tight";
    if (filter === "perfect") return m.fit_level === "perfect";
    return true;
  });

  /* ── render ──────────────────────────────────────────────── */

  if (loading) {
    return (
      <div style={{ display: "flex", justifyContent: "center", alignItems: "center", height: "100%", color: "var(--text-secondary)" }}>
        Detecting hardware...
      </div>
    );
  }

  const sys = status?.system;

  return (
    <div style={{ height: "100%", overflow: "auto", padding: "24px 32px" }}>
      {/* ── Header ─────────────────────────────────────────── */}
      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 24 }}>
        <div>
          <h1 style={{ fontSize: 22, fontWeight: 700, margin: 0, color: "var(--text-primary)" }}>
            Local Models
          </h1>
          <p style={{ margin: "4px 0 0", fontSize: 13, color: "var(--text-secondary)" }}>
            Run LLMs locally — no Ollama or LM Studio needed
          </p>
        </div>
        <div style={{ display: "flex", gap: 8 }}>
          <button
            onClick={() => setShowSettings(!showSettings)}
            style={btnStyle("secondary")}
          >
            Settings
          </button>
          <button onClick={refresh} style={btnStyle("secondary")}>
            Refresh
          </button>
        </div>
      </div>

      {/* ── Settings Panel ─────────────────────────────────── */}
      {showSettings && (
        <div style={{ ...cardStyle, marginBottom: 16 }}>
          <h3 style={{ margin: "0 0 12px", fontSize: 14, fontWeight: 600 }}>Settings</h3>

          {/* llama-server path */}
          <div style={{ display: "flex", gap: 8, alignItems: "center", marginBottom: 12 }}>
            <label style={{ fontSize: 13, color: "var(--text-secondary)", minWidth: 120 }}>
              llama-server path:
            </label>
            <input
              type="text"
              value={serverPath}
              onChange={(e) => setServerPath(e.target.value)}
              placeholder="/usr/local/bin/llama-server"
              style={inputStyle}
            />
            <button onClick={handleSetServerPath} style={btnStyle("primary")}>
              Save
            </button>
          </div>

          {/* Import GGUF */}
          <div style={{ display: "flex", gap: 8, alignItems: "center", marginBottom: 12 }}>
            <label style={{ fontSize: 13, color: "var(--text-secondary)", minWidth: 120 }}>
              Import GGUF:
            </label>
            <input
              type="text"
              value={importPath}
              onChange={(e) => setImportPath(e.target.value)}
              placeholder="/path/to/model.gguf"
              style={inputStyle}
            />
            <button onClick={handleImport} style={btnStyle("primary")}>
              Import
            </button>
          </div>

          {/* Auto-unload TTL */}
          <div style={{ display: "flex", gap: 8, alignItems: "center", marginBottom: 8 }}>
            <label style={{ fontSize: 13, color: "var(--text-secondary)", minWidth: 120 }}>
              Auto-unload (sec):
            </label>
            <input
              type="number"
              value={ttlSecs}
              min={0}
              step={60}
              onChange={(e) => setTtlSecs(Number(e.target.value))}
              placeholder="0 = never"
              style={{ ...inputStyle, maxWidth: 120 }}
            />
            <button onClick={handleSetTtl} style={btnStyle("primary")}>
              Set
            </button>
            <span style={{ fontSize: 11, color: "var(--text-tertiary)" }}>
              {ttlSecs === 0 ? "disabled" : `${Math.floor(ttlSecs / 60)}m ${ttlSecs % 60}s idle`}
            </span>
          </div>

          <div style={{ fontSize: 12, color: "var(--text-tertiary)" }}>
            Models directory: {status?.models_dir ?? "~/.clawdesk/models"}
          </div>
        </div>
      )}

      {/* ── System Info ────────────────────────────────────── */}
      {sys && (
        <div style={{ ...cardStyle, marginBottom: 20 }}>
          <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(180px, 1fr))", gap: 16 }}>
            <InfoTile
              label="CPU"
              value={sys.cpu_name}
              sub={`${sys.total_cpu_cores} cores`}
            />
            <InfoTile
              label="RAM"
              value={fmtGb(sys.total_ram_gb)}
              sub={`${fmtGb(sys.available_ram_gb)} available`}
            />
            <InfoTile
              label="GPU"
              value={sys.gpu_name ?? "None"}
              sub={
                sys.has_gpu
                  ? `${fmtGb(sys.gpu_vram_gb ?? 0)} VRAM • ${sys.backend.toUpperCase()}`
                  : "CPU-only inference"
              }
            />
            <InfoTile
              label="Backend"
              value={sys.backend.toUpperCase()}
              sub={
                sys.unified_memory
                  ? "Unified Memory"
                  : sys.has_gpu
                  ? `${sys.gpu_count} GPU(s)`
                  : "CPU"
              }
            />
            <InfoTile
              label="llama-server"
              value={status?.llama_server_available ? "Available" : "Not Found"}
              sub={
                status?.llama_server_available
                  ? "Ready to run models"
                  : "Install llama.cpp"
              }
              valueColor={status?.llama_server_available ? "#22c55e" : "#ef4444"}
            />
          </div>
        </div>
      )}

      {/* ── Tab Bar ────────────────────────────────────────── */}
      <div style={{ display: "flex", gap: 0, marginBottom: 16, borderBottom: "1px solid var(--border)" }}>
        {(
          [
            ["recommended", `Recommended (${filteredModels.length})`],
            ["downloaded", `Downloaded (${status?.downloaded_models.length ?? 0})`],
            ["running", `Running (${status?.running_models.length ?? 0})`],
          ] as [string, string][]
        ).map(([key, label]) => (
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
              color:
                activeTab === key
                  ? "var(--accent)"
                  : "var(--text-secondary)",
              borderBottom:
                activeTab === key
                  ? "2px solid var(--accent)"
                  : "2px solid transparent",
            }}
          >
            {label}
          </button>
        ))}

        {activeTab === "recommended" && (
          <div style={{ marginLeft: "auto", display: "flex", gap: 4, alignItems: "center", paddingRight: 8 }}>
            {(["all", "runnable", "perfect"] as const).map((f) => (
              <button
                key={f}
                onClick={() => setFilter(f)}
                style={{
                  padding: "4px 10px",
                  fontSize: 11,
                  border: "1px solid var(--border)",
                  borderRadius: 4,
                  background: filter === f ? "var(--accent)" : "transparent",
                  color: filter === f ? "#fff" : "var(--text-secondary)",
                  cursor: "pointer",
                }}
              >
                {f.charAt(0).toUpperCase() + f.slice(1)}
              </button>
            ))}
          </div>
        )}
      </div>

      {/* ── Download Progress ──────────────────────────────── */}
      {Object.values(downloads).length > 0 && (
        <div style={{ marginBottom: 16 }}>
          {Object.values(downloads).map((dl) => (
            <div key={dl.model_name} style={{ ...cardStyle, marginBottom: 8 }}>
              <div style={{ display: "flex", justifyContent: "space-between", marginBottom: 6 }}>
                <span style={{ fontSize: 13, fontWeight: 500 }}>
                  Downloading {dl.model_name}
                </span>
                <span style={{ fontSize: 12, color: "var(--text-secondary)" }}>
                  {dl.percent.toFixed(1)}% •{" "}
                  {fmtGb(dl.downloaded_bytes / 1073741824)} /{" "}
                  {fmtGb(dl.total_bytes / 1073741824)}
                </span>
              </div>
              <div style={{ height: 4, borderRadius: 2, background: "var(--border)", overflow: "hidden" }}>
                <div
                  style={{
                    height: "100%",
                    width: `${dl.percent}%`,
                    background: "var(--accent)",
                    borderRadius: 2,
                    transition: "width 0.3s",
                  }}
                />
              </div>
            </div>
          ))}
        </div>
      )}

      {/* ── Recommended Tab ────────────────────────────────── */}
      {activeTab === "recommended" && (
        <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(340px, 1fr))", gap: 12 }}>
          {filteredModels.map((m) => (
            <ModelCard
              key={m.model.name + m.best_quant}
              fit={m}
              downloading={!!downloads[m.model.name]}
              onDownload={() => handleDownload(m)}
              onStart={() => handleStart(m.model.name)}
              isRunning={
                status?.running_models.some(
                  (r) =>
                    r.name.toLowerCase().includes(m.model.name.toLowerCase()) &&
                    r.state === "ready"
                ) ?? false
              }
            />
          ))}
          {filteredModels.length === 0 && (
            <div style={{ gridColumn: "1/-1", textAlign: "center", padding: 40, color: "var(--text-secondary)" }}>
              No models match the current filter.
            </div>
          )}
        </div>
      )}

      {/* ── Downloaded Tab ─────────────────────────────────── */}
      {activeTab === "downloaded" && (
        <div>
          {(status?.downloaded_models ?? []).length === 0 ? (
            <div style={{ textAlign: "center", padding: 40, color: "var(--text-secondary)" }}>
              No models downloaded yet. Check the Recommended tab to find models
              that fit your hardware.
            </div>
          ) : (
            <table style={{ width: "100%", borderCollapse: "collapse", fontSize: 13 }}>
              <thead>
                <tr style={{ borderBottom: "1px solid var(--border)", textAlign: "left" }}>
                  <th style={thStyle}>Model</th>
                  <th style={thStyle}>Size</th>
                  <th style={thStyle}>Status</th>
                  <th style={{ ...thStyle, textAlign: "right" }}>Actions</th>
                </tr>
              </thead>
              <tbody>
                {(status?.downloaded_models ?? []).map((dm) => {
                  const running = status?.running_models.find((r) =>
                    r.name.toLowerCase().includes(dm.name.toLowerCase())
                  );
                  return (
                    <tr key={dm.name} style={{ borderBottom: "1px solid var(--border)" }}>
                      <td style={tdStyle}>
                        <span style={{ fontWeight: 500 }}>{dm.name}</span>
                      </td>
                      <td style={tdStyle}>{fmtGb(dm.size_gb)}</td>
                      <td style={tdStyle}>
                        {running ? (
                          <span style={{ color: STATE_COLORS[running.state], fontWeight: 500 }}>
                            {running.state} (:{running.port})
                          </span>
                        ) : (
                          <span style={{ color: "var(--text-tertiary)" }}>
                            Stopped
                          </span>
                        )}
                      </td>
                      <td style={{ ...tdStyle, textAlign: "right" }}>
                        <div style={{ display: "flex", gap: 6, justifyContent: "flex-end" }}>
                          {running?.state === "ready" ? (
                            <button onClick={() => handleStop(dm.name)} style={btnStyle("danger")}>
                              Stop
                            </button>
                          ) : (
                            <button onClick={() => handleStart(dm.name)} style={btnStyle("primary")}>
                              Start
                            </button>
                          )}
                          <button onClick={() => handleDelete(dm.name)} style={btnStyle("danger")}>
                            Delete
                          </button>
                        </div>
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          )}
        </div>
      )}

      {/* ── Running Tab ────────────────────────────────────── */}
      {activeTab === "running" && (
        <div>
          {(status?.running_models ?? []).length === 0 ? (
            <div style={{ textAlign: "center", padding: 40, color: "var(--text-secondary)" }}>
              No models currently running. Start a model from the Downloaded tab.
            </div>
          ) : (
            <table style={{ width: "100%", borderCollapse: "collapse", fontSize: 13 }}>
              <thead>
                <tr style={{ borderBottom: "1px solid var(--border)", textAlign: "left" }}>
                  <th style={thStyle}>Model</th>
                  <th style={thStyle}>State</th>
                  <th style={thStyle}>Port</th>
                  <th style={thStyle}>PID</th>
                  <th style={{ ...thStyle, textAlign: "right" }}>Actions</th>
                </tr>
              </thead>
              <tbody>
                {(status?.running_models ?? []).map((rm) => (
                  <tr key={rm.name} style={{ borderBottom: "1px solid var(--border)" }}>
                    <td style={tdStyle}>
                      <span style={{ fontWeight: 500 }}>{rm.name}</span>
                      <div style={{ fontSize: 11, color: "var(--text-tertiary)" }}>
                        {rm.model_path}
                      </div>
                    </td>
                    <td style={tdStyle}>
                      <span style={{ color: STATE_COLORS[rm.state] ?? "#6b7280", fontWeight: 500 }}>
                        {rm.state}
                      </span>
                    </td>
                    <td style={tdStyle}>{rm.port}</td>
                    <td style={tdStyle}>{rm.pid ?? "—"}</td>
                    <td style={{ ...tdStyle, textAlign: "right" }}>
                      <button onClick={() => handleStop(rm.name)} style={btnStyle("danger")}>
                        Stop
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>
      )}
    </div>
  );
}

/* ────────────────────────── sub-components ──────────────────── */

function InfoTile({
  label,
  value,
  sub,
  valueColor,
}: {
  label: string;
  value: string;
  sub: string;
  valueColor?: string;
}) {
  return (
    <div>
      <div style={{ fontSize: 11, color: "var(--text-tertiary)", textTransform: "uppercase", letterSpacing: 0.5, marginBottom: 4 }}>
        {label}
      </div>
      <div style={{ fontSize: 15, fontWeight: 600, color: valueColor ?? "var(--text-primary)", marginBottom: 2 }}>
        {value}
      </div>
      <div style={{ fontSize: 12, color: "var(--text-secondary)" }}>{sub}</div>
    </div>
  );
}

function ModelCard({
  fit,
  downloading,
  onDownload,
  onStart,
  isRunning,
}: {
  fit: ModelFit;
  downloading: boolean;
  onDownload: () => void;
  onStart: () => void;
  isRunning: boolean;
}) {
  const m = fit.model;
  return (
    <div style={cardStyle}>
      {/* header */}
      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "flex-start", marginBottom: 8 }}>
        <div>
          <div style={{ fontSize: 14, fontWeight: 600, color: "var(--text-primary)" }}>
            {m.name}
          </div>
          <div style={{ fontSize: 12, color: "var(--text-secondary)" }}>
            {m.provider} • {m.parameter_count} • {m.use_case}
          </div>
        </div>
        <div
          style={{
            fontSize: 11,
            fontWeight: 600,
            padding: "2px 8px",
            borderRadius: 4,
            background: FIT_COLORS[fit.fit_level] + "20",
            color: FIT_COLORS[fit.fit_level],
          }}
        >
          {FIT_LABELS[fit.fit_level]}
        </div>
      </div>

      {/* stats */}
      <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr 1fr", gap: 8, marginBottom: 10 }}>
        <MiniStat label="Score" value={fit.score.toFixed(0)} />
        <MiniStat label="Speed" value={fmtTps(fit.estimated_tps)} />
        <MiniStat label="Memory" value={fmtGb(fit.memory_required_gb)} />
        <MiniStat label="Quant" value={fit.best_quant} />
        <MiniStat label="Context" value={`${(m.context_length / 1024).toFixed(0)}K`} />
        <MiniStat label="Mode" value={fit.run_mode.replace("_", " ")} />
      </div>

      {/* utilization bar */}
      <div style={{ marginBottom: 10 }}>
        <div style={{ display: "flex", justifyContent: "space-between", fontSize: 11, color: "var(--text-tertiary)", marginBottom: 3 }}>
          <span>Memory utilization</span>
          <span>{fit.utilization_pct.toFixed(0)}%</span>
        </div>
        <div style={{ height: 4, borderRadius: 2, background: "var(--border)", overflow: "hidden" }}>
          <div
            style={{
              height: "100%",
              width: `${Math.min(fit.utilization_pct, 100)}%`,
              background: FIT_COLORS[fit.fit_level],
              borderRadius: 2,
            }}
          />
        </div>
      </div>

      {/* actions */}
      <div style={{ display: "flex", gap: 6 }}>
        {fit.installed ? (
          isRunning ? (
            <span style={{ fontSize: 12, color: "#22c55e", fontWeight: 500 }}>
              Running
            </span>
          ) : (
            <button onClick={onStart} style={btnStyle("primary")}>
              Start
            </button>
          )
        ) : (
          <button
            onClick={onDownload}
            disabled={downloading || fit.fit_level === "too_tight"}
            style={btnStyle(
              fit.fit_level === "too_tight" ? "disabled" : "primary"
            )}
          >
            {downloading ? "Downloading..." : "Download"}
          </button>
        )}
      </div>
    </div>
  );
}

function MiniStat({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <div style={{ fontSize: 10, color: "var(--text-tertiary)", textTransform: "uppercase" }}>
        {label}
      </div>
      <div style={{ fontSize: 13, fontWeight: 500, color: "var(--text-primary)" }}>
        {value}
      </div>
    </div>
  );
}

/* ────────────────────────── styles ──────────────────────────── */

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
  flex: 1,
  padding: "6px 10px",
  fontSize: 13,
  border: "1px solid var(--border)",
  borderRadius: 6,
  background: "var(--surface)",
  color: "var(--text-primary)",
  outline: "none",
};

function btnStyle(
  variant: "primary" | "secondary" | "danger" | "disabled"
): React.CSSProperties {
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
      return {
        ...base,
        background: "transparent",
        border: "1px solid var(--border)",
        color: "var(--text-secondary)",
      };
    case "danger":
      return { ...base, background: "#ef444420", color: "#ef4444" };
    case "disabled":
      return { ...base, background: "var(--border)", color: "var(--text-tertiary)", opacity: 0.6 };
  }
}
