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

//! The effective configuration: files merged, defaults filled in, values
//! parsed, references resolved. This is what the daemon runs from and what
//! reload compares (resolved objects, not file text).

use std::collections::HashMap;
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use std::time::Duration;

use ipnet::IpNet;

use super::model::{AdapterDef, AppParams, ConfigFile, ConnectPolicy, PortDef, Role};
use super::{Problem, Source};

pub const DEFAULT_LOG_LEVEL: &str = "info";
pub const DEFAULT_CONTROL_GROUP: &str = "kiss-tnc-bridge";
pub const DEFAULT_BLE_MAX_CLIENTS: u32 = 1;
/// Longest id for bridges, ports and adapters, same as a Linux interface name.
pub const MAX_ID_LEN: usize = 15;

// ── Value types ─────────────────────────────────────────────────────────

/// A Bluetooth adapter address.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Mac(pub [u8; 6]);

impl FromStr for Mac {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() != 6 {
            return Err(format!(
                "{s:?} is not a MAC address (expected XX:XX:XX:XX:XX:XX)"
            ));
        }
        let mut out = [0u8; 6];
        for (i, p) in parts.iter().enumerate() {
            if p.len() != 2 {
                return Err(format!(
                    "{s:?} is not a MAC address (expected XX:XX:XX:XX:XX:XX)"
                ));
            }
            out[i] = u8::from_str_radix(p, 16)
                .map_err(|_| format!("{s:?} is not a MAC address: bad octet {p:?}"))?;
        }
        Ok(Mac(out))
    }
}

impl fmt::Display for Mac {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = self.0;
        write!(
            f,
            "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
            b[0], b[1], b[2], b[3], b[4], b[5]
        )
    }
}

impl fmt::Debug for Mac {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self}")
    }
}

/// An adapter value as written: an `[[adapter]]` id, a MAC, or `hciN`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdapterValue {
    Id(String),
    Mac(Mac),
    Hci(String),
}

impl AdapterValue {
    pub fn parse(s: &str) -> AdapterValue {
        if let Ok(mac) = s.parse::<Mac>() {
            AdapterValue::Mac(mac)
        } else if is_hci_name(s) {
            AdapterValue::Hci(s.to_string())
        } else {
            AdapterValue::Id(s.to_string())
        }
    }
}

pub fn is_hci_name(s: &str) -> bool {
    s.strip_prefix("hci")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Which adapter a Bluetooth port uses, after ids are resolved.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AdapterSel {
    /// BlueZ's default adapter: the first one present.
    Default,
    Mac(Mac),
    Hci(String),
}

impl fmt::Display for AdapterSel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AdapterSel::Default => f.write_str("default adapter"),
            AdapterSel::Mac(m) => write!(f, "{m}"),
            AdapterSel::Hci(h) => f.write_str(h),
        }
    }
}

/// A `tcp-server` binding.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Listen {
    Addr(SocketAddr),
    Interface { name: String, port: u16 },
}

impl Listen {
    pub fn port(&self) -> u16 {
        match self {
            Listen::Addr(a) => a.port(),
            Listen::Interface { port, .. } => *port,
        }
    }

    pub fn is_loopback(&self) -> bool {
        matches!(self, Listen::Addr(a) if a.ip().is_loopback())
    }

    pub fn is_unspecified(&self) -> bool {
        matches!(self, Listen::Addr(a) if a.ip().is_unspecified())
    }

    /// Whether two enabled listeners would fight over a socket. Interface
    /// bindings are compared by name only; whether an address lives on an
    /// interface is a host question answered at bind time.
    pub fn conflicts_with(&self, other: &Listen) -> bool {
        if self.port() != other.port() {
            return false;
        }
        match (self, other) {
            (Listen::Addr(a), Listen::Addr(b)) => {
                a.ip() == b.ip() || a.ip().is_unspecified() || b.ip().is_unspecified()
            }
            (Listen::Interface { name: a, .. }, Listen::Interface { name: b, .. }) => a == b,
            (Listen::Interface { .. }, Listen::Addr(a))
            | (Listen::Addr(a), Listen::Interface { .. }) => a.ip().is_unspecified(),
        }
    }
}

