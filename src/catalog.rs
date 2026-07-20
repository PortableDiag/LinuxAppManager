//! Building the catalog: for each source, what's installed vs what's latest.

use crate::model::{compare, Latest, Source, UNKNOWN_VERSION};
use crate::{backends, config, sources};
use std::cmp::Ordering;

/// One row in the app list.
#[derive(Clone)]
pub struct Entry {
    pub source: Source,
    pub installed: Option<String>,
    pub latest: Option<Latest>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Status {
    NotInstalled,
    UpToDate,
    UpdateAvailable,
    /// Installed but we couldn't resolve a latest version to compare against.
    Unknown,
}

impl Entry {
    pub fn status(&self) -> Status {
        match (&self.installed, &self.latest) {
            (None, _) => Status::NotInstalled,
            // Binary present but its version is indeterminate (a build of your
            // own that App Manager didn't install) — don't imply an update.
            (Some(i), _) if i == UNKNOWN_VERSION => Status::Unknown,
            (Some(_), None) => Status::Unknown,
            (Some(i), Some(l)) => match compare(&l.version, i) {
                Ordering::Greater => Status::UpdateAvailable,
                _ => Status::UpToDate,
            },
        }
    }

    /// Whether we actually have something to download (a resolved asset URL).
    /// A `bin` app can know its latest tag yet have no downloadable artifact.
    pub fn installable(&self) -> bool {
        self.latest
            .as_ref()
            .map(|l| !l.download_url.is_empty())
            .unwrap_or(false)
    }

    /// Subtitle line, mirroring the Android app's phrasing.
    pub fn subtitle(&self) -> String {
        match self.status() {
            Status::NotInstalled => match &self.latest {
                Some(l) => format!("Not installed · Latest {}", l.version),
                None => "Not installed".to_string(),
            },
            Status::UpToDate => {
                format!("Up to date · {}", self.installed.as_deref().unwrap_or("?"))
            }
            Status::UpdateAvailable => {
                let base = format!(
                    "Installed {} → {}",
                    self.installed.as_deref().unwrap_or("?"),
                    self.latest.as_ref().map(|l| l.version.as_str()).unwrap_or("?")
                );
                // Tag exists but there's nothing to download (source-only release).
                if self.installable() {
                    base
                } else {
                    format!("{base} · source only")
                }
            }
            Status::Unknown => {
                let inst = self.installed.as_deref().unwrap_or("?");
                if inst == UNKNOWN_VERSION {
                    match &self.latest {
                        Some(l) => format!("Installed · version unknown · latest {}", l.version),
                        None => "Installed · version unknown".to_string(),
                    }
                } else {
                    format!("Installed {inst} · latest unknown")
                }
            }
        }
    }
}

/// Query every source. Blocking (network + dpkg) — call off the UI thread.
/// If a source's stored `kind` no longer matches its release (the author
/// changed the packaging), it's re-detected here and the corrected list is
/// persisted so the entry self-heals — both the returned Entry and the saved
/// source carry the new kind, so a follow-up Install uses the right backend.
pub fn build(srcs: &[Source]) -> Vec<Entry> {
    let mut list = srcs.to_vec();
    let mut healed = false;
    let entries = list
        .iter_mut()
        .map(|s| {
            let (latest, corrected) = sources::resolve_latest(s);
            if let Some(kind) = corrected {
                s.kind = kind;
                healed = true;
            }
            Entry {
                source: s.clone(),
                installed: backends::detect_installed(s),
                latest,
            }
        })
        .collect();
    if healed {
        let _ = config::save_sources(&list);
    }
    entries
}

/// Outcome of an auto-update pass.
#[derive(Default)]
pub struct AutoUpdate {
    pub updated: Vec<String>,
    pub failed: Vec<(String, String)>,
}

/// Install pending updates for every `auto_update` source that actually has a
/// downloadable one waiting. Blocking — run off the UI thread.
pub fn auto_update(srcs: &[Source]) -> AutoUpdate {
    let mut out = AutoUpdate::default();
    for entry in build(srcs) {
        if !entry.source.auto_update
            || entry.status() != Status::UpdateAvailable
            || !entry.installable()
        {
            continue;
        }
        let Some(latest) = &entry.latest else { continue };
        match backends::install(&entry.source, latest) {
            Ok(()) => out.updated.push(entry.source.name.clone()),
            Err(e) => out.failed.push((entry.source.name.clone(), e.to_string())),
        }
    }
    out
}
