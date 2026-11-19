# packaging/launchd — plist templates

| template | domain | installed by | needs root |
|---|---|---|---|
| `com.barbarian.syswatch.plist` | `gui/$UID` LaunchAgent | `scripts/install.sh` | no |
| `com.barbarian.syswatch.collector.plist` | `gui/$UID` LaunchAgent | `scripts/install.sh` | no |
| `com.barbarian.syswatch.dashboard.plist` | `gui/$UID` LaunchAgent | `scripts/install.sh` | no |
| `com.barbarian.syswatch.power.plist` | **`system` LaunchDaemon** | **dashboard grant flow only** | **yes — one macOS admin prompt** |

All templates use `__HOME__`, never a machine-specific absolute path.
`scripts/install.sh` renders `__HOME__` with `sed` and skips the power daemon on
purpose (it is a system-domain daemon; bootstrapping it as a user agent would
make `powermetrics` fail with "must be invoked as the superuser").

## The root tier (T15 → T19)

- UI: `http://127.0.0.1:8123/#permissions` → **Grant** on *Package power*
- API: `POST /api/permissions/grant?id=package_power`
  (uninstall: `POST /api/permissions/revoke?id=package_power`)
- The dashboard never runs as root and never stores a password. `osascript`
  runs `do shell script ... with administrator privileges`; macOS handles the
  credential and hands it to `launchctl`, not to syswatch.
- **T19: the daemon is ONE Rust binary**, `syswatch-powerd` (source
  `src/bin/powerd.rs`, installed to
  `/usr/local/libexec/syswatch/syswatch-powerd`). It loops
  `powermetrics -n 1 -i 5000 -f text -s …`, parses the text in-process, reads
  the AppleSMC through IOKit on a connection it keeps open, and writes one JSON
  snapshot per sample, atomically, to
  `~/Library/Application Support/syswatch/root/sample.json`.
  The first real sample is dumped to `powermetrics.raw.log` as evidence of the
  exact field names on this macOS build; sampler-probe results and failures are
  appended to `powermetrics.err.log`.
- The binary is **built before** the prompt (`cargo build --release --bin
  syswatch-powerd`, as the user). The admin prompt only copies a file and talks
  to `launchctl` — a compiler never runs as root.
- A snapshot older than 30 s is treated as **absent** by the dashboard: the
  power page says *needs root* instead of showing a stale number.

## Temperatures — AppleSMC, not powermetrics

Verified on macOS 15.8.3 / 24H241 (Apple Silicon): `powermetrics -s thermal`
prints **only** the pressure level, and `-s smc` is rejected outright
(`unrecognized sampler: smc`). Die temperatures are not reachable through
powermetrics on this platform.

They are reachable through the AppleSMC IOKit user client — the mechanism iStat
Menus and [`smctemp`](https://github.com/narugit/smctemp) use. T19 folded that
reader into the daemon binary; the retired standalone sources are kept for
provenance:

| file | role |
|---|---|
| `src/bin/powerd.rs` → `mod smc` | **live** — IOKit FFI + key table + probe logic |
| `smc_temp.c` | RETIRED (T19). Original C reader, MIT-port of smctemp's protocol + exelban/stats key names; the Rust module is a line-for-line port of its decoding, key inventory and probe order |
| `syswatch_power_sampler.sh` | RETIRED (T19). Original shell loop; `src/bin/powerd.rs` ports its powermetrics flags, `awk` parser and sampler probe |

- **Probe, don't assume:** the reader enumerates the SMC's own key index
  (`#KEY` + per-index keys) and keeps every `T*` key whose type it can decode
  and whose value lands in −20…120 °C. Results are cached in
  `<root dir>/smc_keys.txt` (enumerating ~1800 keys costs ~0.4 s; the loop runs
  every 5 s).
- Labels come from a small table (CPU core / GPU / battery / palm rest / SSD);
  anything unmapped is `Sensor <key>`. CPU rows carry their SMC key, because a
  name table is written for a chip generation and a variant die publishes
  different keys.
- Measured cost on Mac14,15: full sweep (131 rows) ≈ 75 ms, named set (17 rows)
  ≈ 10 ms. The daemon reads the **named set** each 5 s cycle and dumps the full
  sweep once at startup to `smc_all.json`.
- A key that fails to read, decodes to an unknown type, or lands outside the
  band is simply **absent** from `temps` — never 0 °C.
- Redeploy: `scripts/deploy_root_sampler.sh` (builds, self-checks, then one
  admin prompt; `bootout` + `bootstrap` the existing job — the plist's
  `ProgramArguments` changed in T19, so `kickstart -k` alone is not enough;
  `--no-restart` skips the reload).

`scripts/install.sh` carries the same install snippet in its NOTES block so the
privileged step can be audited without reading `dashboard/server.py`.
