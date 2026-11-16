#!/usr/bin/env bash
# Build official Linux packages (native GPUI GUI, not Tauri/WebView).
# Produces FindX-${VERSION}-linux-x64.deb and FindX-${VERSION}-linux-x64.tar.gz.
# Never emits FindX-*-setup.exe. Does not publish or tag a GitHub release.
set -euo pipefail

usage() {
  cat <<USAGE
Usage: $(basename "$0") [--skip-build] [--release-dir DIR] [--out DIR] [--version V]
  --skip-build    Reuse binaries already in --release-dir
  --release-dir   Directory containing findx2-gui-native, findx2, fx, findx2-service
  --out           Output directory for packages (default: <repo>/dist)
  --version       Override product version (default: workspace Cargo.toml)
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

if [[ "$(uname -s)" != Linux ]]; then
  echo "pack-linux.sh must run on Linux; refusing to invent a Linux package on $(uname -s)" >&2
  exit 1
fi
case "$(uname -m)" in
  x86_64|amd64) ARCH=x64 ;;
  *)
    echo "official Linux packages are currently produced only for x86_64, not $(uname -m)" >&2
    exit 1
    ;;
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
    echo "missing $p ; build the official binaries first or pass --release-dir" >&2
    exit 1
  fi
}
need findx2-gui-native
need findx2
need fx
need findx2-service

STAGE=$(mktemp -d /tmp/findx-linux-stage-XXXXXX)
cleanup() { rm -rf "$STAGE"; }
trap cleanup EXIT

install -m 0755 "$RELEASE_DIR/findx2-gui-native" "$STAGE/FindX"
install -m 0755 "$RELEASE_DIR/findx2" "$STAGE/findx2"
install -m 0755 "$RELEASE_DIR/fx" "$STAGE/fx"
install -m 0755 "$RELEASE_DIR/findx2-service" "$STAGE/findx2-service"
printf '1\r\n' > "$STAGE/FindX.installed"
printf '%s\n' "$VERSION" > "$STAGE/version"
cp "$HERE/install.sh" "$HERE/uninstall.sh" "$STAGE/"
chmod 0755 "$STAGE/install.sh" "$STAGE/uninstall.sh"
sed -e "s|@PREFIX@|/opt/FindX|g" -e "s|@VERSION@|$VERSION|g" \
  "$HERE/findx.desktop.in" > "$STAGE/findx.desktop"
sed -e "s|@PREFIX@|/opt/FindX|g" "$HERE/findx2.service.in" > "$STAGE/findx2.service"
cp "$HERE/findx.desktop.in" "$HERE/findx2.service.in" "$STAGE/"
mkdir -p "$STAGE/icons"
ICON=
for cand in \
  "$REPO/gui/src-tauri/icons/128x128.png" \
  "$REPO/gui/src-tauri/icons/icon.png"
 do
  if [[ -f "$cand" ]]; then ICON=$cand; break; fi
done
if [[ -z "$ICON" ]]; then
  echo "missing app icon under gui/src-tauri/icons" >&2
  exit 1
fi
cp "$ICON" "$STAGE/icons/findx.png"
cp "$ICON" "$STAGE/findx.png"

# --- tarball (pkgformat=tar) ---
TAR_DIR="FindX-${VERSION}-linux-${ARCH}"
TAR_STAGE=$(mktemp -d /tmp/findx-linux-tar-XXXXXX)
mkdir -p "$TAR_STAGE/$TAR_DIR"
cp -a "$STAGE"/. "$TAR_STAGE/$TAR_DIR/"
printf 'tar\n' > "$TAR_STAGE/$TAR_DIR/FindX.pkgformat"
TAR_PATH="$OUT/FindX-${VERSION}-linux-${ARCH}.tar.gz"
tar -C "$TAR_STAGE" -czf "$TAR_PATH" "$TAR_DIR"
rm -rf "$TAR_STAGE"

