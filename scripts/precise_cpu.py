#!/usr/bin/env python3
"""Precise CPU% measurement using cumulative CPU time delta.

psutil.cpu_percent(interval=None) misses short-lived CPU bursts because
the rolling-window baseline is too coarse on Windows. Instead, capture
the process's cumulative user+system CPU time, sleep exactly N seconds,
then compute (delta_cpu_time / wall_seconds) * 100.

Usage:
    python precise_cpu.py <name-substring> <duration-seconds>
"""
import sys
import time
import psutil


def find_pid(substr: str):
    for p in psutil.process_iter(["name", "pid"]):
        try:
            if substr.lower() in (p.info["name"] or "").lower():
                return p.info["pid"]
        except (psutil.NoSuchProcess, psutil.AccessDenied):
            continue
    return None


def main():
    substr = sys.argv[1]
    dur = float(sys.argv[2])
    pid = find_pid(substr)
    if pid is None:
        print(f"no process matching '{substr}'", file=sys.stderr)
        sys.exit(2)
    p = psutil.Process(pid)
    p.cpu_percent(interval=None)  # prime
    t0 = time.time()
    c0 = sum(p.cpu_times()[:4])
    r0 = p.memory_info().rss
    time.sleep(dur)
    c1 = sum(p.cpu_times()[:4])
    r1 = p.memory_info().rss
    wall = time.time() - t0
    used = c1 - c0
    pct = (used / wall) * 100.0
    print(
        f"pid={pid} wall={wall:.2f}s cpu_used={used:.3f}s "
        f"cpu_pct_mean={pct:.3f}% "
        f"rss_avg={(r0+r1)/2/1024/1024:.2f}MB rss_end={r1/1024/1024:.2f}MB"
    )


if __name__ == "__main__":
    main()