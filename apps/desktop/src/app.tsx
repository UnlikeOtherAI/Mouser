import { useEffect, useState } from "react";
import { ClipboardProgress } from "./components/clipboard-progress";
import { SideNav } from "./components/side-nav";
import { DIAGNOSTICS_NAV_ITEM, NAV_ITEMS } from "./lib/mock-data";
import {
  readDiagnosticsPreference,
  writeDiagnosticsPreference,
} from "./lib/diagnostics-preference";
import {
  readSectionPreference,
  writeSectionPreference,
} from "./lib/section-preference";
import { useApplySettings } from "./lib/use-apply-settings";
import { useWorkspace } from "./lib/workspace-context";
import type { ClipboardTransfer, SectionId } from "./lib/types";
import { GeneralSection } from "./sections/general-section";
import { DevicesSection } from "./sections/devices-section";
import { LayoutSection } from "./sections/layout-section";
import { InputSection } from "./sections/input-section";
import { ClipboardSection } from "./sections/clipboard-section";
import { SecuritySection } from "./sections/security-section";
import { DiagnosticsSection } from "./sections/diagnostics-section";

// No in-flight transfers without the engine; live progress arrives over IPC.
const CLIPBOARD_TRANSFERS: ClipboardTransfer[] = [];

const SECTION_TITLES: Record<SectionId, string> = {
  general: "General",
  devices: "Devices",
  layout: "Workspace Layout",
  input: "Input",
  clipboard: "Clipboard",
  security: "Security",
  diagnostics: "Diagnostics",
};

interface GeneralSettingsProps {
  showDiagnostics: boolean;
  onShowDiagnosticsChange: (next: boolean) => void;
}

function renderSection(
  id: SectionId,
  general: GeneralSettingsProps,
): React.JSX.Element {
  switch (id) {
    case "general":
      return (
        <GeneralSection
          showDiagnostics={general.showDiagnostics}
          onShowDiagnosticsChange={general.onShowDiagnosticsChange}
        />
      );
    case "devices":
      return <DevicesSection />;
    case "layout":
      return <LayoutSection />;
    case "input":
      return <InputSection />;
    case "clipboard":
      return <ClipboardSection />;
    case "security":
      return <SecuritySection />;
    case "diagnostics":
      return <DiagnosticsSection />;
  }
}

/** Top-level settings/layout shell: left nav + active section panel. */
export function App(): React.JSX.Element {
  const [active, setActive] = useState<SectionId>(() => {
    const saved = readSectionPreference();
    // Don't restore the Diagnostics tab when it's disabled — it has no nav entry.
    if (saved === "diagnostics" && !readDiagnosticsPreference()) return "layout";
    return saved;
  });
  const [showDiagnostics, setShowDiagnostics] = useState(
    readDiagnosticsPreference,
  );

  // Remember the active section across restarts, so the next launch lands back on it.
  useEffect(() => {
    writeSectionPreference(active);
  }, [active]);

  // General prefs (tray icon, launch-at-login, theme, auto-update) are
  // daemon-owned. Apply the daemon's persisted values to this machine whenever
  // they change; the apply side never writes settings back, so there is no loop.
  const { settings } = useWorkspace();
  useApplySettings(settings);

  // Launch-at-login is strictly opt-in: Mouser never enables it automatically. The
  // user turns it on from the "Launch at login" toggle in General settings, which
  // now writes the daemon-owned `launch_at_login` setting; `useApplySettings`
  // reflects that into the real OS autostart state.

  function handleShowDiagnosticsChange(next: boolean): void {
    setShowDiagnostics(next);
    writeDiagnosticsPreference(next);
    // Don't strand the user on a now-hidden tab.
    if (!next && active === "diagnostics") setActive("devices");
  }

  const navItems = showDiagnostics
    ? [...NAV_ITEMS, DIAGNOSTICS_NAV_ITEM]
    : NAV_ITEMS;

  return (
    <div className="flex h-screen w-screen overflow-hidden bg-ink text-fg">
      <SideNav items={navItems} active={active} onSelect={setActive} />
      <main
        id={`panel-${active}`}
        role="tabpanel"
        aria-labelledby={`tab-${active}`}
        tabIndex={0}
        className="flex-1 overflow-y-auto"
      >
        <div className="mx-auto max-w-3xl px-8 py-7">
          <h1 className="mb-5 text-xl font-semibold tracking-tight">
            {SECTION_TITLES[active]}
          </h1>
          {renderSection(active, {
            showDiagnostics,
            onShowDiagnosticsChange: handleShowDiagnosticsChange,
          })}
        </div>
      </main>
      <ClipboardProgress transfers={CLIPBOARD_TRANSFERS} />
    </div>
  );
}
