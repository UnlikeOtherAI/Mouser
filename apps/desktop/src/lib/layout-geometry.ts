import type { Device, Monitor } from "./types";

// The layout canvas is a fixed logical drawing surface; the real (logical-point)
// monitor arrangement is fit into it. Everything the view draws is in canvas px;
// everything we store/snap is in world (logical) points.
export const CANVAS_W = 1000;
export const CANVAS_H = 520;

const PAD = 40; // canvas-px breathing room around the whole arrangement
const MAX_SCALE = 0.28; // don't blow a single small screen up to fill the canvas
export const MIN_RECT = 64; // floor so a tiny screen stays clickable (canvas px)

export interface Viewport {
  /** Multiplier from world (logical points) to canvas px. */
  scale: number;
  offsetX: number;
  offsetY: number;
}

export interface CanvasRect {
  x: number;
  y: number;
  w: number;
  h: number;
}

interface WorldRect {
  x: number;
  y: number;
  w: number;
  h: number;
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

/** Fit every monitor's world rect into the canvas, centered, with padding. */
export function computeViewport(devices: Device[]): Viewport {
  const mons = devices.flatMap((d) => d.monitors);
  if (mons.length === 0) {
    return { scale: MAX_SCALE, offsetX: PAD, offsetY: PAD };
  }
  let minX = Infinity;
  let minY = Infinity;
  let maxX = -Infinity;
  let maxY = -Infinity;
  for (const m of mons) {
    minX = Math.min(minX, m.x);
    minY = Math.min(minY, m.y);
    maxX = Math.max(maxX, m.x + m.width);
    maxY = Math.max(maxY, m.y + m.height);
  }
  const worldW = Math.max(1, maxX - minX);
  const worldH = Math.max(1, maxY - minY);
  const scale = Math.min(
    MAX_SCALE,
    (CANVAS_W - PAD * 2) / worldW,
    (CANVAS_H - PAD * 2) / worldH,
  );
  const offsetX = (CANVAS_W - worldW * scale) / 2 - minX * scale;
  const offsetY = (CANVAS_H - worldH * scale) / 2 - minY * scale;
  return { scale, offsetX, offsetY };
}

/** World monitor -> canvas rect (px), honoring a minimum clickable size. */
export function toCanvasRect(m: Monitor, vp: Viewport): CanvasRect {
  return {
    x: m.x * vp.scale + vp.offsetX,
    y: m.y * vp.scale + vp.offsetY,
    w: Math.max(MIN_RECT, m.width * vp.scale),
    h: Math.max(MIN_RECT, m.height * vp.scale),
  };
}

/** Canvas-px point -> world (logical) point. */
export function canvasToWorld(
  cx: number,
  cy: number,
  vp: Viewport,
): { x: number; y: number } {
  return { x: (cx - vp.offsetX) / vp.scale, y: (cy - vp.offsetY) / vp.scale };
}

/** Keep a monitor's world position so its drawn rect stays inside the canvas. */
export function clampWorldToCanvas(
  worldX: number,
  worldY: number,
  width: number,
  height: number,
  vp: Viewport,
): { x: number; y: number } {
  const cw = Math.max(MIN_RECT, width * vp.scale);
  const ch = Math.max(MIN_RECT, height * vp.scale);
  const cx = clamp(worldX * vp.scale + vp.offsetX, 0, CANVAS_W - cw);
  const cy = clamp(worldY * vp.scale + vp.offsetY, 0, CANVAS_H - ch);
  return canvasToWorld(cx, cy, vp);
}

/**
 * Magnetic edge snapping (like the macOS Displays arrangement). Given a proposed
 * world rect and the other monitors' world rects, snap X and Y independently to
 * the nearest edge — adjacency (my edge meets their opposite edge) or alignment
 * (shared edges line up) — when within `threshold` world units.
 */
export function snapToEdges(
  proposed: WorldRect,
  others: WorldRect[],
  threshold: number,
): { x: number; y: number } {
  const pL = proposed.x;
  const pR = proposed.x + proposed.w;
  const pT = proposed.y;
  const pB = proposed.y + proposed.h;

  let bestX = proposed.x;
  let bestDX = threshold;
  let bestY = proposed.y;
  let bestDY = threshold;

  for (const o of others) {
    const oL = o.x;
    const oR = o.x + o.w;
    const oT = o.y;
    const oB = o.y + o.h;

    // X: each candidate is { resulting x, distance from the moved edge }.
    const xCands = [
      { x: oR, d: Math.abs(pL - oR) }, // sit to the right of o
      { x: oL - proposed.w, d: Math.abs(pR - oL) }, // sit to the left of o
      { x: oL, d: Math.abs(pL - oL) }, // align left edges
      { x: oR - proposed.w, d: Math.abs(pR - oR) }, // align right edges
    ];
    for (const c of xCands) {
      if (c.d < bestDX) {
        bestDX = c.d;
        bestX = c.x;
      }
    }

    const yCands = [
      { y: oB, d: Math.abs(pT - oB) }, // sit below o
      { y: oT - proposed.h, d: Math.abs(pB - oT) }, // sit above o
      { y: oT, d: Math.abs(pT - oT) }, // align top edges
      { y: oB - proposed.h, d: Math.abs(pB - oB) }, // align bottom edges
    ];
    for (const c of yCands) {
      if (c.d < bestDY) {
        bestDY = c.d;
        bestY = c.y;
      }
    }
  }

  return { x: bestX, y: bestY };
}
