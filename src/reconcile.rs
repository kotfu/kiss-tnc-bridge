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

//! The reload plan: what applying a new config to the running daemon
//! would do, worked out without doing it. `ktbctl check` shows it,
//! `reload` applies it, and `--non-disruptive` refuses it when it would
//! disrupt anyone.
//!
//! The comparison is between resolved configs, port by port, so file
//! layout, comments and adapter ids that still name the same MAC change
//! nothing. Each port is unchanged, updated in place, restarted, started or
//! stopped; only changes that alter the link itself restart a port (see
//! the reload table in docs/plan-1.0.md).
//!
//! **Disruptive** means someone would notice: a session disconnected, or a
//! TNC link that's in use dropping while the port restarts or the bridge
//! stops. Restarting or stopping things nobody is using isn't.

use std::collections::BTreeMap;
use std::fmt;
use std::net::{IpAddr, SocketAddr};

use serde::{Deserialize, Serialize};

use crate::bridge::SessionId;
use crate::config::{AdapterSel, Bridge, Config, Mac, Port, PortKind};
use crate::port::listen::allowed;

/// One live app session, as the plan sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionView {
    pub id: SessionId,
    /// Higher is newer: `max_clients` drops the newest first.
    pub seq: u64,
}

/// A running bridge, as the plan sees it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BridgeView {
    /// The TNC link is up or coming up.
    pub tnc_active: bool,
    pub sessions: Vec<SessionView>,
}

/// Which MAC an adapter value means on this host right now, if known.
pub type AdapterLookup<'a> = &'a dyn Fn(&AdapterSel) -> Option<Mac>;

/// What the plan needs to know about the running daemon.
#[derive(Default)]
pub struct Runtime<'a> {
    pub bridges: BTreeMap<String, BridgeView>,
    /// Which MAC an adapter value means right now, if known: `Default` and
    /// `hciN` resolve to a MAC through the adapters present, so pointing a
    /// port at the adapter it already uses changes nothing.
    pub adapter_mac: Option<AdapterLookup<'a>>,
}

