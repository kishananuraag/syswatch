#!/usr/bin/env python3
"""Export the iStat Menus 7 history database into a portable JSONL corpus.

Why: iStat keeps 87 days of machine-level samples in one SQLite file. That is a
free, real, high-fidelity fixture for our own graphs -- and a reference to beat.
This script never writes to the database (opened read-only via URI) and has no
dependencies outside the standard library.

Output layout (default out dir: fixtures/istat/):
    <table>.jsonl     one JSON object per sample, keys = column names
    manifest.json     row counts, time ranges, interval ladders, units
    sample.jsonl      merged multi-metric view for the given interval + window
                      (this is the graph test sample)

Usage:
    python3 scripts/istat_export.py                       # everything, then sample
    python3 scripts/istat_export.py --days 7               # last 7 days only
    python3 scripts/istat_export.py --sample-interval 90 \
        --sample-from "2026-11-15T08:00" --sample-to "2026-11-15T15:00"
"""
from __future__ import annotations

import argparse
import json
import os
import sqlite3
import sys
from datetime import datetime, timedelta, timezone

DEFAULT_DB = os.path.expanduser(
    "~/Library/Application Support/iStat Menus 7/history/history.db"
)

IST = timezone(timedelta(hours=5, minutes=30))

# table -> (columns, unit note). Confirmed by sampling real rows on 2026-11-15.
TABLES = {
    "cpu": (["time", "interval", "user", "system"], "percent of total machine capacity (0-100 x all cores)"),
    "load": (["time", "interval", "one", "five", "fifteen"], "load average"),
    "memory": (["time", "interval", "pressure", "wired", "app", "compressed", "total"],
               "MB (total = installed RAM in MB); pressure = percent"),
    "swap": (["time", "interval", "swap"], "GB used"),
    "gpu": (["time", "interval", "key", "processor", "memory"], "percent"),
    "network": (["time", "interval", "key", "upload", "download", "uploadpeak", "downloadpeak"],
                "MB/s"),
    "diskactivity": (["time", "interval", "key", "read", "write", "readpeak", "writepeak"],
                     "KB/s"),
    "battery": (["time", "interval", "key", "percentage", "source"],
                "percent; source 1.0 = on AC power"),
    "sensors": (["time", "interval", "key", "value", "peak"],
                "raw SMC/IOHID value; key is an md5 with no name stored in the DB"),
}

MAP_TABLES = {
    "sensors": "sensorsmap",
    "network": "networkmap",
    "gpu": "gpumap",
    "diskactivity": "diskactivitymap",
    "battery": "batterymap",
}


def connect_ro(path: str) -> sqlite3.Connection:
    if not os.path.exists(path):
        sys.exit(f"error: history db not found: {path}")
    uri = f"file:{path}?mode=ro"
    try:
        con = sqlite3.connect(uri, uri=True, timeout=5)
        con.execute("select count(*) from cpu").fetchone()
        return con
    except sqlite3.OperationalError as exc:
        # iStat holds an exclusive lock while writing; copy then read the copy.
        import shutil
        import tempfile

        tmp = os.path.join(tempfile.gettempdir(), "istat_history_copy.db")
        shutil.copyfile(path, tmp)
        print(f"note: db locked ({exc}); reading a snapshot copy instead", file=sys.stderr)
        return sqlite3.connect(f"file:{tmp}?mode=ro", uri=True, timeout=5)


def iso(ts: int) -> str:
    return datetime.fromtimestamp(ts, IST).isoformat(timespec="seconds")


def export_table(con: sqlite3.Connection, table: str, cols: list[str],
                 out_dir: str, since: int | None) -> dict:
    where, params = "", []
    if since is not None:
        where, params = " where time >= ?", [since]
    sql = f"select {','.join(cols)} from {table}{where} order by time, interval"
    n = 0
    lo = hi = None
    intervals: dict[int, int] = {}
    path = os.path.join(out_dir, f"{table}.jsonl")
    with open(path, "w", encoding="utf-8") as fh:
        for row in con.execute(sql, params):
            rec = dict(zip(cols, row))
            fh.write(json.dumps(rec, separators=(",", ":")) + "\n")
            n += 1
            t = rec["time"]
            lo = t if lo is None else min(lo, t)
            hi = t if hi is None else max(hi, t)
            iv = rec.get("interval")
            if iv is not None:
                intervals[iv] = intervals.get(iv, 0) + 1
    return {
        "rows": n,
        "file": os.path.basename(path),
        "columns": cols,
        "units": TABLES[table][1],
        "first_ist": iso(lo) if lo else None,
        "last_ist": iso(hi) if hi else None,
        "intervals": dict(sorted(intervals.items())),
    }


def export_maps(con: sqlite3.Connection, out_dir: str) -> dict:
    maps = {}
    for table, map_table in MAP_TABLES.items():
        try:
            keys = [r[0] for r in con.execute(f"select key from {map_table}")]
        except sqlite3.OperationalError:
            keys = []
        maps[table] = {"map_table": map_table, "key_count": len(keys),
                       "keys": keys[:64]}
    return maps


