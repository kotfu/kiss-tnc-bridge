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

//! Edits to dynamic config: what the control API's `bridge.*`, `port.*`,
//! `tnc.*` and `adapter.*` commands do to `dynamic.toml`, before the daemon
//! validates the result, works out the plan, and applies it.
//!
//! Objects travel as JSON with the same keys as the TOML. These functions
//! enforce the rules validation can't see from a single config: static
//! objects can't be changed, ids and names can't be taken twice, and TNCs
//! and adapters can't be deleted while something refers to them. Ids that
//! a file may leave out (`ktbrN`, port ids) are made explicit, so they
//! don't shift when other objects come and go.

use serde_json::Value;
use toml_edit::{ArrayOfTables, Item, Table};

use super::protocol::{ApiError, ErrorCode};
use crate::config::model::{ConfigFile, PortDef};
use crate::config::store::{DynamicStore, Kind};
use crate::config::{Config, Source};

/// What the edits check against besides `dynamic.toml`.
pub struct World<'a> {
    /// The static files, as last read.
    pub statics: &'a [(Source, ConfigFile)],
    /// The config in effect.
    pub cfg: &'a Config,
}

impl World<'_> {
    fn files(&self) -> impl Iterator<Item = (&Source, &ConfigFile)> {
        self.statics.iter().map(|(s, f)| (s, f))
    }

    /// The file a static object of `kind` with `id` is defined in.
    pub fn static_source(&self, kind: Kind, id: &str) -> Option<&Source> {
        self.files()
            .find(|(_, f)| match kind {
                Kind::Adapter => f.adapter.iter().any(|a| a.id == id),
                Kind::Tnc => f.tnc.iter().any(|t| t.id == id),
                Kind::Bridge => f.bridge.iter().any(|b| b.id.as_deref() == Some(id)),
            })
            .map(|(s, _)| s)
    }

    fn static_bridge_names(&self) -> impl Iterator<Item = &str> {
        self.files()
            .flat_map(|(_, f)| f.bridge.iter().map(|b| b.name.as_str()))
    }
}

fn not_found(kind: Kind, id: &str) -> ApiError {
    ApiError::new(ErrorCode::NotFound, format!("there's no {kind} {id:?}"))
        .object(format!("{kind} {id}"))
}

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError::new(ErrorCode::BadRequest, msg)
}

/// The dynamic object to change, or why it can't be.
fn editable<'s>(
    store: &'s mut DynamicStore,
    world: &World,
    kind: Kind,
    id: &str,
) -> Result<&'s mut Table, ApiError> {
    if let Some(src) = world.static_source(kind, id) {
        return Err(ApiError::new(
            ErrorCode::StaticObjectReadonly,
            format!(
                "{kind} {id} is static config ({src}); edit that file instead, or clone it into a dynamic one"
            ),
        )
        .object(src.to_string()));
    }
    store.table_mut(kind, id).ok_or_else(|| not_found(kind, id))
}

// ── JSON to TOML ────────────────────────────────────────────────────────

/// An object as a TOML table: nested objects become sub-tables
/// (`[tnc.spp]`), arrays of objects arrays of tables (`[[bridge.port]]`).
pub fn json_to_table(v: &Value) -> Result<Table, ApiError> {
    let Value::Object(map) = v else {
        return Err(bad("expected a JSON object"));
    };
    let mut t = Table::new();
    for (k, v) in map {
        match v {
            Value::Null => {}
            Value::Object(_) => {
                t.insert(k, Item::Table(json_to_table(v)?));
            }
            Value::Array(items) if !items.is_empty() && items.iter().all(Value::is_object) => {
                let mut a = ArrayOfTables::new();
                for item in items {
                    a.push(json_to_table(item)?);
                }
                t.insert(k, Item::ArrayOfTables(a));
            }
            other => {
                t.insert(k, Item::Value(json_to_value(other)?));
            }
        }
    }
    Ok(t)
}

