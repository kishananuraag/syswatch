# T18 — collector storage cascade (disk-side down-sampling)

Status: **implemented and deployed** to the user LaunchAgent
`com.barbarian.syswatch.collector` on 2026-11-19 10:12 IST.
Acceptance output: `reports/T18.md`.

## Measured starting point (this machine, this repo, before the change)

    ~/Library/Application Support/syswatch/logs/
      2026-11-15.jsonl     402,388 B /     59 lines
      2026-11-18.jsonl  97,507,590 B / 14,455 lines = 6,745 B/line
      2026-11-19.jsonl 102,633,317 B / 15,239 lines = 6,735 B/line
      total 202.5 MB after 2 days of collection

**6.9 KB per 1 s sample → 24.9 MB/h, 597 MB/day, 218 GB/year.**
82% of the line is `processes.by_cpu/by_mem/by_energy` (6,065 of 7,438 B).

Note the brief's "6 h of 1 s data ≈ 24 MB" assumed ~1.1 KB/sample. At the
measured 6.9 KB/sample, 6 h is 149 MB, which on its own breaks the 100 MB
acceptance bound — so the raw buffer is capped by **bytes** as well as by age.
That byte cap is what binds on this machine.

## Layout on disk

    logs/
      YYYY-MM-DD.jsonl            hard link -> that day's finest tier file
      raw/YYYY-MM-DDTHH.jsonl     1 s raw, hourly files, append-only
      tier15s/YYYY-MM-DD.jsonl    15 s buckets   (demoted at 1 d)
      tier60s/YYYY-MM-DD.jsonl    60 s buckets   (demoted at 7 d)
      tier300s/YYYY-MM-DD.jsonl   300 s buckets  (demoted at 30 d)
      tier1800s/YYYY-MM-DD.jsonl  1800 s buckets (demoted at 1 y)
      archive/YYYY-MM-DD.jsonl    1 merged record/day, kept forever
      state/                      reserved (the compactor is stateless)

### Why the day file is a hard link

`dashboard/server.py` discovers collector files with
`os.listdir(COLLECTOR_LOGS)` filtered on `.jsonl` — **non-recursive** — and the
dashboard is out of scope. If the day files existed only in `tier*/`, the
dashboard would find zero files and `/api/graph` would lose every collector
point. So the log root keeps one name per day, hard-linked to that day's finest
tier file: same inode, so `du` counts the bytes once, and it is swapped
atomically (link to a temp name, then `rename` over the day name) as the day is
demoted. If `hard_link` fails (foreign fs / Windows) it falls back to a copy.

`raw/` is deliberately **not** linked. 1 s data would flood the dashboard's
120 k-point ring and evict real history for no visual gain, because
`bucketize()` caps every range at 500 points anyway.

The 15 s tier is written on the tick path (one line every 15 s), so the day
link is current within 15 s. That is also what makes the migration safe: every
sample lands in `tier15s` whether or not it is still inside the raw window.

## Record schema

Identical top-level keys to `Snapshot`; every continuous metric keeps a plain
number at its old key (the bucket mean) and gains a `<key>_stat` sibling
`{min,max,mean,n}`. That is what makes the change invisible to `src/stats.rs`,
`src/ui.rs` (history view) and the dashboard's `HistoryIndex._parse`, which all
index scalars.

```json
{"timestamp":"2026-11-19T09:43:15Z","uptime_secs":140233,
 "cpu":{"total_pct":27.804,"per_core_pct":[],"freq_mhz":3504,
        "total_pct_stat":{"min":18.848,"max":43.987,"mean":27.804,"n":15}},
 "memory":{"used_bytes":15100000000,"total_bytes":17100000000,"pct":88.1,
           "swap_used_bytes":0,"swap_total_bytes":0,
           "pct_stat":{...}},
 "temperature":52.0,"temperature_stat":{...},
 "disks":[{"mount":"/","fs":"apfs","used_pct":58.123,"free_gb":95.593,
           "total_gb":228.274,"used_pct_stat":{...},"free_gb_stat":{...}}, ...],
 "networks":[{"name":"en0","rx_bps":25710.4,"tx_bps":173056.9,
              "rx_bps_stat":{...},"tx_bps_stat":{...}}, ...],
 "energy":{"rusage_version":6,"attribution":"processes_only","source":"...",
           "window_s":5.08,"attributed_w":0.091,"attributed_w_stat":{...},
           "processes_total":483,"processes_measured":292,"unreadable_pids":191,
           "counter_resets":0,"zero_energy_pids":0,
           "gpu_time":"unavailable_no_rusage_field",
           "units":{"energy":"nJ","power":"W","share":"%"}},
 "process_count":512,
 "agg":{"tier":15,"start":"2026-11-19T09:43:15Z","n":15}}
```

