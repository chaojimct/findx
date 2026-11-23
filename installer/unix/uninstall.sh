#!/usr/bin/env bash
# Remove an official FindX Linux installation from PREFIX (default /opt/FindX).
# Always preserves the per-user index/config. --destdir never touches live services.
set -euo pipefail

PREFIX=/opt/FindX
DESTDIR=
while [[ $# -gt 0 ]]; do
  case "$1" in
    --prefix) PREFIX="${2:?}"; shift 2 ;;
    --destdir) DESTDIR="${2:?}"; shift 2 ;;
    -h|--help)
      echo "Usage: $(basename "$0") [--prefix DIR] [--destdir DIR]"
      exit 0
      ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

[[ "$PREFIX" = /* && "$PREFIX" != / && "$PREFIX" != *..* ]] || { echo "unsafe prefix" >&2; exit 2; }
[[ -z "$DESTDIR" || ( "$DESTDIR" = /* && "$DESTDIR" != / ) ]] || { echo "unsafe destdir" >&2; exit 2; }
[[ -f "$DESTDIR$PREFIX/FindX.installed" ]] || { echo "not a marked FindX install" >&2; exit 1; }
if [[ -z "$DESTDIR" && "$PREFIX" == /opt/FindX ]] && command -v systemctl >/dev/null; then
  # Do not continue removal if the current user's engine could still be writing.
  if systemctl --user is-active --quiet findx2.service; then
    systemctl --user stop findx2.service
  fi
  systemctl --user disable findx2.service 2>/dev/null || true
fi
rm -f "$DESTDIR/usr/share/applications/findx.desktop"
rm -f "$DESTDIR/usr/share/icons/hicolor/128x128/apps/findx.png"
rm -f "$DESTDIR/usr/lib/systemd/user/findx2.service"
for pair in findx:FindX findx2:findx2 fx:fx; do
  name=${pair%:*}; target=${pair#*:}
  link="$DESTDIR/usr/bin/$name"
  if [[ -L "$link" && "$(readlink "$link")" = "$PREFIX/$target" ]]; then rm "$link"; fi
done
for file in FindX findx2 fx findx2-service FindX.installed FindX.pkgformat version install.sh uninstall.sh; do
  rm -f "$DESTDIR$PREFIX/$file"
done
rmdir "$DESTDIR$PREFIX" 2>/dev/null || true
# Per-user index, exclusions, preferences, unit overrides and backups are ALWAYS
# preserved, including when the package manager removes/purges the package.
echo "Removed FindX binaries; user index/config preserved"
