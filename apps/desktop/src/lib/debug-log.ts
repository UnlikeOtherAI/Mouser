import { useSyncExternalStore } from "react";

/** One line in the in-app diagnostics log (frontend-side events). */
export interface LogEntry {
  /** Wall-clock time the entry was recorded (local time string). */
  time: string;
  level: "info" | "error";
  message: string;
}

const MAX_ENTRIES = 400;

// A tiny module-level store so the log is shared across components (the Devices
// section records events; the Diagnostics section displays them) without threading
// it through props or a context provider.
let entries: LogEntry[] = [];
const listeners = new Set<() => void>();

function emit(): void {
  for (const listener of listeners) listener();
}

/** Append a diagnostics line (kept to the most recent {@link MAX_ENTRIES}). */
export function logDebug(level: LogEntry["level"], message: string): void {
  const time =
    typeof Date === "undefined" ? "" : new Date().toLocaleTimeString();
  entries = [...entries, { time, level, message }].slice(-MAX_ENTRIES);
  emit();
}

/** Clear the in-app log. */
export function clearDebugLog(): void {
  entries = [];
  emit();
}

function getSnapshot(): LogEntry[] {
  return entries;
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** Subscribe a component to the shared diagnostics log. */
export function useDebugLog(): LogEntry[] {
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
}
