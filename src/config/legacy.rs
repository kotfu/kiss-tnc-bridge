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

//! The 0.5 per-TNC settings, kept only for the old BLE bridge manager
//! until the Phase 1 bridge core replaces it. The daemon derives these from
//! the real config at startup.

#[derive(Debug, Clone)]
pub struct TncConfig {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub max_clients: usize,
    pub adapter: Option<String>,
}
