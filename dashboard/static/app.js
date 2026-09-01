/* syswatch dashboard — vanilla JS + Chart.js (local, no CDN dependency) */
"use strict";

const RANGES = ["10m", "15m", "1h", "2d", "5d", "7d"];
let range = localStorage.getItem("syswatch_range") || "1h";
if (!RANGES.includes(range)) range = "1h";

const $ = (id) => document.getElementById(id);
const fmtBps = (v) => {
  if (v == null || isNaN(v)) return "\u2013";
  const u = ["bps", "Kbps", "Mbps", "Gbps"];
  let i = 0;
  while (v >= 1000 && i < u.length - 1) { v /= 1000; i++; }
  return v.toFixed(1) + " " + u[i];
};
const fmtGB = (b) => (b / 1073741824).toFixed(1);
const hhmm = (iso) => new Date(iso).toTimeString().slice(0, 5);

/* ---------------------------------------------------------- Chart.js helpers */
Chart.defaults.color = "#7C8896";
Chart.defaults.borderColor = "rgba(35,43,54,.8)";
Chart.defaults.font.family = "'Segoe UI',sans-serif";
Chart.defaults.font.size = 10;
Chart.defaults.animation = false;

function smoothLine(ctx, labels, datasets, yOpts = {}) {
  return new Chart(ctx, {
    type: "line",
    data: { labels, datasets },
    options: {
      responsive: true,
      maintainAspectRatio: false,
      interaction: { mode: "index", intersect: false },
      plugins: { legend: { display: false } },
      scales: {
        x: { ticks: { maxTicksLimit: 6, maxRotation: 0 }, grid: { display: false } },
        y: Object.assign({ beginAtZero: true, ticks: { maxTicksLimit: 5 } }, yOpts),
      },
      elements: { point: { radius: 0 }, line: { tension: 0.4, borderWidth: 1.7 } },
    },
  });
}

function ds(label, data, color, fill) {
  return {
    label, data, borderColor: color,
    backgroundColor: color + "22",
    fill: !!fill,
  };
}

const charts = {}; // key -> Chart instance
// Track latest labels + series so renderTemps() can build per-sensor charts
// without an extra /api round-trip.
let currentLabels = [];
let currentSeries = { cpu: [], ram: [], down_bps: [], up_bps: [], tempC: {} };
function updateChart(key, ctxId, labels, datasets, yOpts) {
  if (charts[key]) {
    charts[key].data.labels = labels;
    charts[key].data.datasets = datasets;
    if (yOpts && yOpts.suggestedMax != null)
      charts[key].options.scales.y.suggestedMax = yOpts.suggestedMax;
    charts[key].update("none");
    return;
  }
  charts[key] = smoothLine($(ctxId), labels, datasets, yOpts || {});
}

/* ------------------------------------------------------------ range selector */
(function buildChips() {
  const bar = $("rangeChips");
  RANGES.forEach((r) => {
    const b = document.createElement("button");
    b.textContent = r;
    if (r === range) b.classList.add("active");
    b.onclick = () => {
      range = r;
      localStorage.setItem("syswatch_range", r);
      bar.querySelectorAll("button").forEach((x) => x.classList.remove("active"));
      b.classList.add("active");
      refreshHistory();
    };
    bar.appendChild(b);
  });
})();

function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g,
    (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}

/* ---------------------------------------------------------------- rendering */
function renderCurrent(d) {
  const cpu = d.cpu ? d.cpu.total_pct : null;
  $("cpuBig").innerHTML = (cpu == null ? "\u2013" : cpu.toFixed(1)) +
    '<span class="unit">%</span>';
  const meta = [];
  if (d.cpu && d.cpu.freq_mhz) meta.push((d.cpu.freq_mhz / 1000).toFixed(2) + " GHz");
  if (d.proc_count) meta.push(d.proc_count + " procs");
  $("cpuMeta").textContent = meta.join(" \u00b7 ");

  const cores = (d.cpu && d.cpu.per_core_pct) || [];
  $("coreCount").textContent = cores.length + " cores";
  $("cores").innerHTML = cores.map((p, i) =>
    '<div class="core"><span>C' + i + '</span>' +
    '<div class="barwrap"><div class="barfill" style="width:' +
    Math.min(p, 100).toFixed(1) + '%"></div></div>' +
    '<span class="val">' + Math.round(p) + '%</span></div>'
  ).join("") || '<span class="na">not available</span>';

  if (d.ram && d.ram.total_bytes) {
    const pct = d.ram.pct;
    $("ramBig").innerHTML = pct.toFixed(1) + '<span class="unit">%</span>';
    $("ramSub").textContent =
      fmtGB(d.ram.used_bytes) + " / " + fmtGB(d.ram.total_bytes) + " GB used";
    $("ramFill").style.width = Math.min(pct, 100) + "%";
    $("swapSub").textContent = d.ram.swap_total_bytes ?
      "swap: " + fmtGB(d.ram.swap_used_bytes) + " / " +
      fmtGB(d.ram.swap_total_bytes) + " GB" : "";
  } else {
    $("ramBig").textContent = "\u2013";
    $("ramSub").textContent = "not available";
  }

  const net = d.net || {};
  $("netBig").innerHTML =
    "&darr; " + fmtBps(net.down_bps) + "<br>&uarr; " + fmtBps(net.up_bps);
  const topIface = ((net.ifaces || [])[0] || {});
  $("netSub").textContent = topIface.name ? ("via " + topIface.name) : "";

  const row = (p) => "<tr><td>" + p.pid + "</td><td>" + escapeHtml(p.name) +
    '</td><td class="num">' + (p.cpu_pct || 0).toFixed(1) +
    '</td><td class="num">' + (p.mem_mb || 0).toFixed(0) + "</td></tr>";
  const pc = (d.processes && d.processes.by_cpu) || [];
  const pm = (d.processes && d.processes.by_mem) || [];
  $("procCpu").innerHTML = pc.map(row).join("") ||
    '<tr><td colspan="4" class="empty">waiting for data\u2026</td></tr>';
  $("procMem").innerHTML = pm.map(row).join("") ||
    '<tr><td colspan="4" class="empty">waiting for data\u2026</td></tr>';
}

