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
//! `ble-server`: an app port that serves the [BLE KISS API](https://github.com/hessu/aprs-specs/blob/master/BLE-KISS-API.md)
//! to phones and tablets. One GATT application and one advertisement per
//! port, registered on the port's adapter, so enabling or disabling a
//! bridge touches only its own service.
//!
//! Each connected central is one bridge session, keyed by its address. The
//! phone writes KISS bytes to the TX characteristic (any fragmentation) and
//! subscribes to notifications on RX; frames to the phone are chunked to
//! the notification MTU.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bluer::adv::{Advertisement, AdvertisementHandle};
use bluer::gatt::local::{
    Application, Characteristic, CharacteristicControl, CharacteristicControlEvent,
    CharacteristicNotify, CharacteristicNotifyMethod, CharacteristicWrite,
    CharacteristicWriteMethod, Service, characteristic_control, service_control,
};
use bluer::gatt::{CharacteristicReader, CharacteristicWriter};
use bluer::{Adapter, Address, DeviceEvent, DeviceProperty};
use futures::StreamExt;
use tokio::io::AsyncReadExt;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::bridge::{AppEvent, Frame, SessionId};
use crate::kiss::buffer::KissReassemblyBuffer;
use crate::kiss::frame::FEND;

pub const KISS_SERVICE_UUID: Uuid = Uuid::from_u128(0x00000001_ba2a_46c9_ae49_01b0961f68bb);
pub const KISS_TX_CHAR_UUID: Uuid = Uuid::from_u128(0x00000002_ba2a_46c9_ae49_01b0961f68bb);
pub const KISS_RX_CHAR_UUID: Uuid = Uuid::from_u128(0x00000003_ba2a_46c9_ae49_01b0961f68bb);

/// Frames queued per session on the way to the phone.
const SESSION_QUEUE: usize = 64;
/// A notification that takes longer than this means the phone is gone.
const NOTIFY_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BleState {
    Starting,
    Advertising,
    /// At `max_clients`: connected phones are served, the advertisement is withdrawn.
    Full,
    Failed(String),
    Stopped,
}

pub struct BleServerPort {
    pub bridge_id: String,
    pub port_id: String,
    pub adapter: Adapter,
    /// The bridge's effective public name: advertised, and set as the
    /// adapter alias. It changes after a DNS-SD collision; connected phones
    /// stay connected and the advertisement restarts with the new name.
    pub public_name: watch::Receiver<String>,
    pub max_clients: u32,
    pub app_tx: mpsc::Sender<AppEvent>,
    pub shutdown: CancellationToken,
}

pub struct BleServerHandle {
    pub state: watch::Receiver<BleState>,
    pub sessions: Arc<AtomicUsize>,
}

enum BleEvent {
    /// A client's reader or writer ended, or its device disconnected.
    Gone { addr: Address, generation: u64 },
}

struct Client {
    generation: u64,
    /// Frames from the bridge to this phone; taken by the writer task once
    /// the phone subscribes.
    downlink_rx: Option<mpsc::Receiver<Frame>>,
    writer_attached: bool,
}

impl BleServerPort {
    pub fn spawn(self) -> (BleServerHandle, tokio::task::JoinHandle<()>) {
        let (state_tx, state_rx) = watch::channel(BleState::Starting);
        let sessions = Arc::new(AtomicUsize::new(0));
        let handle = BleServerHandle {
            state: state_rx,
            sessions: sessions.clone(),
        };
        let task = tokio::spawn(self.run(state_tx, sessions));
        (handle, task)
    }

    async fn prepare(&self, public_name: &str) -> Result<(), String> {
        let name = self.adapter.name();
        crate::bluetooth::power_on(&self.adapter).await?;
        // Pairable, so the Just Works bond BlueZ starts when a phone touches a
        // protected characteristic can complete.
        self.adapter
            .set_pairable(true)
            .await
            .map_err(|e| format!("{name}: set pairable: {e}"))?;
        self.set_alias(public_name).await
    }

