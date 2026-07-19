# Changelog

## 0.1.3 — 2026-07-19

- **Quick add** — ＋ Add takes a single GitHub repo/URL and auto-detects the
  name, description, and installable kind (with a "Manual…" fallback). Also
  `--add <repo>`.
- **Self-install** — running a loose binary (USB, repo dir, fresh download)
  offers to install itself into `~/.local/bin` by copying the running
  executable — offline, no download — with an embedded icon + menu entry. Also
  `--install-self`.

## 0.1.2 — 2026-07-19

- **Follow a GitHub user** — enumerate an account's repos (your own private ones
  too, via your token) and auto-add those with a release installable on this
  host; kind auto-detected. `▾ menu > Follow GitHub user…` / `--follow-user`.
- **Architecture-aware asset selection** — picks the host-arch `.deb`/`.AppImage`
  (was taking the first, e.g. arm64 on x86_64).
- **Edit / Remove from list** on each app; **Uninstall** relabelled (distinct
  from removing the source entry).
- **Custom install path** for `bin` apps — track/update a binary in any location
  (leading `~/` expands), not just `~/.local/bin`.
- **App icon + .desktop + `scripts/install.sh`** for a proper menu install.
- Honest **"version unknown"** display for self-built binaries (no phantom update
  arrow); **no "Open"** on App Manager's own entry.

## 0.1.1 — 2026-07-19

- **App detail page** — clicking a row pushes a page with the description,
  details (status / installed / latest / size / kind / source), release notes
  from the GitHub release, and actions (install / update / open / remove +
  auto-update toggle).
- **Fix: private-repo asset downloads** — resolve via the authenticated asset
  API URL and follow the pre-signed redirect unauthenticated. Previously
  `browser_download_url` returned an HTML login page, so private installs/updates
  silently failed.

## 0.1.0 — 2026-07-19

Later in 0.1.0 dev:

- **`bin` backend** — single executables in `~/.local/bin`, version tracked in a
  config sidecar; `--list` flag dumps the catalog headlessly.
- **Private-repo auth** — GitHub API calls send `$GITHUB_TOKEN`/`$GH_TOKEN` or
  the `gh auth token`, so private repos resolve.
- **Sharing / config** — `official-config.json` in the repo as the curated list;
  header ▾ menu with **Import official list / Import from file / Export config**;
  a **+ Add app** dialog. Import merges by `id`. Toasts via AdwToastOverlay.

Initial skeleton:

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
