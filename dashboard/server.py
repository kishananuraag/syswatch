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
  /api/agent-events     T4b: JSON array of the last N lines of
                        .hermes/run/events.jsonl (tail read; missing -> [])
  /api/agent-stream     T4b: SSE (text/event-stream) tail of the same log:
                        {"kind":"hello"} on connect, then one data: <line>
                        per line appended (survives delete/rotate)
  /api/consumers        T14C: the full ranked consumer list for one module and
                        one range (module=cpu|memory|gpu|disk|network|power|all,
                        range=<RANGES>, by=process|name, opt-in offset/limit).
                        Every row carries the module's own metric plus count,
                        range, window_start/window_end and the aggregation.
                        pid=<n>&series=1 returns that process's own series.
  /api/permissions      T15: every privilege the system can use — granted or
                        not, what it unlocks, how to grant it, and the root
                        model in one sentence. ?probe=0 skips the osascript AX
                        probe (the cheap call the tab badge uses).
  /api/apps             T17B: the application cut of the same range store —
                        one row per app name (avg/peak CPU %, peak RSS, energy
                        Wh, disk R/W MB, network MB, share of total, a bucketed
                        energy sparkline and each metric's peak moment).
                        ?range=<RANGES>. No limit param: the whole list.
  /api/apps/series      T17B: one app's own CPU / memory / energy / disk /
                        network series + peaks + coverage (?range=&name=).
  /api/battery          T17B: pack state, capacities (ioreg, cached 24 h),
                        life at current draw, and the degradation outlook built
                        from our own 30-min battery history (or "insufficient
                        data" plus the reason).
  POST /api/permissions/grant?id=   ONE macOS admin prompt installs the root
                        powermetrics LaunchDaemon (idempotent; no prompt when
                        already granted). No password is ever read by us.
  POST /api/permissions/revoke?id=  one prompt removes it again.

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
import bisect
import json
import math
import os
import plistlib
import re
import select
import shlex
import shutil
import hashlib
import shutil
import signal
import socket
import statistics
import struct
import subprocess
import sys
import threading
import time
from datetime import datetime, timedelta, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import socketserver
from urllib.parse import urlparse, parse_qs
from urllib.request import urlopen

import alerts
import sensors

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
# T14: the collector's log dir is platform-specific. On this Mac the Rust
# agent writes to ~/Library/Application Support/syswatch/logs; the Windows
# default is kept for the other machine. Without this, by-energy attribution
# and collector history were silently empty on macOS.
COLLECTOR_LOGS = os.environ.get("SYSWATCH_LOGS") or (
    os.path.expanduser("~/Library/Application Support/syswatch/logs")
    if sys.platform == "darwin" else r"C:\ProgramData\syswatch\logs")

PORT = int(os.environ.get("SYSWATCH_PORT", "8123"))
# S12.2: 1s cadence is the default; env override exists only for tests.
SAMPLE_SECS = float(os.environ.get("SYSWATCH_SAMPLE_SECS", "1"))

# T5: fixture replay mode. When SYSWATCH_FIXTURE points at a merged JSONL
# corpus (e.g. fixtures/istat/sample.jsonl), /api/current and /api/graph
# serve from that file instead of the live psutil collector. Live behaviour
# is unchanged when this variable is absent.
FIXTURE_PATH = os.environ.get("SYSWATCH_FIXTURE")
FIXTURE = None  # set below to a FixtureLoader instance when FIXTURE_PATH is set
# S12.3: ranges from 1m to 1y. "1m" is the only new ultra-short range; long
# ranges (7d/30d/1y) are downsampled server-side to ~500 points max.
RANGES = {"1m": 60, "5m": 300, "15m": 900, "1h": 3600, "6h": 21600,
          "1d": 86400, "7d": 604800, "30d": 2592000, "1y": 31536000}
# Soft target for chart point count per range. Short ranges keep every point
# so 1m of data shows all ~60 ticks; long ranges downsample aggressively.
MAX_POINTS_PER_RANGE = 500


# -------------------------------------------------------- tick ladder (T11)
# Calendar-aware tick positions and labels for every supported range.
# Returns [{"ts": epoch_int, "label": str}, ...] with 5-10 labels.
def _parse_epoch(v):
    """Accept epoch int, float, or ISO string; return epoch int or None."""
    if v is None:
        return None
    try:
        return int(float(v))
    except (TypeError, ValueError):
        pass
    try:
        return int(datetime.fromisoformat(str(v).replace("Z", "+00:00")).timestamp())
    except (ValueError, TypeError):
        return None


def _floor_month(dt):
    """First day of the month containing dt (UTC)."""
    return dt.replace(day=1, hour=0, minute=0, second=0, microsecond=0)


def _add_months(dt, months):
    """Add months to a UTC datetime, clamping day to month end."""
    month = dt.month - 1 + months
    year = dt.year + month // 12
    month = month % 12 + 1
    day = min(dt.day, [31, 29 if (year % 4 == 0 and year % 100 != 0) or (year % 400 == 0) else 28,
                       31, 30, 31, 30, 31, 31, 30, 31, 30, 31][month - 1])
    return dt.replace(year=year, month=month, day=day)


def tick_ladder(range_name, from_ts=None, to_ts=None):
    """Generate time-axis ticks for a range.

    Tick spacing follows the ladder table in docs/ui-analytics-plan.md.
    Day/month ranges align to calendar boundaries; shorter ranges align to
    the spacing interval.  Always emits 5-10 labels.

    T14: labels are LOCAL time (naive datetimes). The axis used to be labelled
    in UTC while every peak chip, record and log line on the page was local,
    so 22:00 on the axis sat next to "04:29 AM" in a card. The epoch values are
    unchanged — only the label text moved to the viewer's clock.
    """
    if range_name not in RANGES:
        return []
    now = datetime.now()
    to_e = _parse_epoch(to_ts)
    to_dt = datetime.fromtimestamp(to_e) if to_e else now
    from_e = _parse_epoch(from_ts)
    if from_e is None:
        from_dt = to_dt - timedelta(seconds=RANGES[range_name])
    else:
        from_dt = datetime.fromtimestamp(from_e)

    # (interval_seconds or special, label format, boundary kind)
    specs = {
        "1m":  (15, "%H:%M:%S", None),
        "5m":  (60, "%H:%M:%S", None),
        "15m": (180, "%H:%M", None),
        "1h":  (600, "%H:%M", None),
        "6h":  (3600, "%H:00", None),
        "1d":  (10800, "%H:00", None),
        "7d":  ("1d", "%a %d", "day"),
        "30d": ("5d", "%d %b", "day"),
        "1y":  ("1mo", "%b", "month"),
    }
    spec = specs[range_name]

    ticks = []
    if spec[2] == "day":
        # Start at the first calendar day boundary >= from_dt.
        cur = from_dt.replace(hour=0, minute=0, second=0, microsecond=0)
        if cur < from_dt:
            cur += timedelta(days=1)
        if spec[0] == "1d":
            while cur <= to_dt:
                ticks.append({"ts": int(cur.timestamp()), "label": cur.strftime(spec[1])})
                cur += timedelta(days=1)
        elif spec[0] == "5d":
            # Anchor to a deterministic 5-day grid: day 1,6,11,16,21,26 of each month.
            while cur <= to_dt:
                if cur.day in (1, 6, 11, 16, 21, 26):
                    ticks.append({"ts": int(cur.timestamp()), "label": cur.strftime(spec[1])})
                    cur += timedelta(days=5)
                else:
                    # Step to the next grid day without emitting.
                    for d in (1, 6, 11, 16, 21, 26):
                        if d > cur.day:
                            cur = cur.replace(day=d)
                            break
                    else:
                        cur = (cur + timedelta(days=5)).replace(day=1)
            # Trim to window and desired density.
            ticks = [t for t in ticks if from_dt.timestamp() <= t["ts"] <= to_dt.timestamp()]
    elif spec[2] == "month":
        cur = _floor_month(from_dt)
        if cur < from_dt:
            cur = _add_months(cur, 1)
        span_years = to_dt.year - from_dt.year
        while cur <= to_dt:
            label = cur.strftime(spec[1])
            if span_years and cur.month == 1:
                label = cur.strftime("%b '%y")
            ticks.append({"ts": int(cur.timestamp()), "label": label})
            cur = _add_months(cur, 1)
    else:
        interval = spec[0]
        # Align start to the previous interval boundary.
        if interval >= 3600:
            start = from_dt.replace(minute=0, second=0, microsecond=0)
        elif interval >= 60:
            start = from_dt.replace(second=0, microsecond=0)
        else:
            start = from_dt.replace(microsecond=0)
        # Rewind to a multiple of interval.
        if interval >= 3600:
            start -= timedelta(hours=start.hour % (interval // 3600))
        elif interval >= 60:
            start -= timedelta(minutes=start.minute % (interval // 60))
        else:
            start -= timedelta(seconds=start.second % interval)
        cur = start
        while cur <= to_dt:
            if cur >= from_dt:
                label = cur.strftime(spec[1])
                # 1d range: day boundary gets a date label instead of HH:00.
                if range_name == "1d" and cur.hour == 0:
                    label = cur.strftime("%d %b")
                ticks.append({"ts": int(cur.timestamp()), "label": label})
            if interval >= 3600:
                cur += timedelta(hours=interval // 3600)
            elif interval >= 60:
                cur += timedelta(minutes=interval // 60)
            else:
                cur += timedelta(seconds=interval)

    # Hard limits from the plan: 5-10 labels in most cases.  A full year has
    # 12 calendar months; the acceptance script allows up to 12, so we cap at
    # 12 rather than aggressively dropping month boundaries.
    if len(ticks) > 12:
        step = math.ceil(len(ticks) / 10)
        ticks = ticks[::step]
    # Ensure at least the endpoints if the window is non-empty.
    if not ticks and from_dt < to_dt:
        ticks = [
            {"ts": int(from_dt.timestamp()), "label": from_dt.strftime(spec[1])},
            {"ts": int(to_dt.timestamp()), "label": to_dt.strftime(spec[1])},
        ]
    return ticks


def _series_values_for_peak(metric, point):
    """Return (key, value) candidates for peak computation for a metric point."""
    if metric == "cpu":
        return [("total", point.get("total")), ("user", point.get("user")), ("system", point.get("system"))]
    if metric == "memory":
        return [("pct", point.get("pct")), ("total_mb", point.get("total_mb")),
                ("wired_mb", point.get("wired_mb")), ("app_mb", point.get("app_mb")),
                ("compressed_mb", point.get("compressed_mb"))]
    if metric == "network":
        # T14: /api/graph now serves down_bps/up_bps (bytes/s) directly, so the
        # peak must be computed on those fields — reading the old *_mbps keys
        # produced a meaningless peak of a field that no longer exists.
        return [("down_bps", point.get("down_bps")), ("up_bps", point.get("up_bps"))]
    if metric == "disk":
        return [("read_bps", point.get("read_bps")), ("write_bps", point.get("write_bps")),
                ("read_iops", point.get("read_iops")), ("used_pct", point.get("used_pct"))]
    if metric == "gpu":
        return [("util_pct", point.get("util_pct")),
                ("util_pct_max", point.get("util_pct_max")),
                ("renderer_pct", point.get("renderer_pct")),
                ("tiler_pct", point.get("tiler_pct"))]
    if metric == "load":
        return [("one", point.get("one")), ("five", point.get("five")), ("fifteen", point.get("fifteen"))]
    if metric == "battery":
        # Charge % only: the battery panel also carries time-to-empty, but the
        # chart plots pct, so a peak taken across two units (percent vs
        # seconds) would have been meaningless.
        return [("pct", point.get("pct"))]
    if metric == "sensors":
        vals = point.get("sensors") or {}
        if isinstance(vals, dict):
            return [(k, v) for k, v in vals.items()]
        return []
    return []


def compute_peak(metric, points):
    """Return {value, ts, is_spike} for a metric series, or None.

    For multi-field metrics the peak is the maximum numeric value across all
    fields.  A spike is defined as max >= median * 1.2 (Almossawi/Datawrapper
    signal-vs-noise rule from the plan).  Missing values are ignored rather
    than treated as zero.
    """
    values = []
    candidates = []
    for p in points:
        ts = p.get("ts")
        for key, val in _series_values_for_peak(metric, p):
            if isinstance(val, (int, float)) and not math.isnan(val):
                values.append(val)
                candidates.append((val, ts, key))
    if not values:
        return None
    values.sort()
    median = statistics.median(values)
    max_val, max_ts, max_key = max(candidates, key=lambda x: x[0])
    is_spike = median > 0 and max_val >= median * 1.2
    return {"value": round(max_val, 3), "ts": _parse_epoch(max_ts),
            "is_spike": bool(is_spike), "field": max_key}


# Format a peak value for display; unit is metric-dependent.
def fmt_peak_value(metric, value):
    if value is None:
        return "unavailable"
    if metric in ("cpu", "gpu", "battery", "memory"):
        return "%.1f%%" % value
    if metric == "network":
        return fmt_bps(value)
    if metric == "disk":
        return fmt_bps(value)
    if metric == "load":
        return "%.2f" % value
    return str(value)


def fmt_bps(n):
    """Bytes/second → the same ladder app.js prints (b/s, KB/s, MB/s, GB/s)."""
    if not isinstance(n, (int, float)):
        return "unavailable"
    if n == 0:
        return "0 b/s"
    units = ["b/s", "KB/s", "MB/s", "GB/s"]
    i = 0
    while n >= 1024 and i < len(units) - 1:
        n /= 1024.0
        i += 1
    return ("%.0f %s" if i == 0 else "%.1f %s") % (n, units[i])


# Primary field per metric — the one the section header and the coverage
# indicator are computed from.  Multi-series metrics (network, disk) carry
# per-field measures in `stats`.
PRIMARY_FIELD = {"cpu": "total", "memory": "pct", "network": "down_mbps",
                 "disk": "read_bps", "gpu": "util_pct", "battery": "pct",
                 "load": "one"}


def metric_measures(metric, points, from_e, to_e):
    """Per-field mean/min/max/p95 + window coverage, computed from the series.

    T14 Part 2: continuous data gets measures, never binned counts.  Every
    number here comes from a sample that exists; a field with no samples is
    omitted rather than reported as 0.  `coverage_pct` is the share of the
    window the samples actually account for, so a flat stretch of a sparse
    series is never mistaken for a quiet machine.
    """
    fields = []
    for key, _v in _series_values_for_peak(metric, points[0] if points else {}):
        fields.append(key)
    if not fields:
        return {}
    stats = {}
    for f in fields:
        vals = [(p.get(f), p.get("ts")) for p in points
                if isinstance(p.get(f), (int, float))]
        if not vals:
            continue
        nums = sorted(v for v, _ in vals)
        n = len(nums)
        p95 = nums[min(n - 1, int(math.ceil(n * 0.95)) - 1)]
        max_v, max_ts = max(vals, key=lambda x: x[0])
        stats[f] = {"n": n, "mean": round(sum(nums) / n, 3),
                    "min": round(nums[0], 3), "max": round(max_v, 3),
                    "max_ts": _parse_epoch(max_ts),
                    "p95": round(p95, 3),
                    "median": round(statistics.median(nums), 3)}
    window_s = max(1, (to_e or 0) - (from_e or 0))
    ts = sorted(p["ts"] for p in points if isinstance(p.get("ts"), (int, float)))
    coverage = 0.0
    if len(ts) >= 2:
        gaps = [b - a for a, b in zip(ts, ts[1:]) if b > a]
        step = statistics.median(gaps) if gaps else 0
        covered = min(window_s, (ts[-1] - ts[0]) + step)
        coverage = round(100.0 * covered / window_s, 1)
    return {"fields": stats, "coverage_pct": coverage,
            "window_s": window_s, "points": len(points),
            "primary": PRIMARY_FIELD.get(metric)}


# Where every series actually comes from — shown in each chart footer so a
# surprising number can be traced to a probe without opening the code.
GRAPH_SOURCES = {
    "cpu": "live 1 s sampler · psutil cpu_percent (+ collector snapshots)",
    "memory": "live 1 s sampler · vm_statistics64 via psutil",
    "network": "live 1 s sampler · per-interface byte deltas",
    "disk": "ioreg IOBlockStorageDriver cumulative bytes → B/s, 5 s sample",
    "gpu": "ioreg IOAccelerator Device Utilization %, 5 s sample",
    "battery": "15 s power ring · ioreg AppleSmartBattery pct + AvgTimeToEmpty",
    "load": "live 1 s sampler · getloadavg",
}
GRAPH_NOTES = {
    "disk": "read/write throughput (B/s) — differentiated counters, not the "
            "cumulative total and not free space",
    "gpu": "Device Utilization % plus the per-bucket max; Neural Engine needs "
           "root (T15)",
    "battery": "charge % and time-to-empty from the same 15 s ring",
}


def _graph_empty_reason(metric, from_e, to_e):
    """Say exactly how much history exists when a window is empty."""
    window = max(1, int((to_e or 0) - (from_e or 0)))
    if metric in ("gpu", "disk"):
        ring = GPU_RING if metric == "gpu" else DISK_RING
        span = ring.available_span()
        age = time.time() - ring.started_ts
        if span is None:
            return ("%s sampler has produced no sample yet (ring up %.0f s, "
                    "cadence %.0f s)" % (metric, age, IO_SAMPLE_SECS))
        return ("%s ring holds %.0f s of samples; this window is %s — no bucket "
                "in range yet" % (metric, span, fmt_span(window)))
    return "no %s samples in this window yet" % metric


def fmt_span(secs):
    if secs < 120:
        return "%.0f s" % secs
    if secs < 7200:
        return "%.0f min" % (secs / 60.0)
    if secs < 172800:
        return "%.1f h" % (secs / 3600.0)
    return "%.1f d" % (secs / 86400.0)


def _decimate(points, keys, cap=MAX_POINTS_PER_RANGE):
    """Average-bucket a metric series down to <=cap points, keeping gaps as gaps.

    T13: /api/graph is polled every 5 s by six cards, and a 7d window over the
    live ring is 120k raw points (~8 MB of JSON).  Bucketing keeps the payload
    flat without inventing values: a bucket with no numeric sample emits null,
    never 0.
    """
    if len(points) <= cap:
        return points
    stride = math.ceil(len(points) / cap)
    out = []
    for i in range(0, len(points), stride):
        chunk = points[i:i + stride]
        merged = {"ts": chunk[0]["ts"]}
        for k in keys:
            vals = [c[k] for c in chunk if isinstance(c.get(k), (int, float))]
            merged[k] = round(sum(vals) / len(vals), 3) if vals else None
        out.append(merged)
    return out


def _live_graph_points(metric, from_e, to_e):
    """Build metric-shaped points from the live history index (no fixture)."""
    now = datetime.now(timezone.utc)
    if to_e is None:
        to_e = int(now.timestamp())
    if from_e is None:
        from_e = to_e - RANGES["1h"]
    if metric == "battery":
        # Battery % and time-to-empty live in the power ring (the dashboard
        # sampler does not read the battery; the 15 s ring does).
        pts = [{"ts": int(p["ts"]), "pct": p.get("pct"),
                "time_remaining_s": p.get("time_remaining_s")}
               for p in POWER_RING.series(to_e - from_e, to_e)
               if isinstance(p.get("pct"), (int, float))
               or isinstance(p.get("time_remaining_s"), (int, float))]
        return pts
    if metric == "gpu":
        # T14: sampled dashboard-side every 5 s from ioreg IOAccelerator.
        # `util_pct` is the bucket mean, `util_pct_max` the bucket peak.
        return GPU_RING.series(to_e - from_e, to_e)
    if metric == "disk":
        # T14 addendum C: the disk chart is read/write B/s, differentiated from
        # the cumulative IOBlockStorageDriver counters. Capacity is a hero
        # number in /api/current.disk, never a chart series.
        return DISK_RING.series(to_e - from_e, to_e)
    with HIST.lock:
        raw = list(HIST.pts)
    pts = []
    for p in raw:
        e = _ts_epoch(p.get("ts"))
        if e is None or e < from_e or e > to_e:
            continue
        if metric == "cpu":
            total = p.get("cpu")
            pts.append({"ts": e, "cpu": total, "total": total})
        elif metric == "memory":
            pct = p.get("ram")
            pts.append({"ts": e, "ram": pct, "pct": pct})
        elif metric == "network":
            down = p.get("down_bps")
            up = p.get("up_bps")
            pts.append({"ts": e,
                        "down_bps": down, "up_bps": up,
                        "down_mbps": round(down / 1e6, 3) if isinstance(down, (int, float)) else None,
                        "up_mbps": round(up / 1e6, 3) if isinstance(up, (int, float)) else None})
        elif metric == "load":
            one = p.get("load1")
            if not isinstance(one, (int, float)):
                continue
            pts.append({"ts": e, "one": one})
        elif metric == "sensors":
            temps = p.get("temps") or []
            pts.append({"ts": e,
                        "sensors": {"Sensor %d" % i: t for i, t in enumerate(temps)
                                    if isinstance(t, (int, float))}})
    pts.sort(key=lambda p: p["ts"])
    pts = _coalesce_same_ts(pts)
    if metric == "cpu":
        return _decimate(pts, ("cpu", "total"))
    if metric == "memory":
        return _decimate(pts, ("ram", "pct"))
    if metric == "network":
        return _decimate(pts, ("down_bps", "up_bps", "down_mbps", "up_mbps"))
    return pts


# ----------------------------------------------------------- power / battery (T11)
# Honest tiers:
#   1. battery watts (V*A) + charging state (unprivileged ioreg/psutil)
#   2. per-app watts from the Rust collector's by_energy (reuse, no recompute)
#   3. CPU/GPU/Neural package power — null when SMC/IOReport is unreadable,
#      never fabricated as 0.

def _ioreg_battery():
    """Read AppleSmartBattery via ioreg; return dict or None."""
    try:
        out = subprocess.check_output(
            ["/usr/sbin/ioreg", "-n", "AppleSmartBattery", "-r", "-a"],
            stderr=subprocess.DEVNULL, timeout=5)
        data = plistlib.loads(out)
        if data and isinstance(data, list):
            return data[0]
    except Exception:
        pass
    return None


def _psutil_battery():
    """Return psutil sensors_battery() as a dict, or None."""
    if psutil is None or not hasattr(psutil, "sensors_battery"):
        return None
    try:
        b = psutil.sensors_battery()
        if b is None:
            return None
        return {"percent": b.percent, "secsleft": b.secsleft,
                "power_plugged": b.power_plugged}
    except Exception:
        return None


def _battery_health_pct(ioreg):
    """Compute health % from ioreg capacity vs design capacity."""
    design = ioreg.get("DesignCapacity")
    # NominalChargeCapacity is the best modern estimate; fall back to MaxCapacity.
    current = ioreg.get("NominalChargeCapacity") or ioreg.get("MaxCapacity")
    if current and design:
        return round(current / design * 100, 1)
    return None


def _battery_condition(health_pct):
    if health_pct is None:
        return "unavailable"
    if health_pct >= 80:
        return "good"
    if health_pct >= 60:
        return "fair"
    return "poor"


def _battery_cells(ioreg):
    """Return list of per-cell voltages (mV) if exposed."""
    bd = ioreg.get("BatteryData") or {}
    cells = bd.get("CellVoltage")
    if isinstance(cells, list) and cells:
        return [int(c) for c in cells]
    return None


def _adapter_watts(details):
    """Charging-port input W from ioreg `AppleRawAdapterDetails`.

    Verified today on battery: the key exists but is an EMPTY array
    (`ExternalConnected = false`), so there is no adapter wattage to report and
    the caller keeps `adapter_w = None`.  On AC the array carries one dict per
    adapter; only keys with an unambiguous unit are used — `Watts` (W) and
    `AdapterPowerTenthWatt` (0.1 W).  A key whose unit is unknown is skipped
    rather than guessed.

    Returns (watts or None, source_text or None).
    """
    best = None
    used = None
    for d in details or []:
        if not isinstance(d, dict):
            continue
        for key, scale in (("Watts", 1.0), ("AdapterPowerTenthWatt", 0.1)):
            v = d.get(key)
            if isinstance(v, (int, float)) and not isinstance(v, bool) and v > 0:
                w = float(v) * scale
                if best is None or w > best:
                    best = round(w, 2)
                    used = "AppleRawAdapterDetails.%s" % key
    return best, used


# T13: 1 s TTL — see _read_battery_state docstring; keep > sampler cadence so
# the two hot call sites share one ioreg spawn per second.
BATTERY_TTL = 1.0
_BATT_CACHE = {"mono": 0.0, "state": None}


def _read_battery_state():
    """Return an honest battery state dict (cached for BATTERY_TTL seconds).

    T13: three call sites now hit this on short cadences — the 1 s sampler, the
    /api/power poll (1 s from the UI) and the 15 s power ring.  Each uncached
    call spawns `ioreg` (~20 ms), so the cache caps it at ~1 spawn/s for the
    whole process instead of 3.
    """
    now = time.monotonic()
    cached = _BATT_CACHE
    if cached["state"] is not None and now - cached["mono"] < BATTERY_TTL:
        return dict(cached["state"])
    state = _read_battery_state_raw()
    cached.update({"mono": now, "state": dict(state)})
    return state


def _read_battery_state_raw():
    """Uncached battery read: ioreg first, psutil as the fallback."""
    state = {
        "pct": None,
        "time_remaining_s": None,
        "charging": None,
        "cycles": None,
        "condition": "unavailable",
        "health_pct": None,
        "cells": None,
        "battery_w": None,
        "temp_c": None,
        "external_connected": None,
        "adapter_w": None,
        "adapter_source": None,
        "adapter_rated_w": None,
        "voltage_v": None,
        "amperage_ma": None,
        "nominal_wh": None,
        "unavailable_reason": None,
    }
    ioreg = _ioreg_battery()
    pbat = _psutil_battery()

    if ioreg is None and pbat is None:
        state["unavailable_reason"] = "no battery API exposed on this machine"
        return state

    if ioreg:
        voltage_mv = ioreg.get("AppleRawBatteryVoltage") or ioreg.get("Voltage")
        amperage_ma = ioreg.get("Amperage")
        if voltage_mv and amperage_ma:
            # mV * mA / 1e6 -> W.  Sign of amperage indicates direction;
            # wattage is reported as a positive draw.
            state["battery_w"] = round(abs(voltage_mv * amperage_ma) / 1e6, 2)
        if isinstance(voltage_mv, (int, float)) and voltage_mv:
            state["voltage_v"] = round(float(voltage_mv) / 1000.0, 3)
        # Pack energy from the gauge's own mAh and the measured voltage: used
        # only to express energy drawn as a fraction of a full pack, never as a
        # substitute for a measurement.
        nominal_mah = ioreg.get("NominalChargeCapacity") or ioreg.get("AppleRawMaxCapacity")
        if isinstance(nominal_mah, (int, float)) and state["voltage_v"]:
            state["nominal_wh"] = round(nominal_mah * state["voltage_v"] / 1000.0, 2)
        if isinstance(amperage_ma, (int, float)):
            state["amperage_ma"] = int(amperage_ma)
        state["external_connected"] = bool(ioreg.get("ExternalConnected"))
        adapter_w, adapter_src = _adapter_watts(ioreg.get("AppleRawAdapterDetails"))
        state["adapter_w"] = adapter_w
        state["adapter_source"] = adapter_src
        rated = ioreg.get("AppleRawAdapterDetails")
        if isinstance(rated, list):
            for d in rated:
                if isinstance(d, dict):
                    for key in ("Watts", "AdapterPowerTenthWatt"):
                        v = d.get(key)
                        if isinstance(v, (int, float)) and v > 0:
                            w = v * (0.1 if key == "AdapterPowerTenthWatt" else 1.0)
                            if state["adapter_rated_w"] is None or w > state["adapter_rated_w"]:
                                state["adapter_rated_w"] = round(w, 1)
        state["charging"] = bool(ioreg.get("IsCharging"))
        state["cycles"] = ioreg.get("CycleCount")
        state["health_pct"] = _battery_health_pct(ioreg)
        state["condition"] = _battery_condition(state["health_pct"])
        cells = _battery_cells(ioreg)
        if cells is not None:
            state["cells"] = cells
        # Unprivileged battery temperature (ioreg Temperature is centi-°C and
        # only present on some packs — absence stays None, never 0 °C).
        raw_temp = ioreg.get("Temperature")
        if isinstance(raw_temp, (int, float)) and raw_temp:
            state["temp_c"] = round(float(raw_temp) / 100.0, 2)
        # Capacity percent from ioreg if psutil missing.
        max_cap = ioreg.get("MaxCapacity")
        cur_cap = ioreg.get("CurrentCapacity")
        if cur_cap and max_cap:
            state["pct"] = round(cur_cap / max_cap * 100, 1)
        time_empty = ioreg.get("AvgTimeToEmpty")
        time_full = ioreg.get("AvgTimeToFull")
        if state["charging"] and time_full not in (None, 65535):
            state["time_remaining_s"] = int(time_full) * 60
        elif not state["charging"] and time_empty not in (None, 65535):
            state["time_remaining_s"] = int(time_empty) * 60

    if pbat:
        if state["pct"] is None:
            state["pct"] = pbat.get("percent")
        if state["charging"] is None:
            state["charging"] = bool(pbat.get("power_plugged"))
        secs = pbat.get("secsleft")
        if secs is not None and secs >= 0 and state["time_remaining_s"] is None:
            state["time_remaining_s"] = int(secs)

    if state["battery_w"] is None:
        state["unavailable_reason"] = "voltage/current not exposed by battery API"
    return state


# T13: the UI now polls /api/power at 1 s; the power ring itself samples at
# 15 s (T20 — it reads the privileged powermetrics sample).
# Reading 1 MB of collector JSONL per call would burn ~1 MB/s of file I/O for a
# snapshot that only changes every ~6 s, so the parse is cached for SNAP_TTL.
SNAP_TTL = 15.0
_SNAP_CACHE = {"mono": 0.0, "key": None, "snap": None}


def _latest_collector_snapshot():
    """Return the most recent Rust collector snapshot dict, or None.

    Reads only the newest .jsonl file in COLLECTOR_LOGS and tails up to
    1 MB so we do not re-read the full multi-day log per request.  Cached for
    SNAP_TTL seconds (keyed on path+mtime+size, so a new file invalidates it
    immediately) because T13's 1 s poll loop calls this on the hot path.
    """
    try:
        names = sorted(n for n in os.listdir(COLLECTOR_LOGS) if n.endswith(".jsonl"))
    except Exception:
        return None
    if not names:
        return None
    path = os.path.join(COLLECTOR_LOGS, names[-1])
    try:
        st = os.stat(path)
        size = st.st_size
        key = (path, st.st_mtime, size)
    except OSError:
        return None
    now = time.monotonic()
    cached = _SNAP_CACHE
    # TTL first: the collector rewrites its file every ~6 s, so an mtime check
    # would invalidate the cache on nearly every call and the UI's 1 s poll
    # would re-read + re-parse 1 MB once a second. A path change (day rollover)
    # still invalidates immediately.
    if cached["snap"] is not None and now - cached["mono"] < SNAP_TTL:
        if cached["key"] and cached["key"][0] == path:
            return cached["snap"]
    try:
        with open(path, "rb") as f:
            f.seek(max(0, size - 1_000_000))
            raw = f.read()
    except OSError:
        return None
    snap = None
    for line in raw.decode("utf-8", errors="replace").splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            snap = json.loads(line)
        except ValueError:
            continue
    cached.update({"mono": now, "key": key, "snap": snap})
    return snap


def _by_app_from_collector(snap):
    """Return top-10 by_energy rows from a collector snapshot, or None."""
    if not snap:
        return None
    procs = snap.get("processes") or {}
    rows = procs.get("by_energy") or []
    out = []
    for r in rows[:10]:
        watts = r.get("watts")
        name = r.get("name", "?")
        pid = r.get("pid")
        if watts is None:
            continue
        out.append({"pid": pid, "name": name, "watts": round(watts, 3)})
    return out or None


def _package_power(snap):
    """Return {cpu_w, gpu_w} from collector energy; null when unavailable.

    The package dict only contains numeric/null values so the contract is
    machine-readable; the UI uses the top-level package_unavailable_reason
    for the human-readable explanation.
    """
    cpu_w = None
    gpu_w = None
    reason = "SMC/IOReport package power unreadable without root on this machine"
    if snap:
        energy = snap.get("energy") or {}
        pkg = energy.get("package_w")
        if isinstance(pkg, (int, float)):
            # Collector exposes a single package total; we cannot honestly split
            # it into CPU vs GPU, so both remain null per the honesty rule.
            reason = "collector provides total package power only; CPU/GPU split unavailable"
    return {"cpu_w": cpu_w, "gpu_w": gpu_w}, reason


# --------------------------------------- root tier file (T15 privileged daemon)
# The privileged daemon (T15) writes ONE JSON snapshot per sample.  The
# dashboard never runs as root and never holds a password: it only READS this
# file.  A file older than ROOT_SAMPLE_STALE_S is treated as ABSENT, so a daemon
# that died cannot leave a stale temperature on screen pretending to be live.
ROOT_SAMPLE_PATH = os.environ.get("SYSWATCH_ROOT_SAMPLE") or os.path.expanduser(
    "~/Library/Application Support/syswatch/root/sample.json")
ROOT_SAMPLE_TTL = 2.0
ROOT_SAMPLE_STALE_S = 30.0
# T20: the powermetrics cadence.  One number owns the whole chain — the root
# daemon's `--interval-ms`, the power/temperature ring below, and the coverage
# line the UI prints ("sampling every 15 s").  powermetrics is a subprocess that
# samples the machine's own counters, so it is the dominant cost of the agent:
# at 5 s it spiked 12×/min, at 15 s 4×/min, which is what brings the total to
# the 1.5–2 % CPU budget.  Override only for a measurement, never in shipping.
POWERMETRICS_INTERVAL_MS = int(os.environ.get("SYSWATCH_POWERMETRICS_MS", "15000"))
POWERMETRICS_SAMPLE_SECS = POWERMETRICS_INTERVAL_MS / 1000.0
_ROOT_CACHE = {"mono": 0.0, "payload": None}
# Fields the T15 daemon emits (brief T15 addendum).  Anything absent stays
# absent: the registry turns an omission into `unavailable` + reason, never 0.
ROOT_NUMERIC_KEYS = ("cpu_temp_c", "gpu_temp_c", "ssd_temp_c", "battery_temp_c",
                     "package_w", "cpu_w", "gpu_w", "ane_w", "ane_util_pct",
                     # T16B — SMC reader aggregates and powermetrics cluster data
                     "cpu_avg_c", "gpu_avg_c", "battery_c", "ssd_c",
                     "gpu_freq_mhz", "gpu_residency_pct")
ROOT_TEXT_KEYS = ("thermal_pressure",)


def _root_temps(v):
    """Normalise the T16B SMC reader output to [{key,label,c}].

    The daemon emits exactly that shape; anything malformed is dropped, so a
    broken reader can never render as 0 °C.
    """
    out = []
    if not isinstance(v, list):
        return out
    for row in v:
        if not isinstance(row, dict):
            continue
        c = row.get("c")
        if not (isinstance(c, (int, float)) and not isinstance(c, bool)):
            continue
        out.append({"key": str(row.get("key") or ""),
                    "label": str(row.get("label") or "sensor"),
                    "c": round(float(c), 2)})
    return out


def _root_cluster(v):
    """Normalise {freq_mhz,residency_pct} → the two numbers or None."""
    if not isinstance(v, dict):
        return None
    out = {}
    for k in ("freq_mhz", "residency_pct"):
        x = v.get(k)
        if isinstance(x, (int, float)) and not isinstance(x, bool):
            out[k] = round(float(x), 2)
    return out or None


def root_grant_state():
    """Describe the grant route honestly, probing whether T15 installed it.

    T15 adds the POST /api/permissions/grant handler.  Until that handler
    exists the UI must not offer a button that 404s, so the probe is dynamic:
    the same call starts working the moment T15 lands, with no edit here.
    """
    installed = hasattr(Handler, "do_POST") if "Handler" in globals() else False
    state = _root_file_probe()
    return {
        "endpoint": "/api/permissions/grant?id=package_power",
        "revoke_endpoint": "/api/permissions/revoke?id=package_power",
        "endpoint_available": bool(installed),
        "granted": bool(state["fresh"]),
        "unlocks": "temperatures · package power · Neural Engine",
        "detail": ("granted through the T15/T19 privileged daemon (syswatch-powerd: "
                   "powermetrics + AppleSMC, one macOS admin prompt); the dashboard "
                   "never runs as root and never holds the password"),
        "how": ("one admin prompt installs the LaunchDaemon %s, which runs the "
                "syswatch-powerd binary (/usr/bin/powermetrics -n 1 -i 5000 -f text "
                "-s thermal,... plus the AppleSMC reader) and writes %s every 15 s"
                % (DAEMON_LABEL, os.path.basename(ROOT_SAMPLE_PATH))),
    }


def _root_cluster_temps(v):
    """Normalise cpu_cluster_temps_c (list or dict) to [{label, c}]."""
    out = []
    if isinstance(v, dict):
        for k in v:
            c = v.get(k)
            if isinstance(c, (int, float)):
                out.append({"label": str(k), "c": round(float(c), 2)})
    elif isinstance(v, list):
        for i, c in enumerate(v):
            label = None
            if isinstance(c, dict):
                label = c.get("label") or c.get("cluster") or c.get("name")
                c = c.get("c", c.get("temp_c"))
            if isinstance(c, (int, float)):
                out.append({"label": label or "cluster %d" % (i + 1),
                            "c": round(float(c), 2)})
    return out


def root_sample():
    """Read the T15 privileged snapshot; never guesses when it is absent."""
    now = time.monotonic()
    cached = _ROOT_CACHE
    if cached["payload"] is not None and now - cached["mono"] < ROOT_SAMPLE_TTL:
        return cached["payload"]
    out = {"available": False, "reason": None, "age_s": None,
           "path": ROOT_SAMPLE_PATH, "data": {}}
    try:
        st = os.stat(ROOT_SAMPLE_PATH)
    except OSError:
        out["reason"] = ("needs root — the privileged daemon has not written %s "
                         "(temperatures, package power and Neural Engine are "
                         "root-only on macOS)" % os.path.basename(ROOT_SAMPLE_PATH))
        cached.update({"mono": now, "payload": out})
        return out
    age = max(0.0, time.time() - st.st_mtime)
    out["age_s"] = round(age, 1)
    if age > ROOT_SAMPLE_STALE_S:
        out["reason"] = ("needs root — last privileged sample is %.0f s old "
                         "(> %.0f s), so its values are withheld rather than "
                         "shown as live" % (age, ROOT_SAMPLE_STALE_S))
        cached.update({"mono": now, "payload": out})
        return out
    try:
        with open(ROOT_SAMPLE_PATH, "rb") as f:
            raw = json.loads(f.read().decode("utf-8", errors="replace"))
    except (OSError, ValueError) as e:
        out["reason"] = "privileged sample unreadable (%s)" % e
        cached.update({"mono": now, "payload": out})
        return out
    data = {}
    if isinstance(raw, dict):
        for k in ROOT_NUMERIC_KEYS + ROOT_TEXT_KEYS:
            v = raw.get(k)
            if k in ROOT_NUMERIC_KEYS:
                if isinstance(v, (int, float)) and not isinstance(v, bool):
                    data[k] = round(float(v), 2)
            elif isinstance(v, str) and v:
                data[k] = v
        clusters = _root_cluster_temps(raw.get("cpu_cluster_temps_c"))
        if clusters:
            data["cpu_cluster_temps_c"] = clusters
        # T16B: SMC sensor rows, prior-sweep aggregates and the powermetrics
        # cluster data — the daemon emits them, the registry renders them.
        temps = _root_temps(raw.get("temps"))
        if temps:
            data["temps"] = temps
        for k in ("e_cluster", "p_cluster"):
            cl = _root_cluster(raw.get(k))
            if cl:
                data[k] = cl
        per_cpu = []
        for c in (raw.get("per_cpu") or []):
            cl = _root_cluster(c)
            if cl:
                per_cpu.append(cl)
        if per_cpu:
            data["per_cpu"] = per_cpu
        if isinstance(raw.get("smc"), dict):
            smc = raw["smc"]
            data["smc"] = {k: smc.get(k) for k in
                           ("ok", "mode", "keys", "discovered", "emitted",
                            "read_failures", "error") if k in smc}
        fans = raw.get("fans_rpm")
        if isinstance(fans, list):
            rpm = [int(x) for x in fans if isinstance(x, (int, float))]
            if rpm:
                data["fans_rpm"] = rpm
        ts = raw.get("ts") or raw.get("timestamp")
        if ts is not None:
            data["ts"] = ts
    if not data:
        out["reason"] = ("privileged sample carries none of the expected keys "
                         "(cpu_temp_c, package_w, thermal_pressure, …)")
        cached.update({"mono": now, "payload": out})
        return out
    out["available"] = True
    out["data"] = data
    cached.update({"mono": now, "payload": out})
    return out


def power_state():
    """Build the full /api/power response."""
    batt = _read_battery_state()
    snap = None if FIXTURE else _latest_collector_snapshot()
    by_app = _by_app_from_collector(snap)
    pkg, pkg_reason = _package_power(snap)
    root = root_sample()
    root_data = root.get("data") or {}
    root_w = root_data.get("package_w") if root.get("available") else None
    if isinstance(root_w, (int, float)):
        pkg = {"cpu_w": root_data.get("cpu_w"),
               "gpu_w": root_data.get("gpu_w"),
               "ane_w": root_data.get("ane_w"),
               "package_w": root_w,
               "source": ("powermetrics via the root daemon file "
                          "(package total = CPU + GPU + ANE)"),
               "age_s": root.get("age_s")}
        pkg_reason = ("root powermetrics sample, %s s old"
                      % root.get("age_s"))
    # The elevated block: every value present here was measured by the root
    # daemon; anything the daemon did not emit stays None with the reason.
    elevated = {
        "available": bool(root.get("available")),
        "age_s": root.get("age_s"),
        "reason": root.get("reason"),
        "file": os.path.basename(ROOT_SAMPLE_PATH),
        "source": ("powermetrics, run by the root LaunchDaemon %s; the "
                   "dashboard only reads the file" % DAEMON_LABEL),
        "package_w": root_w,
        "cpu_w": root_data.get("cpu_w") if root.get("available") else None,
        "gpu_w": root_data.get("gpu_w") if root.get("available") else None,
        "ane_w": root_data.get("ane_w") if root.get("available") else None,
        "cpu_temp_c": root_data.get("cpu_temp_c") if root.get("available") else None,
        "gpu_temp_c": root_data.get("gpu_temp_c") if root.get("available") else None,
        "cpu_cluster_temps_c": root_data.get("cpu_cluster_temps_c") if root.get("available") else None,
        # T16B: SMC sensor rows + real cluster clocks from the same daemon file.
        "temps": root_data.get("temps") if root.get("available") else None,
        "cpu_avg_c": root_data.get("cpu_avg_c") if root.get("available") else None,
        "gpu_avg_c": root_data.get("gpu_avg_c") if root.get("available") else None,
        "battery_c": root_data.get("battery_c") if root.get("available") else None,
        "ssd_c": root_data.get("ssd_c") if root.get("available") else None,
        "e_cluster": root_data.get("e_cluster") if root.get("available") else None,
        "p_cluster": root_data.get("p_cluster") if root.get("available") else None,
        "gpu_freq_mhz": root_data.get("gpu_freq_mhz") if root.get("available") else None,
        "gpu_residency_pct": root_data.get("gpu_residency_pct") if root.get("available") else None,
        "per_cpu": root_data.get("per_cpu") if root.get("available") else None,
        "smc": root_data.get("smc") if root.get("available") else None,
        "thermal_pressure": root_data.get("thermal_pressure") if root.get("available") else None,
        "fans_rpm": root_data.get("fans_rpm") if root.get("available") else None,
        "ane_util_pct": root_data.get("ane_util_pct") if root.get("available") else None,
    }
    return {
        "battery_w": batt.get("battery_w"),
        "charging": batt.get("charging"),
        "external_connected": batt.get("external_connected"),
        "adapter_w": batt.get("adapter_w"),
        "adapter_source": batt.get("adapter_source"),
        "adapter_rated_w": batt.get("adapter_rated_w"),
        "package_w": root_w,
        "root_sample": {"available": bool(root.get("available")),
                        "reason": root.get("reason"),
                        "age_s": root.get("age_s")},
        "root": elevated,
        "by_app": by_app or [],
        "by_app_basis": ("sum of per-PID proc_pid_rusage(RUSAGE_INFO_V6) energy "
                         "deltas over the last collector sweep — a measured "
                         "floor, not the machine total"),
        "package": pkg,
        "package_unavailable_reason": pkg_reason,
        "battery": batt,
        "tiers": {
            "tier1": "battery V*A + charging + per-app attribution",
            "tier2": "adapter input W from ioreg AppleRawAdapterDetails (while connected)",
            "tier3": ("root powermetrics package power + die temps + fans + ANE "
                      "(T15 daemon file, read-only)"),
        },
    }


def api_power_response():
    return power_state()


# --------------------------------------------- permissions panel + grant (T15)
# The honest state of every privilege syswatch can use, and the ONE place that
# can escalate.  Hard rules encoded here:
#   * this server NEVER runs as root and NEVER holds a password;
#   * the only escalation path is a macOS admin dialog (`with administrator
#     privileges`) that the USER approves — the credential goes straight to
#     launchctl, is never read by us, never logged, never stored;
#   * granting only ever happens on an explicit POST.  No page load, no poll,
#     no background thread can reach it.
ROOT_DIR = os.path.dirname(ROOT_SAMPLE_PATH) or "."
REPO_DIR = os.path.dirname(BASE_DIR)
DAEMON_LABEL = "com.barbarian.syswatch.power"
DAEMON_PLIST_PATH = "/Library/LaunchDaemons/%s.plist" % DAEMON_LABEL
DAEMON_LIBEXEC_DIR = "/usr/local/libexec/syswatch"
DAEMON_LOG_DIR = os.path.expanduser("~/Library/Logs/syswatch")
DAEMON_LOG_PATH = os.path.join(DAEMON_LOG_DIR, "power-daemon.log")
POWER_PLIST_TEMPLATE = os.path.join(REPO_DIR, "packaging", "launchd",
                                    DAEMON_LABEL + ".plist")
# T19: the daemon is ONE Rust binary.  It spawns powermetrics, parses the text,
# reads the AppleSMC through IOKit and writes sample.json atomically — the
# shell wrapper (power_sampler.sh) and the C helper (smc_temp) it used to fork
# twice per cycle are gone.
#
# The binary is BUILT before the admin prompt, never by it: `cargo build` under
# `with administrator privileges` would put a compiler in the root path for no
# reason.  The prompt only ever copies a file and talks to launchctl.
POWERD_BIN_NAME = "syswatch-powerd"
POWERD_INSTALL_PATH = os.path.join(DAEMON_LIBEXEC_DIR, POWERD_BIN_NAME)
# Where a prebuilt copy may already be.  First hit wins; SYSWATCH_POWERD_BIN
# overrides for packaging (a release tarball ships the binary next to the app).
POWERD_BUILD_CANDIDATES = (
    os.path.join(REPO_DIR, "target", "release", POWERD_BIN_NAME),
    os.path.join(REPO_DIR, "target", "debug", POWERD_BIN_NAME),
)
# Legacy T15/T16B artefacts — removed on grant so an upgrade leaves exactly one
# implementation on disk, and removed again on revoke.
LEGACY_SCRIPT_PATH = os.path.join(DAEMON_LIBEXEC_DIR, "power_sampler.sh")
LEGACY_SMC_PATH = os.path.join(DAEMON_LIBEXEC_DIR, "smc_temp")
ROOT_PROBE_TTL = 2.0
AX_PROBE_TTL = 60.0
_ROOT_PROBE = {"mono": 0.0, "state": None}
_AX_PROBE = {"mono": 0.0, "granted": None, "detail": None, "checked_ts": None}


def _run(cmd, timeout=10.0):
    """Run a command without a shell; never raises, never prints its output."""
    try:
        p = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                           timeout=timeout)
        return (p.returncode,
                p.stdout.decode("utf-8", "replace"),
                p.stderr.decode("utf-8", "replace"))
    except (OSError, subprocess.SubprocessError) as e:
        return -1, "", "%r" % (e,)


def _root_file_probe(force=False):
    """Stat the privileged snapshot: exists, age, fresh (<= 30 s)."""
    now = time.monotonic()
    cached = _ROOT_PROBE
    if (not force and cached["state"] is not None
            and now - cached["mono"] < ROOT_PROBE_TTL):
        return cached["state"]
    state = {"path": ROOT_SAMPLE_PATH, "exists": False, "age_s": None,
             "fresh": False,
             "plist_installed": os.path.exists(DAEMON_PLIST_PATH)}
    try:
        st = os.stat(ROOT_SAMPLE_PATH)
        state["exists"] = True
        state["age_s"] = round(max(0.0, time.time() - st.st_mtime), 1)
        state["fresh"] = state["age_s"] <= ROOT_SAMPLE_STALE_S
    except OSError:
        pass
    cached.update({"mono": now, "state": state})
    return state


def _daemon_loaded():
    """Is the root LaunchDaemon loaded in the system domain? (readable, no root)"""
    rc, _out, _err = _run(["/bin/launchctl", "print", "system/" + DAEMON_LABEL],
                          timeout=3.0)
    return rc == 0


def probe_assistive_access(force=False):
    """Harmless AX read of System Events; -1719 means Accessibility is denied.

    Cached for AX_PROBE_TTL because every attempt spawns osascript.  Set
    SYSWATCH_NO_AX_PROBE=1 to disable the probe entirely (it never grants
    anything either way — macOS only grants AX in System Settings).
    """
    now = time.monotonic()
    cached = _AX_PROBE
    if (not force and cached["checked_ts"] is not None
            and now - cached["mono"] < AX_PROBE_TTL):
        return {"granted": cached["granted"], "detail": cached["detail"],
                "checked_ts": cached["checked_ts"], "cached": True}
    if os.environ.get("SYSWATCH_NO_AX_PROBE") == "1":
        return {"granted": None, "cached": False, "checked_ts": None,
                "detail": "AX probe disabled (SYSWATCH_NO_AX_PROBE=1)"}
    rc, out, err = _run(["/usr/bin/osascript", "-e",
                         'tell application "System Events" to count processes'],
                        timeout=6.0)
    low = (err or "").lower()
    if rc == 0:
        granted = True
        detail = "osascript AX read returned %s processes" % (out.strip() or "?")
    elif ("-1719" in err or "assistive access" in low
          or "not allowed" in low or "not authorized" in low):
        granted = False
        detail = "-1719: not allowed assistive access (granted only in System Settings)"
    else:
        granted = False
        detail = ("osascript AX read failed: %s"
                  % ((err or out).strip()[:200] or ("exit %s" % rc)))
    checked = time.time()
    cached.update({"mono": now, "granted": granted, "detail": detail,
                   "checked_ts": checked})
    return {"granted": granted, "detail": detail, "checked_ts": checked,
            "cached": False}


def _permission_rows(probe_ax=True):
    """Every privilege syswatch uses, with what it unlocks and how to grant it.

    `grant_id` is the row's grant route: three root-tier rows share the one
    LaunchDaemon grant, because one macOS prompt unlocks all of them.
    """
    root = _root_file_probe()
    if root["fresh"]:
        root_detail = ("privileged sample %s, %.0f s old — live"
                       % (os.path.basename(ROOT_SAMPLE_PATH), root["age_s"]))
        root_reason = None
    elif root["exists"]:
        root_detail = ("privileged sample is %.0f s old (> %.0f s) — withheld, "
                       "not shown as live"
                       % (root["age_s"], ROOT_SAMPLE_STALE_S))
        root_reason = root_detail
    else:
        root_detail = ("no privileged sample yet — powermetrics refuses to run "
                       "without root on this macOS build")
        root_reason = root_detail
    grant_how = ("one macOS admin prompt installs the root LaunchDaemon %s "
                 "(the syswatch-powerd binary: powermetrics + AppleSMC, 15 s "
                 "cadence). The dashboard stays unprivileged: it only reads the "
                 "JSON file the daemon writes." % DAEMON_LABEL)

    def root_row(pid, label, unlock):
        return {"id": pid, "label": label,
                "granted": bool(root["fresh"]),
                "required": True,
                "group": "Elevated sensors (temperatures, package power, Neural Engine)",
                "unlock_text": unlock,
                "how_to_grant": grant_how,
                "grant_id": "package_power",
                "root_tier": True,
                "detail": root_detail,
                "reason": root_reason,
                "source": "powermetrics via root LaunchDaemon"}

    rows = [
        root_row("package_power", "Package power (CPU + GPU + ANE)",
                 "CPU/GPU package watts"),
        root_row("temperatures", "Die temperatures + thermal pressure",
                 "CPU/GPU die °C, per-cluster °C, thermal pressure, fan RPM"),
        root_row("neural_engine", "Neural Engine",
                 "Neural Engine watts and utilisation"),
    ]

    ax = (probe_assistive_access() if probe_ax
          else {"granted": None, "detail": "not probed yet (cheap request)",
                "checked_ts": None})
    rows.append({
        "id": "assistive_access",
        "label": "Assistive access (Accessibility)",
        "granted": ax["granted"],
        "required": False,
        "group": "macOS privacy",
        "unlock_text": "menu-bar AX diagnostics",
        "how_to_grant": ("System Settings → Privacy & Security → Accessibility: "
                         "enable the app that hosts the dashboard. macOS never "
                         "lets a process grant this for itself, and no admin "
                         "prompt can do it — it is a per-app toggle."),
        "grant_id": "assistive_access",
        "root_tier": False,
        "detail": ax["detail"],
        "reason": None if ax["granted"] else ax["detail"],
        "source": "osascript AX read of System Events",
        "checked_ts": ax.get("checked_ts"),
    })

    batt = _ioreg_battery()
    battery_ok = bool(batt)
    rows.append({
        "id": "battery_stats",
        "label": "Battery stats",
        "granted": battery_ok,
        "required": False,
        "group": "Unprivileged",
        "unlock_text": "battery %, watts, cycles, health, cells (unprivileged ioreg)",
        "how_to_grant": "nothing to grant — this is read unprivileged with ioreg",
        "grant_id": None,
        "root_tier": False,
        "detail": ("ioreg AppleSmartBattery parsed" if battery_ok
                   else "no battery pack found by ioreg (desktop Mac?)"),
        "reason": None if battery_ok else "ioreg reports no battery",
        "source": "ioreg AppleSmartBattery",
    })
    return rows, root


def permissions_state(probe_ax=True):
    """The /api/permissions payload: rows + the root model in one sentence."""
    rows, root = _permission_rows(probe_ax=probe_ax)
    missing = sorted({r["grant_id"] for r in rows
                      if r.get("required") and r.get("granted") is False
                      and r.get("grant_id")})
    return {
        "permissions": rows,
        "root_tier": {
            "granted": bool(root["fresh"]),
            "file": ROOT_SAMPLE_PATH,
            "age_s": root["age_s"],
            "daemon_label": DAEMON_LABEL,
            "daemon_plist": DAEMON_PLIST_PATH,
            "plist_installed": root["plist_installed"],
            "model": ("the dashboard HTTP server never runs as root and never "
                      "holds a password; root is granted only by a macOS admin "
                      "prompt that installs one LaunchDaemon, and the "
                      "dashboard only reads the JSON file that daemon writes"),
        },
        "grant": root_grant_state(),
        "missing_required": missing,
        "checked_ts": int(time.time()),
        "mode": "fixture" if FIXTURE else "live",
    }


def _applescript_string(s):
    """Escape a shell command for an AppleScript double-quoted literal."""
    return '"' + str(s).replace("\\", "\\\\").replace('"', '\\"') + '"'


def _admin_shell(steps, timeout=240.0):
    """Run shell steps under ONE macOS admin prompt; never sees the password.

    `do shell script ... with administrator privileges` is macOS' own dialog:
    osascript receives the credential from the window server, uses it for the
    privileged action and discards it.  Nothing in this process can read it.
    """
    script = "; ".join(steps)
    return _run(["/usr/bin/osascript", "-e",
                 "do shell script %s with administrator privileges"
                 % _applescript_string(script)], timeout=timeout)


def _is_declined(rc, err):
    low = (err or "").lower()
    return rc != 0 and ("user canceled" in low or "user cancelled" in low
                        or "-128" in (err or ""))


def _stage_daemon_plist():
    """Render the __HOME__ template into the user's support dir (unprivileged).

    The privileged step then only copies files and talks to launchctl, so the
    admin prompt never needs to touch anything inside the user's home.
    """
    with open(POWER_PLIST_TEMPLATE, "r", encoding="utf-8") as f:
        text = f.read()
    rendered = text.replace("__HOME__", os.path.expanduser("~"))
    if "__HOME__" in rendered:                      # defensive, cannot happen
        raise RuntimeError("template still contains __HOME__")
    # T20: the cadence has ONE source of truth.  The template ships 15000 ms;
    # the server's POWERMETRICS_INTERVAL_MS (config key `powermetrics.interval_ms`)
    # rewrites it here, so a staged plist can never disagree with the ring that
    # reads its output.
    rendered, n = re.subn(r"(--interval-ms</string>\s*<string>)\d+(</string>)",
                          r"\g<1>%d\g<2>" % POWERMETRICS_INTERVAL_MS, rendered)
    if n != 1:
        raise RuntimeError("power plist template has no single --interval-ms "
                           "argument (found %d)" % n)
    os.makedirs(ROOT_DIR, exist_ok=True)
    staged = os.path.join(ROOT_DIR, DAEMON_LABEL + ".plist")
    with open(staged, "w", encoding="utf-8") as f:
        f.write(rendered)
    os.chmod(staged, 0o644)
    return staged


def _powerd_source_mtime():
    """Newest mtime among the inputs of the daemon binary."""
    newest = 0.0
    for path in (os.path.join(REPO_DIR, "src", "bin", "powerd.rs"),
                 os.path.join(REPO_DIR, "Cargo.toml"),
                 os.path.join(REPO_DIR, "Cargo.lock")):
        try:
            newest = max(newest, os.path.getmtime(path))
        except OSError:
            pass
    return newest


def _powerd_binary(force_build=False):
    """Resolve the syswatch-powerd binary to install → (path, None|reason).

    The build happens HERE, as the logged-in user, BEFORE the admin prompt:
    `with administrator privileges` must only ever copy a file and talk to
    launchctl, never compile anything as root.  A stale build is rebuilt, so
    "Grant" always means "install what is in this repo right now".
    """
    override = os.environ.get("SYSWATCH_POWERD_BIN")
    if override:
        if os.path.exists(override):
            return override, None
        return None, "SYSWATCH_POWERD_BIN=%s does not exist" % override
    src_mtime = _powerd_source_mtime()
    for cand in POWERD_BUILD_CANDIDATES:
        if os.path.exists(cand) and not force_build and \
                os.path.getmtime(cand) >= src_mtime:
            return cand, None
    cargo = shutil.which("cargo") or "/opt/homebrew/bin/cargo"
    if not os.path.exists(cargo):
        for cand in POWERD_BUILD_CANDIDATES:      # stale-but-working beats none
            if os.path.exists(cand):
                return cand, None
        return None, ("the Rust daemon is not built and cargo was not found — "
                      "run `cargo build --release --bin %s`" % POWERD_BIN_NAME)
    # launchd's default PATH is /usr/bin:/bin:/usr/sbin:/sbin, so rustc is not
    # on it.  Hand cargo a PATH that contains the toolchain it was found next
    # to, or the build fails with "no such command: rustc" instead of building.
    toolchain_bin = os.path.dirname(os.path.realpath(cargo))
    env_path = os.pathsep.join([toolchain_bin, os.path.dirname(cargo),
                                "/usr/bin", "/bin", "/usr/sbin", "/sbin"])
    rc, out, err = _run(["/usr/bin/env", "PATH=" + env_path, cargo,
                         "build", "--release", "--bin", POWERD_BIN_NAME],
                        timeout=900.0)
    if rc != 0:
        return None, ("cargo build --release --bin %s failed: %s"
                      % (POWERD_BIN_NAME, ((err or out).strip()[:400]
                                           or ("exit %s" % rc))))
    for cand in POWERD_BUILD_CANDIDATES:
        if os.path.exists(cand):
            return cand, None
    return None, ("cargo reported success but %s is missing"
                  % POWERD_BUILD_CANDIDATES[0])


def _installed_plist_program():
    """argv[0] of the INSTALLED LaunchDaemon, or None when unreadable."""
    try:
        with open(DAEMON_PLIST_PATH, "r", encoding="utf-8") as f:
            text = f.read()
    except OSError:
        return None
    marker = "<key>ProgramArguments</key>"
    i = text.find(marker)
    if i < 0:
        return None
    j = text.find("<string>", i)
    k = text.find("</string>", j)
    if j < 0 or k < 0:
        return None
    return text[j + len("<string>"):k].strip()


def _daemon_stale_parts(binary):
    """Why the installed root tier is NOT current (empty list = up to date).

    T19: the check is the binary's hash, plus the absence of the two artefacts
    this release removed.  An installed tree that still contains the shell
    wrapper is a pre-T19 grant, and "Grant" must upgrade it.
    """
    stale = []
    try:
        with open(binary, "rb") as f:
            repo_hash = hashlib.md5(f.read()).hexdigest()
        with open(POWERD_INSTALL_PATH, "rb") as f:
            installed_hash = hashlib.md5(f.read()).hexdigest()
        if repo_hash != installed_hash:
            stale.append("daemon binary changed")
    except OSError:
        stale.append("installed daemon missing")
    if os.path.exists(LEGACY_SCRIPT_PATH):
        stale.append("legacy shell sampler still installed")
    if os.path.exists(LEGACY_SMC_PATH):
        stale.append("legacy smc_temp still installed")
    program = _installed_plist_program()
    if program != POWERD_INSTALL_PATH:
        stale.append("installed plist does not start %s" % POWERD_BIN_NAME)
    return stale


def grant_root_tier():
    """Install the root LaunchDaemon — idempotent, one prompt, no password."""
    if not os.path.exists(POWER_PLIST_TEMPLATE):
        return {"granted": False, "ok": False, "prompted": False,
                "reason": "installer template missing: %s"
                          % POWER_PLIST_TEMPLATE}
    binary, why_not = _powerd_binary()
    if binary is None:
        return {"granted": False, "ok": False, "prompted": False,
                "reason": why_not}
    state = _root_file_probe(force=True)
    if state["fresh"] and _daemon_loaded():
        # T19: "already granted" only if the INSTALLED daemon is current.  If
        # the repo's binary changed — or a pre-T19 shell + C helper is still on
        # disk — Grant must re-run the privileged install: the button means
        # install-or-update.
        stale_parts = _daemon_stale_parts(binary)
        if not stale_parts:
            return {"granted": True, "ok": True, "prompted": False,
                    "action": "already",
                    "reason": ("already granted — the root daemon is loaded and the "
                               "sample is %.0f s old" % state["age_s"]),
                    "age_s": state["age_s"]}

    try:
        staged = _stage_daemon_plist()
    except (OSError, RuntimeError) as e:
        return {"granted": False, "ok": False, "prompted": False,
                "reason": "could not stage the daemon plist: %r" % (e,)}
    try:
        os.makedirs(DAEMON_LOG_DIR, exist_ok=True)
    except OSError:
        pass
    steps = [
        "/bin/mkdir -p %s /Library/LaunchDaemons"
        % shlex.quote(DAEMON_LIBEXEC_DIR),
        # The one binary.  755 root:wheel so unprivileged code cannot swap it
        # for its own payload between reboots.
        "/usr/bin/install -m 755 -o root -g wheel %s %s"
        % (shlex.quote(binary), shlex.quote(POWERD_INSTALL_PATH)),
        "/usr/bin/install -m 644 -o root -g wheel %s %s"
        % (shlex.quote(staged), shlex.quote(DAEMON_PLIST_PATH)),
        "/usr/bin/touch %s" % shlex.quote(DAEMON_LOG_PATH),
        "/bin/chmod 644 %s" % shlex.quote(DAEMON_LOG_PATH),
        "/bin/launchctl bootout system/%s >/dev/null 2>&1 || true" % DAEMON_LABEL,
        "/bin/launchctl bootstrap system %s" % shlex.quote(DAEMON_PLIST_PATH),
    ]
    # T19 cleanup: the shell wrapper and the C helper are gone.  Removed AFTER
    # the new daemon is bootstrapped, so an upgrade never leaves a window with
    # neither implementation installed.  A failure here is cosmetic.
    for legacy in (LEGACY_SCRIPT_PATH, LEGACY_SMC_PATH):
        steps.append("/bin/rm -f %s || true" % shlex.quote(legacy))
    rc, out, err = _admin_shell(steps)
    if _is_declined(rc, err):
        log_event("permissions", "root grant declined by the user")
        return {"granted": False, "ok": False, "prompted": True,
                "declined": True, "reason": "user declined",
                "detail": "the admin prompt was dismissed; nothing was installed"}
    if rc != 0:
        log_event("permissions", "root grant failed: exit %s" % rc)
        return {"granted": False, "ok": False, "prompted": True,
                "reason": "installer failed",
                "detail": ((err or out).strip()[:400] or ("exit %s" % rc))}
    # powermetrics needs its interval to elapse before it emits a sample; wait for the real
    # evidence instead of claiming success from intent.
    deadline = time.time() + 10.0
    while time.time() < deadline:
        state = _root_file_probe(force=True)
        if state["fresh"]:
            break
        time.sleep(0.5)
    root = root_sample()
    ok = bool(state["fresh"])
    log_event("permissions", "root grant installed (fresh=%s)" % ok)
    return {"granted": ok, "ok": ok, "prompted": True,
            "action": "installed",
            "age_s": state["age_s"],
            "daemon_loaded": _daemon_loaded(),
            "daemon_binary": os.path.exists(POWERD_INSTALL_PATH),
            "reason": None if ok else
                      ("daemon installed but no fresh sample yet — %s"
                       % (root.get("reason") or "first sample pending")),
            "detail": ("root daemon %s installed (%s); the dashboard reads %s only"
                       % (DAEMON_LABEL, POWERD_BIN_NAME,
                          os.path.basename(ROOT_SAMPLE_PATH)))}


def revoke_root_tier():
    """Remove the root LaunchDaemon and its snapshot — same one-prompt pattern."""
    installed = (os.path.exists(DAEMON_PLIST_PATH) or _daemon_loaded()
                 or os.path.exists(ROOT_SAMPLE_PATH))
    if not installed:
        return {"granted": False, "ok": True, "prompted": False,
                "action": "already",
                "reason": "the root tier is not installed — nothing to revoke"}
    steps = [
        "/bin/launchctl bootout system/%s >/dev/null 2>&1 || true" % DAEMON_LABEL,
        "/bin/rm -f %s" % shlex.quote(DAEMON_PLIST_PATH),
        "/bin/rm -f %s" % shlex.quote(POWERD_INSTALL_PATH),
        # The two pre-T19 artefacts, in case this revoke runs on a box that was
        # granted before the Rust daemon landed.
        "/bin/rm -f %s" % shlex.quote(LEGACY_SCRIPT_PATH),
        "/bin/rm -f %s" % shlex.quote(LEGACY_SMC_PATH),
        "/bin/rm -f %s" % shlex.quote(ROOT_SAMPLE_PATH),
        "/bin/rm -f %s" % shlex.quote(os.path.join(ROOT_DIR,
                                                   DAEMON_LABEL + ".plist")),
    ]
    rc, out, err = _admin_shell(steps)
    if _is_declined(rc, err):
        log_event("permissions", "root revoke declined by the user")
        return {"granted": True, "ok": False, "prompted": True,
                "declined": True, "reason": "user declined",
                "detail": "the admin prompt was dismissed; nothing was removed"}
    if rc != 0:
        log_event("permissions", "root revoke failed: exit %s" % rc)
        return {"granted": True, "ok": False, "prompted": True,
                "reason": "uninstall failed",
                "detail": ((err or out).strip()[:400] or ("exit %s" % rc))}
    state = _root_file_probe(force=True)
    log_event("permissions", "root tier revoked (file_exists=%s)"
              % state["exists"])
    return {"granted": bool(state["fresh"]), "ok": True, "prompted": True,
            "action": "revoked", "file_exists": state["exists"],
            "reason": None}


PERMISSION_GRANT_IDS = ("package_power", "temperatures", "neural_engine",
                        "assistive_access", "battery_stats")


def grant_permission(pid):
    """Route one permission id to its grant action.  Never prompts implicitly."""
    if pid not in PERMISSION_GRANT_IDS:
        return None
    if pid in ("package_power", "temperatures", "neural_engine"):
        out = grant_root_tier()
        out["id"] = pid
        out["grant_id"] = "package_power"
        return out
    if pid == "assistive_access":
        # No password can grant this on macOS: it is a per-app switch the user
        # must flip. Opening the exact pane is the most we can honestly do.
        rc, _out, _err = _run(["/usr/bin/open",
                               "x-apple.systempreferences:com.apple.preference.security"
                               "?Privacy_Accessibility"], timeout=5.0)
        ax = probe_assistive_access(force=True)
        return {"id": pid, "grant_id": pid, "granted": bool(ax["granted"]),
                "ok": rc == 0, "prompted": False, "action": "opened_settings",
                "reason": ("Accessibility can only be granted by the user in "
                           "System Settings — the pane is now open"),
                "detail": ax["detail"]}
    return {"id": pid, "grant_id": None, "granted": True, "ok": True,
            "prompted": False, "action": "none",
            "reason": "battery stats need no privilege — already granted"}


def revoke_permission(pid):
    if pid not in PERMISSION_GRANT_IDS:
        return None
    if pid in ("package_power", "temperatures", "neural_engine"):
        out = revoke_root_tier()
        out["id"] = pid
        out["grant_id"] = "package_power"
        return out
    return {"id": pid, "granted": None, "ok": False, "prompted": False,
            "reason": ("%s has no uninstall step — macOS privacy switches are "
                       "changed in System Settings" % pid)}


# ------------------------------------------------- power history ring (T13)
# The UI needs W over 1m / 1h / 1d / 7d, and 7 days of samples is 121k rows at
# 5 s and 40k at 15 s. Same trick the collector uses for its own history: keep
# one fine tier for the recent past and progressively coarser tiers behind it,
# each bucket carrying the mean AND the max (a mean-only ring hides the 30 W
# spikes that are the whole point of a power panel).
#
#   tier 0: 15 s buckets ->  15 min    (60 rows)    live shape, one sample each
#   tier 1: 60 s buckets ->   6 h      (360 rows)
#   tier 2: 600 s buckets ->  7 d     (1008 rows)   covers the 1d and 7d windows
#
# T20: the cadence is powermetrics' cadence (15 s), not 5 s.  The ring reads
# the root sample and ioreg on the same 15 s edge, so the timeline resolution is
# exactly the resolution the source produces — a 5 s tick would have repeated
# the same privileged reading three times and implied a detail that does not
# exist.  Bucket width is reported in every payload (`bucket_s`) so the UI never
# implies more resolution than exists, and every window reports `coverage_s` /
# `available_from_s` so a filling ring reads as "filling", never as a fabricated
# average.
POWER_SAMPLE_SECS = POWERMETRICS_SAMPLE_SECS
POWER_TIERS = ((15, 900), (60, 21600), (600, 604800))
POWER_WINDOWS = (("1m", 60), ("1h", 3600), ("1d", 86400), ("7d", 604800))


class PowerRing:
    """Tiered in-memory power history: draw, adapter input, package W.

    T14B: every bucket carries six measured fields — battery draw, adapter
    input, root package power, attributed per-app sum, charge % and time
    remaining — each as sum/count/max, so a mean never hides the spike and a
    field that was not measured in that bucket stays None (never 0).
    """

    FIELDS = ("battery_w", "adapter_w", "package_w", "attributed_w",
              "pct", "time_remaining_s", "cpu_w", "gpu_w", "ane_w",
              # T20 — temperatures are a first-class module, so the ring keeps
              # the privileged reading: the thermal tile's sparkline and the
              # temperature-over-time chart are this series, not a proxy.
              "cpu_temp_c", "gpu_temp_c")

    def __init__(self):
        self.lock = threading.Lock()
        self.tiers = {b: [] for b, _ in POWER_TIERS}
        self.oldest_ts = None
        self.newest_ts = None
        self.samples = 0
        self.last = None
        self.started_ts = time.time()

    def _blank(self, key):
        row = {"ts": key, "n": 0, "top_app": None, "top_w": None,
               "charging": None}
        for f in self.FIELDS:
            row[f + "_sum"] = 0.0
            row[f + "_n"] = 0
            row[f + "_max"] = None
            row[f + "_max_ts"] = None
        return row

    def add(self, ts, sample):
        """Record one sample dict (missing keys are simply not measured)."""
        with self.lock:
            self.samples += 1
            if self.oldest_ts is None or ts < self.oldest_ts:
                self.oldest_ts = ts
            if self.newest_ts is None or ts > self.newest_ts:
                self.newest_ts = ts
            self.last = dict(sample, ts=ts)
            for bucket_s, retention in POWER_TIERS:
                rows = self.tiers[bucket_s]
                key = int(ts // bucket_s) * bucket_s
                if not rows or rows[-1]["ts"] != key:
                    rows.append(self._blank(key))
                row = rows[-1]
                row["n"] += 1
                for f in self.FIELDS:
                    v = sample.get(f)
                    if isinstance(v, (int, float)) and not isinstance(v, bool):
                        row[f + "_sum"] += float(v)
                        row[f + "_n"] += 1
                        if row[f + "_max"] is None or v > row[f + "_max"]:
                            row[f + "_max"] = float(v)
                            row[f + "_max_ts"] = ts
                top_app = sample.get("top_app")
                if top_app and isinstance(top_app.get("watts"), (int, float)):
                    if row["top_w"] is None or top_app["watts"] > row["top_w"]:
                        row["top_app"] = top_app.get("name")
                        row["top_w"] = top_app.get("watts")
                if sample.get("charging") is not None:
                    row["charging"] = sample.get("charging")
                # Trim by wall-clock retention, not row count, so a long pause
                # (laptop asleep) cannot leave stale rows in the window.
                while rows and rows[0]["ts"] < ts - retention:
                    rows.pop(0)

    def available_span(self):
        if self.oldest_ts is None or self.newest_ts is None:
            return None
        return max(0.0, self.newest_ts - self.oldest_ts)

    def _tier_for(self, window_s):
        """Finest tier that can hold everything we actually have for the window.

        A ring that is 20 s old must not answer a 1 h query with 60 s buckets
        (that would show one fat point); it answers from the 15 s tier and says
        `bucket_s: 15`. As history accumulates the same query automatically
        moves to the coarser tier that covers it.
        """
        span = self.available_span()
        need = window_s if span is None else min(window_s, span)
        for bucket_s, retention in POWER_TIERS:
            if retention >= need:
                return bucket_s, retention
        return POWER_TIERS[-1]

    @staticmethod
    def _row_mean(row, field):
        n = row.get(field + "_n") or 0
        return round(row.get(field + "_sum", 0.0) / n, 3) if n else None

    def _rows(self, window_s, now):
        bucket_s, _retention = self._tier_for(window_s)
        cut = now - window_s
        with self.lock:
            rows = [dict(r) for r in self.tiers[bucket_s] if r["ts"] + bucket_s > cut]
        return bucket_s, rows

    def window(self, window_s, now):
        bucket_s, rows = self._rows(window_s, now)
        oldest = self.oldest_ts
        newest = self.newest_ts
        n = sum(r["n"] for r in rows)
        out = {"window_s": window_s, "bucket_s": bucket_s, "samples": n}
        for f in self.FIELDS:
            num = sum(r.get(f + "_n") or 0 for r in rows)
            if num:
                out[f] = round(sum(r.get(f + "_sum") or 0.0 for r in rows) / num, 3)
            else:
                out[f] = None
            maxes = [r[f + "_max"] for r in rows if r.get(f + "_max") is not None]
            out[f + "_max"] = round(max(maxes), 3) if maxes else None
        peak_row = None
        for r in rows:
            if r.get("battery_w_max") is None:
                continue
            if peak_row is None or r["battery_w_max"] > peak_row["battery_w_max"]:
                peak_row = r
        # T13 field names stay: mean_w / max_w / mean_attributed_w.
        out["mean_w"] = out["battery_w"]
        out["max_w"] = out["battery_w_max"]
        out["max_ts"] = peak_row.get("battery_w_max_ts") if peak_row else None
        out["mean_attributed_w"] = out["attributed_w"]
        out["mean_adapter_w"] = out["adapter_w"]
        out["adapter_w_max"] = out["adapter_w_max"]
        out["mean_package_w"] = out["package_w"]
        out["mean_cpu_w"] = out["cpu_w"]
        out["mean_gpu_w"] = out["gpu_w"]
        out["mean_ane_w"] = out["ane_w"]
        coverage = 0.0
        if oldest is not None and newest is not None:
            coverage = max(0.0, min(window_s, newest - oldest))
        out["coverage_s"] = round(coverage, 1)
        out["available_from_s"] = (round(newest - oldest, 1)
                                   if oldest is not None else None)
        out["complete"] = bool(coverage >= window_s * 0.98)
        if out["mean_w"] is None and out["mean_attributed_w"] is None:
            # Honest null: the ring has nothing for this window yet.
            out["reason"] = "ring still filling — no samples in this window yet"
        return out

    def all_windows(self, now):
        return {name: self.window(secs, now) for name, secs in POWER_WINDOWS}

    def series(self, window_s, now):
        """Points for the chart: per-bucket mean + max, oldest first."""
        bucket_s, rows = self._rows(window_s, now)
        pts = []
        for r in rows:
            pt = {"ts": r["ts"], "src": "live", "bucket_s": bucket_s,
                  "n": r["n"], "top_app": r["top_app"],
                  "charging": r["charging"]}
            measured = False
            for f in self.FIELDS:
                v = self._row_mean(r, f)
                pt[f] = v
                pt[f + "_max"] = r.get(f + "_max")
                if v is not None:
                    measured = True
            if not measured:
                continue
            pts.append(pt)
        return pts

    def state(self, now):
        with self.lock:
            return {
                "sampling_s": POWER_SAMPLE_SECS,
                "samples": self.samples,
                "oldest_ts": self.oldest_ts,
                "newest_ts": self.newest_ts,
                "available_from_s": (round(self.newest_ts - self.oldest_ts, 1)
                                     if self.oldest_ts is not None else None),
                "tiers": [{"bucket_s": b, "retention_s": r,
                           "rows": len(self.tiers[b])} for b, r in POWER_TIERS],
                "last": self.last,
                "ring_age_s": round(now - self.started_ts, 1),
            }


POWER_RING = PowerRing()


def power_ring_loop():
    """Sample draw + adapter + (root) package W + die temps every 15 s (T20)."""
    while True:
        try:
            batt = _read_battery_state()
            snap = _latest_collector_snapshot()
            energy = (snap or {}).get("energy") or {}
            attributed = energy.get("attributed_w")
            if not isinstance(attributed, (int, float)):
                attributed = None
            rows = ((snap or {}).get("processes") or {}).get("by_energy") or []
            top = None
            for r in rows:
                if isinstance(r.get("watts"), (int, float)):
                    top = {"name": r.get("name"), "pid": r.get("pid"),
                           "watts": round(r["watts"], 3)}
                    break
            root = root_sample()
            root_data = root.get("data") or {}
            POWER_RING.add(time.time(), {
                "battery_w": batt.get("battery_w"),
                "adapter_w": batt.get("adapter_w"),
                "package_w": root_data.get("package_w") if root.get("available") else None,
                "cpu_w": root_data.get("cpu_w") if root.get("available") else None,
                "gpu_w": root_data.get("gpu_w") if root.get("available") else None,
                "ane_w": root_data.get("ane_w") if root.get("available") else None,
                # T20: the same privileged read carries the die temperatures.
                # T16B builds emit cpu_avg_c/gpu_avg_c instead of the older
                # cpu_temp_c/gpu_temp_c, so alias the two here — a missing key
                # stays None (never 0 °C).
                "cpu_temp_c": ((root_data.get("cpu_temp_c")
                                if root_data.get("cpu_temp_c") is not None
                                else root_data.get("cpu_avg_c"))
                               if root.get("available") else None),
                "gpu_temp_c": ((root_data.get("gpu_temp_c")
                                if root_data.get("gpu_temp_c") is not None
                                else root_data.get("gpu_avg_c"))
                               if root.get("available") else None),
                "attributed_w": attributed,
                "pct": batt.get("pct"),
                "time_remaining_s": batt.get("time_remaining_s"),
                "charging": batt.get("charging"),
                "top_app": top,
            })
        except Exception as e:  # never let the ring thread die
            log_event("power", "ring sample failed: %r" % e)
        time.sleep(POWER_SAMPLE_SECS)


# ------------------------------------------- GPU / disk rates / pressure (T14)
# T14 #11: GPU utilisation, disk throughput and battery time-to-empty are all
# readable UNPRIVILEGED on macOS, so the dashboard samples them itself (no
# src/ changes).  Each probe spawns ioreg once (14 ms GPU / 12 ms disk,
# measured on this M2), so they run on their own 5 s cadence in a dedicated
# thread.  Nothing here is ever called from the 1 s tick path: sample_once()
# only reads the in-memory ring heads.
IO_SAMPLE_SECS = 5.0
# Tiers are (bucket_s, retention_s): a 1 h query reads 5 s buckets, a 7 d
# query reads 10 min buckets, and nothing keeps 500k raw rows in RAM.
IO_RING_TIERS = ((5, 1800), (60, 21600), (600, 604800), (3600, 2592000))


class BucketRing:
    """Ring of multi-field samples with per-bucket mean AND max.

    Why both: utilisation/throughput need the mean ("how busy on average") and
    the max ("how bad the spike was").  /api/graph emits `<field>` = bucket
    mean and `<field>_max` = highest single sample inside the bucket, exactly
    as the brief asks for GPU ("ring with max+mean").

    A field that was never sampled in a bucket stays None, never 0 — a gap is
    reported as a gap.
    """

    def __init__(self, fields, tiers=IO_RING_TIERS, name="ring"):
        self.fields = tuple(fields)
        self.name = name
        self.lock = threading.Lock()
        self.tiers = {b: [] for b, _ in tiers}
        self.retention = {b: r for b, r in tiers}
        self.order = [b for b, _ in tiers]
        self.samples = 0
        self.oldest_ts = None
        self.newest_ts = None
        self.last = None
        self.started_ts = time.time()

    def _blank(self, key):
        row = {"ts": key, "n": 0}
        for f in self.fields:
            row[f] = 0.0
            row[f + "_n"] = 0
            row[f + "_max"] = None
            row[f + "_max_ts"] = None
        return row

    def add(self, ts, row):
        with self.lock:
            self.samples += 1
            if self.oldest_ts is None or ts < self.oldest_ts:
                self.oldest_ts = ts
            if self.newest_ts is None or ts > self.newest_ts:
                self.newest_ts = ts
            self.last = dict(row, ts=ts)
            for bucket_s in self.order:
                rows = self.tiers[bucket_s]
                key = int(ts // bucket_s) * bucket_s
                if not rows or rows[-1]["ts"] != key:
                    rows.append(self._blank(key))
                r = rows[-1]
                r["n"] += 1
                for f in self.fields:
                    v = row.get(f)
                    if isinstance(v, (int, float)) and not math.isnan(v):
                        r[f] += float(v)
                        r[f + "_n"] += 1
                        if r[f + "_max"] is None or v > r[f + "_max"]:
                            r[f + "_max"] = float(v)
                            r[f + "_max_ts"] = ts
                # Wall-clock retention (not row count) so a sleep gap cannot
                # leave stale rows answering a fresh window.
                while rows and rows[0]["ts"] < ts - self.retention[bucket_s]:
                    rows.pop(0)

    def available_span(self):
        if self.oldest_ts is None or self.newest_ts is None:
            return None
        return max(0.0, self.newest_ts - self.oldest_ts)

    def tier_for(self, window_s):
        span = self.available_span()
        need = window_s if span is None else min(window_s, span)
        for bucket_s in self.order:
            if self.retention[bucket_s] >= need:
                return bucket_s
        return self.order[-1]

    def series(self, window_s, now):
        """Per-bucket mean + max, oldest first. Empty list when nothing yet."""
        bucket_s = self.tier_for(window_s)
        cut = now - window_s
        with self.lock:
            rows = [dict(r) for r in self.tiers[bucket_s]
                    if r["ts"] + bucket_s > cut]
        pts = []
        for r in rows:
            out = {"ts": r["ts"], "n": r["n"], "bucket_s": bucket_s,
                   "max_ts": r.get(self.fields[0] + "_max_ts")}
            for f in self.fields:
                n = r[f + "_n"]
                out[f] = round(r[f] / n, 3) if n else None
                out[f + "_max"] = r[f + "_max"]
            pts.append(out)
        return pts

    def state(self, now):
        with self.lock:
            return {
                "name": self.name,
                "sampling_s": IO_SAMPLE_SECS,
                "samples": self.samples,
                "fields": list(self.fields),
                "last": self.last,
                "oldest_ts": self.oldest_ts,
                "newest_ts": self.newest_ts,
                "available_from_s": (round(self.newest_ts - self.oldest_ts, 1)
                                     if self.oldest_ts is not None else None),
                "tiers": [{"bucket_s": b, "retention_s": self.retention[b],
                           "rows": len(self.tiers[b])} for b in self.order],
                "ring_age_s": round(now - self.started_ts, 1),
            }


GPU_FIELDS = ("util_pct", "renderer_pct", "tiler_pct", "mem_used_mb")
DISK_FIELDS = ("read_bps", "write_bps", "read_iops", "write_iops")
GPU_RING = BucketRing(GPU_FIELDS, name="gpu")
DISK_RING = BucketRing(DISK_FIELDS, name="disk")


def _ioreg_probe(klass):
    """Return every ioreg object of `klass` as dicts (binary plist → real types).

    `ioreg -a` hands back a plist, so numbers arrive as ints instead of the
    strings a text scrape would produce.  Returns [] on any failure — callers
    report `unavailable` + the reason rather than inventing a value.
    """
    try:
        out = subprocess.check_output(
            ["/usr/sbin/ioreg", "-r", "-d", "1", "-w0", "-a", "-c", klass],
            stderr=subprocess.DEVNULL, timeout=5)
        objs = plistlib.loads(out)
    except Exception:
        return []
    if isinstance(objs, dict):
        return [objs]
    return [o for o in objs if isinstance(o, dict)] if isinstance(objs, list) else []


def _as_float(v):
    return float(v) if isinstance(v, (int, float)) else None


def read_gpu_util():
    """IOAccelerator PerformanceStatistics → utilisation + memory (unprivileged).

    Verified live on this M2: `Device Utilization %` 2–16, Renderer/Tiler equal,
    `In use system memory` ~440 MB, `gpu-core-count` 10.  GPU frequency is NOT
    in PerformanceStatistics on Apple Silicon, so it is reported None with a
    reason instead of a guessed clock.
    """
    out = {"available": False, "reason": None,
           "util_pct": None, "renderer_pct": None, "tiler_pct": None,
           "mem_used_mb": None, "mem_alloc_mb": None,
           "freq_mhz": None, "cores": None, "model": None,
           "source": "ioreg IOAccelerator PerformanceStatistics"}
    objs = _ioreg_probe("IOAccelerator")
    if not objs:
        out["reason"] = "ioreg IOAccelerator returned no object"
        return out
    obj = objs[0]
    perf = obj.get("PerformanceStatistics") or {}
    util = perf.get("Device Utilization %")
    if not isinstance(util, (int, float)):
        out["reason"] = ("IOAccelerator exposes PerformanceStatistics but no "
                         "Device Utilization % on this GPU")
        return out
    out["available"] = True
    out["util_pct"] = float(util)
    out["renderer_pct"] = _as_float(perf.get("Renderer Utilization %"))
    out["tiler_pct"] = _as_float(perf.get("Tiler Utilization %"))
    used = perf.get("In use system memory")
    if isinstance(used, (int, float)):
        out["mem_used_mb"] = round(used / 1048576.0, 1)
    alloc = perf.get("Alloc system memory")
    if isinstance(alloc, (int, float)):
        out["mem_alloc_mb"] = round(alloc / 1048576.0, 1)
    # Frequency appears on some GPUs only; never synthesise it.
    for key in ("GPUFrequency", "Core Clock", "gpu-core-clock"):
        v = perf.get(key)
        if isinstance(v, (int, float)) and v:
            out["freq_mhz"] = round(v / 1e6, 2) if v > 1e6 else float(v)
            break
    if out["freq_mhz"] is None:
        out["freq_reason"] = ("PerformanceStatistics carries no GPU clock on "
                              "Apple Silicon; needs powermetrics (root)")
    cores = obj.get("gpu-core-count")
    if isinstance(cores, int):
        out["cores"] = cores
    out["gpu_model"] = obj.get("model")
    return out


def read_disk_counters():
    """IOBlockStorageDriver cumulative byte/op counters, summed across drives.

    These are CUMULATIVE since boot — never a rate.  _disk_rates()
    differentiates them, and a counter that went backwards (drive reset, hot
    plug) yields None for that field instead of a negative or a fake 0.
    """
    objs = _ioreg_probe("IOBlockStorageDriver")
    if not objs:
        return None, "ioreg IOBlockStorageDriver returned no object"
    total = {"read_bytes": 0, "write_bytes": 0,
             "read_ops": 0, "write_ops": 0, "drives": 0}
    for obj in objs:
        st = obj.get("Statistics") or {}
        rb = st.get("Bytes (Read)")
        wb = st.get("Bytes (Write)")
        if not isinstance(rb, (int, float)) or not isinstance(wb, (int, float)):
            continue
        total["read_bytes"] += int(rb)
        total["write_bytes"] += int(wb)
        ro = st.get("Operations (Read)")
        wo = st.get("Operations (Write)")
        total["read_ops"] += int(ro) if isinstance(ro, (int, float)) else 0
        total["write_ops"] += int(wo) if isinstance(wo, (int, float)) else 0
        total["drives"] += 1
    if not total["drives"]:
        return None, "no IOBlockStorageDriver exposed Statistics"
    return total, None


_IO_PREV = {"ts": None, "counters": None}


def _disk_rates(now, counters):
    """Differentiate cumulative counters over the real sample interval."""
    prev_ts, prev = _IO_PREV["ts"], _IO_PREV["counters"]
    _IO_PREV["ts"], _IO_PREV["counters"] = now, counters
    if prev is None or prev_ts is None:
        return None
    dt = now - prev_ts
    if dt < 0.5:
        return None
    row = {}
    for field, key in (("read_bps", "read_bytes"), ("write_bps", "write_bytes")):
        delta = counters[key] - prev[key]
        row[field] = round(delta / dt, 1) if delta >= 0 else None
    for field, key in (("read_iops", "read_ops"), ("write_iops", "write_ops")):
        delta = counters[key] - prev[key]
        row[field] = round(delta / dt, 2) if delta >= 0 else None
    return row


def io_ring_loop():
    """Sample GPU + disk counters every IO_SAMPLE_SECS (first pass primes).

    Kept out of sampler_loop on purpose: a probe is ~26 ms of two ioreg spawns,
    which at 1 s would be 2.6 % of a core for numbers that cannot change that
    fast.  The rings are read (never written) by request handlers.
    """
    while True:
        now = time.time()
        try:
            gpu = read_gpu_util()
            if gpu.get("available"):
                GPU_RING.add(now, gpu)
        except Exception as e:
            log_event("io", "gpu probe failed: %r" % e)
        try:
            counters, _err = read_disk_counters()
            if counters is not None:
                rates = _disk_rates(now, counters)
                if rates is not None:
                    DISK_RING.add(now, rates)
        except Exception as e:
            log_event("io", "disk probe failed: %r" % e)
        time.sleep(IO_SAMPLE_SECS)


def gpu_state():
    """Latest GPU sample shaped for /api/current (never spawns ioreg)."""
    last = GPU_RING.last
    if not last:
        return {"available": False,
                "reason": "GPU probe has not completed its first 5 s sample yet",
                "util_pct": None}
    out = {k: v for k, v in last.items() if k != "ts"}
    out["available"] = True
    out["sampled_ts"] = last.get("ts")
    return out


def disk_io_state():
    """Latest disk throughput sample shaped for /api/current."""
    last = DISK_RING.last
    if not last:
        return {"available": False,
                "reason": "disk counters primed — first B/s delta lands after 5 s",
                "read_bps": None, "write_bps": None}
    out = {k: v for k, v in last.items() if k != "ts"}
    out["available"] = True
    out["sampled_ts"] = last.get("ts")
    return out


# --- pressure (T14 addendum E, overview gauges) ---------------------------
# Pressure, not raw utilisation, is what predicts a slow machine.  Every gauge
# below is either a real kernel number or a LABELLED derivation; none of them
# claim to be Apple's internal pressure metric.
_PRESSURE_CACHE = {"mono": 0.0, "level": None, "reason": None}
MEMORY_PRESSURE_LEVELS = {1: "normal", 2: "warning", 4: "critical"}


def _sysctl_str(key):
    """One sysctl value as a stripped string, None when the key is absent."""
    try:
        raw = subprocess.check_output(["/usr/sbin/sysctl", "-n", key],
                                      stderr=subprocess.DEVNULL, timeout=3)
        return raw.decode("utf-8", errors="replace").strip()
    except Exception:
        return None


def _read_cpu_clusters():
    """P/E clusters **in logical-CPU index order**, with the right name on each.

    EVIDENCE (measured on this Mac, T14C): a single busy thread sampled through
    `/api/current` three times rose at indices 4, 6 and 4 (53 %, 73 %, 82 %)
    while indices 0-3 stayed near 30 %.  On Apple Silicon the EFFICIENCY cluster
    is enumerated FIRST: logical CPUs 0..(E-1) = efficiency, the next P =
    performance.  The old code mapped perflevel0 ("Performance") onto cores 0-3,
    i.e. it labelled the efficiency cores "performance".

    Truthfulness notes:
      * `hw.perflevel0.name = Performance`, `hw.perflevel1.name = Efficiency`
        (verified), but the perflevel NUMBER is NOT the core order — the counts
        are keyed off the NAME here, and the span is built from the efficiency
        count, never from perflevel order.
      * Discovered, never assumed: no perflevel keys → a single unnamed "cpu"
        cluster from hw.ncpu (the UI then makes no P/E claim); nothing readable
        at all → [].
    """
    count_by_name = {}
    for idx in range(0, 4):
        name = _sysctl_str("hw.perflevel%d.name" % idx)
        n = _sysctl_str("hw.perflevel%d.logicalcpu" % idx)
        if not name or n is None:
            continue
        try:
            cores = int(n)
        except ValueError:
            continue
        if cores > 0:
            count_by_name[name.strip().lower()] = cores

    e_cores = count_by_name.get("efficiency")
    p_cores = count_by_name.get("performance")
    ncpu = CORE_COUNT
    if ncpu is None:
        try:
            ncpu = int(_sysctl_str("hw.ncpu") or 0) or None
        except ValueError:
            ncpu = None

    out = []
    if e_cores and p_cores:
        # index order == enumeration order: efficiency first, then performance
        out.append({"index": 0, "name": "efficiency", "cores": e_cores})
        out.append({"index": e_cores, "name": "performance", "cores": p_cores})
        used = e_cores + p_cores
        if ncpu and ncpu > used:
            # hw.ncpu disagrees with the perflevel counts: keep the extra cores
            # visible and labelled as unattributed rather than silently dropped.
            out.append({"index": used, "name": "unattributed",
                        "cores": ncpu - used})
    elif e_cores:
        out.append({"index": 0, "name": "efficiency", "cores": e_cores})
    elif p_cores:
        out.append({"index": 0, "name": "performance", "cores": p_cores})
    elif ncpu:
        out.append({"index": 0, "name": "cpu", "cores": ncpu})
    return out


# CORE_COUNT is resolved *before* the clusters: the cluster builder needs it as
# the "how many logical CPUs does the kernel actually expose" cross-check.
CORE_COUNT = (psutil.cpu_count() if psutil else None) or None
CPU_CLUSTERS = _read_cpu_clusters()
CLUSTER_BASIS = ("hw.perflevel1.logicalcpu efficiency cores occupy logical CPUs "
                 "0..E-1; hw.perflevel0.logicalcpu performance cores follow — "
                 "verified with a single-thread load (spikes at indices 4-6)")


def cluster_payload(clusters, per_core_pct, freq_mhz=None):
    """One tick's P/E cluster rows: static topology + this sample's numbers.

    `util_pct` is the arithmetic mean of the cluster's OWN slice of
    `per_core_pct`, starting at the cluster's `index` (efficiency first on
    Apple Silicon), so the number can never be a mean of the wrong cores.
    A cluster whose slice is not sampled yet carries `util_pct: None` and the
    exact reason; it is never rendered as 0 %.

    `freq_mhz` is the package clock psutil reports (macOS exposes no
    per-cluster clock unprivileged), so it is tagged `freq_scope: "package"`.
    Per-cluster clocks must come from IOReport (root) and are left absent
    rather than copied onto the wrong cluster.
    """
    per_core = per_core_pct if isinstance(per_core_pct, list) else []
    out = []
    for ci, c in enumerate(clusters or []):
        row = dict(c)
        base = row.get("index")
        if not isinstance(base, int) or isinstance(base, bool):
            base = sum(int(x.get("cores") or 0) for x in clusters[:ci])
        cores = int(row.get("cores") or 0)
        sl = per_core[base:base + cores]
        vals = [v for v in sl
                if isinstance(v, (int, float)) and not isinstance(v, bool)]
        if vals:
            row["util_pct"] = round(sum(vals) / len(vals), 1)
            row["util_source"] = ("mean of per_core_pct[%d..%d]"
                                  % (base, base + len(sl) - 1))
        else:
            row["util_pct"] = None
            row["util_reason"] = ("per_core_pct[%d..%d] missing — the sampler "
                                  "has no per-core delta for this cluster yet"
                                  % (base, base + cores - 1))
        if isinstance(freq_mhz, (int, float)) and not isinstance(freq_mhz, bool):
            row["freq_mhz"] = round(float(freq_mhz))
            row["freq_scope"] = "package"
        out.append(row)
    return out


def read_memory_pressure_level():
    """kern.memorystatus_vm_pressure_level (unprivileged, verified live = 1).

    Cached for 5 s: this is a sysctl spawn and pressure does not move faster
    than that.  Unknown level (missing key / sysctl failure) → None + reason.
    """
    now = time.monotonic()
    if now - _PRESSURE_CACHE["mono"] < 5.0:
        return _PRESSURE_CACHE["level"], _PRESSURE_CACHE["reason"]
    level, reason = None, None
    try:
        out = subprocess.check_output(
            ["/usr/sbin/sysctl", "-n", "kern.memorystatus_vm_pressure_level"],
            stderr=subprocess.DEVNULL, timeout=3)
        level = int(out.strip())
    except Exception as e:
        reason = "kern.memorystatus_vm_pressure_level unreadable (%r)" % e
    _PRESSURE_CACHE.update({"mono": now, "level": level, "reason": reason})
    return level, reason


def pressure_state(cpu_pct, load1, ncpu, ram_pct, disk_used_pct):
    """Build the three overview gauges: CPU, memory, storage pressure.

    - CPU pressure    : derived. load-per-core is the run-queue proxy (1.0 means
                        every core has a runnable thread waiting); utilisation
                        alone cannot see queueing. The derivation is in the
                        payload so the tooltip can name it.
    - Memory pressure : the real kernel level, mapped to normal/warning/critical
                        (1/2/4), plus the used-% for the bar length.
    - Storage pressure: free-ratio thresholds (<10 % free critical, <25 %
                        warning) — the iStat convention.
    """
    out = {}

    if isinstance(load1, (int, float)) and ncpu:
        per_core = load1 / float(ncpu)
        pct = round(min(1.0, max(0.0, per_core)) * 100, 1)
        out["cpu"] = {
            "available": True,
            "pct": pct,
            "level": ("critical" if per_core >= 1.0 else
                      "warning" if per_core >= 0.7 else "normal"),
            "load_per_core": round(per_core, 3),
            "util_pct": (round(cpu_pct, 1) if isinstance(cpu_pct, (int, float)) else None),
            "source": "getloadavg(1) / hw.ncpu",
            "derivation": ("load1 / logical cores; 100 % = one runnable thread "
                           "waiting per core. Not Apple's internal number."),
        }
    else:
        out["cpu"] = {"available": False, "reason": "load average unavailable"}

    level, reason = read_memory_pressure_level()
    if level is not None:
        out["memory"] = {
            "available": True,
            "level": MEMORY_PRESSURE_LEVELS.get(level, "level %d" % level),
            "raw_level": level,
            "pct": (round(ram_pct, 1) if isinstance(ram_pct, (int, float)) else None),
            "source": "kern.memorystatus_vm_pressure_level",
        }
    else:
        out["memory"] = {"available": False, "reason": reason,
                         "pct": (round(ram_pct, 1) if isinstance(ram_pct, (int, float)) else None)}

    if isinstance(disk_used_pct, (int, float)):
        free = round(100.0 - disk_used_pct, 1)
        out["storage"] = {
            "available": True,
            "pct": free,
            "level": ("critical" if free < 10 else "warning" if free < 25 else "normal"),
            "free_pct": free,
            "used_pct": round(disk_used_pct, 1),
            "source": "statfs free ratio",
            "derivation": "<10 % free = critical, <25 % = warning",
        }
    else:
        out["storage"] = {"available": False, "reason": "free space unavailable"}
    return out


# --- disk capacity (10 s TTL — a statfs call, not a chart series) ---------
DISK_USAGE_TTL = 10.0
_DISK_USAGE_CACHE = {"mono": 0.0, "state": None}


def disk_usage_state():
    """Capacity for the hero number. Never charted (T14 addendum C).

    On macOS the root filesystem is a read-only system volume (df /: 10 % used)
    while user data lives on /System/Volumes/Data — reporting the system volume
    would say "10 % full" on a half-full disk.  The data volume is preferred
    when it exists and `mount` states which one was measured.
    """
    now = time.monotonic()
    if (_DISK_USAGE_CACHE["state"] is not None
            and now - _DISK_USAGE_CACHE["mono"] < DISK_USAGE_TTL):
        return dict(_DISK_USAGE_CACHE["state"])
    mount = os.sep
    if os.path.isdir("/System/Volumes/Data"):
        mount = "/System/Volumes/Data"
    state = {"available": False, "reason": None, "used_pct": None,
             "free_gb": None, "total_gb": None, "mount": mount}
    try:
        du = psutil.disk_usage(mount)
        state.update({"available": True, "used_pct": round(du.percent, 2),
                      "free_gb": round(du.free / 1073741824.0, 2),
                      "total_gb": round(du.total / 1073741824.0, 2),
                      "free_bytes": du.free, "total_bytes": du.total,
                      "source": "statfs via psutil.disk_usage"})
    except Exception as e:
        state["reason"] = "statfs failed (%r)" % e
    _DISK_USAGE_CACHE["mono"] = now
    _DISK_USAGE_CACHE["state"] = dict(state)
    return state


# --- per-volume rows (30 s TTL: one statfs per mount, not a chart) --------
VOLUME_TTL = 30.0
_VOLUME_CACHE = {"mono": 0.0, "rows": None}


def disk_volumes():
    """Per-volume free/used rows for the storage table (cached 30 s)."""
    now = time.monotonic()
    if _VOLUME_CACHE["rows"] is not None and now - _VOLUME_CACHE["mono"] < VOLUME_TTL:
        return list(_VOLUME_CACHE["rows"])
    rows = []
    try:
        for part in psutil.disk_partitions(all=False):
            try:
                du = psutil.disk_usage(part.mountpoint)
            except Exception as e:
                rows.append({"mount": part.mountpoint, "device": part.device,
                             "used_pct": None, "free_gb": None,
                             "reason": "statfs failed (%r)" % e})
                continue
            rows.append({"mount": part.mountpoint, "device": part.device,
                         "used_pct": round(du.percent, 2),
                         "free_gb": round(du.free / 1073741824.0, 2),
                         "total_gb": round(du.total / 1073741824.0, 2)})
    except Exception:
        rows = []
    # Hide the synthetic sub-gigabyte system volumes (xarts, iSCPreboot,
    # Hardware): they are firmware mounts, not storage the user can free.
    rows = [r for r in rows
            if not isinstance(r.get("total_gb"), (int, float)) or r["total_gb"] >= 1.0]
    _VOLUME_CACHE.update({"mono": now, "rows": rows})
    return list(rows)


def sensors_payload():
    """T14 addendum A/B: the discovered sensor catalogue for /api/sensors.

    Passes the values the server ALREADY sampled (1 s snapshot, 5 s GPU/disk
    rings, power ring, root daemon file) so the registry never becomes a second
    sampler.  Every row is either measured or `unavailable` + reason.
    """
    snap = (SAMPLER.current if SAMPLER else None) or {}
    batt = snap.get("battery") or {}
    live_batt = _read_battery_state()
    root = root_sample()
    ctx = {
        "snap": snap,
        "gpu": GPU_RING.last or {},
        "disk_io": DISK_RING.last or {},
        "disk": disk_usage_state(),
        "clusters": CPU_CLUSTERS,
        "volumes": disk_volumes(),
        "battery": live_batt,
        "battery_temp_c": live_batt.get("temp_c"),
        "root": root,
        "grant": root_grant_state(),
    }
    out = sensors.registry(ctx)
    out["generated_ts"] = int(time.time())
    out["mode"] = "fixture" if FIXTURE else "live"
    return out


# ------------------------------- collector history → seeded timeline (T14B #2)
# Addendum 2: a freshly loaded page must show a real timeline, not three dots.
# The collector's JSONL log is the only history store on this machine, so it is
# read with a HARD byte cap and the payload states how much of the requested
# window that cap actually covered.  Only fields the collector really records
# are used: attributed per-app watts, CPU %, memory %, network rates, process
# count.  It does NOT record battery draw, GPU utilisation or disk throughput —
# those stay unseeded and are reported as such, never invented.
COLLECTOR_HISTORY_TTL = 60.0
COLLECTOR_HISTORY_MAX_BYTES = 30 * 1024 * 1024
COLLECTOR_HISTORY_MAX_FILES = 3
_COLLECTOR_HIST_CACHE = {"mono": 0.0, "key": None, "rows": None, "meta": None}


def _collector_row(d):
    """One collector snapshot → the small dict the seed/analysis needs."""
    ts = d.get("timestamp") or d.get("ts")
    if isinstance(ts, str):
        e = _ts_epoch(ts)
    elif isinstance(ts, (int, float)):
        e = float(ts)
    else:
        e = None
    if e is None:
        return None
    cpu = d.get("cpu") or {}
    mem = d.get("memory") or {}
    energy = d.get("energy") or {}
    rx = tx = 0.0
    rx_n = tx_n = 0
    for nic in d.get("networks") or []:
        if not isinstance(nic, dict) or nic.get("name") == "lo0":
            continue   # loopback would double-count local traffic
        v = nic.get("rx_bps")
        if isinstance(v, (int, float)):
            rx += float(v)
            rx_n += 1
        v = nic.get("tx_bps")
        if isinstance(v, (int, float)):
            tx += float(v)
            tx_n += 1
    per_app = {}
    for r in ((d.get("processes") or {}).get("by_energy") or []):
        if not isinstance(r, dict):
            continue
        w = r.get("watts")
        name = r.get("name") or ("pid %s" % r.get("pid"))
        if isinstance(w, (int, float)):
            per_app[name] = per_app.get(name, 0.0) + float(w)
    return {
        "ts": e,
        "attributed_w": energy.get("attributed_w")
        if isinstance(energy.get("attributed_w"), (int, float)) else None,
        "cpu": cpu.get("total_pct") if isinstance(cpu.get("total_pct"), (int, float)) else None,
        "ram": mem.get("pct") if isinstance(mem.get("pct"), (int, float)) else None,
        "down_bps": rx if rx_n else None,
        "up_bps": tx if tx_n else None,
        "proc_count": d.get("process_count"),
        "per_app": per_app,
        "temp_c": d.get("temperature") if isinstance(d.get("temperature"), (int, float)) else None,
    }


def _collector_history(window_s, now=None):
    """Bounded tail read of the collector log → measured rows inside the window.

    Returns (rows, meta).  `meta` is the disclosure: how many bytes were read,
    how much of the requested window those bytes covered, and which fields the
    store simply does not contain.  Cached for COLLECTOR_HISTORY_TTL because
    /api/power/history is polled every 5 s and parsing 30 MB per poll would make
    the dashboard the busiest process on the machine.
    """
    now = time.time() if now is None else now
    cut = now - window_s
    try:
        names = sorted(n for n in os.listdir(COLLECTOR_LOGS) if n.endswith(".jsonl"))
    except OSError:
        return [], {"available": False, "rows": 0,
                    "reason": "collector log directory unreadable (%s)" % COLLECTOR_LOGS}
    if not names:
        return [], {"available": False, "rows": 0,
                    "reason": "collector has written no JSONL log yet"}
    files = list(reversed(names))[:COLLECTOR_HISTORY_MAX_FILES]
    key = (tuple(files), int(window_s))
    cached = _COLLECTOR_HIST_CACHE
    if (cached["rows"] is not None and cached["key"] == key
            and time.monotonic() - cached["mono"] < COLLECTOR_HISTORY_TTL):
        return cached["rows"], cached["meta"]
    rows = []
    budget = COLLECTOR_HISTORY_MAX_BYTES
    scanned = 0
    for name in files:
        if budget <= 0:
            break
        path = os.path.join(COLLECTOR_LOGS, name)
        try:
            size = os.path.getsize(path)
        except OSError:
            continue
        take = int(min(budget, size))
        try:
            with open(path, "rb") as f:
                f.seek(max(0, size - take))
                raw = f.read()
        except OSError:
            continue
        scanned += take
        budget -= take
        for line in raw.decode("utf-8", errors="replace").splitlines():
            line = line.strip()
            if not line.startswith("{"):
                continue
            try:
                d = json.loads(line)
            except ValueError:
                continue
            row = _collector_row(d)
            if row is not None and row["ts"] >= cut:
                rows.append(row)
        if rows and min(r["ts"] for r in rows) <= cut:
            break
    rows.sort(key=lambda r: r["ts"])
    # A rotated file can repeat a sweep at the boundary; keep one row per epoch.
    dedup = []
    for r in rows:
        if dedup and abs(dedup[-1]["ts"] - r["ts"]) < 0.5:
            continue
        dedup.append(r)
    rows = dedup
    covered = (rows[-1]["ts"] - rows[0]["ts"]) if len(rows) > 1 else 0.0
    meta = {
        "available": bool(rows),
        "rows": len(rows),
        "scanned_bytes": scanned,
        "cap_bytes": COLLECTOR_HISTORY_MAX_BYTES,
        "covered_s": round(covered, 1),
        "window_s": window_s,
        "coverage_pct": round(min(100.0, 100.0 * covered / window_s), 1) if window_s else 0.0,
        "fields": ["attributed_w", "per_app watts", "cpu", "ram",
                   "down_bps", "up_bps", "proc_count"],
        "missing": {
            "battery_w": "the collector log has no battery voltage/current field",
            "gpu_util_pct": "the collector records gpu_time only "
                            "(unavailable_no_rusage_field), never GPU utilisation",
            "disk_bps": "the collector records disk capacity, not byte counters",
        },
        "source": COLLECTOR_LOGS,
    }
    if not rows:
        meta["reason"] = "no collector rows inside this window"
    cached.update({"mono": time.monotonic(), "key": key, "rows": rows, "meta": meta})
    return rows, meta


def _merge_power_points(live_points, hist_rows, bucket_s):
    """Union live ring points with seeded history; the live sample wins.

    History supplies `src: "history"` and only the fields the collector
    actually measured (attributed W).  A same-bucket live point keeps its own
    values and merely gains the history figure when it has none.
    """
    out = {int(p["ts"]): dict(p) for p in live_points}
    if hist_rows and bucket_s:
        acc = {}
        for r in hist_rows:
            w = r.get("attributed_w")
            if not isinstance(w, (int, float)):
                continue
            key = int(r["ts"] // bucket_s) * bucket_s
            slot = acc.setdefault(key, {"sum": 0.0, "n": 0})
            slot["sum"] += w
            slot["n"] += 1
        for key, slot in acc.items():
            mean = round(slot["sum"] / slot["n"], 3)
            if key in out:
                pt = out[key]
                pt["history_attributed_w"] = mean
                if pt.get("attributed_w") is None:
                    pt["attributed_w"] = mean
                continue
            out[key] = {"ts": key, "src": "history", "bucket_s": bucket_s,
                        "battery_w": None, "adapter_w": None, "package_w": None,
                        "pct": None, "time_remaining_s": None,
                        "cpu_w": None, "gpu_w": None, "ane_w": None,
                        "attributed_w": mean, "history_attributed_w": mean,
                        "top_app": None, "charging": None}
    return [out[k] for k in sorted(out)]


def _integrate_wh(points, key, gap_s=120.0):
    """∫ value dt in Wh (trapezoid); a gap longer than gap_s is not interpolated."""
    ws = 0.0
    pairs = 0
    prev = None
    for p in points:
        v = p.get(key)
        if not isinstance(v, (int, float)):
            prev = None
            continue
        if prev is not None:
            dt = p["ts"] - prev[0]
            if 0 < dt <= gap_s:
                ws += (v + prev[1]) / 2.0 * dt
                pairs += 1
        prev = (p["ts"], v)
    return round(ws / 3600.0, 4), pairs


def _p95(vals):
    nums = sorted(v for v in vals if isinstance(v, (int, float)))
    if not nums:
        return None
    return round(nums[min(len(nums) - 1, int(math.ceil(len(nums) * 0.95)) - 1)], 3)


def _per_app_wh(hist_rows, window_s):
    """Per-app Wh over the window, integrated from the collector sweeps.

    Rectangular rule over consecutive sweeps (cadence ~6 s, capped at 30 s so a
    sleep gap is not charged as consumption).  The collector records only its
    TOP-25 by energy per sweep, so this is a measured FLOOR of per-app energy,
    not the whole machine — `basis` says exactly that.
    """
    totals = {}
    latest = {}
    prev_ts = None
    for r in hist_rows:
        apps = r.get("per_app") or {}
        for name, w in apps.items():
            latest[name] = w
        if prev_ts is not None:
            dt = r["ts"] - prev_ts
            if 0 < dt <= 30:
                for name, w in apps.items():
                    totals[name] = totals.get(name, 0.0) + w * dt / 3600.0
        prev_ts = r["ts"]
    if not totals:
        return [], "no collector sweeps inside this window, so no per-app Wh"
    total = sum(totals.values())
    rows = []
    for name, wh in sorted(totals.items(), key=lambda kv: -kv[1]):
        rows.append({"name": name, "wh": round(wh, 5),
                     "share_pct": round(100.0 * wh / total, 2) if total else None,
                     "latest_w": round(latest.get(name, 0.0), 3)})
    return rows[:15], ("rectangular integration of the collector's top-25 by "
                       "energy per sweep — a measured floor, gaps > 30 s excluded")


def _pct_series(points):
    return [(p["ts"], p["pct"]) for p in points
            if isinstance(p.get("pct"), (int, float))]


def _power_analysis(points, window_s, now, batt, per_app, hist_meta):
    """Everything the power page can honestly compute from this window."""
    batt_pts = [p for p in points if isinstance(p.get("battery_w"), (int, float))]
    vals = [p["battery_w"] for p in batt_pts]
    peak_pt = max(batt_pts, key=lambda p: p["battery_w"]) if batt_pts else None
    battery_wh, battery_pairs = _integrate_wh(points, "battery_w")
    attributed_wh, attributed_pairs = _integrate_wh(points, "attributed_w")
    adapter_wh, adapter_pairs = _integrate_wh(points, "adapter_w")
    package_wh, package_pairs = _integrate_wh(points, "package_w")
    cpu_wh, cpu_pairs = _integrate_wh(points, "cpu_w")
    gpu_wh, gpu_pairs = _integrate_wh(points, "gpu_w")
    ane_wh, ane_pairs = _integrate_wh(points, "ane_w")
    # An integral with zero measured pairs is NOT 0 Wh — it is "not measured",
    # so it is published as null and the UI prints the reason instead.
    battery_wh = battery_wh if battery_pairs else None
    attributed_wh = attributed_wh if attributed_pairs else None
    adapter_wh = adapter_wh if adapter_pairs else None
    package_wh = package_wh if package_pairs else None
    cpu_wh = cpu_wh if cpu_pairs else None
    gpu_wh = gpu_wh if gpu_pairs else None
    ane_wh = ane_wh if ane_pairs else None

    # Drain rate from the measured charge series only (never from a projection).
    pct = _pct_series(points)
    drain_pct_per_h = None
    projected_empty_s = None
    charge_cycles = 0
    if len(pct) >= 2:
        span = pct[-1][0] - pct[0][0]
        if span >= 120:
            delta = pct[-1][1] - pct[0][1]
            drain_pct_per_h = round(-delta / (span / 3600.0), 3)
        direction = None
        for (_t0, v0), (_t1, v1) in zip(pct, pct[1:]):
            d = v1 - v0
            if d == 0:
                continue
            nd = 1 if d > 0 else -1
            if direction is not None and nd != direction and nd > 0:
                charge_cycles += 1
            direction = nd
        if drain_pct_per_h is not None and drain_pct_per_h > 0.01 and pct[-1][1] > 0:
            projected_empty_s = int(round(pct[-1][1] / drain_pct_per_h * 3600.0))

    # Charging efficiency: measured only when both sides exist at the same time.
    efficiency = None
    efficiency_note = None
    adapter_now = None
    drawn_now = None
    recent = [p for p in points if p["ts"] >= now - 120]
    for p in reversed(recent):
        if (isinstance(p.get("adapter_w"), (int, float)) and p["adapter_w"] > 0
                and isinstance(p.get("battery_w"), (int, float))):
            efficiency = round(100.0 * p["battery_w"] / p["adapter_w"], 1)
            adapter_now = p["adapter_w"]
            drawn_now = p["battery_w"]
            efficiency_note = ("estimate: battery draw ÷ adapter input at the same "
                               "sample; charging loss and system load are inside it")
            break
    adapter_vals = [p["adapter_w"] for p in points
                    if isinstance(p.get("adapter_w"), (int, float))]
    if adapter_now is None and adapter_vals:
        adapter_now = adapter_vals[-1]

    return {
        "window_s": window_s,
        "samples": len(points),
        "energy": {
            "battery_wh": battery_wh,
            "attributed_wh": attributed_wh,
            "adapter_wh": adapter_wh,
            "package_wh": package_wh,
            "cpu_wh": cpu_wh,
            "gpu_wh": gpu_wh,
            "ane_wh": ane_wh,
            "pairs": {"battery": battery_pairs, "attributed": attributed_pairs,
                      "adapter": adapter_pairs, "package": package_pairs,
                      "cpu": cpu_pairs, "gpu": gpu_pairs, "ane": ane_pairs},
            "method": "trapezoid over measured W samples; gaps > 120 s excluded",
            "package_source": ("root powermetrics samples in this window "
                               "(CPU + GPU + ANE)" if package_pairs else None),
        },
        "battery": {
            "available": bool(batt_pts),
            "mean_w": round(sum(vals) / len(vals), 3) if vals else None,
            "peak_w": round(peak_pt["battery_w"], 3) if peak_pt else None,
            "peak_ts": peak_pt["ts"] if peak_pt else None,
            "p95_w": _p95(vals),
            "min_w": round(min(vals), 3) if vals else None,
            "samples": len(vals),
            "reason": None if batt_pts else
                      "no battery-draw samples in this window (15 s ring)",
        },
        "adapter": {
            "available": bool(adapter_vals),
            "now_w": adapter_now,
            "mean_w": round(sum(adapter_vals) / len(adapter_vals), 3) if adapter_vals else None,
            "rated_w": batt.get("adapter_rated_w"),
            "external_connected": batt.get("external_connected"),
            "efficiency_pct": efficiency,
            "efficiency_note": efficiency_note,
            "reason": None if adapter_vals else
                      ("on battery — ioreg AppleRawAdapterDetails is empty while "
                       "ExternalConnected is false"),
        },
        "drain": {
            "pct_per_h": drain_pct_per_h,
            "projected_empty_s": projected_empty_s,
            "charge_cycles_in_window": charge_cycles,
            "equivalent_full_cycles": (
                round(battery_wh / batt["nominal_wh"], 4)
                if isinstance(battery_wh, (int, float))
                and isinstance(batt.get("nominal_wh"), (int, float))
                and batt["nominal_wh"] > 0
                else None),
            "pack_wh": batt.get("nominal_wh"),
            "reason": None if drain_pct_per_h is not None else
                      "not enough charge samples (needs 2 samples over 2 min)",
        },
        "per_app": {"rows": per_app or [], "basis": None},
        "tariff": {
            "configured": False,
            "cost": None,
            "detail": "no electricity tariff configured — cost stays hidden "
                      "until one is set (never assumed)",
        },
        "excluded": {
            "package_w": None if package_pairs else
                         "needs root — no privileged powermetrics sample in this window",
            "cpu_w": None if cpu_pairs else
                     "needs root — no privileged CPU power sample in this window",
            "gpu_w": None if gpu_pairs else
                     "needs root — no privileged GPU power sample in this window",
            "ane_w": None if ane_pairs else
                     "needs root — no privileged ANE power sample in this window",
        },
        "history": hist_meta,
    }


def _per_app_series(hist_rows, bucket_s, top_names, cap=120):
    """Per-app watts over the window from the collector sweeps (real series).

    Each sweep measures only its top-25 apps, so a point exists only where that
    app was measured; a missing bucket stays missing (no interpolation).  The
    drawer plots these watts, bucketed to the chart's own bucket width.
    """
    if not hist_rows or not top_names:
        return {"names": list(top_names or []), "points": [], "bucket_s": bucket_s,
                "basis": "collector top-25 by energy per sweep"}
    wanted = set(top_names)
    acc = {}
    for r in hist_rows:
        apps = r.get("per_app") or {}
        key = int(r["ts"] // bucket_s) * bucket_s
        slot = acc.setdefault(key, {})
        for name, w in apps.items():
            if name in wanted:
                s = slot.setdefault(name, {"sum": 0.0, "n": 0})
                s["sum"] += w
                s["n"] += 1
    pts = []
    for ts in sorted(acc):
        row = {"ts": ts}
        for name, s in acc[ts].items():
            row[name] = round(s["sum"] / s["n"], 3)
        pts.append(row)
    if len(pts) > cap:
        step = math.ceil(len(pts) / cap)
        pts = pts[::step]
    return {"names": list(top_names), "points": pts, "bucket_s": bucket_s,
            "basis": "collector top-25 by energy per sweep; a gap means the app "
                     "was not in that sweep's top 25"}


def _coverage_text(sampling_s, live_n, hist_n, oldest_ts, now):
    """`sampling every 15 s · 78 samples · since 03:32 (36 min)` + history split."""
    parts = ["sampling every %s s" % (("%g" % sampling_s) if sampling_s else "?")]
    if hist_n:
        parts.append("%d live + %d history samples" % (live_n, hist_n))
    else:
        parts.append("%d samples" % live_n)
    if oldest_ts:
        since = datetime.fromtimestamp(oldest_ts).strftime("%H:%M")
        covered = max(0.0, now - oldest_ts)
        parts.append("since %s (%s)" % (since, fmt_span(covered)))
    else:
        parts.append("no samples yet")
    return " · ".join(parts)


def power_history_payload(rng=None):
    """Build /api/power/history: series, analysis and the coverage disclosure."""
    now = time.time()
    window_s = RANGES.get(rng, 3600) if rng else 3600
    bucket_s = POWER_RING._tier_for(window_s)[0]
    live_points = POWER_RING.series(window_s, now)
    hist_rows, hist_meta = _collector_history(window_s, now)
    points = _merge_power_points(live_points, hist_rows, bucket_s)
    batt = _read_battery_state()
    per_app, per_app_basis = ([], None)
    per_app_series = None
    analysis = None
    if rng:
        # Only the ranged request needs the analysis block and the per-app
        # integration; the UI's 1 s poll calls this every second for `current`
        # and `windows` alone, and must stay cheap.
        per_app, per_app_basis = _per_app_wh(hist_rows, window_s)
        analysis = _power_analysis(points, window_s, now, batt, per_app, hist_meta)
        analysis["per_app"]["basis"] = per_app_basis
        top_names = [r["name"] for r in (per_app or [])[:5]]
        per_app_series = _per_app_series(hist_rows, bucket_s, top_names)
    live_n = sum(1 for p in points if p.get("src") == "live")
    hist_n = sum(1 for p in points if p.get("src") == "history")
    oldest = points[0]["ts"] if points else None
    newest = points[-1]["ts"] if points else None

    out = {
        "ts": int(now),
        "mode": "fixture" if FIXTURE else "live",
        "sampling_s": POWER_SAMPLE_SECS,
        "samples": len(points),
        # T20: the cadence, named explicitly so the UI can print it and a
        # reviewer can compare it against the installed plist in one glance.
        "powermetrics": {
            "interval_ms": POWERMETRICS_INTERVAL_MS,
            "sampling_s": POWERMETRICS_SAMPLE_SECS,
            "daemon_arg": "--interval-ms %d" % POWERMETRICS_INTERVAL_MS,
            "sources": "syswatch-powerd: powermetrics package/CPU/GPU/ANE watts "
                       "+ AppleSMC die temperatures",
        },
        "live_samples": live_n,
        "history_samples": hist_n,
        "oldest_ts": oldest,
        "newest_ts": newest,
        "coverage_text": _coverage_text(POWER_SAMPLE_SECS, live_n, hist_n, oldest, now),
        "root": (lambda r: {
            "available": bool(r.get("available")),
            "age_s": r.get("age_s"),
            "reason": r.get("reason"),
            "cpu_w": (r.get("data") or {}).get("cpu_w"),
            "gpu_w": (r.get("data") or {}).get("gpu_w"),
            "ane_w": (r.get("data") or {}).get("ane_w"),
            "package_w": (r.get("data") or {}).get("package_w"),
            # T16B emits cpu_avg_c/gpu_avg_c; older builds used cpu_temp_c.
            # This payload feeds the hero band — alias so the chip always
            # shows the value the daemon actually wrote.
            "cpu_temp_c": (r.get("data") or {}).get("cpu_temp_c")
                if (r.get("data") or {}).get("cpu_temp_c") is not None
                else (r.get("data") or {}).get("cpu_avg_c"),
            "gpu_temp_c": (r.get("data") or {}).get("gpu_temp_c")
                if (r.get("data") or {}).get("gpu_temp_c") is not None
                else (r.get("data") or {}).get("gpu_avg_c"),
            "cpu_avg_c": (r.get("data") or {}).get("cpu_avg_c"),
            "gpu_avg_c": (r.get("data") or {}).get("gpu_avg_c"),
            "cpu_cluster_temps_c": (r.get("data") or {}).get("cpu_cluster_temps_c"),
            "thermal_pressure": (r.get("data") or {}).get("thermal_pressure"),
            "fans_rpm": (r.get("data") or {}).get("fans_rpm"),
            "ane_util_pct": (r.get("data") or {}).get("ane_util_pct"),
            "source": ("powermetrics via root LaunchDaemon %s — read-only file, "
                       "dashboard stays unprivileged" % DAEMON_LABEL),
        })(root_sample()),
        "coverage_note": (
            "history seeds attributed per-app W only — battery draw, adapter "
            "input and package W are live-ring series and start where the "
            "15 s ring started" if hist_n else
            "live 15 s ring only; no collector history falls inside this window"),
        "ring": POWER_RING.state(now),
        "windows": POWER_RING.all_windows(now),
        "analysis": analysis,
        "per_app_series": per_app_series,
        "history": hist_meta,
        "honesty": {
            "battery_w": "measured V*A from AppleSmartBattery (unprivileged ioreg)",
            "adapter_w": "ioreg AppleRawAdapterDetails while connected; empty on battery",
            "package_w": ("root powermetrics 'Combined Power (CPU + GPU + ANE)' via the "
                          "T15 daemon file; null while the daemon is absent or its "
                          "sample is > 30 s old"),
            "cpu_w": "root powermetrics 'CPU Power' line (mW → W); null without the daemon",
            "gpu_w": "root powermetrics 'GPU Power' line (mW → W); null without the daemon",
            "ane_w": "root powermetrics 'ANE Power' line (mW → W); null without the daemon",
            "attributed_w": "sum of per-PID rusage energy deltas (a measured floor)",
            "history": "seeded from the collector JSONL under a hard byte cap; the "
                       "coverage line states how much of the window that covered",
            "mean": "time-weighted bucket mean",
            "max": "highest single sample inside the window",
        },
    }
    current = {
        "battery_w": None, "adapter_w": None, "package_w": None,
        "attributed_w": None, "charging": None, "pct": None,
        "time_remaining_s": None, "top_app": None,
        "cpu_w": None, "gpu_w": None, "ane_w": None,
    }
    last = POWER_RING.state(now)["last"]
    if last:
        current.update({k: last.get(k) for k in (
            "battery_w", "adapter_w", "package_w", "attributed_w", "charging",
            "pct", "time_remaining_s", "top_app", "cpu_w", "gpu_w", "ane_w")})
        current["ts"] = last.get("ts")
    # A package number is only current while the privileged sample is fresh;
    # otherwise it is withheld here rather than shown as a live reading.
    root_now = out["root"]
    if not root_now.get("available"):
        current["package_w"] = None
        current["cpu_w"] = None
        current["gpu_w"] = None
        current["ane_w"] = None
    current["root_available"] = bool(root_now.get("available"))
    current["root_reason"] = root_now.get("reason")
    current["external_connected"] = batt.get("external_connected")
    current["adapter_rated_w"] = batt.get("adapter_rated_w")
    if current.get("pct") is None:
        current["pct"] = batt.get("pct")
    out["current"] = current
    if rng:
        out["range"] = rng
        out["points"] = points
        out["ticks"] = tick_ladder(rng, now - window_s, now)
        out["bucket_s"] = bucket_s
        out["window_s"] = window_s
        if not points:
            out["available"] = False
            out["unavailable_reason"] = (
                "power ring has no samples in this window yet (ring age %.0fs, "
                "collector history holds %d rows)"
                % (now - POWER_RING.started_ts, len(hist_rows)))
        else:
            out["available"] = True
            if live_n == 0:
                out["note"] = ("this window is seeded from the collector log only — "
                               "no live 15 s ring samples fall inside it")
    return out

# ---------------------------------------------------------- analytics (T13)
# Every number below is derived from a measured series.  Nothing is modelled,
# extrapolated or filled in: a window with no samples reports null + a reason,
# and a metric this host cannot measure is listed under `unavailable`.
ANALYTICS_TTL = 20.0
_ANALYTICS_CACHE = {"mono": 0.0, "payload": None}
# T14B: the model page asks for a specific range (/api/analytics?range=1h), so
# the cache is per range instead of one global payload.
_ANALYTICS_CACHE_BY_RANGE = {}
_ANALYTICS_LOCK = threading.Lock()
# The per-pid CPU baseline must be refreshed on its own cadence: a scan only
# yields a percentage if a previous scan gave it a cpu_times baseline.
TOP_APPS_TTL = 20.0
TOP_APPS_WARM_SECS = 15.0
_TOP_APPS_CACHE = {"mono": 0.0, "rows": None, "scanned": 0, "baseline": {}}


def _coalesce_same_ts(points, ts_key="ts"):
    """Merge samples that land on the same timestamp into one point.

    T13: the live sampler (1 s) and the Rust collector (~6 s) both feed the
    same ring, so the same second often carries two measurements with
    different coverage (the collector has disk capacity, the live tick does
    not). Counting both would double-weight that second in every mean, and
    plotting both draws two dots on one x. Merge instead: keep every non-null
    field, prefer the first non-null seen for a given key.

    Input must be sorted ascending by timestamp; order is preserved.
    """
    out = []
    for p in points:
        if out and p.get(ts_key) == out[-1].get(ts_key):
            merged = out[-1]
            for k, v in p.items():
                if k != ts_key and merged.get(k) is None and v is not None:
                    merged[k] = v
        else:
            out.append(dict(p))
    return out


def _hist_points():
    """Oldest-first list of (epoch, point) from the history index, coalesced.

    Coalescing is keyed on the numeric epoch, not the raw `ts` string: the
    collector emits "...02Z" and the live sampler "...02+00:00" — the same
    second in two spellings.
    """
    with HIST.lock:
        raw = list(HIST.pts)
    out = []
    for p in raw:
        e = p.get("e")
        if e is None:
            e = _ts_epoch(p.get("ts"))
        if e is not None:
            out.append((e, p))
    out.sort(key=lambda x: x[0])
    merged = []
    for e, p in out:
        if merged and merged[-1][0] == e:
            held = merged[-1][1]
            for k, v in p.items():
                if k != "ts" and held.get(k) is None and v is not None:
                    held[k] = v
        else:
            merged.append((e, dict(p)))
    return merged


def _peak(vals):
    """(max, ts_of_max) for a list of (value, ts) pairs; (None, None) if empty."""
    best = None
    for v, ts in vals:
        if best is None or v > best[0]:
            best = (v, ts)
    return (best[0], best[1]) if best else (None, None)


def _range_stats(points, epochs, now, span):
    """Mean + peak for cpu / memory / network rx-tx inside one window."""
    cut = now - span
    start = bisect.bisect_left(epochs, cut)
    cpu_sum = cpu_n = 0
    ram_sum = ram_n = 0
    rx_sum = rx_n = 0
    tx_sum = tx_n = 0
    cpu_peaks = []
    ram_peaks = []
    rx_peaks = []
    tx_peaks = []
    for e, p in points[start:]:
        if e > now:
            break
        v = p.get("cpu")
        if isinstance(v, (int, float)):
            cpu_sum += v
            cpu_n += 1
            cpu_peaks.append((v, e))
        v = p.get("ram")
        if isinstance(v, (int, float)):
            ram_sum += v
            ram_n += 1
            ram_peaks.append((v, e))
        v = p.get("down_bps")
        if isinstance(v, (int, float)):
            rx_sum += v
            rx_n += 1
            rx_peaks.append((v, e))
        v = p.get("up_bps")
        if isinstance(v, (int, float)):
            tx_sum += v
            tx_n += 1
            tx_peaks.append((v, e))
    cpu_max, cpu_max_ts = _peak(cpu_peaks)
    ram_max, ram_max_ts = _peak(ram_peaks)
    rx_max, rx_max_ts = _peak(rx_peaks)
    tx_max, tx_max_ts = _peak(tx_peaks)
    return {
        "window_s": span,
        "samples": cpu_n,
        "cpu": {"mean_pct": round(cpu_sum / cpu_n, 2) if cpu_n else None,
                "peak_pct": round(cpu_max, 2) if cpu_max is not None else None,
                "peak_ts": cpu_max_ts},
        "memory": {"mean_pct": round(ram_sum / ram_n, 2) if ram_n else None,
                   "peak_pct": round(ram_max, 2) if ram_max is not None else None,
                   "peak_ts": ram_max_ts},
        "network": {
            "rx_mean_mbps": round(rx_sum / rx_n / 1e6, 3) if rx_n else None,
            "rx_peak_mbps": round(rx_max / 1e6, 3) if rx_max is not None else None,
            "rx_peak_ts": rx_max_ts,
            "tx_mean_mbps": round(tx_sum / tx_n / 1e6, 3) if tx_n else None,
            "tx_peak_mbps": round(tx_max / 1e6, 3) if tx_max is not None else None,
            "tx_peak_ts": tx_max_ts,
        },
        "available": bool(cpu_n or ram_n or rx_n),
    }


# T13: nine ranges over a 120k-point ring used to mean ~1M python iterations per
# /api/analytics call (measured 1.5-3 s of CPU, i.e. the dashboard itself showed
# up as the busiest CPU process). One pass into 10 s buckets keeps the means
# exact (they are sample-weighted) and the peaks exact (a bucket's max IS a real
# sample, with its real timestamp); the range queries then touch ~12k buckets
# instead of 120k points.
STATS_BUCKET_S = 10


def _stats_summary(points):
    """10 s buckets: sample-weighted sums + exact max (value and timestamp).

    Unrolled per metric on purpose: the generic `for name, src` version built
    four strings per point per metric, which measured ~4x slower on a 120k
    point ring.
    """
    buckets = []
    cur = None
    for e, p in points:
        key = int(e // STATS_BUCKET_S) * STATS_BUCKET_S
        if cur is None or cur["ts"] != key:
            cur = {"ts": key, "n": 0,
                   "cpu_sum": 0.0, "cpu_n": 0, "cpu_max": None, "cpu_max_ts": None,
                   "ram_sum": 0.0, "ram_n": 0, "ram_max": None, "ram_max_ts": None,
                   "rx_sum": 0.0, "rx_n": 0, "rx_max": None, "rx_max_ts": None,
                   "tx_sum": 0.0, "tx_n": 0, "tx_max": None, "tx_max_ts": None}
            buckets.append(cur)
        cur["n"] += 1
        v = p.get("cpu")
        if isinstance(v, (int, float)):
            cur["cpu_sum"] += v
            cur["cpu_n"] += 1
            if cur["cpu_max"] is None or v > cur["cpu_max"]:
                cur["cpu_max"] = v
                cur["cpu_max_ts"] = e
        v = p.get("ram")
        if isinstance(v, (int, float)):
            cur["ram_sum"] += v
            cur["ram_n"] += 1
            if cur["ram_max"] is None or v > cur["ram_max"]:
                cur["ram_max"] = v
                cur["ram_max_ts"] = e
        v = p.get("down_bps")
        if isinstance(v, (int, float)):
            cur["rx_sum"] += v
            cur["rx_n"] += 1
            if cur["rx_max"] is None or v > cur["rx_max"]:
                cur["rx_max"] = v
                cur["rx_max_ts"] = e
        v = p.get("up_bps")
        if isinstance(v, (int, float)):
            cur["tx_sum"] += v
            cur["tx_n"] += 1
            if cur["tx_max"] is None or v > cur["tx_max"]:
                cur["tx_max"] = v
                cur["tx_max_ts"] = e
    return buckets


def _range_stats_from_summary(summary, summary_ts, now, span):
    """Same shape as _range_stats, computed from the 10 s summary."""
    cut = now - span
    start = bisect.bisect_left(summary_ts, cut - STATS_BUCKET_S)
    sums = {"cpu": 0.0, "ram": 0.0, "rx": 0.0, "tx": 0.0}
    counts = {"cpu": 0, "ram": 0, "rx": 0, "tx": 0}
    peaks = {}
    for b in summary[start:]:
        if b["ts"] > now:
            break
        for name in sums:
            if b[name + "_n"]:
                sums[name] += b[name + "_sum"]
                counts[name] += b[name + "_n"]
                mx = b[name + "_max"]
                if mx is not None and (name not in peaks or mx > peaks[name][0]):
                    peaks[name] = (mx, b[name + "_max_ts"])
    cpu_n = counts["cpu"]
    rx_n = counts["rx"]
    tx_n = counts["tx"]
    ram_n = counts["ram"]
    return {
        "window_s": span,
        "samples": cpu_n,
        "resolution_s": STATS_BUCKET_S,
        "cpu": {"mean_pct": round(sums["cpu"] / cpu_n, 2) if cpu_n else None,
                "peak_pct": round(peaks["cpu"][0], 2) if "cpu" in peaks else None,
                "peak_ts": peaks.get("cpu", (None, None))[1]},
        "memory": {"mean_pct": round(sums["ram"] / ram_n, 2) if ram_n else None,
                   "peak_pct": round(peaks["ram"][0], 2) if "ram" in peaks else None,
                   "peak_ts": peaks.get("ram", (None, None))[1]},
        "network": {
            "rx_mean_mbps": round(sums["rx"] / rx_n / 1e6, 3) if rx_n else None,
            "rx_peak_mbps": round(peaks["rx"][0] / 1e6, 3) if "rx" in peaks else None,
            "rx_peak_ts": peaks.get("rx", (None, None))[1],
            "tx_mean_mbps": round(sums["tx"] / tx_n / 1e6, 3) if tx_n else None,
            "tx_peak_mbps": round(peaks["tx"][0] / 1e6, 3) if "tx" in peaks else None,
            "tx_peak_ts": peaks.get("tx", (None, None))[1],
        },
        "available": bool(cpu_n or ram_n or rx_n),
    }


def _busiest_hour_from_summary(summary, summary_ts, now, span=86400):
    """Highest-mean-CPU calendar hour in the last 24 h (from the summary)."""
    cut = now - span
    start = bisect.bisect_left(summary_ts, cut - STATS_BUCKET_S)
    buckets = {}
    for b in summary[start:]:
        if b["ts"] > now:
            break
        if not b["cpu_n"]:
            continue
        key = int(b["ts"] // 3600) * 3600
        a = buckets.setdefault(key, {"sum": 0.0, "n": 0, "max": None})
        a["sum"] += b["cpu_sum"]
        a["n"] += b["cpu_n"]
        if b["cpu_max"] is not None:
            a["max"] = b["cpu_max"] if a["max"] is None else max(a["max"], b["cpu_max"])
    if not buckets:
        return {"hour_start": None, "mean_cpu_pct": None, "samples": 0,
                "reason": "no CPU samples in the last 24 h"}
    key = max(buckets, key=lambda k: buckets[k]["sum"] / buckets[k]["n"])
    b = buckets[key]
    return {
        "hour_start": datetime.fromtimestamp(key, tz=timezone.utc).isoformat(timespec="seconds"),
        "hour_start_epoch": key,
        "mean_cpu_pct": round(b["sum"] / b["n"], 2),
        "peak_cpu_pct": round(b["max"], 2) if b["max"] is not None else None,
        "samples": b["n"],
        "hours_observed": len(buckets),
    }


def _busiest_hour(points, now, span=86400):
    """Highest-mean-CPU calendar hour in the last 24 h."""
    cut = now - span
    buckets = {}
    for e, p in points:
        if e < cut or e > now:
            continue
        v = p.get("cpu")
        if not isinstance(v, (int, float)):
            continue
        key = int(e // 3600) * 3600
        b = buckets.setdefault(key, {"sum": 0.0, "n": 0, "max": None})
        b["sum"] += v
        b["n"] += 1
        b["max"] = v if b["max"] is None else max(b["max"], v)
    if not buckets:
        return {"hour_start": None, "mean_cpu_pct": None, "samples": 0,
                "reason": "no CPU samples in the last 24 h"}
    key = max(buckets, key=lambda k: buckets[k]["sum"] / buckets[k]["n"])
    b = buckets[key]
    return {
        "hour_start": datetime.fromtimestamp(key, tz=timezone.utc).isoformat(timespec="seconds"),
        "hour_start_epoch": key,
        "mean_cpu_pct": round(b["sum"] / b["n"], 2),
        "peak_cpu_pct": round(b["max"], 2) if b["max"] is not None else None,
        "samples": b["n"],
        "hours_observed": len(buckets),
    }


def _network_today(points, now):
    """Integrate the rx/tx rate series since UTC midnight -> GB.

    Trapezoid between consecutive samples; a gap > 15 s is treated as "not
    measured" and contributes nothing (never as zero traffic).  `covered_s`
    reports how much of the day was actually sampled so the number cannot be
    mistaken for a full-day total while the dashboard was down.
    """
    day_start = datetime.fromtimestamp(now, tz=timezone.utc).replace(
        hour=0, minute=0, second=0, microsecond=0).timestamp()
    rx = tx = 0.0
    covered = 0.0
    gaps = 0
    prev = None
    first_ts = None
    for e, p in points:
        if e < day_start or e > now:
            continue
        rx_bps = p.get("down_bps")
        tx_bps = p.get("up_bps")
        if not isinstance(rx_bps, (int, float)) or not isinstance(tx_bps, (int, float)):
            prev = None
            continue
        if first_ts is None:
            first_ts = e
        if prev is not None:
            dt = e - prev[0]
            if dt <= 15:
                rx += (rx_bps + prev[1]) / 2 * dt
                tx += (tx_bps + prev[2]) / 2 * dt
                covered += dt
            else:
                gaps += 1
        prev = (e, rx_bps, tx_bps)
    return {
        "day_start": datetime.fromtimestamp(day_start, tz=timezone.utc).isoformat(timespec="seconds"),
        "rx_gb": round(rx / 1e9, 3),
        "tx_gb": round(tx / 1e9, 3),
        "covered_s": round(covered, 1),
        "first_sample": first_ts,
        "gaps_skipped": gaps,
        "method": "trapezoid over measured rx/tx rate samples; gaps >15 s excluded",
    }


def _top_apps_by_cpu(n=10):
    """Live top-N by CPU, cached TOP_APPS_TTL so the 5 s poll cannot scan pids.

    CPU% is a delta of the pid's own cpu_times between two analytics scans, so
    the FIRST scan after a restart has no baseline and reports null — psutil's
    own cpu_percent() returns 0.0 in that situation, which would render as
    "every app is idle", a value the machine never measured.
    """
    now_wall = time.time()
    cached = _TOP_APPS_CACHE
    if cached["rows"] is not None and time.monotonic() - cached["mono"] < TOP_APPS_TTL:
        return cached["rows"], cached["scanned"]
    if psutil is None:
        return None, 0
    baseline = cached["baseline"]
    wall_gap = now_wall - cached["mono_wall"] if cached.get("mono_wall") else 0.0
    rows = []
    new_baseline = {}
    try:
        for p in psutil.process_iter(attrs=["pid", "name", "cpu_times", "memory_info"]):
            try:
                inf = p.info
                pid = inf["pid"]
                ct = inf.get("cpu_times")
                cpu_secs = (ct.user + ct.system) if ct is not None else None
                mi = inf.get("memory_info")
                pct = None
                if cpu_secs is not None:
                    new_baseline[pid] = cpu_secs
                    prev = baseline.get(pid)
                    if prev is not None and wall_gap > 0.5:
                        pct = round(max(0.0, cpu_secs - prev) / wall_gap * 100, 2)
                rows.append({"pid": pid, "name": inf.get("name") or "?",
                             "cpu_pct": pct,
                             "mem_mb": round((mi.rss if mi else 0) / 1048576, 1)})
            except (psutil.NoSuchProcess, psutil.AccessDenied):
                continue
    except Exception:
        return None, 0
    rows.sort(key=lambda r: (-1 if r["cpu_pct"] is None else r["cpu_pct"]), reverse=True)
    rows = rows[:n]
    cached.update({"mono": time.monotonic(), "mono_wall": now_wall,
                   "rows": rows, "scanned": len(rows),
                   "baseline": new_baseline})
    return rows, len(rows)


def _top_apps_warm_loop():
    """Keep the per-pid CPU baseline warm every TOP_APPS_WARM_SECS.

    A process scan costs ~30-60 ms for ~500 pids on this machine; at 15 s that
    is well under 1 % of a core and it keeps /api/analytics instant, instead of
    making the first visitor wait for a second scan (or read "warming").
    """
    while True:
        try:
            _top_apps_by_cpu(10)
        except Exception:
            pass
        time.sleep(TOP_APPS_WARM_SECS)


def _process_count_trend(points, now, span=3600, cap=60):
    """Process-count series from measured samples (live ring + collector)."""
    cut = now - span
    vals = []
    for e, p in points:
        if e < cut or e > now:
            continue
        c = p.get("proc_count")
        if isinstance(c, int):
            vals.append({"ts": int(e), "count": c})
    if not vals:
        return {"series": [], "current": None, "mean": None,
                "reason": "no process-count samples in the last hour"}
    if len(vals) > cap:
        stride = math.ceil(len(vals) / cap)
        vals = vals[::stride]
    mean = sum(v["count"] for v in vals) / len(vals)
    return {"series": vals, "current": vals[-1]["count"], "mean": round(mean, 1),
            "window_s": span}


MEASURE_SPECS = (
    # id, label, unit, round digits, extractor from a history point
    ("cpu", "CPU", "%", 2, lambda p: p.get("cpu")),
    ("memory", "Memory", "%", 2, lambda p: p.get("ram")),
    ("net_down", "Network in", "Mbps", 3,
     lambda p: (p.get("down_bps") / 1e6) if isinstance(p.get("down_bps"), (int, float)) else None),
    ("net_up", "Network out", "Mbps", 3,
     lambda p: (p.get("up_bps") / 1e6) if isinstance(p.get("up_bps"), (int, float)) else None),
    ("proc_count", "Processes", "n", 0, lambda p: p.get("proc_count")),
    ("attributed_w", "Attributed energy", "W", 3, lambda p: p.get("attributed_w")),
    ("temperature", "Temperature", "°C", 1, lambda p: p.get("temp_c")),
)


def _measure_row(mid, label, unit, digits, cur, prev, span, window_s):
    """One row of the model page's measures table (avg/max/p95/Δ/coverage)."""
    vals = sorted(v for _t, v in cur)
    n = len(vals)
    if not n:
        return {"id": mid, "label": label, "unit": unit, "available": False,
                "reason": "no samples in this window", "samples": 0}
    mean = sum(vals) / n
    p95 = vals[min(n - 1, int(math.ceil(n * 0.95)) - 1)]
    peak_ts = None
    peak = None
    for t, v in cur:
        if peak is None or v > peak:
            peak, peak_ts = v, t
    prev_mean = (sum(v for _t, v in prev) / len(prev)) if prev else None
    delta = round(mean - prev_mean, digits) if prev_mean is not None else None
    delta_pct = (round(100.0 * (mean - prev_mean) / prev_mean, 1)
                 if prev_mean not in (None, 0) else None)
    ts = sorted(t for t, _v in cur)
    coverage = 0.0
    if len(ts) >= 2:
        gaps = [b - a for a, b in zip(ts, ts[1:]) if b > a]
        step = statistics.median(gaps) if gaps else 0
        coverage = round(100.0 * min(window_s, (ts[-1] - ts[0]) + step) / span, 1)
    return {
        "id": mid, "label": label, "unit": unit, "available": True,
        "avg": round(mean, digits), "max": round(peak, digits),
        "max_ts": peak_ts, "p95": round(p95, digits),
        "min": round(vals[0], digits), "median": round(statistics.median(vals), digits),
        "prev_avg": round(prev_mean, digits) if prev_mean is not None else None,
        "delta": delta, "delta_pct": delta_pct,
        "baseline": "previous equal-length window" if prev_mean is not None
                    else "no previous window measured",
        "coverage_pct": coverage, "samples": n,
        "window_s": span, "source": "history index (1 s sampler + collector sweeps)",
    }


def _measures_for_window(points, now, span):
    """avg / max / p95 / Δ vs previous window / coverage for every metric."""
    cut = now - span
    prev_cut = cut - span
    buckets = {m[0]: ([], []) for m in MEASURE_SPECS}
    window_s = span
    for e, p in points:
        if e < prev_cut or e > now:
            continue
        target = 0 if e >= cut else 1
        for mid, _l, _u, _d, fn in MEASURE_SPECS:
            v = fn(p)
            if isinstance(v, (int, float)):
                buckets[mid][target].append((e, float(v)))
    rows = []
    for mid, label, unit, digits, _fn in MEASURE_SPECS:
        cur, prev = buckets[mid]
        rows.append(_measure_row(mid, label, unit, digits, cur, prev, span, window_s))
    # Metrics this machine cannot measure are listed, not omitted: an absent row
    # would look like "zero" rather than "unavailable".
    rows.append({"id": "disk_read", "label": "Disk read", "unit": "B/s",
                 "available": False,
                 "reason": "the collector history records disk capacity, not byte "
                           "counters, so no throughput series exists for a window",
                 "samples": 0})
    # Root-tier watts live in the 15 s power ring, not in the history index:
    # they exist only while the privileged daemon is granted.  A granted window
    # gets real avg/max/p95; an ungranted one says why instead of reading 0 W.
    ring = POWER_RING.series(min(span * 2, RANGES["30d"]), now)
    ring_buckets = {k: ([], []) for k in ("package_w", "cpu_w", "gpu_w")}
    for p in ring:
        e = p.get("ts")
        if not isinstance(e, (int, float)) or e < prev_cut or e > now:
            continue
        target = 0 if e >= cut else 1
        for k in ring_buckets:
            v = p.get(k)
            if isinstance(v, (int, float)):
                ring_buckets[k][target].append((e, float(v)))
    for mid, label, digits in (("package_w", "Package power (root)", 2),
                               ("cpu_w", "CPU power (root)", 2),
                               ("gpu_w", "GPU power (root)", 2)):
        cur, prev = ring_buckets[mid]
        row = _measure_row(mid, label, "W", digits, cur, prev, span, window_s)
        if row.get("available"):
            row["source"] = ("root powermetrics 15 s ring (T15 daemon) — "
                             "package total is CPU + GPU + ANE")
        else:
            row["reason"] = ("needs root — no privileged powermetrics sample in "
                             "this window (grant it on the Permissions page)")
        rows.append(row)
    return rows


def _findings_for_window(measures, ranges, busiest, apps, net, power_an, rng):
    """3–4 sentences, each computed from the measured series (never templated)."""
    out = []
    by_id = {m["id"]: m for m in measures}
    cpu = by_id.get("cpu") or {}
    if cpu.get("available"):
        line = ("Over the last %s CPU averaged %s and peaked at %s%s"
                % (rng, _fmt_num(cpu["avg"], "%"), _fmt_num(cpu["max"], "%"),
                   (" at " + fmt_clock_short(cpu["max_ts"])) if cpu.get("max_ts") else ""))
        if cpu.get("delta") is not None:
            line += (", %s than the previous %s"
                     % ("higher" if cpu["delta"] >= 0 else "lower", rng))
        out.append(line + ".")
    mem = by_id.get("memory") or {}
    if mem.get("available") and mem.get("delta") is not None:
        out.append("Memory held %s of RAM (peak %s), %s pp versus the previous window."
                   % (_fmt_num(mem["avg"], "%"), _fmt_num(mem["max"], "%"),
                      ("+" if mem["delta"] >= 0 else "") + str(mem["delta"])))
    if isinstance(busiest, dict) and busiest.get("mean_cpu_pct") is not None:
        out.append("The busiest hour of the last 24 h began at %s with %s average CPU "
                   "across %s observed hours."
                   % (fmt_clock_short(busiest.get("hour_start_epoch")),
                      _fmt_num(busiest["mean_cpu_pct"], "%"),
                      busiest.get("hours_observed", "?")))
    top = next((a for a in (apps or []) if isinstance(a.get("watts"), (int, float))), None)
    if top:
        out.append("The largest single consumer in the latest sweep was %s at %s."
                   % (top.get("name"), _fmt_num(top["watts"], "W")))
    elif isinstance(power_an, dict):
        batt = (power_an.get("battery") or {})
        if batt.get("mean_w") is not None:
            out.append("Battery draw averaged %s over the last %s (peak %s)."
                       % (_fmt_num(batt["mean_w"], "W"), rng,
                          _fmt_num(batt.get("peak_w"), "W")))
    if len(out) < 3 and isinstance(net, dict) and net.get("rx_gb") is not None:
        out.append("Network moved %s GB in and %s GB out today, measured over %s."
                   % (net["rx_gb"], net.get("tx_gb"),
                      fmt_span(net.get("covered_s") or 0)))
    return out[:4]


def _fmt_num(v, unit):
    if not isinstance(v, (int, float)):
        return "unavailable"
    if unit == "%":
        return "%.1f %%" % v
    if unit == "W":
        return "%.2f W" % v
    if unit == "Mbps":
        return "%.2f Mbps" % v
    if unit == "B/s":
        return fmt_bps(v)
    return ("%d" % v) if float(v).is_integer() else ("%.2f" % v)


def fmt_clock_short(epoch):
    """Epoch seconds (or ISO string) → local HH:MM for a sentence."""
    e = epoch
    if isinstance(e, str):
        e = _ts_epoch(e)
    if not isinstance(e, (int, float)):
        return "—"
    return datetime.fromtimestamp(e).strftime("%H:%M")


def _records_for_window(measures, busiest, apps, rng):
    """Records feed: `03:12 · CPU peaked 91 % — Chrome Helper`, from real peaks."""
    by_id = {m["id"]: m for m in measures}
    top = next((a for a in (apps or []) if isinstance(a.get("watts"), (int, float))), None)
    rows = []
    for mid, label, unit in (("cpu", "CPU", "%"), ("memory", "Memory", "%"),
                             ("net_down", "network in", "Mbps")):
        m = by_id.get(mid) or {}
        if m.get("available") and m.get("max_ts"):
            rows.append({"ts": m["max_ts"],
                         "text": "%s peaked %s" % (label, _fmt_num(m["max"], unit)),
                         "metric": mid, "value": m["max"]})
    if isinstance(busiest, dict) and busiest.get("hour_start_epoch"):
        rows.append({"ts": busiest["hour_start_epoch"],
                     "text": "busiest hour began (%s avg CPU)"
                             % _fmt_num(busiest.get("mean_cpu_pct"), "%"),
                     "metric": "cpu", "value": busiest.get("mean_cpu_pct")})
    if top:
        rows.append({"ts": None,
                     "text": "top energy app %s at %s (latest sweep)"
                             % (top.get("name"), _fmt_num(top.get("watts"), "W")),
                     "metric": "energy", "value": top.get("watts")})
    rows.sort(key=lambda r: -(r["ts"] or 0))
    return rows[:8]


def analytics_payload(rng=None):
    """Build /api/analytics — machine usage from the measured series only."""
    now = time.time()
    rng = rng if rng in RANGES else "1h"
    span = RANGES[rng]
    cached = _ANALYTICS_CACHE_BY_RANGE.setdefault(
        rng, {"mono": 0.0, "payload": None})
    if cached["payload"] is not None and time.monotonic() - cached["mono"] < ANALYTICS_TTL:
        return cached["payload"]
    with _ANALYTICS_LOCK:
        if cached["payload"] is not None and time.monotonic() - cached["mono"] < ANALYTICS_TTL:
            return cached["payload"]  # computed while we waited
        t_start = time.monotonic()
        phases = {}

        def phase(name, fn, *args):
            t0 = time.monotonic()
            out = fn(*args)
            phases[name] = round((time.monotonic() - t0) * 1000, 1)
            return out

        points = phase("hist_points", _hist_points)
        epochs = [e for e, _ in points]
        summary = phase("stats_summary", _stats_summary, points)
        summary_ts = [b["ts"] for b in summary]
        ranges = phase("ranges", lambda: {
            name: _range_stats_from_summary(summary, summary_ts, now, span)
            for name, span in RANGES.items()})
        # T14: a short history answered a 30 d query with the same numbers as
        # 7 d and looked like a full window.  Coverage says how much of the
        # window the samples actually account for, so the UI can say "filling".
        for name, secs in RANGES.items():
            row = ranges.get(name)
            if not isinstance(row, dict):
                continue
            n = row.get("samples") or 0
            row["coverage_pct"] = round(
                min(100.0, 100.0 * n * STATS_BUCKET_S / secs), 1) if secs else 0.0
            row["complete"] = row["coverage_pct"] >= 98.0
        by_cpu, scanned = phase("top_apps", _top_apps_by_cpu, 10)
        snap = _latest_collector_snapshot()
        by_energy = _by_app_from_collector(snap) or []
        hist_rows, hist_meta = phase("collector_history", _collector_history, span, now)
        hist_extra = [(r["ts"], {"attributed_w": r.get("attributed_w"),
                                 "temp_c": r.get("temp_c")}) for r in hist_rows]
        measures = phase("measures", _measures_for_window, points + hist_extra, now, span)
        power_an = phase("power_analysis", power_history_payload, rng)
        busiest = _busiest_hour_from_summary(summary, summary_ts, now)
        net_today = phase("network_today", _network_today, points, now)
        findings = _findings_for_window(measures, ranges, busiest, by_energy,
                                        net_today, power_an.get("analysis"), rng)
        records = _records_for_window(measures, busiest, by_energy, rng)
        uptime_s = None
        try:
            if psutil is not None:
                uptime_s = round(now - psutil.boot_time(), 1)
        except Exception:
            uptime_s = None
        load = {"one": None, "five": None, "fifteen": None}
        try:
            if psutil is not None:
                la = psutil.getloadavg()
                load = {"one": la[0], "five": la[1], "fifteen": la[2]}
        except Exception:
            pass
        collector_uptime = (snap or {}).get("uptime_secs")
        payload = {
            "ts": int(now),
            "generated_iso": datetime.fromtimestamp(now, tz=timezone.utc).isoformat(timespec="seconds"),
            "source": {
                "history_points": len(points),
                "history_first": (datetime.fromtimestamp(epochs[0], tz=timezone.utc)
                                  .isoformat(timespec="seconds") if epochs else None),
                "history_last": (datetime.fromtimestamp(epochs[-1], tz=timezone.utc)
                                 .isoformat(timespec="seconds") if epochs else None),
                "collector_snapshot_ts": (snap or {}).get("timestamp"),
                "collector_processes_measured": ((snap or {}).get("energy") or {}).get(
                    "processes_measured"),
                "fixture": bool(FIXTURE),
            },
            "ranges": ranges,
            "range": rng,
            "window_s": span,
            "measures": measures,
            "findings": findings,
            "records": records,
            "history_store": hist_meta,
            "busiest_hour_24h": busiest,
            "stats": {"resolution_s": STATS_BUCKET_S, "buckets": len(summary),
                      "compute_ms": None, "phases_ms": phases,
                      "measures": {m["id"]: {k: m.get(k) for k in (
                          "avg", "max", "p95", "min", "prev_avg", "delta",
                          "delta_pct", "coverage_pct", "samples", "available")}
                          for m in measures}},
            "apps": {
                "by_cpu": by_cpu if by_cpu is not None else [],
                "by_cpu_available": by_cpu is not None,
                "by_cpu_scanned": scanned,
                "by_cpu_basis": "delta of each pid's own cpu_times between analytics "
                                "scans (cache %ds); null until the second scan"
                                % int(TOP_APPS_TTL),
                "by_energy": by_energy,
                "by_energy_basis": "collector processes.by_energy (rusage ri_energy_nj "
                                   "delta over the collector's ~6 s window)",
                "by_energy_available": bool(by_energy),
            },
            "network_today": net_today,
            "machine": {
                "uptime_s": uptime_s,
                "uptime_human": (("%dd %02dh %02dm" % (uptime_s // 86400,
                                                       (uptime_s % 86400) // 3600,
                                                       (uptime_s % 3600) // 60))
                                 if uptime_s else None),
                "collector_uptime_s": collector_uptime,
                "load": load,
                "process_count": phase("proc_trend", _process_count_trend, points, now),
                "proc_count": (snap or {}).get("process_count"),
            },
            "unavailable": {
                "gpu": "the collector log records gpu_time only; GPU utilisation is "
                       "sampled by the dashboard's own 5 s IOAccelerator ring, so it "
                       "is a live number here, not a windowed history series",
                "disk_io": "the collector records disk capacity only; the dashboard's "
                           "5 s ring holds read/write B/s but no on-disk history, so "
                           "no disk throughput measures can be computed for a window",
                "package_total_w": (
                    "root powermetrics 'Combined Power (CPU + GPU + ANE)' via the "
                    "T15 daemon file — measured, reported by the package_w measure "
                    "row while the daemon is granted"
                    if root_sample().get("available") else
                    "needs root — the T15 privileged daemon file is absent or stale, "
                    "and SMC/IOReport are not readable unprivileged"),
            },
        }
        payload["stats"]["compute_ms"] = round((time.monotonic() - t_start) * 1000, 1)
        cached.update({"mono": time.monotonic(), "payload": payload})
        return payload


BOOT_TS = datetime.now(timezone.utc).isoformat(timespec="seconds")
# T4a: monotonic start stamp backing /api/health uptime_s. Monotonic (not
# wall clock) so NTP jumps or manual clock changes can't report a negative
# or wildly wrong uptime.
START_MONO = time.monotonic()


# -------------------------------------------------------- fixture replay (T5)
class FixtureLoader:
    """Load a merged JSONL fixture and serve it like live snapshots.

    Each line is expected to match the shape produced by scripts/istat_export.py:
    {ts, iso, interval, cpu, load, mem, swap_gb, gpu, net, disk, batt, sensors}.
    The loader converts any record into the same nested JSON shape emitted by
    /api/current so the frontend can treat fixture data and live data the same.
    """

    def __init__(self, path):
        self.path = path
        self.records = []
        self._load()

    def _load(self):
        p = self.path
        if not os.path.isabs(p):
            # Resolve relative to the repo root (parent of dashboard/).
            p = os.path.join(os.path.dirname(BASE_DIR), p)
        with open(p, "r", encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                self.records.append(json.loads(line))
        self.records.sort(key=lambda r: r.get("ts", 0))
        log_event("fixture", "loaded %d records from %s" % (len(self.records), self.path))

    @staticmethod
    def _parse_ts(v):
        """Accept epoch int or ISO string; return epoch int or None."""
        if v is None:
            return None
        try:
            return int(v)
        except (TypeError, ValueError):
            pass
        try:
            return int(datetime.fromisoformat(v.replace("Z", "+00:00")).timestamp())
        except (ValueError, TypeError):
            return None

    def current(self):
        """Return the latest fixture record in /api/current shape."""
        if not self.records:
            return None
        return self._to_current(self.records[-1])

    def latest_ts(self):
        """Epoch of the newest fixture record."""
        if not self.records:
            return None
        return self.records[-1].get("ts")

    def graph(self, metric, from_ts=None, to_ts=None):
        """Return all fixture points for a metric, optionally windowed."""
        f = self._parse_ts(from_ts)
        t = self._parse_ts(to_ts)
        pts = []
        for r in self.records:
            ts = r.get("ts")
            if f is not None and ts < f:
                continue
            if t is not None and ts > t:
                continue
            pts.append(self._to_graph_point(metric, r))
        return {"metric": metric, "from": from_ts, "to": to_ts, "points": pts}

    def _to_graph_point(self, metric, r):
        ts = r.get("ts")
        if metric == "cpu":
            cpu = r.get("cpu") or {}
            total = cpu.get("total")
            return {"ts": ts, "cpu": total, "user": cpu.get("user"),
                    "system": cpu.get("system"), "total": total}
        if metric == "memory":
            m = r.get("mem") or {}
            used = (m.get("wired_mb") or 0) + (m.get("app_mb") or 0) + (m.get("compressed_mb") or 0)
            total = m.get("total_mb") or used
            pct = round(used / total * 100, 2) if total else None
            return {"ts": ts, "ram": pct, "pct": pct,
                    "wired_mb": m.get("wired_mb"),
                    "app_mb": m.get("app_mb"),
                    "compressed_mb": m.get("compressed_mb"),
                    "total_mb": total}
        if metric == "network":
            nets = r.get("net") or []
            up = sum(n.get("up_mbps", 0) for n in nets)
            down = sum(n.get("down_mbps", 0) for n in nets)
            return {"ts": ts,
                    "up_mbps": round(up, 3) if up else None,
                    "down_mbps": round(down, 3) if down else None,
                    "up_bps": round(up * 1e6, 3) if up else None,
                    "down_bps": round(down * 1e6, 3) if down else None}
        if metric == "disk":
            disks = r.get("disk") or []
            rd = sum(d.get("read_kbps", 0) for d in disks)
            wr = sum(d.get("write_kbps", 0) for d in disks)
            return {"ts": ts, "read_kbps": round(rd, 3) if rd else None,
                    "write_kbps": round(wr, 3) if wr else None}
        if metric == "gpu":
            gpus = r.get("gpu") or []
            if gpus:
                proc = sum(g.get("proc_pct", 0) for g in gpus) / len(gpus)
            else:
                proc = None
            return {"ts": ts, "gpu_pct": round(proc, 3) if proc is not None else None}
        if metric == "load":
            load = r.get("load") or {}
            return {"ts": ts, "one": load.get("one"),
                    "five": load.get("five"), "fifteen": load.get("fifteen")}
        if metric == "battery":
            batt = r.get("batt") or {}
            return {"ts": ts, "pct": batt.get("pct"),
                    "on_ac": batt.get("on_ac")}
        if metric == "sensors":
            return {"ts": ts, "sensors": r.get("sensors", {})}
        return {"ts": ts}

    def _to_current(self, r):
        m = r.get("mem") or {}
        wired = m.get("wired_mb", 0) or 0
        app = m.get("app_mb", 0) or 0
        compressed = m.get("compressed_mb", 0) or 0
        used_mb = wired + app + compressed
        total_mb = m.get("total_mb") or used_mb
        swap_gb = r.get("swap_gb") or 0
        nets = r.get("net") or []
        down_mbps = sum(n.get("down_mbps", 0) for n in nets)
        up_mbps = sum(n.get("up_mbps", 0) for n in nets)
        cpu = r.get("cpu") or {}
        gpus = r.get("gpu") or []
        gpu_proc = (sum(g.get("proc_pct", 0) for g in gpus) / len(gpus)) if gpus else None
        gpu_mem = (sum(g.get("mem_pct", 0) for g in gpus) / len(gpus)) if gpus else None
        # Sensors are unnamed md5 keys in the fixture; expose them generically.
        temps = [{"label": "sensor " + str(k), "c": v}
                 for k, v in (r.get("sensors") or {}).items()]
        batt = r.get("batt") or {}
        snap = {
            "ts": r.get("iso"),
            "source": "istat_fixture",
            "fixture": True,
            "cpu": {"total_pct": cpu.get("total"),
                    "per_core_pct": [],
                    "freq_mhz": None},
            "ram": {"used_bytes": int(used_mb * 1048576),
                    "total_bytes": int(total_mb * 1048576),
                    "pct": round(used_mb / total_mb * 100, 2) if total_mb else 0,
                    "swap_used_bytes": int(swap_gb * 1073741824),
                    "swap_total_bytes": 0},
            "net": {"ifaces": [{"name": "all",
                                "down_bps": round(down_mbps * 1e6, 1),
                                "up_bps": round(up_mbps * 1e6, 1)}],
                    "down_bps": round(down_mbps * 1e6, 1),
                    "up_bps": round(up_mbps * 1e6, 1)},
            "temps_c": temps,
            "processes": {"by_cpu": [], "by_mem": []},
            "proc_count": 0,
            "boot_ts": BOOT_TS,
            "load": r.get("load"),
            "gpu_pct": round(gpu_proc, 2) if gpu_proc is not None else None,
            "gpu": [{"util_pct": round(g.get("proc_pct"), 2) if g.get("proc_pct") is not None else None,
                     "mem_pct": round(g.get("mem_pct"), 2) if g.get("mem_pct") is not None else None,
                     "temp_c": None, "freq_mhz": None, "fps": None} for g in gpus] if gpus else [],
            "disk": r.get("disk"),
            "battery": {"pct": batt.get("pct"), "on_ac": batt.get("on_ac")},
        }
        # legacy alias used by alert rules and older clients
        snap["memory"] = snap["ram"]
        return snap


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
        # T14: drop the pseudo interfaces macOS insists on exposing (gif0, stf0,
        # anpi0, XHC*, awdl*, llw*, bridge*). They always read 0 B/s and, with
        # the sort below, they crowded the real en0/utun rows out of the top 5.
        skip_prefix = ("lo", "loopback", "gif", "stf", "anpi", "awdl", "llw",
                       "ap1", "bridge", "p2p", "xhc", "vnic", "ipsec")
        for name, st in after.items():
            prev = before.get(name) if before else None
            if not prev:
                continue
            low = name.lower()
            if low.startswith(skip_prefix):
                continue
            down = max(0.0, (st.bytes_recv - prev[1]) / dt)
            up = max(0.0, (st.bytes_sent - prev[0]) / dt)
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
        # One call each for the total and the per-core delta (psutil keeps a
        # separate baseline per shape, so a second total call would read ~0).
        total_pct = psutil.cpu_percent(interval=None)
        per_core = psutil.cpu_percent(interval=None, percpu=True)
        disk_state = disk_usage_state()
        now_mono2 = time.monotonic()
        if self._first_proc_scan or (now_mono2 - self._last_proc_scan) >= self.PROC_SCAN_SECS:
            self._proc_cache = self._scan_processes()
            self._last_proc_scan = now_mono2
            self._first_proc_scan = False
        by_cpu, by_mem, proc_count = self._proc_cache
        now_mono = time.monotonic()
        net2 = psutil.net_io_counters(pernic=True)
        try:
            la = psutil.getloadavg()
        except Exception:
            la = (None, None, None)
        with self.lock:
            prev_t, prev_io = self._last_net or (None, None)
            rates = (self._net_rates(prev_io, net2, now_mono - prev_t)
                     if prev_io and now_mono - prev_t >= 1 else [])
            self._last_net = (now_mono, net2)
            snap = {
                "ts": datetime.now(timezone.utc).isoformat(timespec="seconds"),
                "source": "psutil",
                "cpu": {"total_pct": total_pct,
                        "per_core_pct": [round(p, 1) for p in per_core],
                        "freq_mhz": freq,
                        "cores": len(per_core) or None,
                        # Index-ordered (efficiency 0..E-1, performance E..):
                        # every UI label follows this array, nothing assumes
                        # "first N are performance" (T14C #1). Each row also
                        # carries this tick's util_pct (T14D) so the UI never
                        # has to re-derive the span itself.
                        "clusters": cluster_payload(CPU_CLUSTERS, per_core, freq),
                        "cluster_basis": CLUSTER_BASIS,
                        "load": {"one": la[0], "five": la[1], "fifteen": la[2]}},
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
                "load": {"one": la[0], "five": la[1], "fifteen": la[2]},
                "battery": _read_battery_state(),
                # T14: these three come from the 5 s IO/pressure rings — the
                # 1 s path only reads their heads (no ioreg/sysctl spawn here).
                "gpu": gpu_state(),
                "disk_io": disk_io_state(),
                "disk": dict(disk_state, volumes=disk_volumes()),
                "pressure": pressure_state(
                    total_pct, la[0], CORE_COUNT, vm.percent,
                    disk_state.get("used_pct")),
                # T14 honesty: name what is still genuinely unavailable, with the
                # exact reason.  A missing number is never rendered as 0.
                "unavailable": {
                    "neural_engine": "powermetrics ANEMetrics needs root — "
                                     "arrives with the T15 privileged daemon",
                    "gpu_frequency": "PerformanceStatistics carries no GPU clock "
                                     "on Apple Silicon; needs powermetrics (root)",
                    "package_power": "CPU/GPU/ANE package watts need powermetrics "
                                     "(root) — T15",
                    "temperatures": "no named temperature sensors unprivileged; "
                                    "powermetrics needs root — T15",
                },
            }
            # legacy alias so existing alert rules (metrics read "memory") work
            snap["memory"] = snap["ram"]
            self.current = snap
        return snap


# ============================== per-process range store (T14C #3: consumers)
# WHY this exists: the module pages have to answer "who consumed over the
# selected range", not "who is busy right now".  Nothing on this machine kept
# per-PID history before: the collector log carries its top-25 by energy plus
# the top-5 by CPU/RAM per sweep, and the dashboard's own history file is
# machine-level only (cpu / ram / net).  So the dashboard keeps its own bounded
# ring, filled OFF the 1 s tick path:
#
#   * psutil scan of every PID          -> name, RSS, cpu_times delta   (cheap)
#   * proc_pid_rusage(RUSAGE_INFO_V6)   -> energy nJ + disk byte deltas
#                                        (~25 ms per 400 PIDs, measured)
#   * nettop -P -x -l 1                 -> per-PID byte counters, own cadence
#
# The rusage cost is exactly why this runs at PROC_RING_SECS (20 s) instead of
# on the tick.  No existing throttle (last_disk_refresh, ENERGY_REFRESH,
# last_sweep -- all in the collector) is touched or weakened: this is a separate,
# slower pass inside the dashboard process.
PROC_RING_SECS = 20.0
PROC_RING_MAX_S = 3 * 3600.0            # keep 3 h of sweeps (~15 MB at idle)
PROC_RING_MAX_SWEEPS = int(PROC_RING_MAX_S / PROC_RING_SECS) + 60
NET_SCAN_SECS = 30.0                    # nettop spawn cadence
# A row must earn its place in memory: any measurable CPU / energy / disk /
# network delta, or a resident set of at least PROC_ROW_MIN_MB. An idle daemon
# holding 4 MB is not a "consumer" and would otherwise cost ~400 rows/sweep.
PROC_ROW_MIN_MB = 20.0
PROC_ROW_MIN_CPU = 0.05

# struct rusage_info_v6 field offsets.  Layout from the SDK header
# ($(xcrun --show-sdk-path)/usr/include/sys/resource.h): a 16-byte ri_uuid
# followed by uint64 fields in declaration order.  Verified on this machine:
# ri_diskio_bytesread == 144 matches the Rust collector's compile-time
# offset_of! assert, and a busy loop moved ri_energy_nj by ~7.7e9 nJ in 2 s
# (about 3.9 W).  NOTE: this struct has NO GPU-time field -- per-process GPU
# share is therefore null everywhere, never 0 (see CONSUMER_UNAVAILABLE).
_RUSAGE_BUF_BYTES = 512                 # sizeof(rusage_info_current) == 464
_RUSAGE_OFF = {
    "resident": 64,        # ri_resident_size
    "disk_read": 144,      # ri_diskio_bytesread
    "disk_write": 152,     # ri_diskio_byteswritten
    "energy_nj": 336,      # ri_energy_nj
}
_RUSAGE_V6 = 6

_CONSUMER_UNAVAILABLE = {
    "gpu_pct": ("proc_pid_rusage(RUSAGE_INFO_V6) has no GPU-time field in this "
                "SDK (ri_gpu_time does not exist), so per-process GPU share "
                "cannot be measured -- it is null, never 0"),
}


def _load_libproc():
    """libproc handle for proc_pid_rusage, or None off macOS / on failure."""
    if sys.platform != "darwin":
        return None
    try:
        import ctypes
        import ctypes.util
        path = ctypes.util.find_library("proc") or "/usr/lib/libproc.dylib"
        lib = ctypes.CDLL(path)
        lib.proc_pid_rusage.restype = ctypes.c_int
        lib.proc_pid_rusage.argtypes = [ctypes.c_int, ctypes.c_int,
                                        ctypes.c_void_p]
        return lib
    except Exception:
        return None


_LIBPROC = _load_libproc()


def _rusage_v6(pid, buf):
    """One proc_pid_rusage(RUSAGE_INFO_V6) read -> counters, or None.

    None means "this PID could not be measured" (EPERM on root-owned procs,
    already exited).  Callers keep the field null; it is never filled with 0.
    """
    if _LIBPROC is None:
        return None
    try:
        if _LIBPROC.proc_pid_rusage(int(pid), _RUSAGE_V6, buf) != 0:
            return None
        raw = buf.raw
        return {k: struct.unpack_from("<Q", raw, off)[0]
                for k, off in _RUSAGE_OFF.items()}
    except Exception:
        return None


def _nettop_bytes():
    """Per-PID cumulative (bytes_in, bytes_out) from nettop, or None.

    nettop is the only unprivileged per-process network source on macOS.
    `-P` prints one row per process, `-x` is raw, `-l 1` is a single snapshot.
    nettop cuts the name at ~16 chars and appends the pid as `name.pid`, so the
    pid is recovered from the suffix.
    """
    if sys.platform != "darwin":
        return None
    try:
        raw = subprocess.check_output(
            ["/usr/bin/nettop", "-P", "-x", "-l", "1",
             "-J", "bytes_in,bytes_out"],
            stderr=subprocess.DEVNULL, timeout=20)
    except Exception:
        return None
    out = {}
    for line in raw.decode("utf-8", errors="replace").splitlines()[1:]:
        line = line.strip()
        if not line:
            continue
        parts = line.rsplit(None, 2)     # name.pid   bytes_in   bytes_out
        if len(parts) != 3:
            continue
        head, b_in, b_out = parts
        try:
            n_in, n_out = int(b_in), int(b_out)
        except ValueError:
            continue
        tail = head.rsplit(".", 1)[-1]
        if not tail.isdigit():
            continue
        out[int(tail)] = (n_in, n_out)
    return out


class ProcRange:
    """Bounded per-process history so consumers can be ranked over a window.

    Row layout (plain tuples: 400 PIDs x sweeps of dicts is megabytes of key
    overhead for no benefit):
        (pid, name, cpu_pct, mem_mb, energy_nj, disk_read, disk_write,
         net_in_bps, net_out_bps)
    Any field may be None: null means "not measurable this sweep" (first
    sighting, EPERM, counter reset), never "measured zero".
    """

    def __init__(self):
        self.lock = threading.Lock()
        self.sweeps = []          # oldest first: {"ts", "dt", "rows": [...]}
        self.prev_cpu = {}        # pid -> (cpu_seconds, wall_ts)
        self.prev_energy = {}     # pid -> nJ
        self.prev_disk = {}       # pid -> (read_bytes, write_bytes)
        self.prev_net = {}        # pid -> (bytes_in, bytes_out)
        self.last_net_mono = 0.0
        self.started_ts = None
        self.cadence_s = PROC_RING_SECS
        self.sweeps_done = 0

    # -- one pass ---------------------------------------------------------
    def sweep(self):
        if psutil is None:
            return None
        import ctypes
        wall = time.time()
        mono = time.monotonic()
        buf = ctypes.create_string_buffer(_RUSAGE_BUF_BYTES) if _LIBPROC else None

        cpu_now = {}
        energy_now = {}
        disk_now = {}
        staged = []
        for p in psutil.process_iter(attrs=["pid", "name", "cpu_times",
                                            "memory_info"]):
            try:
                inf = p.info
                pid = int(inf["pid"])
                name = sys.intern(inf.get("name") or ("pid %d" % pid))
                mi = inf.get("memory_info")
                mem_mb = round((mi.rss if mi else 0) / 1048576.0, 1)
                ct = inf.get("cpu_times")
            except (psutil.NoSuchProcess, psutil.AccessDenied):
                continue
            except Exception:
                continue
            staged.append((pid, name, mem_mb, ct))

        # network deltas on their own (slower) cadence
        net_deltas = {}
        if self.last_net_mono == 0.0 or (mono - self.last_net_mono) >= NET_SCAN_SECS:
            cur_net = _nettop_bytes()
            if cur_net is not None:
                dt_net = (mono - self.last_net_mono) if self.last_net_mono else 0.0
                if dt_net > 0:
                    for pid, (b_in, b_out) in cur_net.items():
                        prev = self.prev_net.get(pid)
                        if prev is None:
                            continue        # first sighting: no baseline
                        net_deltas[pid] = (max(0, b_in - prev[0]) / dt_net,
                                           max(0, b_out - prev[1]) / dt_net)
                self.prev_net = cur_net
                self.last_net_mono = mono

        rows = []
        for pid, name, mem_mb, ct in staged:
            cpu_pct = None
            if ct is not None:
                cpu_secs = ct.user + ct.system
                cpu_now[pid] = (cpu_secs, wall)
                prev = self.prev_cpu.get(pid)
                if prev is not None and wall - prev[1] > 0.5:
                    cpu_pct = round(max(0.0, cpu_secs - prev[0]) /
                                    (wall - prev[1]) * 100.0, 2)
            energy = d_read = d_write = None
            if buf is not None:
                r = _rusage_v6(pid, buf)
                if r is not None:
                    e = r["energy_nj"]
                    pe = self.prev_energy.get(pid)
                    if pe is not None and e >= pe:
                        energy = e - pe
                    energy_now[pid] = e
                    rd, wr = r["disk_read"], r["disk_write"]
                    pd = self.prev_disk.get(pid)
                    if pd is not None and rd >= pd[0] and wr >= pd[1]:
                        d_read = rd - pd[0]
                        d_write = wr - pd[1]
                    disk_now[pid] = (rd, wr)
            net = net_deltas.get(pid)
            net_in = net[0] if net else None
            net_out = net[1] if net else None

            measurable = (
                (cpu_pct is not None and cpu_pct >= PROC_ROW_MIN_CPU)
                or (energy or 0) > 0
                or (d_read or 0) > 0 or (d_write or 0) > 0
                or (net_in or 0) > 0 or (net_out or 0) > 0
                or mem_mb >= PROC_ROW_MIN_MB)
            if not measurable:
                continue
            rows.append((pid, name, cpu_pct, mem_mb, energy, d_read, d_write,
                         net_in, net_out))

        self.prev_cpu = cpu_now
        self.prev_energy = energy_now
        self.prev_disk = disk_now

        with self.lock:
            prev_ts = self.sweeps[-1]["ts"] if self.sweeps else None
            dt = round(min(300.0, wall - prev_ts), 2) if prev_ts else None
            self.sweeps.append({"ts": wall, "dt": dt, "rows": rows})
            self.sweeps_done += 1
            if self.started_ts is None:
                self.started_ts = wall
            cut = wall - PROC_RING_MAX_S
            while self.sweeps and (len(self.sweeps) > PROC_RING_MAX_SWEEPS
                                   or self.sweeps[0]["ts"] < cut):
                self.sweeps.pop(0)
        return rows

    # -- read side --------------------------------------------------------
    def since(self, cut):
        with self.lock:
            return [s for s in self.sweeps if s["ts"] >= cut]

    def span(self):
        with self.lock:
            if not self.sweeps:
                return {"sweeps": 0, "first_ts": None, "last_ts": None}
            return {"sweeps": len(self.sweeps),
                    "first_ts": self.sweeps[0]["ts"],
                    "last_ts": self.sweeps[-1]["ts"]}


PROC_RING = ProcRange()


def proc_ring_loop():
    """Fill PROC_RING every PROC_RING_SECS, forever.  Never on the tick path."""
    while True:
        try:
            PROC_RING.sweep()
        except Exception as e:      # a broken sweep must not kill the thread
            log_event("consumers", "process sweep failed: %r" % e)
        time.sleep(PROC_RING_SECS)


# Which row field each module ranks by, and how the number was made.
CONSUMER_MODULES = {
    "cpu": {"metric": "cpu_avg_pct", "unit": "%", "label": "CPU",
            "aggregation": ("mean and max of the per-sweep CPU % (100 % = one "
                            "core) across every sample in the window")},
    "memory": {"metric": "mem_peak_mb", "unit": "MB", "label": "Memory",
               "aggregation": ("peak resident size across the window's sweeps; "
                               "avg is the mean of the same sweeps")},
    "gpu": {"metric": "energy_wh", "unit": "Wh", "label": "GPU",
            "aggregation": ("per-process GPU time is not exposed by this SDK "
                            "(ri_gpu_time absent), so rows are ranked by "
                            "measured rusage energy -- the closest measured "
                            "proxy for GPU work")},
    "disk": {"metric": "disk_bps", "unit": "B/s", "label": "Disk",
             "aggregation": ("(ri_diskio_bytesread + ri_diskio_byteswritten) "
                             "deltas / the span those deltas cover")},
    "network": {"metric": "net_bps", "unit": "B/s", "label": "Network",
                "aggregation": ("nettop per-PID bytes_in/bytes_out deltas / "
                                "span; nettop is sampled every %g s"
                                % NET_SCAN_SECS)},
    "power": {"metric": "energy_wh", "unit": "Wh", "label": "Power",
              "aggregation": ("sum of ri_energy_nj deltas / 3.6e12 -- a measured "
                              "floor per process, never the machine total")},
    "all": {"metric": "energy_wh", "unit": "Wh", "label": "All",
            "aggregation": ("every metric the range store holds: CPU %, memory, "
                            "rusage energy, disk and network byte deltas")},
}


def _consumer_rows(window_s, now, group_by="process"):
    """Aggregate the range store into one row per process (or per name).

    Returns (rows, meta).  Energy for names the ring never saw is back-filled
    from the collector log's per-app watts (name-level, already used by the
    power page) so a fresh server still has real depth in short windows; the
    back-fill applies only where the ring has no energy for that name, so
    nothing is double counted.
    """
    cut = now - window_s
    sweeps = PROC_RING.since(cut)
    agg = {}
    for sweep in sweeps:
        ts = sweep["ts"]
        for (pid, name, cpu_pct, mem_mb, energy, d_read, d_write,
             net_in, net_out) in sweep["rows"]:
            key = (pid, name) if group_by == "process" else (None, name)
            a = agg.get(key)
            if a is None:
                a = agg[key] = {
                    "pid": key[0], "name": name, "pids": set(),
                    "cpu_sum": 0.0, "cpu_n": 0, "cpu_max": None,
                    "mem_sum": 0.0, "mem_n": 0, "mem_peak": None,
                    "energy_wh": 0.0, "energy_n": 0,
                    "disk_read": 0, "disk_write": 0, "disk_dt": 0.0,
                    "disk_n": 0,
                    "net_in": 0.0, "net_out": 0.0, "net_n": 0,
                    "first_ts": ts, "last_ts": ts,
                }
            a["pids"].add(pid)
            a["last_ts"] = ts
            if cpu_pct is not None:
                a["cpu_sum"] += cpu_pct
                a["cpu_n"] += 1
                a["cpu_max"] = cpu_pct if a["cpu_max"] is None else max(a["cpu_max"], cpu_pct)
            if mem_mb is not None:
                a["mem_sum"] += mem_mb
                a["mem_n"] += 1
                a["mem_peak"] = mem_mb if a["mem_peak"] is None else max(a["mem_peak"], mem_mb)
            if energy is not None:
                a["energy_wh"] += energy / 3.6e12
                a["energy_n"] += 1
            if d_read is not None or d_write is not None:
                a["disk_read"] += d_read or 0
                a["disk_write"] += d_write or 0
                a["disk_dt"] += float(sweep.get("dt") or 0.0)
                a["disk_n"] += 1
            if net_in is not None or net_out is not None:
                a["net_in"] += net_in or 0.0
                a["net_out"] += net_out or 0.0
                a["net_n"] += 1

    # NOTE: no collector-log back-fill here. The collector records only its
    # top-25 by energy and truncates process names (e.g. "Google Chrome Helper
    # (Ap"), so merging it into a by-process/per-app ranking produced duplicate
    # and misaligned rows. The range store is the single source; `coverage` says
    # exactly how much of the requested window it actually holds.

    rows = []
    for a in agg.values():
        span_s = a["disk_dt"] if a["disk_dt"] > 0 else None
        net_span = (a["net_n"] * PROC_RING_SECS) if a["net_n"] else None
        rows.append({
            "pid": a["pid"],
            "name": a["name"],
            "processes": len(a["pids"]),
            "samples": max(a["cpu_n"], a["mem_n"]),
            "cpu_avg_pct": round(a["cpu_sum"] / a["cpu_n"], 2) if a["cpu_n"] else None,
            "cpu_max_pct": round(a["cpu_max"], 2) if a["cpu_max"] is not None else None,
            "mem_avg_mb": round(a["mem_sum"] / a["mem_n"], 1) if a["mem_n"] else None,
            "mem_peak_mb": round(a["mem_peak"], 1) if a["mem_peak"] is not None else None,
            "energy_wh": round(a["energy_wh"], 6) if a["energy_n"] else None,
            "energy_samples": a["energy_n"],
            "energy_source": "rusage" if a["energy_n"] else None,
            "disk_read_bps": round(a["disk_read"] / span_s, 1) if span_s else None,
            "disk_write_bps": round(a["disk_write"] / span_s, 1) if span_s else None,
            "disk_bps": (round((a["disk_read"] + a["disk_write"]) / span_s, 1)
                         if span_s else None),
            "disk_samples": a["disk_n"],
            "net_in_bps": round(a["net_in"] / net_span, 1) if net_span else None,
            "net_out_bps": round(a["net_out"] / net_span, 1) if net_span else None,
            "net_bps": (round((a["net_in"] + a["net_out"]) / net_span, 1)
                        if net_span else None),
            # T17B: window TOTALS in MB, not just rates. Disk counters are
            # byte deltas, so MB is exact; nettop reports rates, so its MB is
            # the rate the row already divides by the span it covers.
            "disk_read_mb": (round(a["disk_read"] / 1e6, 3)
                             if a["disk_n"] else None),
            "disk_write_mb": (round(a["disk_write"] / 1e6, 3)
                              if a["disk_n"] else None),
            "net_mb": (round(((a["net_in"] + a["net_out"]) * PROC_RING_SECS) / 1e6, 3)
                       if a["net_n"] else None),
            "net_samples": a["net_n"],
            "gpu_pct": None,          # never attributable per process (see note)
            "first_ts": a["first_ts"],
            "last_ts": a["last_ts"],
        })

    covered = 0.0
    if len(sweeps) > 1:
        covered = max(0.0, sweeps[-1]["ts"] - sweeps[0]["ts"])
    meta = {
        "sweeps": len(sweeps),
        "cadence_s": PROC_RING_SECS,
        "retention_s": PROC_RING_MAX_S,
        "row_min_mb": PROC_ROW_MIN_MB,
        "covered_s": round(covered, 1),
        "coverage_pct": round(min(100.0, 100.0 * covered / window_s), 1) if window_s else 0.0,
        "store": PROC_RING.span(),
        "group_by": group_by,
    }
    if not sweeps:
        meta["reason"] = ("the per-process range store has no sweep inside this "
                          "window yet -- it fills every %g s from dashboard start"
                          % PROC_RING_SECS)
    return rows, meta


def consumers_payload(module, rng, offset=0, limit=None, group_by="process",
                      pid=None, name=None, want_series=False, now=None):
    """The /api/consumers payload: full ranked list for the window (T14C #3).

    `limit` is opt-in pagination -- offset/limit never truncate silently because
    `count` always carries the full row total and `returned` the page size.
    """
    now = time.time() if now is None else now
    window_s = RANGES[rng]
    spec = CONSUMER_MODULES[module]

    if want_series:
        return _consumer_series(module, window_s, now, pid=pid, name=name)

    rows, meta = _consumer_rows(window_s, now, group_by=group_by)
    metric = spec["metric"]

    def rank_value(r):
        v = r.get(metric)
        return -1e18 if v is None else v

    rows.sort(key=lambda r: (-rank_value(r), r.get("name") or ""))

    total = 0.0
    for r in rows:
        v = r.get(metric)
        if v is not None and v > 0:
            total += v
    for r in rows:
        v = r.get(metric)
        r["share_pct"] = (round(100.0 * v / total, 2)
                          if v is not None and total else None)
        r["metric"] = metric

    count = len(rows)
    start = max(0, int(offset or 0))
    page = rows[start:] if not limit else rows[start:start + int(limit)]

    return {
        "module": module,
        "metric": metric,
        "unit": spec["unit"],
        "label": spec["label"],
        "range": rng,
        "window_start": datetime.fromtimestamp(now - window_s,
                                               timezone.utc).isoformat(timespec="seconds"),
        "window_end": datetime.fromtimestamp(now, timezone.utc).isoformat(timespec="seconds"),
        "window_s": window_s,
        "count": count,
        "returned": len(page),
        "offset": start,
        "limit": int(limit) if limit else count,
        "aggregation": spec["aggregation"],
        "aggregation_detail": {
            "source": ("dashboard per-process range store (psutil + "
                       "proc_pid_rusage V6 + nettop)"),
            "cadence_s": PROC_RING_SECS,
            "group_by": group_by,
            "ranked_by": metric,
        },
        "unavailable": _CONSUMER_UNAVAILABLE,
        "coverage": meta,
        "rows": page,
    }


def _consumer_series(module, window_s, now, pid=None, name=None):
    """Per-process series for the drawer: the range store's own samples.

    `pid` selects one process. `name` selects EVERY pid carrying that name and
    sums them per sweep — that is what the by-name ranking does, so an app's
    drawer matches its row instead of silently showing one of its helpers.
    """
    cut = now - window_s
    spec = CONSUMER_MODULES[module]
    metric = spec["metric"]
    pts = []
    for sweep in PROC_RING.since(cut):
        dt = sweep.get("dt") or 0
        cpu = mem = energy = disk = net = None
        matched = 0
        for row in sweep["rows"]:
            (r_pid, r_name, cpu_pct, mem_mb, e_nj, d_read, d_write,
             net_in, net_out) = row
            if pid is not None:
                if r_pid != pid:
                    continue
            elif name is not None:
                if r_name != name:
                    continue
            else:
                break
            matched += 1
            if cpu_pct is not None:
                cpu = (cpu or 0.0) + cpu_pct
            if mem_mb is not None:
                mem = (mem or 0.0) + mem_mb
            if e_nj is not None:
                energy = (energy or 0) + e_nj
            if d_read is not None or d_write is not None:
                disk = (disk or 0) + (d_read or 0) + (d_write or 0)
            if net_in is not None or net_out is not None:
                net = (net or 0.0) + (net_in or 0.0) + (net_out or 0.0)
        if not matched:
            continue
        if metric == "cpu_avg_pct":
            v = round(cpu, 2) if cpu is not None else None
        elif metric == "mem_peak_mb":
            v = round(mem, 1) if mem is not None else None
        elif metric == "energy_wh":
            # nJ burned during the sweep -> average W over that sweep
            v = round(energy / 1e9 / dt, 3) if energy is not None and dt else None
        elif metric == "disk_bps":
            v = round(disk / dt, 1) if disk is not None and dt else None
        elif metric == "net_bps":
            v = round(net, 1) if net is not None else None
        else:
            v = None
        if v is not None:
            pts.append({"ts": sweep["ts"], "value": v})
    label = {"cpu_avg_pct": "CPU %", "mem_peak_mb": "RSS MB",
             "energy_wh": "energy W", "disk_bps": "disk B/s",
             "net_bps": "network B/s"}.get(metric, metric)
    return {
        "module": module,
        "metric": metric,
        "unit": spec["unit"],
        "series_label": label,
        "pid": pid,
        "name": name,
        "window_start": datetime.fromtimestamp(now - window_s,
                                               timezone.utc).isoformat(timespec="seconds"),
        "window_end": datetime.fromtimestamp(now, timezone.utc).isoformat(timespec="seconds"),
        "points": pts,
        "aggregation": spec["aggregation"],
        "unavailable": _CONSUMER_UNAVAILABLE,
    }


# ================================================= T17B · Apps (the app cut)
# The per-module consumer lists answer "who consumed CPU / disk / network"; the
# Apps view asks the same question across every metric at once and ranks by
# measured energy. Both read the SAME range store (`_consumer_rows`), so an app
# cannot show one number in Apps and a different one in a module drawer. The
# only thing Apps adds is (a) the time-bucketed sparkline, (b) byte totals in
# MB for the window, and (c) the peak moment of each metric.
APPS_SPARK_BUCKETS = 44          # 22 px sparkline: 44 buckets is more than the
                                 # pixels can resolve, and the payload stays small
APPS_EMPTY_TEXT = ("no sweep in this range yet — the store sweeps every 20 s")


def _app_spark_and_peaks(window_s, now, names, buckets=APPS_SPARK_BUCKETS):
    """Per-app sparkline (W) + the peak moment of CPU / memory / energy.

    One pass over the range store. A bucket where the app was not measured is
    null — the sparkline breaks there instead of drawing a zero. `bucket_s` is
    derived from the window so 1m and 1y both come out at <= `buckets` points.
    """
    cut = now - window_s
    sweeps = PROC_RING.since(cut)
    bucket_s = max(PROC_RING_SECS, int(math.ceil(float(window_s) / max(1, buckets))))
    start = int((now - window_s) // bucket_s) * bucket_s
    n = max(1, int(math.ceil((now - start) / float(bucket_s))))
    wanted = set(names or ())
    acc = {}          # name -> bucket index -> [nj_sum, dt_sum]
    peaks = {}        # name -> {"cpu": (v, ts), "mem": (v, ts), "w": (v, ts)}
    for sweep in sweeps:
        ts = sweep["ts"]
        dt = sweep.get("dt") or 0.0
        bi = int((ts - start) // bucket_s)
        if bi < 0 or bi >= n:
            continue
        for (pid, name, cpu_pct, mem_mb, e_nj, _d_read, _d_write,
             _net_in, _net_out) in sweep["rows"]:
            if name not in wanted:
                continue
            p = peaks.get(name)
            if p is None:
                p = peaks[name] = {"cpu": (None, None), "mem": (None, None),
                                   "w": (None, None)}
            if cpu_pct is not None and (p["cpu"][0] is None or cpu_pct > p["cpu"][0]):
                p["cpu"] = (cpu_pct, ts)
            if mem_mb is not None and (p["mem"][0] is None or mem_mb > p["mem"][0]):
                p["mem"] = (mem_mb, ts)
            if e_nj is not None and dt:
                w = e_nj / 1e9 / dt
                if p["w"][0] is None or w > p["w"][0]:
                    p["w"] = (round(w, 4), ts)
                slot = acc.setdefault(name, {}).setdefault(bi, [0.0, 0.0])
                slot[0] += e_nj
                slot[1] += dt
    sparks = {}
    for name, row in acc.items():
        vals = [None] * n
        for bi, (nj, dt) in row.items():
            if dt:
                vals[bi] = round(nj / 1e9 / dt, 4)
        sparks[name] = vals
    return {
        "metric": "energy_w",
        "unit": "W",
        "bucket_s": bucket_s,
        "start_ts": start,
        "buckets": n,
        "sparks": sparks,
        "peaks": peaks,
    }


def apps_payload(rng, now=None):
    """Build /api/apps — one row per application over the selected range.

    Not a live feed: the range store sweeps every 20 s, so this is a ranged
    read and the UI refreshes it only when the range (or the view) changes.
    """
    now = time.time() if now is None else now
    window_s = RANGES.get(rng)
    if window_s is None:
        return {"error": "unknown range", "range": rng,
                "ranges": list(RANGES.keys())}
    rows, meta = _consumer_rows(window_s, now, group_by="name")
    spark = _app_spark_and_peaks(window_s, now, [r["name"] for r in rows])

    energy_total = 0.0
    for r in rows:
        if isinstance(r.get("energy_wh"), (int, float)) and r["energy_wh"] > 0:
            energy_total += r["energy_wh"]
    totals = {"energy_wh": round(energy_total, 6) if energy_total else None,
              "disk_read_mb": 0.0, "disk_write_mb": 0.0, "net_mb": 0.0,
              "disk_rows": 0, "net_rows": 0, "energy_rows": 0}
    for r in rows:
        if isinstance(r.get("energy_wh"), (int, float)):
            totals["energy_rows"] += 1
        if isinstance(r.get("disk_read_mb"), (int, float)):
            totals["disk_read_mb"] += r["disk_read_mb"]
            totals["disk_write_mb"] += r.get("disk_write_mb") or 0.0
            totals["disk_rows"] += 1
        if isinstance(r.get("net_mb"), (int, float)):
            totals["net_mb"] += r["net_mb"]
            totals["net_rows"] += 1
    for k in ("disk_read_mb", "disk_write_mb", "net_mb"):
        totals[k] = round(totals[k], 3) if totals.get(
            "disk_rows" if k != "net_mb" else "net_rows") else None
    if not totals.get("energy_rows"):
        totals["energy_wh"] = None

    out_rows = []
    for r in rows:
        row = dict(r)
        # share of total is the ONLY share shown (no "gap to leader")
        row["share_pct"] = (round(100.0 * row["energy_wh"] / energy_total, 2)
                            if energy_total and isinstance(row.get("energy_wh"), (int, float))
                            else None)
        row["spark"] = spark["sparks"].get(row["name"])
        pk = spark["peaks"].get(row["name"]) or {}
        row["peak_cpu_ts"] = (pk.get("cpu") or (None, None))[1]
        row["peak_mem_ts"] = (pk.get("mem") or (None, None))[1]
        row["peak_w"] = (pk.get("w") or (None, None))[0]
        row["peak_w_ts"] = (pk.get("w") or (None, None))[1]
        out_rows.append(row)

    # energy desc: the Apps question is "where did the energy go"
    out_rows.sort(key=lambda r: (-(r.get("energy_wh") or -1e18),
                                 -(r.get("cpu_avg_pct") or 0.0),
                                 r.get("name") or ""))
    return {
        "module": "apps",
        "range": rng,
        "window_s": window_s,
        "window_start": datetime.fromtimestamp(now - window_s,
                                               timezone.utc).isoformat(timespec="seconds"),
        "window_end": datetime.fromtimestamp(now, timezone.utc).isoformat(timespec="seconds"),
        "count": len(out_rows),
        "rows": out_rows,
        "totals": totals,
        "share_basis": ("share of measured per-app energy (sum of ri_energy_nj "
                        "deltas) — a measured floor, not the machine total"),
        "spark": {"metric": spark["metric"], "unit": spark["unit"],
                  "bucket_s": spark["bucket_s"], "start_ts": spark["start_ts"],
                  "buckets": spark["buckets"]},
        "coverage": meta,
        "cadence_s": PROC_RING_SECS,
        "empty_text": APPS_EMPTY_TEXT,
        "basis": ("every metric the per-process range store holds, grouped by "
                  "application name: mean and peak CPU %, peak RSS, rusage "
                  "energy, disk byte deltas and nettop byte deltas"),
        "cross_module": ("the per-module consumer lists are the module cut; this "
                         "view is the application cut of the same range store"),
    }


APP_SERIES_FIELDS = (
    ("cpu", "CPU %", "%"),
    ("mem_mb", "memory MB", "MB"),
    ("energy_w", "energy W", "W"),
    ("disk_bps", "disk B/s", "B/s"),
    ("net_bps", "network B/s", "B/s"),
)


def app_series_payload(name, rng, now=None):
    """One app's own series across the range: CPU, memory, energy, disk, net.

    Every pid carrying `name` is summed per sweep, which is exactly what the
    by-name Apps row does, so the drawer matches its row. A field the sweep did
    not measure stays null (a gap), never 0.
    """
    now = time.time() if now is None else now
    window_s = RANGES.get(rng)
    if window_s is None:
        return {"error": "unknown range", "range": rng}
    if not name:
        return {"error": "name required"}
    cut = now - window_s
    sweeps = PROC_RING.since(cut)
    pts = []
    peaks = {k: (None, None) for k, _l, _u in APP_SERIES_FIELDS}
    for sweep in sweeps:
        ts = sweep["ts"]
        dt = sweep.get("dt") or 0.0
        cpu = mem = energy = disk = net = None
        matched = 0
        for row in sweep["rows"]:
            (r_pid, r_name, cpu_pct, mem_mb, e_nj, d_read, d_write,
             net_in, net_out) = row
            if r_name != name:
                continue
            matched += 1
            if cpu_pct is not None:
                cpu = (cpu or 0.0) + cpu_pct
            if mem_mb is not None:
                mem = (mem or 0.0) + mem_mb
            if e_nj is not None:
                energy = (energy or 0) + e_nj
            if d_read is not None or d_write is not None:
                disk = (disk or 0) + (d_read or 0) + (d_write or 0)
            if net_in is not None or net_out is not None:
                net = (net or 0.0) + (net_in or 0.0) + (net_out or 0.0)
        if not matched:
            continue
        point = {
            "ts": ts,
            "cpu": round(cpu, 2) if cpu is not None else None,
            "mem_mb": round(mem, 1) if mem is not None else None,
            "energy_w": round(energy / 1e9 / dt, 4) if energy is not None and dt else None,
            "disk_bps": round(disk / dt, 1) if disk is not None and dt else None,
            "net_bps": round(net, 1) if net is not None else None,
        }
        for k in peaks:
            v = point.get(k)
            if v is not None and (peaks[k][0] is None or v > peaks[k][0]):
                peaks[k] = (v, ts)
        pts.append(point)
    covered = (sweeps[-1]["ts"] - sweeps[0]["ts"]) if len(sweeps) > 1 else 0.0
    return {
        "name": name,
        "range": rng,
        "window_s": window_s,
        "points": pts,
        "series": [{"key": k, "label": label, "unit": unit}
                   for k, label, unit in APP_SERIES_FIELDS],
        "peaks": {k: {"value": v[0], "ts": v[1]} for k, v in peaks.items()},
        "coverage": {
            "sweeps": len(sweeps),
            "cadence_s": PROC_RING_SECS,
            "covered_s": round(covered, 1),
            "coverage_pct": round(min(100.0, 100.0 * covered / window_s), 1)
                            if window_s else 0.0,
            "reason": None if sweeps else APPS_EMPTY_TEXT,
        },
        "basis": ("per-sweep values for every pid carrying this name, summed; "
                  "energy W = ri_energy_nj delta / 1e9 / sweep seconds"),
        "empty_text": APPS_EMPTY_TEXT,
    }


# ============================================ T17B · battery facts + outlook
# The pack's static facts (design capacity, max capacity) do not change from
# second to second, so they are read once per day instead of on every poll:
# one ioreg spawn per 24 h. Health % and cycles come from the same read as the
# rest of the battery block (BATTERY_TTL).
BATTERY_CAP_TTL_S = 86400.0
_BATT_CAP = {"mono": 0.0, "data": None}


def battery_capacity_static(force=False):
    """Design / max capacity in mAh from ioreg, cached for 24 h.

    `MaxCapacity` is a *percentage* on some packs and a mAh figure on others —
    it is published under `max_capacity_pct` or `max_capacity_mah` depending on
    what the value can actually be, never guessed into one column.
    """
    now = time.monotonic()
    if (not force and _BATT_CAP["data"] is not None
            and now - _BATT_CAP["mono"] < BATTERY_CAP_TTL_S):
        return dict(_BATT_CAP["data"])
    ioreg = _ioreg_battery()
    data = {
        "available": False,
        "design_mah": None,
        "nominal_mah": None,
        "max_capacity_mah": None,
        "max_capacity_pct": None,
        "health_pct": None,
        "health_basis": None,
        "source": "ioreg AppleSmartBattery",
        "cache_ttl_s": BATTERY_CAP_TTL_S,
        "read_ts": int(time.time()),
        "reason": None,
    }
    if ioreg is None:
        data["reason"] = "ioreg AppleSmartBattery returned nothing"
        _BATT_CAP.update({"mono": now, "data": dict(data)})
        return data
    design = ioreg.get("DesignCapacity")
    nominal = ioreg.get("NominalChargeCapacity")
    raw_max = ioreg.get("AppleRawMaxCapacity")
    max_cap = ioreg.get("MaxCapacity")
    if isinstance(design, (int, float)) and design > 0:
        data["design_mah"] = int(design)
    if isinstance(nominal, (int, float)) and nominal > 0:
        data["nominal_mah"] = int(nominal)
    if isinstance(raw_max, (int, float)) and raw_max > 0:
        data["max_capacity_mah"] = int(raw_max)
    # MaxCapacity <= 100 next to a design capacity in the thousands is a
    # percentage, not a mAh reading.
    if isinstance(max_cap, (int, float)) and max_cap > 0:
        if data["design_mah"] and max_cap <= 100 < data["design_mah"] // 10:
            data["max_capacity_pct"] = round(float(max_cap), 1)
        elif max_cap > 1000:
            data["max_capacity_mah"] = data["max_capacity_mah"] or int(max_cap)
    if data["design_mah"] and data["nominal_mah"]:
        data["available"] = True
        data["health_pct"] = round(100.0 * data["nominal_mah"] / data["design_mah"], 1)
        data["health_basis"] = ("NominalChargeCapacity ÷ DesignCapacity (both mAh, "
                                "read from the pack)")
    else:
        data["reason"] = ("the pack does not expose both DesignCapacity and "
                          "NominalChargeCapacity, so health % cannot be computed")
    _BATT_CAP.update({"mono": now, "data": dict(data)})
    return data


# Our OWN battery history. The collector log carries no battery fields and the
# 1 s history rows carry no cycle count, so the degradation outlook needs a
# record we keep ourselves: one row every 30 min (~48/day, ~25 KB/day).
BATTERY_HISTORY_PATH = os.environ.get(
    "SYSWATCH_BATTERY_HISTORY",
    os.path.join(DATA_DIR, "battery-history.jsonl"))
BATTERY_HISTORY_SECS = 1800.0
BATTERY_OUTLOOK_MIN_DAYS = 14.0
BATTERY_OUTLOOK_CAVEAT = ("estimate; assumes current usage pattern, not a "
                          "prediction.")


BATTERY_HISTORY_CACHE_S = 60.0
_BATT_HIST = {"mono": 0.0, "key": None, "rows": []}


def battery_history_rows():
    """Every row we have recorded, oldest first. Never raises.

    Cached for 60 s keyed on (mtime, size): /api/battery is polled every 5 s by
    the Analytics view, and re-parsing the file each time buys nothing — a row
    lands once per 30 min.
    """
    now = time.monotonic()
    try:
        st = os.stat(BATTERY_HISTORY_PATH)
        key = (st.st_mtime, st.st_size)
    except OSError:
        return []
    if (_BATT_HIST["key"] == key and now - _BATT_HIST["mono"] < BATTERY_HISTORY_CACHE_S):
        return list(_BATT_HIST["rows"])
    rows = []
    try:
        with open(BATTERY_HISTORY_PATH, "r", encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    r = json.loads(line)
                except ValueError:
                    continue
                if isinstance(r, dict) and isinstance(r.get("ts"), (int, float)):
                    rows.append(r)
    except OSError:
        return []
    rows.sort(key=lambda r: r["ts"])
    _BATT_HIST.update({"mono": now, "key": key, "rows": rows})
    return list(rows)


def record_battery_history(now=None, force=False):
    """Append one row (ts, cycles, health %, capacities) at most every 30 min."""
    now = time.time() if now is None else now
    rows = battery_history_rows()
    if rows and not force and now - rows[-1]["ts"] < BATTERY_HISTORY_SECS:
        return {"written": False, "reason": "last row is recent",
                "age_s": round(now - rows[-1]["ts"], 1)}
    batt = _read_battery_state()
    cap = battery_capacity_static()
    row = {
        "ts": int(now),
        "cycles": batt.get("cycles"),
        "health_pct": batt.get("health_pct"),
        "pct": batt.get("pct"),
        "charging": batt.get("charging"),
        "design_mah": cap.get("design_mah"),
        "nominal_mah": cap.get("nominal_mah"),
        "external_connected": batt.get("external_connected"),
    }
    if row["cycles"] is None and row["health_pct"] is None:
        return {"written": False, "reason": "no cycle count or health to record"}
    try:
        os.makedirs(os.path.dirname(BATTERY_HISTORY_PATH) or ".", exist_ok=True)
        with open(BATTERY_HISTORY_PATH, "a", encoding="utf-8") as f:
            f.write(json.dumps(row, separators=(",", ":")) + "\n")
    except OSError as e:
        return {"written": False, "reason": "write failed: %r" % (e,)}
    return {"written": True, "row": row}


def battery_history_loop():
    """Keep the battery history warm: one append per 30 min, forever."""
    while True:
        try:
            record_battery_history()
        except Exception as e:      # never let the recorder kill the thread
            log_event("battery", "battery history failed: %r" % e)
        time.sleep(600.0)


def battery_degradation(now=None):
    """Cycle rate + a linear projection to 80 % health, from OUR OWN history.

    Honest by construction: with under two weeks of recorded history the answer
    is "insufficient data", and with history but no measurable health decline
    it says that instead of inventing a slope. When it does project, the caveat
    travels with the number.
    """
    now = time.time() if now is None else now
    rows = [r for r in battery_history_rows()
            if isinstance(r.get("cycles"), int)
            and isinstance(r.get("health_pct"), (int, float))]
    out = {
        "available": False,
        "reason": None,
        "caveat": BATTERY_OUTLOOK_CAVEAT,
        "min_days": BATTERY_OUTLOOK_MIN_DAYS,
        "rows": len(rows),
        "path": os.path.basename(BATTERY_HISTORY_PATH),
        "cadence_s": BATTERY_HISTORY_SECS,
        "span_days": None,
        "cycles_per_day": None,
        "health_pct_per_month": None,
        "health_pct": rows[-1]["health_pct"] if rows else None,
        "cycles": rows[-1]["cycles"] if rows else None,
        "days_to_80": None,
        "projected_80_ts": None,
        "projected_80_date": None,
        "method": ("linear fit over our own recorded (ts, cycles, health %); "
                   "80 % is the threshold, not a failure date"),
    }
    if len(rows) < 2:
        out["reason"] = ("insufficient data — %d recorded sample%s of battery "
                         "health (needs 2 spanning %d days)"
                         % (len(rows), "" if len(rows) == 1 else "s",
                            int(BATTERY_OUTLOOK_MIN_DAYS)))
        return out
    span_days = (rows[-1]["ts"] - rows[0]["ts"]) / 86400.0
    out["span_days"] = round(span_days, 2)
    if span_days <= 0:
        out["reason"] = "insufficient data — the recorded samples share one timestamp"
        return out
    cycles_per_day = (rows[-1]["cycles"] - rows[0]["cycles"]) / span_days
    out["cycles_per_day"] = round(cycles_per_day, 3)
    if span_days < BATTERY_OUTLOOK_MIN_DAYS:
        out["reason"] = ("insufficient data — %.1f days of battery history "
                         "(needs %.0f)" % (span_days, BATTERY_OUTLOOK_MIN_DAYS))
        return out
    drop_per_day = (rows[0]["health_pct"] - rows[-1]["health_pct"]) / span_days
    out["health_pct_per_month"] = round(drop_per_day * 30.0, 3)
    health_now = rows[-1]["health_pct"]
    if health_now <= 80:
        out["reason"] = ("health is already at or below 80 %% (%.1f %%) — no "
                         "projection to make" % health_now)
        return out
    if drop_per_day <= 0.0005:
        out["reason"] = ("no measurable health decline over %.1f days — a "
                         "projection would be invented, so none is shown"
                         % span_days)
        return out
    days_to_80 = (health_now - 80.0) / drop_per_day
    out["available"] = True
    out["days_to_80"] = round(days_to_80, 1)
    out["projected_80_ts"] = int(rows[-1]["ts"] + days_to_80 * 86400)
    out["projected_80_date"] = datetime.fromtimestamp(
        out["projected_80_ts"]).strftime("%Y-%m-%d")
    out["reason"] = None
    return out


def battery_payload(now=None):
    """Build /api/battery — the numbers the Power & battery section needs.

    Everything here is either measured now (V × A, charge %, cycles), a static
    pack fact read from ioreg once a day, or an explicitly-labelled estimate
    (life at current draw, the 80 % projection). Nothing is a forecast dressed
    up as a measurement.
    """
    now = time.time() if now is None else now
    batt = _read_battery_state()
    cap = battery_capacity_static()
    deg = battery_degradation(now)
    draw = batt.get("battery_w")
    capacity_wh = batt.get("nominal_wh")
    discharging = batt.get("external_connected") is False
    life_h = None
    life_note = None
    if (isinstance(draw, (int, float)) and draw > 0.05
            and isinstance(capacity_wh, (int, float)) and capacity_wh > 0):
        # Wh ÷ W = h. Always labelled *at current draw* (that is the whole
        # point: it is not a prediction). While on AC the pack is not the
        # energy source, so the note says that instead of pretending.
        life_h = round(capacity_wh / draw, 2)
        life_note = ("at current draw: pack energy (%.2f Wh) ÷ draw right now "
                     "(%.2f W)" % (capacity_wh, draw))
        if not discharging:
            life_note += (" — the pack is on AC and not draining, so read this "
                          "as capacity ÷ draw, not as remaining time")
    else:
        life_note = ("no positive draw or no pack energy reading, so "
                     "capacity ÷ draw has nothing to divide")
    return {
        "ts": int(now),
        "pct": batt.get("pct"),
        "charging": batt.get("charging"),
        "external_connected": batt.get("external_connected"),
        "draw_w": draw,
        "adapter_w": batt.get("adapter_w"),
        "adapter_rated_w": batt.get("adapter_rated_w"),
        "voltage_v": batt.get("voltage_v"),
        "amperage_ma": batt.get("amperage_ma"),
        "temperature_c": batt.get("temp_c"),
        "time_to_empty_s": (batt.get("time_remaining_s")
                            if (batt.get("charging") is not True
                                and isinstance(batt.get("time_remaining_s"), int)
                                and batt.get("time_remaining_s") > 0) else None),
        "charge_to_full_s": (batt.get("time_remaining_s")
                             if (batt.get("charging") is True
                                 and isinstance(batt.get("time_remaining_s"), int)
                                 and batt.get("time_remaining_s") > 0) else None),
        "capacity": cap,
        "health_pct": (cap.get("health_pct") if cap.get("health_pct") is not None
                       else batt.get("health_pct")),
        "condition": batt.get("condition"),
        "cycles": batt.get("cycles"),
        "pack_wh": capacity_wh,
        "life_at_current_draw_h": life_h,
        "life_note": life_note,
        "degradation": deg,
        "tariff": {
            "configured": False,
            "cost": None,
            "detail": ("no electricity tariff configured — cost stays hidden "
                       "until a rate is entered in ⚙ settings"),
        },
        "sources": {
            "draw_w": "AppleSmartBattery Voltage × Amperage via ioreg (unprivileged)",
            "capacity": "ioreg AppleSmartBattery DesignCapacity / NominalChargeCapacity, read once per 24 h",
            "cycles": "ioreg AppleSmartBattery CycleCount",
            "time": "ioreg AvgTimeToEmpty / AvgTimeToFull (Apple's own estimate)",
            "degradation": ("our own battery history (%s), one row per 30 min"
                            % os.path.basename(BATTERY_HISTORY_PATH)),
        },
    }


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


def _pid_exists(pid):
    """Cross-platform process-existence check with stdlib fallback.

    Uses psutil when available.  When psutil is missing (e.g. bare test
    runners) we fall back to os.kill(0) on Unix and OpenProcess on Windows.

    T13: a ZOMBIE counts as gone. psutil.pid_exists() returns True for
    <defunct> processes, so a killed dashboard left the sampler *and* the power
    ring dead on the next start (the lock file's PID still "existed") while the
    HTTP server happily served an empty dashboard.
    """
    if psutil is not None:
        try:
            if not psutil.pid_exists(pid):
                return False
            try:
                return psutil.Process(pid).status() != psutil.STATUS_ZOMBIE
            except (psutil.NoSuchProcess, psutil.AccessDenied):
                return False
        except Exception:
            return psutil.pid_exists(pid)
    if sys.platform == "win32":
        try:
            import ctypes
            kernel = ctypes.windll.kernel32
            handle = kernel.OpenProcess(0x0400, False, pid)  # PROCESS_QUERY_INFORMATION
            if handle:
                kernel.CloseHandle(handle)
                return True
            return False
        except Exception:
            return False
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except OSError as e:
        # EPERM means a process exists but we can't signal it.
        return e.errno == 1
    except OverflowError:
        return False
    return True


def _singleton_acquire():
    """B11: PID-lock guard. If dashboard/.sampler.pid exists and that PID is
    still alive on this machine, exit immediately. Otherwise claim the slot
    with our PID. Cheap, atomic enough on Windows for our purposes — at worst
    a stale entry from a crashed process is harmless (we overwrite)."""
    # Port 8123 is the canonical dashboard port; keep the legacy lock file
    # name so existing tests (B11) continue to work.  Other ports get a
    # port-scoped lock so fixture/dev servers can run alongside production.
    lock_name = ".sampler.pid" if PORT == 8123 else ".sampler.%d.pid" % PORT
    lock = os.path.join(BASE_DIR, lock_name)
    my_pid = os.getpid()
    try:
        if os.path.exists(lock):
            try:
                with open(lock, "r") as f:
                    other = int(f.read().strip() or "0")
            except (OSError, ValueError):
                other = 0
            if other and other != my_pid:
                if _pid_exists(other):
                    log_event("sampler", "another instance pid=%d alive; exiting" % other)
                    sys.exit(0)
        with open(lock, "w") as f:
            f.write(str(my_pid))
    except OSError:
        pass  # never block startup on lock I/O errors


def sampler_loop(sampler):
    """S12.2: 1s tick (down from 5s). Sample psutil, append a compact JSONL line
    to the daily file, log events, feed alerter, gzip files older than 7 days.

    At 1s cadence this writes ~86,400 lines/day (~1.5 MB). The file is opened
    once and held across ticks to avoid fsync per line. Appends are atomic
    enough for our purposes (the OS may reorder within a single fd, but we
    only ever read whole files via tail_newest())."""
    _singleton_acquire()  # B11: refuse to start if another instance is alive
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
        # T13: the collector carries disk CAPACITY (used_pct/free_gb) and a
        # process count, but no read/write throughput and no GPU field. Keep
        # only what is really measured; the API reports the rest as
        # unavailable with a reason instead of synthesising a zero.
        disk_pct = None
        disk_free_gb = None
        disks = s.get("disks")
        if isinstance(disks, list):
            for d in disks:
                if not isinstance(d, dict):
                    continue
                if d.get("mount") != "/" and disk_pct is not None:
                    continue
                if isinstance(d.get("used_pct"), (int, float)):
                    disk_pct = round(float(d["used_pct"]), 2)
                    disk_free_gb = d.get("free_gb")
                    if d.get("mount") == "/":
                        break
        uptime_s = s.get("uptime_secs")
        return {"ts": s.get("timestamp"), "e": _ts_epoch(s.get("timestamp")),
                "cpu": cpu, "ram": ram,
                "down_bps": down, "up_bps": up,
                "temps": [t for t in temps if isinstance(t, (int, float))],
                "disk_pct": disk_pct,
                "disk_free_gb": (round(float(disk_free_gb), 2)
                                 if isinstance(disk_free_gb, (int, float)) else None),
                "proc_count": (s.get("process_count")
                               if isinstance(s.get("process_count"), int) else None),
                "uptime_s": (uptime_s if isinstance(uptime_s, (int, float)) else None)}

    def index_all(self):
        """Full pass over existing files (background thread; may take a while)."""
        try:
            self._index_all_run()
        finally:
            # always release the tail loop, even on failure or missing dir
            self.index_done.set()

    def _index_dashboard_history(self):
        """T13: index the dashboard's own daily JSONL into the ring.

        Without this, the ring only holds what the current process appended
        live, so /api/graph?range=1d is empty seconds after a restart even
        though HISTORY_DIR holds days of 1 s samples. These lines are already
        compact ({ts,cpu,ram,down_bps,up_bps,temps,...}) — no conversion.
        """
        try:
            names = sorted(n for n in os.listdir(HISTORY_DIR) if n.endswith(".jsonl"))
        except OSError:
            return []
        pts = []
        for name in names[-9:]:
            path = os.path.join(HISTORY_DIR, name)
            try:
                with open(path, "r", encoding="utf-8", errors="replace") as f:
                    for line in f:
                        line = line.strip()
                        if not line:
                            continue
                        try:
                            s = json.loads(line)
                        except ValueError:
                            continue
                        ts = s.get("ts")
                        if _ts_epoch(ts) is None:
                            continue
                        temps = s.get("temps")
                        if not isinstance(temps, list):
                            temps = []
                        pts.append({
                            "ts": ts,
                            "e": _ts_epoch(ts),
                            "cpu": s.get("cpu"), "ram": s.get("ram"),
                            "down_bps": s.get("down_bps"), "up_bps": s.get("up_bps"),
                            "temps": [t for t in temps if isinstance(t, (int, float))],
                            "proc_count": (s.get("proc_count")
                                           if isinstance(s.get("proc_count"), int) else None),
                            "load1": s.get("load1"),
                        })
            except OSError:
                continue
        return pts

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
        # T13: the dashboard's own daily 1 s samples. Indexed after the
        # collector files so the in-memory cap trims the oldest first.
        try:
            own = self._index_dashboard_history()
            if own:
                pts.extend(own)
                log_event("history", "indexed dashboard history: %d points" % len(own))
        except Exception as e:
            log_event("history", "dashboard history read error: %s" % e)
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
                # B11: bound reads. Collector JSONLs grow to 75-120MB;
                # reading the whole tail every 15s leaked RSS. Cap to the
                # last 5 MB — ~10 minutes of new collector data at typical
                # 5s/sample cadence. Advance offset to read_from + bytes
                # actually read (clamped to size) so we don't re-emit the
                # same lines.
                read_from = off
                if size - read_from > 5_000_000:
                    read_from = size - 5_000_000
                f.seek(read_from)
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
            # B11: advance offset to end of what we consumed (not "off + len"
            # which would re-read on next tick when we capped read_from).
            self.offsets[name] = min(size, read_from + len(raw))
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
        load = snap.get("load") or {}
        p = {"ts": snap["ts"],
             "e": _ts_epoch(snap["ts"]),
             "cpu": snap["cpu"]["total_pct"],
             "ram": snap["ram"]["pct"],
             "down_bps": snap["net"]["down_bps"],
             "up_bps": snap["net"]["up_bps"],
             "temps": [t["c"] for t in (snap.get("temps_c") or [])],
             # T13: the live ring has no disk/GPU source (psutil's sampler does
             # not read either), so these stay absent here; the collector index
             # supplies disk capacity when the Rust collector is running.
             "proc_count": snap.get("proc_count"),
             "load1": load.get("one") if isinstance(load, dict) else None}
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


# ------------------------------------------- T4b: agent event stream (.hermes)
# tools/driver.py appends one JSON object per line to .hermes/run/events.jsonl
# (schema: briefs/D1.md — {"ts","job","kind","model","attempt","exit",
# "duration_s","text"}). Two read-only endpoints expose it to the cockpit:
# a bounded tail read (/api/agent-events) and an SSE tail (/api/agent-stream).
# The file may not exist yet (briefs/T4.md context) — both endpoints treat
# "missing" as empty, never as an error. Path resolves from THIS FILE (repo
# root), not the process cwd, so launching from anywhere finds the same log.
AGENT_EVENTS_PATH = os.environ.get(
    "SYSWATCH_AGENT_EVENTS",
    os.path.join(os.path.dirname(BASE_DIR), ".hermes", "run", "events.jsonl"))
AGENT_EVENTS_MAX_LIMIT = 500   # same cap as the /api/logs ring
AGENT_TAIL_BYTES = 2_000_000   # tail window; a 100MB log is never fully loaded
AGENT_STREAM_POLL_S = 0.5      # SSE poll cadence (new lines appear in <=0.5s)


def agent_events_tail(limit):
    """Last `limit` lines of the agent event log, oldest-first. [] on any failure.

    T4b: seek from EOF and read at most AGENT_TAIL_BYTES — never the whole
    file. Missing (not created yet), unreadable, or empty -> [] and never an
    error, per the brief."""
    start = 0
    try:
        with open(AGENT_EVENTS_PATH, "rb") as f:
            f.seek(0, os.SEEK_END)
            size = f.tell()
            if size == 0:
                return []
            start = max(0, size - AGENT_TAIL_BYTES)
            f.seek(start)
            raw = f.read()
    except OSError:
        return []
    lines = raw.decode("utf-8", errors="replace").splitlines()
    if start > 0 and lines:
        lines.pop(0)  # mid-file seek: first line is likely a partial line
    lines = [ln for ln in lines if ln.strip()]
    return lines[-limit:]


def _sse_peer_closed(conn):
    """True once the SSE client has hung up (FIN received, or socket error).

    select + MSG_PEEK peeks without consuming bytes and without writing
    anything, so an idle stream notices a disconnected client on the next
    poll instead of leaking the handler thread until server shutdown."""
    try:
        readable, _, _ = select.select([conn], [], [], 0)
        if not readable:
            return False
        return conn.recv(1, socket.MSG_PEEK) == b""
    except (OSError, ValueError):
        return True  # socket already gone -> treat as closed


def _range_from_secs(secs):
    """Pick the smallest named range that covers secs (fallback 1h).

    T11 wrote this INSIDE the Handler class body by accident, which swallowed
    api_logs() and the T4b agent-stream handlers into its (dead) body — every
    /api/logs call returned 500. Restored to module level by T13.
    """
    for name in ("1m", "5m", "15m", "1h", "6h", "1d", "7d", "30d", "1y"):
        if secs <= RANGES[name]:
            return name
    return "1h"


# ------------------------------------------------------------------ HTTP layer
# T5: enable fixture replay when requested. Do this late so the loader can
# reference BASE_DIR / BOOT_TS without import-order issues.
if FIXTURE_PATH:
    try:
        FIXTURE = FixtureLoader(FIXTURE_PATH)
    except Exception as e:
        log_event("fixture", "failed to load %s: %r" % (FIXTURE_PATH, e))
        FIXTURE = None

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
        if route in ("/", "/analytics"):
            return self._serve_file("index.html", "text/html")
        if route.startswith("/static/"):
            return self._serve_file(route[len("/static/"):], None)
        try:
            if route == "/api/health":
                return self.api_health()
            if route == "/api/current":
                return self.api_current()
            if route == "/api/history":
                return self.api_history(parse_qs(url.query))
            if route == "/api/graph":
                return self.api_graph(parse_qs(url.query))
            if route == "/api/power":
                return self._json(power_state())
            if route == "/api/power/history":
                return self.api_power_history(parse_qs(url.query))
            if route == "/api/analytics":
                return self.api_analytics(parse_qs(url.query))
            if route == "/api/consumers":
                return self.api_consumers(parse_qs(url.query))
            if route == "/api/apps":
                return self.api_apps(parse_qs(url.query))
            if route == "/api/apps/series":
                return self.api_app_series(parse_qs(url.query))
            if route == "/api/battery":
                return self._json(battery_payload())
            if route == "/api/sensors":
                return self._json(sensors_payload())
            if route == "/api/logs":
                return self.api_logs(parse_qs(url.query))
            if route == "/api/alerts":
                return self._json(alerts.state())
            if route == "/api/agent-events":
                return self.api_agent_events(parse_qs(url.query))
            if route == "/api/agent-stream":
                return self.api_agent_stream()
            if route == "/api/permissions":
                return self._json(permissions_state(
                    probe_ax=(parse_qs(url.query).get("probe") or ["1"])[0]
                    .lower() not in ("0", "false", "no")))
        except Exception as e:
            log_event("http", "%s failed: %r" % (route, e))
            import traceback
            traceback.print_exc()
            return self._json({"error": repr(e)}, 500)
        self._json({"error": "not found"}, 404)

    def do_POST(self):
        """The ONLY mutation surface in this product (T15).

        Two routes, both explicit user actions: grant and revoke.  Nothing here
        runs on page load or in a poll, and no credential is ever captured —
        macOS' own admin dialog hands it to launchctl, not to us.

        CSRF guard: a same-origin fetch from app.js sets X-Syswatch; a random
        web page cannot (it would need a CORS preflight this server never
        approves). Without it, any local page could spam admin dialogs.
        """
        if self.headers.get("X-Syswatch") != "grant":
            return self._json(
                {"error": "missing X-Syswatch header",
                 "detail": ("this endpoint only accepts a same-origin POST from "
                            "the dashboard UI; it is the one route that can open a "
                            "macOS admin prompt")}, 403)
        url = urlparse(self.path)
        qs = parse_qs(url.query)
        pid = (qs.get("id") or ["package_power"])[0]
        try:
            if url.path == "/api/permissions/grant":
                out = grant_permission(pid)
                if out is None:
                    return self._json(
                        {"error": "unknown permission id",
                         "allowed": list(PERMISSION_GRANT_IDS)}, 400)
                code = 200 if out.get("ok") else 409
                return self._json(out, code)
            if url.path == "/api/permissions/revoke":
                out = revoke_permission(pid)
                if out is None:
                    return self._json(
                        {"error": "unknown permission id",
                         "allowed": list(PERMISSION_GRANT_IDS)}, 400)
                code = 200 if out.get("ok") else 409
                return self._json(out, code)
        except Exception as e:
            log_event("http", "%s failed: %r" % (url.path, e))
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
        if FIXTURE:
            snap = FIXTURE.current()
            if snap:
                return self._json(snap)
            return self._json({"error": "fixture empty or failed"}, 503)
        snap = SAMPLER.current if SAMPLER else None
        if not snap:
            return self._json({"error": "first sample in progress"}, 503)
        self._json(snap)

    def api_health(self):
        """T4a: liveness probe. Used by the single-instance check on startup
        and by anything that wants to know the server is up, its pid, and how
        long it has been running. Deliberately independent of the sampler —
        health must answer even while the first sample is still in progress."""
        self._json({"ok": True, "pid": os.getpid(),
                    "uptime_s": round(time.monotonic() - START_MONO, 3),
                    "port": PORT})

    def api_power_history(self, qs):
        """T13: current W + 1m/1h/1d/7d windows + chart series from the ring."""
        rng = (qs.get("range") or [None])[0]
        if rng and rng not in RANGES:
            return self._json(
                {"error": "range must be one of " + ",".join(RANGES)}, 400)
        self._json(power_history_payload(rng))

    def api_analytics(self, qs):
        """T14B: the model page. `?range=` picks the measures window."""
        rng = (qs.get("range") or [None])[0]
        if rng and rng not in RANGES:
            return self._json(
                {"error": "range must be one of " + ",".join(RANGES)}, 400)
        self._json(analytics_payload(rng))

    def api_consumers(self, qs):
        """T14C #3: WHO consumed a module's resource over the selected range.

        Query: module=cpu|memory|gpu|disk|network|power|all, range=<RANGES>,
        by=process|name, offset/limit (opt-in paging, count is always total),
        pid=<n>&series=1 -> that process's own series for the drawer.
        """
        module = (qs.get("module") or ["cpu"])[0]
        if module not in CONSUMER_MODULES:
            return self._json(
                {"error": "module must be one of " +
                          ",".join(sorted(CONSUMER_MODULES))}, 400)
        rng = (qs.get("range") or ["1h"])[0]
        if rng not in RANGES:
            return self._json(
                {"error": "range must be one of " + ",".join(RANGES)}, 400)
        group_by = (qs.get("by") or ["process"])[0]
        if group_by not in ("process", "name"):
            return self._json({"error": "by must be process|name"}, 400)
        try:
            offset = int((qs.get("offset") or ["0"])[0] or 0)
            limit_raw = (qs.get("limit") or [""])[0]
            limit = int(limit_raw) if limit_raw else None
            pid_raw = (qs.get("pid") or [""])[0]
            pid = int(pid_raw) if pid_raw else None
        except ValueError:
            return self._json(
                {"error": "offset, limit and pid must be integers"}, 400)
        name = (qs.get("name") or [None])[0]
        want_series = (qs.get("series") or [""])[0].lower() in ("1", "true", "yes")
        self._json(consumers_payload(module, rng, offset=offset, limit=limit,
                                     group_by=group_by, pid=pid, name=name,
                                     want_series=want_series))

    def api_apps(self, qs):
        """T17B: one row per application over the selected range.

        Query: range=<RANGES>. No offset/limit — the whole list is returned and
        the UI virtualises it, so a top-5 cap can never hide an app.
        """
        rng = (qs.get("range") or ["1h"])[0]
        if rng not in RANGES:
            return self._json(
                {"error": "range must be one of " + ",".join(RANGES)}, 400)
        self._json(apps_payload(rng))

    def api_app_series(self, qs):
        """T17B: one app's own CPU / memory / energy / disk / network series."""
        name = (qs.get("name") or [""])[0]
        if not name:
            return self._json({"error": "name is required"}, 400)
        rng = (qs.get("range") or ["1h"])[0]
        if rng not in RANGES:
            return self._json(
                {"error": "range must be one of " + ",".join(RANGES)}, 400)
        self._json(app_series_payload(name, rng))

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

    def api_graph(self, qs):
        """Serve a metric series for the selected window.

        In fixture mode data comes from the iStat fixture; in live mode it is
        read from the dashboard's history index.  The response always includes
        server-computed ticks (the time-axis ladder) and a peak chip.

        Query params:
          metric  required — cpu | memory | network | disk | gpu | load |
                              battery | sensors
          range   optional — one of RANGES (sets from/to automatically)
          from    optional — epoch or ISO start timestamp
          to      optional — epoch or ISO end timestamp

        Returns: {metric, range, from, to, points:[...], ticks:[...], peak:{...}}.
        """
        metric = (qs.get("metric") or [""])[0]
        metric_aliases = {"net": "network", "mem": "memory", "ram": "memory"}
        metric = metric_aliases.get(metric, metric)
        if metric not in ("cpu", "memory", "network", "disk", "gpu",
                          "load", "battery", "sensors"):
            return self._json({"error": "unknown metric: " + metric}, 400)

        rng = (qs.get("range") or [None])[0]
        if rng and rng in RANGES:
            if FIXTURE:
                # Fixture replay: "now" is the newest fixture record so 1h/1d
                # ranges actually contain data instead of being empty.
                to_e = FIXTURE.latest_ts() or int(datetime.now(timezone.utc).timestamp())
            else:
                to_e = int(datetime.now(timezone.utc).timestamp())
            from_e = to_e - RANGES[rng]
        else:
            from_e = _parse_epoch((qs.get("from") or [None])[0])
            to_e = _parse_epoch((qs.get("to") or [None])[0])
            # Fixture replay has no "now": a parameterless call means the whole
            # corpus. Windowing to today would return nothing for an old file,
            # and the replay exists precisely to serve that file.
            if FIXTURE and from_e is None and to_e is None:
                every = FIXTURE.graph(metric, None, None).get("points") or []
                if every:
                    from_e = int(every[0]["ts"])
                    to_e = int(every[-1]["ts"])
            if from_e is None:
                to_e = to_e or int(datetime.now(timezone.utc).timestamp())
                from_e = to_e - RANGES["1h"]
            if to_e is None:
                to_e = from_e + RANGES["1h"]
            rng = None

        if FIXTURE:
            points = FIXTURE.graph(metric, from_e, to_e)["points"]
        else:
            points = _live_graph_points(metric, from_e, to_e)

        ticks = tick_ladder(rng or _range_from_secs(to_e - from_e), from_e, to_e)
        peak = compute_peak(metric, points)
        note = GRAPH_NOTES.get(metric)
        # T13: every graph says whether it is real, and if not, why.  A metric
        # this machine cannot measure is never rendered as a flat zero line.
        # T14: GPU and disk are sampled dashboard-side every 5 s (ioreg), so
        # they are real measured series now — no "unsupported" marker.  Every
        # response still states its source, and an empty window says how much
        # ring history actually exists instead of drawing a flat zero.
        out = {"metric": metric, "range": rng,
               "from": from_e, "to": to_e,
               "points": points, "ticks": ticks, "peak": peak,
               "available": bool(points),
               "source": GRAPH_SOURCES.get(metric),
               "measures": metric_measures(metric, points, from_e, to_e)}
        if metric in ("gpu", "disk") and not FIXTURE:
            ring = GPU_RING if metric == "gpu" else DISK_RING
            out["ring"] = ring.state(time.time())
            # T14B addendum 2: say whether this ring could be seeded from the
            # collector history store. It cannot for GPU/disk — the store has no
            # such field — and the footer shows why instead of a silent gap.
            out["history"] = {
                "seeded": 0,
                "available": False,
                "reason": ("the collector log records gpu_time only "
                           "(unavailable_no_rusage_field), never GPU utilisation"
                           if metric == "gpu" else
                           "the collector log records disk capacity (used/free), "
                           "not byte counters, so no throughput history exists"),
                "live_samples": ring.state(time.time()).get("samples", 0),
            }
        if note:
            out["note"] = note
        if not points:
            out["unavailable_reason"] = _graph_empty_reason(metric, from_e, to_e)
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

    # -- T4b: agent event stream -------------------------------------------
    def api_agent_events(self, qs):
        """T4b #1: JSON array of the last N lines of .hermes/run/events.jsonl.

        Tail read (agent_events_tail), never the whole file. Missing file
        -> []. Each element is the line's JSON object; a malformed line is
        passed through as its raw string rather than dropped or failed."""
        try:
            limit = int((qs.get("limit") or ["50"])[0])
        except ValueError:
            limit = 50
        limit = max(1, min(limit, AGENT_EVENTS_MAX_LIMIT))
        out = []
        for line in agent_events_tail(limit):
            try:
                out.append(json.loads(line))
            except ValueError:
                out.append(line)
        self._json(out)

    def api_agent_stream(self):
        """T4b #2: SSE tail of .hermes/run/events.jsonl.

        Runs on its own thread (ThreadingHTTPServer) with its own file
        offset, so a slow or hung cockpit tab blocks neither other requests
        nor other tabs. Header/loop errors are logged here and swallowed:
        once the SSE headers are out, a 500 body would corrupt the stream."""
        # No second request will be parsed on this connection: the response
        # is close-delimited (no Content-Length) by design.
        self.close_connection = True
        try:
            self._agent_stream_run()
        except Exception as e:  # noqa: BLE001 - never let a stream error 500
            log_event("agent-stream", "ended: %r" % e)

    def _agent_stream_run(self):
        try:
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream; charset=utf-8")
            self.send_header("Cache-Control", "no-cache")
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(b'data: {"kind":"hello"}\n\n')
            self.wfile.flush()
        except OSError:
            return  # client hung up during the handshake

        # Per-connection tail state.
        pos = None    # read offset; None = not positioned yet
        ino = None    # inode at last sighting (rotation detector)
        gone = False  # stat failed since last sighting (delete detector)
        partial = b""  # trailing line without its newline yet
        while not _sse_peer_closed(self.connection):
            try:
                st = os.stat(AGENT_EVENTS_PATH)
            except OSError:
                gone = True  # missing: not created yet / deleted / mid-rotate
                time.sleep(AGENT_STREAM_POLL_S)
                continue
            try:
                if gone or (ino is not None and st.st_ino and st.st_ino != ino):
                    pos, partial = 0, b""  # recreated or rotated -> read from top
                gone = False
                if pos is None:
                    pos = st.st_size  # first sighting: EOF, only NEW lines
                elif st.st_size < pos:
                    pos, partial = 0, b""  # truncated -> re-read from top
                ino = st.st_ino or ino
                if st.st_size > pos:
                    with open(AGENT_EVENTS_PATH, "rb") as f:
                        f.seek(pos)
                        chunk = f.read()
                    pos += len(chunk)
                    data = partial + chunk
                    lines = data.split(b"\n")
                    partial = b""
                    if not data.endswith(b"\n"):
                        partial = lines.pop()  # wait for the newline
                    for ln in lines:
                        text = ln.decode("utf-8", errors="replace").rstrip("\r")
                        if not text.strip():
                            continue
                        self.wfile.write(("data: %s\n\n" % text).encode("utf-8"))
                    self.wfile.flush()
            except (BrokenPipeError, ConnectionAbortedError, ConnectionResetError):
                return  # client hung up mid-stream
            except OSError as e:
                log_event("agent-stream", "read error: %r" % e)
                return
            time.sleep(AGENT_STREAM_POLL_S)

    def log_message(self, fmt, *args):
        pass


class _FastBindServer(ThreadingHTTPServer):
    """ThreadingHTTPServer that skips socket.getfqdn() during bind.

    WHY (T4a): http.server.HTTPServer.server_bind() calls socket.getfqdn()
    on the bound address AFTER socket.bind() but BEFORE listen(). Measured
    on this machine: getfqdn('127.0.0.1')=47.9s and getfqdn('0.0.0.0')=48.0s
    while gethostbyaddr('127.0.0.1')=0.00s — the DNS stack stalls ~35-50s,
    so startup only reached READY after 36s and the acceptance's `sleep 6`
    could never see a serving server (the socket was bound but not yet
    listening, which also produces confusing EADDRINUSE on a second start).
    server_name/server_port are only consulted by CGI redirect logic, which
    this dashboard does not use, so we set them by hand from getsockname().
    """

    def server_bind(self):
        socketserver.TCPServer.server_bind(self)  # setsockopt(SO_REUSEADDR)+bind
        host, port = self.socket.getsockname()[:2]
        self.server_name = host
        self.server_port = port


def _probe_existing(port, timeout=1.0):
    """T4a single-instance probe: is anything already serving on 127.0.0.1:port?

    Returns:
      int  -> pid reported by an existing /api/health (only our server
              serves it, so this is the live dashboard's pid)
      -1   -> port answers but no pid could be read (foreign occupant)
      None -> nothing accepted a connection; port is free to bind

    Never raises. Order: quick GET /api/health first (gives us the pid),
    fall back to a bare TCP connect (proves the port is occupied even if
    the occupant doesn't speak HTTP)."""
    try:
        with urlopen("http://127.0.0.1:%d/api/health" % port,
                     timeout=timeout) as r:
            data = json.loads(r.read().decode("utf-8"))
        pid = data.get("pid")
        return pid if isinstance(pid, int) else -1
    except Exception:
        pass
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=0.5):
            return -1
    except OSError:
        return None


def main():
    # T4a #1 - single instance: probe BEFORE any side effects (data dirs,
    # alerter, sampler threads, .sampler.pid lock, bind). If 8123 already
    # answers, print the live pid and exit 0 without touching anything --
    # no second bind, no kill of the existing server.
    existing = _probe_existing(PORT)
    if existing is not None:
        print("ALREADY_RUNNING pid=%d" % existing, flush=True)
        sys.exit(0)

    os.makedirs(DATA_DIR, exist_ok=True)
    roll_store()

    if FIXTURE:
        # T5: fixture replay mode — no live psutil sampler or collector tail.
        alerts.start_alerter(source=FIXTURE.current)
        log_event("server", "fixture replay mode: %s" % FIXTURE_PATH)
    else:
        alerts.start_alerter(source=lambda: SAMPLER.current if SAMPLER else None)

    # mirror alert fires into the events ring for the Logs tile
    orig_fire = alerts.Alerter._fire

    def _fire_with_event(self, rule, value):
        log_event("alerts", "FIRE %s %s %s current=%s" %
                  (rule.get("metric"), rule.get("op"), rule.get("value"),
                   round(value, 1)))
        orig_fire(self, rule, value)

    alerts.Alerter._fire = _fire_with_event

    if FIXTURE:
        # History is served from /api/graph in fixture mode; nothing to index.
        pass
    else:
        threading.Thread(target=sampler_loop, args=(SAMPLER,),
                         name="syswatch-sampler", daemon=True).start()
        threading.Thread(target=HIST.index_all, name="syswatch-history",
                         daemon=True).start()
        threading.Thread(target=HIST.tail_loop, name="syswatch-tail",
                         daemon=True).start()
        # T13: the power ring fills independently of requests so the 1m/1h/1d
        # windows have real history the moment a client opens the page.
        threading.Thread(target=power_ring_loop, name="syswatch-power",
                         daemon=True).start()
        # T14: GPU utilisation + disk throughput rings (5 s, ioreg). Kept off
        # the 1 s tick path — a probe is ~26 ms of two ioreg spawns.
        threading.Thread(target=io_ring_loop, name="syswatch-io",
                         daemon=True).start()
        # T13: prime + keep warm the analytics per-pid CPU baseline so
        # /api/analytics reports measured % instead of "warming".
        threading.Thread(target=_top_apps_warm_loop,
                         name="syswatch-analytics-warm", daemon=True).start()
        # T14C #3: per-process range store for the Consumers panels. 20 s
        # cadence (rusage costs ~25 ms per 400 PIDs); deliberately NOT on the
        # 1 s tick path.
        threading.Thread(target=proc_ring_loop, name="syswatch-proc-range",
                         daemon=True).start()
        # T17B: keep our own battery history warm (one row per 30 min) so the
        # degradation outlook has a baseline of our own making. The collector
        # log carries no cycle count and the 1 s history rows carry none
        # either, so this is the only source. Off every tick path.
        threading.Thread(target=battery_history_loop, name="syswatch-battery",
                         daemon=True).start()

    # T4a: bind loopback only -- never expose the dashboard to the network
    # (tray and every consumer already use 127.0.0.1). _FastBindServer skips
    # the ~48s getfqdn() stall in the stdlib bind path (see class docstring).
    srv = _FastBindServer(("127.0.0.1", PORT), Handler)
    log_event("server", "dashboard v2 ready on port %d" % PORT)
    print("SYSWATCH_DASHBOARD_READY on port %d" % PORT, flush=True)

    # T4a #3 - clean SIGTERM: shutdown() blocks until the serve_forever()
    # loop exits, and that loop runs in THIS thread, so calling shutdown()
    # directly inside the handler would deadlock. Hop to a helper thread;
    # serve_forever() then returns and the finally block closes the socket,
    # releasing the port (well inside the 2s budget).
    def _on_sigterm(signum, frame):
        threading.Thread(target=srv.shutdown, name="sigterm-shutdown",
                         daemon=True).start()

    try:
        signal.signal(signal.SIGTERM, _on_sigterm)
    except ValueError:
        pass  # not the main thread (in-process tests); default SIGTERM applies
    try:
        srv.serve_forever()
    finally:
        srv.server_close()  # closes the listening socket -> port released
        print("SYSWATCH_DASHBOARD_STOPPED port=%d" % PORT, flush=True)


if __name__ == "__main__":
    main()