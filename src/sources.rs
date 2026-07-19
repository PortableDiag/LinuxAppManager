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

    // Use the asset *API* URL (api.github.com/.../releases/assets/{id}) rather
    // than browser_download_url: it accepts a token, so private-repo assets
    // download. See backends::download for the octet-stream + redirect dance.
    let (download_url, size) = match &asset {
        Some(a) => (
            a["url"].as_str().unwrap_or("").to_string(),
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
pub fn github_token() -> Option<String> {
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

// Arch tokens seen in release asset names. HOST is what matches this build.
#[cfg(target_arch = "x86_64")]
const HOST_ARCH: &[&str] = &["x86_64", "amd64", "x64"];
#[cfg(target_arch = "aarch64")]
const HOST_ARCH: &[&str] = &["aarch64", "arm64"];
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
const HOST_ARCH: &[&str] = &[];

const ARCH_TOKENS: &[&str] = &[
    "x86_64", "amd64", "x64", "aarch64", "arm64", "armv7l", "armv7", "armhf",
    "arm", "i686", "i386", "ppc64le", "ppc64", "s390x", "riscv64",
];

fn asset_name(a: &serde_json::Value) -> String {
    a["name"].as_str().unwrap_or("").to_lowercase()
}

/// From assets already filtered to the right kind, pick the one for this host's
/// architecture: prefer a name carrying a host-arch token; otherwise the first
/// that carries no *foreign*-arch token (arch-neutral); else the first.
fn best_by_arch<'a>(cands: &[&'a serde_json::Value]) -> Option<&'a serde_json::Value> {
    if let Some(a) = cands
        .iter()
        .find(|a| HOST_ARCH.iter().any(|g| asset_name(a).contains(g)))
    {
        return Some(a);
    }
    if let Some(a) = cands.iter().find(|a| {
        let n = asset_name(a);
        !ARCH_TOKENS
            .iter()
            .any(|t| !HOST_ARCH.contains(t) && n.contains(t))
    }) {
        return Some(a);
    }
    cands.first().copied()
}

/// Like `best_by_arch` but strict: no "else first" fallback. Returns a match
/// only if there's a host-arch asset or a truly arch-neutral one (no arch token
/// at all). Used to decide whether a repo is installable on this host.
fn best_by_arch_strict<'a>(cands: &[&'a serde_json::Value]) -> Option<&'a serde_json::Value> {
    if let Some(a) = cands
        .iter()
        .find(|a| HOST_ARCH.iter().any(|g| asset_name(a).contains(g)))
    {
        return Some(a);
    }
    cands
        .iter()
        .find(|a| {
            let n = asset_name(a);
            !ARCH_TOKENS.iter().any(|t| n.contains(t))
        })
        .copied()
}

/// Choose the release asset that matches this source's kind and host arch.
/// deb/appimage go by extension; a `bin` app matches an asset named exactly
/// like its executable (or any extension-less asset).
fn pick_asset<'a>(assets: &'a [serde_json::Value], src: &Source) -> Option<&'a serde_json::Value> {
    let cands: Vec<&serde_json::Value> = match src.kind {
        Kind::Deb | Kind::AppImage => {
            let ext = src.kind.ext();
            assets
                .iter()
                .filter(|a| asset_name(a).ends_with(ext))
                .collect()
        }
        Kind::Bin => {
            let exec = src.package_name();
            // An exact executable-name match wins outright.
            if let Some(a) = assets.iter().find(|a| a["name"].as_str() == Some(exec)) {
                return Some(a);
            }
            assets
                .iter()
                .filter(|a| !asset_name(a).contains('.'))
                .collect()
        }
    };
    best_by_arch(&cands)
}

