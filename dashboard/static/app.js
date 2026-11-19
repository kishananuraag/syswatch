// syswatch dashboard — T20 "nested layers"
//
// ONE dashboard, three views and a gear: Instrument · Apps · Analytics ·
// #settings (which also carries the knowledge manual — reference material is not
// a layer). The Instrument is ONE viewport in three depths:
//
//   L1  nine tiles in one 3-column grid (CPU · Memory · GPU · Battery · Package
//       power · Network · Disk · Load · Thermal). Number (24 px, tabular) +
//       40 px sparkline + one 10 px delta + one meta line. No chart, no
//       measures, no findings, no coverage line, no bar.
//       Click a tile → FOCUS: it takes 2 columns × 2 rows in the same grid and
//       shows its chart + stats (per-core for CPU, all SMC temperatures for
//       Thermal, measures otherwise); the other eight dim to 0.35 and go inert.
//       Thermal focuses without expanding. Click again / click elsewhere /
//       Escape unfocuses. ⚙ settings → customize tiles turns on drag-to-reorder
//       and per-tile visibility (localStorage `syswatch-tile-config`).
//   L2  a focused tile's "full details" opens the module IN PLACE (in flow, no
//       drawer): full scrollable chart + measures + top consumers + coverage.
//   L3  one toggle deeper inside L2: findings, records, per-core, the raw SMC
//       key table and the peak moments. Collapsed by default.
//
// Hover any L1 object for a 30 px mini chart of the last 60 s and the read that
// produced it.
//
// Data shape it consumes:
//   /api/current            1 s numbers (cpu, ram, gpu, disk_io, disk, net,
//                           pressure, battery, processes)
//   /api/graph?metric=&range=  series + server-computed `ticks` + `peak` +
//                           `measures`
//   /api/power + /api/power/history  5 s power ring (W, %, time remaining) and
//                           the root-tier package/CPU/GPU/ANE series
//   /api/sensors            the capability registry (every row says its source)
//   /api/consumers          per-range "who consumed it" rows per module
//   /api/apps + /api/apps/series   T17B: the application cut of the same range
//                           store (one row per app + that app's own series)
//   /api/battery            T17B: pack state, capacities (24 h cache), outlook
//   /api/analytics          the analytics payload
//
// The x axis is a plain LINEAR axis of epoch-ms with no date adapter (the
// vendored Chart.js 4.x has none), so the axis labels come from the server's
// calendar-aligned tick ladder. `chart.data.labels` is set from those same
// ticks — that is the array Chart.js reports as the axis labels, and leaving it
// empty was the T14 defect (labels.length === 0 on every chart).
//
// Every rendered number carries unit · source · window. A value this machine
// cannot read renders `needs root` / `unavailable` with the reason — never a 0,
// never a dash with no explanation.

const $ = (id) => document.getElementById(id);
const RANGES = ["1m", "5m", "15m", "1h", "6h", "1d", "7d", "30d", "1y"];
const RANGE_S = {
  "1m": 60, "5m": 300, "15m": 900, "1h": 3600, "6h": 21600, "1d": 86400,
  "7d": 604800, "30d": 2592000, "1y": 31536000,
};
const DEFAULT_RANGE = "1h";
const VIEW_IDS = ["instrument", "apps", "analytics", "knowledge", "settings"];
// The eight scrolling sections of the Instrument, in scroll order.
const SECTIONS = ["cpu", "memory", "gpu", "disk", "network", "power",
  "battery", "thermal"];
// Sections that have a per-app "who consumed it" store on the server.
const CONSUMER_SECTIONS = ["cpu", "memory", "gpu", "disk", "network", "power"];

const state = {
  range: DEFAULT_RANGE,
  view: "instrument",
  current: null,        // /api/current
  power: null,          // /api/power
  powerHistory: null,   // /api/power/history?range=
  powerSummary: null,   // /api/power/history (windows)
  graphs: {},           // metric -> /api/graph payload
  analytics: null,
  apps: null,           // /api/apps?range= → the Apps range table (app cut)
  battery: null,        // /api/battery → pack state, capacities, outlook
  batteryAt: 0,
  sensors: null,        // /api/sensors (registry, all groups)
  permissions: null,    // /api/permissions (privileges + grant state)
  filter: {},           // {app, core, bucket} — cross-filter state
  lastOk: 0,            // performance.now() of the last healthy 1 s poll
  permBusy: false,      // true while a grant/revoke prompt is in flight
  visible: {},          // section id -> true (IntersectionObserver)
  settingsOpen: false,  // the settings drawer is open (2 s permission poll)
  settingsPoll: 0,      // interval handle for that poll
  permSig: null,        // last rendered permission signature
  rangeFrom: null,      // "brush" | null — where the current range came from
  inflight: {},
  requests: 0,
};

// ---------------------------------------------------------------- formatters
function isNum(v) { return typeof v === "number" && isFinite(v); }
function na(text) { return "<span class='na'>" + (text || "unavailable") + "</span>"; }

function fmtPct(n, d) {
  if (!isNum(n)) return "—";
  return n.toFixed(d == null ? 1 : d) + "%";
}
function fmtBytes(n, d) {
  if (!isNum(n)) return "—";
  if (n === 0) return "0 B/s";
  const u = ["B/s", "KB/s", "MB/s", "GB/s"];
  let i = 0;
  while (n >= 1024 && i < u.length - 1) { n /= 1024; i++; }
  return n.toFixed(i === 0 ? 0 : (d == null ? 1 : d)) + " " + u[i];
}
function fmtGB(n) { return isNum(n) ? n.toFixed(1) + " GB" : "—"; }
function fmtMB(n) { return isNum(n) ? n.toFixed(0) + " MB" : "—"; }
function fmtW(n) { return isNum(n) ? n.toFixed(2) + " W" : "—"; }
function fmtInt(n) { return isNum(n) ? String(Math.round(n)) : "—"; }

function fmtSpan(secs) {
  if (!isNum(secs)) return "—";
  if (secs < 90) return Math.round(secs) + " s";
  if (secs < 5400) return Math.round(secs / 60) + " min";
  if (secs < 172800) return (secs / 3600).toFixed(1) + " h";
  return (secs / 86400).toFixed(1) + " d";
}
// 5:32 remaining — hours only when there are hours.
function fmtClock(secs) {
  if (!isNum(secs)) return null;
  const t = Math.max(0, Math.round(secs));
  const h = Math.floor(t / 3600);
  const m = Math.round((t % 3600) / 60);
  if (h > 0) return h + ":" + String(m).padStart(2, "0");
  return m + " min";
}
function fmtTime(epoch) {
  if (!isNum(epoch)) return "";
  const d = new Date(epoch * 1000);
  return isNaN(d.getTime()) ? "" :
    // 24 h so a card's timestamp reads the same way the axis does.
    d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit", hour12: false });
}
// UTC ISO from the API → the viewer's clock, HH:MM.
function fmtHour(iso) {
  if (!iso) return "";
  const d = new Date(iso);
  if (isNaN(d.getTime())) return "";
  return d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit", hour12: false });
}
function fmtDateTime(epoch) {
  if (!isNum(epoch)) return "";
  const d = new Date(epoch * 1000);
  if (isNaN(d.getTime())) return "";
  return d.toLocaleString(undefined, { month: "short", day: "numeric",
    hour: "2-digit", minute: "2-digit" });
}
function esc(s) {
  return String(s == null ? "" : s).replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}

// Join the parts of an attribution line, dropping the ones this sample does not
// have. A trailing "· —" is a meaningless element, so it never gets built.
function parts() {
  const out = [];
  for (let i = 0; i < arguments.length; i++) {
    const v = arguments[i];
    if (v !== null && v !== undefined && v !== "" && v !== "—") out.push(v);
  }
  return out.join(" · ");
}

// A delta is never bare: it names the baseline it is compared against, e.g.
// "+1.2 W vs 1h avg". When the two sides are equal it says so in words.
function delta(cur, base, unit, digits, baseLabel) {
  if (!isNum(cur) || !isNum(base)) return null;
  const d = cur - base;
  const step = 1 / Math.pow(10, digits == null ? 1 : digits);
  const label = baseLabel || "baseline";
  if (Math.abs(d) < step / 2) return "= " + label;
  const sign = d > 0 ? "+" : "−";
  return sign + Math.abs(d).toFixed(digits) + (unit ? " " + unit : "") + " vs " + label;
}

// One toast stack, used for a grant that flipped and for a brush that moved the
// range — so the UI never changes something without saying so.
function toast(text, kind) {
  const box = $("toasts");
  if (!box) return;
  const el = document.createElement("div");
  el.className = "toast" + (kind ? " is-" + kind : "");
  el.textContent = text;
  box.appendChild(el);
  setTimeout(() => {
    el.classList.add("is-out");
    setTimeout(() => { if (el.parentNode) el.parentNode.removeChild(el); }, 200);
  }, kind === "warn" ? 6000 : 3200);
  while (box.children.length > 3) box.removeChild(box.firstChild);
}

// A hero value: the number big, the unit small. Missing data is never 0.
function heroHtml(value, unit, word) {
  if (value === null || value === undefined) return na("no sample");
  return '<span>' + esc(value) +
    (unit ? '<span class="unit"> ' + esc(unit) + "</span>" : "") + "</span>";
}

// ------------------------------------------------------------- fetch layer
// One request per key in flight; a late response for a key that has since been
// re-requested is dropped, so switching range fast cannot paint stale data.
const seqs = {};
async function fetchJSON(key, url) {
  if (state.inflight[key]) return null;
  const seq = (seqs[key] || 0) + 1;
  seqs[key] = seq;
  state.inflight[key] = true;
  try {
    const r = await fetch(url, { cache: "no-store" });
    if (!r.ok) throw new Error("HTTP " + r.status);
    const data = await r.json();
    if (seqs[key] !== seq) return null;
    return data;
  } catch (e) {
    return null;
  } finally {
    state.inflight[key] = false;
    state.requests++;
  }
}

// ------------------------------------------------------------------ charts
// "calm instrument" palette (T17A): one accent, everything else is a grey step.
const FONT = '-apple-system, BlinkMacSystemFont, "SF Pro Text", "Segoe UI", system-ui, sans-serif';
const ACCENT = "#5EEAD4";   // the ONE accent colour
const MUTED = "#8B93A7";    // secondary series
const DIM = "#5B6273";      // axes labels, coverage
const LINE = "#262A33";     // gridlines + axis border
const WARN = "#F87171";     // warn state only
const AMBER = "#F59E0B";    // battery state only

Chart.defaults.color = DIM;
Chart.defaults.font.family = FONT;
Chart.defaults.font.size = 10;
Chart.defaults.animation = false;

// Soft gradient fill for the primary series: accent at 18 % fading to nothing.
function accentFill(ctx) {
  const chart = ctx.chart;
  const area = chart.chartArea;
  if (!area) return "rgba(94,234,212,0.12)";
  const g = chart.ctx.createLinearGradient(0, area.top, 0, area.bottom);
  g.addColorStop(0, "rgba(94,234,212,0.18)");
  g.addColorStop(1, "rgba(94,234,212,0.00)");
  return g;
}

function chartConfig() {
  return {
    type: "line",
    data: { labels: [], datasets: [] },
    options: {
      responsive: true,
      maintainAspectRatio: false,
      animation: false,
      parsing: false,
      normalized: true,
      interaction: { mode: "index", intersect: false },
      layout: { padding: { top: 4, right: 4, bottom: 0, left: 0 } },
      plugins: {
        legend: { display: false },
        tooltip: {
          displayColors: false,
          backgroundColor: "#181A1F",
          borderColor: LINE,
          borderWidth: 1,
          titleColor: MUTED,
          bodyColor: "#E6E9EF",
          callbacks: {
            title: (items) => (items.length ? fmtDateTime(items[0].parsed.x / 1000) : ""),
          },
        },
      },
      scales: {
        x: {
          type: "linear",              // no date adapter is vendored
          bounds: "data",
          grid: { color: LINE, drawTicks: false, lineWidth: 1 },
          border: { color: LINE },
          ticks: {
            autoSkip: false,
            maxRotation: 0,
            minRotation: 0,
            padding: 6,
            maxTicksLimit: 10,
            color: DIM,
            font: { size: 10 },
          },
        },
        y: {
          beginAtZero: true,
          grid: { color: LINE, drawTicks: false, lineWidth: 1 },
          border: { display: false },
          ticks: { maxTicksLimit: 4, padding: 6, color: DIM, font: { size: 10 } },
        },
      },
      elements: {
        line: { borderWidth: 1.6, tension: 0.2 },
        point: { radius: 0, hitRadius: 8 },
      },
    },
  };
}

// Always returns a Chart bound to the canvas that is on screen now. A view
// switch replaces the DOM, so an instance pointing at a detached canvas is
// destroyed rather than reused.
const CHARTS = {};
function ensureChart(canvasId) {
  const cv = $(canvasId);
  if (!cv) return null;
  let ch = CHARTS[canvasId];
  if (ch && ch.canvas !== cv) { ch.destroy(); delete CHARTS[canvasId]; ch = null; }
  if (!ch) {
    ch = new Chart(cv.getContext("2d"), chartConfig());
    CHARTS[canvasId] = ch;
  }
  return ch;
}

function destroyCharts() {
  Object.keys(CHARTS).forEach((k) => {
    try { CHARTS[k].destroy(); } catch (e) { /* already gone */ }
    delete CHARTS[k];
  });
}

function xy(points, key) {
  const out = [];
  for (const p of points || []) {
    out.push({ x: p.ts * 1000, y: isNum(p[key]) ? p[key] : null });
  }
  return out;
}

const SERIES = {
  cpu: [{ key: "total", label: "CPU %", accent: true, fill: true }],
  memory: [{ key: "pct", label: "Memory %", accent: true, fill: true }],
  gpu: [{ key: "util_pct", label: "GPU %", accent: true, fill: true },
        { key: "util_pct_max", label: "peak %", accent: false }],
  network: [{ key: "down_bps", label: "down", accent: true, fill: true },
            { key: "up_bps", label: "up", accent: false }],
  disk: [{ key: "read_bps", label: "read", accent: true, fill: true },
         { key: "write_bps", label: "write", accent: false }],
  battery: [{ key: "pct", label: "charge %", accent: true, fill: true }],
  power: [{ key: "battery_w", label: "battery W", accent: true, fill: true }],
};

const Y_FMT = {
  cpu: (v) => fmtPct(v, 0),
  memory: (v) => fmtPct(v, 0),
  gpu: (v) => fmtPct(v, 0),
  battery: (v) => fmtPct(v, 0),
  network: (v) => fmtBytes(v, 0),
  disk: (v) => fmtBytes(v, 0),
  power: (v) => (isNum(v) ? v.toFixed(1) + " W" : ""),
};

// ---------------------------------------------------------------------------
// The label fix. `ticks` is the server's calendar-aligned ladder; it becomes
// (a) chart.data.labels — the axis label array Chart.js exposes, and
// (b) the scale's own tick positions via afterBuildTicks, so the axis renders
//     exactly those labels instead of raw epoch numbers.
// ---------------------------------------------------------------------------
function applyTicks(chart, ticks) {
  const tl = (ticks || []).filter((t) => isNum(t.ts) && t.label);
  const byMs = {};
  tl.forEach((t) => { byMs[Math.round(t.ts * 1000)] = String(t.label); });
  chart.data.labels = tl.map((t) => String(t.label));
  chart.options.scales.x.afterBuildTicks = (axis) => {
    if (!tl.length) return;
    axis.ticks = tl.map((t) => ({ value: Math.round(t.ts * 1000) }));
  };
  chart.options.scales.x.ticks.callback = (v) => byMs[Math.round(v)] || "";
  return tl;
}

function paintSeries(canvasId, metric, points, ticks) {
  const ch = ensureChart(canvasId);
  if (!ch) return false;
  const sets = (SERIES[metric] || []).map((s) => ({
    s: s, data: xy(points, s.key),
  })).filter((row) => row.data.some((p) => isNum(p.y)));
  const tl = applyTicks(ch, ticks);
  ch.options.scales.x.display = true;
  ch.options.scales.y.display = true;
  ch.options.scales.y.ticks.callback = Y_FMT[metric] || ((v) => String(v));

  const xs = [];
  sets.forEach((row) => row.data.forEach((p) => { if (isNum(p.x)) xs.push(p.x); }));
  tl.forEach((t) => xs.push(t.ts * 1000));
  const dataXs = [];
  sets.forEach((row) => row.data.forEach((p) => { if (isNum(p.y)) dataXs.push(p.x); }));
  const lo = Math.min.apply(null, xs.length ? xs : [0]);
  const hi = Math.max.apply(null, xs.length ? xs : [1]);
  ch.options.scales.x.min = lo;
  ch.options.scales.x.max = hi;

  ch.data.datasets = sets.map((row) => {
    const color = row.s.accent ? ACCENT : MUTED;
    return {
      label: row.s.label,
      data: row.data,
      borderColor: color,
      backgroundColor: row.s.fill === true ? accentFill : "transparent",
      fill: row.s.fill === true,
      borderWidth: 1.6,
      borderDash: row.s.accent ? [] : [3, 3],
      spanGaps: false,
      pointRadius: 0,
      tension: 0.2,
    };
  });
  ch.update("none");
  return dataXs.length > 0;
}

// Multi-series painter for charts that overlay several units-of-measure on ONE
// axis (power: draw / adapter / package). Distinct colour + dash per series, no
// stacking — the brief forbids stacked power.
function paintMulti(canvasId, specs, points, ticks, yFmt) {
  const ch = ensureChart(canvasId);
  if (!ch) return false;
  const sets = specs.map((s) => ({ s: s, data: xy(points, s.key) }))
    .filter((row) => row.data.some((p) => isNum(p.y)));
  const tl = applyTicks(ch, ticks);
  ch.options.scales.x.display = true;
  ch.options.scales.y.display = true;
  ch.options.scales.y.ticks.callback = yFmt || ((v) => String(v));
  const xs = [];
  sets.forEach((row) => row.data.forEach((p) => { if (isNum(p.x)) xs.push(p.x); }));
  tl.forEach((t) => xs.push(t.ts * 1000));
  const lo = Math.min.apply(null, xs.length ? xs : [0]);
  const hi = Math.max.apply(null, xs.length ? xs : [1]);
  ch.options.scales.x.min = lo;
  ch.options.scales.x.max = hi;
  ch.data.datasets = sets.map((row) => ({
    label: row.s.label,
    data: row.data,
    borderColor: row.s.color || MUTED,
    backgroundColor: row.s.fill ? accentFill : "transparent",
    fill: row.s.fill === true,
    borderWidth: 1.6,
    borderDash: row.s.dash || [],
    spanGaps: false,
    pointRadius: 0,
    tension: 0.2,
  }));
  ch.update("none");
  return sets.length > 0;
}

// Empty series: axes off, labels still populated so the axis contract holds,
// and the reason line carries the explanation instead of a 0–1 axis implying
// a broken chart.
function clearChart(canvasId, ticks, reason) {
  const ch = ensureChart(canvasId);
  if (!ch) return;
  applyTicks(ch, ticks);
  ch.data.datasets = [];
  ch.options.scales.x.display = false;
  ch.options.scales.y.display = false;
  ch.update("none");
  return reason;
}

// Peak/chip text for a metric, honest about "steady".
function peakText(metric, peak) {
  if (!peak || !isNum(peak.value)) return "";
  const v = metric === "network" || metric === "disk"
    ? fmtBytes(peak.value)
    : metric === "power" ? fmtW(peak.value) : fmtPct(peak.value);
  const when = fmtTime(peak.ts);
  return (peak.is_spike ? "peak " : "steady ") + v + (when ? " · " + when : "");
}

// --------------------------------------------------------------- shell bits
// The pill is static markup in index.html (it is the page's only range control
// and it must survive without JavaScript building it). This only syncs state.
function syncRangePill() {
  const el = $("rangePill");
  if (!el) return;
  el.querySelectorAll(".range").forEach((b) => {
    b.setAttribute("aria-pressed", String(b.dataset.range === state.range));
  });
  const hint = $("rangeHint");
  if (hint) {
    hint.textContent = state.rangeFrom === "brush"
      ? "brushed · range snapped to " + state.range + " · double-click any chart to reset"
      : "drag a chart to zoom · double-click resets";
  }
}

// setRange is the ONE mutation point for time: the pill, a brushed chart and
// the keyboard all land here, so the pill can never disagree with the charts.
function setRange(r, origin) {
  if (!RANGES.includes(r)) return;
  const same = r === state.range && origin !== "brush";
  if (same) return;
  state.range = r;
  state.rangeFrom = origin === "brush" ? "brush" : null;
  syncRangePill();
  refreshView(true);
}

// Double-click anywhere on a chart: back to the default window.
function resetRange() {
  if (state.range === DEFAULT_RANGE && !state.rangeFrom) return;
  setRange(DEFAULT_RANGE);
  toast("range reset to " + DEFAULT_RANGE, "ok");
}

function syncTabs() {
  $("tabs").querySelectorAll(".tab").forEach((t) => {
    t.setAttribute("aria-selected", String(t.dataset.view === state.view));
  });
}

function setLive(ok, staleSecs) {
  const el = $("live");
  const label = $("liveLabel");
  if (ok) {
    el.classList.remove("is-stale");
    label.textContent = "live";
    el.title = "1 s /api/current poll healthy";
  } else {
    el.classList.add("is-stale");
    label.textContent = isNum(staleSecs) ? "stale " + staleSecs + "s" : "stale";
    el.title = "no successful /api/current poll for " + (staleSecs || 0) + " s";
  }
}

function markPoll(ok) {
  const now = performance.now();
  if (ok) { state.lastOk = now; setLive(true); return; }
  const age = state.lastOk ? (now - state.lastOk) / 1000 : null;
  if (age === null || age >= 5) setLive(false, age === null ? null : Math.round(age));
}

// ======================================================== sensor registry (T14B)
// Groups come from /api/sensors and render where they belong (no 11th tab: the
// design contract allows 8–10 views). A group whose only rows need root renders
// ONE amber row — never a placeholder 0 °C.
function sensorGroup(id) {
  const groups = (state.sensors || {}).groups || [];
  return groups.find((g) => g.id === id) || null;
}

function sensorById(id) {
  const all = (state.sensors || {}).sensors || [];
  return all.find((s) => s.id === id) || null;
}

function sensorText(s) {
  if (!s.available || s.value === null || s.value === undefined) return null;
  if (typeof s.value === "string") return s.value;
  const p = isNum(s.precision) ? s.precision
    : (s.unit === "%" ? 1 : s.unit === "W" ? 2 : 1);
  const num = Number(s.value);
  if (!isFinite(num)) return null;
  const txt = num.toFixed(Math.max(0, Math.min(3, p)));
  // "n" is the registry's marker for a count; the label already says what it is.
  return s.unit && s.unit !== "n" ? txt + " " + s.unit : txt;
}

function sensorRowHtml(s) {
  const txt = sensorText(s);
  const title = esc(s.reason || s.source || "");
  if (txt === null) {
    // The reason is printed, not hidden in a tooltip: a row that says nothing
    // is the exact complaint T16A §5 is about.
    const why = s.reason || s.source || "unavailable";
    return '<div class="stat-row" title="' + title + '"><span class="label">' +
      esc(s.label) + '</span><span class="val is-locked">' +
      (s.root_gated ? '<span class="warn-pill">needs root</span>' : "") +
      '<span class="note">' + esc(why) + "</span></span></div>";
  }
  return '<div class="stat-row" title="' + title + '"><span class="label">' +
    esc(s.label) + '</span><span class="val">' + esc(txt) + "</span></div>";
}

function lockedGroupHtml(g) {
  const reason = ((g.sensors || [])[0] || {}).reason || "needs root";
  const grant = (state.sensors || {}).grant || {};
  return '<div class="stat-row is-locked"><span class="label">' + esc(g.label) +
    '</span><span class="val"><span class="warn-pill">needs root</span>' +
    '<button class="grant" type="button" data-grant="' + esc(g.id) + '">Grant</button>' +
    "</span></div>" +
    '<div class="reason is-warn" data-grant-note="' + esc(g.id) + '">' +
    esc(reason) + "</div>" +
    (grant.how ? '<div class="reason" data-grant-how="' + esc(g.id) + '" hidden></div>' : "");
}

// opts.skip: sensor ids already rendered as charts/rows elsewhere in the view.
function sensorPanelHtml(gid, opts) {
  opts = opts || {};
  const g = sensorGroup(gid);
  if (!g) return "";                       // group with nothing on this device
  if (g.needs_root) return lockedGroupHtml(g);
  const skip = opts.skip || [];
  const rows = (g.sensors || []).filter((s) => !skip.includes(s.id));
  const shown = isNum(opts.limit) ? rows.slice(0, opts.limit) : rows;
  const hidden = rows.length - shown.length;
  return shown.map(sensorRowHtml).join("") +
    (hidden > 0 ? '<div class="reason">+' + hidden + " more measured rows in this group</div>" : "");
}

function wireGrants() {
  document.querySelectorAll("[data-grant]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const gid = btn.dataset.grant;
      const grant = (state.sensors || {}).grant || {};
      const note = document.querySelector('[data-grant-note="' + gid + '"]');
      const how = document.querySelector('[data-grant-how="' + gid + '"]');
      if (!grant.endpoint_available) {
        // T15 owns the grant flow. Until it is installed the honest answer is
        // the exact mechanism, not a button that 404s.
        btn.textContent = "how";
        btn.disabled = true;
        if (note) note.textContent = grant.detail || "grant flow not installed";
        if (how) {
          how.hidden = false;
          how.textContent = grant.how +
            " — one macOS admin prompt, the dashboard never runs as root";
        }
        return;
      }
      btn.disabled = true;
      btn.textContent = "granting…";
      try {
        const r = await fetch(grant.endpoint,
          { method: "POST", headers: { "X-Syswatch": "grant" } });
        const d = await r.json();
        btn.textContent = d && d.granted ? "granted" : "not granted";
        if (note) note.textContent = (d && (d.reason || d.detail)) || "";
      } catch (e) {
        btn.textContent = "failed";
        if (note) note.textContent = "grant request failed: " + e.message;
      }
    });
  });
}

// ============================================ cross-filter chips (T14B #5)
function activeFilters() {
  const f = state.filter || {};
  const out = [];
  if (f.app) out.push(["app", f.app]);
  if (f.core) out.push(["core", f.core]);
  if (f.bucket) out.push(["window", fmtTime(f.bucket)]);
  return out;
}

function renderChips() {
  const el = $("chips");
  if (!el) return;
  const active = activeFilters();
  if (!active.length) {
    el.hidden = true;
    el.innerHTML = "";
    return;
  }
  el.hidden = false;
  el.innerHTML = '<span class="micro">filtered by</span>' +
    active.map(([k, v]) =>
      '<span class="chip is-active">' + esc(k) + " · " + esc(v) +
      ' <button class="chip-x" type="button" data-clear="' + k +
      '" aria-label="remove ' + k + ' filter">✕</button></span>').join("") +
    '<button class="chip-clear" type="button" data-clear="all">clear</button>';
  el.querySelectorAll("[data-clear]").forEach((b) => {
    b.addEventListener("click", () => {
      const k = b.dataset.clear;
      if (k === "all") state.filter = {};
      else delete state.filter[k];
      renderChips();
      updateView();
      renderView();
    });
  });
}

function setFilter(patch) {
  state.filter = Object.assign({}, state.filter || {}, patch);
  renderChips();
  updateView();
}

function clearFilter() {
  if (!activeFilters().length) return;
  state.filter = {};
  renderChips();
  renderView();
}

// ======================================== drill-through drawer (T14B #5)
function closeDrawer() {
  const el = $("drawer");
  if (!el || el.hidden) return;
  el.hidden = true;
  el.setAttribute("aria-hidden", "true");
  document.body.classList.remove("has-drawer");
}

function openDrawerForApp(name) {
  const ph = state.powerHistory || {};
  const rows = ((ph.analysis || {}).per_app || {}).rows || [];
  const row = rows.find((r) => r.name === name) || null;
  const series = ph.per_app_series || {};
  const hasSeries = (series.names || []).includes(name) && (series.points || []).length;
  const cur = ((state.power || {}).by_app || []).filter((a) => a.name === name);
  const watts = cur.reduce((a, b) => a + (isNum(b.watts) ? b.watts : 0), 0);
  const live = ((state.current || {}).processes || {}).by_cpu || [];
  const proc = live.filter((p) => p.name === name);
  const cpuPct = proc.reduce((a, b) => a + (isNum(b.cpu_pct) ? b.cpu_pct : 0), 0);
  const memMb = proc.reduce((a, b) => a + (isNum(b.mem_mb) ? b.mem_mb : 0), 0);

  showDrawer({
    kind: "app detail",
    title: name,
    chart: hasSeries ? { key: name, points: series.points,
      ticks: ph.ticks || [], label: "measured W per collector sweep · " + state.range } : null,
    rows: [
      ["Energy over " + state.range, row ? row.wh.toFixed(4) + " Wh" : na("no sweeps in window")],
      ["Share of measured app energy", row && isNum(row.share_pct) ? row.share_pct.toFixed(1) + " %" : "—"],
      ["Latest sweep", row ? fmtW(row.latest_w) : na("not in the latest top 25")],
      ["Live attribution now", proc.length ? fmtW(watts) : na("not in the latest sweep")],
      ["CPU now", proc.length ? fmtPct(cpuPct, 1) : na("no live process row")],
      ["Memory now", proc.length ? fmtMB(memMb) : "—"],
      ["Processes", proc.length ? String(proc.length) : "—"],
    ],
    note: (rows.length ? "" : "no per-app measurements in this window · ") +
      ("basis: " + (((ph.analysis || {}).per_app || {}).basis || "collector sweeps")),
  });
}

function openDrawerForCore(label, index) {
  const cpu = (state.current || {}).cpu || {};
  const per = cpu.per_core_pct || [];
  const clusters = cpu.clusters || [];
  let i = 0;
  let cluster = null;
  for (let ci = 0; ci < clusters.length; ci++) {
    const c = clusters[ci];
    const base = isNum(c.index) ? c.index
      : clusters.slice(0, ci).reduce((a, x) => a + (x.cores || 0), 0);
    if (index >= base && index < base + c.cores) { cluster = c; break; }
    i = base + c.cores;
  }
  const tempId = cluster ? "cpu." + cluster.name + ".temp" : null;
  const grp = sensorGroup("cpu");
  const tempRow = tempId && grp
    ? (grp.sensors || []).find((s) => s.id === tempId) : null;
  showDrawer({
    kind: "core detail",
    title: label,
    chart: null,
    rows: [
      ["Utilisation now", isNum(per[index]) ? fmtPct(per[index], 1) : na("no sample")],
      ["Cluster", cluster ? cluster.name + " (" + cluster.cores + " cores)" : na("unknown")],
      ["Cluster temperature", tempRow && tempRow.available
        ? tempRow.value.toFixed(1) + " °C" : na("needs root — per-core temp is root-only")],
      ["Core index", String(index)],
    ],
    note: "per-core utilisation is a delta of host_processor_info on the 1 s tick; " +
      "per-core time series are not retained, so this drawer shows the live value " +
      "and the cluster it belongs to",
  });
}

function openDrawerForBucket(ts) {
  const ph = state.powerHistory || {};
  const pts = ph.points || [];
  let best = null;
  pts.forEach((p) => {
    if (!best || Math.abs(p.ts - ts) < Math.abs(best.ts - ts)) best = p;
  });
  if (!best) {
    showDrawer({ kind: "spike detail", title: fmtTime(ts),
      rows: [["sample", na("no point at that time")]],
      note: "the clicked bucket has no measured sample" });
    return;
  }
  showDrawer({
    kind: "bucket detail · " + (best.src === "history" ? "seeded from history" : "live 5 s ring"),
    title: fmtDateTime(best.ts),
    chart: null,
    rows: [
      ["battery draw", isNum(best.battery_w) ? fmtW(best.battery_w) : na("no live ring sample in this bucket")],
      ["adapter input", isNum(best.adapter_w) ? fmtW(best.adapter_w) : na("on battery / not connected")],
      ["package W (root)", isNum(best.package_w) ? fmtW(best.package_w) : na("needs root — T15 daemon")],
      ["attributed apps", isNum(best.attributed_w) ? fmtW(best.attributed_w) : na("no sweep in this bucket")],
      ["charge", isNum(best.pct) ? fmtPct(best.pct, 1) : "—"],
      ["time remaining", isNum(best.time_remaining_s) ? (fmtClock(best.time_remaining_s) || "—") : "—"],
      ["biggest app", best.top_app ? esc(best.top_app) : "—"],
      ["bucket width", isNum(best.bucket_s) ? best.bucket_s + " s" : "—"],
    ],
    note: "one bucket of the " + state.range + " window · history buckets carry only the " +
      "measured attributed W (the collector log has no battery or package field)",
  });
}

function showDrawer(spec) {
  const el = $("drawer");
  if (!el) return;
  $("drawerKind").textContent = spec.kind || "";
  $("drawerTitle").textContent = spec.title || "";
  const body = $("drawerBody");
  const chartHtml = spec.chart
    ? '<div class="chart-box"><canvas id="drawer-chart"></canvas></div>' +
      '<div class="chart-foot"><span>' + esc(spec.chart.label || "") + "</span>" +
      '<span>' + (spec.chart.points || []).length + " samples</span></div>"
    : "";
  body.innerHTML = chartHtml + statRows(spec.rows || []) +
    (spec.note ? '<div class="reason">' + esc(spec.note) + "</div>" : "");
  el.hidden = false;
  el.setAttribute("aria-hidden", "false");
  document.body.classList.add("has-drawer");
  if (spec.chart) {
    paintMulti("drawer-chart", [{ key: spec.chart.key, label: spec.chart.key,
      color: ACCENT, fill: true }], spec.chart.points, spec.chart.ticks,
      spec.chart.fmt || ((v) => (isNum(v) ? v.toFixed(2) + " W" : "")));
  }
}

// ============================================================ the hero band
// Six vitals read from the 1 s /api/current poll and the 5 s power ring. Each
// tile prints: the value (big) · its unit (small) · a delta chip that names its
// baseline · the source that measured it. No tile grows a chart — charts are
// the sections' job — and no tile prints a number the API did not measure.

function heroValue(id, value, unit) {
  const el = $("v-" + id);
  if (!el) return;
  el.className = "hero";
  el.innerHTML = heroHtml(value, unit);
}

function heroWord(id, word) {
  const el = $("v-" + id);
  if (!el) return;
  el.className = "hero is-word";
  el.innerHTML = word ? esc(word) : na("no sample");
}

function heroSub(id, text) {
  const el = $("v-" + id + "-sub");
  if (el) el.textContent = text || "";
}

function heroAttrib(id, text) {
  const el = $("v-" + id + "-attrib");
  if (el) el.textContent = text || "";
}

function heroDelta(id, text, dir) {
  const el = $("v-" + id + "-delta");
  if (!el) return;
  el.hidden = !text;
  el.textContent = text || "";
  el.className = "delta" + (dir === "up" ? " is-up" : dir === "down" ? " is-down" : "");
}

function heroDial(id, pct, level) {
  const el = $("v-" + id + "-dial");
  if (!el) return;
  const bounded = isNum(pct) ? Math.max(0, Math.min(100, pct)) : 0;
  el.style.setProperty("--p", String(bounded));
  el.className = "dial" + (level === "warning" ? " is-warn"
    : level === "critical" ? " is-hot" : "");
  const word = $("v-" + id + "-dial-word");
  if (word) {
    word.textContent = level || (isNum(pct) ? Math.round(pct) + " %" : "n/a");
  }
}

