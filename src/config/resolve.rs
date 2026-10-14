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

use super::model::{AdapterDef, AppParams, ConfigFile, ConnectPolicy, PortDef, Role, TncDef};
use super::{Problem, Source};

pub const DEFAULT_LOG_LEVEL: &str = "info";
pub const DEFAULT_CONTROL_GROUP: &str = "kiss-tnc-bridge";
pub const DEFAULT_BLE_MAX_CLIENTS: u32 = 1;
pub const DEFAULT_SERIAL_BAUD: u32 = 9600;
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

/// What a TNC gets beyond plain KISS: commands at connect and teardown,
/// and checks that need the TNC to answer them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TncModel {
    /// Any TNC that's always in KISS mode. Nothing is sent to it.
    Generic,
    /// Kenwood TH-D74 and TH-D75: the same command set, so one profile.
    ThD7x,
}

/// One `model` a config can name, and what it does.
pub struct ModelInfo {
    pub model: TncModel,
    /// What to write in the config; more than one for models that share a
    /// profile.
    pub names: &'static [&'static str],
    pub summary: &'static str,
}

/// Every model, in the order to list them. The config error, `-t` and the
/// docs take them from here, and so will `ktbctl`.
pub const MODELS: &[ModelInfo] = &[
    ModelInfo {
        model: TncModel::Generic,
        names: &["generic"],
        summary: "the default: any TNC that's always in KISS mode; nothing is sent to it",
    },
    ModelInfo {
        model: TncModel::ThD7x,
        names: &["th-d74", "th-d75"],
        summary: "Kenwood TH-D74/D75: switched to KISS mode at every connect, checked for KISS on \
                  the right interface (menu 983), reconnected if it stops answering, and with \
                  exit_kiss_on_disconnect put back in the mode it was found in",
    },
];

impl TncModel {
    pub fn info(self) -> &'static ModelInfo {
        MODELS
            .iter()
            .find(|m| m.model == self)
            .expect("every model is in MODELS")
    }
}

impl FromStr for TncModel {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Some(m) = MODELS.iter().find(|m| m.names.contains(&s)) {
            return Ok(m.model);
        }
        let mut msg = format!("model {s:?} isn't a known model. The models are:");
        for m in MODELS {
            msg.push_str(&format!("\n  {:<16} {}", m.names.join(", "), m.summary));
        }
        msg.push_str("\nFor a TNC that isn't listed, use generic (or leave model out).");
        Err(msg)
    }
}

impl fmt::Display for TncModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.info().names.join("/"))
    }
}

// ── Effective config ────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub global: Global,
    pub control: Control,
    pub adapters: Vec<Adapter>,
    pub tncs: Vec<Tnc>,
    pub bridges: Vec<Bridge>,
}

impl Config {
    pub fn tnc(&self, id: &str) -> Option<&Tnc> {
        self.tncs.iter().find(|t| t.id == id)
    }

    /// An adapter as the user named it: `builtin (B8:27:EB:…)` when an
    /// `[[adapter]]` id covers the MAC, otherwise the MAC or `hciN` itself.
    pub fn describe_adapter(&self, sel: &AdapterSel) -> String {
        if let AdapterSel::Mac(mac) = sel
            && let Some(a) = self.adapters.iter().find(|a| a.mac == *mac)
        {
            return format!("{} ({mac})", a.id);
        }
        sel.to_string()
    }
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

/// A hardware KISS TNC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tnc {
    pub id: String,
    pub name: String,
    pub model: TncModel,
    pub spp: Option<Spp>,
    pub serial: Option<SerialDevice>,
    pub source: Source,
}

/// How to reach a TNC over Bluetooth Classic SPP.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Spp {
    pub address: Mac,
    /// The adapter it's paired on, after the adapter fallback.
    pub adapter: AdapterSel,
    /// `None`: found by SDP through BlueZ.
    pub channel: Option<u8>,
}

