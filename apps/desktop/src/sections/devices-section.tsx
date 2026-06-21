import { MOCK_DEVICES } from "../lib/mock-data";
import { osGlyph, osLabel, stateMeta } from "../lib/os-meta";
import { cx } from "../lib/cx";

/** Lists the machines in the workspace (static placeholder data). */
export function DevicesSection(): React.JSX.Element {
  return (
    <div className="space-y-3">
      <p className="text-sm text-muted">
        Devices discovered on your local network. Pairing and live status arrive
        with the engine connection.
      </p>
      <ul className="space-y-2">
        {MOCK_DEVICES.map((device) => {
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
                    {device.role === "coordinator" ? "Coordinator" : "Member"}
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
    </div>
  );
}
