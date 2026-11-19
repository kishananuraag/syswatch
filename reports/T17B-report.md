# T17B report — Apps view · Knowledge page · Power & battery analytics

Date: 2026-11-19 · repo `/Users/barbarian/Documents/projects/syswatch` · machine Mac14,15 (Apple M2)

**Status: code complete, verified live.** All three surfaces are built and were
exercised in a real headless Chrome against a live dashboard process on a spare
port. The one thing this session must not do — restart the process on **8123** —
was not done, so the *API* half of the new UI is not live on 8123 yet (the static
page is, because index.html/app.js/css are read per request). That state is
reported honestly, not hidden: the Apps view says so on screen.

---

## 1. What was built

### A. Apps view — the application cut (`#apps`)

* New endpoint **`/api/apps?range=`** (`apps_payload`) built on the SAME range
  store function the per-module consumer lists use (`_consumer_rows(..., group_by="name")`):
  name, avg CPU %, peak CPU %, memory MB, energy Wh, disk read/write MB,
  network MB, share of measured energy, a 44-bucket energy sparkline, and the
  peak moment of CPU / memory / energy.
  `share_pct` is the only share shown — no "gap to leader" anywhere.
* **`/api/apps/series?range=&name=`** (`app_series_payload`): that app's own CPU,
  memory, energy W, disk B/s and network B/s series + per-field peaks + coverage.
  Every pid carrying the name is summed per sweep, exactly like the row, so the
  drawer cannot disagree with its row.
* Frontend: a virtualised grid (34 px rows, ~30 DOM rows + 9 buffer), sticky
  header inside the scroller (so header and rows share one content box and never
  drift by a scrollbar width), sort on all 9 columns, name filter, per-row
  sparkline drawn on a bare canvas. Refreshes on range/view change only — the
  1 s and 5 s timers explicitly skip `#apps`.
* Row click opens the L2 panel for that app: five series with avg/peak/moment,
  peak-moments stat block, coverage ("a null is a sweep this app was not in,
  never a zero"), a findings block, and five buttons into the module cut.
* Empty state text is exactly the brief's: `no sweep in this range yet — the store sweeps every 20 s`.

### B. Knowledge page — the manual (`#knowledge`, the `?` tab in the topbar)

32 cards across 10 sections — CPU · Memory · GPU/ANE · Battery · Package power ·
Thermal · Network · Disk · Consumers & Apps · Coverage & the x-axis. Every card
carries name · what it measures · formula (when derived) · source · cadence +
retention tier · what a gap means · a `open module →` button that opens that
module's L2 drawer. Includes the **P/E cluster mapping rule** (efficiency cores
enumerated first: logical CPUs `0 … E−1`, verified with a single-thread load
spiking at indices 4–6 on this M2) and the **x-axis ladder**
(1m→15 s … 1y→1 month, labels local time).

### C. Power & battery (Analytics section)

* Energy used for the range, headline naming which rail it came from
  (package / attributed floor / battery), plus a stacked bar of the top 5 apps +
  "other" and a legend with mWh and share.
