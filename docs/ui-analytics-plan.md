# syswatch analytics redesign — plan v3 (2026-11-19)

Research-grounded: Shneiderman's information-seeking mantra ("overview first,
zoom and filter, then details-on-demand", 1996, validated across dashboard
studies), the ONS data-visualisation manual (axes), Almossawi's time-series
guidelines, Datawrapper's time-axis rules (5–10 labels, no repetition), and the
GLE calendar-semantics paper (ticks at calendar boundaries, never at arbitrary
numeric intervals).

## The three governing rules

1. **Overview first.** One screen answers "is my machine healthy right now?"
   Six KPI cards with sparklines, nothing else. Everything deeper is one click
   away, never crammed on top.
2. **Zoom and filter.** Every card carries the same time-range control. A range
   change re-fetches, it does not re-render stale data.
3. **Details on demand.** Hover shows exact value + timestamp; click opens the
   section page. No raw tables in the overview.

## Time axis ladder (the fix for "random numbers")

Chart.js never formats our timestamps today — raw `p.ts` strings go to
`autoSkip`. Replace with a server-side `tick_ladder` per range, computed on
calendar boundaries, not uniform numeric intervals:

| Range | Tick every | Label format | Boundary rule |
|---|---|---|---|
| 1m | 15 s | `:05 :20 :35 :50` | — |
| 5m | 1 min | `HH:MM:SS` | — |
| 15m | 3 min | `HH:MM` | — |
| 1h | 10 min | `HH:MM` | — |
| 6h | 1 h | `HH:00` | — |
| 1d | 3 h | `HH:00` | day boundary gets `16 Nov` |
| 7d | 1 calendar day | `Mon 16` | ticks on day starts |
| 30d | ~5 calendar days | `16 Nov` | ticks on day starts |
| 1y | 1 calendar month | `Jan Feb Mar…` | year once, at Jan |

Hard limits: 5–10 labels visible (fewer on narrow cards); two-level labels only
where the table says so; **gaps in data render as gaps, never as zero**; y-axis
starts at 0 with SI prefixes (k/M) chosen server-side.

## Spike/peak (defined, not vague)

Every card gets a **peak chip**: the max value inside the selected window,
formatted with the same unit as the axis, with its timestamp —
`peak 87% · 14:32`. For network/disk cards, peak is the max of (rx or tx) and
shows which. A spike is only shown if it stands ≥ 20 % above the window median,
otherwise the chip reads `steady` — a chip that says "peak 3.1 KB/s" next to a
3 KB/s line is noise, not information.

## Information architecture

```
OVERVIEW (screenfit, Shneiderman layer 1)
  6 KPI cards + sparkline: CPU · Memory pressure · GPU · Network · Disk · Power
  each card: big number, sparkline, peak chip, range control (shared)

SECTION PAGES (layer 2, one per domain — reachable by clicking a card)
  cpu      user/system stacked, per-core E/P rings, per-core temps list
  memory   pressure ring, wired/app/compressed stacked, swap line
  gpu      util %, mem %, temp, freq, FPS
  disk     free space, read/write rates, per-volume
  network  up/down, per-interface
  battery  %, time remaining, health/cycles/condition, cells, energy mode
  power    see below
  apps     per-app CPU AND per-app energy (our capability iStat lacks)

DETAILS (layer 3)
  tooltips exact value+time; parity table stays at docs/parity.md, linked
```

## Power (where it fits)

Power is **not a sidebar subsection — it is a first-class domain**, because
"where does the energy go" is syswatch's reason to exist and iStat cannot do
per-app attribution at all. The panel has three honest tiers:

- **Tier 1 (no root, always available):** battery watts (voltage × amperage via
  IOKit/SMC unprivileged keys), charging state, and **per-app watts ranked**
  (already verified: `rusage` attribution, `by_energy`).
- **Tier 2 (SMC read, usually unprivileged):** CPU/GPU/Neural package power —
  if a key is unreadable, show `unavailable` **never zero, never a guess**.
- **Tier 3 (root, optional one-shot `powermetrics`):** package totals. Off by
  default; the UI labels it "needs root" instead of hiding it.

Overview KPI = current total draw (Tier 1) + biggest app.

## Stats from the iStat reference that must exist (parity targets)

per-core temps · memory pressure · E/P-core split · GPU (util/mem/temp/freq/FPS)
· disk free + rates · up/down · load 1/5/15 · uptime · battery
(health/cycles/condition/cells/energy mode/significant-energy) · per-process CPU
· per-process energy (NEW — ours) · total power (NEW — ours, Tier 1/2).

Sensor names stay as hypotheses (iStat stores md5 keys — if we map one, we prove
it by range and label it a hypothesis; otherwise we show `Sensor <n>`).

## Honesty rules carried over

- UNTESTED parity rows stay visibly UNTESTED; a made-up green is a defect.
- Missing/zero-capability sensors render as `unavailable`, styled dim, with the
  reason — never as a misleading 0.
- No CDN, no framework, no build step. Same constraint as T5/T9.
