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
//! `tcp-server`: an app port that accepts KISS TCP clients (Pat, Xastir,
//! YAAC, aprs.fi over Wi-Fi) and turns each into a bridge session.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use ipnet::IpNet;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::bridge::{AppEvent, Frame, SessionId};
use crate::config::Listen;
use crate::port::listen::{BindError, allowed, bind};
use crate::port::stream::{KissReader, KissWriter};

/// Frames queued per session on the way to the client.
const SESSION_QUEUE: usize = 64;
/// How often a port waiting for its interface tries again.
pub const INTERFACE_RETRY: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerState {
    Starting,
    Listening(std::net::SocketAddr),
    WaitingForInterface(String),
    Failed(String),
    Stopped,
}

/// The settings a reload changes without closing the listener. Sessions a
/// tighter setting no longer admits are kicked by the reconciler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TcpLive {
    /// `None`: no limit.
    pub max_clients: Option<u32>,
    /// Empty: any peer that can reach the socket.
    pub allow: Vec<IpNet>,
}

pub struct TcpServerPort {
    pub bridge_id: String,
    pub port_id: String,
    pub listen: Listen,
    pub live: watch::Receiver<TcpLive>,
    pub app_tx: mpsc::Sender<AppEvent>,
    pub shutdown: CancellationToken,
}

pub struct TcpServerHandle {
    pub state: watch::Receiver<ServerState>,
    pub sessions: Arc<AtomicUsize>,
}

impl TcpServerPort {
    /// Spawn the port. It binds (retrying every 5 s while an interface is
    /// missing), accepts clients, and runs until shut down.
    pub fn spawn(self) -> (TcpServerHandle, tokio::task::JoinHandle<()>) {
        let (state_tx, state_rx) = watch::channel(ServerState::Starting);
        let sessions = Arc::new(AtomicUsize::new(0));
        let handle = TcpServerHandle {
            state: state_rx,
            sessions: sessions.clone(),
        };
        let task = tokio::spawn(self.run(state_tx, sessions));
        (handle, task)
    }

    async fn run(self, state_tx: watch::Sender<ServerState>, sessions: Arc<AtomicUsize>) {
        let listener = loop {
            match bind(&self.listen).await {
                Ok(l) => break l,
                Err(BindError::NoInterface(name)) => {
                    tracing::warn!(bridge = %self.bridge_id, port = %self.port_id, "waiting for interface {name}");
                    state_tx.send_replace(ServerState::WaitingForInterface(name));
                    tokio::select! {
                        _ = tokio::time::sleep(INTERFACE_RETRY) => {}
                        _ = self.shutdown.cancelled() => {
                            state_tx.send_replace(ServerState::Stopped);
                            return;
                        }
                    }
                }
                Err(e) => {
                    tracing::error!(bridge = %self.bridge_id, port = %self.port_id, listen = %self.listen, "can't listen: {e}");
                    state_tx.send_replace(ServerState::Failed(e.to_string()));
                    return;
                }
            }
        };
        let local = listener.local_addr().ok();
        tracing::info!(bridge = %self.bridge_id, port = %self.port_id, listen = %self.listen, "listening");
        if let Some(local) = local {
            state_tx.send_replace(ServerState::Listening(local));
        }
        if self.listen.is_unspecified() && self.live.borrow().allow.is_empty() {
            tracing::warn!(
                bridge = %self.bridge_id, port = %self.port_id,
                "listening on all addresses with no allow list: anyone who can connect can transmit on your radio"
            );
        }

        let mut rejections = Rejections::default();
        loop {
            let (stream, peer) = tokio::select! {
                _ = self.shutdown.cancelled() => break,
                res = listener.accept() => match res {
                    Ok(x) => x,
                    Err(e) => {
                        tracing::warn!(bridge = %self.bridge_id, port = %self.port_id, "accept failed: {e}");
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        continue;
                    }
                },
            };
            let live = self.live.borrow().clone();
            if !allowed(&live.allow, peer.ip()) {
                if rejections.should_log(peer.ip()) {
                    tracing::warn!(bridge = %self.bridge_id, port = %self.port_id, %peer, "rejected: not in the allow list (repeats from this address logged at debug for a minute)");
                } else {
                    tracing::debug!(bridge = %self.bridge_id, port = %self.port_id, %peer, "rejected: not in the allow list");
                }
                continue;
            }
            if let Some(max) = live.max_clients
                && sessions.load(Ordering::Relaxed) >= max as usize
            {
                if rejections.should_log(peer.ip()) {
                    tracing::warn!(bridge = %self.bridge_id, port = %self.port_id, %peer, max, "rejected: max clients reached (repeats from this address logged at debug for a minute)");
                } else {
                    tracing::debug!(bridge = %self.bridge_id, port = %self.port_id, %peer, max, "rejected: max clients reached");
                }
                continue;
            }
            let _ = stream.set_nodelay(true);
            let id = SessionId::new(self.port_id.clone(), peer.to_string());
            sessions.fetch_add(1, Ordering::Relaxed);
            tokio::spawn(session(
                stream,
                id,
                self.app_tx.clone(),
                sessions.clone(),
                self.shutdown.child_token(),
            ));
        }
        state_tx.send_replace(ServerState::Stopped);
        tracing::info!(bridge = %self.bridge_id, port = %self.port_id, "stopped listening");
    }
}

