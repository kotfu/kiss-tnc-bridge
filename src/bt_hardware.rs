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

//! What a Bluetooth adapter is, from sysfs: built into the board, a USB
//! dongle (and which one), or a PCI card. A MAC alone doesn't tell a Pi's
//! own Bluetooth from the dongle plugged into it.

use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hardware {
    /// `usb`, `board` (a UART or SDIO chip on the board), `pci`, or `other`.
    pub bus: &'static str,
    /// A USB device's product string: `TP-Link UB500 Adapter`.
    pub product: Option<String>,
    /// A USB device's `vendor:product`: `2357:0604`.
    pub usb_id: Option<String>,
}

/// What `hci` (`hci0`) is, or `None` if sysfs doesn't say.
pub fn adapter_hardware(hci: &str) -> Option<Hardware> {
    hardware_under(Path::new("/sys/class/bluetooth"), hci)
}

fn hardware_under(root: &Path, hci: &str) -> Option<Hardware> {
    let dev = std::fs::canonicalize(root.join(hci).join("device")).ok()?;
    // A USB adapter's device is an interface; the device above it has the
    // ids and strings.
    let mut dir = dev.as_path();
    for _ in 0..3 {
        let read = |name: &str| {
            std::fs::read_to_string(dir.join(name))
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        };
        if let (Some(v), Some(p)) = (read("idVendor"), read("idProduct")) {
            return Some(Hardware {
                bus: "usb",
                product: read("product"),
                usb_id: Some(format!("{v}:{p}")),
            });
        }
        dir = dir.parent()?;
    }
    let path = dev.to_string_lossy();
    let bus = if path.contains("/usb") {
        "usb"
    } else if path.contains("serial") || path.contains("/mmc") {
        "board"
    } else if path.contains("/pci") {
        "pci"
    } else {
        "other"
    };
    Some(Hardware {
        bus,
        product: None,
        usb_id: None,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn a_board_chip_and_a_usb_dongle() {
        let t = tempfile::tempdir().unwrap();
        let sys = t.path();
        // A Pi 4's own chip, on its UART.
        let uart = sys.join("devices/platform/soc/fe201000.serial/serial0/serial0-0");
        std::fs::create_dir_all(&uart).unwrap();
        // A dongle: hci → the USB interface → the USB device.
        let usb = sys.join("devices/platform/scb/fd500000.pcie/usb1/1-1/1-1.1");
        std::fs::create_dir_all(usb.join("1-1.1:1.0")).unwrap();
        std::fs::write(usb.join("idVendor"), "2357\n").unwrap();
        std::fs::write(usb.join("idProduct"), "0604\n").unwrap();
        std::fs::write(usb.join("product"), "TP-Link UB500 Adapter\n").unwrap();
        let class = sys.join("class/bluetooth");
        for (hci, target) in [("hci0", uart), ("hci1", usb.join("1-1.1:1.0"))] {
            std::fs::create_dir_all(class.join(hci)).unwrap();
            symlink(target, class.join(hci).join("device")).unwrap();
        }

        assert_eq!(
            hardware_under(&class, "hci0").unwrap(),
            Hardware {
                bus: "board",
                product: None,
                usb_id: None
            }
        );
        assert_eq!(
            hardware_under(&class, "hci1").unwrap(),
            Hardware {
                bus: "usb",
                product: Some("TP-Link UB500 Adapter".into()),
                usb_id: Some("2357:0604".into())
            }
        );
        assert_eq!(hardware_under(&class, "hci9"), None);
    }
}
