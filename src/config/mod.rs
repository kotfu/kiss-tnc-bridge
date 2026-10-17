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

//! Configuration.
//!
//! Static config is TOML in `/etc/kiss-tnc-bridge/`: the main file plus
//! `conf.d/*.toml`, all read-only to the daemon. This module turns those
//! files into one effective set of adapters, TNCs and bridges with defaults filled
//! in and references resolved ([`resolve`]), and checks it ([`validate`]).
//! The same path serves `kiss-tnc-bridge -t`, startup, and later the
//! control API's `config.check` and reload.
//!
//! Stages:
//!
//! 1. [`load`]: read the files into [`model::ConfigFile`]s, one per file,
//!    remembering where each came from.
//! 2. [`resolve`]: build the effective [`Config`]. Value errors (a bad MAC,
//!    an unknown adapter id, a listen spec that doesn't parse) stop here.
//! 3. [`validate`]: structural rules and resource conflicts among enabled
//!    bridges, plus warnings, including best-effort host checks.

pub mod load;
pub mod model;
pub mod resolve;
pub mod state;
pub mod store;
pub mod validate;

use std::fmt;
use std::path::PathBuf;

pub use load::{Loaded, load};
pub use model::{AppParams, ConnectPolicy, Role};
pub use resolve::{
    Adapter, AdapterSel, AdapterValue, Bridge, Config, Control, Global, Listen, Mac, Port,
    PortKind, SerialDevice, Spp, Tnc, TncModel, resolve,
};
pub use validate::{Report, validate};

/// Where an object was defined.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Source {
    Static(PathBuf),
    Dynamic,
}

impl Source {
    pub fn is_static(&self) -> bool {
        matches!(self, Source::Static(_))
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Static(p) => write!(f, "static: {}", p.display()),
            Source::Dynamic => write!(f, "dynamic"),
        }
    }
}

/// One error or warning, tied to the object and file it concerns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub source: Option<Source>,
    /// What the problem is about, e.g. `bridge ktbr0 port lan`.
    pub object: String,
    pub message: String,
}

impl Problem {
    pub fn new(
        source: Option<Source>,
        object: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            source,
            object: object.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(Source::Static(p)) = &self.source {
            write!(f, "{}: ", p.display())?;
        }
        if !self.object.is_empty() {
            write!(f, "{}: ", self.object)?;
        }
        write!(f, "{}", self.message)
    }
}

/// Everything the effective config is built from, as read from disk. Kept
/// by the daemon so a change made through the API can rebuild from the
/// static files as last read plus a new `dynamic.toml`, touching nothing
/// else.
#[derive(Debug, Clone)]
pub struct Sources {
    /// The main file, then `conf.d` in name order.
    pub statics: Vec<(Source, model::ConfigFile)>,
    pub dynamic: Option<load::DynamicFile>,
    /// `state.toml`, for enable overrides.
    pub state: state::State,
    pub legacy_conf_present: bool,
    /// Files that couldn't be read and were left out (`-t` without root).
    pub notes: Vec<Problem>,
}

/// Read the static files, and `dynamic.toml` and `state.toml` from
/// `state_dir` if given. A static file that can't be read or parsed is an
/// error; dynamic and state files that can't be read are left out with a
/// note, since `-t` often runs without the daemon's permissions.
pub fn read_sources(
    main_file: &std::path::Path,
    state_dir: Option<&std::path::Path>,
) -> Result<Sources, Vec<Problem>> {
    let loaded = load(main_file).map_err(|p| vec![p])?;
    let mut notes = Vec::new();
    let mut dynamic = None;
    let mut st = state::State::default();
    if let Some(dir) = state_dir {
        match load::load_dynamic(dir) {
            Ok(d) => dynamic = d,
            Err(e) => notes.push(Problem::new(
                None,
                "",
                format!(
                    "can't read {}: {e}; checked the static config only",
                    dir.join(load::DYNAMIC_FILE_NAME).display()
                ),
            )),
        }
        let path = dir.join(state::STATE_FILE_NAME);
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<state::State>(&text) {
                Ok(s) => st = s,
                Err(e) => notes.push(Problem::new(
                    None,
                    "",
                    format!(
                        "{} is unreadable, so enable overrides are ignored: {}",
                        path.display(),
                        e.to_string().trim_end()
                    ),
                )),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => notes.push(Problem::new(
                None,
                "",
                format!(
                    "can't read {}: {e}; enable overrides not applied",
                    path.display()
                ),
            )),
        }
    }
    Ok(Sources {
        statics: loaded.files,
        dynamic,
        state: st,
        legacy_conf_present: loaded.legacy_conf_present,
        notes,
    })
}

