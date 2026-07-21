//! Per-install-method detect / install / remove / open.
//!
//! deb goes through apt behind pkexec (the Linux equivalent of Android's
//! per-app install confirmation). AppImages are managed entirely in $HOME,
//! no root: dropped in ~/Applications with a .desktop entry and a `.version`
//! sidecar (there is no reliable way to read a version back out of an
//! arbitrary AppImage, so we record what we installed).

use crate::config;
use crate::model::{Kind, Latest, Source};
use anyhow::{anyhow, Context, Result};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

// Embedded so a loose binary (USB / fresh download) can install itself with its
// icon and menu entry, no network and no side files needed.
const SELF_ICON: &str = include_str!("../data/com.procomputation.LinuxAppManager.svg");
const SELF_DESKTOP: &str = include_str!("../data/com.procomputation.LinuxAppManager.desktop");
const SELF_ID: &str = "com.procomputation.LinuxAppManager";

/// The AppImage file we are currently running from, if any. The AppImage
/// runtime exports $APPIMAGE = absolute path of the .AppImage file.
pub fn running_appimage() -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var_os("APPIMAGE")?);
    p.is_file().then_some(p)
}

/// $APPDIR — the AppImage's unpacked content tree (squashfs mount or the
/// extract-and-run temp dir). Only trusted while $APPIMAGE is also set.
fn running_appdir() -> Option<PathBuf> {
    running_appimage()?;
    let d = PathBuf::from(std::env::var_os("APPDIR")?);
    d.join("AppRun").is_file().then_some(d)
}

/// Install the *currently running* App Manager, plus its icon and .desktop
/// entry. Works offline / from a USB.
///
/// When launched from an AppImage, the whole content tree ($APPDIR) is copied
/// to ~/.local/share/linux-app-manager/app and a tiny launcher script is
/// written to ~/.local/bin/linux-app-manager. The installed copy is then plain
/// files — starting it needs neither FUSE nor the AppImage runtime, and every
/// bundled GTK/libadwaita library is on disk next to it. (Copying just
/// current_exe() would strand the binary without those libs — they vanish with
/// the AppImage mount — and copying the .AppImage file kept launch dependent
/// on the runtime working on the target machine.)
///
/// When running as a loose binary, it is copied to ~/.local/bin as before.
pub fn install_self() -> Result<PathBuf> {
    let bundle = config::self_bundle_dir();
    let wrapper = config::localbin_dir().join("linux-app-manager");

    if let Some(appdir) = running_appdir() {
        install_bundle_tree(&appdir)?;
    } else {
        let exe = std::env::current_exe().context("locating the running binary")?;
        if !exe.starts_with(&bundle) {
            // Loose binary → ~/.local/bin, atomic over any previous install.
            std::fs::create_dir_all(config::localbin_dir())?;
            let staged = config::localbin_dir().join(".linux-app-manager.new");
            std::fs::copy(&exe, &staged).context("copying binary")?;
            let mut perms = std::fs::metadata(&staged)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&staged, perms)?;
            std::fs::rename(&staged, &wrapper)?;
        }
        // else: we ARE the installed bundle — nothing to copy, just refresh
        // the icon/menu entry below.
    }

    finish_self_install(env!("CARGO_PKG_VERSION"))?;
    Ok(wrapper)
}

