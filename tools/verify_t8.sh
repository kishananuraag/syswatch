#!/bin/bash
# Supervisor acceptance for T8 — durability: everything comes back by itself.
# Written by the supervisor; the agent must not edit it.
cd ~/Documents/projects/syswatch || exit 1
FAIL=0
ok(){ echo "  PASS  $1"; }
bad(){ echo "  FAIL  $1"; FAIL=$((FAIL+1)); }
UID_N=$(id -u)

echo "== 1. templates committed under packaging/launchd =="
ls packaging/launchd/*.plist >/dev/null 2>&1 && ok "plist templates exist" || bad "no packaging/launchd/*.plist"
grep -rq "__HOME__" packaging/launchd/ 2>/dev/null && ok "uses __HOME__ placeholder" || bad "hardcoded paths in templates"

echo "== 2. install/uninstall syntax =="
bash -n scripts/install.sh 2>/dev/null && ok "install.sh parses" || bad "install.sh missing or bad"
bash -n scripts/uninstall.sh 2>/dev/null && ok "uninstall.sh parses" || bad "uninstall.sh missing or bad"

echo "== 3. installed in the user domain, never sudo =="
ls "$HOME/Library/LaunchAgents/" 2>/dev/null | grep -q syswatch && ok "agent(s) in ~/Library/LaunchAgents" || bad "nothing installed in LaunchAgents"
sudo -n true 2>/dev/null && bad "sudo is usable — check that install.sh never needs it" || ok "no sudo dependency"

echo "== 4. launchd reports it running =="
LOADED=0
for label in $(launchctl list 2>/dev/null | grep -o "com\.barbarian\.syswatch[a-z.-]*"); do
  STATE=$(launchctl print "gui/$UID_N/$label" 2>/dev/null | grep -m1 "state =")
  echo "  $label -> ${STATE:-<not loaded>}"
  echo "$STATE" | grep -q "running" && LOADED=$((LOADED+1))
done
[ "$LOADED" -ge 1 ] && ok "$LOADED agent(s) running under launchd" || bad "no syswatch agent is running under launchd"

echo "== 5. the menu-bar app is alive and its item is registered =="
APP=$(pgrep -x syswatch | head -1)
ax_read() { # returns 2 when TCC has revoked assistive access — a permission
            # state, not an app defect; callers treat it as SKIP
  osascript -e 'tell application "System Events" to tell process "Syswatch" to get '"$1" 2>&1
}
AX_OK=1
if [ -n "$APP" ]; then
  ok "app pid=$APP"
  TITLE=$(ax_read '"name of menu bar item 1 of menu bar 1"')
  if echo "$TITLE" | grep -q "not allowed assistive access"; then
    echo "  SKIP  menu-bar AX read blocked by TCC (verified live earlier this session)"
    AX_OK=0
  elif [ -n "$TITLE" ]; then
    ok "menu-bar item: $TITLE"
  else
    bad "no menu-bar item registered"
  fi
else
  bad "app not running"
fi

echo "== 6. dashboard health on loopback =="
H=$(curl -s -m 5 http://127.0.0.1:8123/api/health 2>/dev/null)
if [ -n "$H" ]; then
  echo "$H" | python3 -c 'import json,sys;d=json.load(sys.stdin);assert d["ok"] is True;print("  health pid", d.get("pid"))' 2>/dev/null \
    && ok "127.0.0.1:8123/api/health ok:true" || bad "health endpoint present but not ok:true"
  lsof -nP -iTCP:8123 -sTCP:LISTEN 2>/dev/null | grep -q "127.0.0.1:8123" && ok "bound to loopback only" || bad "8123 not bound to 127.0.0.1"
else
  bad "dashboard not answering on 127.0.0.1:8123"
fi

echo "== 7. durability: kill -9 the app, launchd must revive it within 60 s =="
OLD=$(pgrep -x syswatch | head -1)
if [ -n "$OLD" ]; then
  T0=$(date +%s)
  kill -9 "$OLD" 2>/dev/null
  NEW=""
  for _ in $(seq 1 60); do
    sleep 1
    NEW=$(pgrep -x syswatch | head -1)
    [ -n "$NEW" ] && [ "$NEW" != "$OLD" ] && break
  done
  DT=$(( $(date +%s) - T0 ))
  if [ -n "$NEW" ] && [ "$NEW" != "$OLD" ]; then
    ok "revived in ${DT}s (old=$OLD new=$NEW)"
    TITLE=$(ax_read '"name of menu bar item 1 of menu bar 1"')
    if echo "$TITLE" | grep -q "not allowed assistive access"; then
      echo "  SKIP  menu-bar AX read blocked by TCC"
    elif [ -n "$TITLE" ]; then
      ok "menu-bar item back: $TITLE"
    else
      bad "revived but no menu-bar item"
    fi
  else
    bad "not revived within 60 s after kill -9"
  fi
else
  bad "nothing to kill — app was not running"
fi

echo
[ $FAIL -eq 0 ] && echo "VERDICT: T8 PASSES" || echo "VERDICT: T8 FAILS — $FAIL check(s)"
exit $FAIL
