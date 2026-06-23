import { useState } from "react";
import { SectionCard } from "../components/section-card";
import { Segmented } from "../components/segmented";
import { SettingRow } from "../components/setting-row";
import { Toggle } from "../components/toggle";
import { useWorkspace } from "../lib/use-workspace";
import type { ThemeChoice } from "../lib/theme-preference";

interface GeneralSectionProps {
  showDiagnostics: boolean;
  onShowDiagnosticsChange: (next: boolean) => void;
}

/** General application preferences — daemon-owned, edited over IPC (the same
 * state the MCP server reads/writes). "Show diagnostics" stays UI-local: it only
 * toggles whether this shell renders the Diagnostics tab. */
export function GeneralSection({
  showDiagnostics,
  onShowDiagnosticsChange,
}: GeneralSectionProps): React.JSX.Element {
  const { settings, updateSettings, resetData } = useWorkspace();
  const [confirmingReset, setConfirmingReset] = useState(false);
  const [resetting, setResetting] = useState(false);

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
              checked={settings.launch_at_login}
              onChange={(next) =>
                void updateSettings({ launch_at_login: next })
              }
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
              checked={settings.show_tray_icon}
              onChange={(next) => void updateSettings({ show_tray_icon: next })}
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
              value={settings.theme}
              onChange={(next) => void updateSettings({ theme: next })}
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
              checked={settings.auto_update}
              onChange={(next) => void updateSettings({ auto_update: next })}
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

      <SectionCard title="Reset">
        <SettingRow
          title="Reset Mouser"
          description="Forget all paired devices and restore default settings. This device keeps its own identity, but other devices will need to pair with it again."
          control={
            confirmingReset ? (
              <div className="flex items-center gap-2">
                <button
                  type="button"
                  disabled={resetting}
                  onClick={() => {
                    setResetting(true);
                    void resetData().finally(() => {
                      setResetting(false);
                      setConfirmingReset(false);
                    });
                  }}
                  className="rounded-lg border border-rose-500/50 bg-rose-500/10 px-3 py-1 text-xs font-medium text-rose-200 hover:bg-rose-500/20 disabled:opacity-50"
                >
                  {resetting ? "Resetting…" : "Reset everything"}
                </button>
                <button
                  type="button"
                  disabled={resetting}
                  onClick={() => setConfirmingReset(false)}
                  className="rounded-lg border border-ink-line px-3 py-1 text-xs font-medium text-fg hover:bg-ink-line disabled:opacity-50"
                >
                  Cancel
                </button>
              </div>
            ) : (
              <button
                type="button"
                onClick={() => setConfirmingReset(true)}
                className="rounded-lg border border-rose-500/50 px-3 py-1 text-xs font-medium text-rose-200 hover:bg-rose-500/10"
              >
                Reset…
              </button>
            )
          }
        />
      </SectionCard>
    </div>
  );
}
