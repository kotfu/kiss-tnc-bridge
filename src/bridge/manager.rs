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

use std::collections::HashMap;
use std::time::Duration;

use bluer::adv::{Advertisement, AdvertisementHandle};
use bluer::gatt::local::{CharacteristicControl, CharacteristicControlEvent};
use bluer::gatt::CharacteristicWriter;
use bluer::Address;
use futures::StreamExt;
use tokio::sync::mpsc;

use crate::ble::session::BleClientSession;
use crate::bridge::tcp::TcpKissConnection;
use crate::config::TncConfig;
use crate::error::Error;
use crate::kiss::frame::FEND;

enum BleEvent {
    Data(Address, Vec<u8>),
    Disconnected(Address),
}

pub struct BridgeManager {
    config: TncConfig,
    adapter: bluer::Adapter,
    tx_control: CharacteristicControl,
    rx_control: CharacteristicControl,
}

impl BridgeManager {
    pub fn new(
        config: TncConfig,
        adapter: bluer::Adapter,
        tx_control: CharacteristicControl,
        rx_control: CharacteristicControl,
    ) -> Self {
        Self {
            config,
            adapter,
            tx_control,
            rx_control,
        }
    }

    /// Forcibly disconnect a BLE device at the link layer.
    async fn disconnect_device(&self, addr: Address, tnc_name: &str) {
        match self.adapter.device(addr) {
            Ok(device) => {
                if let Err(e) = device.disconnect().await {
                    tracing::debug!(
                        tnc = tnc_name,
                        addr = %addr,
                        error = %e,
                        "failed to disconnect rejected BLE client"
                    );
                }
            }
            Err(e) => {
                tracing::debug!(
                    tnc = tnc_name,
                    addr = %addr,
                    error = %e,
                    "could not get device handle to disconnect"
                );
            }
        }
    }

    async fn start_advertising(&self) -> Result<AdvertisementHandle, Error> {
        let adv = Advertisement {
            advertisement_type: bluer::adv::Type::Peripheral,
            service_uuids: vec![
                uuid::Uuid::from_u128(0x00000001_ba2a_46c9_ae49_01b0961f68bb),
            ]
            .into_iter()
            .collect(),
            local_name: Some(self.config.name.clone()),
            discoverable: Some(true),
            ..Default::default()
        };
        let handle = self.adapter.advertise(adv).await?;
        tracing::info!(tnc = self.config.name, "advertising BLE service");
        Ok(handle)
    }