/// Copy an unpacked AppImage tree (must contain AppRun) into the bundle dir,
/// staged + rename so a half-finished copy is never live, then point the
/// ~/.local/bin launcher at it.
fn install_bundle_tree(tree: &Path) -> Result<()> {
    let bundle = config::self_bundle_dir();
    let parent = bundle.parent().ok_or_else(|| anyhow!("bad bundle dir"))?;
    std::fs::create_dir_all(parent)?;
    let staged = parent.join(".app.new");
    let _ = std::fs::remove_dir_all(&staged);
    std::fs::create_dir_all(&staged)?;
    // cp -a keeps the symlinks and exec bits a .so tree needs; std has no
    // recursive copy.
    let status = Command::new("cp")
        .arg("-a")
        .arg(format!("{}/.", tree.display()))
        .arg(&staged)
        .status()
        .context("running cp")?;
    if !status.success() {
        return Err(anyhow!("copying the app bundle failed"));
    }
    // The running copy keeps its already-mapped files even as the old dir goes.
    let _ = std::fs::remove_dir_all(&bundle);
    std::fs::rename(&staged, &bundle)?;

    // Launcher script: menu entry and $PATH both go through it. Clear the
    // AppImage variables so a copy started from inside the running AppImage
    // can't inherit a stale mount path.
    let wrapper = config::localbin_dir().join("linux-app-manager");
    std::fs::create_dir_all(config::localbin_dir())?;
    let staged_w = config::localbin_dir().join(".linux-app-manager.new");
    std::fs::write(
        &staged_w,
        format!(
            "#!/bin/sh\nunset APPDIR APPIMAGE ARGV0\nexec \"{}/AppRun\" \"$@\"\n",
            bundle.display()
        ),
    )?;
    let mut perms = std::fs::metadata(&staged_w)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&staged_w, perms)?;
    std::fs::rename(&staged_w, &wrapper)?;

    // Retire the v0.1.9-era whole-AppImage copy AND its version sidecar,
    // superseded by the bundle. The stale sidecar matters: self is `appimage`
    // kind, so detection reads the appimage sidecar first — a leftover here
    // would shadow the real version recorded in versions/ and make App Manager
    // perpetually show an update for a version it's already running.
    let _ = std::fs::remove_file(config::appimage_dir().join(format!("{SELF_ID}.AppImage")));
    let _ = std::fs::remove_file(config::appimage_dir().join(format!("{SELF_ID}.version")));
    Ok(())
}

/// Icon, menu entry (absolute Exec — ~/.local/bin isn't on every launcher's
/// $PATH), recorded version, and a desktop-cache nudge.
fn finish_self_install(version: &str) -> Result<()> {
    let icon_dir = config::data_dir_icons();
    std::fs::create_dir_all(&icon_dir)?;
    std::fs::write(icon_dir.join(format!("{SELF_ID}.svg")), SELF_ICON)?;
    let app_dir = config::desktop_dir();
    std::fs::create_dir_all(&app_dir)?;
    let exec = config::localbin_dir().join("linux-app-manager");
    let desktop =
        SELF_DESKTOP.replace("Exec=linux-app-manager", &format!("Exec={}", exec.display()));
    std::fs::write(app_dir.join(format!("{SELF_ID}.desktop")), desktop)?;

    std::fs::create_dir_all(config::versions_dir())?;
    std::fs::write(config::versions_dir().join(SELF_ID), version)?;
    refresh_menu_caches();
    Ok(())
}

/// Nudge the desktop environment to re-read menu entries. KDE in particular
/// caches .desktop files (sycoca) and can keep launching a stale Exec line
/// long after the file on disk changed. All best-effort.
fn refresh_menu_caches() {
    let apps = config::desktop_dir();
    let icons = dirs::data_dir().map(|d| d.join("icons/hicolor"));
    let mut cmds: Vec<Vec<String>> = vec![
        vec!["update-desktop-database".into(), apps.display().to_string()],
        vec!["kbuildsycoca6".into()],
        vec!["kbuildsycoca5".into()],
    ];
    if let Some(icons) = icons {
        cmds.push(vec![
            "gtk-update-icon-cache".into(),
            "-f".into(),
            "-t".into(),
            icons.display().to_string(),
        ]);
    }
    for cmd in cmds {
        let _ = Command::new(&cmd[0])
            .args(&cmd[1..])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

/// Installed version, or `None` if not present. Detection is method-agnostic:
/// it probes every way an app could be installed (custom path, dpkg,
/// ~/.local/bin, ~/Applications, PATH) and reports the first hit — so an app is
/// found however it actually got there, regardless of its declared `kind`.
/// (The `kind` still governs how install/update fetches it.)
pub fn detect_installed(src: &Source) -> Option<String> {
    // 1. An explicit custom path is authoritative for this app.
    if let Some(p) = src
        .install_path
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        return expand_tilde(p).exists().then(|| sidecar_or_unknown(src));
    }

    // 2. A real dpkg package gives a real version — prefer it.
    if let Some(v) = dpkg_version(src.package_name()) {
        return Some(v);
    }

    // 3. A managed binary/AppImage we (or the user) put in the usual spots.
    if config::localbin_dir().join(src.package_name()).exists()
        || config::apps_dir().join(&src.id).join("AppRun").exists()
        || config::appimage_dir()
            .join(format!("{}.AppImage", src.id))
            .exists()
    {
        return Some(sidecar_or_unknown(src));
    }

    // 4. Anything else reachable on $PATH (e.g. /usr/bin, /usr/local/bin).
    if on_path(src.package_name()) {
        return Some(crate::model::UNKNOWN_VERSION.to_string());
    }

    None
}

/// dpkg-recorded version of a package, if installed.
fn dpkg_version(pkg: &str) -> Option<String> {
    let out = Command::new("dpkg-query")
        .args(["-W", "-f=${Version}", pkg])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!v.is_empty()).then_some(v)
}