/// Build and validate the effective config from `sources`. The config is
/// returned only when it's usable; otherwise every error is.
pub fn build(sources: &Sources, host_checks: bool) -> Result<(Config, Report), Vec<Problem>> {
    let mut files = sources.statics.clone();
    let mut set_aside = Vec::new();
    if let Some(d) = &sources.dynamic {
        let parsed = load::parse_dynamic(&d.path, &d.text).map_err(|p| vec![p])?;
        let (kept, warnings) = set_aside_collisions(&sources.statics, parsed);
        set_aside = warnings;
        files.push((Source::Dynamic, kept));
    }
    let mut cfg = resolve(&files)?;
    apply_overrides(&mut cfg, &sources.state);
    let mut report = validate(&cfg, host_checks);
    report.warnings.extend(set_aside);
    report.warnings.extend(sources.notes.iter().cloned());
    if sources.legacy_conf_present {
        report.warnings.push(Problem::new(
            None,
            "",
            "/etc/kiss-tnc-bridge.conf (the 0.5 config) exists and is ignored; \
             1.0 reads /etc/kiss-tnc-bridge/kiss-tnc-bridge.toml instead, see the README",
        ));
    }
    if report.errors.is_empty() {
        Ok((cfg, report))
    } else {
        Err(report.errors)
    }
}

/// Read and build in one step: startup and `-t`.
pub fn load_all(
    main_file: &std::path::Path,
    state_dir: Option<&std::path::Path>,
    host_checks: bool,
) -> Result<(Config, Report), Vec<Problem>> {
    build(&read_sources(main_file, state_dir)?, host_checks)
}

/// Dynamic objects whose id, or for bridges whose name, a static object
/// already uses are left out with a warning: static config wins, and the
/// dynamic file isn't changed. References to a left-out TNC or adapter then
/// resolve to the static one.
fn set_aside_collisions(
    statics: &[(Source, model::ConfigFile)],
    mut dynamic: model::ConfigFile,
) -> (model::ConfigFile, Vec<Problem>) {
    let mut warnings = Vec::new();
    let all = || statics.iter().map(|(_, f)| f);
    let adapter_ids: Vec<&str> = all()
        .flat_map(|f| f.adapter.iter().map(|a| a.id.as_str()))
        .collect();
    let tnc_ids: Vec<&str> = all()
        .flat_map(|f| f.tnc.iter().map(|t| t.id.as_str()))
        .collect();
    let bridge_ids: Vec<&str> = all()
        .flat_map(|f| f.bridge.iter().filter_map(|b| b.id.as_deref()))
        .collect();
    let bridge_names: Vec<&str> = all()
        .flat_map(|f| f.bridge.iter().map(|b| b.name.as_str()))
        .collect();
    let mut warn = |object: String, why: &str| {
        warnings.push(Problem::new(
            Some(Source::Dynamic),
            object,
            format!("{why}; the static one wins and this one is ignored"),
        ));
    };
    dynamic.adapter.retain(|a| {
        let clash = adapter_ids.contains(&a.id.as_str());
        if clash {
            warn(
                format!("adapter {}", a.id),
                "a static adapter has the same id",
            );
        }
        !clash
    });
    dynamic.tnc.retain(|t| {
        let clash = tnc_ids.contains(&t.id.as_str());
        if clash {
            warn(format!("tnc {}", t.id), "a static tnc has the same id");
        }
        !clash
    });
    dynamic.bridge.retain(|b| {
        let id = b.id.as_deref().unwrap_or("");
        let object = format!("bridge {}", if id.is_empty() { &b.name } else { id });
        if bridge_ids.contains(&id) {
            warn(object, "a static bridge has the same id");
            false
        } else if bridge_names.contains(&b.name.as_str()) {
            warn(
                object,
                &format!("a static bridge is also named {:?}", b.name),
            );
            false
        } else {
            true
        }
    });
    (dynamic, warnings)
}

