// The persistent shell: wordmark, device switcher, the three views, the shared
// time range, and the privacy/live badge. It never navigates — views swap
// underneath so the range and the device stay put.
import type { Range } from "../data/types";
import type { TabName } from "../state/useDashboard";

const RANGES: Range[] = ["1m", "5m", "15m", "1h", "6h", "1d", "7d", "30d", "1y"];

const TABS: Array<{ id: TabName; label: string }> = [
  { id: "overview", label: "Overview" },
  { id: "apps", label: "Apps" },
  { id: "analytics", label: "Analytics" },
];

interface Props {
  tab: TabName;
  onTab: (tab: TabName) => void;
  range: Range;
  onRange: (range: Range) => void;
  deviceMenuOpen: boolean;
  settingsOpen: boolean;
  onSettings: () => void;
  onDevicePill: () => void;
}

export function Topbar({
  tab,
  onTab,
  range,
  onRange,
  deviceMenuOpen,
  settingsOpen,
  onSettings,
  onDevicePill,
}: Props) {
  return (
    <header className="topbar">
      <div className="brand-wrap">
        <span className="wordmark">syswatch</span>
        <button
          className="device-pill"
          id="devicePill"
          data-device-pill=""
          aria-label="Switch device or add a device"
          onClick={onDevicePill}
        >
          <span className="dot"></span>
          <span>This Mac</span>
          <span className="dev-count" id="devCount">
            1 device
          </span>
          <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
            <path
              d="M4 6l4 4 4-4"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.5"
              strokeLinecap="round"
              strokeLinejoin="round"
            />
          </svg>
        </button>
      </div>

      <nav className="tabs" role="tablist" aria-label="screens">
        {TABS.map((t) => (
          <button
            key={t.id}
            className={t.id === tab ? "tab active" : "tab"}
            data-tab={t.id}
            role="tab"
            onClick={() => onTab(t.id)}
          >
            {t.label}
          </button>
        ))}
        <button
          className={settingsOpen ? "iconbtn is-open" : "iconbtn"}
          aria-label="Settings"
          aria-expanded={settingsOpen}
          data-settings-btn=""
          onClick={onSettings}
        >
          <svg viewBox="0 0 16 16" width="16" height="16" aria-hidden="true">
            <circle cx="8" cy="8" r="2.2" fill="none" stroke="currentColor" strokeWidth="1.4" />
            <path
              d="M8 1.5v2M8 12.5v2M1.5 8h2M12.5 8h2M3.4 3.4l1.4 1.4M11.2 11.2l1.4 1.4M12.6 3.4l-1.4 1.4M4.8 11.2l-1.4 1.4"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.4"
              strokeLinecap="round"
            />
          </svg>
        </button>
      </nav>

      <div className="range-wrap">
        <div id="ranges" className="ranges">
          {RANGES.map((r) => (
            <button
              key={r}
              className={r === range ? "range-pill active" : "range-pill"}
              data-range={r}
              aria-pressed={r === range}
              onClick={() => onRange(r)}
            >
              {r}
            </button>
          ))}
        </div>
        <div className="secure">
          <span id="secureText">Private · only your devices can see this</span>
          <span className="live">
            <span className="livedot"></span>live
          </span>
        </div>
      </div>
    </header>
  );
}
