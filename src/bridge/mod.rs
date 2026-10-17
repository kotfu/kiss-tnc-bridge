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

//! The bridge: one task per enabled bridge, owning the TNC port's state
//! machine, the registry of app sessions, and the forwarding rules.
//!
//! Forwarding follows roles, not Ethernet-style flooding:
//!
//! - every frame from an app session goes to the TNC, serialized so frames
//!   never interleave on the link;
//! - every frame from the TNC is broadcast to every app session;
//! - app-to-app never happens.
//!
//! App ports (TCP server, BLE server, fakes in tests) talk to the bridge
//! through [`AppEvent`]s and get frames back on a per-session channel. The
//! TNC side is a [`TncConnector`] that produces a byte stream on demand, so
//! the same bridge serves TCP, RFCOMM, serial and `tokio::io::duplex`.
//!
//! The bridge never blocks its select loop: connecting runs in a spawned
//! task, backoff is a timer polled as its own arm, and the link's reader and
//! writer are tasks that report back over a channel.

pub mod policy;

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime};

use tokio::io::{AsyncRead, AsyncWrite, ReadHalf, WriteHalf};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::Sleep;
use tokio_util::sync::CancellationToken;

use crate::config::{AppParams, ConnectPolicy};
use crate::control::events::Hub;
use crate::control::protocol::Dir;
use crate::error::Error;
use crate::port::stream::{KissReader, KissWriter};
use policy::{Verdict, filter_app_frame};

/// A KISS frame as forwarded: the raw bytes between FENDs, still stuffed.
pub type Frame = Vec<u8>;

/// A byte stream to a TNC.
pub trait Link: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Link for T {}
pub type BoxLink = Box<dyn Link>;
pub type ConnectFuture = Pin<Box<dyn Future<Output = Result<Opened, Error>> + Send>>;

/// A connected TNC link, ready for KISS: any init sequence has already run.
pub struct Opened {
    pub link: BoxLink,
    /// Byte sequences, sent [`TEARDOWN_GAP`] apart, that take the TNC out of
    /// KISS mode. Sent when the bridge closes the link on purpose (idle,
    /// shutdown, restart), and only if `exit_kiss_on_disconnect` is set at
    /// the time; never when the link is lost.
    pub exit_kiss: Vec<Vec<u8>>,
}

impl Opened {
    pub fn new(link: BoxLink) -> Self {
        Self {
            link,
            exit_kiss: Vec::new(),
        }
    }
}

/// How a bridge reaches its TNC: TCP connect, RFCOMM, opening a serial
/// device, or handing out a duplex pair in tests.
///
/// Errors steer the state machine: [`Error::Unpaired`] and
/// [`Error::DeviceAbsent`] mean "wait and check again", anything else means
/// "back off and retry".
pub trait TncConnector: Send + Sync + 'static {
    fn connect(&self) -> ConnectFuture;
    /// For logs: `127.0.0.1:6700`, `tnc d75 over spp`, …
    fn describe(&self) -> String;
}

/// A connector from a closure that opens a plain link (no init, no teardown).
pub struct FnConnector<F> {
    f: F,
    desc: String,
}

impl<F, Fut> FnConnector<F>
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<BoxLink, Error>> + Send + 'static,
{
    pub fn new(desc: impl Into<String>, f: F) -> Self {
        Self {
            f,
            desc: desc.into(),
        }
    }
}

impl<F, Fut> TncConnector for FnConnector<F>
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<BoxLink, Error>> + Send + 'static,
{
    fn connect(&self) -> ConnectFuture {
        let fut = (self.f)();
        Box::pin(async move { fut.await.map(Opened::new) })
    }

    fn describe(&self) -> String {
        self.desc.clone()
    }
}

/// One live connection on an app port: `phones/7C:2A:…`, `lan/192.168.4.20:51515`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionId {
    pub port: String,
    pub peer: String,
}

impl SessionId {
    pub fn new(port: impl Into<String>, peer: impl Into<String>) -> Self {
        Self {
            port: port.into(),
            peer: peer.into(),
        }
    }
}

/// On the wire as one string, `port/peer`.
impl serde::Serialize for SessionId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> serde::Deserialize<'de> for SessionId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        let (port, peer) = s
            .split_once('/')
            .ok_or_else(|| serde::de::Error::custom(format!("{s:?} is not port/peer")))?;
        Ok(SessionId::new(port, peer))
    }
}

#[cfg(feature = "schema")]
impl schemars::JsonSchema for SessionId {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "SessionId".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "port/peer: phones/7C:2A:…, lan/192.168.4.20:51515"
        })
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.port, self.peer)
    }
}

/// What app ports tell the bridge.
pub enum AppEvent {
    /// A client attached. `tx` carries frames from the TNC to it; when the
    /// port can't keep up, frames to that session are dropped, not queued
    /// without bound.
    SessionOpened {
        id: SessionId,
        tx: mpsc::Sender<Frame>,
        /// Cancelling it makes the port drop the session (a `kick`, or a
        /// reload that tightens `allow` or lowers `max_clients`).
        kick: CancellationToken,
    },
    SessionClosed {
        id: SessionId,
    },
    /// A complete frame from the client.
    Frame {
        id: SessionId,
        frame: Frame,
    },
}

/// The TNC port's state, for `status` and the TUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TncState {
    Idle,
    Connecting,
    Connected,
    Backoff {
        attempt: u32,
        retry_in: Duration,
    },
    /// An `spp-client` TNC isn't paired on its adapter. Checked again every
    /// [`UNPAIRED_POLL`], without paging the radio.
    Unpaired(String),
    /// A `serial` TNC's device path doesn't exist. Checked again every
    /// [`DEVICE_POLL`].
    WaitingForDevice(String),
}

impl TncState {
    /// The state without its details: `connected`, `backoff`, ….
    pub fn name(&self) -> &'static str {
        match self {
            TncState::Idle => "idle",
            TncState::Connecting => "connecting",
            TncState::Connected => "connected",
            TncState::Backoff { .. } => "backoff",
            TncState::Unpaired(_) => "unpaired",
            TncState::WaitingForDevice(_) => "waiting for device",
        }
    }
}

impl fmt::Display for TncState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TncState::Idle => f.write_str("idle"),
            TncState::Connecting => f.write_str("connecting"),
            TncState::Connected => f.write_str("connected"),
            TncState::Backoff { attempt, retry_in } => {
                write!(
                    f,
                    "backoff (attempt {attempt}, retry in {}s)",
                    retry_in.as_secs()
                )
            }
            TncState::Unpaired(why) => write!(f, "unpaired: {why}"),
            TncState::WaitingForDevice(why) => write!(f, "waiting for device: {why}"),
        }
    }
}

#[derive(Debug, Default)]
pub struct Counters {
    /// Frames from apps forwarded to the TNC.
    pub frames_up: AtomicU64,
    /// Frames from the TNC, counted once each, however many sessions got them.
    pub frames_down: AtomicU64,
    /// Frames dropped: by policy, by a full queue, or a slow session.
    pub dropped: AtomicU64,
    pub sessions: AtomicUsize,
    /// Successful TNC connects.
    pub tnc_connects: AtomicU64,
}

impl Counters {
    pub fn frames_up(&self) -> u64 {
        self.frames_up.load(Ordering::Relaxed)
    }
    pub fn frames_down(&self) -> u64 {
        self.frames_down.load(Ordering::Relaxed)
    }
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
    pub fn sessions(&self) -> usize {
        self.sessions.load(Ordering::Relaxed)
    }
    pub fn tnc_connects(&self) -> u64 {
        self.tnc_connects.load(Ordering::Relaxed)
    }
}