/// Enable overrides from `state.toml` apply to static bridges; a dynamic
/// bridge's `enabled` is part of its definition.
fn apply_overrides(cfg: &mut Config, st: &state::State) {
    for b in cfg.bridges.iter_mut().filter(|b| b.source.is_static()) {
        if let Some(enabled) = st.enabled_override(&b.id) {
            b.enabled_override = Some(enabled);
            b.enabled = enabled;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const MAIN: &str = r#"
[[adapter]]
id = "builtin"
mac = "B8:27:EB:12:34:56"

[[tnc]]
id = "d75"
model = "th-d75"
spp = { address = "00:11:22:33:44:55", adapter = "builtin" }

[[bridge]]
id = "ktbr0"
name = "Field"
  [[bridge.port]]
  type = "spp-client"
  tnc = "d75"
  [[bridge.port]]
  type = "ble-server"
"#;

    fn setup(main: &str, dynamic: Option<&str>, state: Option<&str>) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(load::MAIN_FILE_NAME), main).unwrap();
        let st = dir.path().join("state");
        std::fs::create_dir(&st).unwrap();
        if let Some(d) = dynamic {
            std::fs::write(st.join(load::DYNAMIC_FILE_NAME), d).unwrap();
        }
        if let Some(s) = state {
            std::fs::write(st.join(state::STATE_FILE_NAME), s).unwrap();
        }
        dir
    }

    fn run(dir: &Path) -> Result<(Config, Report), Vec<String>> {
        load_all(
            &dir.join(load::MAIN_FILE_NAME),
            Some(&dir.join("state")),
            false,
        )
        .map_err(|e| e.iter().map(|p| p.to_string()).collect())
    }

    #[test]
    fn dynamic_objects_join_and_may_use_static_ones() {
        let dir = setup(
            MAIN,
            Some(
                r#"
[[bridge]]
id = "ktbr1"
name = "Desk"
enabled = false
  [[bridge.port]]
  type = "spp-client"
  tnc = "d75"
  [[bridge.port]]
  type = "ble-server"
  adapter = "builtin"
"#,
            ),
            None,
        );
        let (cfg, report) = run(dir.path()).unwrap();
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        let b = cfg.bridges.iter().find(|b| b.id == "ktbr1").unwrap();
        assert_eq!(b.source, Source::Dynamic);
        assert!(!b.enabled);
    }

    #[test]
    fn static_config_cant_refer_to_dynamic_objects() {
        let main = MAIN.replace("tnc = \"d75\"", "tnc = \"usb\"");
        let dir = setup(
            &main,
            Some("[[tnc]]\nid = \"usb\"\nserial = { path = \"/dev/serial/by-id/x\" }\n"),
            None,
        );
        let e = run(dir.path()).unwrap_err();
        assert!(
            e.iter()
                .any(|m| m.contains("tnc \"usb\" is defined in dynamic config")),
            "{e:?}"
        );

        let main = MAIN.replace(
            "type = \"ble-server\"",
            "type = \"ble-server\"\n  adapter = \"dongle\"",
        );
        let dir = setup(
            &main,
            Some("[[adapter]]\nid = \"dongle\"\nmac = \"00:1A:7D:DA:71:13\"\n"),
            None,
        );
        let e = run(dir.path()).unwrap_err();
        assert!(
            e.iter()
                .any(|m| m.contains("adapter \"dongle\" is defined in dynamic config")),
            "{e:?}"
        );
    }

    #[test]
    fn static_wins_collisions_and_the_dynamic_one_is_set_aside() {
        let dir = setup(
            MAIN,
            Some(
                r#"
[[adapter]]
id = "builtin"
mac = "00:1A:7D:DA:71:13"

[[tnc]]
id = "d75"
serial = { path = "/dev/serial/by-id/other" }

[[bridge]]
id = "ktbr0"
name = "Mine"
  [[bridge.port]]
  type = "tcp-client"
  address = "127.0.0.1:6700"
  [[bridge.port]]
  type = "tcp-server"
  listen = "8001"

[[bridge]]
id = "ktbr7"
name = "Field"
  [[bridge.port]]
  type = "tcp-client"
  address = "127.0.0.1:6700"
  [[bridge.port]]
  type = "tcp-server"
  listen = "8002"
"#,
            ),
            None,
        );
        let (cfg, report) = run(dir.path()).unwrap();
        let w: Vec<String> = report.warnings.iter().map(|p| p.to_string()).collect();
        assert_eq!(w.len(), 4, "{w:?}");
        assert!(w.iter().all(|m| m.contains("the static one wins")), "{w:?}");
        assert!(
            w.iter().any(|m| m.contains("also named \"Field\"")),
            "{w:?}"
        );
        assert_eq!(cfg.bridges.len(), 1);
        assert_eq!(cfg.adapters[0].mac.to_string(), "B8:27:EB:12:34:56");
        assert!(cfg.tnc("d75").unwrap().spp.is_some());
    }

    #[test]
    fn overrides_apply_to_static_bridges_only() {
        let state = "[bridge.ktbr0]\nenabled = false\n[bridge.ktbr1]\nenabled = true\n";
        let dir = setup(
            MAIN,
            Some(
                "[[bridge]]\nid = \"ktbr1\"\nname = \"Desk\"\nenabled = false\n[[bridge.port]]\ntype = \"tcp-client\"\naddress = \"a:1\"\n[[bridge.port]]\ntype = \"tcp-server\"\nlisten = \"8001\"\n",
            ),
            Some(state),
        );
        let (cfg, _) = run(dir.path()).unwrap();
        let k0 = cfg.bridges.iter().find(|b| b.id == "ktbr0").unwrap();
        assert!(!k0.enabled);
        assert_eq!(k0.enabled_override, Some(false));
        let k1 = cfg.bridges.iter().find(|b| b.id == "ktbr1").unwrap();
        assert!(!k1.enabled);
        assert_eq!(k1.enabled_override, None);
    }

    #[test]
    fn dynamic_files_hold_objects_only() {
        let dir = setup(MAIN, Some("[global]\nlog_level = \"debug\"\n"), None);
        let e = run(dir.path()).unwrap_err();
        assert!(
            e.iter().any(|m| m.contains("belong in the main file")),
            "{e:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_dynamic_config_is_a_note() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            return; // root reads it anyway
        }
        let dir = setup(MAIN, Some("[[tnc]]\nid = \"x\"\n"), None);
        let path = dir.path().join("state").join(load::DYNAMIC_FILE_NAME);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let (_, report) = run(dir.path()).unwrap();
        assert!(
            report
                .warnings
                .iter()
                .any(|p| p.message.contains("checked the static config only")),
            "{:?}",
            report.warnings
        );
    }
}
