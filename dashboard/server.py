#!/usr/bin/env python3
"""syswatch web dashboard v2 — live psutil sampler + daily JSONL store + collector history.

Endpoints:
  /                     static/index.html
  /static/*             app.js, style.css, chart.umd.min.js (local copy)
  /api/current          live CPU total+per-core, RAM used/total/pct+swap,
                        top-5 processes by CPU and by RAM, net rates, temps, freq
  /api/history?range=   1m|5m|15m|1h|6h|1d|7d|30d|1y -> {points:[{ts,cpu,ram,...}],
                        series:{cpu,ram,...}, temp_series:[...], sensor_labels:[...]}
                        Returns {empty:true, ...} (no points at all) when the
                        range has zero data so the UI can show a clean
                        "no data in this range yet" state.
  /api/logs             last N events: live sampler ticks + collector service events
                        + alert fires + server lifecycle, newest first

Data sources: psutil for LIVE values; the Rust collector's JSONL snapshots in
SYSWATCH_LOGS (default C:\\ProgramData\\syswatch\\logs) are indexed once in a
background thread and tailed incrementally so /api/history serves his real
3-day history without re-reading 226MB per request.

Persistence (S12.2): the dashboard's own samples are written to a daily-rotating
JSONL file under HISTORY_DIR (default dashboard/history/YYYY-MM-DD.jsonl). Files
older than 7 days are gzipped in-place to keep disk usage sane at 1s cadence
(~1.5 MB/day uncompressed, ~400 KB/day gzipped).

S12.3: /api/history supports ranges from 1m to 1y with auto-downsampling to
~500 points max so 1y of data stays a small JSON payload.

Stdlib only except psutil.
"""
import gzip
import json
import os
import shutil
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
# S12.2: history moved out of data/samples.jsonl into a daily-rotating dir.
HISTORY_DIR = os.environ.get("SYSWATCH_HISTORY",
                             os.path.join(BASE_DIR, "history"))
COLLECTOR_LOGS = os.environ.get("SYSWATCH_LOGS", r"C:\ProgramData\syswatch\logs")

PORT = int(os.environ.get("SYSWATCH_PORT", "8123"))
# S12.2: 1s cadence is the default; env override exists only for tests.
SAMPLE_SECS = float(os.environ.get("SYSWATCH_SAMPLE_SECS", "1"))
# S12.3: ranges from 1m to 1y. "1m" is the only new ultra-short range; long
# ranges (7d/30d/1y) are downsampled server-side to ~500 points max.
RANGES = {"1m": 60, "5m": 300, "15m": 900, "1h": 3600, "6h": 21600,
          "1d": 86400, "7d": 604800, "30d": 2592000, "1y": 31536000}
# Soft target for chart point count per range. Short ranges keep every point
# so 1m of data shows all ~60 ticks; long ranges downsample aggressively.
MAX_POINTS_PER_RANGE = 500

BOOT_TS = datetime.now(timezone.utc).isoformat(timespec="seconds")


# ---------------------------------------------------------------- temperatures
# psutil has no sensors_temperatures() on Windows; try it anyway (future-proof,
# works if this ever runs on Linux), then LibreHardwareMonitor / OpenHardware-
# Monitor WMI namespaces if Kishan installs one. No sensor found -> honest
# "no sensors exposed" everywhere. NEVER fabricate a value.
#
# Direct WMI via pywin32 (win32com.client) instead of a powershell subprocess.
# Why: spawning `powershell -NoProfile -Command ...` every 30 s flashes a
# console window each tick (event 40961, "PowerShell console is starting up").
# The SWbemLocator call is in-process and silent — same Sensor class, same
# fields, but no subprocess flicker and ~10x faster (~5 ms vs ~250 ms cold).
#
# SensorType=2 == "Temperature" in the LHM/OHM WMI schemas (they mirror the
# OpenHardwareMonitor SensorType enum). Name is a string like "CPU Package",
# Value is a float in Celsius.
_WMI_CONNECT_TIMEOUT = 5  # seconds; SWbemLocator.ConnectServer ignores the
                          # timeout arg so we enforce it ourselves below.
_WMI_LOCATOR = None      # lazy-init: import pywin32 only on the WMI path so a
                          # missing-pywin32 box doesn't break psutil sampling.


