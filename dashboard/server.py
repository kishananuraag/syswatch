#!/usr/bin/env python3
"""syswatch web dashboard — stdlib only."""
import json
import os
from datetime import datetime, timedelta, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse, parse_qs

import alerts

LOGS_DIR = os.environ.get("SYSWATCH_LOGS", r"C:\ProgramData\syswatch\logs")
PORT = 8787
MAX_POINTS = 720

HTML = """<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>syswatch</title>
<style>
  :root { --bg:#0d1117; --panel:#161b22; --border:#30363d; --fg:#e6edf3;
          --dim:#8b949e; --accent:#58a6ff; --green:#3fb950; --red:#f85149;
          --yellow:#d29922; --purple:#bc8cff; }
  * { box-sizing:border-box; margin:0; }
  body { background:var(--bg); color:var(--fg); font:14px/1.5 -apple-system,'Segoe UI',sans-serif;
         padding:20px; max-width:1100px; margin:0 auto; }
  h1 { font-size:20px; margin-bottom:4px; }
  .sub { color:var(--dim); font-size:12px; margin-bottom:16px; }
  .grid { display:grid; grid-template-columns:1fr 1fr; gap:14px; }
  @media (max-width:800px){ .grid{grid-template-columns:1fr;} }
  .card { background:var(--panel); border:1px solid var(--border); border-radius:8px; padding:14px; }
  .card h2 { font-size:13px; text-transform:uppercase; letter-spacing:.05em; color:var(--dim); margin-bottom:8px; }
  .big { font-size:42px; font-weight:600; line-height:1.1; }
  .unit { font-size:16px; color:var(--dim); font-weight:400; }
  canvas { width:100%; height:120px; display:block; margin-top:8px; }
  table { width:100%; border-collapse:collapse; font-size:13px; }
  th,td { text-align:left; padding:4px 6px; border-bottom:1px solid var(--border); }
  th { color:var(--dim); font-weight:500; }
  td.num { text-align:right; font-variant-numeric:tabular-nums; }
  .barwrap { height:18px; background:#21262d; border-radius:4px; overflow:hidden; margin-top:6px; }
  .barfill { height:100%; background:var(--accent); transition:width .4s; }
  .disks div { margin:4px 0; font-size:13px; }
  .temp { color:var(--yellow); }
  #status { float:right; color:var(--dim); font-size:12px; }
  nav { margin-bottom:14px; }
  nav a { color:var(--dim); text-decoration:none; margin-right:16px; font-size:13px;
          cursor:pointer; padding-bottom:2px; }
  nav a.active { color:var(--fg); border-bottom:2px solid var(--accent); }
  .core { display:grid; grid-template-columns:34px 1fr 40px; gap:8px; align-items:center;
          margin:4px 0; font-size:12px; color:var(--dim); }
  .core .barwrap { height:10px; margin-top:0; }
  .coreval { text-align:right; color:var(--fg); font-variant-numeric:tabular-nums; }
  .panel { display:none; margin-bottom:14px; }
  .panel .card { margin-bottom:14px; }
  #rangebar { display:inline-flex; gap:4px; text-transform:none; letter-spacing:0; font-weight:400; }
  #rangebar button { background:none; border:1px solid var(--border); color:var(--dim);
                     border-radius:10px; font-size:11px; padding:1px 8px; cursor:pointer; }
  #rangebar button:hover { color:var(--fg); }
  #rangebar button.active { color:var(--bg); background:var(--accent); border-color:var(--accent); }
  .empty { color:var(--dim); font-style:italic; font-weight:400; text-transform:none; letter-spacing:0; }
  #logfilter { width:260px; max-width:100%; background:var(--bg); border:1px solid var(--border);
               color:var(--fg); border-radius:6px; padding:5px 9px; font-size:13px; margin-bottom:10px; }
  .wbtn { background:none; border:1px solid var(--border); color:var(--dim); border-radius:4px;
          cursor:pointer; font-size:11px; padding:1px 6px; margin-left:4px; }
  .wbtn:hover { color:var(--fg); }
  .wbtn.pinned { color:var(--accent); border-color:var(--accent); }
  #loglines { background:#010409; border:1px solid var(--border); border-radius:8px; padding:12px;
              font:12px/1.6 Consolas,Menlo,monospace; color:var(--dim); white-space:pre-wrap;
              word-break:break-all; height:60vh; overflow-y:auto; }
</style>
</head>
<body>
<div><h1>syswatch <span id="status">loading…</span></h1>
<div class="sub" id="sub"></div></div>
<nav>
  <a id="tab-overview" class="active" onclick="showTab('overview')">Overview</a>
  <a id="tab-cpu" onclick="showTab('cpu')">CPU</a>
  <a id="tab-network" onclick="showTab('network')">Network</a>
  <a id="tab-ram" onclick="showTab('ram')">RAM</a>
  <a id="tab-processes" onclick="showTab('processes')">Processes</a>
  <a id="tab-logs" onclick="showTab('logs')">Logs</a>
</nav>
<div class="grid" id="overview">
  <div class="card" id="w-cpu"><h2 style="display:flex;align-items:center;gap:8px;flex-wrap:wrap">CPU total <span id="rangebar"></span></h2>
    <div class="big" id="cpuBig">–<span class="unit">%</span></div>
    <canvas id="cpuChart"></canvas></div>
  <div class="card" id="w-cores" style="grid-column:1/-1"><h2>CPU per core</h2><div id="cores"></div></div>
  <div class="card" id="w-mem"><h2>Memory</h2>
    <div class="big" id="ramBig">–<span class="unit">%</span></div>
    <div class="barwrap"><div class="barfill" id="ramBar"></div></div>
    <canvas id="ramChart"></canvas></div>
  <div class="card" id="w-net"><h2>Network rx/tx</h2>
    <div class="big" id="netBig" style="font-size:24px">–</div>
    <canvas id="netChart"></canvas></div>
  <div class="card" id="w-temp"><h2>Temperature</h2>
    <div class="big" id="tempBig" style="font-size:26px">–</div>
    <canvas id="tempChart"></canvas></div>
  <div class="card" id="w-disks"><h2>Disks / Uptime</h2>
    <div class="disks" id="disks"></div>
    <div id="uptime" style="margin-top:6px;color:var(--dim)"></div></div>
  <div class="card" id="w-procs" style="grid-column:1/-1"><h2>Top processes by CPU</h2>
    <table><thead><tr><th>PID</th><th>Name</th><th style="text-align:right">CPU %</th><th style="text-align:right">Mem MB</th></tr></thead>
    <tbody id="procs"></tbody></table></div>
  <div class="card" id="w-alerts" style="grid-column:1/-1"><h2>Alerts</h2><div id="alerts">loading…</div></div>
</div>
<div class="panel" id="cpuPanel">
  <div class="card"><h2>CPU total % — history</h2><canvas id="cpuChartLg" style="height:220px"></canvas></div>
  <div class="card"><h2>CPU per core</h2><div id="coresDetail"></div></div>
</div>
<div class="panel" id="networkPanel">
  <div class="card"><h2>Network ↓ rx / ↑ tx — history</h2><canvas id="netChartLg" style="height:220px"></canvas></div>
  <div class="card"><h2>Interfaces</h2>
    <table><thead><tr><th>Interface</th><th style="text-align:right">Rx</th><th style="text-align:right">Tx</th></tr></thead>
    <tbody id="ifaces"><tr><td colspan="3" class="empty">waiting for data…</td></tr></tbody></table></div>
</div>
<div class="panel" id="ramPanel">
  <div class="card"><h2>Memory % — history</h2><canvas id="ramChartLg" style="height:220px"></canvas></div>
  <div class="card"><h2>Top processes by memory</h2>
    <table><thead><tr><th>PID</th><th>Name</th><th style="text-align:right">CPU %</th><th style="text-align:right">Mem MB</th></tr></thead>
    <tbody id="memprocs"><tr><td colspan="4" class="empty">waiting for data…</td></tr></tbody></table></div>
</div>
<div class="panel" id="processesPanel">
  <div class="card"><h2>Top processes by CPU</h2>
    <table><thead><tr><th>PID</th><th>Name</th><th style="text-align:right">CPU %</th><th style="text-align:right">Mem MB</th></tr></thead>
    <tbody id="procsLg"><tr><td colspan="4" class="empty">waiting for data…</td></tr></tbody></table></div>
  <div class="card"><h2>Top processes by memory</h2>
    <table><thead><tr><th>PID</th><th>Name</th><th style="text-align:right">CPU %</th><th style="text-align:right">Mem MB</th></tr></thead>
    <tbody id="procsMem"><tr><td colspan="4" class="empty">waiting for data…</td></tr></tbody></table></div>
</div>
<div class="logs-panel panel" id="logsPanel">
  <input id="logfilter" placeholder="filter logs…" oninput="loadLogs()">
  <pre id="loglines">loading…</pre>
</div>
<script>
// ---- widget pin/reorder (localStorage 'syswatch_widgets') ----
const WKEY='syswatch_widgets';
function loadWidgetState(){ try{ return JSON.parse(localStorage.getItem(WKEY))||{}; }catch(e){ return {}; } }
function saveWidgetState(s){ try{ localStorage.setItem(WKEY,JSON.stringify(s)); }catch(e){} }
function persistWidgets(){ const g=document.getElementById('overview');
  saveWidgetState({order:[...g.children].map(c=>c.id),
                   pinned:Object.fromEntries([...g.children].filter(c=>c.dataset.pinned).map(c=>[c.id,1]))}); }
function moveWidget(card,dir){ const g=document.getElementById('overview');
  const kids=[...g.children], i=kids.indexOf(card), j=i+dir;
  if(j<0||j>=kids.length) return;
  const pinnedFirst=kids.filter(k=>k.dataset.pinned), rest=kids.filter(k=>!k.dataset.pinned);
  const arr=card.dataset.pinned?pinnedFirst:rest;
  const ai=arr.indexOf(card); arr.splice(ai,1); arr.splice(Math.min(Math.max(ai+dir,0),arr.length),0,card);
  [...pinnedFirst,...rest].forEach(k=>g.appendChild(k)); persistWidgets(); }
function togglePin(card){ card.dataset.pinned=card.dataset.pinned?'':'1';
  card.querySelector('.wpin').classList.toggle('pinned',!!card.dataset.pinned); persistWidgets(); }
function initWidgets(){ const st=loadWidgetState(), g=document.getElementById('overview');
  const cards=[...g.children];
  cards.forEach(card=>{
    if(st.pinned&&st.pinned[card.id]) card.dataset.pinned='1';
    const h=card.querySelector('h2');
    const pin=document.createElement('button'); pin.className='wbtn wpin'+(card.dataset.pinned?' pinned':'');
    pin.textContent='pin'; pin.title='Pin widget'; pin.onclick=()=>togglePin(card);
    const up=document.createElement('button'); up.className='wbtn'; up.textContent='↑'; up.title='Move up'; up.onclick=()=>moveWidget(card,-1);
    const dn=document.createElement('button'); dn.className='wbtn'; dn.textContent='↓'; dn.title='Move down'; dn.onclick=()=>moveWidget(card,1);
    h.append(pin,up,dn); });
  if(Array.isArray(st.order)){
    const pinned=[],rest=[];
    for(const id of st.order){ const c=document.getElementById(id); if(!c) continue; (c.dataset.pinned?pinned:rest).push(c); }
    cards.forEach(c=>{ if(!pinned.includes(c)&&!rest.includes(c)) (c.dataset.pinned?pinned:rest).push(c); });
    [...pinned,...rest].forEach(c=>g.appendChild(c));
  } else {
    cards.filter(c=>c.dataset.pinned).forEach(c=>g.appendChild(c));
  } }
initWidgets();
const RANGES = {'10m':600e3,'15m':900e3,'30m':1800e3,'1h':3600e3,'3h':10800e3,
                '6h':21600e3,'12h':43200e3,'1d':86400e3,'3d':259200e3,'7d':604800e3};
let range = '1h';
(function buildRangeBar(){
  const bar=document.getElementById('rangebar');
  Object.keys(RANGES).forEach(function(r){
    const b=document.createElement('button'); b.textContent=r;
    if(r===range) b.classList.add('active');
    b.onclick=function(){ range=r;
      bar.querySelectorAll('button').forEach(x=>x.classList.remove('active'));
      b.classList.add('active'); refresh(); };
    bar.appendChild(b); });
})();
function fmtBps(v){ if(v==null) return '–'; const u=['bps','Kbps','Mbps','Gbps']; let i=0;
  while(v>=1000&&i<u.length-1){v/=1000;i++;} return v.toFixed(1)+' '+u[i]; }
function fmtUp(s){ const d=Math.floor(s/86400),h=Math.floor(s%86400/3600),m=Math.floor(s%3600/60);
  return d?`${d}d ${h}h`:h?`${h}h ${m}m`:`${m}m`; }
function draw(id, series, opts){
  const c=document.getElementById(id), dpr=window.devicePixelRatio||1;
  const w=c.clientWidth, h=c.clientHeight;
  if(!w||!h) return; // canvas hidden (inactive tab)
  c.width=w*dpr; c.height=h*dpr;
  const x=c.getContext('2d'); x.scale(dpr,dpr);
  x.clearRect(0,0,w,h);
  const colors=['#58a6ff','#3fb950','#bc8cff'];
  let max=opts.max||1;
  if(!opts.fixedMax) for(const s of series) for(const v of s.data) if(v!=null&&v>max) max=v;
  let any=false;
  series.forEach(function(s,si){
    x.strokeStyle=s.color||colors[si%colors.length];
    x.lineWidth=1.5; x.lineJoin='round'; x.lineCap='round';
    const n=s.data.length, run=[];
    const flushRun=function(){
      if(!run.length) return;
      any=true;
      if(run.length===1){ // isolated point
        x.beginPath(); x.arc(run[0][0],run[0][1],1.5,0,2*Math.PI);
        x.fillStyle=x.strokeStyle; x.fill();
      } else {
        // smooth bezier: quadratic through midpoints (no pointy spikes)
        x.beginPath(); x.moveTo(run[0][0],run[0][1]);
        for(let i=1;i<run.length-1;i++){
          const mx=(run[i][0]+run[i+1][0])/2, my=(run[i][1]+run[i+1][1])/2;
          x.quadraticCurveTo(run[i][0],run[i][1],mx,my); }
        x.lineTo(run[run.length-1][0],run[run.length-1][1]);
        x.stroke();
      }
      run.length=0;
    };
    for(let i=0;i<n;i++){ const v=s.data[i];
      if(v==null){ flushRun(); continue; }
      run.push([n>1?i/(n-1)*w:0, h-2-(v/max)*(h-8)]); }
    flushRun();
  });
  if(!any){ x.fillStyle='#8b949e'; x.font='italic 12px sans-serif'; x.textAlign='center';
    x.fillText(opts.empty||'no data in selected range', w/2, h/2); }
}
let lastD=null,lastHist=null,lastAl=null;
async function refresh(){
  try{
    const [lr,hr,ar]=await Promise.all([fetch('/api/latest'),fetch('/api/history?range='+range),fetch('/api/alerts')]);
    if(lr.status===404||hr.status===404){
      document.getElementById('status').textContent='waiting for first snapshot…';
      return;
    }
    if(!lr.ok||!hr.ok||!ar.ok) throw 0;
    lastD=await lr.json(); lastHist=await hr.json(); lastAl=await ar.json();
    renderAll();
    document.getElementById('status').textContent='updated '+new Date().toLocaleTimeString();
    document.getElementById('sub').textContent='latest snapshot: '+lastD.timestamp;
  }catch(e){
    document.getElementById('status').innerHTML=
      '<span style="color:var(--red)">error fetching data</span> · <a href="#" onclick="refresh();return false" style="color:var(--accent)">retry</a>';
  }
}
function renderAll(){
  const d=lastD,hist=lastHist,al=lastAl;
  if(!d||!hist) return;
  const set=function(id,v){ document.getElementById(id).innerHTML=v; };
  document.getElementById('cpuBig').innerHTML=(d.cpu&&d.cpu.total_pct!=null?d.cpu.total_pct.toFixed(1):'–')+'<span class="unit">%</span>';
  if(d.memory&&d.memory.pct!=null){
    document.getElementById('ramBig').innerHTML=d.memory.pct.toFixed(1)+'<span class="unit">%</span>';
    document.getElementById('ramBar').style.width=Math.min(d.memory.pct,100)+'%';
  } else {
    document.getElementById('ramBig').innerHTML='–<span class="unit">%</span>';
  }
  const nets=d.networks||[];
  const rx=nets.reduce((a,n)=>a+(n.rx_bps||0),0), tx=nets.reduce((a,n)=>a+(n.tx_bps||0),0);
  document.getElementById('netBig').textContent='↓ '+fmtBps(rx)+'   ↑ '+fmtBps(tx);
  draw('cpuChart',[{data:hist.cpu}],{max:100,fixedMax:true});
  draw('ramChart',[{data:hist.ram,color:'#3fb950'}],{max:100,fixedMax:true});
  draw('netChart',[{data:hist.rx},{data:hist.tx}],{});
  // temperature tile — auto-detected sensors only, never fabricated
  const tc=d.temperature_c;
  const curTemps=Array.isArray(tc)?tc:(tc!=null?[tc]:[]);
  const tbig=document.getElementById('tempBig');
  if(!curTemps.length){ tbig.className='empty'; tbig.style.fontSize='';
    tbig.textContent='no sensor detected'; }
  else { tbig.className='big'; tbig.style.fontSize='26px';
    tbig.textContent=curTemps.map(v=>Number(v).toFixed(0)).join('° / ')+' °C'; }
  const tcolors=['#d29922','#bc8cff','#3fb950','#58a6ff','#f85149'];
  draw('tempChart',(hist.temp&&hist.temp.length?hist.temp:[{data:[]}]).map((s,i)=>
    ({data:s,color:tcolors[i%tcolors.length]})),{empty:'no temperature data in range'});
  document.getElementById('disks').innerHTML=(d.disks||[]).map(k=>
    `<div>${k.mount} — ${k.used_pct.toFixed(1)}% used (${k.free_gb.toFixed(0)} GB free of ${k.total_gb.toFixed(0)} GB)</div>`).join('')||'<span class="empty">no disk data</span>';
  const coresHtml=((d.cpu&&d.cpu.per_core_pct)||[]).map((p,i)=>
    `<div class="core"><span>C${i}</span><div class="barwrap"><div class="barfill" style="width:${Math.min(p,100).toFixed(1)}%"></div></div><span class="coreval">${Math.round(p)}%</span></div>`).join('')||'<span class="empty">waiting for data…</span>';
  set('cores',coresHtml); set('coresDetail',coresHtml);
  document.getElementById('uptime').textContent='uptime: '+fmtUp(d.uptime_secs||0)+' · '+(d.process_count||0)+' processes · '+hist.points.length+' points ('+range+')';
  const procRow=p=>`<tr><td>${p.pid}</td><td>${p.name}</td><td class="num">${(p.cpu_pct||0).toFixed(1)}</td><td class="num">${(p.mem_mb||0).toFixed(0)}</td></tr>`;
  const byCpu=(d.processes&&d.processes.by_cpu)||[];
  const noProc='<tr><td colspan="4" class="empty">waiting for data…</td></tr>';
  set('procs',byCpu.map(procRow).join('')||noProc);
  set('procsLg',byCpu.map(procRow).join('')||noProc);
  const byMem=[...byCpu].sort((a,b)=>(b.mem_mb||0)-(a.mem_mb||0));
  set('memprocs',byMem.map(procRow).join('')||noProc);
  set('procsMem',byMem.map(procRow).join('')||noProc);
  set('ifaces',(nets.length?nets:[{name:'(no interface data)',rx_bps:null,tx_bps:null}]).map(n=>
    `<tr><td>${n.name}</td><td class="num">↓ ${fmtBps(n.rx_bps)}</td><td class="num">↑ ${fmtBps(n.tx_bps)}</td></tr>`).join(''));
  draw('cpuChartLg',[{data:hist.cpu}],{max:100,fixedMax:true});
  draw('ramChartLg',[{data:hist.ram,color:'#3fb950'}],{max:100,fixedMax:true});
  draw('netChartLg',[{data:hist.rx},{data:hist.tx}],{});
  document.getElementById('alerts').innerHTML=(al.rules||[]).map(r=>{
    const last=r.last_fired?new Date(r.last_fired).toLocaleTimeString():'never';
    return `<div>${r.metric} ${r.op} ${r.value} for ${r.duration_secs}s — last fired: ${last}</div>`; }).join('')||'no alert rules';
}
let currentTab='overview';
const TABS={overview:'overview',cpu:'cpuPanel',network:'networkPanel',ram:'ramPanel',processes:'processesPanel',logs:'logsPanel'};
function showTab(t){ currentTab=t;
  for(const k in TABS){
    const el=document.getElementById(TABS[k]); if(!el) continue;
    el.style.display=k===t?(k==='overview'?'grid':'block'):'none';
  }
  document.querySelectorAll('nav a').forEach(a=>a.classList.toggle('active',a.id==='tab-'+t));
  if(t==='logs') loadLogs(); else renderAll(); }
async function loadLogs(){
  if(currentTab!=='logs') return;
  const f=document.getElementById('logfilter').value.trim();
  try{
    const r=await fetch('/api/logs?limit=100&filter='+encodeURIComponent(f));
    if(!r.ok) throw 0;
    const d=await r.json();
    document.getElementById('loglines').textContent=(d.lines||[]).join('\n')||'(no matching lines)';
  }catch(e){ document.getElementById('loglines').textContent='error fetching logs'; }
}
setInterval(refresh,5000); setInterval(loadLogs,5000); refresh(); loadLogs();
</script>
</body>
</html>"""


