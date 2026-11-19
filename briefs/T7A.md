# T7A — Optimisation pass O1–O8, each with a before/after number

## Goal
Make the collector and tray cheaper than they are today, proving every change with two numbers produced by the same command. Target: **< 1 % CPU @ 1 s, < 15 MB RSS, flat RAM over 24 h.** Baseline from T2: 3.16 % CPU, 10,198 KB RSS, 100 % JSON validity.

## The eight optimisations (from plan v2 — do only these, in this order)
| # | Change | Measure with |
|---|---|---|
| O1 | Feature-gate the HTTP stack out of TUI/tray builds (`reqwest`/`tokio` behind a feature) | `ls -lh target/release/`, `ps -o rss` |
| O2 | Release profile: `lto="fat"`, `codegen-units=1`, `strip=true`, `panic="abort"` | binary size |
| O3 | Selective `sysinfo` refresh (`ProcessRefreshKind`) — 432 procs/tick is the dominant cost | `scripts/measure_collector.py` 60 s |
| O4 | Differential panel refresh (disks ×5 ticks, procs ×2, CPU/mem ×1) | same harness, syscall count via `sudo fs_usage` if available else tick timing |
| O5 | Sensors on a 5 s background thread, cached, never blocking render | frame-time log |
| O6 | Zero-alloc render loop (reuse buffers, no per-frame `String`) | `%cpu` at 250 ms |
| O7 | Idle-aware redraw (skip when the snapshot is unchanged) | idle `%cpu` floor |
| O8 | Fixed-size ring buffers for history (it grows unbounded today) | 30 min soak, RSS delta |

## Constraints
- Every optimisation is **behind a measurement**: write `docs/perf.md` with a table `step | before | after | command`.
- No behaviour change: every existing JSON field and CLI flag keeps working; `--json` stays valid 100 % of the time.
- No new dependencies unless a removal is the point (O1).
- Do not touch `dashboard/`, `briefs/`, `GOAL.md`, `LEDGER.jsonl`, `tools/`, `scripts/measure_collector.py`.

## Acceptance commands (all must exit 0)
```
cargo build --release 2>&1 | tail -3
ls -lh target/release/syswatch target/release/syswatch-tray
python3 scripts/measure_collector.py --seconds 60 --interval 1 --out /tmp/t7a_before.csv
python3 - <<'PY'
import csv, statistics as st
r = list(csv.DictReader(open('/tmp/t7a_before.csv')))
c = [float(x['cpu_pct']) for x in r if x.get('cpu_pct')]
m = [float(x['rss_kb']) for x in r if x.get('rss_kb')]
print(f"cpu {st.mean(c):.2f}%  rss {st.mean(m)/1024:.1f} MB  n={len(r)}")
assert st.mean(c) < 1.0, f"cpu target < 1%: {st.mean(c):.2f}%"
assert st.mean(m) < 15360, f"rss target < 15 MB: {st.mean(m)/1024:.1f} MB"
print("COST_OK")
PY
./target/release/syswatch --json | python3 -c "import json,sys;d=json.load(sys.stdin);assert d.get('cpu') and d.get('memory');print('JSON_STILL_OK')"
test -f docs/perf.md && grep -c '|' docs/perf.md
```

## Report
Paste the before/after table with the commands that produced each pair. If an optimisation did not help, say so and revert it — a reverted change with a number is a valid result. **Do not claim success.**
