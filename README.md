# syswatch

An iStat Menus–style terminal system monitor written in Rust. Live CPU (total +
per-core), memory/swap, disks, network throughput rates, and top-process tables
in a clean, flicker-free dashboard — or machine-readable JSON for scripting.

## Install / build

```sh
cargo build --release
./target/release/syswatch            # start the dashboard
```

Requires Rust 1.85+ (edition 2024). Tested on Windows; uses only sysinfo, so
macOS/Linux should work too.

## Usage

```sh
syswatch                       # live dashboard
syswatch --json                # one JSON snapshot, then exit (pipe-friendly)
syswatch --hide net --hide disk
syswatch --interval-ms 500 --top-n 8 --bar-width 30
syswatch --temp                # show CPU temperature if a sensor is exposed
syswatch --no-color
```

### Flags

| Flag | Meaning |
|---|---|
| `-i, --interval-ms <MS>` | Refresh interval in milliseconds (default 1000) |
| `-j, --json` | Print one pretty JSON snapshot and exit |
| `-n, --top-n <N>` | Rows per process table (default 5) |
| `--bar-width <W>` | Gauge width in characters (default 20) |
| `--temp` | Show temperature sensor reading when available |
| `--no-color` | Disable ANSI colors |
| `--hide <PANEL>` | Hide panels: `cpu`, `ram`, `disk`, `net`, `proc` (repeatable) |

### Config file

Optional TOML at `~/.config/syswatch/config.toml` (or `$XDG_CONFIG_HOME/syswatch/config.toml`).
See [`config.example.toml`](config.example.toml). Precedence: **CLI flags > config file > defaults**.

```toml
interval_ms = 1000
bar_width   = 20
top_n       = 5
temperature = false

[panels]
cpu  = true
ram  = true
disk = true
net  = true
proc = true
```

### JSON output

`--json` emits the full snapshot with serde: timestamp, uptime, total + per-core
CPU %, frequency, memory/swap bytes and %, disks (mount, fs, used %, free GB),
per-interface network **rates** (bytes/sec since last tick), and two ranked
process tables (`top_by_cpu`, `top_by_mem`) plus total process count.

```sh
syswatch --json | jq '.cpu.total_pct, .top_by_cpu[0].name'
```

## Demo

```
╔════════════════════════════════════════════════════════╗
║ syswatch v0.2.0                                        ║
╠════════════════════════════════════════════════════════╣
║ CPU   █████████░░░░░░░░░  43.7% @ 1.3 GHz              ║
║       c0█████  71 c1█░░░░  11 c2████░  73 ...          ║
║ MEM   ███████████░░░░░░░  55.5%  (8.7 GiB / 15.7 GiB)  ║
║ DSK   C:         ████████░░░░░░░░  41.3%  279G free    ║
║ NET   Wi-Fi        ↓ 84.0 KiB/s  ↑ 0.1 KiB/s           ║
╠════════════════════════════════════════════════════════╣
║  NAME(CPU)              PID   CPU%     MEM             ║
║ syswatch.exe           35440   35.7     21M            ║
╚════════════════════════════════════════════════════════╝
 up 6d 20h · 289 procs · 1000ms refresh · ctrl+c quit
```

Bars are severity-colored (green → yellow → red) when color is enabled.

## Architecture

- `src/main.rs` — entry point, module wiring
- `src/cli.rs` — clap CLI surface + run loop
- `src/config.rs` — defaults ← config file ← CLI overrides
- `src/stats.rs` — sysinfo collection, per-tick network rates, serde types
- `src/ui.rs` — double-buffered frame renderer (single write per tick)
