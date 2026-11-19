#!/usr/bin/env python3
"""Measure syswatch's steady-state CPU/RSS over a window, and compare two runs.

Standard library only (no psutil). Per-tick numbers come from `ps`.

Measurement mode:
    python3 measure_collector.py --duration 60 --out before.csv
    python3 measure_collector.py --pid 134652 --duration 60 --out after.csv
    python3 measure_collector.py --name syswatch.exe --duration 60 --all --out a.csv

Comparison mode:
    python3 measure_collector.py --compare before.csv after.csv

CSV columns, exactly:
    ts,elapsed_s,cpu_pct,rss_kb,json_ok,cpu_total_pct

    cpu_pct       CPU percent of the measured syswatch process   (`ps -o %cpu=`)
    rss_kb        resident set size of that process, in KB        (`ps -o rss=`)
    json_ok       1 if `syswatch --json` returned valid JSON this tick, else 0
    cpu_total_pct machine-wide CPU percent from that JSON snapshot

What gets measured (first hit wins):
    1. --pid PID
    2. live process matching --name (default: syswatch, syswatch-tray,
       syswatch.exe, syswatch-tray.exe)
    3. a collector spawned from --bin (unless --no-spawn)

Exit codes: 0 ok, 2 bad args / no target / unreadable input CSV.
"""
import argparse
import csv
import json
import os
import shlex
import shutil
import statistics
import subprocess
import sys
import threading
import time

CSV_HEADER = ["ts", "elapsed_s", "cpu_pct", "rss_kb", "json_ok", "cpu_total_pct"]

DEFAULT_NAMES = ["syswatch", "syswatch-tray", "syswatch.exe", "syswatch-tray.exe"]

DEFAULT_SPAWN_ARGS = "--interval-ms 1000 --no-color"

BIN_CANDIDATES = [
    "target/release/syswatch",
    "target/debug/syswatch",
    "target/release/syswatch-tray.exe",
    "target/release/syswatch.exe",
    "target/debug/syswatch.exe",
    "target/release/syswatch-tray",
]

HAVE_PS = shutil.which("ps") is not None


# ---------------------------------------------------------------- ps helpers

def _ps(args):
    """Run `ps` and return stripped stdout, or None when ps fails."""
    try:
        res = subprocess.run(["ps"] + args, capture_output=True, text=True, timeout=15)
    except (OSError, subprocess.SubprocessError):
        return None
    if res.returncode != 0:
        return None
    return res.stdout.strip() or None


def _group_int(text: str) -> int:
    return int(text.strip().replace(",", "") or 0)


def sample_pid_ps(pid: int):
    """(cpu_pct, rss_kb) for pid via ps, or None when the process is gone."""
    out = _ps(["-o", "%cpu=,rss=", "-p", str(pid)])
    if out is None:
        return None
    row = out.splitlines()[0].split()
    if len(row) < 2:
        return None
    try:
        return float(row[0]), int(float(row[1]))
    except ValueError:
        return None


