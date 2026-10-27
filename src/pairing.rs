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

//! Finding and pairing radios over Bluetooth Classic, for `bluetooth.scan`,
//! `bluetooth.pair` and `bluetooth.unpair`.
//!
//! Phones never pair with the bridge, but a radio's TNC is reached over
//! RFCOMM, which needs a pairing; the TH-D75 uses SSP numeric comparison.
//! A pairing registers an agent on the daemon's own D-Bus connection for
//! just that pairing. BlueZ hands a `Pair` call to the caller's agent
//! (`agent_get(sender)`), so the agent never needs to be the default, and
//! whatever default agent the system has is left alone.

use std::sync::Arc;
use std::time::Duration;

use bluer::agent::{Agent, ReqError};
use bluer::{Adapter, AdapterEvent, Address, DiscoveryFilter, DiscoveryTransport, Session};
use futures::StreamExt;
use tokio::sync::{Mutex, mpsc};

use crate::control::protocol::{BtDevice, PairingStep};

/// How long a pairing may take, including someone pressing OK on the radio
/// (the TH-D75 gives 30 s).
pub const PAIR_TIMEOUT: Duration = Duration::from_secs(90);
/// How long to look for a device that BlueZ doesn't know yet before
/// pairing it.
const FIND_TIMEOUT: Duration = Duration::from_secs(30);

/// What a Bluetooth name says about the device: (what it is, its model),
/// from the radios in the model table.
pub fn known_tnc(name: Option<&str>) -> Option<(&'static str, &'static str)> {
    crate::config::radio_by_bluetooth_name(name?).map(|r| (r.name, r.model))
}

/// Look for Bluetooth Classic devices on `adapter` for `seconds`. Likely
/// TNCs come first, then the strongest signal. `tnc_for` names the TNC
/// already defined at an address.
pub async fn scan(
    adapter: &Adapter,
    seconds: u64,
    tnc_for: impl Fn(Address) -> Option<String>,
) -> Result<Vec<BtDevice>, String> {
    adapter
        .set_discovery_filter(DiscoveryFilter {
            transport: DiscoveryTransport::BrEdr,
            ..Default::default()
        })
        .await
        .map_err(|e| format!("can't scan on {}: {e}", adapter.name()))?;
    // Discovery runs while the stream lives, so it stops at the end of
    // this block, before the devices are looked at.
    let mut seen: Vec<Address> = Vec::new();
    {
        let events = adapter
            .discover_devices()
            .await
            .map_err(|e| format!("can't scan on {}: {e}", adapter.name()))?;
        futures::pin_mut!(events);
        let _ = tokio::time::timeout(Duration::from_secs(seconds), async {
            while let Some(ev) = events.next().await {
                if let AdapterEvent::DeviceAdded(addr) = ev
                    && !seen.contains(&addr)
                {
                    seen.push(addr);
                }
            }
        })
        .await;
    }

    let mut out = Vec::new();
    for addr in seen {
        let Ok(dev) = adapter.device(addr) else {
            continue;
        };
        // Phones and computers show up too; anything with a BR/EDR name is
        // listed, TNCs first.
        let name = dev.name().await.ok().flatten();
        let known = known_tnc(name.as_deref());
        out.push(BtDevice {
            address: addr.to_string(),
            rssi: dev.rssi().await.ok().flatten(),
            icon: dev.icon().await.ok().flatten(),
            paired: dev.is_paired().await.unwrap_or(false),
            known_tnc: known.map(|k| k.0.to_string()),
            model: known.map(|k| k.1.to_string()),
            tnc: tnc_for(addr),
            name,
        });
    }
    out.sort_by_key(|d| {
        (
            d.known_tnc.is_none(),
            std::cmp::Reverse(d.rssi.unwrap_or(i16::MIN)),
        )
    });
    Ok(out)
}

/// A reply to a [`PairingStep`] that needs one.
#[derive(Debug, Clone, Default)]
pub struct Reply {
    pub accept: Option<bool>,
    pub pin: Option<String>,
    pub passkey: Option<u32>,
}

/// The person on the other end of a pairing: steps go to them, and the
/// replies come back.
pub struct Prompter {
    pub steps: mpsc::Sender<PairingStep>,
    pub replies: Mutex<mpsc::Receiver<Reply>>,
}

impl Prompter {
    fn tell(&self, step: PairingStep) {
        let _ = self.steps.try_send(step);
    }

    async fn ask(&self, step: PairingStep) -> Result<Reply, ReqError> {
        self.steps
            .send(step)
            .await
            .map_err(|_| ReqError::Canceled)?;
        let mut replies = self.replies.lock().await;
        match tokio::time::timeout(PAIR_TIMEOUT, replies.recv()).await {
            Ok(Some(r)) => Ok(r),
            _ => Err(ReqError::Canceled),
        }
    }
}