def build_sample(con: sqlite3.Connection, out_dir: str, interval: int,
                 t_from: int | None, t_to: int | None) -> dict:
    """Merge every metric into one record per timestamp for the graph fixture."""
    where, params = " where interval = ?", [interval]
    if t_from is not None:
        where += " and time >= ?"
        params.append(t_from)
    if t_to is not None:
        where += " and time <= ?"
        params.append(t_to)

    rows: dict[int, dict] = {}

    def slot(ts: int) -> dict:
        return rows.setdefault(ts, {
            "ts": ts, "iso": iso(ts), "interval": interval,
            "cpu": None, "load": None, "mem": None, "swap_gb": None,
            "gpu": [], "net": [], "disk": [], "batt": None, "sensors": {},
        })

    for t, iv, user, system in con.execute(f"select time,interval,user,system from cpu{where}", params):
        s = slot(t)
        s["cpu"] = {"user": round(user, 2), "system": round(system, 2),
                    "total": round(user + system, 2)}
    for t, iv, one, five, fifteen in con.execute(f"select time,interval,one,five,fifteen from load{where}", params):
        slot(t)["load"] = {"one": one, "five": five, "fifteen": fifteen}
    for t, iv, pressure, wired, app, compressed, total in con.execute(
            f"select time,interval,pressure,wired,app,compressed,total from memory{where}", params):
        slot(t)["mem"] = {"pressure_pct": round(pressure, 1), "wired_mb": round(wired, 1),
                          "app_mb": round(app, 1), "compressed_mb": round(compressed, 1),
                          "total_mb": total}
    for t, iv, swap in con.execute(f"select time,interval,swap from swap{where}", params):
        slot(t)["swap_gb"] = round(swap, 3)
    for t, iv, key, proc, mem in con.execute(f"select time,interval,key,processor,memory from gpu{where}", params):
        slot(t)["gpu"].append({"key": key, "proc_pct": proc, "mem_pct": mem})
    for t, iv, key, up, down, upk, downk in con.execute(
            f"select time,interval,key,upload,download,uploadpeak,downloadpeak from network{where}", params):
        slot(t)["net"].append({"key": key, "up_mbps": up, "down_mbps": down,
                               "up_peak": upk, "down_peak": downk})
    for t, iv, key, r, w, rp, wp in con.execute(
            f"select time,interval,key,read,write,readpeak,writepeak from diskactivity{where}", params):
        slot(t)["disk"].append({"key": key, "read_kbps": r, "write_kbps": w,
                                "read_peak": rp, "write_peak": wp})
    for t, iv, key, pct, src in con.execute(
            f"select time,interval,key,percentage,source from battery{where}", params):
        slot(t)["batt"] = {"pct": pct, "on_ac": bool(src)}
    for t, iv, key, val, peak in con.execute(
            f"select time,interval,key,value,peak from sensors{where}", params):
        slot(t)["sensors"][str(key)] = round(val, 2)

    path = os.path.join(out_dir, "sample.jsonl")
    with open(path, "w", encoding="utf-8") as fh:
        for ts in sorted(rows):
            fh.write(json.dumps(rows[ts], separators=(",", ":")) + "\n")
    times = sorted(rows)
    return {"rows": len(rows), "file": "sample.jsonl", "interval": interval,
            "first_ist": iso(times[0]) if times else None,
            "last_ist": iso(times[-1]) if times else None}


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--db", default=DEFAULT_DB, help="path to history.db")
    ap.add_argument("--out", default="fixtures/istat", help="output directory")
    ap.add_argument("--days", type=float, default=None, help="only the last N days")
    ap.add_argument("--tables", default=None, help="comma list, default all")
    ap.add_argument("--sample-interval", type=int, default=90,
                    help="interval ladder step for sample.jsonl (default 90 s)")
    ap.add_argument("--sample-from", default=None, help="ISO local time, e.g. 2026-11-15T08:00")
    ap.add_argument("--sample-to", default=None)
    ap.add_argument("--no-sample", action="store_true", help="skip sample.jsonl")
    args = ap.parse_args()

    os.makedirs(args.out, exist_ok=True)
    con = connect_ro(args.db)

    now = int(datetime.now(IST).timestamp())
    since = int(now - args.days * 86400) if args.days else None
    wanted = [t.strip() for t in args.tables.split(",")] if args.tables else list(TABLES)

    manifest = {"generated_ist": iso(now), "source_db": args.db,
                "source_schema_version": "iStat Menus 7",
                "tables": {}, "sample": None, "maps": {}}
    for table in wanted:
        cols = TABLES[table][0]
        info = export_table(con, table, cols, args.out, since)
        manifest["tables"][table] = info
        print(f"{table:14s} {info['rows']:>8d} rows  {info['first_ist']} -> {info['last_ist']}")

    manifest["maps"] = export_maps(con, args.out)

    if not args.no_sample:
        def parse(s: str | None) -> int | None:
            if not s:
                return None
            return int(datetime.fromisoformat(s).replace(tzinfo=IST).timestamp())

        sample = build_sample(con, args.out, args.sample_interval,
                             parse(args.sample_from), parse(args.sample_to))
        manifest["sample"] = sample
        print(f"sample.jsonl   {sample['rows']:>8d} rows  {sample['first_ist']} -> {sample['last_ist']}")

    with open(os.path.join(args.out, "manifest.json"), "w", encoding="utf-8") as fh:
        json.dump(manifest, fh, indent=2)
    print(f"\nwrote {args.out}/manifest.json")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