def _wmi_sensors(namespace):
    """Query <namespace>/Sensor for Temperature entries via direct WMI.

    Returns [(name, value_celsius), ...] — same shape the old powershell path
    produced. Empty list on any failure (namespace missing, no Temperature
    rows, pywin32 not installed, RPC unavailable) — the caller treats [] the
    same regardless of why.
    """
    global _WMI_LOCATOR
    try:
        import win32com.client  # noqa: F401 — deferred so a pywin32-less box
                                # still serves psutil/temps-via-psutil.
        if _WMI_LOCATOR is None:
            _WMI_LOCATOR = win32com.client.Dispatch("WbemScripting.SWbemLocator")
        # namespace is e.g. "root/LibreHardwareMonitor" -> "root\LibreHardwareMonitor"
        ns = namespace.replace("/", "\\")
        svc = _WMI_LOCATOR.ConnectServer(ns)
        items = svc.ExecQuery("SELECT Name,Value FROM Sensor WHERE SensorType=2")
        sensors = []
        for it in items:
            try:
                name = it.Name
                val = it.Value
            except Exception:
                continue
            if isinstance(val, (int, float)):
                sensors.append((name or "?", round(float(val), 1)))
        return sensors
    except Exception:
        # pywintypes.com_error on missing namespace / no provider / RPC down.
        # Anything else (attribute, type) is also fine to swallow — _wmi_sensors
        # is best-effort and the caller already retries with the next namespace.
        return []


_WMI_CACHE = {"ts": 0.0, "sensors": None}
# Cache WMI sensor reads for 5s. Sampling is now 1s, but a WMI query costs
# ~5 ms; hammering it every tick would 5x the sampler CPU. 5s keeps the
# temperature line fresh enough on a 1s chart (one new data point per 5 ticks).
_WMI_TTL = 5.0


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
    """S12.2: legacy single-file store is gone. Kept as a no-op so callers that
    expect it (e.g. main()) still work. The dashboard no longer rolls a JSONL
    file at startup; daily files under HISTORY_DIR are self-managing."""
    return None


# --------------------------------------------------- daily-rotating JSONL store
# Each line of HISTORY_DIR/YYYY-MM-DD.jsonl is one compact sampler tick:
# {"ts","cpu","ram","down_bps","up_bps","temps":[...]}. Files older than
# HISTORY_GZIP_AFTER_DAYS days are gzipped in-place to keep disk usage sane
# (a day of 1s samples is ~1.5 MB uncompressed, ~400 KB gzipped).
HISTORY_GZIP_AFTER_DAYS = 7


def _history_path_for(day):
    """Return the JSONL path for a given datetime (UTC)."""
    name = day.strftime("%Y-%m-%d") + ".jsonl"
    return os.path.join(HISTORY_DIR, name)


def _history_path_for_today():
    return _history_path_for(datetime.now(timezone.utc))


def _gzip_old_files():
    """Compress any *.jsonl in HISTORY_DIR whose date is older than the cutoff
    and isn't already gzipped. Runs once per minute from the sampler loop so
    the cost is negligible."""
    try:
        names = os.listdir(HISTORY_DIR)
    except OSError:
        return
    cutoff = datetime.now(timezone.utc) - timedelta(days=HISTORY_GZIP_AFTER_DAYS)
    for name in names:
        if not name.endswith(".jsonl") or name.startswith("."):
            continue
        # parse YYYY-MM-DD.jsonl -> date
        try:
            day = datetime.strptime(name[:-6], "%Y-%m-%d").replace(tzinfo=timezone.utc)
        except ValueError:
            continue
        if day >= cutoff:
            continue
        src = os.path.join(HISTORY_DIR, name)
        dst = src + ".gz"
        if os.path.exists(dst):
            continue
        try:
            with open(src, "rb") as fin, gzip.open(dst, "wb", compresslevel=6) as fout:
                shutil.copyfileobj(fin, fout)
            os.remove(src)
            log_event("history", "compressed %s -> %s.gz" % (name, name))
        except OSError as e:
            log_event("history", "gzip failed %s: %s" % (name, e))


