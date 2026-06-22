import { useWorkspace } from "../lib/use-workspace";
import { osGlyph, osLabel, stateMeta } from "../lib/os-meta";
import { cx } from "../lib/cx";

/** Lists the machines in the workspace — this computer now, peers once paired. */
export function DevicesSection(): React.JSX.Element {
  const { devices, loading } = useWorkspace();

  return (
    <div className="space-y-3">
      <p className="text-sm text-muted">
        This computer is shown below. Other machines on your network appear here
        once the Mouser engine is running and paired.
      </p>
      <ul className="space-y-2">
        {devices.map((device) => {
          const meta = stateMeta(device.state);
          return (
            <li
              key={device.id}
              className="flex items-center justify-between rounded-xl border border-ink-line bg-ink-card px-4 py-3"
            >
              <div className="flex items-center gap-3">
                <span aria-hidden="true" className="text-xl">
                  {osGlyph(device.os)}
                </span>
                <div>
                  <p className="text-sm font-semibold text-slate-100">
                    {device.name}
                  </p>
                  <p className="text-xs text-muted">
                    {osLabel(device.os)} ·{" "}
                    {device.role === "coordinator" ? "This device" : "Member"} ·{" "}
                    {device.monitors.length}{" "}
                    {device.monitors.length === 1 ? "display" : "displays"}
                  </p>
                </div>
              </div>
              <div className="flex items-center gap-2">
                <span
                  aria-hidden="true"
                  className={cx("h-2.5 w-2.5 rounded-full", meta.dot)}
                />
                <span className={cx("text-xs font-medium", meta.text)}>
                  {meta.label}
                </span>
              </div>
            </li>
          );
        })}
      </ul>
      {!loading && devices.length <= 1 ? (
        <p className="rounded-xl border border-dashed border-ink-line px-4 py-3 text-xs text-muted">
          No other devices found yet. Peer discovery arrives with the engine
          connection.
        </p>
      ) : null}
    </div>
  );
}
