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

//! `dynamic.toml`: the adapters, TNCs and bridges created through the
//! control API, in the state directory and owned by the daemon.
//!
//! Edits go through `toml_edit`, so comments and formatting in the file
//! survive. Writes are atomic. Before every write the store checks that the
//! file on disk is still what it last read or wrote: if someone edited it
//! by hand while the daemon ran, the store refuses to overwrite the edit
//! (`ktbctl reload` picks it up instead).
//!
//! Revisions are a stable hash of text, so a client can fetch an object,
//! edit it, and put it back only if nobody changed it in between.

use std::fmt;
use std::path::{Path, PathBuf};

use toml_edit::{ArrayOfTables, DocumentMut, Item, Table};

use super::state::write_atomic;

const HEADER: &str = "\
# Dynamic config for kiss-tnc-bridge: adapters, TNCs and bridges created
# with ktbctl. The daemon writes this file. To change it by hand, use
# `ktbctl edit`, which checks the result before the daemon uses it.

";

/// The three kinds of object config files define.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Adapter,
    Tnc,
    Bridge,
}

impl Kind {
    /// The array-of-tables key: `[[adapter]]`, `[[tnc]]`, `[[bridge]]`.
    pub fn key(self) -> &'static str {
        match self {
            Kind::Adapter => "adapter",
            Kind::Tnc => "tnc",
            Kind::Bridge => "bridge",
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.key())
    }
}

