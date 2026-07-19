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
use std::path::PathBuf;
use std::process::Command;

// Embedded so a loose binary (USB / fresh download) can install itself with its
// icon and menu entry, no network and no side files needed.
const SELF_ICON: &str = include_str!("../data/com.procomputation.LinuxAppManager.svg");
const SELF_DESKTOP: &str = include_str!("../data/com.procomputation.LinuxAppManager.desktop");
const SELF_ID: &str = "com.procomputation.LinuxAppManager";

/// Install the *currently running* App Manager binary to ~/.local/bin, plus its
/// icon and .desktop entry. Copies this executable (works offline / from a USB),
/// not a download. Atomic rename, so it's safe even over the running install.
pub fn install_self() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("locating the running binary")?;
    let dir = config::localbin_dir();
    std::fs::create_dir_all(&dir)?;
    let dest = dir.join("linux-app-manager");
    let staged = dir.join(".linux-app-manager.new");
    std::fs::copy(&exe, &staged).context("copying binary")?;
    let mut perms = std::fs::metadata(&staged)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&staged, perms)?;
    std::fs::rename(&staged, &dest)?;

    let icon_dir = config::data_dir_icons();
    std::fs::create_dir_all(&icon_dir)?;
    std::fs::write(icon_dir.join(format!("{SELF_ID}.svg")), SELF_ICON)?;
    let app_dir = config::desktop_dir();
    std::fs::create_dir_all(&app_dir)?;
    std::fs::write(app_dir.join(format!("{SELF_ID}.desktop")), SELF_DESKTOP)?;

    std::fs::create_dir_all(config::versions_dir())?;
    std::fs::write(config::versions_dir().join(SELF_ID), env!("CARGO_PKG_VERSION"))?;
    Ok(dest)
}

/// Installed version, or `None` if not present.
pub fn detect_installed(src: &Source) -> Option<String> {
    match src.kind {
        Kind::Deb => {
            let out = Command::new("dpkg-query")
                .args(["-W", "-f=${Version}", src.package_name()])
                .output()
                .ok()?;
            if !out.status.success() {
                return None;
            }
            let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
            (!v.is_empty()).then_some(v)
        }
        Kind::AppImage => {
            let sidecar = version_sidecar(src);
            std::fs::read_to_string(sidecar)
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        }
        Kind::Bin => {
            if !bin_path(src).exists() {
                return None;
            }
            // Binary is present; report the recorded version, else "unknown".
            Some(
                std::fs::read_to_string(bin_sidecar(src))
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| crate::model::UNKNOWN_VERSION.to_string()),
            )
        }
    }
}

/// Download the latest release and install/update it.
pub fn install(src: &Source, latest: &Latest) -> Result<()> {
    std::fs::create_dir_all(config::cache_dir())?;
    let file = download(src, latest)?;
    match src.kind {
        Kind::Deb => install_deb(&file),
        Kind::AppImage => install_appimage(src, latest, &file),
        Kind::Bin => install_bin(src, latest, &file),
    }
}

/// Remove an installed app.
pub fn remove(src: &Source) -> Result<()> {
    match src.kind {
        Kind::Deb => run_pkexec(&["apt-get", "remove", "-y", src.package_name()]),
        Kind::AppImage => {
            let _ = std::fs::remove_file(appimage_path(src));
            let _ = std::fs::remove_file(version_sidecar(src));
            let _ = std::fs::remove_file(desktop_path(src));
            Ok(())
        }
        Kind::Bin => {
            let _ = std::fs::remove_file(bin_path(src));
            let _ = std::fs::remove_file(bin_sidecar(src));
            Ok(())
        }
    }
}

/// Launch an installed app (best effort).
pub fn open(src: &Source) -> Result<()> {
    match src.kind {
        Kind::AppImage => {
            Command::new(appimage_path(src))
                .spawn()
                .context("launching AppImage")?;
            Ok(())
        }
        Kind::Bin => {
            Command::new(bin_path(src))
                .spawn()
                .context("launching binary")?;
            Ok(())
        }
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
    let dir = config::appimage_dir();
    std::fs::create_dir_all(&dir)?;
    let dest = appimage_path(src);
    // Move into place (copy across filesystems, then drop the temp).
    if file != &dest {
        std::fs::copy(file, &dest)?;
        if file.starts_with(config::cache_dir()) {
            let _ = std::fs::remove_file(file);
        }
    }
    let mut perms = std::fs::metadata(&dest)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&dest, perms)?;

    write_desktop(src, &dest)?;
    std::fs::write(version_sidecar(src), &latest.version)?;
    Ok(())
}

fn install_bin(src: &Source, latest: &Latest, file: &PathBuf) -> Result<()> {
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
    Ok(())
}

/// Write a `bin` app's installed-version sidecar. Public so the UI/CLI can
/// register an already-copied binary without re-downloading it.
pub fn record_bin_version(src: &Source, version: &str) -> Result<()> {
    std::fs::create_dir_all(config::versions_dir())?;
    std::fs::write(bin_sidecar(src), version)?;
    Ok(())
}

fn write_desktop(src: &Source, exec: &PathBuf) -> Result<()> {
    let dir = config::desktop_dir();
    std::fs::create_dir_all(&dir)?;
    let mut f = std::fs::File::create(desktop_path(src))?;
    write!(
        f,
        "[Desktop Entry]\n\
         Type=Application\n\
         Name={name}\n\
         Exec={exec}\n\
         Icon={id}\n\
         Terminal=false\n\
         Categories=Utility;\n",
        name = src.name,
        exec = exec.display(),
        id = src.id,
    )?;
    Ok(())
}

fn appimage_path(src: &Source) -> PathBuf {
    config::appimage_dir().join(format!("{}.AppImage", src.id))
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
