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

#[cfg(target_os = "linux")]
pub mod manager;
pub mod policy;
pub mod tcp;

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncWrite, ReadHalf, WriteHalf};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Sleep;
use tokio_util::sync::CancellationToken;

use crate::config::{AppParams, ConnectPolicy};
use crate::error::Error;
use crate::port::stream::{KissReader, KissWriter};
use policy::{Verdict, filter_app_frame};

/// A KISS frame as forwarded: the raw bytes between FENDs, still stuffed.
pub type Frame = Vec<u8>;

/// A byte stream to a TNC.
pub trait Link: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Link for T {}
pub type BoxLink = Box<dyn Link>;
pub type ConnectFuture = Pin<Box<dyn Future<Output = Result<BoxLink, Error>> + Send>>;

/// How a bridge reaches its TNC: TCP connect, RFCOMM, opening a serial
/// device, or handing out a duplex pair in tests.
pub trait TncConnector: Send + Sync + 'static {
    fn connect(&self) -> ConnectFuture;
    /// For logs: `127.0.0.1:6700`, `tnc d75 over spp`, …
    fn describe(&self) -> String;
}

/// A connector from a closure.
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
        Box::pin((self.f)())
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
    Backoff { attempt: u32, retry_in: Duration },
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

/// The TNC port's settings, from config.
#[derive(Debug, Clone)]
pub struct TncSettings {
    pub port_id: String,
    pub connect: ConnectPolicy,
    pub idle_timeout: Duration,
    pub app_params: AppParams,
}

/// How everything else talks to a running bridge.
#[derive(Clone)]
pub struct BridgeHandle {
    pub id: String,
    pub app_tx: mpsc::Sender<AppEvent>,
    pub state: watch::Receiver<TncState>,
    pub counters: Arc<Counters>,
    pub shutdown: CancellationToken,
}

/// Uplink frames held while the TNC connects: at most this many, and none
/// older than this.
pub const UPLINK_QUEUE_FRAMES: usize = 32;
pub const UPLINK_QUEUE_AGE: Duration = Duration::from_secs(5);
pub const BACKOFF_MIN: Duration = Duration::from_secs(1);
pub const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// Frames queued for the TNC writer. Matches the app queue so a burst that
/// fits one doesn't overflow the other.
const WRITER_QUEUE: usize = 256;
const APP_QUEUE: usize = 256;

pub struct Bridge {
    id: String,
    connector: Box<dyn TncConnector>,
    settings: TncSettings,
    app_rx: mpsc::Receiver<AppEvent>,
    state_tx: watch::Sender<TncState>,
    counters: Arc<Counters>,
    shutdown: CancellationToken,
}

