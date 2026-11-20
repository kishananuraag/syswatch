// syswatch new UI (8 screens) — dark single-page dashboard.
// Tokens: bg #0D1013, surface #141A1F, accent #6EE7C8, Inter 400/600, tabular numerals.

const $ = (id) => document.getElementById(id);

const state = {
  view: "overview",
  range: "1h",
  current: null,
  history: null,
  analytics: null,
  logs: null,
  alerts: null,
  timer: null,
};

const RANGES = ["1m", "5m", "15m", "1h", "6h", "1d", "7d", "30d", "1y"];

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
  if (!Array.isArray(arr) || arr.length < 2) return "";
  const nums = arr.map((x) => (isNum(x) ? x : 0));
  const min = Math.min.apply(null, nums);
  const max = Math.max.apply(null, nums);
  const range = (max - min) || 1;
  return nums.map((v, i) => {
    const x = (i / (nums.length - 1)) * 100;
    const y = 30 - ((v - min) / range) * 30;
    return (i === 0 ? "M" : "L") + x.toFixed(1) + " " + y.toFixed(1);
  }).join(" ");
}

function sparkSvg(arr, h) {
  const path = sparkPath(arr);
  const height = h || 44;
  return '<svg class="spark" viewBox="0 0 100 30" preserveAspectRatio="none" style="width:100%;height:' + height + 'px;display:block;margin:4px 0 2px" aria-hidden="true">' +
    '<path d="' + path + '" fill="none" stroke="#6EE7C8" stroke-width="1.5" vector-effect="non-scaling-stroke" stroke-linejoin="round"></path>' +
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

function renderOverview() {
  const el = $("#view-overview");
  if (!el) return;
  const c = state.current;
  const cores = c && c.cpu ? c.cpu.cores : "—";
  const procCount = c ? c.proc_count : "—";
  const tempCount = c && Array.isArray(c.temps_c) ? c.temps_c.length : 0;

  const chipsHtml = [
    chip("CPU", fmt(cpuOf(c), 1), "%", cores + " cores", "load " + fmt(c && c.cpu && c.cpu.load ? c.cpu.load.one : null, 2)),
    chip("Memory", fmt(ramPct(c), 1), "%", bytesFmt(c && c.ram ? c.ram.used_bytes : null) + " used", c && c.pressure && c.pressure.memory ? c.pressure.memory.level : ""),
    chip("GPU", fmt(gpuUtil(c), 1), "%", c && c.gpu ? (c.gpu.gpu_model || "GPU") : "GPU", c && c.gpu && c.gpu.mem_used_mb ? fmt(c.gpu.mem_used_mb, 0) + " MB vram" : ""),
    chip("Battery", fmt(batPct(c), 0), "%", c && c.battery ? (c.battery.charging ? "charging" : "on battery") : "battery", c && c.battery ? fmt(c.battery.health_pct, 1) + "% health" : ""),
    chip("Package power", fmt(pkgW(c), 2), "W", c && c.power ? c.power.thermal_pressure || "thermal" : "power", c && c.power ? "cpu " + fmt(c.power.cpu_w, 2) + " W" : ""),
    chip("Network", bpsFmt(netDown(c)), "", c && c.net ? (c.net.ifaces ? c.net.ifaces.length + " ifaces" : "") : "network", c && c.net ? bpsFmt(c.net.up_bps) + " up" : ""),
    chip("Disk", fmt(diskUsed(c), 1), "%", c && c.disk ? c.disk.mount || "disk" : "disk", c && c.disk ? bytesFmt(c.disk.free_bytes) + " free" : ""),
    chip("Thermal", fmt(maxTemp(c), 1), "°C", tempCount + " sensors", c && c.pressure && c.pressure.cpu ? c.pressure.cpu.level : ""),
  ].join("");

  const topCpu = (c && c.processes && Array.isArray(c.processes.by_cpu)) ? c.processes.by_cpu.slice(0, 5) : [];
  const consumersHtml = topCpu.map((p) => {
    const w = Math.min(100, (p.cpu_pct || 0));
    return '<div class="list-row">' +
      '<span style="flex:0 0 230px;max-width:40%;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">' + esc(p.name) + '</span>' +
      '<span class="bar" style="flex:1"><span style="width:' + w + '%"></span></span>' +
      '<span style="flex:0 0 56px;text-align:right;font-variant-numeric:tabular-nums">' + fmt(p.cpu_pct, 1) + '%</span>' +
      '</div>';
  }).join("") || '<p class="muted">no data</p>';

  const memPressure = c && c.pressure && c.pressure.memory ? c.pressure.memory.level : "unknown";
  const collectorOk = c && c.collector ? c.collector.available : false;

  el.innerHTML =
    '<div class="chips">' + chipsHtml + '</div>' +
    '<div class="row">' +
      '<div class="col card"><h3>Top consumers · CPU</h3>' + consumersHtml +
        '<div style="margin-top:8px"><button class="viewbtn" data-view="cpu" style="border-bottom:0">All details</button></div>' +
      '</div>' +
      '<div class="col card"><h3>Status</h3>' +
        '<div class="status-row"><div><div class="title warn">Memory pressure: ' + esc(memPressure) + '</div><div class="sub">' + fmt(ramPct(c), 1) + '% in use</div></div></div>' +
        '<div class="status-row"><div><div class="title">Collector ' + (collectorOk ? "running" : "unavailable") + '</div><div class="sub">' + (c && c.collector ? esc(c.collector.note || "") : "") + '</div></div></div>' +
        '<div class="status-row"><div><div class="title">' + tempCount + ' sensors · ' + (c && c.cpu ? c.cpu.cores : "—") + ' cores</div><div class="sub">Deep power data ' + (c && c.power && c.power.available ? "available" : "locked") + '</div></div></div>' +
      '</div>' +
    '</div>';
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
      '<div class="kv"><span class="k">mean</span><span class="v">' + fmt(cpu.mean_pct, 2) + ' %</span></div>' +
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
  try { state.history = await j("/api/history?range=" + encodeURIComponent(state.range)); } catch (e) { state.history = null; }
  try { state.analytics = await j("/api/analytics?range=" + encodeURIComponent(state.range)); } catch (e) { state.analytics = null; }
  try { state.logs = await j("/api/logs?limit=8"); } catch (e) { state.logs = null; }
  try { state.alerts = await j("/api/alerts"); } catch (e) { state.alerts = null; }
  render();
}

function boot() {
  wireEvents();
  showView("overview");
  refresh();
  state.timer = setInterval(refresh, 5000);
}

boot();