fn json_to_value(v: &Value) -> Result<toml_edit::Value, ApiError> {
    Ok(match v {
        Value::Bool(b) => (*b).into(),
        Value::String(s) => s.as_str().into(),
        Value::Number(n) => match n.as_i64() {
            Some(i) => i.into(),
            None => n
                .as_f64()
                .ok_or_else(|| bad(format!("{n} is not a number TOML can hold")))?
                .into(),
        },
        Value::Array(items) => {
            let mut a = toml_edit::Array::new();
            for i in items {
                a.push(json_to_value(i)?);
            }
            toml_edit::Value::Array(a)
        }
        Value::Object(map) => {
            let mut t = toml_edit::InlineTable::new();
            for (k, v) in map {
                if !v.is_null() {
                    t.insert(k, json_to_value(v)?);
                }
            }
            toml_edit::Value::InlineTable(t)
        }
        Value::Null => return Err(bad("null has no TOML form")),
    })
}

fn str_key<'v>(v: &'v Value, key: &str) -> Option<&'v str> {
    v.get(key).and_then(Value::as_str)
}

// ── Bridges ─────────────────────────────────────────────────────────────

fn bridge_ids(store: &DynamicStore, world: &World) -> Vec<String> {
    let mut ids: Vec<String> = world
        .files()
        .flat_map(|(_, f)| f.bridge.iter().filter_map(|b| b.id.clone()))
        .collect();
    ids.extend(store.ids(Kind::Bridge));
    ids
}

/// The lowest free `ktbrN`.
fn next_bridge_id(store: &DynamicStore, world: &World) -> String {
    let taken = bridge_ids(store, world);
    (0..)
        .map(|n| format!("ktbr{n}"))
        .find(|id| !taken.contains(id))
        .expect("some N is free")
}

fn check_bridge_name(
    store: &DynamicStore,
    world: &World,
    name: &str,
    except: Option<&str>,
) -> Result<(), ApiError> {
    if world.static_bridge_names().any(|n| n == name) {
        return Err(ApiError::new(
            ErrorCode::Conflict,
            format!("a static bridge is already named {name:?}"),
        ));
    }
    let taken = store.tables(Kind::Bridge).into_iter().any(|t| {
        t.get("name").and_then(|v| v.as_str()) == Some(name)
            && t.get("id").and_then(|v| v.as_str()) != except
    });
    if taken {
        return Err(ApiError::new(
            ErrorCode::Conflict,
            format!("a bridge is already named {name:?}"),
        ));
    }
    Ok(())
}

/// Give every port in a bridge table an explicit id: its type plus the
/// lowest free index, as the default would have been.
fn name_ports(t: &mut Table) {
    let Some(ports) = t.get_mut("port").and_then(Item::as_array_of_tables_mut) else {
        return;
    };
    let mut taken: Vec<String> = ports
        .iter()
        .filter_map(|p| p.get("id").and_then(|v| v.as_str()).map(str::to_string))
        .collect();
    for p in ports.iter_mut() {
        if p.get("id").is_none() {
            let kind = p
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("port")
                .to_string();
            let id = (0..)
                .map(|n| format!("{kind}{n}"))
                .find(|id| !taken.contains(id))
                .expect("some N is free");
            p.insert("id", toml_edit::value(id.clone()));
            taken.push(id);
        }
    }
}

/// `bridge.create`: a new dynamic bridge, ports and all. Returns its id.
pub fn create_bridge(
    store: &mut DynamicStore,
    world: &World,
    def: &Value,
) -> Result<String, ApiError> {
    let name = str_key(def, "name").ok_or_else(|| bad("a bridge needs a name"))?;
    let id = match str_key(def, "id") {
        Some(id) => {
            if bridge_ids(store, world).iter().any(|x| x == id) {
                return Err(ApiError::new(
                    ErrorCode::Conflict,
                    format!("bridge id {id} is taken"),
                ));
            }
            id.to_string()
        }
        None => next_bridge_id(store, world),
    };
    check_bridge_name(store, world, name, None)?;
    let mut t = json_to_table(def)?;
    t.insert("id", toml_edit::value(id.clone()));
    // New bridges start disabled, so they can be built up a port at a time
    // and enabled when they're complete.
    if !t.contains_key("enabled") {
        t.insert("enabled", toml_edit::value(false));
    }
    name_ports(&mut t);
    store.put(Kind::Bridge, &id, t);
    Ok(id)
}

