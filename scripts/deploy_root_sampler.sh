#!/bin/bash
# Deploy the T19 root power daemon — ONE admin prompt.
#
# T19 replaced the shell wrapper (syswatch_power_sampler.sh) and the C helper
# (smc_temp) with a single Rust binary, `syswatch-powerd`.  This script is the
# command-line twin of the dashboard's Grant button (POST /api/permissions
# /grant?id=package_power) and does exactly the same thing, in this order:
#
#   0. PRE-FLIGHT, unprivileged: `cargo build --release --bin syswatch-powerd`
#      and a one-cycle self-check.  A compiler never runs as root, and a broken
#      binary never reaches /usr/local/libexec.
#   1. install the binary -> /usr/local/libexec/syswatch/syswatch-powerd
#      (root:wheel 0755)
#   2. install the reloaded plist -> /Library/LaunchDaemons/
#      com.barbarian.syswatch.power.plist  (same label, same grant — this is an
#      update of the EXISTING job, not a new agent)
#   3. remove the two artefacts T19 retired, AFTER the new daemon is up
#   4. `launchctl bootout` + `bootstrap` — the plist's ProgramArguments changed
#      (a bare kickstart would restart the old command line)
#
# No password is ever seen, echoed or stored here: it is consumed by macOS in
# the admin dialog before any of these steps run.
#
# Usage: scripts/deploy_root_sampler.sh [--no-restart]

set -uo pipefail
cd "$(dirname "$0")/.."

RESTART=1
[ "${1:-}" = "--no-restart" ] && RESTART=0

REPO="$PWD"
LABEL="com.barbarian.syswatch.power"
DEST_DIR="/usr/local/libexec/syswatch"
BIN="$REPO/target/release/syswatch-powerd"
TPL="$REPO/packaging/launchd/$LABEL.plist"
OUT_DIR="$HOME/Library/Application Support/syswatch/root"
STAGED_PLIST="$OUT_DIR/$LABEL.plist"

[ -f "$TPL" ] || { echo "missing $TPL"; exit 1; }

# ── 0. pre-flight, as the user ───────────────────────────────────────────────
echo "== building syswatch-powerd =="
cargo build --release --bin syswatch-powerd 2>&1 | tail -3
[ -x "$BIN" ] || { echo "build failed — nothing installed"; exit 1; }
"$BIN" --version || { echo "binary does not run — nothing installed"; exit 1; }

echo "== single-cycle self-check (no root: SMC + parser + atomic write) =="
TMPCHK=$(mktemp -d)
grep -v '^#' "$OUT_DIR/powermetrics.raw.log" > "$TMPCHK/replay.txt" 2>/dev/null
if [ -s "$TMPCHK/replay.txt" ]; then
    if "$BIN" --once --replay "$TMPCHK/replay.txt" --out-dir "$TMPCHK"; then
        KEYS=$(python3 -c "import json,sys;print(len(json.load(open('$TMPCHK/sample.json'))))")
        echo "self-check: sample.json valid, $KEYS keys"
    else
        echo "self-check FAILED — nothing installed"; rm -rf "$TMPCHK"; exit 1
    fi
else
    echo "self-check: skipped (no captured powermetrics.raw.log to replay)"
fi
rm -rf "$TMPCHK"

# ── render the plist into the user's support dir (unprivileged) ──────────────
python3 - "$TPL" "$STAGED_PLIST" <<'PY'
import sys
src, dst = sys.argv[1], sys.argv[2]
import os
text = open(src, encoding="utf-8").read()
text = text.replace("__HOME__", os.path.expanduser("~"))
os.makedirs(os.path.dirname(dst), exist_ok=True)
open(dst, "w", encoding="utf-8").write(text)
os.chmod(dst, 0o644)
PY
[ -f "$STAGED_PLIST" ] || { echo "could not stage the plist"; exit 1; }

STEP_LIST=(
  "/bin/mkdir -p $(printf %q "$DEST_DIR") /Library/LaunchDaemons"
  "/usr/bin/install -m 755 -o root -g wheel $(printf %q "$BIN") $(printf %q "$DEST_DIR/syswatch-powerd")"
  "/usr/bin/install -m 644 -o root -g wheel $(printf %q "$STAGED_PLIST") $(printf %q "/Library/LaunchDaemons/$LABEL.plist")"
)
if [ "$RESTART" = "1" ]; then
    STEP_LIST+=(
      "/bin/launchctl bootout system/$LABEL >/dev/null 2>&1 || true"
      "/bin/launchctl bootstrap system $(printf %q "/Library/LaunchDaemons/$LABEL.plist")"
    )
fi
# T19 cleanup: the retired shell + C artefacts.  After the new daemon is up.
STEP_LIST+=(
  "/bin/rm -f $(printf %q "$DEST_DIR/power_sampler.sh")"
  "/bin/rm -f $(printf %q "$DEST_DIR/smc_temp")"
)

JOINED=$(
    printf '%s; ' "${STEP_LIST[@]}" | sed 's/; $//'
)
ESC=${JOINED//\\/\\\\}
ESC=${ESC//\"/\\\"}

echo "== one admin prompt: install ${LABEL} =="
osascript -e "do shell script \"$ESC\" with administrator privileges"
RC=$?
if [ $RC -ne 0 ]; then
    echo "admin step failed (exit $RC) — user declined, or a command failed"
    exit $RC
fi

echo "== installed =="
ls -l "$DEST_DIR/syswatch-powerd" "/Library/LaunchDaemons/$LABEL.plist"
echo "== waiting for the daemon's next sample =="
for i in $(seq 1 20); do
    sleep 1
    if [ -f "$OUT_DIR/sample.json" ]; then
        AGE=$(( $(date +%s) - $(stat -f %m "$OUT_DIR/sample.json") ))
        [ "$AGE" -le 8 ] && break
    fi
done
python3 - "$OUT_DIR/sample.json" <<'PY'
import json, sys, time
p = sys.argv[1]
d = json.load(open(p))
print("age_s        ", round(time.time() - d.get("ts", 0)))
print("keys         ", len(d), sorted(d.keys()))
print("temps rows   ", len(d.get("temps") or []))
print("e/p cluster  ", d.get("e_cluster"), d.get("p_cluster"))
print("smc          ", d.get("smc"))
PY
echo "== daemon process =="
ps -Ao pid=,rss=,command= | grep -v grep | grep syswatch-powerd || true