def load_snapshots():
    """Yield parsed snapshot dicts from all JSONL files in LOGS_DIR, oldest first."""
    files = []
    try:
        for name in os.listdir(LOGS_DIR):
            if name.endswith(".jsonl"):
                files.append(os.path.join(LOGS_DIR, name))
    except OSError:
        return
    for path in sorted(files):
        try:
            with open(path, encoding="utf-8", errors="replace") as f:
                for line in f:
                    line = line.strip()
                    if not line:
                        continue
                    try:
                        yield json.loads(line)
                    except (json.JSONDecodeError, ValueError):
                        continue
        except OSError:
            continue


def parse_ts(ts):
    try:
        return datetime.fromisoformat(ts.replace("Z", "+00:00"))
    except (ValueError, AttributeError, TypeError):
        return None


def net_totals(snap):
    rx = sum(n.get("rx_bps") or 0 for n in snap.get("networks") or [])
    tx = sum(n.get("tx_bps") or 0 for n in snap.get("networks") or [])
    return rx, tx


def fmt_rate(bps):
    bps = bps or 0
    if bps >= 1_000_000:
        return "%.1fMB/s" % (bps / 1_000_000)
    if bps >= 1_000:
        return "%.1fKB/s" % (bps / 1_000)
    return "%.0fB/s" % bps


