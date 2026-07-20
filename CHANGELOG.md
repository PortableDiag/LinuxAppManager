# Changelog

## 0.1.8 — 2026-07-20

- **Self-healing `kind`** — if an app's stored `kind` no longer matches its
  release (e.g. the author switched a cargo-dist `.tar.gz` to a bare binary),
  App Manager now re-detects it on Refresh and updates the list automatically —
  previously the entry silently became uninstallable until you re-added it. The
  re-detection reuses the release data already fetched, so it costs no extra API
  calls and only runs when the stored kind matches nothing.

## 0.1.7 — 2026-07-20

- **Follow is now a real subscription** — "Follow GitHub user…" remembers the
  account (in `follows.json`); App Manager re-scans followed accounts on startup
  and on Refresh and auto-merges any **newly published** installable repos. It
  used to be a one-shot import, so repos created after you followed never showed
  up until you followed again. New `--discover` CLI runs the same re-scan.
  (Network note: one release lookup per repo per followed account each scan — a
  large or many-repo account eats into the anonymous 60-req/hour GitHub limit;
  set `$GITHUB_TOKEN` to raise it.)

## 0.1.6 — 2026-07-20

- **Tarball releases** — a new `tar` kind installs apps shipped as
  `.tar.gz`/`.tar.xz`/`.tar.zst` archives (the cargo-dist / common Rust layout):
  it downloads, extracts, finds the executable inside, and drops it in
  `~/.local/bin` — no root, version tracked in a sidecar, managed like `bin`
  after that. **Fix:** Follow-GitHub-user and quick-add now surface repos whose
  only release asset is a tarball (e.g. `riptide`), which were previously
  detected as uninstallable and silently dropped from the list.

## 0.1.5 — 2026-07-20

- **Self-contained AppImage** — `scripts/build-appimage.sh` bundles the whole
  GTK4 + libadwaita stack (~100 shared objects, pixbuf loaders, GIO modules,
  icon theme, gsettings schemas) into one `dist/LinuxAppManager-x86_64.AppImage`.
  Copy it to any distro and run — **no system packages on the target**. Fixes a
  fresh **KDE / Kubuntu** box dying on `libadwaita-1.so.0: cannot open shared
  object file` (Plasma doesn't ship libadwaita) without touching the target's
  package set. Verified: libadwaita/gtk-4 resolve to the bundled copies and the
  binary loads and runs.
- **Runtime-dependency preflight** — for the apt-based install path,
  `scripts/install.sh` checks for `libgtk-4-1` / `libadwaita-1-0` and installs
  any that are missing via pkexec/apt. README documents both paths.

## 0.1.4 — 2026-07-19

- **Public-only auth** — App Manager never reads your `gh` login; it uses the
  anonymous public API by default. A token is sent only from an explicit
  `$GITHUB_TOKEN`/`$GH_TOKEN` env var (opt-in). Follow-user lists public repos.
- **Method-agnostic install detection** — an app is detected however it was
  installed (dpkg, `~/.local/bin`, `~/Applications`, `$PATH`, or a custom
  `install_path`), regardless of its declared `kind`.
- **AppImage** detection honors a custom `install_path` and reports a present
  AppImage as installed even without a sidecar (matches `bin`).

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