/// A stable 64-bit hash (FNV-1a) of some text, as 16 hex digits. Stable
/// across Rust versions and restarts, unlike `std`'s hasher.
pub fn revision(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

#[derive(Debug)]
pub enum StoreError {
    /// The file changed on disk since the store last read or wrote it.
    ChangedOnDisk(PathBuf),
    /// TOML that doesn't parse, or isn't one object of the expected kind.
    Invalid(String),
    Io(std::io::Error),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::ChangedOnDisk(p) => write!(
                f,
                "{} changed on disk since the daemon read it; run 'ktbctl reload' to use the edit",
                p.display()
            ),
            StoreError::Invalid(m) => f.write_str(m),
            StoreError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for StoreError {}

#[derive(Debug, Clone)]
pub struct DynamicStore {
    path: PathBuf,
    doc: DocumentMut,
    /// The revision of the file as last read or written; `None` if there
    /// was no file.
    on_disk: Option<String>,
}

impl DynamicStore {
    /// Open `dynamic.toml` at `path`; a missing file is an empty store.
    pub fn open(path: &Path) -> Result<DynamicStore, StoreError> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => Some(t),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(StoreError::Io(e)),
        };
        Self::from_text(path, text.as_deref())
    }

    /// A store over text already read (`None`: no file).
    pub fn from_text(path: &Path, text: Option<&str>) -> Result<DynamicStore, StoreError> {
        // The header belongs to the file, not to the first object, which
        // toml_edit would otherwise attach it to.
        let body = text.unwrap_or("");
        let body = body.strip_prefix(HEADER).unwrap_or(body);
        let doc = body
            .parse::<DocumentMut>()
            .map_err(|e| StoreError::Invalid(format!("{}: {e}", path.display())))?;
        Ok(DynamicStore {
            path: path.to_path_buf(),
            doc,
            on_disk: text.map(revision),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The whole file as it would be written.
    pub fn text(&self) -> String {
        let body = self.doc.to_string();
        if body.trim().is_empty() {
            body
        } else {
            format!("{HEADER}{body}")
        }
    }

    /// The revision of the whole file, for `config.get` / `config.put`.
    pub fn revision(&self) -> String {
        revision(&self.text())
    }

    fn array(&self, kind: Kind) -> Option<&ArrayOfTables> {
        self.doc.get(kind.key()).and_then(Item::as_array_of_tables)
    }

    fn index_of(&self, kind: Kind, id: &str) -> Option<usize> {
        self.array(kind)?
            .iter()
            .position(|t| t.get("id").and_then(|v| v.as_str()) == Some(id))
    }

    pub fn ids(&self, kind: Kind) -> Vec<String> {
        self.array(kind)
            .map(|a| {
                a.iter()
                    .filter_map(|t| t.get("id").and_then(|v| v.as_str()).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn contains(&self, kind: Kind, id: &str) -> bool {
        self.index_of(kind, id).is_some()
    }

    /// One object as standalone TOML (`[[bridge]]` and its ports), ready
    /// to edit and put back.
    pub fn get(&self, kind: Kind, id: &str) -> Option<String> {
        let i = self.index_of(kind, id)?;
        let table = self.array(kind)?.get(i)?.clone();
        let mut one = ArrayOfTables::new();
        one.push(table);
        let mut doc = DocumentMut::new();
        doc.insert(kind.key(), Item::ArrayOfTables(one));
        Some(doc.to_string())
    }

    /// The revision of one object, from its standalone TOML.
    pub fn object_revision(&self, kind: Kind, id: &str) -> Option<String> {
        self.get(kind, id).map(|t| revision(&t))
    }

    /// Parse standalone TOML holding exactly one object of `kind`, as
    /// `get` returns it, into a table for [`put`](Self::put).
    pub fn parse_object(kind: Kind, text: &str) -> Result<Table, StoreError> {
        let doc = text
            .parse::<DocumentMut>()
            .map_err(|e| StoreError::Invalid(e.to_string().trim_end().to_string()))?;
        let others: Vec<&str> = doc
            .iter()
            .map(|(k, _)| k)
            .filter(|k| *k != kind.key())
            .collect();
        if !others.is_empty() {
            return Err(StoreError::Invalid(format!(
                "expected one [[{kind}]], also found {}",
                others.join(", ")
            )));
        }
        match doc.get(kind.key()).and_then(Item::as_array_of_tables) {
            Some(a) if a.len() == 1 => Ok(a.get(0).expect("one").clone()),
            Some(a) => Err(StoreError::Invalid(format!(
                "expected one [[{kind}]], found {}",
                a.len()
            ))),
            None => Err(StoreError::Invalid(format!("expected a [[{kind}]]"))),
        }
    }

    /// The object with `id`, to read.
    pub fn table(&self, kind: Kind, id: &str) -> Option<&Table> {
        let i = self.index_of(kind, id)?;
        self.array(kind)?.get(i)
    }

    /// The object with `id`, to change in place.
    pub fn table_mut(&mut self, kind: Kind, id: &str) -> Option<&mut Table> {
        let i = self.index_of(kind, id)?;
        self.doc
            .get_mut(kind.key())
            .and_then(Item::as_array_of_tables_mut)?
            .get_mut(i)
    }

    /// Every object of a kind, to search.
    pub fn tables(&self, kind: Kind) -> Vec<&Table> {
        self.array(kind)
            .map(|a| a.iter().collect())
            .unwrap_or_default()
    }

    /// Every object of a kind, to change in place.
    pub fn tables_mut(&mut self, kind: Kind) -> Vec<&mut Table> {
        match self
            .doc
            .get_mut(kind.key())
            .and_then(Item::as_array_of_tables_mut)
        {
            Some(a) => a.iter_mut().collect(),
            None => Vec::new(),
        }
    }

    /// Replace the whole file's content (`config.put` of all of it). What's
    /// on disk is still what it was; `save` writes the new text.
    pub fn replace_text(&mut self, text: &str) -> Result<(), StoreError> {
        let body = text.strip_prefix(HEADER).unwrap_or(text);
        self.doc = body
            .parse::<DocumentMut>()
            .map_err(|e| StoreError::Invalid(e.to_string().trim_end().to_string()))?;
        Ok(())
    }

    /// Replace the object with `id`, in place, or append it.
    pub fn put(&mut self, kind: Kind, id: &str, table: Table) {
        match self.index_of(kind, id) {
            Some(i) => {
                let array = self
                    .doc
                    .get_mut(kind.key())
                    .and_then(Item::as_array_of_tables_mut)
                    .expect("index_of found it");
                *array.get_mut(i).expect("index_of found it") = table;
            }
            None => {
                let item = self
                    .doc
                    .entry(kind.key())
                    .or_insert(Item::ArrayOfTables(ArrayOfTables::new()));
                if let Some(array) = item.as_array_of_tables_mut() {
                    array.push(table);
                }
            }
        }
    }

    /// Remove the object with `id`; whether there was one.
    pub fn remove(&mut self, kind: Kind, id: &str) -> bool {
        let Some(i) = self.index_of(kind, id) else {
            return false;
        };
        let array = self
            .doc
            .get_mut(kind.key())
            .and_then(Item::as_array_of_tables_mut)
            .expect("index_of found it");
        array.remove(i);
        if array.is_empty() {
            self.doc.remove(kind.key());
        }
        true
    }

    /// Whether the file on disk is still what the store last read or wrote.
    pub fn unchanged_on_disk(&self) -> Result<bool, StoreError> {
        let now = match std::fs::read_to_string(&self.path) {
            Ok(t) => Some(revision(&t)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(StoreError::Io(e)),
        };
        Ok(now == self.on_disk)
    }

    /// Write the store to disk, unless the file was edited by hand since
    /// the store last read or wrote it.
    pub fn save(&mut self) -> Result<(), StoreError> {
        if !self.unchanged_on_disk()? {
            return Err(StoreError::ChangedOnDisk(self.path.clone()));
        }
        let text = self.text();
        write_atomic(&self.path, text.as_bytes()).map_err(StoreError::Io)?;
        self.on_disk = Some(revision(&text));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BRIDGE: &str = r#"[[bridge]]
id = "ktbr2"
name = "Shack relay"   # kept as written

  [[bridge.port]]
  type = "tcp-client"
  address = "192.168.4.1:8001"

  [[bridge.port]]
  type = "ble-server"
"#;

    #[test]
    fn revisions_are_stable() {
        // Fixed values: a change here breaks clients holding revisions.
        assert_eq!(revision(""), "cbf29ce484222325");
        assert_eq!(revision("a"), "af63dc4c8601ec8c");
        assert_ne!(revision("a"), revision("b"));
    }

    #[test]
    fn put_get_remove_and_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dynamic.toml");
        let mut store = DynamicStore::open(&path).unwrap();
        assert!(store.ids(Kind::Bridge).is_empty());

        let table = DynamicStore::parse_object(Kind::Bridge, BRIDGE).unwrap();
        store.put(Kind::Bridge, "ktbr2", table);
        assert_eq!(store.ids(Kind::Bridge), vec!["ktbr2"]);
        let got = store.get(Kind::Bridge, "ktbr2").unwrap();
        assert!(got.contains("# kept as written"), "{got}");
        assert!(got.contains("[[bridge.port]]"), "{got}");
        store.save().unwrap();

        // What's on disk parses as config and round-trips.
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# Dynamic config"), "{text}");
        let parsed = crate::config::load::parse_str(&text).unwrap();
        assert_eq!(parsed.bridge[0].name, "Shack relay");
        let again = DynamicStore::open(&path).unwrap();
        assert_eq!(again.get(Kind::Bridge, "ktbr2"), Some(got.clone()));
        assert_eq!(again.revision(), store.revision());

        // Replacing keeps the position; removing the last drops the array.
        let renamed = BRIDGE.replace("Shack relay", "Relay");
        store.put(
            Kind::Bridge,
            "ktbr2",
            DynamicStore::parse_object(Kind::Bridge, &renamed).unwrap(),
        );
        assert!(
            store
                .get(Kind::Bridge, "ktbr2")
                .unwrap()
                .contains("\"Relay\"")
        );
        assert_ne!(
            store.object_revision(Kind::Bridge, "ktbr2").unwrap(),
            revision(&got)
        );
        assert!(store.remove(Kind::Bridge, "ktbr2"));
        assert!(!store.remove(Kind::Bridge, "ktbr2"));
        assert!(!store.text().contains("bridge"));
    }

    #[test]
    fn hand_edits_are_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dynamic.toml");
        std::fs::write(&path, BRIDGE).unwrap();
        let mut store = DynamicStore::open(&path).unwrap();
        std::fs::write(&path, BRIDGE.replace("Shack relay", "Edited")).unwrap();
        assert!(!store.unchanged_on_disk().unwrap());
        store.remove(Kind::Bridge, "ktbr2");
        let err = store.save().unwrap_err();
        assert!(matches!(err, StoreError::ChangedOnDisk(_)), "{err}");
        assert!(std::fs::read_to_string(&path).unwrap().contains("Edited"));
        // A file created behind an empty store counts too.
        let other = dir.path().join("new.toml");
        let mut empty = DynamicStore::open(&other).unwrap();
        std::fs::write(&other, BRIDGE).unwrap();
        assert!(matches!(empty.save(), Err(StoreError::ChangedOnDisk(_))));
    }

    #[test]
    fn objects_must_be_one_of_the_kind() {
        assert!(DynamicStore::parse_object(Kind::Tnc, BRIDGE).is_err());
        let two = format!("{BRIDGE}{}", BRIDGE.replace("ktbr2", "ktbr3"));
        let err = DynamicStore::parse_object(Kind::Bridge, &two).unwrap_err();
        assert!(err.to_string().contains("found 2"), "{err}");
        let extra = format!("[[tnc]]\nid = \"x\"\n{BRIDGE}");
        let err = DynamicStore::parse_object(Kind::Bridge, &extra).unwrap_err();
        assert!(err.to_string().contains("also found tnc"), "{err}");
        assert!(DynamicStore::parse_object(Kind::Bridge, "[[bridge]\n").is_err());
    }
}
