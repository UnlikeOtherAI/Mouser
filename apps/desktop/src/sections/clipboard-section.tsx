import { useState } from "react";
import { SectionCard } from "../components/section-card";
import { Segmented } from "../components/segmented";
import { SettingRow } from "../components/setting-row";
import { Toggle } from "../components/toggle";
import { DEFAULT_CLIPBOARD_SETTINGS } from "../lib/mock-data";
import type { ClipboardSettings, SyncDirection } from "../lib/types";

// Size presets for `max_auto_sync_bytes` (§7.7; 0 = unlimited). Keyed by string
// so they fit the `Segmented` radio-group contract.
const SIZE_PRESETS = [
  { value: "0", label: "Unlimited", bytes: 0 },
  { value: "1", label: "1 MiB", bytes: 1024 * 1024 },
  { value: "10", label: "10 MiB", bytes: 10 * 1024 * 1024 },
  { value: "50", label: "50 MiB", bytes: 50 * 1024 * 1024 },
] as const;

type SizeKey = (typeof SIZE_PRESETS)[number]["value"];

function sizeKeyFor(bytes: number): SizeKey {
  const match = SIZE_PRESETS.find((p) => p.bytes === bytes);
  return match ? match.value : "0";
}

/**
 * Shared clipboard preferences (§7.7), mirroring `ClipboardSettings` in
 * crates/mouser-clipboard/src/settings.rs. State is local — no backend wiring
 * yet; the engine enforces these on send and on receipt once IPC lands.
 */
export function ClipboardSection(): React.JSX.Element {
  const [settings, setSettings] = useState<ClipboardSettings>(
    DEFAULT_CLIPBOARD_SETTINGS,
  );

  function update<K extends keyof ClipboardSettings>(
    key: K,
    value: ClipboardSettings[K],
  ): void {
    setSettings((prev) => ({ ...prev, [key]: value }));
  }

  // Master switch gates every other control (§7.7: master off ⇒ no offer sent,
  // inbound offers ignored).
  const enabled = settings.sharedClipboard;

  return (
    <div className="space-y-6">
      <SectionCard
        title="Sharing"
        description="Copy on one device, paste on another across the workspace."
      >
        <SettingRow
          title="Shared clipboard"
          description="Master switch. When off, nothing is sent and inbound copies are ignored."
          control={
            <Toggle
              label="Shared clipboard"
              labelHidden
              checked={settings.sharedClipboard}
              onChange={(next) => update("sharedClipboard", next)}
            />
          }
        />
        <SettingRow
          title="Direction"
          description="Whether this device sends, receives, or both."
          control={
            <Segmented<SyncDirection>
              label="Clipboard direction"
              value={settings.direction}
              onChange={(next) => update("direction", next)}
              disabled={!enabled}
              options={[
                { value: "bidirectional", label: "Bidirectional" },
                { value: "send_only", label: "Send only" },
                { value: "receive_only", label: "Receive only" },
              ]}
            />
          }
        />
      </SectionCard>

      <SectionCard
        title="Formats"
        description="Which kinds of copied content may transfer."
      >
        <SettingRow
          title="Text"
          description="Plain text, HTML, and rich text (RTF)."
          control={
            <Toggle
              label="Sync text"
              labelHidden
              checked={settings.syncText}
              disabled={!enabled}
              onChange={(next) => update("syncText", next)}
            />
          }
        />
        <SettingRow
          title="Images"
          description="Copied images (PNG)."
          control={
            <Toggle
              label="Sync images"
              labelHidden
              checked={settings.syncImages}
              disabled={!enabled}
              onChange={(next) => update("syncImages", next)}
            />
          }
        />
        <SettingRow
          title="Files"
          description="Copied file references."
          control={
            <Toggle
              label="Sync files"
              labelHidden
              checked={settings.syncFiles}
              disabled={!enabled}
              onChange={(next) => update("syncFiles", next)}
            />
          }
        />
      </SectionCard>

      <SectionCard title="Transfer">
        <SettingRow
          title="Auto-sync size limit"
          description="Skip pre-fetching copies larger than this; you can still paste them on demand."
          control={
            <Segmented<SizeKey>
              label="Auto-sync size limit"
              value={sizeKeyFor(settings.maxAutoSyncBytes)}
              onChange={(next) => {
                const preset = SIZE_PRESETS.find((p) => p.value === next);
                update("maxAutoSyncBytes", preset ? preset.bytes : 0);
              }}
              disabled={!enabled}
              options={SIZE_PRESETS.map((p) => ({
                value: p.value,
                label: p.label,
              }))}
            />
          }
        />
        <SettingRow
          title="Prefer macOS/iOS Universal Clipboard between Apple devices"
          description="Let Handoff carry clipboard between two Apple devices instead of Mouser, avoiding double-paste."
          control={
            <Toggle
              label="Prefer Universal Clipboard between Apple devices"
              labelHidden
              checked={settings.preferNativeApple}
              disabled={!enabled}
              onChange={(next) => update("preferNativeApple", next)}
            />
          }
        />
      </SectionCard>
    </div>
  );
}
