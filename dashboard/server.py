#!/usr/bin/env python3
"""syswatch web dashboard v2 — live psutil sampler + rolling JSONL store + collector history.

Endpoints:
  /                     static/index.html
  /static/*             app.js, style.css, chart.umd.min.js (local copy)
  /api/current          live CPU total+per-core, RAM used/total/pct+swap,
                        top-5 processes by CPU and by RAM, net rates, temps, freq
  /api/history?range=   10m|15m|1h|2d|5d|7d -> {points:[{ts,cpu,ram,...}], sensors:[...]}
  /api/logs             last N events: live sampler ticks + collector service events
                        + alert fires + server lifecycle, newest first

Data sources: psutil for LIVE values; the Rust collector's JSONL snapshots in
SYSWATCH_LOGS (default C:\\ProgramData\\syswatch\\logs) are indexed once in a
background thread and tailed incrementally so /api/history serves his real
3-day history without re-reading 226MB per request. The sampler also appends
every tick to dashboard/data/samples.jsonl (rolling, MAX_STORE_LINES).

Stdlib only except psutil.
"""
import json
import os
import threading
import time
from datetime import datetime, timedelta, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse, parse_qs

import alerts

try:
    import psutil
except ImportError:
    psutil = None

BASE_DIR = os.path.dirname(os.path.abspath(__file__))
STATIC_DIR = os.path.join(BASE_DIR, "static")
DATA_DIR = os.environ.get("SYSWATCH_DATA", os.path.join(BASE_DIR, "data"))
STORE_PATH = os.path.join(DATA_DIR, "samples.jsonl")
COLLECTOR_LOGS = os.environ.get("SYSWATCH_LOGS", r"C:\ProgramData\syswatch\logs")

PORT = int(os.environ.get("SYSWATCH_PORT", "8123"))
SAMPLE_SECS = float(os.environ.get("SYSWATCH_SAMPLE_SECS", "5"))
MAX_STORE_LINES = 17280  # ~24h at 5s; rolled on startup
RANGES = {"10m": 600, "15m": 900, "1h": 3600, "2d": 172800, "5d": 432000, "7d": 604800}
MAX_POINTS_PER_RANGE = 480

BOOT_TS = datetime.now(timezone.utc).isoformat(timespec="seconds")


# ---------------------------------------------------------------- temperatures
# psutil has no sensors_temperatures() on Windows; try it anyway (future-proof,
# works if this ever runs on Linux), then LibreHardwareMonitor / OpenHardware-
# Monitor WMI namespaces if Kishan installs one. No sensor found -> honest
# "no sensors exposed" everywhere. NEVER fabricate a value.
def _wmi_sensors(namespace):
    """Query <namespace>/Sensor for Temperature entries via powershell CIM cmdlets."""
    import subprocess
    cmd = ("Get-CimInstance -Namespace %s -ClassName Sensor -ErrorAction Stop "
           "| Where-Object { $_.SensorType -eq 'Temperature' } "
           "| Select-Object Name,Value | ConvertTo-Json -Compress" % namespace)
    r = subprocess.run(["powershell", "-NoProfile", "-Command", cmd],
                       capture_output=True, text=True, timeout=20)
    out = (r.stdout or "").strip()
    if not out or r.returncode != 0:
        return []
    data = json.loads(out)
    if isinstance(data, dict):
        data = [data]
    sensors = []
    for s in data:
        name, val = s.get("Name"), s.get("Value")
        if isinstance(val, (int, float)):
            sensors.append((name or "?", round(float(val), 1)))
    return sensors


_WMI_CACHE = {"ts": 0.0, "sensors": None}
_WMI_TTL = 30.0  # seconds; a PowerShell spawn per sample tick is too heavy


