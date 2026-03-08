import { useState, useEffect, useCallback } from "react";
import { listen } from "@tauri-apps/api/event";
import * as api from "../api";
import type { LocalModelsStatus, ModelFit, RunningModel } from "../api";
import { PageLayout } from "../components/PageLayout";
import { Icon } from "../components/Icon";

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

interface DownloadProgress {
  model_name: string;
  percent: number;
  downloaded_bytes: number;
  total_bytes: number;
}

interface Props {
  pushToast: (text: string) => void;
}

function fmtGb(n: number): string {
  return n < 1 ? `${(n * 1024).toFixed(0)} MB` : `${n.toFixed(1)} GB`;
}

function fmtTps(n: number): string {
  return `${n.toFixed(1)} tok/s`;
}

function formatStateLabel(state: RunningModel["state"]): string {
  return state.charAt(0).toUpperCase() + state.slice(1);
}

function findMatchingRunningModel(name: string, runningModels: RunningModel[]): RunningModel | undefined {
  const normalizedName = name.toLowerCase();
  return runningModels.find((running) => {
    const runningName = running.name.toLowerCase();
    return runningName.includes(normalizedName) || normalizedName.includes(runningName);
  });
}

function isModelActive(state?: RunningModel["state"]): boolean {
  return state === "ready" || state === "starting" || state === "stopping";
}

function pluralize(count: number, singular: string, plural = `${singular}s`): string {
  return `${count} ${count === 1 ? singular : plural}`;
}