/// `bridge.update`: replace a dynamic bridge's definition.
pub fn update_bridge(
    store: &mut DynamicStore,
    world: &World,
    id: &str,
    def: &Value,
) -> Result<(), ApiError> {
    editable(store, world, Kind::Bridge, id)?;
    if let Some(other) = str_key(def, "id").filter(|x| *x != id) {
        return Err(bad(format!(
            "a bridge's id can't change ({id} to {other}); create a new bridge instead"
        )));
    }
    let name = str_key(def, "name").ok_or_else(|| bad("a bridge needs a name"))?;
    check_bridge_name(store, world, name, Some(id))?;
    let mut t = json_to_table(def)?;
    t.insert("id", toml_edit::value(id));
    name_ports(&mut t);
    store.put(Kind::Bridge, id, t);
    Ok(())
}

pub fn delete_bridge(store: &mut DynamicStore, world: &World, id: &str) -> Result<(), ApiError> {
    editable(store, world, Kind::Bridge, id)?;
    store.remove(Kind::Bridge, id);
    Ok(())
}

/// `bridge.clone`: copy any bridge, static or dynamic, into a new dynamic
/// one, disabled (it would conflict with the original otherwise), its
/// public name following its new name.
pub fn clone_bridge(
    store: &mut DynamicStore,
    world: &World,
    from: &str,
    new_id: Option<&str>,
    name: &str,
) -> Result<String, ApiError> {
    let mut t = match world.static_source(Kind::Bridge, from) {
        Some(Source::Static(path)) => static_bridge_table(path, from)?,
        _ => store
            .table(Kind::Bridge, from)
            .cloned()
            .ok_or_else(|| not_found(Kind::Bridge, from))?,
    };
    let id = match new_id {
        Some(id) if bridge_ids(store, world).iter().any(|x| x == id) => {
            return Err(ApiError::new(
                ErrorCode::Conflict,
                format!("bridge id {id} is taken"),
            ));
        }
        Some(id) => id.to_string(),
        None => next_bridge_id(store, world),
    };
    check_bridge_name(store, world, name, None)?;
    t.decor_mut().clear();
    t.insert("id", toml_edit::value(id.clone()));
    t.insert("name", toml_edit::value(name));
    t.insert("enabled", toml_edit::value(false));
    t.remove("public_name");
    name_ports(&mut t);
    store.put(Kind::Bridge, &id, t);
    Ok(id)
}

/// A static bridge's table, read from its file as it is now.
fn static_bridge_table(path: &std::path::Path, id: &str) -> Result<Table, ApiError> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        ApiError::new(
            ErrorCode::Internal,
            format!("can't read {}: {e}", path.display()),
        )
    })?;
    let doc = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| ApiError::new(ErrorCode::Internal, format!("{}: {e}", path.display())))?;
    doc.get("bridge")
        .and_then(Item::as_array_of_tables)
        .and_then(|a| {
            a.iter()
                .find(|t| t.get("id").and_then(|v| v.as_str()) == Some(id))
        })
        .cloned()
        .ok_or_else(|| not_found(Kind::Bridge, id))
}

/// A dynamic bridge's `enabled`. Static bridges use overrides instead.
pub fn set_enabled(store: &mut DynamicStore, id: &str, enabled: bool) -> Result<(), ApiError> {
    let t = store
        .table_mut(Kind::Bridge, id)
        .ok_or_else(|| not_found(Kind::Bridge, id))?;
    t.insert("enabled", toml_edit::value(enabled));
    Ok(())
}

// ── Ports ───────────────────────────────────────────────────────────────

fn ports_mut(t: &mut Table) -> &mut ArrayOfTables {
    if !matches!(t.get("port"), Some(Item::ArrayOfTables(_))) {
        t.insert("port", Item::ArrayOfTables(ArrayOfTables::new()));
    }
    t.get_mut("port")
        .and_then(Item::as_array_of_tables_mut)
        .expect("just made sure")
}

fn port_index(t: &Table, port: &str) -> Option<usize> {
    t.get("port")?
        .as_array_of_tables()?
        .iter()
        .position(|p| p.get("id").and_then(|v| v.as_str()) == Some(port))
}

