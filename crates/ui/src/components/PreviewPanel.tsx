import { useState, useEffect, useCallback, useRef } from "react";
import * as api from "../api";
import type { PreviewService } from "../api";

interface Props {
  onClose: () => void;
}

export function PreviewPanel({ onClose }: Props) {
  const [services, setServices] = useState<PreviewService[]>([]);
  const [active, setActive] = useState<PreviewService | null>(null);
  const [viewport, setViewport] = useState<"desktop" | "mobile">("desktop");
  const [addPort, setAddPort] = useState("");
  const [addLabel, setAddLabel] = useState("");
  const [checking, setChecking] = useState(false);
  const iframeRef = useRef<HTMLIFrameElement>(null);

  const refresh = useCallback(async () => {
    try {
      const list = await api.previewList();
      setServices(list);
      if (list.length > 0 && !active) {
        setActive(list[0]);
      }
    } catch (e) {
      console.error("Failed to load preview services:", e);
    }
  }, [active]);

  useEffect(() => {
    refresh();
    const interval = setInterval(refresh, 5000);
    return () => clearInterval(interval);
  }, [refresh]);

  const handleAdd = async () => {
    const port = parseInt(addPort, 10);
    if (!port || port < 1 || port > 65535) return;

    setChecking(true);
    try {
      const alive = await api.previewCheckPort(port);
      if (!alive) {
        alert(`No service detected on port ${port}`);
        setChecking(false);
        return;
      }
      const label = addLabel.trim() || `Service :${port}`;
      const svc = await api.previewRegister(`manual_${port}`, label, port);
      setServices((prev) => [...prev, svc]);
      setActive(svc);
      setAddPort("");
      setAddLabel("");
    } catch (e: any) {
      alert(`Failed: ${e}`);
    } finally {
      setChecking(false);
    }
  };

  const handleRemove = async (id: string) => {
    try {
      await api.previewRemove(id);
      setServices((prev) => prev.filter((s) => s.id !== id));
      if (active?.id === id) {
        setActive(null);
      }
    } catch (e) {
      console.error("Failed to remove:", e);
    }
  };

  const handleRefreshIframe = () => {
    if (iframeRef.current) {
      iframeRef.current.src = iframeRef.current.src;
    }
  };

  return (
    <div style={panelStyle}>
      {/* Header */}
      <div style={headerStyle}>
        <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <span style={{ fontSize: 14, fontWeight: 600 }}>Preview</span>
          {active && (
            <span style={{ fontSize: 11, color: "var(--text-tertiary)" }}>
              {active.url}
            </span>
          )}
        </div>
        <div style={{ display: "flex", gap: 4 }}>
          {/* Viewport toggle */}
          <button
            onClick={() => setViewport("desktop")}
            style={iconBtnStyle(viewport === "desktop")}
            title="Desktop"
          >
            ▢
          </button>
          <button
            onClick={() => setViewport("mobile")}
            style={iconBtnStyle(viewport === "mobile")}
            title="Mobile"
          >
            ▯
          </button>
          <button onClick={handleRefreshIframe} style={iconBtnStyle(false)} title="Refresh">
            ↻
          </button>
          <button onClick={onClose} style={iconBtnStyle(false)} title="Close">
            ✕
          </button>
        </div>
      </div>

      {/* Service tabs */}
      {services.length > 1 && (
        <div style={tabBarStyle}>
          {services.map((svc) => (
            <button
              key={svc.id}
              onClick={() => setActive(svc)}
              style={{
                ...tabStyle,
                borderBottom: active?.id === svc.id ? "2px solid var(--accent)" : "2px solid transparent",
                color: active?.id === svc.id ? "var(--accent)" : "var(--text-secondary)",
              }}
            >
              {svc.label}
              <span
                onClick={(e) => { e.stopPropagation(); handleRemove(svc.id); }}
                style={{ marginLeft: 6, cursor: "pointer", fontSize: 10, opacity: 0.5 }}
              >
                ✕
              </span>
            </button>
          ))}
        </div>
      )}

      {/* Preview iframe or empty state */}
      {active ? (
        <div style={iframeContainerStyle}>
          <div style={{
            width: viewport === "mobile" ? 375 : "100%",
            height: "100%",
            margin: viewport === "mobile" ? "0 auto" : undefined,
            border: viewport === "mobile" ? "1px solid var(--border)" : "none",
            borderRadius: viewport === "mobile" ? 12 : 0,
            overflow: "hidden",
            transition: "width 0.2s",
          }}>
            <iframe
              ref={iframeRef}
              src={active.url}
              style={iframeStyle}
              sandbox="allow-scripts allow-same-origin allow-forms allow-popups allow-popups-to-escape-sandbox"
              title={`Preview: ${active.label}`}
            />
          </div>
        </div>
      ) : (
        <div style={emptyStyle}>
          <div style={{ marginBottom: 16 }}>
            <div style={{ fontSize: 14, fontWeight: 600, marginBottom: 4 }}>No preview active</div>
            <div style={{ fontSize: 12, color: "var(--text-secondary)" }}>
              Add a running service by port number, or the agent will register one automatically.
            </div>
          </div>
          <div style={{ display: "flex", gap: 6, alignItems: "center" }}>
            <input
              type="text"
              value={addLabel}
              onChange={(e) => setAddLabel(e.target.value)}
              placeholder="Label (optional)"
              style={inputStyle}
            />
            <input
              type="number"
              value={addPort}
              onChange={(e) => setAddPort(e.target.value)}
              placeholder="Port"
              onKeyDown={(e) => e.key === "Enter" && handleAdd()}
              style={{ ...inputStyle, width: 80 }}
              min={1}
              max={65535}
            />
            <button
              onClick={handleAdd}
              disabled={checking || !addPort}
              style={btnStyle}
            >
              {checking ? "Checking..." : "Add"}
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

/* ── Styles ────────────────────────────────────────────────── */

const panelStyle: React.CSSProperties = {
  display: "flex",
  flexDirection: "column",
  height: "100%",
  background: "var(--surface)",
  borderLeft: "1px solid var(--border)",
};

const headerStyle: React.CSSProperties = {
  display: "flex",
  justifyContent: "space-between",
  alignItems: "center",
  padding: "8px 12px",
  borderBottom: "1px solid var(--border)",
  background: "var(--surface)",
};

const tabBarStyle: React.CSSProperties = {
  display: "flex",
  gap: 0,
  borderBottom: "1px solid var(--border)",
  overflowX: "auto",
};

const tabStyle: React.CSSProperties = {
  padding: "6px 12px",
  fontSize: 12,
  fontWeight: 500,
  border: "none",
  background: "none",
  cursor: "pointer",
  whiteSpace: "nowrap",
};

const iframeContainerStyle: React.CSSProperties = {
  flex: 1,
  overflow: "hidden",
  background: "#fff",
};

const iframeStyle: React.CSSProperties = {
  width: "100%",
  height: "100%",
  border: "none",
};

const emptyStyle: React.CSSProperties = {
  flex: 1,
  display: "flex",
  flexDirection: "column",
  justifyContent: "center",
  alignItems: "center",
  padding: 32,
  color: "var(--text-secondary)",
};

const inputStyle: React.CSSProperties = {
  padding: "6px 10px",
  fontSize: 12,
  border: "1px solid var(--border)",
  borderRadius: 6,
  background: "var(--surface)",
  color: "var(--text-primary)",
  outline: "none",
};

const btnStyle: React.CSSProperties = {
  padding: "6px 14px",
  fontSize: 12,
  fontWeight: 500,
  border: "none",
  borderRadius: 6,
  cursor: "pointer",
  background: "var(--accent)",
  color: "#fff",
};

function iconBtnStyle(active: boolean): React.CSSProperties {
  return {
    width: 28,
    height: 28,
    display: "flex",
    alignItems: "center",
    justifyContent: "center",
    border: "1px solid var(--border)",
    borderRadius: 4,
    background: active ? "var(--accent)" : "transparent",
    color: active ? "#fff" : "var(--text-secondary)",
    cursor: "pointer",
    fontSize: 14,
  };
}
