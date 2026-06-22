import { useWorkspace } from "../lib/use-workspace";
import { osGlyph, osLabel, stateMeta } from "../lib/os-meta";
import { cx } from "../lib/cx";

/** Lists the machines in the workspace — this computer plus any LAN peers. */
export function DevicesSection(): React.JSX.Element {
  // `peers` come from a UI-side mDNS browse (polled); the engine will own
  // discovery over `mouser-ipc` later. Until then this surfaces real peers now.
  const { devices, peers, loading } = useWorkspace();

  return (
    <div className="space-y-3">
      <p className="text-sm text-muted">
        This computer is shown below. Other machines running Mouser on your
        network appear here as they are discovered.
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
        {peers.map((peer) => (
          <li
            key={peer.id}
            className="flex items-center justify-between rounded-xl border border-ink-line bg-ink-card px-4 py-3"
          >
            <div className="flex items-center gap-3">
              <span aria-hidden="true" className="text-xl">
                {osGlyph(peer.os)}
              </span>
              <div>
                <p className="text-sm font-semibold text-slate-100">
                  {peer.name}
                </p>
                <p className="text-xs text-muted">
                  {osLabel(peer.os)} · {peer.host}:{peer.port}
                </p>
              </div>
            </div>
            <div className="flex items-center gap-2">
              <span
                aria-hidden="true"
                className="h-2.5 w-2.5 rounded-full bg-sky-400"
              />
              <span className="text-xs font-medium text-sky-300">
                Discovered
              </span>
            </div>
          </li>
        ))}
      </ul>
      {!loading && peers.length === 0 ? (
        <p className="rounded-xl border border-dashed border-ink-line px-4 py-3 text-xs text-muted">
          No other devices found yet. They appear here once another machine runs
          Mouser on this network.
        </p>
      ) : null}
    </div>
  );
}
