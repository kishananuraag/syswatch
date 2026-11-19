#!/bin/bash
# Reverse scripts/install.sh cleanly. No sudo, user domain only.
set -euo pipefail

UID_N=$(id -u)
LAUNCHD_DIR="$HOME/Library/LaunchAgents"

for label in com.barbarian.syswatch.dashboard com.barbarian.syswatch.collector com.barbarian.syswatch; do
    launchctl bootout "gui/$UID_N/$label" 2>/dev/null || true
    rm -f "$LAUNCHD_DIR/$label.plist"
    echo "Removed $label"
done

echo "Syswatch LaunchAgents removed."
