# syswatch — Goal-Driven Autonomy Plan **v3**: from scratch, dormant supervisor
*Supersedes v2 (2026-11-11). Two new decisions from Kishan (2026-11-15) drive this version.*
**Home base:** Mac (Apple Silicon, 8 cores, 8 GB) · **Executor:** `opencode run` on `*-free` models · **Goal owner:** Kishan · **Supervisor:** Hermes (wakes only on exception)
**Status:** PLAN ONLY. No dispatches, no installs, no commits from this file.

---

## 0. Decisions locked in this version

| # | Decision | Consequence |
|---|---|---|
| **V1** | **Build from scratch. Do not clone iStat Menus.** | No decompiled code, no lifted assets, no pixel-copy of their UI. REA (T3) is restricted to *behavioural* research: which OS APIs and SMC/IOReport keys a monitor must touch. |
| **V2** | **Supervisor sets the goal only; agents iterate to the goal.** | Hermes writes `GOAL.md` + briefs + acceptance commands. It does **not** write code, does not babysit, and is not in the loop between dispatches. |
| **V3** | **Dormant supervision (the token fix).** | Driver runs unattended at $0 CPU-and-time cost; Hermes is invoked *by the driver* only on 5 exception triggers, in one short turn with a ≤2 KB packet. Between wakes: zero tokens. |
| **V4** | **Plan mode first, always.** | Every build task runs `opencode run --agent plan` on the brief → `.hermes/plans/<id>.md`, then `--agent build` against that plan. A plan that cannot name its acceptance commands does not get built. |
| **V5** | **Lightweight is a measured target, not an adjective.** | < 1 % CPU @ 1 s · < 15 MB RSS · one process · no `powermetrics` root daemon · no subprocess per sample. iStat is the reference, and it runs two `powermetrics` instances right now — that is the bar to beat, not to copy. |

### Why not clone iStat Menus
- It is **closed-source commercial software** (paid licence). Its EULA forbids reverse-engineering; decompiling to re-use code is a licence and copyright problem, not a shortcut.
- The valuable knowledge is **not in their binary** — it is in macOS: SMC keys, `IOReport` channels, `libproc`, `proc_pid_rusage`. Every one of those is discoverable from the OS with `ioreg`, `libproc.h`, and `powermetrics -n 1`, legally and cheaply.
- What iStat actually does well is **presentation discipline**: fixed layout, one glance per metric, no chart junk. We copy *that intent* into our own design (`DESIGN.md`, `design_concepts/`), never their pixels.

---

## 1. What the app is (unchanged, restated)

One menu-bar app + CLI + JSON API on this Mac. Per-core CPU, memory/swap, disk + disk-I/O, network rates, top processes, temperature, fan, power, battery — **plus two things iStat does not store at all**: instantaneous per-process CPU and per-process energy with attribution.

That gap is the product. iStat shows per-process energy live and throws it away; its history DB has 9 metric tables and **zero process dimension**.

---

## 2. Task graph (v3)

```
D1 (queue driver) ── done when driver.py passes 9/9 acceptance
 ├── T3  REA behavioural parity research → docs/parity.md          [light]
 ├── D2  dormant supervisor + plan-mode support in driver           [build]  deps: D1
 ├── T7B instantaneous per-process CPU (proc_pidinfo deltas)        [build]  deps: D1
 ├── T7C per-app energy + attribution (proc_pid_rusage)             [build]  deps: D1
 ├── T5  parity matrix + graphs fed by fixtures/istat corpus        [build]  deps: T3, T7B, T7C
 └── T6  menu-bar app from scratch, ≤15 MB / ≤1 %                   [build]  deps: T5
      ├── T7A optimisation pass O1–O8 (feature-gate, ring buffers)  [build]  deps: T6
      ├── T8  LaunchAgent durability (survives reboot < 60 s)       [build]  deps: T6
      └── T9  cockpit: live agent work inside syswatch              [build]  deps: D1, T6
```

`kind`: `light` = bounded, no build; `build` = compiles/edits `src/`, gated by the thermal guard.