    pub async fn run(mut self) -> Result<(), Error> {
        let mut clients: HashMap<Address, BleClientSession> = HashMap::new();
        let mut writers: HashMap<Address, mpsc::Sender<Vec<u8>>> = HashMap::new();
        let mut tcp: Option<TcpKissConnection> = None;
        let mut tcp_reconnect_delay = Duration::from_secs(1);
        let mut adv_handle: Option<AdvertisementHandle> =
            Some(self.start_advertising().await?);

        let (ble_event_tx, mut ble_event_rx) = mpsc::channel::<BleEvent>(64);

        let tnc_name = &self.config.name;
        tracing::info!(tnc = tnc_name, "bridge manager started");

        loop {
            tokio::select! {
                evt = self.tx_control.next() => {
                    match evt {
                        Some(CharacteristicControlEvent::Write(req)) => {
                            let addr = req.device_address();
                            if !clients.contains_key(&addr)
                                && writers.len() >= self.config.max_clients
                            {
                                tracing::warn!(
                                    tnc = tnc_name,
                                    addr = %addr,
                                    max = self.config.max_clients,
                                    "rejecting BLE client: max clients reached"
                                );
                                drop(req);
                                self.disconnect_device(addr, tnc_name).await;
                                continue;
                            }

                            match req.accept() {
                                Ok(reader) => {
                                    tracing::info!(
                                        tnc = tnc_name,
                                        addr = %addr,
                                        "BLE client connected (write)"
                                    );
                                    clients.entry(addr).or_insert_with(|| {
                                        BleClientSession::new(addr)
                                    });

                                    if tcp.is_none() {
                                        match TcpKissConnection::connect(
                                            &self.config.host,
                                            self.config.port,
                                        ).await {
                                            Ok(conn) => {
                                                tracing::info!(
                                                    tnc = tnc_name,
                                                    host = self.config.host,
                                                    port = self.config.port,
                                                    "TCP connected"
                                                );
                                                tcp = Some(conn);
                                                tcp_reconnect_delay = Duration::from_secs(1);
                                            }
                                            Err(e) => {
                                                tracing::error!(
                                                    tnc = tnc_name,
                                                    error = %e,
                                                    "TCP connection failed"
                                                );
                                            }
                                        }
                                    }

                                    let tx = ble_event_tx.clone();
                                    tokio::spawn(Self::ble_reader_task(addr, reader, tx));
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        tnc = tnc_name,
                                        addr = %addr,
                                        error = %e,
                                        "failed to accept BLE write"
                                    );
                                }
                            }
                        }
                        Some(_) => {}
                        None => {
                            tracing::info!(tnc = tnc_name, "TX control stream ended");
                            break;
                        }
                    }
                }

                evt = self.rx_control.next() => {
                    match evt {
                        Some(CharacteristicControlEvent::Notify(notifier)) => {
                            let addr = notifier.device_address();
                            // Reconnects from the same address don't count
                            // against the limit — only genuinely new devices.
                            if !writers.contains_key(&addr)
                                && writers.len() >= self.config.max_clients
                            {
                                tracing::warn!(
                                    tnc = tnc_name,
                                    addr = %addr,
                                    max = self.config.max_clients,
                                    "rejecting BLE client: max clients reached"
                                );
                                drop(notifier);
                                self.disconnect_device(addr, tnc_name).await;
                                continue;
                            }
                            tracing::info!(
                                tnc = tnc_name,
                                addr = %addr,
                                "BLE client subscribed (notify)"
                            );
                            let (send_tx, send_rx) = mpsc::channel::<Vec<u8>>(32);
                            writers.insert(addr, send_tx);
                            let name = tnc_name.to_string();
                            tokio::spawn(Self::ble_writer_task(
                                self.adapter.clone(),
                                addr,
                                notifier,
                                send_rx,
                                ble_event_tx.clone(),
                                name,
                            ));

                            // Establish the TCP connection on subscribe too,
                            // not only on write — the phone may subscribe for
                            // RX notifications before it ever sends a frame.
                            if tcp.is_none() {
                                match TcpKissConnection::connect(
                                    &self.config.host,
                                    self.config.port,
                                ).await {
                                    Ok(conn) => {
                                        tracing::info!(
                                            tnc = tnc_name,
                                            host = self.config.host,
                                            port = self.config.port,
                                            "TCP connected"
                                        );
                                        tcp = Some(conn);
                                        tcp_reconnect_delay = Duration::from_secs(1);
                                    }
                                    Err(e) => {
                                        tracing::error!(
                                            tnc = tnc_name,
                                            error = %e,
                                            "TCP connection failed"
                                        );
                                    }
                                }
                            }

                            // Stop advertising when full so new devices
                            // don't see us and try to connect.
                            if writers.len() >= self.config.max_clients
                                && adv_handle.is_some()
                            {
                                tracing::info!(
                                    tnc = tnc_name,
                                    "max clients reached, stopping advertisement"
                                );
                                adv_handle = None;
                            }
                        }
                        Some(_) => {}
                        None => {
                            tracing::info!(tnc = tnc_name, "RX control stream ended");
                            break;
                        }
                    }
                }

                Some(event) = ble_event_rx.recv() => {
                    match event {
                        BleEvent::Data(addr, data) => {
                            if let Some(session) = clients.get_mut(&addr) {
                                if let Err(e) = session.kiss_buffer.push(&data) {
                                    tracing::warn!(
                                        tnc = tnc_name,
                                        addr = %addr,
                                        error = %e,
                                        "KISS buffer error"
                                    );
                                    continue;
                                }
                                let frames = session.kiss_buffer.drain_frames();
                                for raw_frame in frames {
                                    let encoded = Self::wrap_with_fend(&raw_frame);
                                    if let Some(ref mut conn) = tcp {
                                        if let Err(e) = conn.write_frame(&encoded).await {
                                            tracing::error!(
                                                tnc = tnc_name,
                                                error = %e,
                                                "TCP write failed"
                                            );
                                            tcp = None;
                                        }
                                    }
                                }
                            }
                        }
                        BleEvent::Disconnected(addr) => {
                            tracing::info!(
                                tnc = tnc_name,
                                addr = %addr,
                                "BLE client disconnected"
                            );
                            clients.remove(&addr);
                            // Only remove the writer if the channel is
                            // actually dead — guards against a reconnect
                            // that already replaced the sender.
                            let writer_dead = writers
                                .get(&addr)
                                .map_or(true, |s| s.is_closed());
                            if writer_dead {
                                writers.remove(&addr);
                            }
                            if clients.is_empty() && writers.is_empty() {
                                if tcp.is_some() {
                                    tracing::info!(
                                        tnc = tnc_name,
                                        "last client disconnected, closing TCP"
                                    );
                                    tcp = None;
                                }
                            }
                            // Resume advertising if we now have capacity.
                            if writers.len() < self.config.max_clients
                                && adv_handle.is_none()
                            {
                                match self.start_advertising().await {
                                    Ok(h) => adv_handle = Some(h),
                                    Err(e) => tracing::error!(
                                        tnc = tnc_name,
                                        error = %e,
                                        "failed to resume advertising"
                                    ),
                                }
                            }
                        }
                    }
                }

                frame_result = async {
                    match tcp.as_mut() {
                        Some(conn) => conn.read_frame().await,
                        None => std::future::pending().await,
                    }
                } => {
                    match frame_result {
                        Ok(raw_frame) => {
                            let encoded = Self::wrap_with_fend(&raw_frame);
                            let removed_any = Self::dispatch_to_clients(
                                &mut writers,
                                encoded,
                                tnc_name,
                            );
                            if removed_any
                                && writers.len() < self.config.max_clients
                                && adv_handle.is_none()
                            {
                                match self.start_advertising().await {
                                    Ok(h) => adv_handle = Some(h),
                                    Err(e) => tracing::error!(
                                        tnc = tnc_name,
                                        error = %e,
                                        "failed to resume advertising"
                                    ),
                                }
                            }
                        }
                        Err(e) => {
                            tracing::error!(
                                tnc = tnc_name,
                                error = %e,
                                "TCP read failed"
                            );
                            tcp = None;
                            if !clients.is_empty() {
                                tracing::info!(
                                    tnc = tnc_name,
                                    delay = ?tcp_reconnect_delay,
                                    "scheduling TCP reconnect"
                                );
                                tokio::time::sleep(tcp_reconnect_delay).await;
                                tcp_reconnect_delay = (tcp_reconnect_delay * 2)
                                    .min(Duration::from_secs(30));
                                match TcpKissConnection::connect(
                                    &self.config.host,
                                    self.config.port,
                                ).await {
                                    Ok(conn) => {
                                        tracing::info!(
                                            tnc = tnc_name,
                                            "TCP reconnected"
                                        );
                                        tcp = Some(conn);
                                        tcp_reconnect_delay = Duration::from_secs(1);
                                    }
                                    Err(e) => {
                                        tracing::error!(
                                            tnc = tnc_name,
                                            error = %e,
                                            "TCP reconnect failed"
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        tracing::info!(tnc = tnc_name, "bridge manager stopped");
        Ok(())
    }

    async fn ble_reader_task(
        addr: Address,
        mut reader: bluer::gatt::CharacteristicReader,
        tx: mpsc::Sender<BleEvent>,
    ) {
        let mut buf = vec![0u8; 512];
        loop {
            use tokio::io::AsyncReadExt;
            match reader.read(&mut buf).await {
                Ok(0) | Err(_) => {
                    let _ = tx.send(BleEvent::Disconnected(addr)).await;
                    break;
                }
                Ok(n) => {
                    if tx.send(BleEvent::Data(addr, buf[..n].to_vec())).await.is_err() {
                        break;
                    }
                }
            }
        }
    }

    fn wrap_with_fend(raw_frame: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(raw_frame.len() + 2);
        out.push(FEND);
        out.extend_from_slice(raw_frame);
        out.push(FEND);
        out
    }

    /// Non-blocking dispatch: sends the frame into each client's channel.
    /// Removes clients whose channels are closed (writer task exited) or
    /// full (client can't keep up). Never awaits, never blocks the select
    /// loop. Returns true if any clients were removed.
    fn dispatch_to_clients(
        writers: &mut HashMap<Address, mpsc::Sender<Vec<u8>>>,
        frame_bytes: Vec<u8>,
        tnc_name: &str,
    ) -> bool {
        let mut to_remove = Vec::new();
        for (&addr, sender) in writers.iter() {
            if let Err(e) = sender.try_send(frame_bytes.clone()) {
                tracing::warn!(
                    tnc = tnc_name,
                    addr = %addr,
                    error = %e,
                    "BLE client send channel failed, removing"
                );
                to_remove.push(addr);
            }
        }
        let removed = !to_remove.is_empty();
        for addr in to_remove {
            writers.remove(&addr);
        }
        removed
    }

    /// Per-client task: owns the CharacteristicWriter and drains the
    /// channel, writing BLE notifications with a timeout. Also monitors
    /// the BLE connection state so we detect disconnects promptly even
    /// when no TCP data is flowing. On exit (for any reason), sends a
    /// disconnect event to the main loop.
    async fn ble_writer_task(
        adapter: bluer::Adapter,
        addr: Address,
        mut writer: CharacteristicWriter,
        mut rx: mpsc::Receiver<Vec<u8>>,
        ble_event_tx: mpsc::Sender<BleEvent>,
        tnc_name: String,
    ) {
        let mtu = writer.mtu();
        let chunk_size = if mtu > 0 { mtu as usize } else { 20 };

        let exit_reason = loop {
            tokio::select! {
                frame = rx.recv() => {
                    match frame {
                        Some(frame_bytes) => {
                            let mut failed = false;
                            for chunk in frame_bytes.chunks(chunk_size) {
                                match tokio::time::timeout(
                                    Duration::from_secs(2),
                                    writer.send(chunk),
                                ).await {
                                    Ok(Ok(())) => {}
                                    Ok(Err(e)) => {
                                        tracing::warn!(
                                            tnc = &tnc_name,
                                            addr = %addr,
                                            error = %e,
                                            "BLE notify failed"
                                        );
                                        failed = true;
                                        break;
                                    }
                                    Err(_) => {
                                        tracing::warn!(
                                            tnc = &tnc_name,
                                            addr = %addr,
                                            "BLE notify timed out, removing stale client"
                                        );
                                        failed = true;
                                        break;
                                    }
                                }
                            }
                            if failed {
                                break "write failed";
                            }
                        }
                        None => break "channel closed",
                    }
                }
                _ = Self::wait_for_disconnect(&adapter, addr) => {
                    break "BLE disconnected";
                }
            }
        };

        tracing::info!(
            tnc = &tnc_name,
            addr = %addr,
            reason = exit_reason,
            "BLE writer task exiting"
        );
        let _ = ble_event_tx.send(BleEvent::Disconnected(addr)).await;
    }

    /// Polls the BLE connection state and returns when the device is no
    /// longer connected. Checks once per second.
    async fn wait_for_disconnect(adapter: &bluer::Adapter, addr: Address) {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            match adapter.device(addr) {
                Ok(device) => match device.is_connected().await {
                    Ok(true) => continue,
                    _ => return,
                },
                Err(_) => return,
            }
        }
    }
}