/// Enumerate an account's repos (the token owner's own — incl. private — plus
/// the named user's public repos), then keep those whose latest release has an
/// asset installable on this host, auto-detecting the kind. Network-heavy: one
/// release lookup per repo.
pub fn follow_user(user: &str) -> Result<Vec<Source>> {
    let user = user
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("github.com/")
        .trim_matches('/')
        .to_string();
    if user.is_empty() {
        return Err(anyhow::anyhow!("no username given"));
    }

    // The token owner's own repos (public + private), filtered to this account.
    let mut repos = gh_repo_pages("https://api.github.com/user/repos?affiliation=owner");
    repos.retain(|r| {
        r["owner"]["login"]
            .as_str()
            .map(|l| l.eq_ignore_ascii_case(&user))
            .unwrap_or(false)
    });
    // Plus the user's public repos (covers other people's accounts).
    let mut seen: std::collections::HashSet<String> = repos
        .iter()
        .filter_map(|r| r["full_name"].as_str().map(str::to_string))
        .collect();
    for r in gh_repo_pages(&format!("https://api.github.com/users/{user}/repos")) {
        if let Some(f) = r["full_name"].as_str() {
            if seen.insert(f.to_string()) {
                repos.push(r);
            }
        }
    }
    if repos.is_empty() {
        return Err(anyhow::anyhow!(
            "no repos found for '{user}' (private repos need your gh token)"
        ));
    }

    // Don't re-add repos already tracked (even under a different id, e.g. a
    // curated com.procomputation.* entry pointing at the same GitHub repo).
    let already: std::collections::HashSet<String> = config::load_sources()
        .unwrap_or_default()
        .iter()
        .filter_map(|s| match &s.origin {
            Origin::Github { repo } => Some(repo.to_lowercase()),
            _ => None,
        })
        .collect();

    // Candidates: not already tracked, valid name/full_name.
    let candidates: Vec<(String, String, Option<String>)> = repos
        .iter()
        .filter_map(|r| {
            let full = r["full_name"].as_str()?;
            let name = r["name"].as_str()?;
            if already.contains(&full.to_lowercase()) {
                return None;
            }
            Some((full.to_string(), name.to_string(), r["description"].as_str().map(str::to_string)))
        })
        .collect();

    // One release lookup per repo — do them in parallel so a big account
    // doesn't take minutes.
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    let out = Mutex::new(Vec::new());
    let next = AtomicUsize::new(0);
    let workers = candidates.len().min(8).max(1);
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some((full, name, desc)) = candidates.get(i) else { break };
                if let Some(kind) = detect_installable(full, name) {
                    out.lock().unwrap().push(Source {
                        id: name.clone(),
                        name: name.clone(),
                        description: desc.clone(),
                        kind,
                        package: Some(name.clone()),
                        install_path: None,
                        origin: Origin::Github { repo: full.clone() },
                        auto_update: false,
                    });
                }
            });
        }
    });
    Ok(out.into_inner().unwrap())
}

/// Fetch up to 1000 repos from a paginated /repos endpoint (best effort).
fn gh_repo_pages(base: &str) -> Vec<serde_json::Value> {
    let sep = if base.contains('?') { '&' } else { '?' };
    let mut out = Vec::new();
    for page in 1..=10 {
        let url = format!("{base}{sep}per_page=100&page={page}");
        let mut req = ureq::get(&url)
            .set("User-Agent", "LinuxAppManager")
            .set("Accept", "application/vnd.github+json");
        if let Some(t) = github_token() {
            req = req.set("Authorization", &format!("Bearer {t}"));
        }
        let Ok(resp) = req.call() else { break };
        let Ok(json) = resp.into_json::<serde_json::Value>() else { break };
        let items = json.as_array().cloned().unwrap_or_default();
        let n = items.len();
        out.extend(items);
        if n < 100 {
            break;
        }
    }
    out
}

/// The installable kind for a repo's latest release, if any (arch-strict).
/// Preference: a bare binary named like the repo, then AppImage, then deb.
fn detect_installable(repo: &str, exec: &str) -> Option<Kind> {
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let mut req = ureq::get(&url)
        .set("User-Agent", "LinuxAppManager")
        .set("Accept", "application/vnd.github+json");
    if let Some(t) = github_token() {
        req = req.set("Authorization", &format!("Bearer {t}"));
    }
    let json: serde_json::Value = req.call().ok()?.into_json().ok()?;
    let assets = json["assets"].as_array()?;

    let bin_exact: Vec<&serde_json::Value> =
        assets.iter().filter(|a| a["name"].as_str() == Some(exec)).collect();
    if best_by_arch_strict(&bin_exact).is_some() {
        return Some(Kind::Bin);
    }
    let appimg: Vec<&serde_json::Value> =
        assets.iter().filter(|a| asset_name(a).ends_with(".appimage")).collect();
    if best_by_arch_strict(&appimg).is_some() {
        return Some(Kind::AppImage);
    }
    let debs: Vec<&serde_json::Value> =
        assets.iter().filter(|a| asset_name(a).ends_with(".deb")).collect();
    if best_by_arch_strict(&debs).is_some() {
        return Some(Kind::Deb);
    }
    let nodot: Vec<&serde_json::Value> =
        assets.iter().filter(|a| !asset_name(a).contains('.')).collect();
    if best_by_arch_strict(&nodot).is_some() {
        return Some(Kind::Bin);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Kind, Origin, Source};

    fn assets(names: &[&str]) -> Vec<serde_json::Value> {
        names
            .iter()
            .map(|n| serde_json::json!({ "name": n }))
            .collect()
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn picks_host_arch_deb() {
        let src = Source {
            id: "tabby".into(),
            name: "Tabby".into(),
            description: None,
            kind: Kind::Deb,
            origin: Origin::Github { repo: "Eugeny/tabby".into() },
            package: Some("tabby".into()),
            install_path: None,
            auto_update: false,
        };
        // arm64 deb listed first must NOT win on x86_64.
        let a = assets(&[
            "tabby-1.0-linux-arm64.deb",
            "tabby-1.0-linux-armv7l.deb",
            "tabby-1.0-linux-x64.deb",
        ]);
        let picked = pick_asset(&a, &src).unwrap();
        assert_eq!(picked["name"], "tabby-1.0-linux-x64.deb");
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