// Mean of a measured field over the plotted window — the baseline a delta chip
// names. Null when the window holds no sample of that field (never 0).
function seriesMean(points, key) {
  let sum = 0;
  let n = 0;
  (points || []).forEach((p) => { if (isNum(p[key])) { sum += p[key]; n++; } });
  return n ? sum / n : null;
}

function graphMean(metric, field) {
  const m = ((state.graphs[metric] || {}).measures || {}).fields || {};
  const f = m[field];
  return f && isNum(f.mean) ? f.mean : null;
}

function deltaDir(cur, base) {
  if (!isNum(cur) || !isNum(base)) return null;
  return cur > base ? "up" : cur < base ? "down" : null;
}

function updateHeroBand() {
  const d = state.current || {};
  const cpu = d.cpu || {};
  const ram = d.ram || {};
  const gpu = d.gpu || {};
  const batt = d.battery || {};
  const pr = d.pressure || {};
  const un = d.unavailable || {};
  const ph = state.powerHistory || {};
  const cur = ph.current || {};
  const root = ph.root || {};
  const rootLive = root.available === true;
  const rootAge = isNum(root.age_s) ? root.age_s.toFixed(0) + " s old" : "no sample yet";
  const baseline = state.range + " avg";

  // --- CPU: utilisation big, pressure in the dial --------------------------
  const cpuPct = isNum(cpu.total_pct) ? cpu.total_pct : meanOf(cpu.per_core_pct);
  heroValue("cpu", isNum(cpuPct) ? cpuPct.toFixed(1) : null, "%");
  const pc = pr.cpu || {};
  heroDial("cpu", pc.available === false ? null : pc.pct, pc.level);
  heroSub("cpu", parts(
    isNum(cpu.cores) ? cpu.cores + " logical cores" : null,
    isNum(pc.pct) ? "pressure " + pc.pct.toFixed(0) + " %" : null,
    (d.load && isNum(d.load.one)) ? "load " + d.load.one.toFixed(2) : null
  ) || (un.cpu || "waiting for the first CPU sample"));
  heroAttrib("cpu", "host_processor_info · 1 s tick · baseline: chart mean");
  const cpuMean = graphMean("cpu", "total");
  heroDelta("cpu", isNum(cpuPct) ? delta(cpuPct, cpuMean, "pp", 1, baseline) : null,
    deltaDir(cpuPct, cpuMean));

  // --- Memory: used % big, pressure in the dial ---------------------------
  heroValue("memory", isNum(ram.pct) ? ram.pct.toFixed(1) : null, "%");
  const pm = pr.memory || {};
  heroDial("memory", pm.available === false ? null : pm.pct, pm.level);
  heroSub("memory", parts(
    isNum(ram.used_bytes) && isNum(ram.total_bytes)
      ? fmtMB(ram.used_bytes / 1048576) + " of " + fmtMB(ram.total_bytes / 1048576) : null,
    pm.level ? "pressure " + pm.level +
      (isNum(pm.raw_level) ? " (kern level " + pm.raw_level + ")" : "") : null
  ) || (un.memory || "waiting for the first memory sample"));
  heroAttrib("memory", "vm_statistics64 · pressure: kern level · 1 s tick");
  const memMean = graphMean("memory", "pct");
  heroDelta("memory", isNum(ram.pct) ? delta(ram.pct, memMean, "pp", 1, baseline) : null,
    deltaDir(ram.pct, memMean));

  // --- GPU: device utilisation -------------------------------------------
  const gpuLive = gpu.available !== false && isNum(gpu.util_pct);
  heroValue("gpu", gpuLive ? gpu.util_pct.toFixed(1) : null, "%");
  heroSub("gpu", gpuLive ? parts(
    isNum(gpu.renderer_pct) ? "renderer " + gpu.renderer_pct.toFixed(0) + " %" : null,
    isNum(gpu.mem_used_mb) ? fmtMB(gpu.mem_used_mb) + " in use" : null,
    isNum(gpu.cores) ? gpu.cores + " cores" : null
  ) : (gpu.reason || un.gpu || "no GPU probe on this machine"));
  heroAttrib("gpu", "ioreg IOAccelerator · 1 s tick");
  const gpuMean = graphMean("gpu", "util_pct");
  heroDelta("gpu", gpuLive ? delta(gpu.util_pct, gpuMean, "pp", 1, baseline) : null,
    deltaDir(gpu.util_pct, gpuMean));

  // --- Battery: time remaining first, then charge and draw ----------------
  const secs = isNum(cur.time_remaining_s) ? cur.time_remaining_s : batt.time_remaining_s;
  const clock = fmtClock(secs);
  const charging = batt.charging === true || cur.charging === true;
  const draw = isNum(cur.battery_w) ? cur.battery_w : batt.battery_w;
  heroValue("battery", clock, clock ? (charging ? "to full" : "remaining") : null);
  heroSub("battery", parts(
    isNum(batt.pct) ? batt.pct.toFixed(0) + " %" : null,
    isNum(draw) ? fmtW(draw) + " draw" : null,
    charging ? "charging" : batt.charging === false ? "discharging" : null
  ) || "waiting for an AppleSmartBattery read");
  heroAttrib("battery", "ioreg AppleSmartBattery · draw = V × A");
  const drawMean = seriesMean(ph.points, "battery_w");
  heroDelta("battery", isNum(draw) ? delta(draw, drawMean, "W", 2, baseline) : null,
    deltaDir(draw, drawMean));

  // --- Package power: the root tier, live ---------------------------------
  const pkg = rootLive && isNum(root.package_w) ? root.package_w
    : isNum(cur.package_w) ? cur.package_w : null;
  heroValue("power", isNum(pkg) ? pkg.toFixed(2) : null, rootLive ? "W package" : null);
  const tier = $("v-power-tier");
  if (tier) tier.hidden = !rootLive;
  const rails = [];
  if (isNum(root.cpu_w)) rails.push("CPU " + root.cpu_w.toFixed(2));
  if (isNum(root.gpu_w)) rails.push("GPU " + root.gpu_w.toFixed(2));
  if (isNum(root.ane_w)) rails.push("ANE " + root.ane_w.toFixed(2));
  heroSub("power", rootLive && rails.length ? rails.join(" · ") + " W"
    : (root.reason || un.package_power || "needs root — no privileged sample"));
  heroAttrib("power", "powermetrics · root sample " + rootAge);
  const pkgMean = seriesMean(ph.points, "package_w");
  heroDelta("power", isNum(pkg) ? delta(pkg, pkgMean, "W", 2, baseline) : null,
    deltaDir(pkg, pkgMean));

  // --- Thermal: pressure level now; die temps need the SMC reader (T16B) --
  const thermSensor = sensorById("thermal.pressure");
  const therm = (thermSensor && thermSensor.available ? thermSensor.value : null) ||
    root.thermal_pressure || null;
  heroWord("thermal", therm ? String(therm) : null);
  // T16B emits cpu_avg_c/gpu_avg_c (SMC-derived); older keys cpu_temp_c
  // kept for compatibility. Either satisfies the chip — no stale fallback.
  const cpuC = isNum(root.cpu_temp_c) ? root.cpu_temp_c
    : isNum(root.cpu_avg_c) ? root.cpu_avg_c : null;
  const gpuC = isNum(root.gpu_temp_c) ? root.gpu_temp_c
    : isNum(root.gpu_avg_c) ? root.gpu_avg_c : null;
  heroSub("thermal", parts(
    isNum(cpuC) ? "CPU " + cpuC.toFixed(1) + " °C" : null,
    isNum(gpuC) ? "GPU " + gpuC.toFixed(1) + " °C" : null
  ) || "die temperatures · needs SMC reader");
  heroAttrib("thermal", "powermetrics · root sample " + rootAge);
}

// ------------------------------------------------------------- data loading
async function loadGraph(metric) {
  const rng = state.range;
  const data = await fetchJSON("graph:" + metric + ":" + rng,
    "/api/graph?metric=" + encodeURIComponent(metric) + "&range=" + encodeURIComponent(rng));
  if (!data) return null;
  if (data.range !== state.range) return null;   // a late answer never paints
  state.graphs[metric] = data;
  return data;
}

async function loadPowerHistory() {
  const rng = state.range;
  const data = await fetchJSON("power:hist:" + rng,
    "/api/power/history?range=" + encodeURIComponent(rng));
  if (!data || data.range !== state.range) return null;
  state.powerHistory = data;
  return data;
}

// The Instrument charts one wide CPU window plus the power timeline; the six
// tiles read their sparklines from the same /api/graph payloads, so every panel
// on L1 is the same window under the same pill.
async function loadInstrumentData() {
  // Numbers first: /api/current is one cheap read, so the tiles are populated
  // before the range windows and the root sample land.
  if (!state.current) await refreshCurrent();
  updateBento();
  await Promise.all([
    ...["cpu", "memory", "gpu", "disk", "network", "battery", "load"].map(async (m) => {
      await loadGraph(m);
      paintBentoMetric(m);
    }),
    loadPowerHistory().then(() => paintBentoMetric("power")),
    refreshPower(),
  ]);
  paintBentoCharts();
  updateBento();
  loadTopConsumers(false);
}

// Paint one metric's section chart from whatever is in state now.
function paintOneChart(metric) {
  const data = state.graphs[metric];
  const canvasId = metric + "-chart";
  if (!data || !$(canvasId)) return;
  const painted = paintSeries(canvasId, metric, data.points || [], data.ticks || []);
  if (!painted) clearChart(canvasId, data.ticks, data.unavailable_reason);
  setFoot(metric, data);
  wireBrush(canvasId);
}

// ------------------------------------------------------------- section shell
// One grammar for all eight sections: a heading with the live value on the
// right, a chart panel with a coverage footer, then the evidence blocks. Every
// id is prefixed per section — all eight sections are on the page at once, so
// nothing may be assumed unique by accident.

function secHead(id, title, note) {
  return '<div class="sec-head"><h2 id="' + id + '-title">' + esc(title) + "</h2>" +
    (note ? '<span class="micro">' + esc(note) + "</span>" : "") +
    '<span class="sec-live"><span class="hero" id="' + id + '-hero">…</span>' +
    '<span class="attrib" id="' + id + '-attrib"></span></span></div>';
}

// A page that carries its own tables does not get a live value in its heading:
// an empty "…" there is exactly the placeholder T16A §5 says to delete.
function plainHead(title, note) {
  return '<div class="sec-head"><h2>' + esc(title) + "</h2>" +
    (note ? '<span class="micro">' + esc(note) + "</span>" : "") + "</div>";
}

function chartPanel(id, label) {
  return '<div class="panel chart-panel">' +
    (label ? '<div class="micro">' + esc(label) + "</div>" : "") +
    '<div class="chart-box"><canvas id="' + id + '-chart"></canvas>' +
    '<div class="brush" id="' + id + '-brush" hidden></div></div>' +
    '<div class="chart-foot"><span id="' + id + '-src"></span>' +
    '<span id="' + id + '-cov"></span></div>' +
    '<div class="reason" id="' + id + '-reason"></div>' +
    "</div>";
}

function sectionShell(id, title, note, body) {
  return '<section class="sec" id="sec-' + id + '" data-sec="' + id + '">' +
    secHead(id, title, note) + body + "</section>";
}

// Section heading number: big value, small unit, attribution that names source
// and time. A section with no sample says why in the attribution.
function setHeader(id, value, unit, attrib) {
  const hero = $(id + "-hero");
  if (hero) hero.innerHTML = heroHtml(value, unit);
  const el = $(id + "-attrib");
  if (el) el.textContent = attrib || "";
}

function statRows(rows) {
  if (!rows.length) return "";
  return '<div class="stats">' + rows.map((r) =>
    '<div class="stat-row"><span class="label">' + esc(r[0]) + "</span>" +
    '<span class="val">' + r[1] + "</span></div>").join("") + "</div>";
}

// Server-computed measures for the primary field of a metric.
function measureRows(metric, data, fmt) {
  const m = (data && data.measures) || {};
  const fields = m.fields || {};
  const primary = m.primary || (SERIES[metric] || [{}])[0].key;
  const f = fields[primary];
  if (!f) return [["no samples in this window", na("nothing measured yet")]];
  return [
    ["mean", fmt(f.mean)],
    ["median", fmt(f.median)],
    ["p95", fmt(f.p95)],
    ["max", fmt(f.max) + (isNum(f.max_ts) ? " · " + fmtTime(f.max_ts) : "")],
    ["min", fmt(f.min)],
    ["samples", fmtInt(f.n) + " in " + state.range],
  ];
}

// Coverage footer: the source, the share of the window that was measured, and
// whether the seeded history is in play. A gap must never read as a drop.
function setFoot(id, data) {
  const src = $(id + "-src");
  const cov = $(id + "-cov");
  const reason = $(id + "-reason");
  if (src) src.textContent = (data && data.source) || "";
  if (cov) {
    const m = (data && data.measures) || {};
    const bits = [];
    if (isNum(m.coverage_pct)) {
      bits.push("measured " + m.coverage_pct.toFixed(0) + " % of " + state.range);
    }
    if (data && data.history) {
      bits.push(data.history.available === false
        ? "history: none — " + (data.history.reason || "no store field")
        : data.history.seeded + " seeded + " + (data.history.live_samples || 0) + " live samples");
    }
    cov.textContent = bits.join(" · ");
  }
  if (reason) {
    const empty = data && !(data.points || []).length;
    reason.textContent = empty
      ? (data.unavailable_reason || "no samples in this range yet")
      : (data && data.note) || "";
    reason.className = "reason" + (empty ? " is-warn" : "");
  }
}

// ======================================= consumers (T14C #3: who consumed it)
// Every module chart answers "how much"; this panel answers "who", over the
// SAME selected range. It is not a live feed: the server's range store sweeps
// every 20 s, so a slow TTL re-read is all the freshness this needs.
//
// Virtualised on purpose: the range can hold 400 processes, and rendering 400
// rows on every view update is what pushes a page over the < 1 % CPU budget.
// Only the visible window plus a small buffer lives in the DOM (~30 rows),
// swapped on scroll inside requestAnimationFrame, and rows never animate.
const CONS_ROW_H = 30;
const CONS_BUFFER = 9;
const CONS_TTL_MS = 45000;
const CONS = {};            // module -> instance bound to the DOM on screen

// Columns per module. First entry is the name column; every other entry is a
// sortable metric the server really measured for that module.
const CONS_COLS = {
  cpu: [["name", "consumer"], ["cpu_avg_pct", "avg cpu %"], ["cpu_max_pct", "peak cpu %"],
    ["share_pct", "of total %"], ["mem_peak_mb", "memory MB"]],
  memory: [["name", "consumer"], ["mem_peak_mb", "peak MB"], ["mem_avg_mb", "avg MB"],
    ["share_pct", "of total %"], ["cpu_avg_pct", "cpu avg %"]],
  gpu: [["name", "consumer"], ["energy_wh", "energy mWh"], ["share_pct", "of total %"],
    ["cpu_avg_pct", "cpu avg %"], ["mem_peak_mb", "memory MB"]],
  disk: [["name", "consumer"], ["disk_read_bps", "read B/s"], ["disk_write_bps", "write B/s"],
    ["share_pct", "of total %"], ["disk_bps", "total B/s"]],
  network: [["name", "consumer"], ["net_in_bps", "in B/s"], ["net_out_bps", "out B/s"],
    ["share_pct", "of total %"], ["net_bps", "total B/s"]],
  power: [["name", "consumer"], ["energy_wh", "energy mWh"], ["share_pct", "of total %"],
    ["cpu_avg_pct", "cpu avg %"], ["mem_peak_mb", "memory MB"]],
  all: [["name", "app"], ["cpu_avg_pct", "avg cpu %"], ["cpu_max_pct", "peak cpu %"],
    ["mem_peak_mb", "memory MB"], ["energy_wh", "energy Wh"], ["disk_bps", "disk B/s"],
    ["net_bps", "network B/s"]],
};

function fmtWh(v) {
  if (!isNum(v)) return "—";
  if (v >= 0.01) return v.toFixed(3) + " Wh";
  return (v * 1000).toFixed(2) + " mWh";
}

// A cell with no sample says "n/a" — never a bare dash that could be read as
// zero. The column header carries the unit, so the cell can stay short.
function consFmt(key, v) {
  if (key === "share_pct") return isNum(v) ? v.toFixed(1) + " %" : "n/a";
  if (!isNum(v)) return "n/a";
  if (key.indexOf("cpu_") === 0) return fmtPct(v, 1);
  if (key.indexOf("mem_") === 0) return fmtMB(v);
  // one unit per column: the header says mWh, every cell is mWh
  if (key === "energy_wh") return (v * 1000).toFixed(2) + " mWh";
  if (key.indexOf("disk_") === 0 || key.indexOf("net_") === 0) return fmtBytes(v, 1);
  return String(v);
}

function consGrid(n) {
  return "grid-template-columns:minmax(0,2.4fr) repeat(" + Math.max(1, n - 1) +
    ",minmax(0,1fr))";
}

function consumersPanelHtml(module) {
  return '<div class="panel cons" data-cons="' + esc(module) + '">' +
    '<div class="cons-top"><span class="micro" id="cons-title-' + module +
    '">consumers · loading…</span>' +
    '<input class="cons-filter" id="cons-filter-' + module + '" type="search" ' +
    'placeholder="filter by name" aria-label="filter consumers by name"></div>' +
    '<div class="cons-head" id="cons-head-' + module + '" role="row"></div>' +
    '<div class="cons-scroll" id="cons-scroll-' + module + '">' +
    '<div class="cons-canvas" id="cons-canvas-' + module + '"></div>' +
    "</div>" +
    '<div class="chart-foot"><span id="cons-src-' + module + '"></span>' +
    '<span id="cons-cov-' + module + '"></span></div>' +
    "</div>";
}

// Bind (or rebind) the panel to the DOM that is on screen right now. A view
// switch replaces the nodes, so an instance pointing at a dead canvas is
// replaced — the same rule the charts follow.
function consInstance(module) {
  const canvas = $("cons-canvas-" + module);
  const scroller = $("cons-scroll-" + module);
  const head = $("cons-head-" + module);
  if (!canvas || !scroller || !head) return null;
  let inst = CONS[module];
  if (inst && inst.canvas === canvas) return inst;
  inst = CONS[module] = {
    module: module, canvas: canvas, scroller: scroller, head: head,
    payload: null, rows: [], view: [], key: null, fetchedAt: 0,
    pending: false, sort: null, dir: -1, filter: "", raf: 0,
  };
  head.addEventListener("click", (ev) => {
    const btn = ev.target.closest("[data-sort]");
    if (!btn) return;
    const k = btn.dataset.sort;
    if (inst.sort === k) inst.dir = -inst.dir;
    else { inst.sort = k; inst.dir = k === "name" ? 1 : -1; }
    inst.scroller.scrollTop = 0;
    consApply(inst);
  });
  const input = $("cons-filter-" + module);
  if (input) {
    input.addEventListener("input", () => {
      inst.filter = input.value || "";
      inst.scroller.scrollTop = 0;
      consApply(inst);
    });
  }
  scroller.addEventListener("scroll", () => {
    if (inst.raf) return;
    inst.raf = requestAnimationFrame(() => {
      inst.raf = 0;
      paintConsBody(inst);
    });
  }, { passive: true });
  canvas.addEventListener("click", (ev) => {
    const row = ev.target.closest("[data-row]");
    if (!row) return;
    const r = inst.view[Number(row.dataset.row)];
    if (r) openDrawerForProcess(inst.module, r.pid, r.name);
  });
  canvas.addEventListener("keydown", (ev) => {
    if (ev.key !== "Enter" && ev.key !== " ") return;
    const row = ev.target.closest("[data-row]");
    if (!row) return;
    ev.preventDefault();
    const r = inst.view[Number(row.dataset.row)];
    if (r) openDrawerForProcess(inst.module, r.pid, r.name);
  });
  return inst;
}

async function consLoad(module, force) {
  const inst = consInstance(module);
  if (!inst) return;
  const key = module + ":" + state.range;
  if (!force && inst.key === key && Date.now() - inst.fetchedAt < CONS_TTL_MS) return;
  if (inst.pending) return;
  inst.pending = true;
  const d = await fetchJSON("consumers:" + key,
    "/api/consumers?module=" + encodeURIComponent(module) +
    "&range=" + encodeURIComponent(state.range) + "&limit=0");
  inst.pending = false;
  if (!d || !Array.isArray(d.rows)) return;
  inst.key = key;
  inst.fetchedAt = Date.now();
  inst.payload = d;
  inst.rows = d.rows;
  consApply(inst);
}

function consApply(inst) {
  const cols = CONS_COLS[inst.module] || CONS_COLS.cpu;
  const metric = (inst.payload || {}).metric || cols[1][0];
  const sortKey = inst.sort || metric;
  const dir = inst.sort ? inst.dir : -1;
  const q = (inst.filter || "").trim().toLowerCase();
  let rows = inst.rows.filter((r) =>
    !q || String(r.name || "").toLowerCase().indexOf(q) >= 0);
  rows = rows.slice().sort(byKey(sortKey, dir));
  inst.view = rows;
  paintConsHead(inst, cols, sortKey, dir);
  paintConsBody(inst);
  paintConsFoot(inst);
}

function paintConsHead(inst, cols, sortKey, dir) {
  inst.head.setAttribute("style", consGrid(cols.length));
  inst.head.innerHTML = cols.map(([k, label]) =>
    '<button type="button" class="cons-col' + (k === "name" ? " is-name" : "") +
    (k === sortKey ? " is-sorted" : "") + '" data-sort="' + esc(k) + '">' +
    esc(label) + (k === sortKey ? "<i>" + (dir < 0 ? "▼" : "▲") + "</i>" : "") +
    "</button>").join("");
}

function paintConsBody(inst) {
  const cols = CONS_COLS[inst.module] || CONS_COLS.cpu;
  const n = inst.view.length;
  const viewport = inst.scroller.clientHeight || 300;
  inst.canvas.style.height = n ? (n * CONS_ROW_H) + "px" : "auto";
  if (!n) {
    inst.canvas.innerHTML = '<div class="cons-empty na">' +
      (((inst.payload || {}).coverage || {}).reason ||
        "no process was measured in this range yet") + "</div>";
    return;
  }
  const first = Math.max(0, Math.floor(inst.scroller.scrollTop / CONS_ROW_H) - CONS_BUFFER);
  const count = Math.ceil(viewport / CONS_ROW_H) + CONS_BUFFER * 2;
  const last = Math.min(n, first + count);
  const grid = consGrid(cols.length);
  const out = [];
  for (let i = first; i < last; i++) {
    const r = inst.view[i];
    const cells = cols.map(([k]) => {
      if (k === "name") {
        const extra = isNum(r.processes) && r.processes > 1
          ? ' <span class="count">×' + r.processes + "</span>" : "";
        const pid = isNum(r.pid) ? "pid " + r.pid : "grouped by name";
        return '<span class="cons-name" title="' + esc(pid) + '">' +
          esc(r.name || "?") + extra + "</span>";
      }
      return '<span class="num">' + esc(consFmt(k, r[k])) + "</span>";
    }).join("");
    out.push('<div class="cons-row" role="row" tabindex="0" data-row="' + i +
      '" style="top:' + (i * CONS_ROW_H) + "px;" + grid + '">' + cells + "</div>");
  }
  inst.canvas.innerHTML = out.join("");
}

function paintConsFoot(inst) {
  const d = inst.payload || {};
  const cov = d.coverage || {};
  const title = $("cons-title-" + inst.module);
  if (title) {
    title.textContent = "consumers · " + (isNum(d.count) ? d.count : 0) +
      " processes · " + state.range;
  }
  const src = $("cons-src-" + inst.module);
  if (src) {
    src.textContent = (d.aggregation || "") + (d.unit === "Wh"
      ? " · cells shown in mWh (measured in Wh)"
      : d.unit ? " · unit: " + d.unit : "");
  }
  const covEl = $("cons-cov-" + inst.module);
  if (covEl) {
    const parts = [];
    if (d.window_start) parts.push("window " + fmtHour(d.window_start) +
      " → " + fmtHour(d.window_end));
    parts.push(isNum(cov.coverage_pct)
      ? cov.coverage_pct.toFixed(0) + " % of range measured" : "no sweeps in range");
    if (isNum(cov.sweeps)) parts.push(cov.sweeps + " sweeps @" + cov.cadence_s + " s");
    covEl.textContent = parts.join(" · ");
  }
}

function consTicks(points) {
  const n = (points || []).length;
  if (!n) return [];
  const k = Math.min(5, n);
  const out = [];
  for (let i = 0; i < k; i++) {
    const p = points[Math.round(i * (n - 1) / Math.max(1, k - 1))];
    if (p && !out.some((t) => t.ts === p.ts)) {
      out.push({ ts: p.ts, label: fmtTime(p.ts) });
    }
  }
  return out;
}

// Row click → this process's own series over the same range (drill-through).
async function openDrawerForProcess(module, pid, name) {
  const part = isNum(pid) ? "&pid=" + pid : "&name=" + encodeURIComponent(name || "");
  const d = await fetchJSON("cons-series:" + module + ":" + (pid || name) + ":" + state.range,
    "/api/consumers?module=" + encodeURIComponent(module) +
    "&range=" + encodeURIComponent(state.range) + part + "&series=1");
  const pts = (d && d.points) || [];
  const unit = (d && d.unit) || "";
  // share of total for the row that opened this drawer (never a gap to anyone)
  const inst = CONS[module];
  let row = inst && inst.payload
    ? (inst.payload.rows || []).find((r) =>
      (isNum(pid) ? r.pid === pid : r.name === name)) : null;
  if (!row && !isNum(pid) && state.apps && Array.isArray(state.apps.rows)) {
    row = state.apps.rows.find((r) => r.name === name) || null;
  }
  const yFmt = module === "cpu" ? (v) => (isNum(v) ? v.toFixed(1) + " %" : "")
    : module === "memory" ? (v) => (isNum(v) ? v.toFixed(0) + " MB" : "")
      : module === "power" || module === "gpu"
        ? (v) => (isNum(v) ? v.toFixed(2) + " W" : "")
        : (v) => fmtBytes(v, 1);
  showDrawer({
    kind: "consumer detail · " + module + " · " + state.range,
    title: name || ("pid " + pid),
    chart: pts.length ? {
      key: "value", points: pts, ticks: consTicks(pts), fmt: yFmt,
      label: ((d && d.series_label) || module) + " · " + state.range,
    } : null,
    rows: [
      ["process", esc(name || "—") + (isNum(pid) ? " · pid " + pid : "")],
      ["share of total " + ((d && d.unit) || ""),
        row && isNum(row.share_pct) ? row.share_pct.toFixed(1) + " % of total" : "—"],
      ["samples in range", String(pts.length)],
      ["measured by", '<span class="reason">' +
        esc((d && d.aggregation) || "per-process range store") + "</span>"],
      ["unit", esc(unit || "")],
      ["gpu share", '<span class="na">not attributable per process — ' +
        "no per-PID GPU time in this SDK</span>"],
    ],
    note: "one point per range-store sweep (20 s). Missing spans stay missing — " +
      "the store never interpolates a process it did not measure.",
  });
}

function mountConsumers(module) {
  // Called from every module update (including the 1 s tick), so it must be
  // free: it binds the panel once and only re-reads the range store on its own
  // slow TTL. It never repaints rows that have not changed.
  const inst = consInstance(module);
  if (!inst) return;
  consLoad(module, false);
}

// ================================================================== sections
// Every section answers the same four questions in the same order: how much
// (chart), how much over THIS window (measures + coverage footer), who consumed
// it (consumers), and what else this machine can say about it (sensors, with a
// reason on every row it cannot read).

function panelHtml(inner, micro) {
  return '<div class="panel">' +
    (micro ? '<div class="micro">' + esc(micro) + "</div>" : "") + inner + "</div>";
}

function colsHtml(a, b) {
  return '<div class="cols">' + (a || "") + (b || "") + "</div>";
}

// ===================================================================== LEGACY
// The eight stacked section builders below (cpuSectionHtml … thermalSectionHtml,
// their update* twins, and the old hero-band writers) are NOT referenced by any
// route since T17A: #instrument is the static bento in index.html and the drawer
// is built from l2BodyHtml. They are kept only because the drawer reuses their
// vocabulary (clusterBadge, clusterSpan, meanOf, POWER_SPECS, statRows,
// measureRows) and because deleting 1 400 lines of UI in the same commit that
// re-architects the page is a worse trade than leaving a clearly-marked corpse.
// Nothing here paints on screen; a follow-up cleanup task may delete it wholesale.
// ================================================================ CPU section
// Cluster helpers: the P/E split comes from hw.perflevel* (name + logicalcpu)
// and the per-core array is in LOGICAL CPU INDEX ORDER, efficiency cluster
// first on Apple Silicon — so a cluster's slice starts at its own index.
function clusterTempMap() {
  const out = {};
  const g = sensorGroup("cpu");
  ((g || {}).sensors || []).forEach((s) => {
    const m = /^cpu\.(.+)\.temp$/.exec(s.id);
    if (m && s.available && isNum(s.value)) out[m[1]] = s.value;
  });
  return out;
}

function clusterSpan(clusters, ci) {
  const c = clusters[ci] || {};
  if (isNum(c.index)) return c.index;
  let base = 0;
  for (let i = 0; i < ci; i++) base += (clusters[i].cores || 0);
  return base;
}

function meanOf(list) {
  const vals = (list || []).filter(isNum);
  return vals.length ? vals.reduce((a, b) => a + b, 0) / vals.length : null;
}

function clusterBadge(c) {
  if (c.name === "performance") return "P";
  if (c.name === "efficiency") return "E";
  return (c.name || "?").slice(0, 1).toUpperCase();
}

function cpuSectionHtml() {
  return sectionShell("cpu", "CPU", "utilisation · P/E clusters · per core",
    chartPanel("cpu", "CPU utilisation over the last " + state.range) +
    panelHtml('<div class="rings" id="cpu-rings"></div>' +
      '<div class="micro">per core · click a core for its own reading</div>' +
      '<div id="cpu-cores"></div>', "P/E clusters · index order, then each logical core") +
    consumersPanelHtml("cpu") +
    colsHtml(
      panelHtml('<div id="cpu-stats"></div>' +
        '<div class="micro" style="margin-top:12px">pressure</div>' +
        '<div id="cpu-pressure"></div>', "measures · " + state.range),
      panelHtml('<div id="cpu-sensors"></div>' +
        '<div class="micro" style="margin-top:12px">temperatures</div>' +
        '<div id="cpu-temps"></div>', "capabilities this machine reports")));
}

function updateCpuSection() {
  const d = state.current || {};
  const cpu = d.cpu || {};
  const g = state.graphs.cpu;
  const clusters = Array.isArray(cpu.clusters) ? cpu.clusters : [];
  const perCore = Array.isArray(cpu.per_core_pct) ? cpu.per_core_pct : [];
  const discoveredCores = isNum(cpu.cores) ? cpu.cores
    : (perCore.length ? perCore.length
      : clusters.reduce((a, c) => a + (c.cores || 0), 0) || null);
  const liveTotal = isNum(cpu.total_pct) ? cpu.total_pct : meanOf(perCore);

  setHeader("cpu", isNum(liveTotal) ? liveTotal.toFixed(1) : null, "%",
    parts("now · host_processor_info",
      isNum(discoveredCores) ? discoveredCores + " logical" : "core count missing",
      isNum((d.load || {}).one) ? "load " + d.load.one.toFixed(2) : null,
      state.range + " mean " + (isNum(graphMean("cpu", "total"))
        ? graphMean("cpu", "total").toFixed(1) + " %" : "—")));

  const temps = clusterTempMap();
  const picked = (state.filter || {}).core || null;

  // -- per-cluster rings (P/E). The per-core array is in LOGICAL CPU INDEX
  // ORDER (efficiency first on Apple Silicon), so each slice starts at the
  // cluster's own `index` — nothing assumes "the first N cores are performance".
  const holder = $("cpu-rings");
  if (holder) {
    if (!clusters.length) {
      holder.innerHTML = "<span class='na'>P/E split unavailable — " +
        "cpu.clusters is empty (hw.perflevel0/1.name + logicalcpu unreadable)</span>";
    } else {
      holder.innerHTML = clusters.map((c, ci) => {
        const base = clusterSpan(clusters, ci);
        const slice = perCore.slice(base, base + (c.cores || 0));
        const avg = isNum(c.util_pct) ? c.util_pct : meanOf(slice);
        const badge = clusterBadge(c);
        const cls = avg == null ? "" : avg >= 85 ? "is-hot" : avg >= 60 ? "is-warn" : "is-ok";
        const temp = temps[c.name];
        const selected = picked === c.name || (picked && picked.indexOf(badge) === 0);
        const label = (c.name ? c.name.charAt(0).toUpperCase() + c.name.slice(1) : "CPU") +
          " · " + c.cores + " cores";
        const ticks = slice.map((v, n) => {
          const h = Math.max(2, Math.min(100, isNum(v) ? v : 0));
          const tcls = !isNum(v) ? "" : v >= 85 ? "is-hot" : v >= 60 ? "is-warn" : "";
          return '<i class="ring-tick ' + tcls + '" style="height:' + h + '%" ' +
            'title="' + esc(badge + (n + 1) + " · " +
              (isNum(v) ? v.toFixed(0) + " %" : "not sampled")) + '"></i>';
        }).join("");
        return '<button type="button" class="ring' + (selected ? " is-selected" : "") +
          '" data-filter-core="' + esc(c.name) + '" aria-label="filter by ' + esc(c.name) +
          ' cluster">' +
          '<span class="ring-dial ' + cls + '" style="--p:' +
          Math.max(0, Math.min(100, isNum(avg) ? avg : 0)) + '">' +
          '<span class="ring-val">' + (isNum(avg) ? avg.toFixed(0) + " %" : "—") + "</span></span>" +
          '<span class="ring-label">' + esc(label) + "</span>" +
          '<span class="ring-sub">cpu ' + base + "–" + (base + c.cores - 1) +
          (isNum(c.freq_mhz) ? " · " + Math.round(c.freq_mhz) + " MHz" : "") + "</span>" +
          '<span class="ring-ticks" aria-hidden="true">' + ticks + "</span>" +
          '<span class="ring-sub">' + (isNum(temp)
            ? "cluster " + temp.toFixed(1) + " °C"
            : '<span class="chip">cluster °C · needs root</span>') +
          "</span></button>";
      }).join("");
      holder.querySelectorAll("[data-filter-core]").forEach((b) => {
        b.addEventListener("click", () => {
          const name = b.dataset.filterCore;
          setFilter({ core: state.filter.core === name ? null : name });
        });
      });
    }
  }

  // -- per-core rows, labelled from each cluster's span
  const cores = $("cpu-cores");
  if (cores) {
    if (!perCore.length) {
      cores.innerHTML = "<span class='na'>per-core utilisation unavailable — " +
        "cpu.per_core_pct missing in /api/current</span>";
    } else {
      const labels = [];
      const names = [];
      clusters.forEach((c, ci) => {
        const badge = clusterBadge(c);
        const base = clusterSpan(clusters, ci);
        for (let n = 0; n < (c.cores || 0); n++) {
          labels[base + n] = badge + (n + 1);
          names[base + n] = c.name;
        }
      });
      cores.innerHTML = perCore.map((v, i) => {
        const name = labels[i] || "core " + (i + 1);
        const cls = v >= 85 ? "is-hot" : v >= 60 ? "is-warn" : "";
        const dim = picked && names[i] !== picked && name.indexOf(picked) !== 0;
        return '<button type="button" class="core-row' + (dim ? " is-dim" : "") +
          '" data-core-index="' + i + '" data-core-label="' + esc(name) + '">' +
          "<span>" + esc(name) + "</span>" +
          '<span class="core-bar"><i class="' + cls + '" style="width:' +
          Math.max(0, Math.min(100, v || 0)) + '%"></i></span>' +
          '<span class="val">' + (isNum(v) ? v.toFixed(0) + " %" : "—") + "</span></button>";
      }).join("");
      cores.querySelectorAll("[data-core-index]").forEach((b) => {
        b.addEventListener("click", () => {
          const label = b.dataset.coreLabel;
          setFilter({ core: state.filter.core === label ? null : label });
          openDrawerForCore(label, Number(b.dataset.coreIndex));
        });
      });
    }
  }

  const stats = $("cpu-stats");
  if (stats) stats.innerHTML = statRows(measureRows("cpu", g, (v) => fmtPct(v, 1)));

  const pg = $("cpu-pressure");
  if (pg) {
    const p = (d.pressure || {}).cpu || {};
    pg.innerHTML = p.available
      ? statRows([
        ["pressure", fmtPct(p.pct, 0) + " (" + esc(p.level) + ")"],
        ["load / core", isNum(p.load_per_core) ? p.load_per_core.toFixed(2) + " · derived" : "—"],
      ]) + '<div class="reason">' + esc(p.derivation || p.source || "") + "</div>"
      : statRows([["pressure", na(p.reason || "no pressure source")]]);
  }

  const sens = $("cpu-sensors");
  if (sens) {
    const dupes = ((sensorGroup("cpu") || {}).sensors || [])
      .filter((x) => /^cpu\.(efficiency|performance)\./.test(x.id))
      .map((x) => x.id);
    sens.innerHTML = (sensorPanelHtml("cpu", {
      skip: ["cpu.total", "cpu.cores"].concat(dupes),
    }) || "<span class='na'>no cpu capability rows</span>") +
      sensorPanelHtml("clocks");
  }
  const therm = $("cpu-temps");
  if (therm) {
    const thermal = sensorGroup("thermal");
    therm.innerHTML = sensorPanelHtml("thermal") ||
      (thermal && thermal.needs_root
        ? lockedGroupHtml(thermal)
        : "<span class='na'>die temperatures · needs SMC reader (T16B)</span>");
  }
  mountConsumers("cpu");
  wireGrants();
}

