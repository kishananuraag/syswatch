#!/bin/bash
# One-off Go-lane dispatch for a single brief, with the two hard-won fixes:
#   1. env sanitised — a child of this chat session inherits OPENCODE /
#      OPENCODE_SESSION_ID / OPENCODE_TERMINAL / AI_AGENT and gets reaped ~30 s in.
#   2. prompt passed via "$(cat file)" — briefs contain backticks and bash would
#      execute them if the text were inline.
# Usage: tools/dispatch_one.sh T8 [model]
set -uo pipefail
cd "$(dirname "$0")/.." || exit 1

ID="${1:?task id, e.g. T8}"
# 2026-11-19 Kishan: "use glm 5.3 for a lot of the work, ds4 only where actually needed".
# glm-5.3 is inside the Go subscription with a $500/mo cap; ds4.1-flash stays the explicit
# override (pass it as $2) for tasks that genuinely need the reasoning tier.
MODEL="${2:-glm-5.3}"
BRIEF="briefs/$ID.md"
LOG="logs/$ID-go.log"
[ -f "$BRIEF" ] || { echo "no brief at $BRIEF"; exit 1; }
mkdir -p logs

PROMPT=/tmp/dispatch_${ID}_prompt.txt
{
  cat "$BRIEF"
  cat <<'EOF'

---
## Supervisor context (authoritative, do not re-derive)
Repository is the current directory. Release build: `cargo build --release`.
Acceptance is a script on disk and it is NOT yours to edit: `tools/verify_TASK.sh`
(substitute the task id). Never modify anything under `tools/`.

Facts already verified on this machine — do not contradict them:
  * `RUSAGE_INFO_V6 = 6` and `struct rusage_info_v6` are in `sys/resource.h`
    (not libproc.h); `ri_energy_nj` is at line 410.
  * `ri_gpu_time` does not exist in this SDK: GPU time must be reported as null.
  * Per-PID `proc_pid_rusage` costs ~25 ms per 400 PIDs — never add it to the
    1 s tick path without a slower cadence (`ENERGY_REFRESH`).
  * Do not remove or weaken existing refresh throttles: `last_disk_refresh`,
    `ENERGY_REFRESH`, `last_sweep`. They were measured in.
  * The menu-bar app is already installed at ~/Applications/Syswatch.app and
    running; do not delete it, and do not break `--headless`.

Finish by running your own acceptance commands and pasting real output.
EOF
} > "$PROMPT"

echo "START $(date +%T) task=$ID model=opencode-go/$MODEL prompt=$(wc -c < "$PROMPT")B" > "$LOG"
TOKEN=$(mtx span dispatch "$ID" 2>/dev/null)
env -u OPENCODE -u OPENCODE_SESSION_ID -u OPENCODE_TERMINAL -u AI_AGENT \
  opencode run --agent build --auto --model "opencode-go/$MODEL" "$(cat "$PROMPT")" >> "$LOG" 2>&1 < /dev/null
RC=$?
[ -n "$TOKEN" ] && mtx end "$TOKEN" "$RC" 2>/dev/null
echo "RC=$RC at $(date +%T)" >> "$LOG"
echo "rc=$RC log=$LOG"
exit $RC
