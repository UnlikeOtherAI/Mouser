import { useState } from "react";
import { ClipboardProgress } from "./components/clipboard-progress";
import { SideNav } from "./components/side-nav";
import { MOCK_CLIPBOARD_TRANSFERS, NAV_ITEMS } from "./lib/mock-data";
import type { SectionId } from "./lib/types";
import { GeneralSection } from "./sections/general-section";
import { DevicesSection } from "./sections/devices-section";
import { LayoutSection } from "./sections/layout-section";
import { InputSection } from "./sections/input-section";
import { ClipboardSection } from "./sections/clipboard-section";
import { SecuritySection } from "./sections/security-section";

const SECTION_TITLES: Record<SectionId, string> = {
  general: "General",
  devices: "Devices",
  layout: "Workspace Layout",
  input: "Input",
  clipboard: "Clipboard",
  security: "Security",
};

function renderSection(id: SectionId): React.JSX.Element {
  switch (id) {
    case "general":
      return <GeneralSection />;
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
  }
}

/** Top-level settings/layout shell: left nav + active section panel. */
export function App(): React.JSX.Element {
  const [active, setActive] = useState<SectionId>("layout");

  return (
    <div className="flex h-screen w-screen overflow-hidden bg-ink text-slate-100">
      <SideNav items={NAV_ITEMS} active={active} onSelect={setActive} />
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
          {renderSection(active)}
        </div>
      </main>
      <ClipboardProgress transfers={MOCK_CLIPBOARD_TRANSFERS} />
    </div>
  );
}