impl Runtime<'_> {
    fn same_adapter(&self, a: &AdapterSel, b: &AdapterSel) -> bool {
        if a == b {
            return true;
        }
        match self.adapter_mac {
            Some(f) => matches!((f(a), f(b)), (Some(x), Some(y)) if x == y),
            None => false,
        }
    }

    fn sessions(&self, bridge: &str, port: &str) -> Vec<&SessionView> {
        self.bridges
            .get(bridge)
            .map(|b| b.sessions.iter().filter(|s| s.id.port == port).collect())
            .unwrap_or_default()
    }

    fn tnc_active(&self, bridge: &str) -> bool {
        self.bridges.get(bridge).is_some_and(|b| b.tnc_active)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Unchanged,
    Updated,
    Restarted,
    Started,
    Stopped,
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Action::Unchanged => "unchanged",
            Action::Updated => "updated",
            Action::Restarted => "restarted",
            Action::Started => "started",
            Action::Stopped => "stopped",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PortPlan {
    pub id: String,
    pub action: Action,
    /// What changed, for the report: `allow`, `listen`, ….
    pub changes: Vec<String>,
    /// Sessions this would disconnect.
    pub disconnects: Vec<SessionId>,
    /// A TNC port whose link is in use would drop it.
    pub drops_tnc_link: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BridgePlan {
    pub id: String,
    pub action: Action,
    /// Bridge-level changes: `name`, `public_name`, `app_params`.
    pub changes: Vec<String>,
    pub ports: Vec<PortPlan>,
    /// Stopping a bridge whose TNC link or sessions are in use.
    pub stops_in_use: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Plan {
    /// Daemon-wide changes applied live: `log_level`, `control.group`.
    pub global: Vec<String>,
    pub bridges: Vec<BridgePlan>,
}

impl Plan {
    /// Nothing at all to do.
    pub fn is_noop(&self) -> bool {
        self.global.is_empty() && self.bridges.iter().all(|b| b.action == Action::Unchanged)
    }

    /// Every session the plan disconnects.
    pub fn disconnects(&self) -> Vec<&SessionId> {
        self.bridges
            .iter()
            .flat_map(|b| b.ports.iter().flat_map(|p| p.disconnects.iter()))
            .collect()
    }

    /// Why the plan is disruptive; empty if it isn't.
    pub fn disruptions(&self) -> Vec<String> {
        let mut out = Vec::new();
        for b in &self.bridges {
            if b.stops_in_use {
                out.push(format!("bridge {} stops while in use", b.id));
            }
            for p in &b.ports {
                for s in &p.disconnects {
                    out.push(format!("disconnects {}/{s}", b.id));
                }
                if p.drops_tnc_link {
                    out.push(format!(
                        "TNC port {}/{} restarts while its link is in use",
                        b.id, p.id
                    ));
                }
            }
        }
        out
    }

    pub fn disruptive(&self) -> bool {
        !self.disruptions().is_empty()
    }
}

/// What applying `new` over `old` would do to the running daemon.
pub fn plan(old: &Config, new: &Config, rt: &Runtime) -> Plan {
    let mut global = Vec::new();
    if old.global.log_level != new.global.log_level {
        global.push("log_level".into());
    }
    if old.control.group != new.control.group {
        global.push("control.group".into());
    }

    let running = |c: &Config| -> BTreeMap<String, Bridge> {
        c.bridges
            .iter()
            .filter(|b| b.enabled)
            .map(|b| (b.id.clone(), b.clone()))
            .collect()
    };
    let before = running(old);
    let after = running(new);

    let mut bridges = Vec::new();
    // Stopped bridges first, the order they're applied in.
    for (id, b) in &before {
        if !after.contains_key(id) {
            bridges.push(stop_bridge(b, rt));
        }
    }
    for (id, nb) in &after {
        match before.get(id) {
            Some(ob) => bridges.push(compare_bridge(ob, nb, rt)),
            None => bridges.push(BridgePlan {
                id: id.clone(),
                action: Action::Started,
                changes: Vec::new(),
                ports: nb.ports.iter().map(started).collect(),
                stops_in_use: false,
            }),
        }
    }
    Plan { global, bridges }
}

fn started(p: &Port) -> PortPlan {
    PortPlan {
        id: p.id.clone(),
        action: Action::Started,
        changes: Vec::new(),
        disconnects: Vec::new(),
        drops_tnc_link: false,
    }
}

fn stop_bridge(b: &Bridge, rt: &Runtime) -> BridgePlan {
    let ports = b
        .ports
        .iter()
        .map(|p| PortPlan {
            id: p.id.clone(),
            action: Action::Stopped,
            changes: Vec::new(),
            disconnects: rt
                .sessions(&b.id, &p.id)
                .into_iter()
                .map(|s| s.id.clone())
                .collect(),
            drops_tnc_link: false,
        })
        .collect::<Vec<_>>();
    let in_use = rt.tnc_active(&b.id) || ports.iter().any(|p| !p.disconnects.is_empty());
    BridgePlan {
        id: b.id.clone(),
        action: Action::Stopped,
        changes: Vec::new(),
        ports,
        stops_in_use: in_use,
    }
}

fn compare_bridge(old: &Bridge, new: &Bridge, rt: &Runtime) -> BridgePlan {
    let mut changes = Vec::new();
    if old.name != new.name {
        changes.push("name".into());
    }
    if old.public_name != new.public_name {
        changes.push("public_name".into());
    }
    if old.app_params != new.app_params {
        changes.push("app_params".into());
    }

    let mut ports = Vec::new();
    for op in &old.ports {
        if !new.ports.iter().any(|p| p.id == op.id) {
            ports.push(PortPlan {
                id: op.id.clone(),
                action: Action::Stopped,
                changes: Vec::new(),
                disconnects: rt
                    .sessions(&old.id, &op.id)
                    .into_iter()
                    .map(|s| s.id.clone())
                    .collect(),
                drops_tnc_link: false,
            });
        }
    }
    for np in &new.ports {
        match old.ports.iter().find(|p| p.id == np.id) {
            Some(op) => ports.push(compare_port(&old.id, op, np, rt)),
            None => ports.push(started(np)),
        }
    }
    let action = if changes.is_empty() && ports.iter().all(|p| p.action == Action::Unchanged) {
        Action::Unchanged
    } else {
        Action::Updated
    };
    BridgePlan {
        id: new.id.clone(),
        action,
        changes,
        ports,
        stops_in_use: false,
    }
}

fn compare_port(bridge: &str, old: &Port, new: &Port, rt: &Runtime) -> PortPlan {
    let mut changes: Vec<String> = Vec::new();
    let mut restart = false;
    let mut kick: Vec<SessionId> = Vec::new();
    let note = |c: &mut Vec<String>, name: &str| c.push(name.to_string());

    if old.role != new.role
        || std::mem::discriminant(&old.kind) != std::mem::discriminant(&new.kind)
    {
        note(
            &mut changes,
            if old.role != new.role { "role" } else { "type" },
        );
        restart = true;
    } else {
        match (&old.kind, &new.kind) {
            (
                PortKind::TcpClient {
                    host: oh,
                    port: op,
                    connect: oc,
                    idle_timeout: oi,
                },
                PortKind::TcpClient {
                    host: nh,
                    port: np,
                    connect: nc,
                    idle_timeout: ni,
                },
            ) => {
                if (oh, op) != (nh, np) {
                    note(&mut changes, "address");
                    restart = true;
                }
                if oc != nc {
                    note(&mut changes, "connect");
                }
                if oi != ni {
                    note(&mut changes, "idle_timeout");
                }
            }
            (
                PortKind::SppClient {
                    tnc: ot,
                    model: om,
                    spp: os,
                    connect: oc,
                    idle_timeout: oi,
                    exit_kiss_on_disconnect: oe,
                },
                PortKind::SppClient {
                    tnc: nt,
                    model: nm,
                    spp: ns,
                    connect: nc,
                    idle_timeout: ni,
                    exit_kiss_on_disconnect: ne,
                },
            ) => {
                if os.address != ns.address || os.channel != ns.channel {
                    note(&mut changes, "spp");
                    restart = true;
                } else if !rt.same_adapter(&os.adapter, &ns.adapter) {
                    note(&mut changes, "spp.adapter");
                    restart = true;
                }
                if om != nm {
                    note(&mut changes, "model");
                    restart = true;
                }
                if ot != nt {
                    note(&mut changes, "tnc");
                }
                if oc != nc {
                    note(&mut changes, "connect");
                }
                if oi != ni {
                    note(&mut changes, "idle_timeout");
                }
                if oe != ne {
                    note(&mut changes, "exit_kiss_on_disconnect");
                }
            }
            (
                PortKind::Serial {
                    tnc: ot,
                    model: om,
                    serial: os,
                    connect: oc,
                    idle_timeout: oi,
                    exit_kiss_on_disconnect: oe,
                },
                PortKind::Serial {
                    tnc: nt,
                    model: nm,
                    serial: ns,
                    connect: nc,
                    idle_timeout: ni,
                    exit_kiss_on_disconnect: ne,
                },
            ) => {
                if os != ns {
                    note(&mut changes, "serial");
                    restart = true;
                }
                if om != nm {
                    note(&mut changes, "model");
                    restart = true;
                }
                if ot != nt {
                    note(&mut changes, "tnc");
                }
                if oc != nc {
                    note(&mut changes, "connect");
                }
                if oi != ni {
                    note(&mut changes, "idle_timeout");
                }
                if oe != ne {
                    note(&mut changes, "exit_kiss_on_disconnect");
                }
            }
            (
                PortKind::TcpServer {
                    listen: ol,
                    max_clients: om,
                    allow: oa,
                    announce: on,
                },
                PortKind::TcpServer {
                    listen: nl,
                    max_clients: nm,
                    allow: na,
                    announce: nn,
                },
            ) => {
                if ol != nl {
                    note(&mut changes, "listen");
                    restart = true;
                } else {
                    let sessions = rt.sessions(bridge, &old.id);
                    if oa != na {
                        note(&mut changes, "allow");
                        // A security setting: sessions no longer allowed go.
                        for s in &sessions {
                            let ip = s.id.peer.parse::<SocketAddr>().map(|a| a.ip()).ok();
                            if ip.is_some_and(|ip: IpAddr| !allowed(na, ip)) {
                                kick.push(s.id.clone());
                            }
                        }
                    }
                    if om != nm {
                        note(&mut changes, "max_clients");
                        if let Some(max) = nm {
                            newest_beyond(&sessions, *max as usize, &mut kick);
                        }
                    }
                }
                if on != nn {
                    note(&mut changes, "announce");
                }
            }
            (
                PortKind::BleServer {
                    adapter: oa,
                    max_clients: om,
                },
                PortKind::BleServer {
                    adapter: na,
                    max_clients: nm,
                },
            ) => {
                if !rt.same_adapter(oa, na) {
                    note(&mut changes, "adapter");
                    restart = true;
                } else if om != nm {
                    note(&mut changes, "max_clients");
                    newest_beyond(&rt.sessions(bridge, &old.id), *nm as usize, &mut kick);
                }
            }
            _ => unreachable!("same discriminant"),
        }
    }

    let is_tnc = new.role == crate::config::Role::Tnc;
    let mut drops_tnc_link = false;
    let disconnects = if restart {
        if is_tnc {
            drops_tnc_link = rt.tnc_active(bridge);
            Vec::new()
        } else {
            rt.sessions(bridge, &old.id)
                .into_iter()
                .map(|s| s.id.clone())
                .collect()
        }
    } else {
        kick.sort_by(|a, b| a.peer.cmp(&b.peer));
        kick.dedup();
        kick
    };
    let action = if restart {
        Action::Restarted
    } else if changes.is_empty() {
        Action::Unchanged
    } else {
        Action::Updated
    };
    PortPlan {
        id: new.id.clone(),
        action,
        changes,
        disconnects,
        drops_tnc_link,
    }
}

/// The newest sessions over `max`, oldest-first order preserved.
fn newest_beyond(sessions: &[&SessionView], max: usize, out: &mut Vec<SessionId>) {
    if sessions.len() <= max {
        return;
    }
    let mut by_age: Vec<&&SessionView> = sessions.iter().collect();
    by_age.sort_by_key(|s| s.seq);
    out.extend(by_age[max..].iter().map(|s| s.id.clone()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::load::parse_str;
    use crate::config::{Source, resolve};

    fn cfg(text: &str) -> Config {
        resolve(&[(Source::Static("t.toml".into()), parse_str(text).unwrap())]).unwrap()
    }

    const BASE: &str = r#"
[global]
log_level = "info"

[[adapter]]
id = "builtin"
mac = "B8:27:EB:12:34:56"

[[tnc]]
id = "d75"
name = "TH-D75"
model = "th-d75"
spp = { address = "00:11:22:33:44:55", adapter = "builtin" }
serial = { path = "/dev/serial/by-id/usb-JVCKENWOOD_TH-D75-if00" }

[[bridge]]
id = "ktbr0"
name = "Field"
  [[bridge.port]]
  id = "radio"
  type = "spp-client"
  tnc = "d75"
  [[bridge.port]]
  id = "phones"
  type = "ble-server"
  adapter = "builtin"
  max_clients = 2
  [[bridge.port]]
  id = "lan"
  type = "tcp-server"
  listen = "0.0.0.0:8001"
  allow = ["192.168.4.0/24"]

[[bridge]]
id = "ktbr3"
name = "Desk"
enabled = false
  [[bridge.port]]
  id = "radio"
  type = "serial"
  tnc = "d75"
  [[bridge.port]]
  id = "lan"
  type = "tcp-server"
  listen = "0.0.0.0:8001"
"#;

    fn s(port: &str, peer: &str, seq: u64) -> SessionView {
        SessionView {
            id: SessionId::new(port, peer),
            seq,
        }
    }

    /// ktbr0 busy: link up, two phones, three LAN clients.
    fn busy() -> Runtime<'static> {
        let mut rt = Runtime::default();
        rt.bridges.insert(
            "ktbr0".into(),
            BridgeView {
                tnc_active: true,
                sessions: vec![
                    s("phones", "7C:2A:00:00:00:01", 1),
                    s("phones", "7C:2A:00:00:00:02", 2),
                    s("lan", "192.168.4.20:5000", 3),
                    s("lan", "192.168.4.200:5001", 4),
                    s("lan", "192.168.4.21:5002", 5),
                ],
            },
        );
        rt
    }

    fn with(edit: impl Fn(&str) -> String) -> (Config, Config) {
        (cfg(BASE), cfg(&edit(BASE)))
    }

    fn port<'a>(p: &'a Plan, bridge: &str, port: &str) -> &'a PortPlan {
        p.bridges
            .iter()
            .find(|b| b.id == bridge)
            .and_then(|b| b.ports.iter().find(|x| x.id == port))
            .unwrap_or_else(|| panic!("no {bridge}/{port} in {p:#?}"))
    }

    #[test]
    fn no_change_is_a_no_op() {
        let p = plan(&cfg(BASE), &cfg(BASE), &busy());
        assert!(p.is_noop(), "{p:#?}");
        assert!(!p.disruptive());
    }

    #[test]
    fn layout_and_names_that_resolve_the_same_change_nothing() {
        // Comments, key order, the adapter's id, and a value written as a
        // MAC instead of an id.
        let edited = BASE
            .replace("[global]", "# a comment\n[global]")
            .replace("id = \"builtin\"", "id = \"pi4\"")
            .replace("adapter = \"builtin\" }", "adapter = \"pi4\" }")
            .replace(
                "adapter = \"builtin\"\n",
                "adapter = \"B8:27:EB:12:34:56\"\n",
            );
        let p = plan(&cfg(BASE), &cfg(&edited), &busy());
        assert!(p.is_noop(), "{p:#?}");
    }

    #[test]
    fn default_and_named_adapters_compare_by_mac() {
        let (old, new) = with(|t| t.replace("  adapter = \"builtin\"\n", ""));
        let mac: Mac = "B8:27:EB:12:34:56".parse().unwrap();
        let f = move |a: &AdapterSel| match a {
            AdapterSel::Default => Some(mac),
            AdapterSel::Mac(m) => Some(*m),
            AdapterSel::Hci(_) => None,
        };
        let mut rt = busy();
        rt.adapter_mac = Some(&f);
        assert!(plan(&old, &new, &rt).is_noop());
        // Without knowing the host, a different value is a different adapter.
        let p = plan(&old, &new, &busy());
        assert_eq!(port(&p, "ktbr0", "phones").action, Action::Restarted);
    }

    #[test]
    fn display_only_changes() {
        let (old, new) = with(|t| t.replace("name = \"TH-D75\"", "name = \"My D75\""));
        assert!(plan(&old, &new, &busy()).is_noop());
        let (old, new) = with(|t| {
            t.replace(
                "name = \"Field\"",
                "name = \"Field kit\"\npublic_name = \"Field\"",
            )
        });
        let p = plan(&old, &new, &busy());
        assert_eq!(p.bridges[0].changes, vec!["name"]);
        assert!(!p.disruptive());
        assert_eq!(p.global, Vec::<String>::new());
    }

    #[test]
    fn public_name_is_live_and_not_disruptive() {
        let (old, new) = with(|t| t.replace("name = \"Field\"", "name = \"Portable\""));
        let p = plan(&old, &new, &busy());
        assert_eq!(p.bridges[0].action, Action::Updated);
        assert_eq!(p.bridges[0].changes, vec!["name", "public_name"]);
        assert!(
            p.bridges[0]
                .ports
                .iter()
                .all(|x| x.action == Action::Unchanged)
        );
        assert!(!p.disruptive());
    }

    #[test]
    fn log_level_is_global_and_live() {
        let (old, new) = with(|t| t.replace("\"info\"", "\"debug\""));
        let p = plan(&old, &new, &busy());
        assert_eq!(p.global, vec!["log_level"]);
        assert!(!p.is_noop());
        assert!(!p.disruptive());
    }

    #[test]
    fn tightening_allow_disconnects_who_is_no_longer_allowed() {
        let (old, new) = with(|t| t.replace("192.168.4.0/24", "192.168.4.0/27"));
        let p = plan(&old, &new, &busy());
        let lan = port(&p, "ktbr0", "lan");
        assert_eq!(lan.action, Action::Updated);
        assert_eq!(
            lan.disconnects,
            vec![SessionId::new("lan", "192.168.4.200:5001")]
        );
        assert!(p.disruptive());
        // Loosening it disconnects nobody.
        let (old, new) = with(|t| t.replace("192.168.4.0/24", "192.168.0.0/16"));
        let p = plan(&old, &new, &busy());
        assert_eq!(port(&p, "ktbr0", "lan").action, Action::Updated);
        assert!(!p.disruptive(), "{:?}", p.disruptions());
    }

    #[test]
    fn lowering_max_clients_disconnects_the_newest() {
        let (old, new) = with(|t| {
            t.replace(
                "allow = [\"192.168.4.0/24\"]",
                "allow = [\"192.168.4.0/24\"]\n  max_clients = 1",
            )
        });
        let p = plan(&old, &new, &busy());
        assert_eq!(
            port(&p, "ktbr0", "lan").disconnects,
            vec![
                SessionId::new("lan", "192.168.4.200:5001"),
                SessionId::new("lan", "192.168.4.21:5002"),
            ]
        );
        let (old, new) = with(|t| t.replace("max_clients = 2", "max_clients = 1"));
        let p = plan(&old, &new, &busy());
        assert_eq!(
            port(&p, "ktbr0", "phones").disconnects,
            vec![SessionId::new("phones", "7C:2A:00:00:00:02")]
        );
        // Raising it disconnects nobody.
        let (old, new) = with(|t| t.replace("max_clients = 2", "max_clients = 5"));
        let p = plan(&old, &new, &busy());
        assert_eq!(port(&p, "ktbr0", "phones").action, Action::Updated);
        assert!(!p.disruptive());
    }

    #[test]
    fn announce_connect_idle_timeout_and_app_params_are_live() {
        for edit in [
            "announce = false",
            "connect = \"always\"",
            "idle_timeout = \"90s\"",
            "exit_kiss_on_disconnect = true",
        ] {
            let at = if edit.starts_with("announce") {
                "allow = [\"192.168.4.0/24\"]"
            } else {
                "tnc = \"d75\"\n  [[bridge.port]]\n  id = \"phones\""
            };
            let new_text = if edit.starts_with("announce") {
                BASE.replace(at, &format!("{at}\n  {edit}"))
            } else {
                BASE.replacen(
                    "tnc = \"d75\"\n  [[bridge.port]]\n  id = \"phones\"",
                    &format!("tnc = \"d75\"\n  {edit}\n  [[bridge.port]]\n  id = \"phones\""),
                    1,
                )
            };
            let p = plan(&cfg(BASE), &cfg(&new_text), &busy());
            assert!(!p.is_noop(), "{edit}");
            assert!(!p.disruptive(), "{edit}: {:?}", p.disruptions());
            assert!(
                p.bridges[0]
                    .ports
                    .iter()
                    .all(|x| x.action != Action::Restarted),
                "{edit}"
            );
        }
        let (old, new) = with(|t| {
            t.replace(
                "name = \"Field\"",
                "name = \"Field\"\napp_params = \"drop\"",
            )
        });
        let p = plan(&old, &new, &busy());
        assert_eq!(p.bridges[0].changes, vec!["app_params"]);
        assert!(!p.disruptive());
    }

    #[test]
    fn link_identity_changes_restart_only_that_port() {
        let cases: &[(&str, &str, &str)] = &[
            ("00:11:22:33:44:55", "00:11:22:33:44:66", "radio"),
            ("model = \"th-d75\"", "model = \"generic\"", "radio"),
            (
                "listen = \"0.0.0.0:8001\"\n  allow",
                "listen = \"0.0.0.0:8002\"\n  allow",
                "lan",
            ),
            (
                "adapter = \"builtin\"\n  max_clients",
                "adapter = \"hci1\"\n  max_clients",
                "phones",
            ),
        ];
        for (from, to, which) in cases {
            let new = BASE.replacen(from, to, 1);
            let p = plan(&cfg(BASE), &cfg(&new), &busy());
            for x in &p.bridges[0].ports {
                let want = if x.id == *which {
                    Action::Restarted
                } else {
                    Action::Unchanged
                };
                assert_eq!(x.action, want, "{from} -> {to}: {x:?}");
            }
            assert!(p.disruptive(), "{from} -> {to}");
        }
        // A restarting TNC port keeps app sessions: nothing disconnected.
        let new = BASE.replacen("00:11:22:33:44:55", "00:11:22:33:44:66", 1);
        let p = plan(&cfg(BASE), &cfg(&new), &busy());
        assert!(p.disconnects().is_empty());
        assert!(port(&p, "ktbr0", "radio").drops_tnc_link);
        // ... and an idle one restarting disrupts nobody.
        let p = plan(&cfg(BASE), &cfg(&new), &Runtime::default());
        assert_eq!(port(&p, "ktbr0", "radio").action, Action::Restarted);
        assert!(!p.disruptive());
        // A restarting app port disconnects its own sessions.
        let new = BASE.replacen("0.0.0.0:8001\"\n  allow", "0.0.0.0:8002\"\n  allow", 1);
        let p = plan(&cfg(BASE), &cfg(&new), &busy());
        assert_eq!(port(&p, "ktbr0", "lan").disconnects.len(), 3);
    }

    #[test]
    fn a_changed_tnc_reaches_every_port_using_it() {
        // Editing [tnc.serial] touches only ports that use it: ktbr3's,
        // which is disabled, so nothing happens.
        let new = BASE.replace("usb-JVCKENWOOD_TH-D75-if00", "usb-other-if00");
        assert!(plan(&cfg(BASE), &cfg(&new), &busy()).is_noop());
        // With ktbr3 running instead, its serial port restarts.
        let desk = BASE
            .replacen("name = \"Field\"", "name = \"Field\"\nenabled = false", 1)
            .replace("name = \"Desk\"\nenabled = false", "name = \"Desk\"");
        let new = desk.replace("usb-JVCKENWOOD_TH-D75-if00", "usb-other-if00");
        let p = plan(&cfg(&desk), &cfg(&new), &Runtime::default());
        assert_eq!(port(&p, "ktbr3", "radio").action, Action::Restarted);
        assert_eq!(port(&p, "ktbr3", "radio").changes, vec!["serial"]);
        assert_eq!(port(&p, "ktbr3", "lan").action, Action::Unchanged);
    }

    #[test]
    fn ports_added_removed_and_ids_changed() {
        let new = BASE.replacen("id = \"lan\"", "id = \"wifi\"", 1);
        let p = plan(&cfg(BASE), &cfg(&new), &busy());
        assert_eq!(port(&p, "ktbr0", "lan").action, Action::Stopped);
        assert_eq!(port(&p, "ktbr0", "lan").disconnects.len(), 3);
        assert_eq!(port(&p, "ktbr0", "wifi").action, Action::Started);
        assert!(p.disruptive());
        // Removing an idle port disrupts nobody.
        let p = plan(&cfg(BASE), &cfg(&new), &Runtime::default());
        assert!(!p.disruptive());
    }

    #[test]
    fn enabling_and_disabling_start_and_stop_bridges() {
        // The switch: disable ktbr0, enable ktbr3, in one reload.
        let new = BASE
            .replacen("name = \"Field\"", "name = \"Field\"\nenabled = false", 1)
            .replace("name = \"Desk\"\nenabled = false", "name = \"Desk\"");
        let p = plan(&cfg(BASE), &cfg(&new), &busy());
        assert_eq!(p.bridges[0].id, "ktbr0", "stops come first");
        assert_eq!(p.bridges[0].action, Action::Stopped);
        assert!(p.bridges[0].stops_in_use);
        assert_eq!(p.disconnects().len(), 5);
        assert_eq!(p.bridges[1].id, "ktbr3");
        assert_eq!(p.bridges[1].action, Action::Started);
        assert!(
            p.bridges[1]
                .ports
                .iter()
                .all(|x| x.action == Action::Started)
        );
        assert!(p.disruptive());
        // The same with nothing in use isn't disruptive.
        let p = plan(&cfg(BASE), &cfg(&new), &Runtime::default());
        assert!(!p.disruptive(), "{:?}", p.disruptions());
        // A bridge id change is one bridge stopping and another starting.
        let new = BASE.replacen("id = \"ktbr0\"", "id = \"ktbr9\"", 1);
        let p = plan(&cfg(BASE), &cfg(&new), &busy());
        let actions: Vec<_> = p
            .bridges
            .iter()
            .map(|b| (b.id.as_str(), b.action))
            .collect();
        assert_eq!(
            actions,
            vec![("ktbr0", Action::Stopped), ("ktbr9", Action::Started)]
        );
    }
}