def sampler_loop(sampler):
    """S12.2: 1s tick (down from 5s). Sample psutil, append a compact JSONL line
    to the daily file, log events, feed alerter, gzip files older than 7 days.

    At 1s cadence this writes ~86,400 lines/day (~1.5 MB). The file is opened
    once and held across ticks to avoid fsync per line. Appends are atomic
    enough for our purposes (the OS may reorder within a single fd, but we
    only ever read whole files via tail_newest())."""
    n = 0
    last_gzip_mono = 0.0
    GZIP_EVERY_SECS = 60.0
    # Open today's file once. If the date flips while we're running, the
    # next _today_path() call returns the new path; we detect the change and
    # reopen. Keeps the hot path free of os.open overhead 86,400x/day.
    current_path = _history_path_for_today()
    f = None
    while True:
        try:
            snap = sampler.sample_once()
            # Roll the file handle if the day changed.
            today = _history_path_for_today()
            if today != current_path:
                if f is not None:
                    f.close()
                current_path = today
                f = None
            if f is None:
                os.makedirs(HISTORY_DIR, exist_ok=True)
                f = open(current_path, "a", encoding="utf-8", buffering=8192)
            line = json.dumps({"ts": snap["ts"], "cpu": snap["cpu"]["total_pct"],
                               "ram": snap["ram"]["pct"],
                               "down_bps": snap["net"]["down_bps"],
                               "up_bps": snap["net"]["up_bps"],
                               "temps": [t["c"] for t in snap["temps_c"]]})
            # Write one JSON line to today's file. flush() pushes bytes
            # to the OS buffer; the OS is responsible for getting them to
            # disk. We do NOT call os.fsync() per tick — on Windows fsync
            # can take 5-7s, which would throttle the 1s sampler. Daily
            # rotation is the durability boundary; the OS buffer survives
            # crashes well enough for an in-memory ring-buffered history.
            try:
                f.write(line)
                f.write("\n")
                f.flush()
            except Exception as _we:
                log_event("sampler", "WRITE_FAIL n=%d: %r" % (n, _we))
                raise
            n += 1
            # Once-a-minute summary so the logs strip stays readable.
            if n % 60 == 1:
                c = snap["cpu"]["total_pct"]
                r = snap["ram"]["pct"]
                t = ("%d sensors" % len(snap["temps_c"])) if snap["temps_c"] else "none"
                log_event("sampler", "tick #%d cpu=%.0f%% ram=%.0f%% temps=%s" % (n, c, r, t))
            # Gzip rotation is cheap; do it once a minute.
            now_mono = time.monotonic()
            if now_mono - last_gzip_mono >= GZIP_EVERY_SECS:
                last_gzip_mono = now_mono
                _gzip_old_files()
            HIST.append_live(snap)
        except Exception as e:
            log_event("sampler", "ERROR: %r" % e)
        # S12.2 heartbeat: append a one-line marker every 60 ticks so we
        # can tell from disk whether sampler_loop is alive even when log_event
        # and f.write are silently broken. Cheap, only every minute.
        if n % 60 == 0 and n > 0:
            try:
                with open(os.path.join(HISTORY_DIR, "_sampler_heartbeat.log"), "a", encoding="utf-8") as hb:
                    hb.write("%s n=%d\n" % (datetime.now(timezone.utc).isoformat(timespec="seconds"), n))
            except Exception:
                pass
        time.sleep(SAMPLE_SECS)


