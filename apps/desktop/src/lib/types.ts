// Shared UI domain types for the Mouser desktop shell.
//
// These mirror the *shape* of what `mouser-ipc` will eventually deliver, but
// are intentionally UI-local for now: this pass has NO backend wiring
// (docs/architecture.md §8 — the Tauri UI links `mouser-ipc`, not core).

export type OsKind = "macos" | "windows" | "linux" | "phone";

export type ConnectionState = "connected" | "connecting" | "offline";

export type DeviceRole = "coordinator" | "member";

/** A physical monitor attached to a device, placed on the layout canvas. */
export interface Monitor {
  id: string;
  /** Logical resolution; used only to size the rectangle proportionally. */
  width: number;
  height: number;
  /** Top-left position on the canvas, in canvas pixels. */
  x: number;
  y: number;
}

/** A connected machine in the workspace. */
export interface Device {
  id: string;
  name: string;
  os: OsKind;
  state: ConnectionState;
  role: DeviceRole;
  monitors: Monitor[];
}

export type SectionId =
  | "general"
  | "devices"
  | "layout"
  | "input"
  | "clipboard"
  | "security";

export interface NavItem {
  id: SectionId;
  label: string;
}