/// How to reach a TNC over a serial device.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SerialDevice {
    pub path: String,
    pub baud: u32,
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
            PortKind::SppClient { .. } => "spp-client",
            PortKind::Serial { .. } => "serial",
        }
    }

    /// The `[[tnc]]` this port uses, if any.
    pub fn tnc_id(&self) -> Option<&str> {
        match &self.kind {
            PortKind::SppClient { tnc, .. } | PortKind::Serial { tnc, .. } => Some(tnc),
            _ => None,
        }
    }

    /// `connect` and `idle_timeout`, for the port types that have them.
    pub fn connect_policy(&self) -> Option<(ConnectPolicy, Duration)> {
        match &self.kind {
            PortKind::TcpClient {
                connect,
                idle_timeout,
                ..
            }
            | PortKind::SppClient {
                connect,
                idle_timeout,
                ..
            }
            | PortKind::Serial {
                connect,
                idle_timeout,
                ..
            } => Some((*connect, *idle_timeout)),
            PortKind::TcpServer { .. } | PortKind::BleServer { .. } => None,
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
    /// The TNC's settings are copied in, so a change to the `[[tnc]]` is a
    /// change to every port that uses it.
    SppClient {
        tnc: String,
        model: TncModel,
        spp: Spp,
        connect: ConnectPolicy,
        idle_timeout: Duration,
        exit_kiss_on_disconnect: bool,
    },
    Serial {
        tnc: String,
        model: TncModel,
        serial: SerialDevice,
        connect: ConnectPolicy,
        idle_timeout: Duration,
        exit_kiss_on_disconnect: bool,
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

    let mut tncs: Vec<Tnc> = Vec::new();
    for (source, file) in files {
        for def in &file.tnc {
            let object = format!("tnc {}", def.id);
            let spp_adapter = def.spp.as_ref().map(|spp| match &spp.adapter {
                Some(a) => resolve_adapter_value(a, source, &object, &mut errors),
                None => global_sel.clone(),
            });
            let (tnc, mut problems) = resolve_tnc(def, source, spp_adapter);
            tncs.push(tnc);
            errors.append(&mut problems);
        }
    }

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
                        let idle_timeout =
                            resolve_idle_timeout(idle_timeout, source, &pobject, &mut errors);
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
                    PortDef::SppClient {
                        tnc,
                        connect,
                        idle_timeout,
                        exit_kiss_on_disconnect,
                        ..
                    } => {
                        let idle_timeout =
                            resolve_idle_timeout(idle_timeout, source, &pobject, &mut errors);
                        let found = tncs.iter().find(|t| t.id == *tnc);
                        match found.map(|t| (t, &t.spp)) {
                            Some((t, Some(spp))) => PortKind::SppClient {
                                tnc: t.id.clone(),
                                model: t.model,
                                spp: spp.clone(),
                                connect: connect.unwrap_or_default(),
                                idle_timeout,
                                exit_kiss_on_disconnect: exit_kiss_on_disconnect.unwrap_or(false),
                            },
                            other => {
                                errors.push(Problem::new(
                                    Some(source.clone()),
                                    &pobject,
                                    tnc_reference_error(tnc, other.is_some(), "spp"),
                                ));
                                continue;
                            }
                        }
                    }
                    PortDef::Serial {
                        tnc,
                        connect,
                        idle_timeout,
                        exit_kiss_on_disconnect,
                        ..
                    } => {
                        let idle_timeout =
                            resolve_idle_timeout(idle_timeout, source, &pobject, &mut errors);
                        let found = tncs.iter().find(|t| t.id == *tnc);
                        match found.map(|t| (t, &t.serial)) {
                            Some((t, Some(serial))) => PortKind::Serial {
                                tnc: t.id.clone(),
                                model: t.model,
                                serial: serial.clone(),
                                connect: connect.unwrap_or_default(),
                                idle_timeout,
                                exit_kiss_on_disconnect: exit_kiss_on_disconnect.unwrap_or(false),
                            },
                            other => {
                                errors.push(Problem::new(
                                    Some(source.clone()),
                                    &pobject,
                                    tnc_reference_error(tnc, other.is_some(), "serial"),
                                ));
                                continue;
                            }
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
            tncs,
            bridges,
        })
    } else {
        Err(errors)
    }
}

fn resolve_idle_timeout(
    value: &Option<String>,
    source: &Source,
    object: &str,
    errors: &mut Vec<Problem>,
) -> Duration {
    match value.as_deref().map(parse_duration) {
        Some(Ok(d)) => d,
        Some(Err(e)) => {
            errors.push(Problem::new(
                Some(source.clone()),
                object,
                format!("idle_timeout: {e}"),
            ));
            Duration::ZERO
        }
        None => Duration::ZERO,
    }
}

fn tnc_reference_error(tnc: &str, defined: bool, section: &str) -> String {
    if defined {
        format!("tnc {tnc:?} has no [tnc.{section}] section, which this port type needs")
    } else {
        format!("tnc {tnc:?} is not defined (no [[tnc]] with that id)")
    }
}

/// `spp_adapter` is the `[tnc.spp]` adapter, already resolved with the
/// fallback to `[global] adapter`. The TNC comes back even with errors, so
/// ports that use it resolve and only the TNC's own problems are reported.
fn resolve_tnc(
    def: &TncDef,
    source: &Source,
    spp_adapter: Option<AdapterSel>,
) -> (Tnc, Vec<Problem>) {
    let object = format!("tnc {}", def.id);
    let mut errors = Vec::new();
    let mut err = |msg: String| errors.push(Problem::new(Some(source.clone()), &object, msg));
    if let Err(e) = check_id(&def.id, "tnc") {
        err(e);
    }
    let name = def.name.clone().unwrap_or_else(|| def.id.clone());
    if name.trim().is_empty() {
        err("name must not be empty".into());
    }
    let model = match def.model.as_deref().map(str::parse::<TncModel>) {
        None => TncModel::Generic,
        Some(Ok(m)) => m,
        Some(Err(e)) => {
            err(e);
            TncModel::Generic
        }
    };
    let spp = match &def.spp {
        None => None,
        Some(spp) => {
            let address = match spp.address.parse::<Mac>() {
                Ok(a) => a,
                Err(e) => {
                    err(format!("spp.address: {e}"));
                    Mac([0; 6])
                }
            };
            if let Some(ch) = spp.channel
                && !(1..=30).contains(&ch)
            {
                err(format!("spp.channel: {ch} is not an RFCOMM channel (1-30)"));
            }
            Some(Spp {
                address,
                adapter: spp_adapter.unwrap_or(AdapterSel::Default),
                channel: spp.channel,
            })
        }
    };
    let serial = match &def.serial {
        None => None,
        Some(serial) => {
            if !serial.path.starts_with('/') {
                err(format!(
                    "serial.path {:?} must be an absolute path, e.g. /dev/serial/by-id/…",
                    serial.path
                ));
            }
            let baud = serial.baud.unwrap_or(DEFAULT_SERIAL_BAUD);
            if baud == 0 {
                err("serial.baud must be more than 0".into());
            }
            Some(SerialDevice {
                path: serial.path.clone(),
                baud,
            })
        }
    };
    if spp.is_none() && serial.is_none() {
        err("a tnc needs at least one of [tnc.spp] or [tnc.serial]".into());
    }
    let tnc = Tnc {
        id: def.id.clone(),
        name,
        model,
        spp,
        serial,
        source: source.clone(),
    };
    (tnc, errors)
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

    fn resolve_text(text: &str) -> Result<Config, Vec<String>> {
        let file = crate::config::load::parse_str(text).unwrap();
        resolve(&[(Source::Static("test.toml".into()), file)])
            .map_err(|e| e.iter().map(|p| p.to_string()).collect())
    }

    const D75: &str = r#"
[global]
adapter = "builtin"

[[adapter]]
id = "builtin"
mac = "B8:27:EB:12:34:56"

[[tnc]]
id = "d75"
name = "TH-D75"
model = "th-d75"
spp = { address = "00:11:22:33:44:55" }
  [tnc.serial]
  path = "/dev/serial/by-id/usb-JVCKENWOOD_TH-D75-if00"

[[bridge]]
id = "ktbr0"
name = "TH-D75"
  [[bridge.port]]
  type = "spp-client"
  tnc = "d75"
  idle_timeout = "90s"
  [[bridge.port]]
  type = "ble-server"

[[bridge]]
id = "ktbr3"
name = "TH-D75 USB"
enabled = false
  [[bridge.port]]
  id = "radio"
  type = "serial"
  tnc = "d75"
  connect = "always"
  exit_kiss_on_disconnect = true
  [[bridge.port]]
  type = "tcp-server"
  listen = "8001"
"#;

    #[test]
    fn tncs_resolve_and_ports_carry_their_settings() {
        let cfg = resolve_text(D75).unwrap();
        let builtin = AdapterSel::Mac("B8:27:EB:12:34:56".parse().unwrap());
        let tnc = cfg.tnc("d75").unwrap();
        assert_eq!(tnc.name, "TH-D75");
        assert_eq!(tnc.model, TncModel::ThD7x);
        let spp = tnc.spp.clone().unwrap();
        // No [tnc.spp] adapter: [global] adapter applies.
        assert_eq!(spp.adapter, builtin);
        assert_eq!(spp.channel, None);
        assert_eq!(tnc.serial.as_ref().unwrap().baud, DEFAULT_SERIAL_BAUD);

        let p = &cfg.bridges[0].ports[0];
        assert_eq!(p.id, "spp-client0");
        assert_eq!(p.role, Role::Tnc);
        assert_eq!(p.tnc_id(), Some("d75"));
        assert_eq!(
            p.kind,
            PortKind::SppClient {
                tnc: "d75".into(),
                model: TncModel::ThD7x,
                spp,
                connect: ConnectPolicy::OnDemand,
                idle_timeout: Duration::from_secs(90),
                exit_kiss_on_disconnect: false,
            }
        );
        let p = &cfg.bridges[1].ports[0];
        assert!(matches!(
            &p.kind,
            PortKind::Serial { serial, connect: ConnectPolicy::Always, exit_kiss_on_disconnect: true, .. }
                if serial.path == "/dev/serial/by-id/usb-JVCKENWOOD_TH-D75-if00"
        ));
        assert_eq!(
            p.connect_policy(),
            Some((ConnectPolicy::Always, Duration::ZERO))
        );
    }

    #[test]
    fn tnc_models() {
        for m in ["th-d74", "th-d75"] {
            assert_eq!(m.parse::<TncModel>().unwrap(), TncModel::ThD7x);
        }
        assert_eq!("generic".parse::<TncModel>().unwrap(), TncModel::Generic);
        assert_eq!(TncModel::ThD7x.to_string(), "th-d74/th-d75");
        // Unknown names, typos and the old "none" are errors that list
        // every model.
        for bad in ["th-d57", "mobilinkd-tnc3", "none", "th-d7x"] {
            let err = bad.parse::<TncModel>().unwrap_err();
            assert!(err.contains("isn't a known model"), "{err}");
            assert!(
                err.contains("generic") && err.contains("th-d74, th-d75"),
                "{err}"
            );
        }
    }

    #[test]
    fn tnc_errors() {
        let e = resolve_text(&D75.replace("tnc = \"d75\"\n  idle", "tnc = \"d74\"\n  idle"))
            .unwrap_err();
        assert!(
            e.iter().any(|m| m.contains("tnc \"d74\" is not defined")),
            "{e:?}"
        );

        let no_serial = D75.replace(
            "  [tnc.serial]\n  path = \"/dev/serial/by-id/usb-JVCKENWOOD_TH-D75-if00\"\n",
            "",
        );
        let e = resolve_text(&no_serial).unwrap_err();
        assert!(
            e.iter().any(|m| m.contains("has no [tnc.serial] section")),
            "{e:?}"
        );

        // A bad value in a TNC is reported once, not again by every port
        // that uses the TNC.
        let e = resolve_text(&D75.replace("model = \"th-d75\"", "model = \"th-d57\"")).unwrap_err();
        assert_eq!(e.len(), 1, "{e:?}");
        assert!(e[0].contains("isn't a known model"), "{e:?}");

        let e = resolve_text("[[tnc]]\nid = \"x\"\nmodel = \"ic-705\"\n").unwrap_err();
        assert!(e.iter().any(|m| m.contains("ic-705")), "{e:?}");
        assert!(e.iter().any(|m| m.contains("at least one of")), "{e:?}");

        let e = resolve_text(
            "[[tnc]]\nid = \"x\"\nspp = { address = \"00:11\", adapter = \"nope\", channel = 31 }\n",
        )
        .unwrap_err();
        assert!(e.iter().any(|m| m.contains("spp.address")), "{e:?}");
        assert!(e.iter().any(|m| m.contains("adapter \"nope\"")), "{e:?}");
        assert!(e.iter().any(|m| m.contains("spp.channel")), "{e:?}");

        let e = resolve_text("[[tnc]]\nid = \"x\"\nserial = { path = \"ttyACM0\", baud = 0 }\n")
            .unwrap_err();
        assert!(e.iter().any(|m| m.contains("absolute path")), "{e:?}");
        assert!(e.iter().any(|m| m.contains("serial.baud")), "{e:?}");
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
