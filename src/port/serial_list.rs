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

//! `serial.list`: the serial devices present, by their stable udev names
//! (`/dev/serial/by-id/…`, or `by-path` for devices without a serial
//! number in their id), with USB details from sysfs and known TNCs
//! flagged, so `ktbctl setup` can offer them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::control::protocol::SerialEntry;

/// USB TNCs this daemon knows: vendor, product, what it is, its `model`.
const KNOWN: &[(u16, u16, &str, &str)] = &[(0x2166, 0x9023, "Kenwood TH-D75", "th-d75")];

/// Serial devices under `root` (normally `/dev/serial`), one entry per
/// device, preferring the by-id name.
pub fn list(cfg: &Config) -> Vec<SerialEntry> {
    list_under(Path::new("/dev/serial"), Path::new("/sys/class/tty"), cfg)
}

fn list_under(root: &Path, sys_tty: &Path, cfg: &Config) -> Vec<SerialEntry> {
    let mut by_device: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    for sub in ["by-id", "by-path"] {
        let Ok(dir) = std::fs::read_dir(root.join(sub)) else {
            continue;
        };
        let mut paths: Vec<PathBuf> = dir.flatten().map(|e| e.path()).collect();
        paths.sort();
        for p in paths {
            if let Ok(dev) = std::fs::canonicalize(&p) {
                by_device
                    .entry(dev)
                    .or_default()
                    .push(p.display().to_string());
            }
        }
    }
    let mut out = Vec::new();
    for (device, mut paths) in by_device {
        let path = paths.remove(0);
        let usb = device.file_name().and_then(|n| usb_info(&sys_tty.join(n)));
        let known = usb
            .as_ref()
            .and_then(|u| KNOWN.iter().find(|k| (k.0, k.1) == (u.vendor, u.product)));
        let tnc = cfg
            .tncs
            .iter()
            .find(|t| {
                t.serial
                    .as_ref()
                    .is_some_and(|s| s.path == path || paths.contains(&s.path))
            })
            .map(|t| t.id.clone());
        out.push(SerialEntry {
            path,
            device: device.display().to_string(),
            also: paths,
            usb_id: usb
                .as_ref()
                .map(|u| format!("{:04x}:{:04x}", u.vendor, u.product)),
            description: usb.and_then(|u| u.description),
            known_tnc: known.map(|k| k.2.to_string()),
            model: known.map(|k| k.3.to_string()),
            tnc,
        });
    }
    out
}

struct Usb {
    vendor: u16,
    product: u16,
    description: Option<String>,
}

/// The USB device behind a tty, found by walking up from its sysfs
/// `device` link to the directory that has `idVendor`.
fn usb_info(tty: &Path) -> Option<Usb> {
    let mut dir = std::fs::canonicalize(tty.join("device")).ok()?;
    for _ in 0..4 {
        let read = |name: &str| {
            std::fs::read_to_string(dir.join(name))
                .ok()
                .map(|s| s.trim().to_string())
        };
        if let (Some(v), Some(p)) = (read("idVendor"), read("idProduct")) {
            let description = match (read("manufacturer"), read("product")) {
                (Some(m), Some(p)) => Some(format!("{m} {p}")),
                (m, p) => m.or(p),
            };
            return Some(Usb {
                vendor: u16::from_str_radix(&v, 16).ok()?,
                product: u16::from_str_radix(&p, 16).ok()?,
                description,
            });
        }
        dir = dir.parent()?.to_path_buf();
    }
    None
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn devices_are_listed_once_with_usb_details() {
        let t = tempfile::tempdir().unwrap();
        let dev = t.path().join("dev");
        std::fs::create_dir_all(&dev).unwrap();
        std::fs::write(dev.join("ttyACM0"), "").unwrap();
        std::fs::write(dev.join("ttyUSB0"), "").unwrap();
        let root = t.path().join("serial");
        std::fs::create_dir_all(root.join("by-id")).unwrap();
        std::fs::create_dir_all(root.join("by-path")).unwrap();
        let by_id = root.join("by-id/usb-JVCKENWOOD_TH-D75-if00");
        symlink(dev.join("ttyACM0"), &by_id).unwrap();
        symlink(
            dev.join("ttyACM0"),
            root.join("by-path/platform-usb-0:1.4:1.0"),
        )
        .unwrap();
        symlink(
            dev.join("ttyUSB0"),
            root.join("by-path/platform-usb-0:1.3:1.0"),
        )
        .unwrap();
        // sysfs: tty -> device (the interface) -> parent with the USB ids.
        let sys = t.path().join("sys");
        let usbdev = sys.join("devices/usb1/1-1.4");
        std::fs::create_dir_all(usbdev.join("1-1.4:1.0")).unwrap();
        std::fs::write(usbdev.join("idVendor"), "2166\n").unwrap();
        std::fs::write(usbdev.join("idProduct"), "9023\n").unwrap();
        std::fs::write(usbdev.join("manufacturer"), "JVCKENWOOD\n").unwrap();
        std::fs::write(usbdev.join("product"), "TH-D75\n").unwrap();
        let tty = sys.join("class/tty/ttyACM0");
        std::fs::create_dir_all(&tty).unwrap();
        symlink(usbdev.join("1-1.4:1.0"), tty.join("device")).unwrap();

        let text = format!(
            "[[tnc]]\nid = \"d75\"\nmodel = \"th-d75\"\nserial = {{ path = \"{}\" }}\n",
            by_id.display()
        );
        let cfg = crate::config::resolve(&[(
            crate::config::Source::Static("t".into()),
            crate::config::load::parse_str(&text).unwrap(),
        )])
        .unwrap();
        let list = list_under(&root, &sys.join("class/tty"), &cfg);
        assert_eq!(list.len(), 2, "{list:?}");
        let d75 = list.iter().find(|e| e.device.ends_with("ttyACM0")).unwrap();
        assert_eq!(d75.path, by_id.display().to_string());
        assert_eq!(d75.also.len(), 1);
        assert_eq!(d75.usb_id.as_deref(), Some("2166:9023"));
        assert_eq!(d75.description.as_deref(), Some("JVCKENWOOD TH-D75"));
        assert_eq!(d75.known_tnc.as_deref(), Some("Kenwood TH-D75"));
        assert_eq!(d75.model.as_deref(), Some("th-d75"));
        assert_eq!(d75.tnc.as_deref(), Some("d75"));
        let other = list.iter().find(|e| e.device.ends_with("ttyUSB0")).unwrap();
        assert!(other.path.contains("by-path"));
        assert_eq!(other.usb_id, None);
        assert_eq!(other.tnc, None);
    }
}
