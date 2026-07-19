//! Core data types and a lenient version comparator.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

/// How an app is delivered and managed on the system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// A `.deb` installed via apt (needs pkexec/polkit).
    Deb,
    /// An `.AppImage` dropped in ~/Applications (no root).
    AppImage,
}

impl Kind {
    /// Lowercase file extension used to pick a release asset.
    pub fn ext(self) -> &'static str {
        match self {
            Kind::Deb => ".deb",
            Kind::AppImage => ".appimage",
        }
    }
}

/// Where a source's latest release comes from.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Origin {
    /// "owner/repo" on github.com — latest release asset resolved via the API.
    Github { repo: String },
    /// A direct download URL to a .deb/.AppImage.
    Url { url: String },
    /// A local folder holding release files.
    Local { path: String },
}

/// One app the manager knows about.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    /// Stable id (also the default dpkg package name / AppImage basename).
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub kind: Kind,
    pub origin: Origin,
    /// For `deb`: the dpkg package to query, if different from `id`.
    #[serde(default)]
    pub package: Option<String>,
}

impl Source {
    /// dpkg package name for deb sources (falls back to `id`).
    pub fn package_name(&self) -> &str {
        self.package.as_deref().unwrap_or(&self.id)
    }
}

/// The latest available release for a source.
#[derive(Debug, Clone)]
pub struct Latest {
    pub version: String,
    pub download_url: String,
    // Surfaced in the per-app detail view (roadmap); resolved now so the data
    // is already there when that lands.
    #[allow(dead_code)]
    pub size: Option<u64>,
    #[allow(dead_code)]
    pub notes: Option<String>,
}

/// Compare two version strings leniently: numeric runs compared as numbers,
/// a leading `v` ignored, separators skipped. Good enough for deb tags and
/// AppImage release names alike (it is not full dpkg version semantics).
pub fn compare(a: &str, b: &str) -> Ordering {
    let ta = tokenize(a);
    let tb = tokenize(b);
    for i in 0..ta.len().max(tb.len()) {
        match (ta.get(i), tb.get(i)) {
            (Some(Tok::Num(x)), Some(Tok::Num(y))) => match x.cmp(y) {
                Ordering::Equal => {}
                o => return o,
            },
            (Some(Tok::Word(x)), Some(Tok::Word(y))) => match x.cmp(y) {
                Ordering::Equal => {}
                o => return o,
            },
            // A numeric segment outranks an alpha one at the same position
            // (1.0 > 1.0beta once we compare the extra tail below).
            (Some(Tok::Num(_)), Some(Tok::Word(_))) => return Ordering::Greater,
            (Some(Tok::Word(_)), Some(Tok::Num(_))) => return Ordering::Less,
            // One version has extra trailing tokens: judge them against "nothing".
            (Some(_), None) => return tail_sign(&ta[i..]),
            (None, Some(_)) => return tail_sign(&tb[i..]).reverse(),
            (None, None) => {}
        }
    }
    Ordering::Equal
}

/// Compare a leftover token run against an absent tail: trailing `.0`s are
/// equal (1.4.0 == 1.4), a further number is greater (1.4.1 > 1.4), a word is
/// a pre-release and thus lesser (1.0beta < 1.0).
fn tail_sign(rest: &[Tok]) -> Ordering {
    for t in rest {
        match t {
            Tok::Num(0) => continue,
            Tok::Num(_) => return Ordering::Greater,
            Tok::Word(_) => return Ordering::Less,
        }
    }
    Ordering::Equal
}

enum Tok {
    Num(u64),
    Word(String),
}

fn tokenize(s: &str) -> Vec<Tok> {
    let s = s.trim().trim_start_matches('v');
    let mut out = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            let mut n = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_ascii_digit() {
                    n.push(c);
                    chars.next();
                } else {
                    break;
                }
            }
            out.push(Tok::Num(n.parse().unwrap_or(0)));
        } else if c.is_alphabetic() {
            let mut w = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_alphabetic() {
                    w.push(c);
                    chars.next();
                } else {
                    break;
                }
            }
            out.push(Tok::Word(w.to_lowercase()));
        } else {
            chars.next(); // skip separators (. - _ + etc.)
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering::*;

    #[test]
    fn versions() {
        assert_eq!(compare("1.4", "1.3"), Greater);
        assert_eq!(compare("v1.4.0", "1.4"), Equal);
        assert_eq!(compare("1.10", "1.9"), Greater);
        assert_eq!(compare("1.0", "1.0beta"), Greater);
        assert_eq!(compare("2.0.1", "2.0.1"), Equal);
    }
}
