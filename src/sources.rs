//! Resolving the latest release for a source.

use crate::model::{Kind, Latest, Origin, Source};
use crate::{config, model};
use anyhow::Result;
use std::path::Path;

/// Best-effort latest release. Errors (network, rate-limit) collapse to `None`
/// so one bad source never blanks the whole catalog.
pub fn resolve_latest(src: &Source) -> Option<Latest> {
    match &src.origin {
        Origin::Github { repo } => github_latest(src, repo).ok().flatten(),
        Origin::Local { path } => local_latest(Path::new(path), src),
        Origin::Url { url } => Some(Latest {
            version: "latest".to_string(),
            download_url: url.clone(),
            size: None,
            notes: None,
        }),
    }
}

fn github_latest(src: &Source, repo: &str) -> Result<Option<Latest>> {
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let mut req = ureq::get(&url)
        .set("User-Agent", "LinuxAppManager")
        .set("Accept", "application/vnd.github+json");
    // Private repos 404 without auth. Reuse the user's existing GitHub token.
    if let Some(token) = github_token() {
        req = req.set("Authorization", &format!("Bearer {token}"));
    }
    let resp = req.call()?;
    let json: serde_json::Value = resp.into_json()?;

    let version = json["tag_name"]
        .as_str()
        .unwrap_or("")
        .trim_start_matches('v')
        .to_string();
    let notes = json["body"].as_str().map(|s| s.to_string());

    let assets = json["assets"].as_array().cloned().unwrap_or_default();
    let asset = pick_asset(&assets, src);

    let (download_url, size) = match &asset {
        Some(a) => (
            a["browser_download_url"].as_str().unwrap_or("").to_string(),
            a["size"].as_u64(),
        ),
        None => (String::new(), None),
    };

    // deb/appimage need a matching artifact to mean anything; a `bin` app can
    // still report its latest tag (source-only release) with no download.
    if asset.is_none() && src.kind != Kind::Bin {
        return Ok(None);
    }
    Ok(Some(Latest {
        version,
        download_url,
        size,
        notes,
    }))
}

/// Parse a config document into sources. Accepts either a bare array (the live
/// sources.json form) or a `{ "version", "sources": [...] }` envelope (export /
/// official form); extra keys are ignored.
pub fn parse_config(text: &str) -> Result<Vec<Source>> {
    let v: serde_json::Value = serde_json::from_str(text)?;
    let arr = if v.is_array() { v } else { v["sources"].clone() };
    Ok(serde_json::from_value(arr)?)
}

/// Fetch the curated official list from the repo (works for a private repo via
/// the API's raw media type + the user's token).
pub fn fetch_official() -> Result<Vec<Source>> {
    let url = format!(
        "https://api.github.com/repos/{}/contents/{}",
        config::OFFICIAL_REPO,
        config::OFFICIAL_PATH
    );
    let mut req = ureq::get(&url)
        .set("User-Agent", "LinuxAppManager")
        .set("Accept", "application/vnd.github.raw+json");
    if let Some(token) = github_token() {
        req = req.set("Authorization", &format!("Bearer {token}"));
    }
    let text = req.call()?.into_string()?;
    parse_config(&text)
}

/// A GitHub token for private-repo access: `$GITHUB_TOKEN` / `$GH_TOKEN`, else
/// whatever `gh auth token` reports. `None` if the user has no gh login.
fn github_token() -> Option<String> {
    for var in ["GITHUB_TOKEN", "GH_TOKEN"] {
        if let Ok(t) = std::env::var(var) {
            if !t.trim().is_empty() {
                return Some(t.trim().to_string());
            }
        }
    }
    let out = std::process::Command::new("gh")
        .args(["auth", "token"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!t.is_empty()).then_some(t)
}

/// Choose the release asset that matches this source's kind. deb/appimage go by
/// extension; a `bin` app matches an asset named exactly like its executable
/// (or any extension-less asset).
fn pick_asset<'a>(assets: &'a [serde_json::Value], src: &Source) -> Option<&'a serde_json::Value> {
    match src.kind {
        Kind::Deb | Kind::AppImage => {
            let ext = src.kind.ext();
            assets.iter().find(|a| {
                a["name"]
                    .as_str()
                    .map(|n| n.to_lowercase().ends_with(ext))
                    .unwrap_or(false)
            })
        }
        Kind::Bin => {
            let exec = src.package_name();
            assets.iter().find(|a| {
                a["name"]
                    .as_str()
                    .map(|n| n == exec || !n.contains('.'))
                    .unwrap_or(false)
            })
        }
    }
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
