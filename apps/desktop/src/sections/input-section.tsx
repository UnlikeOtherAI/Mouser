import { SectionCard } from "../components/section-card";
import { Segmented } from "../components/segmented";
import { SettingRow } from "../components/setting-row";
import { Toggle } from "../components/toggle";
import { useWorkspace } from "../lib/workspace-context";
import type { EdgeBehavior } from "../lib/types";

/** Input ownership / pointer-crossing preferences — daemon-owned, edited over IPC
 * (the same state the MCP server reads/writes). */
export function InputSection(): React.JSX.Element {
  const { settings, updateSettings } = useWorkspace();

  return (
    <div className="space-y-6">
      <SectionCard title="Pointer crossing">
        <SettingRow
          title="Cross screens at edges"
          description="Move the cursor to an adjacent device when it reaches a shared edge."
          control={
            <Toggle
              label="Cross screens at edges"
              labelHidden
              checked={settings.cross_at_edges}
              onChange={(next) => void updateSettings({ cross_at_edges: next })}
            />
          }
        />
        <SettingRow
          title="Edge behaviour"
          description="How quickly ownership transfers when the cursor hits an edge."
          control={
            <Segmented<EdgeBehavior>
              label="Edge behaviour"
              value={settings.edge_behavior}
              onChange={(next) => void updateSettings({ edge_behavior: next })}
              options={[
                { value: "instant", label: "Instant" },
                { value: "delayed", label: "Delayed" },
                { value: "locked", label: "Locked" },
              ]}
            />
          }
        />
        <SettingRow
          title="Wrap around"
          description="Crossing the far edge returns the cursor to the opposite side."
          control={
            <Toggle
              label="Wrap around"
              labelHidden
              checked={settings.wrap_around}
              onChange={(next) => void updateSettings({ wrap_around: next })}
            />
          }
        />
      </SectionCard>

      <SectionCard title="Shared input">
        <SettingRow
          title="Share scroll wheel"
          description="Forward scroll events to the device that owns the cursor."
          control={
            <Toggle
              label="Share scroll wheel"
              labelHidden
              checked={settings.share_scroll}
              onChange={(next) => void updateSettings({ share_scroll: next })}
            />
          }
        />
      </SectionCard>
    </div>
  );
}