// ============================================================= Memory section
function memorySectionHtml() {
  return sectionShell("memory", "Memory", "used / total · pressure",
    chartPanel("memory", "Memory in use over the last " + state.range) +
    consumersPanelHtml("memory") +
    colsHtml(
      panelHtml('<div id="memory-stats"></div>', "measures · " + state.range),
      panelHtml('<div id="memory-detail"></div>', "detail · pressure level")));
}

function updateMemorySection() {
  const d = state.current || {};
  const ram = d.ram || {};
  const p = (d.pressure || {}).memory || {};
  setHeader("memory", isNum(ram.pct) ? ram.pct.toFixed(1) : null, "%",
    parts("now · vm_statistics64",
      pm_used_of_total(ram),
      p.level ? "pressure " + p.level : null,
      isNum(graphMean("memory", "pct")) ? state.range + " mean " +
        graphMean("memory", "pct").toFixed(1) + " %" : null));

  const stats = $("memory-stats");
  if (stats) stats.innerHTML = statRows(measureRows("memory", state.graphs.memory,
    (v) => fmtPct(v, 1)));

  const detail = $("memory-detail");
  if (detail) {
    detail.innerHTML = statRows([
      ["pressure level", p.level
        ? esc(p.level) + (isNum(p.raw_level) ? " (kern level " + p.raw_level + ")" : "")
        : na(p.reason || "no pressure source")],
      ["used", isNum(ram.used_bytes) ? fmtMB(ram.used_bytes / 1048576) : na("not reported")],
      ["installed", isNum(ram.total_bytes) ? fmtMB(ram.total_bytes / 1048576) : na("not reported")],
      ["swap used", isNum(ram.swap_used_bytes) ? fmtMB(ram.swap_used_bytes / 1048576) : na("swap not reported")],
    ]) + '<div class="reason">source: ' + esc(p.source || "vm_statistics64") + "</div>";
  }
  mountConsumers("memory");
  wireGrants();
}

function pm_used_of_total(ram) {
  return isNum(ram.used_bytes) && isNum(ram.total_bytes)
    ? fmtMB(ram.used_bytes / 1048576) + " of " + fmtMB(ram.total_bytes / 1048576)
    : null;
}

// ================================================================ GPU section
function gpuSectionHtml() {
  return sectionShell("gpu", "GPU", "utilisation · device · Neural Engine",
    chartPanel("gpu", "GPU utilisation over the last " + state.range) +
    consumersPanelHtml("gpu") +
    colsHtml(
      panelHtml('<div id="gpu-stats"></div>', "measures · " + state.range),
      panelHtml('<div id="gpu-detail"></div>', "device · Neural Engine (ANE)")));
}

function updateGpuSection() {
  const gpu = (state.current || {}).gpu || {};
  const un = (state.current || {}).unavailable || {};
  setHeader("gpu", gpu.available !== false && isNum(gpu.util_pct)
    ? gpu.util_pct.toFixed(1) : null, "%",
    gpu.available === false
      ? (gpu.reason || "no GPU probe")
      : parts("now · ioreg IOAccelerator",
        isNum(gpu.cores) ? gpu.cores + " cores" : null,
        isNum(graphMean("gpu", "util_pct"))
          ? state.range + " mean " + graphMean("gpu", "util_pct").toFixed(1) + " %" : null));

  const stats = $("gpu-stats");
  if (stats) stats.innerHTML = statRows(measureRows("gpu", state.graphs.gpu,
    (v) => fmtPct(v, 0)));

  const neUtil = sensorById("gpu.neural_engine");
  const nePower = sensorById("gpu.neural_engine_w");
  const freq = sensorById("gpu.freq");
  const neW = nePower && nePower.available ? sensorText(nePower) : null;
  const neU = neUtil && neUtil.available ? sensorText(neUtil) : null;
  const detail = $("gpu-detail");
  if (detail) {
    detail.innerHTML = statRows([
      ["utilisation", isNum(gpu.util_pct) ? fmtPct(gpu.util_pct, 0) : na("no sample")],
      ["renderer", isNum(gpu.renderer_pct) ? fmtPct(gpu.renderer_pct, 0) : na("not reported")],
      ["tiler", isNum(gpu.tiler_pct) ? fmtPct(gpu.tiler_pct, 0) : na("not reported")],
      ["memory in use", isNum(gpu.mem_used_mb) ? fmtMB(gpu.mem_used_mb) : na("not reported")],
      ["cores", isNum(gpu.cores) ? fmtInt(gpu.cores) : na("not reported")],
      ["frequency", sensorText(freq || {}) || '<span class="warn-pill">needs root</span>'],
      // T16A §3: the root-tier ANE row, live from the powermetrics sample.
      ["Neural Engine (ANE)", (neW
        ? "<b>" + esc(neW) + "</b> · "
        : '<span class="warn-pill">needs root</span> ') +
        (neU ? "util " + esc(neU) : '<span class="note">util needs root (ANEMetrics)</span>')],
    ]) + '<div class="reason">ANE power: ' +
      esc((nePower && nePower.source) || "powermetrics ANE Power") +
      (nePower && nePower.available ? "" : " · " +
        esc((nePower && nePower.reason) || un.neural_engine || "no privileged sample")) +
      "</div>";
  }
  mountConsumers("gpu");
  wireGrants();
}

// =============================================================== Disk section
function diskSectionHtml() {
  return sectionShell("disk", "Disk", "read / write throughput",
    chartPanel("disk", "Disk throughput over the last " + state.range +
      " · solid read, dashed write") +
    consumersPanelHtml("disk") +
    colsHtml(
      panelHtml('<div id="disk-stats"></div>', "speeds · " + state.range),
      panelHtml('<div id="disk-cap"></div>' +
        '<table class="table"><thead><tr><th>Volume</th><th class="num">Used</th>' +
        '<th class="num">Free</th></tr></thead><tbody id="disk-vols"></tbody></table>',
        "capacity · volumes")));
}

function updateDiskSection() {
  const d = state.current || {};
  const io = d.disk_io || {};
  const disk = d.disk || {};
  const total = (isNum(io.read_bps) ? io.read_bps : 0) + (isNum(io.write_bps) ? io.write_bps : 0);
  setHeader("disk", io.available === false ? null : fmtBytes(total), "",
    io.available === false
      ? (io.reason || "measuring…")
      : parts("now · IOBlockStorageDriver byte deltas over 5 s",
        "read " + fmtBytes(io.read_bps), "write " + fmtBytes(io.write_bps)));

  const stats = $("disk-stats");
  if (stats) {
    const g = state.graphs.disk || {};
    const f = (g.measures || {}).fields || {};
    const row = (label, k, fmt) => f[k]
      ? [label, fmt(f[k].mean) + " avg · " + fmt(f[k].max) + " max"]
      : [label, na("no samples")];
    const rows = [
      row("read", "read_bps", fmtBytes),
      row("write", "write_bps", fmtBytes),
      ["read IOPS", f.read_iops ? fmtInt(f.read_iops.mean) + " avg · " + fmtInt(f.read_iops.max) + " max" : na("no samples")],
      ["write IOPS", f.write_iops ? fmtInt(f.write_iops.mean) + " avg · " + fmtInt(f.write_iops.max) + " max" : na("no samples")],
    ];
    stats.innerHTML = statRows(rows);
  }

  const cap = $("disk-cap");
  if (cap) {
    cap.innerHTML = disk.available === false
      ? statRows([["capacity", na(disk.reason || "unavailable")]])
      : statRows([
        ["free", fmtGB(disk.free_gb)],
        ["used", fmtPct(disk.used_pct, 1)],
        ["total", fmtGB(disk.total_gb)],
      ]) + '<div class="reason">measured on ' + esc(disk.mount || "/") + "</div>";
  }

  const vols = $("disk-vols");
  if (vols) {
    const rows = Array.isArray(disk.volumes) ? disk.volumes : [];
    vols.innerHTML = rows.length
      ? rows.map((v) => "<tr><td class='name'>" + esc(v.mount || v.device || "?") +
        "</td><td class='num'>" + (isNum(v.used_pct) ? fmtPct(v.used_pct, 1) : na(v.reason || "not reported")) +
        "</td><td class='num'>" + fmtGB(v.free_gb) + "</td></tr>").join("")
      : '<tr><td colspan="3" class="na">no volumes reported</td></tr>';
  }
  mountConsumers("disk");
  wireGrants();
}

// ============================================================ Network section
function networkSectionHtml() {
  return sectionShell("network", "Network", "per-interface rate",
    chartPanel("network", "Network rate over the last " + state.range +
      " · solid down, dashed up") +
    consumersPanelHtml("network") +
    colsHtml(
      panelHtml('<div id="network-stats"></div>', "measures · " + state.range),
      panelHtml('<table class="table"><thead><tr><th>Interface</th>' +
        '<th class="num">Down</th><th class="num">Up</th></tr></thead>' +
        '<tbody id="network-table"></tbody></table>', "interfaces now")));
}

function updateNetworkSection() {
  const net = (state.current || {}).net || {};
  setHeader("network", fmtBytes(net.down_bps), "down",
    parts("now · per-interface byte deltas", "up " + fmtBytes(net.up_bps),
      isNum(graphMean("network", "down_bps"))
        ? state.range + " average down " + fmtBytes(graphMean("network", "down_bps")) : null));

  const stats = $("network-stats");
  if (stats) {
    const g = state.graphs.network || {};
    const f = (g.measures || {}).fields || {};
    const row = (label, k) => f[k]
      ? [label, fmtBytes(f[k].mean) + " avg · " + fmtBytes(f[k].max) + " max"]
      : [label, na("no samples")];
    stats.innerHTML = statRows([row("down", "down_bps"), row("up", "up_bps")]);
  }

  const tbody = $("network-table");
  if (tbody) {
    const ifaces = Array.isArray(net.ifaces) ? net.ifaces : [];
    tbody.innerHTML = ifaces.length
      ? ifaces.map((i) => "<tr><td>" + esc(i.name || "?") + "</td><td class='num'>" +
        fmtBytes(i.down_bps) + "</td><td class='num'>" + fmtBytes(i.up_bps) +
        "</td></tr>").join("")
      : '<tr><td colspan="3" class="na">no interface data</td></tr>';
  }
  mountConsumers("network");
  wireGrants();
}

// ============================================================ Battery section
function batterySectionHtml() {
  return sectionShell("battery", "Battery", "time first · then charge, draw, health",
    chartPanel("battery", "Charge over the last " + state.range) +
    colsHtml(
      panelHtml('<div id="battery-detail"></div>', "pack"),
      panelHtml('<div id="battery-stats"></div>', "measures · " + state.range +
        " · per-app attribution lives in Power")));
}

function updateBatterySection() {
  const d = state.current || {};
  const b = d.battery || {};
  const cur = (state.powerHistory || {}).current || {};
  const secs = isNum(cur.time_remaining_s) ? cur.time_remaining_s : b.time_remaining_s;
  const clock = fmtClock(secs);
  const charging = b.charging === true || cur.charging === true;
  setHeader("battery", clock, clock ? (charging ? "to full" : "remaining") : "",
    clock ? parts("now · ioreg AppleSmartBattery",
      isNum(b.pct) ? fmtPct(b.pct, 0) : null,
      charging ? "charging" : b.charging === false ? "discharging" : null,
      isNum(b.battery_w) ? fmtW(b.battery_w) + " draw" : null)
      : "battery time estimate not available yet");

  const detail = $("battery-detail");
  if (detail) {
    detail.innerHTML = statRows([
      ["charge", isNum(b.pct) ? fmtPct(b.pct, 0) : na("not reported")],
      ["time " + (charging ? "to full" : "remaining"),
        clock || na("estimating…")],
      ["draw", isNum(b.battery_w) ? fmtW(b.battery_w) : na("not reported")],
      ["voltage", isNum(b.voltage_v) ? b.voltage_v.toFixed(2) + " V" : na("not reported")],
      ["amperage", isNum(b.amperage_ma) ? fmtInt(b.amperage_ma) + " mA" : na("not reported")],
      ["health", isNum(b.health_pct) ? fmtPct(b.health_pct, 1) + " of design capacity" : na("not reported")],
      ["cycles", isNum(b.cycles) ? fmtInt(b.cycles) + " cycles" : na("not reported")],
      ["condition", b.condition ? esc(b.condition) : na("not reported")],
      ["temperature", isNum(b.temp_c) ? b.temp_c.toFixed(1) + " °C" : na("key absent on this battery")],
      ["cells", Array.isArray(b.cells) && b.cells.length
        ? esc(b.cells.join(" / ")) + " mV" : na("not exposed")],
    ]);
  }
  const stats = $("battery-stats");
  if (stats) stats.innerHTML = statRows(measureRows("battery", state.graphs.battery,
    (v) => fmtPct(v, 1)));
  // The API's footnote says "time-to-empty" for both fields; while charging the
  // same field is time-to-full, so the page must not repeat the wrong word.
  const rz = $("battery-reason");
  if (rz && charging && rz.textContent.indexOf("time-to-empty") >= 0) {
    rz.textContent = rz.textContent.replace("time-to-empty", "time-to-full");
  }
  wireGrants();
}

// ============================================================== Apps section
// The APPLICATION cut of the range store. The per-module consumer lists answer
// "who ate the CPU"; this view answers "where did the time, memory, disk,
// network and energy go, by application, over this window?" — same store, same
// numbers, one row per app name.
//
// It is a RANGE read, not a live feed: it refreshes only when the range (or the
// view) changes, and the whole list is virtualised (~30 DOM rows + buffer), so
// the 300th app is reachable and no top-5 cap can hide it.
const APPS_ROW_H = 34;
const APPS_BUFFER = 9;
const APPS_TTL_MS = 45000;
// [key, header label, numeric?] — every numeric column carries its unit.
const APPS_COLS = [["name", "application", false],
  ["cpu_avg_pct", "avg CPU %", true], ["cpu_max_pct", "peak CPU %", true],
  ["mem_peak_mb", "memory MB", true], ["energy_wh", "energy Wh", true],
  ["disk_read_mb", "read MB", true], ["disk_write_mb", "write MB", true],
  ["net_mb", "network MB", true], ["share_pct", "share of total", true]];
const APPS_SORT = { key: "energy_wh", dir: -1 };
const APPS = { canvas: null, scroller: null, head: null, filter: "",
  view: [], raf: 0, fetchedAt: 0, key: null };

// Shared comparator for the Apps table and the Consumers panels: numeric keys
// sort by value with nulls last, the name key sorts alphabetically.
function byKey(key, dir) {
  return (a, b) => {
    if (key === "name") {
      return dir < 0
        ? String(b.name || "").localeCompare(String(a.name || ""))
        : String(a.name || "").localeCompare(String(b.name || ""));
    }
    const av = a[key];
    const bv = b[key];
    const an = isNum(av);
    const bn = isNum(bv);
    if (an && !bn) return -1;
    if (!an && bn) return 1;
    if (!an && !bn) return String(a.name || "").localeCompare(String(b.name || ""));
    return dir < 0 ? bv - av : av - bv;
  };
}

function appsGrid() {
  return "grid-template-columns:minmax(0,2.2fr) repeat(8,minmax(0,1fr)) 96px";
}

function appsHeadHtml() {
  const cols = APPS_COLS.map(([k, label]) => {
    return '<button type="button" class="cons-col' + (k === "name" ? " is-name" : "") +
      (APPS_SORT.key === k ? " is-sorted" : "") + '" data-app-sort="' + esc(k) + '">' +
      esc(label) + (APPS_SORT.key === k
        ? "<i>" + (APPS_SORT.dir < 0 ? "▼" : "▲") + "</i>" : "") + "</button>";
  });
  cols.push('<span class="cons-col is-name app-shape-h">shape</span>');
  return cols.join("");
}

// One cell's text. A column with no sample says "n/a" — the header carries the
// unit, and 0 never stands in for "not measured".
function appsFmt(key, v) {
  if (!isNum(v)) return null;
  if (key === "cpu_avg_pct" || key === "cpu_max_pct") return fmtPct(v, 1);
  if (key === "mem_peak_mb") return fmtMB(v);
  if (key === "energy_wh") return fmtWh(v);
  if (key === "disk_read_mb" || key === "disk_write_mb" || key === "net_mb") {
    return v >= 100 ? v.toFixed(0) + " MB"
      : v >= 1 ? v.toFixed(2) + " MB" : (v * 1000).toFixed(0) + " KB";
  }
  if (key === "share_pct") return v.toFixed(1) + " %";
  return String(v);
}

function renderAppsSection() {
  $("view").innerHTML =
    plainHead("Apps", "where the energy, time, memory, disk and network went — by application, over the selected range") +
    '<div class="panel apps-panel">' +
    '<div class="cons-top"><span class="micro" id="apps-title">applications · loading…</span>' +
    '<input class="cons-filter" id="apps-filter" type="search" ' +
    'placeholder="filter by name" aria-label="filter applications by name"></div>' +
    // the head lives INSIDE the scroller so it shares the row content box: a
    // separate head drifts by the scrollbar width (0 px with overlay bars, 11 px
    // with classic ones), and a sorting table must line up with its columns.
    '<div class="cons-scroll apps-scroll" id="apps-scroll">' +
    '<div class="cons-head apps-head" id="apps-head" role="row" ' +
    'style="' + appsGrid() + '"></div>' +
    '<div class="cons-canvas" id="apps-canvas"></div></div>' +
    '<div class="chart-foot"><span id="apps-src"></span><span id="apps-cov"></span></div>' +
    '<div class="reason" id="apps-basis"></div>' +
    "</div>";
  APPS.canvas = null;                 // force a rebind to the new DOM
  const inst = appsInstance();
  if (inst) paintApps();
}

function appsInstance() {
  const canvas = $("apps-canvas");
  const scroller = $("apps-scroll");
  const head = $("apps-head");
  if (!canvas || !scroller || !head) return null;
  if (APPS.canvas === canvas) return APPS;
  APPS.canvas = canvas;
  APPS.scroller = scroller;
  APPS.head = head;
  head.addEventListener("click", (ev) => {
    const btn = ev.target.closest("[data-app-sort]");
    if (!btn) return;
    const k = btn.dataset.appSort;
    if (APPS_SORT.key === k) APPS_SORT.dir = -APPS_SORT.dir;
    else { APPS_SORT.key = k; APPS_SORT.dir = k === "name" ? 1 : -1; }
    scroller.scrollTop = 0;
    paintApps();
  });
  const input = $("apps-filter");
  if (input) {
    input.value = APPS.filter || "";
    input.addEventListener("input", () => {
      APPS.filter = input.value || "";
      scroller.scrollTop = 0;
      paintApps();
    });
  }
  scroller.addEventListener("scroll", () => {
    if (APPS.raf) return;
    APPS.raf = requestAnimationFrame(() => { APPS.raf = 0; paintAppsBody(); });
  }, { passive: true });
  const open = (row) => {
    const r = APPS.view[Number(row.dataset.row)];
    if (r) openAppL2(r.name);
  };
  canvas.addEventListener("click", (ev) => {
    const row = ev.target.closest("[data-row]");
    if (row) open(row);
  });
  canvas.addEventListener("keydown", (ev) => {
    if (ev.key !== "Enter" && ev.key !== " ") return;
    const row = ev.target.closest("[data-row]");
    if (!row) return;
    ev.preventDefault();
    open(row);
  });
  return APPS;
}

// Sort + filter, then paint the visible slice. Never sorts in place (the
// payload order is the server's energy ranking and stays untouched).
function paintApps() {
  const inst = APPS.canvas ? APPS : appsInstance();
  if (!inst) return;
  const d = state.apps || {};
  const q = (inst.filter || "").trim().toLowerCase();
  let rows = (d.rows || []).filter((r) =>
    !q || String(r.name || "").toLowerCase().indexOf(q) >= 0);
  rows = rows.slice().sort(byKey(APPS_SORT.key, APPS_SORT.dir));
  inst.view = rows;
  if (inst.head) {
    inst.head.setAttribute("style", appsGrid());
    inst.head.innerHTML = appsHeadHtml();
  }
  paintAppsBody();
  paintAppsFoot();
}

function paintAppsBody() {
  const inst = APPS;
  if (!inst.canvas) return;
  const d = state.apps || {};
  const n = inst.view.length;
  if (!n) {
    inst.canvas.style.height = "auto";
    inst.canvas.innerHTML = '<div class="cons-empty na">' +
      esc((d.rows && d.rows.length)
        ? "no application matches that filter"
        : (d.empty_text || "no sweep in this range yet")) + "</div>";
    return;
  }
  const viewport = inst.scroller.clientHeight || 420;
  inst.canvas.style.height = (n * APPS_ROW_H) + "px";
  const first = Math.max(0, Math.floor(inst.scroller.scrollTop / APPS_ROW_H) - APPS_BUFFER);
  const count = Math.ceil(viewport / APPS_ROW_H) + APPS_BUFFER * 2;
  const last = Math.min(n, first + count);
  const grid = appsGrid();
  const out = [];
  for (let i = first; i < last; i++) {
    const r = inst.view[i];
    const cells = APPS_COLS.map(([k]) => {
      if (k === "name") {
        return '<span class="cons-name" title="' + esc(r.name || "") + '">' +
          esc(r.name || "?") +
          (isNum(r.processes) && r.processes > 1
            ? ' <span class="count">×' + r.processes + "</span>" : "") + "</span>";
      }
      const txt = appsFmt(k, r[k]);
      return '<span class="num">' + (txt == null ? '<span class="na">n/a</span>' : esc(txt)) +
        "</span>";
    }).join("");
    out.push('<div class="cons-row app-row' +
      (((state.filter || {}).app === r.name) ? " is-picked" : "") +
      '" role="row" tabindex="0" data-row="' + i +
      '" style="top:' + (i * APPS_ROW_H) + "px;" + grid + '">' + cells +
      '<canvas class="app-spark" data-spark="' + i + '" height="22" aria-hidden="true"></canvas>' +
      "</div>");
  }
  inst.canvas.innerHTML = out.join("");
  // sparklines last: they read the row's own bucketed watts (a gap stays a gap)
  const meta = d.spark || {};
  inst.canvas.querySelectorAll("[data-spark]").forEach((cv) => {
    const r = inst.view[Number(cv.dataset.spark)];
    const pts = appSparkPoints(r, meta);
    if (pts.length > 1) drawSparkCanvas(cv, pts, "v");
  });
}

function appSparkPoints(row, meta) {
  if (!row || !Array.isArray(row.spark) || !meta || !isNum(meta.start_ts)) return [];
  const step = isNum(meta.bucket_s) && meta.bucket_s > 0 ? meta.bucket_s : 20;
  const out = [];
  row.spark.forEach((v, i) => {
    if (isNum(v)) out.push({ ts: meta.start_ts + i * step, v: v });
  });
  return out;
}

function paintAppsFoot() {
  const d = state.apps || {};
  const cov = d.coverage || {};
  const title = $("apps-title");
  if (title) {
    title.textContent = "applications · " + (isNum(d.count) ? d.count : 0) +
      " measured over " + state.range +
      (APPS.filter ? " · filtered to “" + APPS.filter + "”" : "");
  }
  const src = $("apps-src");
  if (src) {
    src.textContent = (d.basis || "") +
      (d.share_basis ? " · " + d.share_basis : "") +
      " · energy cells print mWh under 0.01 Wh";
  }
  const covEl = $("apps-cov");
  if (covEl) {
    const bits = [];
    if (d.window_start) {
      bits.push("window " + fmtHour(d.window_start) + " → " + fmtHour(d.window_end));
    }
    bits.push(isNum(cov.coverage_pct)
      ? "measured " + cov.coverage_pct.toFixed(0) + " % of " + state.range
      : "no sweeps in range");
    if (isNum(cov.sweeps)) bits.push(cov.sweeps + " sweeps @" + cov.cadence_s + " s");
    if (d.totals && isNum(d.totals.energy_wh)) {
      bits.push("Σ energy " + d.totals.energy_wh.toFixed(4) + " Wh");
    }
    covEl.textContent = bits.join(" · ");
  }
  const basis = $("apps-basis");
  if (basis) {
    basis.textContent = (d.cross_module || "") +
      " · not a live reading: one read per range change · click a row for that app's own series";
  }
}

// The Apps view is a range read: it re-reads only when the range (or the view)
// changes, and never on the 1 s / 5 s poll. A failed read keeps its reason on
// screen — "the endpoint did not answer" is not the same statement as "the
// store has no sweep".
async function loadApps(force) {
  const key = "apps:" + state.range;
  if (!force && APPS.key === key && Date.now() - APPS.fetchedAt < APPS_TTL_MS) return;
  let d = null;
  let err = null;
  try {
    const r = await fetch("/api/apps?range=" + encodeURIComponent(state.range),
      { cache: "no-store" });
    if (!r.ok) err = "HTTP " + r.status;
    else d = await r.json();
  } catch (e) {
    err = String((e && e.message) || e);
  }
  if (!d) {
    state.apps = { rows: [], count: 0, range: state.range, error: err,
      empty_text: "/api/apps did not answer (" + err + ") — this page is newer " +
        "than the running dashboard process; restart it to serve the range store." };
    paintApps();
    return;
  }
  if (d.range !== state.range) return;
  APPS.key = key;
  APPS.fetchedAt = Date.now();
  state.apps = d;
  paintApps();
}

function updateAppsSection() {
  if ($("apps-canvas")) paintApps();
}

// ------------------------------------------- the app's own drawer (L2 · entity)
// Clicking a row opens the same L2 panel the modules use, but for one
// application: its five measured series, its peak moments and its coverage.
const APP_SERIES_META = {
  cpu: { label: "CPU %", fmt: (v) => fmtPct(v, 1) },
  mem_mb: { label: "memory MB", fmt: (v) => fmtMB(v) },
  energy_w: { label: "energy W", fmt: (v) => (isNum(v) ? v.toFixed(3) + " W" : "—") },
  disk_bps: { label: "disk B/s", fmt: (v) => fmtBytes(v, 1) },
  net_bps: { label: "network B/s", fmt: (v) => fmtBytes(v, 1) },
};

async function openAppL2(name) {
  const el = $("l2");
  if (!el || !name) return;
  L2.appName = name;
  L2.appPanel = true;
  setFilter({ app: name });
  const kind = $("l2Kind");
  if (kind) kind.textContent = "application · " + state.range + " · own series";
  const title = $("l2Title");
  if (title) title.textContent = name;
  const body = $("l2Body");
  if (body) {
    body.innerHTML = '<div class="l2-block"><span class="micro">series · ' +
      esc(state.range) + '</span><div class="app-series" id="app-series">' +
      Object.keys(APP_SERIES_META).map((k) =>
        '<div class="app-series-row"><span class="app-series-label">' +
        esc(APP_SERIES_META[k].label) + '</span>' +
        '<canvas class="app-series-spark" id="appspark-' + k + '" height="22"></canvas>' +
        '<span class="app-series-num na" id="appnum-' + k + '">…</span></div>').join("") +
      "</div><p class=\"cov\" id=\"app-cov\">reading the range store…</p></div>" +
      '<div class="l2-block"><span class="micro">peak moments · this range</span>' +
      '<div id="app-peaks"><span class="na">loading…</span></div></div>' +
      '<div class="l2-block l2-findings"><span class="micro">what this means</span>' +
      '<div id="app-findings"><span class="na">loading…</span></div></div>' +
      '<div class="l2-block"><span class="micro">same numbers, module cut</span>' +
      '<div class="app-links">' +
      ["cpu", "memory", "disk", "network", "power"].map((m) =>
        '<button type="button" class="app-link" data-module="' + m + '">' +
        esc(m) + " →</button>").join("") + "</div></div>";
  }
  el.hidden = false;
  el.setAttribute("aria-hidden", "false");
  document.body.classList.add("has-drawer");
  requestAnimationFrame(() => el.classList.add("is-open"));
  const close = $("l2Close");
  if (close) close.focus();
  const d = await fetchJSON("appseries:" + name + ":" + state.range,
    "/api/apps/series?range=" + encodeURIComponent(state.range) +
    "&name=" + encodeURIComponent(name));
  paintAppSeries(name, d);
}

function paintAppSeries(name, d) {
  if (L2.appName !== name) return;
  const pts = (d && d.points) || [];
  Object.keys(APP_SERIES_META).forEach((k) => {
    const cv = $("appspark-" + k);
    const num = $("appnum-" + k);
    const series = pts.map((p) => ({ ts: p.ts, v: p[k] })).filter((p) => isNum(p.v));
    if (cv && series.length > 1) drawSparkCanvas(cv, series, "v");
    const pk = ((d && d.peaks) || {})[k] || {};
    const vals = series.map((p) => p.v);
    const mean = vals.length ? vals.reduce((a, b) => a + b, 0) / vals.length : null;
    if (num) {
      num.className = "app-series-num" + (series.length ? "" : " na");
      num.innerHTML = series.length
        ? "avg " + esc(APP_SERIES_META[k].fmt(mean)) + " · peak " +
          esc(APP_SERIES_META[k].fmt(pk.value)) +
          (isNum(pk.ts) ? " at " + esc(fmtTime(pk.ts)) : "") +
          " · " + series.length + " samples"
        : "not measured in this range";
    }
  });
  const cov = $("app-cov");
  if (cov) {
    const c = (d && d.coverage) || {};
    cov.textContent = isNum(c.coverage_pct)
      ? "measured " + c.coverage_pct.toFixed(0) + " % of " + state.range +
        " · " + (isNum(c.sweeps) ? c.sweeps : 0) + " sweeps @" +
        (isNum(c.cadence_s) ? c.cadence_s : 20) + " s · a null is a sweep this app " +
        "was not in, never a zero"
      : ((d && d.empty_text) || "no sweep in this range yet");
  }
  const peaks = $("app-peaks");
  if (peaks) {
    const row = (state.apps && (state.apps.rows || []).find((r) => r.name === name)) || {};
    peaks.innerHTML = statRows([
      ["avg CPU", isNum(row.cpu_avg_pct) ? fmtPct(row.cpu_avg_pct, 2) : na("n/a")],
      ["peak CPU", isNum(row.cpu_max_pct)
        ? fmtPct(row.cpu_max_pct, 2) + (isNum(row.peak_cpu_ts) ? " · " + fmtTime(row.peak_cpu_ts) : "")
        : na("n/a")],
      ["peak memory", isNum(row.mem_peak_mb)
        ? fmtMB(row.mem_peak_mb) + (isNum(row.peak_mem_ts) ? " · " + fmtTime(row.peak_mem_ts) : "")
        : na("n/a")],
      ["peak energy", isNum(row.peak_w)
        ? row.peak_w.toFixed(3) + " W" + (isNum(row.peak_w_ts) ? " · " + fmtTime(row.peak_w_ts) : "")
        : na("n/a")],
      ["energy over " + state.range, isNum(row.energy_wh) ? fmtWh(row.energy_wh) : na("n/a")],
      ["share of total", isNum(row.share_pct) ? row.share_pct.toFixed(1) + " %" : na("n/a")],
      ["processes", isNum(row.processes) ? String(row.processes) : "—"],
    ]);
  }
  const f = $("app-findings");
  if (f) {
    const row = (state.apps && (state.apps.rows || []).find((r) => r.name === name)) || {};
    const out = [];
    if (isNum(row.energy_wh)) {
      out.push("<b>" + esc(name) + "</b> used <b>" + esc(fmtWh(row.energy_wh)) +
        "</b> of measured per-app energy over " + state.range +
        (isNum(row.share_pct) ? " — <b>" + row.share_pct.toFixed(1) + " %</b> of the " +
          "measured app total" : "") + ".");
    } else {
      out.push("<b>" + esc(name) + "</b> has no energy measurement in this window; " +
        "the column says n/a rather than 0.");
    }
    if (isNum(row.cpu_avg_pct)) {
      out.push("It averaged <b>" + fmtPct(row.cpu_avg_pct, 2) + "</b> of one core" +
        (isNum(row.cpu_max_pct) ? ", peaking at <b>" + fmtPct(row.cpu_max_pct, 2) + "</b>" : "") +
        (isNum(row.mem_peak_mb) ? ", and held up to <b>" + fmtMB(row.mem_peak_mb) + "</b> resident" : "") +
        ".");
    }
    const tot = ((state.apps || {}).totals) || {};
    out.push("Measured energy is a floor, not the machine total: it is the sum of " +
      "per-PID ri_energy_nj deltas" +
      (isNum(tot.energy_wh) ? " (Σ " + tot.energy_wh.toFixed(4) + " Wh across every app in this window)" : "") +
      ".");
    out.push("Same range store as the module consumers lists — pick a module below " +
      "for the module cut of the same numbers.");
    f.innerHTML = out.map((s) => "<p>" + s + "</p>").join("");
  }
}

// ================================================================ Power section
// One axis, several series, distinct line styles, never stacked. The root tier
// (package / CPU / GPU / ANE) is drawn as its own series the moment the daemon
// file has a sample — and says `needs root` when it does not.
const POWER_SPECS = [
  { key: "battery_w", label: "battery draw", color: ACCENT, fill: true },
  { key: "package_w", label: "package (root)", color: MUTED, dash: [2, 3] },
  { key: "cpu_w", label: "CPU (root)", color: DIM, dash: [1, 4] },
  { key: "gpu_w", label: "GPU (root)", color: DIM, dash: [4, 3] },
  { key: "ane_w", label: "ANE (root)", color: DIM, dash: [5, 2, 2, 2] },
  { key: "attributed_w", label: "attributed apps", color: MUTED, dash: [1, 2] },
];

