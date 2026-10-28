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

//! kiss-tnc-bridge: a KISS switchboard. Bridges connect one KISS TNC to
//! any number of apps over BLE and TCP.
//!
//! This library holds everything the daemon, `ktbctl` and tests share.
//! Only the Bluetooth code (behind `cfg(target_os = "linux")`) needs BlueZ;
//! KISS framing, configuration, bridging and TCP build and test anywhere.

pub mod announce;
pub mod ax25;
#[cfg(target_os = "linux")]
pub mod bluetooth;
pub mod bluez_conf;
pub mod bridge;
pub mod bt_hardware;
pub mod config;
pub mod control;
pub mod ctl;
pub mod error;
pub mod kiss;
#[cfg(target_os = "linux")]
pub mod pairing;
pub mod port;
pub mod reconcile;
pub mod supervisor;
