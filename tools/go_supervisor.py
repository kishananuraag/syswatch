#!/usr/bin/env python3
"""Goal-only supervisor: OpenCode Go builds, OpenCode Go reviews, code decides.

The chat session only sets the goal. This process owns the loop:

    dispatch build (opencode-go)  ->  run acceptance script
                ^                          |
                |   fail: failing output fed back
                +--------------------------+
    pass -> dispatch supervisor review (opencode-go) -> verdict file
                |
                +-- review PASS + acceptance PASS -> LEDGER row, next task
                +-- disagreement or 3 failures     -> reports/NEEDS_HUMAN.md

Design rules that are not negotiable here:
  * The prompt is always written to a file first and passed as "$(cat file)".
    Prompt text is never placed inside the shell command line — briefs contain
    backticks, and bash would execute them (that bug ate two dispatches).
  * The agent's own words are never the verdict: tools/verify_<id>.sh decides.
  * Every attempt writes a ledger row, pass or fail.
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PLAN = ROOT / ".hermes" / "go-plan.txt"
LEDGER = ROOT / "LEDGER.jsonl"
REPORTS = ROOT / "reports"
LOGS = ROOT / "logs"
MODELS = ["deepseek-v4.1-flash", "mimo-v2.6-flash", "beam-free"]
PROMPT = Path("/tmp/go_supervisor_prompt.txt")


def now() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def ledger(**row) -> None:
    row = {"ts": now(), "by": "go-supervisor", **row}
    with LEDGER.open("a") as f:
        f.write(json.dumps(row) + "\n")


def log(msg: str) -> None:
    line = f"[{now()}] {msg}"
    print(line, flush=True)


def run_open_code(model: str, prompt: str, tag: str, agent: str = "build") -> tuple[int, str]:
    """Dispatch one agent turn on the Go lane. Returns (rc, tail of its log)."""
    PROMPT.write_text(prompt)
    logfile = LOGS / f"{tag}.log"
    LOGS.mkdir(exist_ok=True)
    cmd = (
        f'opencode run --agent {agent} --auto --model "opencode-go/{model}" '
        f'"$(cat {PROMPT})" > {logfile} 2>&1 < /dev/null'
    )
    # This chat session is itself an opencode session: it inherits OPENCODE,
    # OPENCODE_SESSION_ID, OPENCODE_TERMINAL and AI_AGENT. Children that keep
    # those believe they belong to this session and get reaped ~30 s in
    # (observed rc=-15 on every dispatch until the env was stripped).
    clean_env = {
        k: v
        for k, v in os.environ.items()
        if k not in {"OPENCODE", "OPENCODE_SESSION_ID", "OPENCODE_TERMINAL", "AI_AGENT"}
    }
    t0 = time.time()
    rc = subprocess.call(["bash", "-lc", cmd], cwd=ROOT, env=clean_env)
    tail = ""
    if logfile.exists():
        lines = logfile.read_text(errors="replace").splitlines()
        tail = "\n".join(lines[-60:])
    log(f"{tag}: model=opencode-go/{model} rc={rc} {time.time()-t0:.0f}s log={logfile.name}")
    return rc, tail


def verify(script: str) -> tuple[bool, str]:
    """The only thing that can declare PASS: the acceptance script on disk."""
    path = ROOT / script
    if not path.exists():
        return False, f"acceptance script {script} does not exist"
    p = subprocess.run(["bash", str(path)], cwd=ROOT, capture_output=True, text=True, timeout=900)
    return p.returncode == 0, (p.stdout or "") + (p.stderr or "")


def pick_model() -> str:
    for m in MODELS:
        p = subprocess.run(
            ["bash", "-lc", f'opencode run --auto --model "opencode-go/{m}" "Reply with exactly: PONG" < /dev/null'],
            cwd=ROOT, capture_output=True, text=True, timeout=300,
        )
        if "PONG" in (p.stdout or ""):
            log(f"lane probe: opencode-go/{m} answers")
            return m
    raise SystemExit("no opencode-go model answered PONG")


def build_prompt(brief_path: Path, feedback: str) -> str:
    brief = brief_path.read_text()
    extra = [
        "\n\n---\n",
        "## Supervisor context (authoritative, do not re-derive)\n",
        f"Repository: {ROOT}. Release build: `cargo build --release`.\n",
        "Acceptance is a script on disk and it is not yours to edit: ",
        "`tools/verify_%s.sh`. Never edit files under `tools/verify_*`.\n"
        % brief_path.stem,
        "Hard facts already verified against this SDK — do not contradict them:\n",
        "  * `RUSAGE_INFO_V6 = 6` and `struct rusage_info_v6` live in `sys/resource.h` "
        "(not libproc.h); `ri_energy_nj` is at line 410.\n",
        "  * `ri_gpu_time` does not exist anywhere in this SDK: GPU time must be `null`.\n",
        "  * Collecting per-PID energy costs ~25 ms per 400 PIDs. Never add a per-PID "
        "syscall to the 1 s tick path without a slower cadence.\n",
        "  * Do not remove or weaken an existing refresh throttle (`last_disk_refresh`, "
        "`ENERGY_REFRESH`); they were measured in.\n",
    ]
    if feedback.strip():
        extra += [
            "\n## Previous attempt failed acceptance. Failing output follows — "
            "fix these specifically, do not rewrite the file:\n```\n",
            feedback[-4000:],
            "\n```\n",
        ]
    return brief + "".join(extra)


def review_prompt(task_id: str, brief_path: Path) -> str:
    return f"""You are the supervisor reviewing task {task_id} in {ROOT}.