export function LocalModelsPage({ pushToast }: Props) {
  const [status, setStatus] = useState<LocalModelsStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [activeTab, setActiveTab] = useState<"recommended" | "downloaded" | "running">("recommended");
  const [downloads, setDownloads] = useState<Record<string, DownloadProgress>>({});
  const [filter, setFilter] = useState<"all" | "runnable" | "perfect">("all");
  const [serverPath, setServerPath] = useState("");
  const [showSettings, setShowSettings] = useState(false);
  const [importPath, setImportPath] = useState("");
  const [ttlSecs, setTtlSecs] = useState(0);

  const refresh = useCallback(async () => {
    try {
      const nextStatus = await api.localModelsStatus();
      setStatus(nextStatus);
    } catch (e) {
      console.error("Failed to load local models status:", e);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

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
      unlisten.then((fn) => fn());
    };
  }, [pushToast, refresh]);

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

  const filteredModels = (status?.recommended_models ?? []).filter((model) => {
    if (filter === "all") return true;
    if (filter === "runnable") return model.fit_level !== "too_tight";
    if (filter === "perfect") return model.fit_level === "perfect";
    return true;
  });

  const sys = status?.system;
  const downloadEntries = Object.values(downloads);
  const recommendedCount = status?.recommended_models.length ?? 0;
  const runnableCount = (status?.recommended_models ?? []).filter((model) => model.fit_level !== "too_tight").length;
  const perfectCount = (status?.recommended_models ?? []).filter((model) => model.fit_level === "perfect").length;
  const downloadedCount = status?.downloaded_models.length ?? 0;
  const runningCount = status?.running_models.length ?? 0;
  const readyCount = (status?.running_models ?? []).filter((model) => model.state === "ready").length;
  const totalDownloadedSize = (status?.downloaded_models ?? []).reduce((sum, model) => sum + model.size_gb, 0);

  if (loading) {
    return (
      <div className="local-models-loading">
        <Icon name="loader" className="local-models-loading__icon" />
        <span>Detecting hardware...</span>
      </div>
    );
  }

  return (
    <PageLayout
      title="Local Models"
      subtitle="Run LLMs locally without Ollama or LM Studio"
      actions={
        <div className="local-models-header-actions">
          <button
            className={`local-models-button local-models-button--secondary ${showSettings ? "is-active" : ""}`}
            onClick={() => setShowSettings((prev) => !prev)}
          >
            <Icon name="settings" />
            <span>Settings</span>
          </button>
          <button className="local-models-button local-models-button--secondary" onClick={refresh}>
            <Icon name="refresh" />
            <span>Refresh</span>
          </button>
        </div>
      }
    >
      <div className="local-models-page">
        <section className="local-models-hero">
          <div className="local-models-hero__copy">
            <div className={`local-models-hero__status ${status?.llama_server_available ? "is-ready" : "is-missing"}`}>
              <Icon name={status?.llama_server_available ? "check" : "alert"} />
              <span>{status?.llama_server_available ? "Engine ready" : "llama-server missing"}</span>
            </div>
            <h2 className="local-models-hero__title">Private inference tuned to this machine.</h2>
            <p className="local-models-hero__body">
              Browse models ranked for your hardware, keep a local library, and launch runtime servers without leaving
              ClawDesk.
            </p>
            <div className="local-models-hero__chips">
              <span className="local-models-chip">{sys?.backend?.toUpperCase() ?? "UNKNOWN"} backend</span>
              <span className="local-models-chip">{sys?.has_gpu ? sys?.gpu_name ?? "GPU" : "CPU inference"}</span>
              <span className="local-models-chip">{fmtGb(sys?.available_ram_gb ?? 0)} free RAM</span>
            </div>
          </div>

          <div className="local-models-hero__stats">
            <HeroStat label="Recommended" value={recommendedCount.toString()} sub={`${runnableCount} runnable`} accent />
            <HeroStat label="Downloaded" value={downloadedCount.toString()} sub={fmtGb(totalDownloadedSize)} />
            <HeroStat label="Live" value={readyCount.toString()} sub={pluralize(runningCount, "process")} />
            <HeroStat label="Perfect Fits" value={perfectCount.toString()} sub="Best overall picks" />
          </div>
        </section>

        {showSettings && (
          <section className="local-models-settings panel-card">
            <div className="local-models-section-heading">
              <div>
                <h3>Runtime Settings</h3>
                <p>Adjust the llama.cpp binary, import external GGUF files, or control idle auto-unload.</p>
              </div>
              <span className="local-models-chip">Models dir: {status?.models_dir ?? "~/.clawdesk/models"}</span>
            </div>

            <div className="local-models-settings__grid">
              <SettingField
                label="llama-server path"
                value={serverPath}
                placeholder="/usr/local/bin/llama-server"
                buttonLabel="Save"
                onChange={setServerPath}
                onSubmit={handleSetServerPath}
              />
              <SettingField
                label="Import GGUF"
                value={importPath}
                placeholder="/path/to/model.gguf"
                buttonLabel="Import"
                onChange={setImportPath}
                onSubmit={handleImport}
              />
              <div className="local-models-setting-card">
                <label className="local-models-setting-card__label">Auto-unload</label>
                <div className="local-models-setting-card__controls">
                  <input
                    type="number"
                    min={0}
                    step={60}
                    value={ttlSecs}
                    onChange={(e) => setTtlSecs(Number(e.target.value))}
                    className="local-models-input local-models-input--short"
                    placeholder="0"
                  />
                  <button className="local-models-button local-models-button--primary" onClick={handleSetTtl}>
                    Set
                  </button>
                </div>
                <p className="local-models-setting-card__meta">
                  {ttlSecs === 0 ? "Disabled" : `${Math.floor(ttlSecs / 60)}m ${ttlSecs % 60}s idle timeout`}
                </p>
              </div>
            </div>
          </section>
        )}

        {sys && (
          <section className="local-models-hardware panel-card">
            <div className="local-models-section-heading">
              <div>
                <h3>Hardware Profile</h3>
                <p>Your model recommendations are ranked against this detected machine profile.</p>
              </div>
            </div>

            <div className="local-models-hardware__grid">
              <HardwareTile icon="cpu" label="CPU" value={sys.cpu_name} detail={`${sys.total_cpu_cores} cores`} />
              <HardwareTile icon="layers" label="Memory" value={fmtGb(sys.total_ram_gb)} detail={`${fmtGb(sys.available_ram_gb)} available`} />
              <HardwareTile
                icon="zap"
                label="Accelerator"
                value={sys.gpu_name ?? "CPU-only"}
                detail={
                  sys.has_gpu
                    ? `${fmtGb(sys.gpu_vram_gb ?? sys.total_gpu_vram_gb ?? 0)} VRAM • ${sys.backend.toUpperCase()}`
                    : "No dedicated GPU detected"
                }
              />
              <HardwareTile
                icon="activity"
                label="Runtime"
                value={status?.llama_server_available ? "Ready" : "Needs setup"}
                detail={
                  status?.llama_server_available ? "Launches local inference servers" : "Install llama.cpp or set a custom path"
                }
                tone={status?.llama_server_available ? "ok" : "warn"}
              />
            </div>
          </section>
        )}

        {downloadEntries.length > 0 && (
          <section className="local-models-downloads panel-card">
            <div className="local-models-section-heading">
              <div>
                <h3>Downloads in Progress</h3>
                <p>Transfers continue in the background while recommendations and runtime state stay live.</p>
              </div>
            </div>

            <div className="local-models-downloads__list">
              {downloadEntries.map((download) => (
                <div key={download.model_name} className="local-models-download-row">
                  <div className="local-models-download-row__header">
                    <div>
                      <div className="local-models-download-row__name">{download.model_name}</div>
                      <div className="local-models-download-row__meta">
                        {fmtGb(download.downloaded_bytes / 1073741824)} of {fmtGb(download.total_bytes / 1073741824)}
                      </div>
                    </div>
                    <span className="local-models-chip">{download.percent.toFixed(1)}%</span>
                  </div>
                  <div className="local-models-progress">
                    <div className="local-models-progress__bar" style={{ width: `${download.percent}%` }} />
                  </div>
                </div>
              ))}
            </div>
          </section>
        )}

        <section className="local-models-library panel-card">
          <div className="local-models-library__toolbar">
            <div className="local-models-tabs" role="tablist" aria-label="Local models views">
              {(
                [
                  ["recommended", `Recommended (${filteredModels.length})`],
                  ["downloaded", `Downloaded (${downloadedCount})`],
                  ["running", `Running (${runningCount})`],
                ] as const
              ).map(([key, label]) => (
                <button
                  key={key}
                  className={`local-models-tab ${activeTab === key ? "is-active" : ""}`}
                  onClick={() => setActiveTab(key)}
                >
                  {label}
                </button>
              ))}
            </div>

            {activeTab === "recommended" && (
              <div className="local-models-filters">
                {([
                  ["all", `All ${recommendedCount}`],
                  ["runnable", `Runnable ${runnableCount}`],
                  ["perfect", `Perfect ${perfectCount}`],
                ] as const).map(([value, label]) => (
                  <button
                    key={value}
                    className={`local-models-filter ${filter === value ? "is-active" : ""}`}
                    onClick={() => setFilter(value)}
                  >
                    {label}
                  </button>
                ))}
              </div>
            )}
          </div>

          {activeTab === "recommended" && (
            <div className="local-models-grid">
              {filteredModels.map((model) => {
                const runningModel = findMatchingRunningModel(model.model.name, status?.running_models ?? []);

                return (
                  <ModelCard
                    key={`${model.model.name}-${model.best_quant}`}
                    fit={model}
                    downloading={Boolean(downloads[model.model.name])}
                    runningState={runningModel?.state}
                    onDownload={() => handleDownload(model)}
                    onStart={() => handleStart(model.model.name)}
                  />
                );
              })}

              {filteredModels.length === 0 && (
                <EmptyState
                  icon="search"
                  title="No models match this filter"
                  description="Try a broader fit filter or refresh recommendations after changing your runtime settings."
                />
              )}
            </div>
          )}

          {activeTab === "downloaded" && (
            <div className="local-models-list">
              {downloadedCount === 0 ? (
                <EmptyState
                  icon="archive"
                  title="No downloaded models yet"
                  description="Start in Recommended to find models that fit this machine and pull them into your local library."
                />
              ) : (
                (status?.downloaded_models ?? []).map((model) => {
                  const running = findMatchingRunningModel(model.name, status?.running_models ?? []);
                  const isActive = isModelActive(running?.state);

                  return (
                    <LibraryRow
                      key={model.name}
                      icon="archive"
                      title={model.name}
                      subtitle={`${fmtGb(model.size_gb)} on disk`}
                      chips={[running ? `${formatStateLabel(running.state)} on :${running.port}` : "Stopped"]}
                      actions={
                        <>
                          {running?.state === "ready" ? (
                            <button className="local-models-button local-models-button--danger" onClick={() => handleStop(model.name)}>
                              <Icon name="pause" />
                              <span>Stop</span>
                            </button>
                          ) : isActive ? (
                            <button className="local-models-button local-models-button--disabled" disabled>
                              <Icon name="loader" />
                              <span>{formatStateLabel(running!.state)}</span>
                            </button>
                          ) : (
                            <button className="local-models-button local-models-button--primary" onClick={() => handleStart(model.name)}>
                              <Icon name="play" />
                              <span>Start</span>
                            </button>
                          )}
                          <button className="local-models-button local-models-button--ghost-danger" onClick={() => handleDelete(model.name)}>
                            <Icon name="trash" />
                            <span>Delete</span>
                          </button>
                        </>
                      }
                    />
                  );
                })
              )}
            </div>
          )}

          {activeTab === "running" && (
            <div className="local-models-list">
              {runningCount === 0 ? (
                <EmptyState
                  icon="activity"
                  title="No models running"
                  description="Start a downloaded model to launch a local inference endpoint and keep it available to the app."
                />
              ) : (
                (status?.running_models ?? []).map((model) => (
                  <LibraryRow
                    key={model.name}
                    icon="cpu"
                    title={model.name}
                    subtitle={model.model_path}
                    chips={[formatStateLabel(model.state), `Port ${model.port}`, model.pid ? `PID ${model.pid}` : "No PID"]}
                    stateColor={STATE_COLORS[model.state] ?? "#6b7280"}
                    actions={
                      <button className="local-models-button local-models-button--danger" onClick={() => handleStop(model.name)}>
                        <Icon name="pause" />
                        <span>Stop</span>
                      </button>
                    }
                  />
                ))
              )}
            </div>
          )}
        </section>
      </div>
    </PageLayout>
  );
}

