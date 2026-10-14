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

//! The supervisor: the running daemon. It holds the effective config and
//! the running bridges, and changes them only by applying a reload
//! [plan](crate::reconcile): startup is a plan from nothing running, a
//! reload (`SIGHUP`, the control API) a plan from the config in effect.
//!
//! A plan is applied in three passes so shared resources (a TNC, a listen
//! socket, a BLE adapter) are released before another bridge claims them:
//! stops first, then live updates (settings, kicks, TNC link restarts),
//! then starts.
//!
//! On Linux it also tracks Bluetooth adapters, per port: a `ble-server`
//! port starts when its adapter is present and powered, stops when it goes
//! away and comes back with it; a TNC's adapter is looked up at every
//! connect, so losing it only backs off that bridge's TNC port. Bluetooth
//! is set up the first time a port needs it.
//!
//! It also owns the bridges' public names (from `state.toml` after a DNS-SD
//! collision) and the DNS-SD announcer for `tcp-server` ports.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::announce::{self, Service};
use crate::bridge::{Bridge, BridgeCmd, BridgeHandle, TncConnector, TncSettings, TncState};
use crate::config::state::StateStore;
use crate::config::{self, AdapterSel, Config, Listen, Mac, PortKind, Problem, Sources};
use crate::error::Error;
use crate::port::tcp_server::{ServerState, TcpLive, TcpServerPort};
use crate::port::{serial, tcp_client};
use crate::reconcile::{self, Action, BridgeView, Plan, Runtime, SessionView};

/// How long shutdown waits for bridges to close their TNC links (and send
/// any exit-KISS sequence) before the process exits anyway.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(3);
/// How long a stopping port or bridge may take during a reload.
const STOP_WAIT: Duration = Duration::from_secs(3);

/// Where the daemon's files are.
#[derive(Debug, Clone)]
pub struct Paths {
    pub main_file: PathBuf,
    pub state_dir: PathBuf,
    pub runtime_dir: PathBuf,
}

/// Sets the log level live (`log_level` in `[global]`).
pub type LogLevelFn = Box<dyn Fn(&str) + Send + Sync>;

/// What the rest of the process asks of the daemon.
pub enum DaemonCmd {
    /// Re-read the config from disk and apply it. With `non_disruptive`, a
    /// plan that would disrupt anyone is refused and nothing is applied.
    Reload {
        non_disruptive: bool,
        reply: Option<oneshot::Sender<Result<ReloadReport, ReloadError>>>,
    },
}

/// What a reload did.
#[derive(Debug)]
pub struct ReloadReport {
    pub plan: Plan,
    pub warnings: Vec<Problem>,
}

#[derive(Debug)]
pub enum ReloadError {
    /// The config on disk is invalid; nothing was applied.
    Invalid(Vec<Problem>),
    /// `non_disruptive` and the plan would disrupt; nothing was applied.
    WouldDisrupt(Plan),
}

