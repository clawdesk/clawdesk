import { useCallback, useState, type CSSProperties } from "react";
import { Icon } from "../components/Icon";

export type ShellNavKey = "chat" | "overview" | "a2a" | "runtime" | "skills" | "automations" | "settings" | "logs";

interface ShellNavItem {
  key: ShellNavKey;
  label: string;
  shortcut: string;
  icon: string;
}

export interface ShellNavGroup {
  label: string;
  items: ShellNavItem[];
}

interface ExtendedCSSProperties extends CSSProperties {
  WebkitAppRegion?: "drag" | "no-drag";
}

export function AppShell({
  sidebarCollapsed,
  compactSidebar,
  activeNav,
  navGroups,
  navItems,
  onNavigate,
  onToggleSidebar,
  onOpenPalette,
  onOpenSettings,
  onToggleTerminal,
  children,
  inspector,
  drawerAsModal,
  onOpenInspectorModal,
  inspectorOpen,
  onToggleInspector,
}: {
  sidebarCollapsed: boolean;
  compactSidebar: boolean;
  activeNav: ShellNavKey;
  navGroups: ShellNavGroup[];
  navItems: ShellNavItem[];
  onNavigate: (nav: ShellNavKey) => void;
  onToggleSidebar: () => void;
  onOpenPalette: () => void;
  onOpenSettings: () => void;
  onToggleTerminal: () => void;
  children: React.ReactNode;
  inspector: React.ReactNode;
  drawerAsModal: boolean;
  onOpenInspectorModal: () => void;
  inspectorOpen: boolean;
  onToggleInspector: () => void;
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
          {navGroups.map((group) => (
            <div key={group.label} className="nav-group">
              {!sidebarCollapsed && (
                <div className="nav-group-label">
                  <span className="nav-group-label__text">{group.label}</span>
                </div>
              )}
              <div className="nav-group__items">
                {group.items.map((item) => (
                  <button
                    key={item.key}
                    className={`nav-item ${activeNav === item.key ? "active" : ""}`}
                    onClick={() => onNavigate(item.key)}
                    title={`${item.label} (${isMac ? "⌘" : "Ctrl+"}${item.shortcut})`}
                    data-nav-label={item.label}
                    aria-current={activeNav === item.key ? "page" : undefined}
                  >
                    <span className="nav-icon-wrap">
                      <Icon name={item.icon} />
                    </span>
                    {!sidebarCollapsed && (
                      <span className="nav-label">
                        {item.label}
                        <kbd className="nav-shortcut">{isMac ? "⌘" : "^"}{item.shortcut}</kbd>
                      </span>
                    )}
                  </button>
                ))}
              </div>
            </div>
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
            <button className="icon-button" onClick={onToggleTerminal} aria-label="Terminal" title="Toggle Terminal (⌘J)">
              <Icon name="terminal" />
            </button>
            <button className="icon-button" onClick={onOpenSettings} aria-label="Settings">
              <Icon name="settings" />
            </button>
          </div>
        </header>

        <div className={`workspace ${!inspectorOpen ? "inspector-closed" : ""}`}>
          <main className={`main-content ${activeNav === "chat" ? "chat-mode" : ""}`}>
            {children}
          </main>

          {!drawerAsModal && inspectorOpen && (
            <aside className="inspector" role="complementary" aria-label="Inspector">
              {inspector}
            </aside>
          )}
        </div>
      </div>
    </div>
  );
}
