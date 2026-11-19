# T16B report — root sampler depth: SMC temperatures, cluster frequencies, menubar truth

Date: 2026-11-19 · Machine: Mac14,15 (Apple M2, macOS 15.8.3 / 24H241) · repo `/Users/barbarian/Documents/projects/syswatch`

**Status: code complete and verified; the privileged half is NOT deployed.** The live
root daemon still runs the old script from `/usr/local/libexec/syswatch/`, so the
live `sample.json` has no `temps`/cluster fields yet. Reaching the final
destination needs one macOS admin prompt (`scripts/deploy_root_sampler.sh`), and
this session has no root path (`sudo` is refused by the harness; the admin dialog
is interactive). Nothing below claims otherwise.

---

## 1. SMC temperature reader — `packaging/launchd/smc_temp.c`

AppleSMC over the IOKit user client, ported protocol from `narugit/smctemp`
(MIT) and key names from `exelban/stats` (MIT). Written as C, not C++.

- **Probe, do not assume.** A static name table supplies the known sensors; the
  helper also enumerates the SMC's own key index (`#KEY` → 1809 keys on this
  machine), keeps every `T*` key whose type it can decode, and caches the result
  (`smc_keys.txt`). Anything unmapped is labelled `Sensor <key>`.
- Type decode covers `flt`, `sp78/sp87/sp96/spb4/spf0`, `fp*`, `ui8/16/32`, `si8/16`.
  An unknown type (`ioft`, seen on `TG0B`/`TR0Z`) yields **no reading** — not 0 °C.
- Range guard −20…120 °C exactly as specified.
- Two profiles, measured on this machine:
  | profile | rows | wall time | who uses it |
  |---|---|---|---|
  | `--fast` (named sensors) | 17 | **0.01 s** | the 5 s loop |
  | `--all` (every decodable key) | 146 | **0.07 s** | once at daemon start → `smc_all.json` |
  | `--discover` (cold, enumerates 1809 keys) | — | **0.40 s** | once per machine, then cached |

  That split is what keeps the daemon inside its budget: a full sweep every 5 s
  would cost ~1.4 % of a core on its own.
- CPU rows carry their SMC key (`CPU performance core 3 (Tp09)`). This die
  publishes 9 `Tp*` keys for 8 cores; the name table is written per chip
  generation, so the key is shown rather than a confidently wrong core index.

Live reading (unprivileged — AppleSMC reads happen to work without root here, but
the daemon is the right home because it has a fixed cadence and no UI thread):

```
{"ok":true,"mode":"fast","source":"AppleSMC (root)","keys":170,"discovered":145,"scanned":1809,"temps":[
 {"key":"TB0T","label":"Battery 1","c":27.20,"type":"flt "},
 {"key":"TB1T","label":"Battery 2","c":27.20,"type":"flt "},
 {"key":"TB2T","label":"Battery 3","c":25.90,"type":"flt "},
 {"key":"TH0T","label":"SSD 4","c":29.30,"type":"flt "}, … ]
 "cpu_avg_c":36.98,"gpu_avg_c":36.00,"battery_c":26.77,"ssd_c":29.30,"emitted":17}
```

Families present on this unit: CPU cores (`Tp01…Tp0r`), GPU (`Tg0f`), battery pack
(`TB0T/TB1T/TB2T`), palm rest (`Ts0P/Ts1P`), SSD (`TH0x/TH0T`). 145 decodable
`T*` keys exist; the rest of the T-namespace is internal rails (`TPD*`, `TRD*`,
`TCM*`) which the full sweep records in `smc_all.json`.

## 2. Sampler — `packaging/launchd/syswatch_power_sampler.sh`

Same 5 s interval (not lowered), same atomically-written `sample.json`. Added:

- per-cycle SMC read (`--split`, 3 lines: temps array / aggregates / meta),
  merged into the snapshot as
  `temps: [{key,label,c}]`, `cpu_avg_c`, `gpu_avg_c`, `battery_c`, `ssd_c`,
  `smc: {ok,mode,keys,discovered,emitted,read_failures}`;
- one full sweep at startup → `smc_all.json` (evidence of every key);
- the awk now parses what powermetrics was already printing:
  `e_cluster {freq_mhz,residency_pct}`, `p_cluster {…}`, `gpu_freq_mhz`,
  `gpu_residency_pct`, `per_cpu [{freq_mhz,residency_pct} × 8]`;