/// `port.add`. Returns the port's id.
pub fn add_port(
    store: &mut DynamicStore,
    world: &World,
    bridge: &str,
    def: &Value,
) -> Result<String, ApiError> {
    serde_json::from_value::<PortDef>(def.clone()).map_err(|e| bad(format!("port: {e}")))?;
    let t = editable(store, world, Kind::Bridge, bridge)?;
    if let Some(id) = str_key(def, "id")
        && port_index(t, id).is_some()
    {
        return Err(ApiError::new(
            ErrorCode::Conflict,
            format!("bridge {bridge} already has a port {id}"),
        ));
    }
    ports_mut(t).push(json_to_table(def)?);
    name_ports(t);
    let id = ports_mut(t)
        .iter()
        .last()
        .and_then(|p| p.get("id"))
        .and_then(|v| v.as_str())
        .expect("name_ports gave it an id")
        .to_string();
    Ok(id)
}

/// `port.update`: replace a port's definition; its id stays.
pub fn update_port(
    store: &mut DynamicStore,
    world: &World,
    bridge: &str,
    port: &str,
    def: &Value,
) -> Result<(), ApiError> {
    serde_json::from_value::<PortDef>(def.clone()).map_err(|e| bad(format!("port: {e}")))?;
    let t = editable(store, world, Kind::Bridge, bridge)?;
    let i = port_index(t, port).ok_or_else(|| {
        ApiError::new(
            ErrorCode::NotFound,
            format!("bridge {bridge} has no port {port}"),
        )
    })?;
    let mut p = json_to_table(def)?;
    p.insert("id", toml_edit::value(port));
    *ports_mut(t).get_mut(i).expect("found") = p;
    Ok(())
}

pub fn remove_port(
    store: &mut DynamicStore,
    world: &World,
    bridge: &str,
    port: &str,
) -> Result<(), ApiError> {
    let t = editable(store, world, Kind::Bridge, bridge)?;
    let i = port_index(t, port).ok_or_else(|| {
        ApiError::new(
            ErrorCode::NotFound,
            format!("bridge {bridge} has no port {port}"),
        )
    })?;
    ports_mut(t).remove(i);
    Ok(())
}

// ── TNCs and adapters ───────────────────────────────────────────────────

fn ids_of(store: &DynamicStore, world: &World, kind: Kind) -> Vec<String> {
    let mut ids: Vec<String> = world
        .files()
        .flat_map(|(_, f)| match kind {
            Kind::Adapter => f.adapter.iter().map(|a| a.id.clone()).collect::<Vec<_>>(),
            Kind::Tnc => f.tnc.iter().map(|t| t.id.clone()).collect(),
            Kind::Bridge => Vec::new(),
        })
        .collect();
    ids.extend(store.ids(kind));
    ids
}

/// `tnc.create` / `adapter.create`. Returns the id.
pub fn create_object(
    store: &mut DynamicStore,
    world: &World,
    kind: Kind,
    def: &Value,
) -> Result<String, ApiError> {
    let id = str_key(def, "id").ok_or_else(|| bad(format!("a {kind} needs an id")))?;
    if ids_of(store, world, kind).iter().any(|x| x == id) {
        return Err(ApiError::new(
            ErrorCode::Conflict,
            format!("{kind} id {id} is taken"),
        ));
    }
    store.put(kind, id, json_to_table(def)?);
    Ok(id.to_string())
}

/// `tnc.update` / `adapter.update`: replace the definition. A new `id`
/// renames it, and dynamic references follow.
pub fn update_object(
    store: &mut DynamicStore,
    world: &World,
    kind: Kind,
    id: &str,
    def: &Value,
) -> Result<String, ApiError> {
    editable(store, world, kind, id)?;
    let new_id = str_key(def, "id").unwrap_or(id).to_string();
    if new_id != id && ids_of(store, world, kind).contains(&new_id) {
        return Err(ApiError::new(
            ErrorCode::Conflict,
            format!("{kind} id {new_id} is taken"),
        ));
    }
    let mut t = json_to_table(def)?;
    t.insert("id", toml_edit::value(new_id.clone()));
    if new_id == id {
        store.put(kind, id, t);
        return Ok(new_id);
    }
    // A rename: replace in place under the new id, then follow references.
    *store.table_mut(kind, id).expect("editable") = t;
    for r in references(store, kind, id)
        .iter()
        .map(|r| r.path.clone())
        .collect::<Vec<_>>()
    {
        set_reference(store, &r, &new_id);
    }
    Ok(new_id)
}

