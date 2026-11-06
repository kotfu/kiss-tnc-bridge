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

//! `ble-client`: a TNC over Bluetooth Low Energy that follows the
//! [BLE KISS API](https://github.com/hessu/aprs-specs/blob/master/BLE-KISS-API.md),
//! such as the one in an Ailunce HA2. The bridge plays the phone's part:
//! it connects to the TNC, subscribes to the RX characteristic, and writes
//! frames to the TX characteristic.
//!
//! Some radios carry plain KISS on a GATT service of their own instead;
//! the TNC's `model` picks the service and characteristics (see
//! [`GattProfile`]). The TIDRADIO TD-H9 is one.
//!
//! Both characteristics go through BlueZ's file-descriptor I/O
//! (`AcquireNotify` and `AcquireWrite`), like the `ble-server` side, so the
//! link is a byte stream with no D-Bus call per notification. Writes are
//! split to the MTU by the writer; the TNC reassembles KISS frames itself.
//!
//! BlueZ forgets an unpaired LE device a little while after it last heard
//! it, so a connect first scans for the TNC when BlueZ doesn't know it.
//! When the bridge lets go of the link, the LE connection is dropped too,
//! so the TNC advertises again and a phone can reach it directly.

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use bluer::gatt::remote::Characteristic;
use bluer::gatt::{CharacteristicReader, CharacteristicWriter};
use bluer::{
    Adapter, AdapterEvent, Address, Device, DeviceEvent, DeviceProperty, DiscoveryFilter,
    DiscoveryTransport, Session,
};
use futures::StreamExt;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use uuid::Uuid;

use crate::bluetooth;
use crate::bridge::{ConnectFuture, Opened, TncConnector};
use crate::config::{Ble, TncModel};
use crate::error::Error;
use crate::port::ble_server::{KISS_RX_CHAR_UUID, KISS_SERVICE_UUID, KISS_TX_CHAR_UUID};
use crate::port::tnc_init::{self, Prefixed};

/// How long to listen for the TNC's advertisement when BlueZ doesn't know
/// it. TNCs advertise every second or so while nothing is connected.
const DISCOVER_TIMEOUT: Duration = Duration::from_secs(15);
/// Longest the LE connect may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// Longest BlueZ may take to read the TNC's GATT services once connected.
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(15);

/// Where a TNC's KISS link is in GATT: a service with a characteristic
/// the bridge writes frames to and one it gets them from as notifications.
#[derive(Debug, PartialEq, Eq)]
pub struct GattProfile {
    pub service: Uuid,
    /// App → TNC.
    pub tx: Uuid,
    /// TNC → app.
    pub rx: Uuid,
    /// For errors: what the service is.
    what: &'static str,
}

/// The [BLE KISS API](https://github.com/hessu/aprs-specs/blob/master/BLE-KISS-API.md).
pub const KISS_API: GattProfile = GattProfile {
    service: KISS_SERVICE_UUID,
    tx: KISS_TX_CHAR_UUID,
    rx: KISS_RX_CHAR_UUID,
    what: "the BLE KISS service",
};

/// The TIDRADIO TD-H9: plain KISS, with every frame padded with extra
/// FENDs (which reassembly skips).
pub const TD_H9: GattProfile = GattProfile {
    service: Uuid::from_u128(0x0000af00_0000_1000_8000_00805f9b34fb),
    tx: Uuid::from_u128(0x0000af01_0000_1000_8000_00805f9b34fb),
    rx: Uuid::from_u128(0x0000af02_0000_1000_8000_00805f9b34fb),
    what: "TIDRADIO's KISS service (af00)",
};

pub fn gatt_profile(model: TncModel) -> &'static GattProfile {
    match model {
        TncModel::TdH9 => &TD_H9,
        TncModel::Generic | TncModel::ThD7x => &KISS_API,
    }
}

pub struct BleConnector {
    session: Session,
    tnc_id: String,
    ble: Ble,
    model: TncModel,
    /// As for `spp-client`: the adapter is powered on at the first connect
    /// if it needs to be, and after that left for whoever turned it off.
    tried_power: Arc<AtomicBool>,
}

