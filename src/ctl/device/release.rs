// Copyright (C) 2026 Jared Crapo
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Firmware releases from GitHub (docs/web-installer.md, What a release
//! publishes), cached under `~/.cache/ktbctl/firmware/<version>/`.
//!
//! Firmware releases are the `firmware-v*` tags, never marked latest (that
//! stays the daemon's), so the newest is picked from the list here.
//! Unauthenticated, GitHub's API allows 60 requests an hour, and each
//! install asks once. Released files never change, so a version already in
//! the cache is installed without the network. Blocking.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use super::board_name;
use super::flash::Reporter;
use super::manifest::{Firmware, Manifest, check, compare_versions, part_matches};

const RELEASES: &str = "https://api.github.com/repos/kotfu/kiss-tnc-bridge/releases?per_page=100";
const TAG_PREFIX: &str = "firmware-v";
/// Larger than any part: the Core2's app partitions are 4 MB.
const MAX_DOWNLOAD: u64 = 16 << 20;

#[derive(Debug, Clone, Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Debug, Clone, Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

/// A firmware release.
#[derive(Debug, Clone)]
struct Release {
    version: String,
    prerelease: bool,
    assets: Vec<GhAsset>,
}

impl Release {
    fn url(&self, name: &str) -> Option<&str> {
        self.assets
            .iter()
            .find(|a| a.name == name)
            .map(|a| a.browser_download_url.as_str())
    }
}

/// The firmware for `board`: `version`, or the newest release (with
/// prereleases if `prerelease`). Every part is checked against the
/// manifest, from the cache or as it's downloaded.
pub fn fetch(
    board: &str,
    version: Option<&str>,
    prerelease: bool,
    say: &Reporter,
) -> Result<Firmware, String> {
    let cache = cache_dir()?;
    if let Some(v) = version
        && let Ok(fw) = Firmware::load(&cache.join(v).join(manifest_name(board, v)))
    {
        return Ok(fw);
    }
    say.step("releases", "Looking for firmware releases on GitHub...");
    let agent = agent();
    let body = get(&agent, RELEASES)?;
    let list: Vec<GhRelease> = serde_json::from_slice(&body)
        .map_err(|e| format!("GitHub's list of releases didn't parse: {e}"))?;
    let all = releases(list);
    let release = pick(&all, version, prerelease, board)?;
    let dir = cache.join(&release.version);
    std::fs::create_dir_all(&dir).map_err(|e| format!("can't make {}: {e}", dir.display()))?;

    let name = manifest_name(board, &release.version);
    let manifest = dir.join(&name);
    let url = release.url(&name).ok_or("the release has no manifest")?;
    download(&agent, url, &manifest)?;
    let m: Manifest = serde_json::from_slice(
        &std::fs::read(&manifest).map_err(|e| format!("can't read {}: {e}", manifest.display()))?,
    )
    .map_err(|e| format!("firmware {}'s manifest didn't parse: {e}", release.version))?;
    for entry in check(&m)? {
        let path = dir.join(&entry.path);
        if std::fs::read(&path).is_ok_and(|d| part_matches(entry, &d).is_ok()) {
            continue;
        }
        let url = release
            .url(&entry.path)
            .ok_or_else(|| format!("the release has no {}", entry.path))?;
        say.step("downloading", &format!("Downloading {}...", entry.path));
        download(&agent, url, &path)?;
    }
    Firmware::load(&manifest)
}

fn manifest_name(board: &str, version: &str) -> String {
    format!("ktb-{board}-{version}-manifest.json")
}

/// The firmware releases in GitHub's list, newest first.
fn releases(list: Vec<GhRelease>) -> Vec<Release> {
    let mut out: Vec<Release> = list
        .into_iter()
        .filter(|r| !r.draft)
        .filter_map(|r| {
            let version = r.tag_name.strip_prefix(TAG_PREFIX)?.to_string();
            Some(Release {
                prerelease: r.prerelease || version.contains('-'),
                version,
                assets: r.assets,
            })
        })
        .collect();
    out.sort_by(|a, b| compare_versions(&b.version, &a.version).unwrap_or(Ordering::Equal));
    out
}

/// The release to install: `version`, or the newest with firmware for
/// `board` (prereleases only if asked for).
fn pick<'a>(
    releases: &'a [Release],
    version: Option<&str>,
    prerelease: bool,
    board: &str,
) -> Result<&'a Release, String> {
    let has_board = |r: &Release| r.url(&manifest_name(board, &r.version)).is_some();
    if let Some(v) = version {
        let v = v.strip_prefix('v').unwrap_or(v);
        let r = releases
            .iter()
            .find(|r| r.version == v)
            .ok_or_else(|| format!("no firmware release {v} on GitHub"))?;
        if !has_board(r) {
            return Err(format!(
                "firmware {v} has no build for the {}",
                board_name(board)
            ));
        }
        return Ok(r);
    }
    releases
        .iter()
        .filter(|r| prerelease || !r.prerelease)
        .find(|r| has_board(r))
        .ok_or_else(|| {
            format!(
                "no firmware release on GitHub has a build for the {}{}",
                board_name(board),
                if prerelease {
                    ""
                } else {
                    " (--prerelease includes test versions)"
                }
            )
        })
}

