//! Loading the source list and resolving app directories.

use crate::model::Source;
use anyhow::Result;
use std::path::{Path, PathBuf};

/// The repo (and file within it) that serves the curated "official" list.
pub const OFFICIAL_REPO: &str = "PortableDiag/LinuxAppManager";
pub const OFFICIAL_PATH: &str = "official-config.json";

/// ~/.config/linux-app-manager
pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("linux-app-manager")
}

/// ~/.cache/linux-app-manager (downloads land here).
pub fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("linux-app-manager")
}

/// ~/Applications — where AppImages are kept.
pub fn appimage_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Applications")
}

/// ~/.local/share/linux-app-manager/app — the extracted self-contained bundle
/// (AppRun + all shared libraries) that self-install unpacks the AppImage
/// into. Launching it is a plain exec — no AppImage runtime, no FUSE.
pub fn self_bundle_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("linux-app-manager")
        .join("app")
}

/// ~/.local/share/linux-app-manager/apps/<id> — installed AppImages, unpacked.
/// Launching an installed app execs its AppRun directly, so it can't break on
/// a machine where FUSE or the AppImage runtime misbehaves.
pub fn apps_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("linux-app-manager")
        .join("apps")
}

/// ~/.local/bin — where single-executable (`bin`) apps live.
pub fn localbin_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".local")
        .join("bin")
}

/// Where `bin`-kind installed-version sidecars are recorded (config dir, so we
/// don't clutter ~/.local/bin with dotfiles).
pub fn versions_dir() -> PathBuf {
    config_dir().join("versions")
}

/// ~/.local/share/applications — .desktop entries.
pub fn desktop_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("applications")
}

/// ~/.local/share/icons/hicolor/scalable/apps — scalable app icons.
pub fn data_dir_icons() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("icons/hicolor/scalable/apps")
}

fn sources_path() -> PathBuf {
    config_dir().join("sources.json")
}

/// Followed GitHub usernames, re-scanned on startup/refresh for new repos.
fn follows_path() -> PathBuf {
    config_dir().join("follows.json")
}

/// The GitHub accounts the user has "followed" (subscribed to for discovery).
pub fn load_follows() -> Vec<String> {
    std::fs::read_to_string(follows_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Remember a followed account so future repos of theirs get auto-discovered.
/// No-op if already followed (case-insensitive).
pub fn add_follow(user: &str) -> Result<()> {
    let user = user.trim();
    if user.is_empty() {
        return Ok(());
    }
    let mut list = load_follows();
    if list.iter().any(|u| u.eq_ignore_ascii_case(user)) {
        return Ok(());
    }
    list.push(user.to_string());
    std::fs::create_dir_all(config_dir())?;
    std::fs::write(follows_path(), serde_json::to_string_pretty(&list)?)?;
    Ok(())
}

/// Load the source list, seeding a default file on first run.
pub fn load_sources() -> Result<Vec<Source>> {
    let path = sources_path();
    if !path.exists() {
        std::fs::create_dir_all(config_dir())?;
        std::fs::write(&path, DEFAULT_SOURCES)?;
    }
    let data = std::fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&data)?)
}

/// Overwrite the live source list.
pub fn save_sources(srcs: &[Source]) -> Result<()> {
    std::fs::create_dir_all(config_dir())?;
    std::fs::write(sources_path(), serde_json::to_string_pretty(srcs)?)?;
    Ok(())
}

/// Merge incoming sources into existing, matching on `id`, or failing that on
/// the same GitHub repo (incoming wins). The repo match keeps an official-list
/// entry from duplicating an auto-discovered one — discovery names sources
/// after the bare repo ("gapless") while the curated list uses reverse-DNS ids.
pub fn merge(existing: &[Source], incoming: Vec<Source>) -> (Vec<Source>, usize, usize) {
    fn repo(s: &Source) -> Option<String> {
        match &s.origin {
            crate::model::Origin::Github { repo } => Some(repo.to_lowercase()),
            _ => None,
        }
    }
    let mut out = existing.to_vec();
    let (mut added, mut updated) = (0, 0);
    for s in incoming {
        let pos = out
            .iter()
            .position(|e| e.id == s.id)
            .or_else(|| out.iter().position(|e| repo(e).is_some() && repo(e) == repo(&s)));
        match pos {
            Some(pos) => {
                out[pos] = s;
                updated += 1;
            }
            None => {
                out.push(s);
                added += 1;
            }
        }
    }
    (out, added, updated)
}

/// Write a shareable config file: the same `{version, sources}` envelope the
/// official list uses, so an export can be dropped straight into the repo.
pub fn export_config(srcs: &[Source], dest: &Path) -> Result<()> {
    let doc = serde_json::json!({
        "_comment": "Linux App Manager config. Import via the header ▾ menu. Only 'sources' is read.",
        "version": 1,
        "sources": srcs,
    });
    std::fs::write(dest, serde_json::to_string_pretty(&doc)?)?;
    Ok(())
}

/// The manager ships in its own list, exactly like the Android App Manager.
/// It installs as a single binary in ~/.local/bin, so its own kind is `bin`.
const DEFAULT_SOURCES: &str = r#"[
  {
    "id": "com.procomputation.LinuxAppManager",
    "name": "App Manager",
    "description": "This app. Manages itself.",
    "kind": "bin",
    "package": "linux-app-manager",
    "auto_update": true,
    "origin": { "type": "github", "repo": "PortableDiag/LinuxAppManager" }
  }
]
"#;

/// Remove a source from the list by id.
pub fn remove_source(id: &str) -> Result<()> {
    let mut srcs = load_sources()?;
    srcs.retain(|s| s.id != id);
    save_sources(&srcs)
}

/// Flip a single source's auto-update flag and save.
pub fn set_auto_update(id: &str, on: bool) -> Result<()> {
    let mut srcs = load_sources()?;
    if let Some(s) = srcs.iter_mut().find(|s| s.id == id) {
        s.auto_update = on;
    }
    save_sources(&srcs)
}
