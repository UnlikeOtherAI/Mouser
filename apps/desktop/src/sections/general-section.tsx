import { useEffect, useState } from "react";
import { SectionCard } from "../components/section-card";
import { Segmented } from "../components/segmented";
import { SettingRow } from "../components/setting-row";
import { Toggle } from "../components/toggle";
import type { ThemeChoice } from "../lib/theme-preference";

interface GeneralSectionProps {
  showTrayIcon: boolean;
  onShowTrayIconChange: (next: boolean) => void;
  theme: ThemeChoice;
  onThemeChange: (next: ThemeChoice) => void;
  showDiagnostics: boolean;
  onShowDiagnosticsChange: (next: boolean) => void;
}

/** General application preferences. */
export function GeneralSection({
  showTrayIcon,
  onShowTrayIconChange,
  theme,
  onThemeChange,
  showDiagnostics,
  onShowDiagnosticsChange,
}: GeneralSectionProps): React.JSX.Element {
  // "Launch at login" reflects the real OS autostart state (macOS LaunchAgent,
  // Windows Run key, Linux .desktop) via tauri-plugin-autostart — not local
  // state. `null` until the initial `isEnabled()` query resolves, which also
  // disables the toggle so we never flash a wrong value or fire before we know.
  const [launchAtLogin, setLaunchAtLogin] = useState<boolean | null>(null);
  const [autoUpdate, setAutoUpdate] = useState(true);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const { isEnabled } = await import("@tauri-apps/plugin-autostart");
        const enabled = await isEnabled();
        if (!cancelled) setLaunchAtLogin(enabled);
      } catch {
        // Browser/dev fallback (no Tauri): treat autostart as off.
        if (!cancelled) setLaunchAtLogin(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  async function handleLaunchAtLoginChange(next: boolean): Promise<void> {
    // Optimistically reflect the request, then reconcile with the real state
    // the plugin reports (so a failed enable/disable doesn't lie to the user).
    setLaunchAtLogin(next);
    try {
      const { enable, disable, isEnabled } = await import(
        "@tauri-apps/plugin-autostart"
      );
      if (next) await enable();
      else await disable();
      setLaunchAtLogin(await isEnabled());
    } catch {
      setLaunchAtLogin(!next);
    }
  }

  return (
    <div className="space-y-6">
      <SectionCard title="Startup">
        <SettingRow
          title="Launch at login"
          description="Start Mouser automatically when you sign in."
          control={
            <Toggle
              label="Launch at login"
              labelHidden
              checked={launchAtLogin ?? false}
              disabled={launchAtLogin === null}
              onChange={(next) => void handleLaunchAtLoginChange(next)}
            />
          }
        />
        <SettingRow
          title="Show tray icon"
          description="Keep Mouser in the system tray instead of the taskbar."
          control={
            <Toggle
              label="Show tray icon"
              labelHidden
              checked={showTrayIcon}
              onChange={onShowTrayIconChange}
            />
          }
        />
      </SectionCard>

      <SectionCard title="Appearance">
        <SettingRow
          title="Theme"
          description="Match the system theme or pick one."
          control={
            <Segmented<ThemeChoice>
              label="Theme"
              value={theme}
              onChange={onThemeChange}
              options={[
                { value: "system", label: "System" },
                { value: "light", label: "Light" },
                { value: "dark", label: "Dark" },
              ]}
            />
          }
        />
      </SectionCard>

      <SectionCard title="Updates">
        <SettingRow
          title="Automatic updates"
          description="Download and install new versions in the background."
          control={
            <Toggle
              label="Automatic updates"
              labelHidden
              checked={autoUpdate}
              onChange={setAutoUpdate}
            />
          }
        />
      </SectionCard>

      <SectionCard title="Diagnostics">
        <SettingRow
          title="Show diagnostics"
          description="Add a Diagnostics tab with the engine log, discovered peer ids, and a connect/pair activity log — for troubleshooting connections."
          control={
            <Toggle
              label="Show diagnostics"
              labelHidden
              checked={showDiagnostics}
              onChange={onShowDiagnosticsChange}
            />
          }
        />
      </SectionCard>
    </div>
  );
}