/// A `ble-server` port waiting for, or running on, its adapter.
pub struct BleSlot {
    pub bridge_id: String,
    pub port_id: String,
    pub adapter: AdapterSel,
    /// The bridge's effective public name, which can change at runtime.
    pub public_name: watch::Receiver<String>,
    pub max_clients: watch::Receiver<u32>,
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

struct RunningBridge {
    cfg: config::Bridge,
    handle: BridgeHandle,
    task: JoinHandle<()>,
    /// The effective public name; the BLE ports and the announcer follow it.
    name: watch::Sender<String>,
    ports: BTreeMap<String, RunningPort>,
}

enum RunningPort {
    /// The TNC port runs inside the bridge task.
    Tnc,
    Tcp {
        token: CancellationToken,
        task: JoinHandle<()>,
        live: watch::Sender<TcpLive>,
        state: watch::Receiver<ServerState>,
        listen: Listen,
        /// Feeds the announcer while the port announces.
        follow: Option<JoinHandle<()>>,
    },
    Ble {
        max_clients: watch::Sender<u32>,
    },
}

/// Run the daemon from `cfg` (built from `sources`) until `shutdown` is
/// cancelled, taking reloads from `cmds`.
pub async fn run(
    sources: Sources,
    cfg: Config,
    paths: Paths,
    log_level: Option<LogLevelFn>,
    mut cmds: mpsc::Receiver<DaemonCmd>,
    shutdown: CancellationToken,
) -> Result<(), Error> {
    let (store, problem) = StateStore::open(&paths.state_dir);
    if let Some(p) = problem {
        tracing::warn!("{p}");
    }
    let empty = Config {
        adapters: Vec::new(),
        tncs: Vec::new(),
        bridges: Vec::new(),
        ..cfg.clone()
    };
    let mut d = Daemon {
        paths,
        cfg: empty,
        sources,
        bridges: BTreeMap::new(),
        state: Arc::new(Mutex::new(store)),
        bt: Bt::default(),
        announcer: None,
        log_level,
        shutdown: shutdown.clone(),
        bluez_errors: Vec::new(),
    };
    let plan = d.plan_for(&cfg).await;
    d.apply(&plan, cfg).await;
    // A daemon that can't reach BlueZ at startup exits, and systemd starts
    // it again until bluetoothd is up. After startup, a reload that can't
    // reach BlueZ logs it and carries on.
    if let Some(e) = d.bluez_errors.drain(..).next() {
        return Err(e);
    }
    if d.bridges.is_empty() {
        tracing::warn!("no bridge is running; check `kiss-tnc-bridge -t`");
    }
    tracing::info!(bridges = d.bridges.len(), "daemon ready");

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            cmd = cmds.recv() => match cmd {
                Some(DaemonCmd::Reload { non_disruptive, reply }) => {
                    let res = d.reload(non_disruptive).await;
                    match (&res, reply) {
                        (_, Some(reply)) => {
                            let _ = reply.send(res);
                        }
                        (Err(ReloadError::Invalid(problems)), None) => {
                            for p in problems {
                                tracing::error!("reload: {p}");
                            }
                            tracing::error!("reload rejected: the config is invalid; nothing changed");
                        }
                        _ => {}
                    }
                }
                None => shutdown.cancelled().await,
            },
        }
    }

    // Every bridge's token is a child of `shutdown`, so they are all
    // closing their TNC links now; wait for that (and the exit-KISS
    // sequence), and for BLE ports to drop their phones' links.
    let tasks = std::mem::take(&mut d.bridges).into_values().map(|b| b.task);
    if tokio::time::timeout(SHUTDOWN_WAIT, futures::future::join_all(tasks))
        .await
        .is_err()
    {
        tracing::warn!("bridges didn't stop in time");
    }
    d.bt.stop().await;
    Ok(())
}

struct Daemon {
    paths: Paths,
    cfg: Config,
    sources: Sources,
    bridges: BTreeMap<String, RunningBridge>,
    state: Arc<Mutex<StateStore>>,
    bt: Bt,
    announcer: Option<mpsc::Sender<announce::Cmd>>,
    log_level: Option<LogLevelFn>,
    shutdown: CancellationToken,
    /// BlueZ failures while applying a plan.
    bluez_errors: Vec<Error>,
}

impl Daemon {
    async fn reload(&mut self, non_disruptive: bool) -> Result<ReloadReport, ReloadError> {
        let sources = config::read_sources(&self.paths.main_file, Some(&self.paths.state_dir))
            .map_err(ReloadError::Invalid)?;
        let (cfg, report) = config::build(&sources, true).map_err(ReloadError::Invalid)?;
        let plan = self.plan_for(&cfg).await;
        if non_disruptive && plan.disruptive() {
            tracing::info!(
                "reload refused: it would disrupt ({})",
                plan.disruptions().join("; ")
            );
            return Err(ReloadError::WouldDisrupt(plan));
        }
        for w in &report.warnings {
            tracing::warn!("{w}");
        }
        self.apply(&plan, cfg).await;
        self.sources = sources;
        for e in self.bluez_errors.drain(..) {
            tracing::error!("{e}");
        }
        Ok(ReloadReport {
            plan,
            warnings: report.warnings,
        })
    }