/// Pair with `addr` on `adapter` and trust it, asking `prompter` what the
/// pairing needs.
pub async fn pair(
    session: &Session,
    adapter: &Adapter,
    addr: Address,
    prompter: Arc<Prompter>,
) -> Result<(), String> {
    let known = adapter.device_addresses().await.unwrap_or_default();
    if !known.contains(&addr) {
        prompter.tell(PairingStep::Searching);
        find(adapter, addr).await?;
    }
    let device = adapter.device(addr).map_err(|e| e.to_string())?;
    if device.is_paired().await.unwrap_or(false) {
        let _ = device.set_trusted(true).await;
        return Ok(());
    }
    prompter.tell(PairingStep::Pairing {
        name: device.name().await.ok().flatten(),
    });

    let p = prompter.clone();
    let confirm = move |req: bluer::agent::RequestConfirmation| {
        let p = p.clone();
        Box::pin(async move {
            let r = p
                .ask(PairingStep::Confirm {
                    passkey: format!("{:06}", req.passkey),
                })
                .await?;
            if r.accept == Some(true) {
                Ok(())
            } else {
                Err(ReqError::Rejected)
            }
        }) as _
    };
    let p = prompter.clone();
    let pin = move |_req: bluer::agent::RequestPinCode| {
        let p = p.clone();
        Box::pin(async move {
            p.ask(PairingStep::EnterPin)
                .await?
                .pin
                .ok_or(ReqError::Rejected)
        }) as _
    };
    let p = prompter.clone();
    let passkey = move |_req: bluer::agent::RequestPasskey| {
        let p = p.clone();
        Box::pin(async move {
            p.ask(PairingStep::EnterPasskey)
                .await?
                .passkey
                .ok_or(ReqError::Rejected)
        }) as _
    };
    let p = prompter.clone();
    let show_pin = move |req: bluer::agent::DisplayPinCode| {
        p.tell(PairingStep::ShowPin { pin: req.pincode });
        Box::pin(async { Ok(()) }) as _
    };
    let p = prompter.clone();
    let show_passkey = move |req: bluer::agent::DisplayPasskey| {
        p.tell(PairingStep::ShowPasskey {
            passkey: format!("{:06}", req.passkey),
        });
        Box::pin(async { Ok(()) }) as _
    };
    let p = prompter.clone();
    let authorize = move |_req: bluer::agent::RequestAuthorization| {
        let p = p.clone();
        Box::pin(async move {
            if p.ask(PairingStep::Authorize).await?.accept == Some(true) {
                Ok(())
            } else {
                Err(ReqError::Rejected)
            }
        }) as _
    };
    // All of them, so BlueZ sees KeyboardDisplay and can use whichever
    // method the device wants. Dropping the handle unregisters it.
    let _agent = session
        .register_agent(Agent {
            request_default: false,
            request_pin_code: Some(Box::new(pin)),
            display_pin_code: Some(Box::new(show_pin)),
            request_passkey: Some(Box::new(passkey)),
            display_passkey: Some(Box::new(show_passkey)),
            request_confirmation: Some(Box::new(confirm)),
            request_authorization: Some(Box::new(authorize)),
            ..Default::default()
        })
        .await
        .map_err(|e| format!("can't register a pairing agent: {e}"))?;

    match tokio::time::timeout(PAIR_TIMEOUT, device.pair()).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(pair_error(&e)),
        // Dropping the pair() future cancels the pairing.
        Err(_) => return Err("pairing timed out".into()),
    }
    device
        .set_trusted(true)
        .await
        .map_err(|e| format!("paired, but can't mark it trusted: {e}"))
}

/// Forget the pairing with `addr` on `adapter`.
pub async fn unpair(adapter: &Adapter, addr: Address) -> Result<(), String> {
    let known = adapter.device_addresses().await.unwrap_or_default();
    if !known.contains(&addr) {
        return Err(format!("{addr} isn't paired on {}", adapter.name()));
    }
    adapter
        .remove_device(addr)
        .await
        .map_err(|e| format!("can't unpair {addr}: {e}"))
}

/// Run discovery until `addr` turns up.
async fn find(adapter: &Adapter, addr: Address) -> Result<(), String> {
    adapter
        .set_discovery_filter(DiscoveryFilter {
            transport: DiscoveryTransport::BrEdr,
            ..Default::default()
        })
        .await
        .map_err(|e| e.to_string())?;
    let events = adapter
        .discover_devices()
        .await
        .map_err(|e| format!("can't search on {}: {e}", adapter.name()))?;
    futures::pin_mut!(events);
    let found = tokio::time::timeout(FIND_TIMEOUT, async {
        while let Some(ev) = events.next().await {
            if matches!(ev, AdapterEvent::DeviceAdded(a) if a == addr) {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    if found {
        Ok(())
    } else {
        Err(format!(
            "didn't find {addr} on {} in {} s. Is it on, nearby, and in pairing mode \
             (discoverable)?",
            adapter.name(),
            FIND_TIMEOUT.as_secs()
        ))
    }
}

/// What a failed pairing means for someone standing at the radio.
fn pair_error(e: &bluer::Error) -> String {
    use bluer::ErrorKind;
    match &e.kind {
        ErrorKind::AuthenticationFailed => {
            "pairing failed: the device didn't confirm. Confirm on it when it asks (some \
             radios give only 30 seconds), and check the passkeys match"
                .into()
        }
        ErrorKind::AuthenticationCanceled | ErrorKind::AuthenticationRejected => {
            "pairing was declined".into()
        }
        ErrorKind::AuthenticationTimeout => "pairing timed out".into(),
        ErrorKind::ConnectionAttemptFailed => {
            "couldn't reach the device: is it on, nearby, and in pairing mode?".into()
        }
        ErrorKind::AlreadyExists => "it's already paired".into(),
        _ => format!("pairing failed: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radios_are_known_by_their_names() {
        assert_eq!(
            known_tnc(Some("TH-D75")),
            Some(("Kenwood TH-D75", "th-d75"))
        );
        assert_eq!(
            known_tnc(Some("TH-D74 Mike")),
            Some(("Kenwood TH-D74", "th-d74"))
        );
        assert_eq!(known_tnc(Some("jPhone")), None);
        assert_eq!(known_tnc(None), None);
    }
}
