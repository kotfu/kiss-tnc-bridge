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

//! USB serial ports, and choosing the one a device command is for.
//!
//! On Linux ports are named by their `/dev/serial/by-id` links, which stay
//! the same across reboots and replugging; on macOS by their `/dev/cu.*`
//! names (the `/dev/tty.*` ones wait for carrier detect).

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::port::serial_list::Usb;

/// The USB serial chips of the boards `ktbctl` manages. The ATOM Lite's
/// and the StickC Plus SE's is the same CH552, which presents itself with
/// FTDI's ids, which other FTDI devices share, so a match is only a likely
/// device until it answers `device.info`.
pub const BOARD_CHIPS: &[(u16, u16, &str)] = &[
    (0x0403, 0x6001, "ATOM Lite or StickC Plus SE"),
    (0x1a86, 0x55d4, "Core2 v1.3"),
];

/// A serial port a device might be on.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DevicePort {
    /// The name to show and use: a `/dev/serial/by-id` path on Linux, a
    /// `/dev/cu.*` name on macOS, or what `--device` gave.
    pub path: String,
    /// The device it is now, with links resolved: `/dev/ttyACM0`.
    pub device: PathBuf,
    /// `0403:6001`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usb_id: Option<String>,
    /// The USB device's manufacturer and product.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The board whose USB serial chip this is, if it's one of them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub looks_like: Option<&'static str>,
    /// The USB vendor and product ids, as numbers.
    #[serde(skip)]
    pub ids: Option<(u16, u16)>,
}

impl DevicePort {
    fn new(path: String, device: PathBuf, usb: Option<Usb>) -> DevicePort {
        let looks_like = usb.as_ref().and_then(|u| {
            BOARD_CHIPS
                .iter()
                .find(|(v, p, _)| *v == u.vendor && *p == u.product)
                .map(|(_, _, board)| *board)
        });
        DevicePort {
            path,
            device,
            usb_id: usb
                .as_ref()
                .map(|u| format!("{:04x}:{:04x}", u.vendor, u.product)),
            ids: usb.as_ref().map(|u| (u.vendor, u.product)),
            description: usb.and_then(|u| u.description),
            looks_like,
        }
    }
}

/// The USB serial ports present.
#[cfg(target_os = "linux")]
pub fn usb_ports() -> Vec<DevicePort> {
    crate::port::serial_list::ports()
        .into_iter()
        .filter(|p| p.usb.is_some())
        .map(|p| DevicePort::new(p.path, p.device, p.usb))
        .collect()
}

