#!/usr/bin/env bash
# Remove binaries, never per-user index/settings/backups. Run as the login user.
set -euo pipefail
[[ "$(uname -s)" == Darwin && "$(id -u)" != 0 ]] || { echo 'Run on macOS as the login user' >&2; exit 1; }
# bootout success or an unloaded service is acceptable; other failures stop removal.
if launchctl print "gui/$(id -u)/com.findx.findx2" >/dev/null 2>&1; then
  launchctl bootout "gui/$(id -u)/com.findx.findx2"
fi
plist="$HOME/Library/LaunchAgents/com.findx.findx2.plist"
# Keep service arguments as a restorable backup, but remove the login registration.
if [[ -f "$plist" ]]; then mv "$plist" "$plist.uninstalled-$(date +%s)-$$"; fi
osascript -e 'do shell script "test -f /Applications/FindX.app/Contents/MacOS/FindX.installed && rm -rf /Applications/FindX.app" with administrator privileges'
echo 'FindX removed. All index/settings and LaunchAgent backup retained.'
