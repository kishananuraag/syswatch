// syswatch dashboard — reference UI copy with live data.
const $ = (id) => document.getElementById(String(id).replace(/^#/, ""));

const state = {
  tab: "overview",
  range: "1h",
  current: null,
  history: null,
  analytics: null,
  consumers: null,
  sensors: null,
  thermal: null,
  battery: null,
  focusedTile: null,
  customize: false,
  tileConfig: null,
  deviceMenuOpen: false,
  timer: null,
};

const RANGES = ["1m", "5m", "15m", "1h", "6h", "1d", "7d", "30d", "1y"];
const WIDE_TILES = new Set(["network", "storage", "thermal"]);
const DEFAULT_TILE_ORDER = ["cpu", "memory", "gpu", "battery", "power", "workload", "network", "storage", "thermal"];
const TILE_META = {
  cpu: { label: "Processor", unit: "%", kind: "cpu" },
  memory: { label: "Memory", unit: "%", kind: "memory" },
  gpu: { label: "Graphics", unit: "%", kind: "gpu" },
  battery: { label: "Battery", unit: "%", kind: "battery" },
  power: { label: "Power use", unit: "W", kind: "power" },
  workload: { label: "Workload", unit: "", kind: "workload" },
  network: { label: "Network traffic", unit: "KB/s", kind: "network" },
  storage: { label: "Storage activity", unit: "MB/s", kind: "storage" },
  thermal: { label: "Temperature", unit: "°C", kind: "thermal" },
};

// ---- tile config (localStorage syswatch-tile-config) ----
function defaultTileConfig() { return { version: 2, order: DEFAULT_TILE_ORDER.slice(), hidden: {} }; }
function loadTileConfig() {
  state.tileConfig = defaultTileConfig();
  try {
    const raw = localStorage.getItem("syswatch-tile-config");
    if (raw) {
      const saved = JSON.parse(raw);
      const order = Array.isArray(saved.order) ? saved.order.filter((id) => TILE_META[id]) : [];
      if (order.length) {
        DEFAULT_TILE_ORDER.forEach((id) => { if (order.indexOf(id) < 0) order.push(id); });
        state.tileConfig.order = order;
      }
      if (saved.hidden && typeof saved.hidden === "object") state.tileConfig.hidden = saved.hidden;
    }
  } catch (e) { state.tileConfig = defaultTileConfig(); }
}
function saveTileConfig() { try { localStorage.setItem("syswatch-tile-config", JSON.stringify(state.tileConfig)); } catch (e) {} }
function tileVisible(id) {
  const cfg = state.tileConfig || defaultTileConfig();
  return !(cfg.hidden && cfg.hidden[id]);
}
function visibleTiles() {
  const cfg = state.tileConfig || defaultTileConfig();
  const order = (cfg.order || DEFAULT_TILE_ORDER.slice()).filter((id) => TILE_META[id]);
  return order.filter(tileVisible);
}
loadTileConfig();

// ---- helpers ----
function esc(v) { return String(v == null ? "" : v).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c])); }
function isNum(v) { return typeof v === "number" && isFinite(v); }
function fmt(v, d) { if (!isNum(v)) return "—"; return v.toFixed(d == null ? 1 : d); }
async function j(url) {
  const r = await fetch(url, { cache: "no-store" });
  if (!r.ok) throw new Error("HTTP " + r.status);
  return r.json();
}