    /// The plan for moving from the config in effect to `new`.
    async fn plan_for(&self, new: &Config) -> Plan {
        let mut rt_bridges = BTreeMap::new();
        for (id, b) in &self.bridges {
            if let Some(snap) = b.handle.snapshot().await {
                rt_bridges.insert(
                    id.clone(),
                    BridgeView {
                        tnc_active: snap.tnc == TncState::Connected,
                        sessions: snap
                            .sessions
                            .into_iter()
                            .map(|s| SessionView {
                                id: s.id,
                                seq: s.seq,
                            })
                            .collect(),
                    },
                );
            }
        }
        let adapters = self.bt.adapters().await;
        let lookup = move |a: &AdapterSel| -> Option<Mac> {
            match a {
                AdapterSel::Mac(m) => Some(*m),
                AdapterSel::Default => adapters.first().map(|(_, m)| *m),
                AdapterSel::Hci(h) => adapters.iter().find(|(n, _)| n == h).map(|(_, m)| *m),
            }
        };
        let rt = Runtime {
            bridges: rt_bridges,
            adapter_mac: Some(&lookup),
        };
        reconcile::plan(&self.cfg, new, &rt)
    }

    async fn apply(&mut self, plan: &Plan, new: Config) {
        log_plan(plan);

        // ── 1. Stops ────────────────────────────────────────────────
        for bp in &plan.bridges {
            match bp.action {
                Action::Stopped => self.stop_bridge(&bp.id).await,
                Action::Updated => {
                    for pp in &bp.ports {
                        let app_restart = pp.action == Action::Restarted
                            && !matches!(self.port(&bp.id, &pp.id), Some(RunningPort::Tnc));
                        if pp.action == Action::Stopped || app_restart {
                            self.stop_port(&bp.id, &pp.id).await;
                        }
                    }
                }
                _ => {}
            }
        }

        // ── 2. Live updates ─────────────────────────────────────────
        if plan.global.iter().any(|g| g == "log_level")
            && let Some(set) = &self.log_level
        {
            set(&new.global.log_level);
            tracing::info!(level = %new.global.log_level, "log level changed");
        }
        for bp in plan.bridges.iter().filter(|b| b.action == Action::Updated) {
            let Some(nb) = new.bridges.iter().find(|b| b.id == bp.id).cloned() else {
                continue;
            };
            if bp.changes.iter().any(|c| c == "public_name") {
                self.public_name_changed(&nb);
            }
            let tnc_updated = bp.changes.iter().any(|c| c == "app_params")
                || bp.ports.iter().any(|p| {
                    p.action == Action::Updated
                        && matches!(self.port(&bp.id, &p.id), Some(RunningPort::Tnc))
                });
            if tnc_updated && let Some(settings) = settings_for(&nb) {
                self.bridge_cmd(&bp.id, BridgeCmd::Settings(settings)).await;
            }
            for pp in &bp.ports {
                let Some(np) = nb.ports.iter().find(|p| p.id == pp.id) else {
                    continue;
                };
                match (pp.action, self.port(&bp.id, &pp.id)) {
                    (Action::Restarted, Some(RunningPort::Tnc)) => {
                        match self.connector_for(&nb, np).await {
                            Ok(c) => self.bridge_cmd(&bp.id, BridgeCmd::Connector(c)).await,
                            Err(e) => {
                                tracing::error!(bridge = %bp.id, port = %pp.id, "can't restart the TNC port: {e}")
                            }
                        }
                    }
                    (Action::Updated, Some(_)) => self.update_port(&nb, np).await,
                    _ => {}
                }
                if pp.action == Action::Updated {
                    for s in &pp.disconnects {
                        self.bridge_cmd(&bp.id, BridgeCmd::Kick(s.clone())).await;
                    }
                }
            }
            if let Some(rb) = self.bridges.get_mut(&bp.id) {
                rb.cfg = nb;
            }
        }

        // ── 3. Starts ───────────────────────────────────────────────
        for bp in &plan.bridges {
            let Some(nb) = new.bridges.iter().find(|b| b.id == bp.id).cloned() else {
                continue;
            };
            match bp.action {
                Action::Started => self.start_bridge(&nb).await,
                Action::Updated => {
                    for pp in &bp.ports {
                        let start = pp.action == Action::Started
                            || (pp.action == Action::Restarted && !self.has_port(&bp.id, &pp.id));
                        if start && let Some(np) = nb.ports.iter().find(|p| p.id == pp.id) {
                            self.start_port(&nb, np).await;
                        }
                    }
                }
                _ => {}
            }
        }

        // Display-only changes (a TNC's name, a moved file) land too.
        for (id, rb) in self.bridges.iter_mut() {
            if let Some(nb) = new.bridges.iter().find(|b| &b.id == id) {
                rb.cfg = nb.clone();
            }
        }
        self.cfg = new;
    }

