#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""D1 — the unattended queue driver.

Runs a queue of tasks (default `.hermes/queue.json`) one at a time:

* strictly sequential (`--jobs 1`, hard cap of 1 agent process),
* deps respected: a task runs only when every dependency status == "done",
* dispatched as `opencode run "<brief>" --model <model>` under a pty,
  tee'd to `logs/<id>-<attempt>.log`,
* verdict = all `## Acceptance commands` exit 0 AND the agent process
  itself exits 0 — never the agent's prose, and never a green command
  hiding a dead dispatch (`Model unavailable` etc.),
* max 3 attempts: attempt 2 = same model + last 80 log lines appended
  under `## Previous attempt failed with`; attempt 3 = next model on the
  rotation ladder; attempt 3 failing = park (`needs_review`) and continue
  with independent tasks,
* thermal guard for `kind == "build"` (1-min load > 50% of cpu_count, or
  `pmset -g therm` CPU_Speed_Limit < 80): wait 60s, re-check up to 10x,
  then park `thermal_deferred`,
* every step appends an event to `.hermes/run/events.jsonl` (text <= 500
  chars) and dispatch/verdict rows to `LEDGER.jsonl`,
* writes `reports/<YYYY-MM-DD>.md` on exit,
* idempotent: tasks already `done` are never re-run (persisted status +
  a completion record in the ledger).

Exit codes:
    0  everything done (or the `--once` unit passed / clean window stop)
    3  some task failed and was parked this run (needs_review / thermal_deferred)
    4  queue exhausted or nothing runnable

