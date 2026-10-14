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

//! BlueZ helpers shared by the Bluetooth ports, the supervisor and `-t`.

use bluer::{Adapter, Address, Session};
use uuid::Uuid;

use crate::config::{AdapterSel, Config, Mac, PortKind, Problem};

/// The Serial Port Profile.
pub const SPP_UUID: Uuid = Uuid::from_u128(0x00001101_0000_1000_8000_00805f9b34fb);

/// The adapter `sel` means right now: by MAC, by `hciN`, or BlueZ's first
/// adapter. `None` if it isn't present.
pub async fn find_adapter(session: &Session, sel: &AdapterSel) -> bluer::Result<Option<Adapter>> {
    let mut names = session.adapter_names().await?;
    names.sort();
    match sel {
        AdapterSel::Default => Ok(names.first().map(|n| session.adapter(n)).transpose()?),
        AdapterSel::Hci(h) => Ok(names
            .iter()
            .find(|n| *n == h)
            .map(|n| session.adapter(n))
            .transpose()?),
        AdapterSel::Mac(mac) => {
            for n in &names {
                let a = session.adapter(n)?;
                if a.address().await.ok().map(|x| Mac(x.0)) == Some(*mac) {
                    return Ok(Some(a));
                }
            }
            Ok(None)
        }
    }
}

/// `hci0 (2C:CF:67:6E:2E:BC)`.
pub async fn describe(adapter: &Adapter) -> String {
    match adapter.address().await {
        Ok(a) => format!("{} ({a})", adapter.name()),
        Err(_) => adapter.name().to_string(),
    }
}

/// Clear an rfkill soft block on `hci`, which BlueZ can't power through.
/// Returns `None` if there was no block, `Some(true)` if it was cleared,
/// `Some(false)` if we lacked permission.
pub fn rfkill_unblock(hci: &str) -> Option<bool> {
    let dir = std::fs::read_dir("/sys/class/rfkill").ok()?;
    for entry in dir.flatten() {
        let p = entry.path();
        if std::fs::read_to_string(p.join("name")).ok()?.trim() != hci {
            continue;
        }
        if std::fs::read_to_string(p.join("soft")).ok()?.trim() != "1" {
            return None;
        }
        return Some(std::fs::write(p.join("soft"), "0\n").is_ok());
    }
    None
}

pub fn rfkill_hint(hci: &str) -> &'static str {
    match rfkill_unblock(hci) {
        Some(false) => " (the adapter is rfkill-blocked and only root can unblock it)",
        _ => "",
    }
}

/// Power an adapter on, clearing an rfkill soft block first if there is one.
pub async fn power_on(adapter: &Adapter) -> Result<(), String> {
    let name = adapter.name();
    if let Some(unblocked) = rfkill_unblock(name) {
        tracing::info!(
            adapter = name,
            "cleared the rfkill soft block{}",
            if unblocked { "" } else { " (no permission)" }
        );
    }
    adapter
        .set_powered(true)
        .await
        .map_err(|e| format!("can't power on adapter {name}: {e}{}", rfkill_hint(name)))
}

/// `-t` host checks: TNCs used by enabled `spp-client` ports that aren't
/// paired on their adapter. `Err` means BlueZ couldn't be asked at all.
pub async fn pairing_warnings(cfg: &Config) -> Result<Vec<Problem>, String> {
    let mut checks = Vec::new();
    for b in cfg.bridges.iter().filter(|b| b.enabled) {
        for p in &b.ports {
            if let PortKind::SppClient { tnc, spp, .. } = &p.kind
                && !checks.iter().any(|(t, _)| t == tnc)
            {
                checks.push((tnc.clone(), spp.clone()));
            }
        }
    }
    if checks.is_empty() {
        return Ok(Vec::new());
    }
    let session = Session::new()
        .await
        .map_err(|e| format!("BlueZ isn't reachable ({e})"))?;
    let mut out = Vec::new();
    for (tnc, spp) in checks {
        let source = cfg.tnc(&tnc).map(|t| t.source.clone());
        let object = format!("tnc {tnc}");
        let adapter = match find_adapter(&session, &spp.adapter).await {
            Ok(Some(a)) => a,
            Ok(None) => {
                out.push(Problem::new(
                    source,
                    object,
                    format!(
                        "{} isn't present, so its pairing can't be checked",
                        describe_sel(cfg, &spp.adapter)
                    ),
                ));
                continue;
            }
            Err(e) => return Err(format!("BlueZ isn't reachable ({e})")),
        };
        let device = adapter.device(Address(spp.address.0));
        let paired = match device {
            Ok(d) => d.is_paired().await.unwrap_or(false),
            Err(_) => false,
        };
        if !paired {
            out.push(Problem::new(
                source,
                object,
                format!(
                    "{} is not paired on adapter {}; pair it with bluetoothctl (see the README)",
                    spp.address,
                    describe(&adapter).await
                ),
            ));
        }
    }
    Ok(out)
}

fn describe_sel(cfg: &Config, sel: &AdapterSel) -> String {
    match sel {
        AdapterSel::Default => "the default Bluetooth adapter".into(),
        other => format!("adapter {}", cfg.describe_adapter(other)),
    }
}
