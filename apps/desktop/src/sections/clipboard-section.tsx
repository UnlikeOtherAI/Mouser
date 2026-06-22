import { SectionCard } from "../components/section-card";
import { Segmented } from "../components/segmented";
import { SettingRow } from "../components/setting-row";
import { Toggle } from "../components/toggle";
import { useWorkspace } from "../lib/use-workspace";
import type { SyncDirection } from "../lib/types";

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
 * Shared clipboard preferences (§7.7) — daemon-owned, edited over IPC (the same
 * state the MCP server reads/writes). The engine enforces these on send/receipt.
 */
export function ClipboardSection(): React.JSX.Element {
  const { settings, updateSettings } = useWorkspace();

  // Master switch gates every other control (§7.7: master off ⇒ no offer sent,
  // inbound offers ignored).
  const enabled = settings.shared_clipboard;

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
              checked={settings.shared_clipboard}
              onChange={(next) => void updateSettings({ shared_clipboard: next })}
            />
          }
        />
        <SettingRow
          title="Direction"
          description="Whether this device sends, receives, or both."
          control={
            <Segmented<SyncDirection>
              label="Clipboard direction"
              value={settings.clipboard_direction}
              onChange={(next) => void updateSettings({ clipboard_direction: next })}
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
              checked={settings.sync_text}
              disabled={!enabled}
              onChange={(next) => void updateSettings({ sync_text: next })}
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
              checked={settings.sync_images}
              disabled={!enabled}
              onChange={(next) => void updateSettings({ sync_images: next })}
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
              checked={settings.sync_files}
              disabled={!enabled}
              onChange={(next) => void updateSettings({ sync_files: next })}
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
              value={sizeKeyFor(settings.max_auto_sync_bytes)}
              onChange={(next) => {
                const preset = SIZE_PRESETS.find((p) => p.value === next);
                void updateSettings({
                  max_auto_sync_bytes: preset ? preset.bytes : 0,
                });
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
              checked={settings.prefer_native_apple}
              disabled={!enabled}
              onChange={(next) => void updateSettings({ prefer_native_apple: next })}
            />
          }
        />
      </SectionCard>
    </div>
  );
}