def render_log_line(snap):
    t = "--:--"
    ts = parse_ts(snap.get("timestamp"))
    if ts and ts.tzinfo:
        t = ts.astimezone().strftime("%H:%M")
    cpu = (snap.get("cpu") or {}).get("total_pct")
    ram = (snap.get("memory") or {}).get("pct")
    rx, _ = net_totals(snap)
    ndisks = len(snap.get("disks") or [])
    procs = (snap.get("processes") or {}).get("by_cpu") or []
    parts = [t, "CPU %d%%" % round(cpu or 0), "RAM %d%%" % round(ram or 0),
             "rx %s" % fmt_rate(rx), "%d disk%s" % (ndisks, "s" if ndisks != 1 else "")]
    if procs:
        top = procs[0]
        parts.append("top: %s %dMB" % (top.get("name"), round(top.get("mem_mb") or 0)))
    return " · ".join(parts)


def build_logs(snaps, filt, limit):
    lines = []
    for s in snaps:
        line = render_log_line(s)
        if filt and filt.lower() not in line.lower():
            continue
        lines.append(line)
    return lines[-limit:]


def build_history(snaps, range_key):
    now = datetime.now(timezone.utc)
    deltas = {"10m": timedelta(seconds=600), "15m": timedelta(seconds=900),
              "30m": timedelta(seconds=1800), "1h": timedelta(hours=1),
              "3h": timedelta(seconds=10800), "6h": timedelta(seconds=21600),
              "12h": timedelta(seconds=43200), "1d": timedelta(seconds=86400),
              "3d": timedelta(seconds=259200), "7d": timedelta(seconds=604800)}
    cutoff = now - deltas[range_key]
    bucket_secs = {"10m": None, "15m": None, "30m": None, "1h": None,
                   "3h": None, "6h": 300, "12h": 300, "1d": 300,
                   "3d": 1800, "7d": 1800}[range_key]

    def temp_series(snap):
        """Normalize temperature_c (scalar | list | absent) to a list of floats."""
        tc = snap.get("temperature_c")
        if isinstance(tc, (list, tuple)):
            return [float(t) if isinstance(t, (int, float)) else None for t in tc]
        if isinstance(tc, (int, float)):
            return [float(tc)]
        return []

    pts = []  # (datetime, cpu, ram, rx, tx, temps)
    for s in snaps:
        ts = parse_ts(s.get("timestamp"))
        if not ts or not ts.tzinfo:
            continue
        if ts >= cutoff:
            cpu = (s.get("cpu") or {}).get("total_pct")
            ram = (s.get("memory") or {}).get("pct")
            rx, tx = net_totals(s)
            pts.append((ts, cpu, ram, rx, tx, temp_series(s)))

    labels, cpu_l, ram_l, rx_l, tx_l, temp_l = [], [], [], [], [], []

    def emit(group):
        if not group:
            return
        n = len(group)
        avg = lambda i: sum(p[i] for p in group if p[i] is not None)
        cnt = lambda i: sum(1 for p in group if p[i] is not None)
        labels.append(group[0][0].astimezone(timezone.utc).isoformat())
        cpu_l.append(round(avg(1) / max(cnt(1), 1), 2))
        ram_l.append(round(avg(2) / max(cnt(2), 1), 2))
        rx_l.append(round(avg(3) / max(cnt(3), 1)))
        tx_l.append(round(avg(4) / max(cnt(4), 1)))
        nsensors = max((len(p[5]) for p in group), default=0)
        tavg = []
        for i in range(nsensors):
            vals = [p[5][i] for p in group
                    if i < len(p[5]) and p[5][i] is not None]
            tavg.append(round(sum(vals) / len(vals), 2) if vals else None)
        temp_l.append(tavg)

    if bucket_secs:
        cur, group = None, []
        for p in pts:
            b = int(p[0].timestamp()) // bucket_secs
            if cur is not None and b != cur:
                emit(group)
                group = []
            cur = b
            group.append(p)
        emit(group)
    else:
        if len(pts) > MAX_POINTS:
            step = len(pts) / MAX_POINTS
            pts = [pts[int(i * step)] for i in range(MAX_POINTS)]
        for p in pts:
            labels.append(p[0].astimezone(timezone.utc).isoformat())
            cpu_l.append(p[1]); ram_l.append(p[2]); rx_l.append(p[3]); tx_l.append(p[4])
            temp_l.append(list(p[5]))

    return {"labels": labels, "cpu": cpu_l, "ram": ram_l, "rx": rx_l, "tx": tx_l,
            "temp": temp_l, "points": [{"ts": l} for l in labels]}


