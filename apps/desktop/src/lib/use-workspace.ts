import { useEffect, useState } from "react";
import type { Device, OsKind, Peer } from "./types";

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

// Shape returned by the `discovered_peers` Tauri command (src-tauri/src/lib.rs).
interface RawPeer {
  id: string;
  name: string;
  os: string;
  host: string;
  port: number;
}

// How often we re-poll the backend's mDNS peer snapshot. UI-side shortcut until
// the engine pushes peers over `mouser-ipc` (no event subscription yet).
const PEER_POLL_MS = 2000;

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

function toPeer(raw: RawPeer): Peer {
  return {
    id: raw.id,
    name: raw.name || raw.host,
    os: osKindOf(raw.os),
    host: raw.host,
    port: raw.port,
  };
}

export interface Workspace {
  /** The local machine, plus any peers once the engine is wired. */
  devices: Device[];
  /** Peers discovered on the LAN over mDNS (UI-side shortcut, polled). */
  peers: Peer[];
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
  const [peers, setPeers] = useState<Peer[]>([]);
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

  // Poll the backend's mDNS peer snapshot. UI-side shortcut: the engine will
  // eventually push peers over `mouser-ipc`, replacing this interval.
  useEffect(() => {
    let cancelled = false;
    let timer: ReturnType<typeof setInterval> | undefined;
    void (async () => {
      let invoke: typeof import("@tauri-apps/api/core").invoke;
      try {
        ({ invoke } = await import("@tauri-apps/api/core"));
      } catch {
        // No Tauri runtime (browser dev) — there are no LAN peers to show.
        return;
      }
      const poll = async (): Promise<void> => {
        try {
          const raw = await invoke<RawPeer[]>("discovered_peers");
          if (!cancelled) setPeers(raw.map(toPeer));
        } catch {
          // Transient invoke failure — keep the last snapshot.
        }
      };
      await poll();
      if (!cancelled) timer = setInterval(() => void poll(), PEER_POLL_MS);
    })();
    return () => {
      cancelled = true;
      if (timer !== undefined) clearInterval(timer);
    };
  }, []);

  return { devices, peers, loading };
}