def read_temperatures():
    """Return list of {'label','c'} for every real temp sensor found, else []."""
    now = time.monotonic()
    if _WMI_CACHE["sensors"] is not None and now - _WMI_CACHE["ts"] < _WMI_TTL:
        return list(_WMI_CACHE["sensors"])
    found = []
    if psutil is not None and hasattr(psutil, "sensors_temperatures"):
        try:
            for chip, entries in (psutil.sensors_temperatures() or {}).items():
                for i, e in enumerate(entries):
                    label = e.label or ("%s %d" % (chip, i))
                    if e.current is not None:
                        found.append({"label": "%s/%s" % (chip, label),
                                      "c": round(float(e.current), 1)})
        except Exception:
            pass
    if not found:
        for ns in ("root/LibreHardwareMonitor", "root/OpenHardwareMonitor"):
            try:
                for name, val in _wmi_sensors(ns):
                    found.append({"label": name, "c": val})
            except Exception:
                continue
            if found:
                break
    _WMI_CACHE["ts"] = now
    _WMI_CACHE["sensors"] = list(found)
    return found


# ------------------------------------------------------------- events (logs UI)
EVENT_LOCK = threading.Lock()
EVENTS = []  # newest-first ring: {"ts","src","msg"}
MAX_EVENTS = 500


def log_event(src, msg):
    with EVENT_LOCK:
        EVENTS.insert(0, {"ts": datetime.now(timezone.utc).isoformat(timespec="seconds"),
                          "src": src, "msg": str(msg)[:300]})
        del EVENTS[MAX_EVENTS:]


# ------------------------------------------------------- live sampler (psutil)
class Sampler:
    """Samples psutil every SAMPLE_SECS; keeps live snapshot + appends to store."""

    PROC_SCAN_SECS = 60.0  # WHY: process_iter over ~350 procs costs 4-5s on Windows;
                           # scanning every tick made the sampler thread ~97% busy at idle.

    def __init__(self):
        self.lock = threading.Lock()
        self.current = None
        self._last_net = None  # (monotonic, {iface: (bytes_sent, bytes_recv)})
        self._first_proc_scan = True
        self._proc_cache = ([], [], 0)
        self._last_proc_scan = 0.0

    @staticmethod
    def _scan_processes():
        """Top-5 process lists by CPU% and by RSS. First pass primes cpu_percent."""
        rows = []
        skip = {"system idle process", "memory compression"}
        for p in psutil.process_iter(attrs=["pid", "name", "cpu_percent",
                                            "memory_info"]):
            try:
                inf = p.info
                mi = inf.get("memory_info")
                rows.append({"pid": inf["pid"], "name": inf.get("name") or "?",
                             "cpu_pct": inf.get("cpu_percent") or 0.0,
                             "mem_mb": round((mi.rss if mi else 0) / 1048576, 1)})
            except (psutil.NoSuchProcess, psutil.AccessDenied):
                continue
        rows = [r for r in rows if r["name"].lower() not in skip]
        by_cpu = sorted(rows, key=lambda r: -r["cpu_pct"])[:5]
        by_mem = sorted(rows, key=lambda r: -r["mem_mb"])[:5]
        return by_cpu, by_mem, len(rows)

    # -- helpers ------------------------------------------------------------
    @staticmethod
    def _net_rates(before, after, dt):
        rates = []
        for name, st in after.items():
            prev = before.get(name) if before else None
            if not prev:
                continue
            down = max(0.0, (st.bytes_recv - prev[1]) / dt)
            up = max(0.0, (st.bytes_sent - prev[0]) / dt)
            if name.lower().startswith(("lo", "loopback")):
                continue
            rates.append({"name": name, "down_bps": round(down, 1),
                          "up_bps": round(up, 1)})
        return sorted(rates, key=lambda r: -(r["down_bps"] + r["up_bps"]))[:5]

    def sample_once(self):
        vm = psutil.virtual_memory()
        swap = psutil.swap_memory()
        freq = None
        try:
            f = psutil.cpu_freq()
            freq = round(f.current) if f else None
        except Exception:
            pass
        temps = read_temperatures()
        per_core = psutil.cpu_percent(interval=None, percpu=True)
        now_mono2 = time.monotonic()
        if self._first_proc_scan or (now_mono2 - self._last_proc_scan) >= self.PROC_SCAN_SECS:
            self._proc_cache = self._scan_processes()
            self._last_proc_scan = now_mono2
            self._first_proc_scan = False
        by_cpu, by_mem, proc_count = self._proc_cache
        now_mono = time.monotonic()
        net2 = psutil.net_io_counters(pernic=True)
        with self.lock:
            prev_t, prev_io = self._last_net or (None, None)
            rates = (self._net_rates(prev_io, net2, now_mono - prev_t)
                     if prev_io and now_mono - prev_t >= 1 else [])
            self._last_net = (now_mono, net2)
            snap = {
                "ts": datetime.now(timezone.utc).isoformat(timespec="seconds"),
                "source": "psutil",
                "cpu": {"total_pct": psutil.cpu_percent(interval=None),
                        "per_core_pct": [round(p, 1) for p in per_core],
                        "freq_mhz": freq},
                "ram": {"used_bytes": vm.used, "total_bytes": vm.total,
                        "pct": vm.percent,
                        "swap_used_bytes": swap.used,
                        "swap_total_bytes": swap.total},
                "net": {"ifaces": rates,
                        "down_bps": round(sum(r["down_bps"] for r in rates), 1),
                        "up_bps": round(sum(r["up_bps"] for r in rates), 1)},
                "temps_c": temps,
                "processes": {"by_cpu": by_cpu, "by_mem": by_mem},
                "proc_count": proc_count,
                "boot_ts": BOOT_TS,
            }
            # legacy alias so existing alert rules (metrics read "memory") work
            snap["memory"] = snap["ram"]
            self.current = snap
        return snap


