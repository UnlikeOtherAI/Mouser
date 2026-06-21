import type { ConnectionState, OsKind } from "./types";

// Emoji glyphs match the brief's device-rectangle examples
// (🪟 Windows, 🍎 macOS, 🐧 Linux, 📱 Phone).
const OS_GLYPH: Record<OsKind, string> = {
  macos: "🍎",
  windows: "🪟",
  linux: "🐧",
  phone: "📱",
};

const OS_LABEL: Record<OsKind, string> = {
  macos: "macOS",
  windows: "Windows",
  linux: "Linux",
  phone: "Phone",
};

export function osGlyph(os: OsKind): string {
  return OS_GLYPH[os];
}

export function osLabel(os: OsKind): string {
  return OS_LABEL[os];
}

interface StateMeta {
  label: string;
  /** Tailwind classes for the status dot. */
  dot: string;
  text: string;
}

const STATE_META: Record<ConnectionState, StateMeta> = {
  connected: { label: "Connected", dot: "bg-emerald-400", text: "text-emerald-300" },
  connecting: { label: "Connecting", dot: "bg-amber-400", text: "text-amber-300" },
  offline: { label: "Offline", dot: "bg-slate-500", text: "text-slate-400" },
};

export function stateMeta(state: ConnectionState): StateMeta {
  return STATE_META[state];
}