    /// The adapter's alias is what radios see while pairing and what phones
    /// show in their Bluetooth settings.
    async fn set_alias(&self, public_name: &str) -> Result<(), String> {
        if self.adapter.alias().await.ok().as_deref() != Some(public_name) {
            self.adapter
                .set_alias(public_name.to_string())
                .await
                .map_err(|e| format!("{}: set alias: {e}", self.adapter.name()))?;
        }
        Ok(())
    }

    async fn advertise(&self, public_name: &str) -> bluer::Result<AdvertisementHandle> {
        let adv = Advertisement {
            advertisement_type: bluer::adv::Type::Peripheral,
            service_uuids: std::iter::once(KISS_SERVICE_UUID).collect(),
            local_name: Some(public_name.to_string()),
            discoverable: Some(true),
            ..Default::default()
        };
        self.adapter.advertise(adv).await
    }

    async fn run(self, state_tx: watch::Sender<BleState>, sessions: Arc<AtomicUsize>) {
        let fail = |state_tx: &watch::Sender<BleState>, msg: String| {
            tracing::error!(bridge = %self.bridge_id, port = %self.port_id, "{msg}");
            state_tx.send_replace(BleState::Failed(msg));
        };
        let mut name_rx = self.public_name.clone();
        let mut public_name = name_rx.borrow_and_update().clone();
        let mut follow_name = true;
        if let Err(e) = self.prepare(&public_name).await {
            fail(&state_tx, e);
            return;
        }
        let (app, mut tx_control, mut rx_control) = build_application();
        let _app_handle = match self.adapter.serve_gatt_application(app).await {
            Ok(h) => h,
            Err(e) => {
                fail(&state_tx, format!("can't register the GATT service: {e}"));
                return;
            }
        };
        let mut adv = match self.advertise(&public_name).await {
            Ok(h) => Some(h),
            Err(e) => {
                fail(&state_tx, format!("can't advertise: {e}"));
                return;
            }
        };
        state_tx.send_replace(BleState::Advertising);
        tracing::info!(
            bridge = %self.bridge_id, port = %self.port_id, adapter = self.adapter.name(),
            name = %public_name, max_clients = self.max_clients, "advertising KISS service"
        );

        let (ev_tx, mut ev_rx) = mpsc::channel::<BleEvent>(64);
        let mut clients: HashMap<Address, Client> = HashMap::new();
        let mut generation: u64 = 0;
        let max = self.max_clients as usize;

        loop {
            tokio::select! {
                _ = self.shutdown.cancelled() => break,
                res = name_rx.changed(), if follow_name => {
                    if res.is_err() {
                        follow_name = false;
                        continue;
                    }
                    public_name = name_rx.borrow_and_update().clone();
                    tracing::info!(bridge = %self.bridge_id, port = %self.port_id, name = %public_name, "public name changed");
                    if let Err(e) = self.set_alias(&public_name).await {
                        tracing::warn!(bridge = %self.bridge_id, port = %self.port_id, "{e}");
                    }
                    if adv.take().is_some() {
                        match self.advertise(&public_name).await {
                            Ok(h) => adv = Some(h),
                            Err(e) => tracing::error!(bridge = %self.bridge_id, port = %self.port_id, "can't advertise the new name: {e}"),
                        }
                    }
                }
                ev = tx_control.next() => match ev {
                    Some(CharacteristicControlEvent::Write(req)) => {
                        let addr = req.device_address();
                        if !self.admit(&clients, addr, max).await {
                            drop(req);
                            continue;
                        }
                        let reader = match req.accept() {
                            Ok(r) => r,
                            Err(e) => {
                                tracing::warn!(bridge = %self.bridge_id, port = %self.port_id, %addr, "can't accept a write: {e}");
                                continue;
                            }
                        };
                        let generation = self.ensure_client(&mut clients, &mut generation, addr, &ev_tx, &sessions).await;
                        let id = self.session_id(addr);
                        tokio::spawn(reader_task(reader, id, self.app_tx.clone(), ev_tx.clone(), addr, generation));
                    }
                    Some(_) => {}
                    None => {
                        fail(&state_tx, "GATT write control stream ended".into());
                        break;
                    }
                },
                ev = rx_control.next() => match ev {
                    Some(CharacteristicControlEvent::Notify(notifier)) => {
                        let addr = notifier.device_address();
                        if !self.admit(&clients, addr, max).await {
                            drop(notifier);
                            continue;
                        }
                        let generation = self.ensure_client(&mut clients, &mut generation, addr, &ev_tx, &sessions).await;
                        let client = clients.get_mut(&addr).expect("just ensured");
                        match client.downlink_rx.take() {
                            Some(rx) => {
                                client.writer_attached = true;
                                tracing::info!(bridge = %self.bridge_id, port = %self.port_id, %addr, mtu = notifier.mtu(), "client subscribed");
                                tokio::spawn(writer_task(notifier, rx, ev_tx.clone(), addr, generation));
                            }
                            None => tracing::debug!(bridge = %self.bridge_id, port = %self.port_id, %addr, "repeated subscribe ignored"),
                        }
                        if clients.len() >= max && adv.take().is_some() {
                            tracing::info!(bridge = %self.bridge_id, port = %self.port_id, "max clients reached, advertisement withdrawn");
                            state_tx.send_replace(BleState::Full);
                        }
                    }
                    Some(_) => {}
                    None => {
                        fail(&state_tx, "GATT notify control stream ended".into());
                        break;
                    }
                },
                Some(BleEvent::Gone { addr, generation: g }) = ev_rx.recv() => {
                    let current = clients.get(&addr).map(|c| c.generation);
                    if current != Some(g) {
                        continue; // a stale report from a session already replaced
                    }
                    clients.remove(&addr);
                    sessions.store(clients.len(), Ordering::Relaxed);
                    tracing::info!(bridge = %self.bridge_id, port = %self.port_id, %addr, "client disconnected");
                    let _ = self.app_tx.send(AppEvent::SessionClosed { id: self.session_id(addr) }).await;
                    if clients.len() < max && adv.is_none() {
                        match self.advertise(&public_name).await {
                            Ok(h) => {
                                adv = Some(h);
                                state_tx.send_replace(BleState::Advertising);
                            }
                            Err(e) => tracing::error!(bridge = %self.bridge_id, port = %self.port_id, "can't resume advertising: {e}"),
                        }
                    }
                }
            }
        }

        for addr in clients.keys() {
            let _ = self
                .app_tx
                .send(AppEvent::SessionClosed {
                    id: self.session_id(*addr),
                })
                .await;
        }
        sessions.store(0, Ordering::Relaxed);
        drop(adv);
        state_tx.send_replace(BleState::Stopped);
        tracing::info!(bridge = %self.bridge_id, port = %self.port_id, "stopped");
    }

