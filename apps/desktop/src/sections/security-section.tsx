import { SectionCard } from "../components/section-card";
import { SettingRow } from "../components/setting-row";
import { Toggle } from "../components/toggle";
import { useWorkspace } from "../lib/use-workspace";

/** Trust & permission preferences — daemon-owned, edited over IPC. */
export function SecuritySection(): React.JSX.Element {
  const { settings, updateSettings } = useWorkspace();

  return (
    <div className="space-y-6">
      <SectionCard
        title="Trust"
        description="New devices must be approved before they can send input."
      >
        <SettingRow
          title="Require approval for new devices"
          description="Show a pairing prompt with a short verification code (SAS)."
          control={
            <Toggle
              label="Require approval for new devices"
              labelHidden
              checked={settings.require_approval}
              onChange={(next) => void updateSettings({ require_approval: next })}
            />
          }
        />
        <SettingRow
          title="Encrypted connections only"
          description="Refuse to connect to peers that fail certificate pinning."
          control={
            <Toggle
              label="Encrypted connections only"
              labelHidden
              checked={settings.encrypted_only}
              onChange={(next) => void updateSettings({ encrypted_only: next })}
            />
          }
        />
      </SectionCard>

      <SectionCard title="Session">
        <SettingRow
          title="Release input when locked"
          description="Return keyboard and mouse ownership to the local device on sleep or lock."
          control={
            <Toggle
              label="Release input when locked"
              labelHidden
              checked={settings.release_on_lock}
              onChange={(next) => void updateSettings({ release_on_lock: next })}
            />
          }
        />
      </SectionCard>
    </div>
  );
}
