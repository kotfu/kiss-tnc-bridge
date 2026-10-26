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

//! `ktbctl`'s command line.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "ktbctl",
    version,
    about = "Manage kiss-tnc-bridge through its control socket",
    after_help = "Bridges and TNCs can be named by id or by name. Commands that would \
                  disconnect someone ask first in a terminal, and are refused without one \
                  unless given --yes."
)]
pub struct Cli {
    /// The daemon's control socket [env: KTBCTL_SOCKET] [default: /run/kiss-tnc-bridge/control.sock]
    #[arg(long, global = true, value_name = "PATH")]
    pub socket: Option<PathBuf>,
    /// Print the daemon's results as JSON, for scripts
    #[arg(long, global = true)]
    pub json: bool,
    /// Go ahead with changes that would disconnect someone, without asking
    #[arg(long, global = true, conflicts_with = "non_disruptive")]
    pub yes: bool,
    /// Refuse changes that would disconnect someone, without asking
    #[arg(long, global = true)]
    pub non_disruptive: bool,
    #[command(subcommand)]
    pub command: Option<Cmd>,
}

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Bridges, ports, sessions, TNCs and adapters
    Status {
        /// Only this bridge
        bridge: Option<String>,
    },
    /// Bluetooth adapters: those present and those configured
    #[command(subcommand)]
    Adapter(AdapterCmd),
    /// TNCs
    #[command(subcommand)]
    Tnc(TncCmd),
    /// Serial devices, by stable path
    #[command(subcommand)]
    Serial(SerialCmd),
    /// Look for Bluetooth radios nearby (they must be in pairing mode)
    Scan {
        /// The adapter to scan on (id, MAC or hciN) [default: the default adapter]
        #[arg(long)]
        adapter: Option<String>,
        /// How long to look
        #[arg(long, default_value_t = 10)]
        seconds: u64,
    },
    /// Pair with a radio over Bluetooth, and trust it
    Pair(BtWho),
    /// Forget a Bluetooth pairing
    Unpair(BtWho),
    /// Bridges
    #[command(subcommand)]
    Bridge(BridgeCmd),
    /// Enable a bridge
    Enable {
        bridge: String,
        /// On a static bridge: drop the override and go back to what its file says
        #[arg(long)]
        reset: bool,
    },
    /// Disable a bridge
    Disable { bridge: String },
    /// Enable a bridge and disable the enabled bridges in its way
    Switch { bridge: String },
    /// Ports of a bridge
    #[command(subcommand)]
    Port(PortCmd),
    /// Bring a TNC link up now and keep it up
    Connect {
        /// BRIDGE/PORT
        port: String,
    },
    /// Take a TNC link down and keep it down (apps stay connected)
    Disconnect {
        /// BRIDGE/PORT
        port: String,
    },
    /// Disconnect one session
    Kick {
        /// BRIDGE/PORT/SESSION, as status shows it
        session: String,
    },
    /// Edit dynamic config in $EDITOR: all of it, or one bridge, TNC or adapter
    Edit {
        /// A bridge, or `tnc` or `adapter` followed by its id
        #[arg(num_args = 0..=2, value_name = "BRIDGE | tnc TNC | adapter ADAPTER")]
        what: Vec<String>,
    },
    /// Live frames to and from the TNCs
    Monitor {
        /// Only this bridge's frames (by id)
        bridge: Option<String>,
    },
    /// Check the config on disk and show what a reload would do
    Check,
    /// Re-read the config and apply what changed
    Reload,
}

#[derive(Debug, Subcommand)]
pub enum AdapterCmd {
    /// Adapters present on this host, and adapter objects
    List,
    /// Give an adapter's MAC an id
    Add {
        id: String,
        #[arg(long)]
        mac: String,
    },
    /// Remove an adapter object
    Rm { adapter: String },
}

#[derive(Debug, Subcommand)]
pub enum TncCmd {
    /// TNCs, and whether they're paired or present
    List,
    /// The TNC models the daemon knows and what each does
    Models,
    /// Add a TNC: Bluetooth (--spp-address), serial (--serial), or both
    Add(TncAdd),
    /// Remove a TNC no port uses
    Rm { tnc: String },
}

