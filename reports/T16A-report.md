# T16A report — the Instrument, one singular dashboard (2026-11-19)

Built: `dashboard/static/index.html` (new shell), `dashboard/static/app.js`
(rewritten IA + sections + brush + settings drawer), `dashboard/static/app.css`
(new components). Docs contract updated: `docs/ui-design-system.md` Part 3.

**No server change was needed.** `/api/sensors` already exposed the root-tier ANE
rows and `/api/power/history` already carried `ane_w`, so the live dashboard
process (pid 7001, port 8123) was never restarted and never touched. `src/` was
not touched. `cargo build --release` was not run because no Rust file changed.

## 1. IA — four views, one story

- Nav: `Instrument · Apps · Analytics · ⚙` — exactly four `data-view` values.
- Instrument = one continuous page: hero band (6 vitals) → sticky range pill →
  section index → 8 sections in scroll order (CPU, Memory, GPU, Disk, Network,
  Power, Battery, Thermal).
- `⚙` opens the settings drawer (permissions · about · data-source inventory) at
  hash `#settings`; the page behind it is the live instrument, never "loading…".
- Legacy hashes (`#cpu`, `#power`, `#permissions`, …) deep-link into the matching
  Instrument section.

## 2. The timeline

- One sticky, centred pill under the hero band: `1m 5m 15m 1h 6h 1d 7d 30d 1y`,
  32 px hit targets, active step in the accent (`offsetFromCentrePx: 0`,
  `position: sticky`, `top: 50px` = the shell height).
- Brush-to-zoom on every chart: the swept span snaps to the nearest ladder step
  (log-nearest) and becomes the page's range; double-click resets to 1 h.
  Verified with real pointer events: `1h → 15m`, toast `brushed 21 min → range
  15m`, pill followed, double-click returned `1h`.

## 3. Root data wired

- Hero band shows `package_w` live with the `root` chip and
  `CPU 0.26 · GPU 0.04 · ANE 0.00 W` underneath.
- GPU section: **Neural Engine (ANE)** row = `0.00 W · util needs root
  (ANEMetrics)`.
- Power section: battery draw + package + CPU + GPU + ANE + attributed apps as
  series on one axis; the adapter input is a stat, not a series (67 W flattened
  every rail onto the x axis — that was measured on screen).
- Thermal: pressure level (`Nominal`) live; CPU die / GPU die / fans render
  `needs SMC reader` (T16B) instead of a plausible number.

## 4. Grant UX

- Drawer open → cheap permission probe every 2 s; full probe on open and again
  the instant a grant returns, then sensors + power + every section repaint.
- Fixed the "jumps back to grant" class of bug: a cheap probe can no longer
  overwrite what a full probe learned, so the per-app Accessibility toggle stays
  `granted` across polls (verified over two 2 s polls).

## 5. Meaningless-element audit (what was deleted or fixed)

- Overview cards, the `—` peak chips, the empty `.chip` spans, the stray
  hover-only explainer row, the `time` slicer that duplicated the header range,
  and the duplicate capability panels in Memory/GPU/Disk/Network/Battery/Power.
- Column headers now carry units (`avg CPU %`, `energy mWh`, `peak MB`); a cell
  with no sample says `n/a`; `350 n` (registry count marker) is now `350`.
- Adapter no longer drawn as a series; its note sentence trimmed to the series
  actually drawn; battery's `time-to-empty` footnote corrected to `to-full`
  while charging.
- Automated audit: zero elements whose text is a bare number without a unit in
  its own label/header (`10. unlabelled numbers: []`).

## 6. Honesty unchanged

`needs root` / `unavailable` + reason everywhere, coverage footers say what share
of the window was measured, history gaps are named (`history: none — the
collector log records gpu_time only …`), no fabricated values.

## Acceptance — real output

```
$ python3 -m py_compile dashboard/server.py
exit=0

$ curl -s http://127.0.0.1:8123/ | grep -oE 'data-view="[a-z]+"' | sort -u
data-view="analytics"
data-view="apps"
data-view="instrument"
data-view="settings"

$ curl -s http://127.0.0.1:8123/api/sensors | python3 -c "import json,sys;d=json.load(sys.stdin);print('ane:',[s for s in d['sensors'] if 'neural' in s['id'].lower() or 'ane' in s['id'].lower()])"
ane: [{'id': 'gpu.neural_engine', … 'available': False, 'reason': 'needs root — privileged daemon sample absent' …},
      {'id': 'gpu.neural_engine_w', 'label': 'Neural Engine power', 'value': 0.0, 'unit': 'W', 'available': True …},
      {'id': 'power_rails.ane', 'label': 'Neural Engine power', 'value': 0.0, 'unit': 'W', 'available': True …}]

$ curl -s http://127.0.0.1:8123/ | grep -c "thermal_pressure\|Thermal"
3
```

Browser checks (headless Chrome 157 driven over the DevTools protocol — the
obscura tool refuses 127.0.0.1 by its SSRF guard, so CDP was used directly):

```
1. four views only      : ["instrument","apps","analytics","settings"]
2. ready                : True
3. sticky pill centered : {"offsetFromCentrePx":0,"position":"sticky","stickyTopPx":"50px","hitTargetPx":32,
                           "steps":["1m","5m","15m","1h","6h","1d","7d","30d","1y"]}
4. hero band            : {"cpu":"28.5 %","memory":"77.4 %","gpu":"7.0 %","battery":"50 min to full",
                           "package_power":"0.29 W package","package_detail":"CPU 0.26 · GPU 0.04 · ANE 0.00 W",
                           "thermal":"Nominal","rootChip":true}
5. GPU ANE row          : ["Neural Engine (ANE)0.00 W · util needs root (ANEMetrics)"]
6. thermal section      : {"exists":true,"hero":"Nominal",
                           "slots":["CPU dieneeds SMC reader","GPU dieneeds SMC reader","fansneeds SMC reader"]}
7. sections on the page : ["cpu","memory","gpu","disk","network","power","battery","thermal"]
8. chart drag → range   : {"before":"1h","after":"15m","origin":"brush","pill":"15m",
                           "toast":"brushed 21 min → range 15m"}
9. double-click resets  : 1h
10. unlabelled numbers  : []
11. js exceptions       : 0
12. console errors      : []
13. views apps/analytics/settings: hero band + section index hidden, stage content replaced,
    settings drawer opens with 5 permission rows and 9 capability groups
```

Range ladder smoke test (each step settles, charts repaint, footer follows):

```
5m  foot: measured 100 % of 5m   settled in 0.61s
15m foot: measured 100 % of 15m  settled in 1.14s
1h  foot: measured 100 % of 1h   settled in 0.17s
1d  foot: measured 100 % of 1d   settled in 4.97s   (history store read)
```

## Not verified by me

- The feel of a real trackpad drag (only synthesized pointer events, which
  produce the same pointerdown/move/up path and did change the range).
- `tools/verify_T16A.sh` does not exist on disk (only verify_t5/t7c/t8/t10/t11),
  so the brief's acceptance block above is what was run.
- Anything requiring the SMC reader (die temperatures, fans) — that is T16B and
  the UI says so rather than drawing a number.

## LEARNED

Two things cost real time and are worth keeping: (1) `[hidden]` loses to any
class rule that sets `display`, so a `hidden` component without a global
`[hidden]{display:none!important}` silently stays on screen — that was the
hero band "still visible on Apps"; (2) a cheap status poll must never overwrite a
full probe's answer, or the UI flips back to the old state every 2 s — exactly
the "gets back to grant" report.