function HeroStat({
  label,
  value,
  sub,
  accent = false,
}: {
  label: string;
  value: string;
  sub: string;
  accent?: boolean;
}) {
  return (
    <div className={`local-models-hero-stat ${accent ? "is-accent" : ""}`}>
      <span className="local-models-hero-stat__label">{label}</span>
      <strong className="local-models-hero-stat__value">{value}</strong>
      <span className="local-models-hero-stat__sub">{sub}</span>
    </div>
  );
}

function HardwareTile({
  icon,
  label,
  value,
  detail,
  tone = "neutral",
}: {
  icon: string;
  label: string;
  value: string;
  detail: string;
  tone?: "neutral" | "ok" | "warn";
}) {
  return (
    <div className={`local-models-hardware-tile local-models-hardware-tile--${tone}`}>
      <div className="local-models-hardware-tile__icon">
        <Icon name={icon} />
      </div>
      <div className="local-models-hardware-tile__content">
        <span className="local-models-hardware-tile__label">{label}</span>
        <strong className="local-models-hardware-tile__value">{value}</strong>
        <span className="local-models-hardware-tile__detail">{detail}</span>
      </div>
    </div>
  );
}

function SettingField({
  label,
  value,
  placeholder,
  buttonLabel,
  onChange,
  onSubmit,
}: {
  label: string;
  value: string;
  placeholder: string;
  buttonLabel: string;
  onChange: (value: string) => void;
  onSubmit: () => void;
}) {
  return (
    <div className="local-models-setting-card">
      <label className="local-models-setting-card__label">{label}</label>
      <div className="local-models-setting-card__controls">
        <input
          type="text"
          value={value}
          onChange={(e) => onChange(e.target.value)}
          placeholder={placeholder}
          className="local-models-input"
        />
        <button className="local-models-button local-models-button--primary" onClick={onSubmit}>
          {buttonLabel}
        </button>
      </div>
    </div>
  );
}