- failure is silent and honest: no helper → no `temps` key at all, plus one line
  in `powermetrics.err.log`. A failed per-cycle read **deletes** the previous
  cycle's readings file, so stale temperatures can never be merged as current
  (tested with a helper that exits 3: `temps present: False`).

Both paths were exercised end-to-end **without root**, by replaying the real
recorded powermetrics text through the real script and the real helper
(`/tmp/sampler_replay.sh`, stub `pm_replay.sh` that cats the recorded sample):

```
with helper:     keys ['ane_w','battery_c','cpu_avg_c','cpu_w','e_cluster','gpu_avg_c',
                       'gpu_freq_mhz','gpu_residency_pct','gpu_w','p_cluster','package_w',
                       'package_w_source','per_cpu','sample_ms','samplers','smc','source',
                       'ssd_c','temps','thermal_pressure','ts']
                 temps rows 17 | e_cluster {'freq_mhz': 1277, 'residency_pct': 78.65}
                 p_cluster {'freq_mhz': 907, 'residency_pct': 54.03}
                 gpu_freq 444 gpu_res 21.2
                 per_cpu [1330/39.31, 1342/34.08, 1305/33.63, 1329/27.26,
                          1730/9.79, 1857/6.49, 1745/3.2, 1898/1.66]
                 smc {'ok': True, 'mode': 'fast', 'keys': 170, 'discovered': 145,
                      'emitted': 17, 'read_failures': 25}
                 package 0.487 | pressure Nominal
without helper:  has temps key: False | has e_cluster: True
                 err log: "…/smc_temp_missing not installed; temperatures omitted"
```

`read_failures: 25` is the name table probing keys this die does not publish —
one cheap IOCTL each, harmless, and visible rather than hidden.

## 3. Menubar — `src/menubar.rs` + new `src/machsample.rs`

**Title (the complaint).** It rendered `⚡ {cpu}% {attributed_w}W` — attributed
watts (~0.1–0.2 W) wearing the machine's clothes. Now, in order of preference:

1. pack rail `|V × A|` while current flows (or on battery),
2. the battery gauge's own whole-system load (`PowerTelemetryData.SystemLoad`) —
   this is the ~4–5 W number a human means, available even at 100 % on AC,
3. the daemon's measured `package_w` (SoC only) as the last resort.

Attributed watts stay in the dropdown, labelled *"floor, not total"*.

```
TITLE  ⚡ 24%  5.1W          (was: ⚡ 24%  0.09W)
BATT   V×A 0.00 W, 100 %, 350 cycles     ← ioreg, cached 20 s
```

**Two real bugs found while doing it:**

1. *The CPU percentage never updated.* `menubar::run` called `snapshot()` every
   tick and **never `refresh()`** — so the title's `{cpu}%` was frozen at the
   priming sample for the life of the process (measured: 32.64785 on four
   consecutive ticks). Fixed.
2. *`scripts/build_app.sh` left two status items.* After its `pkill`, launchd's
   `KeepAlive` restarts the app, and the script's `open -a` then started a
   second copy (two `syswatch` PIDs, two menu-bar icons — reproduced today).
   It now waits for launchd and only falls back to `open`.

**Dropdown, rebuilt iStat-style** (rendered from a recording of the new sampler,
so these are the rows that appear the moment the daemon is deployed):

```
  RAM 70%  ·  486 procs  ·  Nominal  ·  36.6°
  CPU       23%  ·  P 54%  ·  E 79%  ·  3504 MHz
  Clocks    P 907 MHz  ·  E 1277 MHz  ·  GPU 444 MHz
  Cores     1330 · 1342 · 1305 · 1329 · 1730 · 1857 · 1745 · 1898 MHz (CPU0…7)
    ─────────────
  Battery   100 %  ·  81 % health  ·  350 cycles  ·  12.94 V × 0 mA  ·  system load 5.14 W (gauge)  ·  adapter in 5.14 W  ·  on AC
  Power     package 0.49 W  ·  cpu 0.43  ·  gpu 0.06  ·  ane 0.00
  Apps      0.08 W measured over 284 procs (floor, not total)
  Thermal   Nominal  ·  CPU 36.6°  ·  GPU 36.0°  ·  SSD 29.3°  ·  batt 27.0°  ·  max CPU performance core 3 37.4° (Tp09)
  GPU       21% active
    ─────────────
  Top energy
  mediaanalysisd  0.05 W  96%
  … 5 more rows, each a measured watt + CPU%
```