pub fn connector(session: Session, tnc_id: String, ble: Ble, model: TncModel) -> BleConnector {
    BleConnector {
        session,
        tnc_id,
        ble,
        model,
        tried_power: Arc::default(),
    }
}

impl TncConnector for BleConnector {
    fn connect(&self) -> ConnectFuture {
        let session = self.session.clone();
        let tnc_id = self.tnc_id.clone();
        let ble = self.ble.clone();
        let model = self.model;
        let tried_power = self.tried_power.clone();
        Box::pin(async move {
            let fail = |msg: String| Error::Connect(format!("tnc {tnc_id}: {msg}"));
            let adapter = bluetooth::find_adapter(&session, &ble.adapter)
                .await
                .map_err(|e| fail(format!("BlueZ: {e}")))?
                .ok_or_else(|| fail(format!("{} isn't present", ble.adapter)))?;
            let adapter_desc = bluetooth::describe(&adapter).await;
            if !adapter.is_powered().await.unwrap_or(false) {
                if tried_power.swap(true, Ordering::Relaxed) {
                    return Err(fail(format!("adapter {adapter_desc} is powered off")));
                }
                bluetooth::power_on(&adapter).await.map_err(fail)?;
            }
            tried_power.store(true, Ordering::Relaxed);

            let addr = Address(ble.address.0);
            let device = find_device(&adapter, addr).await.map_err(fail)?;
            let link = open(&device, gatt_profile(model)).await.map_err(|e| {
                // Don't leave a half-set-up LE link holding the TNC.
                let d = device.clone();
                tokio::spawn(async move { d.disconnect().await });
                fail(e)
            })?;
            tracing::debug!(tnc = %tnc_id, %addr, adapter = %adapter_desc, mtu = link.writer.mtu(), "BLE link up");
            let mut link = link;
            let out = tnc_init::init(&mut link, model).await?;
            Ok(Opened {
                link: Box::new(Prefixed::new(link, &out.frames)),
                exit_kiss: out.exit_kiss,
            })
        })
    }

    fn describe(&self) -> String {
        format!("tnc {} at {} over BLE", self.tnc_id, self.ble.address)
    }
}

/// The TNC's BlueZ device, scanning for it first if BlueZ doesn't know it.
/// The scan isn't filtered on a service: not every TNC advertises the
/// service its KISS link is on (the TD-H9 doesn't).
async fn find_device(adapter: &Adapter, addr: Address) -> Result<Device, String> {
    let known = adapter
        .device_addresses()
        .await
        .map_err(|e| format!("BlueZ: {e}"))?
        .contains(&addr);
    if !known {
        adapter
            .set_discovery_filter(DiscoveryFilter {
                transport: DiscoveryTransport::Le,
                ..Default::default()
            })
            .await
            .map_err(|e| format!("can't scan on {}: {e}", adapter.name()))?;
        let events = adapter
            .discover_devices()
            .await
            .map_err(|e| format!("can't scan on {}: {e}", adapter.name()))?;
        futures::pin_mut!(events);
        let heard = async {
            while let Some(ev) = events.next().await {
                if matches!(ev, AdapterEvent::DeviceAdded(a) if a == addr) {
                    return true;
                }
            }
            false
        };
        if !tokio::time::timeout(DISCOVER_TIMEOUT, heard)
            .await
            .unwrap_or(false)
        {
            return Err(format!(
                "not advertising: off, out of range, its Bluetooth is off, or something else is connected to it (nothing heard in {}s)",
                DISCOVER_TIMEOUT.as_secs()
            ));
        }
    }
    adapter.device(addr).map_err(|e| format!("BlueZ: {e}"))
}

