# syswatch BUILD_LOG — v0.1 → v0.2

Date: 2026-08-23 · Toolchain: cargo 1.94.1 (rustc same) on Windows 11

## What changed

### 1. Per-process tables (new)
Live top-N processes by CPU and by memory, side by side, refreshed each tick.
Uses `sysinfo::System::processes()`; names truncated to 22 chars for alignment.

### 2. Module restructure
`main.rs` (135 lines of everything) split into:
- `cli.rs` — clap arg parsing + run loop
- `config.rs` — layered config (defaults ← TOML ← CLI)
- `stats.rs` — collection + serde types (`Snapshot`, `Collector`)
- `ui.rs` — rendering only

### 3. Customization
New flags: `--interval-ms`, `--top-n`, `--bar-width`, `--temp`, `--no-color`,
`--hide cpu|ram|disk|net|proc` (repeatable). Optional TOML config at
`~/.config/syswatch/config.toml` (see `config.example.toml`). Bad configs are
warned about and ignored rather than crashing.

### 4. Clean rendering
Double-buffered: the whole frame is built as one String and written in a single
`write_all` per tick; subsequent frames move the cursor home with `\x1b[H\x1b[J`
instead of clearing the whole screen. Cursor is hidden on frame 1 and restored
on exit. Box width is computed dynamically from content, so per-core rows and
the process tables no longer overflow the border (a v0.2-dev bug we hit and
fixed). Severity coloring: bars/pcts go green → yellow → red at 60/85%.

### 5. JSON via serde
`--json` now emits pretty serde JSON with processes, per-core CPU, swap,
per-interface network *rates* (bytes/sec computed from counter deltas between
ticks — v0.1 printed lifetime totals), temperature when available, uptime, and
timestamp (UTC, no chrono dependency).

## Verified on this machine
- `cargo build --release` passes clean (no warnings).
- `syswatch --json` produced a valid snapshot (12 cores, C:\ NTFS 41.3% used,
  Wi-Fi rates, real process tables).
- Dashboard ran ~4s headless with `--no-color --top-n 3`; frames render aligned
  at full width with correct borders.

## Gotchas hit
- sysinfo 0.33: `Component::temperature()` returns `Option<f32>` → use
  `filter_map`; `System::uptime()` is an associated function.
- Windows keeps the exe locked while running — kill syswatch.exe before
  rebuilding.
- First CPU sample after startup reads ~100%; the collector primes one sample
  before the loop so tick 1 is sane-ish (still warm-up noisy).

## Try it

```sh
cargo run --release -- --top-n 8 --bar-width 30
cargo run --release -- --json | jq .cpu.total_pct
```