function cpuOf(c) { return c && c.cpu ? c.cpu.total_pct : null; }
function ramPct(c) { return c && c.ram ? c.ram.pct : null; }
function gpuUtil(c) { return c && c.gpu ? c.gpu.util_pct : null; }
function batPct(c) { return c && c.battery ? c.battery.pct : (c && c.battery ? c.battery.charge_pct : null); }
function pkgW(c) { return c && c.power ? c.power.package_w : null; }
function diskUsed(c) { return c && c.disk ? c.disk.used_pct : null; }
function netDown(c) { return c && c.net ? c.net.down_bps : null; }
function maxTemp(c) {
  if (c && Array.isArray(c.temps_c) && c.temps_c.length) return Math.max.apply(null, c.temps_c.map((t) => t.c).filter(isNum));
  const s = state.sensors;
  const g = s && Array.isArray(s.groups) ? s.groups.find((x) => x.id === "thermal") : null;
  const rows = g ? (g.sensors || []) : [];
  const vals = rows.map((x) => x.value).filter(isNum);
  return vals.length ? Math.max.apply(null, vals) : null;
}
function storageThroughputMB(c) {
  const r = c && c.disk_io ? c.disk_io.read_bps : null;
  const w = c && c.disk_io ? c.disk_io.write_bps : null;
  if (!isNum(r) && !isNum(w)) return null;
  return ((isNum(r) ? r : 0) + (isNum(w) ? w : 0)) / 1048576;
}
function workloadLoad(c) { return c && c.cpu && c.cpu.load ? c.cpu.load.one : (c && c.load ? c.load.one : null); }
function tileValue(id, c) {
  switch (id) {
    case "cpu": return fmt(cpuOf(c), 1);
    case "memory": return fmt(ramPct(c), 1);
    case "gpu": return fmt(gpuUtil(c), 1);
    case "battery": return fmt(batPct(c), 0);
    case "power": return fmt(pkgW(c), 2);
    case "workload": return fmt(workloadLoad(c), 2);
    case "network": return fmt(isNum(netDown(c)) ? netDown(c) / 1024 : null, 1);
    case "storage": return fmt(storageThroughputMB(c), 1);
    case "thermal": return fmt(maxTemp(c), 1);
  }
  return "—";
}
function rangeWord() {
  const words = {
    "1m": "last minute", "5m": "last 5 min", "15m": "last 15 min", "1h": "last hour",
    "6h": "last 6 hours", "1d": "last day", "7d": "last 7 days", "30d": "last 30 days", "1y": "last year"
  };
  return words[state.range] || "recent average";
}
function pointsVsRange(cur, arr) {
  if (!isNum(cur) || !Array.isArray(arr)) return "";
  const nums = arr.filter(isNum);
  if (nums.length < 3) return "";
  const avg = nums.reduce((a, b) => a + b, 0) / nums.length;
  const d = cur - avg;
  if (Math.abs(d) < 0.5) return "Same as " + rangeWord();
  return fmt(Math.abs(d), 1) + " points " + (d > 0 ? "above " : "below ") + rangeWord();
}
function seriesOf(name) {
  const s = state.history && state.history.series ? state.history.series : null;
  return s && Array.isArray(s[name]) ? s[name] : [];
}
function gpuTemp(c) {
  if (c && Array.isArray(c.temps_c)) {
    const row = c.temps_c.filter((t) => isNum(t.c) && /^GPU/i.test(String(t.label || "")))[0];
    if (row) return row.c;
  }
  if (c && c.gpu && isNum(c.gpu.temp_c)) return c.gpu.temp_c;
  const s = state.thermal;
  const rows = s && Array.isArray(s.sensors) ? s.sensors : [];
  const g = rows.filter((x) => isNum(x.c) && /^GPU/i.test(String(x.label || "")))[0];
  return g ? g.c : null;
}
// real live samples kept client-side for the series the history endpoint does not store
const LIVE = { readMB: [], writeMB: [], gpu: [], battery: [], load: [] };
const LIVE_CAP = 120;
function pushLive(arr, v) {
  arr.push(isNum(v) ? v : null);
  while (arr.length > LIVE_CAP) arr.shift();
}
function pushLiveSeries(c) {
  const io = c && c.disk_io ? c.disk_io : null;
  pushLive(LIVE.readMB, io && isNum(io.read_bps) ? io.read_bps / 1048576 : null);
  pushLive(LIVE.writeMB, io && isNum(io.write_bps) ? io.write_bps / 1048576 : null);
  pushLive(LIVE.gpu, gpuUtil(c));
  pushLive(LIVE.battery, batPct(c));
  pushLive(LIVE.load, workloadLoad(c));
}
function tileSub(id, c) {
  switch (id) {
    case "cpu": return (c && c.cpu && c.cpu.cores ? c.cpu.cores : "—") + " cores";
    case "memory": return fmt(c && c.ram && c.ram.used_bytes ? c.ram.used_bytes / 1073741824 : null, 1) +
      " GB of " + fmt(c && c.ram && c.ram.total_bytes ? c.ram.total_bytes / 1073741824 : null, 1) + " GB in use";
    case "gpu": return "drawing the screen: " + fmt(c && c.gpu && isNum(c.gpu.renderer_pct) ? c.gpu.renderer_pct : null, 0) + " %";
    case "battery": {
      const b = (c && c.battery) || {};
      const api = state.battery || {};
      const tr = isNum(b.time_remaining_min) ? b.time_remaining_min : (isNum(api.time_remaining_min) ? api.time_remaining_min : null);
      let s = tr != null ? Math.floor(tr / 60) + " h " + Math.round(tr % 60) + " min left" : (b.external_connected ? "on adapter" : "on battery");
      if (isNum(b.draw_w)) s += " · using " + fmt(b.draw_w, 1) + " W";
      else if (isNum(b.adapter_w)) s += " · " + fmt(b.adapter_w, 0) + " W";
      return s;
    }
    case "power": return "Processor " + fmt(c && c.power ? c.power.cpu_w : null, 1) + " W" +
      " · Graphics " + fmt(c && c.power ? c.power.gpu_w : null, 1) + " W";
    case "workload": return "5 min " + fmt(c && c.cpu && c.cpu.load ? c.cpu.load.five : null, 2) +
      " · 15 min " + fmt(c && c.cpu && c.cpu.load ? c.cpu.load.fifteen : null, 2);
  }
  return "";
}
function tileDelta(id, c) {
  switch (id) {
    case "cpu": return pointsVsRange(cpuOf(c), seriesOf("cpu"));
    case "memory": return pointsVsRange(ramPct(c), seriesOf("ram"));
    case "gpu": return c && c.gpu && isNum(c.gpu.mem_used_mb) ? fmt(c.gpu.mem_used_mb, 0) + " MB vram" : "vram not reported";
    case "battery": return c && c.battery && isNum(c.battery.health_pct)
      ? "Holds " + fmt(c.battery.health_pct, 0) + " % of new" : "health not reported";
    case "power": {
      const ms = state.thermal && isNum(state.thermal.sample_ms) ? state.thermal.sample_ms : null;
      return ms ? "Updates every " + fmt(ms / 1000, 0) + " seconds" : "root daemon sampling";
    }
    case "workload": return c && c.cpu && c.cpu.cores ? "across " + c.cpu.cores + " cores" : "";
    case "network": return isNum(c && c.net ? c.net.up_bps : null) ? fmt(c.net.up_bps / 1024, 1) + " KB/s up" : "—";
    case "storage": return c && c.disk ? fmt(c.disk.free_gb, 1) + " GB free" : "free space unavailable";
    case "thermal": return (state.sensors && state.sensors.count ? state.sensors.count : "—") + " sensors";
  }
  return "";
}
function tileSeries(id) {
  const h = state.history;
  const s = h && h.series ? h.series : null;
  if (id === "cpu") return s && s.cpu ? s.cpu : [];
  if (id === "memory") return s && s.ram ? s.ram : [];
  if (id === "network") return s && s.down_bps ? s.down_bps : [];
  if (id === "gpu") return LIVE.gpu.slice();
  if (id === "battery") return LIVE.battery.slice();
  if (id === "workload") return LIVE.load.slice();
  if (id === "thermal" && h && Array.isArray(h.temp_series)) {
    const rows = h.temp_series; let len = 0;
    rows.forEach((a) => { if (Array.isArray(a) && a.length > len) len = a.length; });
    const out = [];
    for (let i = 0; i < len; i++) {
      const vals = [];
      rows.forEach((a) => {
        if (Array.isArray(a) && isNum(a[i]) && a[i] > 10 && a[i] < 120) vals.push(a[i]);
      });
      out.push(vals.length ? Math.max.apply(null, vals) : null);
    }
    return out;
  }
  return [];
}
function wideSeries(id) {
  const h = state.history;
  const s = h && h.series ? h.series : null;
  if (id === "network") return [s && s.down_bps ? s.down_bps : [], s && s.up_bps ? s.up_bps : []];
  if (id === "storage") return [LIVE.readMB.slice(), LIVE.writeMB.slice()];
  if (id === "thermal" && h && Array.isArray(h.temp_series)) {
    const rows = h.temp_series;
    const gpuIdx = rows.length > 5 ? 5 : 0;
    const cpuArr = tileSeries("thermal");
    const gpuArr = Array.isArray(rows[gpuIdx]) ? rows[gpuIdx] : [];
    return [cpuArr, gpuArr];
  }
  return [[], []];
}
function timeLabels() {
  const h = state.history;
  const labels = h && Array.isArray(h.labels) ? h.labels : [];
  if (!labels.length) return ["", "", "", "now"];
  const fmt = (iso) => {
    const m = String(iso).match(/T(\d{2}):(\d{2})/);
    return m ? m[1] + ":" + m[2] : String(iso);
  };
  const i1 = 0;
  const i2 = Math.floor(labels.length / 3);
  const i3 = Math.floor((2 * labels.length) / 3);
  return [fmt(labels[i1]), fmt(labels[i2]), fmt(labels[i3]), "now"];
}
function seriesBounds(arrays) {
  const nums = [];
  arrays.forEach((a) => { if (Array.isArray(a)) a.forEach((v) => { if (isNum(v)) nums.push(v); }); });
  if (!nums.length) return null;
  // Robust bounds: one freak sample must not squash the whole line onto the floor.
  const s = nums.slice().sort((x, y) => x - y);
  const at = (p) => s[Math.min(s.length - 1, Math.max(0, Math.round((s.length - 1) * p)))];
  const lo = at(0.02), hi = at(0.98);
  if (!(hi > lo)) return { lo: Math.min.apply(null, nums), hi: Math.max.apply(null, nums) };
  const pad = (hi - lo) * 0.06;
  return { lo: lo - pad, hi: hi + pad };
}
function areaPath(arr, bounds) {
  if (!Array.isArray(arr)) return "";
  const nums = arr.filter(isNum);
  if (nums.length < 2) return "";
  const min = bounds ? bounds.lo : Math.min.apply(null, nums);
  const max = bounds ? bounds.hi : Math.max.apply(null, nums);
  const span = max - min;
  if (!(span > 0.0001)) return "";
  let d = "";
  let run = [];
  const flush = () => {
    if (run.length < 1) return;
    let seg = "M" + run[0].x.toFixed(2) + " " + run[0].y.toFixed(2);
    for (let k = 1; k < run.length; k++) seg += " L" + run[k].x.toFixed(2) + " " + run[k].y.toFixed(2);
    const lastX = run[run.length - 1].x;
    const firstX = run[0].x;
    seg += " L" + lastX.toFixed(2) + " 30 L" + firstX.toFixed(2) + " 30 Z";
    d += seg + " ";
    run = [];
  };
  for (let i = 0; i < arr.length; i++) {
    const v = arr[i];
    if (!isNum(v)) { flush(); continue; }
    const x = (i / (arr.length - 1)) * 100;
    const y = Math.max(0.8, Math.min(29.2, 27 - ((v - min) / span) * 24));
    run.push({ x: x, y: y });
  }
  flush();
  return d.trim();
}
function sparkPath(arr, bounds) {
  if (!Array.isArray(arr)) return "M0 15 L100 15";
  const nums = arr.filter(isNum);
  if (nums.length < 2) return "M0 15 L100 15";
  const min = bounds ? bounds.lo : Math.min.apply(null, nums);
  const max = bounds ? bounds.hi : Math.max.apply(null, nums);
  const span = max - min;
  if (!(span > 0.0001)) return "M0 15 L100 15";
  let d = "", pen = false;
  for (let i = 0; i < arr.length; i++) {
    const v = arr[i];
    if (!isNum(v)) { pen = false; continue; }
    const x = (i / (arr.length - 1)) * 100;
    const y = Math.max(0.8, Math.min(29.2, 27 - ((v - min) / span) * 24));
    d += (pen ? "L" : "M") + x.toFixed(2) + " " + y.toFixed(2) + " ";
    pen = true;
  }
  return d.trim() || "M0 15 L100 15";
}
function sparkSvg(arr, h) {
  const path = sparkPath(arr);
  const height = h || 44;
  return '<svg class="spark" viewBox="0 0 100 30" preserveAspectRatio="none" style="width:100%;height:' + height + 'px" aria-hidden="true">' +
    '<path d="' + path + '" fill="none" stroke="#6EE7C8" stroke-width="1.5" vector-effect="non-scaling-stroke" stroke-linejoin="round"></path>' +
    '</svg>';
}