Rules held: every row is a measured value with a unit; a missing value removes
its field instead of printing a dash; a stale root snapshot is withheld
(`ROOT   STALE, 1257 s old — withheld`) rather than shown as live.

`--menubar-print` (new, read-only) prints exactly that title + those rows and
exits — GUI verification without a screenshot. `--headless` untouched and
re-checked:

```
$ ./target/release/syswatch --headless --interval-ms 1000
syswatch: headless collector started (interval 1000 ms, log …/logs)
```

## 4. Dashboard plumbing (code lands on the next dashboard restart)

`dashboard/sensors.py`, `dashboard/server.py`:

- `_thermal` renders every `temps` row (`thermal.smc.<key>`, source
  "AppleSMC via the root daemon"), adds an aggregate row only for a family whose
  per-sensor rows are absent (no duplicate measurements), and says
  *"the SMC reader ran but returned no sane reading"* instead of *needs root*
  when that is what happened.
- `_clocks` gained real `E-cluster` / `P-cluster` frequency + residency and
  per-CPU clocks from the same daemon file.
- `root_sample()` now passes `temps`, `cpu_avg_c/gpu_avg_c/battery_c/ssd_c`,
  `e_cluster/p_cluster`, `per_cpu`, `smc`; `/api/power`'s `elevated` block
  exposes them.
- `grant_root_tier()` compiles `smc_temp.c` with
  `clang -O2 -framework IOKit -framework CoreFoundation` inside the same admin
  prompt that installs the daemon, and reports `smc_helper: true/false`.

Exercised against a synthetic payload (registry only, no server restart):

```
thermal.smc.Tp01 | CPU performance core 1 (Tp01) | 39.5 °C
thermal.smc.Tg0f | GPU 1 [Tg0f]                  | 36.5 °C
thermal.smc.TB0T | Battery 1 [TB0T]              | 28.0 °C
thermal.smc.ssd    | SSD                          | 30.0 °C
thermal.pressure   | Thermal pressure             | Nominal
clocks.e_cluster   | E-cluster frequency          | 1277 MHz
clocks.e_cluster.residency | E-cluster active residency | 78.65 %
clocks.p_cluster   | P-cluster frequency          | 907 MHz
clocks.gpu         | GPU frequency                | 444 MHz
```

**Not live:** `dashboard/server.py` and `dashboard/sensors.py` are only read at
startup, `pkill -f dashboard/server.py` is forbidden, and the dashboard's
launchd agent is not mine to bounce. The live process (pid 7001, port 8123) keeps
serving the old registry until it restarts for another reason.

## 5. Acceptance — real output

```
$ cc -O2 -framework IOKit -framework CoreFoundation -o /tmp/smc_test packaging/launchd/smc_temp.c && /tmp/smc_test | head -c 300
{"ok":true,"mode":"fast","source":"AppleSMC (root)","keys":170,"discovered":145,"scanned":1809,"temps":[{"key":"TB0T","label":"Battery 1","c":27.20,"type":"flt "},{"key":"TB1T","label":"Battery 2","c":27.20,"type":"flt "},{"key":"TB2T","label":"Battery 3","c":25.90,"type":"flt "},{"key":"TH0T","labe

$ sh -n packaging/launchd/syswatch_power_sampler.sh && echo SH_OK
SH_OK

$ python3 -c "…sample.json…"
age_s 5
keys ['ane_w', 'cpu_w', 'gpu_w', 'package_w', 'package_w_source', 'sample_ms', 'samplers', 'source', 'thermal_pressure', 'ts']
   ← the one line that does NOT pass: live daemon not deployed (§ deploy below)

$ grep -n "attributed" src/menubar.rs | head -3
10://!   T6a rendered `⚡ {cpu}% {attributed_w}W`.  `attributed_w` is the sum of the
221:/// `attributed_w` — that is the sum of what we could pin on processes (~0.1 W
323:        if snap.energy.attributed_w > 0.0 {          ← dropdown "Apps" row only

$ bash scripts/build_app.sh >/dev/null 2>&1 && echo APP_REBUILT
APP_REBUILT
```

Same probe against the recorded new-sampler output (what the live file becomes
after deployment):

