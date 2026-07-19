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

fn sources_path() -> PathBuf {
    config_dir().join("sources.json")
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

/// Merge incoming sources into existing, matching on `id` (incoming wins).
/// Returns the merged list plus (added, updated) counts.
pub fn merge(existing: &[Source], incoming: Vec<Source>) -> (Vec<Source>, usize, usize) {
    let mut out = existing.to_vec();
    let (mut added, mut updated) = (0, 0);
    for s in incoming {
        match out.iter().position(|e| e.id == s.id) {
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
