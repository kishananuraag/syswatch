# SYSWATCH v0.4+ MASTER PLAN — "Monitor Everything, Everywhere"

Updated Aug 24 2026. Fleet-built per PI_FLEET.md. DoD: verified at final destination (dashboard reachable / APK on phone + screenshot).

## Vision
One monitoring ecosystem with a **unified dashboard UI** (desktop + phone):
- **syswatch-core** (Rust): always-on Windows service → JSONL logs (DONE, v0.3)
- **syswatch-dashboard** (Python stdlib web server): live laptop stats + history graphs (DONE v0.4 skeleton)
- **syswatch-apk** (Flutter): Android app — same dashboard UI, monitors phone hardware + battery per app
- **syswatch-web**: deploy the dashboard to a permanent public link (like fintrack-web) for anywhere-access

## UI/UX direction (one design language, both surfaces)
- **Dark-first, single-glance dashboard**: big numbers + sparklines, color = state (green ok / amber watch / red alert)
- **Simplified graphs**: time-axis always labeled (HH:MM, day boundaries), zoom 1h/6h/24h/7d
- **User-readable logs**: raw JSONL → human lines ("14:32 · CPU 61% · Chrome 340MB · Wi-Fi rx 2.1MB/s")
- **Customizable**: user can pin/reorder widgets, set alert thresholds (CPU > 90% for 5min → alert)
- Bottom nav (phone) / sidebar (desktop): Overview | CPU | Memory | Disk | Network | Processes | Logs | Alerts

## Dashboard widget set (MVP → iterate with Kishan feedback)
1. Overview: CPU big-number + sparkline, RAM bar, disk, net rx/tx (live)
2. CPU: per-core bars, freq, top-5 processes by CPU
3. Memory: used/total, swap, top-5 by RAM
4. Disk: per-mount used%, free GB
5. Network: rx/tx bps lines, per-interface
6. Processes: sortable table (CPU/mem), search
7. Logs: readable event stream from JSONL (filter by severity/component)
8. Alerts: threshold rules + history (config file dashboard/config.toml)

## Phases
### P1 — Local dashboard skeleton (DONE v0.4)
- [x] dashboard/server.py stdlib, 0.0.0.0:8787, /api/latest + /api/history
- [x] Canvas charts (no CDN), 1h/6h/24h/7d downsampling
- [x] Auto-refresh 5s, dark theme, top-5 processes
- [ ] Deploy to public link (cloudflared or GitHub Pages pattern)

### P2 — Dashboard hardening (next)
- [ ] Logs viewer (human-readable lines, filter)
- [ ] Per-core CPU bars, disk multi-mount
- [ ] Alerts config (thresholds → visual/telegram ping)
- [ ] Widget pin/reorder (localStorage)
- [ ] Server auto-start (register as sibling service or Task Scheduler)

### P3 — Android app (syswatch-apk, Flutter)
- [ ] flutter create syswatch-apk; shared design tokens
- [ ] battery_plus: level, temp, health, charging state
- [ ] Per-app battery attribution: UsageStatsManager (needs special access — research) OR foreground-usage × drain-rate fallback
- [ ] RAM/storage/network stats (device_info_plus + platform channels)
- [ ] Same dashboard layout: Battery | Hardware | Apps | History
- [ ] APK on Kishan's phone + screenshot proof (DoD)

### P4 — Unify
- [ ] LAN sync phone↔laptop (optional); public dashboard link
- [ ] syswatch-web static deploy so the dashboard is reachable from any device

## Data format (shared)
- JSONL snapshot: {timestamp, uptime_secs, cpu{total,per_core,freq}, memory{used,total,swap}, disks[], networks[], processes{by_cpu,by_mem}, process_count}
- Android adds: battery{level,temp,health,charging}, apps[{pkg,name,drain_pct}]

## Feedback loop
Kishan reviews local dashboard → tells Ox what to change → fleet iterates → redeploy.
Current dashboard URL (LAN): http://192.168.1.44:8787

## Agent task queue (Pi fleet, one bounded task each, ox-alpha)
T1: logs viewer page + human-readable formatting (acceptance: /api/logs?filter= returns readable lines)
T2: per-core CPU bars + multi-disk (acceptance: dashboard shows 12 cores, all mounts)
T3: alerts config + telegram ping hook (acceptance: threshold triggers once)
T4: flutter create syswatch-apk + battery dashboard (acceptance: flutter build apk succeeds)
T5: per-app battery feasibility doc (acceptance: research doc in docs/)
