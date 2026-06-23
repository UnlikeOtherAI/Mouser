import { LayoutCanvas } from "../components/layout-canvas";
import { useWorkspace } from "../lib/workspace-context";

/**
 * Workspace Layout section — the brief's "central visual feature". Shows this
 * machine's real display arrangement on the draggable canvas. Arrangement is
 * local-only in this pass; it will replicate cluster-wide once the engine wires.
 */
export function LayoutSection(): React.JSX.Element {
  const { devices, loading } = useWorkspace();

  return (
    <div className="space-y-4">
      {loading ? (
        <p className="text-sm text-muted">Detecting displays…</p>
      ) : (
        <LayoutCanvas
          key={devices.map((d) => d.id).join(",")}
          initialDevices={devices}
        />
      )}
    </div>
  );
}