// ---- range pills ----
function renderRanges() {
  const el = $("#ranges");
  if (!el) return;
  el.innerHTML = RANGES.map((r) =>
    '<button class="range-pill' + (r === state.range ? " active" : "") + '" data-range="' + r + '">' + r + '</button>'
  ).join("");
  el.querySelectorAll("[data-range]").forEach((b) => {
    b.addEventListener("click", () => { state.range = b.dataset.range; refresh(); });
  });
}

// ---- tabs ----
function setTab(name) {
  state.tab = name;
  document.querySelectorAll(".tab").forEach((b) => b.classList.toggle("active", b.dataset.tab === name));
  document.querySelectorAll(".tabview").forEach((v) => { v.hidden = (v.id !== "tab-" + name); });
}
function renderTabs() {
  document.querySelectorAll(".tab").forEach((b) => {
    b.addEventListener("click", () => setTab(b.dataset.tab));
  });
}

// ---- tiles ----
function wideSparkSvg(id) {
  const [a, b] = wideSeries(id);
  const bounds = seriesBounds([a, b]);
  const pathA = sparkPath(a, bounds);
  const pathB = sparkPath(b, bounds);
  const areaA = areaPath(a, bounds);
  const labels = timeLabels();
  return '<svg class="spark" viewBox="0 0 100 30" preserveAspectRatio="none" style="width:100%;height:120px" aria-hidden="true">' +
    '<path d="M0 7.5H100 M0 15H100 M0 22.5H100" fill="none" stroke="#1E272E" stroke-width="1" vector-effect="non-scaling-stroke"></path>' +
    '<path d="' + areaA + '" fill="rgba(110,231,200,0.09)" stroke="none"></path>' +
    '<path d="' + pathB + '" fill="none" stroke="#8A97A3" stroke-width="1.4" vector-effect="non-scaling-stroke" stroke-linejoin="round"></path>' +
    '<path d="' + pathA + '" fill="none" stroke="#6EE7C8" stroke-width="1.5" vector-effect="non-scaling-stroke" stroke-linejoin="round"></path>' +
    '</svg>' +
    '<span class="tile-xaxis">' + labels.map((l) => '<span>' + esc(l) + '</span>').join("") + '</span>';
}
function wideFooter(id, c) {
  if (id === "network") {
    const down = isNum(netDown(c)) ? netDown(c) / 1024 : null;
    const up = isNum(c && c.net ? c.net.up_bps : null) ? c.net.up_bps / 1024 : null;
    return '<span class="tile-footer">' +
      '<span class="series" title="Downloading ' + esc(fmt(down, 1)) + ' KB/s"><span class="swatch acc"></span>Downloading ' + esc(fmt(down, 1)) + ' KB/s</span>' +
      '<span class="series" title="Uploading ' + esc(fmt(up, 1)) + ' KB/s"><span class="swatch l2"></span>Uploading ' + esc(fmt(up, 1)) + ' KB/s</span>' +
      '</span>';
  }
  if (id === "storage") {
    const io = c && c.disk_io ? c.disk_io : {};
    const mb = (v) => (isNum(v) ? v / 1048576 : null);
    const readMB = mb(io.read_bps);
    const writeMB = mb(io.write_bps);
    const u = (v) => (isNum(v) && v >= 1 ? fmt(v, 1) + " MB/s" : fmt(isNum(v) ? v * 1024 : null, 0) + " KB/s");
    return '<span class="tile-footer">' +
      '<span class="series" title="Reading ' + esc(u(readMB)) + '"><span class="swatch acc"></span>Reading ' + esc(u(readMB)) + '</span>' +
      '<span class="series" title="Writing ' + esc(u(writeMB)) + '"><span class="swatch l2"></span>Writing ' + esc(u(writeMB)) + '</span>' +
      '</span>';
  }
  if (id === "thermal") {
    const cpuT = maxTemp(c);
    const heat = (c && c.thermal && c.thermal.thermal_pressure) ? c.thermal.thermal_pressure : (c && c.power ? c.power.thermal_pressure : "normal");
    const gpuT = gpuTemp(c);
    return '<span class="tile-footer">' +
      '<span class="series" title="Processor ' + esc(fmt(cpuT, 1)) + ' °C"><span class="swatch acc"></span>Processor ' + esc(fmt(cpuT, 1)) + ' °C</span>' +
      '<span class="series" title="Graphics ' + esc(fmt(gpuT, 1)) + ' °C · heat level ' + esc(String(heat).toLowerCase()) + '"><span class="swatch l2"></span>Graphics ' + esc(fmt(gpuT, 1)) + ' °C · heat level ' + esc(String(heat).toLowerCase()) + '</span>' +
      '</span>';
  }
  return "";
}
function renderTiles() {
  const el = $("#tiles");
  if (!el) return;
  const c = state.current;
  el.innerHTML = visibleTiles().map((id) => {
    const meta = TILE_META[id];
    const val = tileValue(id, c);
    const unitHtml = val === "—" || !meta.unit ? "" : '<span class="tile-unit">' + esc(meta.unit) + '</span>';
    const wide = WIDE_TILES.has(id);
    const chevron = '<svg class="chev" viewBox="0 0 16 16" width="14" height="14" aria-hidden="true"><path d="M6 3.5L10.5 8 6 12.5" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"></path></svg>';
    if (wide) {
      return '<div class="tile wide" data-tile="' + esc(id) + '" tabindex="0" role="button" aria-expanded="false">' +
        '<div class="tile-head"><span class="tile-label">' + esc(meta.label) + '</span>' + chevron + '</div>' +
        '<button class="tile-help" aria-label="What is ' + esc(meta.label) + '?"><span>?</span></button>' +
        '<span class="tile-value">' + esc(val) + unitHtml + '</span>' +
        '<span class="tile-spark wide">' + wideSparkSvg(id) + '</span>' +
        wideFooter(id, c) +
        '</div>';
    }
    const series = tileSeries(id);
    const sub = tileSub(id, c);
    const delta = tileDelta(id, c);
    return '<div class="tile" data-tile="' + esc(id) + '" tabindex="0" role="button" aria-expanded="false">' +
      '<div class="tile-head"><span class="tile-label">' + esc(meta.label) + '</span></div>' +
      '<button class="tile-help" aria-label="What is ' + esc(meta.label) + '?"><span>?</span></button>' +
      '<span class="tile-value">' + esc(val) + unitHtml + '</span>' +
      '<span class="tile-spark">' + sparkSvg(series, 44) + '</span>' +
      (sub ? '<span class="tile-sub">' + sub.split(" · ").map((part, i) => '<span title="' + esc(part) + '">' + (i ? "· " : "") + esc(part) + '</span>').join("") + '</span>' : '') +
      (delta ? '<span class="tile-delta" title="' + esc(delta) + '">' + esc(delta) + '</span>' : '') +
      '</div>';
  }).join("");
  // focus interaction
  el.querySelectorAll(".tile").forEach((t) => {
    t.addEventListener("click", (e) => {
      if (e.target.closest(".tile-help")) return;
      const id = t.dataset.tile;
      if (state.focusedTile === id) { state.focusedTile = null; }
      else if (!state.focusedTile) { state.focusedTile = id; }
      else { state.focusedTile = null; }
      applyFocus();
    });
  });
}
function applyFocus() {
  const grid = $("#tiles");
  if (!grid) return;
  grid.querySelectorAll(".tile").forEach((el) => {
    const id = el.getAttribute("data-tile");
    el.classList.toggle("is-focused", state.focusedTile === id);
    el.classList.toggle("is-dimmed", !!state.focusedTile && state.focusedTile !== id);
  });
}

