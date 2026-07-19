//! Resolving the latest release for a source.

use crate::model::{Kind, Latest, Origin, Source};
use crate::{config, model};
use anyhow::Result;
use std::path::Path;

/// Best-effort latest release. Errors (network, rate-limit) collapse to `None`
/// so one bad source never blanks the whole catalog.
pub fn resolve_latest(src: &Source) -> Option<Latest> {
    match &src.origin {
        Origin::Github { repo } => github_latest(repo, src.kind).ok().flatten(),
        Origin::Local { path } => local_latest(Path::new(path), src),
        Origin::Url { url } => Some(Latest {
            version: "latest".to_string(),
            download_url: url.clone(),
            size: None,
            notes: None,
        }),
    }
}

fn github_latest(repo: &str, kind: Kind) -> Result<Option<Latest>> {
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let resp = ureq::get(&url)
        .set("User-Agent", "LinuxAppManager")
        .set("Accept", "application/vnd.github+json")
        .call()?;
    let json: serde_json::Value = resp.into_json()?;

    let version = json["tag_name"]
        .as_str()
        .unwrap_or("")
        .trim_start_matches('v')
        .to_string();
    let notes = json["body"].as_str().map(|s| s.to_string());

    let ext = kind.ext();
    let assets = json["assets"].as_array().cloned().unwrap_or_default();
    let asset = assets.iter().find(|a| {
        a["name"]
            .as_str()
            .map(|n| n.to_lowercase().ends_with(ext))
            .unwrap_or(false)
    });

    let Some(asset) = asset else { return Ok(None) };
    Ok(Some(Latest {
        version,
        download_url: asset["browser_download_url"]
            .as_str()
            .unwrap_or("")
            .to_string(),
        size: asset["size"].as_u64(),
        notes,
    }))
}

/// Highest-versioned matching file in a local folder.
fn local_latest(dir: &Path, src: &Source) -> Option<Latest> {
    let ext = src.kind.ext();
    let mut best: Option<(String, std::path::PathBuf, u64)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        let name = path.file_name()?.to_string_lossy().to_lowercase();
        if !name.ends_with(ext) {
            continue;
        }
        let size = entry.metadata().ok().map(|m| m.len()).unwrap_or(0);
        let ver = version_from_filename(&name);
        match &best {
            Some((bv, _, _)) if model::compare(&ver, bv) != std::cmp::Ordering::Greater => {}
            _ => best = Some((ver, path, size)),
        }
    }
    let (version, path, size) = best?;
    let _ = config::cache_dir(); // (reserved for future caching)
    Some(Latest {
        version,
        download_url: format!("file://{}", path.display()),
        size: Some(size),
        notes: None,
    })
}

/// Pull a version-looking token out of a filename, else "0".
fn version_from_filename(name: &str) -> String {
    name.split(|c: char| c == '-' || c == '_')
        .find(|part| part.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false))
        .map(|s| s.trim_end_matches(".deb").trim_end_matches(".appimage").to_string())
        .unwrap_or_else(|| "0".to_string())
}