function powerLegendHtml() {
  return '<div class="legend">' + POWER_SPECS.map((s) =>
    '<span class="legend-item"><i style="border-top-color:' + s.color +
    ";border-top-style:" + (s.dash && s.dash.length ? "dashed" : "solid") + '"></i>' +
    esc(s.label) + "</span>").join("") +
    '<span class="legend-note">one axis, not stacked · adapter input is a stat, ' +
    "not a series (67 W would flatten every rail)</span></div>";
}

function powerSectionHtml() {
  return sectionShell("power", "Power", "draw · adapter · package · CPU · GPU · ANE",
    chartPanel("power", "Power over the last " + state.range) + powerLegendHtml() +
    consumersPanelHtml("power") +
    panelHtml('<div id="power-analysis"></div>' +
      '<div class="reason" id="power-basis"></div>', "what is being calculated · " + state.range) +
    panelHtml('<div id="power-root"></div>' +
      '<div class="reason">read-only JSON written by the root LaunchDaemon — the dashboard ' +
      'process itself never runs as root. Grant or revoke it in ⚙ settings → permissions.</div>',
      "root tier · powermetrics daemon") +
    colsHtml(
      panelHtml('<table class="table"><thead><tr><th>App</th><th class="num">Wh</th>' +
        '<th class="num">of total</th></tr></thead><tbody id="power-apps"></tbody></table>' +
        '<div class="reason" id="power-apps-basis"></div>', "per-app energy over " + state.range) +
      panelHtml('<div id="power-ring"></div>', "ring state · windows")));
}

function updatePowerSection() {
  const ph = state.powerHistory || {};
  const cur = ph.current || {};
  const an = ph.analysis || {};
  const drawn = isNum(cur.battery_w) ? cur.battery_w : (an.battery || {}).mean_w;
  setHeader("power", isNum(drawn) ? drawn.toFixed(2) : null, "W draw",
    parts("now · battery V×A (ioreg)",
      isNum(cur.pct) ? fmtPct(cur.pct, 0) : null,
      cur.external_connected === false ? "on battery"
        : cur.external_connected === true ? "on power" : null,
      isNum(cur.adapter_w) ? "adapter input " + fmtW(cur.adapter_w) : null));

  // -- root tier rows: package / CPU / GPU / ANE + die temps + fans
  const rootBox = $("power-root");
  if (rootBox) {
    const r = ph.root || {};
    const live = r.available === true;
    const naRoot = na(live ? "field missing in this sample" : "needs root — no privileged sample");
    const rows = [
      ["status", live
        ? "live · " + (isNum(r.age_s) ? r.age_s.toFixed(0) + " s old" : "fresh")
        : na(r.reason || "needs root — no privileged sample")],
      ["package (CPU + GPU + ANE)", live && isNum(r.package_w) ? fmtW(r.package_w) : naRoot],
      ["CPU", live && isNum(r.cpu_w) ? fmtW(r.cpu_w) : naRoot],
      ["GPU", live && isNum(r.gpu_w) ? fmtW(r.gpu_w) : naRoot],
      ["Neural Engine (ANE)", live && isNum(r.ane_w) ? fmtW(r.ane_w) : naRoot],
      ["thermal pressure", live && r.thermal_pressure ? esc(r.thermal_pressure) : naRoot],
      ["CPU die temperature", live && isNum(r.cpu_temp_c || r.cpu_avg_c)
        ? (r.cpu_temp_c ?? r.cpu_avg_c).toFixed(1) + " °C" : na(live ? "needs SMC reader (T16B)" : "needs root")],
      ["GPU die temperature", live && isNum(r.gpu_temp_c || r.gpu_avg_c)
        ? (r.gpu_temp_c ?? r.gpu_avg_c).toFixed(1) + " °C" : na(live ? "needs SMC reader (T16B)" : "needs root")],
      ["fans", live && Array.isArray(r.fans_rpm) && r.fans_rpm.length
        ? r.fans_rpm.map((x) => fmtInt(x)).join(" · ") + " rpm"
        : na(live ? "no fan reported on this machine" : "needs root")],
    ];
    if (live && Array.isArray(r.cpu_cluster_temps_c) && r.cpu_cluster_temps_c.length) {
      r.cpu_cluster_temps_c.forEach((c) => {
        rows.push([c.label || "cluster", isNum(c.c) ? c.c.toFixed(1) + " °C"
          : na("not in this sample")]);
      });
    }
    rootBox.innerHTML = statRows(rows) +
      '<div class="reason">' + esc(r.source || "powermetrics via the root daemon") + "</div>";
  }

  // -- the analysis block: every number carries its method in words
  const box = $("power-analysis");
  if (box) {
    const e = an.energy || {};
    const b = an.battery || {};
    const ad = an.adapter || {};
    const dr = an.drain || {};
    box.innerHTML = statRows([
      ["energy drawn (battery)", isNum(e.battery_wh) ? e.battery_wh.toFixed(3) + " Wh"
        : na(b.reason || "no battery samples")],
      ["energy attributed to apps", isNum(e.attributed_wh) ? e.attributed_wh.toFixed(3) + " Wh"
        : na("no sweeps in this window")],
      ["adapter energy in", isNum(e.adapter_wh) && e.adapter_wh > 0
        ? e.adapter_wh.toFixed(3) + " Wh" : na(ad.reason || "on battery")],
      ["package energy", isNum(e.package_wh) && e.package_wh > 0
        ? e.package_wh.toFixed(3) + " Wh"
        : na((an.excluded || {}).package_w || "needs root — no privileged sample in this window")],
      ["average draw", isNum(b.mean_w) ? fmtW(b.mean_w) : na(b.reason || "no samples")],
      ["peak draw", isNum(b.peak_w) ? fmtW(b.peak_w) +
        (isNum(b.peak_ts) ? " at " + fmtTime(b.peak_ts) : "") : na("no samples")],
      ["p95 draw", isNum(b.p95_w) ? fmtW(b.p95_w) : na("no samples")],
      ["drain rate", isNum(dr.pct_per_h) ? dr.pct_per_h.toFixed(2) + " %/h"
        : na(dr.reason || "not measurable in this window")],
      ["projected empty", isNum(dr.projected_empty_s)
        ? (fmtClock(dr.projected_empty_s) || "—") + " remaining"
        : na(dr.pct_per_h === null ? "needs 2 min of charge samples" : "charging")],
    ]);
  }
  const basis = $("power-basis");
  if (basis) {
    // The stored note lists the series it seeds. The adapter is a stat on this
    // page, not a series, so the sentence is trimmed to what is actually drawn.
    const note = (ph.coverage_note || "")
      .replace("battery draw, adapter input and package W are",
        "battery draw and package W are");
    basis.textContent = parts(ph.coverage_text, note, (an.energy || {}).method);
  }

  // -- per-app Wh share of the measured total (click → that app's own series)
  const tbody = $("power-apps");
  const appRows = ((an.per_app || {}).rows || []);
  if (tbody) {
    if (!appRows.length) {
      tbody.innerHTML = '<tr><td colspan="3" class="na">' +
        esc((an.per_app || {}).basis || "no per-app measurements in this window") + "</td></tr>";
    } else {
      const sum = appRows.reduce((a, r) => a + (isNum(r.wh) ? r.wh : 0), 0);
      tbody.innerHTML = appRows.slice(0, 12).map((a) => {
        const share = isNum(a.share_pct) ? a.share_pct
          : (sum > 0 ? Math.round((a.wh / sum) * 1000) / 10 : 0);
        const picked = (state.filter || {}).app === a.name;
        return '<tr class="row-click' + (picked ? " is-picked" : "") +
          '" data-app="' + esc(a.name) + '" tabindex="0">' +
          "<td class='name'><span class='rowbar'><i style='width:" + share + "%'></i></span>" +
          esc(a.name) + "</td><td class='num'>" + a.wh.toFixed(4) + "</td>" +
          "<td class='num'>" + (isNum(a.share_pct) ? a.share_pct.toFixed(1) + " %" : "—") +
          "</td></tr>";
      }).join("");
      tbody.querySelectorAll("[data-app]").forEach((tr) => {
        const pick = () => {
          const name = tr.dataset.app;
          setFilter({ app: state.filter.app === name ? null : name });
          openDrawerForApp(name);
        };
        tr.addEventListener("click", pick);
        tr.addEventListener("keydown", (ev) => {
          if (ev.key === "Enter" || ev.key === " ") { ev.preventDefault(); pick(); }
        });
      });
    }
  }
  const appBasis = $("power-apps-basis");
  if (appBasis) appBasis.textContent = (an.per_app || {}).basis || "";

  // -- windows + ring state
  const win = $("power-ring");
  if (win) {
    const w = (state.powerSummary || {}).windows || {};
    const order = [["1m", "1 min"], ["1h", "1 hour"], ["1d", "1 day"], ["7d", "7 days"]];
    const r = (state.powerSummary || {}).ring || {};
    win.innerHTML = statRows(order.map(([k, label]) => {
      const row = w[k];
      if (!row || (!isNum(row.mean_w) && !isNum(row.mean_attributed_w))) {
        return [label, na(row && row.reason ? row.reason : "no samples")];
      }
      return [label, (isNum(row.mean_w) ? row.mean_w.toFixed(2) + " W avg · " : "") +
        (isNum(row.max_w) ? row.max_w.toFixed(2) + " W max" : "no peak") +
        (row.complete ? "" : " · filling")];
    }).concat([
      ["samples", isNum(r.samples) ? fmtInt(r.samples) + " at " +
        (isNum(r.sampling_s) ? r.sampling_s : 5) + " s" : na("ring empty")],
    ]));
  }

  mountConsumers("power");
  wireGrants();
}

function paintPowerSectionChart() {
  const ph = state.powerHistory || {};
  const pts = ph.points || [];
  const painted = paintMulti("power-chart", POWER_SPECS, pts, ph.ticks || [],
    (v) => (isNum(v) ? v.toFixed(2) + " W" : ""));
  if (!painted) clearChart("power-chart", ph.ticks, ph.unavailable_reason);
  const src = $("power-src");
  if (src) {
    src.textContent = "battery V×A (ioreg, 5 s) · adapter input (ioreg) · " +
      "package / CPU / GPU / ANE (root powermetrics) · attributed apps (rusage floor)";
  }
  const cov = $("power-cov");
  if (cov) cov.textContent = ph.coverage_text || "";
  const reason = $("power-reason");
  if (reason) {
    reason.textContent = pts.length ? (ph.coverage_note || "")
      : (ph.unavailable_reason || "no samples in this window yet");
    reason.className = "reason" + (pts.length ? "" : " is-warn");
  }
  const ch = CHARTS["power-chart"];
  if (ch) {
    ch.options.onClick = (evt) => {
      const hit = ch.getElementsAtEventForMode(evt, "index", { intersect: false }, true);
      if (!hit.length) { state.filter = {}; renderChips(); return; }
      const idx = hit[0].index;
      const ds = ch.data.datasets[0];
      const pt = ds && ds.data[idx] ? ds.data[idx] : null;
      if (!pt || !isNum(pt.x)) return;
      const ts = pt.x / 1000;
      setFilter({ bucket: ts });
      openDrawerForBucket(ts);
    };
  }
  wireBrush("power-chart");
}

// ============================================================== Thermal section
// Pressure now; die temperatures the moment the SMC reader lands (T16B). The
// slot says what is missing instead of printing a plausible number.
function thermalSectionHtml() {
  return sectionShell("thermal", "Thermal", "pressure level · die temperatures (T16B)",
    colsHtml(
      panelHtml('<div id="thermal-pressure"></div>', "thermal pressure · powermetrics") +
      panelHtml('<div id="thermal-temps"></div>', "die temperatures · SMC reader")));
}

function updateThermalSection() {
  const ph = state.powerHistory || {};
  const r = ph.root || {};
  const live = r.available === true;
  const s = sensorById("thermal.pressure");
  const level = (s && s.available ? s.value : null) || r.thermal_pressure || null;
  setHeader("thermal", level ? String(level) : null, "",
    parts(level ? "now · powermetrics thermal pressure" : null,
      live ? "root sample " + (isNum(r.age_s) ? r.age_s.toFixed(0) + " s old" : "live") : null) ||
    (s && s.reason) || "needs root — thermal pressure is a privileged read");

  const box = $("thermal-pressure");
  if (box) {
    const rows = [];
    if (s) rows.push([s.label, sensorText(s) || na(s.reason || "unavailable")]);
    if (r.thermal_pressure) rows.push(["powermetrics pressure", esc(r.thermal_pressure)]);
    rows.push(["source", '<span class="reason">' +
      esc((s && s.source) || "powermetrics thermal (root) / pmset -g therm") + "</span>"]);
    box.innerHTML = statRows(rows);
  }

  const temps = $("thermal-temps");
  if (temps) {
    const rows = [
      ["CPU die", isNum(r.cpu_temp_c) ? r.cpu_temp_c.toFixed(1) + " °C"
        : '<span class="warn-pill">needs SMC reader</span>'],
      ["GPU die", isNum(r.gpu_temp_c) ? r.gpu_temp_c.toFixed(1) + " °C"
        : '<span class="warn-pill">needs SMC reader</span>'],
    ];
    ((r.cpu_cluster_temps_c) || []).forEach((c) => {
      rows.push([c.label || "cluster", isNum(c.c) ? c.c.toFixed(1) + " °C"
        : '<span class="warn-pill">not reported</span>']);
    });
    rows.push(["fans", Array.isArray(r.fans_rpm) && r.fans_rpm.length
      ? r.fans_rpm.map((x) => fmtInt(x)).join(" · ") + " rpm"
      : '<span class="warn-pill">no fan sensor exposed</span>']);
    temps.innerHTML = statRows(rows) +
      '<div class="reason">die temperatures arrive with the SMC reader (T16B); ' +
      "until then this slot reports the gap rather than a number</div>";
  }
  wireGrants();
}

// ================================================== the instrument (T20 · L1–L3)
// ONE viewport, one tile grid, no page scroll:
//
//   L1  nine tiles in one 3-column grid (T22). Each is a number, a sparkline, a
//       delta and one meta line — the sparkline is the only visual.
//       Click a tile → focus (2 × 2 in the grid, the rest dimmed to 0.35);
//       Thermal focuses without expanding.
//   L2  a focused tile's "full details" expands the module IN PLACE, in flow,
//       pushing L1 down. Chart (horizontally scrollable, drag to zoom) +
//       measures (mean/peak/p95) + top consumers + coverage.
//   L3  inside L2, a collapsed "details" toggle: findings, records, per-core,
//       the full SMC sensor list and the peak moments. The 1 % layer.
//
// Hover is a quick look (a 30 px tooltip of the last 60 s), click is focus.
// Nothing here is a drawer, a slide-over or a route.
const BENTO_CHIPS = ["cpu", "memory", "gpu", "battery", "power", "load"];
const BENTO_TILES = ["network", "disk", "thermal"];
const BENTO_MODULES = BENTO_CHIPS.concat(BENTO_TILES);

const MODULE_LABEL = {
  cpu: "CPU", memory: "Memory", gpu: "GPU", battery: "Battery",
  power: "Package power", thermal: "Thermal", network: "Network", disk: "Disk",
  load: "Load average",
};

// The read behind each module, in the product's own words. Every hover tooltip
// prints exactly one of these — a number with no source is not an answer.
const MODULE_SOURCE = {
  cpu: "host_processor_info · 1 s",
  memory: "vm_statistics64 · 1 s",
  gpu: "ioreg IOAccelerator · 5 s",
  battery: "ioreg AppleSmartBattery · 15 s",
  power: "powermetrics (root) · 15 s",
  thermal: "AppleSMC (root) · 15 s",
  network: "getifaddrs byte deltas · 1 s",
  disk: "ioreg IOBlockStorageDriver · 5 s",
  load: "getloadavg · 1 s",
};

// What each module's series is: the /api/graph metric, the field that carries
// the number, and how that number is written.
const MODULE_SERIES = {
  cpu: { metric: "cpu", key: "total", fmt: (v) => fmtPct(v, 1), pp: true },
  memory: { metric: "memory", key: "pct", fmt: (v) => fmtPct(v, 1), pp: true },
  gpu: { metric: "gpu", key: "util_pct", fmt: (v) => fmtPct(v, 1), pp: true },
  battery: { metric: "battery", key: "pct", fmt: (v) => fmtPct(v, 0), pp: true },
  network: { metric: "network", key: "down_bps", fmt: (v) => fmtBytes(v, 1), pp: false },
  disk: { metric: "disk", key: "read_bps", fmt: (v) => fmtBytes(v, 1), pp: false },
  load: { metric: "load", key: "one", fmt: (v) => v.toFixed(2), pp: false },
  // power and thermal have no /api/graph metric: both live in the 15 s power
  // ring, which now carries the privileged die temperatures as well.
  power: { metric: null, key: "package_w", fmt: (v) => fmtW(v), pp: false },
  thermal: { metric: null, key: "cpu_temp_c", fmt: (v) => v.toFixed(1) + " °C", pp: false },
};

// A difference between two percentages is in percentage POINTS (pp). Writing
// "+54.6 %" there would be wrong arithmetic, not just loose wording.
function moduleDelta(spec, d) {
  const a = Math.abs(d);
  if (spec.pp) return a.toFixed(1) + " pp";
  return spec.fmt ? spec.fmt(a) : String(a);
}

function paintInstrumentCharts() { paintBentoCharts(); }
function instrumentHtml() { return ""; }   // the markup is static in index.html
function observeSections() { state.visible = {}; }

// --------------------------------------------------------------- sparklines
// A 22 px sparkline is drawn on a bare canvas, not a Chart instance: nine live
// Chart.js objects on a 1 s tick would cost more than the chart is worth. Honest
// gaps — a null sample breaks the path instead of being drawn as a zero.
function drawSpark(canvasId, points, key) {
  const cv = $(canvasId);
  if (!cv) return false;
  return drawSparkCanvas(cv, points, key);
}

// The same sparkline on a canvas element the caller already holds (the Apps
// table draws one per visible row, and the app drawer draws five).
function drawSparkCanvas(cv, points, key) {
  if (!cv) return false;
  const dpr = window.devicePixelRatio || 1;
  const w = Math.max(24, cv.clientWidth || (cv.parentElement ? cv.parentElement.clientWidth : 120));
  // the element owns its height (30 px on a chip, 30 px in the hover tip);
  // fall back to the old 22 px only when layout has not run yet.
  const h = Math.max(16, Math.round(cv.clientHeight) || 22);
  if (cv.width !== Math.round(w * dpr) || cv.height !== Math.round(h * dpr)) {
    cv.width = Math.round(w * dpr);
    cv.height = Math.round(h * dpr);
  }
  const ctx = cv.getContext("2d");
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, w, h);

  const pts = points || [];
  const segs = [];
  let seg = [];
  pts.forEach((p) => {
    if (isNum(p[key])) seg.push(p);
    else if (seg.length) { segs.push(seg); seg = []; }
  });
  if (seg.length) segs.push(seg);
  const all = segs.reduce((a, s) => a.concat(s), []);
  if (all.length < 2) return false;

  const x0 = all[0].ts;
  const x1 = all[all.length - 1].ts;
  let lo = Infinity;
  let hi = -Infinity;
  all.forEach((p) => { if (p[key] < lo) lo = p[key]; if (p[key] > hi) hi = p[key]; });
  if (!(hi > lo)) { hi = lo + 1; lo -= 1; }
  const span = (x1 - x0) || 1;
  const sx = (ts) => ((ts - x0) / span) * (w - 1);
  const sy = (v) => h - 2 - ((v - lo) / (hi - lo)) * (h - 4);

  const grad = ctx.createLinearGradient(0, 0, 0, h);
  grad.addColorStop(0, "rgba(94,234,212,0.20)");
  grad.addColorStop(1, "rgba(94,234,212,0.00)");

  segs.forEach((s) => {
    if (s.length < 2) {
      ctx.beginPath();
      ctx.fillStyle = ACCENT;
      ctx.arc(sx(s[0].ts), sy(s[0][key]), 1.1, 0, Math.PI * 2);
      ctx.fill();
      return;
    }
    ctx.beginPath();
    ctx.moveTo(sx(s[0].ts), sy(s[0][key]));
    for (let i = 1; i < s.length; i++) ctx.lineTo(sx(s[i].ts), sy(s[i][key]));
    // fill under the path, so the shape reads at 22 px
    ctx.save();
    const lastX = sx(s[s.length - 1].ts);
    ctx.lineTo(lastX, h);
    ctx.lineTo(sx(s[0].ts), h);
    ctx.closePath();
    ctx.fillStyle = grad;
    ctx.fill();
    ctx.restore();
    // stroke the same path again (a filled sub-path cannot be stroked twice)
    ctx.beginPath();
    ctx.moveTo(sx(s[0].ts), sy(s[0][key]));
    for (let i = 1; i < s.length; i++) ctx.lineTo(sx(s[i].ts), sy(s[i][key]));
    ctx.strokeStyle = ACCENT;
    ctx.lineWidth = 1.6;
    ctx.lineJoin = "round";
    ctx.stroke();
  });
  return true;
}

// Which series feeds a tile's sparkline. Power prefers the root package rail and
// falls back to battery draw — the tile's meta line always names which one.
// Thermal reads the die temperature the ring now records (cpu_temp_c), never a
// power proxy: a proxy would be a different unit pretending to be a temperature.
function chipSpark(module) {
  if (module === "power") {
    const pts = (state.powerHistory || {}).points || [];
    const n = pts.filter((p) => isNum(p.package_w)).length;
    return n >= 2 ? { points: pts, key: "package_w", label: "package" }
      : { points: pts, key: "battery_w", label: "battery" };
  }
  if (module === "thermal") {
    return { points: (state.powerHistory || {}).points || [], key: "cpu_temp_c", label: "cpu" };
  }
  const s = MODULE_SERIES[module];
  const metric = s ? s.metric : module;
  const fresh = SPARK.cache[metric];
  const g = state.graphs[metric] || {};
  return {
    points: fresh && fresh.length ? fresh : (g.points || []),
    key: s ? s.key : null,
    label: module,
    window: fresh && fresh.length ? "1m" : state.range,
  };
}

// L1 sparklines are a FIXED 60 s window with no scroll: a sparkline is a shape,
// not a chart. The full, scrollable window is what focus mode and L2 are for.
//
// Why a separate 1 m read: the page window is 1 h, and its buckets are ~10 s, so
// the last minute of it is six points — a zigzag, not a shape. One cheap 1 m
// read per metric (60 points at the 1 s cadence) is what makes a 30 px sparkline
// legible, and it is refreshed at 15 s, not on every tick.
const SPARK_WINDOW_S = 60;
const SPARK_METRICS = ["cpu", "memory", "gpu", "battery", "network", "disk", "load"];
const SPARK = { at: 0, cache: {}, pending: false };

async function loadSparkSeries(force) {
  if (SPARK.pending) return;
  if (!force && SPARK.at && Date.now() - SPARK.at < 15000) return;
  SPARK.pending = true;
  try {
    await Promise.all(SPARK_METRICS.map(async (m) => {
      const d = await fetchJSON("spark:" + m,
        "/api/graph?metric=" + encodeURIComponent(m) + "&range=1m");
      if (d && Array.isArray(d.points)) SPARK.cache[m] = d.points;
    }));
    SPARK.at = Date.now();
  } finally {
    SPARK.pending = false;
  }
}

function last60(points) {
  const pts = points || [];
  if (!pts.length) return pts;
  const newest = pts[pts.length - 1].ts;
  if (!isNum(newest)) return pts;
  const cut = newest - SPARK_WINDOW_S;
  return pts.filter((p) => isNum(p.ts) && p.ts >= cut);
}

// --------------------------------------------------------- L2 coverage line
// 11 px dim line, the measured share bolded. It is an L2 statement, not an L1
// one: L1 is about NOW, and coverage is about how much of the *window* exists.
// Printing it on a chip was one of the text walls T20 removes.
function coverHtml(data) {
  const m = (data && data.measures) || {};
  const bits = [];
  if (isNum(m.coverage_pct)) {
    bits.push("measured <b>" + m.coverage_pct.toFixed(0) + " %</b> of " + state.range);
  }
  if (isNum(m.points)) bits.push(m.points + " points");
  if (data && data.source) bits.push(esc(String(data.source).split(" · ")[0]));
  if (!bits.length) {
    bits.push('<b>0 %</b> measured — ' + esc((data && data.unavailable_reason)
      || "no samples in this range yet"));
  }
  return bits.join(" · ");
}

// The power ring is not /api/graph: it has its own cadence and its own way of
// saying how much of the window it holds, so it gets its own coverage line.
function powerCoverageHtml() {
  const ph = state.powerHistory || {};
  if (!isNum(ph.samples)) return "coverage · waiting for the power ring";
  const span = isNum(ph.newest_ts) && isNum(ph.oldest_ts) ? ph.newest_ts - ph.oldest_ts : null;
  const covered = isNum(span) && isNum(ph.window_s) && ph.window_s
    ? Math.min(100, (span / ph.window_s) * 100)
    : (isNum(ph.bucket_s) && isNum(ph.window_s) && ph.window_s
      ? Math.min(100, (ph.samples * ph.bucket_s / ph.window_s) * 100) : null);
  const bits = [];
  if (isNum(covered)) bits.push("measured <b>" + covered.toFixed(0) + " %</b> of " + state.range);
  // the server's own sentence: "sampling every 15 s · 78 samples · since 03:32"
  if (ph.coverage_text) bits.push(esc(ph.coverage_text));
  else bits.push(ph.samples + " samples @" + (isNum(ph.sampling_s) ? ph.sampling_s : 15) + " s");
  return bits.join(" · ");
}

// --------------------------------------------------------------- L1 · tiles
function setChip(id, opts) {
  const o = opts || {};
  const num = $("v-" + id);
  if (num) {
    num.className = "tile-num" + (o.value == null || o.value === ""
      ? " na" : "") + (o.cls ? " " + o.cls : "");
    num.textContent = o.value == null || o.value === ""
      ? (o.word || "no sample") : String(o.value);
  }
  const unit = $("v-" + id + "-unit");
  if (unit) {
    unit.textContent = o.unit || "";
    unit.hidden = !o.unit || o.value == null || o.value === "";
  }
  const meta = $("v-" + id + "-meta");
  if (meta) meta.textContent = o.meta || "";
  const del = $("v-" + id + "-delta");
  if (del) del.textContent = o.delta || "";
}

function updateVitals() {
  const d = state.current || {};
  const cpu = d.cpu || {};
  const ram = d.ram || {};
  const gpu = d.gpu || {};
  const batt = d.battery || {};
  const pr = d.pressure || {};
  const un = d.unavailable || {};
  const ph = state.powerHistory || {};
  const pcur = ph.current || {};
  const root = ph.root || {};
  const rootLive = root.available === true;
  const base = state.range + " avg";

  // CPU — utilisation now; cores + pressure on the ONE meta line.
  const cpuPct = isNum(cpu.total_pct) ? cpu.total_pct : meanOf(cpu.per_core_pct);
  const pc = pr.cpu || {};
  const cpuMean = graphMean("cpu", "total");
  setChip("cpu", {
    value: isNum(cpuPct) ? cpuPct.toFixed(1) : null,
    unit: "%",
    meta: parts(
      isNum(cpu.cores) ? cpu.cores + " cores" : null,
      isNum(pc.pct) ? "pressure " + pc.pct.toFixed(0) + " %" : null
    ) || (un.cpu || "waiting for the first CPU sample"),
    delta: isNum(cpuPct) && isNum(cpuMean) ? delta(cpuPct, cpuMean, "pp", 1, base) : "",
  });
  const cSpark = chipSpark("cpu");
  drawSpark("vspark-cpu", last60(cSpark.points), cSpark.key);

  // Memory — used % now, the byte pair on the meta line.
  const pm = pr.memory || {};
  const memMean = graphMean("memory", "pct");
  setChip("memory", {
    value: isNum(ram.pct) ? ram.pct.toFixed(1) : null,
    unit: "%",
    meta: parts(
      isNum(ram.used_bytes) && isNum(ram.total_bytes)
        ? Math.round(ram.used_bytes / 1048576) + " of " + Math.round(ram.total_bytes / 1048576) + " MB" : null,
      pm.level ? "pressure " + pm.level : null
    ) || (un.memory || "waiting for the first memory sample"),
    delta: isNum(ram.pct) && isNum(memMean) ? delta(ram.pct, memMean, "pp", 1, base) : "",
  });
  const mSpark = chipSpark("memory");
  drawSpark("vspark-memory", last60(mSpark.points), mSpark.key);

  // GPU — device utilisation; renderer + wired memory on the meta line.
  const gpuLive = gpu.available !== false && isNum(gpu.util_pct);
  const gpuMean = graphMean("gpu", "util_pct");
  setChip("gpu", {
    value: gpuLive ? gpu.util_pct.toFixed(1) : null,
    unit: "%",
    word: gpu.available === false ? "no probe" : "no sample",
    meta: gpuLive ? parts(
      isNum(gpu.renderer_pct) ? "renderer " + gpu.renderer_pct.toFixed(0) + " %" : null,
      isNum(gpu.mem_used_mb) ? fmtMB(gpu.mem_used_mb) + " wired" : null
    ) : (gpu.reason || un.gpu || "no GPU probe on this machine"),
    delta: gpuLive && isNum(gpuMean) ? delta(gpu.util_pct, gpuMean, "pp", 1, base) : "",
  });
  const gSpark = chipSpark("gpu");
  drawSpark("vspark-gpu", last60(gSpark.points), gSpark.key);

  // Battery — charge %, time remaining and draw. Amber is the ONLY place a
  // second hue is allowed in the product, and only at low charge.
  const secs = isNum(pcur.time_remaining_s) ? pcur.time_remaining_s : batt.time_remaining_s;
  const clock = fmtClock(secs);
  const charging = batt.charging === true || pcur.charging === true;
  const draw = isNum(pcur.battery_w) ? pcur.battery_w : batt.battery_w;
  const lowBatt = isNum(batt.pct) && batt.pct <= 20;
  setChip("battery", {
    value: isNum(batt.pct) ? batt.pct.toFixed(0) : null,
    unit: "%",
    cls: lowBatt ? "is-amber" : "",
    meta: parts(
      clock ? (charging ? clock + " to full" : clock + " left") : null,
      isNum(draw) ? fmtW(draw) + " draw" : null,
      charging ? "charging" : null
    ) || "waiting for an AppleSmartBattery read",
    delta: isNum(batt.health_pct) ? "health " + batt.health_pct.toFixed(0) + " %" : "",
  });
  const bSpark = chipSpark("battery");
  drawSpark("vspark-battery", last60(bSpark.points), bSpark.key);

  // Package power — the root tier, live.
  const pkg = rootLive && isNum(root.package_w) ? root.package_w
    : isNum(pcur.package_w) ? pcur.package_w : null;
  const rails = [];
  if (isNum(root.cpu_w)) rails.push("CPU " + root.cpu_w.toFixed(1));
  if (isNum(root.gpu_w)) rails.push("GPU " + root.gpu_w.toFixed(1));
  if (isNum(root.ane_w)) rails.push("ANE " + root.ane_w.toFixed(1));
  setChip("power", {
    value: isNum(pkg) ? pkg.toFixed(2) : null,
    unit: "W",
    word: "needs root",
    meta: rootLive && rails.length ? rails.join(" · ") + " W"
      : (root.reason || un.package_power || "needs root — no privileged sample"),
    delta: rootLive ? "powermetrics 15 s" : "",
  });
  const pSpark = chipSpark("power");
  drawSpark("vspark-power", last60(pSpark.points), pSpark.key);

  // Load — the runnable-queue length, the cheapest honest "is it queueing?".
  const la = d.load || {};
  const loadMean = graphMean("load", "one");
  setChip("load", {
    value: isNum(la.one) ? la.one.toFixed(2) : null,
    unit: "",
    word: "no loadavg",
    meta: parts(
      isNum(la.five) ? "5m " + la.five.toFixed(2) : null,
      isNum(la.fifteen) ? "15m " + la.fifteen.toFixed(2) : null
    ) || "getloadavg unavailable on this platform",
    delta: isNum(la.one) && isNum(loadMean) ? delta(la.one, loadMean, "", 2, base) : "",
  });
  const lSpark = chipSpark("load");
  drawSpark("vspark-load", last60(lSpark.points), lSpark.key);
}

// ------------------------------------------------------- L2 · measures + cores
// The measures table is L2's job now: mean · peak (with its timestamp) · p95 --
// the three numbers that answer "how much, how bad, how typical" for the window.
function measuresHtml(module) {
  const spec = MODULE_SERIES[module] || {};
  const g = module === "power" || module === "thermal"
    ? null : (state.graphs[module] || {});
  const m = ((g || {}).measures || {}).fields || {};
  const primary = ((g || {}).measures || {}).primary || spec.key;
  const f = m[primary] || {};
  const fmt = spec.fmt || ((v) => String(v));
  const rows = [
    ["mean", isNum(f.mean) ? esc(fmt(f.mean)) : na("no samples")],
    ["peak", isNum(f.max)
      ? esc(fmt(f.max)) + (isNum(f.max_ts) ? ' <span class="note">' + esc(fmtTime(f.max_ts)) + "</span>" : "")
      : na("no samples")],
    ["p95", isNum(f.p95) ? esc(fmt(f.p95)) : na("no samples")],
  ];
  if (module === "power" || module === "thermal") {
    // the ring is not /api/graph: its own window block carries the aggregates
    const windows = (state.powerHistory || {}).windows || {};
    const w = windows[state.range] || windows["1h"] || {};
    const key = module === "thermal" ? "cpu_temp_c" : "package_w";
    const mean = isNum(w[key]) ? w[key] : (module === "thermal" ? null : w.mean_package_w);
    const peak = isNum(w[key + "_max"]) ? w[key + "_max"] : null;
    if (isNum(mean) || isNum(peak)) {
      rows[0] = ["mean", isNum(mean) ? esc(fmt(mean)) : na("no samples")];
      rows[1] = ["peak", isNum(peak) ? esc(fmt(peak)) : na("no samples")];
      rows[2] = ["p95", na("the ring reports mean · max only")];
    }
  }
  return rows.map(([k, v]) =>
    '<div class="mm-row"><span class="mm-k">' + esc(k) + "</span>" +
    '<span class="mm-v">' + v + "</span></div>").join("");
}

// P/E clusters, drawn as the same rings the section used to own — now inside the
// CPU expansion, where a split of the total belongs.
function clusterRingsHtml() {
  const d = state.current || {};
  const cpu = d.cpu || {};
  const clusters = Array.isArray(cpu.clusters) ? cpu.clusters : [];
  const perCore = Array.isArray(cpu.per_core_pct) ? cpu.per_core_pct : [];
  if (!clusters.length) {
    return '<span class="na">P/E split unavailable — cpu.clusters is empty</span>';
  }
  return clusters.map((c, ci) => {
    const start = clusterSpan(clusters, ci);
    const slice = perCore.slice(start, start + (c.cores || 0));
    const avg = isNum(c.util_pct) ? c.util_pct : meanOf(slice);
    const p = Math.max(0, Math.min(100, isNum(avg) ? avg : 0));
    const cls = isNum(avg) && avg >= 85 ? " is-warn" : "";
    return '<div class="pe">' +
      '<div class="pe-dial' + cls + '" style="--p:' + p.toFixed(1) + '">' +
      "<span>" + (isNum(avg) ? avg.toFixed(0) + " %" : "—") + "</span></div>" +
      '<div class="pe-label">' + esc(clusterBadge(c)) + " · " + (c.cores || 0) + " cores</div>" +
      '<div class="pe-sub">cpu ' + start + "–" + (start + (c.cores || 0) - 1) + "</div>" +
      "</div>";
  }).join("");
}