# --- deb (pkgformat=deb) ---
DEB_ROOT=$(mktemp -d /tmp/findx-linux-deb-XXXXXX)
printf 'deb\n' > "$STAGE/FindX.pkgformat"
DESTDIR="$DEB_ROOT" "$STAGE/install.sh" --prefix /opt/FindX --stage "$STAGE" --destdir "$DEB_ROOT"
# install.sh already dropped desktop/unit/symlinks into DESTDIR
mkdir -p "$DEB_ROOT/DEBIAN"
SIZE_KB=$(du -sk "$DEB_ROOT" | awk '{print $1}')
# Runtime Depends are filled from ldd against the staged GUI after packaging tests
# can refine them; keep a conservative Debian 13 / Ubuntu 24.04-friendly set.
cat > "$DEB_ROOT/DEBIAN/control" <<CTRL
Package: findx
Version: ${VERSION}
Section: utils
Priority: optional
Architecture: amd64
Maintainer: FindX maintainers <https://github.com/chaojimct/findx>
Homepage: https://github.com/chaojimct/findx
Installed-Size: ${SIZE_KB}
Depends: libc6, libgcc-s1, libxcb1, libxcb-xkb1, libxkbcommon0, libxkbcommon-x11-0, libx11-6, libxcursor1, libxi6, libxrandr2, libwayland-client0, libvulkan1
Description: FindX local file search (official native GUI)
 Official FindX native GPUI client, CLI, and user-session index service.
 This package does not ship the retired Tauri/WebView client.
CTRL
cat > "$DEB_ROOT/DEBIAN/postinst" <<'POST'
#!/bin/sh
set -e
target_user=${SUDO_USER:-}
if [ -n "$target_user" ] && [ "$target_user" != root ]; then
  uid=$(id -u "$target_user" 2>/dev/null || true)
  runtime=/run/user/${uid:-}
  if [ -n "$uid" ] && [ -d "$runtime" ] && command -v systemctl >/dev/null; then
    # Enable but do not start: first start may begin a full-disk index.
    sudo -u "$target_user" XDG_RUNTIME_DIR="$runtime" systemctl --user daemon-reload || true
    sudo -u "$target_user" XDG_RUNTIME_DIR="$runtime" systemctl --user enable findx2.service || true
  fi
fi
if command -v update-desktop-database >/dev/null; then
  update-desktop-database -q /usr/share/applications || true
fi
if command -v gtk-update-icon-cache >/dev/null; then
  gtk-update-icon-cache -q /usr/share/icons/hicolor || true
fi
POST
cat > "$DEB_ROOT/DEBIAN/prerm" <<'PRERM'
#!/bin/sh
set -e
target_user=${SUDO_USER:-}
if [ -n "$target_user" ] && [ "$target_user" != root ]; then
  uid=$(id -u "$target_user" 2>/dev/null || true)
  runtime=/run/user/${uid:-}
  if [ -n "$uid" ] && [ -d "$runtime" ] && command -v systemctl >/dev/null; then
    sudo -u "$target_user" XDG_RUNTIME_DIR="$runtime" systemctl --user disable --now findx2.service || true
  fi
fi
PRERM
chmod 0755 "$DEB_ROOT/DEBIAN/postinst" "$DEB_ROOT/DEBIAN/prerm"
# Root-owned package metadata is expected; files inside can stay as the builder.
DEB_PATH="$OUT/FindX-${VERSION}-linux-${ARCH}.deb"
dpkg-deb --build --root-owner-group "$DEB_ROOT" "$DEB_PATH" >/dev/null
rm -rf "$DEB_ROOT"

# Refuse to have produced a Windows setup name from this script.
shopt -s nullglob
bad=("$OUT"/FindX-*-setup.exe)
if [[ ${#bad[@]} -gt 0 ]]; then
  echo "refusing to leave a FindX-*-setup.exe next to Linux packages: ${bad[*]}" >&2
  exit 1
fi

(
  cd "$OUT"
  sha256sum "FindX-${VERSION}-linux-${ARCH}.deb" "FindX-${VERSION}-linux-${ARCH}.tar.gz" \
    > "FindX-${VERSION}-linux-${ARCH}.sha256"
)
echo "Official Linux packages:"
ls -l "$OUT/FindX-${VERSION}-linux-${ARCH}.deb" "$OUT/FindX-${VERSION}-linux-${ARCH}.tar.gz"
cat "$OUT/FindX-${VERSION}-linux-${ARCH}.sha256"
