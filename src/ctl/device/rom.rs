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

//! The ESP32's ROM bootloader, through espflash: telling the board from
//! the chip, writing the firmware, and resetting into it. Every call
//! blocks, so these run on a blocking thread (spikes/espflash-usb has the
//! measurements behind the choices here).

use std::time::Duration;

use espflash::connection::{Connection, ResetAfterOperation, ResetBeforeOperation};
use espflash::flasher::Flasher;
use espflash::image_format::Segment;
use espflash::target::{Chip, ProgressCallbacks};
use serde::Serialize;
use tokio_serial::{SerialPort, UsbPortInfo};

use super::manifest::Part;
use super::ports::DevicePort;

/// The rate to write at. Both boards take it; the ATOM Lite's USB serial
/// chip, a CH552, can't do 921600, 460800 or 230400.
pub const FAST_BAUD: u32 = 1_500_000;

/// The ROM's own rate, which every board takes.
pub const SLOW_BAUD: u32 = 115_200;

/// What the ROM says about the chip.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChipInfo {
    /// `ESP32-PICO-D4`, as esptool names the package.
    pub package: String,
    /// `1.1`.
    pub revision: String,
    /// `4MB`.
    pub flash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
}

impl ChipInfo {
    /// The boards this chip could be. The ATOM Lite and the StickC Plus SE
    /// have the same chip and flash, so it can't tell them apart.
    pub fn boards(&self) -> &'static [&'static str] {
        boards_for(&self.package, &self.flash)
    }

    /// `an ESP32-PICO-D4 (revision 1.1) with 4MB of flash`.
    pub fn describe(&self) -> String {
        format!(
            "an {} (revision {}) with {} of flash",
            self.package, self.revision, self.flash
        )
    }
}

fn boards_for(package: &str, flash: &str) -> &'static [&'static str] {
    match (package, flash) {
        ("ESP32-PICO-D4", "4MB") => &["atom-lite", "stickc-plus-se"],
        ("ESP32-D0WDQ6-V3", "16MB") => &["core2"],
        _ => &[],
    }
}

/// esptool's name for an ESP32's package, from eFuse block 0, word 3.
/// espflash doesn't report it.
fn package_name(word3: u32, revision_major: u32) -> String {
    let pkg = ((word3 >> 9) & 0x7) | (((word3 >> 2) & 0x1) << 3);
    let v3 = revision_major >= 3;
    match pkg {
        0 if v3 => "ESP32-D0WDQ6-V3".into(),
        0 => "ESP32-D0WDQ6".into(),
        1 if v3 => "ESP32-D0WD-V3".into(),
        1 => "ESP32-D0WD".into(),
        2 => "ESP32-D2WD".into(),
        4 => "ESP32-U4WDH".into(),
        5 if v3 => "ESP32-PICO-V3".into(),
        5 => "ESP32-PICO-D4".into(),
        6 => "ESP32-PICO-V3-02".into(),
        7 => "ESP32-D0WDR2-V3".into(),
        n => format!("ESP32 (package {n})"),
    }
}

const EFUSE_BLK0_WORD3: u32 = 0x3FF5_A00C;

/// A connection to the ROM bootloader, with espflash's stub loaded.
pub struct Rom {
    flasher: Flasher,
    pub chip: ChipInfo,
    pub baud: u32,
}

impl Rom {
    /// Resets the device into its ROM bootloader and loads the stub, then
    /// switches to `baud`. About 7 s on these boards: espflash waits out
    /// two commands the original ESP32's ROM doesn't answer. If it fails,
    /// the device may be left in its download mode: `hard_reset` it.
    pub fn connect(port: &DevicePort, baud: u32) -> Result<Rom, String> {
        let serial = tokio_serial::new(&port.path, SLOW_BAUD)
            .timeout(Duration::from_secs(3))
            .open_native()
            .map_err(|e| format!("can't open {}: {e}", port.path))?;
        let (vid, pid) = port.ids.unwrap_or_default();
        let info = UsbPortInfo {
            vid,
            pid,
            serial_number: None,
            manufacturer: None,
            product: None,
        };
        let conn = Connection::new(
            serial,
            info,
            ResetAfterOperation::HardReset,
            ResetBeforeOperation::DefaultReset,
            SLOW_BAUD,
        );
        let fast = (baud > SLOW_BAUD).then_some(baud);
        let mut flasher = Flasher::connect(conn, true, true, false, None, fast)
            .map_err(|e| format!("couldn't talk to its bootloader: {}", chain(&e)))?;
        if flasher.chip() != Chip::Esp32 {
            return Err(format!(
                "it's an {}, not an ESP32: not a board ktbctl knows",
                flasher.chip()
            ));
        }
        let rom_err = |e: espflash::Error| format!("reading the chip: {}", chain(&e));
        let info = flasher.device_info().map_err(rom_err)?;
        let (major, minor) = info.revision.unwrap_or_default();
        let word3 = flasher
            .connection()
            .read_reg(EFUSE_BLK0_WORD3)
            .map_err(rom_err)?;
        let chip = ChipInfo {
            package: package_name(word3, major),
            revision: format!("{major}.{minor}"),
            flash: info.flash_size.to_string(),
            mac: info.mac_address,
        };
        Ok(Rom {
            flasher,
            chip,
            baud,
        })
    }

