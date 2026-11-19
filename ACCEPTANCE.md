# ACCEPTANCE.md — the commands that decide whether syswatch is done

Referred to by `GOAL.md`. **A dimension is green only when its commands exit 0 and the numbers are pasted.** No prose counts as evidence; no agent's "done" counts as evidence.

Run everything from the repo root on the Mac. `python3` here means the psutil-enabled interpreter noted in `.hermes/environment.md`; every script below tolerates a plain `python3` for stdlib-only work.

---

## A1 — Install: one command, fresh shell works

```bash
bash scripts/install.sh
zsh -lic 'command -v syswatch && syswatch --version'
launchctl print gui/$UID/com.barbarian.syswatch | grep "state = running"
```
Green when: the install is idempotent (running it twice is not an error), `syswatch` resolves in a **new** login shell, and launchd reports `state = running`.

## A2 — Surfaces: menu-bar app + CLI + JSON API

```bash
ls -lh target/release/syswatch target/release/syswatch-tray
./target/release/syswatch --help | head -5
./target/release/syswatch --json | python3 -m json.tool > /dev/null && echo JSON_OK
curl -s -m 5 http://127.0.0.1:8123/api/health | python3 -m json.tool
pgrep -fl syswatch-tray
```
Green when: all three surfaces exist and the API answers on `127.0.0.1:8123` — never on a wildcard address.

## A3 — Metrics present (the iStat set)

```bash
./target/release/syswatch --json > /tmp/a3.json
python3 - <<'PY'
import json
d = json.load(open('/tmp/a3.json'))
need = ['cpu','memory','disks','networks','processes']
missing = [k for k in need if k not in d]
print('missing core blocks:', missing)
assert not missing, missing
cpu = d['cpu']; assert cpu.get('per_core_pct'), 'no per-core CPU'
assert cpu.get('freq_mhz') or cpu.get('freq'), 'no frequency'
mem = d['memory']; assert 'swap_used' in mem and 'used' in mem, sorted(mem)
print('cores', len(cpu['per_core_pct']), '| swap_used', mem['swap_used'])
print('energy block:', 'energy' in d, '| by_energy:', 'by_energy' in d.get('processes', {}))
PY
```
Green when the core blocks are present **and** `processes.by_cpu[].cpu_pct` and `processes.by_energy[].watts` exist (T7B/T7C). Temperature/fan/power are required only where the OS exposes them without root; the parity table (A4) records each one as `PASS` or `UNTESTED — needs root`.

## A4 — Parity with iStat, metric for metric

```bash
test -f docs/parity.md && grep -c '|' docs/parity.md
python3 scripts/parity_check.py --fixture fixtures/istat/sample.jsonl --out /tmp/parity.md
grep -c "PASS" /tmp/parity.md; grep -c "UNTESTED" /tmp/parity.md
```
Green when every row in `docs/parity.md` is `PASS` (with a tolerance and a command behind it) or `UNTESTED — needs root`. **A row that claims PASS without a command is a failure**, and per-process CPU/energy rows are `NEW` (iStat stores neither).

## A5 — Cost: the whole point

```bash
python3 scripts/measure_collector.py --seconds 300 --interval 1 --out /tmp/a5.csv
python3 - <<'PY'
import csv, statistics as st
r = list(csv.DictReader(open('/tmp/a5.csv')))
cpu = [float(x['cpu_pct']) for x in r if x.get('cpu_pct')]
rss = [float(x['rss_kb']) for x in r if x.get('rss_kb')]
ok_json = [x.get('json_ok', '1') for x in r]
print(f"cpu {st.mean(cpu):.2f}%  rss {st.mean(rss)/1024:.1f} MB  samples {len(r)}")
assert st.mean(cpu) < 1.0, f"A5 CPU {st.mean(cpu):.2f}% >= 1%"
assert st.mean(rss) < 15360, f"A5 RSS {st.mean(rss)/1024:.1f} MB >= 15 MB"
print("A5_COST_OK")
PY
```
Green when, over a 5-minute soak: **< 1 % CPU @ 1 s** and **< 15 MB RSS**, with 100 % valid JSON. Blips (single samples) are reported separately, never averaged away silently.

## A6 — Durability: survives a reboot, returns in < 60 s

```bash
bash scripts/install.sh
PID=$(curl -s http://127.0.0.1:8123/api/health | python3 -c "import json,sys;print(json.load(sys.stdin)['pid'])")
T0=$(date +%s); kill -9 $PID; sleep 25
NEW=$(curl -s http://127.0.0.1:8123/api/health | python3 -c "import json,sys;print(json.load(sys.stdin)['pid'])")
test "$NEW" != "$PID" && echo "RESTARTED in $(($(date +%s)-T0))s"
```
Green when the PID changes and the service is answering within 60 s. **A real reboot is Kishan's call**, not an agent step — the kill -9 test is the agent-verifiable half; reboot is confirmed by him and recorded in `LEDGER.jsonl`.

## A7 — Honesty: numbers match the OS within tolerance

```bash
./target/release/syswatch --json > /tmp/a7.json
ps -Ao pid=,%cpu=,rss= | sort -k2 -nr | head -5                     # compare against processes.by_cpu
vm_stat | head -8                                                    # compare against memory
top -l 1 -n 0 | head -4                                              # compare CPU total
netstat -ib | awk 'NR>1 && $7!="" {print $1,$7,$10}' | head -5       # compare network totals
python3 - <<'PY'
import json
d = json.load(open('/tmp/a7.json'))
print('our cpu total', d['cpu'].get('total_pct'))
print('ours top5', [(p.get('name'), p.get('cpu_pct')) for p in d['processes']['by_cpu'][:5]])
PY
```
Green when CPU total is within ±3 % absolute of `top`, per-process CPU ranks match `ps` for the top 5 (±1 rank), memory is within ±64 MB, and network totals are within ±10 % relative.

## A8 — Visibility: opening syswatch shows live agent work

```bash
printf '{"ts":"2026-11-15T18:00:00","job":"A8","kind":"pass","model":"none","attempt":1,"text":"acceptance probe"}\n' >> .hermes/run/events.jsonl
curl -s "http://127.0.0.1:8123/api/agent-events?limit=3" | python3 -c "import json,sys;d=json.load(sys.stdin);print(d[-1]['job']);assert d[-1]['job']=='A8'"
curl -s "http://127.0.0.1:8123/api/queue" | python3 -c "import json,sys;print(len(json.load(sys.stdin).get('tasks',[])))"
```
Green when the dashboard shows the new event within 2 s of it being appended, and the queue pane reflects `.hermes/queue.json` without a reload loop.

## A9 — The autonomy dimension (the token claim)

```bash
python3 tools/driver.py --dry-run
grep -c '"kind":"sleep"\|"kind": "sleep"' .hermes/run/events.jsonl
grep -c '"kind":"wake"\|"kind": "wake"' .hermes/run/events.jsonl
ls -la logs/wake-*-usage.json 2>/dev/null | wc -l
```
Green when a full unattended night finishes **≥ 1 task** with **≤ 6 wakes**, each wake carrying a usage file, and `sleep` events present for the idle stretches. This is the dimension that proves supervision got cheap instead of just moving.

---

## How a dimension flips to green
1. Commands run by the driver (or by Hermes when verifying a claim).
2. Real stdout pasted into `LEDGER.jsonl` as a `verdict` row: `{"job","kind":"verdict","exit","acceptance":"n/m PASS","measured":{...}}`.
3. Only then does `GOAL.md`'s table get a `PASS`.

Anything not measured is `UNTESTED`. There is no third state.
