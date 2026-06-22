import { useCallback, useMemo, useRef, useState } from "react";
import type { Device } from "./types";
import {
  CANVAS_H,
  CANVAS_W,
  canvasToWorld,
  clampWorldToCanvas,
  computeViewport,
  snapToEdges,
  type Viewport,
} from "./layout-geometry";

// Keyboard nudge step, in logical points (world units).
const NUDGE = 24;
const NUDGE_LARGE = 96;
// Pointer-snap pull radius, in canvas px (converted to world units per drag).
const SNAP_PX = 11;

export interface DragState {
  devices: Device[];
  selectedId: string | null;
  select: (id: string) => void;
  viewport: Viewport;
  onRectPointerDown: (
    deviceId: string,
    monitorId: string,
  ) => (event: React.PointerEvent<SVGGElement>) => void;
  onRectKeyDown: (
    deviceId: string,
    monitorId: string,
  ) => (event: React.KeyboardEvent<SVGGElement>) => void;
  canvasWidth: number;
  canvasHeight: number;
  /** Resets every monitor to its initial position. */
  reset: () => void;
}

interface ActiveDrag {
  deviceId: string;
  monitorId: string;
  pointerId: number;
  /** Grab offset from the rect's world origin to the pointer, in world units. */
  grabX: number;
  grabY: number;
  width: number;
  height: number;
}

/** Maps a client point to this <svg>'s canvas-space coordinates. */
function toCanvasPoint(
  svg: SVGSVGElement,
  clientX: number,
  clientY: number,
): { x: number; y: number } {
  const rect = svg.getBoundingClientRect();
  return {
    x: ((clientX - rect.left) / rect.width) * CANVAS_W,
    y: ((clientY - rect.top) / rect.height) * CANVAS_H,
  };
}

/**
 * Drag + keyboard movement for the layout canvas. Positions are tracked in world
 * (logical-point) units and rendered through a stable viewport, so a screen can
 * be dropped next to another and it snaps their edges together. Fully
 * keyboard-operable (arrow keys nudge, Shift = larger step) for the a11y gate.
 */
export function useLayoutDrag(
  initial: Device[],
  svgRef: React.RefObject<SVGSVGElement | null>,
): DragState {
  const [devices, setDevices] = useState<Device[]>(initial);
  const [selectedId, setSelectedId] = useState<string | null>(
    initial[0]?.id ?? null,
  );
  const drag = useRef<ActiveDrag | null>(null);

  // Stable for the life of this layout: derived from the initial arrangement so
  // the viewport doesn't jump while a screen is being dragged.
  const viewport = useMemo(() => computeViewport(initial), [initial]);

  // World rects of every monitor except the one being dragged — snap targets.
  const otherRects = useCallback(
    (skipMonitorId: string) =>
      devices.flatMap((d) =>
        d.monitors
          .filter((m) => m.id !== skipMonitorId)
          .map((m) => ({ x: m.x, y: m.y, w: m.width, h: m.height })),
      ),
    [devices],
  );

  const setMonitorPos = useCallback(
    (deviceId: string, monitorId: string, x: number, y: number) => {
      setDevices((prev) =>
        prev.map((d) =>
          d.id !== deviceId
            ? d
            : {
                ...d,
                monitors: d.monitors.map((m) =>
                  m.id !== monitorId ? m : { ...m, x, y },
                ),
              },
        ),
      );
    },
    [],
  );

  const onPointerMove = useCallback(
    (event: PointerEvent) => {
      const active = drag.current;
      const svg = svgRef.current;
      if (!active || !svg || event.pointerId !== active.pointerId) return;
      const p = toCanvasPoint(svg, event.clientX, event.clientY);
      const world = canvasToWorld(p.x, p.y, viewport);
      const proposed = {
        x: world.x - active.grabX,
        y: world.y - active.grabY,
        w: active.width,
        h: active.height,
      };
      const snapped = snapToEdges(
        proposed,
        otherRects(active.monitorId),
        SNAP_PX / viewport.scale,
      );
      const final = clampWorldToCanvas(
        snapped.x,
        snapped.y,
        active.width,
        active.height,
        viewport,
      );
      setMonitorPos(active.deviceId, active.monitorId, final.x, final.y);
    },
    [otherRects, setMonitorPos, svgRef, viewport],
  );

  const endDrag = useCallback(() => {
    drag.current = null;
    window.removeEventListener("pointermove", onPointerMove);
    window.removeEventListener("pointerup", endDrag);
    window.removeEventListener("pointercancel", endDrag);
  }, [onPointerMove]);

  const onRectPointerDown = useCallback(
    (deviceId: string, monitorId: string) =>
      (event: React.PointerEvent<SVGGElement>) => {
        const svg = svgRef.current;
        if (!svg) return;
        const device = devices.find((d) => d.id === deviceId);
        const monitor = device?.monitors.find((m) => m.id === monitorId);
        if (!device || !monitor) return;
        setSelectedId(deviceId);
        const p = toCanvasPoint(svg, event.clientX, event.clientY);
        const world = canvasToWorld(p.x, p.y, viewport);
        drag.current = {
          deviceId,
          monitorId,
          pointerId: event.pointerId,
          grabX: world.x - monitor.x,
          grabY: world.y - monitor.y,
          width: monitor.width,
          height: monitor.height,
        };
        window.addEventListener("pointermove", onPointerMove);
        window.addEventListener("pointerup", endDrag);
        window.addEventListener("pointercancel", endDrag);
      },
    [devices, endDrag, onPointerMove, svgRef, viewport],
  );

  const onRectKeyDown = useCallback(
    (deviceId: string, monitorId: string) =>
      (event: React.KeyboardEvent<SVGGElement>) => {
        const step = event.shiftKey ? NUDGE_LARGE : NUDGE;
        let dx = 0;
        let dy = 0;
        switch (event.key) {
          case "ArrowLeft":
            dx = -step;
            break;
          case "ArrowRight":
            dx = step;
            break;
          case "ArrowUp":
            dy = -step;
            break;
          case "ArrowDown":
            dy = step;
            break;
          case "Enter":
          case " ":
            setSelectedId(deviceId);
            event.preventDefault();
            return;
          default:
            return;
        }
        event.preventDefault();
        setSelectedId(deviceId);
        const device = devices.find((d) => d.id === deviceId);
        const monitor = device?.monitors.find((m) => m.id === monitorId);
        if (!device || !monitor) return;
        const final = clampWorldToCanvas(
          monitor.x + dx,
          monitor.y + dy,
          monitor.width,
          monitor.height,
          viewport,
        );
        setMonitorPos(deviceId, monitorId, final.x, final.y);
      },
    [devices, setMonitorPos, viewport],
  );

  const reset = useCallback(() => setDevices(initial), [initial]);

  return {
    devices,
    selectedId,
    select: setSelectedId,
    viewport,
    onRectPointerDown,
    onRectKeyDown,
    canvasWidth: CANVAS_W,
    canvasHeight: CANVAS_H,
    reset,
  };
}
