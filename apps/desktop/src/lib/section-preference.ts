import type { SectionId } from "./types";

const STORAGE_KEY = "mouser.activeSection";

const VALID_SECTIONS: readonly SectionId[] = [
  "general",
  "devices",
  "layout",
  "input",
  "clipboard",
  "security",
  "diagnostics",
];

/** The section the user was last on (restored on launch). Defaults to "layout"
 * when nothing valid is stored (first run, or a stale/unknown id). */
export function readSectionPreference(): SectionId {
  if (typeof window === "undefined") return "layout";
  const stored = window.localStorage.getItem(STORAGE_KEY);
  return stored !== null && (VALID_SECTIONS as readonly string[]).includes(stored)
    ? (stored as SectionId)
    : "layout";
}

/** Remember the active section so the next launch lands back on it. */
export function writeSectionPreference(section: SectionId): void {
  if (typeof window === "undefined") return;
  window.localStorage.setItem(STORAGE_KEY, section);
}