    fn session_id(&self, addr: Address) -> SessionId {
        SessionId::new(self.port_id.clone(), addr.to_string())
    }

    /// Whether a device may attach: known devices always, new ones while
    /// below `max_clients`. Rejected devices are dropped at the link layer
    /// so they don't sit half-connected.
    async fn admit(&self, clients: &HashMap<Address, Client>, addr: Address, max: usize) -> bool {
        if clients.contains_key(&addr) || clients.len() < max {
            return true;
        }
        tracing::warn!(bridge = %self.bridge_id, port = %self.port_id, %addr, max, "rejecting client: max clients reached");
        if let Ok(device) = self.adapter.device(addr) {
            let _ = device.disconnect().await;
        }
        false
    }

    /// Register a session for `addr` if there isn't one, and tell the bridge.
    async fn ensure_client(
        &self,
        clients: &mut HashMap<Address, Client>,
        generation: &mut u64,
        addr: Address,
        ev_tx: &mpsc::Sender<BleEvent>,
        sessions: &Arc<AtomicUsize>,
    ) -> u64 {
        if let Some(c) = clients.get(&addr) {
            return c.generation;
        }
        *generation += 1;
        let g = *generation;
        let (tx, rx) = mpsc::channel::<Frame>(SESSION_QUEUE);
        clients.insert(
            addr,
            Client {
                generation: g,
                downlink_rx: Some(rx),
                writer_attached: false,
            },
        );
        sessions.store(clients.len(), Ordering::Relaxed);
        tracing::info!(bridge = %self.bridge_id, port = %self.port_id, %addr, "client connected");
        let _ = self
            .app_tx
            .send(AppEvent::SessionOpened {
                id: self.session_id(addr),
                tx,
            })
            .await;
        // Report when the device drops the link, whatever the GATT streams do.
        let adapter = self.adapter.clone();
        let ev_tx = ev_tx.clone();
        tokio::spawn(async move {
            wait_for_disconnect(&adapter, addr).await;
            let _ = ev_tx
                .send(BleEvent::Gone {
                    addr,
                    generation: g,
                })
                .await;
        });
        g
    }
}