/// The TNC port's settings, from config. All of them apply live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TncSettings {
    pub port_id: String,
    pub connect: ConnectPolicy,
    pub idle_timeout: Duration,
    pub app_params: AppParams,
    pub exit_kiss_on_disconnect: bool,
}

/// Changes to a running bridge.
pub enum BridgeCmd {
    /// New settings, applied live: `connect`, `idle_timeout`,
    /// `app_params`, `exit_kiss_on_disconnect`.
    Settings(TncSettings),
    /// The TNC port restarts with a new link (a reload changed its address,
    /// device or model). App sessions stay connected.
    Connector(Box<dyn TncConnector>),
    /// Drop one app session.
    Kick(SessionId),
    Snapshot(oneshot::Sender<Snapshot>),
}

/// A running bridge's sessions and TNC link, for status and reload plans.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub tnc: TncState,
    /// When the TNC port entered its state.
    pub tnc_since: SystemTime,
    pub sessions: Vec<SessionInfo>,
}

#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub id: SessionId,
    /// Higher is newer.
    pub seq: u64,
    pub since: SystemTime,
}

/// How everything else talks to a running bridge.
#[derive(Clone)]
pub struct BridgeHandle {
    pub id: String,
    pub app_tx: mpsc::Sender<AppEvent>,
    pub cmd_tx: mpsc::Sender<BridgeCmd>,
    pub state: watch::Receiver<TncState>,
    pub counters: Arc<Counters>,
    pub shutdown: CancellationToken,
}

impl BridgeHandle {
    /// The bridge's sessions and link, or `None` if it has stopped.
    pub async fn snapshot(&self) -> Option<Snapshot> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx.send(BridgeCmd::Snapshot(tx)).await.ok()?;
        rx.await.ok()
    }
}

/// Uplink frames held while the TNC connects: at most this many, and none
/// older than this.
pub const UPLINK_QUEUE_FRAMES: usize = 32;
pub const UPLINK_QUEUE_AGE: Duration = Duration::from_secs(5);
pub const BACKOFF_MIN: Duration = Duration::from_secs(1);
pub const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// How often an unpaired TNC's pairing is checked again.
pub const UNPAIRED_POLL: Duration = Duration::from_secs(5);
/// How often a missing serial device is looked for again.
pub const DEVICE_POLL: Duration = Duration::from_secs(2);
/// The pause between teardown steps, so the TNC can act on one before the
/// next (a TH-D75 needs a moment after leaving KISS mode).
pub const TEARDOWN_GAP: Duration = Duration::from_millis(300);
/// How long a deliberate disconnect waits for the teardown to be written.
const TEARDOWN_WAIT: Duration = Duration::from_secs(2);
/// Frames queued for the TNC writer. Matches the app queue so a burst that
/// fits one doesn't overflow the other.
const WRITER_QUEUE: usize = 256;
const APP_QUEUE: usize = 256;

pub struct Bridge {
    id: String,
    connector: Box<dyn TncConnector>,
    settings: TncSettings,
    app_rx: mpsc::Receiver<AppEvent>,
    cmd_rx: mpsc::Receiver<BridgeCmd>,
    state_tx: watch::Sender<TncState>,
    counters: Arc<Counters>,
    shutdown: CancellationToken,
    hub: Option<Arc<Hub>>,
}

impl Bridge {
    /// Report state, activity and frames to the control API's event hub.
    pub fn with_hub(mut self, hub: Arc<Hub>) -> Self {
        self.hub = Some(hub);
        self
    }

    /// `shutdown` stops the bridge when cancelled; ports get child tokens of
    /// it, so cancelling it takes the whole bridge down.
    pub fn new(
        id: impl Into<String>,
        connector: Box<dyn TncConnector>,
        settings: TncSettings,
        shutdown: CancellationToken,
    ) -> (Bridge, BridgeHandle) {
        let id = id.into();
        let (app_tx, app_rx) = mpsc::channel(APP_QUEUE);
        let (cmd_tx, cmd_rx) = mpsc::channel(16);
        let (state_tx, state_rx) = watch::channel(TncState::Idle);
        let counters = Arc::new(Counters::default());
        let handle = BridgeHandle {
            id: id.clone(),
            app_tx,
            cmd_tx,
            state: state_rx,
            counters: counters.clone(),
            shutdown: shutdown.clone(),
        };
        let bridge = Bridge {
            id,
            connector,
            settings,
            app_rx,
            cmd_rx,
            state_tx,
            counters,
            shutdown,
            hub: None,
        };
        (bridge, handle)
    }

    /// Run until shut down or until every app port has dropped its sender.
    pub async fn run(self) {
        let Bridge {
            id,
            connector,
            settings,
            app_rx,
            cmd_rx,
            state_tx,
            counters,
            shutdown,
            hub,
        } = self;
        let (tnc_tx, tnc_rx) = mpsc::channel(64);
        let mut run = Run {
            id,
            connector,
            settings,
            app_rx,
            cmd_rx,
            state_tx,
            counters,
            shutdown,
            tnc_tx,
            tnc_rx,
            sessions: HashMap::new(),
            next_seq: 0,
            link: TncLink::Idle,
            writer_tx: None,
            exit_kiss: Vec::new(),
            generation: 0,
            attempt: 0,
            pending: VecDeque::new(),
            idle_timer: None,
            waiting: None,
            link_start: (0, 0),
            hub,
            state_since: SystemTime::now(),
        };
        run.main().await;
    }
}

// ── Internals ───────────────────────────────────────────────────────────

enum TncEvent {
    Frame { generation: u64, frame: Frame },
    Closed { generation: u64, error: Error },
}

enum TncLink {
    Idle,
    Connecting {
        task: JoinHandle<Result<Opened, Error>>,
    },
    Connected {
        reader: JoinHandle<()>,
        writer: JoinHandle<()>,
    },
    Backoff {
        sleep: Pin<Box<Sleep>>,
    },
}

impl TncLink {
    fn is_waiting(&self) -> bool {
        matches!(self, TncLink::Connecting { .. } | TncLink::Backoff { .. })
    }
}

enum LinkEvent {
    Connected(Result<Opened, Error>),
    BackoffOver,
}

/// The one future the link can be waiting on: the connect task or the
/// backoff timer. Never polled in other states (see the select guard).
async fn link_event(link: &mut TncLink) -> LinkEvent {
    match link {
        TncLink::Connecting { task } => match task.await {
            Ok(res) => LinkEvent::Connected(res),
            Err(e) => LinkEvent::Connected(Err(Error::Config(format!("connect task failed: {e}")))),
        },
        TncLink::Backoff { sleep } => {
            sleep.as_mut().await;
            LinkEvent::BackoffOver
        }
        TncLink::Idle | TncLink::Connected { .. } => std::future::pending().await,
    }
}

