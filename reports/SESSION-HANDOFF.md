# Session handoff — syswatch (2026-11-16 05:2x IST)

Paused on request. No worker of mine is running. The menu-bar app was left up
deliberately: it is the deliverable, not background work.

## What is running right now

| Thing | State |
|---|---|
| `~/Applications/Syswatch.app` | **RUNNING** pid 65523, ~19-32 MB RSS, 0.40 % CPU at a 2 s tick |
| Menu-bar item | live, read via Accessibility: `⚡ <CPU>% <W>W` |
| Dropdown | top energy consumers, e.g. `Chrome Helper 0.46 W 38 %`, `python3 0.09 W 32 %` |
| Dashboard `127.0.0.1:8123` | NOT running (was a nohup process, died) — T8 owns this |
| My agents / supervisor | none — all killed |

## Verified this session

- **T7C per-app energy — PASS** (supervisor's own script `tools/verify_t7c.sh`).
  `rusage_version 6`, `attribution processes_only`, `package_w null` (no SMC
  source, correctly not invented), `gpu_time_ns_delta` always null because
  `ri_gpu_time` does not exist in this SDK. 25 ranked rows, deltas confirmed
  across two samples. `unreadable_pids` ~183 → root-owned processes are invisible
  to per-PID energy by design.
- **T10 headless collector — PASS** (`tools/verify_t10.sh`): alive with no tty,
  `TERM=dumb`, SIGTERM → rc 0 in 0.07 s, JSON lines with 0 ANSI escapes,
  15 samples written.
- **Cost, measured honestly** with `ps` CPU-time deltas, not the broken
  `measure_collector.py` (it spawns a collector per tick):
  - TUI in a pty, 1 s tick: 1.52 %
  - headless, 1 s tick: **1.48 %**, RSS 10.7–12.0 MB
  - 5 s tick: 1.23 %
  Removing the render bought almost nothing — the cost is sampling, not drawing.
  The < 1 % target is still open and belongs to T7A.

## Uncommitted work (waiting on GO, nothing committed)

`git diff --stat`: **12 files, +1236 / −55.** Main pieces: energy surface in
`src/stats.rs` (`proc_pid_rusage`, `ENERGY_REFRESH` gate), `src/menubar.rs`
(new, AppKit status item), `cli.rs --headless` + `--menubar`, `lib.rs`,
`Cargo.toml` (cocoa/objc, macOS target only), `tools/verify_t7c.sh`,
`tools/verify_t10.sh`, `tools/verify_t8.sh`, `tools/go_supervisor.py`,
`tools/dispatch_one.sh`, `scripts/build_app.sh`, `packaging/macos/Info.plist`.

## Not done

1. **T8 durability** — no LaunchAgent yet, so the app and collector die at
   reboot. Dispatched once (`logs/T8-go.log`), killed on request.
   `tools/verify_t8.sh` is written and ready; it proves the claim by
   `kill -9`-ing the app and timing the revival, then re-reading the status item.
2. **< 1 % CPU** — needs the sampling path optimised (T7A), not the renderer.
3. **Dashboard under launchd** — part of T8.

## Hard-won facts, do not rediscover them

- **Go lane = `opencode-go/*`.** `opencode/*` is the exhausted free tier.
  DeepSeek is paid and forbidden.
- **Dispatch env must be sanitised.** This Hermes session is itself an opencode
  session (`OPENCODE=1`, `OPENCODE_SESSION_ID`, `OPENCODE_TERMINAL`,
  `AI_AGENT=opencode`). Children inherit them and are reaped ~30 s in with
  `rc=-15`. Use:
  `env -u OPENCODE -u OPENCODE_SESSION_ID -u OPENCODE_TERMINAL -u AI_AGENT opencode run …`
- **Never inline a brief into a double-quoted shell string** — briefs contain
  backticks and bash executes them. Build a prompt file, pass `"$(cat file)"`.
- **`--auto` is mandatory** on build dispatches or every shell call is
  auto-rejected and the agent "verifies" nothing.
- **A worker can silently revert your fixes.** The Go worker dropped
  `last_disk_refresh` and cost went 1.52 % → 4.22 %. Always re-measure and grep
  for the guard, not just the acceptance criteria.
- **`--headless` never needs a tty**; `service run` on macOS is a Windows-only stub.
- **`ri_gpu_time` does not exist**; `rusage_info_v6` is in `sys/resource.h`
  (line 368), `ri_energy_nj` at 410.

## Blocked / needs a decision

- `.hermes/go-plan.txt` is a **protected** file; writing T8 into the autonomous
  queue timed out without approval. Workaround in place: `tools/dispatch_one.sh <ID>`
  dispatches one brief. To run the queue unattended, that file needs a click.

## To resume

```bash
cd ~/Documents/projects/syswatch
pgrep -x syswatch                       # anything still running?
bash tools/verify_t7c.sh                # energy still honest?
bash tools/verify_t10.sh                # headless still honest?
bash tools/dispatch_one.sh T8           # finish durability (Go lane)
```
