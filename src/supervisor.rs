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
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use serde_json::{Value, json};

use crate::announce::{self, Service};
use crate::bridge::{Bridge, BridgeCmd, BridgeHandle, TncConnector, TncSettings, TncState};
use crate::config::state::StateStore;
use crate::config::store::{DynamicStore, Kind, StoreError};
use crate::config::{self, AdapterSel, Config, Listen, Mac, PortKind, Problem, Sources};
use crate::control::edit::{self, World};
use crate::control::events::Hub;
use crate::control::protocol::{
    self, AdapterStatus, ApiError, BridgeStatus, ChangeResult, CheckResult, Command, ConfigText,
    Counters, ErrorCode, PlanResult, PortStatus, PublicName, SerialStatus, SessionStatus,
    SppStatus, Status, TncStatus, rfc3339,
};
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
    /// A control API command (everything but `subscribe`, which the
    /// connection handles).
    Control {
        command: Command,
        reply: oneshot::Sender<Result<Value, ApiError>>,
        from: Requester,
    },
}

/// Pairings waiting on their requester's replies, by (connection,
/// request id).
#[cfg(target_os = "linux")]
type Pairings =
    Arc<Mutex<std::collections::HashMap<(u64, u64), mpsc::Sender<crate::pairing::Reply>>>>;

/// A device for `bluetooth.pair` or `unpair`.
#[cfg(target_os = "linux")]
struct BtTarget {
    address: bluer::Address,
    adapter: bluer::Adapter,
    /// The TNC it was named by, if any.
    tnc: Option<String>,
}

/// Who sent a control command, for commands that talk back to it while
/// they run (pairing).
#[derive(Debug, Clone)]
pub struct Requester {
    /// Unique per connection.
    pub conn: u64,
    /// The request's id, unique within its connection.
    pub request: u64,
    /// The connection's outbox, for events meant only for it.
    pub events: mpsc::Sender<String>,
}

/// A port's state as `status` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortState {
    pub state: String,
    pub detail: Option<String>,
}

impl PortState {
    pub fn new(state: &str, detail: Option<String>) -> Self {
        Self {
            state: state.to_string(),
            detail,
        }
    }
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
    /// Kept current by the BLE manager, for `status`.
    pub status: watch::Sender<PortState>,
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
        status: watch::Receiver<PortState>,
    },
}

/// Run the daemon from `cfg` (built from `sources`) until `shutdown` is
/// cancelled, taking reloads from `cmds`.
pub async fn run(
    sources: Sources,
    cfg: Config,
    paths: Paths,
    log_level: Option<LogLevelFn>,
    hub: Arc<Hub>,
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
        known_warnings: Default::default(),
        hub,
        store: DynamicStore::from_text(Path::new(""), None).expect("empty"),
        #[cfg(target_os = "linux")]
        bt_busy: Arc::new(tokio::sync::Mutex::new(())),
        #[cfg(target_os = "linux")]
        pairings: Arc::default(),
    };
    d.store = d.store_from_sources();
    // The warnings logged at startup, rebuilt from the same sources.
    if let Ok((_, report)) = config::build(&d.sources, true) {
        d.known_warnings = report.warnings.iter().map(|w| w.to_string()).collect();
    }
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
                Some(DaemonCmd::Control {
                    command,
                    reply,
                    from,
                }) => match command {
                    // These run in the background and answer when done.
                    c @ (Command::BluetoothScan { .. }
                    | Command::BluetoothPair { .. }
                    | Command::BluetoothPairReply { .. }
                    | Command::BluetoothUnpair { .. }) => d.bluetooth(c, reply, from).await,
                    c => {
                        let _ = reply.send(d.control(c).await);
                    }
                },
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
    /// Config warnings already logged, so reloads log only new ones.
    known_warnings: std::collections::HashSet<String>,
    hub: Arc<Hub>,
    /// `dynamic.toml`, as the config in effect was built from.
    store: DynamicStore,
    /// Held by the one Bluetooth scan or pairing allowed at a time.
    #[cfg(target_os = "linux")]
    bt_busy: Arc<tokio::sync::Mutex<()>>,
    /// Pairings waiting on their requester, by (connection, request id):
    /// where that connection's `bluetooth.pair_reply` goes.
    #[cfg(target_os = "linux")]
    pairings: Pairings,
}

