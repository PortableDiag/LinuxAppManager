# Changelog

## 0.1.0 — unreleased

First working skeleton. The Linux twin of the Android App Manager.

- Catalog list: installed-vs-latest per app, mirroring the Android phrasing
  (`Installed 1.3 → 1.4` / `Up to date · 1.4` / `Not installed · Latest 1.4`).
- Sources: GitHub releases (asset auto-resolved by kind), direct URL, local folder.
- Backends:
  - `deb` — install/remove via `apt` behind pkexec/polkit; version via `dpkg-query`.
  - `appimage` — installed into `~/Applications` with a `.desktop` entry and a
    `.version` sidecar; no root.
- Actions: Install / Update / Open / Remove, run off the UI thread.
- Refresh re-reads the catalog.
- Ships in its own source list, ready to self-update.
- GTK4 + libadwaita UI; builds to an ext4 target dir (source is on exfat).