/// Recorded version from a sidecar, else the "unknown" sentinel. The sidecar
/// matching the source's *current* kind is read first: an app that migrated
/// from `bin` to `appimage` (or back) leaves the old kind's sidecar on disk,
/// and reading that first would keep reporting the stale pre-migration version
/// — so the Update button would never clear even after a successful update.
fn sidecar_or_unknown(src: &Source) -> String {
    let order = match src.kind {
        Kind::AppImage => [version_sidecar(src), bin_sidecar(src)],
        _ => [bin_sidecar(src), version_sidecar(src)],
    };
    for p in order {
        if let Ok(s) = std::fs::read_to_string(&p) {
            let s = s.trim();
            if !s.is_empty() {
                return s.to_string();
            }
        }
    }
    crate::model::UNKNOWN_VERSION.to_string()
}

/// Whether an executable named `name` sits in any $PATH directory.
fn on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|d| d.join(name).is_file()))
        .unwrap_or(false)
}

/// Download the latest release and install/update it.
pub fn install(src: &Source, latest: &Latest) -> Result<()> {
    std::fs::create_dir_all(config::cache_dir())?;
    let file = download(src, latest)?;
    let result = match src.kind {
        Kind::Deb => install_deb(&file),
        Kind::AppImage => install_appimage(src, latest, &file),
        Kind::Bin => install_bin(src, latest, &file),
        Kind::Tar => install_tar(src, latest, &file),
    };
    if result.is_ok() {
        cleanup_foreign_artifacts(src);
    }
    result
}

/// Remove artifacts left by a *previous* install of the same app under a
/// different `kind` (e.g. gapless shipped as a bare `bin`, now an `appimage`).
/// Without this, the orphaned binary/bundle and its stale version sidecar hang
/// around — the old binary is often the broken one, and its sidecar can shadow
/// the real installed version. Never touches App Manager itself, whose bin
/// wrapper and appimage bundle are both legitimate parts of one install.
fn cleanup_foreign_artifacts(src: &Source) {
    if src.id == SELF_ID {
        return;
    }
    match src.kind {
        Kind::AppImage => {
            // A leftover bin/tar install of the same app.
            let has_custom_path = src
                .install_path
                .as_deref()
                .map(str::trim)
                .is_some_and(|p| !p.is_empty());
            if !has_custom_path {
                let _ = std::fs::remove_file(config::localbin_dir().join(src.package_name()));
            }
            let _ = std::fs::remove_file(bin_sidecar(src));
        }
        Kind::Bin | Kind::Tar => {
            // A leftover appimage install of the same app.
            let _ = std::fs::remove_dir_all(config::apps_dir().join(&src.id));
            let _ = std::fs::remove_file(config::appimage_dir().join(format!("{}.AppImage", src.id)));
            let _ = std::fs::remove_file(version_sidecar(src));
        }
        Kind::Deb => {}
    }
}

