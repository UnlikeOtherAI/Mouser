import type { IconDefinition } from "@fortawesome/fontawesome-svg-core";
import {
  faApple,
  faLinux,
  faWindows,
} from "@fortawesome/free-brands-svg-icons";
import { faMobileScreen } from "@fortawesome/free-solid-svg-icons";

import type { ConnectionState, OsKind } from "./types";

// Font Awesome brand glyphs for each OS (Apple / Windows / Linux); phone falls back
// to a solid mobile icon (there is no single mobile brand mark).
const OS_ICON: Record<OsKind, IconDefinition> = {
  macos: faApple,
  windows: faWindows,
  linux: faLinux,
  phone: faMobileScreen,
};

const OS_LABEL: Record<OsKind, string> = {
  macos: "macOS",
  windows: "Windows",
  linux: "Linux",
  phone: "Phone",
};

export function osIcon(os: OsKind): IconDefinition {
  return OS_ICON[os];
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
