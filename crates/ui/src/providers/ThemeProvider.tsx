/**
 * Theme Provider — runtime-configurable design token injection.
 *
 * Implements a three-layer theme resolution pipeline:
 *
 *   UserConfig → SystemPreference → ResolvedTokens → DOM
 *
 * The provider is the single authority for visual mode (light/dark),
 * accent chromaticity, typographic scale, motion preference, and
 * density mode. All downstream components read resolved values via
 * the `useTheme()` hook — zero prop drilling, O(1) context lookup.
 *
 * ## Resolution semantics
 *
 *   mode ∈ {light, dark, system}
 *   resolved = mode = system ? matchMedia(prefers-color-scheme) : mode
 *
 * Font scale is clamped to [0.85, 1.15] to prevent layout breakage.
 * Accent color is injected as a CSS custom property (`--brand`) so the
 * entire palette shifts without a re-render.
 *
 * ## Persistence
 *
 * Serialized to `localStorage` under `clawdesk.theme`. On load, missing
 * keys are merged with defaults (forward-compatible schema evolution).
 */

import React, {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
} from "react";

// ── Types ──────────────────────────────────────────

export type ThemeMode = "light" | "dark" | "system";
export type ResolvedMode = "light" | "dark";

export interface ThemeConfig {
  mode: ThemeMode;
  accentColor: string;
  fontScale: number; // 0.85 – 1.15
  reducedMotion: boolean;
  compactMode: boolean;
}

export interface ThemeContextValue {
  /** Current theme configuration. */
  theme: ThemeConfig;
  /** Resolved mode (system resolved to actual light/dark). */
  actualMode: ResolvedMode;
  /** Update the theme mode. */
  setMode: (mode: ThemeMode) => void;
  /** Update the accent color. */
  setAccentColor: (color: string) => void;
  /** Update font scale. */
  setFontScale: (scale: number) => void;
  /** Toggle reduced motion. */
  setReducedMotion: (reduced: boolean) => void;
  /** Toggle compact mode. */
  setCompactMode: (compact: boolean) => void;
  /** CSS class name for the current theme (for portal/modal theming). */
  themeClassName: string;
}

// ── Constants ──────────────────────────────────────

const STORAGE_KEY = "clawdesk.theme";

const DEFAULT_THEME: ThemeConfig = {
  mode: "system",
  accentColor: "#e8612c", // ClawDesk brand orange
  fontScale: 1.0,
  reducedMotion: false,
  compactMode: false,
};

// ── Context ────────────────────────────────────────

const ThemeContext = createContext<ThemeContextValue | undefined>(undefined);

// ── Helpers ────────────────────────────────────────

function loadTheme(): ThemeConfig {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) {
      const parsed = JSON.parse(raw);
      return { ...DEFAULT_THEME, ...parsed };
    }
  } catch {
    /* ignore parse errors */
  }
  return DEFAULT_THEME;
}

function saveTheme(config: ThemeConfig): void {
  localStorage.setItem(STORAGE_KEY, JSON.stringify(config));
}

function resolveMode(mode: ThemeMode): ResolvedMode {
  if (mode === "system") {
    return window.matchMedia("(prefers-color-scheme: dark)").matches
      ? "dark"
      : "light";
  }
  return mode;
}

// ── Provider ───────────────────────────────────────

export function ThemeProvider({ children }: { children: React.ReactNode }) {
  const [theme, setTheme] = useState<ThemeConfig>(loadTheme);
  const [actualMode, setActualMode] = useState<ResolvedMode>(() =>
    resolveMode(theme.mode)
  );

  // Apply theme to DOM
  useEffect(() => {
    const root = document.documentElement;
    const resolved = resolveMode(theme.mode);
    setActualMode(resolved);

    // Set data attribute for CSS selectors
    root.setAttribute("data-theme", resolved);

    // Set CSS classes
    root.classList.remove("light", "dark");
    root.classList.add(resolved);

    // Set accent color CSS variable
    root.style.setProperty("--brand", theme.accentColor);

    // Set font scale
    root.style.setProperty("--font-scale", String(theme.fontScale));

    // Set reduced motion
    if (theme.reducedMotion) {
      root.setAttribute("data-reduced-motion", "true");
    } else {
      root.removeAttribute("data-reduced-motion");
    }

    // Set compact mode
    if (theme.compactMode) {
      root.setAttribute("data-compact", "true");
    } else {
      root.removeAttribute("data-compact");
    }

    // Persist
    saveTheme(theme);
  }, [theme]);

  // Listen for system theme changes
  useEffect(() => {
    if (theme.mode !== "system") return;

    const mediaQuery = window.matchMedia("(prefers-color-scheme: dark)");
    const handler = () => {
      setActualMode(mediaQuery.matches ? "dark" : "light");
      document.documentElement.setAttribute(
        "data-theme",
        mediaQuery.matches ? "dark" : "light"
      );
      document.documentElement.classList.remove("light", "dark");
      document.documentElement.classList.add(
        mediaQuery.matches ? "dark" : "light"
      );
    };

    mediaQuery.addEventListener("change", handler);
    return () => mediaQuery.removeEventListener("change", handler);
  }, [theme.mode]);

  // Memoized setters
  const setMode = useCallback(
    (mode: ThemeMode) => setTheme((prev) => ({ ...prev, mode })),
    []
  );
  const setAccentColor = useCallback(
    (accentColor: string) => setTheme((prev) => ({ ...prev, accentColor })),
    []
  );
  const setFontScale = useCallback(
    (fontScale: number) =>
      setTheme((prev) => ({
        ...prev,
        fontScale: Math.max(0.85, Math.min(1.15, fontScale)),
      })),
    []
  );
  const setReducedMotion = useCallback(
    (reducedMotion: boolean) =>
      setTheme((prev) => ({ ...prev, reducedMotion })),
    []
  );
  const setCompactMode = useCallback(
    (compactMode: boolean) => setTheme((prev) => ({ ...prev, compactMode })),
    []
  );

  const themeClassName = `theme-${actualMode}`;

  const value = useMemo<ThemeContextValue>(
    () => ({
      theme,
      actualMode,
      setMode,
      setAccentColor,
      setFontScale,
      setReducedMotion,
      setCompactMode,
      themeClassName,
    }),
    [
      theme,
      actualMode,
      setMode,
      setAccentColor,
      setFontScale,
      setReducedMotion,
      setCompactMode,
      themeClassName,
    ]
  );

  return (
    <ThemeContext.Provider value={value}>{children}</ThemeContext.Provider>
  );
}

// ── Hook ───────────────────────────────────────────

/**
 * Access the current theme and mode setters.
 *
 * Usage:
 * ```tsx
 * const { actualMode, setMode, theme } = useTheme();
 * ```
 */
export function useTheme(): ThemeContextValue {
  const context = useContext(ThemeContext);
  if (!context) {
    throw new Error("useTheme must be used within a ThemeProvider");
  }
  return context;
}