/// `tnc.delete` / `adapter.delete`, refused while anything refers to it.
pub fn delete_object(
    store: &mut DynamicStore,
    world: &World,
    kind: Kind,
    id: &str,
) -> Result<(), ApiError> {
    editable(store, world, kind, id)?;
    let refs = references(store, kind, id);
    if !refs.is_empty() {
        let list: Vec<String> = refs.iter().map(|r| r.describe.clone()).collect();
        return Err(ApiError::new(
            ErrorCode::InUse,
            format!(
                "{kind} {id} is in use by {}; change those first",
                list.join(", ")
            ),
        )
        .object(format!("{kind} {id}")));
    }
    store.remove(kind, id);
    Ok(())
}

/// Where a reference to a dynamic TNC or adapter is: static config can't
/// refer to dynamic objects, so only `dynamic.toml` needs searching.
struct Reference {
    path: RefPath,
    describe: String,
}

#[derive(Clone)]
enum RefPath {
    /// A port's `tnc` or `adapter` key.
    Port {
        bridge: String,
        port: usize,
        key: &'static str,
    },
    /// A TNC's `spp.adapter`.
    TncSpp { tnc: String },
}

fn references(store: &DynamicStore, kind: Kind, id: &str) -> Vec<Reference> {
    let mut out = Vec::new();
    let key = match kind {
        Kind::Tnc => "tnc",
        Kind::Adapter => "adapter",
        Kind::Bridge => return out,
    };
    for b in store.tables(Kind::Bridge) {
        let bid = b
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();
        if let Some(ports) = b.get("port").and_then(Item::as_array_of_tables) {
            for (i, p) in ports.iter().enumerate() {
                if p.get(key).and_then(|v| v.as_str()) == Some(id) {
                    let pid = p.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                    out.push(Reference {
                        path: RefPath::Port {
                            bridge: bid.clone(),
                            port: i,
                            key,
                        },
                        describe: format!("bridge {bid} port {pid}"),
                    });
                }
            }
        }
    }
    if kind == Kind::Adapter {
        for t in store.tables(Kind::Tnc) {
            let spp_adapter = t
                .get("spp")
                .and_then(|s| s.get("adapter"))
                .and_then(|v| v.as_str());
            if spp_adapter == Some(id) {
                let tid = t
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
                    .to_string();
                out.push(Reference {
                    describe: format!("tnc {tid}"),
                    path: RefPath::TncSpp { tnc: tid },
                });
            }
        }
    }
    out
}