impl Bridge {
    pub fn new(
        id: impl Into<String>,
        connector: Box<dyn TncConnector>,
        settings: TncSettings,
    ) -> (Bridge, BridgeHandle) {
        let id = id.into();
        let (app_tx, app_rx) = mpsc::channel(APP_QUEUE);
        let (state_tx, state_rx) = watch::channel(TncState::Idle);
        let counters = Arc::new(Counters::default());
        let shutdown = CancellationToken::new();
        let handle = BridgeHandle {
            id: id.clone(),
            app_tx,
            state: state_rx,
            counters: counters.clone(),
            shutdown: shutdown.clone(),
        };
        let bridge = Bridge {
            id,
            connector,
            settings,
            app_rx,
            state_tx,
            counters,
            shutdown,
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
            state_tx,
            counters,
            shutdown,
        } = self;
        let (tnc_tx, tnc_rx) = mpsc::channel(64);
        let mut run = Run {
            id,
            connector,
            settings,
            app_rx,
            state_tx,
            counters,
            shutdown,
            tnc_tx,
            tnc_rx,
            sessions: HashMap::new(),
            link: TncLink::Idle,
            generation: 0,
            attempt: 0,
            pending: VecDeque::new(),
            idle_timer: None,
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
        task: JoinHandle<Result<BoxLink, Error>>,
    },
    Connected {
        writer_tx: mpsc::Sender<Frame>,
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
    Connected(Result<BoxLink, Error>),
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
    state_tx: watch::Sender<TncState>,
    counters: Arc<Counters>,
    shutdown: CancellationToken,
    tnc_tx: mpsc::Sender<TncEvent>,
    tnc_rx: mpsc::Receiver<TncEvent>,
    sessions: HashMap<SessionId, mpsc::Sender<Frame>>,
    link: TncLink,
    /// Bumped on every connect, so events from a dead link are ignored.
    generation: u64,
    /// Consecutive failed connects, for backoff.
    attempt: u32,
    /// Uplink frames waiting for the link, with when they arrived.
    pending: VecDeque<(Instant, Frame)>,
    idle_timer: Option<Pin<Box<Sleep>>>,
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
                ev = self.app_rx.recv() => match ev {
                    Some(ev) => self.on_app(ev),
                    None => {
                        tracing::debug!(bridge = %self.id, "all app ports gone");
                        break;
                    }
                },
                Some(ev) = self.tnc_rx.recv() => self.on_tnc(ev),
                ev = link_event(&mut self.link), if self.link.is_waiting() => self.on_link(ev),
                _ = idle_elapsed(&mut self.idle_timer), if self.idle_timer.is_some() => {
                    self.idle_timer = None;
                    if self.sessions.is_empty() {
                        tracing::info!(bridge = %self.id, "idle timeout, disconnecting the TNC");
                        self.disconnect();
                    }
                }
            }
        }
        self.disconnect();
        tracing::info!(bridge = %self.id, "bridge stopped");
    }

    fn set_state(&self, state: TncState) {
        self.state_tx.send_replace(state);
    }

    fn start_connect(&mut self) {
        self.generation += 1;
        let fut = self.connector.connect();
        self.link = TncLink::Connecting {
            task: tokio::spawn(fut),
        };
        self.set_state(TncState::Connecting);
        tracing::info!(bridge = %self.id, port = %self.settings.port_id, tnc = %self.connector.describe(), "connecting");
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

    /// Drop the link, whatever state it's in, and go idle.
    fn disconnect(&mut self) {
        match std::mem::replace(&mut self.link, TncLink::Idle) {
            TncLink::Connecting { task } => task.abort(),
            TncLink::Connected {
                writer_tx,
                reader,
                writer,
            } => {
                drop(writer_tx);
                reader.abort();
                writer.abort();
                tracing::info!(bridge = %self.id, port = %self.settings.port_id, "TNC disconnected");
            }
            TncLink::Backoff { .. } | TncLink::Idle => {}
        }
        self.generation += 1;
        self.pending.clear();
        self.set_state(TncState::Idle);
    }

    fn wants_link(&self) -> bool {
        self.settings.connect == ConnectPolicy::Always || !self.sessions.is_empty()
    }

    fn on_app(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::SessionOpened { id, tx } => {
                tracing::info!(bridge = %self.id, session = %id, "session opened");
                self.sessions.insert(id, tx);
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
                }
                self.counters
                    .sessions
                    .store(self.sessions.len(), Ordering::Relaxed);
                if self.sessions.is_empty() && self.settings.connect == ConnectPolicy::OnDemand {
                    if self.settings.idle_timeout.is_zero() {
                        tracing::info!(bridge = %self.id, "last session left, disconnecting the TNC");
                        self.disconnect();
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
                match filter_app_frame(&frame, self.settings.app_params) {
                    Verdict::Drop(why) => {
                        tracing::warn!(bridge = %self.id, session = %id, "dropped frame from app: {why}");
                        self.counters.dropped.fetch_add(1, Ordering::Relaxed);
                    }
                    Verdict::Pass => self.uplink(frame),
                }
            }
        }
    }

    fn uplink(&mut self, frame: Frame) {
        match &self.link {
            TncLink::Connected { writer_tx, .. } => match writer_tx.try_send(frame) {
                Ok(()) => {
                    self.counters.frames_up.fetch_add(1, Ordering::Relaxed);
                }
                Err(_) => {
                    tracing::warn!(bridge = %self.id, "TNC writer queue full, dropping a frame");
                    self.counters.dropped.fetch_add(1, Ordering::Relaxed);
                }
            },
            TncLink::Idle => {
                self.queue_pending(frame);
                self.start_connect();
            }
            TncLink::Connecting { .. } | TncLink::Backoff { .. } => self.queue_pending(frame),
        }
    }

    fn queue_pending(&mut self, frame: Frame) {
        if self.pending.len() >= UPLINK_QUEUE_FRAMES {
            self.pending.pop_front();
            self.counters.dropped.fetch_add(1, Ordering::Relaxed);
        }
        self.pending.push_back((Instant::now(), frame));
    }

    fn on_tnc(&mut self, ev: TncEvent) {
        match ev {
            TncEvent::Frame { generation, frame } => {
                if generation != self.generation {
                    return;
                }
                self.counters.frames_down.fetch_add(1, Ordering::Relaxed);
                for (id, tx) in &self.sessions {
                    if tx.try_send(frame.clone()).is_err() {
                        tracing::warn!(bridge = %self.id, session = %id, "session can't keep up, dropping a frame");
                        self.counters.dropped.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            TncEvent::Closed { generation, error } => {
                if generation != self.generation || !matches!(self.link, TncLink::Connected { .. })
                {
                    return;
                }
                tracing::warn!(bridge = %self.id, port = %self.settings.port_id, "TNC link lost: {error}");
                if let TncLink::Connected { reader, writer, .. } =
                    std::mem::replace(&mut self.link, TncLink::Idle)
                {
                    reader.abort();
                    writer.abort();
                }
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
            LinkEvent::Connected(Ok(link)) => {
                let generation = self.generation;
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
                self.link = TncLink::Connected {
                    writer_tx,
                    reader,
                    writer,
                };
                self.attempt = 0;
                self.counters.tnc_connects.fetch_add(1, Ordering::Relaxed);
                self.set_state(TncState::Connected);
                tracing::info!(bridge = %self.id, port = %self.settings.port_id, "TNC connected");
                // Frames that arrived while connecting, if they're still fresh.
                let cutoff = Instant::now() - UPLINK_QUEUE_AGE;
                let pending: Vec<_> = self.pending.drain(..).collect();
                for (at, frame) in pending {
                    if at < cutoff {
                        self.counters.dropped.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    self.uplink(frame);
                }
                if !self.wants_link() && self.settings.idle_timeout.is_zero() {
                    // Everyone left while we were connecting.
                    self.disconnect();
                }
            }
            LinkEvent::Connected(Err(e)) => {
                tracing::warn!(bridge = %self.id, port = %self.settings.port_id, tnc = %self.connector.describe(), "connect failed: {e}");
                self.link = TncLink::Idle;
                if self.wants_link() {
                    self.start_backoff();
                } else {
                    self.pending.clear();
                    self.set_state(TncState::Idle);
                }
            }
            LinkEvent::BackoffOver => {
                self.link = TncLink::Idle;
                if self.wants_link() {
                    self.start_connect();
                } else {
                    self.pending.clear();
                    self.set_state(TncState::Idle);
                }
            }
        }
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
    mut rx: mpsc::Receiver<Frame>,
    tx: mpsc::Sender<TncEvent>,
    generation: u64,
) {
    while let Some(frame) = rx.recv().await {
        if let Err(error) = wr.write_frame(&frame).await {
            let _ = tx.send(TncEvent::Closed { generation, error }).await;
            return;
        }
    }
    // The bridge dropped its sender: it's disconnecting on purpose.
    let _ = wr.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::port::stream::KissConnection;
    use std::sync::Mutex;
    use tokio::io::DuplexStream;
    use tokio::time::{Duration, timeout};

    /// Hands the bridge one end of a duplex pair per connect and passes the
    /// other end to the test, which plays the TNC.
    struct FakeTnc {
        peers: mpsc::UnboundedSender<DuplexStream>,
        fail_next: Arc<Mutex<u32>>,
    }

    impl TncConnector for FakeTnc {
        fn connect(&self) -> ConnectFuture {
            let peers = self.peers.clone();
            let fail = self.fail_next.clone();
            Box::pin(async move {
                {
                    let mut n = fail.lock().unwrap();
                    if *n > 0 {
                        *n -= 1;
                        return Err(Error::TcpConnect("fake: refused".into()));
                    }
                }
                let (ours, theirs) = tokio::io::duplex(4096);
                peers
                    .send(theirs)
                    .map_err(|_| Error::TcpConnect("test gone".into()))?;
                Ok(Box::new(ours) as BoxLink)
            })
        }

        fn describe(&self) -> String {
            "fake tnc".into()
        }
    }

    struct Harness {
        handle: BridgeHandle,
        peers: mpsc::UnboundedReceiver<DuplexStream>,
        fail_next: Arc<Mutex<u32>>,
        task: JoinHandle<()>,
    }

    fn start(connect: ConnectPolicy, idle_timeout: Duration, app_params: AppParams) -> Harness {
        let (peers_tx, peers) = mpsc::unbounded_channel();
        let fail_next = Arc::new(Mutex::new(0));
        let connector = FakeTnc {
            peers: peers_tx,
            fail_next: fail_next.clone(),
        };
        let (bridge, handle) = Bridge::new(
            "ktbr0",
            Box::new(connector),
            TncSettings {
                port_id: "tnc".into(),
                connect,
                idle_timeout,
                app_params,
            },
        );
        let task = tokio::spawn(bridge.run());
        Harness {
            handle,
            peers,
            fail_next,
            task,
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
            self.handle
                .app_tx
                .send(AppEvent::SessionOpened { id: id.clone(), tx })
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
        *h.fail_next.lock().unwrap() = 3;
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
}
