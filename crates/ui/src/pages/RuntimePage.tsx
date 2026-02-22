import { useState, useEffect, useCallback } from "react";
import * as api from "../api";
import { PageLayout } from "../components/PageLayout";
import { Icon } from "../components/Icon";
import type {
  DurableRunInfo,
  RuntimeStatusInfo,
  CheckpointEntry,
  DlqEntry,
} from "../types";

// ── Types ─────────────────────────────────────────────────────

type RuntimeTab = "runs" | "dlq" | "status";

// ── Props ─────────────────────────────────────────────────────

export interface RuntimePageProps {
  pushToast: (msg: string) => void;
}

// ── Component ─────────────────────────────────────────────────

export function RuntimePage({ pushToast }: RuntimePageProps) {
  const [tab, setTab] = useState<RuntimeTab>("runs");
  const [runs, setRuns] = useState<DurableRunInfo[]>([]);
  const [dlq, setDlq] = useState<DlqEntry[]>([]);
  const [status, setStatus] = useState<RuntimeStatusInfo | null>(null);
  const [loading, setLoading] = useState(true);
  const [selectedRunId, setSelectedRunId] = useState<string | null>(null);
  const [checkpoints, setCheckpoints] = useState<CheckpointEntry[]>([]);
  const [cpLoading, setCpLoading] = useState(false);

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      const [r, d, s] = await Promise.all([
        api.listDurableRuns().catch(() => [] as DurableRunInfo[]),
        api.getDlq().catch(() => [] as DlqEntry[]),
        api.getRuntimeStatus().catch(() => null),
      ]);
      setRuns(r);
      setDlq(d);
      setStatus(s);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => { refresh(); }, [refresh]);

  const loadCheckpoints = useCallback(async (runId: string) => {
    setSelectedRunId(runId);
    setCpLoading(true);
    try {
      const cps = await api.listCheckpoints(runId);
      setCheckpoints(cps);
    } catch {
      setCheckpoints([]);
    } finally {
      setCpLoading(false);
    }
  }, []);

  const handleCancel = useCallback(async (runId: string) => {
    try {
      await api.cancelDurableRun(runId, "Cancelled from UI");
      pushToast(`Run ${runId.substring(0, 8)} cancelled`);
      refresh();
    } catch (e: any) {
      pushToast(`Cancel failed: ${e}`);
    }
  }, [pushToast, refresh]);

  const handleResume = useCallback(async (runId: string) => {
    try {
      await api.resumeDurableRun(runId);
      pushToast(`Run ${runId.substring(0, 8)} resumed`);
      refresh();
    } catch (e: any) {
      pushToast(`Resume failed: ${e}`);
    }
  }, [pushToast, refresh]);

  const stateColor = (state: string) => {
    switch (state) {
      case "running": return "var(--green)";
      case "suspended": case "pending": return "var(--amber)";
      case "failed": return "var(--red)";
      case "completed": return "var(--cyan)";
      default: return "var(--text-tertiary)";
    }
  };

  const runsByState = {
    running: runs.filter((r) => r.state === "running"),
    suspended: runs.filter((r) => r.state === "suspended"),
    pending: runs.filter((r) => r.state === "pending"),
    failed: runs.filter((r) => r.state === "failed"),
  };

  return (
    <PageLayout
      title="Agent Runtime"
      subtitle="Monitor durable runs, checkpoints, and dead letter queue."
      actions={
        <button className="btn subtle" onClick={refresh} disabled={loading}>
          {loading ? "Loading\u2026" : "Refresh"}
        </button>
      }
    >
      {/* Tab Bar */}
      <div style={{ display: "flex", gap: 4, marginBottom: 16 }}>
        {(["runs", "dlq", "status"] as RuntimeTab[]).map((t) => (
          <button
            key={t}
            className={`btn ${tab === t ? "primary" : "subtle"}`}
            onClick={() => setTab(t)}
          >
            {t === "runs" ? `Runs (${runs.length})` : t === "dlq" ? `DLQ (${dlq.length})` : "Status"}
          </button>
        ))}
      </div>

      {/* ── Runs Tab ──────────────────────────────────────── */}
      {tab === "runs" && (
        <div style={{ display: "flex", gap: 16 }}>
          {/* Run List */}
          <div style={{ flex: 1, display: "flex", flexDirection: "column", gap: 8 }}>
            {/* State summary */}
            <div style={{ display: "flex", gap: 12, marginBottom: 8 }}>
              {(["running", "suspended", "pending", "failed"] as const).map((s) => (
                <div key={s} style={{ display: "flex", alignItems: "center", gap: 4, fontSize: 13, color: "var(--text-secondary)" }}>
                  <span className="status-dot-sm" style={{ backgroundColor: stateColor(s) }} />
                  {s}: {runsByState[s].length}
                </div>
              ))}
            </div>

            {runs.length === 0 && !loading && (
              <div className="empty-state centered" style={{ padding: 40 }}>
                <p>No durable runs recorded.</p>
              </div>
            )}

            {runs.map((run) => (
              <div
                key={run.run_id}
                className="panel-card"
                style={{
                  borderLeft: `3px solid ${stateColor(run.state)}`,
                  cursor: "pointer",
                  background: selectedRunId === run.run_id ? "var(--bg-secondary)" : undefined,
                }}
                onClick={() => loadCheckpoints(run.run_id)}
              >
                <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
                  <div>
                    <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                      <span className="status-dot-sm" style={{ backgroundColor: stateColor(run.state) }} />
                      <strong style={{ color: "var(--text-primary)", fontFamily: "monospace", fontSize: 13 }}>
                        {run.run_id.substring(0, 16)}{run.run_id.length > 16 ? "\u2026" : ""}
                      </strong>
                      <span className="trust-badge" style={{ textTransform: "capitalize" }}>{run.state}</span>
                    </div>
                    <div style={{ color: "var(--text-tertiary)", fontSize: 12, marginTop: 4 }}>
                      Worker: {run.worker_id}
                    </div>
                  </div>
                  <div style={{ display: "flex", gap: 6 }}>
                    {(run.state === "suspended" || run.state === "failed") && (
                      <button className="btn subtle" style={{ fontSize: 12 }} onClick={(e) => { e.stopPropagation(); handleResume(run.run_id); }}>
                        <Icon name="play" /> Resume
                      </button>
                    )}
                    {run.state === "running" && (
                      <button className="btn subtle" style={{ fontSize: 12, color: "var(--red)" }} onClick={(e) => { e.stopPropagation(); handleCancel(run.run_id); }}>
                        Cancel
                      </button>
                    )}
                  </div>
                </div>
              </div>
            ))}
          </div>

          {/* Checkpoint Sidebar */}
          {selectedRunId && (
            <div style={{ width: 320, flexShrink: 0, display: "flex", flexDirection: "column", gap: 8 }}>
              <div className="panel-card">
                <h3 className="panel-title" style={{ margin: 0 }}>
                  <Icon name="clock" className="w-4 h-4" /> Checkpoints
                </h3>
                <div style={{ color: "var(--text-tertiary)", fontSize: 12, marginTop: 4 }}>
                  Run: {selectedRunId.substring(0, 16)}\u2026
                </div>
              </div>

              {cpLoading && (
                <div style={{ color: "var(--text-tertiary)", fontSize: 13, textAlign: "center", padding: 20 }}>
                  Loading checkpoints\u2026
                </div>
              )}

              {!cpLoading && checkpoints.length === 0 && (
                <div className="empty-state" style={{ padding: 20, textAlign: "center" }}>
                  No checkpoints found.
                </div>
              )}

              {checkpoints.map((cp, i) => (
                <div key={i} className="panel-card" style={{ fontSize: 12 }}>
                  <div style={{ display: "flex", justifyContent: "space-between" }}>
                    <span style={{ color: "var(--text-primary)", fontWeight: 500 }}>Step {cp.step_index ?? i}</span>
                    <span style={{ color: "var(--text-tertiary)" }}>{cp.created_at ?? ""}</span>
                  </div>
                  {cp.state_snapshot && (
                    <div style={{ marginTop: 4, padding: 6, background: "var(--bg-tertiary)", borderRadius: 4, fontFamily: "monospace", fontSize: 11, maxHeight: 80, overflow: "auto", color: "var(--text-secondary)" }}>
                      {typeof cp.state_snapshot === "string"
                        ? cp.state_snapshot.substring(0, 200)
                        : JSON.stringify(cp.state_snapshot, null, 2).substring(0, 200)}
                    </div>
                  )}
                </div>
              ))}
            </div>
          )}
        </div>
      )}

      {/* ── DLQ Tab ───────────────────────────────────────── */}
      {tab === "dlq" && (
        <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
          {dlq.length === 0 && !loading && (
            <div className="empty-state centered" style={{ padding: 40 }}>
              <Icon name="check" />
              <p>Dead letter queue is empty — all runs completed cleanly.</p>
            </div>
          )}

          {dlq.map((entry, i) => (
            <div key={entry.id ?? i} className="panel-card" style={{ borderLeft: "3px solid var(--red)" }}>
              <div style={{ display: "flex", justifyContent: "space-between", alignItems: "flex-start" }}>
                <div>
                  <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                    <Icon name="alert" className="w-4 h-4" />
                    <strong style={{ color: "var(--text-primary)", fontFamily: "monospace", fontSize: 13 }}>
                      {(entry.run_id ?? entry.id ?? `dlq-${i}`).substring(0, 16)}
                    </strong>
                  </div>
                  <div style={{ color: "var(--red)", fontSize: 12, marginTop: 4 }}>
                    {entry.error ?? "Unknown error"}
                  </div>
                </div>
                <div style={{ color: "var(--text-tertiary)", fontSize: 11, whiteSpace: "nowrap" }}>
                  {entry.failed_at ?? ""}
                  {entry.retry_count != null && ` · ${entry.retry_count} retries`}
                </div>
              </div>
              {entry.payload && (
                <div style={{ marginTop: 6, padding: 8, background: "var(--bg-tertiary)", borderRadius: 6, fontSize: 11, fontFamily: "monospace", maxHeight: 60, overflow: "auto", color: "var(--text-secondary)" }}>
                  {typeof entry.payload === "string" ? entry.payload.substring(0, 300) : JSON.stringify(entry.payload, null, 2).substring(0, 300)}
                </div>
              )}
            </div>
          ))}
        </div>
      )}

      {/* ── Status Tab ────────────────────────────────────── */}
      {tab === "status" && (
        <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(240px, 1fr))", gap: 12 }}>
          <StatusCard
            label="Durable Runner"
            value={status?.durable_runner_available ? "Available" : "Unavailable"}
            color={status?.durable_runner_available ? "var(--green)" : "var(--red)"}
          />
          <StatusCard label="Worker ID" value={status?.worker_id ?? "unknown"} color="var(--cyan)" />
          <StatusCard label="Checkpoint Store" value={status?.checkpoint_store ?? "none"} color="var(--purple)" />
          <StatusCard label="Journal" value={status?.journal ?? "none"} color="var(--amber)" />
          <StatusCard label="Lease Manager" value={status?.lease_manager ?? "none"} color="var(--brand)" />
          <StatusCard label="Active Runs" value={String(runs.filter((r) => r.state === "running").length)} color="var(--green)" />
          <StatusCard label="Suspended Runs" value={String(runs.filter((r) => r.state === "suspended").length)} color="var(--amber)" />
          <StatusCard label="DLQ Entries" value={String(dlq.length)} color={dlq.length > 0 ? "var(--red)" : "var(--green)"} />
        </div>
      )}
    </PageLayout>
  );
}

// ── Subcomponents ─────────────────────────────────────────────

function StatusCard({ label, value, color }: { label: string; value: string; color: string }) {
  return (
    <div className="panel-card" style={{ borderTop: `3px solid ${color}` }}>
      <div style={{ color: "var(--text-tertiary)", fontSize: 12, marginBottom: 4 }}>{label}</div>
      <div style={{ color: "var(--text-primary)", fontSize: 18, fontWeight: 600 }}>{value}</div>
    </div>
  );
}
