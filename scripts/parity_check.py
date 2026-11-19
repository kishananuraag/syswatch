#!/usr/bin/env python3
"""T5 parity checker — compare the iStat fixture with a live syswatch snapshot.

Usage:
    python3 scripts/parity_check.py --fixture fixtures/istat/sample.jsonl
    python3 scripts/parity_check.py --fixture fixtures/istat/sample.jsonl \
        --json /tmp/syswatch_snapshot.json --out docs/parity.md

When --json is omitted the checker still emits the parity table, but every
comparable row is marked UNTESTED because no live comparison was run.
"""
from __future__ import annotations

import argparse
import json
import math
import sys
from datetime import datetime, timezone
from pathlib import Path

IST = timezone.utc  # fixture timestamps are epoch; we only compare them.

TOLERANCES = {
    "cpu": ("abs", 3.0),          # ±3 % absolute
    "load": ("abs", 0.05),        # ±0.05 absolute
    "memory": ("abs", 64.0),      # ±64 MB per component (or total)
    "swap": ("abs", 0.05),        # ±0.05 GB absolute
    "gpu": ("abs", 5.0),          # ±5 % absolute
    "net": ("rel", 0.10),         # ±10 % relative
    "disk": ("rel", 0.15),        # ±15 % relative
}


def load_fixture(path: str) -> list[dict]:
    p = Path(path)
    if not p.is_absolute():
        p = Path(__file__).resolve().parent.parent / p
    rows = []
    with p.open("r", encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            rows.append(json.loads(line))
    rows.sort(key=lambda r: r.get("ts", 0))
    return rows


def load_live(path: str | None) -> dict | None:
    if not path:
        return None
    p = Path(path)
    with p.open("r", encoding="utf-8") as fh:
        return json.load(fh)


def live_ts(snap: dict) -> int | None:
    """Best-effort epoch timestamp from a live snapshot."""
    ts = snap.get("timestamp")
    if isinstance(ts, str):
        try:
            return int(datetime.fromisoformat(ts.replace("Z", "+00:00")).timestamp())
        except ValueError:
            pass
    if isinstance(ts, (int, float)):
        return int(ts)
    ts = snap.get("ts")
    if isinstance(ts, (int, float)):
        return int(ts)
    if isinstance(ts, str):
        try:
            return int(datetime.fromisoformat(ts.replace("Z", "+00:00")).timestamp())
        except ValueError:
            pass
    return None


def nearest_fixture(fixture: list[dict], epoch: int, window: int = 90) -> dict | None:
    best = None
    best_delta = None
    for rec in fixture:
        delta = abs(rec.get("ts", 0) - epoch)
        if delta > window:
            continue
        if best is None or delta < best_delta:
            best = rec
            best_delta = delta
    return best


def fixture_cpu_total(rec: dict) -> float | None:
    cpu = rec.get("cpu") or {}
    total = cpu.get("total")
    if total is None:
        user = cpu.get("user")
        sys = cpu.get("system")
        if user is not None and sys is not None:
            total = user + sys
    return total


def live_cpu_total(snap: dict) -> float | None:
    cpu = snap.get("cpu") if isinstance(snap.get("cpu"), dict) else None
    if cpu is not None:
        return cpu.get("total_pct")
    # older flat schema
    return snap.get("cpu_pct")


def compare_abs(a: float | None, b: float | None, tol: float) -> tuple[bool, str]:
    if a is None or b is None:
        return False, "missing value"
    diff = abs(a - b)
    ok = diff <= tol
    return ok, f"{a:.2f} vs {b:.2f} (diff {diff:.2f}, tol ±{tol})"


def compare_rel(a: float | None, b: float | None, tol: float) -> tuple[bool, str]:
    if a is None or b is None:
        return False, "missing value"
    if a == 0 and b == 0:
        return True, "both zero"
    if a == 0 or b == 0:
        return False, f"{a:.3f} vs {b:.3f} (one side zero)"
    rel = abs(a - b) / max(abs(a), abs(b))
    ok = rel <= tol
    return ok, f"{a:.3f} vs {b:.3f} (rel diff {rel:.1%}, tol ±{tol:.0%})"


def check_pair(a: float | None, b: float | None, kind: str, tol: float) -> tuple[bool, str]:
    if kind == "abs":
        return compare_abs(a, b, tol)
    return compare_rel(a, b, tol)


def build_verdicts(fixture: list[dict], live: dict | None) -> dict[str, tuple[str, str]]:
    """Return metric -> (status, detail)."""
    rec = None
    if live is not None:
        epoch = live_ts(live)
        if epoch is not None:
            rec = nearest_fixture(fixture, epoch)

    v: dict[str, tuple[str, str]] = {}

    # CPU total %
    if rec is None or live is None:
        v["cpu"] = ("UNTESTED", "no live snapshot / no matching tick")
    else:
        a = fixture_cpu_total(rec)
        b = live_cpu_total(live)
        ok, detail = check_pair(a, b, *TOLERANCES["cpu"])
        v["cpu"] = ("PASS" if ok else "FAIL", detail)

    # Load
    if rec is None or live is None:
        v["load"] = ("UNTESTED", "no live snapshot / no matching tick")
    else:
        a = (rec.get("load") or {}).get("one")
        b = live.get("load") if isinstance(live.get("load"), (int, float)) else None
        ok, detail = check_pair(a, b, *TOLERANCES["load"])
        v["load"] = ("PASS" if ok else "FAIL", detail)

    # Memory breakdown
    if rec is None or live is None:
        v["memory"] = ("UNTESTED", "no live snapshot / no matching tick")
    else:
        m = rec.get("mem") or {}
        fix_used = (m.get("wired_mb") or 0) + (m.get("app_mb") or 0) + (m.get("compressed_mb") or 0)
        mem = live.get("memory") or live.get("ram")
        if isinstance(mem, dict):
            used_bytes = mem.get("used_bytes")
            live_used = used_bytes / 1_048_576 if used_bytes is not None else mem.get("used_mb")
            if live_used is None:
                live_used = mem.get("pct")  # fallback, less meaningful
        else:
            live_used = live.get("ram_used_mb")
        ok, detail = check_pair(fix_used, live_used, *TOLERANCES["memory"])
        v["memory"] = ("PASS" if ok else "FAIL", detail)

    # Swap
    if rec is None or live is None:
        v["swap"] = ("UNTESTED", "no live snapshot / no matching tick")
    else:
        a = rec.get("swap_gb")
        mem = live.get("memory") or live.get("ram")
        if isinstance(mem, dict):
            swap_bytes = mem.get("swap_used_bytes")
            b = swap_bytes / 1_073_741_824 if swap_bytes is not None else None
            if b is None:
                b = mem.get("swap_used_gb")
        else:
            b = None
        ok, detail = check_pair(a, b, *TOLERANCES["swap"])
        v["swap"] = ("PASS" if ok else "FAIL", detail)

    # GPU
    if rec is None or live is None:
        v["gpu"] = ("UNTESTED", "no live snapshot / no matching tick")
    else:
        gpus = rec.get("gpu") or []
        a = sum(g.get("proc_pct", 0) for g in gpus) / max(1, len(gpus)) if gpus else None
        b = live.get("gpu_util_pct")
        ok, detail = check_pair(a, b, *TOLERANCES["gpu"])
        v["gpu"] = ("PASS" if ok else "FAIL", detail)

    # Network
    if rec is None or live is None:
        v["net"] = ("UNTESTED", "no live snapshot / no matching tick")
    else:
        nets = rec.get("net") or []
        fix_down = sum(n.get("down_mbps", 0) for n in nets)
        fix_up = sum(n.get("up_mbps", 0) for n in nets)
        # Live may expose bps rates or cumulative KB counters.
        live_nets = live.get("networks") or live.get("net") or []
        if isinstance(live_nets, list) and live_nets and ("rx_bps" in live_nets[0] or "tx_bps" in live_nets[0]):
            live_down = sum(n.get("rx_bps", 0) for n in live_nets) / 1_000_000
            live_up = sum(n.get("tx_bps", 0) for n in live_nets) / 1_000_000
            ok_down, d_down = check_pair(fix_down, live_down, *TOLERANCES["net"])
            ok_up, d_up = check_pair(fix_up, live_up, *TOLERANCES["net"])
            ok = ok_down and ok_up
            detail = f"down: {d_down}; up: {d_up}"
        else:
            ok = False
            detail = "live snapshot has no bps rates (only cumulative counters or missing)"
        v["net"] = ("PASS" if ok else "FAIL" if live_nets else "UNTESTED", detail)

    # Disk
    if rec is None or live is None:
        v["disk"] = ("UNTESTED", "no live snapshot / no matching tick")
    else:
        disks = rec.get("disk") or []
        fix_read = sum(d.get("read_kbps", 0) for d in disks)
        fix_write = sum(d.get("write_kbps", 0) for d in disks)
        live_disk = live.get("disk_rate") or live.get("disk")
        if isinstance(live_disk, dict) and ("read_kbps" in live_disk or "write_kbps" in live_disk):
            live_read = live_disk.get("read_kbps")
            live_write = live_disk.get("write_kbps")
            ok_r, d_r = check_pair(fix_read, live_read, *TOLERANCES["disk"])
            ok_w, d_w = check_pair(fix_write, live_write, *TOLERANCES["disk"])
            ok = ok_r and ok_w
            detail = f"read: {d_r}; write: {d_w}"
        else:
            ok = False
            detail = "live snapshot has no KB/s disk rates"
        v["disk"] = ("PASS" if ok else "FAIL" if live_disk else "UNTESTED", detail)

    # Per-process capabilities are new, not stored by iStat.
    v["proc_cpu"] = ("NEW", "syswatch capability; no iStat equivalent")
    v["proc_energy"] = ("NEW", "syswatch capability; no iStat equivalent")

    # Temperatures / fans: iStat stores unnamed md5 keys; syswatch has no named
    # mapping proven by value range, so this stays a hypothesis / untested.
    v["sensors"] = ("UNTESTED", "unnamed md5 keys; no proven sensor mapping")

    return v


def render_markdown(v: dict[str, tuple[str, str]]) -> str:
    rows = [
        ("CPU total %", "`cpu.user + cpu.system`", "`cpu.total_pct`", "±3 % abs, same tick", v["cpu"]),
        ("Load", "`load.one`", "`load.one`", "±0.05", v["load"]),
        ("Memory wired/app/compressed", "`memory.*`", "`memory.*`", "±64 MB", v["memory"]),
        ("Swap", "`swap.swap` (GB)", "`memory.swap_used`", "±0.05 GB", v["swap"]),
        ("GPU %", "`gpu.processor`", "`gpu.util_pct`", "±5 % abs", v["gpu"]),
        ("Net rx/tx", "`network.*`", "`network[*].rx_bps`", "±10 % rel", v["net"]),
        ("Disk R/W", "`diskactivity.*`", "`disk.rate`", "±15 % rel", v["disk"]),
        ("Per-proc CPU", "**not stored by iStat**", "`processes.by_cpu`", "n/a — new capability", v["proc_cpu"]),
        ("Per-proc energy", "**not stored by iStat**", "`processes.by_energy`", "n/a — new capability", v["proc_energy"]),
        ("Temp / fan", "`sensors.*` (unnamed keys)", "TBD", "—", v["sensors"]),
    ]
    lines = [
        "# syswatch / iStat parity matrix",
        "",
        "Generated by `scripts/parity_check.py`. A green `PASS` means the nearest",
        "fixture tick within 90 s matched the live snapshot inside the stated tolerance.",
        "`UNTESTED` is an honest statement that no real comparison was run for that row.",
        "",
        "| Metric | iStat source (table.column) | syswatch source | Tolerance | Status |",
        "|---|---|---|---|---|",
    ]
    for metric, istat_src, sw_src, tol, (status, detail) in rows:
        lines.append(f"| {metric} | {istat_src} | {sw_src} | {tol} | **{status}** |")
    lines.append("")
    lines.append("## Details")
    lines.append("")
    for metric, _, _, _, (status, detail) in rows:
        lines.append(f"- **{metric}**: {status} — {detail}")
    lines.append("")
    return "\n".join(lines)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--fixture", required=True, help="path to iStat fixture JSONL")
    ap.add_argument("--json", default=None, help="path to live syswatch --json snapshot")
    ap.add_argument("--out", default=None, help="output markdown file (default stdout)")
    args = ap.parse_args()

    fixture = load_fixture(args.fixture)
    live = load_live(args.json)
    verdicts = build_verdicts(fixture, live)
    md = render_markdown(verdicts)

    if args.out:
        Path(args.out).write_text(md, encoding="utf-8")
    else:
        sys.stdout.write(md)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
