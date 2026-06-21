import { useCallback, useRef } from "react";
import { useLayoutDrag } from "../lib/use-layout-drag";
import type { Device } from "../lib/types";
import { DeviceRect } from "./device-rect";

interface LayoutCanvasProps {
  initialDevices: Device[];
}

// Scale physical pixels down to canvas units; clamp so a phone is still legible
// and a 4K panel doesn't dominate.
const SCALE = 0.12;
const MIN_W = 90;
const MIN_H = 60;

function sizeFor(width: number, height: number): { w: number; h: number } {
  return {
    w: Math.max(MIN_W, Math.round(width * SCALE)),
    h: Math.max(MIN_H, Math.round(height * SCALE)),
  };
}

/**
 * The Workspace Layout canvas: a large gray surface where each device's
 * monitor is a draggable rectangle (docs/brief.md "Layout Canvas" /
 * "Drag Arrangement"). Pointer drag + arrow-key nudging are both supported.
 */
export function LayoutCanvas({
  initialDevices,
}: LayoutCanvasProps): React.JSX.Element {
  const svgRef = useRef<SVGSVGElement | null>(null);
  const monitorSize = useCallback(
    (w: number, h: number) => sizeFor(w, h),
    [],
  );
  const {
    devices,
    selectedId,
    select,
    onRectPointerDown,
    onRectKeyDown,
    canvasWidth,
    canvasHeight,
    reset,
  } = useLayoutDrag(initialDevices, svgRef, monitorSize);

  // Stable 1-based badge numbers, by device order.
  const badgeFor = (id: string): number =>
    devices.findIndex((d) => d.id === id) + 1;

  return (
    <div className="flex flex-col gap-3">
      <div className="flex items-center justify-between">
        <p className="text-sm text-muted">
          Drag a screen to arrange it, or focus one and use the arrow keys
          (hold Shift for larger steps).
        </p>
        <button
          type="button"
          onClick={reset}
          className="rounded-lg border border-ink-line px-3 py-1.5 text-sm font-medium text-muted transition-colors hover:text-slate-200 focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
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
          <rect
            width={canvasWidth}
            height={canvasHeight}
            fill="url(#grid)"
          />

          {devices.flatMap((device) =>
            device.monitors.map((monitor) => {
              const size = sizeFor(monitor.width, monitor.height);
              return (
                <DeviceRect
                  key={monitor.id}
                  device={device}
                  monitor={monitor}
                  width={size.w}
                  height={size.h}
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