/// The USB serial ports present.
#[cfg(not(target_os = "linux"))]
pub fn usb_ports() -> Vec<DevicePort> {
    use tokio_serial::SerialPortType;
    let Ok(ports) = tokio_serial::available_ports() else {
        return Vec::new();
    };
    let mut out: Vec<DevicePort> = ports
        .into_iter()
        .filter(|p| p.port_name.starts_with("/dev/cu."))
        .filter_map(|p| match p.port_type {
            SerialPortType::UsbPort(u) => {
                let description = match (u.manufacturer, u.product) {
                    (Some(m), Some(p)) => Some(format!("{m} {p}")),
                    (m, p) => m.or(p),
                };
                let usb = Usb {
                    vendor: u.vid,
                    product: u.pid,
                    description,
                };
                Some(DevicePort::new(
                    p.port_name.clone(),
                    PathBuf::from(&p.port_name),
                    Some(usb),
                ))
            }
            _ => None,
        })
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// The port a device command is for: the one `--device` names, or else the
/// only one with a board's USB serial chip.
pub fn choose(ports: &[DevicePort], given: Option<&str>) -> Result<DevicePort, String> {
    if let Some(given) = given {
        return named(ports, given);
    }
    let likely: Vec<&DevicePort> = ports.iter().filter(|p| p.looks_like.is_some()).collect();
    match likely.as_slice() {
        [] => Err(
            "no ATOM Lite, StickC Plus SE or Core2 v1.3 found on USB. Check that it's plugged in with a \
                   cable that carries data (some carry only power), or name its port with \
                   --device"
                .into(),
        ),
        [one] => Ok((*one).clone()),
        many => Err(format!(
            "more than one device on USB; pick one with --device:\n{}",
            many.iter()
                .map(|p| format!("  {}", p.path))
                .collect::<Vec<_>>()
                .join("\n")
        )),
    }
}

/// The port `--device` names, by any of its names: a by-id link and the
/// device it points to are the same port. serialport and udev list device
/// names, so both sides are resolved before they're compared.
fn named(ports: &[DevicePort], given: &str) -> Result<DevicePort, String> {
    let device = std::fs::canonicalize(given)
        .map_err(|_| format!("no serial port {given}: is the device plugged in?"))?;
    let found = ports
        .iter()
        .find(|p| same_file(&p.device, &device))
        .cloned();
    Ok(match found {
        Some(p) => DevicePort {
            path: given.to_string(),
            ..p
        },
        // Not a USB serial port we can see; it may still be a device.
        None => DevicePort::new(given.to_string(), device, None),
    })
}

fn same_file(a: &Path, b: &Path) -> bool {
    a == b || std::fs::canonicalize(a).is_ok_and(|a| a == b)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn port(path: &str, device: &Path, vendor: u16, product: u16) -> DevicePort {
        DevicePort::new(
            path.into(),
            device.to_path_buf(),
            Some(Usb {
                vendor,
                product,
                description: None,
            }),
        )
    }

    #[test]
    fn boards_are_recognised_by_their_usb_serial_chip() {
        let d = Path::new("/dev/ttyUSB0");
        assert_eq!(
            port("a", d, 0x0403, 0x6001).looks_like,
            Some("ATOM Lite or StickC Plus SE")
        );
        assert_eq!(port("a", d, 0x1a86, 0x55d4).looks_like, Some("Core2 v1.3"));
        assert_eq!(port("a", d, 0x2166, 0x9023).looks_like, None);
        assert_eq!(
            port("a", d, 0x0403, 0x6001).usb_id.as_deref(),
            Some("0403:6001")
        );
    }

    #[test]
    fn the_only_likely_port_is_chosen() {
        let d = Path::new("/dev/null");
        let radio = port("radio", d, 0x2166, 0x9023);
        let atom = port("atom", d, 0x0403, 0x6001);
        let core2 = port("core2", d, 0x1a86, 0x55d4);
        assert_eq!(
            choose(&[radio.clone(), atom.clone()], None).unwrap().path,
            "atom"
        );
        assert!(
            choose(std::slice::from_ref(&radio), None)
                .unwrap_err()
                .contains("no ATOM Lite")
        );
        let many = choose(&[atom, radio, core2], None).unwrap_err();
        assert!(many.contains("  atom\n  core2"), "{many}");
    }

    #[test]
    fn a_by_id_link_names_the_port_it_points_to() {
        let t = tempfile::tempdir().unwrap();
        let tty = t.path().join("ttyACM0");
        std::fs::write(&tty, "").unwrap();
        let tty = std::fs::canonicalize(&tty).unwrap();
        let link = t.path().join("usb-1a86_USB_Single_Serial_5C8D017949-if00");
        symlink(&tty, &link).unwrap();
        let ports = [port("listed", &tty, 0x1a86, 0x55d4)];

        let by_link = choose(&ports, Some(link.to_str().unwrap())).unwrap();
        assert_eq!(by_link.path, link.to_str().unwrap());
        assert_eq!(by_link.device, tty);
        assert_eq!(by_link.looks_like, Some("Core2 v1.3"));

        let by_device = choose(&ports, Some(tty.to_str().unwrap())).unwrap();
        assert_eq!(by_device.usb_id.as_deref(), Some("1a86:55d4"));

        let gone = t.path().join("ttyACM9");
        assert!(choose(&ports, Some(gone.to_str().unwrap())).is_err());
    }

    #[test]
    fn a_port_that_isnt_listed_can_still_be_named() {
        let t = tempfile::tempdir().unwrap();
        let tty = t.path().join("ttyS0");
        std::fs::write(&tty, "").unwrap();
        let p = choose(&[], Some(tty.to_str().unwrap())).unwrap();
        assert_eq!(p.usb_id, None);
        assert_eq!(p.looks_like, None);
    }
}
