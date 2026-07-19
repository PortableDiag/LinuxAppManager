#!/usr/bin/env bash
# Install App Manager for the current user: binary, icon, and .desktop entry.
# Works both from the repo (scripts/install.sh) and from a standalone bundle
# (all files flat next to this script) — e.g. one grabbed off the NAS.
set -euo pipefail
APP_ID=com.procomputation.LinuxAppManager
HERE="$(cd "$(dirname "$0")" && pwd)"

find_asset() {  # echo first existing candidate for filename $1
  for d in "$HERE" "$HERE/../data" "$HERE/data"; do
    [ -f "$d/$1" ] && { echo "$d/$1"; return 0; }
  done
  return 1
}

BIN=""
for c in "$HERE/linux-app-manager" \
         "$HOME/.cache/cargo-target/linux-app-manager/release/linux-app-manager" \
         "$HERE/../target/release/linux-app-manager"; do
  [ -x "$c" ] && { BIN="$c"; break; }
done
[ -n "$BIN" ] || { echo "error: linux-app-manager binary not found next to the installer" >&2; exit 1; }
ICON="$(find_asset "$APP_ID.svg")"     || { echo "error: $APP_ID.svg not found" >&2; exit 1; }
DESK="$(find_asset "$APP_ID.desktop")" || { echo "error: $APP_ID.desktop not found" >&2; exit 1; }

BIN_DIR="$HOME/.local/bin"
ICON_DIR="$HOME/.local/share/icons/hicolor/scalable/apps"
APP_DIR="$HOME/.local/share/applications"
mkdir -p "$BIN_DIR" "$ICON_DIR" "$APP_DIR"

# Atomic binary swap — safe even if App Manager is running (ETXTBSY-proof).
install -m755 "$BIN" "$BIN_DIR/.linux-app-manager.new"
mv -f "$BIN_DIR/.linux-app-manager.new" "$BIN_DIR/linux-app-manager"
install -m644 "$ICON" "$ICON_DIR/$APP_ID.svg"
install -m644 "$DESK" "$APP_DIR/$APP_ID.desktop"

command -v gtk-update-icon-cache >/dev/null 2>&1 && \
  gtk-update-icon-cache -f -t "$HOME/.local/share/icons/hicolor" 2>/dev/null || true
command -v update-desktop-database >/dev/null 2>&1 && \
  update-desktop-database "$APP_DIR" 2>/dev/null || true

echo "Installed: $BIN_DIR/linux-app-manager (+ icon, menu entry)"
echo "Launch from your app menu, or run: linux-app-manager"
case ":$PATH:" in *":$HOME/.local/bin:"*) ;; *) echo "Note: add ~/.local/bin to your PATH.";; esac