def roll_store():
    """Keep only the newest MAX_STORE_LINES lines of the rolling store."""
    try:
        with open(STORE_PATH, encoding="utf-8", errors="replace") as f:
            lines = [l for l in f.read().splitlines() if l.strip()]
    except OSError:
        return
    if len(lines) > MAX_STORE_LINES:
        tmp = STORE_PATH + ".tmp"
        with open(tmp, "w", encoding="utf-8") as f:
            f.write("\n".join(lines[-MAX_STORE_LINES:]) + "\n")
        os.replace(tmp, STORE_PATH)


def sampler_loop(sampler):
    """5s tick: sample psutil, append to store, log events, feed alerter."""
    n = 0
    while True:
        try:
            snap = sampler.sample_once()
            with open(STORE_PATH, "a", encoding="utf-8") as f:
                f.write(json.dumps({"ts": snap["ts"], "cpu": snap["cpu"]["total_pct"],
                                    "ram": snap["ram"]["pct"],
                                    "down_bps": snap["net"]["down_bps"],
                                    "up_bps": snap["net"]["up_bps"],
                                    "temps": [t["c"] for t in snap["temps_c"]]}) + "\n")
            n += 1
            if n % 12 == 1:  # ~once a minute
                c = snap["cpu"]["total_pct"]
                r = snap["ram"]["pct"]
                t = ("%d sensors" % len(snap["temps_c"])) if snap["temps_c"] else "none"
                log_event("sampler", "tick #%d cpu=%.0f%% ram=%.0f%% temps=%s" % (n, c, r, t))
            HIST.append_live(snap)
        except Exception as e:
            log_event("sampler", "ERROR: %r" % e)
        time.sleep(SAMPLE_SECS)


