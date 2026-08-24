# SYSWATCH v0.3+ MASTER PLAN — "All Stats, One Place"

Created Aug 24 2026. Fleet-built per PI_FLEET.md. DoD: verified at final destination (service running / APK on phone + screenshot).

## Vision
One lightweight monitor ecosystem:
- **syswatch-core** (Rust): always-on Windows background service. Samples hardware stats every N secs → rolling daily log files (JSONL), auto-pruned at 30 days.
- **syswatch-tui**: existing terminal UI + new iStat-style history graphs (date/time columns) read from logs.
- **syswatch-android** (Flutter): phone app — battery drain per app, all hardware stats in one place.
- Shared data format: JSONL snapshot schema identical across platforms.

## Phases
### P1 — Windows service (Rust)
1. `--log` mode: append JSONL to %LOCALAPPDATA%/syswatch/logs/YYYY-MM-DD.jsonl, prune >30d
2. Windows service wrapper (`windows-service` crate) + install/uninstall subcommands
3. Config: sample interval (default 5s), retention days
4. Acceptance: service survives reboot, CPU overhead <1%, logs rotate correctly

### P2 — History TUI (Rust)
5. Load today's + range JSONL; sparkline/gauge graphs with date-time axis labels
6. Day/week/month zoom; process snapshots optional
7. Acceptance: renders 24h of 5s samples (<17k rows) without lag

### P3 — Android app (Flutter, separate repo syswatch-android)
8. battery_plus + device_info_plus: level, temp, health, screen-on drain rate
9. Per-app battery attribution via UsageStatsManager (needs special API — research; fallback: foreground usage time × average drain)
10. Dashboard: battery gauge, temp trend, RAM, storage, network; charts with dates (fl_chart)
11. Acceptance: APK on Kishan's phone, launched, screenshot proof

### P4 — Unify
12. Optional LAN sync: phone views laptop stats & vice versa later

## UI/UX direction (both platforms)
- Dark-first, single-glance dashboard: big numbers + sparklines, color = state (green ok / amber watch / red alert)
- Time-axis graphs always labeled (HH:MM, day boundaries marked)
- Android: bottom nav = Battery | Hardware | Apps | History

## Parallel track
FinTrack v1.2 testing runs alongside — do not block each other; agents split by project dir.

## Agent task queue (Pi fleet, one bounded task each)
T1: Rust --log mode + pruning (acceptance: unit test for rotation/prune)
T2: windows-service wrapper + install cmds (acceptance: sc query shows service after install)
T3: history loading + graph rendering in TUI (acceptance: cargo test + manual screenshot) ✅ DONE 2026-08-24 — h=history, 2=24h, 7=7d, d=day cycle, q=live; data-driven bucketing
T4: Flutter skeleton + battery dashboard ✅ DONE 2026-08-24 — projects/syswatch_android, apk --debug built; push blocked on GitHub repo creation
T5: per-app battery research spike (output: feasibility doc in repo docs/)
