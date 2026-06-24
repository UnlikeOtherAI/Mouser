import { useMemo } from "react";
import { LayoutCanvas } from "../components/layout-canvas";
import { useWorkspace } from "../lib/workspace-context";
import type { Device, Monitor } from "../lib/types";

/** Logical-point gap drawn between this machine and the connected peer's screen. */
const PEER_GAP = 80;
/** Fallback peer screen size when no local proxy size is available. */
const DEFAULT_PEER_SIZE = { width: 1920, height: 1080 };

/**
 * Workspace Layout — shows this machine's displays and, while connected, the peer's screen
 * to the right (the side the cursor crosses to). The peer's exact resolution isn't exchanged
 * yet, so its box is approximated from this machine's primary display and labelled as the
 * remote machine so the topology is clear.
 */
export function LayoutSection(): React.JSX.Element {
  const { devices, peers, connection, loading } = useWorkspace();

  const allDevices = useMemo<Device[]>(() => {
    if (connection.state !== "connected" || !connection.peerId) return devices;
    // Don't double-add if the peer is already represented (future cluster wiring).
    if (devices.some((d) => d.id === connection.peerId)) return devices;

    const localMonitors = devices.flatMap((d) => d.monitors);
    if (localMonitors.length === 0) return devices;

    const rightEdge = Math.max(...localMonitors.map((m) => m.x + m.width));
    const primary = localMonitors.reduce((a, b) =>
      a.width * a.height >= b.width * b.height ? a : b,
    );
    const size =
      primary.width > 0 && primary.height > 0
        ? { width: primary.width, height: primary.height }
        : DEFAULT_PEER_SIZE;

    const peer = peers.find((p) => p.id === connection.peerId);
    const peerMonitor: Monitor = {
      id: `${connection.peerId}-display`,
      width: size.width,
      height: size.height,
      x: rightEdge + PEER_GAP,
      y: 0,
    };
    const peerDevice: Device = {
      id: connection.peerId,
      name: peer?.name ?? "Connected peer",
      os: peer?.os ?? "windows",
      state: "connected",
      role: "member",
      monitors: [peerMonitor],
    };
    return [...devices, peerDevice];
  }, [devices, peers, connection.state, connection.peerId]);

  return (
    <div className="space-y-4">
      {loading ? (
        <p className="text-sm text-muted">Detecting displays…</p>
      ) : (
        <>
          <LayoutCanvas
            key={allDevices.map((d) => d.id).join(",")}
            initialDevices={allDevices}
          />
          {connection.state === "connected" && (
            <p className="text-xs text-muted">
              The connected peer's screen is shown on the right — the edge your
              cursor crosses to. Its size is approximate until live display
              geometry is exchanged.
            </p>
          )}
        </>
      )}
    </div>
  );
}
