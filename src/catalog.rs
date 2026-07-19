//! Building the catalog: for each source, what's installed vs what's latest.

use crate::model::{compare, Latest, Source};
use crate::{backends, sources};
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
            (Some(_), None) => Status::Unknown,
            (Some(i), Some(l)) => match compare(&l.version, i) {
                Ordering::Greater => Status::UpdateAvailable,
                _ => Status::UpToDate,
            },
        }
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
            Status::UpdateAvailable => format!(
                "Installed {} → {}",
                self.installed.as_deref().unwrap_or("?"),
                self.latest.as_ref().map(|l| l.version.as_str()).unwrap_or("?")
            ),
            Status::Unknown => format!(
                "Installed {} · latest unknown",
                self.installed.as_deref().unwrap_or("?")
            ),
        }
    }
}

/// Query every source. Blocking (network + dpkg) — call off the UI thread.
pub fn build(srcs: &[Source]) -> Vec<Entry> {
    srcs.iter()
        .map(|s| Entry {
            source: s.clone(),
            installed: backends::detect_installed(s),
            latest: sources::resolve_latest(s),
        })
        .collect()
}
