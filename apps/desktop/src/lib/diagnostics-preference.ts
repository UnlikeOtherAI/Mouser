const STORAGE_KEY = "mouser.showDiagnostics";

/** Whether the Diagnostics view (engine log + action log) is enabled. Off by default. */
export function readDiagnosticsPreference(): boolean {
  if (typeof window === "undefined") return false;
  return window.localStorage.getItem(STORAGE_KEY) === "true";
}

export function writeDiagnosticsPreference(enabled: boolean): void {
  if (typeof window === "undefined") return;
  window.localStorage.setItem(STORAGE_KEY, enabled ? "true" : "false");
}
