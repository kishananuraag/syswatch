# syswatch design system — "calm instrument" (T14 contract, 2026-11-19)

Research base: Grafana dashboard best practices (visual hierarchy by
alignment/size/color, hierarchical drill-down, USE method), NNGroup "Tabs,
Used Right", Pencil&Paper dashboard UX patterns (single-page canonical, tabs
for 3–7 views sharing context), AYDesign dark-mode patterns 2026 (Linear /
Vercel / Stripe craft), and the gascity DESIGN.md anti-pattern list (what not
to do).

## The verdict from the research

The navigation complaint ("clicking a card takes me to a throwaway page") is a
known failure mode: **in-page tabs must keep users on the same page while
alternating related views** (NNGroup). Our section pages broke that rule by
looking like different sites. The fix is a persistent shell, not better pages.

## Shell (one page, never a page change)

```
┌──────────────────────────────────────────────────────────┐
│ syswatch · Overview  CPU  Memory  GPU  Disk  Net  Batt   │  ← one-row tabs,
│                     Apps  Power  Analytics   [1h ▾] live●│    identical style
├──────────────────────────────────────────────────────────┤
│  (current view renders here, never a route change)       │
└──────────────────────────────────────────────────────────┘
```

- Tabs: 8–10 views sharing one context — the exact case tabs exist for.
- **Shared time-range control lives in the shell header** — switching view
  keeps the range; switching range keeps the view. URL hash syncs (`#power`)
  so back/forward and bookmarks work.
- Clicking a card opens that tab in place: content crossfades (opacity/transform
  only — never animate height/top/width), scroll position resets to top of view.
- A `live●` badge in the header pulses only when the 1 s poll is healthy; a
  stale badge goes dim with the seconds-since-update shown.

## Visual system (dark, Linear-tier craft)

| Token | Value |
|---|---|
| base | `#121317` true grey, never `#000` (halation) |
| surface | `#181A1F` |
| elevated | `#1E2127` (3–6 % lighter steps; depth = lightness, not shadows) |
| border | `#2A2E36` hairlines only |
| text | `#F4F5F8` primary · `#9BA3AE` muted |
| accent | ONE saturated color (electric blue `#4D9FFF`), reserved for live
  state, active tab, and the primary chart series. Everything else desaturated. |
| semantic | green `#5FB97A` ok · amber `#E3B341` warn · red `#E5484D` hot —
  lower saturation than the accent; must survive greyscale |

- **Elevation by background lightness steps, not drop shadows** (Vercel
  pattern). Borderless cards; hairline border only where lightness alone fails.
- Typography: system SF; tabular figures (`font-variant-numeric: tabular-nums`)
  on every number; big KPI numerals at 600 weight; ALL-CAPS micro-labels at
  0.75 rem with 0.04 em tracking; body 15 px/1.55.
- One accent rule: a card that is not the most important thing on screen does
  not get saturated color. Power/CPU may use the accent; everything else is
  greyscale-plus-semantic.

## Overview (screenfit, the "mesmerizing" layer)

- 6 KPI cards in a strict grid (equal gutters 12 px, cards equal height).
  Z-pattern: CPU top-left (Grafana alignment rule), then memory, GPU, network,
  disk, power.
- Each card: micro-label → big tabular numeral → sparkline (accent series, soft
  area fill ≤ 12 % opacity, gridlines 3–8 % above surface) → peak chip → range
  is inherited from shell (NOT a per-card dropdown).
- The whole card is the click target; hover lifts to the `elevated` token and
  shows exact value on the sparkline at the pointer (details on demand).
- Anomaly language: a value crossing its threshold gets the semantic color ON
  THE NUMBER ONLY, plus a conditional mark — never a full-card color wash.

## Section views (drill-down, Grafana hierarchy)

- Every section page opens with the SAME header rhythm: title, current value
  big, then the chart, then the supporting stat rows. Alignment identical
  across sections so the eye never re-learns.
- Stat rows: label left, tabular numeral right, no stripes, no cards-within-
  cards. Separation by whitespace and hairlines.
- Apps section: ranked table, energy column highlighted (the thing iStat
  cannot do), bar-in-row scaled to max.

## Analytics page

Narrative order, not a grid dump: 1) the day in one chart (CPU + energy
overlaid), 2) averages/peaks row, 3) busiest hour, 4) top apps, 5) network
totals. One accent, one font scale.

## Anti-patterns (borrowed and enforced)

- No gradient text/buttons; no glassmorphism/backdrop-blur.
- No side-stripe borders on rows.
- No layout-property animation; opacity/transform only, 150 ms.
- No second typeface.
- No color-only meaning; every state readable in greyscale.
- Density is not insight: no more than 2 chart cards at rest in a section
  above the fold.

---

# Part 2 — Presenting the numbers (researched 2026-11-19)

Sources: Lumivero "How to present statistical results" (storyline, 3–4 key
messages, clean visuals over raw tables), Apollo Technical "present stats in an
interesting way" (comparisons + context), Stats Made Easy ch.5 (counts vs
continuous measures, distributions), Dribbble `tags/stats` (12k shots: hero
numerals, delta chips, ranked rows), and app.formula1dashboard.com (broadcast
telemetry cards).

## The F1 patterns that fit syswatch

