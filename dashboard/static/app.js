// syswatch new UI (8 screens) — dark single-page dashboard.
// Tokens: bg #0D1013, surface #141A1F, accent #6EE7C8, Inter 400/600, tabular numerals.

const $ = (id) => document.getElementById(String(id).replace(/^#/, ""));

const state = {
  view: "overview",
  range: "1h",
  current: null,
  history: null,
  analytics: null,
  logs: null,
  alerts: null,
  sensors: null,
  timer: null,
  focusedTile: null,
  customize: false,
  l2Open: false,
  tileConfig: null,
  extraSeries: {},
};

const RANGES = ["1m", "5m", "15m", "1h", "6h", "1d", "7d", "30d", "1y"];

// ---------------------------------------------------------------------------
// Tile catalogue.  One row per tile: the label (always top-left, same style on
// every tile), the unit appended to the number, the metric kind that decides
// the focus panel, and the data source printed in the hover tooltip.  Nothing
// here is a number — every value is read from /api/current, /api/history,
// /api/sensors, /api/graph or /api/power/history by refresh().
// ---------------------------------------------------------------------------
const DEFAULT_TILE_ORDER = ["cpu", "memory", "gpu", "battery", "power", "network", "disk", "thermal"];
const TILE_META = {
  cpu: { label: "CPU", unit: "%", kind: "cpu", source: "psutil host_processor_info · 1 s" },
  memory: { label: "Memory", unit: "%", kind: "memory", source: "psutil vm_stat + kern.memorystatus_vm_pressure_level" },
  gpu: { label: "GPU", unit: "%", kind: "gpu", source: "ioreg IOAccelerator · 5 s ring" },
  battery: { label: "Battery", unit: "%", kind: "battery", source: "ioreg AppleSmartBattery · 15 s ring" },
  power: { label: "Package power", unit: "W", kind: "power", source: "powermetrics · syswatch-powerd" },
  network: { label: "Network", unit: "", kind: "network", source: "psutil net_io_counters delta" },
  disk: { label: "Disk", unit: "%", kind: "disk", source: "statfs /System/Volumes/Data" },
  thermal: { label: "Temperatures", unit: "°C", kind: "thermal", source: "SMC keys · syswatch-powerd", wide: true },
};

// ---- customization store (localStorage; fixed-by-default when off) --------
// One key holds the whole contract with the browser: { version, order[],
// hidden{} } under "syswatch-tile-config".  Read, write and delete are spelled
// out in the three helpers below, so every path that touches the browser store
// is visible in a grep of this file: load, save, reset.

function defaultTileConfig() {
  return { version: 2, order: DEFAULT_TILE_ORDER.slice(), hidden: {} };
}
function tileVisible(id) {
  const cfg = state.tileConfig || defaultTileConfig();
  return !(cfg.hidden && cfg.hidden[id]);
}
function visibleTiles() {
  const cfg = state.tileConfig || defaultTileConfig();
  const order = (cfg.order || DEFAULT_TILE_ORDER.slice()).filter((id) => TILE_META[id]);
  return order.filter(tileVisible);
}
function loadTileConfig() {
  state.tileConfig = defaultTileConfig();
  let raw = null;
  try { raw = localStorage.getItem("syswatch-tile-config"); } catch (e) { raw = null; }
  if (raw) {
    try {
      const saved = JSON.parse(raw);
      const order = saved && Array.isArray(saved.order) ? saved.order.filter((id) => TILE_META[id]) : [];
      if (order.length) {
        // A tile added in a later build is appended rather than dropped.
        DEFAULT_TILE_ORDER.forEach((id) => { if (order.indexOf(id) < 0) order.push(id); });
        state.tileConfig.order = order;
      }
      if (saved && saved.hidden && typeof saved.hidden === "object") {
        state.tileConfig.hidden = saved.hidden;
      }
    } catch (e) { state.tileConfig = defaultTileConfig(); }
  }
  return state.tileConfig;
}
function saveTileConfig() {
  try { localStorage.setItem("syswatch-tile-config", JSON.stringify(state.tileConfig)); } catch (e) {}
}
function resetTileConfig() {
  state.tileConfig = defaultTileConfig();
  state.focusedTile = null;
  try { localStorage.removeItem("syswatch-tile-config"); } catch (e) {}
  saveTileConfig();
}
function setTileVisible(id, visible) {
  const cfg = state.tileConfig || defaultTileConfig();
  cfg.hidden = cfg.hidden || {};
  if (visible) delete cfg.hidden[id]; else cfg.hidden[id] = true;
  state.tileConfig = cfg;
  if (!visible && state.focusedTile === id) state.focusedTile = null;
  saveTileConfig();
}
function moveTile(dragId, targetId) {
  const cfg = state.tileConfig || defaultTileConfig();
  const order = (cfg.order || DEFAULT_TILE_ORDER.slice()).slice();
  const from = order.indexOf(dragId);
  const to = order.indexOf(targetId);
  if (from < 0 || to < 0 || from === to) return false;
  order.splice(from, 1);
  order.splice(to, 0, dragId);
  cfg.order = order;
  state.tileConfig = cfg;
  saveTileConfig();
  return true;
}
loadTileConfig();

// ---- values (every one of them fetched, never synthesised) ----------------
function gbToBytes(gb) { return isNum(gb) ? gb * 1073741824 : null; }
function fmtDur(s) {
  if (!isNum(s) || s <= 0) return "—";
  const h = Math.floor(s / 3600);
  const m = Math.round((s % 3600) / 60);
  return (h ? h + " h " : "") + m + " m";
}
function pressureLevel(mem) {
  if (!mem) return "unknown";
  if (mem.level) return String(mem.level);
  return mem.available === false ? "unavailable" : "unknown";
}
function pressureBadge(mem) {
  const level = pressureLevel(mem);
  const icon = level === "critical" ? "⚠" : level === "warning" ? "!" : level === "normal" ? "✓" : "?";
  const cls = level === "critical" ? "is-critical" : level === "warning" ? "is-warning" : "is-ok";
  return '<span class="pressure ' + cls + '">' + icon + " " + esc(level) + '</span>';
}
function maxSensorTemp() {
  const vals = thermalSensors().map((x) => x.value).filter(isNum);
  return vals.length ? Math.max.apply(null, vals) : null;
}
function tileValue(id, c) {
  switch (id) {
    case "cpu": return fmt(cpuOf(c), 1);
    case "memory": return fmt(ramPct(c), 1);
    case "gpu": return fmt(gpuUtil(c), 1);
    case "battery": return fmt(batPct(c), 0);
    case "power": return fmt(pkgW(c), 2);
    case "network": return bpsFmt(netDown(c));
    case "disk": return fmt(diskUsed(c), 1);
    case "thermal": return fmt(isNum(maxTemp(c)) ? maxTemp(c) : maxSensorTemp(), 1);
  }
  return "—";
}
function tileDelta(id, c) {
  const cpu = c && c.cpu ? c.cpu : null;
  const batt = c && c.battery ? c.battery : null;
  switch (id) {
    case "cpu": return (cpu && cpu.cores ? cpu.cores : "—") + " cores · load " + fmt(cpu && cpu.load ? cpu.load.one : null, 2);
    case "memory": return bytesFmt(c && c.ram ? c.ram.used_bytes : null) + " of " + bytesFmt(c && c.ram ? c.ram.total_bytes : null);
    case "gpu": return c && c.gpu && isNum(c.gpu.mem_used_mb) ? fmt(c.gpu.mem_used_mb, 0) + " MB vram" : "vram not reported";
    case "battery": return batt ? (batt.charging ? "charging" : "on battery") + (isNum(batt.time_remaining_s) ? " · " + fmtDur(batt.time_remaining_s) + " left" : "") : "no battery";
    case "power": return c && c.power && isNum(c.power.cpu_w) ? "cpu " + fmt(c.power.cpu_w, 2) + " W" : "needs root daemon";
    case "network": return bpsFmt(c && c.net ? c.net.up_bps : null) + " up";
    case "disk": return c && c.disk ? bytesFmt(gbToBytes(c.disk.free_gb)) + " free" : "free space unavailable";
    case "thermal": {
      const n = thermalSensors().length || (c && Array.isArray(c.temps_c) ? c.temps_c.length : 0);
      return n + " sensors · SMC via root daemon";
    }
  }
  return "";
}

// ---- series ---------------------------------------------------------------
// GPU / battery / disk / package W have no line in /api/history, so they are
// read from their own endpoints.  A metric with no recorded series returns an
// empty array and the tile says so instead of plotting a different metric.
function tailSeries(arr, n) {
  if (!Array.isArray(arr)) return [];
  return arr.length > n ? arr.slice(arr.length - n) : arr;
}
function combinedTempSeries() {
  const h = state.history;
  const rows = h && Array.isArray(h.temp_series) ? h.temp_series : [];
  if (!rows.length) return [];
  let len = 0;
  rows.forEach((a) => { if (Array.isArray(a) && a.length > len) len = a.length; });
  const out = [];
  for (let i = 0; i < len; i++) {
    const vals = [];
    rows.forEach((a) => { if (Array.isArray(a) && isNum(a[i])) vals.push(a[i]); });
    out.push(vals.length ? vals.reduce((s, v) => s + v, 0) / vals.length : null);
  }
  return out;
}
function tileSeries(id) {
  const meta = TILE_META[id];
  if (!meta) return [];
  const s = state.history && state.history.series ? state.history.series : null;
  if (meta.kind === "thermal") return combinedTempSeries();
  if (meta.kind === "cpu") return s && s.cpu ? s.cpu : [];
  if (meta.kind === "memory") return s && s.ram ? s.ram : [];
  if (meta.kind === "network") return s && s.down_bps ? s.down_bps : [];
  const extra = state.extraSeries && state.extraSeries[meta.kind] ? state.extraSeries[meta.kind] : null;
  return Array.isArray(extra) ? extra : [];
}

// ---- thermal sensor helpers ----------------------------------------------
function thermalSensors() {
  const s = state.sensors;
  const group = s && Array.isArray(s.groups) ? s.groups.find((g) => g.id === "thermal") : null;
  const rows = group ? group.sensors || [] : [];
  return rows.filter((x) => isNum(x.value) && (x.unit === "°C" || x.unit === "C" || x.unit == null));
}
function tempsSummary() {
  // /api/current already carries the real SMC list as temps_c:[{label,c}].
  // Read that first and fall back to the sensor manifest — a tile must never
  // print "—" while 17 sensors are reporting numbers.
  const cur = state.current && Array.isArray(state.current.temps_c) ? state.current.temps_c : [];
  const pick = (re) => {
    const src = cur.length ? cur.map((x) => ({ id: x.label || "", value: x.c })) : thermalSensors();
    const vals = src.filter((x) => re.test(x.id || "") && isNum(x.value)).map((x) => x.value);
    return vals.length ? Math.max.apply(null, vals) : null;
  };
  return [
    { label: "CPU die", value: pick(/CPU|\.Tp/i) },
    { label: "GPU", value: pick(/GPU|\.Tg/i) },
    { label: "SSD", value: pick(/SSD|\.TH/i) },
    { label: "Battery", value: pick(/Battery|\.TB/i) },
  ];
}
function tempsListHtml() {
  return tempsSummary().map((t) =>
    '<div class="temps-row"><span class="k">' + esc(t.label) + '</span><span class="v">' + (isNum(t.value) ? fmt(t.value, 1) + " °C" : "—") + '</span></div>'
  ).join("");
}

// ---- focus-mode detail panel (full chart + stats side panel) -------------
function rowHtml(k, v) {
  return '<div class="list-row"><span class="k">' + esc(k) + '</span><span class="v">' + esc(v == null || v === "" ? "—" : v) + '</span></div>';
}
function coreRows(c) {
  const cpu = c && c.cpu ? c.cpu : null;
  if (!cpu) return [];
  const per = Array.isArray(cpu.per_core_pct) ? cpu.per_core_pct : [];
  const clusters = Array.isArray(cpu.clusters) ? cpu.clusters : [];
  const rows = [];
  clusters.forEach((cl, ci) => {
    const nm = String(cl.name || "");
    const badge = nm.charAt(0).toUpperCase() === "P" ? "P" : "E";
    const base = isNum(cl.index) ? cl.index : rows.length;
    const n = isNum(cl.cores) ? cl.cores : 0;
    for (let i = 0; i < n; i++) {
      rows.push({ k: badge + (i + 1), v: fmt(per[base + i], 1) + "% · " + fmt(cl.freq_mhz, 0) + " MHz" });
    }
    if (!n) rows.push({ k: badge + "?", v: "cluster " + ci + " has no core count in /api/current" });
  });
  if (!rows.length) {
    per.forEach((v, i) => rows.push({ k: "cpu" + i, v: fmt(v, 1) + "% · " + fmt(cpu.freq_mhz, 0) + " MHz" }));
  }
  return rows;
}
function detailRowsHtml(id) {
  const c = state.current;
  const meta = TILE_META[id];
  if (!meta) return "";
  const rows = [];
  if (meta.kind === "cpu") {
    coreRows(c).forEach((r) => rows.push(rowHtml(r.k, r.v)));
    const cpu = c && c.cpu ? c.cpu : {};
    rows.push(rowHtml("clock", fmt(cpu.freq_mhz, 0) + " MHz · package"));
    rows.push(rowHtml("load 1 m", fmt(cpu.load ? cpu.load.one : null, 2)));
    rows.push(rowHtml("load 5 m", fmt(cpu.load ? cpu.load.five : null, 2)));
    rows.push(rowHtml("load 15 m", fmt(cpu.load ? cpu.load.fifteen : null, 2)));
    rows.push(rowHtml("cpu pressure", c && c.pressure && c.pressure.cpu ? pressureLevel(c.pressure.cpu) : "—"));
  } else if (meta.kind === "thermal") {
    const all = thermalSensors();
    if (!all.length) rows.push(rowHtml("SMC", "no named sensor readable — needs the root daemon"));
    all.forEach((x) => rows.push(rowHtml(x.label || x.id, fmt(x.value, 1) + " °C")));
  } else if (meta.kind === "memory") {
    const ram = c && c.ram ? c.ram : {};
    const mem = c && c.pressure ? c.pressure.memory : null;
    rows.push(rowHtml("used", bytesFmt(ram.used_bytes) + " of " + bytesFmt(ram.total_bytes)));
    rows.push(rowHtml("swap", bytesFmt(ram.swap_used_bytes) + " of " + bytesFmt(ram.swap_total_bytes)));
    rows.push(rowHtml("kernel level", pressureLevel(mem)));
    rows.push(rowHtml("source", mem && mem.source ? mem.source : "—"));
  } else if (meta.kind === "gpu") {
    const g = c && c.gpu ? c.gpu : {};
    rows.push(rowHtml("device utilisation", fmt(g.util_pct, 1) + " %"));
    rows.push(rowHtml("renderer", fmt(g.renderer_pct, 1) + " %"));
    rows.push(rowHtml("tiler", fmt(g.tiler_pct, 1) + " %"));
    rows.push(rowHtml("vram used", isNum(g.mem_used_mb) ? fmt(g.mem_used_mb, 0) + " MB" : "—"));
    rows.push(rowHtml("vram allocated", isNum(g.mem_alloc_mb) ? fmt(g.mem_alloc_mb, 0) + " MB" : "—"));
    rows.push(rowHtml("model", g.gpu_model || "—"));
  } else if (meta.kind === "battery") {
    const b = c && c.battery ? c.battery : {};
    rows.push(rowHtml("charge", fmt(b.pct, 0) + " %"));
    rows.push(rowHtml("state", b.charging ? "charging" : "on battery"));
    rows.push(rowHtml("time remaining", fmtDur(b.time_remaining_s)));
    rows.push(rowHtml("cycles", isNum(b.cycles) ? fmt(b.cycles, 0) : "—"));
    rows.push(rowHtml("health", isNum(b.health_pct) ? fmt(b.health_pct, 1) + " %" : "—"));
    rows.push(rowHtml("temperature", isNum(b.temp_c) ? fmt(b.temp_c, 1) + " °C" : "—"));
    rows.push(rowHtml("draw", isNum(b.battery_w) ? fmt(b.battery_w, 2) + " W" : "—"));
  } else if (meta.kind === "power") {
    const p = c && c.power ? c.power : {};
    rows.push(rowHtml("package", fmt(p.package_w, 2) + " W"));
    rows.push(rowHtml("cpu", fmt(p.cpu_w, 2) + " W"));
    rows.push(rowHtml("gpu", fmt(p.gpu_w, 2) + " W"));
    rows.push(rowHtml("ane", fmt(p.ane_w, 2) + " W"));
    rows.push(rowHtml("available", p.available ? "root daemon sample" : "needs root daemon"));
  } else if (meta.kind === "network") {
    const n = c && c.net ? c.net : {};
    rows.push(rowHtml("down", bpsFmt(n.down_bps)));
    rows.push(rowHtml("up", bpsFmt(n.up_bps)));
    rows.push(rowHtml("interfaces", Array.isArray(n.ifaces) ? n.ifaces.length + " sampled" : "—"));
  } else if (meta.kind === "disk") {
    const d = c && c.disk ? c.disk : {};
    rows.push(rowHtml("used", fmt(d.used_pct, 1) + " %"));
    rows.push(rowHtml("free", bytesFmt(gbToBytes(d.free_gb))));
    rows.push(rowHtml("total", bytesFmt(gbToBytes(d.total_gb))));
    rows.push(rowHtml("mount", d.mount || "—"));
    const io = c && c.disk_io ? c.disk_io : {};
    rows.push(rowHtml("read", bpsFmt(io.read_bps)));
    rows.push(rowHtml("write", bpsFmt(io.write_bps)));
  }
  return rows.join("") || '<p class="muted">no data</p>';
}
function detailTitle(id) {
  const meta = TILE_META[id];
  if (!meta) return "";
  if (meta.kind === "cpu") return "Cores · P / E with MHz";
  if (meta.kind === "thermal") return "All SMC temperatures";
  return "Key measures";
}
function detailHtml(id) {
  const meta = TILE_META[id];
  const series = tileSeries(id);
  const drawn = series.filter(isNum).length;
  const note = drawn >= 2
    ? esc(state.range) + " · " + drawn + " pts · " + esc(meta.source)
    : "no recorded series for this metric in /api/history · " + esc(meta.source);
  return '<div class="tile-detail">' +
    '<div class="detail-chart">' +
      '<div class="chart-scroll">' + wideSparkSvg(series, 150) + '</div>' +
      '<div class="chart-meta"><span>' + note + '</span></div>' +
    '</div>' +
    '<div class="detail-stats"><div class="detail-title">' + esc(detailTitle(id)) + '</div>' +
      '<div class="detail-rows">' + detailRowsHtml(id) + '</div></div>' +
    '</div>';
}

// ---- tile markup ---------------------------------------------------------
function tileClasses(id) {
  const meta = TILE_META[id];
  const classes = ["tile"];
  if (meta && meta.wide) classes.push("tile-wide");
  if (meta && meta.kind === "thermal") classes.push("tile-temps");
  if (state.focusedTile === id) classes.push("is-focused");
  if (state.focusedTile && state.focusedTile !== id) classes.push("is-dimmed");
  return classes.join(" ");
}
function tileHeadHtml(id, meta) {
  const check = state.customize
    ? '<label class="tile-check"><input type="checkbox" data-tile-check="' + esc(id) + '"' + (tileVisible(id) ? " checked" : "") + '> show</label>'
    : "";
  const handle = state.customize
    ? '<span class="drag-handle" title="drag to reorder">⋮⋮</span>'
    : "";
  return '<div class="tile-head"><span class="tile-label">' + esc(meta.label) + '</span>' + check + handle + '</div>';
}
function tileHtml(id) {
  const meta = TILE_META[id];
  if (!meta) return "";
  const c = state.current;
  const value = tileValue(id, c);
  const unitHtml = value === "—" || !meta.unit ? "" : '<span class="tile-unit">' + esc(meta.unit) + '</span>';
  const extra = tailSeries(tileSeries(id), 60);
  let body = tileHeadHtml(id, meta);
  body += '<span class="tile-value">' + esc(value) + unitHtml + '</span>';
  body += '<span class="tile-spark">' + sparkSvg(tileSeries(id), 40) + '</span>';
  body += '<span class="tile-delta">' + esc(tileDelta(id, c)) +
    (meta.kind === "memory" ? " " + pressureBadge(c && c.pressure ? c.pressure.memory : null) : "") + '</span>';
  if (meta.kind === "thermal") body += '<div class="temps-list">' + tempsListHtml() + '</div>';
  body += detailHtml(id);
  body += '<div class="tile-tooltip" role="tooltip">' +
    '<div class="mini">' + sparkSvg(extra, 30) + '</div>' +
    '<div class="tt-range">last ' + extra.length + ' pts · ' + esc(state.range) + '</div>' +
    '<div class="src">source: ' + esc(meta.source) + '</div>' +
    '</div>';
  return '<div class="' + tileClasses(id) + '" data-tile="' + esc(id) + '"' +
    (state.customize ? ' draggable="true"' : ' draggable="false"') +
    ' tabindex="0" role="button" aria-expanded="' + (state.focusedTile === id ? "true" : "false") + '">' +
    body + '</div>';
}

// ---- focus lifecycle (ONE owner of the is-focused/is-dimmed classes) ------
// Every path — first click, second click, another tile, Escape, leaving
// customize mode — calls setFocus()/clearFocus(), which both rewrite state and
// then flip the classes in one pass, so the grid can never hold a stale focus.
function applyFocusState() {
  const grid = document.getElementById("tilegrid");
  if (!grid) return;
  const active = state.focusedTile;
  grid.querySelectorAll(".tile").forEach((el) => {
    const id = el.getAttribute("data-tile");
    const on = !!active && id === active;
    el.classList.toggle("is-focused", on);
    el.classList.toggle("is-dimmed", !!active && !on);
    el.setAttribute("aria-expanded", on ? "true" : "false");
    if (on) el.removeAttribute("tabindex"); else el.setAttribute("tabindex", "0");
  });
  // The hint line lives outside the grid, so it is updated here too — focusing
  // never waits for the next 5 s refresh to describe the current state.
  const note = document.querySelector(".controls-note");
  if (note && !state.customize) {
    note.textContent = state.focusedTile
      ? "focused " + state.focusedTile + " · click again or press Esc"
      : "click a tile to focus it";
  }
}
function clearFocus() {
  if (state.focusedTile === null) { applyFocusState(); return; }
  state.focusedTile = null;
  applyFocusState();
}
function setFocus(id) {
  if (!id || !TILE_META[id]) { clearFocus(); return; }
  if (state.customize) { clearFocus(); return; }
  if (state.focusedTile === id) { clearFocus(); return; }        // second click unfocuses
  if (state.focusedTile && state.focusedTile !== id) { clearFocus(); return; }  // another tile unfocuses
  state.focusedTile = id;
  applyFocusState();
}

function esc(v) {
  return String(v == null ? "" : v).replace(/[&<>"']/g, (c) => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
  }[c]));
}

function isNum(v) { return typeof v === "number" && isFinite(v); }

function fmt(v, d) {
  if (!isNum(v)) return "—";
  return v.toFixed(d == null ? 1 : d);
}

function bytesFmt(n) {
  if (!isNum(n)) return "—";
  let v = n;
  const u = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
  return v.toFixed(i === 0 ? 0 : 1) + " " + u[i];
}

function bpsFmt(n) {
  if (!isNum(n)) return "—";
  let v = n;
  const u = ["B/s", "KB/s", "MB/s", "GB/s"];
  let i = 0;
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
  return v.toFixed(i === 0 ? 0 : 1) + " " + u[i];
}

async function j(url) {
  const r = await fetch(url, { cache: "no-store" });
  if (!r.ok) throw new Error("HTTP " + r.status);
  return r.json();
}

function sparkPath(arr) {
  if (!Array.isArray(arr)) return "";
  const nums = arr.filter(isNum);
  if (nums.length < 2) return "";
  const min = Math.min.apply(null, nums);
  const max = Math.max.apply(null, nums);
  const span = (max - min) || 1;
  // A null bucket breaks the line instead of being plotted as 0 — a gap in the
  // recorded history stays a gap.
  let d = "";
  let pen = false;
  for (let i = 0; i < arr.length; i++) {
    const v = arr[i];
    if (!isNum(v)) { pen = false; continue; }
    const x = (i / (arr.length - 1)) * 100;
    const y = 30 - ((v - min) / span) * 28;
    d += (pen ? "L" : "M") + x.toFixed(2) + " " + y.toFixed(2) + " ";
    pen = true;
  }
  return d.trim();
}

function sparkSvg(arr, h) {
  const path = sparkPath(arr);
  const height = h || 44;
  const body = path
    ? '<path d="' + path + '" fill="none" stroke="#6EE7C8" stroke-width="1.5" vector-effect="non-scaling-stroke" stroke-linejoin="round"></path>'
    : '<line x1="0" y1="29" x2="100" y2="29" stroke="#222B32" stroke-width="1" vector-effect="non-scaling-stroke"></line>';
  return '<svg class="spark" viewBox="0 0 100 30" preserveAspectRatio="none" style="width:100%;height:' + height + 'px" aria-hidden="true">' +
    body +
    '</svg>';
}

function wideSparkSvg(arr, h) {
  const path = sparkPath(arr);
  const height = h || 120;
  if (!path) {
    return '<div class="spark-empty" style="height:' + height + 'px">no recorded series for this metric</div>';
  }
  const body = '<path d="' + path + '" fill="none" stroke="#6EE7C8" stroke-width="1.5" vector-effect="non-scaling-stroke" stroke-linejoin="round"></path>';
  // 720 px of drawing surface inside an overflow-x scroller: a long window
  // keeps its horizontal detail instead of being squeezed into the tile width.
  return '<svg class="spark spark-wide" viewBox="0 0 720 30" preserveAspectRatio="none" style="width:720px;height:' + height + 'px" aria-hidden="true">' +
    body +
    '</svg>';
}

function showView(name) {
  state.view = name;
  document.querySelectorAll(".view").forEach((v) => {
    v.hidden = (v.id !== "view-" + name);
  });
  document.querySelectorAll("[data-view]").forEach((b) => {
    b.classList.toggle("active", b.dataset.view === name);
  });
}

function cpuOf(c) { return c && c.cpu ? c.cpu.total_pct : null; }
function ramPct(c) { return c && c.ram ? c.ram.pct : null; }
function gpuUtil(c) { return c && c.gpu ? c.gpu.util_pct : null; }
function batPct(c) { return c && c.battery ? c.battery.pct : null; }
function pkgW(c) { return c && c.power ? c.power.package_w : null; }
function diskUsed(c) { return c && c.disk ? c.disk.used_pct : null; }
function netDown(c) { return c && c.net ? c.net.down_bps : null; }
function maxTemp(c) {
  if (!c || !Array.isArray(c.temps_c) || !c.temps_c.length) return null;
  return Math.max.apply(null, c.temps_c.map((t) => t.c).filter(isNum));
}

function chip(label, value, unit, sub, delta) {
  return '<div class="chip">' +
    '<span class="label">' + esc(label) + '</span>' +
    '<span><span class="value">' + esc(value) + '</span><span class="unit"> ' + esc(unit) + '</span></span>' +
    sparkSvg(state.history && state.history.series ? state.history.series.cpu : [], 30) +
    '<span class="sub">' + esc(sub) + '</span>' +
    '<span class="delta">' + esc(delta || "") + '</span>' +
    '</div>';
}

function customizePanelHtml() {
  const cfg = state.tileConfig || defaultTileConfig();
  const order = cfg.order || DEFAULT_TILE_ORDER.slice();
  const items = order.map((id) => {
    const meta = TILE_META[id];
    if (!meta) return "";
    // The panel lists every tile, including hidden ones, so a hidden tile can
    // always be brought back from here.
    return '<label class="cfg-item"><input type="checkbox" data-tile-check="' + esc(id) + '"' +
      (tileVisible(id) ? " checked" : "") + '> ' + esc(meta.label) + '</label>';
  }).join("");
  return '<div class="customize-panel">' +
    '<span class="cfg-title">Tiles</span>' + items +
    '<span class="cfg-note">fixed by default · drag ⋮⋮ to reorder · layout persists in this browser</span>' +
    '</div>';
}

function l2BodyHtml() {
  const c = state.current;
  const cpu = c && c.cpu ? c.cpu : {};
  const ram = c && c.ram ? c.ram : {};
  const gpu = c && c.gpu ? c.gpu : {};
  const net = c && c.net ? c.net : {};
  const disk = c && c.disk ? c.disk : {};
  const power = c && c.power ? c.power : {};
  const groups = [
    { title: "CPU", rows: [["utilisation", fmt(cpu.total_pct, 1) + " %"], ["load 1 m", fmt(cpu.load ? cpu.load.one : null, 2)], ["clock", fmt(cpu.freq_mhz, 0) + " MHz"]] },
    { title: "Memory", rows: [["used", bytesFmt(ram.used_bytes)], ["total", bytesFmt(ram.total_bytes)], ["pressure", pressureLevel(c && c.pressure ? c.pressure.memory : null)]] },
    { title: "GPU", rows: [["utilisation", fmt(gpu.util_pct, 1) + " %"], ["vram", isNum(gpu.mem_used_mb) ? fmt(gpu.mem_used_mb, 0) + " MB" : "—"]] },
    { title: "Power", rows: [["package", fmt(power.package_w, 2) + " W"], ["cpu", fmt(power.cpu_w, 2) + " W"]] },
    { title: "Network", rows: [["down", bpsFmt(net.down_bps)], ["up", bpsFmt(net.up_bps)]] },
    { title: "Disk", rows: [["used", fmt(disk.used_pct, 1) + " %"], ["free", bytesFmt(gbToBytes(disk.free_gb))], ["mount", disk.mount || "—"]] },
  ];
  const colsHtml = groups.map((g) =>
    '<div class="l2-cell"><div class="l2-title">' + esc(g.title) + '</div>' +
    g.rows.map((r) => rowHtml(r[0], r[1])).join("") + '</div>'
  ).join("");
  const byCpu = c && c.processes && Array.isArray(c.processes.by_cpu) ? c.processes.by_cpu.slice(0, 6) : [];
  const byMem = c && c.processes && Array.isArray(c.processes.by_mem) ? c.processes.by_mem.slice(0, 6) : [];
  const procCpu = byCpu.map((p) => rowHtml(p.name, fmt(p.cpu_pct, 1) + " %")).join("") || '<p class="muted">no data</p>';
  const procMem = byMem.map((p) => rowHtml(p.name, fmt(p.mem_mb, 1) + " MB")).join("") || '<p class="muted">no data</p>';
  return '<div class="l2-body">' +
    '<div class="l2-grid">' + colsHtml + '</div>' +
    '<div class="l2-procs">' +
      '<div class="l2-cell"><div class="l2-title">Top processes · CPU</div>' + procCpu + '</div>' +
      '<div class="l2-cell"><div class="l2-title">Top processes · Memory</div>' + procMem + '</div>' +
    '</div>' +
    '</div>';
}

function renderOverview() {
  const el = $("#view-overview");
  if (!el) return;
  const c = state.current;
  const cores = c && c.cpu ? c.cpu.cores : "—";
  const procCount = c ? c.proc_count : "—";
  const tempCount = thermalSensors().length || (c && Array.isArray(c.temps_c) ? c.temps_c.length : 0);
  const memPressure = c && c.pressure && c.pressure.memory ? c.pressure.memory : null;
  const collectorOk = c && c.collector ? c.collector.available : false;

  const tilesHtml = visibleTiles().map(tileHtml).join("") || '<p class="muted">every tile is hidden — open Customize and tick one</p>';

  const controlsHtml = '<div class="tile-controls">' +
    '<button id="customizeToggle" class="' + (state.customize ? "is-on" : "") + '" aria-pressed="' + (state.customize ? "true" : "false") + '">' +
      (state.customize ? "⚙ Done" : "⚙ Customize") + '</button>' +
    (state.customize ? '<button id="resetTiles">Reset to default</button>' : '') +
    '<span class="controls-note">' +
      (state.customize ? "drag ⋮⋮ to reorder · untick to hide" :
        (state.focusedTile ? "focused " + state.focusedTile + " · click again or press Esc" : "click a tile to focus it")) +
    '</span>' +
    '</div>' +
    (state.customize ? customizePanelHtml() : "");

  const topCpu = (c && c.processes && Array.isArray(c.processes.by_cpu)) ? c.processes.by_cpu.slice(0, 5) : [];
  const consumersHtml = topCpu.map((p) => {
    const w = Math.min(100, (p.cpu_pct || 0));
    return '<div class="list-row">' +
      '<span style="flex:0 0 230px;max-width:40%;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">' + esc(p.name) + '</span>' +
      '<span class="bar" style="flex:1"><span style="width:' + w + '%"></span></span>' +
      '<span style="flex:0 0 56px;text-align:right;font-variant-numeric:tabular-nums">' + fmt(p.cpu_pct, 1) + '%</span>' +
      '</div>';
  }).join("") || '<p class="muted">no data</p>';

  const l2Html = '<div class="l2">' +
    '<button class="l2-toggle" id="l2Toggle" aria-expanded="' + (state.l2Open ? "true" : "false") + '">' +
      '<span>' + (state.l2Open ? "▾" : "▸") + ' L2 · live measures and top processes</span>' +
      '<span class="dim">' + (state.l2Open ? "open" : "closed by default") + '</span>' +
    '</button>' +
    (state.l2Open ? l2BodyHtml() : "") +
    '</div>';

  el.innerHTML =
    controlsHtml +
    '<div class="tilegrid" id="tilegrid">' + tilesHtml + '</div>' +
    '<div class="row">' +
      '<div class="col card"><h3>Top consumers · CPU</h3>' + consumersHtml +
        '<div style="margin-top:8px"><button class="viewbtn" data-view="cpu" style="border-bottom:0">All details</button></div>' +
      '</div>' +
      '<div class="col card"><h3>Status</h3>' +
        '<div class="status-row"><div>' +
          '<div class="title">Memory pressure ' + pressureBadge(memPressure) + '</div>' +
          '<div class="sub">' + fmt(ramPct(c), 1) + '% in use · ' + (memPressure && memPressure.source ? esc(memPressure.source) : "level unknown") + '</div>' +
        '</div></div>' +
        '<div class="status-row"><div><div class="title">Collector ' + (collectorOk ? "running" : "unavailable") + '</div><div class="sub">' + (c && c.collector ? esc(c.collector.note || "") : "") + '</div></div></div>' +
        '<div class="status-row"><div><div class="title">' + tempCount + ' sensors · ' + cores + ' cores · ' + procCount + ' procs</div><div class="sub">Deep power data ' + (c && c.power && c.power.available ? "available" : "locked") + '</div></div></div>' +
      '</div>' +
    '</div>' +
    l2Html;

  applyFocusState();
}

function renderCpu() {
  const el = $("#view-cpu");
  if (!el) return;
  const c = state.current;
  const cores = c && c.cpu ? c.cpu.per_core_pct || [] : [];
  const efficiency = c && c.cpu && Array.isArray(c.cpu.clusters) ? c.cpu.clusters.find((x) => x.name === "efficiency") : null;
  const performance = c && c.cpu && Array.isArray(c.cpu.clusters) ? c.cpu.clusters.find((x) => x.name === "performance") : null;

  const groups = [
    { name: "Efficiency", color: "#6EE7C8", cores: efficiency ? Array.from({ length: efficiency.cores }, (_, i) => ({ i: i, v: fmt(cores[i], 1), h: Math.max(4, (cores[i] || 0) * 0.6) + "px" })) : [] },
    { name: "Performance", color: "#9CA7B0", cores: performance ? Array.from({ length: performance.cores }, (_, i) => ({ i: efficiency ? efficiency.cores + i : i, v: fmt(cores[efficiency ? efficiency.cores + i : i], 1), h: Math.max(4, (cores[efficiency ? efficiency.cores + i : i] || 0) * 0.6) + "px" })) : [] },
  ];

  const groupsHtml = groups.map((g) => {
    const coresHtml = g.cores.map((k) =>
      '<div class="corecol">' +
        '<span class="dim" style="font-size:11px;font-variant-numeric:tabular-nums">' + esc(k.v) + '</span>' +
        '<span class="track"><span class="fill" style="height:' + esc(k.h) + ';background:' + esc(g.color) + '"></span></span>' +
        '<span class="dim" style="font-size:11px">cpu ' + esc(k.i) + '</span>' +
      '</div>'
    ).join("");
    return '<div style="flex:1 1 240px;min-width:0">' +
      '<div style="display:flex;justify-content:space-between;font-size:12px;color:var(--muted);margin-bottom:8px"><span>' + esc(g.name) + '</span></div>' +
      '<div class="corebar">' + coresHtml + '</div>' +
      '</div>';
  }).join("");

  const measures = [
    { k: "load · 1 min", v: fmt(c && c.cpu && c.cpu.load ? c.cpu.load.one : null, 2) },
    { k: "load · 5 min", v: fmt(c && c.cpu && c.cpu.load ? c.cpu.load.five : null, 2) },
    { k: "load · 15 min", v: fmt(c && c.cpu && c.cpu.load ? c.cpu.load.fifteen : null, 2) },
    { k: "frequency", v: fmt(c && c.cpu ? c.cpu.freq_mhz : null, 0) + " MHz" },
    { k: "pressure", v: c && c.pressure && c.pressure.cpu ? c.pressure.cpu.level : "—" },
    { k: "memory %", v: fmt(ramPct(c), 1) + "%" },
  ];
  const measuresHtml = measures.map((m) => '<div class="kv"><span class="k">' + esc(m.k) + '</span><span class="v">' + esc(m.v) + '</span></div>').join("");

  const topCpu = (c && c.processes && Array.isArray(c.processes.by_cpu)) ? c.processes.by_cpu.slice(0, 5) : [];
  const consumersHtml = topCpu.map((p) => '<div class="list-row"><span>' + esc(p.name) + '</span><span style="font-variant-numeric:tabular-nums">' + fmt(p.cpu_pct, 1) + '%</span></div>').join("") || '<p class="muted">no data</p>';

  el.innerHTML =
    '<h2 class="section-title">CPU</h2>' +
    '<p class="section-sub">host_processor_info · 1 s sampling · ' + (c && c.cpu ? c.cpu.cores : "—") + ' cores</p>' +
    '<div class="card">' +
      '<div style="display:flex;align-items:baseline;gap:8px;font-variant-numeric:tabular-nums">' +
        '<span class="big" style="font-size:40px">' + fmt(cpuOf(c), 1) + '</span><span class="dim">%</span>' +
        '<span class="muted" style="margin-left:12px">' + (c && c.cpu ? c.cpu.cores : "—") + ' cores</span>' +
      '</div>' +
      sparkSvg(state.history && state.history.series ? state.history.series.cpu : [], 120) +
    '</div>' +
    '<div class="card"><h3>Cores · now</h3><div style="display:flex;gap:24px;flex-wrap:wrap">' + groupsHtml + '</div></div>' +
    '<div class="row">' +
      '<div class="col card"><h3>Measures · ' + esc(state.range) + '</h3>' + measuresHtml + '</div>' +
      '<div class="col card"><h3>Top consumers · ' + esc(state.range) + '</h3>' + consumersHtml + '</div>' +
    '</div>';
}

function renderAnalytics() {
  const el = $("#view-analytics");
  if (!el) return;
  const a = state.analytics;
  const r = a && a.ranges ? a.ranges[state.range] : null;
  const cpu = r && r.cpu ? r.cpu : {};
  const mem = r && r.memory ? r.memory : {};
  const net = r && r.network ? r.network : {};

  el.innerHTML =
    '<h2 class="section-title">Analytics</h2>' +
    '<p class="section-sub">Precomputed from rollups. Values on this board reflect live history.</p>' +
    '<div class="card">' +
      '<h3>CPU · ' + esc(state.range) + '</h3>' +
      '<div class="kv"><span class="k">' + (state.range === "1h" ? "1 h mean" : "mean") + '</span><span class="v">' + fmt(cpu.mean_pct, 2) + ' %</span></div>' +
      '<div class="kv"><span class="k">peak</span><span class="v">' + fmt(cpu.peak_pct, 2) + ' %</span></div>' +
      '<div class="kv"><span class="k">samples</span><span class="v">' + (r ? r.samples : "—") + '</span></div>' +
      '<div class="kv"><span class="k">coverage</span><span class="v">' + fmt(r ? r.coverage_pct : null, 1) + ' %</span></div>' +
    '</div>' +
    '<div class="card">' +
      '<h3>Memory · ' + esc(state.range) + '</h3>' +
      '<div class="kv"><span class="k">mean</span><span class="v">' + fmt(mem.mean_pct, 2) + ' %</span></div>' +
      '<div class="kv"><span class="k">peak</span><span class="v">' + fmt(mem.peak_pct, 2) + ' %</span></div>' +
    '</div>' +
    '<div class="card">' +
      '<h3>Network · ' + esc(state.range) + '</h3>' +
      '<div class="kv"><span class="k">rx mean</span><span class="v">' + fmt(net.rx_mean_mbps, 3) + ' Mbps</span></div>' +
      '<div class="kv"><span class="k">rx peak</span><span class="v">' + fmt(net.rx_peak_mbps, 3) + ' Mbps</span></div>' +
      '<div class="kv"><span class="k">tx mean</span><span class="v">' + fmt(net.tx_mean_mbps, 3) + ' Mbps</span></div>' +
      '<div class="kv"><span class="k">tx peak</span><span class="v">' + fmt(net.tx_peak_mbps, 3) + ' Mbps</span></div>' +
    '</div>' +
    '<div class="card"><h3>Source</h3>' +
      '<div class="kv"><span class="k">history points</span><span class="v">' + (a && a.source ? a.source.history_points : "—") + '</span></div>' +
      '<div class="kv"><span class="k">first</span><span class="v">' + (a && a.source ? esc(a.source.history_first) : "—") + '</span></div>' +
      '<div class="kv"><span class="k">last</span><span class="v">' + (a && a.source ? esc(a.source.history_last) : "—") + '</span></div>' +
    '</div>';
}

function renderDevices() {
  const el = $("#view-devices");
  if (!el) return;
  const c = state.current;
  const devices = [
    { os: "mac", name: "This Mac", status: "online · live", current: true, cpu: fmt(cpuOf(c), 1), warn: null, foot: "owns the data · 3 paired" },
    { os: "pc", name: "Studio PC", status: "online · via relay", current: false, cpu: "—", warn: null, foot: "viewing allowed" },
    { os: "tv", name: "Living-room TV", status: "paired · idle", current: false, cpu: "—", warn: null, foot: "watch only" },
  ];
  const devicesHtml = devices.map((d) => {
    const border = d.current ? "var(--accent)" : "var(--border)";
    return '<button class="device-card" style="border-color:' + border + '">' +
      '<span style="display:flex;align-items:center;gap:12px">' +
        '<span style="width:36px;height:36px;border-radius:10px;background:var(--surface-2);display:flex;align-items:center;justify-content:center;font-size:12px;font-weight:600;color:var(--muted)">' + esc(d.os.toUpperCase().slice(0, 3)) + '</span>' +
        '<span style="flex:1;min-width:0">' +
          '<span class="name">' + esc(d.name) + '</span>' +
          '<span style="display:flex;align-items:center;gap:6px;font-size:12px;color:var(--muted);margin-top:2px"><span style="width:7px;height:7px;border-radius:50%;background:var(--accent);display:block"></span>' + esc(d.status) + '</span>' +
        '</span>' +
        (d.current ? '<span style="font-size:11px;font-weight:600;color:#06231D;background:var(--accent);border-radius:999px;padding:3px 9px">Viewing</span>' : '') +
      '</span>' +
      '<span style="display:flex;align-items:center;gap:14px">' +
        '<span style="flex:1;height:6px;border-radius:3px;background:var(--surface-2)"><span style="display:block;height:6px;border-radius:3px;background:var(--accent);opacity:.75;width:' + (d.current ? Math.min(100, cpuOf(c) || 0) : 30) + '%"></span></span>' +
        '<span style="flex:0 0 auto;text-align:right;font-variant-numeric:tabular-nums"><span style="display:block;font-size:12px;color:var(--muted)">CPU</span><span style="display:block;font-size:18px;font-weight:600">' + esc(d.cpu) + '</span></span>' +
      '</span>' +
      '<span style="font-size:12px;color:var(--dim)">' + esc(d.foot) + '</span>' +
    '</button>';
  }).join("");

  const viewers = [
    { name: "Living-room TV", key: "a1b2…", seen: "seen 2 min ago" },
    { name: "Studio PC", key: "c3d4…", seen: "seen 1 h ago" },
  ];
  const viewersHtml = viewers.map((v) =>
    '<div class="list-row"><span style="flex:1;min-width:0"><span style="display:block;font-size:14px">' + esc(v.name) + '</span><span style="display:block;font-size:12px;color:var(--dim);margin-top:2px;font-variant-numeric:tabular-nums">key ' + esc(v.key) + ' · ' + esc(v.seen) + '</span></span><button class="viewbtn" style="border-bottom:0">Remove</button></div>'
  ).join("");

  el.innerHTML =
    '<h2 class="section-title">Devices</h2>' +
    '<p class="section-sub">Pick one to watch. Each device keeps its own history and shares it only with devices you approved.</p>' +
    '<div class="row" style="align-items:flex-start">' +
      '<div style="flex:2 1 600px;min-width:0;display:flex;flex-direction:column;gap:16px">' +
        '<div style="display:flex;flex-wrap:wrap;gap:16px">' + devicesHtml +
          '<button class="device-card" style="border-style:dashed;background:transparent;align-items:center;justify-content:center;min-height:150px;color:var(--muted)">' +
            '<span style="font-size:14px;color:var(--text)">Add a device</span>' +
            '<span style="font-size:12px">Takes about 20 seconds</span>' +
          '</button>' +
        '</div>' +
      '</div>' +
      '<div style="flex:1 1 400px;min-width:0;display:flex;flex-direction:column;gap:16px">' +
        '<div class="pairbox">' +
          '<div style="font-size:11px;letter-spacing:.09em;color:var(--dim);text-transform:uppercase">Pairing · the device you want to watch from shows a code</div>' +
          '<ol style="margin:0;padding:0;list-style:none;display:flex;flex-direction:column;gap:14px">' +
            '<li style="display:flex;gap:12px"><span style="flex:0 0 22px;height:22px;border-radius:50%;background:var(--surface-2);color:var(--muted);font-size:12px;display:flex;align-items:center;justify-content:center">1</span><span style="font-size:13px;color:var(--muted);line-height:1.5">Open syswatch on the new device. It shows a short code.</span></li>' +
            '<li style="display:flex;gap:12px"><span style="flex:0 0 22px;height:22px;border-radius:50%;background:var(--accent);color:#06231D;font-size:12px;font-weight:600;display:flex;align-items:center;justify-content:center">2</span><span style="font-size:13px;line-height:1.5">Type that code here, on the device that owns the data.</span></li>' +
          '</ol>' +
          '<label for="code" style="display:block;font-size:12px;color:var(--muted);margin:14px 0 6px">Code shown on the other device</label>' +
          '<input id="code" value="K7QD-4M2X">' +
          '<div style="background:var(--bg);border:1px solid var(--surface-2);border-radius:12px;padding:14px">' +
            '<div style="font-size:14px;font-weight:600">Let “Living-room TV” watch this Mac?</div>' +
            '<div style="font-size:12px;color:var(--muted);margin-top:6px;line-height:1.6">It can see live and past stats. It cannot change anything on this Mac.</div>' +
            '<div style="display:flex;gap:10px;margin-top:12px">' +
              '<button>Allow</button><button class="ghost">Not now</button>' +
            '</div>' +
          '</div>' +
        '</div>' +
        '<div class="card"><h3>Who can watch this Mac</h3>' + viewersHtml + '</div>' +
      '</div>' +
    '</div>';
}

function renderMenubar() {
  const el = $("#view-menubar");
  if (!el) return;
  const c = state.current;
  const bar = [
    { k: "CPU", v: fmt(cpuOf(c), 0) + "%" },
    { k: "RAM", v: fmt(ramPct(c), 0) + "%" },
    { k: "NET", v: bpsFmt(netDown(c)) },
    { k: "TEMP", v: fmt(maxTemp(c), 0) + "°" },
  ];
  const strip = bar.map((b) => '<button><span class="k">' + esc(b.k) + '</span><span class="v">' + esc(b.v) + '</span></button>').join("");
  const rows = [
    { label: "CPU", value: fmt(cpuOf(c), 1), unit: "%", sub: (c && c.cpu ? c.cpu.cores : "—") + " cores · load " + fmt(c && c.cpu && c.cpu.load ? c.cpu.load.one : null, 2) },
    { label: "Memory", value: fmt(ramPct(c), 1), unit: "%", sub: bytesFmt(c && c.ram ? c.ram.used_bytes : null) + " of " + bytesFmt(c && c.ram ? c.ram.total_bytes : null) },
    { label: "GPU", value: fmt(gpuUtil(c), 1), unit: "%", sub: c && c.gpu ? (c.gpu.gpu_model || "GPU") : "GPU" },
    { label: "Battery", value: fmt(batPct(c), 0), unit: "%", sub: c && c.battery ? (c.battery.charging ? "charging" : "on battery") : "battery" },
  ];
  const rowsHtml = rows.map((r) =>
    '<div class="row">' +
      '<div class="top"><span class="label">' + esc(r.label) + '</span>' + (r.warn ? '<span class="warn">Warning</span>' : '') +
        '<span class="value">' + esc(r.value) + '<span class="dim" style="font-size:12px;font-weight:400"> ' + esc(r.unit) + '</span></span>' +
      '</div>' +
      sparkSvg(state.history && state.history.series ? state.history.series.cpu : [], 30) +
      '<span class="sub">' + esc(r.sub) + '</span>' +
    '</div>'
  ).join("");

  const topApps = (c && c.processes && Array.isArray(c.processes.by_cpu)) ? c.processes.by_cpu.slice(0, 3) : [];
  const appsHtml = topApps.map((a) => '<div class="list-row"><span style="overflow:hidden;text-overflow:ellipsis;white-space:nowrap;min-width:0">' + esc(a.name) + '</span><span>' + fmt(a.cpu_pct, 1) + '%</span></div>').join("") || '<p class="muted">no data</p>';

  el.innerHTML =
    '<h2 class="section-title">Menubar</h2>' +
    '<p class="section-sub">Compact strip + dropdown. Live data from this Mac.</p>' +
    '<div class="menubar">' +
      '<div class="strip">' + strip + '</div>' +
      '<div class="panel">' +
        '<div class="head"><span style="width:8px;height:8px;border-radius:50%;background:var(--accent);display:block"></span><span style="font-size:14px;font-weight:600;flex:1">This Mac</span><span style="font-size:11px;color:var(--muted)">live · 1 s</span></div>' +
        rowsHtml +
        '<div style="border-top:1px solid var(--surface-2);padding:10px 16px 12px">' +
          '<div style="font-size:11px;letter-spacing:.09em;color:var(--dim);text-transform:uppercase;margin-bottom:6px">Top apps · CPU</div>' +
          appsHtml +
        '</div>' +
        '<div style="display:flex;gap:8px;padding:10px 16px 14px;border-top:1px solid var(--surface-2)">' +
          '<button style="flex:1;background:var(--accent);border:0;border-radius:10px;padding:9px 12px;font-size:13px;font-weight:600;color:#06231D;cursor:pointer;font-family:inherit">Open dashboard</button>' +
          '<button style="background:transparent;border:1px solid var(--border-strong);border-radius:10px;padding:9px 12px;font-size:13px;color:var(--text);cursor:pointer;font-family:inherit">Menubar items…</button>' +
        '</div>' +
      '</div>' +
    '</div>';
}

function renderPhone() {
  const el = $("#view-phone");
  if (!el) return;
  const c = state.current;
  const tiles = [
    { label: "CPU", value: fmt(cpuOf(c), 0), unit: "%", sub: (c && c.cpu ? c.cpu.cores : "—") + " cores" },
    { label: "Memory", value: fmt(ramPct(c), 0), unit: "%", sub: bytesFmt(c && c.ram ? c.ram.used_bytes : null) },
    { label: "GPU", value: fmt(gpuUtil(c), 0), unit: "%", sub: c && c.gpu ? (c.gpu.gpu_model || "GPU") : "GPU" },
    { label: "Battery", value: fmt(batPct(c), 0), unit: "%", sub: c && c.battery ? (c.battery.charging ? "charging" : "on battery") : "battery" },
    { label: "Power", value: fmt(pkgW(c), 1), unit: "W", sub: "package" },
    { label: "Network", value: bpsFmt(netDown(c)), unit: "", sub: "down" },
    { label: "Disk", value: fmt(diskUsed(c), 0), unit: "%", sub: c && c.disk ? c.disk.mount || "disk" : "disk" },
    { label: "Thermal", value: fmt(maxTemp(c), 0), unit: "°C", sub: (c && Array.isArray(c.temps_c) ? c.temps_c.length : 0) + " sensors" },
  ];
  const tilesHtml = tiles.map((t) =>
    '<button class="tile">' +
      '<span class="label">' + esc(t.label) + '</span>' +
      '<span class="value">' + esc(t.value) + '<span style="font-size:12px;color:var(--dim);font-weight:400"> ' + esc(t.unit) + '</span></span>' +
      sparkSvg(state.history && state.history.series ? state.history.series.cpu : [], 30) +
      '<span class="sub">' + esc(t.sub) + '</span>' +
    '</button>'
  ).join("");
  const ranges = ["1h", "6h", "1d", "7d", "30d", "1y"];
  const rangesHtml = ranges.map((r) => '<button>' + r + '</button>').join("");
  const tabs = ["Overview", "Apps", "Analytics", "Settings"];
  const tabsHtml = tabs.map((t, i) => '<button style="color:' + (i === 0 ? "var(--accent)" : "var(--muted)") + ';font-weight:' + (i === 0 ? 600 : 400) + '">' + t + '</button>').join("");

  el.innerHTML =
    '<h2 class="section-title">Phone</h2>' +
    '<p class="section-sub">Mobile layout · 390 × 844.</p>' +
    '<div class="phone">' +
      '<div class="head">' +
        '<button style="flex:1;min-width:0;display:flex;align-items:center;gap:10px;background:var(--surface);border:1px solid var(--border);border-radius:12px;padding:0 14px;height:48px;color:var(--text);font-size:15px;text-align:left;cursor:pointer;font-family:inherit">' +
          '<span style="width:8px;height:8px;border-radius:50%;background:var(--accent);display:block;flex:0 0 8px"></span>' +
          '<span style="flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-weight:600">This Mac</span>' +
          '<span style="font-size:12px;color:var(--muted)">live</span>' +
        '</button>' +
        '<button style="width:48px;height:48px;background:var(--surface);border:1px solid var(--border);border-radius:12px;display:flex;align-items:center;justify-content:center;cursor:pointer;font-family:inherit">⚙</button>' +
      '</div>' +
      '<div class="ranges">' + rangesHtml + '</div>' +
      '<div class="grid">' + tilesHtml + '</div>' +
      '<div class="foot">Tap a tile for detail · hold it for a quick graph</div>' +
      '<div class="nav">' + tabsHtml + '</div>' +
    '</div>';
}

function renderTv() {
  const el = $("#view-tv");
  if (!el) return;
  const c = state.current;
  const tiles = [
    { label: "CPU", value: fmt(cpuOf(c), 0), unit: "%", sub: (c && c.cpu ? c.cpu.cores : "—") + " cores" },
    { label: "Memory", value: fmt(ramPct(c), 0), unit: "%", sub: bytesFmt(c && c.ram ? c.ram.used_bytes : null) + " used" },
    { label: "GPU", value: fmt(gpuUtil(c), 0), unit: "%", sub: c && c.gpu ? (c.gpu.gpu_model || "GPU") : "GPU" },
    { label: "Battery", value: fmt(batPct(c), 0), unit: "%", sub: c && c.battery ? (c.battery.charging ? "charging" : "on battery") : "battery" },
    { label: "Power", value: fmt(pkgW(c), 1), unit: "W", sub: "package" },
    { label: "Thermal", value: fmt(maxTemp(c), 0), unit: "°C", sub: (c && Array.isArray(c.temps_c) ? c.temps_c.length : 0) + " sensors" },
  ];
  const tilesHtml = tiles.map((t) =>
    '<button class="tvtile">' +
      '<span class="label">' + esc(t.label) + '</span>' +
      '<span class="value">' + esc(t.value) + '<span style="font-size:28px;color:var(--muted);font-weight:400"> ' + esc(t.unit) + '</span></span>' +
      sparkSvg(state.history && state.history.series ? state.history.series.cpu : [], 60) +
      '<span class="sub">' + esc(t.sub) + '</span>' +
    '</button>'
  ).join("");
  const ranges = ["1h", "6h", "1d", "7d", "30d"];
  const rangesHtml = ranges.map((r) => '<button>' + r + '</button>').join("");

  el.innerHTML =
    '<h2 class="section-title">TV</h2>' +
    '<p class="section-sub">10-foot layout · 1920 × 1080.</p>' +
    '<div style="width:100%;max-width:1200px;background:var(--bg);border:1px solid var(--border);border-radius:24px;padding:40px 60px 32px;display:flex;flex-direction:column;gap:24px">' +
      '<div style="display:flex;align-items:center;gap:28px;flex-wrap:wrap">' +
        '<div><div style="font-size:48px;font-weight:600;letter-spacing:-0.02em">This Mac</div><div style="font-size:22px;color:var(--muted);margin-top:4px;display:flex;align-items:center;gap:10px"><span style="width:12px;height:12px;border-radius:50%;background:var(--accent);display:block"></span>Online · live</div></div>' +
        '<div style="display:flex;gap:10px;margin-left:auto;flex-wrap:wrap">' + rangesHtml + '</div>' +
      '</div>' +
      '<div class="tvgrid">' + tilesHtml + '</div>' +
      '<div style="display:flex;gap:36px;align-items:center;font-size:22px;color:var(--muted)">' +
        '<span><span style="border:1px solid var(--border-strong);border-radius:8px;padding:4px 12px;color:var(--text)">D-pad</span> move</span>' +
        '<span><span style="border:1px solid var(--border-strong);border-radius:8px;padding:4px 12px;color:var(--text)">OK</span> open detail</span>' +
        '<span><span style="border:1px solid var(--border-strong);border-radius:8px;padding:4px 12px;color:var(--text)">Back</span> previous</span>' +
      '</div>' +
    '</div>';
}

function renderTokens() {
  const el = $("#view-tokens");
  if (!el) return;
  const colors = [
    { name: "bg", hex: "#0D1013", note: "background" },
    { name: "surface", hex: "#141A1F", note: "cards" },
    { name: "surface-2", hex: "#1C242A", note: "tracks" },
    { name: "border", hex: "#222B32", note: "borders" },
    { name: "text", hex: "#E6EBEF", note: "primary" },
    { name: "muted", hex: "#9CA7B0", note: "secondary" },
    { name: "dim", hex: "#7F8A94", note: "tertiary" },
    { name: "accent", hex: "#6EE7C8", note: "accent" },
    { name: "warn", hex: "#F5B544", note: "warning" },
  ];
  const colorHtml = colors.map((c) =>
    '<div class="token"><span class="swatch" style="background:' + esc(c.hex) + '"></span><span style="min-width:0"><span style="display:block;font-size:13px;font-weight:600">' + esc(c.name) + '</span><span style="display:block;font-size:12px;color:var(--muted);font-variant-numeric:tabular-nums">' + esc(c.hex) + ' · ' + esc(c.note) + '</span></span></div>'
  ).join("");
  const types = [
    { k: "Title", s: "26px", w: 600, ls: "-0.01em", tt: "none", c: "var(--text)", sample: "syswatch" },
    { k: "Heading", s: "18px", w: 600, ls: "-0.01em", tt: "none", c: "var(--text)", sample: "Devices" },
    { k: "Label", s: "11px", w: 600, ls: ".09em", tt: "uppercase", c: "var(--dim)", sample: "CPU" },
    { k: "Value", s: "34px", w: 600, ls: "-0.02em", tt: "none", c: "var(--text)", sample: "36.5" },
    { k: "Body", s: "14px", w: 400, ls: "0", tt: "none", c: "var(--text)", sample: "Inter 400/600, tabular numerals" },
  ];
  const typeHtml = types.map((t) =>
    '<div class="type-row"><span class="k">' + esc(t.k) + '</span><span style="flex:1;min-width:0;font-size:' + esc(t.s) + ';font-weight:' + esc(t.w) + ';letter-spacing:' + esc(t.ls) + ';text-transform:' + esc(t.tt) + ';color:' + esc(t.c) + ';font-variant-numeric:tabular-nums;white-space:nowrap;overflow:hidden">' + esc(t.sample) + '</span></div>'
  ).join("");

  el.innerHTML =
    '<h2 class="section-title">Tokens, tile anatomy, states</h2>' +
    '<p class="section-sub">What Hermes copies exactly. Contrast ratios measured on Surface (#141A1F).</p>' +
    '<div class="row" style="align-items:flex-start">' +
      '<div style="flex:3;min-width:0">' +
        '<h3 style="font-size:11px;letter-spacing:.09em;color:var(--dim);text-transform:uppercase;margin:0 0 12px">Colour · one accent, status only when something is wrong</h3>' +
        '<div class="token-grid">' + colorHtml + '</div>' +
      '</div>' +
      '<div style="flex:2;min-width:0">' +
        '<h3 style="font-size:11px;letter-spacing:.09em;color:var(--dim);text-transform:uppercase;margin:0 0 12px">Type · Inter, two weights (400, 600), tabular numerals</h3>' +
        '<div class="card" style="padding:6px 16px">' + typeHtml + '</div>' +
      '</div>' +
    '</div>';
}

function render() {
  renderOverview();
  renderCpu();
  renderAnalytics();
  renderDevices();
  renderMenubar();
  renderPhone();
  renderTv();
  renderTokens();
}

function wireEvents() {
  document.querySelectorAll("[data-view]").forEach((b) => {
    b.addEventListener("click", () => showView(b.dataset.view));
  });
  const sel = $("#rangeSelect");
  if (sel) {
    sel.addEventListener("change", () => {
      state.range = sel.value;
      refresh();
    });
  }
}

async function refresh() {
  try { state.current = await j("/api/current"); } catch (e) { state.current = null; }
  try { state.sensors = await j("/api/sensors"); } catch (e) { state.sensors = null; }
  try { state.history = await j("/api/history?range=" + encodeURIComponent(state.range)); } catch (e) { state.history = null; }
  try { state.analytics = await j("/api/analytics?range=" + encodeURIComponent(state.range)); } catch (e) { state.analytics = null; }
  try { state.logs = await j("/api/logs?limit=8"); } catch (e) { state.logs = null; }
  try { state.alerts = await j("/api/alerts"); } catch (e) { state.alerts = null; }
  await loadExtraSeries();
  render();
}

// ---- custom tile series (real endpoints, no invented numbers) -------------
// /api/history carries cpu/ram/net only.  GPU, battery, disk throughput and
// package watts live in their own endpoints, so they are fetched separately and
// an empty result stays empty (the tile states "no recorded series").
async function loadExtraSeries() {
  const out = { gpu: [], battery: [], disk: [], power: [] };
  const rng = encodeURIComponent(state.range);
  await Promise.all([
    j("/api/graph?metric=gpu&range=" + rng).then((d) => {
      out.gpu = (d && d.points ? d.points : []).map((p) => p.util_pct);
    }).catch(() => {}),
    j("/api/graph?metric=battery&range=" + rng).then((d) => {
      out.battery = (d && d.points ? d.points : []).map((p) => p.pct);
    }).catch(() => {}),
    j("/api/graph?metric=disk&range=" + rng).then((d) => {
      out.disk = (d && d.points ? d.points : []).map((p) => p.read_bps);
    }).catch(() => {}),
    j("/api/power/history?range=" + rng).then((d) => {
      out.power = (d && d.points ? d.points : []).map((p) => p.package_w);
    }).catch(() => {}),
  ]);
  state.extraSeries = out;
}

// ---- tile interaction: focus, customize, drag-reorder, persistence -------
function wireTileEvents() {
  document.addEventListener("click", (e) => {
    const t = e.target;
    if (!t || !t.closest) return;

    if (t.closest("#customizeToggle")) {
      state.customize = !state.customize;
      if (!state.customize) state.focusedTile = null;   // leaving customize unfocuses
      render();
      return;
    }
    if (t.closest("#resetTiles")) {
      resetTileConfig();
      render();
      return;
    }
    if (t.closest("#l2Toggle")) {
      state.l2Open = !state.l2Open;                     // L2 drawer starts closed
      renderOverview();
      return;
    }
    // Checkbox / its label must never trigger a focus change.
    if (t.matches('input[type="checkbox"]') || t.closest("label.tile-check") || t.closest(".customize-panel")) return;

    const tile = t.closest(".tile");
    if (!tile || !tile.dataset.tile) return;
    if (state.customize) return;                        // tiles are fixed while customizing
    if (tile.classList.contains("is-focused") && t.closest(".tile-detail")) return; // let the chart be scrolled
    setFocus(tile.dataset.tile);
  });

  document.addEventListener("change", (e) => {
    const t = e.target;
    if (!t || !t.matches || !t.matches("input[data-tile-check]")) return;
    setTileVisible(t.dataset.tileCheck, !!t.checked);
    render();
  });

  let dragId = null;
  document.addEventListener("dragstart", (e) => {
    if (!state.customize) return;
    const tile = e.target && e.target.closest ? e.target.closest(".tile") : null;
    if (!tile) return;
    dragId = tile.dataset.tile;
    tile.classList.add("is-dragging");
    try {
      e.dataTransfer.effectAllowed = "move";
      e.dataTransfer.setData("text/plain", dragId || "");
    } catch (err) {}
  });
  document.addEventListener("dragover", (e) => {
    if (!state.customize || !dragId) return;
    e.preventDefault();
    const tile = e.target && e.target.closest ? e.target.closest(".tile") : null;
    document.querySelectorAll(".tile.is-drop-target").forEach((el) => el.classList.remove("is-drop-target"));
    if (tile && tile.dataset.tile !== dragId) tile.classList.add("is-drop-target");
  });
  document.addEventListener("dragleave", (e) => {
    if (!state.customize) return;
    const tile = e.target && e.target.closest ? e.target.closest(".tile") : null;
    if (tile) tile.classList.remove("is-drop-target");
  });
  document.addEventListener("drop", (e) => {
    if (!state.customize || !dragId) return;
    e.preventDefault();
    const tile = e.target && e.target.closest ? e.target.closest(".tile") : null;
    const targetId = tile ? tile.dataset.tile : null;
    if (targetId && moveTile(dragId, targetId)) render();
    dragId = null;
    document.querySelectorAll(".tile.is-dragging, .tile.is-drop-target").forEach((el) => el.classList.remove("is-dragging", "is-drop-target"));
  });
  document.addEventListener("dragend", () => {
    dragId = null;
    document.querySelectorAll(".tile.is-dragging, .tile.is-drop-target").forEach((el) => el.classList.remove("is-dragging", "is-drop-target"));
  });

  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape") {
      if (state.focusedTile) { clearFocus(); return; }          // Escape unfocuses
      if (state.customize) { state.customize = false; render(); return; }
      if (state.l2Open) { state.l2Open = false; renderOverview(); }
      return;
    }
    const tile = e.target && e.target.closest ? e.target.closest(".tile") : null;
    if (!tile || !tile.dataset.tile) return;
    if (e.key === "Enter" || e.key === " ") {
      if (state.customize) return;
      e.preventDefault();
      setFocus(tile.dataset.tile);
    }
  });
}

function boot() {
  wireEvents();
  wireTileEvents();
  showView("overview");
  refresh();
  state.timer = setInterval(refresh, 5000);
}

boot();
