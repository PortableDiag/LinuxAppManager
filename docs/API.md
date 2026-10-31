# Linux App Manager local API

A local HTTP API that does everything the window does: list apps, install,
update, reinstall, uninstall and open them, add / edit / remove sources, follow
GitHub users, import and export, self-install, and change the API's own port
and key. Built so a script or an agent can drive App Manager without a person
at the keyboard.

**This document ships inside the binary.** `GET /api/docs` serves this exact
file from the running build.

---

## Turning it on

Off by default. **▾ menu → Local API…**: the switch, the port, and the key.

- The API lives in the App Manager window's process. Close the window and it
  stops; it comes back on the next launch if it was left switched on.
- Default address: **`http://127.0.0.1:8422/api`**. Loopback only, never a
  wildcard address.
- The key is generated on first run and kept in
  `~/.config/linux-app-manager/api-key`, mode 0600. Print it without opening a
  window:

```sh
linux-app-manager --api-key
```

- **Change the key** in the dialog: edit it and press ✓ to use your own (16+
  printable characters, no spaces), or press ↻ for a new random one. Either way
  the old key stops working at once.
- **Change the port** in the same dialog. The new port is bound before the old
  one is released, so a port that is already taken leaves the API where it was
  and says so.

---

## Authentication

Every endpoint needs the key. There is no unauthenticated route.

```sh
KEY=$(linux-app-manager --api-key)
BASE=http://127.0.0.1:8422

curl -s -H "Authorization: Bearer $KEY" "$BASE/api/status"
curl -s -H "X-API-Key: $KEY"            "$BASE/api/status"   # same thing
```

---

## Rules that will bite you

- **Check the status code.** `curl` exits 0 on a 4xx; use `-f` or read the
  `error` key. Every error is `{"ok": false, "error": "..."}`.
- **Install / update / uninstall wait until they finish.** A 100+ MB AppImage
  can take minutes. Do not put a short `--max-time` on them.
- **One action per app at a time.** A second install/update/uninstall of an app
  that already has one running — from the API *or* a click in the window —
  gets **409**. Wait for the first to answer.
- **`GET /api/apps` hits GitHub**, one release lookup per app. The anonymous
  GitHub limit is 60 requests an hour; a polling loop will burn it. Poll
  `/api/status` instead, which is local.
- `{id}` is an app id (`com.procomputation.Gapless`) or its display name
  (`Gapless`), case-insensitive. Percent-encode spaces.
- Values can go in a JSON body or the query string (`?on=true`); the body wins.

---

## Status

`GET /api/status` — local only, no network.

```json
{"ok": true, "version": "0.1.19", "in_flight": ["com.procomputation.Gapless"],
 "refreshing": false, "api_port": 8422, "running_from": "...",
 "running_appimage": null, "self_install_offered": null, "config_dir": "..."}
```

`in_flight` lists apps with an install/update/uninstall running right now.

---

## Apps

`GET /api/apps` — every app in the list, resolved fresh:

```json
{"ok": true, "apps": [{
  "id": "com.procomputation.Gapless", "name": "Gapless", "kind": "appimage",
  "status": "update_available", "summary": "Installed 0.6.1 → 0.6.2",
  "installed": "0.6.1", "latest": "0.6.2", "installable": true,
  "download_size": 123456789, "release_notes": "...",
  "auto_update": false, "cli": false, "busy": false, "is_self": false,
  "origin": {"type": "github", "repo": "PortableDiag/gapless"},
  "package": null, "install_path": null, "description": "..."}]}
```

`status` is one of `not_installed`, `up_to_date`, `update_available`,
`unknown` (installed, but the version or the latest release is unknown).

`GET /api/apps/{id}` — one app, same shape, under `"app"`.

### Actions

| Call | Does |
|---|---|
| `POST /api/apps/{id}/install` | download and install the latest release |
| `POST /api/apps/{id}/update` | same — install the latest over the current one |
| `POST /api/apps/{id}/reinstall` | same — clears "version unknown" |
| `POST /api/apps/{id}/uninstall` | remove the installed app (`/remove` also works) |
| `POST /api/apps/{id}/open` | launch it |
| `POST /api/apps/{id}/auto-update` | `{"on": true}` |

```sh
curl -s -X POST -H "Authorization: Bearer $KEY" "$BASE/api/apps/Gapless/update"
# {"ok": true, "id": "com.procomputation.Gapless", "message": "Updated Gapless"}
```

**422** means there is nothing to download for this machine (GitHub
unreachable, rate-limited, or no matching asset). **409** means the app is
already busy. The window shows the same progress toast and refreshes, exactly
as if the button had been clicked.

App Manager's own entry (`com.procomputation.LinuxAppManager`) updates like any
other; restart it to run the new version.

### Editing the list

| Call | Body |
|---|---|
| `POST /api/apps` | `{"repo": "owner/repo"}` (or a GitHub URL) — detected like the ＋ button; or `{"source": {...}}` for a full entry |
| `PATCH /api/apps/{id}` | any of `name`, `description`, `kind` (`bin`/`appimage`/`deb`/`tar`), `package`, `install_path`, `repo` / `url` / `path` (the origin), `auto_update`, `cli` |
| `DELETE /api/apps/{id}` | removes it from the list; does **not** uninstall it |

A full `source`:

```json
{"id": "my-tool", "name": "My Tool", "kind": "bin", "package": "my-tool",
 "origin": {"type": "github", "repo": "me/my-tool"}, "auto_update": false}
```

---

## Sources, following, import and export

| Call | Does |
|---|---|
| `GET /api/sources` | the raw saved list |
| `GET /api/follows` | followed GitHub users |
| `POST /api/follow` | `{"user": "name"}` — follow, and add their installable repos now |
| `POST /api/discover` | re-scan followed users for new repos |
| `POST /api/import-official` | merge the curated official list |
| `POST /api/import` | `{"config": <document or its JSON text>}` or `{"path": "/file.json"}` |
| `GET /api/export` | your list as a shareable config document |
| `POST /api/export` | `{"path": "/file.json"}` — write it to a file |

Merges answer `{"ok": true, "added": 1, "updated": 0, "total": 14}`.

---

## Maintenance

| Call | Does |
|---|---|
| `POST /api/self-install` | install the running copy, like the Install button on App Manager's own row |
| `POST /api/auto-update` | run the auto-update pass now: `{"updated": [...], "failed": [{name, error}]}` |
| `POST /api/refresh` | reload the window's list |

---

## Settings

`GET /api/settings` → `{"api_enabled": true, "api_port": 8422, "listening_on": 8422}`

`POST /api/settings` — any of:

- `api_port` — move the listener (1024–65535). **409** if it cannot bind; the
  API stays on the old port.
- `api_key` — use this key (16+ printable characters, no spaces).
- `regenerate_key: true` — issue a new random key. The answer carries it.
- `api_enabled: false` — switch the API off. To turn it back on you need the
  window.

The reply still arrives on the old connection; the next call must use the new
port and key.
