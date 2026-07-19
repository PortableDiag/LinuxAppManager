//! Loading the source list and resolving app directories.

use crate::model::Source;
use anyhow::Result;
use std::path::PathBuf;

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

/// The manager ships in its own list, exactly like the Android App Manager.
const DEFAULT_SOURCES: &str = r#"[
  {
    "id": "com.procomputation.LinuxAppManager",
    "name": "App Manager",
    "description": "This app. Manages itself.",
    "kind": "appimage",
    "origin": { "type": "github", "repo": "PortableDiag/LinuxAppManager" }
  }
]
"#;
