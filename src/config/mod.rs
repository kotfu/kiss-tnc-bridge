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
//! files into one effective set of adapters and bridges with defaults filled
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
pub mod validate;

use std::fmt;
use std::path::PathBuf;

pub use load::{Loaded, load};
pub use model::{AppParams, ConnectPolicy, Role};
pub use resolve::{
    Adapter, AdapterSel, AdapterValue, Bridge, Config, Control, Global, Listen, Mac, Port,
    PortKind, resolve,
};
pub use validate::{Report, validate};

/// Where an object was defined.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Source {
    Static(PathBuf),
    Dynamic,
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

/// Load, resolve and validate the config under `main_file`'s directory.
/// Errors from every stage come back as problems; the config is returned
/// only when it's usable.
pub fn load_all(
    main_file: &std::path::Path,
    host_checks: bool,
) -> Result<(Config, Report), Vec<Problem>> {
    let loaded = load(main_file).map_err(|p| vec![p])?;
    let cfg = resolve(&loaded.files)?;
    let mut report = validate(&cfg, host_checks);
    if loaded.legacy_conf_present {
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
