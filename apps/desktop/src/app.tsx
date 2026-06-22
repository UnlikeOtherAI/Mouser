import { useEffect, useState } from "react";
import { ClipboardProgress } from "./components/clipboard-progress";
import { SideNav } from "./components/side-nav";
import { DIAGNOSTICS_NAV_ITEM, NAV_ITEMS } from "./lib/mock-data";
import {
  readTrayIconPreference,
  syncTrayIconPreference,
  writeTrayIconPreference,
} from "./lib/tray-preference";
import {
  readDiagnosticsPreference,
  writeDiagnosticsPreference,
} from "./lib/diagnostics-preference";
import type { ThemeChoice } from "./lib/theme-preference";
import { useTheme } from "./lib/use-theme";
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
  showTrayIcon: boolean;
  onShowTrayIconChange: (next: boolean) => void;
  theme: ThemeChoice;
  onThemeChange: (next: ThemeChoice) => void;
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
          showTrayIcon={general.showTrayIcon}
          onShowTrayIconChange={general.onShowTrayIconChange}
          theme={general.theme}
          onThemeChange={general.onThemeChange}
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
  const [active, setActive] = useState<SectionId>("layout");
  const [showTrayIcon, setShowTrayIcon] = useState(readTrayIconPreference);
  const [showDiagnostics, setShowDiagnostics] = useState(
    readDiagnosticsPreference,
  );
  const { theme, setTheme } = useTheme();

  useEffect(() => {
    void syncTrayIconPreference(showTrayIcon);
  }, [showTrayIcon]);

  // Launch-at-login is strictly opt-in: Mouser never enables it automatically. The
  // user turns it on from the "Launch at login" toggle in General settings, which
  // reflects and drives the real OS autostart state via tauri-plugin-autostart.

  function handleShowTrayIconChange(next: boolean): void {
    setShowTrayIcon(next);
    writeTrayIconPreference(next);
  }

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
            showTrayIcon,
            onShowTrayIconChange: handleShowTrayIconChange,
            theme,
            onThemeChange: setTheme,
            showDiagnostics,
            onShowDiagnosticsChange: handleShowDiagnosticsChange,
          })}
        </div>
      </main>
      <ClipboardProgress transfers={CLIPBOARD_TRANSFERS} />
    </div>
  );
}
