import { useCallback, useRef, useState } from "react";
import type { Device } from "./types";

const CANVAS_W = 1000;
const CANVAS_H = 520;
const NUDGE = 8;
const NUDGE_LARGE = 32;

export interface DragState {
  devices: Device[];
  selectedId: string | null;
  select: (id: string) => void;
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
  /** Offset from the rect origin to the pointer, in canvas units. */
  dx: number;
  dy: number;
  width: number;
  height: number;
}

/** Maps a client point to canvas-space coordinates for the given <svg>. */
function toCanvasPoint(
  svg: SVGSVGElement,
  clientX: number,
  clientY: number,
): { x: number; y: number } {
  const rect = svg.getBoundingClientRect();
  const x = ((clientX - rect.left) / rect.width) * CANVAS_W;
  const y = ((clientY - rect.top) / rect.height) * CANVAS_H;
  return { x, y };
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

/**
 * Drag + keyboard movement logic for the layout canvas, kept out of the view
 * component. Movement is pointer-driven and also fully keyboard-operable
 * (arrow keys nudge, Shift = larger step) to satisfy the a11y gate.
 */
export function useLayoutDrag(
  initial: Device[],
  svgRef: React.RefObject<SVGSVGElement | null>,
  monitorSize: (width: number, height: number) => { w: number; h: number },
): DragState {
  const [devices, setDevices] = useState<Device[]>(initial);
  const [selectedId, setSelectedId] = useState<string | null>(
    initial[0]?.id ?? null,
  );
  const drag = useRef<ActiveDrag | null>(null);

  const moveMonitor = useCallback(
    (
      deviceId: string,
      monitorId: string,
      x: number,
      y: number,
      width: number,
      height: number,
    ) => {
      setDevices((prev) =>
        prev.map((d) =>
          d.id !== deviceId
            ? d
            : {
                ...d,
                monitors: d.monitors.map((m) =>
                  m.id !== monitorId
                    ? m
                    : {
                        ...m,
                        x: clamp(x, 0, CANVAS_W - width),
                        y: clamp(y, 0, CANVAS_H - height),
                      },
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
      moveMonitor(
        active.deviceId,
        active.monitorId,
        p.x - active.dx,
        p.y - active.dy,
        active.width,
        active.height,
      );
    },
    [moveMonitor, svgRef],
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
        const size = monitorSize(monitor.width, monitor.height);
        const p = toCanvasPoint(svg, event.clientX, event.clientY);
        drag.current = {
          deviceId,
          monitorId,
          pointerId: event.pointerId,
          dx: p.x - monitor.x,
          dy: p.y - monitor.y,
          width: size.w,
          height: size.h,
        };
        window.addEventListener("pointermove", onPointerMove);
        window.addEventListener("pointerup", endDrag);
        window.addEventListener("pointercancel", endDrag);
      },
    [devices, endDrag, monitorSize, onPointerMove, svgRef],
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
        const size = monitorSize(monitor.width, monitor.height);
        moveMonitor(
          deviceId,
          monitorId,
          monitor.x + dx,
          monitor.y + dy,
          size.w,
          size.h,
        );
      },
    [devices, monitorSize, moveMonitor],
  );

  const reset = useCallback(() => setDevices(initial), [initial]);

  return {
    devices,
    selectedId,
    select: setSelectedId,
    onRectPointerDown,
    onRectKeyDown,
    canvasWidth: CANVAS_W,
    canvasHeight: CANVAS_H,
    reset,
  };
}
