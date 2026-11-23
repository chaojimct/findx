#!/usr/bin/env bash
# Native macOS install. User data and an existing LaunchAgent's options survive upgrades.
set -euo pipefail
[[ "$(uname -s)" == Darwin ]] || { echo 'macOS required' >&2; exit 1; }
[[ "$(id -u)" != 0 ]] || { echo 'Run as the login user; only the app copy requests administrator access.' >&2; exit 1; }
HERE=$(cd "$(dirname "$0")" && pwd)
APP="$HERE/FindX.app"
if [[ "${1:-}" == --app ]]; then APP="${2:?}"; fi
[[ -x "$APP/Contents/MacOS/FindX" && -f "$APP/Contents/MacOS/FindX.installed" ]] || { echo 'Invalid FindX.app' >&2; exit 1; }
quote() { printf "'%s'" "${1//\'/\'\\\'\'}"; }
# Stop/restart only this login user's loaded engine, never a process-name kill.
was_running=0
if launchctl print "gui/$(id -u)/com.findx.findx2" 2>/dev/null | grep -q 'state = running'; then
  was_running=1
  launchctl bootout "gui/$(id -u)/com.findx.findx2"
fi
restore_service() {
  if [[ "$was_running" -eq 1 ]]; then
    launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.findx.findx2.plist"
    launchctl kickstart "gui/$(id -u)/com.findx.findx2"
  fi
}
trap restore_service EXIT
# Fixed, quoted paths; never delete the old app before a complete replacement exists.
if [[ "$APP" != /Applications/FindX.app ]]; then
  command="set -eu; stage=\$(mktemp -d /Applications/.FindX-stage-XXXXXX); ditto $(quote "$APP") \"\$stage/FindX.app\"; backup=/Applications/FindX.backup-\$(date +%s)-\$\$.app; if test -e /Applications/FindX.app; then mv /Applications/FindX.app \"\$backup\"; fi; if ! mv \"\$stage/FindX.app\" /Applications/FindX.app; then test ! -d \"\$backup\" || mv \"\$backup\" /Applications/FindX.app; exit 1; fi; rmdir \"\$stage\""
  osascript -e 'on run argv' -e 'do shell script (item 1 of argv) with administrator privileges' -e 'end run' "$command"
fi
LAUNCH="$HOME/Library/LaunchAgents/com.findx.findx2.plist"
mkdir -p "$(dirname "$LAUNCH")"
if [[ ! -e "$LAUNCH" ]]; then
  cp /Applications/FindX.app/Contents/Resources/com.findx.findx2.plist "$LAUNCH"
  plutil -replace ProgramArguments.2 -string "$HOME/Library/Application Support/FindX/index.bin" "$LAUNCH"
fi
# Do not bootstrap at install: that would start a first full-disk scan unexpectedly.
echo 'Installed FindX. Open the app, then confirm Start service in Settings. User data/options preserved.'
open /Applications/FindX.app
