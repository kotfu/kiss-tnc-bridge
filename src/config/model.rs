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

//! The TOML schema, as written in files. Everything optional here gets a
//! default in [`resolve`](super::resolve); this is the shape serde reads.

use serde::Deserialize;

/// One TOML file: the main file or one from `conf.d/`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    pub global: Option<GlobalSection>,
    pub control: Option<ControlSection>,
    #[serde(default)]
    pub adapter: Vec<AdapterDef>,
    #[serde(default)]
    pub bridge: Vec<BridgeDef>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobalSection {
    pub log_level: Option<String>,
    /// Default adapter for Bluetooth ports: an adapter id, a MAC, or `hciN`.
    pub adapter: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlSection {
    pub group: Option<String>,
}

/// `[[adapter]]`: a readable id for a Bluetooth adapter's MAC.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterDef {
    pub id: String,
    pub mac: String,
}

/// `[[bridge]]` with its inline `[[bridge.port]]`s.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BridgeDef {
    pub id: Option<String>,
    pub name: String,
    pub enabled: Option<bool>,
    pub public_name: Option<String>,
    pub app_params: Option<AppParams>,
    #[serde(default, rename = "port")]
    pub ports: Vec<PortDef>,
}

/// Whether KISS parameter commands from apps reach the TNC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppParams {
    #[default]
    Pass,
    Drop,
}

/// A port is either where the TNC is, or where apps attach.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Tnc,
    App,
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Role::Tnc => "tnc",
            Role::App => "app",
        })
    }
}

/// When a TNC port connects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectPolicy {
    #[default]
    OnDemand,
    Always,
}

/// `[[bridge.port]]`, discriminated by `type`.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum PortDef {
    TcpClient {
        id: Option<String>,
        role: Option<Role>,
        /// `host:port` of a KISS TCP server (Dire Wolf, Graywolf, …).
        address: String,
        connect: Option<ConnectPolicy>,
        idle_timeout: Option<String>,
    },
    TcpServer {
        id: Option<String>,
        role: Option<Role>,
        /// A TCP port number, `address:port`, or `interface:port`.
        listen: String,
        max_clients: Option<u32>,
        allow: Option<Vec<String>>,
        announce: Option<bool>,
    },
    BleServer {
        id: Option<String>,
        role: Option<Role>,
        adapter: Option<String>,
        max_clients: Option<u32>,
    },
}

impl PortDef {
    pub fn type_name(&self) -> &'static str {
        match self {
            PortDef::TcpClient { .. } => "tcp-client",
            PortDef::TcpServer { .. } => "tcp-server",
            PortDef::BleServer { .. } => "ble-server",
        }
    }

    pub fn id(&self) -> Option<&str> {
        match self {
            PortDef::TcpClient { id, .. }
            | PortDef::TcpServer { id, .. }
            | PortDef::BleServer { id, .. } => id.as_deref(),
        }
    }

    pub fn role(&self) -> Option<Role> {
        match self {
            PortDef::TcpClient { role, .. }
            | PortDef::TcpServer { role, .. }
            | PortDef::BleServer { role, .. } => *role,
        }
    }

    /// The role a port type has unless `role` overrides it.
    pub fn default_role(&self) -> Role {
        match self {
            PortDef::TcpClient { .. } => Role::Tnc,
            PortDef::TcpServer { .. } | PortDef::BleServer { .. } => Role::App,
        }
    }
}