# ------------------------------------------- collector history (JSONL indexer)
# The Rust collector service writes full snapshots to COLLECTOR_LOGS/*.jsonl.
# A background thread indexes them ONCE into memory and then tails the newest
# file incrementally, so /api/history serves days of real history without
# re-reading 200MB+ per request. History grows live from now on.
class HistoryIndex:
    """In-memory ring of compact points from the collector's JSONL snapshots."""

    MAX_POINTS = 120000  # S12.2: at 1s cadence a full day is ~86,400 points; the
                          # collector adds ~7-12k/day on top. 120k keeps ~24h of
                          # live 1s samples + a few days of collector snapshots
                          # in memory for chart history. Bucketize caps chart
                          # points to MAX_POINTS_PER_RANGE regardless.

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
        pts = []
        # S12.2: also index the dashboard's legacy single-file store
        # (dashboard/data/samples.jsonl) so its 4 days of 30s points aren't
        # lost on upgrade to the daily-rotation scheme. The new sampler_loop
        # writes to HISTORY_DIR instead; this branch is a one-time migration
        # path that simply stops contributing once samples.jsonl no longer
        # grows. Collector JSONLs (the authoritative multi-day history) are
        # still the primary source.
        legacy = os.environ.get("SYSWATCH_DATA", os.path.join(BASE_DIR, "data"))
        legacy_path = os.path.join(legacy, "samples.jsonl")
        try:
            names = sorted(n for n in os.listdir(COLLECTOR_LOGS) if n.endswith(".jsonl"))
        except OSError:
            log_event("history", "no collector logs dir: %s" % COLLECTOR_LOGS)
            names = []
        for name in names:
            path = os.path.join(COLLECTOR_LOGS, name)
            got, _ = self._read_new(path)
            pts.extend(got)
            log_event("history", "indexed %s: %d points" % (name, len(got)))
        if os.path.exists(legacy_path):
            try:
                got, _ = self._read_new(legacy_path)
                if got:
                    pts.extend(got)
                    log_event("history", "indexed legacy samples.jsonl: %d points" % len(got))
            except Exception as e:
                log_event("history", "legacy samples.jsonl read error: %s" % e)
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
    """Average raw points into ~MAX_POINTS_PER_RANGE buckets for smooth charts.

    S12.3 change: this now also returns the *raw* count of in-range points so
    the caller can distinguish "empty in this range" (no data was ever
    recorded in the requested window — show the no-data state) from "downsampled
    to sparse buckets" (the range has data, but it falls into fewer than
    MAX_POINTS_PER_RANGE buckets because most of the window is unfilled, e.g.
    a 1y range with only 3 days of data).

    Why this matters: a user clicking 1y on a fresh install should see the
    "no data in this range yet" empty state, not an empty chart that implies
    the chart is broken. But a 1y range with 3 days of history should show a
    small cluster of points representing those 3 days, not the empty state.
    """
    now_e = now.timestamp()
    cutoff_e = now_e - range_secs
    pts = []
    for p in points:
        e = _ts_epoch(p.get("ts"))
        if e is not None and e >= cutoff_e:
            pts.append((e, p))
    if not pts:
        return ([], 0)
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
    return (out, len(pts))


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
        # Count raw in-range points BEFORE bucketing so we can emit empty:true
        # even when the chosen range has data that falls into fewer buckets
        # than the chart can draw (e.g. 1y range with only 3 days of history
        # produces 3 buckets — still real data, just sparse). We compare to
        # the requested range window, not the bucket count.
        cutoff = now.timestamp() - RANGES[rng]
        raw_count = 0
        for p in raw:
            e = _ts_epoch(p.get("ts"))
            if e is not None and e >= cutoff:
                raw_count += 1
        pts, _ = bucketize(raw, RANGES[rng], now)
        labels = [p["ts"] for p in pts]
        # Build per-sensor series from the bucketed points (each point has
        # a parallel `temps` array). Only sensors present in the LAST point
        # are projected out, so labels align with temps[0..n-1] of every
        # earlier point (None for missing). WHY: the chart needs a stable
        # sensor list — using the last point means new sensors appear
        # automatically, and missing values are nulled.
        n_sensors = 0
        for p in reversed(pts):
            if p.get("temps"):
                n_sensors = len(p["temps"])
                break
        temp_series = []
        for i in range(n_sensors):
            temp_series.append([p.get("temps", [None] * n_sensors)[i] for p in pts])
        # Sensor labels are best-effort from sensors.json (sensor research map).
        sensor_labels = []
        try:
            sp = os.path.join(os.path.dirname(os.path.abspath(__file__)), "sensors.json")
            if os.path.exists(sp):
                with open(sp) as f:
                    sm = json.load(f)
                # Preserve insertion order: list-valued fields are label arrays
                for key in ("labels", "names", "sensors"):
                    v = sm.get(key)
                    if isinstance(v, list) and v:
                        sensor_labels = list(v)[:n_sensors]
                        break
        except Exception:
            sensor_labels = []
        # Pad labels if sensors.json had fewer
        while len(sensor_labels) < n_sensors:
            sensor_labels.append("Sensor " + str(len(sensor_labels) + 1))
        # S12.3: when no raw points fell inside the requested window we still
        # return the full response shape but flip `empty: true` so the UI can
        # render the "No data in this range yet" state instead of an empty
        # chart with axes. data_started_iso lets the UI say "data starts
        # <time>" for context (e.g. "data starts Aug 26").
        out = {"range": rng, "labels": labels, "points": pts,
               "series": {"cpu": [p["cpu"] for p in pts],
                          "ram": [p["ram"] for p in pts],
                          "down_bps": [p["down_bps"] for p in pts],
                          "up_bps": [p["up_bps"] for p in pts]},
               "temp_series": temp_series,
               "sensor_labels": sensor_labels,
               "history_sources": {"collector_points": len(raw),
                                   "in_range_points": raw_count,
                                   "downsampled_to": len(pts)}}
        if raw_count == 0:
            out["empty"] = True
            # Earliest data we DO have, so the UI can say "data starts …"
            if raw:
                first_iso = raw[0].get("ts")
                if first_iso:
                    out["data_started_iso"] = first_iso
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