/// Remove an installed app.
pub fn remove(src: &Source) -> Result<()> {
    if src.id == SELF_ID {
        // Self lives as a bundle + launcher, whatever kind the list says.
        let _ = std::fs::remove_dir_all(config::self_bundle_dir());
        let _ = std::fs::remove_file(config::localbin_dir().join("linux-app-manager"));
        let _ = std::fs::remove_file(config::appimage_dir().join(format!("{SELF_ID}.AppImage")));
        let _ = std::fs::remove_file(config::desktop_dir().join(format!("{SELF_ID}.desktop")));
        let _ = std::fs::remove_file(config::versions_dir().join(SELF_ID));
        return Ok(());
    }
    match src.kind {
        Kind::Deb => run_pkexec(&["apt-get", "remove", "-y", src.package_name()]),
        Kind::AppImage => {
            let _ = std::fs::remove_dir_all(config::apps_dir().join(&src.id));
            let _ = std::fs::remove_file(appimage_path(src));
            let _ = std::fs::remove_file(version_sidecar(src));
            let _ = std::fs::remove_file(desktop_path(src));
            refresh_menu_caches();
            Ok(())
        }
        // Tar apps land as a single binary in ~/.local/bin, same as Bin.
        Kind::Bin | Kind::Tar => {
            let _ = std::fs::remove_file(bin_path(src));
            let _ = std::fs::remove_file(bin_sidecar(src));
            let _ = std::fs::remove_file(desktop_path(src));
            refresh_menu_caches();
            Ok(())
        }
    }
}

/// Launch an installed app (best effort).
pub fn open(src: &Source) -> Result<()> {
    match src.kind {
        Kind::AppImage => {
            let unpacked = config::apps_dir().join(&src.id).join("AppRun");
            let target = if unpacked.exists() { unpacked } else { appimage_path(src) };
            spawn_app(&target)
        }
        Kind::Bin | Kind::Tar => spawn_app(&bin_path(src)),
        Kind::Deb => {
            // Try the package-named .desktop id; harmless if it doesn't match.
            Command::new("gtk-launch")
                .arg(src.package_name())
                .spawn()
                .context("gtk-launch")?;
            Ok(())
        }
    }
}

/// Spawn a child app with the AppImage runtime variables scrubbed: when the
/// manager itself runs from an AppImage, an inherited $APPDIR would make the
/// child's own AppRun resolve against OUR tree instead of its own.
fn spawn_app(cmd: &Path) -> Result<()> {
    Command::new(cmd)
        .env_remove("APPDIR")
        .env_remove("APPIMAGE")
        .env_remove("ARGV0")
        .spawn()
        .with_context(|| format!("launching {}", cmd.display()))?;
    Ok(())
}

// --- helpers ---------------------------------------------------------------

fn download(src: &Source, latest: &Latest) -> Result<PathBuf> {
    let dest = config::cache_dir().join(format!("{}{}", src.id, src.kind.ext()));
    let url = &latest.download_url;
    // A local file:// source needs no download.
    if let Some(path) = url.strip_prefix("file://") {
        return Ok(PathBuf::from(path));
    }

    let mut reader: Box<dyn std::io::Read + Send> =
        if url.starts_with("https://api.github.com/") {
            // Release-asset download via the API URL. Ask for the raw bytes,
            // but DON'T auto-follow: the 302 points at a pre-signed URL that
            // rejects a stray Authorization header, so we fetch the Location
            // ourselves, unauthenticated. (Public repos need no token.)
            let agent = ureq::builder().redirects(0).build();
            let mut req = agent
                .get(url)
                .set("User-Agent", "LinuxAppManager")
                .set("Accept", "application/octet-stream");
            if let Some(token) = crate::sources::github_token() {
                req = req.set("Authorization", &format!("Bearer {token}"));
            }
            let resp = req.call().context("asset request failed")?;
            match resp.header("Location") {
                Some(loc) => Box::new(
                    ureq::get(loc)
                        .set("User-Agent", "LinuxAppManager")
                        .call()
                        .context("asset redirect failed")?
                        .into_reader(),
                ),
                None => Box::new(resp.into_reader()),
            }
        } else {
            Box::new(
                ureq::get(url)
                    .set("User-Agent", "LinuxAppManager")
                    .call()
                    .context("download request failed")?
                    .into_reader(),
            )
        };

    let mut out = std::fs::File::create(&dest)?;
    std::io::copy(&mut reader, &mut out)?;
    Ok(dest)
}

