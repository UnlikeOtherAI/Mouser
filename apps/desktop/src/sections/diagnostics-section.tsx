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

/** Human label for a remediation action id (see the engine's `HealthItemDto`). */
function remediationLabel(action: string): string {
  switch (action) {
    case "open_network_settings":
      return "Open network settings";
    case "check_firewall":
      return "Open firewall settings";
    default:
      return "Fix";
  }
}

/** Live diagnostics: this device's pairing id + discovered peers, the in-app action
 * log, and the engine daemon's own log — so connect/pair problems are visible. */
export function DiagnosticsSection(): React.JSX.Element {
  const { localId, peers, connection, engineRunning, diagnostics, runRemediation } =
    useWorkspace();
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
      <SectionCard title="Connectivity health">
        {diagnostics.length === 0 ? (
          <p className="px-1 text-xs text-emerald-300">
            All clear — no connectivity problems detected.
          </p>
        ) : (
          <ul className="space-y-2">
            {diagnostics.map((item) => {
              const remediation = item.remediation;
              const tone =
                item.severity === "error"
                  ? "border-rose-500/40 bg-rose-500/5"
                  : item.severity === "warning"
                    ? "border-amber-500/40 bg-amber-500/5"
                    : "border-ink-line bg-ink";
              const dot =
                item.severity === "error"
                  ? "bg-rose-400"
                  : item.severity === "warning"
                    ? "bg-amber-400"
                    : "bg-sky-400";
              return (
                <li
                  key={item.code}
                  className={cx("rounded-lg border px-3 py-2 text-xs", tone)}
                >
                  <div className="flex items-start justify-between gap-3">
                    <div className="space-y-1">
                      <p className="flex items-center gap-2 font-semibold text-fg">
                        <span
                          aria-hidden="true"
                          className={cx("h-2 w-2 rounded-full", dot)}
                        />
                        {item.title}
                      </p>
                      <p className="text-muted">{item.detail}</p>
                    </div>
                    {remediation ? (
                      <button
                        type="button"
                        onClick={() => void runRemediation(remediation)}
                        className="shrink-0 rounded-lg border border-sky-500/50 px-3 py-1 text-xs font-medium text-sky-200 hover:bg-sky-500/10"
                      >
                        {remediationLabel(remediation)}
                      </button>
                    ) : null}
                  </div>
                </li>
              );
            })}
          </ul>
        )}
      </SectionCard>

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
              The engine's own diagnostics (discovery, dials, trust checks),
              captured in-process. Refreshes every {ENGINE_LOG_POLL_MS / 1000}s.
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
              : "(no engine log captured yet)"}
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
