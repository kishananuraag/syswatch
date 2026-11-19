#!/bin/bash
# Supervisor acceptance for T5 — the stats viewer: parity table + graphs.
cd ~/Documents/projects/syswatch || exit 1
FAIL=0
ok(){ echo "  PASS  $1"; }
bad(){ echo "  FAIL  $1"; FAIL=$((FAIL+1)); }

echo "== 1. compiles =="
python3 -m py_compile dashboard/server.py 2>/dev/null && ok "server.py" || bad "server.py syntax"
python3 -m py_compile scripts/parity_check.py 2>/dev/null && ok "parity_check.py" || bad "parity_check.py missing/bad"

echo "== 2. parity table =="
if [ -f docs/parity.md ]; then
  ROWS=$(grep -c '^|' docs/parity.md)
  UNTESTED=$(grep -ci "untested" docs/parity.md)
  echo "  rows=$ROWS untested_mentions=$UNTESTED"
  [ "$ROWS" -ge 10 ] && ok "parity.md has $ROWS table rows" || bad "parity.md too thin ($ROWS rows)"
else
  bad "docs/parity.md missing"
fi

echo "== 3. fixture replay mode on :8125 =="
# Pre-clean: a leftover server from a previous run makes the single-instance
# guard print ALREADY_RUNNING and exit 0, which would look like a broken fixture.
for P in $(lsof -tnP -iTCP:8125 -sTCP:LISTEN 2>/dev/null); do kill -9 "$P" 2>/dev/null; done
pkill -f "SYSWATCH_PORT=8125" 2>/dev/null; sleep 1
( SYSWATCH_FIXTURE=fixtures/istat/sample.jsonl SYSWATCH_PORT=8125 python3 dashboard/server.py > /tmp/t5.log 2>&1 & )
sleep 6
H=$(curl -s -m 5 http://127.0.0.1:8125/api/health)
echo "$H" | grep -q '"ok": *true' && ok "health ok:true (fixture)" || bad "health: $H"
curl -s -m 5 http://127.0.0.1:8125/api/current | python3 -c 'import json,sys;d=json.load(sys.stdin);assert d.get("cpu"),"no cpu";print("  current keys:",len(d))' 2>/dev/null && ok "/api/current serves cpu from fixture" || bad "/api/current fixture path"
G=$(curl -s -m 5 "http://127.0.0.1:8125/api/graph?metric=cpu")
echo "$G" | python3 -c 'import json,sys;d=json.load(sys.stdin);s=d.get("series") or d.get("points");assert s and len(s)>=200;print("  cpu graph points:",len(s))' 2>/dev/null && ok "/api/graph cpu >=200 points" || bad "/api/graph: $G"
SVG=$(curl -s -m 5 http://127.0.0.1:8125/ | grep -ciE "svg|canvas")
[ "$SVG" -ge 1 ] && ok "page renders graphs ($SVG markers)" || bad "no svg/canvas in page"

echo "== 4. existing endpoints unbroken =="
curl -s -m 5 http://127.0.0.1:8125/api/agent-events | head -c 200 | grep -q . && ok "agent-events still serves" || bad "agent-events broken"
pkill -TERM -f "SYSWATCH_PORT=8125" 2>/dev/null; pkill -TERM -f "dashboard/server.py" 2>/dev/null; sleep 2

echo "== 5. live path unchanged (no fixture) on :8126 =="
( SYSWATCH_PORT=8126 python3 dashboard/server.py > /tmp/t5live.log 2>&1 & )
sleep 6
LIVE=$(curl -s -m 5 http://127.0.0.1:8126/api/health)
echo "$LIVE" | grep -q '"ok"' && ok "live health answers" || bad "live health: $LIVE"
# Port-scoped cleanup only. A bare `pkill -f dashboard/server.py` would also
# kill the launchd-managed dashboard on 8123 — that happened once.
for PORT in 8125 8126; do
  for P in $(lsof -tnP -iTCP:$PORT -sTCP:LISTEN 2>/dev/null); do kill -TERM "$P" 2>/dev/null; done
done
sleep 1

echo "== 6. parity checker runs =="
python3 scripts/parity_check.py --fixture fixtures/istat/sample.jsonl --out /tmp/parity_check.md > /tmp/parity_run.log 2>&1 \
  && ok "checker exits 0" || bad "checker failed: $(tail -1 /tmp/parity_run.log)"

echo
[ $FAIL -eq 0 ] && echo "VERDICT: T5 PASSES" || echo "VERDICT: T5 FAILS — $FAIL check(s)"
exit $FAIL
