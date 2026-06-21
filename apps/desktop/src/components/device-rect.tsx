import { osGlyph, osLabel, stateMeta } from "../lib/os-meta";
import type { Device, Monitor } from "../lib/types";

interface DeviceRectProps {
  device: Device;
  monitor: Monitor;
  width: number;
  height: number;
  selected: boolean;
  /** Index shown as the "identification overlay" number (1-based). */
  badge: number;
  onSelect: () => void;
  onPointerDown: (event: React.PointerEvent<SVGGElement>) => void;
  onKeyDown: (event: React.KeyboardEvent<SVGGElement>) => void;
}

/**
 * A single per-monitor device rectangle on the layout canvas.
 *
 * Renders the brief's required contents (name, OS glyph, connection state,
 * role) and the selection treatment (blue border + glow). It is a focusable,
 * keyboard-operable element so the canvas meets the a11y gate
 * (docs/architecture.md §8 — arrow-key nudging).
 */
export function DeviceRect({
  device,
  monitor,
  width,
  height,
  selected,
  badge,
  onSelect,
  onPointerDown,
  onKeyDown,
}: DeviceRectProps): React.JSX.Element {
  const meta = stateMeta(device.state);
  const dotColor =
    device.state === "connected"
      ? "#34d399"
      : device.state === "connecting"
        ? "#fbbf24"
        : "#64748b";

  return (
    <g
      role="listitem"
      tabIndex={0}
      aria-label={`${device.name}, ${osLabel(device.os)}, ${meta.label}, ${device.role}. Use arrow keys to move.`}
      aria-current={selected ? "true" : undefined}
      transform={`translate(${monitor.x}, ${monitor.y})`}
      onPointerDown={onPointerDown}
      onKeyDown={onKeyDown}
      onClick={onSelect}
      className="cursor-grab focus:outline-none active:cursor-grabbing"
      style={{ touchAction: "none" }}
    >
      <rect
        width={width}
        height={height}
        rx={10}
        ry={10}
        fill={selected ? "#1d2942" : "#222838"}
        stroke={selected ? "#4f8cff" : "#3c4456"}
        strokeWidth={selected ? 2.5 : 1.5}
        style={
          selected
            ? { filter: "drop-shadow(0 0 8px rgba(79,140,255,0.55))" }
            : undefined
        }
      />

      {/* OS glyph */}
      <text x={12} y={26} fontSize={18}>
        {osGlyph(device.os)}
      </text>

      {/* Device name */}
      <text
        x={38}
        y={25}
        fontSize={13}
        fontWeight={600}
        fill="#e2e8f0"
        className="font-sans"
      >
        {device.name}
      </text>

      {/* Identification badge — the "massive centered number" overlay idea,
          here shown as a corner chip in the editor. */}
      <g transform={`translate(${width - 30}, 12)`}>
        <rect width={20} height={20} rx={6} fill="#0f121c" opacity={0.7} />
        <text
          x={10}
          y={14}
          fontSize={11}
          fontWeight={700}
          fill="#cbd5e1"
          textAnchor="middle"
          className="font-mono"
        >
          {badge}
        </text>
      </g>

      {/* Connection state dot + role */}
      <circle cx={18} cy={height - 16} r={4} fill={dotColor} />
      <text x={30} y={height - 12} fontSize={11} fill="#94a3b8">
        {meta.label} · {device.role}
      </text>
    </g>
  );
}