class Handler(BaseHTTPRequestHandler):
    def _send(self, code, body, ctype="application/json"):
        self.send_response(code)
        self.send_header("Content-Type", ctype + "; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        url = urlparse(self.path)
        if url.path == "/":
            self._send(200, HTML.encode("utf-8"), "text/html")
        elif url.path == "/api/latest":
            latest = None
            for snap in load_snapshots():
                latest = snap
            if latest is None:
                self._send(404, json.dumps({"error": "no snapshots found"}).encode())
            else:
                self._send(200, json.dumps(latest).encode())
        elif url.path == "/api/history":
            qs = parse_qs(url.query)
            rng = (qs.get("range") or ["1h"])[0]
            valid = ("10m", "15m", "30m", "1h", "3h", "6h",
                     "12h", "1d", "3d", "7d")
            if rng not in valid:
                self._send(400, json.dumps(
                    {"error": "range must be one of " + ",".join(valid)}).encode())
                return
            hist = build_history(load_snapshots(), rng)
            hist.pop("points")
            self._send(200, json.dumps(hist).encode())
        elif url.path == "/api/alerts":
            self._send(200, json.dumps(alerts.state()).encode())
        elif url.path == "/api/logs":
            qs = parse_qs(url.query)
            filt = (qs.get("filter") or [""])[0]
            try:
                limit = int((qs.get("limit") or ["100"])[0])
            except ValueError:
                limit = 100
            limit = max(1, min(limit, 5000))
            lines = build_logs(load_snapshots(), filt, limit)
            self._send(200, json.dumps({"lines": lines}).encode())
        else:
            self._send(404, b'{"error":"not found"}')

    def log_message(self, fmt, *args):
        pass


if __name__ == "__main__":
    alerts.start_alerter()
    srv = ThreadingHTTPServer(("0.0.0.0", PORT), Handler)
    print(f"SYSWATCH_DASHBOARD_READY on port {PORT}", flush=True)
    srv.serve_forever()
