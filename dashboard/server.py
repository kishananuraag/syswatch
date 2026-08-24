#!/usr/bin/env python3
"""syswatch web dashboard — stdlib only."""
import json
import os
from datetime import datetime, timedelta, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse, parse_qs

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
</style>
</head>
<body>
<div><h1>syswatch <span id="status">loading…</span></h1>
<div class="sub" id="sub"></div></div>
<div class="grid">
  <div class="card"><h2>CPU total</h2>
    <div class="big" id="cpuBig">–<span class="unit">%</span></div>
    <canvas id="cpuChart"></canvas></div>
  <div class="card"><h2>Memory</h2>
    <div class="big" id="ramBig">–<span class="unit">%</span></div>
    <div class="barwrap"><div class="barfill" id="ramBar"></div></div>
    <canvas id="ramChart"></canvas></div>
  <div class="card"><h2>Network rx/tx</h2>
    <div class="big" id="netBig" style="font-size:24px">–</div>
    <canvas id="netChart"></canvas></div>
  <div class="card"><h2>Disks / Temperature / Uptime</h2>
    <div class="disks" id="disks"></div>
    <div id="temp" class="temp" style="margin-top:6px"></div>
    <div id="uptime" style="margin-top:6px;color:var(--dim)"></div></div>
  <div class="card" style="grid-column:1/-1"><h2>Top processes by CPU</h2>
    <table><thead><tr><th>PID</th><th>Name</th><th style="text-align:right">CPU %</th><th style="text-align:right">Mem MB</th></tr></thead>
    <tbody id="procs"></tbody></table></div>
</div>
<script>
const RANGES = {'1h':3600e3,'6h':21600e3,'24h':86400e3,'7d':604800e3};
let range = '1h';
function fmtBps(v){ if(v==null) return '–'; const u=['bps','Kbps','Mbps','Gbps']; let i=0;
  while(v>=1000&&i<u.length-1){v/=1000;i++;} return v.toFixed(1)+' '+u[i]; }
function fmtUp(s){ const d=Math.floor(s/86400),h=Math.floor(s%86400/3600),m=Math.floor(s%3600/60);
  return d?`${d}d ${h}h`:h?`${h}h ${m}m`:`${m}m`; }
function draw(id, series, opts){
  const c=document.getElementById(id), dpr=window.devicePixelRatio||1;
  const w=c.clientWidth, h=c.clientHeight;
  c.width=w*dpr; c.height=h*dpr;
  const x=c.getContext('2d'); x.scale(dpr,dpr);
  x.clearRect(0,0,w,h);
  const colors=['#58a6ff','#3fb950','#bc8cff'];
  let max=opts.max||1;
  if(!opts.fixedMax) for(const s of series) for(const v of s.data) if(v!=null&&v>max) max=v;
  for(const s of series){
    x.strokeStyle=s.color||colors[series.indexOf(s)]; x.lineWidth=1.5; x.beginPath();
    const n=s.data.length;
    for(let i=0;i<n;i++){ const v=s.data[i]; if(v==null) continue;
      const px=n>1?i/(n-1)*w:0, py=h-2-(v/max)*(h-8);
      i===0||s.data[i-1]==null ? x.moveTo(px,py) : x.lineTo(px,py); }
    x.stroke(); }
}
async function refresh(){
  try{
    const [lr,hr]=await Promise.all([fetch('/api/latest'),fetch('/api/history?range='+range)]);
    if(!lr.ok||!hr.ok) throw 0;
    const d=await lr.json(), hist=await hr.json();
    document.getElementById('cpuBig').innerHTML=d.cpu.total_pct.toFixed(1)+'<span class="unit">%</span>';
    document.getElementById('ramBig').innerHTML=d.memory.pct.toFixed(1)+'<span class="unit">%</span>';
    document.getElementById('ramBar').style.width=Math.min(d.memory.pct,100)+'%';
    const rx=d.networks.reduce((a,n)=>a+(n.rx_bps||0),0), tx=d.networks.reduce((a,n)=>a+(n.tx_bps||0),0);
    document.getElementById('netBig').textContent='↓ '+fmtBps(rx)+'   ↑ '+fmtBps(tx);
    draw('cpuChart',[{data:hist.cpu}],{max:100,fixedMax:true});
    draw('ramChart',[{data:hist.ram,color:'#3fb950'}],{max:100,fixedMax:true});
    draw('netChart',[{data:hist.rx},{data:hist.tx}],{});
    document.getElementById('disks').innerHTML=(d.disks||[]).map(k=>
      `<div>${k.mount} — ${k.used_pct.toFixed(1)}% used (${k.free_gb.toFixed(0)} GB free of ${k.total_gb.toFixed(0)} GB)</div>`).join('');
    document.getElementById('temp').textContent = d.temperature_c!=null ? '🌡 '+(Array.isArray(d.temperature_c)?d.temperature_c.join(', '):d.temperature_c)+' °C' : '';
    document.getElementById('uptime').textContent='uptime: '+fmtUp(d.uptime_secs||0)+' · '+d.process_count+' processes · '+hist.points.length+' points ('+range+')';
    document.getElementById('procs').innerHTML=(d.processes.by_cpu||[]).map(p=>
      `<tr><td>${p.pid}</td><td>${p.name}</td><td class="num">${p.cpu_pct.toFixed(1)}</td><td class="num">${p.mem_mb.toFixed(0)}</td></tr>`).join('');
    document.getElementById('status').textContent='updated '+new Date().toLocaleTimeString();
    document.getElementById('sub').textContent='latest snapshot: '+d.timestamp;
  }catch(e){ document.getElementById('status').textContent='error fetching data'; }
}
setInterval(refresh,5000); refresh();
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


def build_history(snaps, range_key):
    now = datetime.now(timezone.utc)
    deltas = {"1h": timedelta(hours=1), "6h": timedelta(hours=6),
              "24h": timedelta(hours=24), "7d": timedelta(days=7)}
    cutoff = now - deltas[range_key]
    bucket_secs = {"1h": None, "6h": None,
                   "24h": 300, "7d": 1800}[range_key]

    pts = []  # (datetime, cpu, ram, rx, tx)
    for s in snaps:
        ts = parse_ts(s.get("timestamp"))
        if not ts or not ts.tzinfo:
            continue
        if ts >= cutoff:
            cpu = (s.get("cpu") or {}).get("total_pct")
            ram = (s.get("memory") or {}).get("pct")
            rx, tx = net_totals(s)
            pts.append((ts, cpu, ram, rx, tx))

    labels, cpu_l, ram_l, rx_l, tx_l = [], [], [], [], []

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

    return {"labels": labels, "cpu": cpu_l, "ram": ram_l, "rx": rx_l, "tx": tx_l,
            "points": [{"ts": l} for l in labels]}


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
            if rng not in ("1h", "6h", "24h", "7d"):
                self._send(400, json.dumps({"error": "range must be one of 1h,6h,24h,7d"}).encode())
                return
            hist = build_history(load_snapshots(), rng)
            hist.pop("points")
            self._send(200, json.dumps(hist).encode())
        else:
            self._send(404, b'{"error":"not found"}')

    def log_message(self, fmt, *args):
        pass


if __name__ == "__main__":
    srv = ThreadingHTTPServer(("0.0.0.0", PORT), Handler)
    print(f"SYSWATCH_DASHBOARD_READY on port {PORT}", flush=True)
    srv.serve_forever()