---

## 3. Dormant supervision — the actual mechanism

### The problem being solved
AmplifAI-style supervision burns tokens because a paid model sits in the loop: every step, every check, every "did it work" is a turn, and each turn re-sends the whole growing context. Iteration count is high, decision count is low. Paying for both is waste.

### The inversion
**The driver is the loop. The supervisor is an exception handler.**

| Layer | Who | Cost | What it does |
|---|---|---|---|
| Loop | `tools/driver.py` | **$0** — deterministic code, no model | dispatch → run acceptance itself → retry ladder → model rotation → thermal guard → ledger → events |
| Wake | `hermes -z "<packet>"` | paid, **one short turn** | reads a ≤2 KB packet, returns a decision: amend goal / amend acceptance / park / stop |
| Silence | nothing | **$0** | between wakes there is no process, no polling, no context |

### The five wake triggers (only these)
| # | Trigger | Why a human-level decision is genuinely needed |
|---|---|---|
| W1 | A task fails attempt 3 | three different models failed the same brief → the brief is wrong, not the model |
| W2 | Acceptance passes but the diff touches a forbidden path, or grows > 400 changed lines | the agent satisfied the letter and broke the contract |
| W3 | Two different tasks fail with the same error class | systemic misread of the architecture |
| W4 | Queue exhausted, or deps deadlock | routing decision nobody else can make |
| W5 | Thermal-deferred after 10 rechecks, or wake budget hit (6/24 h) | machine state or runaway guard |

Everything else — retries, rotations, transient failures, slow tasks — is handled and logged without waking anything.

### The wake packet (why it is cheap)
```
GOAL: <one line from GOAL.md>
TASK: T7B  attempt 3/3  kind=build  model=nemotron-3.5-lightning-free
FAILING: <exact command>
EXIT: 101   STDOUT_TAIL: <last 40 lines, <=1200 chars>
LEDGER: <3 rows>
ASK: amend brief | amend acceptance | park | stop
```
No conversation history, no repo dump, no re-explanation of the project. The supervisor answers from the packet alone — that is the whole point. `--usage-file` records the spend of each wake so the cost is a number, not a feeling.

### Proof of dormancy (this is how V3 gets accepted)
D2's acceptance runs a synthetic 3-task queue: two tasks pass silently, one fails three times → the run must produce **exactly one** wake call, and the event log must contain `sleep` events for the idle stretches. If it wakes more than once, the design failed its own test.

---

## 4. Metrics we will beat iStat on (measured, not claimed)

| Dimension | iStat Menus 7 (measured on this Mac) | syswatch target |
|---|---|---|
| Sampling cost | 2 × `powermetrics` running as root daemon | one in-process pass over PIDs, no subprocess |
| Privilege | root for temp/power | none for CPU/mem/net/disk/per-proc energy |
| DB size | 151 MB / 87 days, 9 metric tables | ring buffer + same cascade ladder, target < 40 MB |
| Process history | **none stored** | per-process CPU + energy + GPU time, downsampled |
| Energy per app | live UI only, discarded | stored, graphed, attributed with a closing residual |
| Collector CPU | not published | < 1 % @ 1 s, proven by `scripts/measure_collector.py` |

---

## 5. FINISH contract for v3

The project is finished when, on this Mac, with no supervision running:

1. `syswatch --json` matches `ps` / `vm_stat` / `netstat` / `powermetrics -n 1` within stated tolerance (T5 parity table, every row citing output).
2. Its own cost is measured at < 1 % CPU @ 1 s and < 15 MB RSS over a 10-minute soak (T7A).
3. Per-process energy is stored and reconciles: Σ(process watts) ≤ package watts, with the residual reported, never hidden (T7C).
4. `launchctl` survives a reboot and the app is back within 60 s (T8).
5. Opening the app shows live agent work from `.hermes/run/events.jsonl` (T9).
6. A full unattended night produces ≥ 1 finished task and **≤ 6 wake calls**, each logged with its token cost (D2).

Every line above is a command that exits 0, or it is not done.
