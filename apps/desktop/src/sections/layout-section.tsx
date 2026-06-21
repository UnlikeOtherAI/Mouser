import { LayoutCanvas } from "../components/layout-canvas";
import { MOCK_DEVICES } from "../lib/mock-data";

/**
 * Workspace Layout section — the brief's "central visual feature". Hosts the
 * draggable per-monitor canvas. Arrangement is local-only in this pass; it will
 * replicate cluster-wide once the engine is wired.
 */
export function LayoutSection(): React.JSX.Element {
  return (
    <div className="space-y-4">
      <LayoutCanvas initialDevices={MOCK_DEVICES} />
    </div>
  );
}