/// Apps retry refused connections forever (aprs.fi every 5 s), so a refusal
/// is logged as a warning once a minute per address, and at debug between.
#[derive(Default)]
struct Rejections {
    last: std::collections::HashMap<std::net::IpAddr, tokio::time::Instant>,
}

impl Rejections {
    const QUIET: Duration = Duration::from_secs(60);

    fn should_log(&mut self, ip: std::net::IpAddr) -> bool {
        let now = tokio::time::Instant::now();
        self.last
            .retain(|_, t| now.duration_since(*t) < Self::QUIET);
        if self.last.contains_key(&ip) {
            return false;
        }
        self.last.insert(ip, now);
        true
    }
}

/// One client: frames from it go to the bridge, frames from the bridge go
/// to it, until either side closes, the session is kicked, or the port
/// shuts down (`kick` is a child of the port's token).
async fn session(
    stream: TcpStream,
    id: SessionId,
    app_tx: mpsc::Sender<AppEvent>,
    sessions: Arc<AtomicUsize>,
    kick: CancellationToken,
) {
    let (rd, wr) = stream.into_split();
    let mut reader = KissReader::new(rd);
    let mut writer = KissWriter::new(wr);
    let (tx, mut rx) = mpsc::channel::<Frame>(SESSION_QUEUE);
    if app_tx
        .send(AppEvent::SessionOpened {
            id: id.clone(),
            tx,
            kick: kick.clone(),
        })
        .await
        .is_err()
    {
        sessions.fetch_sub(1, Ordering::Relaxed);
        return;
    }
    loop {
        tokio::select! {
            _ = kick.cancelled() => break,
            res = reader.read_frame() => match res {
                Ok(frame) => {
                    if app_tx.send(AppEvent::Frame { id: id.clone(), frame }).await.is_err() {
                        break;
                    }
                }
                Err(e) => {
                    tracing::debug!(session = %id, "client gone: {e}");
                    break;
                }
            },
            Some(frame) = rx.recv() => {
                if let Err(e) = writer.write_frame(&frame).await {
                    tracing::debug!(session = %id, "write failed: {e}");
                    break;
                }
            }
        }
    }
    let _ = writer.shutdown().await;
    sessions.fetch_sub(1, Ordering::Relaxed);
    let _ = app_tx.send(AppEvent::SessionClosed { id }).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn refusals_are_logged_once_a_minute_per_address() {
        let mut r = Rejections::default();
        let a: std::net::IpAddr = "192.168.13.129".parse().unwrap();
        let b: std::net::IpAddr = "192.168.13.130".parse().unwrap();
        assert!(r.should_log(a));
        assert!(!r.should_log(a));
        assert!(r.should_log(b));
        tokio::time::advance(Duration::from_secs(59)).await;
        assert!(!r.should_log(a));
        tokio::time::advance(Duration::from_secs(2)).await;
        assert!(r.should_log(a));
    }
}
