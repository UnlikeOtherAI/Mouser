import { useEffect, useState } from "react";

/** Input-permission status for controlling another machine (mirrors the Rust
 *  `input_permissions` command). `relevant` is false on platforms that don't gate this. */
export interface InputPermissions {
  relevant: boolean;
  accessibility: boolean;
  inputMonitoring: boolean;
}

export type PermissionKind = "accessibility" | "input_monitoring";

const POLL_MS = 3000;

interface RawInputPermissions {
  relevant: boolean;
  accessibility: boolean;
  input_monitoring: boolean;
}

async function tauriInvoke(): Promise<
  typeof import("@tauri-apps/api/core").invoke | null
> {
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    return invoke;
  } catch {
    return null;
  }
}

/**
 * Polls the OS input-permission status (Accessibility / Input Monitoring) and exposes a
 * `request` action that triggers the system prompt and opens the exact Settings pane. The
 * status updates after the user grants (and the app is restarted, which macOS requires).
 */
export function useInputPermissions(): {
  permissions: InputPermissions | null;
  request: (kind: PermissionKind) => Promise<void>;
} {
  const [permissions, setPermissions] = useState<InputPermissions | null>(null);

  useEffect(() => {
    let active = true;
    const poll = async (): Promise<void> => {
      const invoke = await tauriInvoke();
      if (!invoke) return;
      try {
        const raw = await invoke<RawInputPermissions>("input_permissions");
        if (active) {
          setPermissions({
            relevant: raw.relevant,
            accessibility: raw.accessibility,
            inputMonitoring: raw.input_monitoring,
          });
        }
      } catch {
        // Outside Tauri or command unavailable — leave status unknown.
      }
    };
    void poll();
    const id = window.setInterval(() => void poll(), POLL_MS);
    return () => {
      active = false;
      window.clearInterval(id);
    };
  }, []);

  const request = async (kind: PermissionKind): Promise<void> => {
    const invoke = await tauriInvoke();
    if (!invoke) return;
    try {
      await invoke("request_input_permission", { kind });
    } catch {
      // Best-effort; the user can still open Settings manually.
    }
  };

  return { permissions, request };
}
