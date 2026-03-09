# ADR-004: Single-Binary Tauri Desktop Application

## Status
Accepted

## Context
ClawDesk targets three deployment modes: desktop app, CLI, and TUI. The desktop app needs native OS integration (system tray, notifications, deep links, auto-update) while maintaining web-based UI flexibility.

## Decision
Use Tauri 2.0 for the desktop application. Compile the Rust backend + React frontend into a single binary. The CLI and TUI are separate leaf binaries sharing the same core crates.

## Consequences

### Positive
- Single binary deployment — no runtime dependencies
- Native OS integration via Tauri plugins (tray, shell, dialog, autostart)
- React frontend enables rapid UI iteration
- ~10MB binary size vs ~100MB+ for Electron
- Shared Rust core between desktop, CLI, and TUI

### Negative
- WebView rendering has platform-specific quirks (WebKit on macOS/Linux, WebView2 on Windows)
- IPC serialization overhead between Rust backend and JS frontend
- Two build systems (cargo + npm/vite) increase CI complexity

### Neutral
- Tauri 2.0 is stable but evolving — plugin ecosystem still growing

## Alternatives Considered

**Electron:** Larger binary, higher memory usage, but better cross-platform WebView consistency. Rejected for resource efficiency.

**Native GUI (iced/egui):** Pure Rust, no IPC overhead, but slower UI iteration and harder to find frontend contributors. Rejected for ecosystem reasons.

**Terminal-only (TUI):** Already built as `clawdesk-tui`, but desktop users expect native UX patterns (menus, drag-and-drop, system tray).
