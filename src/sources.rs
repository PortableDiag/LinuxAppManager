//! Resolving the latest release for a source.

use crate::model::{Kind, Latest, Origin, Source};
use crate::{config, model};
use anyhow::Result;
use std::path::Path;

/// Best-effort latest release. Errors (network, rate-limit) collapse to `None`
/// so one bad source never blanks the whole catalog. The second tuple element
/// is a corrected `kind` when the stored one no longer matches any release
/// asset (e.g. the author switched a tarball release to a bare binary) — the
/// caller persists it so the entry self-heals.
pub fn resolve_latest(src: &Source) -> (Option<Latest>, Option<Kind>) {
    match &src.origin {
        Origin::Github { repo } => github_latest(src, repo).unwrap_or((None, None)),
        Origin::Local { path } => (local_latest(Path::new(path), src), None),
        Origin::Url { url } => (
            Some(Latest {
                version: "latest".to_string(),
                download_url: url.clone(),
                size: None,
                notes: None,
            }),
            None,
        ),
    }
}

fn github_latest(src: &Source, repo: &str) -> Result<(Option<Latest>, Option<Kind>)> {
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let mut req = ureq::get(&url)
        .set("User-Agent", "LinuxAppManager")
        .set("Accept", "application/vnd.github+json");
    // Anonymous by default (public repos); an optional env token is sent only if
    // the user set one — never the gh login. See github_token().
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
    let mut asset = pick_asset(&assets, src);

    // Self-heal: if the stored kind matches no asset but the release *does* carry
    // an installable one (the author changed the release format), re-detect from
    // the assets already in hand — no extra request — and re-pick with it.
    let mut corrected = None;
    if asset.is_none() {
        if let Some(k) = detect_kind(&assets, src.package_name()) {
            if k != src.kind {
                let healed = Source { kind: k, ..src.clone() };
                asset = pick_asset(&assets, &healed);
                corrected = Some(k);
            }
        }
    }
    let effective_kind = corrected.unwrap_or(src.kind);

    // Use the asset *API* URL (api.github.com/.../releases/assets/{id}). It works
    // anonymously for public repos (and with an optional env token). See
    // backends::download for the octet-stream + redirect dance.
    let (download_url, size) = match &asset {
        Some(a) => (
            a["url"].as_str().unwrap_or("").to_string(),
            a["size"].as_u64(),
        ),
        None => (String::new(), None),
    };

    // deb/appimage/tar need a matching artifact to mean anything; a `bin` app can
    // still report its latest tag (source-only release) with no download.
    if asset.is_none() && effective_kind != Kind::Bin {
        return Ok((None, corrected));
    }
    Ok((
        Some(Latest {
            version,
            download_url,
            size,
            notes,
        }),
        corrected,
    ))
}

/// Parse a config document into sources. Accepts either a bare array (the live
/// sources.json form) or a `{ "version", "sources": [...] }` envelope (export /
/// official form); extra keys are ignored.
pub fn parse_config(text: &str) -> Result<Vec<Source>> {
    let v: serde_json::Value = serde_json::from_str(text)?;
    let arr = if v.is_array() { v } else { v["sources"].clone() };
    Ok(serde_json::from_value(arr)?)
}

/// Fetch the curated official list from the repo (public; anonymous API).
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