// ---- panels ----
function appRowsSorted() {
  const rows = state.consumers && Array.isArray(state.consumers.rows) ? state.consumers.rows : [];
  const c = state.current;
  const cores = (c && c.cpu && isNum(c.cpu.cores) && c.cpu.cores > 0) ? c.cpu.cores : 1;
  const byName = {};
  rows.forEach((r) => {
    const name = r && r.name ? String(r.name) : "unknown";
    if (!byName[name]) byName[name] = { name: name, cpu_pct: 0, procs: 0 };
    byName[name].cpu_pct += isNum(r.cpu_pct) ? r.cpu_pct : 0;
    byName[name].procs += 1;
  });
  return Object.keys(byName).map((k) => {
    const row = byName[k];
    // share of the whole machine, not per-core -- and it can never exceed 100 %
    row.cpu_pct = Math.min(100, row.cpu_pct / cores);
    return row;
  }).sort((x, y) => y.cpu_pct - x.cpu_pct);
}
function appsHtml(rows) {
  if (!rows.length) return '<div class="muted">not available on this agent yet</div>';
  const max = rows.reduce((m, a) => Math.max(m, isNum(a.cpu_pct) ? a.cpu_pct : 0), 0) || 1;
  return rows.map((a) => {
    const v = isNum(a.cpu_pct) ? a.cpu_pct.toFixed(1) + " %" : "—";
    const w = isNum(a.cpu_pct) ? Math.max(2, (a.cpu_pct / max) * 100) : 2;
    return '<div class="approw">' +
      '<span class="appname" title="' + esc(a.name) + '">' + esc(a.name) + '</span>' +
      '<span class="bar"><span style="width:' + Math.min(100, w).toFixed(1) + '%"></span></span>' +
      '<span class="appval">' + esc(v) + '</span>' +
      '</div>';
  }).join("");
}
function renderTopApps() {
  const el = $("#topApps");
  if (!el) return;
  el.innerHTML = appsHtml(appRowsSorted().slice(0, 6));
}
function renderAppsTab() {
  const el = $("#appsTab");
  if (!el) return;
  el.innerHTML = appsHtml(appRowsSorted());
}
function renderStatus() {
  const el = $("#status");
  if (!el) return;
  const c = state.current;
  const mem = c && c.ram ? c.ram : {};
  const cpu = c && c.cpu ? c.cpu : {};
  const sensors = state.sensors;
  const pressure = c && c.pressure && c.pressure.memory ? c.pressure.memory : {};
  const memLevel = pressure.level || (ramPct(c) > 85 ? "warning" : "normal");
  const st1Title = memLevel === "warning" || memLevel === "critical" ? "Memory is getting tight" : "Memory is healthy";
  const st1s = fmt(mem.used_bytes ? mem.used_bytes / 1073741824 : null, 1) + " of " + fmt(mem.total_bytes ? mem.total_bytes / 1073741824 : null, 1) + " GB in use";
  const agentProc = c && c.processes && Array.isArray(c.processes.by_cpu)
    ? c.processes.by_cpu.filter((p) => /syswatch|agent/i.test(String(p.name || "")))[0]
    : null;
  let agentCpu = agentProc && isNum(agentProc.cpu_pct) ? agentProc.cpu_pct : null;
  let agentMem = agentProc && isNum(agentProc.mem_mb) ? agentProc.mem_mb : null;
  if (agentCpu == null) {
    const rows = state.consumers && Array.isArray(state.consumers.rows) ? state.consumers.rows : [];
    const hit = rows.filter((r) => /syswatch|monitor/i.test(String(r.name || "")))[0];
    if (hit) {
      agentCpu = isNum(hit.cpu_pct) ? hit.cpu_pct : null;
      agentMem = isNum(hit.memory_mb) ? hit.memory_mb : (isNum(hit.mem_avg_mb) ? hit.mem_avg_mb : null);
    }
  }
  const st2 = "Monitoring is running";
  const st2s = "uses " + fmt(agentCpu, 1) + " % of the processor" + (agentMem != null ? " · " + fmt(agentMem, 0) + " MB" : "");
  const st3 = (sensors && sensors.count ? sensors.count : "—") + " sensors found";
  const deepLocked = !(c && c.power && c.power.available);
  const st3s = "Deep power readings " + (deepLocked ? "are locked." : "available.");

  // collector log age (real, from the last sample timestamp)
  const lastTs = c && c.ts ? c.ts : null;
  const lastMs = lastTs ? Date.parse(lastTs) : NaN;
  const ageS = isNum(lastMs) ? Math.max(0, Math.round((Date.now() - lastMs) / 1000)) : null;
  const st4 = "Collector log age";
  const st4s = ageS != null ? ageS + " s since the last collector sample" : "no collector sample yet";

  // last sample time
  const st5 = "Last sample time";
  const st5s = lastTs ? String(lastTs) : "not available";

  // disk free
  const disk = c && c.disk ? c.disk : {};
  const st6 = "Disk free";
  const st6s = isNum(disk.free_gb)
    ? fmt(disk.free_gb, 1) + " GB free of " + fmt(disk.total_gb, 1) + " GB"
    : "disk info unavailable";

  // battery health
  const batt = (c && c.battery) || state.battery || {};
  const health = isNum(batt.health_pct) ? batt.health_pct : (state.battery && isNum(state.battery.health_pct) ? state.battery.health_pct : null);
  const cycles = isNum(batt.cycles) ? batt.cycles : (state.battery && isNum(state.battery.cycles) ? state.battery.cycles : null);
  const st7 = "Battery health";
  const st7s = health != null
    ? "Holds " + fmt(health, 1) + " % of its original charge" + (cycles != null ? " · " + fmt(cycles, 0) + " cycles" : "")
    : "health not reported";

  // sensor source
  const therm = state.thermal;
  const st8 = "Sensor source";
  const st8s = (therm && therm.source) ? String(therm.source)
    : (sensors && sensors.platform ? "platform: " + sensors.platform : "source not reported");

  const warnIcon = '<svg class="icon warn" viewBox="0 0 16 16" width="14" height="14" aria-hidden="true"><path d="M8 2.2l6.2 11H1.8z M8 6.6v3 M8 11.6v.1" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linejoin="round" stroke-linecap="round"></path></svg>';
  const dotIcon = '<span class="icon"><span class="sdot"></span></span>';
  el.innerHTML =
    '<div class="statusrow">' + warnIcon + '<div><div class="title warn">' + esc(st1Title) + '</div><div class="sub">' + esc(st1s) + '</div></div></div>' +
    '<div class="statusrow">' + dotIcon + '<div><div class="title">' + esc(st2) + '</div><div class="sub">' + esc(st2s) + '</div></div></div>' +
    '<div class="statusrow">' + dotIcon + '<div><div class="title">' + esc(st3) + '</div><div class="sub">' + esc(st3s) + '</div></div></div>' +
    '<div class="statusrow">' + dotIcon + '<div><div class="title">' + esc(st4) + '</div><div class="sub">' + esc(st4s) + '</div></div></div>' +
    '<div class="statusrow">' + dotIcon + '<div><div class="title">' + esc(st5) + '</div><div class="sub">' + esc(st5s) + '</div></div></div>' +
    '<div class="statusrow">' + dotIcon + '<div><div class="title">' + esc(st6) + '</div><div class="sub">' + esc(st6s) + '</div></div></div>' +
    '<div class="statusrow">' + dotIcon + '<div><div class="title">' + esc(st7) + '</div><div class="sub">' + esc(st7s) + '</div></div></div>' +
    '<div class="statusrow">' + dotIcon + '<div><div class="title">' + esc(st8) + '</div><div class="sub">' + esc(st8s) + '</div></div></div>';
}
function renderAnalytics() {
  const el = $("#analyticsTab");
  if (!el) return;
  const a = state.analytics;
  if (!a || !Array.isArray(a.measures)) { el.innerHTML = '<div class="muted">not available on this agent yet</div>'; return; }
  const findings = Array.isArray(a.findings) ? a.findings : [];
  const measures = a.measures.filter((m) => m.available);
  let html = measures.map((m) =>
    '<div class="measure"><span>' + esc(m.label) + '</span><span>' + fmt(m.avg, 2) + " " + esc(m.unit) + ' avg · max ' + fmt(m.max, 2) + '</span></div>'
  ).join("");
  if (findings.length) html += findings.map((f) => '<div class="finding">' + esc(f) + '</div>').join("");
  el.innerHTML = html || '<div class="muted">not available on this agent yet</div>';
}