Constraints honoured: Python stdlib only; never git commit/push, sudo, or
install; never edits `briefs/`, `GOAL.md`, `src/`, `dashboard/`.
"""

import argparse
import collections
import datetime
import json
import os
import pty
import re
import select
import signal
import subprocess
import sys
import time
from pathlib import Path

# --------------------------------------------------------------------------
# Paths (anchored to the repo root — the driver orchestrates from here)
# --------------------------------------------------------------------------
ROOT = Path(__file__).resolve().parent.parent
LEDGER = ROOT / "LEDGER.jsonl"
EVENTS = ROOT / ".hermes" / "run" / "events.jsonl"
LOGS = ROOT / "logs"
REPORTS = ROOT / "reports"
DEFAULT_QUEUE = ROOT / ".hermes" / "queue.json"

# --------------------------------------------------------------------------
# Tunables (from briefs/D1.md)
# --------------------------------------------------------------------------
MAX_ATTEMPTS = 3
FAILURE_TAIL_LINES = 80
TEXT_MAX = 500
THERMAL_WAIT_S = 60
THERMAL_MAX_RECHECKS = 10
THERMAL_CPU_LIMIT_MIN = 80
ACCEPT_CMD_TIMEOUT_S = 600          # per acceptance command
SHUTDOWN_GRACE_S = 30               # wait for agent after breaking the pty loop

# Rotation ladder — attempt 3 rotates to the first entry that is not the
# task's own model. Ordered from models proven callable today (see
# .hermes/model-roster.md: roster = `opencode models` ANDed with catalog).
ROTATION_LADDER = [
    "opencode/mimo-v2.6-flash-free",
    "opencode/nemotron-3.5-lightning-free",
    "opencode/deepseek-v4.1-flash-free",
    "opencode/north-mini-code-free",
    "opencode/space-bunny-free",
]

RUNNABLE_STATUSES = {"queued", "blocked", "thermal_deferred"}
PARKED_STATUSES = {"needs_review", "failed"}

ANSI_RE = re.compile(r"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b[@-Z\\-_]")
ACCEPT_HEADING_RE = re.compile(r"^##\s*Acceptance\s+commands", re.IGNORECASE)
THERM_RE = re.compile(r"CPU_Speed_Limit\s+(\d+)")


# --------------------------------------------------------------------------
# Small helpers
# --------------------------------------------------------------------------
def now_ts():
    return datetime.datetime.now().replace(microsecond=0).isoformat()


def clip(text, limit=TEXT_MAX):
    """Short single-line-safe event text (never dump whole logs)."""
    t = re.sub(r"\s+", " ", str(text or "")).strip()
    if len(t) > limit:
        t = t[: limit - 3] + "..."
    return t


def append_jsonl(path, obj):
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "a", encoding="utf-8") as fh:
        fh.write(json.dumps(obj, ensure_ascii=False) + "\n")
        fh.flush()


def clean_line(raw):
    """Bytes from the pty -> readable single line (ANSI stripped)."""
    s = raw.decode("utf-8", errors="replace").replace("\r", "")
    return ANSI_RE.sub("", s).strip()


# --------------------------------------------------------------------------
# Driver
# --------------------------------------------------------------------------
class Driver:
    def __init__(self, args, queue_path):
        self.args = args
        self.queue_path = queue_path
        self.dry = args.dry_run
        self.once = args.once
        self.task_timeout = float(args.task_timeout)
        self.deadline = None
        if args.max_minutes is not None:
            self.deadline = time.time() + float(args.max_minutes) * 60.0
        self.q = None
        self.by_id = {}
        self.results = {}       # job id -> row data for the report
        self.active_procs = set()   # the "cap 1 concurrent agent" guard
        self.deferred_this_run = set()  # thermal-parked this run: never re-pick
        self.flags_desc = " ".join(sys.argv[1:]) or "(no flags)"

    # ---------------- queue ----------------
    def load_queue(self):
        with open(self.queue_path, "r", encoding="utf-8") as fh:
            q = json.load(fh)
        if "tasks" not in q or not isinstance(q["tasks"], list):
            raise SystemExit(f"driver: queue {self.queue_path} has no 'tasks' list")
        return q

    def save_queue(self):
        self.q["updated"] = now_ts()
        tmp = self.queue_path.with_suffix(self.queue_path.suffix + ".tmp")
        with open(tmp, "w", encoding="utf-8") as fh:
            json.dump(self.q, fh, indent=2, ensure_ascii=False)
            fh.write("\n")
        os.replace(tmp, self.queue_path)

    def index(self):
        self.by_id = {t.get("id"): t for t in self.q["tasks"] if t.get("id")}

    # ---------------- events / ledger ----------------
    def event(self, job, kind, model=None, attempt=None, exit=None,
              duration_s=None, text=""):
        append_jsonl(EVENTS, {
            "ts": now_ts(),
            "job": job,
            "kind": kind,
            "model": model,
            "attempt": attempt,
            "exit": exit,
            "duration_s": duration_s,
            "text": clip(text),
        })

    def ledger(self, obj):
        row = {"ts": now_ts()}
        row.update(obj)
        append_jsonl(LEDGER, row)

    # ---------------- deps / eligibility ----------------
    def eligible(self, task):
        if task.get("status") not in RUNNABLE_STATUSES:
            return False
        if task.get("id") in self.deferred_this_run:
            return False          # already thermal-parked this run: do not
        for dep in task.get("deps") or []:   # re-select it in a tight loop
            d = self.by_id.get(dep)
            if d is None or d.get("status") != "done":
                return False
        return True

    def block_reason(self, task):
        status = task.get("status")
        if status not in RUNNABLE_STATUSES:
            return f"status={status}"
        parts = []
        for dep in task.get("deps") or []:
            d = self.by_id.get(dep)
            if d is None:
                parts.append(f"dep {dep} not in queue (never 'done')")
            elif d.get("status") != "done":
                parts.append(f"dep {dep} status={d.get('status')}")
        return "waiting on " + ", ".join(parts) if parts else "runnable"

    # ---------------- brief / acceptance ----------------
    def resolve_brief(self, task):
        raw = task.get("brief")
        if not raw:
            return None
        p = Path(raw)
        if p.is_absolute():
            return p if p.is_file() else None
        for cand in (ROOT / p, self.queue_path.parent / p):
            if cand.is_file():
                return cand
        return None

    @staticmethod
    def extract_acceptance(text):
        """Commands from the fenced block after `## Acceptance commands`."""
        lines = text.splitlines()
        i, n = 0, len(lines)
        while i < n and not ACCEPT_HEADING_RE.match(lines[i].strip()):
            i += 1
        if i >= n:
            return []
        i += 1
        while i < n and not lines[i].strip().startswith("```"):
            i += 1
        if i >= n:          # opening fence never found
            return []
        i += 1
        cmds = []
        while i < n:
            stripped = lines[i].strip()
            if stripped.startswith("```"):
                break
            if stripped:
                cmds.append(stripped)
            i += 1
        return cmds

    # ---------------- thermal guard ----------------
    def thermal_status(self):
        """(ok, reason) — True when a build task may start."""
        cpu = os.cpu_count() or 1
        try:
            load1 = os.getloadavg()[0]
        except (OSError, AttributeError):
            load1 = 0.0
        threshold = 0.5 * cpu
        if load1 > threshold:
            return False, f"1-min load {load1:.2f} > 50% of {cpu} cpus ({threshold:.1f})"
        try:
            p = subprocess.run(["pmset", "-g", "therm"], capture_output=True,
                               text=True, timeout=15)
            m = THERM_RE.search(p.stdout or "")
            if m and int(m.group(1)) < THERMAL_CPU_LIMIT_MIN:
                return False, f"CPU_Speed_Limit {m.group(1)} < {THERMAL_CPU_LIMIT_MIN} (throttling)"
        except (OSError, subprocess.TimeoutExpired):
            pass
        return True, "ok"

    def thermal_wait(self, job, attempt, model):
        """Gate a build task. Returns 'ok' | 'deferred' | 'window'."""
        rechecks = 0
        while True:
            ok, reason = self.thermal_status()
            if ok:
                return "ok"
            if rechecks >= THERMAL_MAX_RECHECKS:
                task = self.by_id.get(job)
                if task is not None:
                    task["status"] = "thermal_deferred"
                    self.save_queue()
                self.deferred_this_run.add(job)
                self.event(job, "deferred", model, attempt,
                           text=f"thermal guard: {reason} — {rechecks} rechecks "
                                f"failed; parked thermal_deferred")
                self.ledger({"job": job, "kind": "defer", "status": "thermal_deferred",
                             "reason": reason, "model": model, "attempt": attempt})
                return "deferred"
            self.event(job, "deferred", model, attempt,
                       text=f"thermal guard blocked build task: {reason}; "
                            f"recheck in 60s ({rechecks + 1}/{THERMAL_MAX_RECHECKS})")
            slept = 0.0
            while slept < THERMAL_WAIT_S:
                if self.deadline is not None and time.time() >= self.deadline:
                    self.event(job, "deferred", model, attempt,
                               text="window elapsed during thermal wait; task left queued")
                    return "window"
                time.sleep(min(5.0, THERMAL_WAIT_S - slept))
                slept += 5.0
            rechecks += 1

    # ---------------- agent dispatch (pty) ----------------
    @staticmethod
    def _kill(proc):
        if proc.poll() is not None:
            return
        try:
            os.killpg(proc.pid, signal.SIGTERM)   # start_new_session -> pgid == pid
        except (ProcessLookupError, PermissionError):
            pass
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(proc.pid, signal.SIGKILL)
            except (ProcessLookupError, PermissionError):
                pass
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                pass

    def run_agent(self, job, attempt, model, brief_text, log_path):
        """opencode run under a pty, tee'd to log_path.

        Returns (agent_exit_code, duration_s, tail_lines, stop) where stop is
        None, ("window", text) when --max-minutes elapsed, or
        ("timeout", text) when --task-timeout killed the process group.
        """
        prompt = brief_text
        argv = ["opencode", "run", prompt, "--model", model]
        start = time.monotonic()
        LOGS.mkdir(parents=True, exist_ok=True)

        mfd, sfd = pty.openpty()
        log = open(log_path, "ab")
        log.write(f"=== {now_ts()} attempt={attempt} model={model} "
                  f"cmd=opencode run --model {model} ===\n".encode())
        log.flush()

        try:
            proc = subprocess.Popen(
                argv, stdin=sfd, stdout=sfd, stderr=sfd,
                start_new_session=True, cwd=str(ROOT), close_fds=True,
            )
        except FileNotFoundError:
            os.close(mfd)
            os.close(sfd)
            log.close()
            return 127, round(time.monotonic() - start, 1), \
                ["driver: 'opencode' executable not found on PATH"], None
        os.close(sfd)

        if len(self.active_procs) >= 1:      # hard cap: one agent at a time
            raise RuntimeError("driver: concurrent agent cap (1) violated")
        self.active_procs.add(proc.pid)

        tail = collections.deque(maxlen=FAILURE_TAIL_LINES)
        pending = b""
        interrupted = None
        try:
            while True:
                elapsed = time.monotonic() - start
                if self.deadline is not None and time.time() >= self.deadline:
                    interrupted = ("window",
                                   "max-minutes window elapsed mid-dispatch")
                    self._kill(proc)
                    break
                if elapsed > self.task_timeout:
                    interrupted = ("timeout",
                                   f"task-timeout {int(self.task_timeout)}s exceeded "
                                   f"— process group killed")
                    self._kill(proc)
                    break
                try:
                    ready, _, _ = select.select([mfd], [], [], 1.0)
                except InterruptedError:
                    continue
                if ready:
                    try:
                        data = os.read(mfd, 65536)
                    except OSError:
                        break               # EIO: child side closed
                    if not data:
                        break
                    log.write(data)
                    log.flush()
                    pending += data
                    parts = re.split(rb"[\r\n]+", pending)
                    pending = parts.pop()   # possibly a partial line
                    for part in parts:
                        line = clean_line(part)
                        if line:
                            tail.append(line)
                elif proc.poll() is not None:
                    break
        except KeyboardInterrupt:
            self._kill(proc)
            raise
        finally:
            if pending:
                line = clean_line(pending)
                if line:
                    tail.append(line)
            try:
                os.close(mfd)
            except OSError:
                pass
            log.close()
            self.active_procs.discard(proc.pid)

        if proc.poll() is None:
            try:
                proc.wait(timeout=SHUTDOWN_GRACE_S)
            except subprocess.TimeoutExpired:
                self._kill(proc)
        rc = proc.wait()
        duration = round(time.monotonic() - start, 1)
        return rc, duration, list(tail), interrupted

    # ---------------- verification ----------------
    def verify(self, cmds):
        """Run every acceptance command in the repo root. One row per command."""
        rows = []
        for cmd in cmds:
            t0 = time.monotonic()
            try:
                p = subprocess.run(cmd, shell=True, cwd=str(ROOT),
                                   capture_output=True, text=True,
                                   timeout=ACCEPT_CMD_TIMEOUT_S)
                rc = p.returncode
                out = (p.stdout or "") + (p.stderr or "")
            except subprocess.TimeoutExpired:
                rc = -1
                out = f"TIMEOUT after {ACCEPT_CMD_TIMEOUT_S}s"
            except OSError as exc:
                rc = -1
                out = f"exec error: {exc}"
            rows.append({
                "cmd": cmd,
                "rc": rc,
                "duration_s": round(time.monotonic() - t0, 1),
                "tail": clip("\n".join(out.strip().splitlines()[-8:]), 400),
            })
        return rows

    # ---------------- one task, full retry ladder ----------------
    def process_task(self, task):
        job = task.get("id")
        task_start = time.monotonic()
        base_model = task.get("model") or ROTATION_LADDER[0]
        alt_model = next((m for m in ROTATION_LADDER if m != base_model),
                         ROTATION_LADDER[0])
        row = {
            "model": base_model,
            "kind": task.get("kind"),
            "attempts": 0,
            "duration_s": 0.0,
            "verdict": "",
            "attempt_rows": [],
        }
        self.results[job] = row

        brief_path = self.resolve_brief(task)
        if brief_path is None:
            self.event(job, "fail", base_model, 0,
                       text=f"brief not found: {task.get('brief')}")
            self._park(job, "brief not found — cannot verify", row)
            return "parked"
        brief_text = brief_path.read_text(encoding="utf-8", errors="replace")
        cmds = self.extract_acceptance(brief_text)
        if not cmds:
            self.event(job, "fail", base_model, 0,
                       text="brief has no '## Acceptance commands' block — "
                            "cannot verify, refusing to run blind")
            self._park(job, "no acceptance commands in brief", row)
            return "parked"

        prev_tail = []
        outcome = "parked"
        for attempt in range(1, MAX_ATTEMPTS + 1):
            if self.deadline is not None and time.time() >= self.deadline:
                row["verdict"] = "not finished — window elapsed before attempt"
                self.event(job, "deferred", base_model, attempt,
                           text=f"window elapsed before attempt {attempt}; task left queued")
                return "window"

            model = base_model if attempt < MAX_ATTEMPTS else alt_model

            if task.get("kind") == "build":
                gate = self.thermal_wait(job, attempt, model)
                if gate == "deferred":
                    row["verdict"] = "parked — thermal_deferred"
                    row["duration_s"] = round(time.monotonic() - task_start, 1)
                    return "deferred"
                if gate == "window":
                    row["verdict"] = "not finished — window elapsed in thermal guard"
                    return "window"

            prompt = brief_text
            if attempt > 1:
                prompt += ("\n\n## Previous attempt failed with\n```\n"
                           + "\n".join(prev_tail) + "\n```\n")
                self.event(job, "retry", model, attempt,
                           text=f"attempt {attempt}/{MAX_ATTEMPTS}: "
                                + ("same model" if model == base_model else "rotated to " + model)
                                + f"; last {len(prev_tail)} log lines appended")

            log_path = LOGS / f"{job}-{attempt}.log"
            self.event(job, "dispatch", model, attempt,
                       text=f"attempt {attempt}/{MAX_ATTEMPTS} kind={task.get('kind')} "
                            f"brief={brief_path.name} log={log_path.name}")
            self.ledger({
                "job": job, "model": model, "attempt": attempt,
                "kind": "dispatch", "log": str(log_path),
            })
            print(f"[d1] dispatch {job} attempt {attempt}/{MAX_ATTEMPTS} "
                  f"model={model} log={log_path.name}", flush=True)

            rc, duration, tail, stop = self.run_agent(
                job, attempt, model, prompt, log_path)
            row["attempts"] = attempt
            row["duration_s"] = round(time.monotonic() - task_start, 1)
            rel_log = str(log_path)

            if stop is not None and stop[0] == "window":
                reason_text = stop[1]
                self.ledger({"job": job, "model": model, "attempt": attempt,
                             "kind": "interrupt", "reason": reason_text,
                             "exit": rc, "duration_s": duration})
                self.event(job, "deferred", model, attempt, exit=rc,
                           duration_s=duration,
                           text=f"{reason_text} — task left queued for next window")
                row["verdict"] = "interrupted — left queued"
                row["attempt_rows"].append({
                    "attempt": attempt, "model": model, "agent_exit": rc,
                    "duration_s": duration, "log": rel_log,
                    "failing_cmd": None, "failing_rc": None,
                    "interrupted": True,
                })
                print(f"[d1] WINDOW {job}: {reason_text} — left queued", flush=True)
                return "window"

            stop_text = stop[1] if stop else None
            if stop_text:
                tail = tail + [stop_text]
            prev_tail = tail
            self.event(job, "stdout", model, attempt, exit=rc,
                       duration_s=duration,
                       text=clip(" | ".join(tail[-5:]) or "(no output)"))

            if stop is not None and stop[0] == "timeout":
                # A hung agent killed at --task-timeout is an attempt failure:
                # it goes through the retry ladder and eventually parks.
                fail_reason = stop_text
                self.event(job, "fail", model, attempt, exit=rc,
                           duration_s=duration,
                           text=f"attempt {attempt}: {fail_reason}")
                self.ledger({"job": job, "model": model, "attempt": attempt,
                             "kind": "fail", "exit": rc, "duration_s": duration,
                             "reason": fail_reason})
                print(f"[d1] FAIL {job} attempt {attempt}: {fail_reason}",
                      flush=True)
                row["attempt_rows"].append({
                    "attempt": attempt, "model": model, "agent_exit": rc,
                    "duration_s": duration, "log": rel_log,
                    "failing_cmd": f"opencode run (task-timeout "
                                   f"{int(self.task_timeout)}s kill)",
                    "failing_rc": rc,
                })
                if attempt < MAX_ATTEMPTS:
                    outcome = "retry"
                    continue
                outcome = "parked"
                break

            results = self.verify(cmds)
            n_ok = sum(1 for r in results if r["rc"] == 0)
            acceptance_ok = n_ok == len(cmds)
            first_fail = next((r for r in results if r["rc"] != 0), None)
            # Verdict = acceptance commands AND a live dispatch. A dead agent
            # (e.g. "Model unavailable") must never ride a trivially-green
            # acceptance command to `done` — that would silently no-op the loop.
            passed = acceptance_ok and rc == 0

            if acceptance_ok and rc != 0:
                verify_note = (f"all {n_ok}/{len(results)} acceptance commands "
                               f"exited 0 but agent process exited {rc}")
            elif first_fail:
                verify_note = (f"{n_ok}/{len(results)} acceptance commands exited 0; "
                               f"first failing: {first_fail['cmd']!r} "
                               f"rc={first_fail['rc']}")
            else:
                verify_note = f"{n_ok}/{len(results)} acceptance commands exited 0"
            self.event(job, "verify", model, attempt, duration_s=duration,
                       text=verify_note)
            self.ledger({
                "job": job, "model": model, "attempt": attempt,
                "kind": "verdict", "exit": rc,
                "acceptance": f"{n_ok}/{len(results)} " + ("PASS" if passed else "FAIL"),
                "duration_s": duration,
                **({"failing_cmd": first_fail["cmd"],
                    "failing_rc": first_fail["rc"]} if first_fail else {}),
            })

            row["attempt_rows"].append({
                "attempt": attempt, "model": model, "agent_exit": rc,
                "duration_s": duration, "log": rel_log,
                "failing_cmd": first_fail["cmd"] if first_fail else None,
                "failing_rc": first_fail["rc"] if first_fail else None,
            })

            if passed:
                task["status"] = "done"
                self.save_queue()
                self.event(job, "pass", model, attempt, exit=rc,
                           duration_s=duration,
                           text=f"{len(cmds)}/{len(cmds)} acceptance passed "
                                f"(agent exit {rc}) — status done")
                self.ledger({
                    "job": job, "model": model, "attempt": attempt,
                    "kind": "done", "attempts": attempt,
                    "acceptance": f"{len(cmds)}/{len(cmds)} PASS",
                    "duration_s": round(time.monotonic() - task_start, 1),
                    "log": rel_log,
                })
                row["verdict"] = f"done ({len(cmds)}/{len(cmds)} PASS, attempt {attempt})"
                row["duration_s"] = round(time.monotonic() - task_start, 1)
                print(f"[d1] PASS {job} attempt {attempt} "
                      f"{len(cmds)}/{len(cmds)}", flush=True)
                outcome = "done"
                break

            fail_reason = (f"`{first_fail['cmd']}` exited {first_fail['rc']}"
                           if first_fail else
                           f"agent process exited {rc} (acceptance green, dispatch failed)")
            self.event(job, "fail", model, attempt, exit=rc, duration_s=duration,
                       text=f"attempt {attempt}: {fail_reason}")
            print(f"[d1] FAIL {job} attempt {attempt}: {fail_reason} "
                  f"(agent exit {rc})", flush=True)
            if attempt < MAX_ATTEMPTS:
                outcome = "retry"
            else:
                outcome = "parked"

        if outcome == "parked":
            self._park(job, f"all {MAX_ATTEMPTS} attempts failed", row, rel_log)
        return outcome

    def _park(self, job, reason, row, log_path=None):
        task = self.by_id.get(job)
        if task is not None:
            task["status"] = "needs_review"
            self.save_queue()
        if log_path is None:
            candidates = sorted(LOGS.glob(f"{job}-*.log")) if LOGS.is_dir() else []
            log_path = str(candidates[-1]) if candidates else "(no log)"
        row["verdict"] = f"parked needs_review — {reason}"
        self.event(job, "park", row.get("model"), row.get("attempts"),
                   text=f"{reason}; parked needs_review; log {log_path}")
        self.ledger({"job": job, "kind": "park", "status": "needs_review",
                     "reason": reason, "log": log_path,
                     "attempts": row.get("attempts")})
        print(f"[d1] PARK {job}: {reason} (log {log_path})", flush=True)

    # ---------------- dry run ----------------
    def print_plan(self):
        print(f"DRY RUN — queue {self.queue_path} (nothing changed, nothing run)")
        order = [t for t in self.q["tasks"] if self.eligible(t)]
        print(f"Resolved order (jobs=1, strictly sequential): "
              f"{len(order)} task(s) would run")
        for i, t in enumerate(order, 1):
            deps = t.get("deps") or []
            base = t.get("model") or ROTATION_LADDER[0]
            alt = next((m for m in ROTATION_LADDER if m != base), "-")
            print(f"  {i}. {t.get('id')} kind={t.get('kind')} "
                  f"deps={deps or '[]'} status={t.get('status')}")
            print(f"     model={base}")
            print(f"     brief={t.get('brief')}")
            print(f"     attempts: 1-2 {base}, 3 -> {alt}"
                  + ("  [build: thermal guard applies]"
                     if t.get("kind") == "build" else ""))
        not_run = [t for t in self.q["tasks"] if not self.eligible(t)]
        if not_run:
            print(f"Not runnable ({len(not_run)}):")
            for t in not_run:
                print(f"  {t.get('id')}: {self.block_reason(t)}")
        if not self.q["tasks"]:
            print("Queue is empty.")
        print("No changes made (dry run).")

    # ---------------- report ----------------
    def write_report(self, exit_code, started_ts, run_wall):
        REPORTS.mkdir(parents=True, exist_ok=True)
        date = datetime.date.today().isoformat()
        path = REPORTS / f"{date}.md"
        finished = now_ts()
        lines = []
        if not path.exists():
            lines.append(f"# Driver report — {date}\n")
        lines.append(f"\n## Run {datetime.datetime.now().strftime('%H:%M:%S')}\n")
        lines.append(f"- queue: `{self.queue_path}`")
        lines.append(f"- flags: `{self.flags_desc}`")
        lines.append(f"- started {started_ts} / finished {finished} "
                     f"({run_wall:.0f}s wall)")
        lines.append(f"- exit code: **{exit_code}**\n")
        lines.append("| id | model | attempts | verdict | duration |")
        lines.append("|---|---|---:|---|---:|")
        failures = []
        for t in self.q["tasks"]:
            jid = t.get("id")
            r = self.results.get(jid)
            if r:
                model = r["model"] or "-"
                attempts = str(r["attempts"]) if r["attempts"] else "-"
                verdict = r["verdict"] or t.get("status", "-")
                dur = f"{r['duration_s']:.0f}s" if r["duration_s"] else "-"
            else:
                model = t.get("model") or "-"
                attempts = "-"
                st = t.get("status")
                verdict = (f"done (earlier run)" if st == "done"
                           else f"not run — {self.block_reason(t)}")
                dur = "-"
            lines.append(f"| {jid} | `{model}` | {attempts} | {verdict} | {dur} |")
            for ar in (r["attempt_rows"] if r else []):
                if ar.get("interrupted"):
                    continue          # window kill, not a task failure
                if ar.get("failing_cmd") or (ar.get("agent_exit") not in (0, None)):
                    failures.append((jid, ar))

        lines.append("\n### Failures (failing acceptance command per failed attempt)\n")
        if failures:
            for jid, ar in failures:
                if ar.get("failing_cmd"):
                    what = (f"`{ar['failing_cmd']}` exited {ar['failing_rc']}")
                else:
                    what = f"agent process exited {ar.get('agent_exit')} (acceptance green, dispatch failed)"
                lines.append(f"- **{jid}** attempt {ar['attempt']} "
                             f"(`{ar['model']}`): {what} — log `{ar['log']}`")
        else:
            lines.append("- none this run")

        parked = [t for t in self.q["tasks"]
                  if t.get("status") in PARKED_STATUSES or t.get("status") == "thermal_deferred"]
        lines.append("\n## NEEDS REVIEW\n")
        if parked:
            for t in parked:
                jid = t.get("id")
                r = self.results.get(jid)
                if r and r["attempt_rows"]:
                    log = r["attempt_rows"][-1]["log"]
                else:
                    candidates = sorted(LOGS.glob(f"{jid}-*.log")) if LOGS.is_dir() else []
                    log = str(candidates[-1]) if candidates else "(no log)"
                lines.append(f"- **{jid}** — status `{t.get('status')}`; "
                             f"last log: `{log}`")
        else:
            lines.append("- none — no parked tasks")

        text = "\n".join(lines) + "\n"
        if path.exists():
            with open(path, "a", encoding="utf-8") as fh:
                fh.write(text)
        else:
            path.write_text(text, encoding="utf-8")
        print(f"[d1] report written: {path}", flush=True)
        return path

    # ---------------- main loop ----------------
    def run(self):
        started_ts = now_ts()
        t0 = time.monotonic()
        try:
            self.q = self.load_queue()
        except FileNotFoundError:
            raise SystemExit(f"driver: queue not found: {self.queue_path}")
        except json.JSONDecodeError as exc:
            raise SystemExit(f"driver: queue is not valid JSON: {exc}")
        self.index()

        if self.dry:
            self.print_plan()
            return 0

        # A crash mid-dispatch leaves 'running' behind — re-queue it so a
        # restart resumes idempotently. Done tasks are never touched.
        stale = [t for t in self.q["tasks"] if t.get("status") == "running"]
        if stale:
            for t in stale:
                t["status"] = "queued"
            self.save_queue()
            print(f"[d1] re-queued stale running task(s): "
                  f"{[t.get('id') for t in stale]}", flush=True)

        print(f"[d1] queue={self.queue_path} tasks={len(self.q['tasks'])} "
              f"jobs=1 deadline={self.deadline}", flush=True)

        outcomes = []
        window_stop = False
        try:
            while True:
                if self.deadline is not None and time.time() >= self.deadline:
                    print("[d1] max-minutes window elapsed — stopping cleanly, "
                          "queue left intact", flush=True)
                    window_stop = True
                    break
                cand = next((t for t in self.q["tasks"] if self.eligible(t)), None)
                if cand is None:
                    break
                outcome = self.process_task(cand)
                outcomes.append(outcome)
                if outcome == "window":
                    window_stop = True
                    break
                if self.once:
                    break
        except KeyboardInterrupt:
            print("\n[d1] interrupted — queue left intact", flush=True)
            self.event("-", "deferred", text="run interrupted (SIGINT); queue intact")
            window_stop = True

        run_wall = time.monotonic() - t0
        parks = [o for o in outcomes if o in ("parked", "deferred")]
        tasks = self.q["tasks"]
        all_done = bool(tasks) and all(t.get("status") == "done" for t in tasks)

        if parks:
            code = 3
        elif not tasks:
            code = 4
        elif all_done:
            code = 0
        elif window_stop:
            code = 0
        elif self.once and outcomes and outcomes[0] in ("done", "retry"):
            code = 0
        else:
            code = 4    # nothing runnable / queue exhausted

        self.write_report(code, started_ts, run_wall)
        done_n = sum(1 for t in tasks if t.get("status") == "done")
        print(f"[d1] finished: {done_n}/{len(tasks)} done, "
              f"parked={len(parks)}, exit={code}", flush=True)
        return code


# --------------------------------------------------------------------------
def build_parser():
    epilog = """\