impl FromStr for Listen {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Ok(port) = s.parse::<u16>() {
            return Ok(Listen::Addr(SocketAddr::from(([127, 0, 0, 1], port))));
        }
        if let Ok(addr) = s.parse::<SocketAddr>() {
            return Ok(Listen::Addr(addr));
        }
        if let Some((name, port)) = s.rsplit_once(':') {
            let port: u16 = port
                .parse()
                .map_err(|_| format!("{s:?}: {port:?} is not a TCP port number"))?;
            if name.is_empty() || name.contains(':') || name.contains('/') {
                return Err(format!(
                    "{s:?} is not a TCP port number, address:port, or interface:port"
                ));
            }
            if let Ok(ip) = name.parse::<IpAddr>() {
                return Ok(Listen::Addr(SocketAddr::new(ip, port)));
            }
            return Ok(Listen::Interface {
                name: name.to_string(),
                port,
            });
        }
        Err(format!(
            "{s:?} is not a TCP port number, address:port, or interface:port"
        ))
    }
}

impl fmt::Display for Listen {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Listen::Addr(a) => write!(f, "{a}"),
            Listen::Interface { name, port } => write!(f, "{name}:{port}"),
        }
    }
}

/// `90s`, `2m`, `1h`, `500ms`, or a bare number of seconds.
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let n: u64 = num
        .parse()
        .map_err(|_| format!("{s:?} is not a duration (e.g. 90s, 2m)"))?;
    match unit.trim() {
        "" | "s" => Ok(Duration::from_secs(n)),
        "ms" => Ok(Duration::from_millis(n)),
        "m" => Ok(Duration::from_secs(n * 60)),
        "h" => Ok(Duration::from_secs(n * 3600)),
        _ => Err(format!("{s:?} is not a duration (use ms, s, m or h)")),
    }
}

/// Ids use lowercase letters, digits and `-`, at most 15 characters.
pub fn check_id(id: &str, what: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err(format!("{what} id is empty"));
    }
    if id.len() > MAX_ID_LEN {
        return Err(format!(
            "{what} id {id:?} is longer than {MAX_ID_LEN} characters"
        ));
    }
    if !id
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(format!(
            "{what} id {id:?} may only use lowercase letters, digits and '-'"
        ));
    }
    Ok(())
}

/// `host:port` for a `tcp-client`. The host may be a name.
pub fn parse_host_port(s: &str) -> Result<(String, u16), String> {
    if let Ok(addr) = s.parse::<SocketAddr>() {
        return Ok((addr.ip().to_string(), addr.port()));
    }
    let (host, port) = s
        .rsplit_once(':')
        .ok_or_else(|| format!("{s:?} is not host:port"))?;
    let port: u16 = port
        .parse()
        .map_err(|_| format!("{s:?}: {port:?} is not a TCP port number"))?;
    if host.is_empty() || host.contains(':') {
        return Err(format!(
            "{s:?} is not host:port (wrap IPv6 addresses in [])"
        ));
    }
    Ok((host.to_string(), port))
}

