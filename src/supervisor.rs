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
//! The supervisor: starts the enabled bridges and their ports from the
//! config, and on Linux tracks Bluetooth adapters so `ble-server` ports
//! start when their adapter is present and powered, stop when it goes away,
//! and come back when it returns. Bridges and TCP ports don't depend on any
//! adapter and keep running throughout.
//!
//! Phase 3 turns this into the reconciler that also applies config changes.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::bridge::{Bridge, BridgeHandle, TncConnector, TncSettings};
use crate::config::{AdapterSel, Config, PortKind};
use crate::error::Error;
use crate::port::tcp_client;
use crate::port::tcp_server::{TcpServerHandle, TcpServerPort};

/// A `ble-server` port waiting for, or running on, its adapter.
pub struct BleSlot {
    pub bridge_id: String,
    pub port_id: String,
    pub adapter: AdapterSel,
    pub public_name: String,
    pub max_clients: u32,
    pub app_tx: mpsc::Sender<crate::bridge::AppEvent>,
    pub bridge_shutdown: CancellationToken,
    /// Set while the port runs on an adapter.
    #[cfg(target_os = "linux")]
    pub running: Option<ble::Running>,
    /// The adapter the port last ran on, to recognise its power events.
    pub last_adapter: Option<String>,
    /// The adapter was powered off under us; don't power it back on
    /// ourselves, wait for it to be powered on.
    pub seen_off: bool,
}

pub struct RunningBridge {
    pub handle: BridgeHandle,
    pub tcp_servers: Vec<TcpServerHandle>,
}

/// Run every enabled bridge until `shutdown` is cancelled.
pub async fn run(cfg: Config, shutdown: CancellationToken) -> Result<(), Error> {
    let mut bridges: Vec<RunningBridge> = Vec::new();
    let mut ble_slots: Vec<BleSlot> = Vec::new();

    for b in cfg.bridges.iter().filter(|b| b.enabled) {
        let Some(tnc) = b.tnc_ports().next() else {
            continue; // validation guarantees one; be safe
        };
        let connector: Box<dyn TncConnector> = match &tnc.kind {
            PortKind::TcpClient { host, port, .. } => {
                Box::new(tcp_client::connector(host.clone(), *port))
            }
            other => {
                tracing::warn!(bridge = %b.id, port = %tnc.id, "TNC port type {} isn't supported yet; skipping this bridge", type_name(other));
                continue;
            }
        };
        let (connect, idle_timeout) = match &tnc.kind {
            PortKind::TcpClient {
                connect,
                idle_timeout,
                ..
            } => (*connect, *idle_timeout),
            _ => unreachable!(),
        };
        let (bridge, handle) = Bridge::new(
            b.id.clone(),
            connector,
            TncSettings {
                port_id: tnc.id.clone(),
                connect,
                idle_timeout,
                app_params: b.app_params,
            },
            shutdown.child_token(),
        );
        tokio::spawn(bridge.run());

        let mut tcp_servers = Vec::new();
        for p in b.app_ports() {
            match &p.kind {
                PortKind::TcpServer {
                    listen,
                    max_clients,
                    allow,
                    ..
                } => {
                    let (h, _task) = TcpServerPort {
                        bridge_id: b.id.clone(),
                        port_id: p.id.clone(),
                        listen: listen.clone(),
                        max_clients: *max_clients,
                        allow: allow.clone(),
                        app_tx: handle.app_tx.clone(),
                        shutdown: handle.shutdown.child_token(),
                    }
                    .spawn();
                    tcp_servers.push(h);
                }
                PortKind::BleServer {
                    adapter,
                    max_clients,
                } => ble_slots.push(BleSlot {
                    bridge_id: b.id.clone(),
                    port_id: p.id.clone(),
                    adapter: adapter.clone(),
                    public_name: b.public_name.clone(),
                    max_clients: *max_clients,
                    app_tx: handle.app_tx.clone(),
                    bridge_shutdown: handle.shutdown.clone(),
                    #[cfg(target_os = "linux")]
                    running: None,
                    last_adapter: None,
                    seen_off: false,
                }),
                PortKind::TcpClient { .. } => {
                    tracing::warn!(bridge = %b.id, port = %p.id, "a tcp-client app port isn't supported yet; skipping");
                }
            }
        }
        bridges.push(RunningBridge {
            handle,
            tcp_servers,
        });
    }

    if bridges.is_empty() {
        tracing::warn!("no bridge is running; check `kiss-tnc-bridge -t`");
    }
    tracing::info!(bridges = bridges.len(), "daemon ready");

    #[cfg(target_os = "linux")]
    {
        ble::run(ble_slots, shutdown.clone()).await?;
    }
    #[cfg(not(target_os = "linux"))]
    {
        for s in &ble_slots {
            tracing::warn!(bridge = %s.bridge_id, port = %s.port_id, "ble-server ports need Linux; not started");
        }
        shutdown.cancelled().await;
    }

    // Give bridges a moment to close their links cleanly.
    let _ = Arc::new(bridges);
    tokio::time::sleep(Duration::from_millis(100)).await;
    Ok(())
}

