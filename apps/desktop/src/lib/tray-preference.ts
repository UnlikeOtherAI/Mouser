const STORAGE_KEY = "mouser.showTrayIcon";

export function readTrayIconPreference(): boolean {
  if (typeof window === "undefined") return true;
  return window.localStorage.getItem(STORAGE_KEY) !== "false";
}

export function writeTrayIconPreference(visible: boolean): void {
  window.localStorage.setItem(STORAGE_KEY, visible ? "true" : "false");
}

export async function syncTrayIconPreference(visible: boolean): Promise<void> {
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke<boolean>("set_tray_icon_visible", { visible });
  } catch {
    // Browser/dev fallback: the setting still updates locally when Tauri is
    // unavailable.
  }
}
