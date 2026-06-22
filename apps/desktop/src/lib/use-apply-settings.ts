import { useEffect, useRef } from "react";
import {
  applyTheme,
  resolveEffectiveTheme,
  type ThemeChoice,
} from "./theme-preference";
import type { EngineSettings } from "./types";

/**
 * Applies daemon-owned General preferences to the local machine when they change.
 *
 * The daemon is the single source of truth (settings are persisted there and
 * editable by the UI *and* the MCP server). This hook is the desktop's *apply*
 * side: when `settings` arrives or changes, it reflects the persisted values into
 * the OS / window — system-tray visibility, OS autostart, and the in-app + native
 * theme.
 *
 * Feedback-loop avoidance: each value is applied only when it actually *changed*
 * since the last apply, tracked in refs seeded to `undefined` so the first
 * snapshot is always applied once. This hook never calls `updateSettings`, so an
 * applied side effect can never loop back into another settings write.
 */
export function useApplySettings(settings: EngineSettings): void {
  const lastTrayIcon = useRef<boolean | undefined>(undefined);
  const lastLaunchAtLogin = useRef<boolean | undefined>(undefined);
  const lastTheme = useRef<ThemeChoice | undefined>(undefined);

  // System-tray icon visibility.
  useEffect(() => {
    if (lastTrayIcon.current === settings.show_tray_icon) return;
    lastTrayIcon.current = settings.show_tray_icon;
    void (async () => {
      try {
        const { invoke } = await import("@tauri-apps/api/core");
        await invoke<boolean>("set_tray_icon_visible", {
          visible: settings.show_tray_icon,
        });
      } catch {
        // Browser/dev fallback (no Tauri): nothing to apply.
      }
    })();
  }, [settings.show_tray_icon]);

  // OS autostart (launch at login).
  useEffect(() => {
    if (lastLaunchAtLogin.current === settings.launch_at_login) return;
    lastLaunchAtLogin.current = settings.launch_at_login;
    void (async () => {
      try {
        const { enable, disable } = await import(
          "@tauri-apps/plugin-autostart"
        );
        if (settings.launch_at_login) await enable();
        else await disable();
      } catch {
        // Browser/dev fallback (no Tauri): nothing to apply.
      }
    })();
  }, [settings.launch_at_login]);

  // In-app + native window theme.
  useEffect(() => {
    if (lastTheme.current === settings.theme) return;
    lastTheme.current = settings.theme;
    void applyTheme(settings.theme, resolveEffectiveTheme(settings.theme));
  }, [settings.theme]);

  // While following the system, re-apply when the OS color scheme flips. This
  // does not change `settings`, so it cannot loop back into a settings write.
  useEffect(() => {
    if (settings.theme !== "system") return;
    if (typeof window === "undefined" || !window.matchMedia) return;
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const onChange = (): void => {
      void applyTheme("system", resolveEffectiveTheme("system"));
    };
    media.addEventListener("change", onChange);
    return () => media.removeEventListener("change", onChange);
  }, [settings.theme]);
}