/// `$XDG_CACHE_HOME/ktbctl/firmware`, or `~/.cache/ktbctl/firmware`.
fn cache_dir() -> Result<PathBuf, String> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".cache")))
        .ok_or("no HOME to keep downloaded firmware in; use --manifest")?;
    Ok(base.join("ktbctl").join("firmware"))
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(120)))
        .user_agent(format!("ktbctl/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

fn get(agent: &ureq::Agent, url: &str) -> Result<Vec<u8>, String> {
    let mut res = agent.get(url).call().map_err(|e| match e {
        ureq::Error::StatusCode(403 | 429) => "GitHub refused: too many requests from this \
             address for now (60 an hour). Try again later, or use --manifest"
            .to_string(),
        ureq::Error::StatusCode(code) => format!("GitHub answered {code} for {url}"),
        e => format!(
            "can't reach GitHub: {e}. Without the internet, use --manifest with a \
             release's files"
        ),
    })?;
    res.body_mut()
        .with_config()
        .limit(MAX_DOWNLOAD)
        .read_to_vec()
        .map_err(|e| format!("downloading {url}: {e}"))
}

/// Downloads to a temporary name beside `to`, then renames, so a broken
/// download never looks like a finished one.
fn download(agent: &ureq::Agent, url: &str, to: &Path) -> Result<(), String> {
    let data = get(agent, url)?;
    let tmp = to.with_extension("download");
    std::fs::write(&tmp, &data).map_err(|e| format!("can't write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, to).map_err(|e| format!("can't write {}: {e}", to.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gh(tag: &str, prerelease: bool, boards: &[&str]) -> GhRelease {
        let v = tag.strip_prefix(TAG_PREFIX).unwrap_or(tag);
        GhRelease {
            tag_name: tag.into(),
            prerelease,
            draft: false,
            assets: boards
                .iter()
                .map(|b| GhAsset {
                    name: manifest_name(b, v),
                    browser_download_url: format!("https://example.invalid/{b}/{v}"),
                })
                .collect(),
        }
    }

    fn list() -> Vec<Release> {
        releases(vec![
            gh("v1.3.0", false, &[]), // the daemon's
            gh("firmware-v1.1.0", false, &["atom-lite"]),
            gh("firmware-v1.3.0-rc1", true, &["atom-lite", "core2"]),
            gh("firmware-v1.2.0", false, &["atom-lite", "core2"]),
            gh("firmware-v1.10.0-beta1", false, &["atom-lite"]),
        ])
    }

    #[test]
    fn only_firmware_tags_count_newest_first() {
        let v: Vec<(String, bool)> = list()
            .into_iter()
            .map(|r| (r.version, r.prerelease))
            .collect();
        assert_eq!(
            v,
            [
                ("1.10.0-beta1".into(), true), // a hyphen is a prerelease
                ("1.3.0-rc1".into(), true),
                ("1.2.0".into(), false),
                ("1.1.0".into(), false),
            ]
        );
    }

    #[test]
    fn the_newest_release_for_the_board_is_picked() {
        let l = list();
        assert_eq!(pick(&l, None, false, "atom-lite").unwrap().version, "1.2.0");
        assert_eq!(pick(&l, None, false, "core2").unwrap().version, "1.2.0");
        assert_eq!(
            pick(&l, None, true, "atom-lite").unwrap().version,
            "1.10.0-beta1"
        );
        assert_eq!(pick(&l, None, true, "core2").unwrap().version, "1.3.0-rc1");
        assert!(
            pick(&l, None, false, "stickc-plus-se")
                .unwrap_err()
                .contains("StickC Plus SE")
        );
    }

    #[test]
    fn a_version_can_be_asked_for() {
        let l = list();
        assert_eq!(
            pick(&l, Some("1.3.0-rc1"), false, "core2").unwrap().version,
            "1.3.0-rc1"
        );
        assert_eq!(
            pick(&l, Some("v1.1.0"), false, "atom-lite")
                .unwrap()
                .version,
            "1.1.0"
        );
        assert!(
            pick(&l, Some("1.1.0"), false, "core2")
                .unwrap_err()
                .contains("no build for the Core2")
        );
        assert!(pick(&l, Some("9.9.9"), false, "core2").is_err());
    }
}
