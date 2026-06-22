import { useCallback, useEffect, useState } from "react";
import {
  applyTheme,
  readThemePreference,
  resolveEffectiveTheme,
  writeThemePreference,
  type ThemeChoice,
} from "./theme-preference";

interface UseThemeResult {
  /** The user's choice: "system" | "light" | "dark". */
  theme: ThemeChoice;
  /** Update the choice, persist it, and re-apply the effective theme. */
  setTheme: (next: ThemeChoice) => void;
}

/**
 * Owns the theme choice: applies it to the document + native window, persists
 * it, and — while on "system" — re-applies whenever the OS color scheme flips.
 * Mount once at the app root so the whole UI reflects the chosen theme.
 */
export function useTheme(): UseThemeResult {
  const [theme, setThemeState] = useState<ThemeChoice>(readThemePreference);

  // Apply on mount and whenever the choice changes.
  useEffect(() => {
    void applyTheme(theme, resolveEffectiveTheme(theme));
  }, [theme]);

  // While following the system, re-apply when the OS preference changes.
  useEffect(() => {
    if (theme !== "system") return;
    if (typeof window === "undefined" || !window.matchMedia) return;
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const onChange = (): void => {
      void applyTheme("system", resolveEffectiveTheme("system"));
    };
    media.addEventListener("change", onChange);
    return () => media.removeEventListener("change", onChange);
  }, [theme]);

  const setTheme = useCallback((next: ThemeChoice): void => {
    setThemeState(next);
    writeThemePreference(next);
  }, []);

  return { theme, setTheme };
}