fn type_name(kind: &PortKind) -> &'static str {
    match kind {
        PortKind::TcpClient { .. } => "tcp-client",
        PortKind::TcpServer { .. } => "tcp-server",
        PortKind::BleServer { .. } => "ble-server",
    }
}

#[cfg(target_os = "linux")]
mod ble {
    //! Adapter tracking for `ble-server` ports: start each port when its
    //! adapter is present, stop it when the adapter is unplugged or powered
    //! off, start it again when the adapter returns or is powered back on,
    //! and retry ports that failed to start.

    use std::collections::HashMap;
    use std::time::Duration;

    use bluer::{AdapterEvent, AdapterProperty, SessionEvent};
    use futures::StreamExt;
    use tokio::sync::{mpsc, watch};
    use tokio_util::sync::CancellationToken;

    use super::BleSlot;
    use crate::config::{AdapterSel, Mac};
    use crate::error::Error;
    use crate::port::ble_server::{BleServerPort, BleState};

    enum Event {
        Added(String),
        Removed(String),
        Powered(String, bool),
        Tick,
    }

    struct Present {
        adapter: bluer::Adapter,
        mac: Mac,
        powered: bool,
    }

    pub struct Running {
        pub adapter: String,
        pub token: CancellationToken,
        pub state: watch::Receiver<BleState>,
    }

