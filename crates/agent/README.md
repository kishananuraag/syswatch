# syswatch-agent (T24)

The SysWatch HTTP agent in Rust — `axum` + `tokio`. It serves the dashboard API
from the same sources `dashboard/server.py` reads, and serves the dashboard's own
static files.

**Status: skeleton.** Two endpoints are migrated. `dashboard/server.py` stays the
live server on `:8123`; this binary runs beside it on a test port.

## Run

```bash
cd crates/agent
cargo build --release
./target/release/syswatch-agent --port 8199      # never start on 8123 beside Python
curl -s http://127.0.0.1:8199/api/health
curl -s http://127.0.0.1:8199/api/current
```

| flag | default | meaning |
|---|---|---|
| `-p, --port <N>` | `8123` | listen port |
| `--host <H>` | `127.0.0.1` | bind address (loopback only, like the Python server) |
| `--static-dir <D>` | `<repo>/dashboard/static` | UI files |
| `--logs-dir <D>` | `~/Library/Application Support/syswatch/logs` | collector JSONL |
| `--tick-ms <N>` | `1000` | sampler cadence |

`SYSWATCH_LOGS` is honoured for the log dir, same as the Python server.
Startup prints `SYSWATCH_AGENT_READY on port N`; SIGTERM/Ctrl-C shut down
cleanly and release the port.

## Routes

| route | status |
|---|---|
| `GET /api/health` | **migrated** — `{"ok","pid","uptime_s","port"}`, identical shape and key order to the Python route |
| `GET /api/current` | **migrated** — the Python blocks plus `power`, `collector`, `agent` |
| `GET /`, `/analytics` | `dashboard/static/index.html` |
| `GET /static/*` | `dashboard/static/*` |
| `GET /api/<other>` | JSON 404 naming what is implemented (Python still serves it) |

## Where the numbers come from

| block | source |
|---|---|
| `cpu`, `ram`, `net` | `sysinfo` in-process, deltas over the tick |
| `cpu.clusters` | `hw.perflevel*.logicalcpu` (integer sysctls) + the same per-core means the Python route uses |
| `load` | `getloadavg(3)` |
| `disk` (+ `disk.volumes`) | `sysinfo::Disks` (statfs), `/System/Volumes/Data` preferred |
| `disk_io` | the same IOKit byte counters, differentiated over a 5 s window |
| `gpu` | `ioreg -r -d 1 -w0 -c IOAccelerator`, 5 s cache |
| `battery` | `syswatch_sensors_macos::battery` (IORegistry, 20 s cache) |
| `temps_c`, `power` | the root daemon's `sample.json` via `rootsample::read()` |
| `pressure.memory` | `kern.memorystatus_vm_pressure_level` |
| `processes` | `sysinfo` process scan, 60 s cadence (Python's `PROC_SCAN_SECS`) |
| `collector` | last JSON line of the newest collector log, 1 MB tail, 15 s cache |

A value that could not be read is `null` with a reason in `unavailable` — never
`0`. The sampler runs on its own OS thread, so no request ever waits on a
sample: `/api/current` answers in ~0.5 ms from the cached snapshot.

## Differences from the Python payload (deliberate)

- `ram.used_bytes` and `ram.pct` use one basis here (pct = used/total) and
  `ram.available_bytes` names the other. psutil's payload mixes two: 3.7 GB
  "used" beside 78 % (which is total − available).
- `disk.used_pct` is `(total − available) / total`, consistent with the
  `free_gb` beside it; psutil's percent excludes reserved blocks and reads a
  few points lower. `disk.used_pct_basis` states this in the payload.
- `disk_io.read_iops` / `write_iops` are `null` (sysinfo exposes byte counters
  only); `iops_reason` says so.
- `temps_c` is populated on this machine (the root daemon reads the SMC) where
  the unprivileged psutil path returns `[]`.
- `power` (package/cpu/gpu watts + thermal pressure from `powermetrics`) is an
  extra block — the Python `/api/current` carries these only on `/api/power`.
- `boot_ts` is the machine's real boot instant (`sysinfo`). The Python server's
  `BOOT_TS` is its own process start despite the key name; nothing in `app.js`
  reads it, so the agent reports the truth and says so in
  `agent.boot_ts_source`.
- `disk.volumes` lists the volumes `sysinfo` reports (2 on this Mac: `/` and
  `/System/Volumes/Data`); the Python server's psutil walk also lists VM,
  Preboot, Update and Recovery.

## Tests

```bash
cargo test        # 30 tests: cli, timefmt, collector tail reader, gpu parser,
                  # sampler snapshot shape, router responses
```
