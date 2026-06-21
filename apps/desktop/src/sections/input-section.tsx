import { useState } from "react";
import { SectionCard } from "../components/section-card";
import { Segmented } from "../components/segmented";
import { SettingRow } from "../components/setting-row";
import { Toggle } from "../components/toggle";

type EdgeBehaviour = "instant" | "delayed" | "locked";

/** Input ownership / pointer-crossing preferences (static placeholder). */
export function InputSection(): React.JSX.Element {
  const [crossOnEdge, setCrossOnEdge] = useState(true);
  const [wrapAround, setWrapAround] = useState(false);
  const [shareScroll, setShareScroll] = useState(true);
  const [edge, setEdge] = useState<EdgeBehaviour>("instant");

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
              checked={crossOnEdge}
              onChange={setCrossOnEdge}
            />
          }
        />
        <SettingRow
          title="Edge behaviour"
          description="How quickly ownership transfers when the cursor hits an edge."
          control={
            <Segmented<EdgeBehaviour>
              label="Edge behaviour"
              value={edge}
              onChange={setEdge}
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
              checked={wrapAround}
              onChange={setWrapAround}
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
              checked={shareScroll}
              onChange={setShareScroll}
            />
          }
        />
      </SectionCard>
    </div>
  );
}