/// The KISS service: TX (phone writes) and RX (phone subscribes).
fn build_application() -> (Application, CharacteristicControl, CharacteristicControl) {
    let (_svc_control, svc_handle) = service_control();
    let (tx_control, tx_handle) = characteristic_control();
    let (rx_control, rx_handle) = characteristic_control();
    let service = Service {
        uuid: KISS_SERVICE_UUID,
        primary: true,
        characteristics: vec![
            Characteristic {
                uuid: KISS_TX_CHAR_UUID,
                write: Some(CharacteristicWrite {
                    write: true,
                    write_without_response: true,
                    method: CharacteristicWriteMethod::Io,
                    ..Default::default()
                }),
                control_handle: tx_handle,
                ..Default::default()
            },
            Characteristic {
                uuid: KISS_RX_CHAR_UUID,
                notify: Some(CharacteristicNotify {
                    notify: true,
                    method: CharacteristicNotifyMethod::Io,
                    ..Default::default()
                }),
                control_handle: rx_handle,
                ..Default::default()
            },
        ],
        control_handle: svc_handle,
        ..Default::default()
    };
    let app = Application {
        services: vec![service],
        ..Default::default()
    };
    (app, tx_control, rx_control)
}

/// Bytes from the phone, in whatever fragments BLE delivers, reassembled
/// into frames for the bridge.
async fn reader_task(
    mut reader: CharacteristicReader,
    id: SessionId,
    app_tx: mpsc::Sender<AppEvent>,
    ev_tx: mpsc::Sender<BleEvent>,
    addr: Address,
    generation: u64,
) {
    let mut buf = vec![0u8; 512];
    let mut reassembly = KissReassemblyBuffer::new(4096);
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if let Err(e) = reassembly.push(&buf[..n]) {
                    tracing::warn!(session = %id, "{e}");
                    continue;
                }
                for frame in reassembly.drain_frames() {
                    if app_tx
                        .send(AppEvent::Frame {
                            id: id.clone(),
                            frame,
                        })
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            }
        }
    }
    let _ = ev_tx.send(BleEvent::Gone { addr, generation }).await;
}

/// Frames from the bridge to the phone, delimited and chunked to the MTU.
async fn writer_task(
    writer: CharacteristicWriter,
    mut rx: mpsc::Receiver<Frame>,
    ev_tx: mpsc::Sender<BleEvent>,
    addr: Address,
    generation: u64,
) {
    let mtu = writer.mtu();
    let chunk = if mtu > 0 { mtu } else { 20 };
    while let Some(frame) = rx.recv().await {
        let mut wire = Vec::with_capacity(frame.len() + 2);
        wire.push(FEND);
        wire.extend_from_slice(&frame);
        wire.push(FEND);
        for piece in wire.chunks(chunk) {
            match tokio::time::timeout(NOTIFY_TIMEOUT, writer.send(piece)).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    tracing::debug!(%addr, "notify failed: {e}");
                    let _ = ev_tx.send(BleEvent::Gone { addr, generation }).await;
                    return;
                }
                Err(_) => {
                    tracing::warn!(%addr, "notify timed out; dropping the client");
                    let _ = ev_tx.send(BleEvent::Gone { addr, generation }).await;
                    return;
                }
            }
        }
    }
    // The port closed the session on purpose.
}

async fn wait_for_disconnect(adapter: &Adapter, addr: Address) {
    let Ok(device) = adapter.device(addr) else {
        return;
    };
    let Ok(mut events) = device.events().await else {
        return;
    };
    while let Some(ev) = events.next().await {
        if let DeviceEvent::PropertyChanged(DeviceProperty::Connected(false)) = ev {
            return;
        }
    }
}
