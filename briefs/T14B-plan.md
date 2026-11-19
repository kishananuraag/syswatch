# T14B plan — sensor depth, temperatures, E/P cores, power timeline, model page

Constraints taken as hard:
- 8123 is a launchd service (`com.barbarian.syswatch.dashboard`). Restart only via
  `launchctl kickstart -k gui/<uid>/…`; cleanup of test instances is port-scoped
  (`SYSWATCH_PORT` 8125/8128/8129/8131). Never `pkill -f dashboard/server.py`.
- No fabricated numbers. A source that does not exist reports `unavailable` + reason.
- T14A behaviour stays: 10 tabs, tick labels, 1 s/5 s polls, `document.hidden` pause.

## Server (dashboard/server.py)

1. **Root sample reader** — `~/Library/Application Support/syswatch/root/sample.json`
   (T15 daemon). Cached 2 s, stale > 30 s → treated as absent (never a cached
   number presented as live). Fields: cpu/gpu/cluster temps, package/cpu/gpu/ane W,
   thermal pressure, fans, ssd temp, ane util.
2. **Adapter wattage** — `ioreg AppleSmartBattery` → `AppleRawAdapterDetails`
   (Watts / AdapterPowerTenthWatt). Empty on battery (verified today:
   `ExternalConnected = false`, `AppleRawAdapterDetails = []`) → `null` + `on battery`.
3. **Battery temp** — unprivileged `Temperature` key = 3000 → 30.00 °C (verified live).
4. **PowerRing** gains `adapter_w`, `package_w`, `cpu_w`, `gpu_w`, `pct`,
   `time_remaining_s` per bucket (mean + max), and a `src` tag per series point.
5. **History seeding (addendum 2)** — bounded tail read of the collector JSONL
   (newest file, ≤ 30 MB, 60 s cache): `energy.attributed_w` per sweep + per-app
   `processes.by_energy[].watts`. Merged into `/api/power/history` points as
   `src: "history"` next to `src: "live"`. GPU/disk have **no** such fields in the
   store — reported as seeded 0 rows with the reason, never invented.
6. **`/api/power/history`** top-level `sampling_s / samples / oldest_ts / newest_ts /
   coverage_text`, plus `analysis` (energy Wh by trapezoid, mean, peak + time, p95,
   drain %/h, projected empty, adapter efficiency estimate, charge cycles in window,
   tariff = unconfigured/hidden) and `per_app_wh` over the window.
7. **`/api/analytics?range=`** — `measures` rows (avg/max/p95/min/prev-window Δ/
   coverage per metric), `findings` (3–4 sentences computed from the series),
   `records` feed, `history_store` disclosure.
8. **`/api/sensors`** — thermal/power_rails/gpu NE/cluster temps come from the root
   sample; every root-gated row carries `root_gated: true` + a grant descriptor.
   Windows/Linux adapter classes return `platform: "unsupported yet"`.

## UI (static/app.js + app.css)

1. Sensor groups render where they belong (CPU, GPU, Memory, Disk, Network,
   Battery, Power) — no 11th tab (design contract: 8–10). A group with no readable
   sensor renders one amber row; the thermal group renders
   `Temperatures — needs root` + Grant affordance, never `0 °C`.
2. CPU: per-cluster rings (P/E discovered from `hw.perflevel0/1.logicalcpu`) + per-core
   rows (4 P + 4 E here) with utilisation and cluster temp when the root file exists.
3. Power: timeline chart — battery draw (accent, solid), adapter input (green, dashed),
   package W (dot-dash) on ONE axis, never stacked; coverage line under the visual;
   analysis stat block; per-app Wh table with gap-to-leader.
4. Power BI interaction: click app row / core / chart bucket → cross-filter, removable
   chips under the header, entity drill-through drawer, click empty space to clear.
5. Analytics = model page: slicers (range in shell header, group, app), measures table,
   findings block, records feed.

## Verification

- `python3 -m py_compile dashboard/sensors.py dashboard/server.py`
- the three curl acceptance commands from the brief
- browser pass (headless Chrome over CDP, since the browser tool blocks
  loopback): CPU view shows amber needs-root temperature row, 4+4 cores,
  power view has a chart + coverage line, console has no errors.

## Result (2026-11-19, verified on 8123)

- sensors: 64 rows across 9 groups; `thermal` + `power_rails` report
  `needs_root: true` (single amber row each).
- power history: 723 points, `3 live + 720 history`, 60 min covered from the
  collector log under the 30 MB cap; battery draw is live-only and said so.
- analytics: 9 measure rows + 4 findings + 5 records, server-computed.
- browser: 10 tabs, 2 cluster rings, 8 core rows (P1–P4 / E1–E4), amber
  `Temperatures — needs root` with a Grant control, chart click → chip +
  bucket drawer, app row → drawer with its own measured series, 0 console errors.

## LEARNED

1. **The only real history store here is the collector log.** Battery draw,
   GPU utilisation and disk throughput are *not* in it, so "seed the thin
   series" could only be honoured for attributed W. The honest version is a
   per-field disclosure ("history: none — the store records gpu_time only"),
   not a filled-in chart. Fabricating the missing series would have looked
   better and been worthless.
2. **A `{k: ... for k, span in RANGES.items()}` loop leaks `span`.** The
   analytics measures silently inherited the 1-year window and reported a
   99.9 % CPU peak from two days earlier as "this hour". Shadowed loop names
   are a correctness bug, not style.
3. **Poll cost is a design constraint, not a footnote.** `/api/power/history`
   at 1 s went from 3 ms to 34 ms as soon as analysis was bolted on; splitting
   the payload (cheap `current`/`windows` for the poll, full analysis only for
   a ranged request) put it back at 3–8 ms.
4. **A refused `pkill -f` is a whole class of outage.** Port-scoped kill with
   the port from an env var kept 8123 alive through seven test restarts; the
   launchd restart was `launchctl kickstart -k`, not a process hunt.
5. **Grant UX without T15 must not 404.** Probing `hasattr(Handler, "do_POST")`
   lets the same button become live the moment T15 lands, and until then it
   explains the mechanism instead of pretending to elevate.
