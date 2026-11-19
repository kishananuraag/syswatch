# T7B — Instantaneous per-process CPU (kill the lifetime-average lie)

## Goal
`syswatch --json` must report **instantaneous** per-process CPU: the share of one core consumed during the sample window. Today the number is a `ps`-style lifetime average, which is why a process that spun for 3 hours yesterday still looks hot today.

## Why it matters
This is the difference between "what is burning CPU now" and "what has burned CPU since boot". Every downstream graph, alert and the energy attribution in T7C depends on it being an interval measurement.

## Do this
1. In the collector, per tick, for each PID:
   - `proc_pidinfo(pid, PROC_PIDTASKINFO, 0, &ti, sizeof ti)` → `struct proc_taskinfo`
   - key fields: `pti_total_user` and `pti_total_system` (nanoseconds), `pti_resident_size`, `pti_threadnum`
2. Keep the previous tick's `(user+sys)` per PID. `cpu_pct = 100.0 * delta_ns / (1e9 * elapsed_s)`. **100.0 = one full core** (so an 8-thread spinner reads ~800). Document the scale in `--help`.
3. First sighting of a PID (no previous sample) → `cpu_pct: null`, never 0.0 and never a guess.
4. PIDs we may not inspect (root-owned, kernel) → `cpu_pct: null` with `"perm": false`. Do not substitute the old lifetime average silently.
5. Sort `processes.by_cpu` by `cpu_pct` desc; drop entries whose `cpu_pct` is null from that list but keep them in `process_count`.
6. Bound the map: evict PIDs not seen for 10 ticks (no unbounded growth — that is O8).

## Constraints
- **No new heavyweight crates.** `libc` FFI to libproc is expected; `sysinfo` may stay for other fields but must not be the CPU source.
- No subprocesses (`ps`, `top`, `powermetrics`) anywhere in the sampling path.
- Do not change existing JSON field names other than adding to the process objects; the tray depends on the current shape.
- Do not touch `dashboard/`, `briefs/`, `GOAL.md`, `LEDGER.jsonl`, `tools/`.

## Acceptance commands (all must exit 0)
```
cargo build --release 2>&1 | tail -5
./target/release/syswatch --json > /tmp/t7b.json; python3 -m json.tool /tmp/t7b.json > /dev/null && echo JSON_OK
python3 - <<'PY'
import json
d = json.load(open('/tmp/t7b.json'))
procs = d['processes']['by_cpu']
assert procs, 'no processes'
top = procs[0]
assert 'cpu_pct' in top, sorted(top)
vals = [p['cpu_pct'] for p in procs if p.get('cpu_pct') is not None]
assert vals and max(vals) <= 800.0, max(vals) if vals else None
assert vals == sorted(vals, reverse=True), 'not sorted desc'
print('TOP', top.get('pid'), top.get('name'), top['cpu_pct'])
print('NULLS', sum(1 for p in procs if p.get('cpu_pct') is None))
print('SHAPE_OK')
PY
# instantaneous means it moves: two samples 5s apart must differ for a busy pid
./target/release/syswatch --json | python3 -c "import json,sys;d=json.load(sys.stdin);print(d['processes']['by_cpu'][0]['pid'])" > /tmp/t7b_pid
sleep 5
./target/release/syswatch --json > /tmp/t7b2.json
python3 - <<'PY'
import json
a = {p['pid']: p.get('cpu_pct') for p in json.load(open('/tmp/t7b.json'))['processes']['by_cpu']}
b = {p['pid']: p.get('cpu_pct') for p in json.load(open('/tmp/t7b2.json'))['processes']['by_cpu']}
common = [pid for pid in a if pid in b and a[pid] is not None and b[pid] is not None]
assert common, 'no comparable pids'
diff = sum(1 for pid in common if abs(a[pid]-b[pid]) > 0.5)
print(f'compared {len(common)} pids, {diff} moved > 0.5 -> instantaneous OK')
assert diff > 0, 'numbers identical across samples = still a lifetime average'
PY
python3 scripts/measure_collector.py --seconds 60 --interval 1 --out /tmp/t7b_measure.csv
python3 - <<'PY'
import csv, statistics as st
rows = list(csv.DictReader(open('/tmp/t7b_measure.csv')))
cpu = [float(r['cpu_pct']) for r in rows if r.get('cpu_pct') not in (None, '')]
rss = [float(r['rss_kb']) for r in rows if r.get('rss_kb') not in (None, '')]
mc, mr = st.mean(cpu), st.mean(rss)
print(f'collector cpu {mc:.2f}%  rss {mr/1024:.1f} MB')
assert mc < 3.16, f'regression vs T2 baseline 3.16%: {mc:.2f}%'
assert mr < 20480, f'rss above 20 MB: {mr/1024:.1f} MB'
print('COST_OK')
PY
```

## Report
Paste every command and its real output, including the two `TOP` lines proving the number moved between samples. **Do not claim success.**