// L3 · per-core breakdown: every core, its cluster badge, its share, one row.
function perCoreRowsHtml() {
  const d = state.current || {};
  const cpu = d.cpu || {};
  const clusters = Array.isArray(cpu.clusters) ? cpu.clusters : [];
  const perCore = Array.isArray(cpu.per_core_pct) ? cpu.per_core_pct : [];
  if (!perCore.length) {
    return '<span class="na">per-core utilisation unavailable on this platform</span>';
  }
  const labels = [];
  clusters.forEach((c, ci) => {
    const badge = clusterBadge(c);
    const start = clusterSpan(clusters, ci);
    for (let n = 0; n < (c.cores || 0); n++) labels[start + n] = badge + (n + 1);
  });
  return '<div class="l3-cores">' + perCore.map((v, i) => {
    const name = labels[i] || "c" + (i + 1);
    const pct = Math.max(0, Math.min(100, isNum(v) ? v : 0));
    const cls = isNum(v) && v >= 85 ? " is-warn" : "";
    return '<div class="l3-core" title="core ' + i + " · " +
      (isNum(v) ? v.toFixed(1) + " %" : "not sampled") + '">' +
      "<span>" + esc(name) + "</span>" +
      '<span class="l3-bar"><i class="' + cls + '" style="width:' + pct.toFixed(1) + '"></i></span>' +
      "<span>" + (isNum(v) ? v.toFixed(0) + " %" : "—") + "</span></div>";
  }).join("") + "</div>";
}

// ------------------------------------------------------ L2 · top consumers
// Who consumed it, over the same window as the chart. Five rows in the L2
// expansion — the name and its share of total, nothing else.
//
// L1 deliberately shows none: a tile is number + sparkline + delta + one meta line, and
// a three-row process list inside a 160 px chip would be exactly the text wall
// T20 removes. The full virtualised consumer list is the consumers drawer's job,
// one click away from any row here.
const TOPCONS = { key: null, at: 0, rows: [], count: null, pending: false };

async function loadTopConsumers(force) {
  const key = "cpu:" + state.range;
  if (!force && TOPCONS.key === key && Date.now() - TOPCONS.at < CONS_TTL_MS) return;
  if (TOPCONS.pending) return;
  TOPCONS.pending = true;
  const d = await fetchJSON("topcons:" + key,
    "/api/consumers?module=cpu&range=" + encodeURIComponent(state.range) + "&limit=6");
  TOPCONS.pending = false;
  if (!d || !Array.isArray(d.rows)) return;
  TOPCONS.key = key;
  TOPCONS.at = Date.now();
  TOPCONS.rows = d.rows;
  TOPCONS.count = isNum(d.count) ? d.count : d.rows.length;
  paintL2Consumers();
}

function paintL2Consumers() {
  const box = $("l2-cons");
  if (!box) return;
  const rows = (TOPCONS.rows || []).slice(0, 5);
  const note = $("l2-cons-note");
  if (note) {
    note.textContent = rows.length
      ? "5 of " + (TOPCONS.count == null ? rows.length : TOPCONS.count) + " · " + state.range
      : esc(state.range);
  }
  if (!rows.length) {
    box.innerHTML = '<span class="na">no sweep in this range yet — the store ' +
      "sweeps every 20 s</span>";
    return;
  }
  box.innerHTML = rows.map((r) => {
    const share = isNum(r.share_pct) ? r.share_pct.toFixed(1) + " %" : "n/a";
    return '<button type="button" class="l2i-cons-row" data-app="' + esc(r.name || "") +
      '" title="' + esc(r.name || "") + " · " + esc(share) + ' of total CPU">' +
      '<span class="l2i-cons-name">' + esc(r.name || "?") + "</span>" +
      '<span class="l2i-cons-share">' + share + "</span></button>";
  }).join("");
  box.querySelectorAll("[data-app]").forEach((b) => {
    b.addEventListener("click", () => openDrawerForApp(b.dataset.app));
  });
}

// ------------------------------------------------------------ row 2 · tiles
// Same grammar as every other tile: number (big) + sparkline + ONE 10 px delta
// + ONE 10.5 px meta line. No coverage line here — coverage is a statement about
// a window, and L1 is about now. It lives in L2, where the window is.
function updateTiles() {
  const d = state.current || {};
  const net = d.net || {};
  const io = d.disk_io || {};
  const ph = state.powerHistory || {};
  const root = ph.root || {};
  const base = state.range + " avg";

  // Network — down now, up + busiest interface on the meta line.
  const ifaces = (net.ifaces || []).filter((i) => isNum(i.down_bps) || isNum(i.up_bps));
  const busy = ifaces.slice().sort((a, b) => (b.down_bps || 0) - (a.down_bps || 0))[0];
  const down = isNum(net.down_bps) ? net.down_bps
    : ifaces.reduce((a, i) => a + (i.down_bps || 0), 0);
  const up = isNum(net.up_bps) ? net.up_bps
    : ifaces.reduce((a, i) => a + (i.up_bps || 0), 0);
  setTile("network", {
    value: isNum(down) ? fmtBytes(down, 1) : null,
    delta: bytesDelta(down, graphMean("network", "down_bps"), base),
    meta: parts(
      isNum(up) ? "up " + fmtBytes(up, 1) : null,
      busy && busy.name ? busy.name : null
    ) || (net.reason || "no per-interface delta on this tick"),
  });
  const nSpark = chipSpark("network");
  drawSpark("vspark-network", last60(nSpark.points), nSpark.key);

  // Disk — read now, write + iops on the meta line.
  const rd = isNum(io.read_bps) ? io.read_bps : null;
  const wr = isNum(io.write_bps) ? io.write_bps : null;
  setTile("disk", {
    value: rd != null ? fmtBytes(rd, 1) : null,
    delta: bytesDelta(rd, graphMean("disk", "read_bps"), base),
    meta: parts(
      wr != null ? "write " + fmtBytes(wr, 1) : null,
      isNum(io.read_iops) ? io.read_iops.toFixed(0) + " read iops" : null
    ) || (io.available === false ? (io.reason || "no IOBlockStorageDriver delta")
      : "idle — 0 B/s measured"),
  });
  const dSpark = chipSpark("disk");
  drawSpark("vspark-disk", last60(dSpark.points), dSpark.key);

  // Thermal — the ONE tile that carries every temperature this machine reports
  // (CPU die, GPU, SSD, battery pack) as a compact list, over the CPU-temperature
  // sparkline. The ring only carries CPU/GPU; SSD and pack come from the SMC
  // registry, so a family that is not readable says "—" instead of a fake 0 °C.
  const temps = thermalSummary();
  const cpuC = temps[0].v;
  const gpuC = temps[1].v;
  const pressure = (sensorById("thermal.pressure") || {}).value || root.thermal_pressure || null;
  const hot = isNum(cpuC) && cpuC >= 85;
  const tWin = ((ph.windows || {})[state.range]) || {};
  setTile("thermal", {
    value: isNum(cpuC) ? cpuC.toFixed(1) : null,
    unit: "°C",
    word: "needs root",
    cls: hot ? "is-warn" : "",
    delta: delta(cpuC, isNum(tWin.cpu_temp_c) ? tWin.cpu_temp_c : null, "°C", 1, base),
    meta: isNum(cpuC)
      ? parts(isNum(gpuC) ? "GPU " + gpuC.toFixed(1) + " °C" : null,
        pressure ? "pressure " + pressure : null)
      : (root.reason || "die temperature needs the root daemon"),
  });
  const list = $("thermal-list");
  if (list) {
    list.innerHTML = temps.map((t) => '<span class="tl-row"><span class="tl-k">' +
      esc(t.label) + '</span><span class="tl-v">' +
      (isNum(t.v) ? t.v.toFixed(1) + " °C" : "—") + "</span></span>").join("");
  }
  const tSpark = chipSpark("thermal");
  drawSpark("vspark-thermal", last60(tSpark.points), tSpark.key);
}

// The four temperatures the Thermal tile promises, in that order, each from the
// read that actually produced it. The power ring carries CPU and GPU die °C; the
// SMC registry carries the SSD (TH0x) and the pack (TB0T) — so both are tried,
// ring first, and a missing family is null rather than a zero.
function thermalSummary() {
  const root = (state.powerHistory || {}).root || {};
  const g = sensorGroup("thermal") || {};
  const rows = (g.sensors || []).filter((s) => s.available !== false && isNum(s.value));
  const byId = (id) => {
    const hit = rows.find((s) => s.id === id);
    return hit ? hit.value : null;
  };
  const byLabel = (re) => {
    const hit = rows.find((s) => re.test(s.label || "") || re.test(s.id || ""));
    return hit ? hit.value : null;
  };
  const cpu = isNum(root.cpu_temp_c) ? root.cpu_temp_c
    : isNum(root.cpu_avg_c) ? root.cpu_avg_c
      : byId("thermal.smc.cpu_avg") != null ? byId("thermal.smc.cpu_avg")
        : byLabel(/\bCPU\b|Tp0/);
  const gpu = isNum(root.gpu_temp_c) ? root.gpu_temp_c
    : isNum(root.gpu_avg_c) ? root.gpu_avg_c
      : byId("thermal.smc.gpu_avg") != null ? byId("thermal.smc.gpu_avg")
        : byLabel(/\bGPU\b|Tg0/);
  return [
    { label: "CPU die", v: cpu },
    { label: "GPU", v: gpu },
    { label: "SSD", v: byId("thermal.smc.ssd") != null ? byId("thermal.smc.ssd") : byLabel(/\bSSD\b|TH0/) },
    { label: "Battery", v: byId("thermal.smc.battery") != null ? byId("thermal.smc.battery") : byLabel(/Battery|TB0/) },
  ];
}

// A byte-rate delta is not a percentage: it is a difference between two rates,
// so it is written with the same formatter as the number it belongs to.
function bytesDelta(cur, base, baseLabel) {
  if (!isNum(cur) || !isNum(base)) return "";
  const d = cur - base;
  if (Math.abs(d) < 1024) return "= " + baseLabel;
  return (d > 0 ? "+" : "−") + fmtBytes(Math.abs(d), 1) + " vs " + baseLabel;
}

// The tile twin of setChip: number + unit (folded into the number so the 24 px
// figure keeps its weight) + the 10 px delta + the one meta line.
function setTile(id, opts) {
  const o = opts || {};
  const num = $("t-" + id);
  if (num) {
    const has = o.value != null && o.value !== "";
    const txt = has ? String(o.value) + (o.unit ? " " + o.unit : "")
      : (o.word || "no sample");
    num.textContent = txt;
    num.className = "tile-num" + (has ? "" : " na") + (o.cls ? " " + o.cls : "");
  }
  const del = $("t-" + id + "-delta");
  if (del) del.textContent = o.delta || "";
  const meta = $("t-" + id + "-meta");
  if (meta) meta.textContent = o.meta || "";
}

// The chart box scrolls horizontally by design: the canvas is laid out at a
// readable density (3 px per plotted point, 720 px floor, 6000 px ceiling) and
// the box keeps the newest sample in view. A short window therefore fits, a long
// one becomes a scrub surface — the 6 px scrollbar is a real control, never
// decoration.
function sizeChartInner(canvasId, points) {
  const cv = $(canvasId);
  if (!cv) return null;
  const inner = cv.closest(".chart-inner");
  if (!inner) return null;
  const n = (points || []).length;
  const w = Math.max(720, Math.min(6000, Math.round(n * 3)));
  inner.style.width = w + "px";
  return cv.closest(".chart-scroll");
}

function keepLiveEdge(scroller) {
  if (!scroller) return;
  scroller.scrollLeft = Math.max(0, scroller.scrollWidth - scroller.clientWidth);
}

// -------------------------------------------------------------- L1 painting
// One metric at a time: a slow endpoint must not hold every other sparkline
// hostage, so each chip paints the moment ITS window arrives. L1 has no chart —
// the nine sparklines are all it draws, which is why a 1 s repaint is cheap.
function paintBentoMetric(metric) {
  const s = chipSpark(metric);
  const cv = $("vspark-" + metric);
  if (!cv) return;
  drawSparkCanvas(cv, last60(s.points), s.key);
}

function paintBentoCharts() {
  BENTO_MODULES.forEach(paintBentoMetric);
  paintOpenL2();
  if (FOCUS.module) { paintFocusChart(FOCUS.module); paintFocusSide(); }
  // the 15 s 1 m read: when it lands, the nine sparklines get their real shape
  loadSparkSeries(false).then(() => {
    if (state.view !== "instrument") return;
    BENTO_MODULES.forEach(paintBentoMetric);
    if (FOCUS.module) { paintFocusChart(FOCUS.module); paintFocusSide(); }
  });
}

// Every 1 s tick: numbers + the nine 60 s sparklines. Charts (L2/L3) repaint on
// the 5 s pass, and only while they are open.
function updateBento() {
  updateVitals();
  updateTiles();
  if (l2Open()) {
    loadTopConsumers(false);
    paintL2Side();
  }
}

function renderInstrument() {
  const inst = $("instrument");
  const view = $("view");
  const stage = $("stage");
  if (inst) inst.hidden = false;
  if (view) view.hidden = true;
  if (stage) stage.classList.remove("is-page");
  updateBento();
  paintBentoCharts();
}

function updateInstrument() { updateBento(); }

// ===================================================== L2 · inline expansion
// A module expands IN PLACE. There is no backdrop, no slide-over and no route:
// a slot inside the bento (between the chips and the tiles for a chip, after the
// tiles for a tile) opens in flow, so everything below moves down, and the rest
// of L1 dims to 0.6 without ever disappearing. That is the whole difference
// between L2 and a drawer — you never lose the context you clicked from.
//
//   L2  the full horizontally-scrollable chart (drag to zoom) · measures
//       (mean · peak · p95) · top 5 consumers · coverage · the energy line.
//       For Thermal the side column is the full SMC sensor list instead.
//   L3  one toggle deeper, collapsed by default: findings, records, per-core
//       breakdown, the raw SMC key table and the peak moments.

const L2 = { module: null, appName: null, appPanel: false, l3: false };
const L2CONS_CACHE = {};   // "module:range" -> /api/consumers payload
const L2TOP = {};          // module -> its top consumer, for the records line

const TEMP_SPECS = [
  { key: "cpu_temp_c", label: "CPU die", color: ACCENT, fill: true },
  { key: "gpu_temp_c", label: "GPU die", color: MUTED, dash: [3, 2] },
];
const TEMP_Y_FMT = (v) => (isNum(v) ? v.toFixed(0) + " °C" : "");

function l2SlotFor(module) {
  // T22: one tile grid, so there is one expansion slot — after the grid, in
  // flow. (`module` is kept in the signature: the slot is the same for every
  // module, and callers should not care.)
  return $("l2SlotTiles");
}

function l2ModuleSeries(module) {
  if (module === "power" || module === "thermal") {
    const ph = state.powerHistory || {};
    return {
      kind: "multi",
      specs: module === "power" ? POWER_SPECS : TEMP_SPECS,
      fmt: module === "power" ? Y_FMT.power : TEMP_Y_FMT,
      points: ph.points || [],
      ticks: ph.ticks || [],
      reason: ph.available === false ? "no power ring on this machine"
        : "no samples in this range yet — the ring fills at 15 s",
      data: ph,
    };
  }
  const g = state.graphs[module] || {};
  return {
    kind: "single", metric: module, points: g.points || [], ticks: g.ticks || [],
    reason: g.unavailable_reason || "no samples in this range yet", data: g,
  };
}

// Coverage is a claim about how much of the WINDOW this module measured, so each
// series is measured against its own samples. The power ring also carries history
// rows seeded from the collector (which have watts but no die temperature), so
// Thermal must count temperature samples only — otherwise a single °C reading in
// a seeded hour would be reported as 100 % coverage of that hour.
function l2CoverageHtml(module) {
  const ph = state.powerHistory || {};
  const pm = ph.powermetrics || {};
  const cadence = isNum(pm.interval_ms) ? "powermetrics " + (pm.interval_ms / 1000) + " s" : null;
  if (module === "thermal") {
    const pts = (ph.points || []).filter((p) => isNum(p.cpu_temp_c));
    const window_s = RANGE_S[state.range] || 3600;
    const bits = [];
    if (pts.length >= 1) {
      const span = pts[pts.length - 1].ts - pts[0].ts +
        (isNum(ph.sampling_s) ? ph.sampling_s : 15);
      const covered = Math.max(0, Math.min(100, (span / window_s) * 100));
      bits.push("measured <b>" + covered.toFixed(0) + " %</b> of " + state.range);
    } else {
      bits.push("<b>0 %</b> measured of " + state.range + " — no die-temperature sample yet");
    }
    bits.push(pts.length + " °C samples (live ring)");
    if (cadence) bits.push(cadence);
    return bits.join(" · ");
  }
  if (module === "power") {
    // powerCoverageHtml already carries the server's "sampling every 15 s" line
    return powerCoverageHtml();
  }
  return coverHtml(state.graphs[module]);
}

// Findings: 3–4 sentences, each one a measured statement. No sentence here is
// allowed to invent a number — every figure comes from the payload on screen.
function l2Findings(module) {
  const box = $("l2-findings");
  if (!box) return;
  const out = [];
  const d = state.current || {};
  const ph = state.powerHistory || {};
  const root = ph.root || {};
  const rootLive = root.available === true;
  const cpuC = isNum(root.cpu_temp_c) ? root.cpu_temp_c : (isNum(root.cpu_avg_c) ? root.cpu_avg_c : null);
  const gpuC = isNum(root.gpu_temp_c) ? root.gpu_temp_c : (isNum(root.gpu_avg_c) ? root.gpu_avg_c : null);

  if (module === "thermal") {
    const pressure = (sensorById("thermal.pressure") || {}).value || root.thermal_pressure || null;
    const tpts = (ph.points || []).filter((p) => isNum(p.cpu_temp_c));
    out.push(tpts.length
      ? "The temperature series holds <b>" + tpts.length + "</b> samples of CPU die °C over " +
        state.range + ", recorded by the root daemon at the powermetrics cadence (15 s)."
      : "The temperature series is still filling — the ring starts at 15 s and has no " +
        "sample inside " + state.range + " yet. Gaps stay gaps, never 0 °C.");
    out.push(isNum(cpuC)
      ? "Right now: CPU <b>" + cpuC.toFixed(1) + " °C</b>, GPU <b>" +
        (isNum(gpuC) ? gpuC.toFixed(1) : "—") + " °C</b>, from the root SMC read " +
        (isNum(root.age_s) ? root.age_s.toFixed(0) + " s old" : "just now") + "."
      : "Die temperature reads <b>needs root</b> — the powermetrics daemon has not " +
        "written a sample, so no °C is shown.");
    out.push("Thermal pressure is <b>" + esc(pressure || "not reported") + "</b>; it comes " +
      "from the same privileged read as the die temperatures.");
    out.push(rootLive
      ? "Package rails: CPU " + (isNum(root.cpu_w) ? fmtW(root.cpu_w) : "—") + ", GPU " +
        (isNum(root.gpu_w) ? fmtW(root.gpu_w) : "—") + ", ANE " +
        (isNum(root.ane_w) ? fmtW(root.ane_w) : "—") + "."
      : "Grant the power daemon in ⚙ settings → permissions to fill this module.");
    box.innerHTML = out.map((t) => "<p>" + t + "</p>").join("");
    return;
  }

  if (module === "power") {
    const pcur = ph.current || {};
    const an = ph.analysis || {};
    out.push("Battery draw is <b>" + (isNum(pcur.battery_w) ? fmtW(pcur.battery_w) : "not measured")
      + "</b> now; package power is <b>" +
      (rootLive && isNum(root.package_w) ? fmtW(root.package_w) : "needs root") + "</b>" +
      (rootLive ? " from the root sample " + (isNum(root.age_s) ? root.age_s.toFixed(0) + " s old" : "just now") : "") + ".");
    out.push("Over " + state.range + " the ring holds <b>" + (isNum(ph.samples) ? ph.samples : 0) +
      "</b> samples at " + (isNum(ph.sampling_s) ? ph.sampling_s : 15) + " s cadence (" +
      (isNum(ph.live_samples) ? ph.live_samples : 0) + " live + " +
      (isNum(ph.history_samples) ? ph.history_samples : 0) + " seeded).");
    const pk = (an.battery || {}).peak_w;
    out.push("Peak draw in this window: <b>" + (isNum(pk) ? fmtW(pk) : "not measurable") + "</b>" +
      (isNum((an.battery || {}).peak_ts) ? " at " + fmtTime(an.battery.peak_ts) : "") +
      (isNum((an.energy || {}).battery_wh) ? " · " + an.energy.battery_wh.toFixed(3) + " Wh drawn" : "") + ".");
    out.push("Series are drawn on one axis, not stacked, and adapter input stays a stat — " +
      "67 W on the same axis would flatten every rail.");
    box.innerHTML = out.map((t) => "<p>" + t + "</p>").join("");
    return;
  }

  // graph-backed modules
  const g = state.graphs[module] || {};
  const m = (g.measures || {}).fields || {};
  const spec = MODULE_SERIES[module] || {};
  const primary = (g.measures || {}).primary || spec.key;
  const f = m[primary] || {};
  const label = MODULE_LABEL[module] || module;
  const nowVal = currentModuleValue(module);

  if (isNum(f.mean)) {
    out.push(label + " averaged <b>" + spec.fmt(f.mean) + "</b> over " + state.range +
      (isNum(f.max) ? ", peaking at <b>" + spec.fmt(f.max) + "</b>" +
        (isNum(f.max_ts) ? " at " + fmtTime(f.max_ts) : "") : "") + ".");
  } else {
    out.push(label + " has no measured sample in the " + state.range + " window yet — " +
      "the line is empty on purpose, not zero-filled.");
  }
  const cov = (g.measures || {}).coverage_pct;
  out.push("Coverage is <b>" + (isNum(cov) ? cov.toFixed(0) : 0) + " %</b> of the window (" +
    (isNum((g.measures || {}).points) ? g.measures.points : (g.points || []).length) +
    " points); gaps stay gaps, and the store never interpolates a sample it did not take.");
  if (isNum(nowVal) && isNum(f.mean)) {
    const dpp = nowVal - f.mean;
    out.push("Right now <b>" + spec.fmt(nowVal) + "</b>, " +
      (Math.abs(dpp) < 0.05 ? "level with the window mean" :
        (dpp > 0 ? "above" : "below") + " the window mean (<b>" + (dpp > 0 ? "+" : "−") +
        moduleDelta(spec, dpp) + "</b>)") + ".");
  } else if (isNum(nowVal)) {
    out.push("Right now <b>" + spec.fmt(nowVal) + "</b> from the 1 s tick.");
  }
  out.push("Measured by " + esc(g.source || "the 1 s sampler") + " — every number on this " +
    "page names the read that produced it.");
  box.innerHTML = out.slice(0, 4).map((t) => "<p>" + t + "</p>").join("");
}

function currentModuleValue(module) {
  const d = state.current || {};
  if (module === "cpu") {
    const c = d.cpu || {};
    return isNum(c.total_pct) ? c.total_pct : meanOf(c.per_core_pct);
  }
  if (module === "memory") return (d.ram || {}).pct;
  if (module === "gpu") return (d.gpu || {}).util_pct;
  if (module === "battery") return (d.battery || {}).pct;
  if (module === "network") return isNum((d.net || {}).down_bps) ? d.net.down_bps : null;
  if (module === "disk") return (d.disk_io || {}).read_bps;
  if (module === "load") return (d.load || {}).one;
  return null;
}

const RECORD_METRICS = {
  cpu: ["cpu"], memory: ["memory"], gpu: ["gpu"], disk: ["disk"],
  network: ["net_down", "net_up", "network"], power: ["energy", "power", "battery"],
  battery: ["battery", "energy"], thermal: ["thermal"], load: ["load"],
};

// Records: the server's own record store when it is already on the page (the
// Analytics view loads it), otherwise the extremes this range really measured.
function l2Records(module) {
  const box = $("l2-records");
  if (!box) return;
  const rows = [];
  const an = state.analytics;
  if (an && an.range === state.range && Array.isArray(an.records)) {
    const keys = RECORD_METRICS[module] || [module];
    an.records.forEach((r) => {
      if (keys.indexOf(r.metric) < 0) return;
      rows.push({ ts: r.ts, text: r.text });
    });
  }
  const g = state.graphs[module];
  if (g && (g.measures || {}).fields) {
    const spec = MODULE_SERIES[module] || {};
    Object.keys(g.measures.fields).forEach((k) => {
      const f = g.measures.fields[k];
      if (!isNum(f.max) || !spec.fmt) return;
      rows.push({ ts: f.max_ts, text: k + " peaked " + spec.fmt(f.max) });
      if (isNum(f.min)) rows.push({ ts: null, text: k + " low " + spec.fmt(f.min) });
    });
  }
  if (module === "power" || module === "battery") {
    const an2 = (state.powerHistory || {}).analysis || {};
    const b = an2.battery || {};
    if (isNum(b.peak_w)) rows.push({ ts: b.peak_ts, text: "draw peaked " + fmtW(b.peak_w) });
    if (isNum((an2.energy || {}).battery_wh)) {
      rows.push({ ts: null, text: (an2.energy.battery_wh).toFixed(3) + " Wh drawn in " + state.range });
    }
  }
  if (module === "thermal") {
    const root = (state.powerHistory || {}).root || {};
    const c = isNum(root.cpu_temp_c) ? root.cpu_temp_c : root.cpu_avg_c;
    if (isNum(c)) rows.push({ ts: root.age_s ? Date.now() / 1000 - root.age_s : null,
      text: "CPU die " + c.toFixed(1) + " °C (SMC)" });
    if (isNum(root.package_w)) rows.push({ ts: null, text: "package " + fmtW(root.package_w) });
    if (root.thermal_pressure) rows.push({ ts: null, text: "pressure " + root.thermal_pressure });
    const tpts = ((state.powerHistory || {}).points || []).filter((p) => isNum(p.cpu_temp_c));
    if (tpts.length) {
      const hottest = tpts.reduce((a, b) => (b.cpu_temp_c > a.cpu_temp_c ? b : a));
      rows.push({ ts: hottest.ts, text: "ring peak " + hottest.cpu_temp_c.toFixed(1) + " °C" });
    }
  }
  const top = L2TOP[module];
  if (top && top.range === state.range) {
    rows.push({ ts: null, text: "top consumer " + top.name +
      (isNum(top.share_pct) ? " · " + top.share_pct.toFixed(1) + " % of total" : "") });
  }
  if (!rows.length) {
    box.innerHTML = '<span class="na">no record was written for this module in ' +
      esc(state.range) + "</span>";
    return;
  }
  box.innerHTML = rows.slice(0, 12).map((r) =>
    '<div class="l3-record"><span class="ts">' + (isNum(r.ts) ? esc(fmtTime(r.ts)) : "—") +
    "</span><span>" + esc(r.text) + "</span></div>").join("");
}

// Peak moments: the highest measured point of every field the window holds, with
// its timestamp. Derived from the same payload as the chart — nothing invented.
function peakMomentsHtml(module) {
  const rows = [];
  if (module === "power" || module === "thermal") {
    const windows = (state.powerHistory || {}).windows || {};
    const w = windows[state.range] || windows["1h"] || {};
    const keys = module === "thermal"
      ? [["cpu_temp_c", "CPU die", (v) => v.toFixed(1) + " °C"],
         ["gpu_temp_c", "GPU die", (v) => v.toFixed(1) + " °C"]]
      : [["package_w", "package", fmtW], ["battery_w", "battery draw", fmtW],
         ["cpu_w", "CPU rail", fmtW], ["gpu_w", "GPU rail", fmtW]];
    keys.forEach(([k, label, fmt]) => {
      if (isNum(w[k + "_max"])) rows.push([label + " peak", fmt(w[k + "_max"])]);
      if (isNum(w[k])) rows.push([label + " mean", fmt(w[k])]);
    });
    if (isNum(w.max_ts)) rows.push(["peak at", fmtTime(w.max_ts)]);
    if (isNum(w.coverage_s)) rows.push(["coverage", fmtSpan(w.coverage_s) + " of " + state.range]);
    if (isNum(w.bucket_s)) rows.push(["bucket", w.bucket_s + " s"]);
  } else {
    const g = state.graphs[module] || {};
    const fields = (g.measures || {}).fields || {};
    const spec = MODULE_SERIES[module] || {};
    Object.keys(fields).forEach((k) => {
      const f = fields[k];
      if (!isNum(f.max) || !spec.fmt) return;
      rows.push([k + " peak", spec.fmt(f.max) + (isNum(f.max_ts) ? " · " + fmtTime(f.max_ts) : "")]);
      if (isNum(f.p95)) rows.push([k + " p95", spec.fmt(f.p95)]);
    });
  }
  if (!rows.length) return '<span class="na">no measured peak in this window yet</span>';
  return '<table class="l3-table"><tbody>' + rows.slice(0, 8).map(([k, v]) =>
    "<tr><td>" + esc(k) + "</td><td>" + esc(v) + "</td></tr>").join("") + "</tbody></table>";
}

// Every module shows its own energy contribution in Wh when a rail can actually
// measure it — CPU, GPU and ANE are separate rails in the root powermetrics
// sample, package is their sum. Memory, disk and network have no power rail on
// this machine, and the line says so instead of inventing a number.
const MODULE_ENERGY_RAIL = {
  cpu: ["cpu_wh", "CPU rail"],
  gpu: ["gpu_wh", "GPU rail"],
  power: ["package_wh", "package (CPU + GPU + ANE)"],
  battery: ["battery_wh", "battery draw integral"],
  thermal: ["package_wh", "package (CPU + GPU + ANE)"],
};

function l2EnergyLine(module) {
  const rail = MODULE_ENERGY_RAIL[module];
  if (!rail) {
    return "energy contribution · <span class='na'>not separately measurable — this " +
      "machine exposes power rails for CPU, GPU, ANE and the pack, not for " +
      esc(module) + "</span>";
  }
  const an = (state.powerHistory || {}).analysis || {};
  const wh = (an.energy || {})[rail[0]];
  if (!isNum(wh)) {
    return "energy contribution · " + rail[1] + " · <span class='na'>" +
      (an.energy ? "not measured in " + state.range + " (needs root / no sample)"
        : "no power window loaded yet") + "</span>";
  }
  return "energy contribution · " + esc(rail[1]) + " used <b>" + wh.toFixed(4) +
    " Wh</b> over " + esc(state.range) + " — integrated from measured W, gaps > 120 s excluded";
}

// The full SMC sensor list: every temperature the root reader produced, with its
// own value. This is L2 for Thermal — the °C list is the module's content, not a
// detail hidden one layer down.
function smcListHtml() {
  const g = sensorGroup("thermal");
  if (!g) return '<span class="na">the sensor registry has no thermal group yet</span>';
  if (g.needs_root) return lockedGroupHtml(g);
  const rows = (g.sensors || []).filter((s) => s.available !== false);
  if (!rows.length) {
    return '<span class="na">the registry reports no readable temperature sensor — ' +
      esc(((g.sensors || [])[0] || {}).reason || "needs root") + "</span>";
  }
  return rows.map((s) => '<div class="stat-row"><span class="label">' + esc(s.label) +
    '</span><span class="val">' + esc(sensorText(s) || "—") + "</span></div>").join("") +
    '<div class="reason">' + rows.length + " SMC sensors · each row names the read that " +
    "produced it in the source table below (details)</div>";
}

// L3 for Thermal: the raw key table — key · label · value · unit.
function smcTableHtml() {
  const g = sensorGroup("thermal");
  const rows = ((g || {}).sensors || []);
  if (!rows.length) return '<span class="na">no SMC keys reported by the registry</span>';
  return '<table class="l3-table"><thead><tr><th>key</th><th>label</th>' +
    "<th>value</th><th>unit</th></tr></thead><tbody>" +
    rows.map((s) => "<tr><td>" + esc(s.id || "") + "</td><td>" + esc(s.label || "") +
      "</td><td>" + esc(s.available === false || s.value == null ? "—" : String(s.value)) +
      "</td><td>" + esc(s.unit || "") + "</td></tr>").join("") +
    "</tbody></table>";
}

function l2BodyHtml(module) {
  const label = MODULE_LABEL[module] || module;
  const thermal = module === "thermal";
  const chart = '<div class="l2i-chart">' +
    '<span class="micro">chart · ' + esc(label) + " · " + esc(state.range) +
    ' · drag to zoom · double-click resets</span>' +
    '<div class="chart-scroll"><div class="chart-inner">' +
    '<canvas id="l2-chart"></canvas><div class="brush" id="l2-brush" hidden></div>' +
    "</div></div>" +
    '<p class="cov" id="l2-cov">' + l2CoverageHtml(module) + "</p>" +
    '<p class="cov" id="l2-energy">' + l2EnergyLine(module) + "</p>" +
    "</div>";
  const side = '<div class="l2i-side">' +
    '<div><span class="micro">measures · ' + esc(state.range) + "</span>" +
    '<div id="l2-measures">' + measuresHtml(module) + "</div></div>" +
    (module === "cpu"
      ? '<div class="pe-row" id="l2-rings">' + clusterRingsHtml() + "</div>" : "") +
    '<div><span class="micro">' +
    (thermal ? "SMC sensors · °C"
      : CONSUMER_SECTIONS.indexOf(module) >= 0
        ? 'top consumers · <span id="l2-cons-note"></span>'
        : "attribution · by process") + "</span>" +
    (thermal ? '<div id="l2-sensors">' + smcListHtml() + "</div>"
      : '<div class="l2i-cons" id="l2-cons"><span class="na">loading…</span></div>') +
    "</div></div>";
  return '<div class="l2i-head">' +
    '<span class="l2i-kind">L2 · module</span>' +
    "<h3>" + esc(label) + "</h3>" +
    '<span class="l2i-live">' + esc(MODULE_SOURCE[module] || "") + "</span>" +
    '<span class="l2i-actions">' +
    '<button class="l2i-btn" type="button" id="l2Details" aria-expanded="false" ' +
    'aria-controls="l2-l3">details ▾</button>' +
    '<button class="l2i-btn" type="button" id="l2iClose" aria-label="collapse">✕</button>' +
    "</span></div>" +
    '<div class="l2i-grid">' + chart + side + "</div>" +
    '<div class="l3" id="l2-l3" hidden>' +
    '<div class="l3-col">' +
    '<span class="micro">findings</span>' +
    '<div class="l3-findings" id="l2-findings"><span class="na">computing…</span></div>' +
    '<span class="micro" style="margin-top:9px">records · this range</span>' +
    '<div id="l2-records"><span class="na">loading…</span></div>' +
    "</div>" +
    '<div class="l3-col">' +
    (thermal
      ? '<span class="micro">raw SMC keys · key · label · value · unit</span>' +
        '<div id="l2-smc">' + smcTableHtml() + "</div>"
      : '<span class="micro">per core · now</span><div id="l2-cores">' +
        perCoreRowsHtml() + "</div>") +
    '<span class="micro" style="margin-top:9px">peak moments</span>' +
    '<div id="l2-peaks">' + peakMomentsHtml(module) + "</div>" +
    "</div></div>";
}