Read the brief (`{brief_path}`), then run `git diff` and read the changed code.

Judge one question only: does the change do what the brief asks, without
weakening existing behaviour (refresh throttles, error handling, honesty of
reported values)? Pay particular attention to any removed guard.

Write your verdict to `reports/{task_id}-review.md` in this exact shape:

    VERDICT: PASS
    or
    VERDICT: FAIL
    REASONS: <one line each>

Write PASS only if you are sure. If the diff is empty, write FAIL with
"no changes". Do not edit any source file.
"""


def main() -> int:
    REPORTS.mkdir(exist_ok=True)
    LOGS.mkdir(exist_ok=True)
    model = pick_model()
    plan = [l.strip() for l in PLAN.read_text().splitlines() if l.strip() and not l.startswith("#")]
    log(f"goal: {len(plan)} task(s) queued, lane opencode-go/{model}")

    for entry in plan:
        task_id, brief_s, verify_s, *rest = [p.strip() for p in entry.split("|")]
        max_attempts = int(rest[0]) if rest else 3
        brief = ROOT / brief_s
        ledger(job=task_id, kind="dispatch", phase="queued", model=f"opencode-go/{model}")
        feedback = ""
        passed = False

        # Already done on disk? Don't spend a dispatch on it.
        ok, vout = verify(verify_s)
        if ok:
            log(f"{task_id}: acceptance already passes, skipping build")
            ledger(job=task_id, kind="verdict", verdict="PASS", attempt=0,
                   detail="acceptance passed before dispatch; nothing to build")
            continue

        for attempt in range(1, max_attempts + 1):
            log(f"{task_id} attempt {attempt}/{max_attempts}")
            rc, tail = run_open_code(model, build_prompt(brief, feedback), f"{task_id}-go-{attempt}")
            ok, vout = verify(verify_s)
            (REPORTS / f"{task_id}-attempt{attempt}-verify.txt").write_text(vout)

            if ok:
                # Second opinion on the Go lane; the script already proved behaviour.
                log(f"{task_id}: acceptance PASS, requesting supervisor review")
                review = review_prompt(task_id, brief)
                run_open_code(model, review, f"{task_id}-go-{attempt}-review")
                rfile = REPORTS / f"{task_id}-review.md"
                rtext = rfile.read_text() if rfile.exists() else ""
                if "VERDICT: PASS" in rtext.upper():
                    ledger(job=task_id, kind="verdict", verdict="PASS", attempt=attempt,
                           detail="acceptance script + Go supervisor review both pass")
                    log(f"{task_id}: PASS (script + review)")
                    passed = True
                    break
                feedback = "Supervisor review rejected the diff:\n" + rtext[-2500:]
                log(f"{task_id}: review rejected, retrying")
            else:
                feedback = vout
                ledger(job=task_id, kind="attempt", verdict="FAIL", attempt=attempt, rc=rc)
                log(f"{task_id}: acceptance FAIL on attempt {attempt}")

        if not passed:
            ledger(job=task_id, kind="verdict", verdict="ESCALATED", attempt=max_attempts)
            with (REPORTS / "NEEDS_HUMAN.md").open("a") as f:
                f.write(f"\n## {now()} — {task_id} failed {max_attempts} attempts\n")
                f.write(f"Brief: {brief_s}\nVerify: {verify_s}\n")
                f.write(f"Last failing output: reports/{task_id}-attempt{max_attempts}-verify.txt\n")
                f.write("ASK: amend brief | amend contract | park | stop\n")
            log(f"{task_id}: escalated to reports/NEEDS_HUMAN.md, continuing to next task")

    log("queue drained")
    return 0


if __name__ == "__main__":
    sys.exit(main())