| F1 pattern | syswatch translation |
|---|---|
| Hero number per card with attribution under it (`1.99 s · Lindblad, Round 11 Hungary`) | `4.37 W · Chrome Helper, now` — every headline value names its source and time |
| Delta chip with context (`+2.28 % vs prev. round`) | `+1.2 W vs prev 1h` — always compared to a stated baseline, never bare |
| Gap-to-leader in standings (`390-0`, `279-111`) | Apps table: `Chrome 0.46 W · gap −0.42` — distance to the top consumer |
| Season progress as % of a known whole (87 % of season) | `day sampled 96 %` — coverage of the plotted window, so a gap is never mistaken for a drop |
| Records feed with date + place | `03:12 · CPU peaked 91 % — Chrome Helper` — derived from real collector data |

## The presentation rules (what makes stats readable, not just drawn)

1. **Three or four takeaways per page, not forty numbers.** Analytics opens
   with 3–4 sentence-level findings computed from the series ("CPU averaged
   14 % and peaked 91 % at 03:12, driven by Chrome Helper").
2. **Comparisons and context, always.** Every value that matters carries a
   baseline: previous window, 7-day average, or the top consumer.
3. **Continuous data gets measures, not counts.** CPU/memory/energy show
   mean, median, p95, min/max and spread — not binned counts (Stats Made Easy:
   binning continuous data loses the distribution and misleads).
4. **Ranked rows use gap-to-leader** so the eye compares against the biggest,
   not an arbitrary axis maximum.
5. **Charts replace tables for shape; tables stay for ranking** (Lumivero:
   replace raw tables with visuals, keep the ranked list).
6. **Labels are sentences where space allows** — a record reads
   `03:12 · CPU peaked 91 %`, never `cpu_max 91`.
7. **Consistent fonts/colors across every view**; minimal text; the chart
   carries the shape, the caption carries the meaning.
8. **Never a naked number.** Unit + timestamp + source, or it goes in a
   tooltip.

## Components T14/T15 must render

- **Hero+attribution stat block** (KPI cards, section headers)
- **Delta chip** with explicit baseline label
- **Coverage indicator** (`day sampled 97 %`) in every chart footer
- **Gap-to-leader column** in Apps
- **Findings block** at top of Analytics (3–4 sentence takeaways, generated
  from measured series — no template lorem)
- **Records feed** on Analytics (peaks and events from real data, each with
  time and source app)

---

# Part 3 — T16A "the instrument" (2026-11-19, supersedes the tab shell)

Kishan's verdict on the Part-1 shell: *"the dashboard page should be cool and
singular, not like some 100 modules in the bar I have to pick"*, *"the timeline
is in the corner and we need to implement where the user loves to click"*, and
elements with no meaning must go. Part 1's 8–10-tab bar is **retired**; the
principles above (one accent, lightness for depth, tabular numerals, no
fabricated values) still hold.

## Navigation — four views, one story

`Instrument · Apps · Analytics` plus a `⚙` that opens the **settings drawer**
(permissions · about · data-source inventory). Hash routes: `#instrument`,
`#apps`, `#analytics`, `#settings`. Legacy hashes (`#cpu`, `#power`,
`#permissions`…) deep-link into the matching Instrument section.

The Instrument is ONE continuous page, scroll-told:

1. **Hero band** — six vitals, iStat hierarchy: value big, unit small, delta
   chip that names its baseline (`+1.2 W vs 1h avg`), attribution underneath
   (`powermetrics · root sample 4 s old`). CPU and Memory carry a pressure dial
   (arc = pressure %, the word in the middle = level).
2. **The range pill** — the product's ONLY range control. Sticky, centered
   under the hero band at 50 px (the shell height), 32 px hit targets, active
   step in the accent.
3. **Eight sections in scroll order** — CPU, Memory, GPU, Disk, Network, Power,
   Battery, Thermal. Each: heading with the live value, chart + coverage footer,
   consumers panel (where the server has a per-app store), then its evidence.

## Interaction laws

- **Brush to zoom**: drag horizontally on any chart; the swept span snaps to
  the nearest ladder step (log-nearest, so 11 min → 15m) and becomes the range
  for the whole page. Double-click resets to 1 h. The pill is the same state's
  keyboard/mouse control — it can never disagree with the charts.
- **Repaint what you can see**: an IntersectionObserver gates section repaints,
  so the 1 s tick updates the hero band and on-screen sections only. A system
  monitor has no business burning the CPU it is measuring.
- **Grant UX**: while the settings drawer is open the permission state is
  re-read every 2 s (cheap probe) and a full probe runs the instant a grant
  returns; the panel flips to `granted` with a toast the moment macOS does. A
  cheap probe may never overwrite what a full probe learned (the Accessibility
  toggle is per-app and only the full probe can read it).

## Meaningless-element law (enforced, not aspirational)

Delete or fix anything that shows an unlabelled number, a label with no value
and no reason, a duplicate of another element, or a placeholder that was never
wired. Concretely, in this build:

- every table/consumer column header carries its unit (`avg CPU %`, `energy
  mWh`), and a cell with no sample says `n/a`, never a bare dash;
- a capability row that cannot be read prints `needs root` + the reason inline
  (no hover-only explanation);
- capability sub-panels exist only where they add rows the section does not
  already show (CPU keeps load/frequency; Memory, GPU, Disk, Network, Battery
  and Power dropped the duplicates);
- nothing renders `—` or `…` once data has arrived; empty states name the
  missing field;
- the adapter input is a **stat**, not a power series: 67 W on the same axis
  flattened every rail to the x axis.