fn set_reference(store: &mut DynamicStore, path: &RefPath, to: &str) {
    match path {
        RefPath::Port { bridge, port, key } => {
            if let Some(p) = store
                .table_mut(Kind::Bridge, bridge)
                .and_then(|b| b.get_mut("port"))
                .and_then(Item::as_array_of_tables_mut)
                .and_then(|a| a.get_mut(*port))
            {
                p.insert(key, toml_edit::value(to));
            }
        }
        RefPath::TncSpp { tnc } => {
            if let Some(spp) = store
                .table_mut(Kind::Tnc, tnc)
                .and_then(|t| t.get_mut("spp"))
            {
                match spp {
                    Item::Table(t) => {
                        t.insert("adapter", toml_edit::value(to));
                    }
                    Item::Value(toml_edit::Value::InlineTable(t)) => {
                        t.insert("adapter", to.into());
                    }
                    _ => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::load::parse_str;
    use serde_json::json;

    const STATIC: &str = r#"
[[tnc]]
id = "d75"
spp = { address = "00:11:22:33:44:55" }

[[bridge]]
id = "ktbr0"
name = "Field"
  [[bridge.port]]
  type = "spp-client"
  tnc = "d75"
  [[bridge.port]]
  type = "ble-server"
"#;

    fn statics() -> Vec<(Source, ConfigFile)> {
        vec![(
            Source::Static("/etc/x.toml".into()),
            parse_str(STATIC).unwrap(),
        )]
    }

    fn store() -> DynamicStore {
        DynamicStore::from_text(std::path::Path::new("/nowhere/dynamic.toml"), None).unwrap()
    }

    fn with<T>(f: impl FnOnce(&mut DynamicStore, &World) -> T) -> (T, DynamicStore) {
        let statics = statics();
        let cfg = crate::config::resolve(&statics).unwrap();
        let world = World {
            statics: &statics,
            cfg: &cfg,
        };
        let mut s = store();
        let out = f(&mut s, &world);
        (out, s)
    }

    fn relay() -> Value {
        json!({
            "name": "Relay",
            "port": [
                {"type": "tcp-client", "address": "192.168.4.1:8001"},
                {"type": "ble-server"},
                {"type": "tcp-server", "listen": "8001", "allow": ["192.168.4.0/24"]}
            ]
        })
    }

    #[test]
    fn created_bridges_get_the_next_id_and_named_ports() {
        let (id, s) = with(|s, w| create_bridge(s, w, &relay()).unwrap());
        assert_eq!(id, "ktbr1", "ktbr0 is static");
        let text = s.get(Kind::Bridge, "ktbr1").unwrap();
        assert!(text.contains("id = \"tcp-client0\""), "{text}");
        assert!(text.contains("id = \"ble-server0\""), "{text}");
        assert!(text.contains("allow = [\"192.168.4.0/24\"]"), "{text}");
        // The result is config the loader reads.
        let parsed = parse_str(&s.text()).unwrap();
        assert_eq!(parsed.bridge[0].ports.len(), 3);
    }

    #[test]
    fn ids_and_names_are_taken_once() {
        let (r, _) = with(|s, w| create_bridge(s, w, &json!({"id": "ktbr0", "name": "x"})));
        assert_eq!(r.unwrap_err().code, ErrorCode::Conflict);
        let (r, _) = with(|s, w| create_bridge(s, w, &json!({"name": "Field"})));
        assert_eq!(r.unwrap_err().code, ErrorCode::Conflict);
        let (r, _) = with(|s, w| {
            create_bridge(s, w, &relay()).unwrap();
            create_bridge(s, w, &relay())
        });
        assert!(r.unwrap_err().message.contains("already named"));
        let (r, _) = with(|s, w| create_object(s, w, Kind::Tnc, &json!({"id": "d75"})));
        assert_eq!(r.unwrap_err().code, ErrorCode::Conflict);
    }

    #[test]
    fn static_objects_are_read_only() {
        let (r, _) = with(|s, w| update_bridge(s, w, "ktbr0", &json!({"name": "x"})));
        let e = r.unwrap_err();
        assert_eq!(e.code, ErrorCode::StaticObjectReadonly);
        assert_eq!(e.object.as_deref(), Some("static: /etc/x.toml"));
        let (r, _) = with(|s, w| delete_object(s, w, Kind::Tnc, "d75"));
        assert_eq!(r.unwrap_err().code, ErrorCode::StaticObjectReadonly);
        let (r, _) = with(|s, w| {
            add_port(
                s,
                w,
                "ktbr0",
                &json!({"type": "tcp-server", "listen": "8001"}),
            )
        });
        assert_eq!(r.unwrap_err().code, ErrorCode::StaticObjectReadonly);
        let (r, _) = with(|s, w| delete_bridge(s, w, "ktbr9"));
        assert_eq!(r.unwrap_err().code, ErrorCode::NotFound);
    }

    #[test]
    fn ports_are_added_updated_and_removed() {
        let (_, s) = with(|s, w| {
            create_bridge(s, w, &relay()).unwrap();
            let id = add_port(
                s,
                w,
                "ktbr1",
                &json!({"type": "tcp-server", "listen": "8002"}),
            )
            .unwrap();
            assert_eq!(id, "tcp-server1");
            let e = add_port(
                s,
                w,
                "ktbr1",
                &json!({"id": "tcp-server1", "type": "tcp-server", "listen": "8003"}),
            )
            .unwrap_err();
            assert_eq!(e.code, ErrorCode::Conflict);
            let e = add_port(s, w, "ktbr1", &json!({"type": "usb"})).unwrap_err();
            assert_eq!(e.code, ErrorCode::BadRequest);
            update_port(
                s,
                w,
                "ktbr1",
                "tcp-server1",
                &json!({"type": "tcp-server", "listen": "8009"}),
            )
            .unwrap();
            remove_port(s, w, "ktbr1", "ble-server0").unwrap();
            assert_eq!(
                remove_port(s, w, "ktbr1", "nope").unwrap_err().code,
                ErrorCode::NotFound
            );
        });
        let parsed = parse_str(&s.text()).unwrap();
        let ids: Vec<_> = parsed.bridge[0]
            .ports
            .iter()
            .map(|p| p.id().unwrap().to_string())
            .collect();
        assert_eq!(ids, vec!["tcp-client0", "tcp-server0", "tcp-server1"]);
        assert!(s.text().contains("listen = \"8009\""));
    }

    #[test]
    fn clones_are_dynamic_disabled_and_named_anew() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.toml");
        std::fs::write(
            &path,
            STATIC.replace("name = \"Field\"", "name = \"Field\"\npublic_name = \"F\""),
        )
        .unwrap();
        let statics = vec![(
            Source::Static(path.clone()),
            parse_str(&std::fs::read_to_string(&path).unwrap()).unwrap(),
        )];
        let cfg = crate::config::resolve(&statics).unwrap();
        let w = World {
            statics: &statics,
            cfg: &cfg,
        };
        let mut s = store();
        let id = clone_bridge(&mut s, &w, "ktbr0", None, "Field copy").unwrap();
        assert_eq!(id, "ktbr1");
        let parsed = parse_str(&s.text()).unwrap();
        let b = &parsed.bridge[0];
        assert_eq!(b.name, "Field copy");
        assert_eq!(b.enabled, Some(false));
        assert_eq!(b.public_name, None);
        assert_eq!(b.ports[0].id(), Some("spp-client0"));
        assert_eq!(
            clone_bridge(&mut s, &w, "nope", None, "x")
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
    }

    #[test]
    fn in_use_objects_stay_and_renames_carry_references() {
        let (_, s) = with(|s, w| {
            create_object(
                s,
                w,
                Kind::Adapter,
                &json!({"id": "dongle", "mac": "00:1A:7D:DA:71:13"}),
            )
            .unwrap();
            create_object(
                s,
                w,
                Kind::Tnc,
                &json!({"id": "nino", "serial": {"path": "/dev/serial/by-id/x"}, "spp": {"address": "00:11:22:33:44:66", "adapter": "dongle"}}),
            )
            .unwrap();
            create_bridge(
                s,
                w,
                &json!({"name": "Nino", "port": [{"type": "serial", "tnc": "nino"}, {"type": "ble-server", "adapter": "dongle"}]}),
            )
            .unwrap();
            let e = delete_object(s, w, Kind::Tnc, "nino").unwrap_err();
            assert_eq!(e.code, ErrorCode::InUse);
            assert!(
                e.message.contains("bridge ktbr1 port serial0"),
                "{}",
                e.message
            );
            let e = delete_object(s, w, Kind::Adapter, "dongle").unwrap_err();
            assert!(e.message.contains("tnc nino"), "{}", e.message);
            assert!(
                e.message.contains("bridge ktbr1 port ble-server0"),
                "{}",
                e.message
            );
            // Rename both; references follow.
            update_object(s, w, Kind::Tnc, "nino", &json!({"id": "ninotnc", "serial": {"path": "/dev/serial/by-id/x"}, "spp": {"address": "00:11:22:33:44:66", "adapter": "dongle"}})).unwrap();
            update_object(
                s,
                w,
                Kind::Adapter,
                "dongle",
                &json!({"id": "usb", "mac": "00:1A:7D:DA:71:13"}),
            )
            .unwrap();
            assert_eq!(
                update_object(
                    s,
                    w,
                    Kind::Adapter,
                    "usb",
                    &json!({"id": "usb", "mac": "x"})
                )
                .map(|_| ()),
                Ok(())
            );
        });
        let text = s.text();
        assert!(text.contains("tnc = \"ninotnc\""), "{text}");
        assert!(text.contains("adapter = \"usb\""), "{text}");
        assert!(!text.contains("\"dongle\""), "{text}");
    }
}
