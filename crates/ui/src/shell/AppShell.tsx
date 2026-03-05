import { useCallback, useState, type CSSProperties } from "react";
import { Icon } from "../components/Icon";
import type { DesktopAgent } from "../types";

export type ShellNavKey = "chat" | "overview" | "a2a" | "runtime" | "skills" | "automations" | "agents" | "channels" | "settings" | "logs" | "extensions" | "mcp";

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
  const selectedAgent = agents?.find((a) => a.id === selectedAgentId);

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
            {/* Agent selector pill */}
            {agents && agents.length > 0 && onSelectAgent && (
              <div className="top-agent-selector" data-no-drag>
                <button
                  className="top-agent-pill"
                  onClick={() => setShowAgentDropdown(!showAgentDropdown)}
                  title={selectedAgent ? selectedAgent.name : "Select agent"}
                >
                  <span className="top-agent-icon">{selectedAgent?.icon || "⚡"}</span>
                  <span className="top-agent-name">
                    {selectedAgent?.name || "Select agent"}
                  </span>
                  <Icon name="chevron-down" />
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
                        // Group agents: teams vs solo
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
                            {/* Solo agents */}
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

                            {/* Teams */}
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

                            {/* Create new */}
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
            )}
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
