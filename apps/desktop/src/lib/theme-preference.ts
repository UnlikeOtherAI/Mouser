/**
 * Theme preference persistence + application, mirroring the style of
 * `tray-preference.ts`.
 *
 * The persisted value is the user's *choice* ("system" | "light" | "dark").
 * "system" follows the OS via `prefers-color-scheme`. The effective theme is
 * applied by toggling a class on `<html>` (the `.theme-light` / `.theme-dark`
 * classes that drive the CSS-variable palette in `styles/global.css`) and by
 * asking the Tauri window to match, so native chrome (titlebar, scrollbars)
 * tracks the in-app theme. Falls back gracefully outside Tauri (dev in a plain
 * browser), where only the document class is applied.
 */

export type ThemeChoice = "system" | "light" | "dark";
export type EffectiveTheme = "light" | "dark";

const STORAGE_KEY = "mouser.theme";
const CHOICES: readonly ThemeChoice[] = ["system", "light", "dark"];

function isThemeChoice(value: string | null): value is ThemeChoice {
  return value !== null && (CHOICES as readonly string[]).includes(value);
}

/** Read the persisted theme choice; defaults to "system". */
export function readThemePreference(): ThemeChoice {
  if (typeof window === "undefined") return "system";
  const stored = window.localStorage.getItem(STORAGE_KEY);
  return isThemeChoice(stored) ? stored : "system";
}

/** Persist the theme choice. */
export function writeThemePreference(choice: ThemeChoice): void {
  if (typeof window === "undefined") return;
  window.localStorage.setItem(STORAGE_KEY, choice);
}

/** Whether the OS currently prefers a dark color scheme. */
export function systemPrefersDark(): boolean {
  if (typeof window === "undefined" || !window.matchMedia) return true;
  return window.matchMedia("(prefers-color-scheme: dark)").matches;
}

/** Resolve a choice to the concrete light/dark theme to render. */
export function resolveEffectiveTheme(choice: ThemeChoice): EffectiveTheme {
  if (choice === "system") return systemPrefersDark() ? "dark" : "light";
  return choice;
}

/**
 * Apply an effective theme to the document: toggle the `theme-light` /
 * `theme-dark` classes on `<html>` and set `color-scheme` so form controls and
 * scrollbars match. Also asks the Tauri window to adopt the theme; `system`
 * passes `null` so the OS decides the native chrome.
 */
export async function applyTheme(
  choice: ThemeChoice,
  effective: EffectiveTheme,
): Promise<void> {
  if (typeof document !== "undefined") {
    const root = document.documentElement;
    root.classList.toggle("theme-light", effective === "light");
    root.classList.toggle("theme-dark", effective === "dark");
    root.style.colorScheme = effective;
  }

  try {
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    // `system` -> null lets the OS drive the native window chrome.
    const windowTheme = choice === "system" ? null : effective;
    await getCurrentWindow().setTheme(windowTheme);
  } catch {
    // Browser/dev fallback: the document class still themes the UI when Tauri
    // is unavailable.
  }
}
