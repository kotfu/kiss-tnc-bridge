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

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("config error: {0}")]
    Config(String),
    #[cfg(target_os = "linux")]
    #[error("BLE error: {0}")]
    Ble(#[from] bluer::Error),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("TCP connection failed: {0}")]
    TcpConnect(String),
    #[error("KISS frame error: {0}")]
    KissFrame(String),
}