function paintL2Chart(module) {
  const cv = $("l2-chart");
  if (!cv) return;
  const ser = l2ModuleSeries(module);
  const scroller = sizeChartInner("l2-chart", ser.points);
  const cov = $("l2-cov");
  if (cov) cov.innerHTML = l2CoverageHtml(module);
  if (ser.kind === "multi") {
    const painted = paintMulti("l2-chart", ser.specs, ser.points, ser.ticks, ser.fmt);
    if (!painted) clearChart("l2-chart", ser.ticks, ser.reason);
  } else if (ser.kind === "single") {
    const painted = paintSeries("l2-chart", module, ser.points, ser.ticks);
    if (!painted) clearChart("l2-chart", ser.ticks, ser.reason);
  }
  wireBrush("l2-chart");
  keepLiveEdge(scroller);
}

// Everything in L2 that is text, refreshed on the 5 s pass — and only while the
// expansion is open, so a closed L2 costs nothing.
function paintL2Side() {
  const module = L2.module;
  if (!module) return;
  const meas = $("l2-measures");
  if (meas) meas.innerHTML = measuresHtml(module);
  const rings = $("l2-rings");
  if (rings) rings.innerHTML = clusterRingsHtml();
  const sensors = $("l2-sensors");
  if (sensors) sensors.innerHTML = smcListHtml();
  if (L2.l3) {
    const cores = $("l2-cores");
    if (cores) cores.innerHTML = perCoreRowsHtml();
    const smc = $("l2-smc");
    if (smc) smc.innerHTML = smcTableHtml();
    const peaks = $("l2-peaks");
    if (peaks) peaks.innerHTML = peakMomentsHtml(module);
    l2Findings(module);
    l2Records(module);
  }
}

function paintOpenL2() {
  if (!L2.module) return;
  paintL2Chart(L2.module);
  paintL2Side();
}

async function mountL2Consumers(module) {
  const host = $("l2-cons");
  if (!host) return;
  if (CONSUMER_SECTIONS.indexOf(module) < 0) {
    host.innerHTML = '<div class="reason">' + esc(module === "battery"
      ? "battery drain is not attributable per process: draw is read from the pack " +
        "(V × A), and the per-PID rusage this SDK exposes carries no energy field " +
        "for that split."
      : "no per-process store for this module — " + esc(module) + " is a system-wide " +
        "rate, not an attribution.") + "</div>";
    return;
  }
  const key = module + ":" + state.range;
  let payload = L2CONS_CACHE[key];
  if (!payload) {
    payload = await fetchJSON("l2cons:" + key,
      "/api/consumers?module=" + encodeURIComponent(module) +
      "&range=" + encodeURIComponent(state.range) + "&limit=6");
    if (!payload || !Array.isArray(payload.rows)) {
      if (L2.module === module) {
        host.innerHTML = '<span class="na">the consumer store returned no rows for ' +
          esc(state.range) + "</span>";
      }
      return;
    }
    L2CONS_CACHE[key] = payload;
  }
  if (L2.module !== module) return;     // the user already moved on
  const rows = payload.rows.slice(0, 5);
  const note = $("l2-cons-note");
  if (note) {
    note.textContent = rows.length
      ? rows.length + " of " + (isNum(payload.count) ? payload.count : rows.length) + " · " + state.range
      : esc(state.range);
  }
  if (!rows.length) {
    host.innerHTML = '<span class="na">no sweep in this range yet — the store ' +
      "sweeps every 20 s</span>";
    return;
  }
  host.innerHTML = rows.map((r) => {
    const share = isNum(r.share_pct) ? r.share_pct.toFixed(1) + " %" : "n/a";
    return '<button type="button" class="l2i-cons-row" data-app="' + esc(r.name || "") +
      '" title="' + esc(r.name || "") + " · " + esc(share) + ' of total CPU">' +
      '<span class="l2i-cons-name">' + esc(r.name || "?") + "</span>" +
      '<span class="l2i-cons-share">' + share + "</span></button>";
  }).join("");
  host.querySelectorAll("[data-app]").forEach((b) => {
    b.addEventListener("click", () => openDrawerForApp(b.dataset.app));
  });
  const first = rows[0];
  if (first) {
    L2TOP[module] = { range: state.range, name: first.name, share_pct: first.share_pct };
    if (L2.l3) l2Records(module);
  }
}

// Mark the clicked object open and clear the mark from every other one, so the
// 0.6 dimming has exactly one thing at full opacity.
function setOpenMark(module) {
  BENTO_MODULES.forEach((m) => {
    const el = document.querySelector('#instrument [data-module="' + m + '"]');
    if (!el) return;
    el.classList.toggle("is-open", m === module);
    el.setAttribute("aria-expanded", String(m === module));
  });
}

function wireL2Buttons() {
  const details = $("l2Details");
  if (details) {
    details.addEventListener("click", () => {
      const box = $("l2-l3");
      if (!box) return;
      L2.l3 = box.hidden;
      box.hidden = !L2.l3;
      details.setAttribute("aria-expanded", String(L2.l3));
      details.textContent = L2.l3 ? "details ▴" : "details ▾";
      if (L2.l3 && L2.module) {
        l2Findings(L2.module);
        l2Records(L2.module);
        paintL2Side();
      }
    });
  }
  const close = $("l2iClose");
  if (close) close.addEventListener("click", closeL2);
}

function openL2(module) {
  if (!module || BENTO_MODULES.indexOf(module) < 0) return;
  // opened from a knowledge card inside ⚙ settings? the expansion lives on the
  // instrument, so put the drawer away first and stay on the same page.
  if (state.settingsOpen) closeSettings(true);
  // clicking the open one collapses it; clicking another switches the expansion
  if (L2.module === module) { closeL2(); return; }
  closeL2();
  L2.module = module;
  L2.l3 = false;
  // one state at a time: opening the deep window collapses the focus ring, so
  // the 0.6 L2 dim and the 0.35 focus dim can never stack
  if (FOCUS.module) blurTile();
  const slot = l2SlotFor(module);
  if (!slot) return;
  slot.innerHTML = '<article class="l2-inline" data-module="' + esc(module) +
    '">' + l2BodyHtml(module) + "</article>";
  slot.hidden = false;
  setOpenMark(module);
  const inst = $("instrument");
  if (inst) inst.classList.add("is-expanded");
  const stage = $("stage");
  if (stage) stage.classList.add("is-expanded");
  document.body.classList.add("has-l2");
  requestAnimationFrame(() => paintL2Chart(module));
  l2Findings(module);
  l2Records(module);
  mountL2Consumers(module);
  loadTopConsumers(false);
  wireL2Buttons();
  // Opened from a view that does not keep this module's series in memory?
  // Read it, then repaint the same expansion.
  const spec = MODULE_SERIES[module] || {};
  if (spec.metric && !state.graphs[module]) {
    loadGraph(module).then(() => {
      if (L2.module !== module) return;
      paintL2Chart(module);
      paintL2Side();
    });
  }
  if (!state.powerHistory && (module === "power" || module === "thermal" ||
      module === "battery" || module === "cpu" || module === "gpu")) {
    loadPowerHistory().then(() => {
      if (L2.module !== module) return;
      paintL2Chart(module);
      paintL2Side();
    });
  }
}

function closeL2() {
  const module = L2.module;
  if (!module) return;
  ["l2SlotTiles"].forEach((id) => {
    const slot = $(id);
    if (!slot) return;
    slot.hidden = true;
    slot.innerHTML = "";
  });
  setOpenMark(null);
  L2.module = null;
  L2.l3 = false;
  const inst = $("instrument");
  if (inst) inst.classList.remove("is-expanded");
  const stage = $("stage");
  if (stage) stage.classList.remove("is-expanded");
  document.body.classList.remove("has-l2");
  if (CHARTS["l2-chart"]) {
    try { CHARTS["l2-chart"].destroy(); } catch (e) { /* already gone */ }
    delete CHARTS["l2-chart"];
  }
}

function l2Open() { return !!L2.module; }

// The one overlay that is left: an application's own series (opened from the
// Apps table). It is an OVERLAY because there is no page underneath it to keep —
// you are not looking at the instrument when you open it.
function closeAppPanel() {
  const el = $("l2");
  if (!el || el.hidden) {
    L2.appPanel = false;
    L2.appName = null;
    return;
  }
  el.classList.remove("is-open");
  el.setAttribute("aria-hidden", "true");
  document.body.classList.remove("has-drawer");
  L2.appPanel = false;
  L2.appName = null;
  setTimeout(() => {
    if (el.classList.contains("is-open")) return;
    el.hidden = true;
    const body = $("l2Body");
    if (body) body.innerHTML = "";
  }, 220);
}

// ============================================================ hover-to-reveal
// A quick look, never a click: hover a chip or a tile and a 30 px mini chart of
// the last 60 s appears below it with the read that produced it. The tooltip is
// pointer-events:none, so it can never steal the pointer from the thing it
// describes; leaving the chip hides it after 200 ms.
const HOVER = { module: null, hideAt: 0, timer: 0, cache: {} };

// The last 60 s at the metric's own cadence. For the 1 s metrics that means a
// fresh 1 m read (60 points), cached 15 s — the 1 h window on the page is far too
// coarse to answer "the last minute". For power/thermal the 15 s ring already IS
// the last minute, so it is read in place.
async function ensureHoverSeries(module) {
  if (module === "power" || module === "thermal") return;
  const hit = HOVER.cache[module];
  if (hit && Date.now() - hit.at < 15000) return;
  const metric = (MODULE_SERIES[module] || {}).metric;
  if (!metric) return;
  const d = await fetchJSON("tip:" + metric,
    "/api/graph?metric=" + encodeURIComponent(metric) + "&range=1m");
  if (d) HOVER.cache[module] = { at: Date.now(), points: d.points || [] };
}

function hoverPoints(module) {
  const hit = HOVER.cache[module];
  if (hit) {
    const key = (MODULE_SERIES[module] || {}).key;
    return { points: last60(hit.points), key: key };
  }
  const s = chipSpark(module);   // already the 1 m spark series when it is loaded
  return { points: last60(s.points), key: s.key };
}

function placeHoverTip(el) {
  const tip = $("hoverTip");
  if (!tip || !el) return;
  const r = el.getBoundingClientRect();
  const w = 208;
  let left = r.left + r.width / 2 - w / 2;
  left = Math.max(6, Math.min(left, window.innerWidth - w - 6));
  tip.style.left = left + "px";
  tip.style.top = (r.bottom + 6) + "px";
}

function showHoverTip(el, module) {
  const tip = $("hoverTip");
  if (!tip || !el) return;
  clearTimeout(HOVER.timer);
  HOVER.hideAt = 0;
  HOVER.module = module;
  $("hoverSrc").textContent = MODULE_SOURCE[module] || "";
  placeHoverTip(el);
  tip.hidden = false;
  const s = hoverPoints(module);
  drawSparkCanvas($("hoverSpark"), s.points, s.key);
  if (!s.points.length) {
    $("hoverSrc").textContent = (MODULE_SOURCE[module] || "") + " · no sample in the last 60 s";
  }
  // the fresh 1 m read may land after the tooltip is already up
  ensureHoverSeries(module).then(() => {
    if (HOVER.module !== module || tip.hidden) return;
    const s2 = hoverPoints(module);
    drawSparkCanvas($("hoverSpark"), s2.points, s2.key);
  });
}

function hideHoverTipSoon() {
  clearTimeout(HOVER.timer);
  HOVER.hideAt = Date.now();
  HOVER.timer = setTimeout(() => {
    // the brief's 200 ms grace: long enough to move onto the tip, which is inert
    if (Date.now() - HOVER.hideAt < 195) return;
    const tip = $("hoverTip");
    if (tip) tip.hidden = true;
    HOVER.module = null;
  }, 200);
}

function wireHover() {
  BENTO_MODULES.forEach((m) => {
    const el = document.querySelector('#instrument [data-module="' + m + '"]');
    if (!el) return;
    el.addEventListener("mouseenter", () => showHoverTip(el, m));
    el.addEventListener("mouseleave", hideHoverTipSoon);
    el.addEventListener("focus", () => showHoverTip(el, m));
    el.addEventListener("blur", hideHoverTipSoon);
  });
  // A tile's height changes when the grid reflows (focus, unfocus) or when a
  // list inside it grows. Repaint that tile's sparkline on the resize itself,
  // so no tile ever shows a shape drawn at a height it no longer has.
  if (window.ResizeObserver) {
    const ro = new ResizeObserver((entries) => {
      entries.forEach((e) => {
        const cv = e.target;
        const id = cv && cv.id ? cv.id : "";
        if (id.indexOf("vspark-") !== 0) return;
        const m = id.slice("vspark-".length);
        const s = chipSpark(m);
        drawSparkCanvas(cv, last60(s.points), s.key);
      });
    });
    document.querySelectorAll("#tileGrid .tile-spark").forEach((cv) => ro.observe(cv));
  }
  window.addEventListener("resize", () => {
    const tip = $("hoverTip");
    if (tip) tip.hidden = true;
  });
}

// ---------------------------------------------------------- brush-to-zoom
// Drag horizontally on any chart: the swept span snaps to the nearest ladder
// step and becomes the range for the whole page. Double-click resets. This is
// the interaction the pointer expects from a timeline — the pill is the
// keyboard/mouse equivalent of the same state.
function wireBrush(canvasId) {
  const ch = CHARTS[canvasId];
  if (!ch || ch.__brush) return;
  const cv = ch.canvas;
  const box = cv.parentElement;
  const overlay = box ? box.querySelector(".brush") : null;
  ch.__brush = true;
  let startX = null;

  const xOf = (ev) => {
    const r = cv.getBoundingClientRect();
    return ev.clientX - r.left;
  };
  const scale = () => (ch.scales && ch.scales.x) || null;
  const paint = (a, b) => {
    if (!overlay) return;
    const w = Math.abs(b - a);
    overlay.hidden = w < 4;
    overlay.style.left = Math.min(a, b) + "px";
    overlay.style.width = w + "px";
  };

  cv.addEventListener("pointerdown", (ev) => {
    if (ev.button !== 0 || !scale()) return;
    startX = xOf(ev);
    paint(startX, startX);
    try { cv.setPointerCapture(ev.pointerId); } catch (e) { /* no capture */ }
  });
  cv.addEventListener("pointermove", (ev) => {
    if (startX === null) return;
    paint(startX, xOf(ev));
  });
  const finish = (ev) => {
    if (startX === null) return;
    const endX = xOf(ev);
    if (overlay) overlay.hidden = true;
    const a = Math.min(startX, endX);
    const b = Math.max(startX, endX);
    startX = null;
    if (b - a < 16) return;                       // a click, not a brush
    const s = scale();
    if (!s) return;
    const t1 = s.getValueForPixel(a);
    const t2 = s.getValueForPixel(b);
    if (!isNum(t1) || !isNum(t2)) return;
    const spanS = Math.abs(t2 - t1) / 1000;
    if (!(spanS > 0)) return;
    const snapped = snapRange(spanS);
    setRange(snapped, "brush");
    toast("brushed " + fmtSpan(spanS) + " → range " + snapped, "ok");
  };
  cv.addEventListener("pointerup", finish);
  cv.addEventListener("pointercancel", () => {
    startX = null;
    if (overlay) overlay.hidden = true;
  });
  cv.addEventListener("dblclick", (ev) => { ev.preventDefault(); resetRange(); });
}

// Nearest ladder step by ratio (log space), so 11 min snaps to 15m and 40 min
// snaps to 1h instead of jumping to the ends of the ladder.
function snapRange(spanS) {
  let best = RANGES[0];
  let bestScore = Infinity;
  RANGES.forEach((r) => {
    const score = Math.abs(Math.log(spanS / RANGE_S[r]));
    if (score < bestScore) { bestScore = score; best = r; }
  });
  return best;
}

// ========================================================= Analytics section
// The model page (brief §5): slicers → findings → measures → records → apps.
// ------------------------------------------- Power & battery (T17B · section C)
// Measured energy for the range, the pack's own numbers, and the only
// projection this product allows itself — always with its caveat attached.
// Cost is the one gated row: it stays hidden until a tariff is entered.
const TARIFF_KEY = "syswatch.tariff";

function tariff() {
  try {
    const j = JSON.parse(localStorage.getItem(TARIFF_KEY) || "null");
    if (j && isNum(j.rate) && j.rate > 0) return j;
  } catch (e) { /* unreadable tariff = no tariff */ }
  return null;
}

function saveTariff(rate, currency) {
  try {
    if (isNum(rate) && rate > 0) {
      localStorage.setItem(TARIFF_KEY, JSON.stringify({ rate: rate, currency: currency || "" }));
    } else {
      localStorage.removeItem(TARIFF_KEY);
    }
  } catch (e) { /* private mode: the tariff just does not persist */ }
  updatePowerBattery();
}

function fmtMoney(v, cur) {
  if (!isNum(v)) return "—";
  const s = v >= 1 ? v.toFixed(2) : v >= 0.01 ? v.toFixed(3) : v.toFixed(4);
  return (cur ? cur + " " : "") + s;
}

// One unit per column in the per-app legend: mWh, always (a 0.01 Wh row would
// otherwise switch unit mid-list and read as a different magnitude).
function fmtMWh(v) { return isNum(v) ? (v * 1000).toFixed(2) + " mWh" : "—"; }

async function loadBattery(force) {
  if (!force && state.battery && Date.now() - (state.batteryAt || 0) < 60000) {
    return state.battery;
  }
  const d = await fetchJSON("battery", "/api/battery");
  if (d) { state.battery = d; state.batteryAt = Date.now(); }
  return d;
}

function powerBatteryHtml() {
  return '<section class="sec pb" id="sec-powerbatt" data-sec="powerbatt">' +
    '<div class="sec-head"><h2>Power &amp; battery</h2>' +
    '<span class="micro">measured energy · pack state · what can honestly be projected</span></div>' +
    '<div class="cols">' +
    '<div class="panel"><div class="micro" id="pb-energy-micro">energy used · ' +
    esc(state.range) + "</div>" +
    '<div class="pb-hero" id="pb-energy"></div>' +
    '<div class="pb-bar" id="pb-bar" role="img" aria-label="top apps by measured energy"></div>' +
    '<div class="pb-legend" id="pb-legend"></div>' +
    '<div class="chart-foot"><span id="pb-energy-src"></span>' +
    '<span id="pb-energy-cov"></span></div>' +
    '<div class="pb-cost" id="pb-cost" hidden></div>' +
    "</div>" +
    '<div class="panel"><div class="micro">battery now</div>' +
    '<div id="pb-battery"><span class="na">reading the pack…</span></div></div>' +
    "</div>" +
    '<div class="cols">' +
    '<div class="panel"><div class="micro">module energy contribution · ' +
    esc(state.range) + "</div>" +
    '<table class="table pb-rails"><thead><tr><th>rail</th><th class="num">energy Wh</th>' +
    '<th>source</th></tr></thead><tbody id="pb-rails"></tbody></table>' +
    '<div class="reason" id="pb-rails-note"></div></div>' +
    '<div class="panel"><div class="micro">degradation outlook</div>' +
    '<div id="pb-degrade"><span class="na">reading our battery history…</span></div></div>' +
    "</div>" +
    "</section>";
}

function updatePowerBattery() {
  const ph = state.powerHistory || {};
  const an = ph.analysis || {};
  const energy = an.energy || {};
  const b = state.battery || {};
  const rows = ((an.per_app || {}).rows || []);

  // ---- energy used (headline names the rail it came from) ------------------
  const hero = $("pb-energy");
  if (hero) {
    let wh = null;
    let label = null;
    if (isNum(energy.package_wh)) { wh = energy.package_wh; label = "package (root)"; }
    else if (isNum(energy.attributed_wh)) { wh = energy.attributed_wh; label = "attributed apps (floor)"; }
    else if (isNum(energy.battery_wh)) { wh = energy.battery_wh; label = "battery draw"; }
    hero.innerHTML = wh == null
      ? '<span class="na">no energy integral in this window yet</span>'
      : '<span class="pb-num">' + wh.toFixed(4) + '</span><span class="pb-unit"> Wh</span>' +
        '<span class="pb-rail">' + esc(label) + " · " + esc(state.range) + "</span>";
  }
  const esrc = $("pb-energy-src");
  if (esrc) {
    esrc.textContent = "trapezoid over measured W samples · gaps > 120 s excluded · " +
      "package = root powermetrics, attributed = Σ rusage energy, battery = V × A integral";
  }
  const ecov = $("pb-energy-cov");
  if (ecov) {
    ecov.textContent = isNum(an.samples)
      ? an.samples + " samples · ring " + (ph.coverage_text || "")
      : "no power samples in this range yet";
  }

  // ---- per-app split: top 5 + other ---------------------------------------
  // The ranged per-app energy comes from the dashboard's OWN range store (the
  // same rows the Apps view shows, so both cuts agree); the collector's live
  // top-25 is the fallback when the store has not swept yet.
  const bar = $("pb-bar");
  const legend = $("pb-legend");
  const storeRows = ((state.apps || {}).rows || []).filter((r) => isNum(r.energy_wh));
  const splitRows = storeRows.length
    ? storeRows.slice().sort((a, b) => b.energy_wh - a.energy_wh)
      .map((r) => ({ name: r.name, wh: r.energy_wh }))
    : rows.map((r) => ({ name: r.name, wh: r.wh }));
  const splitBasis = storeRows.length
    ? ("top apps by measured rusage energy over " + state.range +
       " (dashboard range store — the same rows the Apps view shows) · a measured floor")
    : ((an.per_app || {}).basis || "no per-app energy measured in this window");
  if (legend) {
    if (!splitRows.length) {
      legend.innerHTML = '<span class="na">' + esc(splitBasis) + "</span>";
      if (bar) bar.innerHTML = "";
    } else {
      const top = splitRows.slice(0, 5);
      const restWh = splitRows.slice(5).reduce((a, r) => a + (isNum(r.wh) ? r.wh : 0), 0);
      const items = top.map((r) => ({ name: r.name, wh: r.wh || 0 }));
      if (restWh > 0) {
        items.push({ name: "other (" + (splitRows.length - 5) + " apps)", wh: restWh, other: true });
      }
      const sum = items.reduce((a, i) => a + i.wh, 0);
      if (bar && sum > 0) {
        bar.innerHTML = items.map((it, i) =>
          '<span class="pb-seg' + (it.other ? " pb-seg-other" : " pb-seg-" + i) +
          '" style="width:' + (100 * it.wh / sum).toFixed(2) + '%" title="' +
          esc(it.name + " · " + fmtWh(it.wh)) + '"></span>').join("");
      }
      legend.innerHTML = items.map((it, i) =>
        '<div class="pb-leg"><span class="pb-dot' + (it.other ? " pb-dot-other" : " pb-dot-" + i) +
        '"></span><span class="pb-leg-name">' + esc(it.name) + "</span>" +
        '<span class="pb-leg-num">' + esc(fmtMWh(it.wh)) + " · " +
        (sum > 0 ? (100 * it.wh / sum).toFixed(1) : "0.0") + " %</span></div>").join("") +
        '<div class="reason">' + esc(splitBasis) + "</div>";
    }
  }

  // ---- battery now ---------------------------------------------------------
  const battBox = $("pb-battery");
  if (battBox) {
    const cap = b.capacity || {};
    const charging = b.charging === true;
    const stateTxt = charging ? "charging"
      : b.external_connected === true ? "on AC, not charging"
        : b.external_connected === false ? "discharging" : "state unknown";
    battBox.innerHTML = statRows([
      ["draw now", isNum(b.draw_w) ? fmtW(b.draw_w) : na("not reported")],
      ["state", esc(stateTxt) + (isNum(b.adapter_w) ? " · adapter " + fmtW(b.adapter_w) : "")],
      ["time to empty", isNum(b.time_to_empty_s) ? (fmtClock(b.time_to_empty_s) || "—")
        : na(charging ? "charging — not applicable" : "Apple has not estimated one yet")],
      ["charge to full", isNum(b.charge_to_full_s) ? (fmtClock(b.charge_to_full_s) || "—")
        : na(!charging ? "on battery"
          : (isNum(b.pct) && b.pct >= 99.5) ? "full — nothing left to charge"
            : "not estimated yet")],
      ["energy at current draw",
        isNum(b.life_at_current_draw_h) ? b.life_at_current_draw_h.toFixed(2) + " h" : na("n/a")],
      ["health", isNum(b.health_pct) ? fmtPct(b.health_pct, 1) + " of design capacity"
        : na("not reported")],
      ["cycles", isNum(b.cycles) ? fmtInt(b.cycles) + " cycles" : na("not reported")],
      ["design capacity", isNum((cap || {}).design_mah) ? fmtInt(cap.design_mah) + " mAh" : na("not reported")],
      ["max capacity", isNum((cap || {}).max_capacity_mah)
        ? fmtInt(cap.max_capacity_mah) + " mAh" + (isNum(cap.max_capacity_pct)
          ? " · gauge " + cap.max_capacity_pct.toFixed(0) + " %" : "")
        : na((cap || {}).reason || "not reported")],
      ["pack energy", isNum(b.pack_wh) ? b.pack_wh.toFixed(2) + " Wh" : na("no pack reading")],
      ["condition", b.condition ? esc(b.condition) : na("not reported")],
    ]) + '<div class="reason">' + esc(b.life_note || "") +
      " · capacities cached 24 h (ioreg): " + esc((cap || {}).source || "ioreg") + "</div>";
  }

  // ---- module energy contribution -----------------------------------------
  const rails = $("pb-rails");
  if (rails) {
    const defs = [
      ["package (CPU+GPU+ANE)", energy.package_wh, "root powermetrics sample"],
      ["CPU", energy.cpu_wh, "root powermetrics CPU Power line"],
      ["GPU", energy.gpu_wh, "root powermetrics GPU Power line"],
      ["ANE", energy.ane_wh, "root powermetrics ANE Power line"],
      ["attributed apps", energy.attributed_wh, "Σ per-PID rusage energy — a floor"],
      ["battery draw", energy.battery_wh, "ioreg V × A integral"],
      ["adapter input", energy.adapter_wh, "ioreg AppleRawAdapterDetails"],
    ];
    rails.innerHTML = defs.map(([label, wh, src]) => {
      const v = isNum(wh) ? wh.toFixed(4) + " Wh" : '<span class="na">not measured</span>';
      return "<tr><td>" + esc(label) + "</td><td class='num'>" + v + "</td>" +
        "<td class='note'>" + esc(src || "root daemon sample") + "</td></tr>";
    }).join("");
  }
  const railsNote = $("pb-rails-note");
  if (railsNote) {
    const excluded = an.excluded || {};
    const missing = Object.keys(excluded).filter((k) => excluded[k]);
    railsNote.textContent = missing.length
      ? "not measured here: " + missing.map((k) =>
        k + " (" + excluded[k] + ")").join(" · ")
      : "every rail above was measured in this window";
  }

  // ---- cost (tariff-gated) -------------------------------------------------
  // Priced on the rail the meter would actually see: adapter input from the
  // wall when connected, else package, else the attributed floor, else the
  // pack integral. The label always names which one was priced.
  const costBox = $("pb-cost");
  if (costBox) {
    const t = tariff();
    const priced = isNum(energy.adapter_wh) ? { wh: energy.adapter_wh, rail: "adapter input" }
      : isNum(energy.package_wh) ? { wh: energy.package_wh, rail: "package" }
        : isNum(energy.attributed_wh) ? { wh: energy.attributed_wh, rail: "attributed apps (floor)" }
          : isNum(energy.battery_wh) ? { wh: energy.battery_wh, rail: "battery draw" } : null;
    if (!t) {
      costBox.hidden = true;
      costBox.innerHTML = "";
    } else {
      costBox.hidden = false;
      costBox.innerHTML = priced
        ? '<span class="label">cost · ' + esc(state.range) + "</span>" +
          '<span class="val">' + esc(fmtMoney((priced.wh / 1000) * t.rate, t.currency)) +
          "</span><span class=\"note\">priced on " + esc(priced.rail) + " (" +
          priced.wh.toFixed(4) + " Wh) at " + t.rate + " per kWh · the tariff is yours, " +
          "entered in ⚙ settings</span>"
        : '<span class="label">cost</span><span class="val na">no measured energy to price</span>';
    }
  }

  // ---- degradation outlook -------------------------------------------------
  const degBox = $("pb-degrade");
  if (degBox) {
    const d = b.degradation || {};
    if (!b.ts) {
      degBox.innerHTML = '<span class="na">the battery endpoint has not answered yet</span>';
    } else if (d.available && isNum(d.days_to_80)) {
      degBox.innerHTML = statRows([
        ["health now", isNum(d.health_pct) ? fmtPct(d.health_pct, 1) : "—"],
        ["observed cycle rate", isNum(d.cycles_per_day) ? d.cycles_per_day.toFixed(3) + " cycles/day" : "—"],
        ["observed health decline", isNum(d.health_pct_per_month)
          ? d.health_pct_per_month.toFixed(3) + " %/month" : "—"],
        ["projected to 80 %", isNum(d.days_to_80) ? d.days_to_80.toFixed(0) + " days" : "—"],
        ["that date", d.projected_80_date || "—"],
        ["baseline", (isNum(d.span_days) ? d.span_days.toFixed(1) : "?") +
          " days · " + (isNum(d.rows) ? d.rows : 0) + " samples of our own"],
      ]) + '<div class="reason">' + esc(d.caveat || "") + "</div>";
    } else {
      degBox.innerHTML = statRows([
        ["health now", isNum(b.health_pct) ? fmtPct(b.health_pct, 1) : na("not reported")],
        ["cycles now", isNum(b.cycles) ? fmtInt(b.cycles) : na("not reported")],
        ["recorded history", (isNum(d.rows) ? d.rows : 0) + " " +
          ((isNum(d.rows) && d.rows === 1) ? "sample" : "samples") + " · " +
          (isNum(d.span_days) ? d.span_days.toFixed(1) + " days" : "0 days")],
        ["needs", isNum(d.min_days) ? d.min_days.toFixed(0) + " days" : "14 days"],
        ["projection", '<span class="na">insufficient data</span>'],
      ]) + '<div class="reason">' + esc(d.reason || "insufficient data") +
        " · keeping one row per 30 min in " + esc(d.path || "battery-history.jsonl") +
        " · no projection is invented from a short baseline</div>";
    }
  }
}

// ========================================================= Analytics section
// The model page (brief §5): slicers → findings → measures → records → apps.
function renderAnalyticsSection() {
  $("view").innerHTML =
    plainHead("Analytics", "the model behind the instrument") +
    '<div class="panel slicers"><div class="micro">slicers · the time range is the sticky pill</div>' +
    '<div class="slicer-row">' +
    '<label class="slicer">group <select id="an-group">' +
    '<option value="all">all groups</option><option value="cpu">cpu</option>' +
    '<option value="memory">memory</option><option value="network">network</option>' +
    '<option value="power">power / energy</option></select></label>' +
    '<label class="slicer">app <select id="an-app"><option value="all">all apps</option></select></label>' +
    "</div></div>" +
    '<div class="panel"><div class="micro">findings · computed from the series</div>' +
    '<div class="findings" id="an-findings"><p class="na">computing from the measured series…</p></div></div>' +
    '<div class="panel"><div class="micro">the window in one chart · CPU %</div>' +
    '<div class="chart-box tall"><canvas id="an-chart"></canvas>' +
    '<div class="brush" id="an-brush" hidden></div></div>' +
    '<div class="chart-foot"><span id="an-src"></span><span id="an-cov"></span></div></div>' +
    '<div class="panel"><div class="micro">measures · ' + esc(state.range) +
    ' · Δ vs previous window</div>' +
    '<table class="table measures"><thead><tr><th>Metric</th><th class="num">avg</th>' +
    '<th class="num">max</th><th class="num">p95</th><th class="num">Δ prev</th>' +
    '<th class="num">coverage</th></tr></thead><tbody id="an-measures"></tbody></table>' +
    '<div class="reason" id="an-measures-basis"></div></div>' +
    powerBatteryHtml() +
    '<div class="cols">' +
    '<div class="panel"><div class="micro">records</div><div id="an-records"></div></div>' +
    '<div class="panel"><div class="micro">averages &amp; peaks per window</div><div id="an-ranges"></div></div>' +
    "</div>" +
    '<div class="cols">' +
    '<div class="panel"><div class="micro">top apps by CPU now · click for detail</div>' +
    '<table class="table"><thead><tr><th>App</th><th class="num">CPU</th>' +
    '<th class="num">Memory</th></tr></thead><tbody id="an-apps"></tbody></table></div>' +
    '<div class="panel"><div class="micro">network this window</div><div id="an-net"></div></div>' +
    "</div>";
  const groupSel = $("an-group");
  if (groupSel) {
    groupSel.value = state.anGroup || "all";
    groupSel.addEventListener("change", () => {
      state.anGroup = groupSel.value;
      updateAnalyticsSection();
    });
  }
  const appSel = $("an-app");
  if (appSel) {
    appSel.value = state.anApp || "all";
    appSel.addEventListener("change", () => {
      state.anApp = appSel.value;
      updateAnalyticsSection();
    });
  }
}

const MEASURE_GROUPS = {
  cpu: ["cpu"],
  memory: ["memory"],
  network: ["net_down", "net_up"],
  power: ["attributed_w", "package_w"],
};

function measureCell(m) {
  if (!m || !m.available) return na(m && m.reason ? m.reason : "unavailable");
  const unit = m.unit || "";
  const fmt = (v) => (isNum(v) ? v.toFixed(unit === "n" ? 0 : 2) + (unit === "n" ? "" : " " + unit) : "—");
  return fmt(m.avg);
}

