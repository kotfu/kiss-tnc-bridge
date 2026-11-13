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

//! A firmware release's manifest (docs/web-installer.md, The manifest),
//! with its parts read and checked against it before anything is written.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

/// `ktb-<board>-<version>-manifest.json`, in ESP Web Tools' format with
/// fields of our own.
#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub version: String,
    pub board: String,
    #[serde(default)]
    pub built: Option<String>,
    pub builds: Vec<Build>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Build {
    #[serde(rename = "chipFamily")]
    pub chip_family: String,
    pub parts: Vec<PartEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PartEntry {
    /// Relative to the manifest.
    pub path: String,
    pub offset: u32,
    /// Hex.
    pub sha256: String,
    pub size: u64,
}

/// A part, read and checked.
#[derive(Debug, Clone)]
pub struct Part {
    pub name: String,
    pub offset: u32,
    pub data: Vec<u8>,
}

/// A manifest and its parts, every one checked against it.
#[derive(Debug, Clone)]
pub struct Firmware {
    pub version: String,
    pub board: String,
    pub built: Option<String>,
    pub parts: Vec<Part>,
    /// Where it came from, for people: the manifest's path.
    pub source: PathBuf,
}

impl Firmware {
    /// Reads a manifest and the parts beside it, and checks each part's
    /// size and SHA-256. Nothing that fails is written.
    pub fn load(manifest: &Path) -> Result<Firmware, String> {
        let text = std::fs::read(manifest)
            .map_err(|e| format!("can't read {}: {e}", manifest.display()))?;
        let m: Manifest = serde_json::from_slice(&text)
            .map_err(|e| format!("{} isn't a firmware manifest: {e}", manifest.display()))?;
        let dir = manifest.parent().unwrap_or(Path::new("."));
        let mut parts = Vec::new();
        for entry in check(&m)? {
            let path = dir.join(&entry.path);
            let data =
                std::fs::read(&path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
            part_matches(entry, &data)?;
            parts.push(Part {
                name: entry.path.clone(),
                offset: entry.offset,
                data,
            });
        }
        Ok(Firmware {
            version: m.version,
            board: m.board,
            built: m.built,
            parts,
            source: manifest.to_path_buf(),
        })
    }

    /// The bytes to write, in all.
    pub fn size(&self) -> usize {
        self.parts.iter().map(|p| p.data.len()).sum()
    }
}

/// The manifest's parts, if it makes sense: one ESP32 build, parts named
/// by a plain file name beside the manifest (a downloaded manifest can't
/// point anywhere else), in flash order without overlapping.
pub fn check(m: &Manifest) -> Result<&[PartEntry], String> {
    let [build] = m.builds.as_slice() else {
        return Err(format!(
            "the manifest has {} builds; firmware for these boards has one",
            m.builds.len()
        ));
    };
    if build.chip_family != "ESP32" {
        return Err(format!(
            "the firmware is for an {}, not an ESP32",
            build.chip_family
        ));
    }
    if build.parts.is_empty() {
        return Err("the manifest has no parts".into());
    }
    let mut end = 0u64;
    for p in &build.parts {
        if !plain_name(&p.path) {
            return Err(format!(
                "the manifest's part {:?} isn't a file beside it",
                p.path
            ));
        }
        if u64::from(p.offset) < end {
            return Err(format!(
                "the manifest's part {} overlaps the one before it",
                p.path
            ));
        }
        end = u64::from(p.offset) + p.size;
    }
    Ok(&build.parts)
}

fn plain_name(path: &str) -> bool {
    !path.is_empty() && path != "." && path != ".." && !path.contains('/') && !path.contains('\\')
}

/// Whether `data` is the part the manifest describes.
pub fn part_matches(entry: &PartEntry, data: &[u8]) -> Result<(), String> {
    if data.len() as u64 != entry.size {
        return Err(format!(
            "{} is {} bytes; the manifest says {}. The download is damaged or \
             incomplete; nothing was written",
            entry.path,
            data.len(),
            entry.size
        ));
    }
    if !sha256_hex(data).eq_ignore_ascii_case(&entry.sha256) {
        return Err(format!(
            "{} doesn't match the manifest's SHA-256. The download is damaged; \
             nothing was written",
            entry.path
        ));
    }
    Ok(())
}

pub fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Orders firmware versions, semver style: `1.10.0` after `1.9.0`, and a
/// prerelease (`1.2.0-rc1`) before its release. `None` for a version that
/// doesn't parse.
pub fn compare_versions(a: &str, b: &str) -> Option<Ordering> {
    fn parse(v: &str) -> Option<([u64; 3], Option<&str>)> {
        let (core, pre) = match v.split_once('-') {
            Some((c, p)) => (c, Some(p)),
            None => (v, None),
        };
        let mut nums = core.split('.').map(|n| n.parse::<u64>().ok());
        let parsed = [nums.next()??, nums.next()??, nums.next()??];
        nums.next().is_none().then_some((parsed, pre))
    }
    let (an, ap) = parse(a)?;
    let (bn, bp) = parse(b)?;
    Some(an.cmp(&bn).then_with(|| match (ap, bp) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(x), Some(y)) => compare_prerelease(x, y),
    }))
}

