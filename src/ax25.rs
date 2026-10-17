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

//! Just enough AX.25 to show a frame to a person: `W1AW-9>APDR16,WIDE1-1*:
//! text`. Display only: the bridge forwards frames without looking inside,
//! and nothing here decides what happens to one.

use serde::{Deserialize, Serialize};

use crate::kiss::frame::KissFrame;

/// A KISS frame, decoded as far as it decodes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Decoded {
    /// The KISS channel (the command byte's high nibble).
    pub channel: u8,
    /// The KISS command (low nibble): 0 is data.
    pub command: u8,
    /// For data frames that parse as AX.25 UI and I frames.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ax25: Option<Header>,
    /// One line for a person: the TNC2-style header and the information
    /// field with unprintable bytes shown as `.`, or a description of a
    /// frame that isn't AX.25 data.
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Header {
    pub source: String,
    pub destination: String,
    /// Digipeaters, `*` on those that have repeated the frame.
    pub path: Vec<String>,
}

/// Decode `raw`, the bytes between FENDs as forwarded (still stuffed).
pub fn decode(raw: &[u8]) -> Decoded {
    let Ok(frame) = KissFrame::decode(raw) else {
        return Decoded {
            channel: 0,
            command: 0,
            ax25: None,
            summary: format!("undecodable KISS frame ({} bytes)", raw.len()),
        };
    };
    let channel = frame.command >> 4;
    let command = frame.command & 0x0F;
    if command != 0 {
        return Decoded {
            channel,
            command,
            ax25: None,
            summary: format!("KISS command {command:#x}: {}", hex(&frame.data)),
        };
    }
    match parse(&frame.data) {
        Some((header, info)) => {
            let via = if header.path.is_empty() {
                String::new()
            } else {
                format!(",{}", header.path.join(","))
            };
            let summary = format!(
                "{}>{}{via}:{}",
                header.source,
                header.destination,
                printable(info)
            );
            Decoded {
                channel,
                command,
                ax25: Some(header),
                summary,
            }
        }
        None => Decoded {
            channel,
            command,
            ax25: None,
            summary: format!("{} bytes, not AX.25", frame.data.len()),
        },
    }
}

/// The address fields, and the information field of UI and I frames.
fn parse(data: &[u8]) -> Option<(Header, &[u8])> {
    let mut addrs = Vec::new();
    let mut i = 0;
    loop {
        let field = data.get(i..i + 7)?;
        let (call, last, repeated) = address(field)?;
        addrs.push((call, repeated));
        i += 7;
        if last {
            break;
        }
        if addrs.len() > 10 {
            return None; // destination, source and at most 8 digipeaters
        }
    }
    if addrs.len() < 2 {
        return None;
    }
    let control = *data.get(i)?;
    // UI (0x03, either poll/final) and I frames (low bit 0) carry a PID
    // and an information field; others carry neither.
    let info = if control & 0xEF == 0x03 || control & 0x01 == 0 {
        data.get(i + 2..).unwrap_or(&[])
    } else {
        &[]
    };
    let destination = addrs[0].0.clone();
    let source = addrs[1].0.clone();
    let path = addrs[2..]
        .iter()
        .map(|(c, r)| if *r { format!("{c}*") } else { c.clone() })
        .collect();
    Some((
        Header {
            source,
            destination,
            path,
        },
        info,
    ))
}

/// One 7-byte address: callsign, whether it's the last address, and the
/// has-been-repeated bit.
fn address(field: &[u8]) -> Option<(String, bool, bool)> {
    let mut call = String::new();
    for &b in &field[..6] {
        let c = (b >> 1) as char;
        if c != ' ' {
            if !(c.is_ascii_uppercase() || c.is_ascii_digit()) {
                return None;
            }
            call.push(c);
        }
    }
    if call.is_empty() {
        return None;
    }
    let ssid = (field[6] >> 1) & 0x0F;
    if ssid != 0 {
        call.push_str(&format!("-{ssid}"));
    }
    Some((call, field[6] & 0x01 == 1, field[6] & 0x80 != 0))
}

fn printable(b: &[u8]) -> String {
    b.iter()
        .map(|&c| match c {
            0x20..=0x7E => c as char,
            _ => '.',
        })
        .collect()
}

fn hex(b: &[u8]) -> String {
    b.iter()
        .map(|x| format!("{x:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An AX.25 address field for `call` (with SSID), shifted.
    fn addr(call: &str, ssid: u8, last: bool, repeated: bool) -> Vec<u8> {
        let mut out: Vec<u8> = format!("{call:<6}").bytes().map(|b| b << 1).collect();
        out.push(0x60 | (ssid << 1) | u8::from(last) | if repeated { 0x80 } else { 0 });
        out
    }

    fn ui(addrs: &[Vec<u8>], info: &[u8]) -> Vec<u8> {
        let mut data = vec![0x00]; // KISS data, channel 0
        for a in addrs {
            data.extend_from_slice(a);
        }
        data.extend_from_slice(&[0x03, 0xF0]);
        data.extend_from_slice(info);
        // Stuff as the bridge forwards it.
        KissFrame {
            command: data[0],
            data: data[1..].to_vec(),
        }
        .encode()[1..]
            .split_last()
            .unwrap()
            .1
            .to_vec()
    }

    #[test]
    fn a_ui_frame_with_a_path() {
        let raw = ui(
            &[
                addr("APDR16", 0, false, false),
                addr("W1AW", 9, false, false),
                addr("WIDE1", 1, false, true),
                addr("WIDE2", 1, true, false),
            ],
            b"!4237.14N/07120.83W>\r",
        );
        let d = decode(&raw);
        assert_eq!(d.channel, 0);
        let h = d.ax25.unwrap();
        assert_eq!(h.source, "W1AW-9");
        assert_eq!(h.destination, "APDR16");
        assert_eq!(h.path, vec!["WIDE1-1*", "WIDE2-1"]);
        assert_eq!(
            d.summary,
            "W1AW-9>APDR16,WIDE1-1*,WIDE2-1:!4237.14N/07120.83W>."
        );
    }

    #[test]
    fn stuffed_bytes_and_channels() {
        // A FEND in the information field arrives stuffed.
        let raw = ui(
            &[
                addr("APRS", 0, false, false),
                addr("N0CALL", 0, true, false),
            ],
            &[b'a', 0xC0, b'b'],
        );
        let d = decode(&raw);
        assert_eq!(d.summary, "N0CALL>APRS:a.b");
        let mut on_ch1 = raw.clone();
        on_ch1[0] = 0x10;
        assert_eq!(decode(&on_ch1).channel, 1);
    }

    #[test]
    fn not_everything_is_ax25() {
        assert_eq!(decode(&[0x01, 0x32]).summary, "KISS command 0x1: 32");
        assert_eq!(decode(&[0x00, 0x41, 0x42]).summary, "2 bytes, not AX.25");
        assert!(decode(&[0x00, 0x41, 0x42]).ax25.is_none());
        assert!(decode(&[]).summary.contains("undecodable"));
    }
}