function ModelCard({
  fit,
  downloading,
  onDownload,
  onStart,
  runningState,
}: {
  fit: ModelFit;
  downloading: boolean;
  onDownload: () => void;
  onStart: () => void;
  runningState?: RunningModel["state"];
}) {
  const model = fit.model;
  const fitColor = FIT_COLORS[fit.fit_level];
  const isActive = isModelActive(runningState);
  const runningColor = runningState ? STATE_COLORS[runningState] ?? "#6b7280" : "#16a34a";

  return (
    <article className="local-model-card">
      <div className="local-model-card__top">
        <div>
          <div className="local-model-card__eyebrow">{model.provider}</div>
          <h3 className="local-model-card__title">{model.name}</h3>
          <p className="local-model-card__subtitle">
            {model.parameter_count} parameters • {model.use_case}
          </p>
        </div>
        <span className="local-model-card__fit" style={{ color: fitColor, backgroundColor: `${fitColor}18` }}>
          {FIT_LABELS[fit.fit_level]}
        </span>
      </div>

      <div className="local-model-card__scoreband">
        <div>
          <span className="local-model-card__score-label">Fit score</span>
          <strong className="local-model-card__score-value">{fit.score.toFixed(0)}</strong>
        </div>
        <div className="local-model-card__score-meta">
          <span>{fmtTps(fit.estimated_tps)}</span>
          <span>{fmtGb(fit.memory_required_gb)}</span>
        </div>
      </div>

      <div className="local-model-card__stats">
        <MiniStat label="Quant" value={fit.best_quant} />
        <MiniStat label="Context" value={`${(model.context_length / 1024).toFixed(0)}K`} />
        <MiniStat label="Mode" value={fit.run_mode.replace("_", " ")} />
      </div>

      <div className="local-model-card__utilization">
        <div className="local-model-card__utilization-head">
          <span>Memory utilization</span>
          <span>{fit.utilization_pct.toFixed(0)}%</span>
        </div>
        <div className="local-models-progress local-models-progress--tight">
          <div
            className="local-models-progress__bar"
            style={{ width: `${Math.min(fit.utilization_pct, 100)}%`, background: fitColor }}
          />
        </div>
      </div>

      <div className="local-model-card__footer">
        <div className="local-model-card__chips">
          <span className="local-models-chip">{fit.installed ? "Downloaded" : "Remote"}</span>
          <span className="local-models-chip">{fit.use_case}</span>
        </div>

        {fit.installed ? (
          isActive ? (
            <div className="local-model-card__running" style={{ color: runningColor }}>
              <span
                className="local-model-card__running-dot"
                style={{ background: runningColor, boxShadow: `0 0 0 5px ${runningColor}1f` }}
              />
              <span>{formatStateLabel(runningState!)}</span>
            </div>
          ) : (
            <button className="local-models-button local-models-button--primary" onClick={onStart}>
              <Icon name="play" />
              <span>Start</span>
            </button>
          )
        ) : (
          <button
            className={`local-models-button ${fit.fit_level === "too_tight" ? "local-models-button--disabled" : "local-models-button--primary"}`}
            onClick={onDownload}
            disabled={downloading || fit.fit_level === "too_tight"}
          >
            <Icon name={downloading ? "loader" : "archive"} />
            <span>{downloading ? "Downloading..." : fit.fit_level === "too_tight" ? "Too Tight" : "Download"}</span>
          </button>
        )}
      </div>
    </article>
  );
}

