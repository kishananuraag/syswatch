# SYSWATCH DASHBOARD DESIGN.md — v1 (from Kishan's audio feedback, Aug 24 2026)

STATUS: DESIGN ITERATION — NOT approved. No build until Kishan approves.

## Reference target: iStat Menus (Mac menu bar app) — Kishan's stated model
Menu bar shows: CPU, GPU, network speeds (up/down), RAM. Click → detailed panel per section.
Hover CPU → all CPU details. Hover GPU → all GPU details. Efficiency/performance cores shown.
Sensors section: EVERY sensor temperature — individual CPU cores, GPU/graphics cores, memory,
SSD, Wi-Fi, battery. Power: consumed watts, voltage, amperage, frequency per component.
Battery: percentage, current draw, power usage. Per-app: which apps consume what.

## Kishan's feedback → requirements

### 1. BUG: tiles show but NO DATA
- Voice: "tiles are sorted out, but there is no data showing up."
- REQUIREMENT: investigate data flow — dashboard fetches /api/latest + /api/history; if empty,
  show loading state then data; if API fails, show error with retry. NEVER a blank tile.
- Check: server reads C:/ProgramData/syswatch/logs — verify file access + parse (28K+ samples exist).

### 2. Logs column: nothing happens on click
- Voice: "logs are the separate column, nothing happening."
- REQUIREMENT: Logs panel must open/filter. If genuinely no logs (filtered), show "No logs in range"
  with hint. Verify /api/logs endpoint returns the human-readable lines (P2 shipped it — test).

### 3. Time-range selector beside CPU total (MUST)
- Voice: "right beside CPU total I should be able to select the time frame: 10 min, 15 min,
  1 hour, 2-5 days, 7 days."
- REQUIREMENT: Global time-range pills next to the CPU header: 10m | 15m | 30m | 1h | 3h | 6h | 12h | 1d | 3d | 7d.
  Selected range drives ALL charts on the page (CPU, RAM, network, temp).

### 4. Temperatures: smooth beautiful graphs + ALL sensors
- Voice: "make temperature beautiful, smooth leveled graphs, not pointy/spiky. Show all temperatures."
- REQUIREMENT:
  - Temperature tile: smooth line graph (cubic/bezier smoothing, not raw point-to-point)
  - Show ALL available sensors: CPU package, per-core, GPU, memory, SSD, Wi-Fi, battery
  - Sensor list auto-detected from the machine (Windows: LibreHardwareMonitor-style WMI/CPUID access)
- Windows reality check: getting per-sensor temps on Windows requires LibreHardwareMonitor
  (open-source, needs its service/process) or WMI (limited: often only CPU package on laptops).
  PLAN: integrate LibreHardwareMonitor (fire-and-forget process exposing WMI/JSON) as sensor backend;
  fallback = whatever sysinfo exposes. Document per-machine sensor availability.

### 5. iStat-style navigation (the big one)
- Voice: "like iStat menu bar — CPU, GPU, network up/down, RAM; click each for details."
- REQUIREMENT: restructure dashboard into section-driven layout:
  - Top nav / sidebar: **Overview | CPU | GPU | Network | RAM | Sensors | Battery | Apps**
  - Each section = its own focused panel with charts + details (like iStat's click-per-section)
  - CPU section: total + per-core graphs, frequency, efficiency vs performance cores (Windows: P/E cores)
  - Network: up/down dual-line graph (rx/tx), per-interface
  - RAM: usage history + swap + top memory apps
  - GPU: utilization, VRAM, temp (Windows: needs GPU sensors — LibreHardwareMonitor covers NVIDIA/AMD/Intel)
- Keep the single-glance overview as landing, sections for depth.

### 6. Power/battery section
- Voice: "battery percentage, how much power it's taking, watts, voltage, ampere; CPU power consumption."
- REQUIREMENT: Sensors/Battery section shows: battery %, charge/discharge rate (W), voltage,
  amperage, frequency; CPU power draw (W). Source: LibreHardwareMonitor + Windows battery WMI.

### 7. Per-app usage tile
- Voice: "time bar below the section showing what apps are using how much."
- REQUIREMENT: Apps tile under each relevant section (CPU/RAM/Network): live per-process usage
  (cpu %, mem MB, rx/tx if attributable) with a small time bar of top offenders. Process table
  already exists (top-5) — extend to per-section context + history sparkline.

## Design decisions
- Smooth charts: draw with bezier curves (quadratic/cubic) between points instead of straight lines — this
  is the "not pointy" requirement. Implement in canvas renderer.
- Time-range pills are GLOBAL state; all charts re-read the selected range from the same source.
- Sensor availability varies by machine (laptop vs desktop) — UI shows only sensors actually detected,
  never empty rows.
- Data source hierarchy: LibreHardwareMonitor (rich sensors) > sysinfo crate (CPU/RAM/base) > WMI fallback.
  LHM runs as a sidecar process launched with the dashboard (or as part of syswatch service).
- Per-machine sensor map saved to dashboard/sensors.json so UI knows what exists.

## Verification plan
- [ ] Dashboard shows data on load (no blank tiles) — curl /api/latest, /api/history
- [ ] Time-range pills change all charts (visual check + API range param)
- [ ] Smooth curves render (no spikes) — screenshot check
- [ ] Sensor list populated on this laptop (what does LHM actually see here?)
- [ ] Logs panel returns readable lines

## Awaiting Kishan approval
- [ ] Approve v1 (iStat-style sections + time pills + smooth temp graphs + power/battery + per-app)
- [ ] Request changes