function updateAnalyticsSection() {
  const d = state.analytics;
  const g = state.graphs.cpu;
  if (!d) return;

  // 0) app slicer options come from the measured app rows
  const appSel = $("an-app");
  if (appSel) {
    const names = [];
    (((d.apps || {}).by_energy) || []).forEach((a) => { if (a.name) names.push(a.name); });
    (((d.apps || {}).by_cpu) || []).forEach((a) => { if (a.name) names.push(a.name); });
    const uniq = [...new Set(names)].sort((a, b) => a.localeCompare(b));
    const wanted = state.anApp || "all";
    appSel.innerHTML = '<option value="all">all apps</option>' +
      uniq.map((n) => '<option value="' + esc(n) + '">' + esc(n) + "</option>").join("");
    appSel.value = uniq.includes(wanted) ? wanted : "all";
  }

  // 1) findings — the server computes them from the measured series
  const findings = $("an-findings");
  if (findings) {
    const lines = Array.isArray(d.findings) ? d.findings.slice() : [];
    if (state.anApp && state.anApp !== "all") {
      const rows = ((((d.measures || []).find((m) => m.id === "attributed_w")) || {}));
      const per = state.powerHistory && state.powerHistory.analysis;
      const appRow = (((per || {}).per_app || {}).rows || []).find((r) => r.name === state.anApp);
      if (appRow) {
        lines.push("Filtered to " + state.anApp + ": " + appRow.wh.toFixed(4) +
          " Wh measured over " + state.range + " (" +
          (isNum(appRow.share_pct) ? appRow.share_pct.toFixed(1) : "—") + " % of measured app energy).");
      } else if (rows) {
        lines.push("Filtered to " + state.anApp + " — no measured energy row for it in this window.");
      }
    }
    findings.innerHTML = lines.length
      ? lines.map((l) => "<p>" + esc(l) + "</p>").join("")
      : '<p class="na">not enough measured samples yet for a finding.</p>';
  }

  // 2) the window's CPU series
  if (g) {
    paintSeries("an-chart", "cpu", g.points || [], g.ticks || []);
    wireBrush("an-chart");
    const src = $("an-src");
    if (src) src.textContent = g.source || "";
    const cov = $("an-cov");
    if (cov) {
      const m = g.measures || {};
      cov.textContent = isNum(m.coverage_pct)
        ? "sampled " + m.coverage_pct.toFixed(0) + " % of " + state.range : "";
    }
  }

  // 3) the measures table — server computed, filtered by the group slicer
  const mt = $("an-measures");
  if (mt) {
    const group = state.anGroup || "all";
    const wanted = group === "all" ? null : (MEASURE_GROUPS[group] || [group]);
    const rows = (d.measures || []).filter((m) => !wanted || wanted.includes(m.id));
    mt.innerHTML = rows.map((m) => {
      if (!m.available) {
        return "<tr><td>" + esc(m.label) + "</td><td colspan='5' class='na'>" +
          esc(m.reason || "unavailable") + "</td></tr>";
      }
      const unit = m.unit || "";
      const f = (v) => (isNum(v) ? v.toFixed(unit === "n" ? 0 : 2) + (unit === "n" ? "" : " " + unit) : "—");
      const deltaTxt = isNum(m.delta)
        ? (m.delta >= 0 ? "+" : "") + f(m.delta) +
          (isNum(m.delta_pct) ? " (" + (m.delta_pct >= 0 ? "+" : "") + m.delta_pct.toFixed(1) + " %)" : "")
        : na(m.baseline || "no prev window");
      return "<tr><td>" + esc(m.label) + "</td>" +
        "<td class='num'>" + f(m.avg) + "</td>" +
        "<td class='num'>" + f(m.max) + (isNum(m.max_ts) ? " <span class='note'>" + fmtTime(m.max_ts) + "</span>" : "") + "</td>" +
        "<td class='num'>" + f(m.p95) + "</td>" +
        "<td class='num'>" + deltaTxt + "</td>" +
        "<td class='num'>" + (isNum(m.coverage_pct) ? m.coverage_pct.toFixed(0) + " %" : "—") + "</td></tr>";
    }).join("") || '<tr><td colspan="6" class="na">no measures for this slicer</td></tr>';
  }
  const mbasis = $("an-measures-basis");
  if (mbasis) {
    mbasis.textContent = "avg / max / p95 from " +
      ((d.stats || {}).resolution_s ? d.stats.resolution_s + " s buckets of the history index" : "the history index") +
      " · Δ compares to the previous equal-length window · coverage = share of the window with samples · " +
      "a measure with no series says why instead of showing 0.";
  }

  // 4) records feed (server-computed peaks with their timestamps)
  const rec = $("an-records");
  if (rec) {
    const events = (d.records || []).map((r) => {
      const when = isNum(r.ts) ? fmtTime(r.ts) + " · " : "";
      return '<div class="stat-row"><span class="label">' + esc(when + r.text) + "</span>" +
        '<span class="val">' + esc(r.metric || "") + "</span></div>";
    });
    rec.innerHTML = events.length ? events.join("")
      : statRows([["no events in this window", ""]]);
  }

  // 5) averages + peaks per window (all server-side)
  const ranges = $("an-ranges");
  if (ranges) {
    const rows = Object.keys(d.ranges || {}).map((k) => {
      const r = d.ranges[k];
      const c = r.cpu || {}, m = r.memory || {};
      const filling = r.complete === false && isNum(r.coverage_pct)
        ? ' <span class="reason">filling ' + r.coverage_pct.toFixed(0) + " %</span>" : "";
      return [k, "cpu " + (isNum(c.mean_pct) ? fmtPct(c.mean_pct, 1) : "—") + " avg / " +
        (isNum(c.peak_pct) ? fmtPct(c.peak_pct, 1) : "—") + " peak · mem " +
        (isNum(m.mean_pct) ? fmtPct(m.mean_pct, 1) : "—") + filling];
    });
    ranges.innerHTML = statRows(rows);
  }

  // 6) top apps now — clicking cross-filters and opens the detail drawer
  const apps = $("an-apps");
  if (apps) {
    const rows = (d.apps || {}).by_cpu || [];
    apps.innerHTML = rows.length
      ? rows.slice(0, 10).map((p) => "<tr class='row-click' data-app='" + esc(p.name || "?") +
        "' tabindex='0'><td class='name'>" + esc(p.name || "?") +
        "</td><td class='num'>" + (isNum(p.cpu_pct) ? fmtPct(p.cpu_pct, 1) : na("warming")) +
        "</td><td class='num'>" + fmtMB(p.mem_mb) + "</td></tr>").join("")
      : '<tr><td colspan="3" class="na">no live process scan</td></tr>';
    apps.querySelectorAll("[data-app]").forEach((tr) => {
      const go = () => {
        setFilter({ app: tr.dataset.app });
        openDrawerForApp(tr.dataset.app);
      };
      tr.addEventListener("click", go);
      tr.addEventListener("keydown", (ev) => {
        if (ev.key === "Enter" || ev.key === " ") { ev.preventDefault(); go(); }
      });
    });
  }

  // 7) network totals
  const net = $("an-net");
  if (net) {
    const nt = d.network_today || {};
    net.innerHTML = statRows([
      ["in", isNum(nt.rx_gb) ? nt.rx_gb.toFixed(3) + " GB" : na()],
      ["out", isNum(nt.tx_gb) ? nt.tx_gb.toFixed(3) + " GB" : na()],
      ["measured", fmtSpan(nt.covered_s)],
      ["gaps skipped", isNum(nt.gaps_skipped) ? fmtInt(nt.gaps_skipped) : "—"],
    ]);
  }

  // 8) Power & battery — energy Wh, per-app split, pack state, outlook, cost
  updatePowerBattery();
}

// ============================================================ Knowledge page
// The manual. One card per metric: name · what it measures · formula (when it
// is derived) · the read that produced it · cadence + retention tier · what a
// gap means · and a link into that module's L2 drawer. Nothing here is a live
// number: it is the contract behind the numbers on the other three views, so it
// is static and costs no request.
const KNOWLEDGE = [
  {
    id: "cpu", title: "CPU", note: "utilisation, clusters, load",
    cards: [
      {
        name: "CPU utilisation",
        what: "Share of processor time that was busy. The machine figure is 0–100 % " +
          "across every core; a per-process figure is 0–100 % of ONE core, so a " +
          "process can read above 100 % and a 10-core machine can total 1000 %.",
        formula: "Δ CPU time ÷ Δ wall time × 100, per core, then the mean of the " +
          "cores in question",
        source: "host_processor_info, read through psutil.cpu_percent(percpu=True)",
        cadence: "1 s sampler → /api/current · the range index buckets the same " +
          "ticks to 10 s · daily JSONL keeps every 1 s row, gzipped after 7 days",
        gap: "null = that tick never ran (sleep, restart, first baseline). It is " +
          "never drawn as 0 %.",
        module: "cpu",
      },
      {
        name: "P/E cluster split",
        what: "Efficiency and performance clusters as separate rows and rings, so " +
          "a busy E cluster and a busy P cluster are never averaged into one number.",
        formula: "mean of per_core_pct[base .. base + cores − 1] for that cluster",
        source: "hw.perflevel1.logicalcpu (efficiency) and hw.perflevel0.logicalcpu " +
          "(performance) via sysctl, plus the 1 s per-core array",
        cadence: "same 1 s tick as CPU utilisation · topology is read once at start",
        gap: "a cluster with no sampled slice reports null and the reason, never 0 %.",
        module: "cpu",
      },
      {
        name: "P/E cluster mapping rule",
        what: "Efficiency cores are enumerated FIRST on Apple Silicon: logical CPUs " +
          "0 … E−1 are efficiency and the performance cores follow. Every label in " +
          "this product follows that index order — none of them assumes “the first " +
          "N are performance”.",
        formula: "E = hw.perflevel1.logicalcpu · P = hw.perflevel0.logicalcpu · " +
          "E cores occupy indices 0 … E−1",
        source: "sysctl, then verified with a single-thread load: the spike lands " +
          "at indices 4–6 on this 8-core M2, i.e. inside the performance cluster",
        cadence: "static per boot",
        gap: "if the sysctl keys are missing the clusters are not invented — the " +
          "P/E panel says so instead of guessing an order.",
        module: "cpu",
      },
      {
        name: "Load average",
        what: "Runnable-queue length over 1, 5 and 15 minutes, divided by core count " +
          "for the pressure read.",
        formula: "psutil.getloadavg()",
        source: "getloadavg(3)",
        cadence: "1 s sampler (the kernel refreshes its own averages)",
        gap: "null on platforms without getloadavg; the row says unavailable.",
        module: "cpu",
      },
    ],
  },
  {
    id: "memory", title: "Memory", note: "used %, resident set, swap",
    cards: [
      {
        name: "Memory used",
        what: "Physical memory the kernel reports as in use, as a share of installed RAM.",
        formula: "used ÷ total × 100 (swap tracked separately, never added in)",
        source: "vm_statistics64 through psutil.virtual_memory(); swap from " +
          "sysctl vm.swapusage",
        cadence: "1 s sampler · same 10 s range index · 1 s rows in the daily JSONL",
        gap: "null = no sample in that tick; the chart breaks the line rather than " +
          "dropping to 0.",
        module: "memory",
      },
      {
        name: "Resident size per app",
        what: "The largest resident set the app held at any sweep inside the window " +
          "(peak), plus the mean of the same sweeps.",
        formula: "max(rss) and mean(rss) over the window's sweeps, bytes ÷ 1048576 → MB",
        source: "psutil.Process.memory_info().rss for every pid, grouped by app name",
        cadence: "20 s per-process sweep · 3 h ring, kept in memory only",
        gap: "an app absent from a sweep has no point there — it is not a 0 MB reading.",
        module: "memory",
      },
    ],
  },
  {
    id: "gpu", title: "GPU / ANE", note: "utilisation, memory, neural engine",
    cards: [
      {
        name: "GPU utilisation",
        what: "How busy the GPU's own scheduler was during the sample.",
        formula: "as reported, not derived",
        source: "ioreg IOAccelerator PerformanceStatistics “Device Utilization %” " +
          "(unprivileged), with Renderer/Tiler and in-use system memory alongside",
        cadence: "5 s IO ring · tiers: 5 s buckets for 30 min, 60 s for 6 h, " +
          "600 s for 7 d, 3600 s for 30 d",
        gap: "null = the ioreg read failed or the key is absent on this GPU; never 0 %.",
        module: "gpu",
      },
      {
        name: "GPU clock",
        what: "Not measured. Apple Silicon's PerformanceStatistics does not carry a " +
          "GPU frequency, and this SDK exposes none unprivileged.",
        formula: "—",
        source: "checked in IOAccelerator; the field is absent",
        cadence: "n/a",
        gap: "reported as null with the reason instead of a guessed clock. The root " +
          "powermetrics sample may carry a GPU clock; it is labelled root when it does.",
        module: "gpu",
      },
      {
        name: "ANE utilisation and power",
        what: "Neural-engine activity and its share of package power.",
        formula: "as reported by powermetrics ANEMetrics",
        source: "root daemon sample.json (powermetrics), read-only by this dashboard",
        cadence: "5 s daemon sample · a sample older than 30 s is withheld, not shown stale",
        gap: "null without the privileged daemon — the tile says “needs root”.",
        module: "gpu",
      },
      {
        name: "GPU time per process",
        what: "Not measurable on this SDK: rusage_info_v6 has no ri_gpu_time field.",
        formula: "—",
        source: "sys/resource.h, struct rusage_info_v6 (GPU time is absent)",
        cadence: "n/a",
        gap: "per-process GPU share is null everywhere, never 0 %. Apps rank GPU work " +
          "by measured rusage energy as the closest available proxy, and say so.",
        module: "gpu",
      },
    ],
  },
  {
    id: "battery", title: "Battery", note: "charge, draw, health, outlook",
    cards: [
      {
        name: "Charge %",
        what: "State of charge of the pack.",
        formula: "CurrentCapacity ÷ MaxCapacity × 100",
        source: "ioreg AppleSmartBattery (unprivileged), psutil.sensors_battery as fallback",
        cadence: "read at most once a second (1 s cache) · 5 s power ring · ring tiers " +
          "5 s → 15 min, 60 s → 6 h, 600 s → 7 d",
        gap: "null = no battery on this machine or the read failed; never 0 %.",
        module: "battery",
      },
      {
        name: "Draw (W)",
        what: "Power the pack is delivering (or absorbing) right now.",
        formula: "Voltage (mV) × Amperage (mA) ÷ 1e6, sign taken as magnitude",
        source: "ioreg AppleSmartBattery Voltage / Amperage",
        cadence: "1 s (cached), 5 s power ring",
        gap: "null when the pack exposes no voltage/current pair.",
        module: "battery",
      },
      {
        name: "Battery health %",
        what: "The pack's present full-charge capacity against the capacity it was " +
          "designed with.",
        formula: "NominalChargeCapacity ÷ DesignCapacity × 100",
        source: "ioreg AppleSmartBattery; both capacities are static facts, so they " +
          "are read once per 24 h and cached",
        cadence: "24 h cache · health % refreshed with it",
        gap: "null when the pack reports only one of the two capacities — no ratio is " +
          "invented.",
        module: "battery",
      },
      {
        name: "Time to empty / to full",
        what: "Apple's own estimate for the current load while discharging, or for a " +
          "full charge while charging.",
        formula: "as reported",
        source: "ioreg AvgTimeToEmpty / AvgTimeToFull (65535 means “calculating”)",
        cadence: "1 s (cached), 5 s ring",
        gap: "null while the value is 65535 or the direction is unknown.",
        module: "battery",
      },
      {
        name: "Degradation outlook",
        what: "Observed cycle rate and health decline from OUR OWN recorded battery " +
          "history, projected linearly to 80 % health.",
        formula: "cycles/day = ΔCycleCount ÷ Δdays · health/day = Δhealth ÷ Δdays · " +
          "days to 80 % = (health − 80) ÷ (health/day)",
        source: "dashboard/data/battery-history.jsonl — one row per 30 min (the " +
          "collector log and the 1 s history carry no cycle count)",
        cadence: "one row per 30 min · needs at least 14 days before it will project",
        gap: "under 14 days it says “insufficient data”; with no measurable decline it " +
          "says that. The projection always carries: estimate; assumes current usage " +
          "pattern, not a prediction.",
        module: "battery",
      },
    ],
  },
  {
    id: "power", title: "Package power", note: "rails, attributed watts",
    cards: [
      {
        name: "Package power",
        what: "Total power the SoC package is drawing: CPU + GPU + ANE.",
        formula: "CPU + GPU + ANE, each taken from powermetrics' own mW line ÷ 1000",
        source: "root daemon sample.json written by powermetrics " +
          "(com.barbarian.syswatch.power) — the dashboard only reads the file",
        cadence: "5 s daemon sample · a sample older than 30 s is withheld rather than " +
          "shown as current",
        gap: "null without the daemon. The chip says “needs root”, never 0 W.",
        module: "power",
      },
      {
        name: "Attributed W (per app)",
        what: "The sum of the per-process energy the kernel charged to apps. A " +
          "measured FLOOR of consumption, not the machine total.",
        formula: "Σ over processes of (ri_energy_nj delta ÷ 1e9 ÷ sweep seconds)",
        source: "proc_pid_rusage(RUSAGE_INFO_V6) via libproc — ri_energy_nj",
        cadence: "20 s sweep · 3 h ring in memory. Each rusage call costs ~25 ms per " +
          "400 PIDs, which is why it never runs on the 1 s tick path",
        gap: "an app with no rusage delta has no watt figure; the row says n/a rather " +
          "than 0 W.",
        module: "power",
      },
      {
        name: "Energy per app (Wh)",
        what: "What an app's attributed watts add up to over the window.",
        formula: "Σ (W × Δt) ÷ 3600, rectangular rule between consecutive sweeps, " +
          "gaps > 30 s excluded",
        source: "the same per-process rusage energy, grouped by app name",
        cadence: "20 s sweeps, aggregated per range read",
        gap: "apps that were not in the window's sweeps are absent from the table — " +
          "not listed at 0 Wh.",
        module: "power",
      },
    ],
  },
  {
    id: "thermal", title: "Thermal", note: "die temperatures, pressure, fans",
    cards: [
      {
        name: "Die temperature",
        what: "CPU and GPU die temperatures straight from powermetrics' thermal " +
          "sampler.",
        formula: "as reported (per-cluster rows included when the daemon emits them)",
        source: "root daemon sample.json (powermetrics -s thermal); SMC rows when the " +
          "daemon can read them",
        cadence: "5 s daemon sample · withheld when older than 30 s",
        gap: "no stored thermal series exists, so this module shows a live reading and " +
          "an honest gap — never a flat line at 0 °C.",
        module: "thermal",
      },
      {
        name: "Thermal pressure",
        what: "The system's own thermal state: nominal, fair, serious or critical.",
        formula: "as reported",
        source: "powermetrics thermal pressure (root daemon)",
        cadence: "5 s",
        gap: "null without root; the row says so.",
        module: "thermal",
      },
      {
        name: "Battery temperature",
        what: "Pack temperature, not die temperature.",
        formula: "ioreg Temperature ÷ 100 (°C)",
        source: "ioreg AppleSmartBattery (unprivileged)",
        cadence: "1 s (cached)",
        gap: "absent on packs that do not expose the key — reported as absent, never " +
          "0 °C.",
        module: "thermal",
      },
    ],
  },
  {
    id: "network", title: "Network", note: "throughput, per-interface, per-app",
    cards: [
      {
        name: "Throughput (down / up)",
        what: "Bytes per second moved across every interface, summed.",
        formula: "Δ bytes ÷ Δ wall time, per interface, then summed",
        source: "psutil.net_io_counters(pernic=True) → getifaddrs/if_msghdr counters",
        cadence: "1 s sampler · 10 s range index · 1 s daily JSONL",
        gap: "null = no delta yet (first sample after start) — not 0 B/s.",
        module: "network",
      },
      {
        name: "Per-app network",
        what: "Bytes attributed to each process.",
        formula: "nettop bytes_in / bytes_out deltas ÷ the span they cover",
        source: "nettop, sampled by this dashboard (the per-PID counters need it)",
        cadence: "nettop spawn every 30 s · 20 s per-process sweeps · 3 h ring",
        gap: "a missing nettop sample leaves the app with no figure for that sweep; a " +
          "gap means “not measured”, never “no traffic”.",
        module: "network",
      },
    ],
  },
  {
    id: "disk", title: "Disk", note: "throughput, capacity, per-app io",
    cards: [
      {
        name: "Disk throughput",
        what: "Read and write bytes per second across the internal storage driver.",
        formula: "Δ IOBlockStorageDriver Bytes (Read/Write) ÷ Δ wall time",
        source: "ioreg IOBlockStorageDriver statistics (unprivileged)",
        cadence: "5 s IO ring, refresh-throttled · tiers 5 s → 30 min, 60 s → 6 h, " +
          "600 s → 7 d, 3600 s → 30 d",
        gap: "null on the first probe after start (no baseline); never 0 B/s.",
        module: "disk",
      },
      {
        name: "Capacity",
        what: "Used and free space per mounted volume.",
        formula: "statvfs free ÷ total × 100",
        source: "psutil.disk_usage per mount",
        cadence: "1 s sampler",
        gap: "a volume that cannot be stat'ed is listed without numbers.",
        module: "disk",
      },
      {
        name: "Per-app disk",
        what: "Bytes an app read and wrote through its own file descriptors.",
        formula: "ri_diskio_bytesread / ri_diskio_byteswritten deltas ÷ span, and the " +
          "window total in MB",
        source: "proc_pid_rusage(RUSAGE_INFO_V6)",
        cadence: "20 s sweeps · 3 h ring",
        gap: "no delta (first sighting, EPERM, counter reset) = n/a, never 0 MB.",
        module: "disk",
      },
    ],
  },
  {
    id: "consumers", title: "Consumers & Apps", note: "the two cuts of one store",
    cards: [
      {
        name: "Consumers (module cut)",
        what: "Who consumed a module's resource over the window — ranked per process, " +
          "or per app name.",
        formula: "mean and peak of each field across the window's sweeps; energy = " +
          "Σ ri_energy_nj ÷ 3.6e12 Wh",
        source: "the per-process range store: psutil CPU times, rusage V6 energy and " +
          "disk bytes, nettop bytes",
        cadence: "20 s sweeps · 3 h in memory · the collector separately records its " +
          "top-25 by energy every ~6 s",
        gap: "the coverage line says what share of the window the store held; a process " +
          "the store never saw is absent, not zero.",
        module: "cpu",
      },
      {
        name: "Apps (application cut)",
        what: "One row per application for the same range: avg and peak CPU, memory, " +
          "energy, disk, network, and its share of measured app energy.",
        formula: "every pid carrying the app's name is summed per sweep — the same " +
          "aggregation the by-name consumer list uses",
        source: "the SAME range store as Consumers, plus a bucketed energy series for " +
          "the row sparkline",
        cadence: "read once per range change (not polled) · 20 s sweeps behind it",
        gap: "“no sweep in this range yet — the store sweeps every 20 s” is the empty " +
          "state; a filtered-out row is not a missing measurement.",
        module: "cpu",
      },
      {
        name: "Share of total",
        what: "Each row's slice of the measured total for that column. There is no " +
          "“gap to leader” anywhere in the product.",
        formula: "row ÷ Σ rows × 100",
        source: "computed from the same measured rows on the server",
        cadence: "per range read",
        gap: "null when the total is 0 or unmeasured; the cell prints n/a.",
        module: "power",
      },
    ],
  },
  {
    id: "coverage", title: "Coverage & the x-axis", note: "how much of the window is real",
    cards: [
      {
        name: "Coverage",
        what: "The share of the selected window that actually has samples. A chart " +
          "bounded by samples is not the same statement as a fully measured window.",
        formula: "(newest sample − oldest sample) ÷ window length × 100, capped at 100 %",
        source: "whichever store backs the panel: 1 s history index, 5 s power ring, " +
          "20 s per-process ring",
        cadence: "recomputed on every range read",
        gap: "a window with no samples prints “waiting for the range window”, which is " +
          "not the same as “measured 0 %”.",
        module: "cpu",
      },
      {
        name: "The x-axis ladder",
        what: "Tick spacing per range. Labels are LOCAL time; the underlying epochs are " +
          "unchanged, so a tooltip and an axis label always agree.",
        formula: "1m → 15 s · 5m → 60 s · 15m → 3 min · 1h → 10 min · 6h → 1 h · " +
          "1d → 3 h · 7d → 1 day · 30d → 5 days · 1y → 1 month",
        source: "server-side tick ladder (calendar-aligned on day and month boundaries)",
        cadence: "5–10 labels per axis, by construction",
        gap: "a range with no samples keeps its ticks and draws nothing — the axis never " +
          "lies about the window.",
        module: "cpu",
      },
      {
        name: "Brush to zoom",
        what: "Dragging on any chart sets the global range to the nearest ladder step; " +
          "double-click resets. There is one range control, and the brush is its " +
          "pointer equivalent.",
        formula: "—",
        source: "Chart.js scale pixel → epoch, snapped in log space to the ladder",
        cadence: "on release",
        gap: "a drag under 16 px is treated as a click, not a range change.",
        module: "cpu",
      },
    ],
  },
];

function knowCardHtml(c) {
  const rows = [];
  if (c.formula) rows.push(["formula", c.formula]);
  rows.push(["source", c.source]);
  rows.push(["cadence · retention", c.cadence]);
  rows.push(["what a gap means", c.gap]);
  return '<article class="know-card">' +
    '<div class="know-head"><h3>' + esc(c.name) + "</h3>" +
    (c.module ? '<button type="button" class="know-open" data-module="' +
      esc(c.module) + '">open module →</button>' : "") + "</div>" +
    '<p class="know-what">' + esc(c.what) + "</p>" +
    '<div class="know-rows">' + rows.map((r) =>
      '<div class="know-row"><span class="know-k">' + esc(r[0]) + "</span>" +
      '<span class="know-v">' + esc(r[1]) + "</span></div>").join("") + "</div>" +
    "</article>";
}

function renderKnowledgeSection() {
  const nav = KNOWLEDGE.map((s) =>
    '<a class="know-nav" href="#knowledge" data-know="' + esc(s.id) + '">' +
    esc(s.title) + "</a>").join("");
  $("view").innerHTML =
    plainHead("Knowledge", "what every number means, where it comes from, and what a gap is") +
    '<div class="panel know-intro"><div class="micro">how to read this page</div>' +
    '<p>Every card below is the contract behind a number on the other views: what it ' +
    "measures, the formula if it is derived, the read that produced it, its cadence and " +
    "retention tier, and what a gap in it means. A gap always means “not measured” — " +
    "never zero. Each card links into that module's drawer.</p>" +
    '<div class="know-nav-row">' + nav + "</div></div>" +
    KNOWLEDGE.map((s) =>
      '<section class="know-sec" id="know-' + esc(s.id) + '">' +
      '<div class="sec-head"><h2>' + esc(s.title) + "</h2>" +
      '<span class="micro">' + esc(s.note) + "</span></div>" +
      '<div class="know-grid">' + s.cards.map(knowCardHtml).join("") + "</div>" +
      "</section>").join("");
  $("view").querySelectorAll("[data-know]").forEach((a) => {
    a.addEventListener("click", (ev) => {
      ev.preventDefault();
      const el = $("know-" + a.dataset.know);
      if (el) el.scrollIntoView({ block: "start", behavior: "smooth" });
    });
  });
}

function updateKnowledgeSection() {
  // static manual: no numbers to refresh, no request to make
}

// T20: the same manual, rendered inside ⚙ settings. Collapsed by default (it is
// reference material) and grouped exactly as the old page grouped it.
function knowledgeBookHtml() {
  if (!KNOWLEDGE.length) return "";
  return '<div class="panel" id="knowBook">' +
    '<div class="micro">knowledge · what every number means, and what a gap is not</div>' +
    '<div class="reason">Every card is the contract behind a number on the ' +
    "instrument: what it measures, the formula if it is derived, the read that " +
    "produced it, its cadence and retention tier, and what a gap means. A gap " +
    "always means “not measured”, never zero.</div>" +
    KNOWLEDGE.map((sec) =>
      '<details class="know-sec"><summary>' + esc(sec.title) +
      ' <span class="note">' + esc(sec.note) + "</span></summary>" +
      '<div class="know-grid">' + sec.cards.map(knowCardHtml).join("") + "</div>" +
      "</details>").join("") +
    "</div>";
}

// ------------------------------------------- permissions + settings (T15/T16A)
// The dashboard is unprivileged by design. ONE place can escalate: ⚙ settings →
// permissions, and only when a human presses Grant — one macOS admin prompt, no
// password ever touched by our code. While the drawer is open the state is
// re-read every 2 s, so the panel flips to `granted` the moment macOS returns
// instead of waiting for the next slow poll.
const PERM_SIG = { sig: null };

function permRowHtml(p) {
  const stateTxt = p.granted === true ? "granted"
    : p.granted === false ? (p.required ? "not granted · required" : "not granted")
      : "not checked yet";
  const action = p.granted === true
    ? (p.root_tier
      ? '<button class="grant" type="button" data-perm-revoke="' + esc(p.id) + '">Revoke</button>'
      : "")
    : '<button class="grant" type="button" data-perm-grant="' + esc(p.id) + '">Grant</button>';
  return '<div class="perm-row">' +
    '<span class="perm-dot ' + (p.granted === true ? "is-on"
      : p.granted === false ? "is-off" : "is-unknown") + '" aria-hidden="true"></span>' +
    '<div class="perm-main">' +
    '<div class="perm-name">' + esc(p.label) +
    ' <span class="perm-state">' + esc(stateTxt) + "</span></div>" +
    '<div class="reason">unlocks: ' + esc(p.unlock_text) + "</div>" +
    '<div class="reason">how: ' + esc(p.how_to_grant) + "</div>" +
    (p.detail ? '<div class="reason">' + esc(p.detail) + "</div>" : "") +
    '<div class="reason is-warn" data-perm-note="' + esc(p.id) + '"></div>' +
    "</div>" + action + "</div>";
}

function settingsHtml() {
  return '<div class="panel">' +
    '<div class="micro">tiles · the order and visibility of the nine L1 tiles</div>' +
    '<div class="tile-custom">' +
    '<label><input type="checkbox" id="tileEditToggle"' + (TILES.editing ? " checked" : "") +
    '> customize tiles</label>' +
    '<button class="grant" type="button" id="tileReset"' + (TILES.editing ? "" : " hidden") +
    '>reset to default</button>' +
    "</div>" +
    '<div class="reason">on: every tile shows a drag handle (top-left) and a visibility ' +
    "checkbox (top-right). Drag a tile to move it, untick it to hide it. The order and " +
    "the hidden set are stored in this browser only, under " +
    "<b>syswatch-tile-config</b>; reset puts the default nine back.</div>" +
    "</div>" +
    '<div class="panel">' +
    '<div class="micro">permissions · every privilege syswatch uses, what it unlocks, how it is granted</div>' +
    '<p class="perm-model" id="permModel">reading permission state…</p>' +
    '<div class="reason" id="permSummary"></div>' +
    "</div>" +
    '<div id="permGroups"><div class="panel"><span class="na">loading permissions…</span></div></div>' +
    '<div class="panel"><div class="micro">electricity tariff · cost stays hidden until a rate is entered</div>' +
    '<div class="tariff-row">' +
    '<label class="slicer">rate per kWh <input class="cons-filter" id="tariffRate" ' +
    'type="number" min="0" step="0.01" inputmode="decimal" placeholder="not set"></label>' +
    '<label class="slicer">currency <input class="cons-filter" id="tariffCur" type="text" ' +
    'maxlength="4" placeholder="optional"></label>' +
    '<button class="grant" type="button" id="tariffSave">save rate</button>' +
    "</div>" +
    '<div class="reason" id="tariffNote">no rate entered — the Power &amp; battery section ' +
    "shows measured Wh and no cost. The rate never leaves this browser.</div>" +
    '<div class="reason">the manual for every number is below — formula, source, ' +
    "cadence and what a gap means.</div></div>" +
    knowledgeBookHtml() +
    '<div class="panel"><div class="micro">about</div>' +
    '<div class="about-grid" id="aboutGrid"><span class="na">reading…</span></div></div>' +
    '<div class="panel"><div class="micro">data sources · every capability, probed at runtime</div>' +
    '<table class="table src-table"><thead><tr><th>Capability</th><th class="num">Rows</th>' +
    '<th>Source</th><th>State</th></tr></thead><tbody id="sourcesBody"></tbody></table>' +
    '<div class="reason" id="sourcesNote"></div></div>';
}

function renderSettings() {
  $("settingsBody").innerHTML = settingsHtml();
  PERM_SIG.sig = null;
  updatePermissionRows();
  renderSources();
  renderAbout();
  wireTariff();
  wireTileSettings();
}

// The tariff is the ONLY setting a user types, and it is stored in this browser
// only: the server never learns it, so it can never be assumed for anyone.
function wireTariff() {
  const rate = $("tariffRate");
  const cur = $("tariffCur");
  const note = $("tariffNote");
  const btn = $("tariffSave");
  if (!rate || !btn) return;
  const t = tariff();
  if (t) {
    if (!rate.value) rate.value = String(t.rate);
    if (cur && !cur.value) cur.value = t.currency || "";
    if (note) {
      note.textContent = "cost shown for measured Wh at " + t.rate + " per kWh" +
        (t.currency ? " (" + t.currency + ")" : "") +
        " · clear the rate and save to hide it again · stored in this browser only";
    }
  }
  btn.addEventListener("click", () => {
    const r = parseFloat(rate.value);
    const c = cur ? cur.value.trim() : "";
    saveTariff(isFinite(r) && r > 0 ? r : null, c);
    const now = tariff();
    if (note) {
      note.textContent = now
        ? "saved · cost is now shown in the Power & battery section at " + now.rate +
          " per kWh" + (now.currency ? " (" + now.currency + ")" : "") +
          " · stored in this browser only"
        : "no rate entered — measured Wh only, no cost. Nothing is assumed.";
    }
    toast(now ? "tariff saved: " + now.rate + " per kWh" : "tariff cleared", "ok");
  });
}

// Permission rows: rebuilt only when the granted state actually changes, so a
// 2 s poll never steals a click from under the pointer.
function updatePermissionRows() {
  const box = $("permGroups");
  if (!box) return;
  const d = state.permissions;
  if (!d || !Array.isArray(d.permissions)) {
    box.innerHTML = '<div class="panel"><span class="na">loading permissions…</span></div>';
    return;
  }
  const sig = d.permissions.map((p) => p.id + ":" + p.granted).join("|");
  if (sig === PERM_SIG.sig && !state.permBusy) return;
  PERM_SIG.sig = sig;

  const model = $("permModel");
  if (model) {
    model.textContent = ((d.root_tier || {}).model ||
      "the dashboard never runs as root; root needs one macOS admin prompt") + ".";
  }
  const rows = d.permissions;
  const on = rows.filter((r) => r.granted === true).length;
  const missing = d.missing_required || [];
  const sum = $("permSummary");
  if (sum) {
    sum.innerHTML = on + " of " + rows.length + " granted" +
      (missing.length
        ? ' · <span class="warn-pill">' + missing.length + " required still not granted</span>"
        : " · nothing required is missing") +
      (d.checked_ts ? " · checked " + fmtTime(d.checked_ts) : "") +
      " · re-read every 2 s while this drawer is open";
  }
  const groups = [];
  rows.forEach((r) => {
    const name = r.group || "Other";
    let slot = groups.find((g) => g.name === name);
    if (!slot) { slot = { name: name, rows: [] }; groups.push(slot); }
    slot.rows.push(r);
  });
  box.innerHTML = groups.map((g) =>
    '<div class="panel"><div class="micro">' + esc(g.name) + "</div>" +
    '<div class="perm-list">' + g.rows.map(permRowHtml).join("") + "</div></div>"
  ).join("");
  wirePermissionButtons();
}

function renderSources() {
  const tbody = $("sourcesBody");
  if (!tbody) return;
  const reg = state.sensors || {};
  const groups = reg.groups || [];
  tbody.innerHTML = groups.map((g) => {
    const rows = g.sensors || [];
    const reading = rows.filter((s) => s.available).length;
    const sources = [];
    rows.forEach((s) => {
      if (s.source && sources.indexOf(s.source) < 0) sources.push(s.source);
    });
    const src = sources.slice(0, 2).join(" · ") +
      (sources.length > 2 ? " · +" + (sources.length - 2) + " more" : "");
    const stateTxt = rows.length === 0 ? "not on this machine"
      : reading === rows.length ? '<span class="ok">' + reading + " of " + rows.length + " reading</span>"
        : g.needs_root && reading === 0
          ? '<span class="warn">needs root</span>'
          : reading + " of " + rows.length + " reading";
    return "<tr><td>" + esc(g.label) + '</td><td class="num">' + rows.length +
      "</td><td>" + esc(src) + "</td><td>" + stateTxt + "</td></tr>";
  }).join("") || '<tr><td colspan="4" class="na">sensor registry unavailable</td></tr>';
  const note = $("sourcesNote");
  if (note) {
    note.textContent = "platform " + (reg.platform || "unknown") +
      " · " + groups.length + " capability groups · a row that cannot be read reports the reason, never 0";
  }
}