#[derive(Debug, Args)]
pub struct TncAdd {
    pub id: String,
    /// See `ktbctl tnc models` [default: generic]
    #[arg(long)]
    pub model: Option<String>,
    /// A display name [default: the id]
    #[arg(long)]
    pub name: Option<String>,
    /// Its Bluetooth address, for an spp-client port
    #[arg(long, value_name = "ADDR")]
    pub spp_address: Option<String>,
    /// The adapter it's paired on (id, MAC or hciN) [default: the default adapter]
    #[arg(long, requires = "spp_address")]
    pub adapter: Option<String>,
    /// Its serial device, for a serial port: a /dev/serial/by-id/ path
    #[arg(long, value_name = "PATH")]
    pub serial: Option<String>,
    /// Serial baud rate [default: 9600]
    #[arg(long, requires = "serial")]
    pub baud: Option<u32>,
}

/// A Bluetooth device: a TNC's radio, or an address.
#[derive(Debug, Args)]
pub struct BtWho {
    /// A TNC with a Bluetooth address (spp-address)
    #[arg(required_unless_present = "address", conflicts_with = "address")]
    pub tnc: Option<String>,
    /// A Bluetooth address, instead of a TNC
    #[arg(long, value_name = "ADDR")]
    pub address: Option<String>,
    /// With --address: the adapter (id, MAC or hciN) [default: the default adapter]
    #[arg(long, requires = "address")]
    pub adapter: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum SerialCmd {
    /// Serial devices present, with known TNCs flagged
    List,
}

#[derive(Debug, Subcommand)]
pub enum BridgeCmd {
    /// Add a bridge, disabled; add its ports, then enable it
    Add {
        /// [default: the lowest free ktbrN]
        id: Option<String>,
        #[arg(long)]
        name: String,
    },
    /// Copy a bridge into a new dynamic one, disabled
    Clone {
        bridge: String,
        new_id: Option<String>,
        #[arg(long)]
        name: String,
    },
    /// Remove a dynamic bridge
    Rm { bridge: String },
    /// Rename a dynamic bridge
    Name { bridge: String, name: String },
    /// Show a bridge's public name, or set it
    PublicName {
        bridge: String,
        /// The new public name (dynamic bridges only)
        name: Option<String>,
        /// Go back to the bridge's name (dynamic bridges only)
        #[arg(long, conflicts_with_all = ["name", "retry"])]
        default: bool,
        /// Drop a DNS-SD collision adjustment and try the configured name again
        #[arg(long, conflicts_with = "name")]
        retry: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum PortCmd {
    /// Add a port to a dynamic bridge
    Add(Box<PortAdd>),
    /// Remove a port from a dynamic bridge
    Rm {
        /// BRIDGE/PORT
        port: String,
    },
}

#[derive(Debug, Args)]
#[command(after_help = "Examples:\n  \
    ktbctl port add ktbr0 spp-client --tnc d75 --idle-timeout 90s\n  \
    ktbctl port add ktbr3 serial --tnc d75\n  \
    ktbctl port add ktbr0 ble-server --adapter dongle --max-clients 2\n  \
    ktbctl port add ktbr0 tcp-server --listen wlan0:8001 --allow 192.168.1.0/24\n  \
    ktbctl port add ktbr1 tcp-client --address 127.0.0.1:6700")]
pub struct PortAdd {
    pub bridge: String,
    /// spp-client, serial, tcp-client, ble-server or tcp-server
    #[arg(value_name = "TYPE")]
    pub kind: String,
    /// [default: the type and a number, e.g. tcp-server0]
    #[arg(long)]
    pub id: Option<String>,
    /// TNC ports: the TNC (spp-client, serial)
    #[arg(long)]
    pub tnc: Option<String>,
    /// tcp-client: host:port of a TCP KISS TNC
    #[arg(long)]
    pub address: Option<String>,
    /// TNC ports: always, or on_demand (connect while apps are connected)
    #[arg(long)]
    pub connect: Option<String>,
    /// TNC ports: with on_demand, how long to stay connected after the last app leaves (90s, 5m)
    #[arg(long, value_name = "DURATION")]
    pub idle_timeout: Option<String>,
    /// TNC ports: take the TNC out of KISS mode when letting go
    #[arg(long)]
    pub exit_kiss_on_disconnect: bool,
    /// ble-server: the adapter to advertise on
    #[arg(long)]
    pub adapter: Option<String>,
    /// App ports: how many apps at once
    #[arg(long)]
    pub max_clients: Option<u32>,
    /// tcp-server: port, address:port, or interface:port
    #[arg(long)]
    pub listen: Option<String>,
    /// tcp-server: addresses or networks allowed to connect (repeatable)
    #[arg(long, value_name = "CIDR")]
    pub allow: Vec<String>,
    /// tcp-server: don't announce it with DNS-SD
    #[arg(long)]
    pub no_announce: bool,
    /// tnc or app, to use a port type the other way round
    #[arg(long)]
    pub role: Option<String>,
}