```
$ SYSWATCH_ROOT_SAMPLE=/tmp/sample_fresh.json ./target/release/syswatch --menubar-print | head -3
TITLE  ⚡ 23%  5.1W
ROOT   fresh, 1.4 s old, 17 temp sensors
BATT   V×A 0.00 W, 100 %, 350 cycles
```

## 6. Efficiency (measured, after the changes)

**Menubar app** — `ps -o time` delta over 90 s on the launchd instance (2 s tick):

```
window_s 90  cpu_s 0.37 -> 1.05
avg_pct_one_core 0.756   (budget < 1.000)
```

Getting there took two measured fixes. The first rewrite (refresh every tick)
cost **1.63 %**; per-tick instrumentation showed why: rebuilding `NSMenu` + 17
`NSMenuItem`s every 2 s made AppKit do layout/validation work *inside the
run-loop sleep*, where my tick timings could not see it. Now one persistent menu
with 32 reusable disabled slots (title set only when the text changed), a light
CPU/RAM refresh each tick (~0.07 ms), a full collector refresh every 30 s
(~80 ms), and ioreg battery every 20 s (~25 ms):

```
tick timings (SYSWATCH_MENUBAR_DEBUG): light tick 4–8 ms, full-refresh tick ~81 ms
```

**Root daemon** — measured with `scripts/measure_daemon_cpu.sh` (sums the daemon
subtree, 0.5 s sampling so short-lived `powermetrics` children are caught):

```
window_s 30.0   samples 44
avg_cpu_pct_one_core 36.639   share powermetrics 36.639
```

`powermetrics -n 1` on this build burns ~37 % of a core continuously — that is
the pre-existing dominant cost the brief names, and the interval was **not**
lowered. The sampler's own overhead (`power_sampler.sh`, measured before the
change) is 0.067 %; the SMC read adds 0.01 s per 5 s cycle ≈ **0.2 %**, taking
the script side to ≈0.27 %. Two honest numbers, not one flattering one.

## 7. What is not done, and why

| item | state | why |
|---|---|---|
| live `sample.json` gains `temps` + cluster fields | **not deployed** | needs one admin prompt; `sudo` is refused in this session and the dialog is interactive |
| live dashboard shows SMC temps | **not live** | `server.py`/`sensors.py` are read at startup; restarting the dashboard is forbidden in this brief |
| `tools/verify_T16B.sh` | **absent** | only verify_t5/t7c/t8/t10/t11 exist on disk; the brief's acceptance block is what was run |

### To finish (one command, one prompt)

```sh
scripts/deploy_root_sampler.sh
```

It preflights unprivileged (compile + `sh -n`), then under one macOS admin dialog:
compiles `smc_temp` → `/usr/local/libexec/syswatch/smc_temp`, installs the new
`power_sampler.sh`, warms the SMC key cache, and `launchctl kickstart -k` the
**existing** `com.barbarian.syswatch.power` job (same label, same plist — no
bootout, no re-registration). `--no-restart` skips the kickstart, in which case
the update lands on the next boot.

The SMC key cache is already pre-seeded at
`~/Library/Application Support/syswatch/root/smc_keys.txt` (145 keys), so the
daemon's first post-deploy cycle costs 10 ms instead of 400 ms.

## 8. Files

New: `packaging/launchd/smc_temp.c`, `src/machsample.rs`,
`scripts/deploy_root_sampler.sh`, `scripts/measure_daemon_cpu.sh`,
`reports/T16B-report.md`.
Modified: `packaging/launchd/syswatch_power_sampler.sh`,
`packaging/launchd/README.md`, `src/menubar.rs`, `src/stats.rs` (new
`refresh_light`), `src/cli.rs` (`--menubar-print`), `src/lib.rs`, `src/main.rs`,
`scripts/build_app.sh`, `dashboard/sensors.py`, `dashboard/server.py`.
Nothing under `tools/` was touched. Nothing committed — that needs your GO.

## LEARNED

Three things worth keeping. (1) A cached *derived* value is not a measurement:
the title's watts were real numbers about the wrong thing, which is worse than a
missing number — the fix was a source ordering, not a formula. (2) Cost that
happens inside the run-loop sleep is invisible to in-tick timers; a process-level
CPU measurement caught a 2× regression that instrumentation said was not there.
(3) Enumerating 1809 SMC keys costs 0.4 s — an order of magnitude more than
reading the 17 sensors that matter, so discovery belongs in a cache, not a loop.