* Battery now: draw, state, time-to-empty, charge-to-full, health %, cycles,
  design capacity, max capacity (mAh + the gauge's own %), pack Wh, condition,
  and **life at current draw** labelled *at current draw* with the AC caveat.
* Module energy contribution table: package · CPU · GPU · ANE · attributed ·
  battery · adapter, each in Wh or "not measured" with the reason; the same line
  appears in each module's L2 drawer (`energy contribution · CPU rail used … Wh`).
* **Degradation outlook**: current health, observed cycles/day, health decline
  per month, days to 80 % and the projected date — only when ≥ 14 days of our own
  history exist, and always with
  *"estimate; assumes current usage pattern, not a prediction."*
  Below 14 days it prints `insufficient data` plus the count and the needs.
* **Cost** is tariff-gated: hidden until a rate is typed in ⚙ settings
  (browser-local, never sent to the server), then priced on the rail a meter
  would see (adapter input → package → attributed → battery) and labelled with
  that rail.

Theme held: one accent (`#5EEAD4`) plus its opacity ramp, warn red only on the
existing gear badge, **0 box-shadows** in computed styles, `tabular-nums` on
every number.

### New backend pieces

| piece | why |
|---|---|
| `/api/apps`, `/api/apps/series`, `/api/battery` | the three surfaces above |
| `battery_capacity_static()` | design/max capacity from ioreg, **cached 24 h** (they are static facts) |
| `battery-history.jsonl` + `battery_history_loop()` | one row per 30 min of (ts, cycles, health %, capacities) — the collector log and the 1 s history rows carry no cycle count, so the outlook needs our own record. Not on any tick path. |
| `battery_degradation()` | the projection, or an explicit "insufficient data" |
| `disk_read_mb` / `disk_write_mb` / `net_mb` on consumer rows | additive window totals (disk is exact byte deltas; nettop is rate × the span the row already divides by) |

No refresh throttle was touched: `PROC_RING_SECS` (20 s), `ENERGY_REFRESH`,
`last_disk_refresh`, `last_sweep`, `POWER_SAMPLE_SECS`, `BATTERY_TTL` all stand.
The battery recorder runs on its own 600 s thread and appends at most once per 30 min.

---

## 2. Acceptance — real output

```
$ python3 -m py_compile dashboard/server.py
exit=0
$ node --check dashboard/static/app.js
exit=0
$ curl -s http://127.0.0.1:8123/ | grep -c 'data-view="apps"'
1
$ curl -s http://127.0.0.1:8123/ | grep -c 'data-view="knowledge"'
1
```

My own scripted run (kept in `/tmp/verify_t17b.sh`; nothing under `tools/` was
touched) against a live instance on :8149:

```
== 1. static checks ==
  PASS  py_compile dashboard/server.py
  PASS  node --check app.js
== 2. the live page on 8123 ==
  PASS  apps tab appears once
  PASS  knowledge tab appears once
  PASS  8123 health untouched
== 4. /api/apps ==
  count=95  rows=95  sweeps=3  coverage=1.1%
  top row: mediaanalysisd  cpu_avg=90.89 peak=91.7 mem=29.7 energy=0.000839 share=37.42
  no top-5 cap: 95 rows returned (limit param does not exist)
== 5. /api/apps/series ==
  series=['cpu', 'mem_mb', 'energy_w', 'disk_bps', 'net_bps']
  peaks={'cpu': 91.7, 'mem_mb': 29.7, 'energy_w': 0.0759, 'disk_bps': 2447.8, 'net_bps': None}
== 6. /api/battery ==
  draw_w=2.09 health=81.7 cycles=350 design=5760 mAh max=4561 mAh (gauge 100.0%) pack=61.21 Wh
  life at current draw=29.29 h
  degradation available=False reason='insufficient data — 1 recorded sample …'
  cache_ttl_s=86400.0
== 7. degradation projection branch (synthetic 20-day history) ==
  span=20.0 days cycles/day=1.0 health/month=0.6% days_to_80=85.0 date=2027-02-12
  3 days of history -> 'insufficient data — 3.0 days of battery history (needs 14)'
== 8. consumers payload unchanged ==
  metric=cpu_avg_pct count=128 rows=128 keys_ok=True
VERDICT: all checks passed
```

## 3. Browser checks (headless Chrome over CDP, live data)

```
title: applications · 124 measured over 1h
columns: APPLICATION | AVG CPU % | PEAK CPU % | MEMORY MB | ENERGY WH |
         READ MB | WRITE MB | NETWORK MB | SHARE OF TOTAL | SHAPE
domRows: 35   sparkCanvases: 35   totalApps: 104   maxRowReached: 104   ← scroll reaches 50+
sort energy desc → "Python ×3 2.47 mWh 31.6 %"; asc → "UserEventAgent 0.00 mWh"
sort name asc → AeroSpace, akd, amsaccountsd
filter "chrome" → 3 rows (Chrome Helper Renderer ×17, Helper ×8, Chrome ×2)
row click → drawer "Python"
  CPU %      avg 9.7%  · peak 11.1% at 10:36 · 8 samples
  MEMORY MB  avg 246 MB· peak 271 MB at 10:36 · 9 samples
  ENERGY W   avg 0.071 W· peak 0.095 W at 10:37 · 8 samples
  DISK B/S   avg 13.0 KB/s · peak 30.3 KB/s at 10:38 · 8 samples
  NETWORK B/S avg 0 B/s · peak 0 B/s at 10:36 · 4 samples
  peaks: energy over 1h 3.16 mWh · share of total 27.5 % · processes 3
  coverage: measured 5 % of 1h · 9 sweeps @20 s · a null is a sweep this app was not in
  hash stayed #apps (drawer is an overlay, not a route)

knowledge: 32 cards · sections cpu:4 memory:2 gpu:4 battery:5 power:3 thermal:3
           network:2 disk:3 consumers:3 coverage:3
           everyCardHasSource/Cadence/Gap/Links = true · hasPErule · hasLadder

power: hero "0.0338 Wh package (root) · 1h"
       split  "mediaanalysisd 4.83 mWh · 31.8 % | Python 3.54 mWh · 23.3 % |
               Chrome Helper Renderer 2.16 mWh · 14.2 % | python3.14 1.45 mWh · 9.5 % |
               opencode 0.96 mWh · 6.3 % | other (116 apps) 2.28 mWh · 15.0 %"
       battery: draw 2.11 W · charging · health 81.7 % · cycles 350 ·
                design 5760 mAh · max 4561 mAh (gauge 100 %) · pack 61.20 Wh ·
                energy at current draw 29.00 h (labelled, AC caveat included)
       rails: package 0.0338 / CPU 0.0333 / GPU 0.0004 / ANE 0.0000 /
              attributed 0.4026 / battery 0.0763 Wh (adapter present in payload)
       degradation: health 81.7 % · 1 sample · needs 14 days · "insufficient data"
       cost: hidden before a tariff; after entering 8.5/kWh in ⚙ settings →
             "EUR 0.0617 priced on adapter input (7.2583 Wh) at 8.5 per kWh"
theme: boxShadows=0 · tabularOnNumbers=tabular-nums ·
       saturated hues = accent (94,234,212) + warn (248,113,113, gear badge only)
```

## 4. Two blockers / notes that are NOT claimed as done

1. **The 8123 process predates this build.** It must be restarted by whoever owns
   it before `/api/apps`, `/api/apps/series` and `/api/battery` exist there.
   Until then the Apps view renders its honest failure text:
   `"/api/apps did not answer (HTTP 404) — this page is newer than the running
   dashboard process; restart it to serve the range store."`
   *(verified live on 8123 — this is the current on-screen state there)*
2. **The collector's daily file no longer carries `processes`.** As of 10:12 today
   the Rust collector writes raw rows (with `processes`) under `logs/raw/` and
   aggregated 15 s rows (`agg`, no `processes`) to the top-level daily file that
   the dashboard reads. Consequence: `/api/power.by_app` and
   `powerHistory.analysis.per_app` are now empty (evidence:
   `tail -1 logs/raw/*.jsonl` has `processes`; `tail -1 logs/2026-11-19.jsonl`
   has `agg` and no `processes`). That is a different lane's in-flight storage
   change, so it was not "fixed" here.
   **T17B does not depend on it**: the Power & battery per-app split is computed
   from the dashboard's own 20 s range store (the same rows Apps shows) and only
   falls back to the collector rows when they exist. This is stated in the UI
   ("top apps by measured rusage energy over 1h (dashboard range store — the same
   rows the Apps view shows) · a measured floor").

## 5. LEARNED

* One store, two cuts beats two stores. Building Apps on `_consumer_rows` rather
  than a new sweep cost a fraction of the code and made "the same numbers, two
  views" true by construction instead of by discipline.
* The per-app energy breakdown could not be trusted to the collector's snapshot:
  it is a live top-25, and it silently vanished when the collector's schema
  changed. The dashboard's own ranged store is the durable source — a cross-view
  number should come from the store that both views can read.
* A projection is a product decision before it is a formula: the honest version
  costs a file we own (30-min rows) and an explicit "insufficient data" branch,
  and it is worth both — 1 row today, a real answer in 14 days, never a lie today.
