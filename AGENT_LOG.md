
## syswatch

### 2026-08-24 — T3 history graphs done (fleet)
- Pi agent (nemotron-3-super via OpenRouter) implemented ~85%: log loading, serde Deserialize on Snapshot, sparkline bucketing, 2/7 zoom keys, q-to-live. Died mid-task on 429 rate limit (free-tier exhausted for all models).
- Ox finished the delta: real HH:MM axis + day-boundary markers + day legend, 'd' key cycles available dates, log_dirs() scans PROGRAMDATA + LOCALAPPDATA (service vs interactive), median-interval bucketing (data-driven, not config), raw-mode only for interactive TUI (JSON stays pipe-friendly).
- Files: src/ui.rs (render_history + available_dates + median_interval_secs), src/cli.rs (View enum, key handling, raw mode), src/logging.rs (log_dir pub(crate) + log_dirs), src/stats.rs (Deserialize derives).
- Verified: cargo build clean, cargo test ok, PTY smoke test rendered real 24h sparkline (15 buckets) + 7d view, keys h/2/7/q/d work.
- Next: T4 Flutter skeleton (syswatch-android).