/* ------------------------------------------------------------- temperatures */
let tempChartKeys = [];
function renderTemps(d) {
  const temps = d.temps_c || [];
  // Cache so renderHistory() can re-render temp charts without a new /api call.
  window.__lastTemps = temps;
  const area = $("tempArea");
  const note = $("tempNote");
  if (!temps.length) {
    note.textContent = "";
    area.innerHTML = '<span class="na">no sensors exposed on this machine ' +
      '(Windows hides them from psutil; install LibreHardwareMonitor to enable)</span>';
    tempChartKeys.forEach((k) => { if (charts[k]) { charts[k].destroy(); delete charts[k]; } });
    tempChartKeys = [];
    return;
  }
  // one small smooth chart per sensor, laid out in a mini-grid
  if (!area.dataset.grid) {
    area.innerHTML = '<div id="tempGrid" style="display:grid;' +
      'grid-template-columns:repeat(auto-fit,minmax(240px,1fr));gap:12px"></div>';
    area.dataset.grid = "1";
  }
  const grid = $("tempGrid");
  temps.forEach((t, i) => {
    let cell = document.getElementById("tc" + i);
    if (!cell) {
      cell = document.createElement("div");
      cell.id = "tc" + i;
      cell.innerHTML = '<div style="font-size:11px;color:#7C8896;margin-bottom:4px">' +
        escapeHtml(t.label) + '</div><div class="chart-wrap" style="height:70px"><canvas id="tch' + i + '" class="chart"></canvas></div>';
      grid.appendChild(cell);
    }
    cell.querySelector("div").textContent =
      t.label + " \u2014 " + t.c.toFixed(1) + "\u00b0C";
  });
  while (grid.children.length > temps.length)
    grid.removeChild(grid.lastChild);
  // Rebuild chart list: destroy any charts whose cell index > temps.length, keep the rest.
  const newKeys = [];
  // Prefer history-driven labels (the bucketized range), else fall back to
  // a single live label so the chart has SOMETHING to draw.
  const liveLabel = [new Date().toTimeString().slice(0, 5)];
  const labels = (currentLabels && currentLabels.length) ? currentLabels : liveLabel;
  temps.forEach((t, i) => {
    const key = "temp" + i;
    newKeys.push(key);
    // Pull this sensor's series from currentSeries (set by renderHistory).
    // It is a parallel array; align with labels.
    const series = (currentSeries.tempC && currentSeries.tempC[i]) || [];
    const padded = labels.map((_, j) => (j < series.length ? series[j] : null));
    const labelText = (currentSeries.sensorLabels && currentSeries.sensorLabels[i]) || t.label;
    updateChart(
      key,
      "tch" + i,
      labels,
      [ds(labelText + " \u00b0C", padded, "#D29922", true)],
      { suggestedMax: 90 }
    );
  });
  // Destroy orphaned temp charts
  tempChartKeys.forEach((k) => {
    if (!newKeys.includes(k) && charts[k]) { charts[k].destroy(); delete charts[k]; }
  });
  tempChartKeys = newKeys;
}

/* -------------------------------------------------------------- history+net */
function renderHistory(h) {
  const labels = (h.labels || []).map(hhmm);
  const s = h.series || {};
  // Update shared state for renderTemps() to consume
  currentLabels = labels;
  currentSeries = {
    cpu: s.cpu || [],
    ram: s.ram || [],
    down_bps: s.down_bps || [],
    up_bps: s.up_bps || [],
    tempC: s.temp_series || [],
    sensorLabels: s.sensor_labels || [],
  };
  // Clamp y on network chart so a one-time spike doesn't stretch scale to Gbps.
  const netMax = (() => {
    const all = (currentSeries.down_bps || []).concat(currentSeries.up_bps || []);
    if (!all.length) return undefined;
    let m = 0;
    for (const v of all) if (v > m) m = v;
    // Round up to next 100Kbps increment, but cap at 1Gbps
    const step = 100000;
    const cap = 1000000000;
    return Math.min(cap, Math.ceil(m / step) * step);
  })();
  const netYOpts = netMax != null ? { suggestedMax: netMax } : {};
  updateChart("cpu", "cpuChart", labels,
    [ds("CPU %", currentSeries.cpu, "#58A6FF", true)],
    { suggestedMax: 100, ticks: { maxTicksLimit: 5 } });
  updateChart("ram", "ramChart", labels,
    [ds("RAM %", currentSeries.ram, "#2EA043")],
    { suggestedMax: 100 });
  updateChart("net", "netChart", labels,
    [ds("down", currentSeries.down_bps, "#58A6FF"),
     ds("up", currentSeries.up_bps, "#BC8CFF")], netYOpts);
  // Re-render temp charts now that we have labels + tempC
  renderTemps({ temps_c: (window.__lastTemps || []) });
}

