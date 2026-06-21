import { useState } from "react";
import { SectionCard } from "../components/section-card";
import { Segmented } from "../components/segmented";
import { SettingRow } from "../components/setting-row";
import { Toggle } from "../components/toggle";

type Appearance = "system" | "light" | "dark";

/** General application preferences (static — no persistence yet). */
export function GeneralSection(): React.JSX.Element {
  const [launchAtLogin, setLaunchAtLogin] = useState(true);
  const [showMenuBar, setShowMenuBar] = useState(true);
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
          title="Show menu bar icon"
          description="Keep a quick-access icon in the system tray."
          control={
            <Toggle
              label="Show menu bar icon"
              labelHidden
              checked={showMenuBar}
              onChange={setShowMenuBar}
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
