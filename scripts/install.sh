#!/bin/bash
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
PLIST_SRC="$PROJECT_DIR/scripts/com.sa_rebalance.daily.plist"
PLIST_DST="$HOME/Library/LaunchAgents/com.sa_rebalance.daily.plist"
LOG_DIR="$HOME/.local/state/sa_rebalance"

echo "Building release binary..."
(cd "$PROJECT_DIR" && cargo build --release)

echo "Creating log directory at $LOG_DIR"
mkdir -p "$LOG_DIR"

echo "Generating plist at $PLIST_DST (substituting __HOME__ and __PROJECT_DIR__)"
sed -e "s|__PROJECT_DIR__|$PROJECT_DIR|g" -e "s|__HOME__|$HOME|g" "$PLIST_SRC" > "$PLIST_DST"

echo "Unloading any existing job..."
launchctl unload "$PLIST_DST" 2>/dev/null || true

echo "Loading job..."
launchctl load "$PLIST_DST"

echo
echo "Installed. Will run weekdays at 7:30 MT (= 9:30 ET)."
echo "To verify: launchctl list | grep sa_rebalance"
echo "To uninstall: launchctl unload $PLIST_DST && rm $PLIST_DST"
echo "Logs: $LOG_DIR/launchd.out.log, $LOG_DIR/launchd.err.log"
