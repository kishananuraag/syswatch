import { useEffect } from "react";
import { AnalyticsView } from "./components/AnalyticsView";
import { AppsView } from "./components/AppsView";
import { DeviceMenu } from "./components/DeviceMenu";
import { Overview } from "./components/Overview";
import { SettingsMenu } from "./components/SettingsMenu";
import { Topbar } from "./components/Topbar";
import { useDashboard, type TabName } from "./state/useDashboard";
import type { BatteryApi } from "./data/types";

export default function App() {
  const d = useDashboard();

  // A click anywhere off the device switcher or the settings panel closes it,
  // like the original's document-level handler.
  useEffect(() => {
    if (!d.deviceMenuOpen && !d.settingsOpen) return;
    const onDocClick = (e: MouseEvent) => {
      const target = e.target as HTMLElement | null;
      if (!target) return;
      if (d.deviceMenuOpen && !target.closest("[data-device-pill]") && !target.closest("[data-device-menu]")) {
        d.closeDeviceMenu();
      }
      if (d.settingsOpen && !target.closest("[data-settings-btn]") && !target.closest("[data-settings-menu]")) {
        d.closeSettings();
      }
    };
    document.addEventListener("click", onDocClick);
    return () => document.removeEventListener("click", onDocClick);
  }, [d.deviceMenuOpen, d.settingsOpen, d.closeDeviceMenu, d.closeSettings]);

  const shown = (name: TabName) => d.tab === name;

  return (
    <>
      <Topbar
        tab={d.tab}
        onTab={d.selectTab}
        range={d.range}
        onRange={d.pickRange}
        deviceMenuOpen={d.deviceMenuOpen}
        settingsOpen={d.settingsOpen}
        onSettings={d.toggleSettings}
        onDevicePill={d.openDeviceMenu}
      />
      <DeviceMenu
        open={d.deviceMenuOpen}
        pairHint={d.pairHint}
        onAdd={d.showPairing}
        mode={d.mode}
        apiBase={d.apiBase}
        onConnect={d.connect}
      />
      <SettingsMenu
        open={d.settingsOpen}
        tiles={d.tiles}
        onToggleTile={d.toggleTile}
        onResetTiles={d.resetTiles}
        mode={d.mode}
        apiBase={d.apiBase}
        onConnect={d.connect}
      />

      <main className="main">
        <Overview
          current={d.current}
          history={d.history}
          consumers={d.consumers}
          sensors={d.sensors}
          thermal={d.thermal}
          batteryApi={d.battery as BatteryApi}
          range={d.range}
          live={d.live}
          focused={d.focused}
          onFocus={d.focusTile}
          ageSeconds={d.ageSeconds}
          tiles={d.tiles}
          apps={d.rankedApps}
          onOpenApps={d.openApps}
          hidden={!shown("overview")}
        />
        <AppsView
          rows={d.rankedApps}
          range={d.range}
          mode={d.mode}
          hidden={!shown("apps")}
        />
        <AnalyticsView
          analytics={d.analytics}
          range={d.range}
          mode={d.mode}
          metricSeries={d.metricSeries}
          hidden={!shown("analytics")}
        />
      </main>
    </>
  );
}
