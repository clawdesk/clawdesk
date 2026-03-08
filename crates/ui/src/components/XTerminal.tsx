/**
 * XTerminal — Real terminal emulator panel for ClawDesk.
 *
 * Uses xterm.js backed by a real PTY session via Tauri commands.
 * Inspired by open-terminal's WebSocket terminal architecture,
 * adapted for Tauri's IPC + event system.
 *
 * Architecture:
 * - On mount: creates a PTY session via `pty_create_session` Tauri command
 * - Output: listens to `terminal-output` Tauri events, writes to xterm.js
 * - Input: xterm.js onData → `pty_write_input` Tauri command
 * - Resize: xterm.js + FitAddon → `pty_resize` Tauri command
 * - On unmount: kills the PTY session
 */

import { useEffect, useRef, useState, useCallback } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Icon } from "./Icon";
import "@xterm/xterm/css/xterm.css";

interface TerminalSession {
  id: string;
  pid: number;
  shell: string;
  cols: number;
  rows: number;
  created_at: string;
}

interface TerminalOutputPayload {
  session_id: string;
  data: string;
}

interface XTerminalProps {
  visible: boolean;
  onClose: () => void;
}

// Check if running inside Tauri
const isTauri = typeof window !== "undefined" && !!(window as any).__TAURI_INTERNALS__;

