# syswatch / iStat parity matrix

This table states, metric by metric, where the syswatch number comes from and
whether it has been compared against the iStat fixture (`fixtures/istat/sample.jsonl`)
within the stated tolerance. `UNTESTED` is a legitimate value; a fabricated green
is not.

Run the checker with a live snapshot to update the Status column:

```bash
python3 scripts/parity_check.py \
  --fixture fixtures/istat/sample.jsonl \
  --json /tmp/syswatch_snapshot.json \
  --out docs/parity.md
```

| Metric | iStat source (table.column) | syswatch source | Tolerance | Status |
|---|---|---|---|---|
| CPU total % | `cpu.user + cpu.system` | `cpu.total_pct` | ±3 % abs, same tick | **UNTESTED** |
| Load | `load.one` | `load.one` | ±0.05 | **UNTESTED** |
| Memory wired/app/compressed | `memory.*` | `memory.*` | ±64 MB | **UNTESTED** |
| Swap | `swap.swap` (GB) | `memory.swap_used` | ±0.05 GB | **UNTESTED** |
| GPU % | `gpu.processor` | `gpu.util_pct` | ±5 % abs | **UNTESTED** |
| Net rx/tx | `network.*` | `network[*].rx_bps` | ±10 % rel | **UNTESTED** |
| Disk R/W | `diskactivity.*` | `disk.rate` | ±15 % rel | **UNTESTED** |
| Per-proc CPU | **not stored by iStat** | `processes.by_cpu` | n/a — new capability | **NEW** |
| Per-proc energy | **not stored by iStat** | `processes.by_energy` | n/a — new capability | **NEW** |
| Temp / fan | `sensors.*` (unnamed keys) | TBD | — | **UNTESTED** |

## Notes

- **CPU**: iStat stores `user` and `system` as percentages of total machine
  capacity. syswatch `cpu.total_pct` is the same quantity.
- **Memory**: iStat breaks RAM into `wired`, `app`, and `compressed` (all MB).
  syswatch currently reports `used_bytes`/`pct`; a component-level breakdown is
  not yet emitted, so this row compares total used memory.
- **Swap**: iStat reports GB; syswatch reports bytes.
- **Network**: iStat stores per-interface `upload`/`download` in MB/s. syswatch
  stores per-interface `rx_bps`/`tx_bps`. The checker sums interfaces before
  comparing.
- **Disk**: iStat stores per-device `read`/`write` in KB/s. syswatch does not
  yet emit a KB/s rate in the `--json` snapshot, so this row is untested.
- **Per-process CPU / energy**: iStat Menus 7 does not store per-process
  samples in the history database. These are new syswatch capabilities.
- **Temperatures / fans**: iStat stores raw SMC/IOHID values under unnamed md5
  keys. Any mapping from those keys to named sensors is a hypothesis that must
  be proven by value range before it can be marked as fact.
