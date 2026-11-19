# Energy model — who spends it, and where it goes

*Design note for T7C. Status: PLAN. Nothing here is measured yet on this machine; every claim carries the command that must confirm it before it goes into code.*

Two different questions, and most tools answer only one:

| Question | Answer lives in | Stored by iStat? |
|---|---|---|
| **Who** is spending energy? | per-process energy counters | live UI only, **never stored** |
| **Where** is it being spent? | package/CPU/GPU/ANE/DRAM/display rails | yes, machine-level only |

syswatch answers both, in the same tick, and reconciles them so "where" always accounts for "who".

---

## 1. Sources (verify before trusting)

### 1.1 Per-process — `proc_pid_rusage` (no root, same-user PIDs)
```c
struct rusage_info_v6 ri;
proc_pid_rusage(pid, RUSAGE_INFO_V6, (rusage_info_t *)&ri);
```
Fields we intend to use (**verify offsets from the SDK header, never from this doc**):
`ri_energy_nj` (cumulative nanojoules), `ri_gpu_time` (ns GPU time), `ri_user_time`, `ri_system_time`, `ri_interrupt_wkups`, `ri_diskio_bytesread/written`.

```bash
xcrun --show-sdk-path
grep -n "rusage_info_v6" -A 45 "$(xcrun --show-sdk-path)/usr/include/libproc.h"
```
**Probe to settle before building:** read one PID twice 5 s apart and confirm `ri_energy_nj` actually advances and is not zero for ordinary apps on this OS build (macOS 15.7.9). If it is zero, `RUSAGE_INFO_V4`, then `powermetrics --show-process-energy` are the fallbacks, in that order. The chosen source is recorded in the JSON as `rusage_version` — a reader must never have to guess which one produced the number.

Note: Activity Monitor's "Energy Impact" is **not** raw nanojoules — it is a weighted composite of CPU, wakeups, GPU and I/O. We report watts (physical) and, separately, `energy_impact_index` only if we can reproduce its weights honestly. Otherwise the index is omitted. One number that means something beats two that don't.

### 1.2 Machine — subsystem rails
| Rail | Source | Root? |
|---|---|---|
| CPU / GPU / ANE / DRAM power | `powermetrics -n 1 --samplers cpu_power,gpu_power,ane_power` | **yes** |
| IOReport "Energy Model" counters | `IOReportCopyChannelsInGroup("Energy Model")` via IOKit | no (private API, may change) |
| Battery V / A / W, on-AC | `ioreg -rn AppleSmartBattery` | no |
| CPU package watts | SMC keys via AppleSMC (`PSTR`/`PC0C` family) | usually yes |

Policy: **one** `powermetrics` call at startup, cached for the session, and only when the app was launched with the privilege to do it. Never per tick — a per-tick root subprocess is exactly iStat's cost profile and exactly what we are beating.
If no rail source is available, the output says `"attribution": "processes_only"` and the residual is **omitted**, never invented.

---

## 2. The arithmetic

Per tick (default 1 s, floor 1 s):

```
ΔE_i     = ri_energy_nj_i − ri_energy_nj_i(prev)      # nanojoules
W_i      = ΔE_i / 1e9 / Δt                            # watts
share_i  = 100 * W_i / Σ W                            # % of attributed
```

Machine reconciliation, only when a rail source exists:

```
attributed_w   = Σ W_i
unaccounted_w  = package_w − attributed_w
over_attribution_pct = 100 * max(0, -unaccounted_w) / package_w
```

Three legitimate states, all reported as they are:
- `unaccounted_w > 0` → energy spent by things we cannot see (kernel, root daemons, other users).
- `unaccounted_w < 0` → over-attribution (counters overlap or double-count GPU time). Reported as `over_attribution_pct`, **not clamped to zero**.
- No rail → `attribution: processes_only`.

Units are stated in the payload and the UI: **J for the period, W for the instant**. Never a bare "energy" number.

---

## 3. Where the energy goes — the second axis

Per tick, alongside "who":
```
by_rail: { cpu: W, gpu: W, ane: W, dram: W, disk: W, network: W, display: W }
```
Only rails the platform actually reports are present; missing rails are **absent keys**, not zeros. The UI then shows two bars that must line up: "top apps" (who) and "rails" (where), with the residual labelled `unattributed` in grey — visible, never hidden.

That residual is the honest part of the product. Every energy monitor that shows a pie chart summing to 100 % is lying about something.

---

## 4. Storage (this is where naive designs explode)

400 PIDs × 1 sample/s = **34.5 M rows/day**. That is how a monitor becomes the problem.

Rules:
1. Per-process samples are kept only for the **top 15 by watts** per tick, plus one aggregate row `rest`.
   `15 × 86,400/s ≈ 1.3 M rows/day` raw — still too much, so:
2. **Cascade ladder** (borrowed from iStat's own trick, 87 days of history in 151 MB): raw 1 s for 15 min → 15 s for 6 h → 60 s for 24 h → 300 s for 7 d → 1800 s for 30 d. Each rung stores `max` and `mean` per process, so a 3-second spike at 10:04 is still visible a week later.
3. Retention cap: `per-process` 30 days, `rails` 90 days, hard size cap 200 MB with oldest-rung eviction and a log line when it fires.
4. Snapshot shape is stable from day one — if a field is missing on this OS, it is absent, and the writer never writes a fabricated 0.

---

## 5. What breaks, and how we'll know

| Failure | Detection | Behaviour |
|---|---|---|
| `ri_energy_nj` always 0 | startup probe (two reads, 5 s apart) | fall back to V4 → then `powermetrics --show-process-energy` → then `attribution: unavailable` **with a reason string** |
| PID recycled between ticks | compare `ri_proc_start_abstime` (`rusage_info_v6`) | treat as new process, deltas `null` |
| Counter resets (process exec) | `ΔE < 0` | emit `null` for that tick, count it in `counter_resets` |
| Root-owned PIDs unreadable | `EPERM` | `perm: false`, counted in `unreadable_pids`, **never 0 W** |
| Machine sleeps mid-window | `Δt` >> sampling interval | discard the window, log `gap_s`, never annualise a stale delta |

---

## 6. How T7C gets judged

```bash
./target/release/syswatch --json | python3 -c "
import json,sys; d=json.load(sys.stdin)
e=d['energy']; print('attribution', e['attribution'], 'rusage', e['rusage_version'])
print('attributed_w', e.get('attributed_w'), 'package_w', e.get('package_w'), 'unaccounted', e.get('unaccounted_w'))
print('unreadable', e.get('unreadable_pids'), 'resets', e.get('counter_resets'))"
```
Plus, once, a human-comparable check: `sudo powermetrics -n 1 -i 1000 --samplers cpu_power,gpu_power` next to our rail numbers, in the same minute, with both outputs pasted. A parity row that says `PASS` with no such paste is a failure of the process, not of the code.

Target: attributed watts within **±30 %** of the package rail during a quiet minute, with the residual always printed alongside. Tighter than that is not credible on a machine with root daemons.

---

## 7. What this buys over iStat

- Their history DB has **no process dimension at all** — ask "which app drained the battery at 10:04 last Tuesday" and it cannot answer, ever, because the data was discarded at sample time.
- Ours can, from stored data, on a machine with a 151 MB reference DB sitting right there for comparison.
- Cost discipline: no per-tick root subprocess, one in-process pass, ring buffers, cascade ladder, measured against A5 (`< 1 % CPU`, `< 15 MB RSS`).
