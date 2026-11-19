#!/bin/bash
# Build + install the Syswatch menu-bar app from the current release binary.
# Idempotent: rebuilds the bundle in place, ad-hoc signs it, relaunches it.
set -euo pipefail
cd "$(dirname "$0")/.."

APP_NAME="Syswatch"
DEST="${DEST:-$HOME/Applications/$APP_NAME.app}"
BIN="target/release/syswatch"
INTERVAL_MS="${INTERVAL_MS:-2000}"

echo "== build =="
cargo build --release 2>/dev/null
[ -x "$BIN" ] || { echo "no binary at $BIN"; exit 1; }

echo "== bundle -> $DEST =="
pkill -x syswatch 2>/dev/null || true
sleep 0.5
rm -rf "$DEST"
mkdir -p "$DEST/Contents/MacOS" "$DEST/Contents/Resources"
cp packaging/macos/Info.plist "$DEST/Contents/Info.plist"
cp "$BIN" "$DEST/Contents/MacOS/syswatch"
chmod +x "$DEST/Contents/MacOS/syswatch"

# Ad-hoc signature: required for a stable TCC/launch identity on Apple Silicon.
codesign --force --sign - --timestamp=none "$DEST" 2>&1 | tail -1

echo "== launch =="
# The app is owned by the LaunchAgent com.barbarian.syswatch, whose KeepAlive
# restarts it after the pkill above.  Launching a second copy with `open -a`
# while that happens gives TWO status items (seen 2026-08-19 and 2026-11-19),
# so wait for launchd first and only fall back to `open` if nothing came back.
PID=""
for _ in $(seq 1 15); do
    PID=$(pgrep -x syswatch | head -1)
    [ -n "$PID" ] && break
    sleep 1
done
if [ -z "$PID" ]; then
    echo "   launchd did not restart it — launching directly"
    open -a "$DEST" --args --menubar --interval-ms "$INTERVAL_MS"
    sleep 3
    PID=$(pgrep -x syswatch | head -1)
fi
if [ -z "$PID" ]; then echo "LAUNCH_FAILED"; exit 1; fi
COUNT=$(pgrep -x syswatch | wc -l | tr -d ' ')
echo "RUNNING pid=$PID instances=$COUNT rss=$(ps -o rss= -p "$PID" | tr -d ' ')KB"
