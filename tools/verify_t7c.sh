#!/bin/bash
# SUPERVISOR'S OWN verification for T7C — written and run by me, not by the worker.
# The worker's report is a claim. This is the verdict.
cd ~/Documents/projects/syswatch || exit 1
FAIL=0
ok(){ echo "  PASS  $1"; }
bad(){ echo "  FAIL  $1"; FAIL=$((FAIL+1)); }

echo "== 1. build =="
cargo build --release 2>&1 | grep -E "^error" && bad "build" || ok "builds clean"

echo "== 2. output valid + fast =="
S=$(python3 -c 'import time;print(time.time())')
./target/release/syswatch --json > /tmp/v_t7c.json 2>/dev/null || bad "--json exited non-zero"
E=$(python3 -c 'import time;print(time.time())')
python3 -m json.tool /tmp/v_t7c.json > /dev/null 2>&1 && ok "valid JSON" || bad "invalid JSON"
python3 -c "import sys;w=$E-$S;print(f'  wall {w:.2f}s');sys.exit(0 if w<1.0 else 1)" && ok "wall < 1s" || bad "wall >= 1s"

echo "== 3. energy block =="
python3 - <<'PY' || FAIL=$((FAIL+1))
import json,sys
d=json.load(open('/tmp/v_t7c.json'))
e=d.get('energy')
if not e: print('  FAIL  no energy block'); sys.exit(1)
print('  rusage_version', e.get('rusage_version'), '| attribution', e.get('attribution'),
      '| attributed_w', e.get('attributed_w'), '| package_w', e.get('package_w'),
      '| unaccounted_w', e.get('unaccounted_w'))
bad=[]
if e.get('rusage_version') is None: bad.append('rusage_version missing')
if e.get('attribution') not in ('processes_only','closed','over'): bad.append(f"attribution={e.get('attribution')}")
if e.get('package_w') is None and e.get('unaccounted_w') not in (None,): bad.append('unaccounted_w set without package_w')
if bad: print('  FAIL  ' + '; '.join(bad)); sys.exit(1)
print('  PASS  energy block truthful')
PY

echo "== 4. by_energy shape =="
python3 - <<'PY' || FAIL=$((FAIL+1))
import json,sys
d=json.load(open('/tmp/v_t7c.json'))
by=d['processes'].get('by_energy') or []
if not by: print('  FAIL  by_energy empty'); sys.exit(1)
tops=[p for p in by if p.get('watts') is not None]
if not tops: print('  FAIL  every watts null'); sys.exit(1)
if tops!=sorted(tops,key=lambda p:p['watts'],reverse=True): print('  FAIL  not sorted desc'); sys.exit(1)
if any(p.get('gpu_time_ns_delta') is not None for p in by):
    print('  FAIL  gpu_time_ns_delta is populated but ri_gpu_time does not exist in this SDK -> fabricated'); sys.exit(1)
print(f'  {len(by)} rows, {len(tops)} with watts, sorted desc, gpu all null')
for p in tops[:8]:
    print(f"    {str(p.get('name'))[:26]:26s} {p['watts']:7.3f} W  pid {p.get('pid')}")
print('  PASS  by_energy shape')
PY

echo "== 5. delta, not lifetime total =="
sleep 6
./target/release/syswatch --json > /tmp/v_t7c2.json 2>/dev/null
python3 - <<'PY' || FAIL=$((FAIL+1))
import json,sys
a={p['pid']:p.get('energy_nj_delta') for p in json.load(open('/tmp/v_t7c.json'))['processes']['by_energy']}
b={p['pid']:p.get('energy_nj_delta') for p in json.load(open('/tmp/v_t7c2.json'))['processes']['by_energy']}
c=[p for p in a if p in b]
if not c: print('  FAIL  no comparable pids'); sys.exit(1)
moved=sum(1 for p in c if a[p]!=b[p])
print(f'  compared {len(c)} pids, {moved} changed')
if moved==0: print('  FAIL  identical across samples = lifetime totals'); sys.exit(1)
print('  PASS  values are deltas')
PY

echo "== 6. collector cost (pty harness, no agent involved) =="
bash /Users/barbarian/.hermes/profiles/work/cache/scratch/measure_pty.sh 2>/dev/null | grep -E "cpu time|rss"

echo
[ $FAIL -eq 0 ] && echo "VERDICT: T7C PASSES SUPERVISOR ACCEPTANCE" || echo "VERDICT: T7C FAILS — $FAIL check(s)"
exit $FAIL
