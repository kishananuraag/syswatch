# syswatch

A lightweight terminal system monitor built in Rust. Displays live CPU, RAM, disk, and network stats with a clean dashboard UI.

## Demo

```
╔══════════════════════════════════════╗
║             syswatch                 ║
╠══════════════════════════════════════╣
║ CPU  [████░░░░░░░░░░░░░░░░░░]  19.3%
║ RAM  [██████████████░░░░░░░░]  70.4%  (11305 / 16055 MB)
╠══════════════════════════════════════╣
║ DSK  [██████░░░░░░░░░░░░░░░░]  28.9%  (338 GB free)
╠══════════════════════════════════════╣
║ NET  Wi-Fi      ↓     5 KB  ↑     8 KB
╠══════════════════════════════════════╣
║  interval: 1s  [ctrl+c to quit]      ║
╚══════════════════════════════════════╝
```

## Install

```bash
cargo install --git https://github.com/kishananuraag/syswatch
```

Or clone and build locally:

```bash
git clone https://github.com/kishananuraag/syswatch
cd syswatch
cargo install --path .
```

## Usage

```bash
# Live dashboard (refreshes every second)
syswatch

# Custom refresh interval
syswatch --interval 2

# Output as JSON — great for piping to other tools
syswatch --json
```

### JSON output

```json
{
  "cpu_pct": 33.4,
  "ram_used_mb": 13734,
  "ram_total_mb": 16055,
  "disks": [{"free_gb": 324, "used_pct": 31.8}],
  "networks": [{"name": "Wi-Fi", "rx_kb": 5, "tx_kb": 8}]
}
```

## Built with

- [`sysinfo`](https://crates.io/crates/sysinfo) — cross-platform system stats
- [`crossterm`](https://crates.io/crates/crossterm) — terminal control
- [`clap`](https://crates.io/crates/clap) — CLI argument parsing