    fn port(&self, bridge: &str, port: &str) -> Option<&RunningPort> {
        self.bridges.get(bridge)?.ports.get(port)
    }

    fn has_port(&self, bridge: &str, port: &str) -> bool {
        self.port(bridge, port).is_some()
    }

    async fn bridge_cmd(&self, bridge: &str, cmd: BridgeCmd) {
        if let Some(rb) = self.bridges.get(bridge) {
            let _ = rb.handle.cmd_tx.send(cmd).await;
        }
    }

    // ── Bridges ─────────────────────────────────────────────────────

    async fn start_bridge(&mut self, b: &config::Bridge) {
        let Some(tnc) = b.tnc_ports().next() else {
            return; // validation guarantees one
        };
        let connector = match self.connector_for(b, tnc).await {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(bridge = %b.id, port = %tnc.id, "can't start the bridge: {e}");
                return;
            }
        };
        let settings = settings_for(b).expect("a TNC port");
        let (bridge, handle) = Bridge::new(
            b.id.clone(),
            connector,
            settings,
            self.shutdown.child_token(),
        );
        let task = tokio::spawn(bridge.run());
        let name = {
            let mut store = self.state.lock().unwrap();
            announce::effective_public_name(&mut store, &b.id, &b.public_name)
        };
        if name != b.public_name {
            tracing::info!(bridge = %b.id, configured = %b.public_name, name = %name, "using the public name adjusted after a DNS-SD collision");
        }
        let mut ports = BTreeMap::new();
        ports.insert(tnc.id.clone(), RunningPort::Tnc);
        self.bridges.insert(
            b.id.clone(),
            RunningBridge {
                cfg: b.clone(),
                handle,
                task,
                name: watch::channel(name).0,
                ports,
            },
        );
        if announces(b) {
            self.announce_bridge(b).await;
        }
        for p in b.app_ports() {
            self.start_port(b, p).await;
        }
    }

    async fn stop_bridge(&mut self, id: &str) {
        let Some(ids) = self
            .bridges
            .get(id)
            .map(|rb| rb.ports.keys().cloned().collect::<Vec<_>>())
        else {
            return;
        };
        for p in ids {
            self.stop_port(id, &p).await;
        }
        let Some(rb) = self.bridges.remove(id) else {
            return;
        };
        rb.handle.shutdown.cancel();
        if tokio::time::timeout(STOP_WAIT, rb.task).await.is_err() {
            tracing::warn!(bridge = %id, "bridge didn't stop in time");
        }
        if let Some(tx) = &self.announcer {
            let _ = tx.send(announce::Cmd::Forget { id: id.to_string() }).await;
        }
    }

    // ── Ports ───────────────────────────────────────────────────────

    async fn start_port(&mut self, b: &config::Bridge, p: &config::Port) {
        let Some(rb) = self.bridges.get(&b.id) else {
            return;
        };
        let app_tx = rb.handle.app_tx.clone();
        let bridge_token = rb.handle.shutdown.clone();
        let name_rx = rb.name.subscribe();
        let running = match &p.kind {
            PortKind::TcpServer {
                listen,
                max_clients,
                allow,
                announce,
            } => {
                let (live, live_rx) = watch::channel(TcpLive {
                    max_clients: *max_clients,
                    allow: allow.clone(),
                });
                let token = bridge_token.child_token();
                let (h, task) = TcpServerPort {
                    bridge_id: b.id.clone(),
                    port_id: p.id.clone(),
                    listen: listen.clone(),
                    live: live_rx,
                    app_tx,
                    shutdown: token.clone(),
                }
                .spawn();
                let mut running = RunningPort::Tcp {
                    token,
                    task,
                    live,
                    state: h.state,
                    listen: listen.clone(),
                    follow: None,
                };
                if *announce {
                    self.start_following(&b.id, &p.id, &mut running).await;
                }
                running
            }
            PortKind::BleServer {
                adapter,
                max_clients,
            } => {
                let (max_tx, max_rx) = watch::channel(*max_clients);
                let slot = BleSlot {
                    bridge_id: b.id.clone(),
                    port_id: p.id.clone(),
                    adapter: adapter.clone(),
                    public_name: name_rx,
                    max_clients: max_rx,
                    app_tx,
                    bridge_shutdown: bridge_token,
                    #[cfg(target_os = "linux")]
                    running: None,
                    last_adapter: None,
                    seen_off: false,
                };
                match self
                    .bt
                    .add_ble(slot, &self.paths.runtime_dir, &self.shutdown)
                    .await
                {
                    Ok(()) => RunningPort::Ble {
                        max_clients: max_tx,
                    },
                    Err(e) => {
                        tracing::error!(bridge = %b.id, port = %p.id, "can't start the port: {e}");
                        self.bluez_errors.push(e);
                        return;
                    }
                }
            }
            PortKind::TcpClient { .. } | PortKind::SppClient { .. } | PortKind::Serial { .. } => {
                tracing::warn!(bridge = %b.id, port = %p.id, "a {} app port isn't supported yet; skipping", p.type_name());
                return;
            }
        };
        if let Some(rb) = self.bridges.get_mut(&b.id) {
            rb.ports.insert(p.id.clone(), running);
        }
    }

    async fn stop_port(&mut self, bridge: &str, port: &str) {
        let Some(rp) = self
            .bridges
            .get_mut(bridge)
            .and_then(|rb| rb.ports.remove(port))
        else {
            return;
        };
        match rp {
            RunningPort::Tnc => {
                // Ports are stopped one by one only within a running bridge,
                // where a TNC port restarts in place instead.
            }
            RunningPort::Tcp {
                token,
                task,
                follow,
                ..
            } => {
                token.cancel();
                if let Some(f) = follow {
                    f.abort();
                }
                self.announce_down(bridge, port).await;
                if tokio::time::timeout(STOP_WAIT, task).await.is_err() {
                    tracing::warn!(bridge, port, "port didn't stop in time");
                }
            }
            RunningPort::Ble { .. } => self.bt.remove_ble(bridge, port).await,
        }
    }

    /// Apply a port's live settings.
    async fn update_port(&mut self, b: &config::Bridge, p: &config::Port) {
        match &p.kind {
            PortKind::TcpServer {
                max_clients,
                allow,
                announce,
                ..
            } => {
                let follow_wanted = *announce;
                let mut take = None;
                if let Some(rb) = self.bridges.get_mut(&b.id)
                    && let Some(RunningPort::Tcp { live, follow, .. }) = rb.ports.get_mut(&p.id)
                {
                    live.send_replace(TcpLive {
                        max_clients: *max_clients,
                        allow: allow.clone(),
                    });
                    if !follow_wanted && let Some(f) = follow.take() {
                        f.abort();
                        take = Some(false);
                    } else if follow_wanted && follow.is_none() {
                        take = Some(true);
                    }
                }
                match take {
                    Some(false) => self.announce_down(&b.id, &p.id).await,
                    Some(true) => {
                        self.announce_bridge(b).await;
                        if let Some(mut rp) = self
                            .bridges
                            .get_mut(&b.id)
                            .and_then(|rb| rb.ports.remove(&p.id))
                        {
                            self.start_following(&b.id, &p.id, &mut rp).await;
                            if let Some(rb) = self.bridges.get_mut(&b.id) {
                                rb.ports.insert(p.id.clone(), rp);
                            }
                        }
                    }
                    None => {}
                }
            }
            PortKind::BleServer { max_clients, .. } => {
                if let Some(rb) = self.bridges.get(&b.id)
                    && let Some(RunningPort::Ble { max_clients: tx }) = rb.ports.get(&p.id)
                {
                    tx.send_replace(*max_clients);
                }
            }
            // TNC ports update through BridgeCmd::Settings.
            _ => {}
        }
    }

    // ── Public names and DNS-SD ─────────────────────────────────────

    fn public_name_changed(&mut self, b: &config::Bridge) {
        // A new configured name drops any collision adjustment.
        {
            let mut store = self.state.lock().unwrap();
            if let Err(e) = store.update(|s| s.clear_adjusted_public_name(&b.id)) {
                tracing::warn!("can't write {}: {e}", store.path().display());
            }
        }
        if let Some(rb) = self.bridges.get(&b.id) {
            rb.name.send_replace(b.public_name.clone());
        }
        if announces(b) {
            let tx = self.announcer.clone();
            let name = self.bridges.get(&b.id).map(|rb| rb.name.clone());
            if let (Some(tx), Some(name)) = (tx, name) {
                let cmd = announce::Cmd::Bridge {
                    id: b.id.clone(),
                    configured: b.public_name.clone(),
                    name,
                };
                tokio::spawn(async move {
                    let _ = tx.send(cmd).await;
                });
            }
        }
    }

    async fn announce_bridge(&mut self, b: &config::Bridge) {
        let Some(name) = self.bridges.get(&b.id).map(|rb| rb.name.clone()) else {
            return;
        };
        let Some(tx) = self.announcer().await else {
            return;
        };
        let _ = tx
            .send(announce::Cmd::Bridge {
                id: b.id.clone(),
                configured: b.public_name.clone(),
                name,
            })
            .await;
    }

    async fn start_following(&mut self, bridge: &str, port: &str, rp: &mut RunningPort) {
        let Some(tx) = self.announcer().await else {
            return;
        };
        if let RunningPort::Tcp {
            state,
            listen,
            follow,
            ..
        } = rp
        {
            *follow = Some(tokio::spawn(follow_listening(
                state.clone(),
                listen.clone(),
                bridge.to_string(),
                port.to_string(),
                tx,
            )));
        }
    }

    async fn announce_down(&self, bridge: &str, port: &str) {
        if let Some(tx) = &self.announcer {
            let _ = tx
                .send(announce::Cmd::Down {
                    bridge: bridge.to_string(),
                    port: port.to_string(),
                })
                .await;
        }
    }

    /// The announcer, started the first time a port announces.
    async fn announcer(&mut self) -> Option<mpsc::Sender<announce::Cmd>> {
        if self.announcer.is_none() {
            self.announcer = start_announcer(self.state.clone(), &self.shutdown).await;
        }
        self.announcer.clone()
    }

    // ── TNC links ───────────────────────────────────────────────────

    /// The TNC side of a bridge.
    async fn connector_for(
        &mut self,
        b: &config::Bridge,
        tnc: &config::Port,
    ) -> Result<Box<dyn TncConnector>, String> {
        match &tnc.kind {
            PortKind::TcpClient { host, port, .. } => {
                Ok(Box::new(tcp_client::connector(host.clone(), *port)))
            }
            PortKind::Serial {
                tnc: tnc_id,
                model,
                serial: device,
                ..
            } => Ok(Box::new(serial::connector(
                tnc_id.clone(),
                device.clone(),
                *model,
            ))),
            PortKind::SppClient { .. } => match self.bt.spp_connector(tnc).await {
                Ok(c) => Ok(c),
                Err(e) => {
                    let msg = e.to_string();
                    self.bluez_errors.push(e);
                    Err(msg)
                }
            },
            PortKind::TcpServer { .. } | PortKind::BleServer { .. } => Err(format!(
                "bridge {}: a {} TNC port isn't supported",
                b.id,
                tnc.type_name()
            )),
        }
    }
}