/// Connect to the TNC and open both KISS characteristics.
async fn open(device: &Device, profile: &GattProfile) -> Result<BleLink, String> {
    if !device.is_connected().await.unwrap_or(false) {
        tokio::time::timeout(CONNECT_TIMEOUT, device.connect())
            .await
            .map_err(|_| format!("no connection after {}s", CONNECT_TIMEOUT.as_secs()))?
            .map_err(|e| explain(&e))?;
    }
    tokio::time::timeout(RESOLVE_TIMEOUT, services_resolved(device))
        .await
        .map_err(|_| {
            format!(
                "its GATT services weren't read within {}s",
                RESOLVE_TIMEOUT.as_secs()
            )
        })??;

    let (tx, rx) = kiss_characteristics(device, profile).await?;
    let reader = rx
        .notify_io()
        .await
        .map_err(|e| format!("can't subscribe to the KISS RX characteristic: {e}"))?;
    let writer = tx
        .write_io()
        .await
        .map_err(|e| format!("can't open the KISS TX characteristic: {e}"))?;
    Ok(BleLink {
        reader,
        writer,
        device: device.clone(),
    })
}

async fn services_resolved(device: &Device) -> Result<(), String> {
    let events = device.events().await.map_err(|e| format!("BlueZ: {e}"))?;
    futures::pin_mut!(events);
    if device.is_services_resolved().await.unwrap_or(false) {
        return Ok(());
    }
    while let Some(ev) = events.next().await {
        match ev {
            DeviceEvent::PropertyChanged(DeviceProperty::ServicesResolved(true)) => return Ok(()),
            DeviceEvent::PropertyChanged(DeviceProperty::Connected(false)) => {
                return Err("it disconnected while its services were read".into());
            }
            _ => {}
        }
    }
    Err("BlueZ stopped reporting on the device".into())
}

/// The TX (app → TNC) and RX (TNC → app) characteristics.
async fn kiss_characteristics(
    device: &Device,
    profile: &GattProfile,
) -> Result<(Characteristic, Characteristic), String> {
    let bluez = |e: bluer::Error| format!("BlueZ: {e}");
    let mut uuids = Vec::new();
    for service in device.services().await.map_err(bluez)? {
        let uuid = service.uuid().await.map_err(bluez)?;
        if uuid != profile.service {
            uuids.push(uuid);
            continue;
        }
        let (mut tx, mut rx) = (None, None);
        for c in service.characteristics().await.map_err(bluez)? {
            match c.uuid().await.map_err(bluez)? {
                u if u == profile.tx => tx = Some(c),
                u if u == profile.rx => rx = Some(c),
                _ => {}
            }
        }
        return match (tx, rx) {
            (Some(tx), Some(rx)) => Ok((tx, rx)),
            _ => Err(format!(
                "{} is missing its TX or RX characteristic",
                profile.what
            )),
        };
    }
    let mut msg = format!("it doesn't offer {}", profile.what);
    if *profile == KISS_API && uuids.contains(&TD_H9.service) {
        msg.push_str("; if it's a TIDRADIO TD-H9, set its model = \"td-h9\"");
    } else {
        msg.push_str("; is its KISS TNC turned on?");
    }
    Err(msg)
}

/// BlueZ's errors, with what they usually mean for a radio.
fn explain(e: &bluer::Error) -> String {
    let msg = e.to_string();
    if msg.contains("abort-by-local") || msg.contains("Host is down") || msg.contains("timeout") {
        format!(
            "{msg}: the radio didn't answer (off, out of range, its Bluetooth is off, or something else is connected to it)"
        )
    } else {
        msg
    }
}

/// The KISS characteristics as one byte stream. Dropping it drops the LE
/// connection.
pub struct BleLink {
    reader: CharacteristicReader,
    writer: CharacteristicWriter,
    device: Device,
}

impl Drop for BleLink {
    fn drop(&mut self) {
        let device = self.device.clone();
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                let _ = device.disconnect().await;
            });
        }
    }
}

impl AsyncRead for BleLink {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.reader).poll_read(cx, buf)
    }
}

impl AsyncWrite for BleLink {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.writer).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.writer).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.writer).poll_shutdown(cx)
    }
}
