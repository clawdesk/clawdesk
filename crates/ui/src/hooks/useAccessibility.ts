/**
 * Accessibility hooks — keyboard shortcuts, focus trapping, ARIA live.
 *
 * Three composable primitives covering the WCAG 2.1 AA requirements
 * that a desktop AI chat app must satisfy:
 *
 * - `useKeyboardShortcuts()` — declarative global hotkey registration
 *   with automatic `mod` key mapping (Cmd on macOS, Ctrl elsewhere).
 *   Skips firing inside text inputs (except Escape) to avoid conflicts.
 *
 * - `useFocusTrap()` — traps Tab/Shift+Tab inside a container element
 *   (modals, dialogs, command palette). Restores previous focus on
 *   unmount for seamless keyboard workflow.
 *
 * - `useAriaLive()` — programmatic screen reader announcements via
 *   a visually-hidden `aria-live` region. Supports both `polite`
 *   (queue behind current speech) and `assertive` (interrupt) modes.
 */

import { useCallback, useEffect, useRef } from "react";

// ── Keyboard Shortcuts ─────────────────────────────

export interface KeyboardShortcut {
  /** Key combination (e.g., "mod+k", "escape", "mod+shift+p") */
  key: string;
  /** Handler function */
  handler: (e: KeyboardEvent) => void;
  /** Whether the shortcut is active */
  enabled?: boolean;
  /** Description for help overlay */
  description?: string;
}

/**
 * Global keyboard shortcut manager.
 *
 * `mod` maps to Cmd on macOS, Ctrl on Windows/Linux.
 *
 * ```tsx
 * useKeyboardShortcuts([
 *   { key: "mod+k", handler: openCommandPalette, description: "Command palette" },
 *   { key: "mod+n", handler: newChat, description: "New chat" },
 *   { key: "escape", handler: closePanel },
 * ]);
 * ```
 */
export function useKeyboardShortcuts(shortcuts: KeyboardShortcut[]): void {
  useEffect(() => {
    const isMac = navigator.platform.toUpperCase().indexOf("MAC") >= 0;

    function handler(e: KeyboardEvent) {
      for (const shortcut of shortcuts) {
        if (shortcut.enabled === false) continue;

        const parts = shortcut.key.toLowerCase().split("+");
        const key = parts[parts.length - 1];
        const needsMod = parts.includes("mod");
        const needsShift = parts.includes("shift");
        const needsAlt = parts.includes("alt");

        const modKey = isMac ? e.metaKey : e.ctrlKey;
        const keyMatch =
          e.key.toLowerCase() === key || e.code.toLowerCase() === `key${key}`;

        if (
          keyMatch &&
          (!needsMod || modKey) &&
          (!needsShift || e.shiftKey) &&
          (!needsAlt || e.altKey)
        ) {
          // Don't fire shortcuts when typing in inputs (except Escape)
          const target = e.target as HTMLElement;
          const isInput =
            target.tagName === "INPUT" ||
            target.tagName === "TEXTAREA" ||
            target.isContentEditable;

          if (isInput && key !== "escape") continue;

          e.preventDefault();
          shortcut.handler(e);
          return;
        }
      }
    }

    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [shortcuts]);
}

// ── Focus Trap ─────────────────────────────────────

/**
 * Trap focus inside a container element (for modals, dialogs).
 *
 * When active, Tab/Shift+Tab cycles through focusable elements
 * within the container. Escape calls `onEscape` if provided.
 *
 * ```tsx
 * const trapRef = useFocusTrap({ active: isModalOpen, onEscape: closeModal });
 * return <div ref={trapRef}>...</div>;
 * ```
 */
export function useFocusTrap(options: {
  active: boolean;
  onEscape?: () => void;
}): React.RefObject<HTMLDivElement | null> {
  const ref = useRef<HTMLDivElement | null>(null);
  const previousFocusRef = useRef<HTMLElement | null>(null);

  useEffect(() => {
    if (!options.active || !ref.current) return;

    // Save current focus to restore later
    previousFocusRef.current = document.activeElement as HTMLElement;

    const container = ref.current;
    const focusableSelector =
      'a[href], button:not([disabled]), textarea:not([disabled]), input:not([disabled]), select:not([disabled]), [tabindex]:not([tabindex="-1"])';

    function handleKeyDown(e: KeyboardEvent) {
      if (e.key === "Escape" && options.onEscape) {
        e.preventDefault();
        options.onEscape();
        return;
      }

      if (e.key !== "Tab") return;

      const focusable = container.querySelectorAll(focusableSelector);
      if (focusable.length === 0) return;

      const first = focusable[0] as HTMLElement;
      const last = focusable[focusable.length - 1] as HTMLElement;

      if (e.shiftKey) {
        if (document.activeElement === first) {
          e.preventDefault();
          last.focus();
        }
      } else {
        if (document.activeElement === last) {
          e.preventDefault();
          first.focus();
        }
      }
    }

    // Focus the first focusable element
    const firstFocusable = container.querySelector(
      focusableSelector
    ) as HTMLElement;
    if (firstFocusable) {
      requestAnimationFrame(() => firstFocusable.focus());
    }

    document.addEventListener("keydown", handleKeyDown);
    return () => {
      document.removeEventListener("keydown", handleKeyDown);
      // Restore focus
      if (previousFocusRef.current) {
        previousFocusRef.current.focus();
      }
    };
  }, [options.active, options.onEscape]);

  return ref;
}

// ── ARIA Live Region ───────────────────────────────

/**
 * Announce messages to screen readers via a live region.
 *
 * ```tsx
 * const announce = useAriaLive();
 * announce("Message sent"); // polite
 * announce("Error: connection lost", "assertive"); // urgent
 * ```
 */
export function useAriaLive(): (
  message: string,
  priority?: "polite" | "assertive"
) => void {
  const regionRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    // Create a visually hidden live region if it doesn't exist
    let region = document.getElementById(
      "clawdesk-aria-live"
    ) as HTMLDivElement;
    if (!region) {
      region = document.createElement("div");
      region.id = "clawdesk-aria-live";
      region.setAttribute("aria-live", "polite");
      region.setAttribute("aria-atomic", "true");
      region.setAttribute("role", "status");
      Object.assign(region.style, {
        position: "absolute",
        width: "1px",
        height: "1px",
        padding: "0",
        margin: "-1px",
        overflow: "hidden",
        clip: "rect(0, 0, 0, 0)",
        whiteSpace: "nowrap",
        border: "0",
      });
      document.body.appendChild(region);
    }
    regionRef.current = region;

    return () => {
      // Don't remove — other components might be using it
    };
  }, []);

  return useCallback(
    (message: string, priority: "polite" | "assertive" = "polite") => {
      if (!regionRef.current) return;
      regionRef.current.setAttribute("aria-live", priority);
      // Clear then set to ensure the announcement fires
      regionRef.current.textContent = "";
      requestAnimationFrame(() => {
        if (regionRef.current) {
          regionRef.current.textContent = message;
        }
      });
    },
    []
  );
}
