#!/bin/bash
# Supervisor acceptance for T10 — headless collector.
# No tty anywhere: if any check needs a terminal, the task is not done.
cd ~/Documents/projects/syswatch || exit 1
FAIL=0
ok(){ echo "  PASS  $1"; }
bad(){ echo "  FAIL  $1"; FAIL=$((FAIL+1)); }

echo "== 1. flag exists =="
./target/release/syswatch --help 2>&1 | grep -q -- "--headless" && ok "--headless documented" || bad "--headless missing from --help"

echo "== 2. runs with NO controlling terminal =="
# setsid-style: detach from this shell's session entirely; a raw-mode TUI would die here.
python3 - <<'PY'
import os, signal, subprocess, sys, time
os.environ.pop("TERM", None)
p = subprocess.Popen(
    ["./target/release/syswatch", "--headless", "--interval-ms", "1000", "--log"],
    stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    start_new_session=True, env={**os.environ, "TERM": "dumb"},
)
time.sleep(5)
alive = p.poll() is None
if not alive:
    out, err = p.communicate()
    print("  FAIL  headless exited early rc=%s err=%s" % (p.returncode, (err or b"")[:200].decode(errors="replace")))
    sys.exit(1)
print("  PASS  alive after 5s with no tty, TERM=dumb")
# 3. SIGTERM must exit 0 inside 1s
t0 = time.time()
p.send_signal(signal.SIGTERM)
try:
    rc = p.wait(timeout=3.0)
except subprocess.TimeoutExpired:
    p.kill(); print("  FAIL  did not exit within 3s of SIGTERM"); sys.exit(1)
dt = time.time() - t0
print(f"  {'PASS' if rc == 0 else 'FAIL'}  SIGTERM -> rc={rc} in {dt:.2f}s")
sys.exit(0 if rc == 0 and dt < 1.0 else 1)
PY
[ $? -ne 0 ] && FAIL=$((FAIL+1))

echo "== 4. output is JSON lines, no escape codes =="
LOGDIR="$HOME/Library/Application Support/syswatch/logs"
LATEST=$(ls -t "$LOGDIR"/*.jsonl 2>/dev/null | head -1)
if [ -z "$LATEST" ]; then bad "no log file written by --log"; else
  N=$(wc -l < "$LATEST")
  ESC=$(grep -c $'\033' "$LATEST" 2>/dev/null || true)
  VALID=$(head -3 "$LATEST" | python3 -c 'import json,sys;print(sum(1 for l in sys.stdin if json.loads(l)))' 2>/dev/null || echo 0)
  echo "  log=$LATEST lines=$N escapes=$ESC valid_json=$VALID"
  [ "$N" -ge 4 ] && ok "wrote $N samples" || bad "only $N samples"
  [ "$ESC" = "0" ] && ok "no ANSI escapes" || bad "$ESC lines carry escapes"
  [ "$VALID" = "3" ] && ok "JSON lines valid" || bad "invalid JSON lines"
fi

echo
[ $FAIL -eq 0 ] && echo "VERDICT: T10 PASSES" || echo "VERDICT: T10 FAILS — $FAIL check(s)"
exit $FAIL
