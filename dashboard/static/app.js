// syswatch dashboard — S12.3 (range chips) on top of S12.1 (live sampler).
// No new chart libraries. Chart.js (loaded via /static/chart.umd.min.js)
// drives every chart. Range chips live above each chart and trigger a
// per-target reload of /api/history?range=... .

const $ = (id) => document.getElementById(id);
const fmt = {
  pct: (n) => (n == null || isNaN(n)) ? "\u2013" : n.toFixed(1) + "%",
  bps: (n) => (n == null || isNaN(n)) ? "\u2013" : formatBps(n),
  mb: (n) => (n == null || isNaN(n)) ? "\u2013" : n.toFixed(0),
};
function formatBps(n) {
  if (!n) return "0 b/s";
  const u = ["b/s","KB/s","MB/s","GB/s"]; let i = 0;
  while (n >= 1024 && i < u.length - 1) { n /= 1024; i++; }
  return n.toFixed(i === 0 ? 0 : 1) + " " + u[i];
}

// Plain-English time label for the "data starts \u2026" hint in empty states.
function shortDate(iso) {
  if (!iso) return "";
  const d = new Date(iso);
  if (isNaN(d.getTime())) return "";
  return d.toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

// ---------- CHARTS ----------

const charts = {};        // key -> Chart.js instance
const emptyEls = {};      // data-empty id -> DOM overlay
let tempChartKeys = [];   // active per-sensor temp chart keys for cleanup

// Chart.js defaults
Chart.defaults.color = "#E6EDF3";
Chart.defaults.font.family = 'Consolas, "Cascadia Code", monospace';
Chart.defaults.font.size = 11;
Chart.defaults.animation = false;

function makeLineChart(canvas, label, color, opts) {
  opts = opts || {};
  return new Chart(canvas, {
    type: "line",
    data: {
      labels: [],
      datasets: [{
        label,
        data: [],
        borderColor: color,
        backgroundColor: color + "22",
        fill: opts.fill !== false,
        borderWidth: 1.7,
        pointRadius: 0,
        tension: 0.4,
        spanGaps: true,
      }],
    },
    options: {
      responsive: true,
      maintainAspectRatio: false,
      interaction: { mode: "index", intersect: false },
      plugins: { legend: { display: false } },
      scales: {
        x: { ticks: { maxTicksLimit: 6, maxRotation: 0, autoSkip: true }, grid: { color: "rgba(139,148,158,0.08)" } },
        y: Object.assign({ beginAtZero: true, ticks: { maxTicksLimit: 5 } }, opts.y || {}),
      },
    },
  });
}

// ---------- RANGE / EMPTY STATE WIRING ----------

// Per-chart target state. Defaults: 1h for CPU/RAM/Net, 15m for temps.
const TARGETS = {
  cpu:   { range: "1h" },
  ram:   { range: "1h" },
  net:   { range: "1h" },
  temps: { range: "15m" },
};

function setActiveChip(target, range) {
  document.querySelectorAll('.range-chips[data-range-target="' + target + '"] .chip')
    .forEach((b) => b.classList.toggle("active", b.dataset.range === range));
}

function showEmpty(target, message) {
  const el = emptyEls[target];
  if (!el) return;
  el.hidden = false;
  const t = el.querySelector(".empty-text");
  if (t && message) t.textContent = message;
  const d = el.querySelector(".empty-detail");
  if (d) d.style.display = "none";
}

function setEmptyDetail(target, msg) {
  const el = emptyEls[target];
  if (!el) return;
  const d = el.querySelector(".empty-detail");
  if (!d) return;
  if (msg) { d.textContent = msg; d.style.display = ""; }
  else { d.textContent = ""; d.style.display = "none"; }
}

function hideEmpty(target) {
  const el = emptyEls[target];
  if (!el) return;
  el.hidden = true;
}

async function loadHistory(target) {
  const t = TARGETS[target];
  if (!t) return;
  const url = "/api/history?range=" + encodeURIComponent(t.range);
  let data;
  try {
    const r = await fetch(url, { cache: "no-store" });
    data = await r.json();
  } catch (e) {
    showEmpty(target, "Couldn't reach the dashboard");
    return;
  }

  // Empty-state path: API reports zero in-range points.
  if (data && data.empty) {
    showEmpty(target, "No data in this range yet");
    if (data.data_started_iso) {
      setEmptyDetail(target, "Records start " + shortDate(data.data_started_iso));
    } else {
      setEmptyDetail(target, "");
    }
    // Zero out charts so axes don't mislead.
    if (target === "cpu" && charts.cpu) {
      charts.cpu.data.labels = []; charts.cpu.data.datasets[0].data = []; charts.cpu.update();
    }
    if (target === "ram" && charts.ram) {
      charts.ram.data.labels = []; charts.ram.data.datasets[0].data = []; charts.ram.update();
    }
    if (target === "net" && charts.net) {
      charts.net.data.labels = []; charts.net.data.datasets[0].data = []; charts.net.update();
    }
    if (target === "temps") clearTempCharts();
    return;
  }

  hideEmpty(target);

  // CPU
  if (target === "cpu" && charts.cpu) {
    const pts = data.points || [];
    charts.cpu.data.labels = pts.map((p) => p.ts);
    charts.cpu.data.datasets[0].data = pts.map((p) => p.cpu);
    charts.cpu.update();
  }

  // RAM
  if (target === "ram" && charts.ram) {
    const pts = data.points || [];
    charts.ram.data.labels = pts.map((p) => p.ts);
    charts.ram.data.datasets[0].data = pts.map((p) => p.ram);
    charts.ram.update();
  }

  // Network: one chart showing down_bps.
  if (target === "net" && charts.net) {
    const pts = data.points || [];
    charts.net.data.labels = pts.map((p) => p.ts);
    charts.net.data.datasets[0].data = pts.map((p) => p.down_bps);
    charts.net.update();
  }

  // Temperatures: one mini chart per sensor (sensor_labels + temp_series).
  if (target === "temps") {
    renderTempCharts(data);
  }
}

function clearTempCharts() {
  tempChartKeys.forEach((k) => { if (charts[k]) { charts[k].destroy(); delete charts[k]; } });
  tempChartKeys = [];
  const grid = $("tempGrid");
  if (grid) grid.innerHTML = "";
}

function renderTempCharts(data) {
  const sl = data.sensor_labels || [];
  const ts = data.temp_series || [];
  const labels = (data.points || []).map((p) => p.ts);
  const area = $("tempArea");
  if (!area) return;

  if (!sl.length) {
    clearTempCharts();
    area.innerHTML = '<span class="na">no sensors exposed on this machine ' +
      '(Windows hides them from psutil; install LibreHardwareMonitor to enable)</span>';
    return;
  }

  // Build (or reuse) a responsive grid of mini-canvases.
  let grid = $("tempGrid");
  if (!grid) {
    area.innerHTML = '<div id="tempGrid" style="display:grid;' +
      'grid-template-columns:repeat(auto-fit,minmax(220px,1fr));gap:10px"></div>';
    grid = $("tempGrid");
  }
  const newKeys = [];
  sl.forEach((label, i) => {
    const key = "temp_" + i;
    let cell = $("tc" + i);
    if (!cell) {
      cell = document.createElement("div");
      cell.id = "tc" + i;
      cell.style.cssText = "background:rgba(22,27,34,0.5);border:1px solid rgba(139,148,158,0.15);" +
        "border-radius:4px;padding:8px;";
      cell.innerHTML = '<div style="font-size:11px;color:#8B949E;margin-bottom:4px;' +
        'text-transform:none;letter-spacing:0">' + esc(label) + '</div>' +
        '<div style="height:70px"><canvas id="tch' + i + '"></canvas></div>';
      grid.appendChild(cell);
    } else {
      const lblEl = cell.querySelector(".temp-label");
      if (lblEl) lblEl.textContent = label;
    }
    const series = (ts[i] || []).slice();
    // Pad series to align with labels.
    const padded = labels.map((_, j) => (j < series.length ? series[j] : null));
    if (!charts[key]) {
      charts[key] = makeLineChart($("tch" + i), label + " \u00b0C", "#D29922");
      charts[key].options.scales.y.suggestedMax = 90;
    }
    charts[key].data.labels = labels;
    charts[key].data.datasets[0].data = padded;
    charts[key].update();
    newKeys.push(key);
  });
  // Destroy orphaned temp charts (sensor count shrunk).
  tempChartKeys.forEach((k) => {
    if (!newKeys.includes(k) && charts[k]) { charts[k].destroy(); delete charts[k]; }
  });
  tempChartKeys = newKeys;
}

function bindRangeChips() {
  document.querySelectorAll(".range-chips[data-range-target]").forEach((row) => {
    const target = row.dataset.rangeTarget;
    const initial = TARGETS[target] && TARGETS[target].range;
    if (initial) setActiveChip(target, initial);
    row.addEventListener("click", (ev) => {
      const btn = ev.target.closest(".chip");
      if (!btn || !row.contains(btn)) return;
      const rng = btn.dataset.range;
      if (!rng) return;
      TARGETS[target].range = rng;
      setActiveChip(target, rng);
      loadHistory(target);
    });
  });
}

function startHistoryPollers() {
  Object.keys(TARGETS).forEach(loadHistory);
  setInterval(() => {
    Object.keys(TARGETS).forEach(loadHistory);
  }, 5000);
}

// ---------- LIVE CURRENT ----------

async function loadCurrent() {
  try {
    const r = await fetch("/api/current", { cache: "no-store" });
    const d = await r.json();
    $("status").textContent = "live";
    $("cpuBig").innerHTML = fmt.pct(d.cpu) + "<span class=\"unit\">%</span>";
    $("ramBig").textContent = (d.ram || 0).toFixed(0) + " %";
    $("ramSub").textContent =
      (d.ram_used_mb || 0).toFixed(0) + " MB / " +
      (d.ram_total_mb || 0).toFixed(0) + " MB";
    const ramPct = Math.max(0, Math.min(100, d.ram || 0));
    $("ramFill").style.width = ramPct + "%";

    $("netBig").textContent = fmt.bps((d.down_bps || 0) + (d.up_bps || 0));
    $("netSub").textContent = "down " + fmt.bps(d.down_bps || 0) +
                              "  /  up " + fmt.bps(d.up_bps || 0);

    if (Array.isArray(d.cores)) renderCores(d.cores);
    $("coreCount").textContent = "(" + (d.cores ? d.cores.length : 0) + ")";
    renderProcs(d.top_cpu || [], "procCpu", "cpu");
    renderProcs(d.top_mem || [], "procMem", "mem");
    $("clockTime").textContent = new Date().toLocaleTimeString();
    if (d.swap_used_mb != null) {
      $("swapSub").textContent = "swap " + d.swap_used_mb.toFixed(0) + " MB";
    }
    // Cache live temps so future history refreshes can rebuild temp charts.
    window.__lastTemps = d.temps_c || [];
  } catch (e) {
    $("status").textContent = "offline";
  }
}

function renderCores(cores) {
  const root = $("cores");
  if (!root) return;
  root.innerHTML = "";
  cores.forEach((c) => {
    const row = document.createElement("div");
    row.className = "core";
    row.innerHTML =
      '<div class="label">C' + c.idx + '</div>' +
      '<div class="barwrap"><div class="bar" style="width:' + c.pct.toFixed(1) + '%"></div></div>' +
      '<div class="val">' + c.pct.toFixed(1) + '%</div>';
    root.appendChild(row);
  });
}

function renderProcs(rows, id, key) {
  const tb = $(id);
  if (!tb) return;
  if (!rows.length) {
    tb.innerHTML = '<tr><td colspan="4" class="empty">no data</td></tr>';
    return;
  }
  tb.innerHTML = rows.map((r) => {
    const v = key === "cpu" ? fmt.pct(r.cpu) : fmt.mb(r.mem_mb) + " MB";
    return '<tr><td>' + r.pid + '</td><td>' + esc(r.name) +
           '</td><td class="num">' + v + '</td><td class="num">' +
           (key === "cpu" ? fmt.mb(r.mem_mb) + " MB" : fmt.pct(r.cpu)) + '</td></tr>';
  }).join("");
}

function esc(s) {
  return String(s == null ? "" : s).replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}

// ---------- LOGS ----------

async function loadLogs() {
  try {
    const r = await fetch("/api/logs?limit=50", { cache: "no-store" });
    const d = await r.json();
    const lines = (d.lines || []).map((l) => esc(l));
    $("logLines").innerHTML = lines.join("<br>") || "<span class=\"na\">no events</span>";
    $("logCount").textContent = "(" + lines.length + ")";
  } catch (e) {
    $("logLines").innerHTML = "<span class=\"na\">log fetch failed</span>";
  }
}

// ---------- BOOT ----------

window.addEventListener("DOMContentLoaded", () => {
  charts.cpu = makeLineChart($("cpuChart"), "CPU", "#58A6FF");
  charts.ram = makeLineChart($("ramChart"), "RAM", "#3FB950");
  charts.net = makeLineChart($("netChart"), "Network", "#58A6FF", { fill: false });

  // Cache empty-state overlay elements once.
  document.querySelectorAll(".empty-state[data-empty]").forEach((el) => {
    emptyEls[el.dataset.empty] = el;
  });

  bindRangeChips();
  startHistoryPollers();

  loadCurrent();
  setInterval(loadCurrent, 2000);

  loadLogs();
  setInterval(loadLogs, 10000);

  setInterval(() => { $("clockTime").textContent = new Date().toLocaleTimeString(); }, 1000);
});