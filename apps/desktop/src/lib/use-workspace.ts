import { useEffect, useState } from "react";
import type { Device, OsKind } from "./types";

// Shape returned by the `local_device` Tauri command (src-tauri/src/lib.rs).
interface RawMonitor {
  id: string;
  name: string;
  x: number;
  y: number;
  width: number;
  height: number;
  scale: number;
}
interface RawLocalDevice {
  id: string;
  name: string;
  os: string;
  monitors: RawMonitor[];
}

// Browser/dev fallback when there is no Tauri runtime (e.g. `pnpm dev` in a
// plain browser). Two screens side by side so snapping is exercised.
const FALLBACK: Device[] = [
  {
    id: "local",
    name: "This Mac",
    os: "macos",
    state: "connected",
    role: "coordinator",
    monitors: [
      { id: "local-mon-0", width: 1512, height: 982, x: 0, y: 0 },
      { id: "local-mon-1", width: 1920, height: 1080, x: 1512, y: -49 },
    ],
  },
];

function osKindOf(os: string): OsKind {
  if (os === "windows") return "windows";
  if (os === "linux") return "linux";
  return "macos";
}

function toDevice(raw: RawLocalDevice): Device {
  return {
    id: raw.id || "local",
    name: raw.name || "This computer",
    os: osKindOf(raw.os),
    state: "connected",
    role: "coordinator",
    monitors: raw.monitors.map((m) => ({
      id: m.id,
      width: m.width,
      height: m.height,
      x: m.x,
      y: m.y,
      scale: m.scale,
    })),
  };
}

export interface Workspace {
  /** The local machine, plus any peers once the engine is wired. */
  devices: Device[];
  /** True until the real device query resolves (or falls back). */
  loading: boolean;
}

/**
 * Loads the real local device (name, OS, physical display layout) from the
 * Tauri backend. Falls back to a representative two-screen Mac when running
 * outside Tauri so the UI is still usable in a browser.
 */
export function useWorkspace(): Workspace {
  const [devices, setDevices] = useState<Device[]>(FALLBACK);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const { invoke } = await import("@tauri-apps/api/core");
        const raw = await invoke<RawLocalDevice>("local_device");
        if (!cancelled && raw.monitors.length > 0) {
          setDevices([toDevice(raw)]);
        }
      } catch {
        // No Tauri runtime (browser dev) — keep the fallback device.
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  return { devices, loading };
}
