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

type TerminalPhase = "idle" | "connecting" | "ready" | "ended" | "error" | "unsupported";

type Disposable = {
  dispose: () => void;
};

// Check if running inside Tauri
const isTauri = typeof window !== "undefined" && !!(window as any).__TAURI_INTERNALS__;

export function XTerminal({ visible, onClose }: XTerminalProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const sessionRef = useRef<string | null>(null);
  const unlistenRef = useRef<(() => void) | null>(null);
  const inputDisposableRef = useRef<Disposable | null>(null);
  const resizeDisposableRef = useRef<Disposable | null>(null);
  const mountedRef = useRef(true);
  const [phase, setPhase] = useState<TerminalPhase>("idle");
  const [error, setError] = useState<string | null>(null);
  const [session, setSession] = useState<TerminalSession | null>(null);

  const setPhaseSafe = useCallback((nextPhase: TerminalPhase) => {
    if (mountedRef.current) {
      setPhase(nextPhase);
    }
  }, []);

  const setErrorSafe = useCallback((nextError: string | null) => {
    if (mountedRef.current) {
      setError(nextError);
    }
  }, []);

  const setSessionSafe = useCallback((nextSession: TerminalSession | null) => {
    if (mountedRef.current) {
      setSession(nextSession);
    }
  }, []);

  const clearBindings = useCallback(() => {
    inputDisposableRef.current?.dispose();
    inputDisposableRef.current = null;
    resizeDisposableRef.current?.dispose();
    resizeDisposableRef.current = null;
    if (unlistenRef.current) {
      unlistenRef.current();
      unlistenRef.current = null;
    }
  }, []);

  const disposeTerminal = useCallback(() => {
    clearBindings();
    if (termRef.current) {
      termRef.current.dispose();
      termRef.current = null;
    }
    fitRef.current = null;
  }, [clearBindings]);

  const killSession = useCallback(async () => {
    const sessionId = sessionRef.current;
    sessionRef.current = null;
    if (!sessionId || !isTauri) {
      return;
    }
    await invoke("pty_kill_session", { sessionId }).catch(() => {});
  }, []);

  const teardownTerminal = useCallback(async (nextPhase: TerminalPhase = "idle") => {
    await killSession();
    disposeTerminal();
    setSessionSafe(null);
    setErrorSafe(null);
    setPhaseSafe(nextPhase);
  }, [disposeTerminal, killSession, setErrorSafe, setPhaseSafe, setSessionSafe]);

  const bindTerminalIO = useCallback((term: Terminal) => {
    inputDisposableRef.current = term.onData((data) => {
      if (sessionRef.current) {
        invoke("pty_write_input", {
          request: {
            session_id: sessionRef.current,
            data,
          },
        }).catch(() => {});
      }
    });

    resizeDisposableRef.current = term.onResize(({ cols, rows }) => {
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
  }, []);

  const startSession = useCallback(async (term: Terminal, fitAddon: FitAddon) => {
    setErrorSafe(null);

    if (!isTauri) {
      setPhaseSafe("unsupported");
      term.writeln("\x1b[33mThis terminal only works in the desktop app.\x1b[0m");
      term.writeln("\x1b[90mOpen ClawDesk with Tauri to run a real shell here.\x1b[0m");
      return;
    }

    setPhaseSafe("connecting");

    try {
      const dims = fitAddon.proposeDimensions();
      const nextSession = await invoke<TerminalSession>("pty_create_session", {
        request: {
          cols: dims?.cols ?? 80,
          rows: dims?.rows ?? 24,
        },
      });

      sessionRef.current = nextSession.id;
      setSessionSafe(nextSession);
      setPhaseSafe("ready");

      const unlisten = await listen<TerminalOutputPayload>("terminal-output", (event) => {
        const payload = event.payload;
        if (payload.session_id !== sessionRef.current) {
          return;
        }

        if (payload.data === "") {
          term.writeln("\r\n\x1b[90m[The shell has stopped]\x1b[0m");
          sessionRef.current = null;
          setPhaseSafe("ended");
          return;
        }

        term.write(payload.data);
      });

      unlistenRef.current = unlisten;
      fitAddon.fit();
      term.focus();
    } catch (err: any) {
      const message = err?.message || err?.toString() || "Could not start the terminal";
      setErrorSafe(message);
      setSessionSafe(null);
      setPhaseSafe("error");
      term.writeln(`\x1b[31mUnable to start the terminal: ${message}\x1b[0m`);
      await killSession();
    }
  }, [killSession, setErrorSafe, setPhaseSafe, setSessionSafe]);

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

    bindTerminalIO(term);
    await startSession(term, fitAddon);
  }, [bindTerminalIO, startSession]);

  useEffect(() => {
    if (!visible) {
      void teardownTerminal("idle");
      return;
    }

    const timer = setTimeout(() => {
      void initTerminal();
    }, 50);

    return () => clearTimeout(timer);
  }, [initTerminal, teardownTerminal, visible]);

  useEffect(() => {
    if (visible && fitRef.current && termRef.current) {
      const timer = setTimeout(() => {
        fitRef.current?.fit();
        termRef.current?.focus();
      }, 100);
      return () => clearTimeout(timer);
    }
  }, [visible]);

  useEffect(() => {
    if (!visible || !containerRef.current || !fitRef.current) return;

    const ro = new ResizeObserver(() => {
      fitRef.current?.fit();
    });

    ro.observe(containerRef.current);
    return () => ro.disconnect();
  }, [phase, visible]);

  useEffect(() => {
    mountedRef.current = true;

    return () => {
      mountedRef.current = false;
      void killSession();
      disposeTerminal();
    };
  }, [disposeTerminal, killSession]);

  const handleRestart = useCallback(async () => {
    await teardownTerminal("idle");
    if (!visible) {
      return;
    }
    await initTerminal();
  }, [initTerminal, teardownTerminal, visible]);

  const handleClear = useCallback(() => {
    termRef.current?.clear();
    termRef.current?.focus();
  }, []);

  if (!visible) return null;

  const shellName = session?.shell.split("/").pop() ?? session?.shell ?? "Shell";
  const statusLabel =
    phase === "ready"
      ? "Ready"
      : phase === "connecting"
        ? "Starting shell"
        : phase === "ended"
          ? "Shell stopped"
          : phase === "unsupported"
            ? "Desktop app required"
            : phase === "error"
              ? "Needs attention"
              : "Waiting";
  const statusText =
    phase === "ready"
      ? "Type commands here. If it ever feels stuck, start a fresh terminal."
      : phase === "connecting"
        ? "ClawDesk is opening a shell for you."
        : phase === "ended"
          ? "The shell closed. Start a fresh terminal to continue."
          : phase === "unsupported"
            ? "The browser preview cannot open a real shell."
            : phase === "error"
              ? error ?? "ClawDesk could not start the shell."
              : "Open the terminal when you need to run commands directly.";
  const statusTone = phase === "error" ? "error" : phase === "ended" ? "warning" : phase;

  return (
    <div className="xterminal-panel">
      <div className="xterminal-header">
        <div className="xterminal-header-left">
          <Icon name="terminal" />
          <div className="xterminal-title-wrap">
            <span className="xterminal-title">Workspace Terminal</span>
            <span className={`xterminal-status ${statusTone}`}>{statusLabel}</span>
          </div>
          {session && (
            <>
              <span className="xterminal-meta-chip">{shellName}</span>
              <span className="xterminal-meta-chip">{session.cols}x{session.rows}</span>
            </>
          )}
        </div>
        <div className="xterminal-header-actions">
          <button className="xterminal-action-btn" onClick={handleClear} title="Clear output">
            <Icon name="trash" />
          </button>
          <button className="xterminal-action-btn" onClick={handleRestart} title="Start a fresh terminal">
            <Icon name="refresh" />
          </button>
          <button className="xterminal-action-btn" onClick={onClose} title="Close (⌘J)">
            <Icon name="close" />
          </button>
        </div>
      </div>
      <div className={`xterminal-statusbar ${statusTone}`}>
        <span>{statusText}</span>
        {(phase === "ended" || phase === "error") && (
          <button className="xterminal-inline-action" onClick={handleRestart}>
            Start fresh terminal
          </button>
        )}
      </div>
      <div className="xterminal-body-wrap">
        <div className="xterminal-body" ref={containerRef} />
      </div>
    </div>
  );
}