// ---- device menu ----
function setupDeviceMenu() {
  const pill = $("#devicePill");
  const menu = $("#deviceMenu");
  if (!pill || !menu) return;
  pill.addEventListener("click", () => {
    state.deviceMenuOpen = !state.deviceMenuOpen;
    menu.hidden = !state.deviceMenuOpen;
  });
  const add = $("#addDevice");
  if (add) add.addEventListener("click", () => {
    const hint = $("#pairHint");
    if (hint) hint.hidden = false;
  });
  document.addEventListener("click", (e) => {
    if (!e.target.closest("#devicePill") && !e.target.closest("#deviceMenu")) {
      state.deviceMenuOpen = false;
      if (menu) menu.hidden = true;
    }
  });
}

// ---- refresh ----
async function refresh() {
  const tasks = [
    j("/api/current").then((d) => { state.current = d; }).catch(() => {}),
    j("/api/history?range=" + encodeURIComponent(state.range)).then((d) => { state.history = d; }).catch(() => {}),
    j("/api/analytics?range=" + encodeURIComponent(state.range)).then((d) => { state.analytics = d; }).catch(() => {}),
    j("/api/consumers?range=" + encodeURIComponent(state.range)).then((d) => { state.consumers = d; }).catch(() => {}),
    j("/api/sensors").then((d) => { state.sensors = d; }).catch(() => {}),
    j("/api/thermal").then((d) => { state.thermal = d; }).catch(() => {}),
    j("/api/battery").then((d) => { state.battery = d; }).catch(() => {}),
  ];
  await Promise.allSettled(tasks);
  pushLiveSeries(state.current);
  renderRanges();
  renderTiles();
  renderTopApps();
  renderAppsTab();
  renderStatus();
  renderAnalytics();
  applyFocus();
}

function render() { refresh(); }

// ---- keyboard ----
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") { state.focusedTile = null; applyFocus(); }
});

// ---- init ----
renderRanges();
renderTabs();
setupDeviceMenu();
setTab("overview");
refresh();
state.timer = setInterval(refresh, 1000);
