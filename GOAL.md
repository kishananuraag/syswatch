# GOAL — syswatch on the Mac
**Owner:** Kishan (only he edits this file) · **Keeper:** Hermes · **Locked:** 2026-11-11

> One command installs syswatch on this Mac. It runs natively as a menu-bar app + CLI + JSON API, showing per-core CPU, memory/swap, disk and disk-I/O, network rates, top processes, temperature, fan, power and battery wherever the OS exposes them — at iStat Menus parity for those metrics, under 1 % CPU at a 1-second interval and under 15 MB RSS, surviving reboot unattended, and provable by a `--json` snapshot that matches `ps` / `vm_stat` / `netstat` / `powermetrics` within stated tolerance. Opening it shows the agent work in progress, live.

---

## How this goal is measured (see `ACCEPTANCE.md` for the commands)

| Dimension | Target | Proven by |
|---|---|---|
| Install | one command, fresh shell works | T10 acceptance |
| Platforms | menu-bar app + CLI + JSON API on macOS | T6, T4 |
| Metrics | CPU per-core, mem/swap, disk + disk-I/O, net rates, procs, temp, fan, power, battery | T5 parity matrix |
| Parity | matches iStat Menus metric-for-metric where the OS exposes it | `docs/parity.md` (REA evidence) |
| Cost | < 1 % CPU @ 1 s interval, < 15 MB RSS | T7 before/after table |
| Durability | survives reboot, restarts itself within 60 s | T8 LaunchAgent test |
| Honesty | every number matches `ps`/`vm_stat`/`netstat`/`powermetrics` within tolerance | T5 cross-check |
| Visibility | opening syswatch shows live agent work | T9 cockpit |

## Standing constraints
- **Thermal:** one build-heavy task at a time · `CARGO_BUILD_JOBS=4` · `cargo check` before `cargo build --release` · `nice -n 10` · never launch a build above 50 % load or 85 °C.
- **Hours:** no paid supervision from 11:30 IST. Driver chains the queue unattended; anything needing judgement is parked as `needs_review`.
- **Evidence:** a task is done only when its acceptance commands exit 0 — never on an agent's claim.
- **Money:** agents are $0. Paid tokens only for planning and verdicts.