struct Run {
    id: String,
    connector: Box<dyn TncConnector>,
    settings: TncSettings,
    app_rx: mpsc::Receiver<AppEvent>,
    cmd_rx: mpsc::Receiver<BridgeCmd>,
    state_tx: watch::Sender<TncState>,
    counters: Arc<Counters>,
    shutdown: CancellationToken,
    tnc_tx: mpsc::Sender<TncEvent>,
    tnc_rx: mpsc::Receiver<TncEvent>,
    sessions: HashMap<SessionId, Session>,
    next_seq: u64,
    link: TncLink,
    /// Frames to the link's writer task; `Some` while connected.
    writer_tx: Option<mpsc::Sender<WriterMsg>>,
    /// The connected link's way out of KISS mode.
    exit_kiss: Vec<Vec<u8>>,
    /// Bumped on every connect, so events from a dead link are ignored.
    generation: u64,
    /// Consecutive failed connects, for backoff.
    attempt: u32,
    /// Uplink frames waiting for the link, with when they arrived.
    pending: VecDeque<(Instant, Frame, Option<SessionId>)>,
    idle_timer: Option<Pin<Box<Sleep>>>,
    /// Set while polling for pairing or a device instead of backing off.
    waiting: Option<Wait>,
    /// Frames up and down when the link came up, for the totals logged when
    /// it goes.
    link_start: (u64, u64),
    hub: Option<Arc<Hub>>,
    /// When the TNC state last changed (its kind, not its details).
    state_since: SystemTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wait {
    Unpaired,
    Device,
}

struct Session {
    tx: mpsc::Sender<Frame>,
    kick: CancellationToken,
    seq: u64,
    since: SystemTime,
}

/// What the TNC link's writer task is told.
enum WriterMsg {
    Frame(Frame),
    /// Take the TNC out of KISS mode before closing.
    ExitKiss(Vec<Vec<u8>>),
}

impl Run {
    async fn main(&mut self) {
        tracing::info!(bridge = %self.id, tnc = %self.connector.describe(), connect = ?self.settings.connect, "bridge started");
        if self.settings.connect == ConnectPolicy::Always {
            self.start_connect();
        }
        loop {
            tokio::select! {
                _ = self.shutdown.cancelled() => break,
                // App events are taken only while the TNC writer has room, so
                // a flood from one app backs up to its own socket instead of
                // being dropped here or stalling TNC-to-app traffic.
                ev = self.app_rx.recv(), if self.app_ready() => match ev {
                    Some(ev) => self.on_app(ev),
                    None => {
                        tracing::debug!(bridge = %self.id, "all app ports gone");
                        break;
                    }
                },
                _ = writer_slot(&self.writer_tx), if !self.app_ready() => {}
                Some(ev) = self.tnc_rx.recv() => self.on_tnc(ev),
                Some(cmd) = self.cmd_rx.recv() => self.on_cmd(cmd),
                ev = link_event(&mut self.link), if self.link.is_waiting() => self.on_link(ev),
                _ = idle_elapsed(&mut self.idle_timer), if self.idle_timer.is_some() => {
                    self.idle_timer = None;
                    if self.sessions.is_empty() {
                        tracing::info!(bridge = %self.id, "idle timeout, disconnecting the TNC");
                        self.close();
                    }
                }
            }
        }
        // Let the teardown reach the TNC before the bridge task ends.
        if let Some(writer) = self.disconnect() {
            finish_writer(writer).await;
        }
        tracing::info!(bridge = %self.id, "bridge stopped");
    }

    /// Whether the next app event can be taken: always, unless the TNC
    /// writer's queue is full.
    fn app_ready(&self) -> bool {
        self.writer_tx.as_ref().is_none_or(|tx| tx.capacity() > 0)
    }

    fn set_state(&mut self, state: TncState) {
        let old = self.state_tx.send_replace(state);
        let new = self.state_tx.borrow().name();
        if old.name() != new {
            self.state_since = SystemTime::now();
            if let Some(hub) = &self.hub {
                hub.state("tnc", self.tnc_object(), new);
            }
        }
    }

    /// `ktbr0/radio`: the TNC port, for events.
    fn tnc_object(&self) -> String {
        format!("{}/{}", self.id, self.settings.port_id)
    }

    fn start_connect(&mut self) {
        self.generation += 1;
        let fut = self.connector.connect();
        self.link = TncLink::Connecting {
            task: tokio::spawn(fut),
        };
        // A poll while waiting for pairing or a device keeps showing what
        // it's waiting for, rather than flickering through Connecting.
        if self.waiting.is_none() {
            self.set_state(TncState::Connecting);
            tracing::info!(bridge = %self.id, port = %self.settings.port_id, tnc = %self.connector.describe(), "connecting");
        }
    }

    /// Check again after a fixed poll interval, without counting attempts:
    /// the TNC needs pairing or plugging in, not patience. Logged once per
    /// stretch of waiting, not on every poll.
    fn start_wait(&mut self, kind: Wait, why: String) {
        if self.waiting != Some(kind) {
            match kind {
                Wait::Unpaired => {
                    tracing::warn!(bridge = %self.id, port = %self.settings.port_id, "{why}")
                }
                Wait::Device => {
                    tracing::warn!(bridge = %self.id, port = %self.settings.port_id, "waiting for the TNC: {why}")
                }
            }
        }
        self.waiting = Some(kind);
        self.attempt = 0;
        let (delay, state) = match kind {
            Wait::Unpaired => (UNPAIRED_POLL, TncState::Unpaired(why)),
            Wait::Device => (DEVICE_POLL, TncState::WaitingForDevice(why)),
        };
        self.link = TncLink::Backoff {
            sleep: Box::pin(tokio::time::sleep(delay)),
        };
        self.set_state(state);
    }

    fn start_backoff(&mut self) {
        self.attempt += 1;
        let delay = BACKOFF_MIN
            .checked_mul(1u32 << (self.attempt - 1).min(16))
            .unwrap_or(BACKOFF_MAX)
            .min(BACKOFF_MAX);
        self.link = TncLink::Backoff {
            sleep: Box::pin(tokio::time::sleep(delay)),
        };
        self.set_state(TncState::Backoff {
            attempt: self.attempt,
            retry_in: delay,
        });
        tracing::info!(bridge = %self.id, port = %self.settings.port_id, attempt = self.attempt, retry_in_s = delay.as_secs(), "will retry");
    }

    /// Close the link on purpose, whatever state it's in, and go idle. A
    /// connected link's writer is left to send the teardown and finishes on
    /// its own (or is aborted after [`TEARDOWN_WAIT`]).
    fn close(&mut self) {
        if let Some(writer) = self.disconnect() {
            tokio::spawn(finish_writer(writer));
        }
    }

    /// Like [`close`](Self::close), but hands back a connected link's writer
    /// for the caller to wait on.
    fn disconnect(&mut self) -> Option<JoinHandle<()>> {
        let writer = match std::mem::replace(&mut self.link, TncLink::Idle) {
            TncLink::Connecting { task } => {
                task.abort();
                None
            }
            TncLink::Connected { reader, writer } => {
                // Dropping the sender tells the writer to stop, after taking
                // the TNC out of KISS mode if that's wanted.
                if let Some(tx) = self.writer_tx.take()
                    && self.settings.exit_kiss_on_disconnect
                    && !self.exit_kiss.is_empty()
                    && tx
                        .try_send(WriterMsg::ExitKiss(self.exit_kiss.clone()))
                        .is_err()
                {
                    tracing::warn!(bridge = %self.id, "TNC writer busy; not taking the TNC out of KISS mode");
                }
                reader.abort();
                let (up, down) = self.link_frames();
                tracing::info!(bridge = %self.id, port = %self.settings.port_id, frames_up = up, frames_down = down, "TNC disconnected");
                Some(writer)
            }
            TncLink::Backoff { .. } | TncLink::Idle => None,
        };
        self.generation += 1;
        self.pending.clear();
        self.waiting = None;
        self.set_state(TncState::Idle);
        writer
    }

    /// Frames up and down since the link came up.
    fn link_frames(&self) -> (u64, u64) {
        (
            self.counters.frames_up() - self.link_start.0,
            self.counters.frames_down() - self.link_start.1,
        )
    }