/* ------------------------------------------------------------------- alerts */
function renderAlerts(al) {
  const rules = al.rules || [];
  $("alerts").innerHTML = rules.length ? rules.map((r) => {
    const fired = !!r.last_fired;
    return '<div class="alertrow"><span class="dot ' +
      (fired ? "fired" : "ok") + '"></span>' +
      "<div><b>" + escapeHtml(r.metric) + "</b> " + escapeHtml(r.op) + " " +
      r.value + " for " + r.duration_secs + "s</div>" +
      '<span class="alertmeta">' +
      (fired ? "last fired " + new Date(r.last_fired).toLocaleString() : "armed \u00b7 not fired")
      + "</span></div>";
  }).join("") : '<span class="na">no alert rules configured</span>';
}

/* --------------------------------------------------------------- logs strip */
function renderLogs(d) {
  $("logCount").textContent = d.count != null ? ("(" + d.count + " in buffer)") : "";
  const el = $("logLines");
  const stick = el.scrollTop + el.clientHeight >= el.scrollHeight - 8;
  el.innerHTML = (d.events || []).map((e) => {
    const t = new Date(e.ts).toTimeString().slice(0, 8);
    const cls = e.msg && e.msg.startsWith("FIRE") ? " class=\"warn\"" : "";
    return '<div' + cls + '><span class="t">' + t + "</span> [" +
      escapeHtml(e.src) + "] " + escapeHtml(e.msg) + "</div>";
  }).join("") || '<span class="na">no events yet</span>';
  if (stick) el.scrollTop = el.scrollHeight;
}

/* ------------------------------------------------------------ refresh loops */
async function fetchJson(url) {
  const r = await fetch(url);
  if (!r.ok) throw new Error(url + " -> " + r.status);
  return r.json();
}

async function refreshCurrent() {
  try {
    renderCurrent(await fetchJson("/api/current"));
    $("status").textContent = "live \u00b7 " + new Date().toTimeString().slice(0, 8);
    $("status").style.color = "";
  } catch (e) {
    $("status").textContent = "waiting for data\u2026";
    $("status").style.color = "#E10600";
  }
}

async function refreshHistory() {
  try { renderHistory(await fetchJson("/api/history?range=" + range)); }
  catch (e) { /* keep last good chart */ }
}

async function refreshAlerts() {
  try { renderAlerts(await fetchJson("/api/alerts")); } catch (e) {}
}

async function refreshLogs() {
  try { renderLogs(await fetchJson("/api/logs?limit=50")); } catch (e) {}
}

function tickClock() {
  $("clockTime").textContent = new Date().toLocaleTimeString();
}

refreshCurrent();
refreshHistory();
refreshAlerts();
refreshLogs();
tickClock();
// Track interval IDs so we can clear them on tab hide / page unload.
// Without this, hidden tabs keep fetching every 5s, ballooning CPU and RAM.
const intervalIds = [
  setInterval(refreshCurrent, 5000),
  setInterval(refreshHistory, 5000),
  setInterval(tickClock, 1000),
  setInterval(refreshAlerts, 15000),
  setInterval(refreshLogs, 5000),
];
// Pause all refreshes while the tab is hidden, resume on return.
let paused = false;
document.addEventListener("visibilitychange", () => {
  if (document.hidden && !paused) {
    paused = true;
    intervalIds.forEach((id) => clearInterval(id));
  } else if (!document.hidden && paused) {
    paused = false;
    // Manual refresh then restart intervals
    refreshCurrent();
    refreshHistory();
    refreshAlerts();
    refreshLogs();
    intervalIds[0] = setInterval(refreshCurrent, 5000);
    intervalIds[1] = setInterval(refreshHistory, 5000);
    intervalIds[2] = setInterval(tickClock, 1000);
    intervalIds[3] = setInterval(refreshAlerts, 15000);
    intervalIds[4] = setInterval(refreshLogs, 5000);
  }
});
// Final cleanup on unload
window.addEventListener("beforeunload", () => {
  intervalIds.forEach((id) => clearInterval(id));
  // Destroy all charts so their canvas listeners are released
  Object.keys(charts).forEach((k) => { if (charts[k]) { charts[k].destroy(); delete charts[k]; } });
});
