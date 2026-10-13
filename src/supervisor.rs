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
//! and come back when it returns. Adapters are tracked per port: a TNC's
//! adapter is looked up at every connect, so losing it puts only that
//! bridge's TNC port into backoff, and app sessions stay connected.
//!
//! It also owns the bridges' public names (from `state.toml` after a DNS-SD
//! collision) and the DNS-SD announcer for `tcp-server` ports.
//!
//! Phase 3 turns this into the reconciler that also applies config changes.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::announce::{self, Service};
use crate::bridge::{Bridge, BridgeHandle, TncConnector, TncSettings};
use crate::config::state::StateStore;
use crate::config::{self, AdapterSel, Config, Listen, PortKind};
use crate::error::Error;
use crate::port::tcp_server::{ServerState, TcpServerHandle, TcpServerPort};
use crate::port::{serial, tcp_client};

/// How long shutdown waits for bridges to close their TNC links (and send
/// any teardown) before the process exits anyway.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(3);

/// A `ble-server` port waiting for, or running on, its adapter.
pub struct BleSlot {
    pub bridge_id: String,
    pub port_id: String,
    pub adapter: AdapterSel,
    /// The bridge's effective public name, which can change at runtime.
    pub public_name: watch::Receiver<String>,
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
    pub task: JoinHandle<()>,
    pub tcp_servers: Vec<TcpServerHandle>,
}