    fn wants_link(&self) -> bool {
        self.settings.connect == ConnectPolicy::Always || !self.sessions.is_empty()
    }

    fn on_app(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::SessionOpened { id, tx, kick } => {
                tracing::info!(bridge = %self.id, session = %id, "session opened");
                self.next_seq += 1;
                if let Some(hub) = &self.hub {
                    hub.state("session", format!("{}/{id}", self.id), "opened");
                }
                self.sessions.insert(
                    id,
                    Session {
                        tx,
                        kick,
                        seq: self.next_seq,
                        since: SystemTime::now(),
                    },
                );
                self.counters
                    .sessions
                    .store(self.sessions.len(), Ordering::Relaxed);
                self.idle_timer = None;
                if matches!(self.link, TncLink::Idle) {
                    self.start_connect();
                }
            }
            AppEvent::SessionClosed { id } => {
                if self.sessions.remove(&id).is_some() {
                    tracing::info!(bridge = %self.id, session = %id, "session closed");
                    if let Some(hub) = &self.hub {
                        hub.state("session", format!("{}/{id}", self.id), "closed");
                    }
                }
                self.counters
                    .sessions
                    .store(self.sessions.len(), Ordering::Relaxed);
                if self.sessions.is_empty() && self.settings.connect == ConnectPolicy::OnDemand {
                    if self.settings.idle_timeout.is_zero() {
                        tracing::info!(bridge = %self.id, "last session left, disconnecting the TNC");
                        self.close();
                    } else {
                        self.idle_timer =
                            Some(Box::pin(tokio::time::sleep(self.settings.idle_timeout)));
                    }
                }
            }
            AppEvent::Frame { id, frame } => {
                if !self.sessions.contains_key(&id) {
                    tracing::debug!(bridge = %self.id, session = %id, "frame from an unknown session, dropped");
                    self.counters.dropped.fetch_add(1, Ordering::Relaxed);
                    return;
                }
                if let Some(hub) = &self.hub
                    && hub.wants_activity()
                {
                    hub.activity(format!("{}/{id}", self.id), Dir::Rx, frame.len());
                }
                match filter_app_frame(&frame, self.settings.app_params) {
                    Verdict::Drop(why) => {
                        tracing::warn!(bridge = %self.id, session = %id, "dropped frame from app: {why}");
                        self.counters.dropped.fetch_add(1, Ordering::Relaxed);
                    }
                    Verdict::Pass => self.uplink(frame, Some(id)),
                }
            }
        }
    }

    fn on_cmd(&mut self, cmd: BridgeCmd) {
        match cmd {
            BridgeCmd::Settings(new) => self.apply_settings(new),
            BridgeCmd::Connector(connector) => {
                tracing::info!(bridge = %self.id, port = %self.settings.port_id, tnc = %connector.describe(), "TNC port restarting");
                self.close();
                self.connector = connector;
                self.attempt = 0;
                if self.wants_link() {
                    self.start_connect();
                }
            }
            BridgeCmd::Kick(id) => match self.sessions.get(&id) {
                Some(s) => {
                    tracing::info!(bridge = %self.id, session = %id, "disconnecting session");
                    s.kick.cancel();
                }
                None => tracing::debug!(bridge = %self.id, session = %id, "kick: no such session"),
            },
            BridgeCmd::Snapshot(reply) => {
                let mut sessions: Vec<SessionInfo> = self
                    .sessions
                    .iter()
                    .map(|(id, s)| SessionInfo {
                        id: id.clone(),
                        seq: s.seq,
                        since: s.since,
                    })
                    .collect();
                sessions.sort_by_key(|s| s.seq);
                let _ = reply.send(Snapshot {
                    tnc: self.state_tx.borrow().clone(),
                    tnc_since: self.state_since,
                    sessions,
                });
            }
        }
    }

    fn apply_settings(&mut self, new: TncSettings) {
        let old = std::mem::replace(&mut self.settings, new);
        if old == self.settings {
            return;
        }
        tracing::info!(bridge = %self.id, port = %self.settings.port_id, "TNC port settings updated");
        match (old.connect, self.settings.connect) {
            (ConnectPolicy::OnDemand, ConnectPolicy::Always) => {
                self.idle_timer = None;
                if matches!(self.link, TncLink::Idle) {
                    self.start_connect();
                }
            }
            (ConnectPolicy::Always, ConnectPolicy::OnDemand) if self.sessions.is_empty() => {
                if self.settings.idle_timeout.is_zero() {
                    self.close();
                } else {
                    self.idle_timer =
                        Some(Box::pin(tokio::time::sleep(self.settings.idle_timeout)));
                }
            }
            _ => {}
        }
    }

    fn uplink(&mut self, frame: Frame, from: Option<SessionId>) {
        let frame_len = frame.len();
        if matches!(self.link, TncLink::Connected { .. })
            && let Some(hub) = &self.hub
            && hub.wants_frames()
        {
            hub.frame(self.tnc_object(), Dir::Tx, from.as_ref(), &frame);
        }
        match &self.link {
            TncLink::Connected { .. } => match self
                .writer_tx
                .as_ref()
                .map(|tx| tx.try_send(WriterMsg::Frame(frame)))
            {
                Some(Ok(())) => {
                    self.counters.frames_up.fetch_add(1, Ordering::Relaxed);
                    tracing::trace!(bridge = %self.id, bytes = frame_len, "frame to the TNC");
                    if let Some(hub) = &self.hub
                        && hub.wants_activity()
                    {
                        hub.activity(self.tnc_object(), Dir::Tx, frame_len);
                    }
                }
                _ => {
                    // Only reachable if the writer went away under us.
                    tracing::warn!(bridge = %self.id, "TNC writer unavailable, dropping a frame");
                    self.counters.dropped.fetch_add(1, Ordering::Relaxed);
                }
            },
            TncLink::Idle => {
                self.queue_pending(frame, from);
                self.start_connect();
            }
            // Waiting for pairing or a device is still "connecting" as far
            // as uplink goes: queue, and let stale frames age out.
            TncLink::Connecting { .. } | TncLink::Backoff { .. } => self.queue_pending(frame, from),
        }
    }

    fn queue_pending(&mut self, frame: Frame, from: Option<SessionId>) {
        if self.pending.len() >= UPLINK_QUEUE_FRAMES {
            self.pending.pop_front();
            self.counters.dropped.fetch_add(1, Ordering::Relaxed);
        }
        self.pending.push_back((Instant::now(), frame, from));
    }

    fn on_tnc(&mut self, ev: TncEvent) {
        match ev {
            TncEvent::Frame { generation, frame } => {
                if generation != self.generation {
                    return;
                }
                self.counters.frames_down.fetch_add(1, Ordering::Relaxed);
                tracing::trace!(bridge = %self.id, bytes = frame.len(), sessions = self.sessions.len(), "frame from the TNC");
                let hub = self.hub.clone();
                let activity = hub.as_ref().is_some_and(|h| h.wants_activity());
                if let Some(hub) = &hub {
                    if hub.wants_frames() {
                        hub.frame(self.tnc_object(), Dir::Rx, None, &frame);
                    }
                    if activity {
                        hub.activity(self.tnc_object(), Dir::Rx, frame.len());
                    }
                }
                for (id, session) in &self.sessions {
                    if session.tx.try_send(frame.clone()).is_err() {
                        tracing::warn!(bridge = %self.id, session = %id, "session can't keep up, dropping a frame");
                        self.counters.dropped.fetch_add(1, Ordering::Relaxed);
                    } else if let Some(hub) = hub.as_ref().filter(|_| activity) {
                        hub.activity(format!("{}/{id}", self.id), Dir::Tx, frame.len());
                    }
                }
            }
            TncEvent::Closed { generation, error } => {
                if generation != self.generation || !matches!(self.link, TncLink::Connected { .. })
                {
                    return;
                }
                let (up, down) = self.link_frames();
                tracing::warn!(bridge = %self.id, port = %self.settings.port_id, frames_up = up, frames_down = down, "TNC link lost: {error}");
                if let TncLink::Connected { reader, writer } =
                    std::mem::replace(&mut self.link, TncLink::Idle)
                {
                    reader.abort();
                    writer.abort();
                }
                self.writer_tx = None;
                self.generation += 1;
                if self.wants_link() {
                    self.attempt = 0;
                    self.start_backoff();
                } else {
                    self.set_state(TncState::Idle);
                }
            }
        }
    }