fn install_deb(file: &PathBuf) -> Result<()> {
    let path = file
        .to_str()
        .ok_or_else(|| anyhow!("non-utf8 path"))?
        .to_string();
    // Modern apt installs a local .deb and pulls its dependencies.
    run_pkexec(&["apt-get", "install", "-y", &path])
}

fn install_appimage(src: &Source, latest: &Latest, file: &PathBuf) -> Result<()> {
    // App Manager itself installs as an extracted bundle (see install_self),
    // so a self-update must refresh that bundle — dropping the new .AppImage
    // in ~/Applications would fork the install in two places.
    if src.id == SELF_ID {
        return update_self_bundle(latest, file);
    }
    // A custom install_path means the user wants the .AppImage file itself at
    // that spot — honor it verbatim.
    if src.install_path.as_deref().map(str::trim).is_some_and(|p| !p.is_empty()) {
        let dest = appimage_path(src);
        if file != &dest {
            std::fs::copy(file, &dest)?;
            if file.starts_with(config::cache_dir()) {
                let _ = std::fs::remove_file(file);
            }
        }
        let mut perms = std::fs::metadata(&dest)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&dest, perms)?;
        write_desktop(src, &dest, None)?;
        std::fs::write(version_sidecar(src), &latest.version)?;
        refresh_menu_caches();
        return Ok(());
    }

    // Default: unpack the AppImage instead of keeping the file. Launching the
    // installed app is then a plain exec of its AppRun — no FUSE, no embedded
    // runtime involved on the target machine.
    let mut perms = std::fs::metadata(file)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(file, perms)?;
    std::fs::create_dir_all(config::apps_dir())?;
    let workdir = config::apps_dir().join(format!(".{}.extract", src.id));
    let _ = std::fs::remove_dir_all(&workdir);
    std::fs::create_dir_all(&workdir)?;
    // --appimage-extract is handled by the AppImage's embedded runtime and
    // works without FUSE; it unpacks to ./squashfs-root.
    let status = Command::new(file)
        .arg("--appimage-extract")
        .current_dir(&workdir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .context("unpacking the AppImage")?;
    let tree = workdir.join("squashfs-root");
    if !status.success() || !tree.join("AppRun").exists() {
        let _ = std::fs::remove_dir_all(&workdir);
        return Err(anyhow!("could not unpack {}", file.display()));
    }
    let dest = config::apps_dir().join(&src.id);
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::rename(&tree, &dest)?;
    let _ = std::fs::remove_dir_all(&workdir);
    if file.starts_with(config::cache_dir()) {
        let _ = std::fs::remove_file(file);
    }
    // Retire a pre-0.1.11 file-form install from the default spot.
    let _ = std::fs::remove_file(config::appimage_dir().join(format!("{}.AppImage", src.id)));

    write_desktop(src, &dest.join("AppRun"), tree_icon(&dest).as_deref())?;
    std::fs::write(version_sidecar(src), &latest.version)?;
    refresh_menu_caches();
    Ok(())
}

/// An icon file shipped at the root of an extracted AppImage tree (prefer a
/// real *.svg/*.png over the extensionless .DirIcon), referenced by absolute
/// path from the menu entry.
fn tree_icon(dir: &Path) -> Option<PathBuf> {
    let mut png = None;
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if !p.is_file() {
            continue;
        }
        match p.extension().and_then(|x| x.to_str()) {
            Some("svg") => return Some(p),
            Some("png") => png = Some(p),
            _ => {}
        }
    }
    png.or_else(|| {
        let d = dir.join(".DirIcon");
        d.is_file().then_some(d)
    })
}

