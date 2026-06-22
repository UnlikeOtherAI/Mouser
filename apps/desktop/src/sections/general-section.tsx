import { useState } from "react";
import { SectionCard } from "../components/section-card";
import { Segmented } from "../components/segmented";
import { SettingRow } from "../components/setting-row";
import { Toggle } from "../components/toggle";

type Appearance = "system" | "light" | "dark";

interface GeneralSectionProps {
  showTrayIcon: boolean;
  onShowTrayIconChange: (next: boolean) => void;
}

/** General application preferences. */
export function GeneralSection({
  showTrayIcon,
  onShowTrayIconChange,
}: GeneralSectionProps): React.JSX.Element {
  const [launchAtLogin, setLaunchAtLogin] = useState(true);
  const [autoUpdate, setAutoUpdate] = useState(true);
  const [appearance, setAppearance] = useState<Appearance>("system");

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
              checked={launchAtLogin}
              onChange={setLaunchAtLogin}
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
            <Segmented<Appearance>
              label="Theme"
              value={appearance}
              onChange={setAppearance}
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
    </div>
  );
}
