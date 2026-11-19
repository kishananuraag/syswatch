# UI plan v2 — Mac-native, from scratch
*Proposal. Supersedes DESIGN.md v1 **for this platform only**; v1 was written for the Windows laptop (Aug 24) and every one of its requirements carries over — only the data sources change. Status: awaiting Kishan's approval. No build of the app shell without it (T6 is gated on this doc).*

## 0. The one rule
Nothing in this UI is copied from iStat Menus — not code, not icons, not spacing, not layout files. iStat is the **reference for what to show and how fast it must be read**, never the source of how to draw it. If a screen looks like their screen, that's a design failure, not a homage.

## 1. Three surfaces, one language

| Surface | Job | Constraint |
|---|---|---|
| **Menu bar** | one glance: is anything wrong? | ≤ 3 glyphs + 2 numbers, redrawn only on change |
| **Dropdown panel** | the details for one section, hover-to-peek | opens in < 100 ms, no data fetch on hover (read from the in-process ring buffer) |
| **Window** | history and correlation | graphs, time-range pills, per-app tables |

Same type scale, same number formatting, same colour semantics across all three. A number never means something different in the window than it does in the menu bar.

## 2. Menu bar (default configuration)
```
[▲ 12%] [▮▮▯ 68%] [↓1.2 ↑0.4] [⚡ 6.8W]
  CPU     memory      network      energy
```
- Monospaced numerals, right-aligned, fixed-width fields so digits never jitter.
- A sparkline replaces the CPU glyph when the section is expanded (last 60 s, one pixel per sample).
- Colour is **semantic only**: neutral by default; amber when a threshold is crossed; red never used for decoration, only for a real alert (`fail`, `park`, thermal).
- Clicking a block opens that section; ⌥-click opens the window at that section; right-click = preferences.

## 3. Sections (dropdown)
`Overview · CPU · GPU · Memory · Disk · Network · Sensors · Energy · Apps · Agents`

Carried over from v1 requirement #5, plus two new ones (`Energy`, `Agents`):

| Section | Shows |
|---|---|
| Overview | CPU total + per-core strip, memory used/total, swap, net rates, top 3 by CPU, top 3 by energy |
| CPU | total, per-core, P/E core split, frequency, load average, top consumers with instantaneous % |
| GPU | util %, VRAM, top GPU-time consumers (`ri_gpu_time`), rail watts |
| Memory | used/wired/compressed/swap, pressure, top by RSS, swap trend |
| Disk | per-volume free, read/write rates, IOPS, top I/O processes |
| Network | rx/tx dual line, per interface, totals since boot |
| Sensors | **every sensor the machine actually exposes**, grouped (°C / RPM / V / A / W); absent sensors are simply not listed (v1 rule: never an empty row) |
| **Energy** | "who" bar (top apps, watts) next to "where" bar (rails), with `unattributed` grey and always visible; per-app table with a mini sparkline |
| Apps | sortable table: cpu %, rss, energy W, GPU time, disk I/O, wakeups — the columns iStat keeps live and discards |
| **Agents** | live queue chips, current task, last 50 ledger rows (T9). Failure looks like failure |

## 4. Charts that don't lie or look pointy
v1 requirement #4 — "smooth, not pointy" — done properly:
- **Monotone cubic interpolation** (Fritsch–Carlson), not naive Bézier: naive smoothing overshoots and draws negative CPU. Monotone keeps peaks without inventing values below zero.
- Downsample to one column per pixel using a **min/max envelope**, so a 200 ms spike at 10:04 is still visible when zoomed out to 7 days. Smoothing must never erase a real spike.
- Area fills only for stacked composition (wired/app/compressed, cpu user/system). Lines elsewhere. No gradients, no drop shadows, no 3-D.
- Axis labels in IST, monospaced, at most 5 ticks. Crosshair shows the exact value **and its timestamp**.
- Hover latency ≤ 50 ms; animation ≤ 200 ms and suppressed under Reduce Motion.

## 5. Time-range pills (v1 requirement #3, kept exactly)
`10m · 15m · 30m · 1h · 3h · 6h · 12h · 1d · 3d · 7d`, sitting beside the section title, **global state** — every chart in the window re-reads the same range from one source. Switching range must not re-fetch the collector; it re-slices the ring buffer.

## 6. Design system
- Type: one family (SF Pro / system), two weights (regular, medium), three sizes (11 / 13 / 20). Numerals monospaced everywhere.
- Grid: 4 px base; 8 px between related items; 16 px between groups.
- Palette: neutral greys + exactly four semantic colours (ok, warn, alert, muted). Dark and light both specified, contrast ≥ 4.5:1 on all text.
- No icon without a label on hover; no tooltip that repeats the label; no spinner that can't fail.
- Empty states are sentences, not spinners: `idle — queue empty`, `no sensor exposed on this Mac`, `collector not running (launchd: not loaded)`.

## 7. Degradation rules (the machine decides, not the design)
- No root → no machine power rails → Energy shows "who" only, with `processes_only` stated once, quietly.
- No SMC access → Sensors section lists only `ioreg`-visible values, and says how many are hidden by privilege.
- Collector down → every panel shows the last-known value **greyed with its age** (`as of 4m ago`), never a zero, never a blank.
- The app never blocks on data: if a metric is unavailable, the rest still draws.

## 8. Build order → tasks
| Step | Task | Ships |
|---|---|---|
| 1 | T5 | data shapes + graphs on the iStat fixture (works with no collector) |
| 2 | T6 | menu bar + Overview + CPU + Memory sections |
| 3 | T7B/T7C | instantaneous per-process CPU, per-app energy → Apps + Energy sections |
| 4 | T7A | cost discipline proven (< 1 % / < 15 MB) |
| 5 | T8 | launchd durability |
| 6 | T9 | Agents section |
| 7 | — | Sensors + GPU sections once Q1/Q8 (root policy) is answered |

## 9. How the UI gets accepted
- Every value on screen must equal the CLI within the A7 tolerances — checked by screenshot against `syswatch --json` at the same moment.
- Idle cost: a static screen < 0.5 % CPU (T6 asserts it).
- Hover/zoom latency measured, not claimed.
- Every degradation path triggered at least once (kill the collector, deny root, hide a sensor) with a screenshot.

## 10. Open, waiting on Kishan
- Root policy for real temps/power (Q1/Q8) — decides whether Sensors and the "where" bar are full or partial.
- Whether the menu bar defaults above are right, or he wants CPU/RAM/net only.
- Approval of this doc = the T6 build gate.