/// Self-update from a downloaded release AppImage: unpack it (the embedded
/// runtime's --appimage-extract needs no FUSE) and swap the installed bundle.
/// The running copy keeps working off its old, already-mapped files until
/// relaunch.
fn update_self_bundle(latest: &Latest, file: &PathBuf) -> Result<()> {
    let mut perms = std::fs::metadata(file)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(file, perms)?;
    let workdir = config::cache_dir().join(".self-extract");
    let _ = std::fs::remove_dir_all(&workdir);
    std::fs::create_dir_all(&workdir)?;
    let status = Command::new(file)
        .arg("--appimage-extract")
        .current_dir(&workdir)
        .stdout(std::process::Stdio::null())
        .status()
        .context("unpacking the update")?;
    let tree = workdir.join("squashfs-root");
    if !status.success() || !tree.join("AppRun").is_file() {
        let _ = std::fs::remove_dir_all(&workdir);
        return Err(anyhow!("could not unpack the downloaded update"));
    }
    let result = install_bundle_tree(&tree).and_then(|()| finish_self_install(&latest.version));
    let _ = std::fs::remove_dir_all(&workdir);
    if file.starts_with(config::cache_dir()) {
        let _ = std::fs::remove_file(file);
    }
    result
}

fn install_bin(src: &Source, latest: &Latest, file: &PathBuf) -> Result<()> {
    // A bare binary must never overwrite the self bundle's launcher — it would
    // strand the app without its bundled libraries again.
    if src.id == SELF_ID && config::self_bundle_dir().join("AppRun").is_file() {
        return Err(anyhow!(
            "App Manager is installed as a self-contained bundle — update it from an AppImage release"
        ));
    }
    if latest.download_url.is_empty() {
        return Err(anyhow!(
            "{} has no downloadable release asset — build it from source and copy \
             the binary into ~/.local/bin yourself",
            src.name
        ));
    }
    let dest = bin_path(src);
    // Install into the binary's own directory (custom path or ~/.local/bin).
    let dir = dest.parent().map(|p| p.to_path_buf()).unwrap_or_else(config::localbin_dir);
    std::fs::create_dir_all(&dir)?;
    // Stage next to the target, then atomically rename over it. A plain copy
    // fails with ETXTBSY when replacing a running executable (e.g. the manager
    // updating itself); rename swaps the dir entry and leaves the running
    // inode intact, so self-update works — it just takes effect on next launch.
    let staged = dir.join(format!(".{}.new", src.package_name()));
    std::fs::copy(file, &staged)?;
    let mut perms = std::fs::metadata(&staged)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&staged, perms)?;
    std::fs::rename(&staged, &dest)?;
    if file.starts_with(config::cache_dir()) {
        let _ = std::fs::remove_file(file);
    }
    record_bin_version(src, &latest.version)?;
    finish_bin_install(src, &dest)?;
    Ok(())
}

/// GUI binaries get a menu entry (absolute Exec — ~/.local/bin may not be on
/// the launcher's $PATH); terminal tools (`cli: true`) stay out of the menu but
/// need ~/.local/bin on PATH so they're runnable by name.
fn finish_bin_install(src: &Source, dest: &Path) -> Result<()> {
    if src.cli {
        let _ = config::ensure_localbin_on_path();
    } else {
        write_desktop(src, dest, None)?;
        refresh_menu_caches();
    }
    Ok(())
}

/// Extract a release tarball and install the executable it contains, then
/// manage it exactly like a `bin` app. Uses the system `tar`, which
/// auto-detects gzip/xz/zstd/bzip2 from the archive itself.
fn install_tar(src: &Source, latest: &Latest, file: &PathBuf) -> Result<()> {
    if latest.download_url.is_empty() {
        return Err(anyhow!(
            "{} has no downloadable release tarball",
            src.name
        ));
    }
    let workdir = config::cache_dir().join(format!(".{}.extract", src.id));
    let _ = std::fs::remove_dir_all(&workdir);
    std::fs::create_dir_all(&workdir)?;

    let status = Command::new("tar")
        .arg("-xf")
        .arg(file)
        .arg("-C")
        .arg(&workdir)
        .status()
        .context("tar not available")?;
    if !status.success() {
        let _ = std::fs::remove_dir_all(&workdir);
        return Err(anyhow!("failed to extract {}", file.display()));
    }

    let exec = find_executable(&workdir, src.package_name()).ok_or_else(|| {
        anyhow!(
            "no executable named '{}' found in the {} tarball",
            src.package_name(),
            src.name
        )
    })?;

    // Atomic rename into place, same as install_bin (ETXTBSY-safe self-update).
    let dest = bin_path(src);
    let dir = dest
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(config::localbin_dir);
    std::fs::create_dir_all(&dir)?;
    let staged = dir.join(format!(".{}.new", src.package_name()));
    std::fs::copy(&exec, &staged)?;
    let mut perms = std::fs::metadata(&staged)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&staged, perms)?;
    std::fs::rename(&staged, &dest)?;

    let _ = std::fs::remove_dir_all(&workdir);
    if file.starts_with(config::cache_dir()) {
        let _ = std::fs::remove_file(file);
    }
    record_bin_version(src, &latest.version)?;
    finish_bin_install(src, &dest)?;
    Ok(())
}