def sample_pid_tasklist(pid: int):
    """Windows fallback: tasklist gives RSS only, so CPU is reported as 0."""
    try:
        res = subprocess.run(
            ["tasklist", "/fi", f"pid eq {pid}", "/fo", "csv", "/nh"],
            capture_output=True, text=True, timeout=15,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    line = res.stdout.strip().splitlines()
    if not line:
        return None
    try:
        cols = next(csv.reader([line[0]]))
    except (csv.Error, StopIteration):
        return None
    if len(cols) < 5:
        return None
    mem = cols[4].upper().replace("K", "").strip()
    try:
        return 0.0, _group_int(mem)
    except ValueError:
        return None


def sample_pid(pid: int):
    if HAVE_PS:
        return sample_pid_ps(pid)
    return sample_pid_tasklist(pid)


def find_pids(names):
    """PIDs whose image name matches one of `names` (case-insensitive)."""
    if not HAVE_PS:
        return find_pids_tasklist(names)
    out = _ps(["-A", "-o", "pid=", "-o", "comm="])
    if out is None:
        return []
    wanted = {n.lower() for n in names}
    pids = []
    for line in out.splitlines():
        parts = line.strip().split(None, 1)
        if len(parts) != 2:
            continue
        pid_s, comm = parts
        base = comm.replace("\\", "/").rsplit("/", 1)[-1].lower()
        if base in wanted:
            try:
                pids.append(int(pid_s))
            except ValueError:
                continue
    return pids


def find_pids_tasklist(names):
    pids = []
    for name in names:
        try:
            res = subprocess.run(
                ["tasklist", "/fi", f"imagename eq {name}", "/fo", "csv", "/nh"],
                capture_output=True, text=True, timeout=15,
            )
        except (OSError, subprocess.SubprocessError):
            continue
        for line in res.stdout.strip().splitlines():
            try:
                cols = next(csv.reader([line]))
            except (csv.Error, StopIteration):
                continue
            if len(cols) < 2:
                continue
            try:
                pid = int(cols[1])
            except ValueError:
                continue
            if pid not in pids:
                pids.append(pid)
    return pids


# ------------------------------------------------------------ json probe

def resolve_bin(explicit):
    if explicit:
        return explicit
    env = os.environ.get("SYSWATCH_BIN")
    if env:
        return env
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    for rel in BIN_CANDIDATES:
        path = os.path.join(root, rel)
        if os.path.exists(path):
            return path
    return shutil.which("syswatch") or "syswatch"


def probe_json(bin_path, extra_args, timeout):
    """Run `syswatch --json`. Returns (json_ok, cpu_total_pct)."""
    argv = [bin_path] + list(extra_args)
    try:
        res = subprocess.run(argv, capture_output=True, text=True, timeout=timeout)
    except (OSError, subprocess.SubprocessError):
        return 0, 0.0
    if res.returncode != 0:
        return 0, 0.0
    try:
        snap = json.loads(res.stdout)
    except ValueError:
        return 0, 0.0
    total = 0.0
    cpu = snap.get("cpu") if isinstance(snap, dict) else None
    if isinstance(cpu, dict):
        try:
            total = float(cpu.get("total_pct", 0.0) or 0.0)
        except (TypeError, ValueError):
            total = 0.0
    if total == 0.0 and isinstance(snap, dict):
        # older flat schema emitted by src/json.rs
        try:
            total = float(snap.get("cpu_pct", 0.0) or 0.0)
        except (TypeError, ValueError):
            total = 0.0
    return 1, total


# --------------------------------------------------------------- spawn

def process_comm(pid: int):
    """Image name of pid as ps reports it, or None."""
    out = _ps(["-o", "comm=", "-p", str(pid)])
    if out is None:
        return None
    return out.splitlines()[0].strip()


def wait_for_exec(pid: int, bin_path: str, timeout: float = 2.0) -> bool:
    """Wait until the spawned child has exec'd, so the first sample is real.

    A freshly forked child is still Python for a moment; sampling it then
    records the parent's RSS instead of syswatch's.
    """
    want = os.path.basename(bin_path).lower()
    deadline = time.time() + timeout
    while time.time() < deadline:
        comm = process_comm(pid)
        if comm is not None:
            base = comm.replace("\\", "/").rsplit("/", 1)[-1].lower()
            if base == want:
                return True
        time.sleep(0.02)
    return False


def _drain(fd):
    """Keep reading the pty so the collector never blocks on a full buffer."""
    try:
        while True:
            if not os.read(fd, 65536):
                break
    except OSError:
        pass


def spawn_collector(bin_path, extra_args):
    """Start a long-lived collector so steady state can be measured anywhere.

    Returns {"pid", "fd", "proc"}; fd/proc are used for cleanup.
    """
    argv = [bin_path] + list(extra_args)
    try:
        import pty
    except ImportError:
        pty = None
    if pty is not None and os.name == "posix":
        pid, fd = pty.fork()
        if pid == 0:  # child: give the TUI a real tty, then exec
            try:
                os.execv(bin_path, argv)
            finally:
                os._exit(127)
        threading.Thread(target=_drain, args=(fd,), daemon=True).start()
        return {"pid": pid, "fd": fd, "proc": None}
    proc = subprocess.Popen(
        argv,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    return {"pid": proc.pid, "fd": None, "proc": proc}


def stop_collector(spawned):
    if not spawned:
        return
    pid = spawned["pid"]
    try:
        os.kill(pid, 15)  # SIGTERM
    except OSError:
        pass
    proc = spawned["proc"]
    if proc is not None:
        try:
            proc.wait(timeout=3)
        except subprocess.SubprocessError:
            proc.kill()
        return
    deadline = time.time() + 3
    while time.time() < deadline:
        try:
            got, _ = os.waitpid(pid, os.WNOHANG)
        except ChildProcessError:
            return
        if got == pid:
            return
        time.sleep(0.05)
    try:
        os.kill(pid, 9)  # SIGKILL
        os.waitpid(pid, 0)
    except OSError:
        pass


# -------------------------------------------------------------- sampling

def pct(values, p):
    if not values:
        return 0.0
    s = sorted(values)
    idx = max(0, min(len(s) - 1, int(round(p / 100.0 * (len(s) - 1)))))
    return s[idx]


def iso_ts(now):
    return time.strftime("%Y-%m-%dT%H:%M:%S", time.gmtime(now)) + f".{int(now % 1 * 1000):03d}Z"


def sample_tick(pids, all_procs, bin_path, json_args, json_timeout):
    """One tick: (alive, cpu_pct, rss_kb, json_ok, cpu_total_pct)."""
    alive = 0
    cpu = 0.0
    rss = 0
    if all_procs:
        for pid in pids:
            got = sample_pid(pid)
            if got is None:
                continue
            alive += 1
            cpu += got[0]
            rss = max(rss, got[1])
    elif pids:
        got = sample_pid(pids[0])
        if got is not None:
            alive = 1
            cpu, rss = got
    json_ok, total = probe_json(bin_path, json_args, json_timeout)
    return alive, cpu, rss, json_ok, total


def measure(pids, all_procs, interval, duration, bin_path, json_args,
            json_timeout, out_path):
    """Sample the target(s) for `duration`, streaming rows to out_path.

    Returns the list of (cpu_pct, rss_kb, json_ok) tuples collected.
    """
    rows = []
    fh = None
    writer = None
    if out_path:
        fh = open(out_path, "w", newline="")
        writer = csv.writer(fh)
        writer.writerow(CSV_HEADER)
        fh.flush()

    warned = False
    t0 = time.time()
    end = t0 + duration
    next_at = t0
    tick = 0
    try:
        while True:
            now = time.time()
            if now > end:
                break
            elapsed = now - t0
            alive, cpu, rss, json_ok, total = sample_tick(
                pids, all_procs, bin_path, json_args, json_timeout
            )
            rows.append((cpu, rss, json_ok))
            if writer is not None:
                writer.writerow([
                    iso_ts(now),
                    f"{elapsed:.2f}",
                    f"{cpu:.2f}",
                    rss,
                    json_ok,
                    f"{total:.2f}",
                ])
                fh.flush()
            if alive == 0 and not warned:
                print("WARN: target process not visible; rows record 0 CPU / 0 RSS",
                      file=sys.stderr)
                warned = True
            tick += 1
            next_at = t0 + tick * interval
            sleep_for = next_at - time.time()
            if sleep_for > 0:
                time.sleep(sleep_for)
    finally:
        if fh is not None:
            fh.close()
    return rows


def summarize(rows):
    if not rows:
        return None
    cpus = [r[0] for r in rows]
    rss = [r[1] for r in rows]
    ok = [r[2] for r in rows]
    return {
        "samples": len(rows),
        "cpu_mean": statistics.mean(cpus),
        "cpu_p50": statistics.median(cpus),
        "cpu_p95": pct(cpus, 95),
        "cpu_max": max(cpus),
        "rss_mean_kb": statistics.mean(rss),
        "rss_max_kb": max(rss),
        "json_ok_pct": 100.0 * sum(ok) / len(ok),
    }


def print_summary(stats, out_path):
    print()
    print(f"csv         : {out_path or '(none)'}")
    print(f"samples     : {stats['samples']}")
    print(f"cpu mean%   : {stats['cpu_mean']:.3f}")
    print(f"cpu p50%    : {stats['cpu_p50']:.3f}")
    print(f"cpu p95%    : {stats['cpu_p95']:.3f}")
    print(f"cpu max%    : {stats['cpu_max']:.3f}")
    print(f"rss mean MB : {stats['rss_mean_kb'] / 1024:.2f}")
    print(f"rss max MB  : {stats['rss_max_kb'] / 1024:.2f}")
    print(f"json ok%    : {stats['json_ok_pct']:.1f}")


# ------------------------------------------------------------- compare

def _field(row, name):
    return (row.get(name) or "").strip()


def _num(row, name):
    raw = _field(row, name)
    if not raw:
        return 0.0
    try:
        return float(raw)
    except ValueError:
        return 0.0


def load_csv(path):
    """Load a measurement CSV, tolerating the legacy t,cpu_pct,rss_bytes header."""
    with open(path, newline="") as f:
        reader = csv.DictReader(f)
        rows = list(reader)
        fields = reader.fieldnames or []
    if "rss_kb" not in fields and "rss_bytes" in fields:
        for row in rows:
            row["rss_kb"] = str(_num(row, "rss_bytes") / 1024.0)
    return rows


def metrics(rows):
    cpus = [_num(r, "cpu_pct") for r in rows]
    rss = [_num(r, "rss_kb") for r in rows]
    oks = [_num(r, "json_ok") for r in rows]
    return {
        "samples": float(len(rows)),
        "cpu_mean": statistics.mean(cpus) if cpus else 0.0,
        "rss_mean": statistics.mean(rss) if rss else 0.0,
        "rss_max": max(rss) if rss else 0.0,
        "cpu_p95": pct(cpus, 95),
        "json_ok": 100.0 * sum(oks) / len(oks) if oks else 0.0,
    }


def _change(before, after):
    if before == 0.0:
        if after == 0.0:
            return "n/a"
        return "new"
    return f"{(after - before) / before * 100.0:+.2f}%"


def compare_files(before_path, after_path):
    for path in (before_path, after_path):
        if not os.path.exists(path):
            print(f"missing CSV: {path}", file=sys.stderr)
            return 2
    try:
        before = metrics(load_csv(before_path))
        after = metrics(load_csv(after_path))
    except (OSError, csv.Error) as exc:
        print(f"cannot read CSV: {exc}", file=sys.stderr)
        return 2

    labels = [
        ("samples", "samples", "{:.0f}"),
        ("cpu_mean", "cpu_pct mean", "{:.3f}"),
        ("rss_mean", "rss_kb mean", "{:.1f}"),
        ("rss_max", "rss_kb max", "{:.1f}"),
        ("cpu_p95", "cpu_pct p95", "{:.3f}"),
        ("json_ok", "json_ok pct", "{:.1f}"),
    ]
    print(f"before: {before_path}")
    print(f"after : {after_path}")
    print()
    print(f"{'metric':<16}{'before':>12}{'after':>12}{'delta':>12}{'change':>11}")
    print("-" * 63)
    for key, label, fmt in labels:
        b = before[key]
        a = after[key]
        print(f"{label:<16}{fmt.format(b):>12}{fmt.format(a):>12}"
              f"{fmt.format(a - b):>12}{_change(b, a):>11}")
    print()
    return 0


# ---------------------------------------------------------------- main

def build_parser():
    ap = argparse.ArgumentParser(
        description="Measure syswatch steady-state CPU/RSS over time, "
                    "and compare two measurement CSVs.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "CSV columns: " + ",".join(CSV_HEADER) + "\n\n"
            "Target resolution (first hit wins):\n"
            "  1. --pid PID\n"
            "  2. a live process matching --name\n"
            "  3. a collector spawned from --bin (disable with --no-spawn)\n\n"
            "Examples:\n"
            "  %(prog)s --duration 60 --out before.csv\n"
            "  %(prog)s --duration 60 --out after.csv --pid 134652\n"
            "  %(prog)s --compare before.csv after.csv\n"
            "  %(prog)s --name syswatch.exe --all --duration 30 --out a.csv\n"
        ),
    )
    # `--seconds` is an alias for `--duration` (the T7B acceptance command
    # uses it); argparse keeps `duration` as the dest either way.
    ap.add_argument("--duration", "--seconds", type=float, default=60.0,
                    help="seconds to sample (default 60)")
    ap.add_argument("--interval", type=float, default=1.0,
                    help="seconds between samples (default 1)")
    ap.add_argument("--out", default=None, metavar="PATH",
                    help="write CSV to this path")
    ap.add_argument("--csv", dest="csv_legacy", default=None, metavar="PATH",
                    help="legacy alias for --out")
    ap.add_argument("--compare", nargs=2, metavar=("BEFORE", "AFTER"), default=None,
                    help="compare two measurement CSVs and print deltas")
    ap.add_argument("--pid", type=int, default=None,
                    help="measure this PID")
    ap.add_argument("--name", default=None,
                    help="match a live image name instead of the default set "
                         "(syswatch, syswatch-tray, syswatch.exe, syswatch-tray.exe)")
    ap.add_argument("--all", action="store_true",
                    help="measure every process matching --name (sum CPU, max RSS)")
    ap.add_argument("--bin", default=None,
                    help="syswatch binary used for the --json probe and for spawning "
                         "(default: auto-detect target/release/syswatch)")
    ap.add_argument("--spawn", dest="spawn", action="store_true", default=None,
                    help="always spawn a collector from --bin and measure it")
    ap.add_argument("--no-spawn", dest="spawn", action="store_false",
                    help="never spawn; fail if no live target is found")
    ap.add_argument("--spawn-args", default=DEFAULT_SPAWN_ARGS,
                    help=f"args for the spawned collector (default: {DEFAULT_SPAWN_ARGS})")
    ap.add_argument("--json-args", default="--json",
                    help="args for the JSON probe (default: --json)")
    ap.add_argument("--json-timeout", type=float, default=10.0,
                    help="timeout in seconds for one JSON probe (default 10)")
    return ap


def main():
    ap = build_parser()
    args = ap.parse_args()

    if args.compare:
        return compare_files(args.compare[0], args.compare[1])

    if args.duration <= 0:
        print("--duration must be > 0", file=sys.stderr)
        return 2
    if args.interval <= 0:
        print("--interval must be > 0", file=sys.stderr)
        return 2

    out_path = args.out or args.csv_legacy
    bin_path = resolve_bin(args.bin)
    json_args = shlex.split(args.json_args)
    spawn_args = shlex.split(args.spawn_args)

    spawned = None
    if args.pid is not None:
        pids = [args.pid]
    else:
        names = [args.name] if args.name else DEFAULT_NAMES
        pids = find_pids(names)
        if len(pids) > 1 and not args.all:
            print(f"multiple matches for {names}: {pids}. Pass --pid or --all.",
                  file=sys.stderr)
            return 2
        if not pids:
            if args.spawn is False:
                print("no live target found and --no-spawn given", file=sys.stderr)
                return 2
            print(f"no live target found; spawning {bin_path} {args.spawn_args}",
                  file=sys.stderr)
            try:
                spawned = spawn_collector(bin_path, spawn_args)
            except OSError as exc:
                print(f"cannot spawn collector: {exc}", file=sys.stderr)
                return 2
            if not wait_for_exec(spawned["pid"], bin_path):
                print("WARN: spawned collector did not exec in 2s; first samples "
                      "may be off", file=sys.stderr)
            pids = [spawned["pid"]]

    print(f"measuring pid={pids} all={args.all} for {args.duration}s @ {args.interval}s",
          file=sys.stderr)

    stats = None
    try:
        rows = measure(pids, args.all, args.interval, args.duration, bin_path,
                       json_args, args.json_timeout, out_path)
        stats = summarize(rows)
    except KeyboardInterrupt:
        print("interrupted", file=sys.stderr)
        return 130
    finally:
        if spawned is not None:
            stop_collector(spawned)

    if stats is None:
        print("no samples collected", file=sys.stderr)
        return 3
    print_summary(stats, out_path)
    return 0


if __name__ == "__main__":
    sys.exit(main())
