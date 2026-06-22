import { useRef } from "react";
import { useLayoutDrag } from "../lib/use-layout-drag";
import { toCanvasRect } from "../lib/layout-geometry";
import type { Device } from "../lib/types";
import { DeviceRect } from "./device-rect";

interface LayoutCanvasProps {
  initialDevices: Device[];
}

/**
 * The Workspace Layout canvas: a gray surface that shows the real display
 * arrangement (fit to the canvas) where each monitor is a draggable rectangle
 * (docs/brief.md "Layout Canvas" / "Drag Arrangement"). Dragging snaps screen
 * edges together; arrow keys nudge a focused screen.
 */
export function LayoutCanvas({
  initialDevices,
}: LayoutCanvasProps): React.JSX.Element {
  const svgRef = useRef<SVGSVGElement | null>(null);
  const {
    devices,
    selectedId,
    select,
    viewport,
    onRectPointerDown,
    onRectKeyDown,
    canvasWidth,
    canvasHeight,
    reset,
  } = useLayoutDrag(initialDevices, svgRef);

  // Stable 1-based badge numbers, by device order.
  const badgeFor = (id: string): number =>
    devices.findIndex((d) => d.id === id) + 1;

  return (
    <div className="flex flex-col gap-3">
      <div className="flex items-center justify-between">
        <p className="text-sm text-muted">
          Drag a screen to arrange it — edges snap together. Or focus one and use
          the arrow keys (hold Shift for larger steps).
        </p>
        <button
          type="button"
          onClick={reset}
          className="rounded-lg border border-ink-line px-3 py-1.5 text-sm font-medium text-muted transition-colors hover:text-fg-strong focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
        >
          Reset layout
        </button>
      </div>

      <div className="overflow-hidden rounded-xl border border-ink-line">
        <svg
          ref={svgRef}
          viewBox={`0 0 ${canvasWidth} ${canvasHeight}`}
          role="list"
          aria-label="Workspace layout canvas. Each item is a movable screen."
          className="block w-full bg-canvas"
          style={{ aspectRatio: `${canvasWidth} / ${canvasHeight}` }}
        >
          {/* Subtle grid to imply a workspace plane. */}
          <defs>
            <pattern
              id="grid"
              width={40}
              height={40}
              patternUnits="userSpaceOnUse"
            >
              <path
                d="M 40 0 L 0 0 0 40"
                fill="none"
                stroke="#454b58"
                strokeWidth={1}
              />
            </pattern>
          </defs>
          <rect width={canvasWidth} height={canvasHeight} fill="url(#grid)" />

          {devices.flatMap((device) =>
            device.monitors.map((monitor) => {
              const rect = toCanvasRect(monitor, viewport);
              return (
                <DeviceRect
                  key={monitor.id}
                  device={device}
                  x={rect.x}
                  y={rect.y}
                  width={rect.w}
                  height={rect.h}
                  selected={device.id === selectedId}
                  badge={badgeFor(device.id)}
                  onSelect={() => select(device.id)}
                  onPointerDown={onRectPointerDown(device.id, monitor.id)}
                  onKeyDown={onRectKeyDown(device.id, monitor.id)}
                />
              );
            }),
          )}
        </svg>
      </div>
    </div>
  );
}