    pub async fn run(mut slots: Vec<BleSlot>, shutdown: CancellationToken) -> Result<(), Error> {
        if slots.is_empty() {
            shutdown.cancelled().await;
            return Ok(());
        }
        let session = bluer::Session::new().await?;
        // Phones pair with Just Works: an agent with no callbacks is
        // NoInputNoOutput. The radio's pairing window (Phase 2) swaps this out
        // while pairing.
        let _agent = session
            .register_agent(bluer::agent::Agent {
                request_default: true,
                ..Default::default()
            })
            .await?;

        let (ev_tx, mut ev_rx) = mpsc::channel::<Event>(32);
        let session_events = session.events().await?;
        let tx = ev_tx.clone();
        tokio::spawn(async move {
            tokio::pin!(session_events);
            while let Some(ev) = session_events.next().await {
                let ev = match ev {
                    SessionEvent::AdapterAdded(n) => Event::Added(n),
                    SessionEvent::AdapterRemoved(n) => Event::Removed(n),
                };
                if tx.send(ev).await.is_err() {
                    break;
                }
            }
        });
        let tx = ev_tx.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(30));
            tick.tick().await;
            loop {
                tick.tick().await;
                if tx.send(Event::Tick).await.is_err() {
                    break;
                }
            }
        });

        let mut monitors: HashMap<String, tokio::task::JoinHandle<()>> = HashMap::new();
        loop {
            reconcile(&session, &mut slots, &mut monitors, &ev_tx).await;
            let ev = tokio::select! {
                _ = shutdown.cancelled() => break,
                ev = ev_rx.recv() => match ev { Some(ev) => ev, None => break },
            };
            match ev {
                Event::Added(n) => {
                    tracing::info!(adapter = %n, "Bluetooth adapter appeared");
                    for s in slots
                        .iter_mut()
                        .filter(|s| s.last_adapter.as_deref() == Some(&n))
                    {
                        s.seen_off = false;
                    }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                Event::Removed(n) => {
                    tracing::warn!(adapter = %n, "Bluetooth adapter removed");
                    monitors.remove(&n);
                }
                Event::Powered(n, true) => {
                    for s in slots
                        .iter_mut()
                        .filter(|s| s.last_adapter.as_deref() == Some(&n))
                    {
                        if s.seen_off {
                            tracing::info!(adapter = %n, bridge = %s.bridge_id, port = %s.port_id, "adapter powered on, resuming");
                        }
                        s.seen_off = false;
                    }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                Event::Powered(n, false) => {
                    for s in slots.iter_mut() {
                        if let Some(r) = &s.running
                            && r.adapter == n
                        {
                            tracing::warn!(adapter = %n, bridge = %s.bridge_id, port = %s.port_id, "adapter powered off, stopping until it's powered on");
                            r.token.cancel();
                            s.running = None;
                            s.seen_off = true;
                        }
                    }
                }
                Event::Tick => {}
            }
        }
        for slot in &mut slots {
            if let Some(r) = slot.running.take() {
                r.token.cancel();
            }
        }
        Ok(())
    }

    async fn reconcile(
        session: &bluer::Session,
        slots: &mut [BleSlot],
        monitors: &mut HashMap<String, tokio::task::JoinHandle<()>>,
        ev_tx: &mpsc::Sender<Event>,
    ) {
        let mut present: HashMap<String, Present> = HashMap::new();
        let mut names = session.adapter_names().await.unwrap_or_default();
        names.sort();
        for name in &names {
            let Ok(adapter) = session.adapter(name) else {
                continue;
            };
            let Ok(addr) = adapter.address().await else {
                continue;
            };
            let powered = adapter.is_powered().await.unwrap_or(false);
            present.insert(
                name.clone(),
                Present {
                    adapter: adapter.clone(),
                    mac: Mac(addr.0),
                    powered,
                },
            );
            monitors
                .entry(name.clone())
                .or_insert_with(|| spawn_monitor(adapter, name.clone(), ev_tx.clone()));
        }
        monitors.retain(|n, _| present.contains_key(n));

        for slot in slots.iter_mut() {
            let want: Option<String> = match &slot.adapter {
                AdapterSel::Default => names.first().cloned(),
                AdapterSel::Hci(h) => names.iter().find(|n| *n == h).cloned(),
                AdapterSel::Mac(m) => present
                    .iter()
                    .find(|(_, p)| p.mac == *m)
                    .map(|(n, _)| n.clone()),
            };
            if let Some(r) = &slot.running {
                let finished = matches!(*r.state.borrow(), BleState::Failed(_) | BleState::Stopped);
                if want.as_deref() != Some(&r.adapter)
                    || !present.contains_key(&r.adapter)
                    || finished
                {
                    if !finished {
                        tracing::warn!(bridge = %slot.bridge_id, port = %slot.port_id, adapter = %r.adapter, "stopping: adapter gone or changed");
                    }
                    r.token.cancel();
                    slot.running = None;
                }
            }
            if slot.running.is_some() {
                continue;
            }
            match want {
                Some(name) if present[&name].powered || !slot.seen_off => {
                    let token = slot.bridge_shutdown.child_token();
                    let (handle, _task) = BleServerPort {
                        bridge_id: slot.bridge_id.clone(),
                        port_id: slot.port_id.clone(),
                        adapter: present[&name].adapter.clone(),
                        public_name: slot.public_name.clone(),
                        max_clients: slot.max_clients,
                        app_tx: slot.app_tx.clone(),
                        shutdown: token.clone(),
                    }
                    .spawn();
                    slot.last_adapter = Some(name.clone());
                    slot.running = Some(Running {
                        adapter: name,
                        token,
                        state: handle.state,
                    });
                }
                Some(name) => {
                    tracing::debug!(bridge = %slot.bridge_id, port = %slot.port_id, adapter = %name, "adapter is powered off; waiting");
                }
                None => tracing::warn!(
                    bridge = %slot.bridge_id, port = %slot.port_id, adapter = %slot.adapter,
                    "waiting for the Bluetooth adapter to appear"
                ),
            }
        }
    }

    fn spawn_monitor(
        adapter: bluer::Adapter,
        name: String,
        tx: mpsc::Sender<Event>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let Ok(events) = adapter.events().await else {
                return;
            };
            tokio::pin!(events);
            while let Some(ev) = events.next().await {
                if let AdapterEvent::PropertyChanged(AdapterProperty::Powered(on)) = ev
                    && tx.send(Event::Powered(name.clone(), on)).await.is_err()
                {
                    break;
                }
            }
        })
    }
}
