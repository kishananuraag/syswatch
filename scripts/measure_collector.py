#!/usr/bin/env python3
"""Measure syswatch collector's CPU% and RSS over a window.

Usage:
    python measure_collector.py --pid <PID> --duration 60 --interval 2
    python measure_collector.py --name syswatch.exe --duration 60 --interval 2
    python measure_collector.py --name syswatch.exe --duration 60 --interval 2 --all

If --all is passed, sum CPU and pick the *largest* RSS across all matching processes
(useful when service is running). Otherwise measure a specific PID.
"""
import argparse
import csv
import statistics
import sys
import time

import psutil


def find_pids(name: str):
    out = []
    for p in psutil.process_iter(["name"]):
        try:
            if p.info["name"] and p.info["name"].lower() == name.lower():
                out.append(p.pid)
        except (psutil.NoSuchProcess, psutil.AccessDenied):
            continue
    return out


def measure(pid: int, interval: float, duration: float, all_procs: bool):
    """Sample PID(s) over `duration`. Uses interval=None (rolling) by default
    but supports per-sample `interval=0.5` for low-burst processes where
    the rolling window misses activity."""
    samples = []
    end = time.time() + duration
    psutil.cpu_count()  # warm
    last = time.time()
    proc_interval = None if interval >= 1.0 else max(0.05, interval / 2)
    # Initial sample so cpu_percent has a baseline.
    try:
        if all_procs:
            for p in psutil.process_iter(["name", "pid"]):
                try:
                    if p.info["name"] and p.info["name"].lower() == "syswatch.exe":
                        p.cpu_percent(interval=None)
                except (psutil.NoSuchProcess, psutil.AccessDenied):
                    continue
        else:
            psutil.Process(pid).cpu_percent(interval=None)
    except psutil.NoSuchProcess:
        pass
    while time.time() < end:
        time.sleep(interval)
        now = time.time()
        dt = now - last
        last = now
        if all_procs:
            total_cpu = 0.0
            max_rss = 0
            for p in psutil.process_iter(["name", "pid"]):
                try:
                    if p.info["name"] and p.info["name"].lower() == "syswatch.exe":
                        total_cpu += p.cpu_percent(interval=proc_interval)
                        max_rss = max(max_rss, p.memory_info().rss)
                except (psutil.NoSuchProcess, psutil.AccessDenied):
                    continue
            samples.append((now, total_cpu, max_rss))
        else:
            try:
                p = psutil.Process(pid)
                cpu = p.cpu_percent(interval=proc_interval)
                rss = p.memory_info().rss
                samples.append((now, cpu, rss))
            except psutil.NoSuchProcess:
                samples.append((now, 0.0, 0))
                print(f"WARN: pid {pid} died at sample {len(samples)}", file=sys.stderr)
                break
    return samples


def pct(values, p):
    if not values:
        return 0.0
    s = sorted(values)
    idx = max(0, min(len(s) - 1, int(round(p / 100.0 * (len(s) - 1)))))
    return s[idx]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--pid", type=int, default=None)
    ap.add_argument("--name", default="syswatch.exe")
    ap.add_argument("--duration", type=float, default=60.0)
    ap.add_argument("--interval", type=float, default=2.0)
    ap.add_argument("--all", action="store_true",
                    help="Measure all processes matching --name (sum CPU, max RSS)")
    ap.add_argument("--csv", default=None, help="Optional CSV output path")
    args = ap.parse_args()

    if args.pid is None:
        pids = find_pids(args.name)
        if not pids:
            print(f"no process named {args.name} found", file=sys.stderr)
            sys.exit(2)
        if len(pids) > 1 and not args.all:
            print(f"multiple matches for {args.name}: {pids}. Pass --pid or --all.",
                  file=sys.stderr)
            sys.exit(2)
        args.pid = pids[0]
        print(f"target: {args.name} pid={args.pid}", file=sys.stderr)

    print(f"measuring pid={args.pid} all={args.all} for {args.duration}s @ {args.interval}s",
          file=sys.stderr)
    samples = measure(args.pid, args.interval, args.duration, args.all)

    if args.csv:
        with open(args.csv, "w", newline="") as f:
            w = csv.writer(f)
            w.writerow(["t", "cpu_pct", "rss_bytes"])
            for t, c, r in samples:
                w.writerow([f"{t:.3f}", f"{c:.3f}", r])

    cpus = [s[1] for s in samples if s[1] >= 0]
    rss = [s[2] for s in samples]
    if not cpus:
        print("no samples collected", file=sys.stderr)
        sys.exit(3)

    print()
    print(f"samples     : {len(cpus)}")
    print(f"cpu mean%   : {statistics.mean(cpus):.3f}")
    print(f"cpu p50%    : {statistics.median(cpus):.3f}")
    print(f"cpu p95%    : {pct(cpus, 95):.3f}")
    print(f"cpu max%    : {max(cpus):.3f}")
    print(f"rss mean MB : {statistics.mean(rss) / 1024 / 1024:.2f}")
    print(f"rss max MB  : {max(rss) / 1024 / 1024:.2f}")


if __name__ == "__main__":
    main()