# ------------------------------------------- collector history (JSONL indexer)
# The Rust collector service writes full snapshots to COLLECTOR_LOGS/*.jsonl.
# A background thread indexes them ONCE into memory and then tails the newest
# file incrementally, so /api/history serves days of real history without
# re-reading 200MB+ per request. History grows live from now on.
class HistoryIndex:
    """In-memory ring of compact points from the collector's JSONL snapshots."""

    MAX_POINTS = 86400  # ~48h at the collector's ~2s cadence; ~30MB RAM

    def __init__(self):
        self.lock = threading.Lock()
        self.pts = []          # oldest-first dicts: ts,cpu,ram,down_bps,up_bps
        self.offsets = {}      # file name -> bytes already consumed
        self._partial = {}     # file name -> incomplete trailing line
        self.index_done = threading.Event()  # tail must not race the full pass

    def _parse(self, line):
        try:
            s = json.loads(line)
        except ValueError:
            return None
        try:
            cpu = s["cpu"]["total_pct"]
            ram = s["memory"]["pct"]
            nets = s.get("networks") or []
            down = round(sum(n.get("rx_bps") or 0 for n in nets), 1)
            up = round(sum(n.get("tx_bps") or 0 for n in nets), 1)
        except (KeyError, TypeError):
            return None  # legacy schema without per-core/memory dict
        temps = s.get("temperature_c")
        if isinstance(temps, (int, float)):
            temps = [temps]
        elif not isinstance(temps, list):
            temps = []
        return {"ts": s.get("timestamp"), "cpu": cpu, "ram": ram,
                "down_bps": down, "up_bps": up,
                "temps": [t for t in temps if isinstance(t, (int, float))]}

    def index_all(self):
        """Full pass over existing files (background thread; may take a while)."""
        try:
            self._index_all_run()
        finally:
            # always release the tail loop, even on failure or missing dir
            self.index_done.set()

    def _index_all_run(self):
        try:
            names = sorted(n for n in os.listdir(COLLECTOR_LOGS) if n.endswith(".jsonl"))
        except OSError:
            log_event("history", "no collector logs dir: %s" % COLLECTOR_LOGS)
            return
        pts = []
        for name in names:
            path = os.path.join(COLLECTOR_LOGS, name)
            got, _ = self._read_new(path)
            pts.extend(got)
            log_event("history", "indexed %s: %d points" % (name, len(got)))
        pts.sort(key=lambda p: p["ts"] or "")
        with self.lock:
            self.pts = pts[-self.MAX_POINTS:]
        self.index_done.set()
        log_event("history", "full index done: %d points total" % len(self.pts))

    def _read_new(self, path):
        """Read bytes after last offset; return (points, lines_read). Update offsets.

        Handles the trailing partial line of a file being actively written."""
        name = os.path.basename(path)
        try:
            size = os.path.getsize(path)
        except OSError:
            return [], 0
        with self.lock:
            off = self.offsets.get(name, 0)
            if off > size:      # rotated/truncated -> re-read whole file
                off = 0
                self.offsets[name] = 0
            if size == off:
                return [], 0
        try:
            with open(path, "rb") as f:
                f.seek(off)
                raw = f.read()
        except OSError as e:
            log_event("history", "read error %s: %s" % (name, e))
            return [], 0
        got = []
        data = raw.decode("utf-8", errors="replace")
        lines = data.split("\n")
        tail = None
        if not data.endswith("\n"):
            tail = lines.pop()
        head = self._partial.pop(name, None)
        if head is not None:
            if lines:
                lines[0] = head + lines[0]
            else:
                tail = head + (tail or "")
        if tail is not None:
            self._partial[name] = tail
        else:
            self._partial.pop(name, None)
        for line in lines:
            p = self._parse(line)
            if p and p["ts"]:
                got.append(p)
        with self.lock:
            self.offsets[name] = off + len(raw)
        return got, len(lines)

    def tail_newest(self):
        """Incremental: consume only new bytes of the two newest JSONL files."""
        if not self.index_done.is_set():
            return  # full index still running; it owns the offsets right now
        try:
            names = sorted(n for n in os.listdir(COLLECTOR_LOGS) if n.endswith(".jsonl"))
        except OSError:
            return
        fresh = []
        for name in names[-2:]:
            got, _ = self._read_new(os.path.join(COLLECTOR_LOGS, name))
            fresh.extend(got)
        if fresh:
            fresh.sort(key=lambda p: p["ts"] or "")
            with self.lock:
                merged = self.pts + fresh
                merged.sort(key=lambda p: p["ts"] or "")
                self.pts = merged[-self.MAX_POINTS:]

    def tail_loop(self):
        while True:
            self.tail_newest()
            time.sleep(15)

    def append_live(self, snap):
        """Feed a dashboard-sampler tick into the ring (keeps charts live
        even if the collector service is stopped)."""
        p = {"ts": snap["ts"],
             "cpu": snap["cpu"]["total_pct"],
             "ram": snap["ram"]["pct"],
             "down_bps": snap["net"]["down_bps"],
             "up_bps": snap["net"]["up_bps"],
             "temps": [t["c"] for t in (snap.get("temps_c") or [])]}
        with self.lock:
            self.pts.append(p)
            extra = len(self.pts) - self.MAX_POINTS
            if extra > 0:
                del self.pts[:extra]