/// An OPTIONAL GitHub token, taken only from `$GITHUB_TOKEN` / `$GH_TOKEN`.
/// App Manager uses the anonymous public API by default — it never reads your
/// `gh` login. A token is sent only if you explicitly set one in the
/// environment (e.g. to raise the 60/hour anonymous rate limit). Returns None
/// otherwise, so everything works against public repos with no auth.
pub fn github_token() -> Option<String> {
    for var in ["GITHUB_TOKEN", "GH_TOKEN"] {
        if let Ok(t) = std::env::var(var) {
            if !t.trim().is_empty() {
                return Some(t.trim().to_string());
            }
        }
    }
    None
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

/// Whether an (already-lowercased) asset name is a binary tarball we can unpack.
/// `.sha256`/`.asc` sidecars end differently, so they're naturally excluded.
fn is_tarball(name: &str) -> bool {
    [".tar.gz", ".tgz", ".tar.xz", ".tar.zst", ".tar.bz2", ".tar"]
        .iter()
        .any(|ext| name.ends_with(ext))
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
        Kind::Tar => assets
            .iter()
            .filter(|a| is_tarball(&asset_name(a)))
            .collect(),
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

    // The user's public repos (anonymous API — no gh login used).
    let repos = gh_repo_pages(&format!("https://api.github.com/users/{user}/repos"));
    if repos.is_empty() {
        return Err(anyhow::anyhow!(
            "no public repos found for '{user}'"
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

/// Re-enumerate every followed account and return installable repos that aren't
/// tracked yet. `follow_user` already skips repos already in the list, so this
/// yields only genuinely new apps. Network-heavy — call off the UI thread.
pub fn discover_follows(users: &[String]) -> Vec<Source> {
    let mut out = Vec::new();
    for user in users {
        if let Ok(list) = follow_user(user) {
            out.extend(list);
        }
    }
    out
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

/// The installable kind for a repo's latest release, if any. Fetches the
/// release, then delegates to `detect_kind` on its assets.
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
    detect_kind(assets, exec)
}

/// The installable kind implied by a set of release assets, if any (arch-strict).
/// Preference: a bare binary named like the repo, then AppImage, deb, tarball,
/// then any extension-less binary. Operates on already-fetched assets, so
/// callers that already hold the release JSON incur no extra request.
pub fn detect_kind(assets: &[serde_json::Value], exec: &str) -> Option<Kind> {
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
    let tars: Vec<&serde_json::Value> =
        assets.iter().filter(|a| is_tarball(&asset_name(a))).collect();
    if best_by_arch_strict(&tars).is_some() {
        return Some(Kind::Tar);
    }
    let nodot: Vec<&serde_json::Value> =
        assets.iter().filter(|a| !asset_name(a).contains('.')).collect();
    if best_by_arch_strict(&nodot).is_some() {
        return Some(Kind::Bin);
    }
    None
}

/// Build a Source from a pasted GitHub repo or URL, auto-detecting name,
/// description, and installable kind. Falls back to `bin` when no installable
/// release asset is found (the user can Edit… to adjust).
pub fn resolve_repo(input: &str) -> Result<Source> {
    let repo = parse_repo(input)
        .ok_or_else(|| anyhow::anyhow!("not a GitHub owner/repo or URL"))?;
    let info = gh_get(&format!("https://api.github.com/repos/{repo}"))?;
    let name = info["name"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| repo.rsplit('/').next().unwrap_or(&repo).to_string());
    let description = info["description"].as_str().map(str::to_string);
    let kind = detect_installable(&repo, &name).unwrap_or(Kind::Bin);
    Ok(Source {
        id: name.clone(),
        name: name.clone(),
        description,
        kind,
        package: Some(name.clone()),
        install_path: None,
        origin: Origin::Github { repo },
        auto_update: false,
    })
}

/// Extract `owner/repo` from a URL or bare `owner/repo` string.
fn parse_repo(input: &str) -> Option<String> {
    let s = input
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("www.")
        .trim_start_matches("github.com/")
        .trim_start_matches('/');
    let parts: Vec<&str> = s.split('/').filter(|p| !p.is_empty()).collect();
    (parts.len() >= 2).then(|| format!("{}/{}", parts[0], parts[1]))
}

/// GET a GitHub API endpoint as JSON, with the user's token if available.
fn gh_get(url: &str) -> Result<serde_json::Value> {
    let mut req = ureq::get(url)
        .set("User-Agent", "LinuxAppManager")
        .set("Accept", "application/vnd.github+json");
    if let Some(t) = github_token() {
        req = req.set("Authorization", &format!("Bearer {t}"));
    }
    Ok(req.call()?.into_json()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Kind, Origin, Source};

    #[test]
    fn parses_repo_forms() {
        assert_eq!(parse_repo("owner/repo").as_deref(), Some("owner/repo"));
        assert_eq!(parse_repo("https://github.com/owner/repo").as_deref(), Some("owner/repo"));
        assert_eq!(
            parse_repo("github.com/owner/repo/releases/tag/v1").as_deref(),
            Some("owner/repo")
        );
        assert_eq!(parse_repo("just-a-word"), None);
    }

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
