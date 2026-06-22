import { useCallback, useEffect, useState } from "react";
import { SectionCard } from "../components/section-card";
import { useWorkspace } from "../lib/use-workspace";
import { clearDebugLog, useDebugLog } from "../lib/debug-log";
import { cx } from "../lib/cx";

const ENGINE_LOG_POLL_MS = 2000;

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

/** Live diagnostics: this device's pairing id + discovered peers, the in-app action
 * log, and the engine daemon's own log — so connect/pair problems are visible. */
export function DiagnosticsSection(): React.JSX.Element {
  const { localId, peers, connection, engineRunning } = useWorkspace();
  const actionLog = useDebugLog();
  const [engineLog, setEngineLog] = useState<string>("");
  const [engineLogError, setEngineLogError] = useState<string | null>(null);

  const refreshEngineLog = useCallback(async (): Promise<void> => {
    const invoke = await tauriInvoke();
    if (invoke === null) {
      setEngineLogError("Engine log is only available in the desktop app.");
      return;
    }
    try {
      const text = await invoke<string>("engine_log");
      setEngineLog(text);
      setEngineLogError(null);
    } catch (e) {
      setEngineLogError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  useEffect(() => {
    void refreshEngineLog();
    const timer = setInterval(() => void refreshEngineLog(), ENGINE_LOG_POLL_MS);
    return () => clearInterval(timer);
  }, [refreshEngineLog]);

  return (
    <div className="space-y-6">
      <SectionCard title="This device">
        <div className="space-y-2 px-1 py-1 text-xs">
          <p className="text-muted">
            Engine:{" "}
            <span className={engineRunning ? "text-emerald-300" : "text-rose-300"}>
              {engineRunning ? "running" : "not reachable"}
            </span>{" "}
            · connection: <span className="text-fg">{connection.state}</span>
          </p>
          <p className="text-muted">
            Pairing id (other devices trust this):
          </p>
          <code className="block break-all rounded-md bg-ink px-2 py-1 text-fg-strong">
            {localId ?? "—"}
          </code>
        </div>
      </SectionCard>

      <SectionCard title="Discovered peers">
        {peers.length === 0 ? (
          <p className="px-1 text-xs text-muted">No peers discovered.</p>
        ) : (
          <ul className="space-y-2 text-xs">
            {peers.map((peer) => (
              <li key={peer.id} className="rounded-md bg-ink px-2 py-1.5">
                <p className="font-medium text-fg">
                  {peer.name}{" "}
                  <span className={peer.trusted ? "text-sky-300" : "text-muted"}>
                    ({peer.trusted ? "paired" : "not paired"})
                  </span>{" "}
                  · {peer.host}:{peer.port}
                </p>
                <code className="block break-all text-fg-strong">{peer.id}</code>
              </li>
            ))}
          </ul>
        )}
      </SectionCard>

      <SectionCard title="Engine log">
        <div className="space-y-2">
          <div className="flex items-center justify-between">
            <p className="text-xs text-muted">
              The <code>mouserd</code> daemon's own diagnostics (discovery, dials,
              trust checks). Refreshes every {ENGINE_LOG_POLL_MS / 1000}s.
            </p>
            <button
              type="button"
              onClick={() => void refreshEngineLog()}
              className="rounded-lg border border-ink-line px-3 py-1 text-xs font-medium text-fg hover:bg-ink-line"
            >
              Refresh
            </button>
          </div>
          {engineLogError ? (
            <p className="text-xs text-rose-300">{engineLogError}</p>
          ) : null}
          <pre className="max-h-72 overflow-auto whitespace-pre-wrap break-all rounded-md bg-ink px-3 py-2 text-[11px] leading-relaxed text-fg">
            {engineLog.trim().length > 0
              ? engineLog
              : "(no engine log yet — the daemon writes here once it starts)"}
          </pre>
        </div>
      </SectionCard>

      <SectionCard title="Activity log">
        <div className="space-y-2">
          <div className="flex items-center justify-between">
            <p className="text-xs text-muted">
              Connect / pair / disconnect actions and connection state changes.
            </p>
            <button
              type="button"
              onClick={clearDebugLog}
              className="rounded-lg border border-ink-line px-3 py-1 text-xs font-medium text-fg hover:bg-ink-line"
            >
              Clear
            </button>
          </div>
          <div className="max-h-72 overflow-auto rounded-md bg-ink px-3 py-2 text-[11px] leading-relaxed">
            {actionLog.length === 0 ? (
              <p className="text-muted">No activity yet.</p>
            ) : (
              <ul className="space-y-0.5">
                {actionLog
                  .slice()
                  .reverse()
                  .map((entry, idx) => (
                    <li
                      key={`${entry.time}-${idx}`}
                      className={cx(
                        "font-mono",
                        entry.level === "error" ? "text-rose-300" : "text-fg",
                      )}
                    >
                      <span className="text-muted">{entry.time}</span>{" "}
                      {entry.message}
                    </li>
                  ))}
              </ul>
            )}
          </div>
        </div>
      </SectionCard>
    </div>
  );
}
