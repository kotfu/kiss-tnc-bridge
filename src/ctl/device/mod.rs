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

//! ESP32 devices (the ATOM Lite and Core2) on USB serial: finding them,
//! opening their port without resetting them, asking what they run, and
//! installing firmware on them (docs/plan-phase-d.md, D1). `ktbctl device`
//! talks to them directly, with no daemon.

mod cmd;
pub mod flash;
pub mod manifest;
pub mod ports;
pub mod probe;
mod release;
pub mod rom;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// `device.info`'s result (docs/control-api.md, Devices). Fields this
/// version doesn't know are kept in `other`, so `--json` shows everything
/// the device said.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceInfo {
    /// `atom-lite`, `core2` or `stickc-plus-se`.
    pub board: String,
    pub firmware: Firmware,
    pub protocol: u32,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bluetooth: Option<Bluetooth>,
    /// Seconds since it last started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uptime: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heap: Option<Heap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reset: Option<String>,
    #[serde(default)]
    pub errors: Vec<DeviceError>,
    #[serde(flatten)]
    pub other: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Firmware {
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub built: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition: Option<String>,
    /// `valid`, or `pending_verify` on a new image's first boot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// The version that was rolled back from, if the last update was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rolled_back: Option<String>,
    #[serde(flatten)]
    pub other: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bluetooth {
    pub address: String,
}

/// Internal RAM, in bytes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Heap {
    pub free: u64,
    pub min: u64,
    pub largest: u64,
}

/// Something the device can't fix by retrying.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceError {
    pub code: String,
    pub message: String,
}

/// A likely port and what answered on it, for `ktbctl device list`.
#[derive(Debug, Serialize)]
pub struct Found {
    #[serde(flatten)]
    pub port: ports::DevicePort,
    /// What the firmware said; absent if nothing answered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub info: Option<DeviceInfo>,
    /// Why the port couldn't be asked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A board's name for people: `ATOM Lite` for `atom-lite`.
pub fn board_name(board: &str) -> &str {
    match board {
        "atom-lite" => "ATOM Lite",
        "core2" => "Core2",
        "stickc-plus-se" => "StickC Plus SE",
        other => other,
    }
}