// ── Effective config ────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub global: Global,
    pub control: Control,
    pub adapters: Vec<Adapter>,
    pub bridges: Vec<Bridge>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Global {
    pub log_level: String,
    pub adapter: AdapterSel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Control {
    pub group: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adapter {
    pub id: String,
    pub mac: Mac,
    pub source: Source,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bridge {
    pub id: String,
    pub name: String,
    /// The BLE advertised name and DNS-SD instance name. Defaults to `name`.
    pub public_name: String,
    pub enabled: bool,
    pub app_params: AppParams,
    pub ports: Vec<Port>,
    pub source: Source,
}

impl Bridge {
    pub fn tnc_ports(&self) -> impl Iterator<Item = &Port> {
        self.ports.iter().filter(|p| p.role == Role::Tnc)
    }

    pub fn app_ports(&self) -> impl Iterator<Item = &Port> {
        self.ports.iter().filter(|p| p.role == Role::App)
    }

    pub fn port(&self, id: &str) -> Option<&Port> {
        self.ports.iter().find(|p| p.id == id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Port {
    pub id: String,
    pub role: Role,
    pub kind: PortKind,
}

impl Port {
    pub fn type_name(&self) -> &'static str {
        match &self.kind {
            PortKind::TcpClient { .. } => "tcp-client",
            PortKind::TcpServer { .. } => "tcp-server",
            PortKind::BleServer { .. } => "ble-server",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortKind {
    TcpClient {
        host: String,
        port: u16,
        connect: ConnectPolicy,
        idle_timeout: Duration,
    },
    TcpServer {
        listen: Listen,
        /// `None`: no limit.
        max_clients: Option<u32>,
        /// Empty: any peer that can reach the socket.
        allow: Vec<IpNet>,
        announce: bool,
    },
    BleServer {
        adapter: AdapterSel,
        max_clients: u32,
    },
}

// ── Resolution ──────────────────────────────────────────────────────────

/// Merge the loaded files into the effective config. Every value error is
/// collected so `-t` can report them all at once.
pub fn resolve(files: &[(Source, ConfigFile)]) -> Result<Config, Vec<Problem>> {
    let mut errors = Vec::new();

    // [global] and [control] come from the main file only (load checks that
    // conf.d files don't carry them).
    let mut log_level = DEFAULT_LOG_LEVEL.to_string();
    let mut global_adapter: Option<(Source, String)> = None;
    let mut control_group = DEFAULT_CONTROL_GROUP.to_string();
    for (source, file) in files {
        if let Some(g) = &file.global {
            if let Some(l) = &g.log_level {
                log_level = l.clone();
            }
            if let Some(a) = &g.adapter {
                global_adapter = Some((source.clone(), a.clone()));
            }
        }
        if let Some(c) = &file.control
            && let Some(g) = &c.group
        {
            control_group = g.clone();
        }
    }

    // Adapters first: everything else may reference them by id.
    let mut adapters: Vec<Adapter> = Vec::new();
    for (source, file) in files {
        for def in &file.adapter {
            match resolve_adapter(def, source) {
                Ok(a) => adapters.push(a),
                Err(p) => errors.push(p),
            }
        }
    }
    let adapter_ids: HashMap<&str, Mac> = adapters.iter().map(|a| (a.id.as_str(), a.mac)).collect();

    let resolve_adapter_value = |value: &str,
                                 source: &Source,
                                 object: &str,
                                 errors: &mut Vec<Problem>|
     -> AdapterSel {
        match AdapterValue::parse(value) {
            AdapterValue::Mac(m) => AdapterSel::Mac(m),
            AdapterValue::Hci(h) => AdapterSel::Hci(h),
            AdapterValue::Id(id) => {
                match adapter_ids.get(id.as_str()) {
                    Some(mac) => AdapterSel::Mac(*mac),
                    None => {
                        errors.push(Problem::new(
                        Some(source.clone()),
                        object,
                        format!("adapter {id:?} is not a MAC, an hciN name, or a defined [[adapter]] id"),
                    ));
                        AdapterSel::Default
                    }
                }
            }
        }
    };

    let global_sel = match &global_adapter {
        Some((source, value)) => {
            resolve_adapter_value(value, source, "[global] adapter", &mut errors)
        }
        None => AdapterSel::Default,
    };

    let mut bridges: Vec<Bridge> = Vec::new();
    for (source, file) in files {
        for def in &file.bridge {
            let object = match &def.id {
                Some(id) => format!("bridge {id}"),
                None => format!("bridge {:?}", def.name),
            };
            let Some(id) = def.id.clone() else {
                errors.push(Problem::new(
                    Some(source.clone()),
                    &object,
                    "a bridge in a config file needs an id (e.g. id = \"ktbr0\")",
                ));
                continue;
            };
            if let Err(e) = check_id(&id, "bridge") {
                errors.push(Problem::new(Some(source.clone()), &object, e));
            }
            if def.name.trim().is_empty() {
                errors.push(Problem::new(
                    Some(source.clone()),
                    &object,
                    "name must not be empty",
                ));
            }
            if def
                .public_name
                .as_deref()
                .is_some_and(|n| n.trim().is_empty())
            {
                errors.push(Problem::new(
                    Some(source.clone()),
                    &object,
                    "public_name must not be empty",
                ));
            }

            let mut ports = Vec::new();
            let mut counts: HashMap<&'static str, usize> = HashMap::new();
            for pdef in &def.ports {
                let n = counts.entry(pdef.type_name()).or_insert(0);
                let pid = match pdef.id() {
                    Some(pid) => pid.to_string(),
                    None => format!("{}{}", pdef.type_name(), n),
                };
                *n += 1;
                let pobject = format!("{object} port {pid}");
                if let Err(e) = check_id(&pid, "port") {
                    errors.push(Problem::new(Some(source.clone()), &pobject, e));
                }
                let role = pdef.role().unwrap_or_else(|| pdef.default_role());
                let kind = match pdef {
                    PortDef::TcpClient {
                        address,
                        connect,
                        idle_timeout,
                        ..
                    } => {
                        let (host, port) = match parse_host_port(address) {
                            Ok(hp) => hp,
                            Err(e) => {
                                errors.push(Problem::new(
                                    Some(source.clone()),
                                    &pobject,
                                    format!("address: {e}"),
                                ));
                                (String::new(), 0)
                            }
                        };
                        let idle_timeout = match idle_timeout.as_deref().map(parse_duration) {
                            Some(Ok(d)) => d,
                            Some(Err(e)) => {
                                errors.push(Problem::new(
                                    Some(source.clone()),
                                    &pobject,
                                    format!("idle_timeout: {e}"),
                                ));
                                Duration::ZERO
                            }
                            None => Duration::ZERO,
                        };
                        PortKind::TcpClient {
                            host,
                            port,
                            connect: connect.unwrap_or_default(),
                            idle_timeout,
                        }
                    }
                    PortDef::TcpServer {
                        listen,
                        max_clients,
                        allow,
                        announce,
                        ..
                    } => {
                        let listen = match listen.parse::<Listen>() {
                            Ok(l) => l,
                            Err(e) => {
                                errors.push(Problem::new(
                                    Some(source.clone()),
                                    &pobject,
                                    format!("listen: {e}"),
                                ));
                                Listen::Addr(SocketAddr::from(([127, 0, 0, 1], 0)))
                            }
                        };
                        if *max_clients == Some(0) {
                            errors.push(Problem::new(
                                Some(source.clone()),
                                &pobject,
                                "max_clients must be at least 1 (leave it out for no limit)",
                            ));
                        }
                        let mut nets = Vec::new();
                        for a in allow.iter().flatten() {
                            match a
                                .parse::<IpNet>()
                                .or_else(|_| a.parse::<IpAddr>().map(IpNet::from))
                            {
                                Ok(net) => nets.push(net),
                                Err(_) => errors.push(Problem::new(
                                    Some(source.clone()),
                                    &pobject,
                                    format!("allow: {a:?} is not an address or CIDR network"),
                                )),
                            }
                        }
                        PortKind::TcpServer {
                            announce: announce.unwrap_or(!listen.is_loopback()),
                            listen,
                            max_clients: *max_clients,
                            allow: nets,
                        }
                    }
                    PortDef::BleServer {
                        adapter,
                        max_clients,
                        ..
                    } => {
                        let adapter = match adapter {
                            Some(a) => resolve_adapter_value(a, source, &pobject, &mut errors),
                            None => global_sel.clone(),
                        };
                        let max_clients = max_clients.unwrap_or(DEFAULT_BLE_MAX_CLIENTS);
                        if max_clients == 0 {
                            errors.push(Problem::new(
                                Some(source.clone()),
                                &pobject,
                                "max_clients must be at least 1",
                            ));
                        }
                        PortKind::BleServer {
                            adapter,
                            max_clients,
                        }
                    }
                };
                ports.push(Port {
                    id: pid,
                    role,
                    kind,
                });
            }

            bridges.push(Bridge {
                id,
                name: def.name.clone(),
                public_name: def.public_name.clone().unwrap_or_else(|| def.name.clone()),
                enabled: def.enabled.unwrap_or(true),
                app_params: def.app_params.unwrap_or_default(),
                ports,
                source: source.clone(),
            });
        }
    }

    if errors.is_empty() {
        Ok(Config {
            global: Global {
                log_level,
                adapter: global_sel,
            },
            control: Control {
                group: control_group,
            },
            adapters,
            bridges,
        })
    } else {
        Err(errors)
    }
}

fn resolve_adapter(def: &AdapterDef, source: &Source) -> Result<Adapter, Problem> {
    let object = format!("adapter {}", def.id);
    check_id(&def.id, "adapter").map_err(|e| Problem::new(Some(source.clone()), &object, e))?;
    if is_hci_name(&def.id) || def.id.parse::<Mac>().is_ok() {
        return Err(Problem::new(
            Some(source.clone()),
            &object,
            "an adapter id can't look like an hciN name or a MAC address",
        ));
    }
    let mac = def
        .mac
        .parse::<Mac>()
        .map_err(|e| Problem::new(Some(source.clone()), &object, format!("mac: {e}")))?;
    Ok(Adapter {
        id: def.id.clone(),
        mac,
        source: source.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_round_trips() {
        let m: Mac = "b8:27:eb:12:34:56".parse().unwrap();
        assert_eq!(m.to_string(), "B8:27:EB:12:34:56");
        assert!("b8:27:eb:12:34".parse::<Mac>().is_err());
        assert!("zz:27:eb:12:34:56".parse::<Mac>().is_err());
    }

    #[test]
    fn adapter_values() {
        assert_eq!(
            AdapterValue::parse("hci1"),
            AdapterValue::Hci("hci1".into())
        );
        assert_eq!(
            AdapterValue::parse("dongle"),
            AdapterValue::Id("dongle".into())
        );
        assert!(matches!(
            AdapterValue::parse("00:1A:7D:DA:71:13"),
            AdapterValue::Mac(_)
        ));
        assert_eq!(AdapterValue::parse("hci"), AdapterValue::Id("hci".into()));
    }

    #[test]
    fn listen_forms() {
        assert_eq!(
            "8001".parse::<Listen>().unwrap(),
            Listen::Addr("127.0.0.1:8001".parse().unwrap())
        );
        assert_eq!(
            "0.0.0.0:8001".parse::<Listen>().unwrap(),
            Listen::Addr("0.0.0.0:8001".parse().unwrap())
        );
        assert_eq!(
            "[::1]:8001".parse::<Listen>().unwrap(),
            Listen::Addr("[::1]:8001".parse().unwrap())
        );
        assert_eq!(
            "wlan0:8001".parse::<Listen>().unwrap(),
            Listen::Interface {
                name: "wlan0".into(),
                port: 8001
            }
        );
        assert!("wlan0".parse::<Listen>().is_err());
        assert!("wlan0:x".parse::<Listen>().is_err());
        assert!("70000".parse::<Listen>().is_err());
        assert!("8001".parse::<Listen>().unwrap().is_loopback());
        assert!("0.0.0.0:8001".parse::<Listen>().unwrap().is_unspecified());
    }

    #[test]
    fn listen_conflicts() {
        let l = |s: &str| s.parse::<Listen>().unwrap();
        assert!(l("8001").conflicts_with(&l("127.0.0.1:8001")));
        assert!(l("8001").conflicts_with(&l("0.0.0.0:8001")));
        assert!(!l("8001").conflicts_with(&l("8002")));
        assert!(!l("8001").conflicts_with(&l("192.168.4.1:8001")));
        assert!(l("wlan0:8001").conflicts_with(&l("wlan0:8001")));
        assert!(!l("wlan0:8001").conflicts_with(&l("eth0:8001")));
        assert!(l("wlan0:8001").conflicts_with(&l("0.0.0.0:8001")));
        assert!(!l("wlan0:8001").conflicts_with(&l("8001")));
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("90s").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_duration("2m").unwrap(), Duration::from_secs(120));
        assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        assert_eq!(parse_duration("5").unwrap(), Duration::from_secs(5));
        assert_eq!(parse_duration("0s").unwrap(), Duration::ZERO);
        assert!(parse_duration("abc").is_err());
        assert!(parse_duration("5d").is_err());
    }

    #[test]
    fn ids() {
        assert!(check_id("ktbr0", "bridge").is_ok());
        assert!(check_id("th-d75", "bridge").is_ok());
        assert!(check_id("Ktbr0", "bridge").is_err());
        assert!(check_id("a-very-long-bridge-id", "bridge").is_err());
        assert!(check_id("", "bridge").is_err());
        assert!(check_id("with space", "bridge").is_err());
    }

    #[test]
    fn host_port() {
        assert_eq!(
            parse_host_port("127.0.0.1:6700").unwrap(),
            ("127.0.0.1".into(), 6700)
        );
        assert_eq!(
            parse_host_port("direwolf.local:8001").unwrap(),
            ("direwolf.local".into(), 8001)
        );
        assert_eq!(parse_host_port("[::1]:8001").unwrap(), ("::1".into(), 8001));
        assert!(parse_host_port("direwolf.local").is_err());
        assert!(parse_host_port(":8001").is_err());
    }
}
