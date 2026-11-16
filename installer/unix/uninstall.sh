#!/usr/bin/env bash
# Remove an official FindX Linux installation from PREFIX (default /opt/FindX).
# Does not delete the per-user index under ~/.local/share/FindX unless --purge-data.
set -euo pipefail

PREFIX=/opt/FindX
PURGE_DATA=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --prefix) PREFIX="${2:?}"; shift 2 ;;
    --purge-data) PURGE_DATA=1; shift ;;
    -h|--help)
      echo "Usage: $(basename "$0") [--prefix DIR] [--purge-data]"
      exit 0
      ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

target_user=${SUDO_USER:-${USER:-}}
if command -v systemctl >/dev/null && [[ -n "$target_user" && "$target_user" != root ]]; then
  uid=$(id -u "$target_user" 2>/dev/null || true)
  runtime=/run/user/${uid:-}
  if [[ -n "$uid" && -d "$runtime" ]]; then
    if [[ "$(id -u)" -eq 0 ]]; then
      sudo -u "$target_user" XDG_RUNTIME_DIR="$runtime" systemctl --user disable --now findx2.service || true
    else
      systemctl --user disable --now findx2.service || true
    fi
  fi
fi

rm -f /usr/share/applications/findx.desktop
rm -f /usr/share/icons/hicolor/128x128/apps/findx.png
rm -f /usr/lib/systemd/user/findx2.service
rm -f /usr/bin/findx /usr/bin/findx2 /usr/bin/fx
if [[ -d "$PREFIX" ]]; then
  rm -f "$PREFIX/FindX" "$PREFIX/findx2" "$PREFIX/fx" "$PREFIX/findx2-service" \
    "$PREFIX/FindX.installed" "$PREFIX/FindX.pkgformat" "$PREFIX/version" \
    "$PREFIX/install.sh" "$PREFIX/uninstall.sh"
  rmdir "$PREFIX" 2>/dev/null || true
fi

if [[ $PURGE_DATA -eq 1 && -n "$target_user" && "$target_user" != root ]]; then
  home=$(getent passwd "$target_user" | cut -d: -f6)
  if [[ -n "$home" && -d "$home/.local/share/FindX" ]]; then
    rm -rf "$home/.local/share/FindX"
  fi
fi

echo "Removed official FindX from $PREFIX"
