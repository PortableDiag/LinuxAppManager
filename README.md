# Linux App Manager

A small, private **sideload/catalog manager for your own Linux apps** — the Linux
twin of the [Android App Manager](https://github.com/PortableDiag/AppManager). It shows
each app's **installed version** next to the **latest version** you've published, and
gives you one-click **Install / Update / Open / Remove** for every app in your catalog —
no app store, no accounts, no analytics.

> App id `com.procomputation.LinuxAppManager` · GTK4 + libadwaita · Rust ·
> targets Ubuntu / Debian / Kubuntu (GTK 4.12+, libadwaita 1.5+)

Like the Android version, **App Manager shows up in its own list and updates itself.**

---

## What it does

- **One list, two versions per app.** Each card shows `Installed 1.3 → 1.4` when an update
  is waiting, `Up to date · 1.4` when it isn't, or `Not installed · Latest 1.4` for apps
  you haven't put on this machine yet.
- **One-click actions** per app: **Install / Update** (download + install), **Open**
  (launch), **Remove** (uninstall).
- **Multiple install methods, one catalog.** A source declares its `kind`, and the right
  backend handles it:
  - **`deb`** — installed through `apt` behind **pkexec/polkit** (the system auth dialog
    is the Linux equivalent of Android's per-app install confirmation). Version read via
    `dpkg-query`.
  - **`appimage`** — dropped in `~/Applications`, marked executable, given a `.desktop`
    launcher, no root. Installed version tracked in a `.version` sidecar.
  - **`tar`** — a `.tar.gz`/`.tar.xz`/`.tar.zst` release archive (the cargo-dist /
    common Rust layout). The executable is extracted out of it into `~/.local/bin`,
    no root, version tracked in a sidecar — managed like `bin` from there on.
- **Refresh** re-reads the catalog and re-checks what's installed.

It never phones home. The only network it touches is the source URLs **you** give it.

---

## Where it gets apps

The catalog is built from a list of **sources** in `~/.config/linux-app-manager/sources.json`.
Each source is one app:

```json
[
  {
    "id": "com.procomputation.Gapless",
    "name": "Gapless",
    "kind": "appimage",
    "origin": { "type": "github", "repo": "PortableDiag/gapless" }
  },
  {
    "id": "myapp",
    "name": "My App",
    "kind": "deb",
    "package": "myapp",
    "origin": { "type": "github", "repo": "you/myapp" }
  }
]
```

**Origin types:**

- **`github`** — `{ "type": "github", "repo": "owner/repo" }`. The latest release's asset
  matching the app's `kind` (`.deb` / `.AppImage`) is found automatically via the GitHub
  API — no asset URL to construct.
- **`url`** — `{ "type": "url", "url": "http://host/app.deb" }`. A direct download.
- **`local`** — `{ "type": "local", "path": "/media/.../apks" }`. Highest-versioned
  matching file in a folder wins.

On first run a default `sources.json` is written that contains **only App Manager itself**,
so it can update itself out of the box.

### Adding apps & sharing the list

- **＋ (header)** — add one app from a dialog: name, GitHub `owner/repo`,
  executable/package name, and kind.
- **▾ menu (header):**
  - **Import official list** — pulls the curated `official-config.json` from this
    repo (works for the private repo via your `gh` token) and merges it in.
  - **Import from file…** — load a `sources.json` or exported config someone shared.
  - **Export config…** — write your list as a shareable `{version, sources}` file
    (drop it straight into a repo as an official list).

Imports **merge by `id`** — incoming entries add or replace, existing ones stay.
`kind` is one of `bin` / `appimage` / `deb` / `tar`.

---

## Build

Rust + the GTK4 / libadwaita development packages:

```bash
sudo apt install libgtk-4-dev libadwaita-1-dev build-essential
cargo build --release
```

Builds go to `~/.cache/cargo-target/linux-app-manager` (the source lives on an exfat
volume, which lacks the file locking cargo needs — see `.cargo/config.toml`).

### Runtime dependencies

The binary is dynamically linked against the GTK4 and libadwaita **runtime** libraries
(`libgtk-4-1`, `libadwaita-1-0`) — distinct from the `-dev` packages above. GNOME
desktops already have them; a fresh **KDE / Kubuntu (Plasma)** box usually does **not**
ship libadwaita, so the app dies on launch with:

```
libadwaita-1.so.0: cannot open shared object file: No such file or directory
```

`scripts/install.sh` detects and installs any missing runtime libs for you (via
pkexec/apt). To do it by hand:

```bash
sudo apt install libgtk-4-1 libadwaita-1-0
```

### Self-contained build (no system libraries)

To run on a machine where you **won't** install GTK/libadwaita — e.g. a KDE box you
don't want to pollute — build a single-file AppImage that carries the entire GTK4 +
libadwaita stack with it:

```bash
scripts/build-appimage.sh          # → dist/LinuxAppManager-x86_64.AppImage
```

Copy that one file to the other machine, `chmod +x`, and run it. Nothing is installed
system-wide; it uses its own bundled libraries. (The build assembles under `~/.cache`
because the repo's exfat volume can't hold the symlinks an AppDir needs; only the
finished `.AppImage` lands back in `dist/`.)

---

## Roadmap

Shipped in **v0.1**: GitHub / URL / local sources, deb + AppImage backends,
installed-vs-latest list, install / update / open / remove, refresh.

Next: search / sort / filter chips, per-app detail view (changelog, size, dates),
Update-all, background update checks (systemd user timer),
Flatpak passthrough, in-app self-update, themes.

## License

MIT.
