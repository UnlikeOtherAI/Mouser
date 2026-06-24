import {
  useInputPermissions,
  type PermissionKind,
} from "../lib/use-input-permissions";

const LABELS: Record<PermissionKind, string> = {
  accessibility: "Accessibility",
  input_monitoring: "Input Monitoring",
};

/**
 * Warns when this machine lacks the OS grants needed to control a peer (drive its cursor)
 * and offers one-click access to the right Settings pane. Without these, the cursor
 * silently won't cross to the other machine. Renders nothing when grants are present or the
 * platform doesn't gate this.
 */
export function PermissionBanner(): React.JSX.Element | null {
  const { permissions, request } = useInputPermissions();
  if (!permissions || !permissions.relevant) return null;

  const missing: PermissionKind[] = [];
  if (!permissions.accessibility) missing.push("accessibility");
  if (!permissions.inputMonitoring) missing.push("input_monitoring");
  if (missing.length === 0) return null;

  return (
    <div
      role="alert"
      className="mb-5 rounded-lg border border-amber-500/40 bg-amber-500/5 px-4 py-3"
    >
      <p className="text-sm font-medium text-fg-strong">
        Mouser needs {missing.map((k) => LABELS[k]).join(" & ")} to control
        another computer
      </p>
      <p className="mt-1 text-xs text-muted">
        Without {missing.length > 1 ? "these" : "this"}, your cursor can't cross
        to the other machine. Grant access below, then{" "}
        <span className="font-medium text-fg">restart Mouser</span> (macOS
        applies the change on restart).
      </p>
      <div className="mt-3 flex flex-wrap gap-2">
        {missing.map((kind) => (
          <button
            key={kind}
            type="button"
            onClick={() => void request(kind)}
            className="rounded-lg border border-amber-500/50 px-3 py-1 text-xs font-medium text-amber-200 hover:bg-amber-500/10"
          >
            Grant {LABELS[kind]}
          </button>
        ))}
      </div>
    </div>
  );
}
