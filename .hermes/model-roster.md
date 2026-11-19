# Free Model Roster — selection rules (macOS / opencode zen)

**Rule: never pick "the first free model in the list".** Models are chosen per task shape, and the LEDGER scoreboard overrides this table once real data exists.

## The three sizes that matter
- **ctx** = how much of the repo + brief + error output it can hold at once
- **out** = how much it can write in one turn (low `out` = truncates edits mid-file)
- **quality** = unknown until measured. Context size is *not* quality.

## Roster

| Tier | Model | ctx / out | Use for |
|---|---|---|---|
| **Workhorse (1M)** | `opencode/space-bunny-free` | 1M / 524k | whole-repo reasoning, multi-file refactors, anything where the brief + 3 source files + error log must fit. Highest output ceiling in the catalog — won't truncate. |
| **Strong (1M)** | `opencode/nemotron-3-ultra-free` | 1M / 128k | hard logic, Rust borrow-checker fights, algorithmic work |
| **Strong (1M)** | `opencode/mimo-v2-pro-free` | 1M / 64k | reasoning-heavy, but cap edits (64k out) |
| **Code-specialised** | `opencode/north-mini-code-free` | 256k / 64k | narrow, surgical code edits where the file fits |
| **Bulk edits (1M)** | `opencode/muse-spark-1.3-contributor-free` | 1M / 131k | mechanical, repetitive, many-file changes |
| **Fast / cheap-ish** | `opencode/glm-5.3-flash-free` | 200k | small isolated fixes, quick retries |
| **Control (floor)** | `opencode/deepseek-v4.1-flash-free` | 200k / 128k | keep as the *baseline* — if it passes, the task was easy; if it fails, rotate. Do not start here. |

**Avoid as primary:** `big-pickle` (200k/32k — 32k output truncates multi-file work), `trinity-large-preview-free` and `ling-2.6-flash-free` (no reasoning support).

## Rotation ladder (on failure)
1. same model + the exact failing output pasted back
2. **rotate up** → Workhorse (`space-bunny-free`)
3. rotate → Strong (`nemotron-3-ultra-free`) then Code-specialised (`north-mini-code-free`)
4. **stop.** Three failures = the brief is wrong, not the model. Re-plan.

## Hard truths
- A free model failing a task is data about the **brief**, as often as about the model. Check the brief before blaming the model.
- Verify with acceptance commands; never with the agent's own summary.
- Record every attempt in `LEDGER.jsonl`: job, model, attempt, exit, acceptance result, duration. After ~20 rows, this table gets replaced by measured pass rates.

---

## Measured scoreboard (real results, not estimates)

| Date | Job | Model | Result | Wall | Note |
|---|---|---|---|---|---|
| 2026-11-11 | T2 measurement harness (Python, 60s sampling, `--compare`) | `deepseek-v4.1-flash-free` | **PASS 5/5** | ~6 min | passed first attempt; wrote +602/-104 lines; honest about its own caveats (`ps` lifetime-average CPU, CRLF) |

**Correction to this file's own advice:** DS4 was written off as "will fail anyway". It passed a real test-able task unhurried. Keep it in the rotation for *small, well-specified* work — it is slow, not stupid. Weak-model bias is as wrong as first-in-list laziness.

---

## RULE (from Kishan, 2026-11-11): agents run on **opencode zen free models only**
OpenRouter is demoted to secondary — its free tier is capped (`free-models-per-day`) and produced zero work.
Primary lane = `opencode/*-free` in the terminal. Model ranking comes from measured results, not vibes.

## CORRECTION 2026-11-11 — roster rebuilt from `opencode models`, not the cache

**Why this exists:** `opencode/space-bunny-free` is listed in `~/.cache/opencode/models.json` (catalog) but **not enabled on this account** — the first T4 dispatch died instantly with `Model unavailable`. A model you cannot call has zero context window. Rule: **the roster is built from `opencode models` ANDed with the catalog for limits.**

| Actually callable | ctx | out | reasoning |
|---|---|---|---|
| `openrouter/nvidia/nemotron-3-ultra-550b-a55b:free` | 1000000 | 65536 | True |
| `openrouter/nvidia/nemotron-3.5-lightning:free` | 1000000 | 65536 | True |
| `opencode/nemotron-3.5-lightning-free` | 262144 | 262144 | True |
| `openrouter/google/gemma-4-26b-a4b-it:free` | 262144 | 32768 | True |
| `openrouter/google/gemma-4-31b-it:free` | 262144 | 32768 | True |
| `openrouter/cohere/north-mini-code:free` | 256000 | 64000 | True |

Use the `openrouter/*` free entries for big-context work; the `opencode/*` zen free tier tops out around 262k.


## Human priors (Kishan's field notes — counted as evidence until the ledger overrules them)
| Model | Note |
|---|---|
| `mimo` family (`opencode/mimo-v2.6-flash-free`) | **good quality, slower to process** — use for correctness-critical work, not throughput |
| `deepseek-v4.1-flash-free` | passed T2 first attempt; slow but honest. Keep for small, well-specified tasks |
| `nemotron-3.5-lightning-free` | 262k ctx / 262k out — current default for medium-large files |

## How models get rated (no opinions)
1. Every dispatch appends a LEDGER row: model, attempt, exit, wall time, acceptance result, diff size.
2. Ranking = **acceptance pass rate first**, then **seconds per KB changed** (quality beats speed; speed breaks ties).
3. 3 strikes on the same task class = the model leaves that class's rotation; 3 passes = it becomes the default for that class.
4. A model that fails for *quota* reasons is never scored as a quality failure — different column.
