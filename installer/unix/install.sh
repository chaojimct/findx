#!/usr/bin/env bash
# Install an official FindX Linux stage (the directory this script lives in, or
# --stage DIR) into PREFIX (default /opt/FindX). Never touches a Windows
# installation or any path under Program Files.
set -euo pipefail

usage() {
  cat <<USAGE
Usage: $(basename "$0") [--prefix DIR] [--destdir DIR] [--stage DIR] [--no-service] [--no-symlinks]
  --prefix      Install root for binaries (default: /opt/FindX)
  --destdir     Optional staging root prepended to every installed path (package builds)
  --stage       Directory that already contains FindX, findx2, fx, findx2-service
  --no-service  Do not install or enable the systemd user unit
  --no-symlinks Do not create /usr/bin/findx, findx2, fx
USAGE
}

PREFIX=/opt/FindX
DESTDIR=
STAGE=
NO_SERVICE=0
NO_SYMLINKS=0
HERE=$(cd "$(dirname "$0")" && pwd)

while [[ $# -gt 0 ]]; do
  case "$1" in
    --prefix) PREFIX="${2:?}"; shift 2 ;;
    --destdir) DESTDIR="${2:?}"; shift 2 ;;
    --stage) STAGE="${2:?}"; shift 2 ;;
    --no-service) NO_SERVICE=1; shift ;;
    --no-symlinks) NO_SYMLINKS=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage; exit 2 ;;
  esac
done

# Never let a malformed prefix redirect writes to the filesystem root.
[[ "$PREFIX" = /* && "$PREFIX" != / && "$PREFIX" != *..* && "$PREFIX" != *$'\n'* ]] || { echo "unsafe prefix" >&2; exit 2; }
[[ -z "$DESTDIR" || ( "$DESTDIR" = /* && "$DESTDIR" != / ) ]] || { echo "unsafe destdir" >&2; exit 2; }

if [[ -z "$STAGE" ]]; then
  STAGE="$HERE"
fi
STAGE=$(cd "$STAGE" && pwd)

for req in FindX findx2 fx findx2-service; do
  if [[ ! -f "$STAGE/$req" ]]; then
    echo "stage is missing $req (looked in $STAGE)" >&2
    exit 1
  fi
done

dest() {
  # $1 = absolute path as it should appear on the installed system
  printf '%s%s' "$DESTDIR" "$1"
}

install_file() {
  local src=$1 abs=$2 mode=${3:-0755}
  local target
  target=$(dest "$abs")
  mkdir -p "$(dirname "$target")"
  # Rename a complete sibling file: an executing GUI/service must never be
  # truncated (ETXTBSY), and interrupted copies leave the old binary intact.
  local pending
  pending=$(mktemp "${target}.new-XXXXXX")
  if install -m "$mode" "$src" "$pending"; then
    mv -f "$pending" "$target"
  else
    rm -f "$pending"
    return 1
  fi
}

# Official layout
install_file "$STAGE/FindX" "$PREFIX/FindX" 0755
install_file "$STAGE/findx2" "$PREFIX/findx2" 0755
install_file "$STAGE/fx" "$PREFIX/fx" 0755
install_file "$STAGE/findx2-service" "$PREFIX/findx2-service" 0755

if [[ -f "$STAGE/FindX.installed" ]]; then
  install_file "$STAGE/FindX.installed" "$PREFIX/FindX.installed" 0644
else
  target=$(dest "$PREFIX/FindX.installed")
  mkdir -p "$(dirname "$target")"
  printf '1\r\n' > "$target"
  chmod 0644 "$target"
fi

if [[ -f "$STAGE/FindX.pkgformat" ]]; then
  install_file "$STAGE/FindX.pkgformat" "$PREFIX/FindX.pkgformat" 0644
else
  target=$(dest "$PREFIX/FindX.pkgformat")
  mkdir -p "$(dirname "$target")"
  if [[ -n "$DESTDIR" ]]; then
    printf 'deb\n' > "$target"
  else
    printf 'tar\n' > "$target"
  fi
  chmod 0644 "$target"
fi

if [[ -f "$STAGE/version" ]]; then
  install_file "$STAGE/version" "$PREFIX/version" 0644
fi

# Keep install/uninstall helpers next to the official binaries for tarball updates.
if [[ -f "$STAGE/install.sh" ]]; then
  install_file "$STAGE/install.sh" "$PREFIX/install.sh" 0755
fi
if [[ -f "$STAGE/uninstall.sh" ]]; then
  install_file "$STAGE/uninstall.sh" "$PREFIX/uninstall.sh" 0755
fi

VERSION=unknown
if [[ -f "$STAGE/version" ]]; then
  VERSION=$(tr -d '[:space:]' < "$STAGE/version")
fi

# Desktop entry and icon
desktop_src="$STAGE/findx.desktop.in"
if [[ ! -f "$desktop_src" && -f "$STAGE/findx.desktop.in" ]]; then
  desktop_src=$(mktemp)
  sed -e "s|@PREFIX@|$PREFIX|g" -e "s|@VERSION@|$VERSION|g" \
    "$STAGE/findx.desktop.in" > "$desktop_src"
  trap 'rm -f "$desktop_src"' EXIT
fi
if [[ -f "$desktop_src" ]]; then
  tmp=$(mktemp)
  sed -e "s|@PREFIX@|$PREFIX|g" -e "s|@VERSION@|$VERSION|g" "$desktop_src" > "$tmp"
  target=$(dest /usr/share/applications/findx.desktop)
  mkdir -p "$(dirname "$target")"
  install -m 0644 "$tmp" "$target"
  rm -f "$tmp"
fi

icon_src=
for cand in "$STAGE/icons/findx.png" "$STAGE/findx.png" "$STAGE/icon.png"; do
  if [[ -f "$cand" ]]; then
    icon_src=$cand
    break
  fi
done
if [[ -n "$icon_src" ]]; then
  install_file "$icon_src" /usr/share/icons/hicolor/128x128/apps/findx.png 0644
fi

if [[ $NO_SYMLINKS -eq 0 ]]; then
  mkdir -p "$(dest /usr/bin)"
  ln -sfn "$PREFIX/FindX" "$(dest /usr/bin/findx)"
  ln -sfn "$PREFIX/findx2" "$(dest /usr/bin/findx2)"
  ln -sfn "$PREFIX/fx" "$(dest /usr/bin/fx)"
fi

if [[ $NO_SERVICE -eq 0 ]]; then
  unit_src="$STAGE/findx2.service.in"
  if [[ ! -f "$unit_src" && -f "$STAGE/findx2.service.in" ]]; then
    unit_src=$(mktemp)
    sed -e "s|@PREFIX@|$PREFIX|g" "$STAGE/findx2.service.in" > "$unit_src"
  fi
  if [[ -f "$unit_src" ]]; then
    tmp=$(mktemp)
    sed -e "s|@PREFIX@|$PREFIX|g" "$unit_src" > "$tmp"
    target=$(dest /usr/lib/systemd/user/findx2.service)
    mkdir -p "$(dirname "$target")"
    install -m 0644 "$tmp" "$target"
    rm -f "$tmp"
  fi
fi

# Installing files must not change the invoking/root user's service state.
# In particular: no --now, no automatic first full-disk scan, no re-enable on upgrade.
# The GUI manages only the current user's exact registered service after confirmation.
echo "Installed official FindX into ${DESTDIR}${PREFIX}"
echo "For this user: systemctl --user daemon-reload; systemctl --user start findx2.service"
