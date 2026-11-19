# T7C — Per-app energy: how much each app spends, and where it goes

## Goal
The collector must report energy **per process** and account for the machine's total spend, closing the gap with an explicit residual. iStat Menus shows per-app energy live and then throws it away — its history DB has nine metric tables and **zero** process dimension. Storing it is the product difference.

## Do this
1. Per PID per tick, call `proc_pid_rusage(pid, RUSAGE_INFO_V6, (rusage_info_t *)&ri)`.
   **Verify every field offset from the SDK header before writing code** — do not trust this brief for layout:
   ```
   xcrun --show-sdk-path     # then read usr/include/libproc.h
   grep -n "rusage_info_v6" -A 40 "$(xcrun --show-sdk-path)/usr/include/libproc.h"
   ```
   Fields of interest (names per header, whichever exist): `ri_energy_nj`, `ri_gpu_time`, `ri_user_time`, `ri_system_time`, `ri_diskio_bytesread`, `ri_diskio_byteswritten`, `ri_interrupt_wkups`.
   If `RUSAGE_INFO_V6` is rejected on this OS, fall back to `RUSAGE_INFO_V4` and record which one you used in the JSON (`"rusage_version": 4|6`).
2. Per tick, `d_energy_nj = ri_energy_nj - prev`, `watts = d_energy_nj / 1e9 / elapsed_s`.
   Emit per process: `energy_nj_delta`, `watts` (3 dp), `gpu_time_ns_delta`, `energy_share_pct` (of the sum of all processes this tick).
   First sighting → all deltas `null`, never 0.
3. `processes.by_energy` sorted by `watts` desc, capped at 25 entries.
4. Machine-level block, `energy`:
   - `attributed_w` = Σ process watts
   - `unaccounted_w` = `package_w - attributed_w` **only if** package power is actually available
   - `package_w` must come from a real source (SMC/IOReport/powermetrics). **If no source is available, omit `package_w` and `unaccounted_w` entirely and set `"attribution": "processes_only"`.** Never fabricate a machine total to make the numbers close.
5. `perm: false` for PIDs we cannot read (root-owned). They must not silently become 0 W; they are reported as unreadable and counted in `"unreadable_pids"`.

## Required truthfulness rules
- Sum of process watts may exceed package watts (billing vs measurement) — that is a documented condition, not an error to fudge. Report `over_attribution_pct` when it happens.
- No sampling of `powermetrics` per tick (it is a root daemon: expensive, and exactly what we are beating). At most one `powermetrics -n 1` probe **at startup**, cached, optional.
- No subprocess per sample. No new heavyweight crates.

## Constraints
- Do not touch `dashboard/`, `briefs/`, `GOAL.md`, `LEDGER.jsonl`, `tools/`.
- Keep every existing JSON field. Additive only.
- `--json` must still complete in < 1 s wall for 400+ processes.

## Acceptance commands (all must exit 0)
```
xcrun --show-sdk-path >/dev/null; echo SDK_OK
grep -n "ri_energy_nj\|ri_gpu_time" "$(xcrun --show-sdk-path)/usr/include/libproc.h" | head -5
cargo build --release 2>&1 | tail -5
./target/release/syswatch --json > /tmp/t7c.json; python3 -m json.tool /tmp/t7c.json > /dev/null && echo JSON_OK
python3 - <<'PY'
import json
d = json.load(open('/tmp/t7c.json'))
e = d.get('energy')
assert e, 'no energy block'
assert 'rusage_version' in e, 'must record which rusage version was used'
assert e.get('attribution') in ('processes_only', 'closed', 'over'), e.get('attribution')
by = d['processes']['by_energy']
assert by, 'no by_energy'
tops = [p for p in by if p.get('watts') is not None]
assert tops, 'every watts value is null'
assert tops == sorted(tops, key=lambda p: p['watts'], reverse=True), 'not sorted desc'
print('rusage', e['rusage_version'], 'attribution', e['attribution'])
print('attributed_w', e.get('attributed_w'), 'unreadable_pids', e.get('unreadable_pids'))
for p in tops[:5]:
    print(f"  {p.get('name')[:28]:28s} {p['watts']:7.3f} W  share {p.get('energy_share_pct')}%")
if e.get('package_w') is None:
    assert 'unaccounted_w' not in e or e['unaccounted_w'] is None, 'must not invent a residual without package power'
print('ENERGY_SHAPE_OK')
PY
# energy must be a delta measurement, not a lifetime total: two samples differ
sleep 6
./target/release/syswatch --json > /tmp/t7c2.json
python3 - <<'PY'
import json
a = {p['pid']: p.get('energy_nj_delta') for p in json.load(open('/tmp/t7c.json'))['processes']['by_energy'] if p.get('energy_nj_delta') is not None}
b = {p['pid']: p.get('energy_nj_delta') for p in json.load(open('/tmp/t7c2.json'))['processes']['by_energy'] if p.get('energy_nj_delta') is not None}
common = [p for p in a if p in b]
assert common, 'no comparable pids'
moved = sum(1 for p in common if a[p] != b[p])
print(f'compared {len(common)} pids, {moved} changed -> delta measurement OK')
assert moved > 0, 'identical across samples = lifetime totals, not a delta'
PY
/usr/bin/time -p ./target/release/syswatch --json > /dev/null
```

## Report
Paste commands and real output, **plus** the `libproc.h` grep lines proving the field names and the arithmetic you used. If per-process energy is unavailable on this OS build, say so plainly and report what the probe returned — an honest gap beats a fabricated number. **Do not claim success.**
