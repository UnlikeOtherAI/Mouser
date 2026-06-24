import { useMemo } from "react";
import { LayoutCanvas } from "../components/layout-canvas";
import { useWorkspace } from "../lib/workspace-context";
import type { CrossEdge, Device, Monitor } from "../lib/types";

/** Logical-point gap drawn between this machine and the connected peer's screen. */
const PEER_GAP = 80;
/** Fallback peer screen size when no local proxy size is available. */
const DEFAULT_PEER_SIZE = { width: 1920, height: 1080 };

const EDGE_OPTIONS: { value: CrossEdge; label: string }[] = [
  { value: "left", label: "Left" },
  { value: "right", label: "Right" },
  { value: "top", label: "Top" },
  { value: "bottom", label: "Bottom" },
];

/**
 * Workspace Layout — shows this machine's displays and, while connected, the peer's screen
 * on the edge the cursor crosses to. That edge is user-configurable (the engine uses it to
 * decide where the cursor leaves this screen); the peer's box is approximated from this
 * machine's primary display until live per-peer geometry is exchanged.
 */
export function LayoutSection(): React.JSX.Element {
  const { devices, peers, connection, settings, updateSettings, loading } =
    useWorkspace();
  const edge = settings.cross_edge;

  const allDevices = useMemo<Device[]>(() => {
    if (connection.state !== "connected" || !connection.peerId) return devices;
    // Don't double-add if the peer is already represented (future cluster wiring).
    if (devices.some((d) => d.id === connection.peerId)) return devices;

    const localMonitors = devices.flatMap((d) => d.monitors);
    if (localMonitors.length === 0) return devices;

    const minX = Math.min(...localMonitors.map((m) => m.x));
    const maxX = Math.max(...localMonitors.map((m) => m.x + m.width));
    const minY = Math.min(...localMonitors.map((m) => m.y));
    const maxY = Math.max(...localMonitors.map((m) => m.y + m.height));
    const primary = localMonitors.reduce((a, b) =>
      a.width * a.height >= b.width * b.height ? a : b,
    );
    const w = primary.width > 0 ? primary.width : DEFAULT_PEER_SIZE.width;
    const h = primary.height > 0 ? primary.height : DEFAULT_PEER_SIZE.height;

    // Place the peer on the configured edge so the picture matches the crossing direction.
    const pos =
      edge === "left"
        ? { x: minX - PEER_GAP - w, y: minY }
        : edge === "top"
          ? { x: minX, y: minY - PEER_GAP - h }
          : edge === "bottom"
            ? { x: minX, y: maxY + PEER_GAP }
            : { x: maxX + PEER_GAP, y: minY }; // right (default)

    const peer = peers.find((p) => p.id === connection.peerId);
    const peerMonitor: Monitor = {
      id: `${connection.peerId}-display`,
      width: w,
      height: h,
      x: pos.x,
      y: pos.y,
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
  }, [devices, peers, connection.state, connection.peerId, edge]);

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
          <div className="flex items-center gap-3">
            <span className="text-xs font-medium text-fg">
              Other computer is on my
            </span>
            <div className="flex gap-1">
              {EDGE_OPTIONS.map((opt) => (
                <button
                  key={opt.value}
                  type="button"
                  onClick={() => void updateSettings({ cross_edge: opt.value })}
                  aria-pressed={edge === opt.value}
                  className={`rounded-lg border px-3 py-1 text-xs font-medium ${
                    edge === opt.value
                      ? "border-accent bg-accent text-on-accent"
                      : "border-ink-line text-fg hover:bg-ink-line"
                  }`}
                >
                  {opt.label}
                </button>
              ))}
            </div>
          </div>
          <p className="text-xs text-muted">
            Move your cursor off your screen's <strong>{edge}</strong> edge to
            control the other computer. Changing the side applies on the next
            connection.
          </p>
        </>
      )}
    </div>
  );
}