function MiniStat({ label, value }: { label: string; value: string }) {
  return (
    <div className="local-model-card__stat">
      <span className="local-model-card__stat-label">{label}</span>
      <strong className="local-model-card__stat-value">{value}</strong>
    </div>
  );
}

function LibraryRow({
  icon,
  title,
  subtitle,
  chips,
  actions,
  stateColor,
}: {
  icon: string;
  title: string;
  subtitle: string;
  chips: string[];
  actions: React.ReactNode;
  stateColor?: string;
}) {
  return (
    <div className="local-models-row">
      <div className="local-models-row__identity">
        <div className="local-models-row__icon" style={stateColor ? { color: stateColor, borderColor: `${stateColor}33` } : undefined}>
          <Icon name={icon} />
        </div>
        <div className="local-models-row__copy">
          <div className="local-models-row__title">{title}</div>
          <div className="local-models-row__subtitle">{subtitle}</div>
        </div>
      </div>
      <div className="local-models-row__chips">
        {chips.map((chip) => (
          <span key={chip} className="local-models-chip">
            {chip}
          </span>
        ))}
      </div>
      <div className="local-models-row__actions">{actions}</div>
    </div>
  );
}

function EmptyState({
  icon,
  title,
  description,
}: {
  icon: string;
  title: string;
  description: string;
}) {
  return (
    <div className="local-models-empty-state">
      <div className="local-models-empty-state__icon">
        <Icon name={icon} />
      </div>
      <h3>{title}</h3>
      <p>{description}</p>
    </div>
  );
}
