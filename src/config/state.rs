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

//! `state.toml`: runtime state that outlives a restart but isn't config,
//! kept in the state directory (`/var/lib/kiss-tnc-bridge`) and owned by
//! the daemon.
//!
//! Today that's public names adjusted after a DNS-SD collision. Enable
//! overrides for static bridges join it with the control API.
//!
//! Writes are atomic (temp file, fsync, rename, fsync the directory), so a
//! power cut on a battery-powered device leaves the old file or the new
//! one, never half of either.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const STATE_FILE_NAME: &str = "state.toml";

const HEADER: &str = "\
# Runtime state for kiss-tnc-bridge. The daemon writes this file; it isn't
# configuration, and hand edits are overwritten. Configuration lives in
# /etc/kiss-tnc-bridge/.

";

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    #[serde(default, rename = "bridge", skip_serializing_if = "BTreeMap::is_empty")]
    pub bridges: BTreeMap<String, BridgeState>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BridgeState {
    /// The public name in use after a DNS-SD collision, e.g. "TH-D75 (2)".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_name: Option<String>,
    /// The configured public name the adjustment was made from. When the
    /// configured name changes, the adjustment no longer applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_name_configured: Option<String>,
}

impl BridgeState {
    fn is_empty(&self) -> bool {
        self == &BridgeState::default()
    }
}

impl State {
    /// The adjusted public name for a bridge, if one was made from the name
    /// configured now.
    pub fn adjusted_public_name(&self, bridge: &str, configured: &str) -> Option<&str> {
        let b = self.bridges.get(bridge)?;
        match (&b.public_name, &b.public_name_configured) {
            (Some(name), Some(from)) if from == configured => Some(name),
            _ => None,
        }
    }

    pub fn set_adjusted_public_name(&mut self, bridge: &str, configured: &str, name: &str) {
        let b = self.bridges.entry(bridge.to_string()).or_default();
        b.public_name = Some(name.to_string());
        b.public_name_configured = Some(configured.to_string());
    }

    pub fn clear_adjusted_public_name(&mut self, bridge: &str) {
        if let Some(b) = self.bridges.get_mut(bridge) {
            b.public_name = None;
            b.public_name_configured = None;
        }
        self.bridges.retain(|_, b| !b.is_empty());
    }
}

/// `state.toml` in a state directory, read once and written on change.
#[derive(Debug)]
pub struct StateStore {
    path: PathBuf,
    state: State,
}

impl StateStore {
    /// Read the state in `dir`. A missing file is an empty state. A file
    /// that can't be read or parsed is reported and treated as empty; it's
    /// replaced at the next write, which is better than refusing to start
    /// over a runtime detail.
    pub fn open(dir: &Path) -> (StateStore, Option<String>) {
        let path = dir.join(STATE_FILE_NAME);
        let (state, problem) = match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<State>(&text) {
                Ok(s) => (s, None),
                Err(e) => (
                    State::default(),
                    Some(format!(
                        "{} is unreadable and will be replaced: {}",
                        path.display(),
                        e.to_string().trim_end()
                    )),
                ),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (State::default(), None),
            Err(e) => (
                State::default(),
                Some(format!("can't read {}: {e}", path.display())),
            ),
        };
        (StateStore { path, state }, problem)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    /// Change the state and write it if anything changed.
    pub fn update(&mut self, f: impl FnOnce(&mut State)) -> std::io::Result<()> {
        let mut next = self.state.clone();
        f(&mut next);
        if next == self.state {
            return Ok(());
        }
        let body = toml::to_string(&next).map_err(std::io::Error::other)?;
        write_atomic(&self.path, format!("{HEADER}{body}").as_bytes())?;
        self.state = next;
        Ok(())
    }
}

/// Replace `path` with `bytes` so readers see the old file or the new one,
/// even across a power cut.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(".tmp");
    let tmp = dir.join(tmp_name);
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    // The rename itself is durable once the directory is synced.
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_empty_and_writes_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let (mut store, problem) = StateStore::open(dir.path());
        assert!(problem.is_none());
        assert_eq!(store.state(), &State::default());
        // Nothing to write: no file appears.
        store.update(|_| {}).unwrap();
        assert!(!store.path().exists());

        store
            .update(|s| s.set_adjusted_public_name("ktbr0", "TH-D75", "TH-D75 (2)"))
            .unwrap();
        let text = std::fs::read_to_string(store.path()).unwrap();
        assert!(text.starts_with("# Runtime state"), "{text}");
        assert!(text.contains("[bridge.ktbr0]"), "{text}");
        assert!(!dir.path().join("state.toml.tmp").exists());

        let (again, problem) = StateStore::open(dir.path());
        assert!(problem.is_none());
        assert_eq!(again.state(), store.state());
        assert_eq!(
            again.state().adjusted_public_name("ktbr0", "TH-D75"),
            Some("TH-D75 (2)")
        );
        // Made from another configured name: doesn't apply.
        assert_eq!(again.state().adjusted_public_name("ktbr0", "Other"), None);
        assert_eq!(again.state().adjusted_public_name("ktbr1", "TH-D75"), None);
    }

    #[test]
    fn clearing_removes_empty_bridges() {
        let mut s = State::default();
        s.set_adjusted_public_name("ktbr0", "A", "A (2)");
        s.clear_adjusted_public_name("ktbr0");
        assert!(s.bridges.is_empty());
        assert_eq!(toml::to_string(&s).unwrap(), "");
    }

    #[test]
    fn a_broken_file_is_reported_and_replaced() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(STATE_FILE_NAME), "[bridge.ktbr0\n").unwrap();
        let (mut store, problem) = StateStore::open(dir.path());
        assert!(problem.unwrap().contains("will be replaced"));
        store
            .update(|s| s.set_adjusted_public_name("ktbr0", "A", "A (2)"))
            .unwrap();
        let (again, problem) = StateStore::open(dir.path());
        assert!(problem.is_none());
        assert_eq!(
            again.state().adjusted_public_name("ktbr0", "A"),
            Some("A (2)")
        );
    }
}
