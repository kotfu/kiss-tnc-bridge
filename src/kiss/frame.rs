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

use crate::error::Error;

pub const FEND: u8 = 0xC0;
pub const FESC: u8 = 0xDB;
pub const TFEND: u8 = 0xDC;
pub const TFESC: u8 = 0xDD;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KissFrame {
    pub command: u8,
    pub data: Vec<u8>,
}

impl KissFrame {
    /// Decode a raw KISS frame (the bytes between FEND delimiters).
    /// Performs byte un-stuffing.
    pub fn decode(raw: &[u8]) -> Result<Self, Error> {
        if raw.is_empty() {
            return Err(Error::KissFrame("empty frame".into()));
        }

        let command = raw[0];
        let mut data = Vec::with_capacity(raw.len() - 1);
        let mut i = 1;
        while i < raw.len() {
            if raw[i] == FESC {
                i += 1;
                if i >= raw.len() {
                    return Err(Error::KissFrame("truncated escape sequence".into()));
                }
                match raw[i] {
                    TFEND => data.push(FEND),
                    TFESC => data.push(FESC),
                    other => {
                        return Err(Error::KissFrame(format!(
                            "invalid escape byte 0x{other:02X}"
                        )));
                    }
                }
            } else {
                data.push(raw[i]);
            }
            i += 1;
        }

        Ok(KissFrame { command, data })
    }

    /// Encode into wire format with FEND delimiters and byte stuffing.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.data.len() + 4);
        out.push(FEND);
        Self::stuff_byte(self.command, &mut out);
        for &b in &self.data {
            Self::stuff_byte(b, &mut out);
        }
        out.push(FEND);
        out
    }

    fn stuff_byte(b: u8, out: &mut Vec<u8>) {
        match b {
            FEND => {
                out.push(FESC);
                out.push(TFEND);
            }
            FESC => {
                out.push(FESC);
                out.push(TFESC);
            }
            _ => out.push(b),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_simple_frame() {
        let frame = KissFrame {
            command: 0x00,
            data: vec![0x01, 0x02, 0x03],
        };
        assert_eq!(frame.encode(), vec![FEND, 0x00, 0x01, 0x02, 0x03, FEND]);
    }

    #[test]
    fn encode_stuffs_fend_in_data() {
        let frame = KissFrame {
            command: 0x00,
            data: vec![0xAA, FEND, 0xBB],
        };
        assert_eq!(
            frame.encode(),
            vec![FEND, 0x00, 0xAA, FESC, TFEND, 0xBB, FEND]
        );
    }

    #[test]
    fn encode_stuffs_fesc_in_data() {
        let frame = KissFrame {
            command: 0x00,
            data: vec![0xAA, FESC, 0xBB],
        };
        assert_eq!(
            frame.encode(),
            vec![FEND, 0x00, 0xAA, FESC, TFESC, 0xBB, FEND]
        );
    }

    #[test]
    fn encode_stuffs_fend_in_command() {
        let frame = KissFrame {
            command: FEND,
            data: vec![0x01],
        };
        assert_eq!(frame.encode(), vec![FEND, FESC, TFEND, 0x01, FEND]);
    }

    #[test]
    fn decode_simple_frame() {
        let raw = &[0x00, 0x01, 0x02, 0x03];
        let frame = KissFrame::decode(raw).unwrap();
        assert_eq!(frame.command, 0x00);
        assert_eq!(frame.data, vec![0x01, 0x02, 0x03]);
    }

    #[test]
    fn decode_unstuffs_fend() {
        let raw = &[0x00, 0xAA, FESC, TFEND, 0xBB];
        let frame = KissFrame::decode(raw).unwrap();
        assert_eq!(frame.data, vec![0xAA, FEND, 0xBB]);
    }

    #[test]
    fn decode_unstuffs_fesc() {
        let raw = &[0x00, 0xAA, FESC, TFESC, 0xBB];
        let frame = KissFrame::decode(raw).unwrap();
        assert_eq!(frame.data, vec![0xAA, FESC, 0xBB]);
    }

    #[test]
    fn decode_empty_errors() {
        assert!(KissFrame::decode(&[]).is_err());
    }

    #[test]
    fn decode_truncated_escape_errors() {
        let raw = &[0x00, 0xAA, FESC];
        assert!(KissFrame::decode(raw).is_err());
    }

    #[test]
    fn decode_invalid_escape_errors() {
        let raw = &[0x00, 0xAA, FESC, 0x00];
        assert!(KissFrame::decode(raw).is_err());
    }

    #[test]
    fn roundtrip() {
        let original = KissFrame {
            command: 0x00,
            data: vec![0xAA, FEND, 0xBB, FESC, 0xCC, FEND, FESC],
        };
        let encoded = original.encode();
        // Strip FEND delimiters for decode
        let inner = &encoded[1..encoded.len() - 1];
        let decoded = KissFrame::decode(inner).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn decode_command_only() {
        let raw = &[0x05];
        let frame = KissFrame::decode(raw).unwrap();
        assert_eq!(frame.command, 0x05);
        assert!(frame.data.is_empty());
    }
}
