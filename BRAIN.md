# syswatch — BRAIN

*Hermes reads this before any syswatch work. Updated after every touch.*

## What it is
Rust system resource watcher. Tray app + JSON API server. Runs on Windows.

## Current state (2026-09-03)
- Branch: master
- Last commit: `63505d3` B11: PID-lock singleton guard + 5MB history read cap + design concepts (centered/grouped/bento)
- Uncommitted: 2 files
- Binary: `target/release/syswatch-tray.exe` (built, was running PID 134652 as of 9/3)
- Dashboard: web UI on localhost:port (see README.md)

## What works
- Singleton PID-lock (no duplicate instances)
- 5MB cap on history reads
- Processes panel + CPU fix
- Web dashboard v1 (Aug 25)
- 6/6 dashboard history ranges verified

## What's broken / pending
- 2 uncommitted files — review before next push
- Tray app long-term stability unknown (was running fine at last check)

## How to build
- Windows: `cargo build --release`
- Run: `target/release/syswatch-tray.exe`
- **Done = binary running + JSON output inspected + (if UI) browser opened**

## Known pitfalls
- Tray icon hover/menu can crash on Windows version mismatch
- `proc_list` Windows API sometimes returns empty on first call after sleep
- History file can grow > 5MB without the cap — fixed in B11

## Where things live
- Source: `src/`
- Tray: `tray/`
- Dashboard: `dashboard/`
- History CSV: `after_*.csv` (test artifacts, do not commit)

## Last touched
2026-08-25. Hermes deployed+verified tray app 9/3 (~12.8 MB RAM, working).
