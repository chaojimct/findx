#!/usr/bin/env bash
# Build an official (unsigned unless a Developer ID is available) macOS app + dmg
# from the native GPUI GUI. Never invents a .dmg on Linux/Windows.
set -euo pipefail

usage() {
  cat <<USAGE
Usage: $(basename "$0") [--skip-build] [--release-dir DIR] [--out DIR] [--version V]
  Must be run on macOS. Refuses to emit a fake .dmg on any other OS.
USAGE
}

HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
SKIP_BUILD=0
RELEASE_DIR=
OUT=
VERSION=

while [[ $# -gt 0 ]]; do
  case "$1" in
    --skip-build) SKIP_BUILD=1; shift ;;
    --release-dir) RELEASE_DIR="${2:?}"; shift 2 ;;
    --out) OUT="${2:?}"; shift 2 ;;
    --version) VERSION="${2:?}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage; exit 2 ;;
  esac
done

if [[ "$(uname -s)" != Darwin ]]; then
  echo "pack-macos.sh must run on macOS; refusing to emit a fake .dmg from $(uname -s)" >&2
  exit 1
fi

case "$(uname -m)" in
  arm64|aarch64) ARCH=aarch64 ;;
  x86_64) ARCH=x64 ;;
  *) echo "unsupported macOS arch $(uname -m)" >&2; exit 1 ;;
esac

if [[ -z "$VERSION" ]]; then
  VERSION=$(python3 - <<PY
import re, pathlib
text = pathlib.Path("$REPO/Cargo.toml").read_text()
m = re.search(r'(?ms)\[workspace\.package\].*?^version\s*=\s*"([^"]+)"', text)
if not m:
    raise SystemExit("missing workspace version")
print(m.group(1))
PY
)
fi

if [[ -z "$RELEASE_DIR" ]]; then
  RELEASE_DIR="$REPO/target/release"
fi
if [[ -z "$OUT" ]]; then
  OUT="$REPO/dist"
fi
mkdir -p "$OUT"

if [[ $SKIP_BUILD -eq 0 ]]; then
  (cd "$REPO" && cargo build --release --locked -p findx2-cli -p findx2-service -p findx2-gui-native)
fi

need() {
  local p="$RELEASE_DIR/$1"
  if [[ ! -f "$p" ]]; then
    echo "missing $p" >&2
    exit 1
  fi
}
need findx2-gui-native
need findx2
need fx
need findx2-service

STAGE=$(mktemp -d /tmp/findx-macos-stage-XXXXXX)
cleanup() { rm -rf "$STAGE"; }
trap cleanup EXIT

APP="$STAGE/FindX.app"
MACOS="$APP/Contents/MacOS"
RES="$APP/Contents/Resources"
mkdir -p "$MACOS" "$RES"
install -m 0755 "$RELEASE_DIR/findx2-gui-native" "$MACOS/FindX"
install -m 0755 "$RELEASE_DIR/findx2" "$MACOS/findx2"
install -m 0755 "$RELEASE_DIR/fx" "$MACOS/fx"
install -m 0755 "$RELEASE_DIR/findx2-service" "$MACOS/findx2-service"
printf '1\r\n' > "$MACOS/FindX.installed"
printf '%s\n' "$VERSION" > "$MACOS/version"
sed -e "s|@VERSION@|$VERSION|g" "$HERE/Info.plist.in" > "$APP/Contents/Info.plist"
if [[ -f "$REPO/gui/src-tauri/icons/icon.icns" ]]; then
  cp "$REPO/gui/src-tauri/icons/icon.icns" "$RES/AppIcon.icns"
fi
sed -e "s|@PREFIX@|/Applications/FindX.app/Contents/MacOS|g" \
  "$HERE/findx2.plist.in" > "$RES/com.findx.findx2.plist"

# Sign only when a real Developer ID is present. Unsigned is still a real .app.
SIGNED=0
if security find-identity -v -p codesigning 2>/dev/null | grep -q 'Developer ID Application'; then
  IDENTITY=$(security find-identity -v -p codesigning | awk -F'\"' '/Developer ID Application/ {print $2; exit}')
  if [[ -n "$IDENTITY" ]]; then
    codesign --force --sign "$IDENTITY" --timestamp --options runtime \
      "$MACOS/FindX" "$MACOS/findx2" "$MACOS/fx" "$MACOS/findx2-service" "$APP" || true
    SIGNED=1
  fi
fi

VOL="FindX ${VERSION}"
DMG_TMP="$STAGE/rw.dmg"
DMG_PATH="$OUT/FindX-${VERSION}-macos-${ARCH}.dmg"
rm -f "$DMG_TMP" "$DMG_PATH"
# Place install.sh beside the .app so the dmg is self-contained.
DMG_SRC="$STAGE/dmgroot"
mkdir -p "$DMG_SRC"
cp -R "$APP" "$DMG_SRC/FindX.app"
cp "$HERE/install.sh" "$DMG_SRC/install.sh"
chmod 0755 "$DMG_SRC/install.sh"
hdiutil create -volname "$VOL" -srcfolder "$DMG_SRC" -ov -format UDRW "$DMG_TMP" >/dev/null
# Add an Applications shortcut for the standard drag-install UX.
MOUNT=$(hdiutil attach -readwrite -noverify -nobrowse "$DMG_TMP" | awk '/\/Volumes\// {print $NF; exit}')
if [[ -n "$MOUNT" ]]; then
  ln -s /Applications "$MOUNT/Applications" || true
  hdiutil detach "$MOUNT" >/dev/null
fi
hdiutil convert "$DMG_TMP" -format UDZO -imagekey zlib-level=9 -o "$DMG_PATH" >/dev/null

# Guard: this job must never emit a Windows setup.exe
if compgen -G "$OUT/FindX-*-setup.exe" > /dev/null; then
  echo "refusing to leave a FindX-*-setup.exe next to macOS packages" >&2
  exit 1
fi

(
  cd "$OUT"
  shasum -a 256 "$(basename "$DMG_PATH")" > "FindX-${VERSION}-macos-${ARCH}.sha256"
)
echo "Official macOS package (signed=$SIGNED, unsigned is expected without a Developer ID):"
ls -l "$DMG_PATH"
cat "$OUT/FindX-${VERSION}-macos-${ARCH}.sha256"
