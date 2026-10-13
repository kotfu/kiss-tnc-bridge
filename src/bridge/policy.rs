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

//! The KISS policy filter: what an app may send to a shared TNC.
//!
//! Sharing one radio among several apps needs rules a single Dire Wolf
//! connection never did. The filter runs on every app-to-TNC frame.

use crate::config::AppParams;

/// KISS command byte values (low nibble). The high nibble is the KISS
/// channel and passes through untouched.
pub const CMD_DATA: u8 = 0x00;
pub const CMD_TXDELAY: u8 = 0x01;
pub const CMD_PERSISTENCE: u8 = 0x02;
pub const CMD_SLOTTIME: u8 = 0x03;
pub const CMD_TXTAIL: u8 = 0x04;
pub const CMD_FULLDUPLEX: u8 = 0x05;
pub const CMD_SETHARDWARE: u8 = 0x06;
/// KISS "return": leaves KISS mode. A whole-byte command, no channel.
pub const CMD_RETURN: u8 = 0xFF;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Drop(&'static str),
}

/// Decide whether `raw` (the bytes between FENDs, still stuffed) may go to
/// the TNC.
pub fn filter_app_frame(raw: &[u8], app_params: AppParams) -> Verdict {
    let Some(&cmd) = raw.first() else {
        return Verdict::Drop("empty frame");
    };
    if cmd == CMD_RETURN {
        // On Kenwood radios this takes the TNC out of KISS mode and breaks
        // it for every app. Only the daemon may send it, at teardown.
        return Verdict::Drop("KISS return (C0 FF C0) from an app");
    }
    match cmd & 0x0F {
        CMD_DATA => Verdict::Pass,
        CMD_TXDELAY | CMD_PERSISTENCE | CMD_SLOTTIME | CMD_TXTAIL | CMD_FULLDUPLEX
        | CMD_SETHARDWARE => match app_params {
            AppParams::Pass => Verdict::Pass,
            AppParams::Drop => Verdict::Drop("KISS parameter command (app_params = \"drop\")"),
        },
        _ => Verdict::Drop("unknown KISS command"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_passes_either_way() {
        assert_eq!(
            filter_app_frame(&[0x00, 1, 2, 3], AppParams::Pass),
            Verdict::Pass
        );
        assert_eq!(
            filter_app_frame(&[0x00, 1, 2, 3], AppParams::Drop),
            Verdict::Pass
        );
        // Data on KISS channel 1 too.
        assert_eq!(
            filter_app_frame(&[0x10, 1, 2, 3], AppParams::Drop),
            Verdict::Pass
        );
    }

    #[test]
    fn kiss_return_is_always_dropped() {
        assert!(matches!(
            filter_app_frame(&[0xFF], AppParams::Pass),
            Verdict::Drop(_)
        ));
        assert!(matches!(
            filter_app_frame(&[0xFF], AppParams::Drop),
            Verdict::Drop(_)
        ));
    }

    #[test]
    fn parameters_follow_app_params() {
        for cmd in [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x16] {
            assert_eq!(
                filter_app_frame(&[cmd, 0x32], AppParams::Pass),
                Verdict::Pass,
                "cmd {cmd:#x}"
            );
            assert!(
                matches!(
                    filter_app_frame(&[cmd, 0x32], AppParams::Drop),
                    Verdict::Drop(_)
                ),
                "cmd {cmd:#x}"
            );
        }
    }

    #[test]
    fn junk_is_dropped() {
        assert!(matches!(
            filter_app_frame(&[], AppParams::Pass),
            Verdict::Drop(_)
        ));
        assert!(matches!(
            filter_app_frame(&[0x0C, 1], AppParams::Pass),
            Verdict::Drop(_)
        ));
    }
}
