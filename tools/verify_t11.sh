#!/bin/bash
# Supervisor acceptance for T11 — analytics revamp.
cd ~/Documents/projects/syswatch || exit 1
FAIL=0
ok(){ echo "  PASS  $1"; }
bad(){ echo "  FAIL  $1"; FAIL=$((FAIL+1)); }

echo "== 1. compiles =="
python3 -m py_compile dashboard/server.py 2>/dev/null && ok "server.py" || bad "server.py"

echo "== 2. fixture replay on :8128 =="
for P in $(lsof -tnP -iTCP:8128 -sTCP:LISTEN 2>/dev/null); do kill -9 "$P" 2>/dev/null; done; sleep 1
( SYSWATCH_FIXTURE=fixtures/istat/sample.jsonl SYSWATCH_PORT=8128 python3 dashboard/server.py > /tmp/t11.log 2>&1 & )
sleep 6
curl -s -m 5 http://127.0.0.1:8128/api/health | grep -q '"ok": *true' && ok "health ok" || bad "health"

echo "== 3. tick ladder per range =="
curl -s -m 5 "http://127.0.0.1:8128/api/graph?metric=cpu&range=1y" | python3 -c '
import json,sys; d=json.load(sys.stdin); t=d.get("ticks") or []
assert 3<=len(t)<=12, len(t)
import re
assert not re.match(r"^\d+$", t[0]["label"]), "raw epoch label"
print("  1y ticks:", len(t), [x["label"] for x in t])' 2>/dev/null && ok "1y → month names" || bad "1y ladder"
curl -s -m 5 "http://127.0.0.1:8128/api/graph?metric=cpu&range=7d" | python3 -c '
import json,sys,re; d=json.load(sys.stdin); t=d.get("ticks") or []
assert 3<=len(t)<=10, len(t)
assert re.match(r"^[A-Za-z]{3} ?\d", t[0]["label"]), t[0]["label"]
print("  7d ticks:", len(t), [x["label"] for x in t])' 2>/dev/null && ok "7d → weekday+date on day boundaries" || bad "7d ladder"
curl -s -m 5 "http://127.0.0.1:8128/api/graph?metric=cpu&range=1d" | python3 -c '
import json,sys,re; d=json.load(sys.stdin); t=d.get("ticks") or []
assert 4<=len(t)<=10, len(t)
assert re.match(r"^\d{1,2}:\d{2}", t[0]["label"]), t[0]["label"]
print("  1d ticks:", len(t), [x["label"] for x in t])' 2>/dev/null && ok "1d → HH:00 times" || bad "1d ladder"
curl -s -m 5 "http://127.0.0.1:8128/api/graph?metric=cpu&range=1m" | python3 -c '
import json,sys; d=json.load(sys.stdin); t=d.get("ticks") or []
assert 3<=len(t)<=6, len(t)
print("  1m ticks:", len(t), [x["label"] for x in t])' 2>/dev/null && ok "1m → seconds" || bad "1m ladder"

echo "== 4. peak chip data =="
curl -s -m 5 "http://127.0.0.1:8128/api/graph?metric=net&range=1d" | python3 -c '
import json,sys; d=json.load(sys.stdin); p=d.get("peak")
assert p and "value" in p and "ts" in p and "is_spike" in p, p
print("  peak:", p)' 2>/dev/null && ok "peak object present" || bad "peak missing"

echo "== 5. power endpoint (honesty) =="
curl -s -m 5 http://127.0.0.1:8128/api/power | python3 -c '
import json,sys; d=json.load(sys.stdin)
print("  battery_w:", d.get("battery_w"), "| charging:", d.get("charging"), "| by_app rows:", len(d.get("by_app") or []))
assert "battery_w" in d
if d.get("package"):
    for k,v in d["package"].items(): assert v is None or isinstance(v,(int,float)), k
print("  package nulls are null, not 0")' 2>/dev/null && ok "/api/power shape" || bad "/api/power"

echo "== 6. UI mentions peak/power, no CDN =="
PAGE=$(curl -s -m 5 http://127.0.0.1:8128/)
echo "$PAGE" | grep -qi peak && ok "peak in UI" || bad "peak absent from UI"
echo "$PAGE" | grep -qi power && ok "power in UI" || bad "power absent from UI"
echo "$PAGE" | grep -qiE "cdn\.|unpkg|jsdelivr" && bad "CDN reference found" || ok "no CDN"

echo "== 7. values exist, gaps are gaps =="
for M in cpu ram net; do
curl -s -m 5 "http://127.0.0.1:8128/api/graph?metric=$M&range=7d" | python3 -c '
import json,sys; d=json.load(sys.stdin); pts=d.get("points") or []
field = "cpu" if d.get("metric")=="cpu" or sys.argv[1]=="cpu" else None
key = {"cpu":"cpu","ram":"ram","net":"down_bps"}[sys.argv[1]]
nonnull=[p.get(key) for p in pts if isinstance(p.get(key),(int,float))]
nulls=[p for p in pts if p.get(key) is None]
print(f"  {sys.argv[1]}: points={len(pts)} numeric={len(nonnull)} gaps={len(nulls)}")
assert len(nonnull) > 0, "series has NO numeric values"
' "$M" 2>/dev/null && ok "$M has real values" || bad "$M series empty (value mapping)"
done

echo "== 8. port-scoped shutdown; live path untouched =="
for P in $(lsof -tnP -iTCP:8128 -sTCP:LISTEN 2>/dev/null); do kill -TERM "$P" 2>/dev/null; done
sleep 2
curl -s -m 5 http://127.0.0.1:8123/api/health | grep -q '"ok": *true' && ok "launchd dashboard on 8123 untouched" || bad "8123 health"
echo "$P" >/dev/null; pgrep -f "dashboard/server.py" | wc -l | xargs echo "  (dashboard/server.py processes still alive: $(pgrep -f 'dashboard/server.py' | wc -l | tr -d ' '))"

echo
[ $FAIL -eq 0 ] && echo "VERDICT: T11 PASSES" || echo "VERDICT: T11 FAILS — $FAIL check(s)"
exit $FAIL