exit codes:
  0   everything done (or the --once unit passed / clean window stop)
  3   some task failed and was parked this run (needs_review / thermal_deferred)
  4   queue exhausted or nothing runnable

examples:
  python3 tools/driver.py --dry-run
  python3 tools/driver.py --once --max-minutes 120
  python3 tools/driver.py --queue /tmp/d1/q.json --once
"""
    p = argparse.ArgumentParser(
        prog="tools/driver.py",
        description="Unattended task queue driver: dispatch -> acceptance-verify "
                    "-> retry with model rotation -> park. One agent at a time.",
        epilog=epilog,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    p.add_argument("--queue", default=str(DEFAULT_QUEUE),
                   help="queue json path (default: .hermes/queue.json; "
                        "relative paths resolve from the current directory)")
    p.add_argument("--once", action="store_true",
                   help="process exactly one task (full retry ladder) then exit")
    p.add_argument("--dry-run", action="store_true",
                   help="print the resolved order and what would run; "
                        "changes nothing, runs nothing")
    p.add_argument("--max-minutes", type=float, default=None, metavar="N",
                   help="stop cleanly after N minutes, leaving the queue intact")
    p.add_argument("--jobs", type=int, default=1, metavar="1",
                   help="concurrent tasks (only 1 is supported for now)")
    p.add_argument("--task-timeout", type=float, default=45 * 60, metavar="SEC",
                   help="kill a hung agent process group after SEC seconds "
                        "(default: 2700 = 45 min)")
    return p


def main(argv=None):
    parser = build_parser()
    args = parser.parse_args(argv)
    if args.jobs != 1:
        parser.error("--jobs only supports 1 for now (sequential)")
    if args.max_minutes is not None and args.max_minutes <= 0:
        parser.error("--max-minutes must be > 0")
    if args.task_timeout <= 0:
        parser.error("--task-timeout must be > 0")

    queue_path = Path(args.queue).expanduser()
    if not queue_path.is_absolute():
        queue_path = (Path.cwd() / queue_path).resolve()

    driver = Driver(args, queue_path)
    return driver.run()


if __name__ == "__main__":
    sys.exit(main())