/// `beta2` before `beta10`, and `beta` before `rc`.
fn compare_prerelease(a: &str, b: &str) -> Ordering {
    let split = |s: &str| {
        let i = s.find(|c: char| c.is_ascii_digit()).unwrap_or(s.len());
        let (word, num) = s.split_at(i);
        (word.to_string(), num.parse::<u64>().ok())
    };
    split(a).cmp(&split(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_release(dir: &Path, app: &[u8], app_sha: Option<&str>) -> PathBuf {
        std::fs::write(dir.join("ktb-core2-v1.3-1.3.0-bootloader.bin"), b"boot").unwrap();
        std::fs::write(dir.join("ktb-core2-v1.3-1.3.0-app.bin"), app).unwrap();
        let manifest = serde_json::json!({
        "name": "kiss-tnc-bridge", "version": "1.3.0", "board": "core2-v1.3",
        "built": "2026-11-20T02:00:00Z",
        "builds": [{"chipFamily": "ESP32", "parts": [
            {"path": "ktb-core2-v1.3-1.3.0-bootloader.bin", "offset": 4096,
             "sha256": sha256_hex(b"boot"), "size": 4},
            {"path": "ktb-core2-v1.3-1.3.0-app.bin", "offset": 65536,
             "sha256": app_sha.map_or_else(|| sha256_hex(app), str::to_string),
             "size": app.len()},
        ]}]});
        let path = dir.join("ktb-core2-v1.3-1.3.0-manifest.json");
        std::fs::write(&path, manifest.to_string()).unwrap();
        path
    }

    #[test]
    fn a_release_loads_with_its_parts_checked() {
        let t = tempfile::tempdir().unwrap();
        let fw = Firmware::load(&write_release(t.path(), b"app image", None)).unwrap();
        assert_eq!(fw.board, "core2-v1.3");
        assert_eq!(fw.version, "1.3.0");
        assert_eq!(fw.parts.len(), 2);
        assert_eq!(fw.parts[1].offset, 65536);
        assert_eq!(fw.parts[1].data, b"app image");
        assert_eq!(fw.size(), 13);
    }

    #[test]
    fn a_damaged_part_is_refused() {
        let t = tempfile::tempdir().unwrap();
        let wrong = sha256_hex(b"something else");
        let e = Firmware::load(&write_release(t.path(), b"app image", Some(&wrong))).unwrap_err();
        assert!(e.contains("doesn't match the manifest's SHA-256"), "{e}");

        let t = tempfile::tempdir().unwrap();
        let m = write_release(t.path(), b"app image", None);
        std::fs::write(t.path().join("ktb-core2-v1.3-1.3.0-app.bin"), b"app ima").unwrap();
        let e = Firmware::load(&m).unwrap_err();
        assert!(e.contains("is 7 bytes; the manifest says 9"), "{e}");
    }

    fn manifest(parts: serde_json::Value, family: &str) -> Manifest {
        serde_json::from_value(serde_json::json!({
            "version": "1.3.0", "board": "atom-lite",
            "builds": [{"chipFamily": family, "parts": parts}]}))
        .unwrap()
    }

    #[test]
    fn manifests_that_dont_make_sense_are_refused() {
        let part = |path: &str, offset: u32, size: u64| serde_json::json!({"path": path, "offset": offset, "sha256": "", "size": size});
        let ok = manifest(serde_json::json!([part("a.bin", 4096, 100)]), "ESP32");
        assert!(check(&ok).is_ok());
        let s3 = manifest(serde_json::json!([part("a.bin", 4096, 100)]), "ESP32-S3");
        assert!(check(&s3).unwrap_err().contains("ESP32-S3"));
        for bad in ["../a.bin", "/etc/passwd", "sub/a.bin", "..", ""] {
            let m = manifest(serde_json::json!([part(bad, 4096, 100)]), "ESP32");
            assert!(
                check(&m).unwrap_err().contains("isn't a file beside it"),
                "{bad}"
            );
        }
        let overlap = manifest(
            serde_json::json!([part("a.bin", 4096, 100), part("b.bin", 4100, 10)]),
            "ESP32",
        );
        assert!(check(&overlap).unwrap_err().contains("overlaps"));
        let none = manifest(serde_json::json!([]), "ESP32");
        assert!(check(&none).is_err());
    }

    #[test]
    fn versions_order_like_semver() {
        use Ordering::*;
        let c = |a, b| compare_versions(a, b);
        assert_eq!(c("1.2.0", "1.10.0"), Some(Less));
        assert_eq!(c("1.3.0", "1.3.0"), Some(Equal));
        assert_eq!(c("1.3.0-rc1", "1.3.0"), Some(Less));
        assert_eq!(c("1.3.0-beta2", "1.3.0-beta10"), Some(Less));
        assert_eq!(c("1.3.0-beta1", "1.3.0-rc1"), Some(Less));
        assert_eq!(c("1.2.0-dev", "1.2.0"), Some(Less));
        assert_eq!(c("2.0.0", "1.9.9"), Some(Greater));
        assert_eq!(c("1.2", "1.2.0"), None);
        assert_eq!(c("one", "1.2.0"), None);
    }
}
