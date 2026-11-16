#!/usr/bin/env bash
# Copy FindX.app into /Applications and install the per-user LaunchAgent.
# Intended to run on macOS from the official .dmg (or a built .app).
set -euo pipefail

if [[ "$(uname -s)" != Darwin ]]; then
  echo "install.sh must run on macOS" >&2
  exit 1
fi

HERE=$(cd "$(dirname "$0")" && pwd)
APP=
if [[ -d "$HERE/FindX.app" ]]; then
  APP="$HERE/FindX.app"
elif [[ -d "$HERE/../FindX.app" ]]; then
  APP=$(cd "$HERE/.." && pwd)/FindX.app
elif [[ "$(basename "$HERE")" == "MacOS" && -d "$HERE/../../.." ]]; then
  APP=$(cd "$HERE/../../.." && pwd)
  # When this script is not inside the bundle, skip; only used if copied.
fi

if [[ -z "$APP" || ! -d "$APP/Contents/MacOS" ]]; then
  # Allow --app
  if [[ "${1:-}" == "--app" ]]; then
    APP="${2:?}"
  fi
fi
if [[ -z "$APP" || ! -x "$APP/Contents/MacOS/FindX" ]]; then
  echo "cannot find official FindX.app next to this script" >&2
  exit 1
fi

DEST=/Applications/FindX.app
if [[ "$APP" != "$DEST" ]]; then
  if [[ "$(id -u)" -eq 0 ]]; then
    rm -rf "$DEST"
    ditto "$APP" "$DEST"
  else
    osascript -e "do shell script \"rm -rf /Applications/FindX.app && ditto '$(printf %q "$APP")' /Applications/FindX.app\" with administrator privileges"
  fi
fi

# LaunchAgent with this user's home (launchd does not expand $HOME in ProgramArguments).
LAUNCH="$HOME/Library/LaunchAgents"
mkdir -p "$LAUNCH"
SRC="$DEST/Contents/Resources/com.findx.findx2.plist"
if [[ -f "$SRC" ]]; then
  sed -e "s|__HOME__|$HOME|g" "$SRC" > "$LAUNCH/com.findx.findx2.plist"
  launchctl bootout "gui/$(id -u)/com.findx.findx2" >/dev/null 2>&1 || true
  launchctl bootstrap "gui/$(id -u)" "$LAUNCH/com.findx.findx2.plist" || \
    launchctl load "$LAUNCH/com.findx.findx2.plist" || true
fi
echo "Installed official FindX.app to $DEST (LaunchAgent enabled, not a signed/notarized package unless already signed)."