/// Find the binary to install inside an extracted tarball tree. Prefers a file
/// named exactly like the package, then any executable extension-less file
/// (cargo-dist ships `name-vX-target/name`), then a lone file if that's all
/// there is.
fn find_executable(root: &Path, package: &str) -> Option<PathBuf> {
    let mut files = Vec::new();
    collect_files(root, &mut files);
    if let Some(p) = files
        .iter()
        .find(|p| p.file_name().and_then(|n| n.to_str()) == Some(package))
    {
        return Some(p.clone());
    }
    if let Some(p) = files.iter().find(|p| {
        let is_exec = std::fs::metadata(p)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false);
        let no_ext = p
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| !n.contains('.'))
            .unwrap_or(false);
        is_exec && no_ext
    }) {
        return Some(p.clone());
    }
    (files.len() == 1).then(|| files[0].clone())
}

/// Collect every regular file under `dir`, recursively.
fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_files(&p, out);
        } else if p.is_file() {
            out.push(p);
        }
    }
}

/// Write a `bin` app's installed-version sidecar. Public so the UI/CLI can
/// register an already-copied binary without re-downloading it.
pub fn record_bin_version(src: &Source, version: &str) -> Result<()> {
    std::fs::create_dir_all(config::versions_dir())?;
    std::fs::write(bin_sidecar(src), version)?;
    Ok(())
}

fn write_desktop(src: &Source, exec: &Path, icon: Option<&Path>) -> Result<()> {
    let dir = config::desktop_dir();
    std::fs::create_dir_all(&dir)?;
    let mut f = std::fs::File::create(desktop_path(src))?;
    write!(
        f,
        "[Desktop Entry]\n\
         Type=Application\n\
         Name={name}\n\
         Exec={exec}\n\
         Icon={icon}\n\
         Terminal=false\n\
         Categories=Utility;\n",
        name = src.name,
        exec = exec.display(),
        icon = icon
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| src.id.clone()),
    )?;
    Ok(())
}

/// Where an AppImage lives: a custom `install_path` if set, else
/// ~/Applications/<id>.AppImage.
fn appimage_path(src: &Source) -> PathBuf {
    match &src.install_path {
        Some(p) if !p.trim().is_empty() => expand_tilde(p.trim()),
        _ => config::appimage_dir().join(format!("{}.AppImage", src.id)),
    }
}

fn version_sidecar(src: &Source) -> PathBuf {
    config::appimage_dir().join(format!("{}.version", src.id))
}

fn desktop_path(src: &Source) -> PathBuf {
    config::desktop_dir().join(format!("{}.desktop", src.id))
}

/// Where a `bin` app's executable lives: its custom `install_path` if set,
/// otherwise ~/.local/bin/<package>.
fn bin_path(src: &Source) -> PathBuf {
    match &src.install_path {
        Some(p) if !p.trim().is_empty() => expand_tilde(p.trim()),
        _ => config::localbin_dir().join(src.package_name()),
    }
}

/// Expand a leading `~/` (or bare `~`) to the home directory.
fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(rest);
    }
    PathBuf::from(path)
}

fn bin_sidecar(src: &Source) -> PathBuf {
    config::versions_dir().join(src.id.clone())
}

/// Run a privileged command through polkit. pkexec pops the system auth dialog.
fn run_pkexec(args: &[&str]) -> Result<()> {
    let status = Command::new("pkexec")
        .args(args)
        .status()
        .context("pkexec not available")?;
    if status.success() {
        Ok(())
    } else {
        Err(anyhow!("command failed: pkexec {}", args.join(" ")))
    }
}
