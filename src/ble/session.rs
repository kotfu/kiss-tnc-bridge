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

use bluer::Address;

use crate::kiss::buffer::KissReassemblyBuffer;

pub struct BleClientSession {
    #[allow(dead_code)]
    pub address: Address,
    pub kiss_buffer: KissReassemblyBuffer,
}

impl BleClientSession {
    pub fn new(address: Address) -> Self {
        Self {
            address,
            kiss_buffer: KissReassemblyBuffer::new(4096),
        }
    }
}
