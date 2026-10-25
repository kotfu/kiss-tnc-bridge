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

//! Whether this host's BlueZ will ask phones to pair.
//!
//! When a phone connects, bluetoothd acts as a GATT client toward it and
//! reads its battery level, which iOS gives only to paired devices, so the
//! phone is asked to pair with the bridge. The bridge needs no pairing and
//! doesn't take part in it, so that pairing fails and the phone is dropped
//! and reconnects. Any one of these stops bluetoothd probing phones:
//!
//! - `ReverseServiceDiscovery = false` in `[General]` of
//!   `/etc/bluetooth/main.conf`: no probing of devices that connect to the
//!   host; the host's own connections are unaffected.
//! - `Client = false` in `[GATT]`: no GATT client at all.
//! - bluetoothd run without its battery plugin (`-P battery`).

use std::path::Path;

pub const MAIN_CONF: &str = "/etc/bluetooth/main.conf";

/// What to tell someone whose host would ask phones to pair.
pub const ADVICE: &str = "BlueZ on this host reads the battery level of phones that connect, which asks them to pair with the bridge; set ReverseServiceDiscovery = false under [General] in /etc/bluetooth/main.conf and restart bluetooth (see \"Phones and pairing\" in the README)";

/// Whether bluetoothd, configured by `main_conf` and started with
/// `bluetoothd_args` (its command line, program name first), probes phones.
pub fn probes_phones(main_conf: &str, bluetoothd_args: &[String]) -> bool {
    !(setting_is_false(main_conf, "General", "ReverseServiceDiscovery")
        || setting_is_false(main_conf, "GATT", "Client")
        || battery_plugin_disabled(bluetoothd_args))
}

/// [`probes_phones`] for this host, or `None` when neither
/// `/etc/bluetooth/main.conf` nor a running bluetoothd can be found.
pub fn host_probes_phones() -> Option<bool> {
    let conf = std::fs::read_to_string(MAIN_CONF).ok();
    let args = bluetoothd_args(Path::new("/proc"));
    if conf.is_none() && args.is_none() {
        return None;
    }
    Some(probes_phones(
        conf.as_deref().unwrap_or(""),
        &args.unwrap_or_default(),
    ))
}

fn setting_is_false(conf: &str, section: &str, key: &str) -> bool {
    let mut current = "";
    let mut value = None;
    for line in conf.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            current = name.trim();
        } else if current == section
            && let Some((k, v)) = line.split_once('=')
            && k.trim() == key
        {
            value = Some(v.trim().to_ascii_lowercase());
        }
    }
    value.as_deref() == Some("false")
}

fn battery_plugin_disabled(args: &[String]) -> bool {
    let mut lists = Vec::new();
    let mut it = args.iter().skip(1);
    while let Some(arg) = it.next() {
        if arg == "-P" || arg == "--noplugin" {
            lists.extend(it.next().cloned());
        } else if let Some(list) = arg.strip_prefix("--noplugin=") {
            lists.push(list.to_string());
        } else if let Some(list) = arg.strip_prefix("-P") {
            lists.push(list.to_string());
        }
    }
    lists
        .iter()
        .flat_map(|l| l.split(','))
        .any(|p| p == "battery" || p == "*")
}

/// The command line of the running bluetoothd, found under `proc`.
fn bluetoothd_args(proc: &Path) -> Option<Vec<String>> {
    for entry in std::fs::read_dir(proc).ok()?.flatten() {
        let dir = entry.path();
        if std::fs::read_to_string(dir.join("comm")).ok()?.trim() != "bluetoothd" {
            continue;
        }
        let cmdline = std::fs::read(dir.join("cmdline")).ok()?;
        return Some(
            cmdline
                .split(|b| *b == 0)
                .filter(|a| !a.is_empty())
                .map(|a| String::from_utf8_lossy(a).into_owned())
                .collect(),
        );
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    const STOCK: &str = "[General]\n#ReverseServiceDiscovery = true\n\n[GATT]\n#Client = true\n";

    #[test]
    fn a_stock_host_probes_phones() {
        assert!(probes_phones(
            STOCK,
            &args(&["/usr/libexec/bluetooth/bluetoothd"])
        ));
        assert!(probes_phones("", &[]));
    }

    #[test]
    fn either_setting_stops_it() {
        let rsd = "[General]\nReverseServiceDiscovery = false\n";
        assert!(!probes_phones(rsd, &[]));
        let client = "[General]\nName = pi\n[GATT]\nClient=False\n";
        assert!(!probes_phones(client, &[]));
    }

    #[test]
    fn settings_count_only_in_their_section() {
        assert!(probes_phones(
            "[GATT]\nReverseServiceDiscovery = false\n",
            &[]
        ));
        assert!(probes_phones("[General]\nClient = false\n", &[]));
        assert!(probes_phones(
            "[General]\nReverseServiceDiscovery = true\n",
            &[]
        ));
    }

    #[test]
    fn so_does_dropping_the_battery_plugin() {
        for a in [
            &["bluetoothd", "-P", "battery"][..],
            &["bluetoothd", "-Pbattery"],
            &["bluetoothd", "--noplugin=sap,battery"],
            &["bluetoothd", "--noplugin", "*"],
        ] {
            assert!(!probes_phones(STOCK, &args(a)), "{a:?}");
        }
        assert!(probes_phones(STOCK, &args(&["bluetoothd", "-P", "sap"])));
    }

    #[test]
    fn bluetoothd_is_found_under_proc() {
        let t = tempfile::tempdir().unwrap();
        let other = t.path().join("10");
        std::fs::create_dir(&other).unwrap();
        std::fs::write(other.join("comm"), "bash\n").unwrap();
        let bt = t.path().join("603");
        std::fs::create_dir(&bt).unwrap();
        std::fs::write(bt.join("comm"), "bluetoothd\n").unwrap();
        std::fs::write(
            bt.join("cmdline"),
            b"/usr/libexec/bluetooth/bluetoothd\0-P\0battery\0",
        )
        .unwrap();
        assert_eq!(
            bluetoothd_args(t.path()).unwrap(),
            args(&["/usr/libexec/bluetooth/bluetoothd", "-P", "battery"])
        );
    }
}
