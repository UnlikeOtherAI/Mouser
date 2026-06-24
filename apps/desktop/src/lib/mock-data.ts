import type { NavItem } from "./types";

// Static UI constants for the desktop shell. Live device/transfer data now comes
// from the real machine via `useWorkspace` (the `local_device` Tauri command);
// cluster-wide peers arrive once the engine is wired over `mouser-ipc`.

export const NAV_ITEMS: NavItem[] = [
  { id: "general", label: "General" },
  { id: "devices", label: "Devices" },
  { id: "layout", label: "Layout" },
  { id: "input", label: "Input" },
  { id: "clipboard", label: "Clipboard" },
  { id: "security", label: "Security" },
];

/** Extra nav entry shown only when the Diagnostics preference is enabled. */
export const DIAGNOSTICS_NAV_ITEM: NavItem = {
  id: "diagnostics",
  label: "Diagnostics",
};
