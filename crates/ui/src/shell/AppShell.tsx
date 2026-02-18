import { useCallback, type CSSProperties } from "react";
import { Icon } from "../components/Icon";

export type ShellNavKey = "now" | "ask" | "routines" | "accounts" | "library";

interface ShellNavItem {
  key: ShellNavKey;
  label: string;
  shortcut: string;
  icon: string;
}

interface ShellStatus {
  level: "ok" | "warn" | "error";
  summary: string;
}

interface ExtendedCSSProperties extends CSSProperties {
  WebkitAppRegion?: "drag" | "no-drag";
}

function statusChip(level: ShellStatus["level"]): string {
  if (level === "ok") return "status-dot status-ok";
  if (level === "warn") return "status-dot status-warn";
  return "status-dot status-error";
}

export function AppShell({
  sidebarCollapsed,
  compactSidebar,
  activeNav,
  navItems,
  onNavigate,
  onToggleSidebar,
  onOpenPalette,
  status,
  onToggleStatus,
  safeMode,
  onToggleSafeMode,
  onOpenApprovals,
  approvalCount,
  onOpenSettings,
  showSafeModeBanner,
  children,
  inspector,
  drawerAsModal,
  onOpenInspectorModal,
}: {
  sidebarCollapsed: boolean;
  compactSidebar: boolean;
  activeNav: ShellNavKey;
  navItems: ShellNavItem[];
  onNavigate: (nav: ShellNavKey) => void;
  onToggleSidebar: () => void;
  onOpenPalette: () => void;
  status: ShellStatus;
  onToggleStatus: () => void;
  safeMode: boolean;
  onToggleSafeMode: () => void;
  onOpenApprovals: () => void;
  approvalCount: number;
  onOpenSettings: () => void;
  showSafeModeBanner: boolean;
  children: React.ReactNode;
  inspector: React.ReactNode;
  drawerAsModal: boolean;
  onOpenInspectorModal: () => void;
}) {
  const isMac = typeof navigator !== "undefined" && /Mac/.test(navigator.userAgent);
  const dragStyle: ExtendedCSSProperties | undefined = isMac
    ? { WebkitAppRegion: "drag" }
    : undefined;
  const noDragStyle: ExtendedCSSProperties | undefined = isMac
    ? { WebkitAppRegion: "no-drag" }
    : undefined;
  const handleTopBarMouseDown = useCallback(
    async (event: React.MouseEvent<HTMLElement>) => {
      if (!isMac || event.button !== 0) return;
      const target = event.target as HTMLElement | null;
      if (target?.closest("button, input, textarea, select, a, [role='button'], [data-no-drag]")) {
        return;
      }
      try {
        const { getCurrentWindow } = await import("@tauri-apps/api/window");
        await getCurrentWindow().startDragging();
      } catch {
        // If not running in Tauri, ignore.
      }
    },
    [isMac]
  );

  return (
    <div className={`app-shell ${isMac ? "mac-shell" : ""} ${sidebarCollapsed ? "sidebar-collapsed" : ""}`}>
      <aside className={`sidebar ${sidebarCollapsed ? "compact" : ""}`}>
        <div className="brand">
          <span className="brand-mark">
            <img src="/logo.svg" alt="ClawDesk logo" className="brand-logo" />
          </span>
          <button
            className="collapse-toggle"
            onClick={onToggleSidebar}
            aria-label={sidebarCollapsed ? "Expand sidebar" : "Collapse sidebar"}
            aria-pressed={sidebarCollapsed}
            title={sidebarCollapsed ? "Expand sidebar" : "Collapse sidebar"}
            disabled={compactSidebar}
          >
            <Icon name={sidebarCollapsed ? "collapse-right" : "collapse-left"} />
          </button>
        </div>

        <nav className="nav-list" aria-label="Primary">
          {navItems.map((item) => (
            <button
              key={item.key}
              className={`nav-item ${activeNav === item.key ? "active" : ""}`}
              onClick={() => onNavigate(item.key)}
              title={item.label}
              data-nav-label={item.label}
              aria-current={activeNav === item.key ? "page" : undefined}
            >
              <span className="nav-icon-wrap">
                <Icon name={item.icon} />
              </span>
              {!sidebarCollapsed && <span className="nav-label">{item.label}</span>}
            </button>
          ))}
        </nav>
      </aside>

      <div className={`app-main ${isMac ? "mac-chrome" : ""}`}>
        {isMac && (
          <div
            className="mac-titlebar-strip"
            data-tauri-drag-region
            style={dragStyle}
            aria-hidden="true"
          />
        )}

        <header
          className={`top-bar ${isMac ? "top-bar-mac" : ""}`}
          {...(isMac ? { "data-tauri-drag-region": true } : {})}
          style={dragStyle}
          onMouseDown={handleTopBarMouseDown}
        >
          <button className="command-bar" onClick={onOpenPalette} style={noDragStyle}>
            <div className="command-primary">
              <span className="command-icon-wrap">
                <Icon name="search" />
              </span>
              <div className="command-copy">
                <span>Search ClawDesk: requests, routines, proof, accounts</span>
              </div>
            </div>
            <kbd>⌘K</kbd>
          </button>

          <div className="top-actions" style={noDragStyle}>
            <button className="status-button" onClick={onToggleStatus}>
              <span className={statusChip(status.level)} />
              <span>{status.summary}</span>
            </button>

            <button
              className={`safe-toggle-pill ${safeMode ? "on" : "off"}`}
              title="Safe Mode prevents sending/writing/executing without approval."
              onClick={onToggleSafeMode}
              aria-pressed={safeMode}
              aria-label={safeMode ? "Safe Mode ON" : "Safe Mode OFF"}
            >
              <Icon name={safeMode ? "safe-on" : "safe-off"} />
              <span>{safeMode ? "Safe" : "Active"}</span>
            </button>

            <button className="icon-button" onClick={onOpenApprovals} aria-label="Approvals inbox">
              <Icon name="bell" />
              {approvalCount > 0 && <span className="badge">{approvalCount}</span>}
            </button>

            <button className="icon-button" onClick={onOpenSettings} aria-label="Settings">
              <Icon name="settings" />
            </button>
          </div>
        </header>

        {showSafeModeBanner && (
          <div className="banner warning">
            Safe Mode is OFF. Sending, writing, and execution actions can run with fewer gates.
          </div>
        )}

        <div className="workspace">
          <main className="main-content">{children}</main>

          {!drawerAsModal && (
            <aside className="inspector" role="complementary" aria-label="Inspector">
              {inspector}
            </aside>
          )}
        </div>
      </div>
    </div>
  );
}
