import { useState } from "react";
import { SectionCard } from "../components/section-card";
import { Segmented } from "../components/segmented";
import { SettingRow } from "../components/setting-row";
import { Toggle } from "../components/toggle";

type HistoryDepth = "off" | "10" | "50";

/** Shared clipboard preferences (static placeholder). */
export function ClipboardSection(): React.JSX.Element {
  const [shareClipboard, setShareClipboard] = useState(true);
  const [shareImages, setShareImages] = useState(true);
  const [shareFiles, setShareFiles] = useState(false);
  const [history, setHistory] = useState<HistoryDepth>("10");

  return (
    <div className="space-y-6">
      <SectionCard title="Sharing">
        <SettingRow
          title="Share clipboard"
          description="Copy on one device, paste on another across the workspace."
          control={
            <Toggle
              label="Share clipboard"
              labelHidden
              checked={shareClipboard}
              onChange={setShareClipboard}
            />
          }
        />
        <SettingRow
          title="Include images"
          description="Sync copied images, not just text."
          control={
            <Toggle
              label="Include images"
              labelHidden
              checked={shareImages}
              onChange={setShareImages}
            />
          }
        />
        <SettingRow
          title="Include files"
          description="Allow copied files to transfer between devices."
          control={
            <Toggle
              label="Include files"
              labelHidden
              checked={shareFiles}
              onChange={setShareFiles}
            />
          }
        />
      </SectionCard>

      <SectionCard title="History">
        <SettingRow
          title="Clipboard history"
          description="Keep a cluster-wide history of recent clipboard entries."
          control={
            <Segmented<HistoryDepth>
              label="Clipboard history"
              value={history}
              onChange={setHistory}
              options={[
                { value: "off", label: "Off" },
                { value: "10", label: "10 items" },
                { value: "50", label: "50 items" },
              ]}
            />
          }
        />
      </SectionCard>
    </div>
  );
}
