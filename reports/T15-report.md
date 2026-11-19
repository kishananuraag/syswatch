# T15 — permissions panel + root power tier · worker report

Date: 2026-11-19 · machine: M2, macOS 15.8.3 (24H241), dashboard on 127.0.0.1:8123
(launchd `gui/$UID/com.barbarian.syswatch.dashboard`, single process).

**The real grant was NOT run** (it would pop an admin prompt). Everything below
is either a real probe on this machine or a stubbed run of the exact code path.

## What exists now

| piece | file |
|---|---|
| `/api/permissions`, grant/revoke, probes | `dashboard/server.py` (§ permissions, ~line 1030) |
| permissions view + power root panel | `dashboard/static/app.js`, `index.html`, `app.css` |
| root LaunchDaemon template | `packaging/launchd/com.barbarian.syswatch.power.plist` (`__HOME__` ×4) |
| privileged sampler (parses powermetrics → JSON) | `packaging/launchd/syswatch_power_sampler.sh` |
| install notes + root-daemon skip | `scripts/install.sh` (NOTES block, logic unchanged for the 3 agents) |
| template index | `packaging/launchd/README.md` |

## Acceptance (real output)

```
$ python3 -m py_compile dashboard/server.py        # (no output = OK)
$ curl -s http://127.0.0.1:8123/api/permissions | python3 -c "..."
permissions rows: 5
$ grep -c "__HOME__" packaging/launchd/com.barbarian.syswatch.power.plist
4
$ grep -c "administrator privileges" dashboard/server.py
2
$ curl -s http://127.0.0.1:8123/ | grep -ci "permissions"
1
$ grep -rE "store|save" dashboard/server.py | grep -iE "password|credential" && echo "LEAK FOUND"
(exit 1 — no match, no LEAK FOUND)
```

Port hygiene: one listener on 8123 (PID 5494 at the time of writing), one
`dashboard/server.py` process; `/Library/LaunchDaemons/com.barbarian.syswatch.power.plist`
does not exist (the daemon is not installed until Kishan presses Grant).

## Permissions payload (live, granted=false for the root tier)

```
package_power      granted=false required · CPU/GPU package watts
temperatures       granted=false required · die °C, cluster °C, pressure, fans
neural_engine      granted=false required · NE watts + utilisation
assistive_access   granted=true          · "osascript AX read returned 71 processes"
battery_stats      granted=true          · "ioreg AppleSmartBattery parsed"
missing_required: ["package_power"]
```

## Grant / decline / revoke shapes (no prompt fired)

```
POST /api/permissions/grant?id=nonsense                     -> 400 unknown permission id
POST .../revoke?id=package_power  (not installed)            -> 200 {ok:true, action:"already", prompted:false}
POST .../grant?id=battery_stats                             -> 200 {granted:true, action:"none", prompted:false}
POST .../revoke?id=package_power  (no X-Syswatch header)     -> 403 CSRF guard
POST .../grant?id=package_power with a fresh sample file
     and _daemon_loaded()==true (stubbed)                   -> 200 {action:"already", prompted:false}, 0 subprocesses
```

The exact command the install path produces (stubbed `_run`, printed, not run):

```
do shell script "/bin/mkdir -p /usr/local/libexec/syswatch /Library/LaunchDaemons; \
/usr/bin/install -m 755 -o root -g wheel <repo>/packaging/launchd/syswatch_power_sampler.sh \
  /usr/local/libexec/syswatch/power_sampler.sh; \
/usr/bin/install -m 644 -o root -g wheel <staged plist> \
  /Library/LaunchDaemons/com.barbarian.syswatch.power.plist; \
/usr/bin/touch ~/Library/Logs/syswatch/power-daemon.log; /bin/chmod 644 ...; \
/bin/launchctl bootout system/com.barbarian.syswatch.power >/dev/null 2>&1 || true; \
/bin/launchctl bootstrap system /Library/LaunchDaemons/com.barbarian.syswatch.power.plist" \
with administrator privileges
```

## Staleness / honesty proof

Synthetic `sample.json` (fresh) → `power_state()`:
`package_w 1.06, cpu_w 0.81, gpu_w 0.24, ane_w 0.01, cpu_temp_c 47.31,
gpu_temp_c 39.87, thermal_pressure Nominal`.

Same file aged to 45 s → `available:false`,
reason `"needs root — last privileged sample is 45 s old (> 30 s), so its values
are withheld rather than shown as live"`, `package_w: null`, thermal sensor
group collapses back to one amber `needs_root` row.

Parser (awk) on a synthetic powermetrics block:
`CPU Power: 812 mW → cpu_w 0.812`, `Combined Power (CPU + GPU + ANE): 1063 mW →
package_w 1.063`, `CPU die temperature: 47.31 C → cpu_temp_c 47.31`,
`P-Cluster die temperature` → `cpu_cluster_temps_c[{label,c}]`,
`Current pressure level: Nominal`, `Fan: 1203 rpm`. Absent fields are **omitted**
from the JSON (verified with an Intel-style block: no temp/fan keys at all).

Aggregates: with a fake 60-sample ring, the analytics measure rows become
`package_w avg 10.0 max 11.0 p95 11.0 (60 samples, source "root powermetrics 5 s
ring")`, same for `cpu_w` / `gpu_w`. Ungranted they say
`needs root — no privileged powermetrics sample in this window`.

## Corrections to the brief (verified, not guessed)

1. `powermetrics` is at **`/usr/bin/powermetrics`**, not `/usr/sbin` (nothing at
   /usr/sbin on this build).
2. The brief's `-f <file> -i 5` is wrong on macOS 15.8.3: `-f | --format` is
   `text|plist`; the file flag is `-o | --output-file`; `-i N` is **milliseconds**
   (default 5000); one sample is `-n 1`. The daemon therefore runs
   `powermetrics -n 1 -i 5000 -f text -s thermal,cpu_power,gpu_power,ane_power`.
3. `smc` is **not** in `powermetrics --help` on this build (the man page still
   documents it), so the sampler probes it once at startup and drops it if
   rejected — no hard failure, no silent temperature loss.
4. `packaging/launchd/install.sh` does not exist in this repo (T8's path is
   `scripts/install.sh`). The NOTES block went there and in
   `packaging/launchd/README.md`. The only functional change to the T8 path is
   skipping `com.barbarian.syswatch.power.plist` in the LaunchAgents glob —
   without that, `scripts/install.sh` would bootstrap a root daemon as a user
   agent and powermetrics would fail.

## Not verified (cannot be, without the admin prompt)

- **No real powermetrics sample exists on this machine yet.** `powermetrics`
  refuses: `powermetrics must be invoked as the superuser`. So the exact field
  names on this build are unproven — this is why the sampler dumps its first
  real sample to `~/Library/Application Support/syswatch/root/powermetrics.raw.log`
  and the JSON carries `package_w_source`. If a label differs (e.g. different
  die-temperature wording), the value appears as "field missing" in the UI, not
  as 0 — and the raw dump makes the fix a one-line regex change.
- The admin prompt / decline / revoke paths were exercised only with `_run`
  stubbed. Kishan's real grant is the first true test.

## LEARNED

Everything privileged here is a **file contract**: root writes, the web server
only stats+reads, and freshness (30 s) is the security boundary that keeps a
dead daemon from looking alive. The permission model then falls out of that one
contract — three root features share one grant because they share one file, and
the panel's job is to say that plainly instead of hiding a "needs root" button.
Second lesson: `--samplers smc` is documented but not supported on 15.8.3, so a
capability probe at daemon start beats trusting either the man page or the brief.