impl Daemon {
    async fn reload(&mut self, non_disruptive: bool) -> Result<ReloadReport, ReloadError> {
        let sources = config::read_sources(&self.paths.main_file, Some(&self.paths.state_dir))
            .map_err(ReloadError::Invalid)?;
        let (cfg, report) = config::build(&sources, true).map_err(ReloadError::Invalid)?;
        let plan = self.plan_for(&cfg).await;
        if non_disruptive && plan.disruptive() {
            tracing::info!("reload refused: it would {}", plan.describe_disruptions());
            return Err(ReloadError::WouldDisrupt(plan));
        }
        // Only what's new: the same warning on every reload is noise.
        let warnings: std::collections::HashSet<String> =
            report.warnings.iter().map(|w| w.to_string()).collect();
        for w in warnings.difference(&self.known_warnings) {
            tracing::warn!("{w}");
        }
        self.known_warnings = warnings;
        self.apply(&plan, cfg).await;
        self.sources = sources;
        self.store = self.store_from_sources();
        if !plan.is_noop() {
            self.hub.state("config", "", "reloaded");
        }
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

    // ── Control API ─────────────────────────────────────────────────

    async fn control(&mut self, command: Command) -> Result<Value, ApiError> {
        match command {
            Command::Status => Ok(to_value(&self.status().await)),
            Command::Reload { non_disruptive } => match self.reload(non_disruptive).await {
                Ok(r) => Ok(to_value(&PlanResult::new(r.plan, true, &r.warnings))),
                Err(ReloadError::Invalid(ps)) => Err(ApiError::new(
                    ErrorCode::ConfigInvalid,
                    format!(
                        "the config has {} error(s); nothing was applied (kiss-tnc-bridge -t shows them)",
                        ps.len()
                    ),
                )
                .data(to_value(&protocol::problems(&ps)))),
                Err(ReloadError::WouldDisrupt(plan)) => {
                    let message = format!(
                        "nothing was reloaded: the reload would {}; reload without non_disruptive to go ahead",
                        plan.describe_disruptions()
                    );
                    let result = PlanResult::new(plan, false, &[]);
                    Err(ApiError::new(ErrorCode::ReloadWouldDisrupt, message)
                    .data(to_value(&result)))
                }
            },
            Command::ConfigCheck { dynamic } => Ok(to_value(&self.check(dynamic).await)),
            Command::BluetoothScan { .. }
            | Command::BluetoothPair { .. }
            | Command::BluetoothPairReply { .. }
            | Command::BluetoothUnpair { .. } => Err(ApiError::new(
                ErrorCode::Internal,
                "Bluetooth commands are run by Daemon::bluetooth",
            )),
            Command::Subscribe { .. } => Err(ApiError::new(
                ErrorCode::BadRequest,
                "subscribe is handled by the connection",
            )),
            Command::ConfigGet { kind, object } => self.config_get(kind.map(Into::into), object),
            Command::ConfigPut {
                kind,
                object,
                toml,
                revision,
                non_disruptive,
            } => {
                let kind: Option<Kind> = kind.map(Into::into);
                let object = match (kind, object) {
                    (Some(Kind::Bridge), Some(o)) => Some(self.bridge_id(&o).unwrap_or(o)),
                    (Some(Kind::Tnc), Some(o)) => Some(self.tnc_id(&o).unwrap_or(o)),
                    (_, o) => o,
                };
                self.change(non_disruptive, move |store, world, _| {
                    config_put(store, world, kind, object.as_deref(), &toml, &revision)
                })
                .await
            }
            Command::BridgeCreate {
                definition,
                non_disruptive,
            } => {
                self.change(non_disruptive, move |store, world, _| {
                    edit::create_bridge(store, world, &definition).map(Some)
                })
                .await
            }
            Command::BridgeUpdate {
                bridge,
                definition,
                non_disruptive,
            } => {
                let id = self.bridge_id(&bridge)?;
                self.change(non_disruptive, move |store, world, _| {
                    edit::update_bridge(store, world, &id, &definition).map(|()| None)
                })
                .await
            }
            Command::BridgeDelete {
                bridge,
                non_disruptive,
            } => {
                let id = self.bridge_id(&bridge)?;
                self.change(non_disruptive, move |store, world, _| {
                    edit::delete_bridge(store, world, &id).map(|()| None)
                })
                .await
            }
            Command::BridgeClone {
                bridge,
                new_id,
                name,
                non_disruptive,
            } => {
                let id = self.bridge_id(&bridge)?;
                self.change(non_disruptive, move |store, world, _| {
                    edit::clone_bridge(store, world, &id, new_id.as_deref(), &name).map(Some)
                })
                .await
            }
            Command::BridgeEnable {
                bridge,
                reset,
                non_disruptive,
            } => {
                let id = self.bridge_id(&bridge)?;
                let enabled = if reset { None } else { Some(true) };
                self.change(non_disruptive, move |store, world, overrides| {
                    set_enabled(store, world, overrides, &id, enabled).map(|()| None)
                })
                .await
            }
            Command::BridgeDisable {
                bridge,
                non_disruptive,
            } => {
                let id = self.bridge_id(&bridge)?;
                self.change(non_disruptive, move |store, world, overrides| {
                    set_enabled(store, world, overrides, &id, Some(false)).map(|()| None)
                })
                .await
            }
            Command::BridgeSwitch {
                bridge,
                non_disruptive,
            } => {
                let id = self.bridge_id(&bridge)?;
                let in_the_way = conflicting(&self.cfg, &id);
                self.change(non_disruptive, move |store, world, overrides| {
                    for other in &in_the_way {
                        set_enabled(store, world, overrides, other, Some(false))?;
                    }
                    set_enabled(store, world, overrides, &id, Some(true)).map(|()| None)
                })
                .await
            }
            Command::BridgeRetryPublicName { bridge } => {
                let id = self.bridge_id(&bridge)?;
                self.retry_public_name(&id).await;
                Ok(json!({}))
            }
            Command::PortAdd {
                bridge,
                definition,
                non_disruptive,
            } => {
                let bridge = self.bridge_id(&bridge)?;
                self.change(non_disruptive, move |store, world, _| {
                    edit::add_port(store, world, &bridge, &definition).map(Some)
                })
                .await
            }
            Command::PortUpdate {
                bridge,
                port,
                definition,
                non_disruptive,
            } => {
                let bridge = self.bridge_id(&bridge)?;
                self.change(non_disruptive, move |store, world, _| {
                    edit::update_port(store, world, &bridge, &port, &definition).map(|()| None)
                })
                .await
            }
            Command::PortRemove {
                bridge,
                port,
                non_disruptive,
            } => {
                let bridge = self.bridge_id(&bridge)?;
                self.change(non_disruptive, move |store, world, _| {
                    edit::remove_port(store, world, &bridge, &port).map(|()| None)
                })
                .await
            }
            Command::PortConnect { bridge, port } => self.hold(&bridge, &port, true).await,
            Command::PortDisconnect { bridge, port } => self.hold(&bridge, &port, false).await,
            Command::SessionKick { session } => self.kick(&session).await,
            Command::TncCreate {
                definition,
                non_disruptive,
            } => {
                self.change(non_disruptive, move |store, world, _| {
                    edit::create_object(store, world, Kind::Tnc, &definition).map(Some)
                })
                .await
            }
            Command::TncUpdate {
                tnc,
                definition,
                non_disruptive,
            } => {
                let id = self.tnc_id(&tnc)?;
                self.change(non_disruptive, move |store, world, _| {
                    edit::update_object(store, world, Kind::Tnc, &id, &definition).map(Some)
                })
                .await
            }
            Command::TncDelete {
                tnc,
                non_disruptive,
            } => {
                let id = self.tnc_id(&tnc)?;
                self.change(non_disruptive, move |store, world, _| {
                    edit::delete_object(store, world, Kind::Tnc, &id).map(|()| None)
                })
                .await
            }
            Command::AdapterCreate {
                definition,
                non_disruptive,
            } => {
                self.change(non_disruptive, move |store, world, _| {
                    edit::create_object(store, world, Kind::Adapter, &definition).map(Some)
                })
                .await
            }
            Command::AdapterUpdate {
                adapter,
                definition,
                non_disruptive,
            } => {
                self.change(non_disruptive, move |store, world, _| {
                    edit::update_object(store, world, Kind::Adapter, &adapter, &definition)
                        .map(Some)
                })
                .await
            }
            Command::AdapterDelete {
                adapter,
                non_disruptive,
            } => {
                self.change(non_disruptive, move |store, world, _| {
                    edit::delete_object(store, world, Kind::Adapter, &adapter).map(|()| None)
                })
                .await
            }
            Command::SerialList => Ok(to_value(&crate::port::serial_list::list(&self.cfg))),
            Command::TncModels => Ok(to_value(
                &config::MODELS
                    .iter()
                    .map(|m| protocol::ModelEntry {
                        names: m.names.iter().map(|n| n.to_string()).collect(),
                        summary: m.summary.to_string(),
                    })
                    .collect::<Vec<_>>(),
            )),
        }
    }

    fn store_from_sources(&self) -> DynamicStore {
        let path = self.paths.state_dir.join(config::load::DYNAMIC_FILE_NAME);
        let text = self.sources.dynamic.as_ref().map(|d| d.text.as_str());
        DynamicStore::from_text(&path, text).unwrap_or_else(|e| {
            // build() parsed this text already; toml_edit should too.
            tracing::error!("{e}; changes through the API won't keep dynamic.toml's formatting");
            DynamicStore::from_text(&path, None).expect("empty")
        })
    }

    /// The one way config changes through the API: edit a copy of
    /// `dynamic.toml` (and enable overrides), build the config from the
    /// static files as last read plus that copy, check it, work out the
    /// plan, honour `non_disruptive`, then save and apply. Static files
    /// aren't re-read, so the change touches only what it edited.
    async fn change(
        &mut self,
        non_disruptive: bool,
        edit: impl FnOnce(
            &mut DynamicStore,
            &World,
            &mut Vec<(String, Option<bool>)>,
        ) -> Result<Option<String>, ApiError>,
    ) -> Result<Value, ApiError> {
        let mut store = self.store.clone();
        let mut overrides = Vec::new();
        let id = {
            let world = World {
                statics: &self.sources.statics,
                cfg: &self.cfg,
            };
            edit(&mut store, &world, &mut overrides)?
        };
        let mut sources = self.sources.clone();
        let text = store.text();
        sources.dynamic = (!text.trim().is_empty() || sources.dynamic.is_some()).then(|| {
            config::load::DynamicFile {
                path: store.path().to_path_buf(),
                text,
            }
        });
        for (b, o) in &overrides {
            sources.state.set_enabled_override(b, *o);
        }
        let (cfg, report) = config::build(&sources, true).map_err(|ps| {
            ApiError::new(
                ErrorCode::ConfigInvalid,
                format!(
                    "the change would leave the config invalid: {}",
                    ps.iter()
                        .map(|p| p.to_string())
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
            )
            .data(to_value(&protocol::problems(&ps)))
        })?;
        let plan = self.plan_for(&cfg).await;
        if non_disruptive && plan.disruptive() {
            let message = format!(
                "nothing was changed: this would {}; send it again without non_disruptive to go ahead",
                plan.describe_disruptions()
            );
            let result = PlanResult::new(plan, false, &[]);
            return Err(
                ApiError::new(ErrorCode::ReloadWouldDisrupt, message).data(to_value(&result))
            );
        }
        if store.text() != self.store.text() {
            store.save().map_err(store_error)?;
        }
        if !overrides.is_empty() {
            let mut st = self.state.lock().unwrap();
            if let Err(e) = st.update(|s| {
                for (b, o) in &overrides {
                    s.set_enabled_override(b, *o);
                }
            }) {
                return Err(ApiError::new(
                    ErrorCode::Internal,
                    format!("can't write {}: {e}", st.path().display()),
                ));
            }
        }
        self.apply(&plan, cfg).await;
        self.sources = sources;
        self.store = store;
        for e in self.bluez_errors.drain(..) {
            tracing::error!("{e}");
        }
        if !plan.is_noop() {
            self.hub.state("config", "", "changed");
        }
        Ok(to_value(&ChangeResult {
            id,
            plan: PlanResult::new(plan, true, &report.warnings),
        }))
    }

    fn config_get(&self, kind: Option<Kind>, object: Option<String>) -> Result<Value, ApiError> {
        let text = match (kind, object) {
            (None, None) => ConfigText {
                toml: self.store.text(),
                revision: self.store.revision(),
            },
            (Some(kind), Some(id)) => {
                let id = match kind {
                    Kind::Bridge => self.bridge_id(&id)?,
                    Kind::Tnc => self.tnc_id(&id)?,
                    Kind::Adapter => id,
                };
                let world = World {
                    statics: &self.sources.statics,
                    cfg: &self.cfg,
                };
                if let Some(src) = world.static_source(kind, &id) {
                    return Err(ApiError::new(
                        ErrorCode::StaticObjectReadonly,
                        format!("{kind} {id} is static config; see {src}"),
                    )
                    .object(src.to_string()));
                }
                ConfigText {
                    toml: self.store.get(kind, &id).ok_or_else(|| {
                        ApiError::new(ErrorCode::NotFound, format!("there's no {kind} {id:?}"))
                    })?,
                    revision: self.store.object_revision(kind, &id).expect("just got it"),
                }
            }
            _ => {
                return Err(ApiError::new(
                    ErrorCode::BadRequest,
                    "give both kind and object for one object, or neither for all of dynamic.toml",
                ));
            }
        };
        Ok(to_value(&text))
    }

    /// A bridge's id from its id or its name.
    fn bridge_id(&self, id_or_name: &str) -> Result<String, ApiError> {
        self.cfg
            .bridges
            .iter()
            .find(|b| b.id == id_or_name)
            .or_else(|| self.cfg.bridges.iter().find(|b| b.name == id_or_name))
            .map(|b| b.id.clone())
            .ok_or_else(|| {
                ApiError::new(
                    ErrorCode::NotFound,
                    format!("there's no bridge {id_or_name:?}"),
                )
            })
    }

    /// A TNC's id from its id or its name.
    /// `bluetooth.*` commands. Scans and pairings take a while, so they run
    /// in their own tasks and answer `reply` when done; one at a time.
    #[cfg(target_os = "linux")]
    async fn bluetooth(
        &mut self,
        command: Command,
        reply: oneshot::Sender<Result<Value, ApiError>>,
        from: Requester,
    ) {
        use crate::pairing;
        let failed = |e: String| ApiError::new(ErrorCode::Failed, e);
        let busy = || {
            ApiError::new(
                ErrorCode::Busy,
                "another Bluetooth scan or pairing is running; try again when it's done",
            )
        };
        match command {
            Command::BluetoothPairReply {
                request,
                accept,
                pin,
                passkey,
            } => {
                let tx = self
                    .pairings
                    .lock()
                    .unwrap()
                    .get(&(from.conn, request))
                    .cloned();
                let res = match tx {
                    Some(tx) => tx
                        .send(pairing::Reply {
                            accept,
                            pin,
                            passkey,
                        })
                        .await
                        .map(|()| json!({}))
                        .map_err(|_| ApiError::new(ErrorCode::NotFound, "that pairing is over")),
                    None => Err(ApiError::new(
                        ErrorCode::NotFound,
                        format!(
                            "no pairing started by this connection's request {request} is waiting for a reply"
                        ),
                    )),
                };
                let _ = reply.send(res);
            }
            Command::BluetoothScan { adapter, seconds } => {
                let target = match self.bt_adapter(adapter.as_deref()).await {
                    Ok(a) => a,
                    Err(e) => {
                        let _ = reply.send(Err(e));
                        return;
                    }
                };
                let tncs: Vec<(bluer::Address, String)> = self
                    .cfg
                    .tncs
                    .iter()
                    .filter_map(|t| {
                        t.spp
                            .as_ref()
                            .map(|s| (bluer::Address(s.address.0), t.id.clone()))
                    })
                    .collect();
                let lock = self.bt_busy.clone();
                let secs = seconds.unwrap_or(10).clamp(1, 60);
                tokio::spawn(async move {
                    let Ok(_held) = lock.try_lock() else {
                        let _ = reply.send(Err(busy()));
                        return;
                    };
                    let res = pairing::scan(&target, secs, |a| {
                        tncs.iter().find(|(x, _)| *x == a).map(|(_, id)| id.clone())
                    })
                    .await
                    .map(|l| to_value(&l))
                    .map_err(failed);
                    let _ = reply.send(res);
                });
            }
            Command::BluetoothPair {
                tnc,
                address,
                adapter,
            } => {
                let target = match self.bt_target(tnc, address, adapter).await {
                    Ok(t) => t,
                    Err(e) => {
                        let _ = reply.send(Err(e));
                        return;
                    }
                };
                let session = match self.bt.session().await {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = reply.send(Err(failed(e.to_string())));
                        return;
                    }
                };
                let (step_tx, mut step_rx) = mpsc::channel::<protocol::PairingStep>(8);
                let (reply_tx, reply_rx) = mpsc::channel(4);
                let key = (from.conn, from.request);
                // Each step goes to the requester's connection, and only
                // there.
                let (events, request) = (from.events.clone(), from.request);
                tokio::spawn(async move {
                    while let Some(step) = step_rx.recv().await {
                        let msg = protocol::EventMessage {
                            v: protocol::VERSION,
                            event: protocol::Event::Pairing { request, step },
                        };
                        let Ok(line) = serde_json::to_string(&msg) else {
                            continue;
                        };
                        if events.send(line).await.is_err() {
                            break;
                        }
                    }
                });
                let (lock, pairings, hub) = (
                    self.bt_busy.clone(),
                    self.pairings.clone(),
                    self.hub.clone(),
                );
                tokio::spawn(async move {
                    let Ok(_held) = lock.try_lock() else {
                        let _ = reply.send(Err(busy()));
                        return;
                    };
                    pairings.lock().unwrap().insert(key, reply_tx);
                    let prompter = Arc::new(pairing::Prompter {
                        steps: step_tx,
                        replies: tokio::sync::Mutex::new(reply_rx),
                    });
                    let res =
                        pairing::pair(&session, &target.adapter, target.address, prompter).await;
                    pairings.lock().unwrap().remove(&key);
                    if res.is_ok()
                        && let Some(t) = &target.tnc
                    {
                        hub.state("tnc", t.clone(), "paired");
                    }
                    let adapter = crate::bluetooth::describe(&target.adapter).await;
                    let _ = reply.send(
                        res.map(|()| {
                            to_value(&protocol::PairResult {
                                address: target.address.to_string(),
                                adapter,
                                paired: true,
                            })
                        })
                        .map_err(failed),
                    );
                });
            }
            Command::BluetoothUnpair {
                tnc,
                address,
                adapter,
            } => {
                let res = match self.bt_target(tnc, address, adapter).await {
                    Ok(t) => match pairing::unpair(&t.adapter, t.address).await {
                        Ok(()) => Ok(to_value(&protocol::PairResult {
                            address: t.address.to_string(),
                            adapter: crate::bluetooth::describe(&t.adapter).await,
                            paired: false,
                        })),
                        Err(e) => Err(failed(e)),
                    },
                    Err(e) => Err(e),
                };
                let _ = reply.send(res);
            }
            _ => {
                let _ = reply.send(self.control(command).await);
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    async fn bluetooth(
        &mut self,
        _command: Command,
        reply: oneshot::Sender<Result<Value, ApiError>>,
        _from: Requester,
    ) {
        let _ = reply.send(Err(ApiError::new(
            ErrorCode::Failed,
            "Bluetooth needs Linux with BlueZ",
        )));
    }

    /// The adapter an API request names (id, MAC or hciN), or the default
    /// one.
    #[cfg(target_os = "linux")]
    async fn bt_adapter(&mut self, value: Option<&str>) -> Result<bluer::Adapter, ApiError> {
        let sel = match value {
            None => AdapterSel::Default,
            Some(v) => self.cfg.adapter_sel(v).ok_or_else(|| {
                ApiError::new(ErrorCode::NotFound, format!("there's no adapter {v:?}"))
            })?,
        };
        let session = self
            .bt
            .session()
            .await
            .map_err(|e| ApiError::new(ErrorCode::Failed, e.to_string()))?;
        match crate::bluetooth::find_adapter(&session, &sel).await {
            Ok(Some(a)) => Ok(a),
            Ok(None) => Err(ApiError::new(
                ErrorCode::NotFound,
                format!("{} isn't present", self.cfg.describe_adapter(&sel)),
            )),
            Err(e) => Err(ApiError::new(ErrorCode::Failed, e.to_string())),
        }
    }

    /// The device a `bluetooth.pair` or `unpair` names: a TNC's `spp`
    /// address and adapter, or an address and adapter.
    #[cfg(target_os = "linux")]
    async fn bt_target(
        &mut self,
        tnc: Option<String>,
        address: Option<String>,
        adapter: Option<String>,
    ) -> Result<BtTarget, ApiError> {
        let bad = |m: String| ApiError::new(ErrorCode::BadRequest, m);
        if let Some(t) = tnc {
            let id = self.tnc_id(&t)?;
            let spp = self
                .cfg
                .tnc(&id)
                .and_then(|t| t.spp.clone())
                .ok_or_else(|| {
                    bad(format!(
                        "tnc {id} has no Bluetooth address (an spp section) to pair with"
                    ))
                })?;
            let session = self
                .bt
                .session()
                .await
                .map_err(|e| ApiError::new(ErrorCode::Failed, e.to_string()))?;
            let adapter = match crate::bluetooth::find_adapter(&session, &spp.adapter).await {
                Ok(Some(a)) => a,
                Ok(None) => {
                    return Err(ApiError::new(
                        ErrorCode::NotFound,
                        format!("{} isn't present", self.cfg.describe_adapter(&spp.adapter)),
                    ));
                }
                Err(e) => return Err(ApiError::new(ErrorCode::Failed, e.to_string())),
            };
            return Ok(BtTarget {
                address: bluer::Address(spp.address.0),
                adapter,
                tnc: Some(id),
            });
        }
        let address = address.ok_or_else(|| bad("name a tnc, or an address".into()))?;
        let mac: Mac = address.parse().map_err(bad)?;
        Ok(BtTarget {
            address: bluer::Address(mac.0),
            adapter: self.bt_adapter(adapter.as_deref()).await?,
            tnc: None,
        })
    }

    fn tnc_id(&self, id_or_name: &str) -> Result<String, ApiError> {
        self.cfg
            .tncs
            .iter()
            .find(|t| t.id == id_or_name)
            .or_else(|| self.cfg.tncs.iter().find(|t| t.name == id_or_name))
            .map(|t| t.id.clone())
            .ok_or_else(|| {
                ApiError::new(
                    ErrorCode::NotFound,
                    format!("there's no tnc {id_or_name:?}"),
                )
            })
    }

    async fn hold(&mut self, bridge: &str, port: &str, up: bool) -> Result<Value, ApiError> {
        let bridge = self.bridge_id(bridge)?;
        match self.port(&bridge, port) {
            Some(RunningPort::Tnc) => {
                self.bridge_cmd(&bridge, BridgeCmd::Hold(up)).await;
                Ok(json!({}))
            }
            Some(_) => Err(ApiError::new(
                ErrorCode::BadRequest,
                format!(
                    "{bridge}/{port} isn't a TNC port; connect and disconnect apply to TNC ports"
                ),
            )),
            None => Err(ApiError::new(
                ErrorCode::NotFound,
                format!("bridge {bridge} has no running port {port}"),
            )),
        }
    }

    async fn kick(&mut self, session: &str) -> Result<Value, ApiError> {
        let not_found = || {
            ApiError::new(
                ErrorCode::NotFound,
                format!("there's no session {session:?}"),
            )
        };
        let (bridge, rest) = session.split_once('/').ok_or_else(not_found)?;
        let (port, peer) = rest.split_once('/').ok_or_else(not_found)?;
        let id = crate::bridge::SessionId::new(port, peer);
        let rb = self.bridges.get(bridge).ok_or_else(not_found)?;
        let snap = rb.handle.snapshot().await.ok_or_else(not_found)?;
        if !snap.sessions.iter().any(|s| s.id == id) {
            return Err(not_found());
        }
        let _ = rb.handle.cmd_tx.send(BridgeCmd::Kick(id)).await;
        Ok(json!({}))
    }

    async fn retry_public_name(&mut self, id: &str) {
        {
            let mut store = self.state.lock().unwrap();
            if let Err(e) = store.update(|s| s.clear_adjusted_public_name(id)) {
                tracing::warn!("can't write {}: {e}", store.path().display());
            }
        }
        let Some(b) = self.cfg.bridges.iter().find(|b| b.id == id).cloned() else {
            return;
        };
        if let Some(rb) = self.bridges.get(id) {
            rb.name.send_replace(b.public_name.clone());
            tracing::info!(bridge = %id, name = %b.public_name, "trying the configured public name again");
        }
        if announces(&b) {
            self.announce_bridge(&b).await;
        }
    }

    /// Validate the config on disk, or with `dynamic` standing in for
    /// `dynamic.toml`, and work out what a reload would do.
    async fn check(&self, dynamic: Option<String>) -> CheckResult {
        let invalid = |errors: Vec<Problem>| CheckResult {
            valid: false,
            errors: protocol::problems(&errors),
            warnings: Vec::new(),
            plan: None,
            disruptive: false,
            disruptions: Vec::new(),
        };
        let mut sources =
            match config::read_sources(&self.paths.main_file, Some(&self.paths.state_dir)) {
                Ok(s) => s,
                Err(errors) => return invalid(errors),
            };
        if let Some(text) = dynamic {
            sources.dynamic = Some(config::load::DynamicFile {
                path: self.paths.state_dir.join(config::load::DYNAMIC_FILE_NAME),
                text,
            });
        }
        match config::build(&sources, true) {
            Err(errors) => invalid(errors),
            Ok((cfg, report)) => {
                let plan = self.plan_for(&cfg).await;
                let disruptions = plan.disruptions();
                CheckResult {
                    valid: true,
                    errors: Vec::new(),
                    warnings: protocol::problems(&report.warnings),
                    disruptive: !disruptions.is_empty(),
                    disruptions,
                    plan: Some(plan),
                }
            }
        }
    }

    async fn status(&self) -> Status {
        let cfg = &self.cfg;
        // Adapters: those present, and those an [[adapter]] names.
        let present = self.bt.adapter_details().await;
        let mut adapters: Vec<AdapterStatus> = present
            .iter()
            .map(|(hci, mac, powered)| {
                let named = cfg.adapters.iter().find(|a| a.mac == *mac);
                AdapterStatus {
                    id: named.map(|a| a.id.clone()),
                    mac: mac.to_string(),
                    hci: Some(hci.clone()),
                    present: true,
                    powered: Some(*powered),
                    source: named.map(|a| a.source.to_string()),
                }
            })
            .collect();
        for a in &cfg.adapters {
            if !present.iter().any(|(_, mac, _)| *mac == a.mac) {
                adapters.push(AdapterStatus {
                    id: Some(a.id.clone()),
                    mac: a.mac.to_string(),
                    hci: None,
                    present: false,
                    powered: None,
                    source: Some(a.source.to_string()),
                });
            }
        }

        let mut tncs = Vec::new();
        for t in &cfg.tncs {
            let in_use_by = cfg
                .bridges
                .iter()
                .find(|b| b.enabled && b.ports.iter().any(|p| p.tnc_id() == Some(&t.id)))
                .map(|b| b.id.clone());
            let spp = match &t.spp {
                Some(spp) => Some(SppStatus {
                    address: spp.address.to_string(),
                    adapter: cfg.describe_adapter(&spp.adapter),
                    paired: self.bt.paired(&spp.adapter, spp.address).await,
                }),
                None => None,
            };
            tncs.push(TncStatus {
                id: t.id.clone(),
                name: t.name.clone(),
                model: t.model.to_string(),
                spp,
                serial: t.serial.as_ref().map(|s| SerialStatus {
                    path: s.path.clone(),
                    baud: s.baud,
                    present: std::path::Path::new(&s.path).exists(),
                }),
                in_use_by,
                source: t.source.to_string(),
                editable: !t.source.is_static(),
            });
        }

        let mut bridges = Vec::new();
        for b in &cfg.bridges {
            let rb = self.bridges.get(&b.id);
            let snap = match rb {
                Some(rb) => rb.handle.snapshot().await,
                None => None,
            };
            let mut ports = Vec::new();
            for p in &b.ports {
                let sessions: Vec<SessionStatus> = snap
                    .iter()
                    .flat_map(|s| s.sessions.iter())
                    .filter(|s| s.id.port == p.id)
                    .map(|s| SessionStatus {
                        id: format!("{}/{}", b.id, s.id),
                        since: rfc3339(s.since),
                    })
                    .collect();
                let (state, since) = match (rb.and_then(|rb| rb.ports.get(&p.id)), &snap) {
                    (Some(RunningPort::Tnc), Some(snap)) => (
                        PortState::new(snap.tnc.name(), tnc_detail(&snap.tnc)),
                        Some(rfc3339(snap.tnc_since)),
                    ),
                    (Some(RunningPort::Tcp { state, .. }), _) => (tcp_state(&state.borrow()), None),
                    (Some(RunningPort::Ble { status, .. }), _) => (status.borrow().clone(), None),
                    _ if !b.enabled => (PortState::new("disabled", None), None),
                    _ => (PortState::new("stopped", None), None),
                };
                ports.push(PortStatus {
                    id: p.id.clone(),
                    kind: p.type_name().to_string(),
                    role: p.role.to_string(),
                    state: state.state,
                    detail: state.detail,
                    since,
                    settings: port_settings(cfg, p),
                    sessions,
                });
            }
            bridges.push(BridgeStatus {
                id: b.id.clone(),
                name: b.name.clone(),
                enabled: b.enabled,
                enabled_override: b.enabled_override,
                public_name: PublicName {
                    configured: b.public_name.clone(),
                    effective: rb
                        .map(|rb| rb.name.borrow().clone())
                        .unwrap_or_else(|| b.public_name.clone()),
                },
                source: b.source.to_string(),
                editable: !b.source.is_static(),
                incomplete: b.incomplete(),
                ports,
                counters: rb.map(|rb| {
                    let c = &rb.handle.counters;
                    Counters {
                        frames_up: c.frames_up(),
                        frames_down: c.frames_down(),
                        dropped: c.dropped(),
                        tnc_connects: c.tnc_connects(),
                    }
                }),
            });
        }
        Status {
            version: env!("CARGO_PKG_VERSION").to_string(),
            adapters,
            tncs,
            bridges,
        }
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
        let task = tokio::spawn(bridge.with_hub(self.hub.clone()).run());
        self.hub.state("bridge", &b.id, "started");
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
        self.hub.state("bridge", id, "stopped");
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
                let (status_tx, status_rx) = watch::channel(PortState::new("starting", None));
                let slot = BleSlot {
                    bridge_id: b.id.clone(),
                    port_id: p.id.clone(),
                    adapter: adapter.clone(),
                    public_name: name_rx,
                    max_clients: max_rx,
                    app_tx,
                    bridge_shutdown: bridge_token,
                    status: status_tx,
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
                        status: status_rx,
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
        self.hub
            .state("port", format!("{}/{}", b.id, p.id), "started");
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
        self.hub
            .state("port", format!("{bridge}/{port}"), "stopped");
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
                    && let Some(RunningPort::Ble {
                        max_clients: tx, ..
                    }) = rb.ports.get(&p.id)
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

fn store_error(e: StoreError) -> ApiError {
    match e {
        StoreError::ChangedOnDisk(_) => ApiError::new(ErrorCode::ChangedOnDisk, e.to_string()),
        StoreError::Invalid(m) => ApiError::new(ErrorCode::BadRequest, m),
        StoreError::Io(e) => ApiError::new(ErrorCode::Internal, e.to_string()),
    }
}

/// `config.put`: replace all of `dynamic.toml`, or one object, if the
/// revision is still current. For one object, revision `""` means it
/// doesn't exist yet, and empty TOML deletes it.
fn config_put(
    store: &mut DynamicStore,
    world: &World,
    kind: Option<Kind>,
    id: Option<&str>,
    toml: &str,
    revision: &str,
) -> Result<Option<String>, ApiError> {
    let conflict = || {
        ApiError::new(
            ErrorCode::RevisionConflict,
            "it changed since you got it; get it again and redo the edit",
        )
    };
    match (kind, id) {
        (None, None) => {
            if store.revision() != revision {
                return Err(conflict());
            }
            store.replace_text(toml).map_err(store_error)?;
            Ok(None)
        }
        (Some(kind), Some(id)) => {
            if let Some(src) = world.static_source(kind, id) {
                return Err(ApiError::new(
                    ErrorCode::StaticObjectReadonly,
                    format!("{kind} {id} is static config ({src})"),
                )
                .object(src.to_string()));
            }
            let current = store.object_revision(kind, id).unwrap_or_default();
            if current != revision {
                return Err(conflict());
            }
            if toml.trim().is_empty() {
                store.remove(kind, id);
                return Ok(None);
            }
            let mut table = DynamicStore::parse_object(kind, toml).map_err(store_error)?;
            match table.get("id").and_then(|v| v.as_str()) {
                Some(other) if other != id => {
                    return Err(ApiError::new(
                        ErrorCode::BadRequest,
                        format!("the TOML is for {kind} {other}, not {id}"),
                    ));
                }
                _ => {
                    table.insert("id", toml_edit::value(id));
                }
            }
            store.put(kind, id, table);
            Ok(Some(id.to_string()))
        }
        _ => Err(ApiError::new(
            ErrorCode::BadRequest,
            "give both kind and object for one object, or neither for all of dynamic.toml",
        )),
    }
}

/// Enable, disable, or (`None`) reset a bridge: an override for a static
/// bridge, `enabled` in the definition for a dynamic one.
fn set_enabled(
    store: &mut DynamicStore,
    world: &World,
    overrides: &mut Vec<(String, Option<bool>)>,
    id: &str,
    enabled: Option<bool>,
) -> Result<(), ApiError> {
    if world.static_source(Kind::Bridge, id).is_some() {
        overrides.push((id.to_string(), enabled));
        return Ok(());
    }
    match enabled {
        Some(on) => edit::set_enabled(store, id, on),
        None => Err(ApiError::new(
            ErrorCode::BadRequest,
            format!("bridge {id} is dynamic: it has no override to reset"),
        )),
    }
}

/// The enabled bridges that couldn't run alongside `id`: the same TNC,
/// listen socket, public name, or BLE adapter.
fn conflicting(cfg: &Config, id: &str) -> Vec<String> {
    cfg.bridges
        .iter()
        .filter(|b| b.enabled && b.id != id)
        .filter(|b| {
            let mut pair = cfg.clone();
            for x in pair.bridges.iter_mut() {
                x.enabled = x.id == id || x.id == b.id;
            }
            !config::validate(&pair, false).errors.is_empty()
        })
        .map(|b| b.id.clone())
        .collect()
}

fn to_value<T: serde::Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

fn tnc_detail(s: &TncState) -> Option<String> {
    match s {
        TncState::Backoff { attempt, retry_in } => Some(format!(
            "attempt {attempt}, retry in {}s",
            retry_in.as_secs()
        )),
        TncState::Unpaired(why) | TncState::WaitingForDevice(why) => Some(why.clone()),
        _ => None,
    }
}

fn tcp_state(s: &ServerState) -> PortState {
    match s {
        ServerState::Starting => PortState::new("starting", None),
        ServerState::Listening(a) => PortState::new("listening", Some(a.to_string())),
        ServerState::WaitingForInterface(i) => {
            PortState::new("waiting for interface", Some(i.clone()))
        }
        ServerState::Failed(e) => PortState::new("failed", Some(e.clone())),
        ServerState::Stopped => PortState::new("stopped", None),
    }
}

/// A port's settings as `status` shows them: what the config says, with
/// references resolved for display.
fn port_settings(cfg: &Config, p: &config::Port) -> Value {
    let policy = |connect: &config::ConnectPolicy, idle: &Duration| json!({"connect": connect, "idle_timeout": format!("{}s", idle.as_secs())});
    let mut v = match &p.kind {
        PortKind::TcpClient {
            host,
            port,
            connect,
            idle_timeout,
        } => {
            let mut v = policy(connect, idle_timeout);
            v["address"] = json!(format!("{host}:{port}"));
            v
        }
        PortKind::SppClient {
            tnc,
            connect,
            idle_timeout,
            exit_kiss_on_disconnect,
            ..
        }
        | PortKind::Serial {
            tnc,
            connect,
            idle_timeout,
            exit_kiss_on_disconnect,
            ..
        } => {
            let mut v = policy(connect, idle_timeout);
            v["tnc"] = json!(tnc);
            v["exit_kiss_on_disconnect"] = json!(exit_kiss_on_disconnect);
            v
        }
        PortKind::TcpServer {
            listen,
            max_clients,
            allow,
            announce,
        } => json!({
            "listen": listen.to_string(),
            "max_clients": max_clients,
            "allow": allow.iter().map(|n| n.to_string()).collect::<Vec<_>>(),
            "announce": announce,
        }),
        PortKind::BleServer {
            adapter,
            max_clients,
        } => json!({"adapter": cfg.describe_adapter(adapter), "max_clients": max_clients}),
    };
    if let Value::Object(m) = &mut v {
        m.retain(|_, x| !x.is_null());
    }
    v
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
        tracing::info!("config: no changes to running bridges");
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
        self.session = Some(session.clone());
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

    /// Adapters present now, with whether they're powered. Opens a BlueZ
    /// session if there isn't one (without the agent: listing isn't using).
    async fn adapter_details(&self) -> Vec<(String, Mac, bool)> {
        let session = match &self.session {
            Some(s) => s.clone(),
            None => match bluer::Session::new().await {
                Ok(s) => s,
                Err(_) => return Vec::new(),
            },
        };
        let mut names = session.adapter_names().await.unwrap_or_default();
        names.sort();
        let mut out = Vec::new();
        for n in names {
            if let Ok(a) = session.adapter(&n)
                && let Ok(addr) = a.address().await
            {
                out.push((n, Mac(addr.0), a.is_powered().await.unwrap_or(false)));
            }
        }
        out
    }

    /// Whether `address` is paired on the adapter `sel` means; `None` if
    /// that can't be told.
    async fn paired(&self, sel: &AdapterSel, address: Mac) -> Option<bool> {
        let session = match &self.session {
            Some(s) => s.clone(),
            None => bluer::Session::new().await.ok()?,
        };
        let adapter = crate::bluetooth::find_adapter(&session, sel).await.ok()??;
        let device = adapter.device(bluer::Address(address.0)).ok()?;
        Some(device.is_paired().await.unwrap_or(false))
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
        slot: BleSlot,
        _runtime_dir: &std::path::Path,
        _shutdown: &CancellationToken,
    ) -> Result<(), Error> {
        // Not an error the daemon should exit over: there's no BlueZ to
        // wait for on this platform.
        tracing::warn!("ble-server ports need Linux; not started");
        slot.status.send_replace(PortState::new(
            "unsupported",
            Some("ble-server ports need Linux".into()),
        ));
        Ok(())
    }

    async fn remove_ble(&mut self, _bridge: &str, _port: &str) {}

    async fn adapters(&self) -> Vec<(String, Mac)> {
        Vec::new()
    }

    async fn adapter_details(&self) -> Vec<(String, Mac, bool)> {
        Vec::new()
    }

    async fn paired(&self, _sel: &AdapterSel, _address: Mac) -> Option<bool> {
        None
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

    use super::{BleSlot, PortState};
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
                    tokio::spawn(report_state(
                        handle.state.clone(),
                        slot.status.clone(),
                        name.clone(),
                    ));
                    slot.running = Some(Running {
                        adapter: name,
                        token,
                        state: handle.state,
                        task,
                    });
                }
                Some(name) => {
                    tracing::debug!(bridge = %slot.bridge_id, port = %slot.port_id, adapter = %name, "adapter is powered off; waiting");
                    slot.status
                        .send_replace(PortState::new("adapter off", Some(name)));
                }
                None => {
                    tracing::warn!(
                        bridge = %slot.bridge_id, port = %slot.port_id, adapter = %slot.adapter,
                        "waiting for the Bluetooth adapter to appear"
                    );
                    slot.status.send_replace(PortState::new(
                        "waiting for adapter",
                        Some(slot.adapter.to_string()),
                    ));
                }
            }
        }
    }

    /// Mirror a running port's state into its slot's status, for `status`.
    async fn report_state(
        mut state: watch::Receiver<BleState>,
        status: watch::Sender<PortState>,
        adapter: String,
    ) {
        loop {
            let s = match &*state.borrow_and_update() {
                BleState::Starting => PortState::new("starting", Some(adapter.clone())),
                BleState::Advertising => PortState::new("advertising", Some(adapter.clone())),
                BleState::Full => {
                    PortState::new("full", Some(format!("{adapter}: not advertising")))
                }
                BleState::Failed(e) => PortState::new("failed", Some(e.clone())),
                BleState::Stopped => PortState::new("stopped", None),
            };
            status.send_replace(s);
            if state.changed().await.is_err() {
                return;
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
