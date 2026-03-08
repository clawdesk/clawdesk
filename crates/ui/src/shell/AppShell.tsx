import { useCallback, useState, type CSSProperties } from "react";
import { Icon } from "../components/Icon";
import type { DesktopAgent } from "../types";

export type ShellNavKey = "chat" | "overview" | "a2a" | "runtime" | "skills" | "automations" | "agents" | "channels" | "files" | "settings" | "logs" | "extensions" | "mcp" | "local-models" | "documents";

export type GatewayStatus = "connected" | "degraded" | "offline";

export interface TopBarStatus {
  gateway: GatewayStatus;
  agentCount: number;
  lastChecked?: string;
  pendingApprovals?: number;
}

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
  agents,
  selectedAgentId,
  onSelectAgent,
  onOpenJourney,
  status,
  browserEnabled,
  onToggleBrowser,
  safeMode,
  onToggleSafeMode,
  notificationCount,
  onOpenNotifications,
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
  /** Agent list for top-right selector */
  agents?: DesktopAgent[];
  selectedAgentId?: string | null;
  onSelectAgent?: (id: string | null) => void;
  onOpenJourney?: (agentId?: string) => void;
  /** Top bar status & quick toggles */
  status?: TopBarStatus;
  browserEnabled?: boolean;
  onToggleBrowser?: () => void;
  safeMode?: boolean;
  onToggleSafeMode?: () => void;
  notificationCount?: number;
  onOpenNotifications?: () => void;
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

  const [showAgentDropdown, setShowAgentDropdown] = useState(false);
  const [showStatusPopover, setShowStatusPopover] = useState(false);
  const selectedAgent = agents?.find((a) => a.id === selectedAgentId);

  // Status pulse animation class
  const statusClass = status?.gateway === "connected" ? "status-ok"
    : status?.gateway === "degraded" ? "status-warn"
    : "status-error";
  const statusLabel = status?.gateway === "connected" ? "Connected"
    : status?.gateway === "degraded" ? "Degraded"
    : "Offline";

  return (
    <div className={`app-shell ${isMac ? "mac-shell" : ""} ${sidebarCollapsed ? "sidebar-collapsed" : ""}`}>
      <aside className={`sidebar ${sidebarCollapsed ? "compact" : ""}`}>
        <div className="brand">
          <div className="brand-icon-group">
            <span className="brand-mark">
              <img src="/logo.svg" alt="ClawDesk logo" className="brand-logo" />
            </span>
            <span className="brand-alpha-tag">alpha</span>
          </div>
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
          {/* ── LEFT: Status + Agent ── */}
          <div className="tb-left" style={noDragStyle}>
            <div className="tb-status-wrap">
              <button
                className="tb-icon-btn"
                onClick={() => setShowStatusPopover(!showStatusPopover)}
                title={`Status: ${statusLabel}`}
                aria-label="Status"
              >
                <span className={`tb-dot ${statusClass}`} />
              </button>
              {showStatusPopover && (
                <>
                  <div className="tb-popover-backdrop" onClick={() => setShowStatusPopover(false)} />
                  <div className="tb-status-popover">
                    <div className="tb-pop-row">
                      <span className={`tb-dot ${statusClass}`} />
                      <span className="tb-pop-label">Gateway</span>
                      <span className="tb-pop-value">{statusLabel}</span>
                    </div>
                    {status && (
                      <div className="tb-pop-row">
                        <Icon name="users" />
                        <span className="tb-pop-label">Agents</span>
                        <span className="tb-pop-value">{status.agentCount}</span>
                      </div>
                    )}
                    {status?.lastChecked && (
                      <div className="tb-pop-row">
                        <Icon name="clock" />
                        <span className="tb-pop-label">Uptime</span>
                        <span className="tb-pop-value">{status.lastChecked}</span>
                      </div>
                    )}
                    {(status?.pendingApprovals ?? 0) > 0 && (
                      <div className="tb-pop-row tb-pop-warn">
                        <Icon name="alert" />
                        <span className="tb-pop-label">Approvals</span>
                        <span className="tb-pop-value">{status!.pendingApprovals} pending</span>
                      </div>
                    )}
                  </div>
                </>
              )}
            </div>

            {agents && agents.length > 0 && onSelectAgent ? (
              <div className="top-agent-selector" data-no-drag>
                <button
                  className="tb-agent-picker"
                  onClick={() => setShowAgentDropdown(!showAgentDropdown)}
                  title={selectedAgent ? selectedAgent.name : "Select agent"}
                >
                  <span className="tb-agent-avatar">{selectedAgent?.icon || "⚡"}</span>
                  <span className="tb-agent-name">{selectedAgent?.name || "Select Agent"}</span>
                  <Icon name="chevron-down" className="tb-agent-chevron" />
                </button>
                {showAgentDropdown && (
                  <>
                    <div
                      className="top-agent-backdrop"
                      onClick={(e) => { e.stopPropagation(); setShowAgentDropdown(false); }}
                    />
                    <div
                      className="top-agent-dropdown"
                      onClick={(e) => e.stopPropagation()}
                    >
                      {(() => {
                        const soloAgents = agents.filter((a) => !a.team_id);
                        const teamMap = new Map<string, typeof agents>();
                        for (const a of agents) {
                          if (a.team_id) {
                            const list = teamMap.get(a.team_id) || [];
                            list.push(a);
                            teamMap.set(a.team_id, list);
                          }
                        }

                        return (
                          <>
                            {soloAgents.map((a) => (
                              <button
                                key={a.id}
                                className={`top-agent-option ${selectedAgentId === a.id ? "active" : ""}`}
                                onClick={(e) => {
                                  e.stopPropagation();
                                  onSelectAgent(a.id);
                                  setShowAgentDropdown(false);
                                }}
                              >
                                <span className="top-agent-option-icon">{a.icon}</span>
                                <div className="top-agent-option-info">
                                  <span className="top-agent-option-name">{a.name}</span>
                                  <span className="top-agent-option-model">{a.model === "default" ? "Auto" : a.model}</span>
                                </div>
                                {selectedAgentId === a.id && <span className="top-agent-active-dot" />}
                              </button>
                            ))}

                            {[...teamMap.entries()].map(([teamId, teamAgents]) => {
                              const router = teamAgents.find((a) => a.team_role === "router") || teamAgents[0];
                              const isTeamSelected = teamAgents.some((a) => a.id === selectedAgentId);
                              return (
                                <div key={teamId} className="top-agent-team-group">
                                  <button
                                    className={`top-agent-option top-agent-team-header ${isTeamSelected ? "active" : ""}`}
                                    onClick={(e) => {
                                      e.stopPropagation();
                                      onSelectAgent(router.id);
                                      setShowAgentDropdown(false);
                                    }}
                                  >
                                    <span className="top-agent-option-icon">👥</span>
                                    <div className="top-agent-option-info">
                                      <span className="top-agent-option-name">Team: {router.name}</span>
                                      <span className="top-agent-option-model">{teamAgents.length} agents · routes to team</span>
                                    </div>
                                    {isTeamSelected && <span className="top-agent-active-dot" />}
                                  </button>
                                  <div className="top-agent-team-members">
                                    {teamAgents.map((a) => (
                                      <button
                                        key={a.id}
                                        className={`top-agent-option top-agent-team-member ${selectedAgentId === a.id ? "active" : ""}`}
                                        onClick={(e) => {
                                          e.stopPropagation();
                                          onSelectAgent(a.id);
                                          setShowAgentDropdown(false);
                                        }}
                                      >
                                        <span className="top-agent-option-icon">{a.icon}</span>
                                        <div className="top-agent-option-info">
                                          <span className="top-agent-option-name">{a.name}</span>
                                          <span className="top-agent-option-model">{a.team_role || "member"}</span>
                                        </div>
                                        {selectedAgentId === a.id && <span className="top-agent-active-dot" />}
                                      </button>
                                    ))}
                                  </div>
                                </div>
                              );
                            })}

                            {onOpenJourney && (
                              <button
                                className="top-agent-option top-agent-new"
                                onClick={(e) => { e.stopPropagation(); setShowAgentDropdown(false); onOpenJourney(); }}
                              >
                                <span className="top-agent-option-icon">✨</span>
                                <div className="top-agent-option-info">
                                  <span className="top-agent-option-name">Create Agent</span>
                                  <span className="top-agent-option-model">Single agent or team</span>
                                </div>
                              </button>
                            )}
                          </>
                        );
                      })()}
                    </div>
                  </>
                )}
              </div>
            ) : (
              <span className="tb-agent-placeholder">No agents</span>
            )}
          </div>

          <div className="tb-center" aria-hidden="true" />

          {/* ── RIGHT: Toggles + Search + Actions ── */}
          <div className="tb-right" style={noDragStyle}>
            {/* Toggle group: Browser & Safe mode */}
            <div className="tb-toggle-group">
              <button
                className={`tb-pill ${browserEnabled ? "tb-pill-on" : ""}`}
                onClick={onToggleBrowser}
                title={`Browser: ${browserEnabled ? "On" : "Off"}`}
                aria-pressed={browserEnabled}
              >
                <Icon name="globe" />
                <span className="tb-pill-text">Browser</span>
                <span className={`tb-pill-dot ${browserEnabled ? "on" : ""}`} />
              </button>

              <button
                className={`tb-pill ${safeMode ? "tb-pill-on" : ""}`}
                onClick={onToggleSafeMode}
                title={`Safe mode: ${safeMode ? "On" : "Off"}`}
                aria-pressed={safeMode}
              >
                <Icon name={safeMode ? "safe-on" : "safe-off"} />
                <span className="tb-pill-text">Safe</span>
                <span className={`tb-pill-dot ${safeMode ? "on" : ""}`} />
              </button>
            </div>

            <span className="tb-sep" />

            <button className="tb-search-btn" onClick={onOpenPalette} title="Search (⌘K)" aria-label="Search">
              <Icon name="search" />
              <span className="tb-search-text">Search</span>
              <kbd className="tb-kbd">{isMac ? "⌘" : "^"}K</kbd>
            </button>

            <span className="tb-sep" />

            <div className="tb-action-group">
              <button
                className="tb-action-btn"
                onClick={onOpenNotifications}
                aria-label="Notifications"
                title="Notifications"
              >
                <span className="tb-action-icon-wrap">
                  <Icon name="bell" />
                  {(notificationCount ?? 0) > 0 && (
                    <span className="tb-notif-badge">{notificationCount}</span>
                  )}
                </span>
              </button>

              <button className="tb-action-btn" onClick={onToggleTerminal} title="Terminal (⌘J)" aria-label="Terminal">
                <Icon name="terminal" />
              </button>

              <button className="tb-action-btn" onClick={onOpenSettings} title="Settings" aria-label="Settings">
                <Icon name="settings" />
              </button>
            </div>
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
