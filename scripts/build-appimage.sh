#!/usr/bin/env bash
# Build a fully self-contained AppImage of App Manager.
#
# The binary dynamically links the whole GTK4 + libadwaita stack (~100 shared
# objects). GNOME desktops have them; a fresh KDE/Kubuntu box does not ship
# libadwaita, so a bare binary dies on "libadwaita-1.so.0: cannot open shared
# object file". This bundles GTK4, libadwaita, their transitive libs, the
# gdk-pixbuf loaders, GIO modules, icon theme and gsettings schemas into one
# file you copy to any distro and run — no system packages on the target.
#
# Output: dist/LinuxAppManager-x86_64.AppImage
set -euo pipefail

APP_ID=com.procomputation.LinuxAppManager
REPO="$(cd "$(dirname "$0")/.." && pwd)"
TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/cargo-target/linux-app-manager}"
BIN="$TARGET_DIR/release/linux-app-manager"
# The repo lives on an exfat volume, which supports neither the symlinks nor the
# permission bits linuxdeploy needs for an AppDir. Assemble on a native fs under
# ~/.cache and copy only the finished single-file AppImage back into the repo.
TOOLS="$HOME/.cache/linux-app-manager/appimage-tools"
WORK="$HOME/.cache/linux-app-manager/appimage-build"
APPDIR="$WORK/AppDir"
DIST="$REPO/dist"

# AppImage tools are themselves AppImages — extract-and-run avoids needing FUSE.
export APPIMAGE_EXTRACT_AND_RUN=1
# Tell the GTK plugin we are a GTK4 app (it otherwise guesses).
export DEPLOY_GTK_VERSION=4

fetch() {  # fetch URL DEST (skip if present)
  local url="$1" dest="$2"
  [ -f "$dest" ] && return 0
  echo "  ↓ $(basename "$dest")"
  curl -f#L --retry 3 -o "$dest" "$url"
  chmod +x "$dest"
}

echo "==> Building release binary"
cargo build --release --manifest-path "$REPO/Cargo.toml"
[ -x "$BIN" ] || { echo "error: release binary not found at $BIN" >&2; exit 1; }

echo "==> Fetching bundling tools into $TOOLS"
mkdir -p "$TOOLS"
fetch "https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-x86_64.AppImage" \
      "$TOOLS/linuxdeploy-x86_64.AppImage"
fetch "https://github.com/linuxdeploy/linuxdeploy-plugin-gtk/raw/master/linuxdeploy-plugin-gtk.sh" \
      "$TOOLS/linuxdeploy-plugin-gtk.sh"
fetch "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage" \
      "$TOOLS/appimagetool-x86_64.AppImage"

echo "==> Assembling AppDir"
rm -rf "$WORK"
mkdir -p "$APPDIR/usr/bin" \
         "$APPDIR/usr/share/applications" \
         "$APPDIR/usr/share/icons/hicolor/scalable/apps"
install -m755 "$BIN" "$APPDIR/usr/bin/linux-app-manager"
install -m644 "$REPO/data/$APP_ID.desktop" "$APPDIR/usr/share/applications/$APP_ID.desktop"
install -m644 "$REPO/data/$APP_ID.svg" \
        "$APPDIR/usr/share/icons/hicolor/scalable/apps/$APP_ID.svg"

echo "==> Running linuxdeploy + gtk plugin"
PATH="$TOOLS:$PATH" "$TOOLS/linuxdeploy-x86_64.AppImage" \
  --appdir "$APPDIR" \
  --plugin gtk \
  --desktop-file "$APPDIR/usr/share/applications/$APP_ID.desktop" \
  --icon-file "$APPDIR/usr/share/icons/hicolor/scalable/apps/$APP_ID.svg" \
  --executable "$APPDIR/usr/bin/linux-app-manager"

echo "==> Packing AppImage"
( cd "$WORK" && PATH="$TOOLS:$PATH" ARCH=x86_64 \
    "$TOOLS/appimagetool-x86_64.AppImage" AppDir "LinuxAppManager-x86_64.AppImage" )

mkdir -p "$DIST"
OUT="$DIST/LinuxAppManager-x86_64.AppImage"
# cp (not mv) — final .AppImage is a plain file, fine to land on the exfat repo.
cp -f "$WORK/LinuxAppManager-x86_64.AppImage" "$OUT"
chmod +x "$OUT"
echo
echo "Built: $OUT ($(du -h "$OUT" | cut -f1))"
echo "Copy that single file to the other box and run it — no apt install needed."
