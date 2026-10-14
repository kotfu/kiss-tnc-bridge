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

//! Reading the config files: the static main file and `conf.d/*.toml` next
//! to it, in name order, and the daemon's own `dynamic.toml` in the state
//! directory.

use std::path::{Path, PathBuf};

use super::model::ConfigFile;
use super::{Problem, Source};

pub const MAIN_FILE_NAME: &str = "kiss-tnc-bridge.toml";
pub const CONF_D: &str = "conf.d";
/// Dynamic config, in the state directory, written by the daemon.
pub const DYNAMIC_FILE_NAME: &str = "dynamic.toml";
/// Where 0.5 kept its INI config. It's never read; its presence is a warning.
pub const LEGACY_CONF: &str = "/etc/kiss-tnc-bridge.conf";

#[derive(Debug)]
pub struct Loaded {
    pub main_file: PathBuf,
    pub conf_d: PathBuf,
    /// The main file first, then `conf.d` files in name order.
    pub files: Vec<(Source, ConfigFile)>,
    pub legacy_conf_present: bool,
}

/// Read the main file and its `conf.d`. The first unreadable or unparseable
/// file is the error: there's no point validating a set that's missing a
/// file.
pub fn load(main_file: &Path) -> Result<Loaded, Problem> {
    let conf_d = main_file
        .parent()
        .map(|d| d.join(CONF_D))
        .unwrap_or_else(|| PathBuf::from(CONF_D));

    let mut files = Vec::new();
    files.push((
        Source::Static(main_file.to_path_buf()),
        parse_file(main_file)?,
    ));

    if conf_d.is_dir() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&conf_d)
            .map_err(|e| {
                Problem::new(
                    None,
                    conf_d.display().to_string(),
                    format!("can't read directory: {e}"),
                )
            })?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.extension().is_some_and(|x| x == "toml")
                    && p.file_name()
                        .is_some_and(|n| !n.to_string_lossy().starts_with('.'))
                    && p.is_file()
            })
            .collect();
        entries.sort();
        for path in entries {
            let file = parse_file(&path)?;
            if file.global.is_some() || file.control.is_some() {
                return Err(Problem::new(
                    Some(Source::Static(path.clone())),
                    "",
                    format!(
                        "[global] and [control] belong in the main file ({MAIN_FILE_NAME}), not in conf.d"
                    ),
                ));
            }
            files.push((Source::Static(path), file));
        }
    }

    Ok(Loaded {
        main_file: main_file.to_path_buf(),
        conf_d,
        files,
        legacy_conf_present: Path::new(LEGACY_CONF).exists(),
    })
}

/// `dynamic.toml` as read: its text too, which the store hashes to notice
/// hand edits.
#[derive(Debug, Clone)]
pub struct DynamicFile {
    pub path: PathBuf,
    pub text: String,
}

/// Read `dynamic.toml` from the state directory. `Ok(None)` if there isn't
/// one yet; `Err` with the I/O error if it can't be read, so the caller can
/// decide (the daemon can always read it; `-t` run without root can't).
pub fn load_dynamic(state_dir: &Path) -> Result<Option<DynamicFile>, std::io::Error> {
    let path = state_dir.join(DYNAMIC_FILE_NAME);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    Ok(Some(DynamicFile { path, text }))
}

/// Parse a `dynamic.toml` read by [`load_dynamic`] (or a candidate from the
/// API). It holds adapters, TNCs and bridges, never `[global]` or
/// `[control]`.
pub fn parse_dynamic(path: &Path, text: &str) -> Result<ConfigFile, Problem> {
    let file = parse_str(text).map_err(|msg| Problem::new(Some(Source::Dynamic), "", msg))?;
    if file.global.is_some() || file.control.is_some() {
        return Err(Problem::new(
            Some(Source::Dynamic),
            "",
            format!(
                "{}: [global] and [control] belong in the main file ({MAIN_FILE_NAME})",
                path.display()
            ),
        ));
    }
    Ok(file)
}

fn parse_file(path: &Path) -> Result<ConfigFile, Problem> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        Problem::new(
            Some(Source::Static(path.to_path_buf())),
            "",
            format!("can't read: {e}"),
        )
    })?;
    parse_str(&text).map_err(|msg| Problem::new(Some(Source::Static(path.to_path_buf())), "", msg))
}

/// Parse one file's text. The error message carries toml's line/column and
/// snippet, and names unknown keys and port types.
pub fn parse_str(text: &str) -> Result<ConfigFile, String> {
    toml::from_str::<ConfigFile>(text).map_err(|e| e.to_string().trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(dir: &Path, rel: &str, text: &str) -> PathBuf {
        let p = dir.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, text).unwrap();
        p
    }

    #[test]
    fn main_file_then_conf_d_in_name_order() {
        let dir = tempfile::tempdir().unwrap();
        let main = write(
            dir.path(),
            MAIN_FILE_NAME,
            "[global]\nlog_level = \"debug\"\n",
        );
        write(
            dir.path(),
            "conf.d/20-second.toml",
            "[[bridge]]\nid = \"ktbr2\"\nname = \"two\"\n",
        );
        write(
            dir.path(),
            "conf.d/10-first.toml",
            "[[bridge]]\nid = \"ktbr1\"\nname = \"one\"\n",
        );
        write(dir.path(), "conf.d/README", "not toml");
        write(
            dir.path(),
            "conf.d/.hidden.toml",
            "[[bridge]]\nid = \"nope\"\nname = \"x\"\n",
        );
        let loaded = load(&main).unwrap();
        let ids: Vec<String> = loaded
            .files
            .iter()
            .flat_map(|(_, f)| f.bridge.iter().map(|b| b.id.clone().unwrap()))
            .collect();
        assert_eq!(ids, vec!["ktbr1", "ktbr2"]);
        assert_eq!(loaded.files.len(), 3);
        assert_eq!(
            loaded.files[1].0,
            Source::Static(dir.path().join("conf.d/10-first.toml"))
        );
    }

    #[test]
    fn missing_main_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = load(&dir.path().join(MAIN_FILE_NAME)).unwrap_err();
        assert!(err.message.contains("can't read"), "{err}");
    }

    #[test]
    fn global_in_conf_d_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let main = write(dir.path(), MAIN_FILE_NAME, "");
        write(
            dir.path(),
            "conf.d/x.toml",
            "[global]\nlog_level = \"debug\"\n",
        );
        let err = load(&main).unwrap_err();
        assert!(err.message.contains("belong in the main file"), "{err}");
    }

    #[test]
    fn syntax_and_unknown_keys_are_errors() {
        let err =
            parse_str("[[bridge]]\nid = \"ktbr0\"\nname = \"x\"\ncolour = \"red\"\n").unwrap_err();
        assert!(err.contains("colour"), "{err}");
        let err = parse_str("[[bridge]\n").unwrap_err();
        assert!(err.contains("line 1"), "{err}");
        let err = parse_str(
            "[[bridge]]\nid = \"ktbr0\"\nname = \"x\"\n[[bridge.port]]\ntype = \"usb\"\n",
        )
        .unwrap_err();
        assert!(err.contains("usb"), "{err}");
        let err = parse_str("[[bridge]]\nid = \"ktbr0\"\nname = \"x\"\n[[bridge.port]]\ntype = \"tcp-server\"\nlisten = \"8001\"\nhost = \"x\"\n")
            .unwrap_err();
        assert!(err.contains("host"), "{err}");
    }
}
