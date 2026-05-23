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
    tx_control: CharacteristicControl,
    rx_control: CharacteristicControl,
}

impl BridgeManager {
    pub fn new(
        config: TncConfig,
        tx_control: CharacteristicControl,
        rx_control: CharacteristicControl,
    ) -> Self {
        Self {
            config,
            tx_control,
            rx_control,
        }
    }

    pub async fn run(mut self) -> Result<(), Error> {
        let mut clients: HashMap<Address, BleClientSession> = HashMap::new();
        let mut writers: HashMap<Address, CharacteristicWriter> = HashMap::new();
        let mut tcp: Option<TcpKissConnection> = None;
        let mut tcp_reconnect_delay = Duration::from_secs(1);

        let (ble_event_tx, mut ble_event_rx) = mpsc::channel::<BleEvent>(64);

        let tnc_name = &self.config.name;
        tracing::info!(tnc = tnc_name, "bridge manager started");

        loop {
            tokio::select! {
                evt = self.tx_control.next() => {
                    match evt {
                        Some(CharacteristicControlEvent::Write(req)) => {
                            let addr = req.device_address();
                            if clients.len() >= self.config.max_clients {
                                tracing::warn!(
                                    tnc = tnc_name,
                                    addr = %addr,
                                    max = self.config.max_clients,
                                    "rejecting BLE client: max clients reached"
                                );
                                drop(req);
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
                            tracing::info!(
                                tnc = tnc_name,
                                addr = %addr,
                                "BLE client subscribed (notify)"
                            );
                            writers.insert(addr, notifier);
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
                            writers.remove(&addr);
                            if clients.is_empty() {
                                if tcp.is_some() {
                                    tracing::info!(
                                        tnc = tnc_name,
                                        "last client disconnected, closing TCP"
                                    );
                                    tcp = None;
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
                            Self::notify_all_clients(
                                &mut writers,
                                &encoded,
                                tnc_name,
                            ).await;
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
        mut reader: bluer::gatt::local::CharacteristicReader,
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

    async fn notify_all_clients(
        writers: &mut HashMap<Address, CharacteristicWriter>,
        frame_bytes: &[u8],
        tnc_name: &str,
    ) {
        let mut to_remove = Vec::new();
        for (&addr, writer) in writers.iter_mut() {
            let mtu = writer.mtu();
            let chunk_size = if mtu > 0 { mtu as usize } else { 20 };
            for chunk in frame_bytes.chunks(chunk_size) {
                if let Err(e) = writer.send(chunk).await {
                    tracing::warn!(
                        tnc = tnc_name,
                        addr = %addr,
                        error = %e,
                        "BLE notify failed"
                    );
                    to_remove.push(addr);
                    break;
                }
            }
        }
        for addr in to_remove {
            writers.remove(&addr);
        }
    }
}