export function XTerminal({ visible, onClose }: XTerminalProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const sessionRef = useRef<string | null>(null);
  const unlistenRef = useRef<(() => void) | null>(null);
  const [connected, setConnected] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Create terminal + session
  const initTerminal = useCallback(async () => {
    if (!containerRef.current || termRef.current) return;

    const term = new Terminal({
      cursorBlink: true,
      fontSize: 13,
      fontFamily: "'SF Mono', 'Fira Code', 'Cascadia Code', Menlo, monospace",
      theme: {
        background: "#1a1b26",
        foreground: "#c0caf5",
        cursor: "#c0caf5",
        cursorAccent: "#1a1b26",
        selectionBackground: "#33467c",
        selectionForeground: "#c0caf5",
        black: "#15161e",
        red: "#f7768e",
        green: "#9ece6a",
        yellow: "#e0af68",
        blue: "#7aa2f7",
        magenta: "#bb9af7",
        cyan: "#7dcfff",
        white: "#a9b1d6",
        brightBlack: "#414868",
        brightRed: "#f7768e",
        brightGreen: "#9ece6a",
        brightYellow: "#e0af68",
        brightBlue: "#7aa2f7",
        brightMagenta: "#bb9af7",
        brightCyan: "#7dcfff",
        brightWhite: "#c0caf5",
      },
      scrollback: 5000,
      allowProposedApi: true,
    });

    const fitAddon = new FitAddon();
    const webLinksAddon = new WebLinksAddon();
    term.loadAddon(fitAddon);
    term.loadAddon(webLinksAddon);

    term.open(containerRef.current);
    fitAddon.fit();

    termRef.current = term;
    fitRef.current = fitAddon;

    if (!isTauri) {
      term.writeln("\x1b[33m⚠ Terminal requires the Tauri desktop runtime.\x1b[0m");
      term.writeln("\x1b[90mRun with: cargo tauri dev\x1b[0m");
      return;
    }

    // Create PTY session
    try {
      const dims = fitAddon.proposeDimensions();
      const session = await invoke<TerminalSession>("pty_create_session", {
        request: {
          cols: dims?.cols ?? 80,
          rows: dims?.rows ?? 24,
        },
      });
      sessionRef.current = session.id;
      setConnected(true);

      // Listen for PTY output
      const unlisten = await listen<TerminalOutputPayload>("terminal-output", (event) => {
        const payload = event.payload;
        if (payload.session_id === sessionRef.current) {
          if (payload.data === "") {
            // EOF — session ended
            term.writeln("\r\n\x1b[90m[Session ended]\x1b[0m");
            setConnected(false);
          } else {
            term.write(payload.data);
          }
        }
      });
      unlistenRef.current = unlisten;

      // Send keystrokes to PTY
      term.onData((data) => {
        if (sessionRef.current) {
          invoke("pty_write_input", {
            request: {
              session_id: sessionRef.current,
              data,
            },
          }).catch(() => {});
        }
      });

      // Handle resize
      term.onResize(({ cols, rows }) => {
        if (sessionRef.current) {
          invoke("pty_resize", {
            request: {
              session_id: sessionRef.current,
              cols,
              rows,
            },
          }).catch(() => {});
        }
      });
    } catch (err: any) {
      const msg = err?.message || err?.toString() || "Failed to create terminal session";
      setError(msg);
      term.writeln(`\x1b[31m✖ ${msg}\x1b[0m`);
    }
  }, []);

  // Initialize when visible
  useEffect(() => {
    if (visible) {
      // Small delay to ensure container is in DOM
      const timer = setTimeout(initTerminal, 50);
      return () => clearTimeout(timer);
    }
  }, [visible, initTerminal]);

  // Handle resize when visibility changes
  useEffect(() => {
    if (visible && fitRef.current && termRef.current) {
      const timer = setTimeout(() => {
        fitRef.current?.fit();
        termRef.current?.focus();
      }, 100);
      return () => clearTimeout(timer);
    }
  }, [visible]);

  // ResizeObserver for container size changes
  useEffect(() => {
    if (!containerRef.current || !fitRef.current) return;
    const ro = new ResizeObserver(() => {
      fitRef.current?.fit();
    });
    ro.observe(containerRef.current);
    return () => ro.disconnect();
  }, [connected]);

  // Cleanup on unmount
  useEffect(() => {
    return () => {
      if (unlistenRef.current) {
        unlistenRef.current();
        unlistenRef.current = null;
      }
      if (sessionRef.current && isTauri) {
        invoke("pty_kill_session", { sessionId: sessionRef.current }).catch(() => {});
        sessionRef.current = null;
      }
      if (termRef.current) {
        termRef.current.dispose();
        termRef.current = null;
      }
      fitRef.current = null;
      setConnected(false);
    };
  }, []);

  // Restart session
  const handleRestart = useCallback(async () => {
    // Kill old session
    if (sessionRef.current && isTauri) {
      await invoke("pty_kill_session", { sessionId: sessionRef.current }).catch(() => {});
      sessionRef.current = null;
    }
    if (unlistenRef.current) {
      unlistenRef.current();
      unlistenRef.current = null;
    }

    setConnected(false);
    setError(null);

    // Clear terminal and reinitialize
    if (termRef.current) {
      termRef.current.clear();
    }

    if (!isTauri) return;

    try {
      const dims = fitRef.current?.proposeDimensions();
      const session = await invoke<TerminalSession>("pty_create_session", {
        request: {
          cols: dims?.cols ?? 80,
          rows: dims?.rows ?? 24,
        },
      });
      sessionRef.current = session.id;
      setConnected(true);

      const unlisten = await listen<TerminalOutputPayload>("terminal-output", (event) => {
        const payload = event.payload;
        if (payload.session_id === sessionRef.current) {
          if (payload.data === "") {
            termRef.current?.writeln("\r\n\x1b[90m[Session ended]\x1b[0m");
            setConnected(false);
          } else {
            termRef.current?.write(payload.data);
          }
        }
      });
      unlistenRef.current = unlisten;
    } catch (err: any) {
      const msg = err?.message || err?.toString() || "Failed to restart session";
      setError(msg);
      termRef.current?.writeln(`\x1b[31m✖ ${msg}\x1b[0m`);
    }
  }, []);

  if (!visible) return null;

  return (
    <div className="xterminal-panel">
      <div className="xterminal-header">
        <div className="xterminal-header-left">
          <Icon name="terminal" />
          <span className="xterminal-title">Terminal</span>
          {connected && <span className="xterminal-badge connected">●</span>}
          {!connected && !error && <span className="xterminal-badge idle">●</span>}
          {error && <span className="xterminal-badge error">●</span>}
        </div>
        <div className="xterminal-header-actions">
          <button className="xterminal-action-btn" onClick={handleRestart} title="New session">
            <Icon name="refresh" />
          </button>
          <button className="xterminal-action-btn" onClick={onClose} title="Close (⌘J)">
            <Icon name="close" />
          </button>
        </div>
      </div>
      <div className="xterminal-body" ref={containerRef} />
    </div>
  );
}