async function renderAbout() {
  const grid = $("aboutGrid");
  if (!grid) return;
  const h = await fetchJSON("health", "/api/health");
  const reg = state.sensors || {};
  const rt = (state.permissions || {}).root_tier || {};
  const root = (state.power || {}).root || (state.powerHistory || {}).root || {};
  const rootTxt = rt.granted === true || root.available === true
    ? "granted · read-only sample " +
      (isNum(root.age_s) ? root.age_s.toFixed(0) + " s old" : "(daemon file present)")
    : "not granted — package power, ANE and thermal pressure say so";
  const rows = [
    ["dashboard", h ? "pid " + h.pid + " · port " + h.port : na("health endpoint unreachable")],
    ["uptime", h && isNum(h.uptime_s) ? fmtSpan(h.uptime_s) : na("unknown")],
    ["platform", reg.platform || "reading the registry…"],
    ["time range", state.range + " · set by the sticky pill or a chart brush"],
    ["poll cadence", "numbers 1 s · charts 5 s · consumers 20 s sweeps"],
    ["root tier", rootTxt],
    ["honesty", "no fabricated values · gaps render as gaps · every number names its source"],
  ];
  grid.innerHTML = rows.map((r) =>
    '<div class="about-row"><span class="label">' + esc(r[0]) + '</span>' +
    '<span class="val">' + esc(r[1]) + "</span></div>").join("");
}

function wirePermissionButtons() {
  document.querySelectorAll("[data-perm-grant]").forEach((btn) => {
    btn.addEventListener("click", () => permAction("grant", btn.dataset.permGrant, btn));
  });
  document.querySelectorAll("[data-perm-revoke]").forEach((btn) => {
    btn.addEventListener("click", () => permAction("revoke", btn.dataset.permRevoke, btn));
  });
}

function grantedIds(d) {
  const out = {};
  ((d || {}).permissions || []).forEach((p) => { if (p.granted === true) out[p.id] = true; });
  return out;
}

async function permAction(kind, id, btn) {
  if (state.permBusy) return;
  state.permBusy = true;
  const note = document.querySelector('[data-perm-note="' + id + '"]');
  const original = btn.textContent;
  btn.disabled = true;
  btn.textContent = kind === "grant" ? "waiting for macOS…" : "removing…";
  if (note) {
    note.textContent = kind === "grant"
      ? "a macOS admin prompt is open — approving it installs the root daemon; the dashboard never sees the password"
      : "a macOS admin prompt is open — approving it removes the root daemon";
  }
  try {
    const r = await fetch("/api/permissions/" + kind + "?id=" + encodeURIComponent(id),
      { method: "POST", headers: { "X-Syswatch": "grant" } });
    const d = await r.json();
    if (note) {
      note.textContent = (d.granted === true ? "granted · " : "") +
        (d.reason || d.detail || ("HTTP " + r.status));
    }
    btn.textContent = d.ok ? original : "failed";
  } catch (e) {
    if (note) note.textContent = kind + " request failed: " + e.message;
    btn.textContent = "failed";
  }
  state.permBusy = false;
  // The prompt has returned: re-probe NOW, then let the 2 s poll follow it. This
  // is the "it stays on grant for a while" complaint — the drawer must flip in
  // the same instant macOS does.
  await afterGrantFlip(kind === "revoke");
}

// Re-read what the privilege unlocks and repaint everything that showed it.
async function afterGrantFlip(revoking) {
  PERM_SIG.sig = null;
  await refreshPermissions(true);
  updatePermissionRows();
  updatePermissionBadge();
  await refreshSensors();
  await Promise.all([refreshPower(), refreshPowerSummary()]);
  await loadPowerHistory();
  renderSources();
  renderAbout();
  paintInstrumentCharts();
  updateBento();
  if (!revoking) {
    const got = (state.permissions || {}).missing_required || [];
    if (!got.length) toast("permissions granted — root tier is live", "ok");
    else toast("still not granted: " + got.join(", "), "warn");
  }
}

async function refreshPermissions(probe) {
  const raw = await fetchJSON("permissions:" + (probe ? "full" : "cheap"),
    "/api/permissions" + (probe ? "" : "?probe=0"));
  const d = probe ? raw : mergePermissions(raw);
  if (d) {
    state.permissions = d;
    updatePermissionBadge();
  }
  return d;
}

// 2 s poll while the settings drawer is open. Cheap probe (no osascript): it
// still reads the daemon file state, which is what the grant flow actually
// changes. A full probe runs on open and again right after a grant returns.
// The cheap probe reports "not probed yet" for anything that needs osascript
// (the Accessibility toggle). Carrying the last full-probe answer forward keeps
// a granted privilege from flickering back to "not checked" every 2 s.
function mergePermissions(fresh) {
  const prev = state.permissions;
  if (!fresh || !prev || !Array.isArray(prev.permissions)) return fresh;
  const byId = {};
  prev.permissions.forEach((p) => { byId[p.id] = p; });
  (fresh.permissions || []).forEach((p) => {
    const old = byId[p.id];
    if (old && p.granted === null && old.granted !== null) {
      p.granted = old.granted;
      p.detail = old.detail;
      p.stale_probe = true;
    }
  });
  return fresh;
}

async function pollPermissions() {
  if (!state.settingsOpen) return;
  const before = grantedIds(state.permissions);
  const raw = await fetchJSON("permissions:poll", "/api/permissions?probe=0");
  const d = mergePermissions(raw);
  if (!d || !state.settingsOpen) return;
  state.permissions = d;
  updatePermissionBadge();
  updatePermissionRows();
  const after = grantedIds(d);
  const flipped = Object.keys(after).filter((k) => !before[k]);
  if (flipped.length) {
    const labels = flipped.map((k) => {
      const p = (d.permissions || []).find((x) => x.id === k);
      return p ? p.label : k;
    });
    toast("granted: " + labels.join(", ") + " — re-probing sources", "ok");
    await refreshSensors();
    await Promise.all([refreshPower(), refreshPowerSummary()]);
    await loadPowerHistory();
    renderSources();
    renderAbout();
    paintInstrumentCharts();
    updateBento();
    if (l2Open()) paintOpenL2();
  }
}

// Tab badge: an amber dot the moment a REQUIRED privilege is missing, so an
// ungranted requirement is visible without opening the drawer.
function updatePermissionBadge() {
  const dot = $("permDot");
  if (!dot) return;
  const d = state.permissions;
  const missing = d && Array.isArray(d.missing_required) ? d.missing_required.length : 0;
  dot.hidden = !missing;
  dot.title = missing
    ? "required permission not granted: " + (d.missing_required || []).join(", ")
    : "";
}

// ------------------------------------------------------------- settings drawer
function openSettings() {
  const el = $("settingsDrawer");
  if (!el) return;
  state.baseView = state.baseView || "instrument";
  el.hidden = false;
  el.setAttribute("aria-hidden", "false");
  document.body.classList.add("has-drawer");
  state.settingsOpen = true;
  state.view = "settings";
  syncTabs();
  showChrome(state.baseView === "instrument");
  // The page behind the drawer must be the real instrument, never "loading…".
  const view = $("view");
  if (!view.querySelector(".sec, .table, .panel")) {
    renderView();
    refreshView(true);
  }
  renderSettings();
  refreshPermissions(true).then(async () => {
    updatePermissionRows();
    updatePermissionBadge();
    await refreshSensors();
    renderSources();
    renderAbout();
  });
  if (!state.settingsPoll) {
    state.settingsPoll = setInterval(pollPermissions, 2000);
  }
}

function closeSettings(restore) {
  const el = $("settingsDrawer");
  if (state.settingsPoll) {
    clearInterval(state.settingsPoll);
    state.settingsPoll = 0;
  }
  state.settingsOpen = false;
  if (el) {
    el.hidden = true;
    el.setAttribute("aria-hidden", "true");
  }
  document.body.classList.remove("has-drawer");
  if (restore === false) return;
  state.view = state.baseView && state.baseView !== "settings" ? state.baseView : "instrument";
  syncTabs();
  showChrome(state.view === "instrument");
}

// -------------------------------------------------------------- view plumbing
const VIEW_RENDER = {
  instrument: renderInstrument,
  apps: renderAppsSection,
  analytics: renderAnalyticsSection,
  knowledge: renderKnowledgeSection,
};

// Hero band + section index belong to the Instrument only. The range pill is
// the product's one range control, so it stays on every view.
function showChrome(showInstrument) {
  const inst = $("instrument");
  const view = $("view");
  const stage = $("stage");
  if (inst) inst.hidden = !showInstrument;
  if (view) view.hidden = showInstrument;
  // the instrument is one viewport (no page scroll); a document page scrolls
  // inside the stage so the sticky topbar can never be lost
  if (stage) stage.classList.toggle("is-page", !showInstrument);
  // a focused tile is an instrument state: leaving the instrument clears it
  if (!showInstrument && FOCUS.module) blurTile();
}

function renderView() {
  destroyCharts();
  const v = state.view === "settings" ? (state.baseView || "instrument") : state.view;
  const target = (v === "apps" || v === "analytics" || v === "knowledge")
    ? v : "instrument";
  showChrome(target === "instrument");
  (VIEW_RENDER[target] || renderInstrument)();
  renderChips();
}

function updateView() {
  if (state.view === "settings") return;
  if (state.view === "instrument") { updateInstrument(); return; }
  if (state.view === "apps") { updateAppsSection(); return; }
  if (state.view === "analytics") { updateAnalyticsSection(); return; }
  if (state.view === "knowledge") { updateKnowledgeSection(); return; }
}

async function loadViewData() {
  const v = state.view;
  if (v === "instrument" || v === "settings") return loadInstrumentData();
  if (v === "knowledge") return;          // the manual is static: nothing to read
  if (v === "apps") {
    // Apps is a RANGE read: one request per range change, no polling.
    await loadApps(true);
    return;
  }
  if (v === "analytics") {
    const rng = state.range;
    await Promise.all([loadGraph("cpu"), loadGraph("memory"), loadGraph("disk"),
      loadPowerHistory(), loadBattery(), loadApps(false)]);
    const d = await fetchJSON("analytics:" + rng,
      "/api/analytics?range=" + encodeURIComponent(rng));
    if (d) state.analytics = d;
    updateAnalyticsSection();
    return;
  }
}

async function refreshPower() {
  const d = await fetchJSON("power", "/api/power");
  if (d) state.power = d;
  return d;
}

async function refreshPowerSummary() {
  const d = await fetchJSON("power:summary", "/api/power/history");
  if (d) {
    state.powerSummary = d;
    if (state.powerHistory && d.current) state.powerHistory.current = d.current;
  }
  return d;
}

async function refreshCurrent() {
  const d = await fetchJSON("current", "/api/current");
  if (d) state.current = d;
  markPoll(!!d);
  return d;
}

async function refreshSensors() {
  const d = await fetchJSON("sensors", "/api/sensors");
  if (d) state.sensors = d;
  return d;
}

async function refreshView(force) {
  if (document.hidden && !force) return;
  if (state.view === "settings") {
    await Promise.all([loadInstrumentData(), refreshSensors()]);
    return;   // the drawer polls its own permissions; the page behind stays fresh
  }
  await Promise.all([loadViewData(), refreshSensors()]);
  if (state.view === "instrument") {
    // loadInstrumentData already repainted the bento once.
    updateBento();
    if (l2Open()) paintOpenL2();
    return;
  }
  updateView();
}

// ------------------------------------------------------------------- routing
let fadeTimer = null;

const LEGACY = {
  overview: "instrument", cpu: "instrument", memory: "instrument", gpu: "instrument",
  disk: "instrument", network: "instrument", battery: "instrument", power: "instrument",
  permissions: "settings",
  // T20: the manual is reference material, not a layer — it moved into ⚙.
  knowledge: "settings", help: "settings",
};

function navigate(id) {
  if (id === "settings") {
    if (location.hash !== "#settings") location.hash = "settings";
    else openSettings();
    return;
  }
  const target = VIEW_IDS.includes(id) ? id : "instrument";
  if (location.hash === "#" + target) setView(target);
  else location.hash = target;
}

function setView(id) {
  const mapped = LEGACY[id] || id;
  if (mapped === "settings") {
    state.baseView = state.baseView || "instrument";
    openSettings();
    return;
  }
  closeSettings(false);
  const target = VIEW_IDS.includes(mapped) ? mapped : "instrument";
  state.view = target;
  state.baseView = target;
  syncTabs();
  showChrome(target === "instrument");
  const el = $("view");
  if (el) el.classList.add("is-out");
  clearTimeout(fadeTimer);
  fadeTimer = setTimeout(() => {
    renderView();
    if (el) el.classList.remove("is-out");
    if (target === "instrument" && LEGACY[id] && id !== "instrument") {
      // a pre-T17A link (#cpu, #memory, …) used to jump to a stacked section.
      // Now it opens that module's L2 drawer, which is where the section went.
      if (BENTO_CHIPS.indexOf(id) >= 0 || CONSUMER_SECTIONS.indexOf(id) >= 0) openL2(id);
    }
    const stage = $("stage");
    if (stage) stage.scrollTop = 0;
    refreshView(true);
  }, 140);
}

function route() {
  const raw = (location.hash || "").replace(/^#/, "");
  const id = raw || "instrument";
  if (!location.hash) {
    try { history.replaceState(null, "", "#instrument"); } catch (e) { /* ignore */ }
  }
  setView(id);
}

// ---------------------------------------------------------------------- logs
async function loadLogs() {
  try {
    const r = await fetch("/api/logs?limit=40", { cache: "no-store" });
    const d = await r.json();
    const rows = Array.isArray(d.events)
      ? d.events.map((e) => ((e.ts || "").slice(11, 19) + "  " + (e.src || "") +
        "  " + (e.msg || "")).trim())
      : (d.lines || []);
    $("logLines").innerHTML = rows.map(esc).join("<br>") || "<span class='na'>no events</span>";
    $("logCount").textContent = "(" + rows.length + ")";
  } catch (e) {
    $("logLines").innerHTML = "<span class='na'>log fetch failed</span>";
  }
}

// ======================================================== T22 · tile focus mode
// A click on a tile is not a navigation and not an overlay: the tile ITSELF
// grows to 2 columns × 2 rows inside the same grid, the rest of the grid reflows
// around it, and every other tile steps back to 0.35 and goes inert. The
// focused tile swaps its 40 px sparkline for its real series — the same
// /api/graph or power-ring window the range pill is showing — with its stats
// beside it: per-core for CPU, every SMC temperature for Thermal, measures
// otherwise.
//
// Unfocus: a second click on the focused tile, a click anywhere else, or
// Escape. Thermal focuses but never expands (it is the tile whose whole job is
// that the four temperatures fit at rest).
const FOCUS = { module: null };

// The one place a tile's series is read for focus. Same sources as L2 so a
// focused tile and its L2 panel can never disagree about a number.
function paintFocusChart(module) {
  const cv = $("focus-chart");
  if (!cv) return;
  const ser = l2ModuleSeries(module);
  const scroller = sizeChartInner("focus-chart", ser.points);
  if (ser.kind === "multi") {
    const painted = paintMulti("focus-chart", ser.specs, ser.points, ser.ticks, ser.fmt);
    if (!painted) clearChart("focus-chart", ser.ticks, ser.reason);
  } else if (ser.kind === "single") {
    const painted = paintSeries("focus-chart", module, ser.points, ser.ticks);
    if (!painted) clearChart("focus-chart", ser.ticks, ser.reason);
  }
  wireBrush("focus-chart");
  keepLiveEdge(scroller);
}

// Per core, for the CPU tile: badge · utilisation · residency. The clock is a
// PACKAGE clock (Apple Silicon exposes no per-core frequency) and residency is a
// GPU notion on this SDK, so both columns are labelled rather than invented.
function focusCoreRowsHtml() {
  const cpu = (state.current || {}).cpu || {};
  const clusters = Array.isArray(cpu.clusters) ? cpu.clusters : [];
  const perCore = Array.isArray(cpu.per_core_pct) ? cpu.per_core_pct : [];
  if (!perCore.length) {
    return '<span class="na">per-core utilisation unavailable — cpu.per_core_pct ' +
      "missing in /api/current</span>";
  }
  const out = ['<div class="tf-core tf-core-head"><span class="k">core</span>' +
    '<span class="v">util</span><span class="v dim">res</span></div>'];
  clusters.forEach((c, ci) => {
    const start = clusterSpan(clusters, ci);
    const badge = clusterBadge(c);
    const clock = isNum(c.freq_mhz) ? Math.round(c.freq_mhz) + " MHz" : null;
    out.push('<div class="tf-core tf-core-head"><span class="k">' + esc(badge) +
      " cluster</span>" + '<span class="v">' +
      (clock ? esc(clock) + ' <span class="note">pkg</span>' : "clock —") + "</span></div>");
    for (let n = 0; n < (c.cores || 0); n++) {
      const v = perCore[start + n];
      const cls = isNum(v) && v >= 85 ? " is-warn" : "";
      out.push('<div class="tf-core"><span class="k">' + esc(badge + (n + 1)) + "</span>" +
        '<span class="v' + cls + '">' + (isNum(v) ? v.toFixed(0) + " %" : "—") + "</span>" +
        '<span class="v dim">—</span></div>');
    }
  });
  out.push('<p class="tf-note">clock is the package clock (psutil, freq_scope ' +
    "“package”): Apple Silicon exposes no per-core frequency. Per-core residency " +
    "is not exposed either — the one residency this SDK reports is the GPU's.</p>");
  return out.join("");
}

// Everything beside the chart depends on the module: cores for CPU, the four
// temperatures for Thermal, measures + the module's own numbers for the rest.
function focusSideHtml(module) {
  const spec = MODULE_SERIES[module] || {};
  const fmt = spec.fmt || ((v) => String(v));
  const src = '<div class="tf-src">' + esc(MODULE_SOURCE[module] || "") + "</div>";
  if (module === "cpu") return focusCoreRowsHtml() + src;

  let extra = "";
  if (module === "gpu") {
    const g = (state.current || {}).gpu || {};
    const root = (state.powerHistory || {}).root || {};
    const rows = [
      ["renderer", isNum(g.renderer_pct) ? g.renderer_pct.toFixed(0) + " %" : null],
      ["wired VRAM", isNum(g.mem_used_mb) ? fmtMB(g.mem_used_mb) : null],
      ["package clock", isNum(root.gpu_freq_mhz) ? Math.round(root.gpu_freq_mhz) + " MHz" : null],
      ["residency", isNum(root.gpu_residency_pct) ? root.gpu_residency_pct.toFixed(1) + " %" : null],
    ].filter((r) => r[1]);
    if (rows.length) {
      extra = '<div class="tf-core tf-core-head"><span class="k">gpu</span></div>' +
        rows.map(([k, v]) => '<div class="tf-core"><span class="k">' + esc(k) +
          '</span><span class="v">' + esc(v) + "</span></div>").join("");
    }
  }
  if (module === "thermal") {
    return '<span class="micro">every temperature</span>' +
      thermalSummary().map((t) => '<div class="tf-core"><span class="k">' + esc(t.label) +
        '</span><span class="v">' + (isNum(t.v) ? t.v.toFixed(1) + " °C" : "—") +
        "</span></div>").join("") + src;
  }
  if (module === "network") {
    const net = (state.current || {}).net || {};
    const ifs = (net.ifaces || []).filter((i) => i.name);
    if (ifs.length) {
      extra = '<div class="tf-core tf-core-head"><span class="k">iface</span>' +
        '<span class="v">down</span></div>' +
        ifs.slice(0, 6).map((i) => '<div class="tf-core"><span class="k">' +
          esc(i.name) + '</span><span class="v">' +
          (isNum(i.down_bps) ? esc(fmtBytes(i.down_bps, 1)) : "—") +
          "</span></div>").join("");
    }
  }
  if (module === "disk") {
    const io = (state.current || {}).disk_io || {};
    const rows = [
      ["read", isNum(io.read_bps) ? fmtBytes(io.read_bps, 1) : null],
      ["write", isNum(io.write_bps) ? fmtBytes(io.write_bps, 1) : null],
      ["read iops", isNum(io.read_iops) ? io.read_iops.toFixed(0) : null],
      ["write iops", isNum(io.write_iops) ? io.write_iops.toFixed(0) : null],
    ].filter((r) => r[1]);
    if (rows.length) {
      extra = rows.map(([k, v]) => '<div class="tf-core"><span class="k">' + esc(k) +
        '</span><span class="v">' + esc(v) + "</span></div>").join("");
    }
  }
  if (module === "power" || module === "battery") {
    const root = (state.powerHistory || {}).root || {};
    const bat = (state.current || {}).battery || {};
    const rows = [
      ["CPU rail", isNum(root.cpu_w) ? fmtW(root.cpu_w) : null],
      ["GPU rail", isNum(root.gpu_w) ? fmtW(root.gpu_w) : null],
      ["ANE rail", isNum(root.ane_w) ? fmtW(root.ane_w) : null],
      ["health", isNum(bat.health_pct) ? bat.health_pct.toFixed(0) + " %" : null],
      ["cycles", isNum(bat.cycles) ? String(bat.cycles) : null],
    ].filter((r) => r[1]);
    if (rows.length) {
      extra = rows.map(([k, v]) => '<div class="tf-core"><span class="k">' + esc(k) +
        '</span><span class="v">' + esc(v) + "</span></div>").join("");
    }
  }
  return '<span class="micro">measures · ' + esc(state.range) + "</span>" +
    measuresHtml(module) + extra + src;
}

function focusBodyHtml(module) {
  const label = MODULE_LABEL[module] || module;
  return '<div class="tf-chart">' +
    '<div class="tf-head"><span class="micro">' + esc(label) + " · " + esc(state.range) +
    " · drag to zoom</span>" +
    '<button class="tf-btn" type="button" id="focusDetails">full details</button></div>' +
    '<div class="chart-scroll"><div class="chart-inner">' +
    '<canvas id="focus-chart"></canvas><div class="brush" id="focus-brush" hidden></div>' +
    "</div></div>" +
    "</div>" +
    '<div class="tf-side" id="focus-side">' + focusSideHtml(module) + "</div>";
}

function focusTile(module) {
  if (!module) return;
  if (FOCUS.module === module) { blurTile(); return; }
  blurTile();
  const grid = $("tileGrid");
  if (!grid) return;
  FOCUS.module = module;
  tileNodes().forEach((t) => {
    const on = t.dataset.module === module;
    t.classList.toggle("is-focused", on);
    t.classList.toggle("is-dimmed", !on);
    t.setAttribute("aria-expanded", String(on));
  });
  const el = tileNode(module);
  const thermalLike = el && el.classList.contains("tile--noexpand");
  if (el && !thermalLike) {
    const host = el.querySelector(".tile-focus");
    if (host) {
      host.innerHTML = focusBodyHtml(module);
      host.hidden = false;
      requestAnimationFrame(() => paintFocusChart(module));
    }
    // the series may not be in memory yet (a fresh page, or a module whose
    // window has never been read): read it, then paint the same tile
    const spec = MODULE_SERIES[module] || {};
    if (spec.metric && !state.graphs[module]) {
      loadGraph(module).then(() => {
        if (FOCUS.module !== module) return;
        paintFocusChart(module);
        paintFocusSide();      // measures land with the same payload
      });
    }
    if (!state.powerHistory && (module === "power" || module === "thermal" ||
        module === "battery" || module === "cpu" || module === "gpu")) {
      loadPowerHistory().then(() => {
        if (FOCUS.module !== module) return;
        paintFocusChart(module);
        paintFocusSide();
      });
    }
  }
  paintFocusSide();
}

function paintFocusSide() {
  const module = FOCUS.module;
  if (!module) return;
  const side = $("focus-side");
  if (side) side.innerHTML = focusSideHtml(module);
}

function blurTile() {
  const module = FOCUS.module;
  if (!module) return;
  FOCUS.module = null;
  tileNodes().forEach((t) => {
    t.classList.remove("is-focused", "is-dimmed");
    t.setAttribute("aria-expanded", "false");
    const host = t.querySelector(".tile-focus");
    if (host) {
      host.hidden = true;
      host.innerHTML = "";
    }
  });
  if (CHARTS["focus-chart"]) {
    try { CHARTS["focus-chart"].destroy(); } catch (e) { /* already gone */ }
    delete CHARTS["focus-chart"];
  }
}

// ==================================================== T22 · tile customization
// Order and visibility live in localStorage under `syswatch-tile-config`:
//   { v: 1, order: ["cpu", …], hidden: ["load"] }
// A stored key that names a tile this build does not have is ignored, and a
// tile the key omits is appended — so a stale key can never hide a tile for
// good, and a new tile can never be invisible because it postdates the key.
const TILE_STORE = "syswatch-tile-config";
const TILE_DEFAULT_ORDER = ["cpu", "memory", "gpu", "battery", "power",
  "network", "disk", "load", "thermal"];
const TILES = { editing: false, cfg: null };

function tileGridEl() { return $("tileGrid"); }
function tileNodes() {
  const g = tileGridEl();
  return g ? Array.prototype.slice.call(g.querySelectorAll(".tile")) : [];
}
function tileNode(module) {
  return tileNodes().filter((t) => t.dataset.module === module)[0] || null;
}

function defaultTileConfig() {
  return { v: 1, order: TILE_DEFAULT_ORDER.slice(), hidden: [] };
}

function loadTileConfig() {
  const def = defaultTileConfig();
  let raw = null;
  try { raw = window.localStorage.getItem(TILE_STORE); } catch (e) { raw = null; }
  if (!raw) return def;
  let obj = null;
  try { obj = JSON.parse(raw); } catch (e) { return def; }
  if (!obj || !Array.isArray(obj.order)) return def;
  const known = TILE_DEFAULT_ORDER;
  const order = [];
  obj.order.forEach((m) => {
    if (known.indexOf(m) >= 0 && order.indexOf(m) < 0) order.push(m);
  });
  known.forEach((m) => { if (order.indexOf(m) < 0) order.push(m); });
  const hidden = Array.isArray(obj.hidden)
    ? obj.hidden.filter((m) => known.indexOf(m) >= 0) : [];
  return { v: 1, order: order, hidden: hidden };
}

function saveTileConfig(cfg) {
  TILES.cfg = cfg;
  try { window.localStorage.setItem(TILE_STORE, JSON.stringify(cfg)); } catch (e) { /* private mode */ }
}

function tileConfig() {
  if (!TILES.cfg) TILES.cfg = loadTileConfig();
  return TILES.cfg;
}

// Order is applied by re-appending the nodes in the configured sequence: the
// nodes themselves (and every id inside them) never change, so the data layer
// and the focus chart keep working across a reorder.
function applyTileConfig() {
  const g = tileGridEl();
  if (!g) return;
  const cfg = tileConfig();
  cfg.order.forEach((m) => {
    const el = tileNode(m);
    if (el) g.appendChild(el);
  });
  tileNodes().forEach((el) => {
    const m = el.dataset.module;
    const hidden = cfg.hidden.indexOf(m) >= 0;
    el.hidden = hidden;
    const box = el.querySelector("[data-tile-visible]");
    if (box) box.checked = !hidden;
  });
  // a hidden tile cannot stay focused
  if (FOCUS.module && cfg.hidden.indexOf(FOCUS.module) >= 0) blurTile();
}

function setTileEditing(on) {
  TILES.editing = on === true;
  const g = tileGridEl();
  if (g) g.classList.toggle("is-editing", TILES.editing);
  const box = $("tileEditToggle");
  if (box) box.checked = TILES.editing;
  const reset = $("tileReset");
  if (reset) reset.hidden = !TILES.editing;
  if (TILES.editing) blurTile();
  tileNodes().forEach((el) => {
    el.draggable = TILES.editing;
    const grip = el.querySelector(".tile-grip");
    if (grip) grip.setAttribute("aria-hidden", TILES.editing ? "false" : "true");
  });
}

function resetTileConfig() {
  saveTileConfig(defaultTileConfig());
  applyTileConfig();
  toast("tiles reset to the default order and visibility");
}

// Drag-to-reorder with the HTML5 drag API — no library, and the drop index is
// computed from the nodes on the page, so a hidden tile cannot land in a hole.
function wireTileDrag() {
  const g = tileGridEl();
  if (!g) return;
  let dragged = null;
  g.addEventListener("dragstart", (ev) => {
    if (!TILES.editing) { ev.preventDefault(); return; }
    const el = ev.target.closest && ev.target.closest(".tile");
    if (!el) return;
    dragged = el;
    el.classList.add("is-dragging");
    try {
      ev.dataTransfer.effectAllowed = "move";
      ev.dataTransfer.setData("text/plain", el.dataset.module || "");
    } catch (e) { /* Safari needs the try */ }
  });
  g.addEventListener("dragend", () => {
    if (dragged) dragged.classList.remove("is-dragging");
    tileNodes().forEach((t) => t.classList.remove("is-drop-target"));
    dragged = null;
  });
  g.addEventListener("dragover", (ev) => {
    if (!TILES.editing || !dragged) return;
    ev.preventDefault();
    try { ev.dataTransfer.dropEffect = "move"; } catch (e) { /* ignore */ }
    const over = ev.target.closest && ev.target.closest(".tile");
    tileNodes().forEach((t) => t.classList.toggle("is-drop-target", !!over && t === over));
  });
  g.addEventListener("drop", (ev) => {
    if (!TILES.editing || !dragged) return;
    ev.preventDefault();
    const over = ev.target.closest && ev.target.closest(".tile");
    tileNodes().forEach((t) => t.classList.remove("is-drop-target"));
    if (!over || over === dragged) return;
    const nodes = tileNodes();
    const from = nodes.indexOf(dragged);
    const to = nodes.indexOf(over);
    if (from < 0 || to < 0) return;
    const cfg = tileConfig();
    const order = nodes.map((t) => t.dataset.module);
    order.splice(from, 1);
    order.splice(to, 0, dragged.dataset.module);
    cfg.order = order;
    saveTileConfig(cfg);
    applyTileConfig();
  });
}

// Checkboxes: one per tile, inside the tile, only visible while customizing.
function wireTileCustomize() {
  const g = tileGridEl();
  if (!g) return;
  g.addEventListener("change", (ev) => {
    const box = ev.target;
    if (!box || !box.dataset || !box.dataset.tileVisible) return;
    const m = box.dataset.tileVisible;
    const cfg = tileConfig();
    const hidden = cfg.hidden.filter((x) => x !== m);
    if (!box.checked) hidden.push(m);
    cfg.hidden = hidden;
    saveTileConfig(cfg);
    applyTileConfig();
  });
  wireTileDrag();
  applyTileConfig();
}

// The settings row is re-rendered every time the drawer opens, so its two
// controls are bound where they are created, not at boot.
function wireTileSettings() {
  const box = $("tileEditToggle");
  if (box) {
    box.checked = TILES.editing;
    box.addEventListener("change", (ev) => setTileEditing(ev.target.checked));
  }
  const reset = $("tileReset");
  if (reset) {
    reset.hidden = !TILES.editing;
    reset.addEventListener("click", resetTileConfig);
  }
}


// ---------------------------------------------------------------------- boot
window.addEventListener("DOMContentLoaded", () => {
  syncRangePill();

  // ---- navigation: four views, nothing else in the bar
  $("tabs").querySelectorAll(".tab").forEach((t) => {
    t.addEventListener("click", () => navigate(t.dataset.view));
  });
  window.addEventListener("hashchange", route);

  // ---- the one range control (static markup → delegated here)
  const pill = $("rangePill");
  if (pill) {
    pill.addEventListener("click", (ev) => {
      const btn = ev.target.closest("[data-range]");
      if (btn) setRange(btn.dataset.range);
    });
  }

  // ---- L1 → focus (T22): a click on a tile focuses IT (2 × 2, the rest dimmed
  // and inert). A click on the focused tile again unfocuses; a click anywhere
  // outside it also unfocuses, which is what makes a dimmed tile behave like
  // "click to unfocus" even though it is pointer-events:none. A click that lands
  // inside the focus body (the chart, its brush, the full-details button) is not
  // a request to unfocus. Customize mode owns the click instead.
  document.addEventListener("click", (ev) => {
    const tile = ev.target.closest && ev.target.closest("#tileGrid .tile");
    if (TILES.editing) return;
    if (tile) {
      if (ev.target.closest("#focusDetails")) { openL2(tile.dataset.module); return; }
      if (ev.target.closest(".tile-focus")) return;
      focusTile(tile.dataset.module);
      return;
    }
    if (FOCUS.module) { blurTile(); return; }
    // anything else that names a module (a knowledge card, an app row link)
    // still opens that module's L2, as it always did
    const t = ev.target.closest && ev.target.closest("[data-module]");
    if (!t) return;
    if (ev.target.closest("canvas") || ev.target.closest(".l2-inline") ||
        ev.target.closest(".l2")) return;
    openL2(t.dataset.module);
  });
  document.addEventListener("keydown", (ev) => {
    const tile = ev.target.closest && ev.target.closest("#tileGrid .tile");
    if (!tile) return;
    if (ev.key === "Enter" || ev.key === " ") {
      ev.preventDefault();
      if (!TILES.editing) focusTile(tile.dataset.module);
    }
  });

  // ---- drawers
  if ($("drawerClose")) $("drawerClose").addEventListener("click", closeDrawer);
  if ($("drawer")) {
    $("drawer").addEventListener("click", (ev) => {
      if (ev.target.id === "drawer") closeDrawer();
    });
  }
  if ($("settingsClose")) {
    $("settingsClose").addEventListener("click", () => {
      closeSettings();
      if (location.hash === "#settings") {
        try { history.replaceState(null, "", "#" + (state.baseView || "instrument")); }
        catch (e) { location.hash = state.baseView || "instrument"; }
      }
      refreshView(true);
    });
  }
  if ($("settingsDrawer")) {
    $("settingsDrawer").addEventListener("click", (ev) => {
      if (ev.target.id === "settingsDrawer" && $("settingsClose")) $("settingsClose").click();
    });
  }
  document.addEventListener("keydown", (ev) => {
    if (ev.key === "Escape") {
      if (state.settingsOpen) { if ($("settingsClose")) $("settingsClose").click(); }
      else if (L2.appPanel) closeAppPanel();   // the overlay sits above the page
      else if (l2Open()) closeL2();            // then the inline expansion
      else if (FOCUS.module) blurTile();       // then the focused tile
      else if ($("hoverTip") && !$("hoverTip").hidden) {
        const tip = $("hoverTip"); tip.hidden = true; HOVER.module = null;
      } else closeDrawer();
    }
  });

  // ---- the application panel: ✕ and the backdrop both close it, and neither
  // one touches location.hash (it is an overlay, not a route)
  if ($("l2Close")) $("l2Close").addEventListener("click", closeAppPanel);
  if ($("l2Backdrop")) $("l2Backdrop").addEventListener("click", closeAppPanel);
  if ($("view")) {
    $("view").addEventListener("click", (ev) => {
      if (ev.target === $("view")) clearFilter();
    });
  }

  // ---- hover-to-reveal on the nine L1 objects
  wireHover();
  // ---- T22: order + visibility + focus, restored from localStorage
  wireTileCustomize();

  route();
  loadLogs();
  // cheap permission check at boot (a stat + cached AX state, no prompt, no
  // osascript) purely to light the amber dot on the gear.
  refreshPermissions(false).then(() => updatePermissionBadge());
  setInterval(() => {
    if (!document.hidden && !state.settingsOpen) {
      refreshPermissions(false).then(() => {
        updatePermissionBadge();
        if (state.settingsOpen) updatePermissionRows();
      });
    }
  }, 30000);

  // 1 s: numbers only (hero band + the sections on screen). 5 s: charts.
  // Apps never polls — it is a range read.
  setInterval(async () => {
    if (document.hidden || state.settingsOpen) return;
    if (state.view === "apps") return;
    await Promise.all([refreshCurrent(), refreshPowerSummary()]);
    updateView();
  }, 1000);

  setInterval(() => {
    if (document.hidden || state.settingsOpen) return;
    if (state.view === "apps") return;
    refreshView(false);
  }, 5000);

  setInterval(loadLogs, 15000);
});
