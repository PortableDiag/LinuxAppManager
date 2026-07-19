#!/usr/bin/env bash
# Install + enable the systemd --user timer that runs `linux-app-manager
# --auto-update` daily. Idempotent; re-run to update the units.
set -euo pipefail

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
UNIT_SRC="$REPO_DIR/systemd"
UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"

if ! command -v linux-app-manager >/dev/null 2>&1 && [ ! -x "$HOME/.local/bin/linux-app-manager" ]; then
  echo "error: linux-app-manager not found in PATH or ~/.local/bin — install it first" >&2
  exit 1
fi

mkdir -p "$UNIT_DIR"
cp "$UNIT_SRC/linux-app-manager-update.service" "$UNIT_DIR/"
cp "$UNIT_SRC/linux-app-manager-update.timer"   "$UNIT_DIR/"

systemctl --user daemon-reload
systemctl --user enable --now linux-app-manager-update.timer

echo "Timer enabled. Schedule:"
systemctl --user list-timers linux-app-manager-update.timer --no-pager || true
echo
echo "Run once now:   systemctl --user start linux-app-manager-update.service"
echo "See last run:   journalctl --user -u linux-app-manager-update.service -n 20"
echo "Disable:        systemctl --user disable --now linux-app-manager-update.timer"