/// Run every enabled bridge until `shutdown` is cancelled. `state_dir` holds
/// `state.toml`; `runtime_dir` holds what only matters until a reboot.
pub async fn run(
    cfg: Config,
    state_dir: &Path,
    runtime_dir: &Path,
    shutdown: CancellationToken,
) -> Result<(), Error> {
    let enabled: Vec<&config::Bridge> = cfg.bridges.iter().filter(|b| b.enabled).collect();

    let (mut store, problem) = StateStore::open(state_dir);
    if let Some(p) = problem {
        tracing::warn!("{p}");
    }
    let mut names: HashMap<String, watch::Sender<String>> = HashMap::new();
    for b in &enabled {
        let name = announce::effective_public_name(&mut store, &b.id, &b.public_name);
        if name != b.public_name {
            tracing::info!(bridge = %b.id, configured = %b.public_name, name = %name, "using the public name adjusted after a DNS-SD collision");
        }
        names.insert(b.id.clone(), watch::channel(name).0);
    }
    let store = Arc::new(Mutex::new(store));

    let bt = Bt::start(&enabled).await?;
    let announcer = start_announcer(&enabled, &names, store, &shutdown).await;

    let mut bridges: Vec<RunningBridge> = Vec::new();
    let mut ble_slots: Vec<BleSlot> = Vec::new();
    for b in &enabled {
        let Some(tnc) = b.tnc_ports().next() else {
            continue; // validation guarantees one; be safe
        };
        let Some(connector) = connector_for(b, tnc, &bt) else {
            continue;
        };
        let (connect, idle_timeout) = tnc
            .connect_policy()
            .expect("every TNC port type has a connect policy");
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
        let task = tokio::spawn(bridge.run());

        let mut tcp_servers = Vec::new();
        for p in b.app_ports() {
            match &p.kind {
                PortKind::TcpServer {
                    listen,
                    max_clients,
                    allow,
                    announce,
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
                    if *announce && let Some(tx) = &announcer {
                        tokio::spawn(follow_listening(
                            h.state.clone(),
                            listen.clone(),
                            b.id.clone(),
                            p.id.clone(),
                            tx.clone(),
                        ));
                    }
                    tcp_servers.push(h);
                }
                PortKind::BleServer {
                    adapter,
                    max_clients,
                } => ble_slots.push(BleSlot {
                    bridge_id: b.id.clone(),
                    port_id: p.id.clone(),
                    adapter: adapter.clone(),
                    public_name: names[&b.id].subscribe(),
                    max_clients: *max_clients,
                    app_tx: handle.app_tx.clone(),
                    bridge_shutdown: handle.shutdown.clone(),
                    #[cfg(target_os = "linux")]
                    running: None,
                    last_adapter: None,
                    seen_off: false,
                }),
                PortKind::TcpClient { .. }
                | PortKind::SppClient { .. }
                | PortKind::Serial { .. } => {
                    tracing::warn!(bridge = %b.id, port = %p.id, "a {} app port isn't supported yet; skipping", p.type_name());
                }
            }
        }
        bridges.push(RunningBridge {
            handle,
            task,
            tcp_servers,
        });
    }

    if bridges.is_empty() {
        tracing::warn!("no bridge is running; check `kiss-tnc-bridge -t`");
    }
    tracing::info!(bridges = bridges.len(), "daemon ready");

    #[cfg(target_os = "linux")]
    {
        match &bt.session {
            Some(session) => ble::run(session, ble_slots, runtime_dir, shutdown.clone()).await?,
            None => shutdown.cancelled().await,
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = runtime_dir;
        for s in &ble_slots {
            tracing::warn!(bridge = %s.bridge_id, port = %s.port_id, "ble-server ports need Linux; not started");
        }
        shutdown.cancelled().await;
    }

    // Bridges close their TNC links, sending any teardown, before we exit.
    let tasks = bridges.into_iter().map(|b| b.task);
    if tokio::time::timeout(SHUTDOWN_WAIT, futures::future::join_all(tasks))
        .await
        .is_err()
    {
        tracing::warn!("bridges didn't stop in time");
    }
    Ok(())
}

/// The TNC side of a bridge, or `None` (logged) if this build can't reach it.
fn connector_for(b: &config::Bridge, tnc: &config::Port, bt: &Bt) -> Option<Box<dyn TncConnector>> {
    match &tnc.kind {
        PortKind::TcpClient { host, port, .. } => {
            Some(Box::new(tcp_client::connector(host.clone(), *port)))
        }
        PortKind::Serial {
            tnc: tnc_id,
            model,
            serial: device,
            exit_kiss_on_disconnect,
            ..
        } => Some(Box::new(serial::connector(
            tnc_id.clone(),
            device.clone(),
            *model,
            *exit_kiss_on_disconnect,
        ))),
        PortKind::SppClient { .. } => bt.spp_connector(b, tnc),
        PortKind::TcpServer { .. } | PortKind::BleServer { .. } => {
            tracing::warn!(bridge = %b.id, port = %tnc.id, "a {} TNC port isn't supported; skipping this bridge", tnc.type_name());
            None
        }
    }
}

/// Tell the announcer when a `tcp-server` port starts or stops listening.
async fn follow_listening(
    mut state: watch::Receiver<ServerState>,
    listen: Listen,
    bridge: String,
    port: String,
    tx: mpsc::Sender<announce::Cmd>,
) {
    loop {
        let cmd = match &*state.borrow_and_update() {
            ServerState::Listening(bound) => announce::Cmd::Up {
                bridge: bridge.clone(),
                port: port.clone(),
                service: Service::for_listen(&listen, *bound),
            },
            _ => announce::Cmd::Down {
                bridge: bridge.clone(),
                port: port.clone(),
            },
        };
        if tx.send(cmd).await.is_err() {
            return;
        }
        if state.changed().await.is_err() {
            let _ = tx.send(announce::Cmd::Down { bridge, port }).await;
            return;
        }
    }
}

/// Start the DNS-SD announcer if any enabled `tcp-server` port announces.
async fn start_announcer(
    enabled: &[&config::Bridge],
    names: &HashMap<String, watch::Sender<String>>,
    store: Arc<Mutex<StateStore>>,
    shutdown: &CancellationToken,
) -> Option<mpsc::Sender<announce::Cmd>> {
    let announcing: Vec<&config::Bridge> = enabled
        .iter()
        .copied()
        .filter(|b| {
            b.ports
                .iter()
                .any(|p| matches!(p.kind, PortKind::TcpServer { announce: true, .. }))
        })
        .collect();
    if announcing.is_empty() {
        return None;
    }
    #[cfg(target_os = "linux")]
    {
        let (ev_tx, ev_rx) = mpsc::channel(32);
        let (backend, running) = match announce::avahi::connect(ev_tx).await {
            Ok(x) => x,
            Err(e) => {
                tracing::warn!(
                    "can't reach Avahi over D-Bus, so tcp-server ports aren't announced: {e}"
                );
                return None;
            }
        };
        if !running {
            tracing::warn!(
                "Avahi isn't running, so tcp-server ports aren't announced until it starts"
            );
        }
        let (tx, rx) = mpsc::channel(32);
        for b in announcing {
            let _ = tx
                .send(announce::Cmd::Bridge {
                    id: b.id.clone(),
                    configured: b.public_name.clone(),
                    name: names[&b.id].clone(),
                })
                .await;
        }
        let announcer = announce::Announcer::new(backend, store, running);
        tokio::spawn(announcer.run(rx, ev_rx, shutdown.child_token()));
        Some(tx)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (names, store, shutdown);
        tracing::warn!("DNS-SD announcing needs Avahi on Linux; tcp-server ports aren't announced");
        None
    }
}

/// What the Bluetooth ports share: one BlueZ session, the agent phones pair
/// with, and the SPP profile TNC links go through.
struct Bt {
    #[cfg(target_os = "linux")]
    session: Option<bluer::Session>,
    #[cfg(target_os = "linux")]
    _agent: Option<crate::agent::Agent>,
    #[cfg(target_os = "linux")]
    spp: Option<Arc<crate::port::spp_client::SppProfile>>,
}

impl Bt {
    #[cfg(target_os = "linux")]
    async fn start(enabled: &[&config::Bridge]) -> Result<Bt, Error> {
        let kinds = || enabled.iter().flat_map(|b| b.ports.iter().map(|p| &p.kind));
        let ble = kinds().any(|k| matches!(k, PortKind::BleServer { .. }));
        let spp = kinds().any(|k| matches!(k, PortKind::SppClient { .. }));
        if !ble && !spp {
            return Ok(Bt {
                session: None,
                _agent: None,
                spp: None,
            });
        }
        let session = bluer::Session::new().await?;
        // Phones pair with Just Works and bond. Radios need numeric
        // comparison, which is a pairing step (bluetoothctl for now,
        // `ktbctl pair` later).
        let agent = crate::agent::register().await.map_err(Error::Config)?;
        let spp = if spp {
            Some(crate::port::spp_client::SppProfile::register(&session).await?)
        } else {
            None
        };
        Ok(Bt {
            session: Some(session),
            _agent: Some(agent),
            spp,
        })
    }

    #[cfg(not(target_os = "linux"))]
    async fn start(_enabled: &[&config::Bridge]) -> Result<Bt, Error> {
        Ok(Bt {})
    }

    #[cfg(target_os = "linux")]
    fn spp_connector(&self, b: &config::Bridge, p: &config::Port) -> Option<Box<dyn TncConnector>> {
        let PortKind::SppClient {
            tnc,
            model,
            spp,
            exit_kiss_on_disconnect,
            ..
        } = &p.kind
        else {
            return None;
        };
        let (Some(session), Some(profile)) = (&self.session, &self.spp) else {
            tracing::error!(bridge = %b.id, port = %p.id, "no Bluetooth session; skipping this bridge");
            return None;
        };
        Some(Box::new(crate::port::spp_client::connector(
            session.clone(),
            profile.clone(),
            tnc.clone(),
            spp.clone(),
            *model,
            *exit_kiss_on_disconnect,
        )))
    }

    #[cfg(not(target_os = "linux"))]
    fn spp_connector(&self, b: &config::Bridge, p: &config::Port) -> Option<Box<dyn TncConnector>> {
        tracing::warn!(bridge = %b.id, port = %p.id, "spp-client ports need Linux; skipping this bridge");
        None
    }
}

#[cfg(target_os = "linux")]
mod ble {
    //! Adapter tracking for `ble-server` ports: start each port when its
    //! adapter is present, stop it when the adapter is unplugged or powered
    //! off, start it again when the adapter returns or is powered back on,
    //! and retry ports that failed to start.

    use std::collections::HashMap;
    use std::path::Path;
    use std::time::Duration;

    use bluer::{AdapterEvent, AdapterProperty, SessionEvent};
    use futures::StreamExt;
    use tokio::sync::{mpsc, watch};
    use tokio_util::sync::CancellationToken;

    use super::BleSlot;
    use crate::config::{AdapterSel, Mac};
    use crate::error::Error;
    use crate::port::ble_server::{BleServerPort, BleState};

    /// How long shutdown waits for `ble-server` ports to disconnect phones.
    const PORT_STOP_WAIT: Duration = Duration::from_secs(3);

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
        pub task: tokio::task::JoinHandle<()>,
    }

    pub async fn run(
        session: &bluer::Session,
        mut slots: Vec<BleSlot>,
        runtime_dir: &Path,
        shutdown: CancellationToken,
    ) -> Result<(), Error> {
        if slots.is_empty() {
            shutdown.cancelled().await;
            return Ok(());
        }

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
            reconcile(session, &mut slots, &mut monitors, &ev_tx, runtime_dir).await;
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
        // Let the ports drop their phones' links before the daemon exits.
        let mut tasks = Vec::new();
        for slot in &mut slots {
            if let Some(r) = slot.running.take() {
                r.token.cancel();
                tasks.push(r.task);
            }
        }
        if tokio::time::timeout(PORT_STOP_WAIT, futures::future::join_all(tasks))
            .await
            .is_err()
        {
            tracing::warn!("ble-server ports didn't stop in time");
        }
        Ok(())
    }

    async fn reconcile(
        session: &bluer::Session,
        slots: &mut [BleSlot],
        monitors: &mut HashMap<String, tokio::task::JoinHandle<()>>,
        ev_tx: &mpsc::Sender<Event>,
        runtime_dir: &Path,
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
                    let (handle, task) = BleServerPort {
                        bridge_id: slot.bridge_id.clone(),
                        port_id: slot.port_id.clone(),
                        adapter: present[&name].adapter.clone(),
                        public_name: slot.public_name.clone(),
                        max_clients: slot.max_clients,
                        app_tx: slot.app_tx.clone(),
                        shutdown: token.clone(),
                        runtime_dir: runtime_dir.to_path_buf(),
                    }
                    .spawn();
                    slot.last_adapter = Some(name.clone());
                    slot.running = Some(Running {
                        adapter: name,
                        token,
                        state: handle.state,
                        task,
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