    fn on_link(&mut self, ev: LinkEvent) {
        match ev {
            LinkEvent::Connected(Ok(Opened { link, exit_kiss })) => {
                let generation = self.generation;
                self.waiting = None;
                self.exit_kiss = exit_kiss;
                let (rd, wr) = tokio::io::split(link);
                let (writer_tx, writer_rx) = mpsc::channel(WRITER_QUEUE);
                let reader = tokio::spawn(reader_task(
                    KissReader::new(rd),
                    self.tnc_tx.clone(),
                    generation,
                ));
                let writer = tokio::spawn(writer_task(
                    KissWriter::new(wr),
                    writer_rx,
                    self.tnc_tx.clone(),
                    generation,
                ));
                self.link = TncLink::Connected { reader, writer };
                self.writer_tx = Some(writer_tx);
                self.attempt = 0;
                self.counters.tnc_connects.fetch_add(1, Ordering::Relaxed);
                self.link_start = (self.counters.frames_up(), self.counters.frames_down());
                self.set_state(TncState::Connected);
                tracing::info!(bridge = %self.id, port = %self.settings.port_id, "TNC connected");
                // Frames that arrived while connecting, if they're still fresh.
                let cutoff = Instant::now() - UPLINK_QUEUE_AGE;
                let pending: Vec<_> = self.pending.drain(..).collect();
                for (at, frame, from) in pending {
                    if at < cutoff {
                        self.counters.dropped.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    self.uplink(frame, from);
                }
                if !self.wants_link() && self.settings.idle_timeout.is_zero() {
                    // Everyone left while we were connecting.
                    self.close();
                }
            }
            LinkEvent::Connected(Err(e)) => {
                self.link = TncLink::Idle;
                if !self.wants_link() {
                    tracing::info!(bridge = %self.id, port = %self.settings.port_id, "connect failed with nobody waiting: {e}");
                    self.pending.clear();
                    self.waiting = None;
                    self.set_state(TncState::Idle);
                    return;
                }
                match e {
                    Error::Unpaired(why) => self.start_wait(Wait::Unpaired, why),
                    Error::DeviceAbsent(why) => self.start_wait(Wait::Device, why),
                    e => {
                        self.waiting = None;
                        tracing::warn!(bridge = %self.id, port = %self.settings.port_id, tnc = %self.connector.describe(), "connect failed: {e}");
                        self.start_backoff();
                    }
                }
            }
            LinkEvent::BackoffOver => {
                self.link = TncLink::Idle;
                if self.wants_link() {
                    self.start_connect();
                } else {
                    self.pending.clear();
                    self.waiting = None;
                    self.set_state(TncState::Idle);
                }
            }
        }
    }
}

/// Resolves once the TNC writer can take a frame again. Only polled while
/// it can't (see the select guard), and never while disconnected.
async fn writer_slot(writer_tx: &Option<mpsc::Sender<WriterMsg>>) {
    match writer_tx {
        Some(tx) => {
            // The permit is dropped at once: this is a wakeup, not a send.
            let _ = tx.reserve().await;
        }
        None => std::future::pending().await,
    }
}

async fn idle_elapsed(timer: &mut Option<Pin<Box<Sleep>>>) {
    match timer {
        Some(sleep) => sleep.as_mut().await,
        None => std::future::pending().await,
    }
}

async fn reader_task(
    mut rd: KissReader<ReadHalf<BoxLink>>,
    tx: mpsc::Sender<TncEvent>,
    generation: u64,
) {
    loop {
        match rd.read_frame().await {
            Ok(frame) => {
                if tx
                    .send(TncEvent::Frame { generation, frame })
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Err(error) => {
                let _ = tx.send(TncEvent::Closed { generation, error }).await;
                return;
            }
        }
    }
}

async fn writer_task(
    mut wr: KissWriter<WriteHalf<BoxLink>>,
    mut rx: mpsc::Receiver<WriterMsg>,
    tx: mpsc::Sender<TncEvent>,
    generation: u64,
) {
    let mut exited = false;
    while let Some(msg) = rx.recv().await {
        match msg {
            WriterMsg::Frame(frame) => {
                if let Err(error) = wr.write_frame(&frame).await {
                    let _ = tx.send(TncEvent::Closed { generation, error }).await;
                    return;
                }
            }
            WriterMsg::ExitKiss(steps) => {
                for (i, step) in steps.iter().enumerate() {
                    if i > 0 {
                        tokio::time::sleep(TEARDOWN_GAP).await;
                    }
                    if wr.write_bytes(step).await.is_err() {
                        return;
                    }
                }
                exited = true;
            }
        }
    }
    // The bridge dropped its sender: it's disconnecting on purpose. Give
    // the TNC a moment to act on the last step before the link closes.
    if exited {
        tokio::time::sleep(TEARDOWN_GAP).await;
    }
    let _ = wr.shutdown().await;
}

/// Wait for a writer to finish its teardown, then give up on it.
async fn finish_writer(mut writer: JoinHandle<()>) {
    if tokio::time::timeout(TEARDOWN_WAIT, &mut writer)
        .await
        .is_err()
    {
        writer.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::port::stream::KissConnection;
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, DuplexStream};
    use tokio::time::{Duration, timeout};

    #[derive(Debug, Clone, Copy)]
    enum Fail {
        Refused,
        Unpaired,
        Absent,
    }

    /// Hands the bridge one end of a duplex pair per connect and passes the
    /// other end to the test, which plays the TNC. Queued failures are used
    /// up first, one per connect.
    struct FakeTnc {
        peers: mpsc::UnboundedSender<DuplexStream>,
        fail: Arc<Mutex<VecDeque<Fail>>>,
        connects: Arc<AtomicU64>,
        exit_kiss: Vec<Vec<u8>>,
    }

    impl TncConnector for FakeTnc {
        fn connect(&self) -> ConnectFuture {
            let peers = self.peers.clone();
            let fail = self.fail.clone();
            let exit_kiss = self.exit_kiss.clone();
            self.connects.fetch_add(1, Ordering::Relaxed);
            Box::pin(async move {
                let next = fail.lock().unwrap().pop_front();
                match next {
                    Some(Fail::Refused) => return Err(Error::Connect("fake: refused".into())),
                    Some(Fail::Unpaired) => return Err(Error::Unpaired("fake: not paired".into())),
                    Some(Fail::Absent) => {
                        return Err(Error::DeviceAbsent("fake: unplugged".into()));
                    }
                    None => {}
                }
                let (ours, theirs) = tokio::io::duplex(4096);
                peers
                    .send(theirs)
                    .map_err(|_| Error::Connect("test gone".into()))?;
                Ok(Opened {
                    link: Box::new(ours),
                    exit_kiss,
                })
            })
        }

        fn describe(&self) -> String {
            "fake tnc".into()
        }
    }

    struct Harness {
        handle: BridgeHandle,
        peers: mpsc::UnboundedReceiver<DuplexStream>,
        fail: Arc<Mutex<VecDeque<Fail>>>,
        connects: Arc<AtomicU64>,
        task: JoinHandle<()>,
        kicks: Mutex<HashMap<SessionId, CancellationToken>>,
    }

    fn settings(
        connect: ConnectPolicy,
        idle_timeout: Duration,
        app_params: AppParams,
    ) -> TncSettings {
        TncSettings {
            port_id: "tnc".into(),
            connect,
            idle_timeout,
            app_params,
            exit_kiss_on_disconnect: false,
        }
    }

    fn start(connect: ConnectPolicy, idle_timeout: Duration, app_params: AppParams) -> Harness {
        start_with(connect, idle_timeout, app_params, Vec::new())
    }

    fn start_with(
        connect: ConnectPolicy,
        idle_timeout: Duration,
        app_params: AppParams,
        exit_kiss: Vec<Vec<u8>>,
    ) -> Harness {
        let (peers_tx, peers) = mpsc::unbounded_channel();
        let fail = Arc::new(Mutex::new(VecDeque::new()));
        let connects = Arc::new(AtomicU64::new(0));
        let connector = FakeTnc {
            peers: peers_tx,
            fail: fail.clone(),
            connects: connects.clone(),
            exit_kiss: exit_kiss.clone(),
        };
        let (bridge, handle) = Bridge::new(
            "ktbr0",
            Box::new(connector),
            TncSettings {
                exit_kiss_on_disconnect: !exit_kiss.is_empty(),
                ..settings(connect, idle_timeout, app_params)
            },
            CancellationToken::new(),
        );
        let task = tokio::spawn(bridge.run());
        Harness {
            handle,
            peers,
            fail,
            connects,
            task,
            kicks: Mutex::new(HashMap::new()),
        }
    }

    impl Harness {
        async fn tnc(&mut self) -> KissConnection<DuplexStream> {
            let peer = timeout(Duration::from_secs(5), self.peers.recv())
                .await
                .expect("no connect")
                .unwrap();
            KissConnection::new(peer)
        }

        async fn open(&self, port: &str, peer: &str) -> (SessionId, mpsc::Receiver<Frame>) {
            self.open_with(port, peer, 16).await
        }

        async fn open_with(
            &self,
            port: &str,
            peer: &str,
            capacity: usize,
        ) -> (SessionId, mpsc::Receiver<Frame>) {
            let id = SessionId::new(port, peer);
            let (tx, rx) = mpsc::channel(capacity);
            let kick = CancellationToken::new();
            self.kicks.lock().unwrap().insert(id.clone(), kick.clone());
            self.handle
                .app_tx
                .send(AppEvent::SessionOpened {
                    id: id.clone(),
                    tx,
                    kick,
                })
                .await
                .unwrap();
            (id, rx)
        }

        async fn close(&self, id: &SessionId) {
            self.handle
                .app_tx
                .send(AppEvent::SessionClosed { id: id.clone() })
                .await
                .unwrap();
        }

        async fn send(&self, id: &SessionId, frame: &[u8]) {
            self.handle
                .app_tx
                .send(AppEvent::Frame {
                    id: id.clone(),
                    frame: frame.to_vec(),
                })
                .await
                .unwrap();
        }

        async fn wait_state(&mut self, want: impl Fn(&TncState) -> bool) -> TncState {
            timeout(Duration::from_secs(10), async {
                loop {
                    if want(&self.handle.state.borrow()) {
                        return self.handle.state.borrow().clone();
                    }
                    self.handle.state.changed().await.unwrap();
                }
            })
            .await
            .expect("state not reached")
        }
    }

    async fn recv(rx: &mut mpsc::Receiver<Frame>) -> Frame {
        timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("no frame")
            .unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn on_demand_connects_with_first_session_and_routes_by_role() {
        let mut h = start(ConnectPolicy::OnDemand, Duration::ZERO, AppParams::Pass);
        assert_eq!(*h.handle.state.borrow(), TncState::Idle);
        let (a, mut a_rx) = h.open("phones", "A").await;
        let (b, mut b_rx) = h.open("lan", "B").await;
        let mut tnc = h.tnc().await;
        h.wait_state(|s| *s == TncState::Connected).await;

        // TNC -> every session.
        tnc.write_frame(&[0x00, 0xAA]).await.unwrap();
        assert_eq!(recv(&mut a_rx).await, vec![0x00, 0xAA]);
        assert_eq!(recv(&mut b_rx).await, vec![0x00, 0xAA]);

        // App -> TNC, and never app -> app.
        h.send(&a, &[0x00, 0x01]).await;
        assert_eq!(tnc.read_frame().await.unwrap(), vec![0x00, 0x01]);
        h.send(&b, &[0x00, 0x02]).await;
        assert_eq!(tnc.read_frame().await.unwrap(), vec![0x00, 0x02]);
        assert!(
            timeout(Duration::from_millis(100), a_rx.recv())
                .await
                .is_err()
        );
        assert!(
            timeout(Duration::from_millis(100), b_rx.recv())
                .await
                .is_err()
        );

        assert_eq!(h.handle.counters.frames_up(), 2);
        assert_eq!(h.handle.counters.frames_down(), 1);
        assert_eq!(h.handle.counters.sessions(), 2);

        // Last session leaves: the link closes (idle_timeout = 0).
        h.close(&a).await;
        h.close(&b).await;
        h.wait_state(|s| *s == TncState::Idle).await;
        assert!(matches!(tnc.read_frame().await, Err(Error::LinkClosed)));
        h.handle.shutdown.cancel();
        h.task.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn uplink_is_serialized_and_filtered() {
        let mut h = start(ConnectPolicy::OnDemand, Duration::ZERO, AppParams::Drop);
        let (a, _a_rx) = h.open("phones", "A").await;
        let (b, _b_rx) = h.open("lan", "B").await;
        let mut tnc = h.tnc().await;
        h.wait_state(|s| *s == TncState::Connected).await;

        // Two sessions send 50 frames each; all 100 arrive intact.
        let tx = h.handle.app_tx.clone();
        let (a2, b2) = (a.clone(), b.clone());
        let sender_a = tokio::spawn(async move {
            for i in 0..50u8 {
                tx.send(AppEvent::Frame {
                    id: a2.clone(),
                    frame: vec![0x00, 0xA0, i, 0xDB, 0xDC],
                })
                .await
                .unwrap();
            }
        });
        let tx = h.handle.app_tx.clone();
        let sender_b = tokio::spawn(async move {
            for i in 0..50u8 {
                tx.send(AppEvent::Frame {
                    id: b2.clone(),
                    frame: vec![0x00, 0xB0, i],
                })
                .await
                .unwrap();
            }
        });
        let (mut got_a, mut got_b) = (0u8, 0u8);
        for n in 0..100 {
            let f = timeout(Duration::from_secs(5), tnc.read_frame())
                .await
                .unwrap_or_else(|_| panic!("only {n} of 100 frames reached the TNC"))
                .unwrap();
            match f[1] {
                0xA0 => {
                    assert_eq!(f, vec![0x00, 0xA0, got_a, 0xDB, 0xDC]);
                    got_a += 1;
                }
                0xB0 => {
                    assert_eq!(f, vec![0x00, 0xB0, got_b]);
                    got_b += 1;
                }
                other => panic!("interleaved or corrupt frame: {other:#x}"),
            }
        }
        assert_eq!((got_a, got_b), (50, 50));
        sender_a.await.unwrap();
        sender_b.await.unwrap();
        assert_eq!(h.handle.counters.dropped(), 0);

        // Policy: KISS return never reaches the TNC; params dropped under app_params = drop.
        h.send(&a, &[0xFF]).await;
        h.send(&a, &[0x01, 0x32]).await;
        h.send(&a, &[0x00, 0x99]).await;
        assert_eq!(tnc.read_frame().await.unwrap(), vec![0x00, 0x99]);
        assert_eq!(h.handle.counters.dropped(), 2);
        h.handle.shutdown.cancel();
        h.task.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn link_loss_backs_off_reconnects_and_queues_uplink() {
        let mut h = start(ConnectPolicy::OnDemand, Duration::ZERO, AppParams::Pass);
        let (a, mut a_rx) = h.open("phones", "A").await;
        let tnc = h.tnc().await;
        h.wait_state(|s| *s == TncState::Connected).await;

        // The TNC goes away: backoff, then a fresh connect.
        drop(tnc);
        let st = h
            .wait_state(|s| matches!(s, TncState::Backoff { .. }))
            .await;
        assert_eq!(
            st,
            TncState::Backoff {
                attempt: 1,
                retry_in: Duration::from_secs(1)
            }
        );

        // A frame sent while reconnecting is queued, not lost.
        h.send(&a, &[0x00, 0x42]).await;
        let mut tnc2 = h.tnc().await;
        h.wait_state(|s| *s == TncState::Connected).await;
        assert_eq!(tnc2.read_frame().await.unwrap(), vec![0x00, 0x42]);
        assert_eq!(h.handle.counters.tnc_connects(), 2);

        tnc2.write_frame(&[0x00, 0x43]).await.unwrap();
        assert_eq!(recv(&mut a_rx).await, vec![0x00, 0x43]);
        h.handle.shutdown.cancel();
        h.task.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn failed_connects_back_off_exponentially() {
        let mut h = start(ConnectPolicy::OnDemand, Duration::ZERO, AppParams::Pass);
        h.fail.lock().unwrap().extend([Fail::Refused; 3]);
        let (_a, _rx) = h.open("phones", "A").await;
        let st = h
            .wait_state(|s| matches!(s, TncState::Backoff { attempt: 3, .. }))
            .await;
        assert_eq!(
            st,
            TncState::Backoff {
                attempt: 3,
                retry_in: Duration::from_secs(4)
            }
        );
        let _tnc = h.tnc().await;
        h.wait_state(|s| *s == TncState::Connected).await;
        h.handle.shutdown.cancel();
        h.task.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn idle_timeout_holds_the_link_briefly() {
        let mut h = start(
            ConnectPolicy::OnDemand,
            Duration::from_secs(2),
            AppParams::Pass,
        );
        let (a, _rx) = h.open("phones", "A").await;
        let mut tnc = h.tnc().await;
        h.wait_state(|s| *s == TncState::Connected).await;
        h.close(&a).await;
        // Still up a second later; a returning session cancels the timer.
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(*h.handle.state.borrow(), TncState::Connected);
        let (a, _rx) = h.open("phones", "A").await;
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert_eq!(*h.handle.state.borrow(), TncState::Connected);
        // Leave for good: closed after the timeout.
        h.close(&a).await;
        h.wait_state(|s| *s == TncState::Idle).await;
        assert!(matches!(tnc.read_frame().await, Err(Error::LinkClosed)));
        h.handle.shutdown.cancel();
        h.task.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn always_connects_without_sessions_and_stays_up() {
        let mut h = start(ConnectPolicy::Always, Duration::ZERO, AppParams::Pass);
        let _tnc = h.tnc().await;
        h.wait_state(|s| *s == TncState::Connected).await;
        let (a, _rx) = h.open("lan", "A").await;
        h.close(&a).await;
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert_eq!(*h.handle.state.borrow(), TncState::Connected);
        h.handle.shutdown.cancel();
        h.task.await.unwrap();
        assert_eq!(*h.handle.state.borrow(), TncState::Idle);
    }

    #[tokio::test(start_paused = true)]
    async fn slow_session_drops_instead_of_blocking() {
        let mut h = start(ConnectPolicy::OnDemand, Duration::ZERO, AppParams::Pass);
        let (_a, a_rx) = h.open("phones", "A").await; // never read: capacity 16
        let (_b, mut b_rx) = h.open_with("lan", "B", 64).await;
        let mut tnc = h.tnc().await;
        h.wait_state(|s| *s == TncState::Connected).await;
        let fast = tokio::spawn(async move {
            for i in 0..40u8 {
                assert_eq!(recv(&mut b_rx).await, vec![0x00, i]);
            }
        });
        for i in 0..40u8 {
            tnc.write_frame(&[0x00, i]).await.unwrap();
        }
        fast.await.unwrap();
        assert!(
            h.handle.counters.dropped() >= 24,
            "dropped {}",
            h.handle.counters.dropped()
        );
        drop(a_rx);
        h.handle.shutdown.cancel();
        h.task.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn unpaired_and_missing_devices_are_polled_not_backed_off() {
        let mut h = start(ConnectPolicy::OnDemand, Duration::ZERO, AppParams::Pass);
        h.fail.lock().unwrap().extend([
            Fail::Unpaired,
            Fail::Unpaired,
            Fail::Unpaired,
            Fail::Absent,
        ]);
        let (a, mut a_rx) = h.open("phones", "A").await;
        let st = h.wait_state(|s| matches!(s, TncState::Unpaired(_))).await;
        assert_eq!(st, TncState::Unpaired("fake: not paired".into()));
        // Polled at a fixed interval with no attempt count: three checks
        // take two poll intervals, not 1 + 2 s of backoff.
        tokio::time::sleep(UNPAIRED_POLL * 2 + Duration::from_millis(100)).await;
        assert_eq!(h.connects.load(Ordering::Relaxed), 3);
        let st = h
            .wait_state(|s| matches!(s, TncState::WaitingForDevice(_)))
            .await;
        assert_eq!(st, TncState::WaitingForDevice("fake: unplugged".into()));

        // Uplink waits with the link, and the device turning up connects.
        h.send(&a, &[0x00, 0x42]).await;
        let mut tnc = h.tnc().await;
        h.wait_state(|s| *s == TncState::Connected).await;
        assert_eq!(tnc.read_frame().await.unwrap(), vec![0x00, 0x42]);
        tnc.write_frame(&[0x00, 0x43]).await.unwrap();
        assert_eq!(recv(&mut a_rx).await, vec![0x00, 0x43]);

        // Everyone leaving while unpaired stops the polling.
        drop(tnc);
        h.fail.lock().unwrap().push_back(Fail::Unpaired);
        h.wait_state(|s| matches!(s, TncState::Unpaired(_))).await;
        h.close(&a).await;
        h.wait_state(|s| *s == TncState::Idle).await;
        let n = h.connects.load(Ordering::Relaxed);
        tokio::time::sleep(UNPAIRED_POLL * 3).await;
        assert_eq!(h.connects.load(Ordering::Relaxed), n);
        h.handle.shutdown.cancel();
        h.task.await.unwrap();
    }

    async fn read_all(tnc: &mut DuplexStream) -> Vec<u8> {
        let mut got = Vec::new();
        timeout(Duration::from_secs(10), tnc.read_to_end(&mut got))
            .await
            .expect("link not closed")
            .unwrap();
        got
    }

    #[tokio::test(start_paused = true)]
    async fn teardown_is_sent_on_purpose_only() {
        let teardown = vec![vec![0xC0, 0xFF, 0xC0], b"TN 1,0\r".to_vec()];
        let mut h = start_with(
            ConnectPolicy::OnDemand,
            Duration::ZERO,
            AppParams::Pass,
            teardown,
        );
        // The last session leaving closes the link with the teardown.
        let (a, _rx) = h.open("phones", "A").await;
        let mut tnc = timeout(Duration::from_secs(5), h.peers.recv())
            .await
            .unwrap()
            .unwrap();
        h.wait_state(|s| *s == TncState::Connected).await;
        h.close(&a).await;
        assert_eq!(read_all(&mut tnc).await, b"\xC0\xFF\xC0TN 1,0\r".to_vec());

        // A lost link gets none: it's gone, and the reconnect gets its own.
        let (a, _rx) = h.open("phones", "A").await;
        let tnc = timeout(Duration::from_secs(5), h.peers.recv())
            .await
            .unwrap()
            .unwrap();
        h.wait_state(|s| *s == TncState::Connected).await;
        drop(tnc);
        h.wait_state(|s| matches!(s, TncState::Backoff { .. }))
            .await;
        let mut tnc = timeout(Duration::from_secs(5), h.peers.recv())
            .await
            .unwrap()
            .unwrap();
        h.wait_state(|s| *s == TncState::Connected).await;

        // Shutdown waits for the teardown.
        h.handle.shutdown.cancel();
        h.task.await.unwrap();
        assert_eq!(read_all(&mut tnc).await, b"\xC0\xFF\xC0TN 1,0\r".to_vec());
        drop(a);
    }

    #[tokio::test(start_paused = true)]
    async fn settings_apply_live() {
        let mut h = start(ConnectPolicy::OnDemand, Duration::ZERO, AppParams::Pass);
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(h.connects.load(Ordering::Relaxed), 0);
        // on_demand -> always connects with nobody there.
        let s = settings(ConnectPolicy::Always, Duration::ZERO, AppParams::Pass);
        h.handle.cmd_tx.send(BridgeCmd::Settings(s)).await.unwrap();
        let mut tnc = h.tnc().await;
        h.wait_state(|s| *s == TncState::Connected).await;
        // always -> on_demand with nobody there lets go at once.
        let s = settings(ConnectPolicy::OnDemand, Duration::ZERO, AppParams::Drop);
        h.handle.cmd_tx.send(BridgeCmd::Settings(s)).await.unwrap();
        h.wait_state(|s| *s == TncState::Idle).await;
        assert!(matches!(tnc.read_frame().await, Err(Error::LinkClosed)));
        // app_params applies from the next frame.
        let (a, _rx) = h.open("lan", "A").await;
        let mut tnc = h.tnc().await;
        h.wait_state(|s| *s == TncState::Connected).await;
        h.send(&a, &[0x01, 0x32]).await;
        h.send(&a, &[0x00, 0x55]).await;
        assert_eq!(tnc.read_frame().await.unwrap(), vec![0x00, 0x55]);
        h.handle.shutdown.cancel();
        h.task.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn a_new_connector_restarts_the_link_and_keeps_sessions() {
        let mut h = start(ConnectPolicy::OnDemand, Duration::ZERO, AppParams::Pass);
        let (a, mut a_rx) = h.open("phones", "A").await;
        let mut old = h.tnc().await;
        h.wait_state(|s| *s == TncState::Connected).await;

        let (peers_tx, mut peers) = mpsc::unbounded_channel();
        let next = FakeTnc {
            peers: peers_tx,
            fail: Arc::new(Mutex::new(VecDeque::new())),
            connects: Arc::new(AtomicU64::new(0)),
            exit_kiss: Vec::new(),
        };
        h.handle
            .cmd_tx
            .send(BridgeCmd::Connector(Box::new(next)))
            .await
            .unwrap();
        assert!(matches!(old.read_frame().await, Err(Error::LinkClosed)));
        let mut new = KissConnection::new(
            timeout(Duration::from_secs(5), peers.recv())
                .await
                .unwrap()
                .unwrap(),
        );
        h.wait_state(|s| *s == TncState::Connected).await;
        // The session never noticed: frames flow both ways on the new link.
        new.write_frame(&[0x00, 0x77]).await.unwrap();
        assert_eq!(recv(&mut a_rx).await, vec![0x00, 0x77]);
        h.send(&a, &[0x00, 0x78]).await;
        assert_eq!(new.read_frame().await.unwrap(), vec![0x00, 0x78]);
        assert_eq!(h.handle.counters.sessions(), 1);
        h.handle.shutdown.cancel();
        h.task.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn snapshots_and_kicks() {
        let mut h = start(ConnectPolicy::OnDemand, Duration::ZERO, AppParams::Pass);
        let (a, _a_rx) = h.open("phones", "A").await;
        let (b, _b_rx) = h.open("lan", "B").await;
        let _tnc = h.tnc().await;
        h.wait_state(|s| *s == TncState::Connected).await;
        let snap = h.handle.snapshot().await.unwrap();
        assert_eq!(snap.tnc, TncState::Connected);
        let ids: Vec<_> = snap.sessions.iter().map(|s| s.id.clone()).collect();
        assert_eq!(ids, vec![a.clone(), b.clone()], "oldest first");
        assert!(snap.sessions[0].seq < snap.sessions[1].seq);

        h.handle
            .cmd_tx
            .send(BridgeCmd::Kick(b.clone()))
            .await
            .unwrap();
        h.handle.snapshot().await.unwrap(); // the kick has been handled
        assert!(h.kicks.lock().unwrap()[&b].is_cancelled());
        assert!(!h.kicks.lock().unwrap()[&a].is_cancelled());
        h.handle.shutdown.cancel();
        h.task.await.unwrap();
        assert!(h.handle.snapshot().await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn exit_kiss_is_decided_when_letting_go() {
        let exit = vec![vec![0xC0, 0xFF, 0xC0]];
        let mut h = start_with(
            ConnectPolicy::OnDemand,
            Duration::ZERO,
            AppParams::Pass,
            exit,
        );
        // Started with it on; turned off before letting go: nothing sent.
        let (a, _rx) = h.open("phones", "A").await;
        let mut tnc = timeout(Duration::from_secs(5), h.peers.recv())
            .await
            .unwrap()
            .unwrap();
        h.wait_state(|s| *s == TncState::Connected).await;
        let s = settings(ConnectPolicy::OnDemand, Duration::ZERO, AppParams::Pass);
        h.handle
            .cmd_tx
            .send(BridgeCmd::Settings(s.clone()))
            .await
            .unwrap();
        // Commands and app events come on separate channels: wait for the
        // settings to land before the session leaves.
        h.handle.snapshot().await.unwrap();
        h.close(&a).await;
        assert_eq!(read_all(&mut tnc).await, Vec::<u8>::new());
        // Turned back on: sent.
        let on = TncSettings {
            exit_kiss_on_disconnect: true,
            ..s
        };
        h.handle.cmd_tx.send(BridgeCmd::Settings(on)).await.unwrap();
        h.handle.snapshot().await.unwrap();
        let (a, _rx) = h.open("phones", "A").await;
        let mut tnc = timeout(Duration::from_secs(5), h.peers.recv())
            .await
            .unwrap()
            .unwrap();
        h.wait_state(|s| *s == TncState::Connected).await;
        h.close(&a).await;
        assert_eq!(read_all(&mut tnc).await, vec![0xC0, 0xFF, 0xC0]);
        h.handle.shutdown.cancel();
        h.task.await.unwrap();
    }
}
