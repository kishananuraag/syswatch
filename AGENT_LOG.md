
## syswatch

### 2026-08-24 — T3 history graphs done (fleet)
- Pi agent (nemotron-3-super via OpenRouter) implemented ~85%: log loading, serde Deserialize on Snapshot, sparkline bucketing, 2/7 zoom keys, q-to-live. Died mid-task on 429 rate limit (free-tier exhausted for all models).
- Ox finished the delta: real HH:MM axis + day-boundary markers + day legend, 'd' key cycles available dates, log_dirs() scans PROGRAMDATA + LOCALAPPDATA (service vs interactive), median-interval bucketing (data-driven, not config), raw-mode only for interactive TUI (JSON stays pipe-friendly).
- Files: src/ui.rs (render_history + available_dates + median_interval_secs), src/cli.rs (View enum, key handling, raw mode), src/logging.rs (log_dir pub(crate) + log_dirs), src/stats.rs (Deserialize derives).
- Verified: cargo build clean, cargo test ok, PTY smoke test rendered real 24h sparkline (15 buckets) + 7d view, keys h/2/7/q/d work.
- Next: T4 Flutter skeleton (syswatch-android).

### 2026-08-24 — T4 syswatch-android skeleton (Ox, direct)
- Flutter skeleton at projects/syswatch_android: battery_plus dashboard — big % number, state color (green/amber/red), BatteryState stream, session-drain sparkline (CustomPaint, 5s sampling), device_info_plus model/API level.
- Verified: `flutter build apk --debug` succeeded → build/app/outputs/flutter-apk/app-debug.apk (472s Gradle).
- Fixes en route: Developer Mode blocked `flutter pub add` → edited pubspec.yaml manually; gradle-8.12-all download blocked (network) → pinned wrapper distributionUrl to seeded gradle-8.14-bin.
- BLOCKED: push to GitHub — repo kishananuraag/syswatch-android does not exist; needs Kishan to create (or grant gh auth). Local commit 656e342 on master.
- 2026-08-24 ~22:30 IST Ox cron (deepseek-v4-flash, 2 parallel agents): P2 dashboard hardening — T1 logs viewer (/api/logs?filter=&limit=, human-readable lines, Logs tab), T2 per-core CPU bars (12 cores), T3 alerts.py + alerts.json thresholds w/ continuous-hold + 1h cooldown + optional TG ping, /api/alerts + Alerts card. Verified: py_compile OK, live curl of /api/logs + /api/alerts + filter=200. Committed+pushed.

### 2026-08-24 ~23:30 IST — P2 widget pin/reorder (Ox cron, glm-5.2:free worker)
- dashboard/server.py: widget pin toggle + up/down reorder buttons, order+pinned persisted to localStorage ('syswatch_widgets'), pinned-first on load. Client-side only.
- Verified: py_compile OK, live server curl 200 + grep hit. Committed dfdd495, pushed master.
- Note: worker exited 0 after edit without committing (silent-exit pattern) — mediator committed.