    /// Erases the whole flash: everything on the device goes.
    pub fn erase(&mut self) -> Result<(), String> {
        self.flasher
            .erase_flash()
            .map_err(|e| format!("erasing: {}", chain(&e)))
    }

    /// Writes every part in one call, each checked with MD5 after it's
    /// written. (A call per part hung after the first in the spike.)
    pub fn write(
        &mut self,
        parts: &[Part],
        progress: &mut dyn ProgressCallbacks,
    ) -> Result<(), String> {
        let segments: Vec<Segment<'_>> = parts
            .iter()
            .map(|p| Segment::new(p.offset, &p.data))
            .collect();
        self.flasher
            .write_bins_to_flash(&segments, progress)
            .map_err(|e| format!("writing: {}", chain(&e)))
    }

    /// Resets the device out of its bootloader, into the firmware.
    pub fn reset(mut self) -> Result<(), String> {
        let chip = self.flasher.chip();
        self.flasher
            .connection()
            .reset_after(true, chip)
            .map_err(|e| format!("restarting it: {}", chain(&e)))
    }
}

/// Resets the device with its reset line: RTS held without DTR pulls EN
/// low, and letting go with DTR low leaves IO0 high, so it starts its
/// firmware rather than its download mode. For a device a failure left
/// in its bootloader.
pub fn hard_reset(path: &str) -> Result<(), String> {
    let mut port = tokio_serial::new(path, SLOW_BAUD)
        .open_native()
        .map_err(|e| format!("can't open {path} to restart it: {e}"))?;
    let restart = |port: &mut dyn SerialPort| -> Result<(), tokio_serial::Error> {
        port.write_data_terminal_ready(false)?;
        port.write_request_to_send(true)?;
        std::thread::sleep(Duration::from_millis(100));
        port.write_request_to_send(false)
    };
    restart(&mut port).map_err(|e| format!("couldn't restart it: {e}"))
}

/// An error with its causes: espflash's say "Communication error while
/// flashing device" and put what happened in the source.
fn chain(e: &dyn std::error::Error) -> String {
    let mut s = e.to_string();
    let mut source = e.source();
    while let Some(cause) = source {
        let text = cause.to_string();
        if !s.contains(&text) {
            s.push_str(": ");
            s.push_str(&text);
        }
        source = cause.source();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packages_are_named_as_esptool_names_them() {
        // The project's boards, from their eFuses (spikes/espflash-usb).
        let pico = 5 << 9;
        assert_eq!(package_name(pico, 1), "ESP32-PICO-D4");
        assert_eq!(package_name(pico, 3), "ESP32-PICO-V3");
        assert_eq!(package_name(0, 3), "ESP32-D0WDQ6-V3");
        assert_eq!(package_name(0, 1), "ESP32-D0WDQ6");
        // The fourth bit is bit 2.
        assert_eq!(package_name((2 << 9) | (1 << 2), 3), "ESP32 (package 10)");
    }

    #[test]
    fn boards_are_told_apart_by_package_and_flash() {
        assert_eq!(
            boards_for("ESP32-PICO-D4", "4MB"),
            ["atom-lite", "stickc-plus-se"]
        );
        assert_eq!(boards_for("ESP32-D0WDQ6-V3", "16MB"), ["core2"]);
        assert!(boards_for("ESP32-D0WDQ6-V3", "4MB").is_empty());
        assert!(boards_for("ESP32-D0WD", "4MB").is_empty());
    }
}