Deliberate omissions, all of which `Snapshot` accepts after this change:

| omitted | why |
|---|---|
| `processes` | 82% of a raw line, nothing reads it back. `Snapshot::processes` gained `#[serde(default)]` so the history view still parses these lines. |
| `per_core_pct` | written as `[]`; no consumer reads it from disk. |
| `energy.note` | 232 B of identical explanation per record (13 MB/year). It stays on the raw lines. |
| per-metric `*_stat` | only in the 15 s tier. Coarser tiers keep `{min,max,mean,n}` for cpu / memory % / temperature and plain means elsewhere — a year at 1800 s with full per-interface spreads is 40 MB on its own. |

Weighting: bucket means are merged by each statistic's own `n`, so
`mean' = (mean1·n1 + mean2·n2)/(n1+n2)`, `min' = min`, `max' = max`. Verified
in `src/storage.rs::tests::bucket_merge_matches_flat_mean`.

Network interfaces that are absent from a sample count as **zero throughput**,
not as "no sample". Without that, summing per-interface means overstates the
`down_bps` chart whenever an interface (e.g. a utun) flaps — measured 7.7% high
before the fix, 0.002% after.

## Compaction

`compact(dry_run)` is **stateless**: every decision is
`bucket_start < now − tier_retention`, so a second pass over already-compacted
data genuinely finds nothing instead of trusting a watermark.

| step | source | cutoff | destination |
|---|---|---|---|
| raw prune | `raw/*.jsonl` | `now − raw_hours` **or** `raw_max_bytes` | compacted into `tier15s` first if not already covered, then deleted |
| 1 | `tier15s` | `now − 1 d` | `tier60s` |
| 2 | `tier60s` | `now − 7 d` | `tier300s` |
| 3 | `tier300s` | `now − 30 d` | `tier1800s` |
| 4 | `tier1800s` | `now − 1 y` | `archive` (1 record/day) |
| 5 | day links | always | re-point at the finest tier file |

Every file rewrite is `write .tmp-<name>` → `fsync` → `rename` → `fsync(dir)`.
A source file is unlinked only after the destination's `fsync` returned, so a
crash at the worst moment leaves one duplicate bucket, which the next pass
dedupes by `(bucket_start, tier)`.

Two live-writer guards (a `--compact` run may happen while the collector runs):
the current day's file is never rewritten, and the current hour's raw file is
never pruned.

Runs on startup and every hour from the collector tick loop.

## Config (`~/.config/syswatch/config.toml`)

```toml
[storage]
cascade = true      # false = pre-T18 one-raw-file-per-day layout
raw_hours = 6       # age cap for raw 1 s data
raw_max_mb = 24     # byte cap; the binding one on this machine
archive = true      # write the 1 record/day tail
```

## Measured footprint (real records, not estimates)

| tier | records | B/record | size |
|---|---|---|---|
| raw (byte-capped) | ~3,500 | 6,943 | 24 MB |
| tier15s (1 d) | 5,760 | 2,410 | 13.9 MB |
| tier60s (6 d) | 8,640 | 1,308 | 11.3 MB |
| tier300s (23 d) | 6,624 | 1,308 | 8.7 MB |
| tier1800s (335 d) | 16,080 | 1,308 | 21.0 MB |
| archive (365 d) | 365 | 1,308 | 0.5 MB |
| **steady state** | | | **≈79 MB** |

Measured on the day of the change: 202.5 MB of pre-T18 files became **23 MB**.
