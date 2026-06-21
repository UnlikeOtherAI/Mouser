import type {
  ClipboardSettings,
  ClipboardTransfer,
  Device,
  NavItem,
} from "./types";

// Static placeholder data for the no-backend scaffold pass.
// Replaced by live `mouser-ipc` state once wiring lands.

export const NAV_ITEMS: NavItem[] = [
  { id: "general", label: "General" },
  { id: "devices", label: "Devices" },
  { id: "layout", label: "Layout" },
  { id: "input", label: "Input" },
  { id: "clipboard", label: "Clipboard" },
  { id: "security", label: "Security" },
];

export const MOCK_DEVICES: Device[] = [
  {
    id: "dev-mac",
    name: "Studio Mac",
    os: "macos",
    state: "connected",
    role: "coordinator",
    monitors: [{ id: "m-mac-1", width: 2560, height: 1440, x: 300, y: 160 }],
  },
  {
    id: "dev-win",
    name: "Game Rig",
    os: "windows",
    state: "connected",
    role: "member",
    monitors: [{ id: "m-win-1", width: 1920, height: 1080, x: 620, y: 190 }],
  },
  {
    id: "dev-linux",
    name: "Build Box",
    os: "linux",
    state: "connecting",
    role: "member",
    monitors: [{ id: "m-linux-1", width: 1920, height: 1200, x: 60, y: 150 }],
  },
  {
    id: "dev-phone",
    name: "Pixel",
    os: "phone",
    state: "offline",
    role: "member",
    monitors: [{ id: "m-phone-1", width: 412, height: 915, x: 900, y: 230 }],
  },
];

// Spec defaults, matching `ClipboardSettings::default()` in
// crates/mouser-clipboard/src/settings.rs (§7.7): sharing on, all formats on,
// unlimited size, prefer-native on, bidirectional.
export const DEFAULT_CLIPBOARD_SETTINGS: ClipboardSettings = {
  sharedClipboard: true,
  syncText: true,
  syncImages: true,
  syncFiles: true,
  maxAutoSyncBytes: 0,
  preferNativeApple: true,
  direction: "bidirectional",
};

// Sample in-flight transfers so the Mac-style progress indicator renders in the
// no-backend shell. Replaced by live engine progress events once IPC lands.
export const MOCK_CLIPBOARD_TRANSFERS: ClipboardTransfer[] = [
  {
    id: "xfer-1",
    peer: "Game Rig",
    direction: "incoming",
    format: "image",
    received: 3_200_000,
    total: 7_800_000,
    state: "active",
  },
  {
    id: "xfer-2",
    peer: "Studio Mac",
    direction: "outgoing",
    format: "files",
    received: 12_500_000,
    total: 12_500_000,
    state: "done",
  },
  {
    id: "xfer-3",
    peer: "Build Box",
    direction: "incoming",
    format: "text",
    received: 0,
    total: 1_400_000,
    state: "failed",
  },
];
