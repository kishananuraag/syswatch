#!/bin/bash
# Idempotent LaunchAgent install for syswatch (macOS user domain, no sudo).
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
HOME_DIR="$HOME"
LAUNCHD_DIR="$HOME/Library/LaunchAgents"
LOG_DIR="$HOME/Library/Logs/syswatch"
SUPPORT_DIR="$HOME/Library/Application Support/syswatch"

APP_BIN="$HOME/Applications/Syswatch.app/Contents/MacOS/syswatch"
if [ ! -x "$APP_BIN" ]; then
    echo "Menu-bar app not found at $APP_BIN"
    echo "Run: bash scripts/build_app.sh"
    exit 1
fi

# Ensure the release binary exists.
RELEASE_BIN="$REPO_ROOT/target/release/syswatch"
if [ ! -x "$RELEASE_BIN" ]; then
    echo "Building release binary..."
    (cd "$REPO_ROOT" && cargo build --release)
fi
[ -x "$RELEASE_BIN" ] || { echo "No binary at $RELEASE_BIN"; exit 1; }

# Use a distinct executable name for the collector so that
# "pgrep -x syswatch" only matches the menu-bar app.
COLLECTOR_BIN="$SUPPORT_DIR/syswatch-collector"
if [ ! -x "$COLLECTOR_BIN" ] || [ "$RELEASE_BIN" -nt "$COLLECTOR_BIN" ]; then
    cp -f "$RELEASE_BIN" "$COLLECTOR_BIN"
    chmod +x "$COLLECTOR_BIN"
fi

# Ensure log / data directories exist.
mkdir -p "$LOG_DIR"
mkdir -p "$SUPPORT_DIR/logs"
mkdir -p "$SUPPORT_DIR/history"
mkdir -p "$SUPPORT_DIR/data"

# Stop any manually launched menu-bar instance so launchd owns the only one.
pkill -f "syswatch --menubar" 2>/dev/null || true
sleep 1

# Install plists from templates.
mkdir -p "$LAUNCHD_DIR"
for template in "$REPO_ROOT/packaging/launchd"/com.barbarian.syswatch*.plist; do
    name=$(basename "$template")
    # T15: com.barbarian.syswatch.power.plist is a ROOT LaunchDaemon (system
    # domain), not a user agent. Bootstrapping it here would run powermetrics
    # as this user, which it refuses ("must be invoked as the superuser").
    # It is installed ONLY by the dashboard grant flow (one admin prompt):
    #   POST /api/permissions/grant?id=package_power
    if [ "$name" = "com.barbarian.syswatch.power.plist" ]; then
        echo "Skipped $name (root LaunchDaemon — grant it from the Permissions page)"
        continue
    fi
    dest="$LAUNCHD_DIR/$name"
    sed "s|__HOME__|$HOME_DIR|g" "$template" > "$dest"
    chmod 644 "$dest"
    echo "Installed $dest"
done

# Reload (bootout first to stay idempotent).
UID_N=$(id -u)
for label in com.barbarian.syswatch com.barbarian.syswatch.collector com.barbarian.syswatch.dashboard; do
    launchctl bootout "gui/$UID_N/$label" 2>/dev/null || true
done
sleep 1
for plist in "$LAUNCHD_DIR"/com.barbarian.syswatch*.plist; do
    launchctl bootstrap "gui/$UID_N" "$plist"
done

# Wait for the dashboard health endpoint.
for i in $(seq 1 30); do
    if curl -s -m 1 http://127.0.0.1:8123/api/health >/dev/null 2>&1; then
        break
    fi
    sleep 1
done

# Verify.
echo "== launchd state =="
for label in com.barbarian.syswatch com.barbarian.syswatch.collector com.barbarian.syswatch.dashboard; do
    launchctl print "gui/$UID_N/$label" 2>/dev/null | grep -E "state =|program =" || echo "$label: not loaded"
done

echo "== health =="
curl -s -m 5 http://127.0.0.1:8123/api/health | python3 -m json.tool

# ============================================================================
# NOTES — the ROOT tier is NOT installed by this script (T15)
# ============================================================================
# Everything above is the T8 path: user-domain LaunchAgents, no sudo, no
# privilege. The elevated powermetrics sampler is a separate, optional ROOT
# LaunchDaemon and it is granted from the dashboard UI, one admin prompt,
# never automatically:
#
#   open http://127.0.0.1:8123/#permissions   →  "Grant" on Package power
#   (equivalent API call: POST /api/permissions/grant?id=package_power)
#   (uninstall:           POST /api/permissions/revoke?id=package_power)
#
# The exact privileged snippet the grant flow runs — kept here as documentation
# so the install path can be audited without reading server.py. It is executed
# by macOS itself through `do shell script ... with administrator privileges`
# (osascript); the password goes to the window server, never to syswatch.
#
#   /bin/mkdir -p /usr/local/libexec/syswatch /Library/LaunchDaemons
#   /usr/bin/install -m 755 -o root -g wheel \
#       target/release/syswatch-powerd \
#       /usr/local/libexec/syswatch/syswatch-powerd
#   /usr/bin/install -m 644 -o root -g wheel \
#       "$HOME/Library/Application Support/syswatch/root/com.barbarian.syswatch.power.plist" \
#       /Library/LaunchDaemons/com.barbarian.syswatch.power.plist
#   /usr/bin/touch "$HOME/Library/Logs/syswatch/power-daemon.log"
#   /bin/chmod 644 "$HOME/Library/Logs/syswatch/power-daemon.log"
#   /bin/launchctl bootout system/com.barbarian.syswatch.power 2>/dev/null || true
#   /bin/launchctl bootstrap system /Library/LaunchDaemons/com.barbarian.syswatch.power.plist
#
# T19: the installed artefact is ONE Rust binary (built beforehand, as the
# user — never compiled as root). Before T19 this step also copied
# power_sampler.sh and ran clang on smc_temp.c; both are gone.
#
# powermetrics flags as verified on macOS 15.8.3 (`powermetrics -h`):
#   -i <ms>          sample interval in MILLISECONDS (5000 = 5 s)
#   -n 1             one sample, then exit (the wrapper loops)
#   -f text          output format  (NOT a file — that is -o <file>)
#   -s <samplers>    thermal,cpu_power,gpu_power,ane_power (+ smc when supported)
# The wrapper writes ONE JSON snapshot per sample to
#   ~/Library/Application Support/syswatch/root/sample.json   (mode 644)
# and dumps its first real sample to powermetrics.raw.log. A snapshot older
# than 30 s is treated as absent by the dashboard — never shown as live.