# ------------------------------------------------------------- /api/history
def _ts_epoch(ts):
    """ISO timestamp -> unix epoch (handles 'Z' and '+00:00' forms); None on junk."""
    try:
        return datetime.fromisoformat(ts.replace("Z", "+00:00")).timestamp()
    except (ValueError, AttributeError, TypeError):
        return None


def bucketize(points, range_secs, now):
    """Average raw points into ~MAX_POINTS_PER_RANGE buckets for smooth charts."""
    now_e = now.timestamp()
    cutoff_e = now_e - range_secs
    pts = []
    for p in points:
        e = _ts_epoch(p.get("ts"))
        if e is not None and e >= cutoff_e:
            pts.append((e, p))
    if not pts:
        return []
    n_buckets = min(MAX_POINTS_PER_RANGE, max(1, len(pts)))
    span = range_secs / n_buckets
    buckets = {}
    for e, p in pts:
        idx = int((e - cutoff_e) // span)
        b = buckets.setdefault(idx, {})
        for k in ("cpu", "ram", "down_bps", "up_bps"):
            v = p.get(k)
            if isinstance(v, (int, float)):
                b[k] = b.get(k, 0.0) + v
                b["c" + k] = b.get("c" + k, 0) + 1
        for i, t in enumerate(p.get("temps") or []):
            if isinstance(t, (int, float)):
                key = "t%d" % i
                b[key] = b.get(key, 0.0) + t
                b["ct" + str(i)] = b.get("ct" + str(i), 0) + 1
    out = []
    for idx in sorted(buckets):
        b = buckets[idx]

        def avg(k):
            c = b.get("c" + k, 0)
            return round(b[k] / c, 2) if c else None

        ts = datetime.fromtimestamp(cutoff_e + (idx + 0.5) * span, tz=timezone.utc)
        temps = []
        i = 0
        while ("ct%d" % i) in b:
            c = b["ct%d" % i]
            temps.append(round(b["t%d" % i] / c, 2) if c else None)
            i += 1
        out.append({"ts": ts.isoformat(), "cpu": avg("cpu"), "ram": avg("ram"),
                    "down_bps": avg("down_bps"), "up_bps": avg("up_bps"),
                    "temps": temps})
    return out


# ------------------------------------------------------------------ HTTP layer
HIST = HistoryIndex()
SAMPLER = Sampler() if psutil else None


class Handler(BaseHTTPRequestHandler):
    def _send(self, code, body, ctype="application/json"):
        self.send_response(code)
        self.send_header("Content-Type", ctype + "; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        try:
            self.wfile.write(body)
        except (ConnectionAbortedError, BrokenPipeError):
            pass

    def _json(self, obj, code=200):
        self._send(code, json.dumps(obj).encode("utf-8"))

    def do_GET(self):
        url = urlparse(self.path)
        route = url.path
        if route == "/":
            return self._serve_file("index.html", "text/html")
        if route.startswith("/static/"):
            return self._serve_file(route[len("/static/"):], None)
        try:
            if route == "/api/current":
                return self.api_current()
            if route == "/api/history":
                return self.api_history(parse_qs(url.query))
            if route == "/api/logs":
                return self.api_logs(parse_qs(url.query))
            if route == "/api/alerts":
                return self._json(alerts.state())
        except Exception as e:
            log_event("http", "%s failed: %r" % (route, e))
            import traceback
            traceback.print_exc()
            return self._json({"error": repr(e)}, 500)
        self._json({"error": "not found"}, 404)

    def _serve_file(self, rel, ctype):
        path = os.path.normpath(os.path.join(STATIC_DIR, rel))
        if not path.startswith(STATIC_DIR):  # no traversal
            return self._send(403, b'{"error":"forbidden"}')
        types = {".html": "text/html", ".js": "text/javascript",
                 ".css": "text/css", ".map": "application/json",
                 ".svg": "image/svg+xml"}
        ctype = ctype or types.get(os.path.splitext(path)[1], "application/octet-stream")
        try:
            with open(path, "rb") as f:
                self._send(200, f.read(), ctype)
        except OSError:
            self._send(404, b'{"error":"not found"}')

    def api_current(self):
        snap = SAMPLER.current if SAMPLER else None
        if not snap:
            return self._json({"error": "first sample in progress"}, 503)
        self._json(snap)

    def api_history(self, qs):
        rng = (qs.get("range") or ["1h"])[0]
        if rng not in RANGES:
            return self._json(
                {"error": "range must be one of " + ",".join(RANGES)}, 400)
        now = datetime.now(timezone.utc)
        with HIST.lock:
            raw = list(HIST.pts)
        pts = bucketize(raw, RANGES[rng], now)
        labels = [p["ts"] for p in pts]
        out = {"range": rng, "labels": labels, "points": pts,
               "series": {"cpu": [p["cpu"] for p in pts],
                          "ram": [p["ram"] for p in pts],
                          "down_bps": [p["down_bps"] for p in pts],
                          "up_bps": [p["up_bps"] for p in pts]},
               "temp_series": {}, "sensor_labels": [],
               "history_sources": {"collector_points": len(raw)}}
        self._json(out)

    def api_logs(self, qs):
        try:
            limit = int((qs.get("limit") or ["50"])[0])
        except ValueError:
            limit = 50
        limit = max(1, min(limit, MAX_EVENTS))
        src_filter = (qs.get("src") or [""])[0].lower()
        text_filter = ((qs.get("filter") or [""])[0]).lower()
        with EVENT_LOCK:
            evs = list(EVENTS)
        out = []
        for e in evs:
            if src_filter and src_filter not in e["src"].lower():
                continue
            if text_filter and text_filter not in e["msg"].lower():
                continue
            out.append(e)
            if len(out) >= limit:
                break
        self._json({"events": out, "count": len(EVENTS)})

    def log_message(self, fmt, *args):
        pass


def main():
    os.makedirs(DATA_DIR, exist_ok=True)
    roll_store()
    alerts.start_alerter(source=lambda: SAMPLER.current if SAMPLER else None)

    # mirror alert fires into the events ring for the Logs tile
    orig_fire = alerts.Alerter._fire

    def _fire_with_event(self, rule, value):
        log_event("alerts", "FIRE %s %s %s current=%s" %
                  (rule.get("metric"), rule.get("op"), rule.get("value"),
                   round(value, 1)))
        orig_fire(self, rule, value)

    alerts.Alerter._fire = _fire_with_event

    threading.Thread(target=sampler_loop, args=(SAMPLER,),
                     name="syswatch-sampler", daemon=True).start()
    threading.Thread(target=HIST.index_all, name="syswatch-history",
                     daemon=True).start()
    threading.Thread(target=HIST.tail_loop, name="syswatch-tail",
                     daemon=True).start()

    srv = ThreadingHTTPServer(("0.0.0.0", PORT), Handler)
    log_event("server", "dashboard v2 ready on port %d" % PORT)
    print("SYSWATCH_DASHBOARD_READY on port %d" % PORT, flush=True)
    srv.serve_forever()


if __name__ == "__main__":
    main()

