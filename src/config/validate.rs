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

//! Validation of the effective config: the rules shared by `-t`, startup,
//! and later reload and `config.check`.
//!
//! Errors never depend on the host, so a config that passes in CI passes on
//! the device. Warnings include best-effort host checks.

use std::collections::{HashMap, HashSet};

use super::resolve::{AdapterSel, Config, Listen, PortKind};
use super::{Problem, Source};

/// BLE advertising limits the name to about this many bytes of UTF-8.
pub const MAX_BLE_NAME_BYTES: usize = 29;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    pub errors: Vec<Problem>,
    pub warnings: Vec<Problem>,
}

impl Report {
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Check `cfg`. With `host_checks`, also look at what this host has (network
/// interfaces), which `-t` wants and unit tests don't.
pub fn validate(cfg: &Config, host_checks: bool) -> Report {
    let mut r = Report::default();
    let err = |r: &mut Report, source: &Source, object: &str, msg: String| {
        r.errors
            .push(Problem::new(Some(source.clone()), object, msg));
    };
    let warn = |r: &mut Report, source: &Source, object: &str, msg: String| {
        r.warnings
            .push(Problem::new(Some(source.clone()), object, msg));
    };

    // ── Duplicate definitions ───────────────────────────────────────
    let mut seen_adapter_ids: HashMap<&str, &Source> = HashMap::new();
    let mut seen_macs: HashMap<_, (&str, &Source)> = HashMap::new();
    for a in &cfg.adapters {
        let object = format!("adapter {}", a.id);
        if let Some(first) = seen_adapter_ids.insert(&a.id, &a.source) {
            err(
                &mut r,
                &a.source,
                &object,
                format!("duplicate adapter id (also defined in {first})"),
            );
        }
        if let Some((id, first)) = seen_macs.insert(a.mac, (&a.id, &a.source)) {
            err(
                &mut r,
                &a.source,
                &object,
                format!("MAC {} is already adapter {id} ({first})", a.mac),
            );
        }
    }

    let mut seen_bridge_ids: HashMap<&str, &Source> = HashMap::new();
    let mut seen_bridge_names: HashMap<&str, (&str, &Source)> = HashMap::new();
    for b in &cfg.bridges {
        let object = format!("bridge {}", b.id);
        if let Some(first) = seen_bridge_ids.insert(&b.id, &b.source) {
            err(
                &mut r,
                &b.source,
                &object,
                format!("duplicate bridge id (also defined in {first})"),
            );
        }
        if let Some((id, first)) = seen_bridge_names.insert(&b.name, (&b.id, &b.source)) {
            err(
                &mut r,
                &b.source,
                &object,
                format!("name {:?} is already used by bridge {id} ({first})", b.name),
            );
        }

        // ── Per-bridge structure ────────────────────────────────────
        let mut seen_ports: HashSet<&str> = HashSet::new();
        for p in &b.ports {
            if !seen_ports.insert(&p.id) {
                err(
                    &mut r,
                    &b.source,
                    &format!("{object} port {}", p.id),
                    "duplicate port id within the bridge".into(),
                );
            }
        }
        let tnc = b.tnc_ports().count();
        let app = b.app_ports().count();
        if tnc != 1 {
            err(
                &mut r,
                &b.source,
                &object,
                format!(
                    "a bridge needs exactly one TNC port (a tcp-client, or a port with role = \"tnc\"); this one has {tnc}"
                ),
            );
        }
        if app == 0 {
            err(
                &mut r,
                &b.source,
                &object,
                "a bridge needs at least one app port (a ble-server or tcp-server)".into(),
            );
        }

        // ── Per-port warnings ───────────────────────────────────────
        let has_ble = b
            .ports
            .iter()
            .any(|p| matches!(p.kind, PortKind::BleServer { .. }));
        if has_ble && b.public_name.len() > MAX_BLE_NAME_BYTES {
            warn(
                &mut r,
                &b.source,
                &object,
                format!(
                    "public name {:?} is {} bytes; BLE advertising may truncate names over {MAX_BLE_NAME_BYTES}",
                    b.public_name,
                    b.public_name.len()
                ),
            );
        }
        for p in &b.ports {
            let pobject = format!("{object} port {}", p.id);
            if let PortKind::TcpServer {
                listen,
                allow,
                announce,
                ..
            } = &p.kind
            {
                if listen.is_unspecified() && allow.is_empty() {
                    warn(
                        &mut r,
                        &b.source,
                        &pobject,
                        format!(
                            "listen = \"{listen}\" accepts KISS from any host, and anyone who connects can transmit on your radio; add an allow list or bind to an interface or address"
                        ),
                    );
                }
                if *announce && listen.is_loopback() {
                    warn(
                        &mut r,
                        &b.source,
                        &pobject,
                        "announce = true on a loopback binding: nothing on the LAN could connect to what's announced".into(),
                    );
                }
                if host_checks
                    && b.enabled
                    && let Listen::Interface { name, .. } = listen
                    && !interface_exists(name)
                {
                    warn(
                        &mut r,
                        &b.source,
                        &pobject,
                        format!(
                            "interface {name:?} doesn't exist right now; the port will wait for it"
                        ),
                    );
                }
            }
        }
    }

    // ── Conflicts among enabled bridges ─────────────────────────────
    let enabled: Vec<_> = cfg.bridges.iter().filter(|b| b.enabled).collect();
    for (i, a) in enabled.iter().enumerate() {
        for b in &enabled[i + 1..] {
            if a.public_name == b.public_name {
                err(
                    &mut r,
                    &b.source,
                    &format!("bridge {}", b.id),
                    format!(
                        "public name {:?} is also used by enabled bridge {}; apps couldn't tell them apart",
                        b.public_name, a.id
                    ),
                );
            }
            for pa in &a.ports {
                for pb in &b.ports {
                    match (&pa.kind, &pb.kind) {
                        (
                            PortKind::TcpServer { listen: la, .. },
                            PortKind::TcpServer { listen: lb, .. },
                        ) if la.conflicts_with(lb) => {
                            err(
                                &mut r,
                                &b.source,
                                &format!("bridge {} port {}", b.id, pb.id),
                                format!(
                                    "listen = \"{lb}\" conflicts with enabled bridge {} port {} (\"{la}\")",
                                    a.id, pa.id
                                ),
                            );
                        }
                        (
                            PortKind::BleServer { adapter: aa, .. },
                            PortKind::BleServer { adapter: ab, .. },
                        ) if aa == ab => {
                            err(
                                &mut r,
                                &b.source,
                                &format!("bridge {} port {}", b.id, pb.id),
                                format!(
                                    "ble-server on {} is also used by enabled bridge {} port {}; BlueZ would show both KISS services to every phone (one enabled ble-server per adapter)",
                                    describe_adapter(ab),
                                    a.id,
                                    pa.id
                                ),
                            );
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    if !cfg.bridges.is_empty() && enabled.is_empty() {
        r.warnings
            .push(Problem::new(None, "", "no bridge is enabled"));
    }
    if cfg.bridges.is_empty() {
        r.warnings
            .push(Problem::new(None, "", "no bridges are defined"));
    }

    r
}

fn describe_adapter(a: &AdapterSel) -> String {
    match a {
        AdapterSel::Default => "the default adapter".into(),
        other => format!("adapter {other}"),
    }
}

fn interface_exists(name: &str) -> bool {
    let Ok(c) = std::ffi::CString::new(name) else {
        return false;
    };
    // SAFETY: if_nametoindex takes a NUL-terminated string and returns 0 if
    // no interface has that name.
    unsafe { libc::if_nametoindex(c.as_ptr()) != 0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::load::parse_str;
    use crate::config::resolve::resolve;

    fn cfg(text: &str) -> Config {
        let file = parse_str(text).unwrap();
        resolve(&[(Source::Static("test.toml".into()), file)]).unwrap_or_else(|e| panic!("{e:?}"))
    }

    fn errors(text: &str) -> Vec<String> {
        validate(&cfg(text), false)
            .errors
            .iter()
            .map(|p| p.to_string())
            .collect()
    }

    fn warnings(text: &str) -> Vec<String> {
        validate(&cfg(text), false)
            .warnings
            .iter()
            .map(|p| p.to_string())
            .collect()
    }

    const HOME: &str = r#"
[[bridge]]
id = "ktbr1"
name = "Graywolf at home"
  [[bridge.port]]
  type = "tcp-client"
  address = "127.0.0.1:6700"
  [[bridge.port]]
  type = "ble-server"
  [[bridge.port]]
  type = "tcp-server"
  listen = "8001"
"#;

    #[test]
    fn the_home_example_is_valid() {
        let report = validate(&cfg(HOME), false);
        assert!(report.is_ok(), "{:?}", report.errors);
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    }

    #[test]
    fn structural_rules() {
        let e = errors("[[bridge]]\nid = \"ktbr0\"\nname = \"x\"\n");
        assert!(
            e.iter().any(|m| m.contains("exactly one TNC port")),
            "{e:?}"
        );
        assert!(
            e.iter().any(|m| m.contains("at least one app port")),
            "{e:?}"
        );

        // Two TNC ports, even on a disabled bridge.
        let e = errors(
            "[[bridge]]\nid = \"ktbr0\"\nname = \"x\"\nenabled = false\n[[bridge.port]]\ntype = \"tcp-client\"\naddress = \"a:1\"\n[[bridge.port]]\ntype = \"tcp-client\"\naddress = \"b:1\"\n[[bridge.port]]\ntype = \"ble-server\"\n",
        );
        assert!(e.iter().any(|m| m.contains("this one has 2")), "{e:?}");

        // A role override makes a tcp-client an app port.
        let e = errors(
            "[[bridge]]\nid = \"ktbr0\"\nname = \"x\"\n[[bridge.port]]\ntype = \"tcp-client\"\naddress = \"a:1\"\n[[bridge.port]]\ntype = \"tcp-client\"\naddress = \"b:1\"\nrole = \"app\"\n",
        );
        assert!(e.is_empty(), "{e:?}");
    }

    #[test]
    fn duplicates() {
        let e = errors(
            "[[bridge]]\nid = \"ktbr0\"\nname = \"x\"\n[[bridge.port]]\nid = \"p\"\ntype = \"tcp-client\"\naddress = \"a:1\"\n[[bridge.port]]\nid = \"p\"\ntype = \"ble-server\"\n",
        );
        assert!(e.iter().any(|m| m.contains("duplicate port id")), "{e:?}");

        let two = format!("{HOME}{}", HOME.replace("Graywolf at home", "other"));
        let e = errors(&two);
        assert!(e.iter().any(|m| m.contains("duplicate bridge id")), "{e:?}");

        let e = errors(&format!("{HOME}{}", HOME.replace("ktbr1", "ktbr2")));
        assert!(
            e.iter()
                .any(|m| m.contains("is already used by bridge ktbr1")),
            "{e:?}"
        );

        let e = errors(
            "[[adapter]]\nid = \"a\"\nmac = \"00:11:22:33:44:55\"\n[[adapter]]\nid = \"b\"\nmac = \"00:11:22:33:44:55\"\n",
        );
        assert!(
            e.iter().any(|m| m.contains("is already adapter a")),
            "{e:?}"
        );
        let e = errors(
            "[[adapter]]\nid = \"a\"\nmac = \"00:11:22:33:44:55\"\n[[adapter]]\nid = \"a\"\nmac = \"00:11:22:33:44:66\"\n",
        );
        assert!(
            e.iter().any(|m| m.contains("duplicate adapter id")),
            "{e:?}"
        );
    }

    #[test]
    fn conflicts_only_among_enabled_bridges() {
        let other = HOME
            .replace("ktbr1", "ktbr2")
            .replace("Graywolf at home", "Other");
        // Same listen socket, same default adapter: two errors.
        let e = errors(&format!("{HOME}{other}"));
        assert!(
            e.iter()
                .any(|m| m.contains("listen = \"127.0.0.1:8001\" conflicts")),
            "{e:?}"
        );
        assert!(
            e.iter()
                .any(|m| m.contains("ble-server on the default adapter")),
            "{e:?}"
        );
        // Disable one: fine.
        let e = errors(&format!(
            "{HOME}{}",
            other.replace("name = \"Other\"", "name = \"Other\"\nenabled = false")
        ));
        assert!(e.is_empty(), "{e:?}");
        // Different adapters and sockets: fine.
        let e = errors(&format!(
            "{HOME}{}",
            other
                .replace(
                    "type = \"ble-server\"",
                    "type = \"ble-server\"\nadapter = \"hci1\""
                )
                .replace("listen = \"8001\"", "listen = \"8002\"")
        ));
        assert!(e.is_empty(), "{e:?}");
        // Same public name.
        let e = errors(&format!(
            "{HOME}{}",
            other
                .replace(
                    "name = \"Other\"",
                    "name = \"Other\"\npublic_name = \"Graywolf at home\""
                )
                .replace(
                    "type = \"ble-server\"",
                    "type = \"ble-server\"\nadapter = \"hci1\""
                )
                .replace("listen = \"8001\"", "listen = \"8002\"")
        ));
        assert!(
            e.iter()
                .any(|m| m.contains("public name \"Graywolf at home\" is also used")),
            "{e:?}"
        );
    }

    #[test]
    fn warnings_cover_exposure_and_names() {
        let w = warnings(&HOME.replace("listen = \"8001\"", "listen = \"0.0.0.0:8001\""));
        assert!(
            w.iter().any(|m| m.contains("accepts KISS from any host")),
            "{w:?}"
        );
        let w = warnings(&HOME.replace(
            "listen = \"8001\"",
            "listen = \"0.0.0.0:8001\"\nallow = [\"192.168.4.0/24\"]",
        ));
        assert!(w.is_empty(), "{w:?}");
        let w = warnings(&HOME.replace("listen = \"8001\"", "listen = \"8001\"\nannounce = true"));
        assert!(w.iter().any(|m| m.contains("loopback")), "{w:?}");
        let w = warnings(&HOME.replace(
            "Graywolf at home",
            "Graywolf at home with a much longer name",
        ));
        assert!(w.iter().any(|m| m.contains("may truncate")), "{w:?}");
        let w = warnings(&HOME.replace(
            "name = \"Graywolf at home\"",
            "name = \"Graywolf at home\"\nenabled = false",
        ));
        assert!(
            w.iter().any(|m| m.contains("no bridge is enabled")),
            "{w:?}"
        );
        let w = warnings("");
        assert!(
            w.iter().any(|m| m.contains("no bridges are defined")),
            "{w:?}"
        );
    }

    #[test]
    fn interface_check_is_host_only() {
        let text = HOME.replace("listen = \"8001\"", "listen = \"nosuchif9:8001\"");
        assert!(validate(&cfg(&text), false).warnings.is_empty());
        let w: Vec<String> = validate(&cfg(&text), true)
            .warnings
            .iter()
            .map(|p| p.to_string())
            .collect();
        assert!(
            w.iter().any(|m| m.contains("doesn't exist right now")),
            "{w:?}"
        );
    }
}