/// The bridge's TNC settings from its config.
fn settings_for(b: &config::Bridge) -> Option<TncSettings> {
    let tnc = b.tnc_ports().next()?;
    let (connect, idle_timeout) = tnc.connect_policy()?;
    let exit_kiss_on_disconnect = match &tnc.kind {
        PortKind::SppClient {
            exit_kiss_on_disconnect,
            ..
        }
        | PortKind::Serial {
            exit_kiss_on_disconnect,
            ..
        } => *exit_kiss_on_disconnect,
        _ => false,
    };
    Some(TncSettings {
        port_id: tnc.id.clone(),
        connect,
        idle_timeout,
        app_params: b.app_params,
        exit_kiss_on_disconnect,
    })
}

fn announces(b: &config::Bridge) -> bool {
    b.ports
        .iter()
        .any(|p| matches!(p.kind, PortKind::TcpServer { announce: true, .. }))
}

fn log_plan(plan: &Plan) {
    if plan.is_noop() {
        tracing::info!("config: no changes");
        return;
    }
    for g in &plan.global {
        tracing::info!(setting = %g, "config: updated");
    }
    for b in &plan.bridges {
        if b.action != Action::Unchanged {
            tracing::info!(bridge = %b.id, action = %b.action, changes = ?b.changes, "config");
        }
        for p in &b.ports {
            if p.action != Action::Unchanged && b.action == Action::Updated {
                tracing::info!(bridge = %b.id, port = %p.id, action = %p.action, changes = ?p.changes, disconnects = p.disconnects.len(), "config");
            }
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

/// Start the DNS-SD announcer.
async fn start_announcer(
    store: Arc<Mutex<StateStore>>,
    shutdown: &CancellationToken,
) -> Option<mpsc::Sender<announce::Cmd>> {
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
        let announcer = announce::Announcer::new(backend, store, running);
        tokio::spawn(announcer.run(rx, ev_rx, shutdown.child_token()));
        Some(tx)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (store, shutdown);
        tracing::warn!("DNS-SD announcing needs Avahi on Linux; tcp-server ports aren't announced");
        None
    }
}

// ── Bluetooth ───────────────────────────────────────────────────────────

/// What the Bluetooth ports share, set up the first time a port needs it:
/// one BlueZ session, the agent phones pair with, the SPP profile TNC links
/// go through, and the task that runs `ble-server` ports on their adapters.
#[derive(Default)]
struct Bt {
    #[cfg(target_os = "linux")]
    session: Option<bluer::Session>,
    #[cfg(target_os = "linux")]
    _agent: Option<crate::agent::Agent>,
    #[cfg(target_os = "linux")]
    spp: Option<Arc<crate::port::spp_client::SppProfile>>,
    #[cfg(target_os = "linux")]
    ble: Option<(mpsc::Sender<ble::Cmd>, JoinHandle<()>)>,
}

#[cfg(target_os = "linux")]
impl Bt {
    async fn session(&mut self) -> Result<bluer::Session, Error> {
        if let Some(s) = &self.session {
            return Ok(s.clone());
        }
        let session = bluer::Session::new().await?;
        // Phones pair with Just Works and bond. Radios need numeric
        // comparison, which is a pairing step (bluetoothctl for now,
        // `ktbctl pair` later).
        let agent = crate::agent::register().await.map_err(Error::Config)?;
        self.session = Some(session.clone());
        self._agent = Some(agent);
        Ok(session)
    }

    async fn spp_connector(&mut self, p: &config::Port) -> Result<Box<dyn TncConnector>, Error> {
        let PortKind::SppClient {
            tnc, model, spp, ..
        } = &p.kind
        else {
            return Err(Error::Config("not an spp-client port".into()));
        };
        let session = self.session().await?;
        if self.spp.is_none() {
            self.spp = Some(crate::port::spp_client::SppProfile::register(&session).await?);
        }
        let profile = self.spp.clone().expect("just registered");
        Ok(Box::new(crate::port::spp_client::connector(
            session,
            profile,
            tnc.clone(),
            spp.clone(),
            *model,
        )))
    }

    async fn add_ble(
        &mut self,
        slot: BleSlot,
        runtime_dir: &std::path::Path,
        shutdown: &CancellationToken,
    ) -> Result<(), Error> {
        if self.ble.is_none() {
            let session = self.session().await?;
            let (tx, rx) = mpsc::channel(16);
            let task = tokio::spawn(ble::run(
                session,
                rx,
                runtime_dir.to_path_buf(),
                shutdown.child_token(),
            ));
            self.ble = Some((tx, task));
        }
        let (tx, _) = self.ble.as_ref().expect("just started");
        tx.send(ble::Cmd::Add(slot))
            .await
            .map_err(|_| Error::Config("the BLE port manager stopped".into()))
    }

    async fn remove_ble(&mut self, bridge: &str, port: &str) {
        if let Some((tx, _)) = &self.ble {
            let (done, wait) = oneshot::channel();
            let cmd = ble::Cmd::Remove {
                bridge: bridge.to_string(),
                port: port.to_string(),
                done,
            };
            if tx.send(cmd).await.is_ok() {
                let _ = wait.await;
            }
        }
    }

    /// Adapters present now, by name, in BlueZ's order.
    async fn adapters(&self) -> Vec<(String, Mac)> {
        let Some(session) = &self.session else {
            return Vec::new();
        };
        let mut names = session.adapter_names().await.unwrap_or_default();
        names.sort();
        let mut out = Vec::new();
        for n in names {
            if let Ok(a) = session.adapter(&n)
                && let Ok(addr) = a.address().await
            {
                out.push((n, Mac(addr.0)));
            }
        }
        out
    }

    /// Wait for the BLE ports to drop their phones (shutdown is cancelled).
    async fn stop(&mut self) {
        if let Some((_, task)) = self.ble.take() {
            let _ = task.await;
        }
    }
}

#[cfg(not(target_os = "linux"))]
impl Bt {
    async fn spp_connector(&mut self, _p: &config::Port) -> Result<Box<dyn TncConnector>, Error> {
        Err(Error::Config("spp-client ports need Linux".into()))
    }

    async fn add_ble(
        &mut self,
        _slot: BleSlot,
        _runtime_dir: &std::path::Path,
        _shutdown: &CancellationToken,
    ) -> Result<(), Error> {
        // Not an error the daemon should exit over: there's no BlueZ to
        // wait for on this platform.
        tracing::warn!("ble-server ports need Linux; not started");
        Ok(())
    }

    async fn remove_ble(&mut self, _bridge: &str, _port: &str) {}

    async fn adapters(&self) -> Vec<(String, Mac)> {
        Vec::new()
    }

    async fn stop(&mut self) {}
}

#[cfg(target_os = "linux")]
mod ble {
    //! Adapter tracking for `ble-server` ports: start each port when its
    //! adapter is present, stop it when the adapter is unplugged or powered
    //! off, start it again when the adapter returns or is powered back on,
    //! and retry ports that failed to start. Ports come and go with
    //! reloads, through [`Cmd`].

    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use bluer::{AdapterEvent, AdapterProperty, SessionEvent};
    use futures::StreamExt;
    use tokio::sync::{mpsc, oneshot, watch};
    use tokio_util::sync::CancellationToken;

    use super::BleSlot;
    use crate::config::{AdapterSel, Mac};
    use crate::port::ble_server::{BleServerPort, BleState};

    pub enum Cmd {
        Add(BleSlot),
        /// Stop a port; `done` once it has dropped its phones and stopped.
        Remove {
            bridge: String,
            port: String,
            done: oneshot::Sender<()>,
        },
    }

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
        session: bluer::Session,
        mut cmds: mpsc::Receiver<Cmd>,
        runtime_dir: PathBuf,
        shutdown: CancellationToken,
    ) {
        let mut slots: Vec<BleSlot> = Vec::new();
        let (ev_tx, mut ev_rx) = mpsc::channel::<Event>(32);
        match session.events().await {
            Ok(session_events) => {
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
            }
            Err(e) => tracing::error!("can't follow Bluetooth adapters coming and going: {e}"),
        }
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
            reconcile(&session, &mut slots, &mut monitors, &ev_tx, &runtime_dir).await;
            let ev = tokio::select! {
                _ = shutdown.cancelled() => break,
                cmd = cmds.recv() => match cmd {
                    Some(Cmd::Add(slot)) => {
                        slots.push(slot);
                        continue;
                    }
                    Some(Cmd::Remove { bridge, port, done }) => {
                        if let Some(i) = slots
                            .iter()
                            .position(|s| s.bridge_id == bridge && s.port_id == port)
                        {
                            let slot = slots.remove(i);
                            if let Some(r) = slot.running {
                                r.token.cancel();
                                if tokio::time::timeout(PORT_STOP_WAIT, r.task).await.is_err() {
                                    tracing::warn!(bridge, port, "ble-server port didn't stop in time");
                                }
                            }
                        }
                        let _ = done.send(());
                        continue;
                    }
                    None => {
                        shutdown.cancelled().await;
                        break;
                    }
                },
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
                        max_clients: slot.max_clients.clone(),
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
